//! Vetro wire protocol: messages, framing, FEC and versioning (ARCHITECTURE section 7).
//!
//! No I/O, no clock, no threads: the transport feeds bytes in and takes bytes out, and passes the
//! current time as `now_us` where it matters. `clippy.toml` enforces this.
//!
//! Everything that comes from the peer is untrusted. Every decode path returns a typed error and
//! never panics, and every length and count is bounded by the constants below.
//!
//! - [`messages`]: control, input and telemetry stream messages (postcard, length-prefixed).
//! - [`datagram`]: the datagram channel (video fragments, cursor position), fixed binary header.
//! - [`Fragmenter`] / [`Reassembler`]: frame fragmentation with Reed–Solomon parity, and bounded
//!   reassembly with FEC recovery.
//! - [`version`]: protocol version and negotiation.
#![forbid(unsafe_code)]

pub mod datagram;
mod error;
mod fragment;
pub mod messages;
mod reassembly;
pub mod version;

pub use error::{DecodeError, EncodeError};
pub use fragment::{FragmentError, Fragmenter};
pub use reassembly::{Frame, PushOutcome, Reassembler, ReassemblerConfig, ReassemblyError};

/// Largest total payload of one frame's fragments, parity included (8 MiB).
///
/// Bounds the memory one in-flight frame can take on the receiver.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Largest number of fragments (data + parity) of one frame.
pub const MAX_FRAGMENTS: u16 = 8192;

/// Largest number of parity fragments of one frame (the `fec_parity` field is a `u8`).
pub const MAX_PARITY_FRAGMENTS: u8 = u8::MAX;

/// Largest payload of one fragment. Always even (Reed–Solomon works on 16-bit symbols).
pub const MAX_FRAGMENT_PAYLOAD: usize = 65_534;
