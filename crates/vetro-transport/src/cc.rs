//! Low-latency congestion controller (ADR-0006, ARCHITECTURE section 9).
//!
//! Video bitrate is limited by the encoder, driven by delay-based rate control from the
//! client's `LinkFeedback`; QUIC's controller must never add its own queue on top. So:
//! - the window is huge and ignores ordinary loss (Wi-Fi loss is mostly not congestion, and FEC
//!   covers it) and ECN marks;
//! - the window is above `u32::MAX`, which turns quinn's pacer off: an encoded frame leaves as
//!   one burst instead of waiting on pacing timers (timer granularity on Windows is ~1 ms or
//!   worse, a whole budget stage);
//! - safety net: persistent congestion (every packet lost for ~3 PTOs, i.e. the path is gone or
//!   saturated) drops the window to [`FLOOR`], which also re-enables pacing; each acked byte then
//!   grows it back by one byte, doubling per RTT, until it is full again.

use quinn::congestion::{Controller, ControllerFactory};
use quinn_proto::RttEstimator;
use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

/// Normal window: above `u32::MAX`, so quinn does not pace.
pub(crate) const WINDOW: u64 = 1 << 33;
/// Window right after persistent congestion.
pub(crate) const FLOOR: u64 = 128 * 1024;

/// Builds [`LowLatency`] controllers; set on every connection's transport config.
#[derive(Debug, Default)]
pub(crate) struct LowLatencyFactory;

impl ControllerFactory for LowLatencyFactory {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(LowLatency { window: WINDOW })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LowLatency {
    window: u64,
}

impl LowLatency {
    fn acked(&mut self, bytes: u64) {
        self.window = self.window.saturating_add(bytes).min(WINDOW);
    }

    fn congested(&mut self, persistent: bool) {
        if persistent {
            self.window = FLOOR;
        }
    }
}

impl Controller for LowLatency {
    fn on_ack(
        &mut self,
        _now: Instant,
        _sent: Instant,
        bytes: u64,
        _app_limited: bool,
        _rtt: &RttEstimator,
    ) {
        self.acked(bytes);
    }

    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        is_persistent_congestion: bool,
        _lost_bytes: u64,
    ) {
        self.congested(is_persistent_congestion);
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

    fn fresh() -> Box<dyn Controller> {
        Arc::new(LowLatencyFactory).build(Instant::now(), 1200)
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
    fn persistent_congestion_falls_to_the_floor_then_regrows() {
        let mut cc = LowLatency { window: WINDOW };
        cc.congested(true);
        assert_eq!(cc.window, FLOOR);
        // Acks of one window double it.
        cc.acked(FLOOR);
        assert_eq!(cc.window, 2 * FLOOR);
        let mut rtts = 1;
        while cc.window < WINDOW {
            let w = cc.window;
            cc.acked(w);
            rtts += 1;
        }
        assert_eq!(cc.window, WINDOW);
        // 128 KiB -> 8 GiB is 16 doublings.
        assert_eq!(rtts, 16);
    }

    #[test]
    fn window_never_exceeds_the_cap() {
        let mut cc = LowLatency { window: WINDOW };
        cc.acked(u64::MAX);
        assert_eq!(cc.window, WINDOW);
    }
}
