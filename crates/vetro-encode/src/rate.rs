//! Delay-based rate control (ARCHITECTURE sections 7 and 9).
//!
//! The transport's congestion controller ignores loss and never shrinks its window, so this is
//! the only thing that keeps the encoder from sending faster than the link. The client reports
//! a [`LinkFeedback`] per interval: the change of one-way delay (queues building or draining),
//! loss before FEC and the rate it received. The host lowers the bitrate as soon as queues
//! build, and probes back up slowly while the link is calm.
//!
//! Rules, one report at a time:
//! - **Overuse** (delay grew by [`OVERUSE_US`] or more, or loss before FEC reached
//!   [`HEAVY_LOSS_PERMILLE`]): back off to [`BACKOFF`] × the lower of the target and the
//!   received rate, but never below half the target in one step, then hold for [`HOLD_US`].
//! - **Underuse** (delay shrank by [`OVERUSE_US`] or more): hold, the queue is draining.
//! - **Normal**: after the hold, probe up by [`PROBE_PER_S`] × the interval (at most
//!   [`MAX_PROBE_STEP`] per report), and only while the link carries at least half the target,
//!   so a still screen (no frames) does not ratchet the target up to the cap unseen.
//! - Moderate loss is Wi-Fi noise, not congestion: FEC covers it ([`fec_percent`]), the bitrate
//!   stays.
//!
//! The target stays in `[min, max]`; a change smaller than [`MIN_CHANGE`] is not passed to the
//! encoder (unless it reaches a bound), so the encoder is not reset for noise.

use vetro_protocol::messages::LinkFeedback;

/// Queue growth over one report that counts as congestion.
pub const OVERUSE_US: i32 = 2_000;
/// Loss before FEC (per mille) that counts as congestion even without a delay signal: above
/// this the 25 % FEC ceiling is close to overwhelmed.
pub const HEAVY_LOSS_PERMILLE: u16 = 100;
/// Back-off factor applied to the delivered rate.
pub const BACKOFF: f64 = 0.85;
/// Largest cut in one report: never below this fraction of the current target.
pub const MAX_BACKOFF_STEP: f64 = 0.5;
/// No probing for this long after a back-off.
pub const HOLD_US: u64 = 1_000_000;
/// Probe rate: +8 % per second of calm link.
pub const PROBE_PER_S: f64 = 0.08;
/// Largest increase in one report.
pub const MAX_PROBE_STEP: f64 = 0.08;
/// Changes smaller than this fraction are not applied.
pub const MIN_CHANGE: f64 = 0.05;
/// FEC parity bounds on Wi-Fi (section 7).
pub const FEC_MIN: u8 = 5;
pub const FEC_MAX: u8 = 25;
/// Lowest target, as a fraction of the cap.
const MIN_FRACTION: u32 = 20;
/// Absolute floor: below this a 4:2:0 desktop is unreadable anyway.
const FLOOR_KBPS: u32 = 1_000;

/// What a report changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// New encoder bitrate, when it moved by at least [`MIN_CHANGE`].
    pub bitrate_kbps: Option<u32>,
    /// FEC parity for the next frames.
    pub fec_percent: u8,
}

#[derive(Debug, Clone)]
pub struct RateController {
    target: f64,
    applied: u32,
    min: u32,
    max: u32,
    hold_until_us: u64,
    fec_percent: u8,
}

impl RateController {
    /// Starts at `max_kbps`, the configured cap (the bitrate sent in `Configure`).
    pub fn new(max_kbps: u32) -> Self {
        let max = max_kbps.max(FLOOR_KBPS);
        Self {
            target: f64::from(max),
            applied: max,
            min: (max / MIN_FRACTION).max(FLOOR_KBPS),
            max,
            hold_until_us: 0,
            fec_percent: 10,
        }
    }

    /// The bitrate the encoder runs at.
    pub fn bitrate_kbps(&self) -> u32 {
        self.applied
    }

    pub fn fec_percent(&self) -> u8 {
        self.fec_percent
    }

