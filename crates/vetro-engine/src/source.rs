//! The seam between "something that makes encoded frames" and the session that sends them.
//!
//! The real pipeline (DDA capture, M1-9, feeding the FFmpeg encoder, M1-7) implements
//! [`VideoSource`] with capture and encode inside it; the synthetic [`MovingBars`] implements it
//! directly. The engine runs the source on its own thread (`pump`), so a source may block.

use std::time::{Duration, Instant};

/// The host clock: microseconds since engine start, monotonic (QPC on Windows). Every host
/// timestamp (frame headers, telemetry, clock-sync answers) comes from one `Clock`.
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

/// What the source reports about one encoded frame (host clock, microseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    pub keyframe: bool,
    pub capture_us: u64,
    pub encode_start_us: u64,
    pub encode_end_us: u64,
}

/// Longest a [`VideoSource::next_frame`] call may block, so stopping a session or the engine
/// waits at most this long for the source thread.
pub const MAX_BLOCK: Duration = Duration::from_millis(20);

/// A producer of encoded frames (one access unit each), in order.
pub trait VideoSource: Send + 'static {
    /// Waits for the next frame and writes its encoded bytes into `out` (cleared first; the
    /// buffer is reused, so steady state allocates nothing). `force_keyframe` asks for a frame
    /// the decoder can start from. Returns `None` when no frame came within [`MAX_BLOCK`] (a
    /// still screen, a slow refresh), so the caller can check for shutdown; it must never block
    /// longer than that (DDA: `AcquireNextFrame` with a timeout of at most `MAX_BLOCK`).
    fn next_frame(
        &mut self,
        force_keyframe: bool,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Option<FrameMeta>;
}

/// Synthetic source for `--no-vdd --test-pattern moving-bars`.
///
/// There is no codec yet: each "encoded" frame is opaque bytes of a realistic size (bitrate /
/// refresh for a P-frame, 4× that for a keyframe) filled with [`moving_bars`] for its sequence
/// number, so a test can check every byte. Frames are paced at the refresh rate; a missed tick
/// is skipped, never made up with a burst.
pub struct MovingBars {
    interval: Duration,
    next: Option<Instant>,
    seq: u32,
    p_bytes: usize,
    key_bytes: usize,
}

/// Largest synthetic keyframe; leaves room for FEC parity under the 8 MiB frame limit.
const MAX_SYNTHETIC_FRAME: usize = vetro_protocol::MAX_FRAME_BYTES / 2;

impl MovingBars {
    pub fn new(refresh_mhz: u32, bitrate_kbps: u32) -> Self {
        let refresh_mhz = u64::from(refresh_mhz.max(1));
        // bytes per frame = kbit/s × 1000 / 8 / (mHz / 1000)
        let p = u64::from(bitrate_kbps) * 125_000 / refresh_mhz;
        let p_bytes = usize::try_from(p)
            .unwrap_or(usize::MAX)
            .clamp(1, MAX_SYNTHETIC_FRAME);
        Self {
            interval: Duration::from_nanos(1_000_000_000_000 / refresh_mhz),
            next: None,
            seq: 0,
            p_bytes,
            key_bytes: (p_bytes * 4).min(MAX_SYNTHETIC_FRAME),
        }
    }
}

impl VideoSource for MovingBars {
    fn next_frame(
        &mut self,
        force_keyframe: bool,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Option<FrameMeta> {
        let now = Instant::now();
        let due = *self.next.get_or_insert(now);
        if due > now + MAX_BLOCK {
            // A slow refresh: sleep one slice and let the caller check for shutdown.
            std::thread::sleep(MAX_BLOCK);
            return None;
        }
        if due > now {
            std::thread::sleep(due - now);
        }
        let now = Instant::now();
        self.next = Some((due + self.interval).max(now));

        let capture_us = clock.now_us();
        let keyframe = force_keyframe || self.seq == 0;
        let len = if keyframe {
            self.key_bytes
        } else {
            self.p_bytes
        };
        out.clear();
        moving_bars(self.seq, len, out);
        self.seq = self.seq.wrapping_add(1);
        Some(FrameMeta {
            keyframe,
            capture_us,
            encode_start_us: capture_us,
            encode_end_us: clock.now_us(),
        })
    }
}

/// Appends `len` bytes of frame `seq`: eight bars, 32 bytes wide, that move one bar per frame.
pub fn moving_bars(seq: u32, len: usize, out: &mut Vec<u8>) {
    // Truncation is the point: only the bar index modulo 8 matters.
    let shift = (seq % 8) as usize;
    out.extend((0..len).map(|i| ((((i % 256) / 32 + shift) % 8) * 32) as u8));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_bitrate_and_refresh() {
        let s = MovingBars::new(120_000, 50_000);
        assert_eq!(s.p_bytes, 52_083);
        assert_eq!(s.key_bytes, 4 * 52_083);
        assert_eq!(s.interval, Duration::from_nanos(8_333_333));
        // Absurd inputs stay inside the frame limit and above zero.
        assert_eq!(MovingBars::new(1, u32::MAX).key_bytes, MAX_SYNTHETIC_FRAME);
        assert_eq!(MovingBars::new(u32::MAX, 1).p_bytes, 1);
    }

    #[test]
    fn first_and_forced_frames_are_keyframes_and_bars_move() {
        let clock = Clock::new();
        let mut s = MovingBars::new(1_000_000, 8_000); // 1000 Hz keeps the test fast
        let mut buf = Vec::new();
        let mut last_capture = 0;
        for seq in 0..6u32 {
            let meta = s.next_frame(seq == 4, &clock, &mut buf).unwrap();
            assert_eq!(meta.keyframe, seq == 0 || seq == 4);
            assert_eq!(buf.len(), if meta.keyframe { 4000 } else { 1000 });
            let mut want = Vec::new();
            moving_bars(seq, buf.len(), &mut want);
            assert_eq!(buf, want);
            assert!(meta.capture_us >= last_capture);
            assert!(meta.encode_end_us >= meta.encode_start_us);
            last_capture = meta.capture_us;
        }
    }

    #[test]
    fn a_slow_refresh_never_blocks_past_max_block() {
        let clock = Clock::new();
        let mut s = MovingBars::new(1, 8_000); // one frame per 1000 s
        let mut buf = Vec::new();
        assert!(s.next_frame(false, &clock, &mut buf).is_some());
        let t = Instant::now();
        assert!(s.next_frame(false, &clock, &mut buf).is_none());
        assert!(
            t.elapsed() < MAX_BLOCK * 5,
            "returned after {:?}",
            t.elapsed()
        );
    }

    #[test]
    fn bars_shift_by_one_per_frame() {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        moving_bars(0, 256, &mut a);
        moving_bars(1, 256, &mut b);
        assert_eq!(a[0], 0);
        assert_eq!(a[32], 32);
        assert_eq!(b[0], a[32]);
        assert_eq!(&b[..224], &a[32..]);
    }
}
