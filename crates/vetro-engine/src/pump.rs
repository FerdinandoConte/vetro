//! Runs a [`VideoSource`] on its own thread and hands its frames to the session through a
//! one-frame mailbox (ARCHITECTURE section 10: bounded queue, pooled buffers).
//!
//! Frames are dropped *before* they are made, never after: the source makes the next frame only
//! once the session has taken the previous one. With a real encoder every P-frame references the
//! frame before it, so throwing an encoded frame away would leave the client a frame whose
//! reference never arrived. A session that falls behind instead makes the source skip
//! capture ticks (its pacer never bursts), and the next frame is captured fresh.

use crate::source::{Clock, FrameMeta, VideoSource};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::Notify;
use vetro_protocol::datagram::{FrameFlags, FrameInfo};

/// Buffers in circulation: one being filled, one in the mailbox, one being sent.
const POOL: usize = 3;

pub struct EncodedFrame {
    pub info: FrameInfo,
    pub meta: FrameMeta,
    pub data: Vec<u8>,
}

/// Holds at most one frame; its one producer waits for the slot to be empty.
pub struct Mailbox {
    slot: Mutex<Option<EncodedFrame>>,
    free: Mutex<Vec<Vec<u8>>>,
    ready: Notify,
    taken: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Mailbox {
    pub fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            free: Mutex::new((0..POOL).map(|_| Vec::new()).collect()),
            ready: Notify::new(),
            taken: Condvar::new(),
        }
    }

    /// A pooled buffer; it grows to the largest frame once and is reused after that.
    pub fn buffer(&self) -> Vec<u8> {
        lock(&self.free).pop().unwrap_or_default()
    }

    /// Returns a buffer to the pool. The pool never grows past [`POOL`].
    pub fn recycle(&self, buf: Vec<u8>) {
        let mut free = lock(&self.free);
        if free.len() < POOL {
            free.push(buf);
        }
    }

    /// Waits up to `timeout` for the session to take the unsent frame; `true` once the slot is
    /// empty.
    pub fn wait_empty(&self, timeout: Duration) -> bool {
        let slot = lock(&self.slot);
        let (slot, _) = self
            .taken
            .wait_timeout_while(slot, timeout, |s| s.is_some())
            .unwrap_or_else(PoisonError::into_inner);
        slot.is_none()
    }

    /// Puts `frame` in the mailbox, which the single producer emptied first (`wait_empty`).
    /// Returns `false`, and drops nothing, if the slot was still full.
    #[must_use]
    pub fn publish(&self, frame: EncodedFrame) -> bool {
        let mut slot = lock(&self.slot);
        if slot.is_some() {
            drop(slot);
            self.recycle(frame.data);
            return false;
        }
        *slot = Some(frame);
        drop(slot);
        self.ready.notify_one();
        true
    }

    /// The newest frame, waiting for one if the mailbox is empty. Cancel safe.
    pub async fn take(&self) -> EncodedFrame {
        loop {
            if let Some(f) = lock(&self.slot).take() {
                self.taken.notify_one();
                return f;
            }
            // `notify_one` stores a permit, so a publish between the check and here is kept.
            self.ready.notified().await;
        }
    }
}

/// Shared between the session and the pump thread.
#[derive(Default)]
struct Flags {
    stop: AtomicBool,
    keyframe: AtomicBool,
    /// Rate control's target; 0 = no change requested yet.
    bitrate_kbps: AtomicU32,
}

/// The running source thread; dropping it joins the thread, which takes at most
/// [`crate::source::MAX_BLOCK`] plus one frame's fill. Drop it off the async workers
/// (`spawn_blocking`).
pub struct Pump {
    flags: Arc<Flags>,
    thread: Option<JoinHandle<()>>,
    pub mailbox: Arc<Mailbox>,
}

