//! Vetro host engine (ARCHITECTURE section 5, ADR-0011).
//!
//! M1-4 skeleton: command line, identity path, one session at a time, a synthetic
//! [`source::VideoSource`] on its own thread, the per-frame send path and JSONL telemetry.
//! Capture (M1-9), the encoder (M1-7), the virtual display (M1-8), input injection (M1-10) and
//! the tray IPC (M1-12) slot in later.
//!
//! Session flow (section 7): the client's `Capabilities` (with its display mode) starts the
//! stream: the engine answers `Configure` and starts the source; `KeyframeRequest` forces the
//! next frame to be a keyframe; `ClockSyncRequest` is answered on the host clock that also
//! stamps every frame. Every client stream is read for the whole session (section 8, flow
//! control). A newly established session replaces the running one (fresh beats complete).
#![forbid(unsafe_code)]

pub mod config;
pub mod discovery;
mod pump;
pub mod source;
pub mod telemetry;

pub use config::{Config, TestPattern};

use pump::{EncodedFrame, Pump};
use source::{Clock, MovingBars, VideoSource};
use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use telemetry::{FrameRecord, Telemetry};
use tokio::sync::mpsc;
use vetro_protocol::Fragmenter;
use vetro_protocol::messages::{
    Capabilities, ClockSyncResponse, Configure, ControlMessage, PairToken, Profile,
};
use vetro_transport::{
    Error, Fingerprint, Host, HostSession, Identity, MsgReceiver, PeerStream, close_code,
};

/// Reed–Solomon parity per frame.
/// shortcut: fixed 10 %; section 7 wants adaptive 5–25 % on Wi-Fi from the client's measured
/// loss (with rate control, M1-7) and 0 % on USB (M1-14).
const FEC_PERCENT: u8 = 10;

/// A control reply that cannot be queued in this time means the client stopped reading its
/// control stream; the session is closed rather than left frozen.
const CONTROL_SEND_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a clean stop waits for the close packets to leave.
const CLOSE_WAIT: Duration = Duration::from_millis(500);

/// Lowest refresh the engine streams at; protocol validation caps the top.
const MIN_REFRESH_MHZ: u32 = 10_000;

fn log(args: fmt::Arguments<'_>) {
    // Cold path only: session start/end and refused connections, never per frame.
    eprintln!("vetro-engine: {args}");
}

/// The bound endpoint plus everything a session needs.
pub struct Engine {
    host: Arc<Host>,
    fingerprint: Fingerprint,
    clock: Clock,
    cfg: Config,
    telemetry: Telemetry,
}

enum End {
    Replaced(Box<HostSession>),
    Closed,
    Shutdown,
}

