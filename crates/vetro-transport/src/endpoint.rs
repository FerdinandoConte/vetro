//! Host and client endpoints, session setup and the session channels (ARCHITECTURE section 7).
//!
//! Stream layout of a session:
//! - control: the client's bidirectional stream 0 (the only one either side may open);
//! - input: the client's first unidirectional stream (index 0), highest priority;
//! - telemetry: the client's second unidirectional stream (index 1) and the host's first (index
//!   0), lowest priority;
//! - video fragments and cursor positions: datagrams, host → client only.
//!
//! The transport configs grant the peer exactly these streams, so a peer cannot open anything
//! else; the host never reads client datagrams.

use crate::cc::LowLatencyFactory;
use crate::channel::{MsgReceiver, MsgSender};
use crate::pairing::Trust;
use crate::verify::{ClientPins, HostPin};
use crate::{
    Error, Fingerprint, IDLE_TIMEOUT, Identity, KEEP_ALIVE, PRIORITY_CONTROL, PRIORITY_INPUT,
    PRIORITY_TELEMETRY, SETUP_TIMEOUT, close_code,
};
use bytes::{Bytes, BytesMut};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, TransportConfig, VarInt};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use vetro_protocol::Fragmenter;
use vetro_protocol::datagram::{CursorPosition, FrameInfo, encode_cursor};
use vetro_protocol::messages::{
    Bye, ByeReason, ControlMessage, Hello, InputMessage, PairToken, StreamMessage, TelemetryMessage,
};
use vetro_protocol::version::{ALPN, PROTOCOL_VERSION, Version, negotiate};

/// SNI the client sends. Never checked: the host certificate is pinned by fingerprint.
const SERVER_NAME: &str = "vetro";
const HOST_DATAGRAM_RECEIVE_BYTES: usize = 2048;
/// UDP socket receive buffer: room for whole frame bursts (measured in M1-3, see ARCHITECTURE
/// section 9). The send buffer stays at the OS default: bytes there cannot be dropped.
const SOCKET_RECEIVE_BYTES: usize = 4 * 1024 * 1024;
/// quinn's datagram send queue, where the oldest datagrams are dropped first: the host's only
/// sizeable send queue (the socket send buffer stays at the OS default). It must hold the
/// frame being queued plus the tail of the previous one: with 400 kB frames at 120 fps, 512 KiB
/// lost 12–14 % of frames on loopback and 1 MiB 0.2–0.8 % (M1-3). 1 MiB is about two large
/// keyframes, or 160 ms at 50 Mbit/s if the link stalls (rate control must prevent that, M1-7).
const DATAGRAM_SEND_QUEUE_BYTES: usize = 1024 * 1024;
/// TLS handshakes plus `Hello` exchanges in progress at once, in total and per remote IP; more
/// incoming connections are refused. Slots are taken only after QUIC address validation
/// (Retry), so spoofed sources cannot take any, and one real host cannot take more than 2.
/// Worst case about 64 × 0.5 MiB of buffers (the connection receive window).
const MAX_SETUPS: usize = 64;
const MAX_SETUPS_PER_IP: usize = 2;

/// One stream's flow-control window: its largest framed message (section 7.1).
fn stream_window<M: StreamMessage>() -> u64 {
    4 + M::MAX_BYTES as u64
}

