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
use vetro_protocol::datagram::{Datagram, FragmentHeader, FrameFlags, FrameInfo, decode_datagram};
use vetro_protocol::messages::{
    ByeReason, Capabilities, CodecSupport, Configure, ControlMessage, Display, KeyframeRequest,
    MAX_CODECS, PairToken, Profile, StreamMessage, TelemetryMessage,
};
use vetro_protocol::{PushOutcome, Reassembler, ReassemblerConfig};
use vetro_transport::{Client, ClientLink, MsgSender, close_code};

/// What the session needs to know about the host and this device.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: SocketAddr,
    /// From the pairing QR; the only certificate accepted.
    pub host_fingerprint: vetro_transport::Fingerprint,
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
    /// The host broke the protocol; the client closes with code 1.
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
/// the control stream has stalled; a keyframe request that does not fit is retried.
const CONTROL_QUEUE: usize = 16;
/// Telemetry messages waiting for the stream (one per frame); dropped when full.
const TELEMETRY_QUEUE: usize = 64;

/// Runs one session until `stop` resolves (`Ok(End::Stopped)`), the host says `Bye`, or
/// something fails. `stop` is honoured at every point, also while connecting. Updates `stats`
/// as it goes. On return the connection is closed: code 1 after [`Error::Protocol`], else 0.
/// Before dropping the runtime, give [`Client::wait_idle`] a moment so the close packet leaves.
pub async fn run<D: Decoder>(
    client: &Client,
    cfg: &Config,
    decoder: &mut D,
    stats: &Stats,
    stop: impl Future<Output = ()>,
) -> Result<End, Error> {
    tokio::pin!(stop);
    let setup = async {
        let mut session = client
            .connect(cfg.host, cfg.host_fingerprint, cfg.pair_token.clone())
            .await?;
        let caps = capabilities(&cfg.display, decoder.profiles());
        session.control_tx.send(&caps).await?;
        Ok::<_, Error>(session)
    };
    let session = tokio::select! {
        s = setup => s?,
        () = &mut stop => return Ok(End::Stopped),
    };
    let link = session.link();
    let vetro_transport::ClientSession {
        control_tx,
        mut control_rx,
        // Kept open for the input path (M1-10).
        input_tx: _input_tx,
        telemetry_tx,
        ..
    } = session;

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
        sync_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
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
        let rtt_us = || u64::try_from(link.rtt().as_micros()).unwrap_or(u64::MAX);

        loop {
            tokio::select! {
                () = &mut stop => return Ok(End::Stopped),
                msg = control_rx.recv() => match msg? {
                    Some(ControlMessage::Configure(c)) => {
                        decoder.configure(&c, events.clone()).map_err(Error::Decoder)?;
                        let mut v = Video::new(&c);
                        expire_tick = tokio::time::interval(Duration::from_micros(v.keys.interval_us));
                        expire_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
                        if let Some(id) = early.take() {
                            v.keys.want(id, false);
                        }
                        v.keys.flush(clock.now_us(), rtt_us(), &request);
                        video = Some(v);
                        stats.set_streaming();
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
                    if let Some((header, payload)) = video_fragment(&dgram, stats) {
                        match &mut video {
                            None => early = Some(header.frame_id),
                            Some(v) => {
                                let received_us = clock.now_us();
                                let mut completed = None;
                                match v.reassembler.push(received_us, &header, payload) {
                                    Ok(PushOutcome::Complete(frame)) => {
                                        stats.frame_complete(frame.fec_recovered);
                                        let info = frame.info;
                                        completed = Some(info);
                                        let decode_in_us = clock.now_us();
                                        if decoder.decode(&info, frame.data).map_err(Error::Decoder)? {
                                            times.queued(info.frame_id, received_us, decode_in_us, frame.fec_recovered);
                                        } else {
                                            stats.decoder_drop();
                                            v.keys.want(info.frame_id, false);
                                        }
                                    }
                                    Ok(PushOutcome::Pending | PushOutcome::Ignored) => {}
                                    Err(_) => stats.bad_datagram(),
                                }
                                // A restart is only detected on a push; it must be seen before
                                // the keyframe check of a frame that completed in that push.
                                v.keys.observe(&v.reassembler, Some(header.frame_id), stats);
                                if let Some(info) = completed {
                                    v.keys.completed(&info);
                                }
                                v.keys.flush(received_us, rtt_us(), &request);
                            }
                        }
                    }
                    // During a datagram burst this arm stays ready, so take decoder events here
                    // too (otherwise their frames' records are overwritten unreported), and give
                    // the budget back so the stream writers and quinn's driver get to run.
                    while let Ok(e) = events_rx.try_recv() {
                        on_event(e, &mut times, stats, &tel)?;
                    }
                    tokio::task::coop::consume_budget().await;
                }
                Some(e) = events_rx.recv() => on_event(e, &mut times, stats, &tel)?,
                _ = expire_tick.tick() => {
                    if let Some(v) = &mut video {
                        let now_us = clock.now_us();
                        v.reassembler.expire(now_us);
                        v.keys.observe(&v.reassembler, None, stats);
                        v.keys.flush(now_us, rtt_us(), &request);
                    }
                }
                _ = sync_tick.tick() => {
                    let _ = ctl.try_send(ControlMessage::ClockSyncRequest(sync.request(clock.now_us())));
                }
            }
        }
    };

    let r = tokio::select! {
        r = main => r,
        r = background => r,
    };
    link.close(match r {
        Err(Error::Protocol(_)) => close_code::PROTOCOL_ERROR,
        _ => close_code::NORMAL,
    });
    r
}

