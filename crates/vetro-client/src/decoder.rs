//! The seam between the platform-neutral session and a video decoder.
//!
//! Android implements it with NDK `AMediaCodec` in async mode ([`crate::android`]); x86 tests use
//! a mock. The session calls [`Decoder::decode`] once per complete access unit, in frame order;
//! the decoder reports output and render times from any thread through [`DecoderEvents`].

use crate::clock::Clock;
use tokio::sync::mpsc;
use vetro_protocol::datagram::FrameInfo;
use vetro_protocol::messages::{Configure, Profile};

pub trait Decoder: Send {
    /// Profiles this device can decode, best first; sent to the host in `Capabilities`.
    fn profiles(&self) -> Vec<Profile>;

    /// Starts decoding the host's stream, replacing any earlier configuration. Called on every
    /// `Configure`, before the first frame of that stream.
    fn configure(&mut self, cfg: &Configure, events: DecoderEvents) -> Result<(), DecoderError>;

    /// Queues one complete access unit and returns at once. `Ok(false)`: no input buffer was free
    /// (or the unit did not fit), so it was dropped; the session requests a keyframe.
    fn decode(&mut self, info: &FrameInfo, data: &[u8]) -> Result<bool, DecoderError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderError {
    pub what: &'static str,
    /// Platform status code, 0 when there is none.
    pub code: i32,
}

impl core::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "decoder: {} ({})", self.what, self.code)
    }
}

impl std::error::Error for DecoderError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderEvent {
    /// The decoder produced `frame_id` and released it for display at `at_us`.
    /// `render_pending`: a [`DecoderEvent::Rendered`] for it will follow.
    Decoded {
        frame_id: u32,
        at_us: u64,
        render_pending: bool,
    },
    /// `frame_id` reached the display at `at_us`.
    Rendered { frame_id: u32, at_us: u64 },
    /// The codec failed; the session ends.
    Error(DecoderError),
}

/// Where a decoder reports what happened, from any thread. Never blocks: when the session lags
/// behind by [`EVENT_QUEUE`] events, new ones are dropped (they only feed telemetry).
#[derive(Debug, Clone)]
pub struct DecoderEvents {
    tx: mpsc::Sender<DecoderEvent>,
    clock: Clock,
}

pub(crate) const EVENT_QUEUE: usize = 64;

impl DecoderEvents {
    pub(crate) fn new(clock: Clock) -> (Self, mpsc::Receiver<DecoderEvent>) {
        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        (Self { tx, clock }, rx)
    }

    /// The session clock (microseconds), the one event times are in.
    pub fn now_us(&self) -> u64 {
        self.clock.now_us()
    }

    pub fn send(&self, event: DecoderEvent) {
        let _ = self.tx.try_send(event);
    }
}
