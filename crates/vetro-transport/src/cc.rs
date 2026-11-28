//! Low-latency congestion controller (ADR-0006, ARCHITECTURE section 9).
//!
//! Video bitrate is limited by the encoder, driven by delay-based rate control from the
//! client's `LinkFeedback` (M1-7); QUIC's controller must never add its own queue on top. So:
//! - the window is huge and ignores ordinary loss (Wi-Fi loss is mostly not congestion, and FEC
//!   covers it) and ECN marks;
//! - the window is above `u32::MAX`, which turns quinn's pacer off: an encoded frame leaves as
//!   one burst instead of waiting on pacing timers (timer granularity on Windows is ~1 ms or
//!   worse, a whole budget stage);
//! - safety net: persistent congestion (every packet lost for ~3 PTOs: the path is gone)
//!   drops the window to [`FLOOR`], so a dead path is not flooded. The first ack of a packet
//!   sent after that event proves the path works again and restores the full window at once.
//!   Growing back by acked bytes would take minutes with app-limited video (a few MB per second
//!   acked against an 8 GiB window), all of it paced.

use quinn::congestion::{Controller, ControllerFactory};
use quinn_proto::RttEstimator;
use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

/// Normal window: above `u32::MAX`, so quinn does not pace.
pub(crate) const WINDOW: u64 = 1 << 33;
/// Window while the path looks dead (after persistent congestion, until a later packet is acked).
pub(crate) const FLOOR: u64 = 128 * 1024;

/// Builds [`LowLatency`] controllers; set on every connection's transport config.
#[derive(Debug, Default)]
pub(crate) struct LowLatencyFactory;

impl ControllerFactory for LowLatencyFactory {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(LowLatency {
            window: WINDOW,
            congested_at: None,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LowLatency {
    window: u64,
    /// When persistent congestion was declared; `None` while the window is full.
    congested_at: Option<Instant>,
}

impl LowLatency {
    /// `on_ack` forwards here unchanged (quinn's `RttEstimator` has no public constructor, so
    /// tests call this directly).
    fn acked(&mut self, sent: Instant) {
        if self.congested_at.is_some_and(|at| sent >= at) {
            self.window = WINDOW;
            self.congested_at = None;
        }
    }

    fn congested(&mut self, now: Instant, persistent: bool) {
        if persistent {
            self.window = FLOOR;
            self.congested_at = Some(now);
        }
    }
}

impl Controller for LowLatency {
    fn on_ack(
        &mut self,
        _now: Instant,
        sent: Instant,
        _bytes: u64,
        _app_limited: bool,
        _rtt: &RttEstimator,
    ) {
        self.acked(sent);
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        _sent: Instant,
        is_persistent_congestion: bool,
        _lost_bytes: u64,
    ) {
        self.congested(now, is_persistent_congestion);
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        self.window
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        WINDOW
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fresh() -> Box<dyn Controller> {
        Arc::new(LowLatencyFactory).build(Instant::now(), 1200)
    }

    fn concrete(cc: Box<dyn Controller>) -> LowLatency {
        *cc.into_any().downcast::<LowLatency>().unwrap()
    }

    #[test]
    fn starts_full_and_unpaced() {
        let cc = fresh();
        assert_eq!(cc.window(), WINDOW);
        assert_eq!(cc.initial_window(), WINDOW);
        assert!(cc.window() > u64::from(u32::MAX));
    }

    #[test]
    fn ordinary_loss_and_ecn_do_not_shrink_the_window() {
        let mut cc = fresh();
        let t = Instant::now();
        for _ in 0..1000 {
            cc.on_congestion_event(t, t, false, 1200);
            cc.on_congestion_event(t, t, false, 0);
        }
        assert_eq!(cc.window(), WINDOW);
    }

    #[test]
    fn a_blackout_drops_to_the_floor_and_the_first_later_ack_restores_it() {
        let t0 = Instant::now();
        let mut cc = fresh();
        // A Wi-Fi hiccup: everything sent in ~3 PTOs was lost.
        let event = t0 + Duration::from_millis(300);
        cc.on_congestion_event(event, t0, true, 500_000);
        assert_eq!(cc.window(), FLOOR);
        assert!(
            cc.window() <= u64::from(u32::MAX),
            "pacing on while the path is dead"
        );
        let mut cc = concrete(cc);

        // Late acks of packets sent before the event prove nothing: still the floor, however
        // many small acks arrive (app-limited video acks far less than the window).
        for i in 0..10_000u32 {
            cc.acked(t0 + Duration::from_micros(u64::from(i)));
        }
        assert_eq!(cc.window, FLOOR);

        // One small ack of a packet sent after the event: the path works, full window at once.
        cc.acked(event + Duration::from_millis(1));
        assert_eq!(cc.window, WINDOW);
        assert!(cc.congested_at.is_none());
    }

    #[test]
    fn a_second_blackout_needs_its_own_later_ack() {
        let t0 = Instant::now();
        let mut cc = concrete(fresh());
        cc.congested(t0, true);
        cc.acked(t0 + Duration::from_millis(1));
        assert_eq!(cc.window, WINDOW);
        let second = t0 + Duration::from_secs(1);
        cc.congested(second, true);
        cc.acked(t0 + Duration::from_millis(2));
        assert_eq!(cc.window, FLOOR);
        cc.acked(second);
        assert_eq!(cc.window, WINDOW);
    }

    #[test]
    fn acks_without_a_blackout_keep_the_window_full() {
        let t0 = Instant::now();
        let mut cc = concrete(fresh());
        cc.acked(t0);
        assert_eq!(cc.window, WINDOW);
    }
}
