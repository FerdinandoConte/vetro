//! Runs a [`VideoSource`] on its own thread and hands its frames to the session through a
//! one-frame mailbox (ARCHITECTURE section 10: bounded queue, drop the oldest, pooled buffers).

use crate::source::{Clock, FrameMeta, VideoSource};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use tokio::sync::Notify;
use vetro_protocol::datagram::{FrameFlags, FrameInfo};

/// Buffers in circulation: one being filled, one in the mailbox, one being sent.
const POOL: usize = 3;

pub struct EncodedFrame {
    pub info: FrameInfo,
    pub meta: FrameMeta,
    pub data: Vec<u8>,
}

/// Holds at most one frame. A new frame replaces an unsent one (fresh beats complete).
pub struct Mailbox {
    slot: Mutex<Option<EncodedFrame>>,
    free: Mutex<Vec<Vec<u8>>>,
    ready: Notify,
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

    /// Puts `frame` in the mailbox. Returns `true` if it replaced a frame that was never sent.
    pub fn publish(&self, frame: EncodedFrame) -> bool {
        let old = lock(&self.slot).replace(frame);
        self.ready.notify_one();
        old.map(|f| self.recycle(f.data)).is_some()
    }

    /// The newest frame, waiting for one if the mailbox is empty. Cancel safe.
    pub async fn take(&self) -> EncodedFrame {
        loop {
            if let Some(f) = lock(&self.slot).take() {
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
    /// section 7.1); a frame dropped from the mailbox leaves a gap the client reports as lost.
    pub fn start(mut source: Box<dyn VideoSource>, clock: Clock) -> std::io::Result<Self> {
        let flags = Arc::new(Flags::default());
        let mailbox = Arc::new(Mailbox::new());
        let (f, mb) = (flags.clone(), mailbox.clone());
        let thread = std::thread::Builder::new()
            .name("vetro-source".into())
            .spawn(move || {
                let mut frame_id = 0u32;
                while !f.stop.load(Ordering::Acquire) {
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
                    if mb.publish(frame) {
                        // The frame just published depends on the dropped one: restart the
                        // chain. shortcut: the real pipeline should drop before encoding
                        // (M1-7/M1-9), so no encoded frame is ever thrown away.
                        f.keyframe.store(true, Ordering::Release);
                    }
                }
            })?;
        Ok(Self {
            flags,
            thread: Some(thread),
            mailbox,
        })
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
            },
            data,
        }
    }

    #[tokio::test]
    async fn newest_frame_wins_and_buffers_are_reused() {
        let mb = Mailbox::new();
        let mut a = mb.buffer();
        a.extend_from_slice(&[1; 64]);
        let a_ptr = a.as_ptr();
        assert!(!mb.publish(frame(0, a)));
        assert!(mb.publish(frame(1, mb.buffer())), "frame 0 was never sent");
        let got = mb.take().await;
        assert_eq!(got.info.frame_id, 1);
        // Frame 0's buffer went back to the pool and comes out again, capacity intact.
        let reused = mb.buffer();
        assert_eq!(reused.as_ptr(), a_ptr);
        assert!(reused.capacity() >= 64);
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