impl Engine {
    /// Binds `cfg.bind` with `identity` and opens the telemetry file. Call inside a tokio
    /// runtime.
    pub fn bind(cfg: &Config, identity: &Identity) -> Result<Self, Error> {
        let telemetry = match &cfg.telemetry_out {
            Some(path) => Telemetry::to_file(path)?,
            None => Telemetry::disabled(),
        };
        // shortcut: pairings live in memory only; M1-12 persists `Host::paired()` after every
        // pairing and passes the stored list here.
        let host = Host::bind(cfg.bind, identity, [])?;
        Ok(Self {
            host: Arc::new(host),
            fingerprint: identity.fingerprint(),
            clock: Clock::new(),
            cfg: cfg.clone(),
            telemetry,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        self.host.local_addr()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Opens a pairing window (5 minutes, one use).
    pub fn start_pairing(&self) -> Result<PairToken, Error> {
        self.host.start_pairing()
    }

    /// The pairing QR's content (ARCHITECTURE section 8), token as 32 lowercase hex digits.
    /// `a=` is the `--bind` address, or for a wildcard bind the first suitable LAN IPv4
    /// address ([`discovery::suitable`]); with none the call fails.
    /// shortcut: one address; listing every interface address comes with the tray QR (M1-12).
    pub fn pair_uri(&self, token: &PairToken) -> Result<String, Error> {
        let addr = self.local_addr()?;
        let ip = discovery::pair_ip(addr, &discovery::lan_ipv4s()).ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no LAN IPv4 address for the pairing URI: join a network or pass --bind <IP>:port",
            ))
        })?;
        let t: String = token.0.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!(
            "vetro://pair?fp={}&t={t}&a={ip}&p={}",
            self.fingerprint,
            addr.port()
        ))
    }

    /// Serves sessions until `shutdown` resolves, then closes the endpoint and flushes
    /// telemetry.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<(), Error> {
        let advertiser = self.advertise();
        let (tx, mut sessions) = mpsc::channel::<HostSession>(1);
        let host = self.host.clone();
        let acceptor = tokio::spawn(async move {
            while let Some(connecting) = host.accept().await {
                let tx = tx.clone();
                // Own task per setup, so a slow peer holds up nobody (transport docs).
                tokio::spawn(async move {
                    match connecting.establish().await {
                        Ok(s) => {
                            let _ = tx.send(s).await;
                        }
                        Err(e) => log(format_args!("connection not set up: {e}")),
                    }
                });
            }
        });

        tokio::pin!(shutdown);
        let mut next = None;
        for session_no in 0u32.. {
            let session = match next.take() {
                Some(s) => s,
                None => tokio::select! {
                    () = &mut shutdown => break,
                    Some(s) = sessions.recv() => s,
                },
            };
            match self
                .serve(session, session_no, &mut sessions, &mut shutdown)
                .await
            {
                End::Replaced(s) => next = Some(*s),
                End::Closed => {}
                End::Shutdown => break,
            }
        }

        acceptor.abort();
        if let Some(a) = advertiser {
            // Withdraw first, so no tablet is sent to an engine that is going away. The goodbye
            // waits on a thread: not on an async worker.
            let _ = tokio::task::spawn_blocking(move || a.stop()).await;
        }
        self.host.close();
        let _ = tokio::time::timeout(CLOSE_WAIT, self.host.wait_idle()).await;
        let dropped = self.telemetry.dropped();
        if dropped > 0 {
            log(format_args!("{dropped} telemetry records dropped"));
        }
        self.telemetry.finish()?;
        log(format_args!("stopped"));
        Ok(())
    }

    /// Publishes the service on the LAN addresses of the bound socket. Discovery is a
    /// convenience (the tablet can always dial an address, section 6.2), so a failure is
    /// logged and the engine serves anyway.
    fn advertise(&self) -> Option<discovery::Advertiser> {
        let bound = self.local_addr().ok()?;
        let addrs = discovery::dial_addrs(bound, &discovery::lan_ipv4s());
        if addrs.is_empty() {
            log(format_args!(
                "mDNS off: no suitable LAN IPv4 address for {bound}"
            ));
            return None;
        }
        match discovery::Advertiser::start(&addrs, bound.port(), self.fingerprint) {
            Ok(a) => {
                log(format_args!("advertising _vetro._udp on {addrs:?}"));
                Some(a)
            }
            Err(e) => {
                log(format_args!("mDNS off: {e}"));
                None
            }
        }
    }

    async fn serve<F: Future<Output = ()>>(
        &self,
        mut s: HostSession,
        session_no: u32,
        sessions: &mut mpsc::Receiver<HostSession>,
        shutdown: &mut Pin<&mut F>,
    ) -> End {
        log(format_args!(
            "session with {}{}",
            s.peer,
            if s.newly_paired {
                " (newly paired)"
            } else {
                ""
            }
        ));
        let streams = s.stream_acceptor();
        let mut accepted = 0;
        let mut input = None;
        let mut client_telemetry = None;
        let mut pump: Option<Pump> = None;
        let mut fragmenter = Fragmenter::new();

        let (end, code, why) = loop {
            tokio::select! {
                () = &mut *shutdown => break (End::Shutdown, close_code::NORMAL, "engine stopping".into()),
                Some(newer) = sessions.recv() => {
                    break (End::Replaced(Box::new(newer)), close_code::NORMAL, "replaced by a newer session".into());
                }
                msg = s.control_rx.recv() => match msg {
                    Ok(Some(m)) => match self.on_control(&mut s, m, &mut pump).await {
                        Ok(true) => {}
                        Ok(false) => break (End::Closed, close_code::NORMAL, "client said bye".into()),
                        Err(e) => break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string()),
                    },
                    Ok(None) => break (End::Closed, close_code::NORMAL, "control stream finished".into()),
                    Err(e) => break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string()),
                },
                st = streams.accept(), if accepted < 2 => match st {
                    Ok(PeerStream::Input(r)) => (accepted, input) = (accepted + 1, Some(r)),
                    Ok(PeerStream::Telemetry(r)) => {
                        (accepted, client_telemetry) = (accepted + 1, Some(r));
                    }
                    Err(e) => break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string()),
                },
                // shortcut: input is read and discarded until injection (M1-10), client
                // telemetry until the HIL harness merges it (M1-11); reading keeps flow-control
                // credit moving (section 8).
                m = drain(&mut input) => if let Err(e) = m {
                    break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string());
                },
                m = drain(&mut client_telemetry) => if let Err(e) = m {
                    break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string());
                },
                frame = next_frame(pump.as_ref()) => {
                    // `next_frame` only yields while a pump runs.
                    if let Some(p) = &pump
                        && let Err(e) = self.send(&s, session_no, &mut fragmenter, p, frame)
                    {
                        break (End::Closed, close_code::NORMAL, e.to_string());
                    }
                }
            }
        };
        s.close(code);
        drop(streams);
        // Joining the source thread blocks up to `source::MAX_BLOCK`: not on an async worker.
        let _ = tokio::task::spawn_blocking(move || drop(pump)).await;
        log(format_args!("session with {} ended: {why}", s.peer));
        end
    }

    /// Handles one control message. `Ok(false)`: the client closed the session.
    async fn on_control(
        &self,
        s: &mut HostSession,
        msg: ControlMessage,
        pump: &mut Option<Pump>,
    ) -> Result<bool, Error> {
        match msg {
            ControlMessage::Capabilities(caps) => {
                // shortcut: a later Capabilities (mode change) is ignored until the virtual
                // display can follow it (M1-8).
                if pump.is_none() {
                    *pump = Some(self.start_stream(s, caps).await?);
                }
            }
            ControlMessage::KeyframeRequest(_) => {
                if let Some(p) = pump {
                    p.request_keyframe();
                }
            }
            ControlMessage::ClockSyncRequest(r) => {
                let host_recv_us = self.clock.now_us();
                let resp = ClockSyncResponse {
                    seq: r.seq,
                    client_send_us: r.client_send_us,
                    host_recv_us,
                    host_send_us: self.clock.now_us(),
                };
                send_control(s, &ControlMessage::ClockSyncResponse(resp)).await?;
            }
            ControlMessage::Bye(_) => return Ok(false),
            _ => {
                return Err(Error::Protocol(
                    "unexpected control message from the client",
                ));
            }
        }
        Ok(true)
    }

    async fn start_stream(&self, s: &mut HostSession, caps: Capabilities) -> Result<Pump, Error> {
        let d = caps
            .display
            .ok_or(Error::Protocol("client Capabilities without a display"))?;
        if d.refresh_mhz < MIN_REFRESH_MHZ {
            return Err(Error::Protocol("client refresh rate below 10 Hz"));
        }
        let configure = Configure {
            // shortcut: HEVC Main as on the reference hardware; negotiation is M1-7.
            profile: Profile::HevcMain,
            width: d.width,
            height: d.height,
            refresh_mhz: d.refresh_mhz,
            bitrate_kbps: self.cfg.bitrate_kbps,
        };
        send_control(s, &ControlMessage::Configure(configure)).await?;
        let source: Box<dyn VideoSource> = match self.cfg.pattern {
            TestPattern::MovingBars => {
                Box::new(MovingBars::new(d.refresh_mhz, self.cfg.bitrate_kbps))
            }
        };
        Ok(Pump::start(source, self.clock)?)
    }

    /// The per-frame path: fragment, queue, record. Allocates nothing in steady state. `Err`
    /// ends the session.
    fn send(
        &self,
        s: &HostSession,
        session: u32,
        fragmenter: &mut Fragmenter,
        pump: &Pump,
        frame: EncodedFrame,
    ) -> Result<(), Error> {
        let res = s.send_frame(fragmenter, frame.info, &frame.data, FEC_PERCENT);
        let send_us = self.clock.now_us();
        let out = match res {
            Ok(datagrams) => {
                // shortcut: host frame times go to the JSONL only; sending them on the
                // telemetry stream (`TelemetryMessage::HostFrame`) would put an awaited stream
                // write in this loop, so it waits for the HIL harness (M1-11).
                self.telemetry.record(FrameRecord {
                    session,
                    frame_id: frame.info.frame_id,
                    keyframe: frame.meta.keyframe,
                    capture_us: frame.meta.capture_us,
                    encode_start_us: frame.meta.encode_start_us,
                    encode_end_us: frame.meta.encode_end_us,
                    send_us,
                    encoded_bytes: u32::try_from(frame.data.len()).unwrap_or(u32::MAX),
                    datagrams: u32::try_from(datagrams).unwrap_or(u32::MAX),
                });
                Ok(())
            }
            // Only this frame is lost (e.g. the path MTU shrank): restart the chain.
            Err(Error::Datagram(_) | Error::Fragment(_)) => {
                pump.request_keyframe();
                Ok(())
            }
            Err(e) => Err(e),
        };
        pump.mailbox.recycle(frame.data);
        out
    }
}

/// Queues a control reply, giving up after [`CONTROL_SEND_TIMEOUT`] (the caller then closes the
/// session with a protocol error), so a client that stops reading control cannot freeze the
/// session loop.
async fn send_control(s: &mut HostSession, msg: &ControlMessage) -> Result<(), Error> {
    tokio::time::timeout(CONTROL_SEND_TIMEOUT, s.control_tx.send(msg))
        .await
        .unwrap_or(Err(Error::Stream(
            "control reply timed out: client not reading",
        )))
}

async fn next_frame(pump: Option<&Pump>) -> EncodedFrame {
    match pump {
        Some(p) => p.mailbox.take().await,
        None => std::future::pending().await,
    }
}

/// Reads and discards one message; a finished stream is set to `None` and never polled again.
async fn drain<M: vetro_protocol::messages::StreamMessage>(
    rx: &mut Option<MsgReceiver<M>>,
) -> Result<(), Error> {
    let Some(r) = rx else {
        return std::future::pending().await;
    };
    if r.recv().await?.is_none() {
        *rx = None;
    }
    Ok(())
}