fn capabilities(d: &Display, profiles: Vec<Profile>) -> ControlMessage {
    let codecs = profiles
        .into_iter()
        .take(MAX_CODECS)
        .map(|profile| CodecSupport {
            profile,
            max_width: d.width,
            max_height: d.height,
            max_refresh_mhz: d.refresh_mhz,
        })
        .collect();
    ControlMessage::Capabilities(Capabilities {
        display: Some(d.clone()),
        codecs,
    })
}

/// The video fragment in `dgram`, if it is one. Cursor positions (M1-9) are skipped; anything
/// that does not decode is a host bug, counted and discarded.
fn video_fragment<'a>(dgram: &'a [u8], stats: &Stats) -> Option<(FragmentHeader, &'a [u8])> {
    match decode_datagram(dgram) {
        Ok(Datagram::Video { header, payload }) => Some((header, payload)),
        Ok(Datagram::Cursor(_)) => None,
        Err(_) => {
            stats.bad_datagram();
            None
        }
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
    keys: Keys,
}

impl Video {
    fn new(c: &Configure) -> Self {
        let cfg = ReassemblerConfig::for_refresh(c.refresh_mhz);
        Self {
            keys: Keys::new(cfg.frame_timeout_us.max(1)),
            reassembler: Reassembler::new(cfg),
        }
    }
}

/// When to ask the host for a keyframe.
///
/// A request is wanted when the reassembler lost a frame (id = `last_lost_frame()`), when the
/// host's numbering restarted (id = first new frame − 1, since `last_lost_frame()` is `None`
/// then; never rate-limited), when the decoder dropped a frame (its id), and when the first
/// complete frame after `Configure` or a restart is not a keyframe (its id − 1: the keyframe
/// before it never arrived).
///
/// Requests are suppressed for one RTT (at least one frame interval) after the previous one: a
/// frame lost in that time left the host before the request arrived, so the keyframe that
/// answers it repairs that loss too. A request that does not fit the control queue stays wanted
/// and is retried on the next check.
struct Keys {
    interval_us: u64,
    seen_lost: u64,
    seen_restarts: u64,
    /// No complete frame yet since `Configure` or the last restart.
    need_key: bool,
    want: Option<Want>,
    last_sent_us: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Want {
    id: u32,
    /// Ignores the rate limit (numbering restart).
    force: bool,
}

impl Keys {
    fn new(interval_us: u64) -> Self {
        Self {
            interval_us,
            seen_lost: 0,
            seen_restarts: 0,
            need_key: true,
            want: None,
            last_sent_us: None,
        }
    }

    /// Wants a request for `id`; the newest id wins, `force` sticks.
    fn want(&mut self, id: u32, force: bool) {
        let force = force || self.want.is_some_and(|w| w.force);
        self.want = Some(Want { id, force });
    }

    /// Turns the reassembler's loss and restart counters into a wanted request. `trigger` is
    /// the frame id just pushed, if any.
    fn observe(&mut self, r: &Reassembler, trigger: Option<u32>, stats: &Stats) {
        let (lost, restarts) = (r.lost_frames(), r.stream_restarts());
        if lost == self.seen_lost && restarts == self.seen_restarts {
            return;
        }
        stats.set_lost(lost, restarts);
        if restarts != self.seen_restarts {
            if let Some(t) = trigger {
                self.want(t.wrapping_sub(1), true);
            }
            self.need_key = true;
        } else if let Some(id) = r.last_lost_frame() {
            self.want(id, false);
        }
        self.seen_lost = lost;
        self.seen_restarts = restarts;
    }

    /// A frame completed: the first one of a stream must be a keyframe.
    fn completed(&mut self, info: &FrameInfo) {
        if std::mem::take(&mut self.need_key) && !info.flags.contains(FrameFlags::KEYFRAME) {
            self.want(info.frame_id.wrapping_sub(1), false);
        }
    }

    /// Sends the wanted request if the rate limit allows; `request` returns whether it was
    /// queued.
    fn flush(&mut self, now_us: u64, rtt_us: u64, request: &impl Fn(u32) -> bool) {
        let Some(w) = self.want else { return };
        let window = rtt_us.max(self.interval_us);
        let ready = w.force
            || self
                .last_sent_us
                .is_none_or(|t| now_us.saturating_sub(t) >= window);
        if !ready {
            // A request went out within the window: its keyframe covers this one.
            self.want = None;
        } else if request(w.id) {
            self.last_sent_us = Some(now_us);
            self.want = None;
        }
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
    use std::cell::RefCell;

    /// A `request` that records what it was asked and answers `queued`.
    fn recorder(queued: bool, log: &RefCell<Vec<u32>>) -> impl Fn(u32) -> bool + '_ {
        move |id| {
            log.borrow_mut().push(id);
            queued
        }
    }

    fn info(frame_id: u32, key: bool) -> FrameInfo {
        FrameInfo {
            frame_id,
            flags: if key {
                FrameFlags::KEYFRAME
            } else {
                FrameFlags::empty()
            },
            capture_ts_us: 0,
        }
    }

    #[test]
    fn a_full_control_queue_keeps_the_request_and_does_not_start_the_window() {
        let log = RefCell::new(Vec::new());
        let mut k = Keys::new(1_000);
        k.want(3, false);
        k.flush(0, 0, &recorder(false, &log));
        assert_eq!(
            k.want,
            Some(Want {
                id: 3,
                force: false
            }),
            "kept for a retry"
        );
        // Had the failed attempt started the window, this would be suppressed.
        k.flush(10, 0, &recorder(true, &log));
        assert_eq!(*log.borrow(), [3, 3]);
        assert_eq!(k.want, None);
    }

    #[test]
    fn requests_inside_the_window_are_suppressed_unless_forced() {
        let log = RefCell::new(Vec::new());
        let mut k = Keys::new(1_000);
        k.want(1, false);
        k.flush(0, 0, &recorder(true, &log));
        k.want(2, false);
        k.flush(500, 0, &recorder(true, &log));
        assert_eq!(k.want, None, "covered by the pending keyframe");
        k.want(3, false);
        k.flush(900, 2_000, &recorder(true, &log));
        assert_eq!(k.want, None, "the window is at least one RTT");
        k.want(4, true);
        k.flush(950, 2_000, &recorder(true, &log));
        k.want(5, false);
        k.flush(2_950, 2_000, &recorder(true, &log));
        assert_eq!(*log.borrow(), [1, 4, 5]);
    }

    #[test]
    fn the_first_frame_of_a_stream_must_be_a_keyframe() {
        let mut k = Keys::new(1_000);
        k.completed(&info(7, true));
        k.completed(&info(8, false));
        assert_eq!(k.want, None, "only the first frame is checked");

        let mut k = Keys::new(1_000);
        k.completed(&info(7, false));
        assert_eq!(
            k.want,
            Some(Want {
                id: 6,
                force: false
            })
        );
    }

    #[test]
    fn a_wanted_restart_stays_forced_when_a_newer_loss_joins_it() {
        let mut k = Keys::new(1_000);
        k.want(9, true);
        k.want(12, false);
        assert_eq!(
            k.want,
            Some(Want {
                id: 12,
                force: true
            })
        );
    }

    #[test]
    fn undecodable_datagrams_are_counted_and_cursor_positions_skipped() {
        let stats = Stats::default();
        assert!(video_fragment(&[], &stats).is_none());
        assert!(video_fragment(&[9, 1, 2], &stats).is_none());
        let mut cursor = Vec::new();
        vetro_protocol::datagram::encode_cursor(
            &vetro_protocol::datagram::CursorPosition {
                seq: 1,
                x: 0,
                y: 0,
                visible: true,
            },
            &mut cursor,
        );
        assert!(video_fragment(&cursor, &stats).is_none());
        assert_eq!(stats.snapshot().bad_datagrams, 2);
    }
}
