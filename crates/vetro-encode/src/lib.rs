//! Vetro encoder abstraction and FFmpeg backends (ARCHITECTURE section 5.3, ADR-0005).
//!
//! - [`Encoder`]: the platform seam (section 12). The engine's encoded source drives one.
//! - [`backend`]: which FFmpeg encoder runs per GPU vendor, its mandatory low-latency options,
//!   and codec negotiation. Pure data, tested everywhere.
//! - [`rate`]: delay-based rate control from the client's `LinkFeedback` (sections 7 and 9).
//! - Windows: [`ffmpeg::FfmpegEncoder`] (FFmpeg loaded at run time, D3D11 zero-copy input) and
//!   [`d3d11`] (device by adapter LUID, GPU test pattern).
#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

pub mod backend;
pub mod rate;

#[cfg(windows)]
pub mod d3d11;
#[cfg(windows)]
mod ffi;
#[cfg(windows)]
pub mod ffmpeg;

pub use backend::Backend;
pub use vetro_protocol::messages::Profile;

/// What the encoder is opened for: the negotiated profile and the client's display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub profile: Profile,
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
    pub bitrate_kbps: u32,
}

/// An encoder failure; the message says which step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Turns GPU surfaces into encoded frames, one access unit per call (no B-frames, one frame in
/// flight, ARCHITECTURE section 5.3).
pub trait Encoder: Send {
    /// The input picture the caller draws into (Windows: an NV12 D3D11 texture slice).
    type Surface;

    /// Hands a free input surface to `fill`, which must write the whole picture (on the GPU),
    /// then encodes it into `out` (cleared first). `force_keyframe` makes it an IDR. Returns
    /// whether the frame is a keyframe.
    fn encode(
        &mut self,
        fill: &mut dyn FnMut(&Self::Surface),
        force_keyframe: bool,
        out: &mut Vec<u8>,
    ) -> Result<bool, Error>;

    /// Retargets the bitrate for the next frames (rate control).
    fn set_bitrate(&mut self, kbps: u32) -> Result<(), Error>;

    fn profile(&self) -> Profile;

    fn bitrate_kbps(&self) -> u32;
}

#[cfg(all(test, windows))]
mod hw_tests;
