//! The datagram channel (host → client): video fragments and cursor position.
//!
//! Every datagram starts with one kind byte:
//!
//! | kind | body |
//! |---|---|
//! | `0` video fragment | fixed 18-byte header (below), then the fragment payload |
//! | `1` cursor position | postcard [`CursorPosition`], nothing after it |
//!
//! Video fragment header, little-endian, fixed size so every fragment of a frame has the same
//! length (Reed–Solomon needs equal shards) and the payload size follows from the path MTU:
//!
//! | offset | field | type |
//! |---|---|---|
//! | 0 | `frame_id` | u32 |
//! | 4 | `frag_index` | u16 |
//! | 6 | `frag_count` (data + parity) | u16 |
//! | 8 | `fec_parity` (parity fragments, the last ones) | u8 |
//! | 9 | `flags` ([`FrameFlags`]) | u8 |
//! | 10 | `capture_ts_us` (host clock) | u64 |

use crate::{DecodeError, MAX_FRAGMENT_PAYLOAD, MAX_FRAGMENTS};
use serde::{Deserialize, Serialize};

pub const KIND_VIDEO: u8 = 0;
pub const KIND_CURSOR: u8 = 1;

/// Size of the video fragment header, without the kind byte.
pub const FRAGMENT_HEADER_BYTES: usize = 18;
/// Bytes of a video datagram that are not payload (kind byte + header).
pub const VIDEO_DATAGRAM_OVERHEAD: usize = 1 + FRAGMENT_HEADER_BYTES;

/// Per-frame flags. Unknown bits are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FrameFlags(u8);

impl FrameFlags {
    /// The frame can be decoded on its own (IDR or the end of an intra-refresh cycle).
    pub const KEYFRAME: Self = Self(1 << 0);
    /// Reserved for the later still-screen 4:4:4 colour pass (ADR-0005).
    pub const REFINEMENT: Self = Self(1 << 1);
    const KNOWN: u8 = Self::KEYFRAME.0 | Self::REFINEMENT.0;

    pub const fn empty() -> Self {
        Self(0)
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    /// `None` if any unknown bit is set.
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & !Self::KNOWN == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The per-frame fields every fragment of a frame repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameInfo {
    /// Increments by one per frame, wrapping.
    pub frame_id: u32,
    pub flags: FrameFlags,
    /// Host clock, microseconds.
    pub capture_ts_us: u64,
}

/// Header of one video fragment. A decoded header is always internally consistent: see
/// [`decode_datagram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentHeader {
    pub frame_id: u32,
    pub frag_index: u16,
    pub frag_count: u16,
    pub fec_parity: u8,
    pub flags: FrameFlags,
    pub capture_ts_us: u64,
}

impl FragmentHeader {
    /// Number of data (non-parity) fragments.
    pub fn data_count(&self) -> u16 {
        self.frag_count.saturating_sub(u16::from(self.fec_parity))
    }

    /// Checks the header against itself and its payload length; `Err` names the field.
    pub fn check(&self, payload_len: usize) -> Result<(), &'static str> {
        if self.frag_count == 0 || self.frag_count > MAX_FRAGMENTS {
            return Err("fragment.frag_count");
        }
        if u16::from(self.fec_parity) >= self.frag_count {
            return Err("fragment.fec_parity");
        }
        if self.frag_index >= self.frag_count {
            return Err("fragment.frag_index");
        }
        if payload_len == 0 || !payload_len.is_multiple_of(2) || payload_len > MAX_FRAGMENT_PAYLOAD
        {
            return Err("fragment.payload");
        }
        Ok(())
    }

    pub(crate) fn write(&self, out: &mut [u8; VIDEO_DATAGRAM_OVERHEAD]) {
        out[0] = KIND_VIDEO;
        out[1..5].copy_from_slice(&self.frame_id.to_le_bytes());
        out[5..7].copy_from_slice(&self.frag_index.to_le_bytes());
        out[7..9].copy_from_slice(&self.frag_count.to_le_bytes());
        out[9] = self.fec_parity;
        out[10] = self.flags.bits();
        out[11..19].copy_from_slice(&self.capture_ts_us.to_le_bytes());
    }
}

/// Host → client, latest wins: drop any position whose `seq` is not newer than the last one seen.
/// Position of the pointer hotspot in virtual-monitor pixels (may be off-screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct CursorPosition {
    pub seq: u32,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

/// One decoded datagram. Video payload borrows from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datagram<'a> {
    Video {
        header: FragmentHeader,
        payload: &'a [u8],
    },
    Cursor(CursorPosition),
}

/// Decodes and checks one datagram. A video header is accepted only if it is consistent:
/// `1 <= frag_count <= MAX_FRAGMENTS`, at least one data fragment, `frag_index < frag_count`,
/// known flags, and a payload that is non-empty, even and at most [`MAX_FRAGMENT_PAYLOAD`].
pub fn decode_datagram(buf: &[u8]) -> Result<Datagram<'_>, DecodeError> {
    let (&kind, body) = buf.split_first().ok_or(DecodeError::Truncated)?;
    match kind {
        KIND_VIDEO => {
            let (h, payload) = body
                .split_first_chunk::<FRAGMENT_HEADER_BYTES>()
                .ok_or(DecodeError::Truncated)?;
            let header = FragmentHeader {
                frame_id: u32::from_le_bytes([h[0], h[1], h[2], h[3]]),
                frag_index: u16::from_le_bytes([h[4], h[5]]),
                frag_count: u16::from_le_bytes([h[6], h[7]]),
                fec_parity: h[8],
                flags: FrameFlags::from_bits(h[9]).ok_or(DecodeError::Invalid("fragment.flags"))?,
                capture_ts_us: u64::from_le_bytes([
                    h[10], h[11], h[12], h[13], h[14], h[15], h[16], h[17],
                ]),
            };
            header.check(payload.len()).map_err(DecodeError::Invalid)?;
            Ok(Datagram::Video { header, payload })
        }
        KIND_CURSOR => {
            let (pos, rest) = postcard::take_from_bytes::<CursorPosition>(body)?;
            if !rest.is_empty() {
                return Err(DecodeError::TrailingBytes);
            }
            Ok(Datagram::Cursor(pos))
        }
        other => Err(DecodeError::UnknownDatagramKind(other)),
    }
}

