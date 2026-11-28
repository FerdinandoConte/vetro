//! One client session: connect, `Capabilities`, wait for `Configure`, then receive, reassemble
//! and decode video, request keyframes on loss, keep the clock aligned and report telemetry
//! (ARCHITECTURE sections 6.2, 7, 11).
//!
//! Everything runs in one task. Stream writes go through small bounded queues to writer futures,
//! so a stalled stream never holds up the datagram path (section 10).

use crate::clock::{CLOCK_SYNC_INTERVAL, Clock, ClockSync};
use crate::decoder::{Decoder, DecoderError, DecoderEvent, DecoderEvents};
use crate::telemetry::{FrameTimes, Stats};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use vetro_protocol::datagram::{Datagram, decode_datagram};
use vetro_protocol::messages::{
    ByeReason, Capabilities, CodecSupport, Configure, ControlMessage, Display, KeyframeRequest,
    MAX_CODECS, PairToken, StreamMessage, TelemetryMessage,
};
use vetro_protocol::{PushOutcome, Reassembler, ReassemblerConfig};
use vetro_transport::{Client, ClientLink, ClientSession, Fingerprint, MsgSender, close_code};

/// What the session needs to know about the host and this device.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: SocketAddr,
    /// From the pairing QR; the only certificate accepted.
    pub host_fingerprint: Fingerprint,
    /// First connection only: the QR's one-time token.
    pub pair_token: Option<PairToken>,
    /// The panel's native mode, sent in `Capabilities`.
    pub display: Display,
    /// The clock telemetry and decoder events are measured on.
    pub clock: Clock,
}

/// How a session ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The caller stopped it.
    Stopped,
    /// The host said `Bye`.
    Bye(ByeReason),
}

