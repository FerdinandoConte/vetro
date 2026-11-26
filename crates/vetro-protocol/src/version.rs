//! Protocol version and its negotiation in `Hello` (ARCHITECTURE section 7).
//!
//! The major version is also carried in the ALPN (`vetro/<major>`), so peers of a different major
//! fail at the TLS handshake already. Within one major, minors are backwards compatible: the
//! session runs at the lower of the two minors, and features added in a later minor are used only
//! when the negotiated minor allows it.
//!
//! A minor may only add enum variants (new messages or new enum values); structs are frozen
//! within a major, because postcard is not self-describing. Each side sends only `Hello` until
//! the peer's `Hello` has arrived. See [`crate::messages`] for the full rules.

use serde::{Deserialize, Serialize};

/// A protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

/// The version this build speaks. One major version is supported at a time until v1.0.
pub const PROTOCOL_VERSION: Version = Version { major: 1, minor: 0 };

/// QUIC ALPN for [`PROTOCOL_VERSION`].
pub const ALPN: &[u8] = b"vetro/1";

/// The peers speak different major versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionMismatch {
    pub local: Version,
    pub remote: Version,
}

impl core::fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "protocol major {} here, {} on the peer",
            self.local.major, self.remote.major
        )
    }
}

impl std::error::Error for VersionMismatch {}

/// Negotiates the session version from both `Hello`s: same major required, lower minor wins.
///
/// On `Err` the side that detects it sends `Bye { reason: VersionMismatch }` and closes.
pub fn negotiate(local: Version, remote: Version) -> Result<Version, VersionMismatch> {
    if local.major != remote.major {
        return Err(VersionMismatch { local, remote });
    }
    Ok(Version {
        major: local.major,
        minor: local.minor.min(remote.minor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn alpn_carries_the_major() {
        assert_eq!(ALPN, format!("vetro/{}", PROTOCOL_VERSION.major).as_bytes());
    }

    #[test]
    fn lower_minor_wins() {
        let a = Version { major: 1, minor: 3 };
        let b = Version { major: 1, minor: 1 };
        assert_eq!(negotiate(a, b), Ok(b));
        assert_eq!(negotiate(b, a), Ok(b));
    }

    #[test]
    fn different_major_is_rejected() {
        let a = Version { major: 1, minor: 0 };
        let b = Version { major: 2, minor: 0 };
        assert_eq!(
            negotiate(a, b),
            Err(VersionMismatch {
                local: a,
                remote: b
            })
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn negotiation_is_symmetric_and_never_exceeds_either_side(a: Version, b: Version) {
            let ab = negotiate(a, b);
            let ba = negotiate(b, a);
            prop_assert_eq!(ab.is_ok(), ba.is_ok());
            prop_assert_eq!(ab.is_ok(), a.major == b.major);
            if let (Ok(v), Ok(w)) = (ab, ba) {
                prop_assert_eq!(v, w);
                prop_assert!(v <= a && v <= b);
            }
        }
    }
}
