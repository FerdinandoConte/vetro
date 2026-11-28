//! The seam between "something that makes encoded frames" and the session that sends them.
//!
//! The real pipeline (DDA capture, M1-9, feeding the FFmpeg encoder, M1-7) implements
//! [`VideoSource`] with capture and encode inside it: [`Encoded`] drives any
//! [`vetro_encode::Encoder`] with a painter that fills its GPU surface (capture, or the GPU test
//! pattern of `--test-pattern gpu-bars`). The synthetic [`MovingBars`] implements it directly,
//! with no codec. The engine runs the source on its own thread (`pump`), so a source may block.

use std::time::{Duration, Instant};
use vetro_encode::Encoder;

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
    /// Bitrate the frame was encoded at (rate control's target once the encoder took it).
    pub bitrate_kbps: u32,
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

    /// Rate control's new target for the next frames.
    fn set_bitrate(&mut self, kbps: u32);
}

/// Paces frames at the refresh rate; a missed tick is skipped, never made up with a burst.
struct Pacer {
    interval: Duration,
    next: Option<Instant>,
}

impl Pacer {
    fn new(refresh_mhz: u32) -> Self {
        Self {
            interval: Duration::from_nanos(1_000_000_000_000 / u64::from(refresh_mhz.max(1))),
            next: None,
        }
    }

    /// Sleeps until the next tick. `false` when it is more than [`MAX_BLOCK`] away: one slice
    /// was slept and the caller should return `None`.
    fn wait(&mut self) -> bool {
        let now = Instant::now();
        let due = *self.next.get_or_insert(now);
        if due > now + MAX_BLOCK {
            std::thread::sleep(MAX_BLOCK);
            return false;
        }
        if due > now {
            std::thread::sleep(due - now);
        }
        self.next = Some((due + self.interval).max(Instant::now()));
        true
    }
}

/// Synthetic source for `--no-vdd --test-pattern moving-bars`.
///
/// There is no codec: each "encoded" frame is opaque bytes of a realistic size (bitrate /
/// refresh for a P-frame, 4× that for a keyframe) filled with [`moving_bars`] for its sequence
/// number, so a test can check every byte. The size follows rate control's bitrate.
pub struct MovingBars {
    pacer: Pacer,
    refresh_mhz: u32,
    bitrate_kbps: u32,
    seq: u32,
    p_bytes: usize,
    key_bytes: usize,
}

/// Largest synthetic keyframe; leaves room for FEC parity under the 8 MiB frame limit.
const MAX_SYNTHETIC_FRAME: usize = vetro_protocol::MAX_FRAME_BYTES / 2;

impl MovingBars {
    pub fn new(refresh_mhz: u32, bitrate_kbps: u32) -> Self {
        let mut s = Self {
            pacer: Pacer::new(refresh_mhz),
            refresh_mhz: refresh_mhz.max(1),
            bitrate_kbps: 0,
            seq: 0,
            p_bytes: 0,
            key_bytes: 0,
        };
        s.set_bitrate(bitrate_kbps);
        s
    }
}

impl VideoSource for MovingBars {
    fn next_frame(
        &mut self,
        force_keyframe: bool,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Option<FrameMeta> {
        if !self.pacer.wait() {
            return None;
        }
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
            bitrate_kbps: self.bitrate_kbps,
        })
    }

    fn set_bitrate(&mut self, kbps: u32) {
        // bytes per frame = kbit/s × 1000 / 8 / (mHz / 1000)
        let p = u64::from(kbps) * 125_000 / u64::from(self.refresh_mhz);
        self.p_bytes = usize::try_from(p)
            .unwrap_or(usize::MAX)
            .clamp(1, MAX_SYNTHETIC_FRAME);
        self.key_bytes = (self.p_bytes * 4).min(MAX_SYNTHETIC_FRAME);
        self.bitrate_kbps = kbps;
    }
}

/// Appends `len` bytes of frame `seq`: eight bars, 32 bytes wide, that move one bar per frame.
pub fn moving_bars(seq: u32, len: usize, out: &mut Vec<u8>) {
    // Truncation is the point: only the bar index modulo 8 matters.
    let shift = (seq % 8) as usize;
    out.extend((0..len).map(|i| ((((i % 256) / 32 + shift) % 8) * 32) as u8));
}

/// Fills an encoder surface with frame `seq` (on the GPU).
pub type Painter<S> = Box<dyn FnMut(&S, u32) + Send>;

/// A real encoder fed by a painter, paced at the refresh rate. Encode time in [`FrameMeta`] is
/// the encoder call (fill, submit, bitstream out), the stage of ARCHITECTURE section 4.
pub struct Encoded<E: Encoder> {
    encoder: E,
    paint: Painter<E::Surface>,
    pacer: Pacer,
    seq: u32,
    failed: bool,
}

