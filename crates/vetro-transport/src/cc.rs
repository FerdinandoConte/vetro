//! Low-latency congestion controller (ADR-0006, ARCHITECTURE section 9).
//!
//! Video bitrate is limited by the encoder, driven by delay-based rate control from the
//! client's `LinkFeedback` (M1-7); QUIC's controller must never add its own queue on top. So
//! the window is fixed:
//! - it ignores loss, ECN marks and persistent congestion. Wi-Fi loss is mostly not
//!   congestion and FEC covers it. QUIC declares persistent congestion only when an ack
//!   arrives after a blackout, i.e. once the path already works again, so shrinking the window
//!   then would only add a cool-down (paced, at Windows timer granularity) right after a hiccup;
//! - it is above `u32::MAX`, which turns quinn's pacer off: an encoded frame leaves as one
//!   burst instead of waiting on pacing timers (~1 ms or worse on Windows, a whole budget stage).
//!
//! What bounds the send rate is the encoder bitrate; what bounds queueing is quinn's
//! drop-oldest datagram queue (`endpoint.rs`).

use quinn::congestion::{Controller, ControllerFactory};
use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

/// The window: above `u32::MAX`, so quinn does not pace.
pub(crate) const WINDOW: u64 = 1 << 33;

/// Builds [`LowLatency`] controllers; set on every connection's transport config.
#[derive(Debug, Default)]
pub(crate) struct LowLatencyFactory;

impl ControllerFactory for LowLatencyFactory {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(LowLatency)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LowLatency;

impl Controller for LowLatency {
    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        _is_persistent_congestion: bool,
        _lost_bytes: u64,
    ) {
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        WINDOW
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

    #[test]
    fn starts_full_and_unpaced() {
        let cc = fresh();
        assert_eq!(cc.window(), WINDOW);
        assert_eq!(cc.initial_window(), WINDOW);
        assert!(cc.window() > u64::from(u32::MAX));
    }

    #[test]
    fn no_congestion_signal_shrinks_the_window() {
        let mut cc = fresh();
        let t = Instant::now();
        for i in 0..1000u64 {
            let now = t + Duration::from_millis(i);
            cc.on_congestion_event(now, t, false, 1200); // loss
            cc.on_congestion_event(now, t, false, 0); // ECN
            cc.on_congestion_event(now, t, true, 500_000); // persistent congestion
            cc.on_mtu_update(1452);
            cc.on_sent(now, 1200, i);
            cc.on_end_acks(now, 0, true, Some(i));
            assert_eq!(cc.window(), WINDOW);
            assert_eq!(cc.clone_box().window(), WINDOW);
        }
    }
}