/// Binds the UDP socket with a large receive buffer, then wraps it in a quinn endpoint.
fn endpoint(addr: SocketAddr, server: Option<quinn::ServerConfig>) -> Result<Endpoint, Error> {
    let socket = std::net::UdpSocket::bind(addr)?;
    // Best effort: the OS may cap it (Linux and Android: net.core.rmem_max), and a smaller
    // buffer still works, only with more loss on bursts.
    let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(SOCKET_RECEIVE_BYTES);
    let runtime = quinn::default_runtime()
        .ok_or_else(|| std::io::Error::other("not inside a tokio runtime"))?;
    Ok(Endpoint::new(
        quinn::EndpointConfig::default(),
        server,
        socket,
        runtime,
    )?)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport_config(host: bool) -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.congestion_controller_factory(Arc::new(LowLatencyFactory));
    t.max_idle_timeout(Some(
        IDLE_TIMEOUT
            .try_into()
            .expect("5 s fits a QUIC idle timeout"),
    ));
    t.keep_alive_interval(Some(KEEP_ALIVE));
    t.datagram_send_buffer_size(DATAGRAM_SEND_QUEUE_BYTES);
    // Flow control sized to the section 7.1 limits: one largest message per stream.
    let control = stream_window::<ControlMessage>();
    let telemetry = stream_window::<TelemetryMessage>();
    t.stream_receive_window(VarInt::from_u64(control).expect("fits a varint"));
    if host {
        // What the client may open: control, input, telemetry.
        t.max_concurrent_bidi_streams(VarInt::from_u32(1));
        t.max_concurrent_uni_streams(VarInt::from_u32(2));
        let total = control + stream_window::<InputMessage>() + telemetry;
        t.receive_window(VarInt::from_u64(total).expect("fits a varint"));
        // quinn sends datagrams only with a receive buffer set. The host never reads client
        // datagrams: quinn drops the oldest beyond this, so they cost at most 2 KiB.
        t.datagram_receive_buffer_size(Some(HOST_DATAGRAM_RECEIVE_BYTES));
    } else {
        // What the host may open: telemetry only.
        t.max_concurrent_bidi_streams(VarInt::from_u32(0));
        t.max_concurrent_uni_streams(VarInt::from_u32(1));
        t.receive_window(VarInt::from_u64(control + telemetry).expect("fits a varint"));
    }
    Arc::new(t)
}

/// The host's TLS config. Resumption is off: a resumed handshake would skip the verifier (and
/// so an unpair). 0-RTT stays off (rustls default `max_early_data_size = 0`).
fn host_tls(identity: &Identity, trust: Arc<Trust>) -> Result<rustls::ServerConfig, Error> {
    let provider = provider();
    let verifier = Arc::new(ClientPins::new(trust, &provider));
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.cert()], identity.key())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    tls.send_tls13_tickets = 0;
    Ok(tls)
}

/// The client's quinn config: pins `host`, presents `identity`, no resumption, no 0-RTT.
fn client_config(
    identity: &Identity,
    host: Fingerprint,
    transport: Arc<TransportConfig>,
) -> Result<quinn::ClientConfig, Error> {
    let provider = provider();
    let verifier = Arc::new(HostPin::new(host, &provider));
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![identity.cert()], identity.key())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.resumption = rustls::client::Resumption::disabled();
    let crypto =
        QuicClientConfig::try_from(tls).map_err(|_| Error::Identity("no QUIC cipher suite"))?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
    cfg.transport_config(transport);
    Ok(cfg)
}

fn close(conn: &Connection, code: u32) {
    conn.close(code.into(), b"");
}

