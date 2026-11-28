//! Client clock and NTP-style alignment with the host clock (ARCHITECTURE sections 7.1 and 11).
//!
//! Every second the client sends `ClockSyncRequest { seq, client_send_us }`; the host answers with
//! its receive and send times. With t1 = client send, t2 = host receive, t3 = host send and
//! t4 = client receive:
//!
//! - `rtt = (t4 − t1) − (t3 − t2)` (the host's processing time does not count),
//! - `offset = ((t2 − t1) + (t3 − t4)) / 2`, so `host_us ≈ client_us + offset`.
//!
//! The offset error is at most `rtt / 2` (fully asymmetric path), so the estimate is the sample
//! with the smallest RTT among the last [`WINDOW`] accepted samples (minimum-RTT filter). A
//! response is rejected when it answers no request still pending (unknown, duplicate or too old
//! `seq`), when its echoed send time is not ours, when its times run backwards, or when its RTT is
//! above [`MAX_RTT_US`].

use std::time::{Duration, Instant};
use vetro_protocol::messages::{ClockSyncRequest, ClockSyncResponse};

/// The client's monotonic clock in microseconds since the session started. Decoder callbacks and
/// telemetry use the same clock.
#[derive(Debug, Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
    pub fn new() -> Self {
        Self(Instant::now())
    }

    pub fn now_us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// How often the client probes the host clock.
pub const CLOCK_SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// Samples kept for the minimum-RTT filter (8 s at one probe per second), and requests that may
/// be pending at once.
pub const WINDOW: usize = 8;
/// A sample slower than this says nothing useful about the offset.
pub const MAX_RTT_US: u64 = 1_000_000;

/// The current host-clock estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockEstimate {
    /// `host_us ≈ client_us + offset_us`.
    pub offset_us: i64,
    /// RTT of the sample the offset comes from; the offset is off by at most half of it.
    pub rtt_us: u64,
}

#[derive(Debug, Default)]
pub struct ClockSync {
    next_seq: u32,
    /// Requests not answered yet: (seq, client_send_us), oldest overwritten first.
    pending: [Option<(u32, u64)>; WINDOW],
    samples: [Option<ClockEstimate>; WINDOW],
    next_sample: usize,
}

