//! Vetro transport: QUIC (quinn) with pinned mutual TLS, pairing, channel priorities, the video
//! datagram path and a low-latency congestion controller (ARCHITECTURE sections 7–9,
//! ADR-0006, ADR-0008).
//!
//! - [`Host`] binds the engine's endpoint, issues pairing tokens and accepts sessions.
//! - [`Client`] connects to a host whose certificate fingerprint it knows from the QR code.
//! - A session exists only after the TLS handshake (both certificates pinned) and the `Hello`
//!   exchange (version negotiated, pairing token redeemed on the first connection). Until then
//!   nothing but `Hello` is sent or read (section 7.1).
//!
//! Crypto: rustls with the `ring` provider, TLS 1.3 only, no session resumption and no 0-RTT,
//! so every connection runs the full handshake through the pinning verifiers.
//!
//! Nothing here logs. Secrets (private key, pairing token) never appear in `Debug` output.
#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod cc;
mod channel;
mod endpoint;
mod identity;
mod pairing;
mod verify;

pub use channel::{MsgReceiver, MsgSender};
pub use endpoint::{Client, ClientLink, ClientSession, Connecting, Host, HostSession, PeerStream};
pub use identity::{Fingerprint, Identity};
pub use pairing::PAIR_TOKEN_TTL;

use std::time::Duration;
use vetro_protocol::version::VersionMismatch;
use vetro_protocol::{DecodeError, EncodeError, FragmentError};

/// Stream priorities (higher is sent first). Input must never wait behind anything else.
pub const PRIORITY_INPUT: i32 = 2;
pub const PRIORITY_CONTROL: i32 = 1;
pub const PRIORITY_TELEMETRY: i32 = 0;

/// Limit for the TLS handshake plus the `Hello` exchange; a peer that stalls is dropped.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
/// A silent peer is gone after this long; keep-alives every [`KEEP_ALIVE`] keep a live one up.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);
pub const KEEP_ALIVE: Duration = Duration::from_secs(1);

/// QUIC application close and stream stop codes (ARCHITECTURE section 9).
pub mod close_code {
    pub const NORMAL: u32 = 0;
    pub const PROTOCOL_ERROR: u32 = 1;
    /// The client is not paired and brought no valid pairing token.
    pub const NOT_PAIRED: u32 = 2;
    pub const VERSION_MISMATCH: u32 = 3;
}

/// Everything that can go wrong in the transport.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Bad local TLS configuration (not expected).
    Tls(rustls::Error),
    /// Identity file or key problem; the string says what.
    Identity(&'static str),
    /// The TLS handshake failed: wrong host fingerprint, client not paired (and no pairing
    /// window open), or a TLS alert from either side.
    Handshake(quinn::ConnectionError),
    /// The host refused a provisional pairing: missing, wrong, expired or reused token.
    NotPaired,
    VersionMismatch(VersionMismatch),
    /// The connection ended after setup.
    Connection(quinn::ConnectionError),
    /// A stream was reset or stopped by the peer, or used after it closed.
    Stream(&'static str),
    /// The peer broke the protocol.
    Decode(DecodeError),
    Protocol(&'static str),
    Encode(EncodeError),
    Fragment(FragmentError),
    Datagram(quinn::SendDatagramError),
    /// Setup took longer than [`SETUP_TIMEOUT`].
    Timeout,
    /// The host was full and a newer connection took this setup's slot.
    Evicted,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o: {e}"),
            Self::Tls(e) => write!(f, "tls config: {e}"),
            Self::Identity(s) => f.write_str(s),
            Self::Handshake(e) => write!(f, "handshake rejected: {e}"),
            Self::NotPaired => f.write_str("device is not paired"),
            Self::VersionMismatch(e) => e.fmt(f),
            Self::Connection(e) => write!(f, "connection: {e}"),
            Self::Stream(s) => write!(f, "stream: {s}"),
            Self::Decode(e) => write!(f, "peer sent invalid data: {e}"),
            Self::Protocol(s) => write!(f, "protocol violation: {s}"),
            Self::Encode(e) => write!(f, "encode: {e}"),
            Self::Fragment(e) => write!(f, "fragment: {e}"),
            Self::Datagram(e) => write!(f, "datagram: {e}"),
            Self::Timeout => f.write_str("session setup timed out"),
            Self::Evicted => f.write_str("setup evicted: host busy with newer connections"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rustls::Error> for Error {
    fn from(e: rustls::Error) -> Self {
        Self::Tls(e)
    }
}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

impl From<EncodeError> for Error {
    fn from(e: EncodeError) -> Self {
        Self::Encode(e)
    }
}

impl From<FragmentError> for Error {
    fn from(e: FragmentError) -> Self {
        Self::Fragment(e)
    }
}

impl From<quinn::SendDatagramError> for Error {
    fn from(e: quinn::SendDatagramError) -> Self {
        match e {
            quinn::SendDatagramError::ConnectionLost(c) => c.into(),
            e => Self::Datagram(e),
        }
    }
}

/// Classifies a connection end: a TLS alert (QUIC crypto error 0x100–0x1ff) from either side is
/// a rejected handshake, our own `NOT_PAIRED` code is a refused pairing.
impl From<quinn::ConnectionError> for Error {
    fn from(e: quinn::ConnectionError) -> Self {
        use quinn::ConnectionError as C;
        let is_tls = |code: quinn::TransportErrorCode| (0x100..0x200).contains(&u64::from(code));
        match &e {
            C::TransportError(t) if is_tls(t.code) => Self::Handshake(e),
            C::ConnectionClosed(c) if is_tls(c.error_code) => Self::Handshake(e),
            C::ApplicationClosed(c)
                if c.error_code == quinn::VarInt::from_u32(close_code::NOT_PAIRED) =>
            {
                Self::NotPaired
            }
            _ => Self::Connection(e),
        }
    }
}

impl From<quinn::WriteError> for Error {
    fn from(e: quinn::WriteError) -> Self {
        match e {
            quinn::WriteError::ConnectionLost(c) => c.into(),
            quinn::WriteError::Stopped(_) => Self::Stream("stopped by the peer"),
            _ => Self::Stream("closed"),
        }
    }
}

impl From<quinn::ReadError> for Error {
    fn from(e: quinn::ReadError) -> Self {
        match e {
            quinn::ReadError::ConnectionLost(c) => c.into(),
            quinn::ReadError::Reset(_) => Self::Stream("reset by the peer"),
            _ => Self::Stream("closed"),
        }
    }
}
