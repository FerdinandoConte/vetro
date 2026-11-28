//! Vetro host engine (ARCHITECTURE section 5, ADR-0011).
//!
//! M1-4 skeleton: command line, identity path, one session at a time, a synthetic
//! [`source::VideoSource`] on its own thread, the per-frame send path and JSONL telemetry.
//! Capture (M1-9), the encoder (M1-7), the virtual display (M1-8), input injection (M1-10) and
//! the tray IPC (M1-12) slot in later. `--test-pattern gpu-bars` already runs the real encoder
//! (M1-7): bars drawn on the GPU, encoded zero-copy by the GPU's FFmpeg hardware encoder.
//!
//! Session flow (section 7): the client's `Capabilities` (with its display mode) starts the
//! stream: the engine negotiates the codec (AV1 > HEVC > H.264), answers `Configure` and starts
//! the source; the client's `LinkFeedback` drives the encoder bitrate and the FEC share
//! (`vetro_encode::rate`); `KeyframeRequest` forces the
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
use std::time::Duration;
use telemetry::{FrameRecord, Telemetry};
use tokio::sync::mpsc;
use vetro_encode::backend;
use vetro_encode::rate::RateController;
use vetro_protocol::Fragmenter;
use vetro_protocol::messages::{
    Capabilities, ClockSyncResponse, Configure, ControlMessage, PairToken, Profile,
    TelemetryMessage,
};
use vetro_transport::{
    Error, Fingerprint, Host, HostSession, Identity, MsgReceiver, PeerStream, close_code,
};

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

/// A running stream: the source thread and the rate controller that retargets it.
struct Stream {
    pump: Pump,
    rate: RateController,
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
        let mut stream: Option<Stream> = None;
        let mut fragmenter = Fragmenter::new();

