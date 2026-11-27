use core::fmt;

/// Why bytes from the peer were rejected. The peer is untrusted: every decode path ends here
/// instead of panicking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A length prefix announces more bytes than the limit for this stream.
    TooLong { len: usize, max: usize },
    /// The datagram ends before its fixed header does.
    Truncated,
    /// postcard could not decode the message (unknown enum tag, bad varint, bad bool, ...).
    Postcard(postcard::Error),
    /// The message decoded but bytes were left over.
    TrailingBytes,
    /// Unknown datagram kind byte.
    UnknownDatagramKind(u8),
    /// A field is outside its allowed range; the string names the field.
    Invalid(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { len, max } => write!(f, "message of {len} bytes exceeds limit {max}"),
            Self::Truncated => f.write_str("datagram shorter than its header"),
            Self::Postcard(e) => write!(f, "postcard: {e}"),
            Self::TrailingBytes => f.write_str("trailing bytes after message"),
            Self::UnknownDatagramKind(k) => write!(f, "unknown datagram kind {k}"),
            Self::Invalid(field) => write!(f, "invalid field: {field}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        Self::Postcard(e)
    }
}

/// Why a local message could not be encoded. Encoding validates first, so we never send what
/// the peer would reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The message breaks a protocol limit; the string names the field.
    Invalid(&'static str),
    /// The encoded message is longer than the limit for its stream.
    TooLong { len: usize, max: usize },
    /// postcard failed (not expected for these types).
    Postcard(postcard::Error),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(field) => write!(f, "invalid field: {field}"),
            Self::TooLong { len, max } => write!(f, "message of {len} bytes exceeds limit {max}"),
            Self::Postcard(e) => write!(f, "postcard: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {}
