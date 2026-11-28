//! Client telemetry (ARCHITECTURE section 11): per-frame stage times sent to the host as
//! `ClientFrameTimes`, and session counters the HUD reads without locks.

use crate::clock::ClockEstimate;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
use vetro_protocol::messages::ClientFrameTimes;

/// Session counters, updated by the session task and read from any thread.
#[derive(Debug, Default)]
pub struct Stats {
    streaming: AtomicBool,
    frames_complete: AtomicU64,
    frames_decoded: AtomicU64,
    frames_lost: AtomicU64,
    fec_recovered: AtomicU64,
    decoder_drops: AtomicU64,
    keyframe_requests: AtomicU64,
    stream_restarts: AtomicU64,
    bad_datagrams: AtomicU64,
    clock_offset_us: AtomicI64,
    /// 0 until the first clock sample, then the sample's RTT + 1.
    clock_rtt_plus_one_us: AtomicU64,
}

/// A copy of [`Stats`] at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSnapshot {
    /// The host sent `Configure`: the session is set up and, on a first connection, the
    /// pairing is confirmed (the host redeems the token before it sends anything else).
    pub streaming: bool,
    /// Frames reassembled (after FEC).
    pub frames_complete: u64,
    /// Frames the decoder reported as decoded.
    pub frames_decoded: u64,
    /// Frames lost in reassembly (`Reassembler::lost_frames`).
    pub frames_lost: u64,
    /// Data fragments rebuilt from parity.
    pub fec_recovered: u64,
    /// Complete frames the decoder had no room for.
    pub decoder_drops: u64,
    pub keyframe_requests: u64,
    pub stream_restarts: u64,
    /// Datagrams that failed to decode or reassemble (a host bug), discarded.
    pub bad_datagrams: u64,
    pub clock: Option<ClockEstimate>,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let rtt = self.clock_rtt_plus_one_us.load(Relaxed);
        StatsSnapshot {
            streaming: self.streaming.load(Relaxed),
            frames_complete: self.frames_complete.load(Relaxed),
            frames_decoded: self.frames_decoded.load(Relaxed),
            frames_lost: self.frames_lost.load(Relaxed),
            fec_recovered: self.fec_recovered.load(Relaxed),
            decoder_drops: self.decoder_drops.load(Relaxed),
            keyframe_requests: self.keyframe_requests.load(Relaxed),
            stream_restarts: self.stream_restarts.load(Relaxed),
            bad_datagrams: self.bad_datagrams.load(Relaxed),
            clock: (rtt > 0).then(|| ClockEstimate {
                offset_us: self.clock_offset_us.load(Relaxed),
                rtt_us: rtt - 1,
            }),
        }
    }

    pub(crate) fn set_streaming(&self) {
        self.streaming.store(true, Relaxed);
    }

    pub(crate) fn frame_complete(&self, fec_recovered: u16) {
        self.frames_complete.fetch_add(1, Relaxed);
        self.fec_recovered
            .fetch_add(u64::from(fec_recovered), Relaxed);
    }

    pub(crate) fn frame_decoded(&self) {
        self.frames_decoded.fetch_add(1, Relaxed);
    }

    pub(crate) fn set_lost(&self, lost: u64, restarts: u64) {
        self.frames_lost.store(lost, Relaxed);
        self.stream_restarts.store(restarts, Relaxed);
    }

    pub(crate) fn decoder_drop(&self) {
        self.decoder_drops.fetch_add(1, Relaxed);
    }

    pub(crate) fn keyframe_request(&self) {
        self.keyframe_requests.fetch_add(1, Relaxed);
    }

    pub(crate) fn bad_datagram(&self) {
        self.bad_datagrams.fetch_add(1, Relaxed);
    }

    pub(crate) fn set_clock(&self, e: ClockEstimate) {
        // Two stores, read without a lock: a HUD may briefly pair one sample's offset with the
        // next one's RTT, which is harmless for display.
        self.clock_offset_us.store(e.offset_us, Relaxed);
        self.clock_rtt_plus_one_us
            .store(e.rtt_us.saturating_add(1), Relaxed);
    }
}