/// Writes a cursor-position datagram into `out` (cleared first). Allocation-free once `out` has
/// grown to 16 bytes.
pub fn encode_cursor(pos: &CursorPosition, out: &mut Vec<u8>) {
    out.clear();
    out.push(KIND_CURSOR);
    // Varints: u32 and two i32 take at most 5 bytes each, plus 1 for the bool.
    let mut tmp = [0u8; 16];
    let used = postcard::to_slice(pos, &mut tmp).expect("a CursorPosition is at most 16 bytes");
    out.extend_from_slice(used);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn header_layout_is_fixed() {
        let h = FragmentHeader {
            frame_id: 0x0403_0201,
            frag_index: 0x0605,
            frag_count: 0x0807,
            fec_parity: 0x09,
            flags: FrameFlags::KEYFRAME,
            capture_ts_us: 0x1211_1009_0807_0605,
        };
        let mut out = [0u8; VIDEO_DATAGRAM_OVERHEAD];
        h.write(&mut out);
        assert_eq!(
            out,
            [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 1, 5, 6, 7, 8, 9, 0x10, 0x11, 0x12
            ]
        );
    }

    fn video(h: FragmentHeader, payload: &[u8]) -> Vec<u8> {
        let mut out = [0u8; VIDEO_DATAGRAM_OVERHEAD];
        h.write(&mut out);
        let mut v = out.to_vec();
        v.extend_from_slice(payload);
        v
    }

    const GOOD: FragmentHeader = FragmentHeader {
        frame_id: 7,
        frag_index: 1,
        frag_count: 3,
        fec_parity: 1,
        flags: FrameFlags::empty(),
        capture_ts_us: 99,
    };

    #[test]
    fn inconsistent_headers_are_rejected() {
        let bad = [
            FragmentHeader {
                frag_count: 0,
                frag_index: 0,
                fec_parity: 0,
                ..GOOD
            },
            FragmentHeader {
                frag_count: MAX_FRAGMENTS + 1,
                ..GOOD
            },
            FragmentHeader {
                fec_parity: 3,
                ..GOOD
            },
            FragmentHeader {
                frag_index: 3,
                ..GOOD
            },
        ];
        for h in bad {
            assert!(
                matches!(
                    decode_datagram(&video(h, &[0, 0])),
                    Err(DecodeError::Invalid(_))
                ),
                "{h:?}"
            );
        }
        let mut unknown_flag = video(GOOD, &[0, 0]);
        unknown_flag[10] = 0x80;
        assert_eq!(
            decode_datagram(&unknown_flag),
            Err(DecodeError::Invalid("fragment.flags"))
        );
        for payload in [&[][..], &[0][..], &[0, 0, 0][..]] {
            assert_eq!(
                decode_datagram(&video(GOOD, payload)),
                Err(DecodeError::Invalid("fragment.payload"))
            );
        }
        assert_eq!(decode_datagram(&[]), Err(DecodeError::Truncated));
        assert_eq!(
            decode_datagram(&[KIND_VIDEO; 18]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_datagram(&[9]),
            Err(DecodeError::UnknownDatagramKind(9))
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn cursor_roundtrips(pos: CursorPosition) {
            let mut out = Vec::new();
            encode_cursor(&pos, &mut out);
            prop_assert_eq!(decode_datagram(&out), Ok(Datagram::Cursor(pos)));
        }

        #[test]
        fn video_header_roundtrips(
            frame_id: u32, frag_count in 1..=MAX_FRAGMENTS, idx: proptest::sample::Index,
            parity: u8, keyframe: bool, ts: u64, half_len in 1usize..64,
        ) {
            let fec_parity = parity.min((frag_count - 1).min(255) as u8);
            let flags = if keyframe { FrameFlags::KEYFRAME } else { FrameFlags::empty() };
            let h = FragmentHeader {
                frame_id, frag_index: idx.index(frag_count.into()) as u16, frag_count,
                fec_parity, flags, capture_ts_us: ts,
            };
            let payload = vec![0xA5; half_len * 2];
            let bytes = video(h, &payload);
            prop_assert_eq!(decode_datagram(&bytes), Ok(Datagram::Video { header: h, payload: &payload }));
        }

        /// Arbitrary bytes never panic; an accepted header is always consistent.
        #[test]
        fn arbitrary_datagrams_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
            if let Ok(Datagram::Video { header, payload }) = decode_datagram(&bytes) {
                prop_assert!(header.frag_index < header.frag_count);
                prop_assert!(header.data_count() >= 1);
                prop_assert!(!payload.is_empty() && payload.len() % 2 == 0);
            }
        }
    }
}
