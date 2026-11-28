//! The mDNS advertisement of a host (ARCHITECTURE sections 5.5, 6.2 and 8).
//!
//! The engine publishes `_vetro._udp.local.` with A records and two TXT entries: `v`, the
//! protocol major, and `fp`, the SHA-256 of the host certificate as 64 lowercase hex digits, so
//! a client can tell its paired host from any other. Both are public data. The pairing token
//! never goes into DNS-SD (section 8): anyone on the LAN can read these records.
//!
//! The record is untrusted input on the client: [`HostAdvert::parse`] never panics, ignores
//! unknown keys (later minors may add some) and rejects anything malformed. A matching
//! fingerprint is only a hint for choosing a host; the TLS handshake still pins it.

use core::fmt;

/// DNS-SD service type (RFC 6763), with the domain label.
pub const SERVICE_TYPE: &str = "_vetro._udp.local.";

const KEY_VERSION: &str = "v";
const KEY_FINGERPRINT: &str = "fp";

/// What the TXT record of a host says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostAdvert {
    /// Protocol major version the host speaks.
    pub major: u16,
    /// SHA-256 of the host certificate (what pairing pins).
    pub fingerprint: [u8; 32],
}

/// A TXT record that is not a valid [`HostAdvert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertError {
    Missing(&'static str),
    Invalid(&'static str),
}

impl fmt::Display for AdvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(k) => write!(f, "TXT key `{k}` is missing"),
            Self::Invalid(k) => write!(f, "TXT key `{k}` is malformed"),
        }
    }
}

impl std::error::Error for AdvertError {}

impl HostAdvert {
    /// The TXT entries to publish, as `(key, value)`.
    pub fn to_txt(&self) -> [(&'static str, String); 2] {
        let fp = self
            .fingerprint
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        [(KEY_VERSION, self.major.to_string()), (KEY_FINGERPRINT, fp)]
    }

    /// Reads TXT entries as `(key, value)`; a bare key has no value. Keys compare ASCII
    /// case-insensitively and the first occurrence of a key wins (RFC 6763 section 6.4).
    pub fn parse<'a>(
        entries: impl IntoIterator<Item = (&'a str, Option<&'a [u8]>)>,
    ) -> Result<Self, AdvertError> {
        let (mut major, mut fp) = (None, None);
        for (key, val) in entries {
            if key.eq_ignore_ascii_case(KEY_VERSION) {
                major.get_or_insert(val);
            } else if key.eq_ignore_ascii_case(KEY_FINGERPRINT) {
                fp.get_or_insert(val);
            }
        }
        let major = major.ok_or(AdvertError::Missing(KEY_VERSION))?;
        let fp = fp.ok_or(AdvertError::Missing(KEY_FINGERPRINT))?;
        Ok(Self {
            major: parse_major(major).ok_or(AdvertError::Invalid(KEY_VERSION))?,
            fingerprint: parse_fingerprint(fp).ok_or(AdvertError::Invalid(KEY_FINGERPRINT))?,
        })
    }
}

fn parse_major(v: Option<&[u8]>) -> Option<u16> {
    let v = v?;
    // Plain decimal only: no sign, no spaces, no leading zero, so one spelling per value.
    let plain = !v.is_empty() && v.len() <= 5 && v.iter().all(u8::is_ascii_digit);
    if !plain || (v[0] == b'0' && v.len() > 1) {
        return None;
    }
    core::str::from_utf8(v).ok()?.parse().ok()
}

fn parse_fingerprint(v: Option<&[u8]>) -> Option<[u8; 32]> {
    let v = v?;
    if v.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None, // lowercase only, as `Fingerprint`'s `Display` and the QR write it
    };
    let mut out = [0u8; 32];
    for (o, pair) in out.iter_mut().zip(v.as_chunks::<2>().0) {
        *o = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entries<'a>(txt: &'a [(&'static str, String)]) -> Vec<(&'a str, Option<&'a [u8]>)> {
        txt.iter().map(|(k, v)| (*k, Some(v.as_bytes()))).collect()
    }

    fn advert() -> HostAdvert {
        HostAdvert {
            major: 1,
            fingerprint: core::array::from_fn(|i| i as u8 * 7 + 1),
        }
    }

    #[test]
    fn txt_has_exactly_the_public_keys() {
        let txt = advert().to_txt();
        assert_eq!(txt[0], ("v", "1".to_string()));
        assert_eq!(txt[1].0, "fp");
        assert_eq!(txt[1].1.len(), 64);
        assert_eq!(txt.len(), 2, "nothing else, in particular no token");
    }

    #[test]
    fn rejects_malformed_records() {
        let fp = "ab".repeat(32);
        let bad: &[&[(&str, &str)]] = &[
            &[],
            &[("v", "1")],
            &[("fp", &fp)],
            &[("v", "01"), ("fp", &fp)],
            &[("v", "+1"), ("fp", &fp)],
            &[("v", "65536"), ("fp", &fp)],
            &[("v", ""), ("fp", &fp)],
            &[("v", "1"), ("fp", &fp[..62])],
            &[("v", "1"), ("fp", &"AB".repeat(32))],
            &[("v", "1"), ("fp", &"zz".repeat(32))],
            &[("v", "1"), ("fp", &format!("{fp}00"))],
        ];
        for txt in bad {
            let e = txt.iter().map(|(k, v)| (*k, Some(v.as_bytes())));
            assert!(HostAdvert::parse(e).is_err(), "{txt:?}");
        }
        // A key with no value at all.
        assert!(HostAdvert::parse([("v", None), ("fp", None)]).is_err());
    }

    #[test]
    fn first_key_wins_unknown_keys_and_case_are_tolerated() {
        let fp = "ab".repeat(32);
        let e = [
            ("x-future", Some(&b"?"[..])),
            ("V", Some(b"1")),
            ("v", Some(b"2")),
            ("FP", Some(fp.as_bytes())),
        ];
        let a = HostAdvert::parse(e).unwrap();
        assert_eq!((a.major, a.fingerprint), (1, [0xab; 32]));
    }

    proptest! {
        #[test]
        fn roundtrip(major in any::<u16>(), fingerprint in any::<[u8; 32]>()) {
            let a = HostAdvert { major, fingerprint };
            prop_assert_eq!(HostAdvert::parse(entries(&a.to_txt())), Ok(a));
        }

        // The record comes from the network: whatever it holds, parsing must not panic, and
        // anything it accepts must re-encode to the same values.
        #[test]
        fn arbitrary_records_never_panic(
            raw in proptest::collection::vec(
                (".{0,6}", proptest::option::of(proptest::collection::vec(any::<u8>(), 0..80))),
                0..8,
            ),
            seed_valid in any::<bool>(),
        ) {
            let mut e: Vec<(&str, Option<&[u8]>)> =
                raw.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
            let valid = advert().to_txt();
            if seed_valid {
                e.extend(entries(&valid));
            }
            if let Ok(a) = HostAdvert::parse(e.iter().copied()) {
                let again = a.to_txt();
                prop_assert_eq!(HostAdvert::parse(entries(&again)), Ok(a));
            }
        }
    }
}