/// Frames whose times are still being collected (received → decoded → rendered). A frame that
/// is not finished after this many newer frames is overwritten and never reported.
const IN_FLIGHT: usize = 16;

/// Collects the client stage times of each frame. Fixed size, allocation-free.
#[derive(Debug, Default)]
pub(crate) struct FrameTimes {
    slots: [Option<ClientFrameTimes>; IN_FLIGHT],
}

impl FrameTimes {
    fn slot(&mut self, frame_id: u32) -> &mut Option<ClientFrameTimes> {
        &mut self.slots[frame_id as usize % IN_FLIGHT]
    }

    fn get(&mut self, frame_id: u32) -> Option<&mut ClientFrameTimes> {
        self.slot(frame_id)
            .as_mut()
            .filter(|t| t.frame_id == frame_id)
    }

    /// The frame was reassembled at `received_us` and queued to the decoder at `decode_in_us`.
    pub(crate) fn queued(&mut self, frame_id: u32, received_us: u64, decode_in_us: u64, fec: u16) {
        *self.slot(frame_id) = Some(ClientFrameTimes {
            frame_id,
            received_us,
            decode_in_us,
            decode_out_us: 0,
            rendered_us: 0,
            fec_recovered: fec,
        });
    }

    /// Returns the finished record when no render time will follow (`rendered_us` stays 0,
    /// meaning unknown).
    pub(crate) fn decoded(
        &mut self,
        frame_id: u32,
        at_us: u64,
        render_pending: bool,
    ) -> Option<ClientFrameTimes> {
        let t = self.get(frame_id)?;
        t.decode_out_us = at_us;
        if render_pending {
            return None;
        }
        self.slot(frame_id).take()
    }

    /// Returns the finished record.
    pub(crate) fn rendered(&mut self, frame_id: u32, at_us: u64) -> Option<ClientFrameTimes> {
        let t = self.get(frame_id)?;
        t.rendered_us = at_us;
        self.slot(frame_id).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_reported_once_with_all_its_times() {
        let mut t = FrameTimes::default();
        t.queued(3, 10, 11, 2);
        assert_eq!(t.decoded(3, 15, true), None);
        let r = t.rendered(3, 20).unwrap();
        assert_eq!(
            r,
            ClientFrameTimes {
                frame_id: 3,
                received_us: 10,
                decode_in_us: 11,
                decode_out_us: 15,
                rendered_us: 20,
                fec_recovered: 2,
            }
        );
        assert_eq!(t.rendered(3, 21), None, "reported once");
    }

    #[test]
    fn without_a_render_callback_the_decode_finishes_the_record() {
        let mut t = FrameTimes::default();
        t.queued(5, 1, 2, 0);
        assert_eq!(t.decoded(5, 3, false).unwrap().rendered_us, 0);
    }

    #[test]
    fn a_stale_slot_is_not_mixed_up_with_a_newer_frame() {
        let mut t = FrameTimes::default();
        t.queued(1, 1, 1, 0);
        t.queued(1 + IN_FLIGHT as u32, 2, 2, 0);
        assert_eq!(t.decoded(1, 5, false), None);
        assert!(t.decoded(1 + IN_FLIGHT as u32, 5, false).is_some());
    }

    #[test]
    fn snapshot_reports_no_clock_until_a_sample() {
        let s = Stats::default();
        assert_eq!(s.snapshot().clock, None);
        s.set_clock(ClockEstimate {
            offset_us: -7,
            rtt_us: 0,
        });
        assert_eq!(
            s.snapshot().clock,
            Some(ClockEstimate {
                offset_us: -7,
                rtt_us: 0
            })
        );
    }
}
