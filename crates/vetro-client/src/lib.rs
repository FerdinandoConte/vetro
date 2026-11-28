//! Vetro Android client core (ARCHITECTURE section 6.2), built as a `cdylib` for the Kotlin app
//! and as an `rlib` for x86 tests.
//!
//! - [`session::run`]: the platform-neutral session (connect with the pinned host fingerprint,
//!   `Capabilities`, `Configure`, reassembly and FEC, keyframe requests, clock sync,
//!   telemetry), driving any [`Decoder`].
//! - [`clock`]: the session clock and NTP-style host clock alignment.
//! - [`telemetry`]: per-frame stage times and lock-free session counters.
//! - `android` (Android only): the NDK `AMediaCodec` decoder and the JNI surface for the app.
//!
//! Nothing here logs. Off Android the crate has no `unsafe` code.
#![cfg_attr(not(target_os = "android"), forbid(unsafe_code))]
#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
#![deny(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

#[cfg(target_os = "android")]
mod android;
pub mod clock;
mod decoder;
pub mod session;
pub mod telemetry;

pub use decoder::{Decoder, DecoderError, DecoderEvent, DecoderEvents};