    /// Takes one client report, received at `now_us` (host clock).
    pub fn on_feedback(&mut self, fb: &LinkFeedback, now_us: u64) -> Decision {
        self.fec_percent = fec_percent(fb.loss_permille);
        let received = f64::from(fb.received_kbps);
        let overuse =
            fb.delay_gradient_us >= OVERUSE_US || fb.loss_permille >= HEAVY_LOSS_PERMILLE;
        if overuse {
            let delivered = if received > 0.0 {
                received.min(self.target)
            } else {
                self.target
            };
            self.target = (delivered * BACKOFF).max(self.target * MAX_BACKOFF_STEP);
            self.hold_until_us = now_us.saturating_add(HOLD_US);
        } else if fb.delay_gradient_us > -OVERUSE_US
            && now_us >= self.hold_until_us
            && fb.interval_us > 0
            && received * 2.0 >= self.target
        {
            let secs = f64::from(fb.interval_us.min(1_000_000)) / 1e6;
            self.target *= 1.0 + (PROBE_PER_S * secs).min(MAX_PROBE_STEP);
        }
        self.target = self.target.clamp(f64::from(self.min), f64::from(self.max));

        // The clamp keeps the value inside u32.
        let next = self.target as u32;
        let at_bound = next == self.min || next == self.max;
        let moved = next != self.applied
            && (at_bound
                || f64::from(next.abs_diff(self.applied)) >= f64::from(self.applied) * MIN_CHANGE);
        let bitrate_kbps = if moved {
            self.applied = next;
            Some(next)
        } else {
            None
        };
        Decision {
            bitrate_kbps,
            fec_percent: self.fec_percent,
        }
    }
}