impl<E: Encoder + 'static> Encoded<E> {
    pub fn new(encoder: E, paint: Painter<E::Surface>, refresh_mhz: u32) -> Self {
        Self {
            encoder,
            paint,
            pacer: Pacer::new(refresh_mhz),
            seq: 0,
            failed: false,
        }
    }
}

impl<E: Encoder + 'static> VideoSource for Encoded<E> {
    fn next_frame(
        &mut self,
        force_keyframe: bool,
        clock: &Clock,
        out: &mut Vec<u8>,
    ) -> Option<FrameMeta> {
        if !self.pacer.wait() {
            return None;
        }
        let capture_us = clock.now_us();
        let (paint, seq) = (&mut self.paint, self.seq);
        // After a failure the next frame must restart the decoder.
        let force = force_keyframe || self.failed;
        match self.encoder.encode(&mut |s| paint(s, seq), force, out) {
            Ok(keyframe) => {
                self.failed = false;
                self.seq = self.seq.wrapping_add(1);
                Some(FrameMeta {
                    keyframe,
                    capture_us,
                    encode_start_us: capture_us,
                    encode_end_us: clock.now_us(),
                    bitrate_kbps: self.encoder.bitrate_kbps(),
                })
            }
            Err(e) => {
                // Cold: only on a failure, logged once until a frame succeeds again.
                if !self.failed {
                    eprintln!("vetro-engine: encode failed: {e}");
                }
                self.failed = true;
                None
            }
        }
    }

    fn set_bitrate(&mut self, kbps: u32) {
        if let Err(e) = self.encoder.set_bitrate(kbps) {
            eprintln!("vetro-engine: bitrate {kbps} kbit/s not applied: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vetro_encode::{Error, Profile};

    #[test]
    fn sizes_follow_bitrate_and_refresh() {
        let mut s = MovingBars::new(120_000, 50_000);
        assert_eq!(s.p_bytes, 52_083);
        assert_eq!(s.key_bytes, 4 * 52_083);
        assert_eq!(s.pacer.interval, Duration::from_nanos(8_333_333));
        s.set_bitrate(25_000);
        assert_eq!((s.p_bytes, s.bitrate_kbps), (26_041, 25_000));
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
            assert_eq!(meta.bitrate_kbps, 8_000);
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

    /// Writes the painted sequence number as the "bitstream"; fails when told to.
    struct Fake {
        kbps: u32,
        fail_next: bool,
        forced: Vec<bool>,
    }

    impl Encoder for Fake {
        type Surface = std::cell::Cell<u32>;

        fn encode(
            &mut self,
            fill: &mut dyn FnMut(&Self::Surface),
            force_keyframe: bool,
            out: &mut Vec<u8>,
        ) -> Result<bool, Error> {
            if std::mem::take(&mut self.fail_next) {
                return Err(Error("device lost".into()));
            }
            let surface = std::cell::Cell::new(u32::MAX);
            fill(&surface);
            out.clear();
            out.extend_from_slice(&surface.get().to_le_bytes());
            self.forced.push(force_keyframe);
            Ok(force_keyframe)
        }

        fn set_bitrate(&mut self, kbps: u32) -> Result<(), Error> {
            self.kbps = kbps;
            Ok(())
        }

        fn profile(&self) -> Profile {
            Profile::HevcMain
        }

        fn bitrate_kbps(&self) -> u32 {
            self.kbps
        }
    }

    #[test]
    fn encoded_source_drives_the_encoder() {
        let clock = Clock::new();
        let fake = Fake {
            kbps: 9_000,
            fail_next: false,
            forced: Vec::new(),
        };
        let paint: Painter<std::cell::Cell<u32>> = Box::new(|s, seq| s.set(seq));
        let mut src = Encoded::new(fake, paint, 1_000_000);
        let mut buf = Vec::new();
        let m = src.next_frame(true, &clock, &mut buf).unwrap();
        assert!(m.keyframe && m.bitrate_kbps == 9_000);
        assert_eq!(buf, 0u32.to_le_bytes());
        src.set_bitrate(4_000);
        let m = src.next_frame(false, &clock, &mut buf).unwrap();
        assert!(!m.keyframe && m.bitrate_kbps == 4_000);
        assert_eq!(buf, 1u32.to_le_bytes());
        assert!(m.encode_end_us >= m.encode_start_us && m.encode_start_us >= m.capture_us);
        // A failed frame yields nothing, and the next one restarts the decoder.
        src.encoder.fail_next = true;
        assert!(src.next_frame(false, &clock, &mut buf).is_none());
        let m = src.next_frame(false, &clock, &mut buf).unwrap();
        assert!(m.keyframe);
        assert_eq!(buf, 2u32.to_le_bytes());
        assert_eq!(src.encoder.forced, [true, false, true]);
    }
}