#[derive(Debug)]
pub enum Error {
    /// The host refused the pairing (close code 2): missing, wrong, expired or used token. Also
    /// reported when the connection closes with code 2 right after a connect that looked fine,
    /// which happens when two devices race for one token (the host checks it again at the end
    /// of its setup).
    NotPaired,
    Transport(vetro_transport::Error),
    Decoder(DecoderError),
    Protocol(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotPaired => f.write_str("pairing refused by the host"),
            Self::Transport(e) => e.fmt(f),
            Self::Decoder(e) => e.fmt(f),
            Self::Protocol(s) => write!(f, "protocol violation: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<vetro_transport::Error> for Error {
    fn from(e: vetro_transport::Error) -> Self {
        match e {
            vetro_transport::Error::NotPaired => Self::NotPaired,
            e => Self::Transport(e),
        }
    }
}

/// Control messages waiting for the stream (keyframe and clock-sync requests). Full only when
/// the control stream has stalled; a request that does not fit is retried on the next loss.
const CONTROL_QUEUE: usize = 16;
/// Telemetry messages waiting for the stream (one per frame); dropped when full.
const TELEMETRY_QUEUE: usize = 64;

/// Runs one session until `stop` resolves (`Ok(End::Stopped)`, connection closed with code 0),
/// the host says `Bye`, or something fails. Updates `stats` as it goes.
pub async fn run<D: Decoder>(
    client: &Client,
    cfg: &Config,
    decoder: &mut D,
    stats: &Stats,
    stop: impl Future<Output = ()>,
) -> Result<End, Error> {
    let session = client
        .connect(cfg.host, cfg.host_fingerprint, cfg.pair_token.clone())
        .await?;
    let link = session.link();
    let ClientSession {
        mut control_tx,
        mut control_rx,
        // Kept open for the input path (M1-10).
        input_tx: _input_tx,
        telemetry_tx,
        ..
    } = session;

    let d = &cfg.display;
    let codecs = decoder
        .profiles()
        .into_iter()
        .take(MAX_CODECS)
        .map(|profile| CodecSupport {
            profile,
            max_width: d.width,
            max_height: d.height,
            max_refresh_mhz: d.refresh_mhz,
        })
        .collect();
    control_tx
        .send(&ControlMessage::Capabilities(Capabilities {
            display: Some(d.clone()),
            codecs,
        }))
        .await?;

    let (ctl, ctl_rx) = mpsc::channel(CONTROL_QUEUE);
    let (tel, tel_rx) = mpsc::channel(TELEMETRY_QUEUE);
    let background = async {
        tokio::join!(
            forward(control_tx, ctl_rx),
            forward(telemetry_tx, tel_rx),
            drain_host_telemetry(&link),
        );
        // A writer only stops when its stream fails; the main loop sees the connection end.
        std::future::pending().await
    };

    let main = async {
        let clock = cfg.clock;
        let (events, mut events_rx) = DecoderEvents::new(clock);
        let mut sync = ClockSync::new();
        let mut times = FrameTimes::default();
        let mut video: Option<Video> = None;
        // A video fragment that came before `Configure` (not expected: the host configures
        // first, but datagrams can overtake the control stream). It may have been the keyframe.
        let mut early: Option<u32> = None;
        let mut sync_tick = tokio::time::interval(CLOCK_SYNC_INTERVAL);
        // Replaced by one frame interval on `Configure`.
        let mut expire_tick = tokio::time::interval(Duration::from_secs(1));
        let request = |id: u32| {
            let msg = ControlMessage::KeyframeRequest(KeyframeRequest { lost_frame_id: id });
            let sent = ctl.try_send(msg).is_ok();
            if sent {
                stats.keyframe_request();
            }
            sent
        };
        tokio::pin!(stop);

        loop {
            tokio::select! {
                () = &mut stop => {
                    link.close(close_code::NORMAL);
                    return Ok(End::Stopped);
                }
                msg = control_rx.recv() => match msg? {
                    Some(ControlMessage::Configure(c)) => {
                        decoder.configure(&c, events.clone()).map_err(Error::Decoder)?;
                        let v = Video::new(&c);
                        expire_tick = tokio::time::interval(Duration::from_micros(v.interval_us));
                        expire_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
                        video = Some(v);
                        stats.set_streaming();
                        if let Some(id) = early.take() {
                            request(id);
                        }
                    }
                    Some(ControlMessage::ClockSyncResponse(r)) => {
                        if let Some(e) = sync.on_response(&r, clock.now_us()) {
                            stats.set_clock(e);
                        }
                    }
                    Some(ControlMessage::Bye(b)) => return Ok(End::Bye(b.reason)),
                    // Host capabilities are informational; cursor shapes arrive with M1-9.
                    Some(_) => {}
                    None => return Err(Error::Protocol("host ended the control stream")),
                },
                dgram = link.read_datagram() => {
                    let dgram = dgram?;
                    let (header, payload) = match decode_datagram(&dgram) {
                        Ok(Datagram::Video { header, payload }) => (header, payload),
                        Ok(Datagram::Cursor(_)) => continue, // Cursor overlay: M1-9.
                        Err(_) => {
                            stats.bad_datagram();
                            continue;
                        }
                    };
                    let Some(v) = &mut video else {
                        early = Some(header.frame_id);
                        continue;
                    };
                    let window_us = v.window_us(&link);
                    let received_us = clock.now_us();
                    let mut dropped = None;
                    match v.reassembler.push(received_us, &header, payload) {
                        Ok(PushOutcome::Complete(frame)) => {
                            stats.frame_complete(frame.fec_recovered);
                            let id = frame.info.frame_id;
                            let decode_in_us = clock.now_us();
                            if decoder.decode(&frame.info, frame.data).map_err(Error::Decoder)? {
                                times.queued(id, received_us, decode_in_us, frame.fec_recovered);
                            } else {
                                stats.decoder_drop();
                                dropped = Some(id);
                            }
                        }
                        Ok(PushOutcome::Pending | PushOutcome::Ignored) => {}
                        Err(_) => stats.bad_datagram(),
                    }
                    if let Some(id) = dropped
                        && v.limiter.allow(received_us, window_us, false)
                    {
                        request(id);
                    }
                    v.check_loss(Some(header.frame_id), received_us, window_us, stats, &request);
                    // During a datagram burst this arm stays ready and the other arms may not
                    // get a turn (tokio's cooperative budget runs out), so take decoder events
                    // here too; otherwise their frames' records are overwritten unreported.
                    while let Ok(e) = events_rx.try_recv() {
                        on_event(e, &mut times, stats, &tel)?;
                    }
                }
                Some(e) = events_rx.recv() => on_event(e, &mut times, stats, &tel)?,
                _ = expire_tick.tick() => {
                    if let Some(v) = &mut video {
                        let now_us = clock.now_us();
                        v.reassembler.expire(now_us);
                        let window_us = v.window_us(&link);
                        v.check_loss(None, now_us, window_us, stats, &request);
                    }
                }
                _ = sync_tick.tick() => {
                    let _ = ctl.try_send(ControlMessage::ClockSyncRequest(sync.request(clock.now_us())));
                }
            }
        }
    };

    tokio::select! {
        r = main => r,
        r = background => r,
    }
}

/// Records a decoder event; a finished frame record goes to the telemetry queue (dropped when
/// full).
fn on_event(
    e: DecoderEvent,
    times: &mut FrameTimes,
    stats: &Stats,
    tel: &mpsc::Sender<TelemetryMessage>,
) -> Result<(), Error> {
    let done = match e {
        DecoderEvent::Decoded {
            frame_id,
            at_us,
            render_pending,
        } => {
            stats.frame_decoded();
            times.decoded(frame_id, at_us, render_pending)
        }
        DecoderEvent::Rendered { frame_id, at_us } => times.rendered(frame_id, at_us),
        DecoderEvent::Error(e) => return Err(Error::Decoder(e)),
    };
    if let Some(t) = done {
        let _ = tel.try_send(TelemetryMessage::ClientFrame(t));
    }
    Ok(())
}

/// Reassembly state of the configured stream.
struct Video {
    reassembler: Reassembler,
    interval_us: u64,
    seen_lost: u64,
    seen_restarts: u64,
    limiter: KeyframeLimiter,
}

impl Video {
    fn new(c: &Configure) -> Self {
        let cfg = ReassemblerConfig::for_refresh(c.refresh_mhz);
        Self {
            interval_us: cfg.frame_timeout_us.max(1),
            reassembler: Reassembler::new(cfg),
            seen_lost: 0,
            seen_restarts: 0,
            limiter: KeyframeLimiter::default(),
        }
    }

    /// Keyframe requests are suppressed for one RTT (at least one frame interval) after the
    /// previous one: a frame lost in that time left the host before the request arrived, so the
    /// keyframe that answers it repairs that loss too.
    fn window_us(&self, link: &ClientLink) -> u64 {
        let rtt = u64::try_from(link.rtt().as_micros()).unwrap_or(u64::MAX);
        rtt.max(self.interval_us)
    }

    /// Requests a keyframe when the reassembler lost a frame (rate-limited) or saw the host's
    /// numbering restart (always). `trigger` is the frame id just pushed; a restart is only ever
    /// detected on a push.
    fn check_loss(
        &mut self,
        trigger: Option<u32>,
        now_us: u64,
        window_us: u64,
        stats: &Stats,
        request: &impl Fn(u32) -> bool,
    ) {
        let r = &self.reassembler;
        let (lost, restarts) = (r.lost_frames(), r.stream_restarts());
        if lost == self.seen_lost && restarts == self.seen_restarts {
            return;
        }
        stats.set_lost(lost, restarts);
        let restarted = restarts != self.seen_restarts;
        // After a restart the reassembler forgets its lost ids (`last_lost_frame` is `None`):
        // everything before the first frame of the new numbering is lost.
        let id = if restarted {
            trigger.map(|t| t.wrapping_sub(1))
        } else {
            r.last_lost_frame()
        };
        if let Some(id) = id
            && self.limiter.allow(now_us, window_us, restarted)
            && !request(id)
        {
            return; // Control queue full: try again on the next check.
        }
        // Sent, or suppressed because the pending keyframe covers this loss.
        self.seen_lost = lost;
        self.seen_restarts = restarts;
    }
}

/// At most one keyframe request per window, except when forced.
#[derive(Debug, Default)]
struct KeyframeLimiter {
    last_us: Option<u64>,
}

impl KeyframeLimiter {
    fn allow(&mut self, now_us: u64, window_us: u64, force: bool) -> bool {
        let ok = force
            || self
                .last_us
                .is_none_or(|t| now_us.saturating_sub(t) >= window_us);
        if ok {
            self.last_us = Some(now_us);
        }
        ok
    }
}

/// Writes queued messages to a stream until the queue closes or the stream fails.
async fn forward<M: StreamMessage>(mut tx: MsgSender<M>, mut rx: mpsc::Receiver<M>) {
    while let Some(m) = rx.recv().await {
        if tx.send(&m).await.is_err() {
            break;
        }
    }
}

/// Reads and discards the host's per-frame telemetry (the HUD and HIL use it later, M1-6 and
/// M1-11). It must be read: an unread stream holds connection flow-control credit.
async fn drain_host_telemetry(link: &ClientLink) {
    if let Ok(mut rx) = link.accept_telemetry().await {
        while let Ok(Some(_)) = rx.recv().await {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_requests_are_rate_limited_unless_forced() {
        let mut l = KeyframeLimiter::default();
        assert!(l.allow(1_000, 500, false), "first request");
        assert!(!l.allow(1_400, 500, false), "inside the window");
        assert!(l.allow(1_400, 500, true), "a restart is always sent");
        assert!(
            !l.allow(1_899, 500, false),
            "the forced one restarted the window"
        );
        assert!(l.allow(1_900, 500, false));
    }
}
