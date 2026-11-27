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
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vetro_protocol::Fragmenter;
use vetro_protocol::datagram::{CursorPosition, FrameInfo, encode_cursor};
use vetro_protocol::messages::{
    Bye, ByeReason, ControlMessage, Hello, InputMessage, PairToken, TelemetryMessage,
};
use vetro_protocol::version::{ALPN, PROTOCOL_VERSION, Version, negotiate};

/// SNI the client sends. Never checked: the host certificate is pinned by fingerprint.
const SERVER_NAME: &str = "vetro";
const HOST_DATAGRAM_RECEIVE_BYTES: usize = 2048;
/// UDP socket buffers: room for whole frame bursts. With the OS defaults, 19–22 % of 52 kB frames
/// were lost on Windows loopback; with 4 MiB, 0.3 % (M1-3, `examples/loopback_bench.rs`).
const SOCKET_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// Binds the UDP socket with large buffers, then wraps it in a quinn endpoint.
fn endpoint(addr: SocketAddr, server: Option<quinn::ServerConfig>) -> Result<Endpoint, Error> {
    let socket = std::net::UdpSocket::bind(addr)?;
    let sock = socket2::SockRef::from(&socket);
    // Best effort: the OS may cap it (Linux and Android: net.core.rmem_max), and a smaller
    // buffer still works, only with more loss on bursts.
    let _ = sock.set_recv_buffer_size(SOCKET_BUFFER_BYTES);
    let _ = sock.set_send_buffer_size(SOCKET_BUFFER_BYTES);
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
    if host {
        // What the client may open: control, input, telemetry.
        t.max_concurrent_bidi_streams(VarInt::from_u32(1));
        t.max_concurrent_uni_streams(VarInt::from_u32(2));
        // quinn sends datagrams only with a receive buffer set. The host never reads client
        // datagrams: quinn drops the oldest beyond this, so they cost at most 2 KiB.
        t.datagram_receive_buffer_size(Some(HOST_DATAGRAM_RECEIVE_BYTES));
    } else {
        // What the host may open: telemetry only.
        t.max_concurrent_bidi_streams(VarInt::from_u32(0));
        t.max_concurrent_uni_streams(VarInt::from_u32(1));
    }
    Arc::new(t)
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
        let provider = provider();
        let verifier = Arc::new(ClientPins::new(trust.clone(), &provider));
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![identity.cert()], identity.key())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        // No resumption: a resumed handshake would skip the verifier (and so an unpair).
        tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        tls.send_tls13_tickets = 0;
        let crypto =
            QuicServerConfig::try_from(tls).map_err(|_| Error::Identity("no QUIC cipher suite"))?;
        let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        cfg.transport_config(transport_config(true));
        Ok(Self {
            endpoint: endpoint(addr, Some(cfg))?,
            trust,
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

    /// Forgets a client: its next handshake is rejected. Returns whether it was paired.
    /// Sessions already running are the caller's to close.
    pub fn unpair(&self, client: &Fingerprint) -> bool {
        self.trust.unpair(client)
    }

    /// The pinned client fingerprints, for the caller to persist.
    pub fn paired(&self) -> Vec<Fingerprint> {
        self.trust.paired()
    }

    /// The next incoming connection, or `None` once the endpoint is closed. Run
    /// [`Connecting::establish`] on its own task so a slow peer cannot hold up the others.
    pub async fn accept(&self) -> Option<Connecting> {
        let incoming = self.endpoint.accept().await?;
        Some(Connecting {
            incoming,
            trust: self.trust.clone(),
        })
    }

    pub fn close(&self) {
        self.endpoint.close(close_code::NORMAL.into(), b"");
    }
}

/// A connection attempt the host has not set up yet.
pub struct Connecting {
    incoming: quinn::Incoming,
    trust: Arc<Trust>,
}

impl Connecting {
    /// TLS handshake (client certificate pinned, or provisional during a pairing window), then
    /// `Hello` (pairing token redeemed if the client is new, version negotiated).
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

        let newly_paired = if self.trust.is_paired(&peer) {
            false
        } else {
            // The verifier let an unknown certificate through only because a pairing window
            // was open; now the token decides.
            let redeemed = hello
                .pair_token
                .as_ref()
                .is_some_and(|t| self.trust.redeem(t, peer, Instant::now()));
            if !redeemed {
                close(&conn, close_code::NOT_PAIRED);
                return Err(Error::NotPaired);
            }
            true
        };

        control_tx.send(&our_hello()).await?;
        let version = agree(&conn, &mut control_tx, hello.version).await?;
        let telemetry_tx = MsgSender::new(conn.open_uni().await?, PRIORITY_TELEMETRY);
        Ok(HostSession {
            peer,
            version,
            newly_paired,
            control_tx,
            control_rx,
            telemetry_tx,
            conn,
        })
    }
}

/// A stream the client opened.
pub enum PeerStream {
    Input(MsgReceiver<InputMessage>),
    Telemetry(MsgReceiver<TelemetryMessage>),
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
    /// every fragment. Returns the number of datagrams. When quinn's send buffer is full the
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
        // shortcut: one allocation per frame (quinn takes owned `Bytes`, the slices below share
        // it); pool these buffers if HIL shows allocator jitter.
        let mut all = BytesMut::with_capacity(count * stride);
        chunks.for_each(|c| all.extend_from_slice(c));
        let all = all.freeze();
        for i in 0..count {
            self.conn
                .send_datagram(all.slice(i * stride..(i + 1) * stride))?;
        }
        Ok(count)
    }

    /// Queues a cursor position datagram (latest wins on the client).
    pub fn send_cursor(&self, pos: &CursorPosition) -> Result<(), Error> {
        let mut buf = Vec::with_capacity(16);
        encode_cursor(pos, &mut buf);
        Ok(self.conn.send_datagram(Bytes::from(buf))?)
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
        let provider = provider();
        let verifier = Arc::new(HostPin::new(host, &provider));
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![self.identity.cert()], self.identity.key())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        tls.resumption = rustls::client::Resumption::disabled();
        let crypto =
            QuicClientConfig::try_from(tls).map_err(|_| Error::Identity("no QUIC cipher suite"))?;
        let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
        cfg.transport_config(transport_config(false));
        let connecting = self
            .endpoint
            .connect_with(cfg, addr, SERVER_NAME)
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
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