fn peer_fingerprint(conn: &Connection) -> Result<Fingerprint, Error> {
    let certs = conn
        .peer_identity()
        .and_then(|id| id.downcast::<Vec<CertificateDer<'static>>>().ok())
        .ok_or(Error::Protocol("no peer certificate"))?;
    let cert = certs
        .first()
        .ok_or(Error::Protocol("no peer certificate"))?;
    Ok(Fingerprint::of(cert))
}

fn our_hello() -> ControlMessage {
    ControlMessage::Hello(Hello {
        version: PROTOCOL_VERSION,
        pair_token: None,
    })
}

/// Negotiates the version; on mismatch sends `Bye { VersionMismatch }` and closes.
async fn agree(
    conn: &Connection,
    control_tx: &mut MsgSender<ControlMessage>,
    remote: Version,
) -> Result<Version, Error> {
    match negotiate(PROTOCOL_VERSION, remote) {
        Ok(v) => Ok(v),
        Err(e) => {
            let bye = ControlMessage::Bye(Bye {
                reason: ByeReason::VersionMismatch,
            });
            let _ = control_tx.send(&bye).await;
            let _ = control_tx.finish();
            close(conn, close_code::VERSION_MISMATCH);
            Err(Error::VersionMismatch(e))
        }
    }
}

/// Reads the first control message, which must be `Hello`.
async fn read_hello(
    conn: &Connection,
    control_rx: &mut MsgReceiver<ControlMessage>,
) -> Result<Hello, Error> {
    match control_rx.recv().await? {
        Some(ControlMessage::Hello(h)) => Ok(h),
        _ => {
            close(conn, close_code::PROTOCOL_ERROR);
            Err(Error::Protocol("first control message is not Hello"))
        }
    }
}

async fn with_timeout<T>(f: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
    tokio::time::timeout(SETUP_TIMEOUT, f)
        .await
        .unwrap_or(Err(Error::Timeout))
}

// ---------------------------------------------------------------------------------------------
// Host

/// The engine's endpoint.
pub struct Host {
    endpoint: Endpoint,
    trust: Arc<Trust>,
    setups: Arc<Setups>,
}

impl Host {
    /// Binds `addr` with `identity`, trusting the `paired` client fingerprints. Call inside a
    /// tokio runtime.
    pub fn bind(
        addr: SocketAddr,
        identity: &Identity,
        paired: impl IntoIterator<Item = Fingerprint>,
    ) -> Result<Self, Error> {
        let trust = Arc::new(Trust::new(paired));
        let crypto = QuicServerConfig::try_from(host_tls(identity, trust.clone())?)
            .map_err(|_| Error::Identity("no QUIC cipher suite"))?;
        let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        cfg.transport_config(transport_config(true));
        Ok(Self {
            endpoint: endpoint(addr, Some(cfg))?,
            trust,
            setups: Arc::default(),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Opens a pairing window: returns a fresh one-time token for the QR code, valid for
    /// [`crate::PAIR_TOKEN_TTL`]. Any earlier token stops working.
    pub fn start_pairing(&self) -> Result<PairToken, Error> {
        self.trust.start_pairing(Instant::now())
    }

    /// Closes the pairing window (the tray hid the QR code): the token stops working and
    /// unknown devices fail at the handshake again.
    pub fn stop_pairing(&self) {
        self.trust.stop_pairing();
    }

    /// Forgets a client: its next handshake is rejected. Returns whether it was paired.
    /// Sessions already running are the caller's to close (M1-12).
    pub fn unpair(&self, client: &Fingerprint) -> bool {
        self.trust.unpair(client)
    }

    /// The pinned client fingerprints, for the caller to persist.
    pub fn paired(&self) -> Vec<Fingerprint> {
        self.trust.paired()
    }

    /// The next incoming connection, or `None` once the endpoint is closed. Run
    /// [`Connecting::establish`] on its own task so a slow peer cannot hold up the others.
    ///
    /// Every client first proves its address with a QUIC Retry (one extra round trip). While
    /// [`MAX_SETUPS`] setups, or [`MAX_SETUPS_PER_IP`] from the same IP, are in progress,
    /// further connections are refused.
    pub async fn accept(&self) -> Option<Connecting> {
        loop {
            let incoming = self.endpoint.accept().await?;
            if !incoming.remote_address_validated() {
                if let Err(e) = incoming.retry() {
                    e.into_incoming().refuse();
                }
                continue;
            }
            let Some(slot) = self.setups.take(incoming.remote_address().ip()) else {
                incoming.refuse();
                continue;
            };
            return Some(Connecting {
                incoming,
                trust: self.trust.clone(),
                _slot: slot,
            });
        }
    }

    pub fn close(&self) {
        self.endpoint.close(close_code::NORMAL.into(), b"");
    }
}

/// Setups in progress per remote IP.
#[derive(Default)]
struct Setups(Mutex<HashMap<IpAddr, usize>>);

impl Setups {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, usize>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take(self: &Arc<Self>, ip: IpAddr) -> Option<SetupSlot> {
        let ip = ip.to_canonical();
        let mut map = self.lock();
        let total: usize = map.values().sum();
        let here = map.get(&ip).copied().unwrap_or(0);
        if total >= MAX_SETUPS || here >= MAX_SETUPS_PER_IP {
            return None;
        }
        *map.entry(ip).or_insert(0) += 1;
        Some(SetupSlot {
            setups: self.clone(),
            ip,
        })
    }

    #[cfg(test)]
    fn in_progress(&self) -> usize {
        self.lock().values().sum()
    }
}

/// Frees one setup slot when the setup ends, however it ends.
struct SetupSlot {
    setups: Arc<Setups>,
    ip: IpAddr,
}

impl Drop for SetupSlot {
    fn drop(&mut self) {
        let mut map = self.setups.lock();
        if let Some(n) = map.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

/// A connection attempt the host has not set up yet.
pub struct Connecting {
    incoming: quinn::Incoming,
    trust: Arc<Trust>,
    _slot: SetupSlot,
}

impl Connecting {
    /// TLS handshake (client certificate pinned, or provisional during a pairing window), then
    /// `Hello` (version negotiated; for a new client, the pairing token checked first and
    /// redeemed last, so a setup that fails midway neither burns the token nor pins the client).
    pub async fn establish(self) -> Result<HostSession, Error> {
        with_timeout(self.run()).await
    }

    async fn run(self) -> Result<HostSession, Error> {
        let conn = self.incoming.accept()?.await?;
        let peer = peer_fingerprint(&conn)?;
        let (send, recv) = conn.accept_bi().await?;
        let mut control_tx = MsgSender::new(send, PRIORITY_CONTROL);
        let mut control_rx = MsgReceiver::new(recv);
        let hello = read_hello(&conn, &mut control_rx).await?;

        // The verifier let an unknown certificate through only because a pairing window was
        // open; now the token decides, before the host sends anything.
        let token = if self.trust.is_paired(&peer) {
            None
        } else {
            match hello.pair_token {
                Some(t) if self.trust.token_matches(&t, Instant::now()) => Some(t),
                _ => {
                    close(&conn, close_code::NOT_PAIRED);
                    return Err(Error::NotPaired);
                }
            }
        };

        control_tx.send(&our_hello()).await?;
        let version = agree(&conn, &mut control_tx, hello.version).await?;
        let telemetry_tx = MsgSender::new(conn.open_uni().await?, PRIORITY_TELEMETRY);

        let newly_paired = token.is_some();
        if let Some(t) = token
            && !self.trust.redeem(&t, peer, Instant::now())
        {
            // Another device used the token, or it expired, while this setup ran.
            close(&conn, close_code::NOT_PAIRED);
            return Err(Error::NotPaired);
        }
        Ok(HostSession {
            peer,
            version,
            newly_paired,
            control_tx,
            control_rx,
            telemetry_tx,
            conn,
            bufs: Mutex::default(),
        })
    }
}

/// A stream the client opened.
pub enum PeerStream {
    Input(MsgReceiver<InputMessage>),
    Telemetry(MsgReceiver<TelemetryMessage>),
}

/// Datagram buffers reused across frames, so steady-state sending allocates nothing.
#[derive(Default)]
struct SendBufs {
    /// Holds queued frame datagrams; `reserve` reclaims the allocation once quinn has sent and
    /// dropped every slice of the previous frame.
    datagrams: BytesMut,
    /// The same for cursor datagrams, kept apart so they never pin a frame buffer.
    cursor: BytesMut,
    cursor_scratch: Vec<u8>,
}

/// The host's side of a session.
pub struct HostSession {
    /// The client's certificate fingerprint.
    pub peer: Fingerprint,
    /// The negotiated protocol version.
    pub version: Version,
    /// The client paired on this connection: persist [`Host::paired`].
    pub newly_paired: bool,
    pub control_tx: MsgSender<ControlMessage>,
    pub control_rx: MsgReceiver<ControlMessage>,
    pub telemetry_tx: MsgSender<TelemetryMessage>,
    conn: Connection,
    bufs: Mutex<SendBufs>,
}

impl HostSession {
    /// The next stream the client opens: input, then telemetry. Anything else closes the
    /// session.
    pub async fn accept_stream(&self) -> Result<PeerStream, Error> {
        let recv = self.conn.accept_uni().await?;
        match recv.id().index() {
            0 => Ok(PeerStream::Input(MsgReceiver::new(recv))),
            1 => Ok(PeerStream::Telemetry(MsgReceiver::new(recv))),
            _ => {
                close(&self.conn, close_code::PROTOCOL_ERROR);
                Err(Error::Protocol("unexpected stream from the client"))
            }
        }
    }

    /// Largest datagram the path takes right now (grows with MTU discovery).
    pub fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    /// Fragments `frame` (with `fec_percent` parity) to the current datagram size and queues
    /// every fragment. Returns the number of datagrams. When quinn's send queue is full the
    /// oldest unsent datagrams are dropped (fresh beats complete, section 10).
    ///
    /// `Err(Error::Datagram(_))` loses only this frame (e.g. the path MTU shrank between sizing
    /// and sending): carry on and send a keyframe. `Err(Error::Connection(_))` ends the session.
    pub fn send_frame(
        &self,
        fragmenter: &mut Fragmenter,
        info: FrameInfo,
        frame: &[u8],
        fec_percent: u8,
    ) -> Result<usize, Error> {
        let max = self
            .conn
            .max_datagram_size()
            .ok_or(Error::Protocol("peer does not accept datagrams"))?;
        let chunks = fragmenter.fragment(info, frame, max, fec_percent)?;
        let count = chunks.len();
        let stride = chunks.clone().next().map_or(0, <[u8]>::len);
        let all = {
            let mut bufs = self.bufs.lock().unwrap_or_else(PoisonError::into_inner);
            bufs.datagrams.reserve(count * stride);
            chunks.for_each(|c| bufs.datagrams.extend_from_slice(c));
            bufs.datagrams.split().freeze()
        };
        for i in 0..count {
            self.conn
                .send_datagram(all.slice(i * stride..(i + 1) * stride))?;
        }
        Ok(count)
    }

    /// Queues a cursor position datagram (latest wins on the client).
    pub fn send_cursor(&self, pos: &CursorPosition) -> Result<(), Error> {
        let dgram = {
            let mut bufs = self.bufs.lock().unwrap_or_else(PoisonError::into_inner);
            let SendBufs {
                cursor,
                cursor_scratch,
                ..
            } = &mut *bufs;
            encode_cursor(pos, cursor_scratch);
            cursor.extend_from_slice(cursor_scratch);
            cursor.split().freeze()
        };
        Ok(self.conn.send_datagram(dgram)?)
    }

    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// Resolves when the session ends, with the reason.
    pub async fn closed(&self) -> Error {
        self.conn.closed().await.into()
    }

    pub fn close(&self, code: u32) {
        close(&self.conn, code);
    }
}

// ---------------------------------------------------------------------------------------------
// Client

/// The tablet's endpoint.
pub struct Client {
    endpoint: Endpoint,
    identity: Identity,
}

impl Client {
    /// Binds a local UDP socket (`0.0.0.0:0` in production). Call inside a tokio runtime.
    pub fn bind(addr: SocketAddr, identity: &Identity) -> Result<Self, Error> {
        Ok(Self {
            endpoint: endpoint(addr, None)?,
            identity: identity.clone(),
        })
    }

    /// Connects to the host at `addr`, accepting only the certificate with fingerprint `host`.
    /// On the first connection pass the QR's `pair_token`; afterwards `None`.
    pub async fn connect(
        &self,
        addr: SocketAddr,
        host: Fingerprint,
        pair_token: Option<PairToken>,
    ) -> Result<ClientSession, Error> {
        self.connect_with(addr, host, pair_token, transport_config(false))
            .await
    }

    async fn connect_with(
        &self,
        addr: SocketAddr,
        host: Fingerprint,
        pair_token: Option<PairToken>,
        transport: Arc<TransportConfig>,
    ) -> Result<ClientSession, Error> {
        let connecting = self.dial(addr, host, transport)?;
        with_timeout(async move {
            let conn = connecting.await?;
            let (send, recv) = conn.open_bi().await?;
            let mut control_tx = MsgSender::new(send, PRIORITY_CONTROL);
            let mut control_rx = MsgReceiver::new(recv);
            control_tx
                .send(&ControlMessage::Hello(Hello {
                    version: PROTOCOL_VERSION,
                    pair_token,
                }))
                .await?;
            let hello = read_hello(&conn, &mut control_rx).await?;
            let version = agree(&conn, &mut control_tx, hello.version).await?;
            // Opened in this order so their indices are 0 (input) and 1 (telemetry).
            let input_tx = MsgSender::new(conn.open_uni().await?, PRIORITY_INPUT);
            let telemetry_tx = MsgSender::new(conn.open_uni().await?, PRIORITY_TELEMETRY);
            Ok(ClientSession {
                version,
                control_tx,
                control_rx,
                input_tx,
                telemetry_tx,
                conn,
            })
        })
        .await
    }

    fn dial(
        &self,
        addr: SocketAddr,
        host: Fingerprint,
        transport: Arc<TransportConfig>,
    ) -> Result<quinn::Connecting, Error> {
        let cfg = client_config(&self.identity, host, transport)?;
        self.endpoint
            .connect_with(cfg, addr, SERVER_NAME)
            .map_err(|e| Error::Io(std::io::Error::other(e)))
    }
}

/// The client's side of a session.
pub struct ClientSession {
    pub version: Version,
    pub control_tx: MsgSender<ControlMessage>,
    pub control_rx: MsgReceiver<ControlMessage>,
    pub input_tx: MsgSender<InputMessage>,
    pub telemetry_tx: MsgSender<TelemetryMessage>,
    conn: Connection,
}

impl ClientSession {
    /// The host's telemetry stream. Anything else closes the session.
    pub async fn accept_telemetry(&self) -> Result<MsgReceiver<TelemetryMessage>, Error> {
        let recv = self.conn.accept_uni().await?;
        if recv.id().index() != 0 {
            close(&self.conn, close_code::PROTOCOL_ERROR);
            return Err(Error::Protocol("unexpected stream from the host"));
        }
        Ok(MsgReceiver::new(recv))
    }

    /// The next datagram (video fragment or cursor position); decode it with
    /// `vetro_protocol::datagram::decode_datagram`.
    pub async fn read_datagram(&self) -> Result<Bytes, Error> {
        Ok(self.conn.read_datagram().await?)
    }

    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// Resolves when the session ends, with the reason.
    pub async fn closed(&self) -> Error {
        self.conn.closed().await.into()
    }

    pub fn close(&self, code: u32) {
        close(&self.conn, code);
    }
}

#[cfg(test)]
mod tests {
    //! Misbehaving peers, built from the crate's own client config.
    use super::*;
    use vetro_protocol::messages::KeyframeRequest;

    fn local() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    struct Rig {
        host_id: Identity,
        host: Host,
        addr: SocketAddr,
    }

    fn rig(paired: &[Fingerprint]) -> Rig {
        let host_id = Identity::generate().unwrap();
        let host = Host::bind(local(), &host_id, paired.iter().copied()).unwrap();
        let addr = host.local_addr().unwrap();
        Rig {
            host_id,
            host,
            addr,
        }
    }

    fn client() -> (Identity, Client) {
        let id = Identity::generate().unwrap();
        let c = Client::bind(local(), &id).unwrap();
        (id, c)
    }

    #[test]
    fn host_tls_never_resumes_and_never_accepts_early_data() {
        let tls = host_tls(&Identity::generate().unwrap(), Arc::default()).unwrap();
        assert_eq!(tls.send_tls13_tickets, 0);
        assert!(!tls.session_storage.can_cache());
        assert_eq!(tls.max_early_data_size, 0);
        assert_eq!(tls.alpn_protocols, vec![ALPN.to_vec()]);
    }

    #[tokio::test]
    async fn a_peer_that_never_sends_hello_is_dropped_at_the_deadline() {
        let (id, c) = client();
        let r = rig(&[id.fingerprint()]);
        let started = Instant::now();
        let (h, conn) = tokio::join!(
            async { r.host.accept().await.unwrap().establish().await },
            async {
                c.dial(r.addr, r.host_id.fingerprint(), transport_config(false))
                    .unwrap()
                    .await
            },
        );
        let _conn = conn.unwrap(); // TLS done, then silence.
        assert!(matches!(h, Err(Error::Timeout)));
        assert!(started.elapsed() >= SETUP_TIMEOUT);
    }

    #[tokio::test]
    async fn a_first_message_other_than_hello_is_a_protocol_error() {
        let (id, c) = client();
        let r = rig(&[id.fingerprint()]);
        let (h, ()) = tokio::join!(
            async { r.host.accept().await.unwrap().establish().await },
            async {
                let conn = c
                    .dial(r.addr, r.host_id.fingerprint(), transport_config(false))
                    .unwrap()
                    .await
                    .unwrap();
                let (send, _recv) = conn.open_bi().await.unwrap();
                let mut tx = MsgSender::<ControlMessage>::new(send, PRIORITY_CONTROL);
                let msg = ControlMessage::KeyframeRequest(KeyframeRequest { lost_frame_id: 0 });
                tx.send(&msg).await.unwrap();
                let _ = conn.closed().await;
            },
        );
        assert!(matches!(h, Err(Error::Protocol(_))));
    }

    #[tokio::test]
    async fn a_setup_that_fails_after_hello_keeps_the_token_and_pins_nobody() {
        let r = rig(&[]);
        let pin = r.host_id.fingerprint();
        let token = r.host.start_pairing().unwrap();

        // This client grants the host no stream for telemetry, so the host's setup stalls
        // after its Hello and hits the deadline.
        let mut stingy = TransportConfig::default();
        stingy.max_concurrent_uni_streams(VarInt::from_u32(0));
        let (_, bad) = client();
        let (h, c) = tokio::join!(
            async { r.host.accept().await.unwrap().establish().await },
            bad.connect_with(r.addr, pin, Some(token.clone()), Arc::new(stingy)),
        );
        assert!(matches!(h, Err(Error::Timeout)), "{:?}", h.err());
        // The client got the host's Hello, so the host failed after it.
        assert!(c.is_ok());
        assert!(r.host.paired().is_empty());

        // The token still pairs a well-behaved device.
        let (good_id, good) = client();
        let (h, c) = tokio::join!(
            async { r.host.accept().await.unwrap().establish().await },
            good.connect(r.addr, pin, Some(token)),
        );
        assert!(h.unwrap().newly_paired && c.is_ok());
        assert_eq!(r.host.paired(), vec![good_id.fingerprint()]);
    }

    #[tokio::test]
    async fn a_third_client_stream_closes_the_session() {
        let (id, c) = client();
        let r = rig(&[id.fingerprint()]);
        let (h, s) = tokio::join!(
            async { r.host.accept().await.unwrap().establish().await },
            c.connect(r.addr, r.host_id.fingerprint(), None),
        );
        let (h, mut s) = (h.unwrap(), s.unwrap());
        s.input_tx.finish().unwrap();
        s.telemetry_tx.finish().unwrap();
        for _ in 0..2 {
            match h.accept_stream().await.unwrap() {
                PeerStream::Input(mut rx) => assert!(rx.recv().await.unwrap().is_none()),
                PeerStream::Telemetry(mut rx) => assert!(rx.recv().await.unwrap().is_none()),
            }
        }
        // Both streams are fully read, so the host grants new credit; a third stream is index 2.
        let mut extra = s.conn.open_uni().await.unwrap();
        extra.write_all(&[0, 0, 0, 0]).await.unwrap();
        assert!(matches!(h.accept_stream().await, Err(Error::Protocol(_))));
    }

    #[tokio::test]
    async fn one_source_holding_its_slots_does_not_lock_out_the_paired_tablet() {
        let (_, flooder) = client();
        let (tablet_id, tablet) = {
            let id = Identity::generate().unwrap();
            // Another source address: the whole 127.0.0.0/8 is loopback.
            let c = Client::bind("127.0.0.2:0".parse().unwrap(), &id).unwrap();
            (id, c)
        };
        let r = rig(&[tablet_id.fingerprint()]);
        let pin = r.host_id.fingerprint();

        // The flooder holds every slot its IP may have: address-validated (Retry), never set up.
        let mut held = Vec::new();
        let mut dials = Vec::new();
        for _ in 0..MAX_SETUPS_PER_IP {
            dials.push(flooder.dial(r.addr, pin, transport_config(false)).unwrap());
            held.push(r.host.accept().await.unwrap());
        }
        assert_eq!(r.host.setups.in_progress(), MAX_SETUPS_PER_IP);
        // Its next attempt is refused inside the accept loop, so its handshake fails.
        let dial = flooder.dial(r.addr, pin, transport_config(false)).unwrap();
        let refused = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                res = dial => res.is_err(),
                _ = r.host.accept() => false,
            }
        })
        .await
        .expect("refusal within 5 s");
        assert!(refused);

        // The paired tablet, from another address, still gets a session.
        let (h, c) = tokio::join!(
            async { r.host.accept().await.unwrap().establish().await },
            tablet.connect(r.addr, pin, None),
        );
        assert!(h.is_ok() && c.is_ok(), "{:?} {:?}", h.err(), c.err());
        drop(held);
        assert_eq!(r.host.setups.in_progress(), 0);
    }

    #[test]
    fn setup_slots_are_capped_per_ip_and_in_total() {
        let setups = Arc::new(Setups::default());
        let ip = |i: u8| IpAddr::from([10, 0, 0, i]);
        let mut slots = Vec::new();
        for i in 0..(MAX_SETUPS / MAX_SETUPS_PER_IP) as u8 {
            for _ in 0..MAX_SETUPS_PER_IP {
                slots.push(setups.take(ip(i)).expect("under both caps"));
            }
            assert!(setups.take(ip(i)).is_none(), "per-IP cap");
        }
        assert_eq!(setups.in_progress(), MAX_SETUPS);
        assert!(setups.take(ip(250)).is_none(), "global cap");
        // A finished setup frees its slot.
        slots.pop();
        assert!(setups.take(ip(250)).is_some());
        drop(slots);
        assert_eq!(setups.in_progress(), 0);
        assert!(setups.lock().is_empty());

        // An IPv4-mapped IPv6 address counts as the same host.
        let v4 = IpAddr::from([192, 168, 1, 5]);
        let mapped = IpAddr::from(std::net::Ipv4Addr::new(192, 168, 1, 5).to_ipv6_mapped());
        let _a = setups.take(v4).unwrap();
        let _b = setups.take(mapped).unwrap();
        assert!(setups.take(v4).is_none());
    }
}