impl ClockSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next probe to send at `now_us` (client clock).
    pub fn request(&mut self, now_us: u64) -> ClockSyncRequest {
        let seq = self.next_seq;
        self.next_seq = seq.wrapping_add(1);
        self.pending[seq as usize % WINDOW] = Some((seq, now_us));
        ClockSyncRequest {
            seq,
            client_send_us: now_us,
        }
    }

    /// Takes a response received at `now_us`. Returns the updated estimate if the sample was
    /// accepted, `None` if it was rejected.
    pub fn on_response(&mut self, r: &ClockSyncResponse, now_us: u64) -> Option<ClockEstimate> {
        let slot = &mut self.pending[r.seq as usize % WINDOW];
        if *slot != Some((r.seq, r.client_send_us)) {
            return None;
        }
        *slot = None;
        let (t1, t2, t3, t4) = (r.client_send_us, r.host_recv_us, r.host_send_us, now_us);
        let total = t4.checked_sub(t1)?;
        let host = t3.checked_sub(t2)?;
        let rtt_us = total.checked_sub(host)?;
        if rtt_us > MAX_RTT_US {
            return None;
        }
        let twice = (i128::from(t2) - i128::from(t1)) + (i128::from(t3) - i128::from(t4));
        let offset_us = i64::try_from(twice / 2).ok()?;
        self.samples[self.next_sample] = Some(ClockEstimate { offset_us, rtt_us });
        self.next_sample = (self.next_sample + 1) % WINDOW;
        self.estimate()
    }

    /// The minimum-RTT sample of the window, if any sample was accepted.
    pub fn estimate(&self) -> Option<ClockEstimate> {
        self.samples
            .iter()
            .flatten()
            .min_by_key(|s| s.rtt_us)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_AHEAD: u64 = 5_000_000_000;

    /// A probe sent at `t1` that takes `up` µs to the host, `hold` µs there and `down` µs back.
    fn exchange(
        c: &mut ClockSync,
        t1: u64,
        up: u64,
        hold: u64,
        down: u64,
    ) -> Option<ClockEstimate> {
        let req = c.request(t1);
        let t2 = t1 + up + HOST_AHEAD;
        let resp = ClockSyncResponse {
            seq: req.seq,
            client_send_us: req.client_send_us,
            host_recv_us: t2,
            host_send_us: t2 + hold,
        };
        c.on_response(&resp, t1 + up + hold + down)
    }

    #[test]
    fn symmetric_path_gives_the_exact_offset_and_rtt_without_host_time() {
        let mut c = ClockSync::new();
        let e = exchange(&mut c, 1_000, 300, 5_000, 300).unwrap();
        assert_eq!(e.offset_us, HOST_AHEAD as i64);
        assert_eq!(e.rtt_us, 600);
    }

    #[test]
    fn asymmetric_path_is_off_by_at_most_half_the_rtt() {
        let mut c = ClockSync::new();
        let e = exchange(&mut c, 1_000, 900, 0, 100).unwrap();
        assert_eq!(e.rtt_us, 1_000);
        assert!((e.offset_us - HOST_AHEAD as i64).unsigned_abs() <= e.rtt_us / 2);
    }

    #[test]
    fn the_minimum_rtt_sample_wins_over_slow_outliers() {
        let mut c = ClockSync::new();
        exchange(&mut c, 0, 200, 0, 200).unwrap();
        // A queue built up on the way to the host: big RTT, skewed offset. Not chosen.
        let e = exchange(&mut c, 1_000_000, 40_000, 0, 200).unwrap();
        assert_eq!(
            e,
            ClockEstimate {
                offset_us: HOST_AHEAD as i64,
                rtt_us: 400
            }
        );
    }

    #[test]
    fn old_samples_age_out_of_the_window() {
        let mut c = ClockSync::new();
        exchange(&mut c, 0, 100, 0, 100).unwrap();
        for i in 1..=WINDOW as u64 {
            exchange(&mut c, i * 1_000_000, 500, 0, 500).unwrap();
        }
        assert_eq!(c.estimate().unwrap().rtt_us, 1_000);
    }

    #[test]
    fn bad_responses_are_rejected() {
        let mut c = ClockSync::new();
        let req = c.request(10_000);
        let good = ClockSyncResponse {
            seq: req.seq,
            client_send_us: req.client_send_us,
            host_recv_us: 50,
            host_send_us: 60,
        };
        // Unknown seq, and an echo that is not our send time.
        assert!(
            c.on_response(
                &ClockSyncResponse {
                    seq: 7,
                    ..good.clone()
                },
                10_100
            )
            .is_none()
        );
        let forged = ClockSyncResponse {
            client_send_us: 1,
            ..good.clone()
        };
        assert!(c.on_response(&forged, 10_100).is_none());
        // Host times backwards.
        let backwards = ClockSyncResponse {
            host_send_us: 40,
            ..good.clone()
        };
        assert!(c.on_response(&backwards, 10_100).is_none());
        // Received before it was sent.
        let mut c = ClockSync::new();
        let req = c.request(10_000);
        let r = ClockSyncResponse {
            seq: req.seq,
            ..good.clone()
        };
        assert!(c.on_response(&r, 9_000).is_none());
        // Host held it longer than the whole round trip.
        let mut c = ClockSync::new();
        let req = c.request(10_000);
        let r = ClockSyncResponse {
            seq: req.seq,
            host_recv_us: 0,
            host_send_us: 500,
            ..good.clone()
        };
        assert!(c.on_response(&r, 10_100).is_none());
        // Too slow to be useful.
        let mut c = ClockSync::new();
        let req = c.request(0);
        let r = ClockSyncResponse {
            seq: req.seq,
            client_send_us: 0,
            host_recv_us: 5,
            host_send_us: 5,
        };
        assert!(c.on_response(&r, MAX_RTT_US + 1).is_none());
        assert!(c.estimate().is_none());
    }

    #[test]
    fn a_duplicate_or_too_old_response_is_rejected() {
        let mut c = ClockSync::new();
        let first = c.request(0);
        let resp = ClockSyncResponse {
            seq: first.seq,
            client_send_us: 0,
            host_recv_us: 10,
            host_send_us: 10,
        };
        assert!(c.on_response(&resp, 20).is_some());
        assert!(c.on_response(&resp, 30).is_none(), "duplicate");

        let mut c = ClockSync::new();
        let old = c.request(0);
        for i in 1..=WINDOW as u64 {
            c.request(i);
        }
        let resp = ClockSyncResponse {
            seq: old.seq,
            ..resp
        };
        assert!(
            c.on_response(&resp, 20).is_none(),
            "overwritten by newer requests"
        );
    }
}