impl Pump {
    /// Starts `source`. Frame ids start at 0 and go up by one per produced frame (ARCHITECTURE
    /// section 7.1); every produced frame reaches the session.
    pub fn start(mut source: Box<dyn VideoSource>, clock: Clock) -> std::io::Result<Self> {
        let flags = Arc::new(Flags::default());
        let mailbox = Arc::new(Mailbox::new());
        let (f, mb) = (flags.clone(), mailbox.clone());
        let thread = std::thread::Builder::new()
            .name("vetro-source".into())
            .spawn(move || {
                let mut frame_id = 0u32;
                let mut bitrate = 0;
                while !f.stop.load(Ordering::Acquire) {
                    // Drop before encoding: no new frame while the last one is unsent.
                    if !mb.wait_empty(crate::source::MAX_BLOCK) {
                        continue;
                    }
                    let target = f.bitrate_kbps.load(Ordering::Acquire);
                    if target != bitrate {
                        bitrate = target;
                        source.set_bitrate(target);
                    }
                    let mut buf = mb.buffer();
                    let force = f.keyframe.swap(false, Ordering::AcqRel);
                    let Some(meta) = source.next_frame(force, &clock, &mut buf) else {
                        mb.recycle(buf);
                        f.keyframe.fetch_or(force, Ordering::AcqRel);
                        continue;
                    };
                    let info = FrameInfo {
                        frame_id,
                        flags: if meta.keyframe {
                            FrameFlags::KEYFRAME
                        } else {
                            FrameFlags::empty()
                        },
                        capture_ts_us: meta.capture_us,
                    };
                    frame_id = frame_id.wrapping_add(1);
                    let frame = EncodedFrame {
                        info,
                        meta,
                        data: buf,
                    };
                    // Only this thread fills the slot, and it was empty above.
                    let published = mb.publish(frame);
                    debug_assert!(published, "the mailbox has a single producer");
                }
            })?;
        Ok(Self {
            flags,
            thread: Some(thread),
            mailbox,
        })
    }

    /// Rate control: the source retargets before its next frame.
    pub fn set_bitrate(&self, kbps: u32) {
        self.flags.bitrate_kbps.store(kbps, Ordering::Release);
    }

    /// The next frame the source makes will be a keyframe.
    pub fn request_keyframe(&self) {
        self.flags.keyframe.store(true, Ordering::Release);
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.flags.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MovingBars;

    fn frame(id: u32, data: Vec<u8>) -> EncodedFrame {
        EncodedFrame {
            info: FrameInfo {
                frame_id: id,
                flags: FrameFlags::empty(),
                capture_ts_us: 0,
            },
            meta: FrameMeta {
                keyframe: false,
                capture_us: 0,
                encode_start_us: 0,
                encode_end_us: 0,
                bitrate_kbps: 0,
            },
            data,
        }
    }

    #[tokio::test]
    async fn an_unsent_frame_is_never_replaced_and_buffers_are_reused() {
        let mb = Mailbox::new();
        let mut a = mb.buffer();
        a.extend_from_slice(&[1; 64]);
        let a_ptr = a.as_ptr();
        assert!(mb.publish(frame(0, a)));
        assert!(
            !mb.wait_empty(Duration::from_millis(5)),
            "frame 0 is unsent"
        );
        let mut b = mb.buffer();
        b.extend_from_slice(&[2; 32]);
        let b_ptr = b.as_ptr();
        assert!(!mb.publish(frame(1, b)), "a full slot refuses");
        let got = mb.take().await;
        assert_eq!(got.info.frame_id, 0, "the unsent frame survives");
        assert!(mb.wait_empty(Duration::ZERO));
        // Refused and sent buffers go back to the pool, capacity intact.
        let b_again = mb.buffer();
        assert_eq!(b_again.as_ptr(), b_ptr);
        mb.recycle(got.data);
        let reused = mb.buffer();
        assert_eq!(reused.as_ptr(), a_ptr);
        assert!(reused.capacity() >= 64);
    }

    /// A session slower than the source: the source skips ticks instead of encoding frames
    /// that would be thrown away, so ids have no gaps and no unasked keyframe appears.
    #[tokio::test]
    async fn a_slow_session_skips_capture_ticks_not_encoded_frames() {
        let pump = Pump::start(Box::new(MovingBars::new(1_000_000, 8_000)), Clock::new()).unwrap();
        let mut ids = Vec::new();
        for _ in 0..10 {
            let f = pump.mailbox.take().await;
            ids.push((f.info.frame_id, f.meta.keyframe));
            pump.mailbox.recycle(f.data);
            // 20 frame intervals of the 1 kHz source pass before the next take.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let want: Vec<_> = (0..10).map(|i| (i, i == 0)).collect();
        assert_eq!(ids, want);
        drop(pump);
    }

    #[test]
    fn pool_is_bounded() {
        let mb = Mailbox::new();
        for _ in 0..10 {
            mb.recycle(Vec::new());
        }
        assert_eq!(lock(&mb.free).len(), POOL);
    }

    #[tokio::test]
    async fn pump_numbers_frames_and_stops() {
        let pump = Pump::start(Box::new(MovingBars::new(1_000_000, 8_000)), Clock::new()).unwrap();
        let mut last = None;
        for _ in 0..20 {
            let f = pump.mailbox.take().await;
            if let Some(prev) = last {
                assert!(f.info.frame_id > prev);
            }
            assert_eq!(f.info.capture_ts_us, f.meta.capture_us);
            last = Some(f.info.frame_id);
            pump.mailbox.recycle(f.data);
        }
        drop(pump); // joins the thread
    }
}