        let (end, code, why) = loop {
            tokio::select! {
                () = &mut *shutdown => break (End::Shutdown, close_code::NORMAL, "engine stopping".into()),
                Some(newer) = sessions.recv() => {
                    break (End::Replaced(Box::new(newer)), close_code::NORMAL, "replaced by a newer session".into());
                }
                msg = s.control_rx.recv() => match msg {
                    Ok(Some(m)) => match self.on_control(&mut s, m, &mut stream).await {
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
                // shortcut: input is read and discarded until injection (M1-10); reading keeps
                // flow-control credit moving (section 8).
                m = next_msg(&mut input) => if let Err(e) = m {
                    break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string());
                },
                // Client frame times wait for the HIL harness (M1-11); link reports drive
                // rate control.
                m = next_msg(&mut client_telemetry) => match m {
                    Ok(Some(TelemetryMessage::LinkFeedback(fb))) => {
                        if let Some(st) = &mut stream {
                            let now = self.clock.now_us();
                            if let Some(kbps) = st.rate.on_feedback(&fb, now).bitrate_kbps {
                                st.pump.set_bitrate(kbps);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => break (End::Closed, close_code::PROTOCOL_ERROR, e.to_string()),
                },
                frame = next_frame(stream.as_ref().map(|st| &st.pump)) => {
                    // `next_frame` only yields while a stream runs.
                    if let Some(st) = &stream
                        && let Err(e) = self.send(&s, session_no, &mut fragmenter, st, frame)
                    {
                        break (End::Closed, close_code::NORMAL, e.to_string());
                    }
                }
            }
        };
        s.close(code);
        drop(streams);
        // Joining the source thread blocks up to `source::MAX_BLOCK`: not on an async worker.
        let _ = tokio::task::spawn_blocking(move || drop(stream)).await;
        log(format_args!("session with {} ended: {why}", s.peer));
        end
    }

    /// Handles one control message. `Ok(false)`: the client closed the session.
    async fn on_control(
        &self,
        s: &mut HostSession,
        msg: ControlMessage,
        stream: &mut Option<Stream>,
    ) -> Result<bool, Error> {
        match msg {
            ControlMessage::Capabilities(caps) => {
                // shortcut: a later Capabilities (mode change) is ignored until the virtual
                // display can follow it (M1-8).
                if stream.is_none() {
                    *stream = Some(self.start_stream(s, caps).await?);
                }
            }
            ControlMessage::KeyframeRequest(_) => {
                if let Some(st) = stream {
                    st.pump.request_keyframe();
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

    /// Negotiates the codec, opens the source, answers `Configure` and starts the pump.
    async fn start_stream(&self, s: &mut HostSession, caps: Capabilities) -> Result<Stream, Error> {
        let d = caps
            .display
            .ok_or(Error::Protocol("client Capabilities without a display"))?;
        if d.refresh_mhz < MIN_REFRESH_MHZ {
            return Err(Error::Protocol("client refresh rate below 10 Hz"));
        }
        // The host's list is known only by opening encoders: candidates are the client's
        // profiles at this mode, best first (ADR-0005).
        let candidates = backend::candidates(None, &caps.codecs, d.width, d.height, d.refresh_mhz);
        let (pattern, kbps) = (self.cfg.pattern, self.cfg.bitrate_kbps);
        let (w, h, hz) = (d.width, d.height, d.refresh_mhz);
        // Opening a hardware encoder takes a few hundred ms: off the async workers.
        let (source, profile) =
            tokio::task::spawn_blocking(move || open_source(pattern, &candidates, w, h, hz, kbps))
                .await
                .map_err(|_| Error::Protocol("source setup panicked"))??;
        if backend::is_experimental(profile) {
            log(format_args!("streaming {profile:?} (experimental)"));
        }
        let configure = Configure {
            profile,
            width: d.width,
            height: d.height,
            refresh_mhz: d.refresh_mhz,
            bitrate_kbps: kbps,
        };
        send_control(s, &ControlMessage::Configure(configure)).await?;
        Ok(Stream {
            pump: Pump::start(source, self.clock)?,
            rate: RateController::new(kbps),
        })
    }

    /// The per-frame path: fragment, queue, record. Allocates nothing in steady state. `Err`
    /// ends the session.
    fn send(
        &self,
        s: &HostSession,
        session: u32,
        fragmenter: &mut Fragmenter,
        stream: &Stream,
        frame: EncodedFrame,
    ) -> Result<(), Error> {
        let (pump, fec_percent) = (&stream.pump, stream.rate.fec_percent());
        let res = s.send_frame(fragmenter, frame.info, &frame.data, fec_percent);
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
                    bitrate_kbps: frame.meta.bitrate_kbps,
                    fec_percent,
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

/// Reads one message; a finished stream is set to `None` and never polled again.
async fn next_msg<M: vetro_protocol::messages::StreamMessage>(
    rx: &mut Option<MsgReceiver<M>>,
) -> Result<Option<M>, Error> {
    let Some(r) = rx else {
        return std::future::pending().await;
    };
    let m = r.recv().await?;
    if m.is_none() {
        *rx = None;
    }
    Ok(m)
}

/// The session's source and the profile it encodes. `candidates` is best first.
fn open_source(
    pattern: TestPattern,
    candidates: &[Profile],
    width: u32,
    height: u32,
    refresh_mhz: u32,
    bitrate_kbps: u32,
) -> Result<(Box<dyn VideoSource>, Profile), Error> {
    let &best = candidates
        .first()
        .ok_or(Error::Protocol("no codec in common with the client"))?;
    match pattern {
        // No codec: the bytes are opaque, any profile will do.
        TestPattern::MovingBars => Ok((Box::new(MovingBars::new(refresh_mhz, bitrate_kbps)), best)),
        #[cfg(windows)]
        TestPattern::GpuBars => gpu_bars(candidates, width, height, refresh_mhz, bitrate_kbps),
        #[cfg(not(windows))]
        TestPattern::GpuBars => {
            let _ = (width, height);
            Err(Error::Protocol("gpu-bars needs Windows"))
        }
    }
}

/// GPU bars into the render adapter's hardware encoder: the first candidate that opens.
/// shortcut: the default hardware adapter; the VDD adapter's LUID comes with M1-8.
#[cfg(windows)]
fn gpu_bars(
    candidates: &[Profile],
    width: u32,
    height: u32,
    refresh_mhz: u32,
    bitrate_kbps: u32,
) -> Result<(Box<dyn VideoSource>, Profile), Error> {
    use vetro_encode::Settings;
    use vetro_encode::d3d11::{Bars, Device};
    use vetro_encode::ffmpeg::{FfmpegEncoder, Surface};

    let fail = |e: vetro_encode::Error| {
        log(format_args!("encoder: {e}"));
        Error::Protocol("no hardware encoder for the negotiated codecs")
    };
    let device = Device::open(None).map_err(fail)?;
    let bars = Bars::new(&device, width, height).map_err(fail)?;
    let mut last = vetro_encode::Error("no candidates".into());
    for &profile in candidates {
        let settings = Settings {
            profile,
            width,
            height,
            refresh_mhz,
            bitrate_kbps,
        };
        match FfmpegEncoder::open(&device, &settings) {
            Ok(enc) => {
                log(format_args!(
                    "encoding {profile:?} with {:?} on {}",
                    enc.backend(),
                    device.adapter
                ));
                let dev = device.clone();
                let paint: source::Painter<Surface> =
                    Box::new(move |s, seq| bars.draw(&dev, &s.texture, s.index, seq));
                let src = source::Encoded::new(enc, paint, refresh_mhz);
                return Ok((Box::new(src), profile));
            }
            Err(e) => last = e,
        }
    }
    Err(fail(last))
}
