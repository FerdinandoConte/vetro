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
/// shortcut: fixed 10 %; adaptive 0–25 % from the client's measured loss comes with rate
/// control (M1-7), 0 % on USB with M1-14.
const FEC_PERCENT: u8 = 10;

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
    /// shortcut: `a=` is the bound address; listing every interface address comes with the
    /// tray QR (M1-12) and mDNS (M1-13).
    pub fn pair_uri(&self, token: &PairToken) -> Result<String, Error> {
        let addr = self.local_addr()?;
        let t: String = token.0.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!(
            "vetro://pair?fp={}&t={t}&a={}&p={}",
            self.fingerprint,
            addr.ip(),
            addr.port()
        ))
    }

    /// Serves sessions until `shutdown` resolves, then closes the endpoint and flushes
    /// telemetry.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<(), Error> {
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
        loop {
            let session = match next.take() {
                Some(s) => s,
                None => tokio::select! {
                    () = &mut shutdown => break,
                    Some(s) = sessions.recv() => s,
                },
            };
            match self.serve(session, &mut sessions, &mut shutdown).await {
                End::Replaced(s) => next = Some(*s),
                End::Closed => {}
                End::Shutdown => break,
            }
        }

        acceptor.abort();
        self.host.close();
        let dropped = self.telemetry.dropped();
        if dropped > 0 {
            log(format_args!("{dropped} telemetry records dropped"));
        }
        self.telemetry.finish()?;
        log(format_args!("stopped"));
        Ok(())
    }

    async fn serve<F: Future<Output = ()>>(
        &self,
        mut s: HostSession,
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
                        && let Err(e) = self.send(&s, &mut fragmenter, p, frame)
                    {
                        break (End::Closed, close_code::NORMAL, e.to_string());
                    }
                }
            }
        };
        s.close(code);
        drop(pump); // joins the source thread (at most one frame interval)
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
                s.control_tx
                    .send(&ControlMessage::ClockSyncResponse(resp))
                    .await?;
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
        let configure = Configure {
            // shortcut: HEVC Main as on the reference hardware; negotiation is M1-7.
            profile: Profile::HevcMain,
            width: d.width,
            height: d.height,
            refresh_mhz: d.refresh_mhz,
            bitrate_kbps: self.cfg.bitrate_kbps,
        };
        s.control_tx
            .send(&ControlMessage::Configure(configure))
            .await?;
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
        fragmenter: &mut Fragmenter,
        pump: &Pump,
        frame: EncodedFrame,
    ) -> Result<(), Error> {
        let res = s.send_frame(fragmenter, frame.info, &frame.data, FEC_PERCENT);
        let send_us = self.clock.now_us();
        let out = match res {
            Ok(datagrams) => {
                self.telemetry.record(FrameRecord {
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