/// Reed–Solomon parity for the measured loss: twice the loss, within [`FEC_MIN`]..=[`FEC_MAX`].
/// shortcut: Wi-Fi bounds always; USB wants 0 % (section 7), set when M1-14 knows the path.
pub fn fec_percent(loss_permille: u16) -> u8 {
    let twice_percent = u32::from(loss_permille).div_ceil(5);
    // Clamped to FEC_MAX, so it fits a u8.
    twice_percent.clamp(u32::from(FEC_MIN), u32::from(FEC_MAX)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MAX: u32 = 50_000;
    const TICK: u32 = 100_000; // 100 ms reports

    fn fb(delay_gradient_us: i32, loss_permille: u16, received_kbps: u32) -> LinkFeedback {
        LinkFeedback {
            interval_us: TICK,
            delay_gradient_us,
            loss_permille,
            received_kbps,
            frames_lost: 0,
        }
    }

    /// Feeds `n` equal reports starting at `*now`, returns every bitrate the encoder got.
    fn run(rc: &mut RateController, now: &mut u64, n: usize, f: &LinkFeedback) -> Vec<u32> {
        let mut out = Vec::new();
        for _ in 0..n {
            *now += u64::from(TICK);
            if let Some(b) = rc.on_feedback(f, *now).bitrate_kbps {
                out.push(b);
            }
        }
        out
    }

    #[test]
    fn congestion_backs_off_below_the_delivered_rate() {
        let mut rc = RateController::new(MAX);
        let mut now = 0;
        // Link delivers 30 Mbit/s while queues grow.
        let cuts = run(&mut rc, &mut now, 1, &fb(5_000, 0, 30_000));
        assert_eq!(cuts, [25_500]); // 0.85 × 30 000
        // Still congested: keeps cutting, each step at most halving.
        let more = run(&mut rc, &mut now, 3, &fb(5_000, 0, 10_000));
        assert_eq!(more, [12_750, 8_500, 7_225]);
    }

    #[test]
    fn a_single_report_never_more_than_halves() {
        let mut rc = RateController::new(MAX);
        let d = rc.on_feedback(&fb(50_000, 0, 1), 1);
        assert_eq!(d.bitrate_kbps, Some(MAX / 2));
    }

    #[test]
    fn heavy_loss_is_congestion_moderate_loss_is_not() {
        let mut rc = RateController::new(MAX);
        let d = rc.on_feedback(&fb(0, 50, MAX), 1);
        assert_eq!(d.bitrate_kbps, None, "5 % loss: FEC, not a back-off");
        assert_eq!(d.fec_percent, 10);
        let d = rc.on_feedback(&fb(0, 150, MAX), 2);
        assert_eq!(d.bitrate_kbps, Some(42_500));
        assert_eq!(d.fec_percent, FEC_MAX);
    }

    #[test]
    fn recovery_holds_then_probes_up_in_bounded_steps_to_the_cap() {
        let mut rc = RateController::new(MAX);
        let mut now = 0;
        run(&mut rc, &mut now, 3, &fb(5_000, 0, 10_000));
        let low = rc.bitrate_kbps();
        // Calm link: nothing for the hold second.
        let calm = |rc: &RateController| fb(0, 0, rc.bitrate_kbps());
        for _ in 0..9 {
            now += u64::from(TICK);
            assert_eq!(rc.on_feedback(&calm(&rc), now).bitrate_kbps, None);
        }
        // Then up, never more than +8 % per report, until the cap.
        let mut last = low;
        for _ in 0..400 {
            now += u64::from(TICK);
            if let Some(b) = rc.on_feedback(&calm(&rc), now).bitrate_kbps {
                assert!(b > last && f64::from(b) <= f64::from(last) * 1.081, "{last} → {b}");
                last = b;
            }
        }
        assert_eq!(rc.bitrate_kbps(), MAX);
    }

    #[test]
    fn draining_queue_and_still_screen_hold() {
        let mut rc = RateController::new(MAX);
        let mut now = 0;
        run(&mut rc, &mut now, 1, &fb(5_000, 0, 20_000));
        now += 2 * HOLD_US;
        let held = rc.bitrate_kbps();
        assert!(run(&mut rc, &mut now, 50, &fb(-3_000, 0, held)).is_empty());
        // No frames (still screen): the target must not creep up unseen.
        assert!(run(&mut rc, &mut now, 50, &fb(0, 0, 0)).is_empty());
        assert_eq!(rc.bitrate_kbps(), held);
    }

    #[test]
    fn floor_and_tiny_changes() {
        let mut rc = RateController::new(MAX);
        let mut now = 0;
        run(&mut rc, &mut now, 100, &fb(9_000, 0, 1));
        assert_eq!(rc.bitrate_kbps(), MAX / 20);
        // A zero-length interval probes nothing.
        let mut rc = RateController::new(MAX);
        rc.on_feedback(&fb(5_000, 0, 40_000), 0);
        let zero = LinkFeedback {
            interval_us: 0,
            ..fb(0, 0, 40_000)
        };
        assert_eq!(rc.on_feedback(&zero, 10 * HOLD_US).bitrate_kbps, None);
        // A tiny cap still has a usable floor.
        assert_eq!(RateController::new(10).bitrate_kbps(), FLOOR_KBPS);
    }

    #[test]
    fn fec_follows_loss_within_bounds() {
        assert_eq!(fec_percent(0), FEC_MIN);
        assert_eq!(fec_percent(30), 6); // 3 % loss → 6 %
        assert_eq!(fec_percent(100), 20);
        assert_eq!(fec_percent(1000), FEC_MAX);
    }

    proptest! {
        /// Whatever the client reports, the bitrate stays in bounds and every applied step is
        /// within the documented limits.
        #[test]
        fn any_trace_stays_bounded(
            trace in proptest::collection::vec(
                (0u32..=2_000_000, any::<i32>(), 0u16..=1000, any::<u32>(), 0u64..=3_000_000),
                1..200,
            )
        ) {
            let mut rc = RateController::new(MAX);
            let mut now = 0u64;
            for (interval_us, g, loss, rx, dt) in trace {
                now += dt;
                let before = rc.bitrate_kbps();
                let f = LinkFeedback {
                    interval_us,
                    delay_gradient_us: g,
                    loss_permille: loss,
                    received_kbps: rx,
                    frames_lost: 0,
                };
                let d = rc.on_feedback(&f, now);
                let after = rc.bitrate_kbps();
                prop_assert!((MAX / 20..=MAX).contains(&after));
                prop_assert!((FEC_MIN..=FEC_MAX).contains(&d.fec_percent));
                prop_assert_eq!(d.bitrate_kbps.is_some(), after != before);
                // Steps are measured against the internal target, which the applied value
                // lags by under MIN_CHANGE, hence the slack.
                prop_assert!(f64::from(after) >= f64::from(before) * MAX_BACKOFF_STEP * 0.95 - 1.0);
                prop_assert!(f64::from(after) <= f64::from(before) * 1.14 + 1.0);
            }
        }
    }
}
