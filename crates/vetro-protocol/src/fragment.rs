//! Host side: one encoded frame → equal-size video datagrams, data then Reed–Solomon parity.
//!
//! The data fragments carry `[frame_len: u32 LE][frame bytes][zero padding]`, cut into
//! `data_count` shards of `shard_bytes` each (even, as Reed–Solomon needs). Then `fec_parity`
//! parity shards follow. The shard size is balanced so padding stays under two bytes per shard.

use crate::datagram::{FragmentHeader, FrameInfo, VIDEO_DATAGRAM_OVERHEAD};
use crate::{MAX_FRAGMENT_PAYLOAD, MAX_FRAGMENTS, MAX_FRAME_BYTES, MAX_PARITY_FRAGMENTS};
use reed_solomon_simd::ReedSolomonEncoder;

pub(crate) const LEN_PREFIX: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum FragmentError {
    /// The path's datagram size leaves no room for a 2-byte payload after the 19-byte overhead.
    DatagramTooSmall { max_datagram_bytes: usize },
    /// The frame needs more than [`MAX_FRAGMENTS`] fragments or [`MAX_FRAME_BYTES`] of payload.
    FrameTooLarge { frame_bytes: usize },
    /// Reed–Solomon refused the shard layout (not expected within the limits above).
    Fec(reed_solomon_simd::Error),
}

impl core::fmt::Display for FragmentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DatagramTooSmall { max_datagram_bytes } => {
                write!(f, "datagram size {max_datagram_bytes} too small")
            }
            Self::FrameTooLarge { frame_bytes } => {
                write!(f, "frame of {frame_bytes} bytes too large")
            }
            Self::Fec(e) => write!(f, "reed-solomon: {e}"),
        }
    }
}

impl std::error::Error for FragmentError {}

impl From<reed_solomon_simd::Error> for FragmentError {
    fn from(e: reed_solomon_simd::Error) -> Self {
        Self::Fec(e)
    }
}

/// Parity fragments for `data_count` data fragments at `fec_percent`: rounded up, so any
/// non-zero percentage protects against at least one lost fragment; capped at 255.
pub fn parity_count(data_count: usize, fec_percent: u8) -> usize {
    (data_count * usize::from(fec_percent))
        .div_ceil(100)
        .min(usize::from(MAX_PARITY_FRAGMENTS))
}

/// Reusable fragmenter. Keep one per session: after the first frames, fragmenting allocates
/// nothing (the datagram buffer and the Reed–Solomon work space only grow to their high-water
/// mark).
#[derive(Default)]
pub struct Fragmenter {
    buf: Vec<u8>,
    encoder: Option<ReedSolomonEncoder>,
}

impl Fragmenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cuts `frame` into datagrams of at most `max_datagram_bytes` (the path's current maximum
    /// datagram size) with `fec_percent` parity (0 on USB, 5–25 on Wi-Fi).
    ///
    /// Every returned datagram has the same length and is ready to send as is.
    pub fn fragment(
        &mut self,
        info: FrameInfo,
        frame: &[u8],
        max_datagram_bytes: usize,
        fec_percent: u8,
    ) -> Result<core::slice::ChunksExact<'_, u8>, FragmentError> {
        let max_shard = max_datagram_bytes
            .saturating_sub(VIDEO_DATAGRAM_OVERHEAD)
            .min(MAX_FRAGMENT_PAYLOAD)
            & !1;
        if max_shard == 0 {
            return Err(FragmentError::DatagramTooSmall { max_datagram_bytes });
        }
        let too_large = FragmentError::FrameTooLarge {
            frame_bytes: frame.len(),
        };
        if frame.len() > MAX_FRAME_BYTES {
            return Err(too_large);
        }
        let total = LEN_PREFIX + frame.len();
        let data = total.div_ceil(max_shard);
        let shard = total.div_ceil(data).next_multiple_of(2);
        let parity = parity_count(data, fec_percent);
        let count = data + parity;
        if count > usize::from(MAX_FRAGMENTS) || count * shard > MAX_FRAME_BYTES {
            return Err(too_large);
        }

        let stride = VIDEO_DATAGRAM_OVERHEAD + shard;
        let n = count * stride;
        if self.buf.len() < n {
            self.buf.resize(n, 0);
        }
        let buf = &mut self.buf[..n];
        // frame.len() <= MAX_FRAME_BYTES, so these casts are lossless (checked above).
        let prefix = (frame.len() as u32).to_le_bytes();
        let mut header = FragmentHeader {
            frame_id: info.frame_id,
            frag_index: 0,
            frag_count: count as u16,
            fec_parity: parity as u8,
            flags: info.flags,
            capture_ts_us: info.capture_ts_us,
        };
        for (i, dgram) in buf.chunks_exact_mut(stride).enumerate() {
            header.frag_index = i as u16;
            let (h, payload) = dgram
                .split_first_chunk_mut::<VIDEO_DATAGRAM_OVERHEAD>()
                .expect("stride is larger than the header");
            header.write(h);
            if i < data {
                fill(payload, i * shard, &prefix, frame);
            }
        }

        if parity > 0 {
            let mut enc = match self.encoder.take() {
                Some(mut e) => {
                    e.reset(data, parity, shard)?;
                    e
                }
                None => ReedSolomonEncoder::new(data, parity, shard)?,
            };
            for d in buf.chunks_exact(stride).take(data) {
                enc.add_original_shard(&d[VIDEO_DATAGRAM_OVERHEAD..])?;
            }
            let rec = enc.encode()?;
            for (r, d) in rec
                .recovery_iter()
                .zip(buf.chunks_exact_mut(stride).skip(data))
            {
                d[VIDEO_DATAGRAM_OVERHEAD..].copy_from_slice(r);
            }
            drop(rec);
            self.encoder = Some(enc);
        }
        Ok(self.buf[..n].chunks_exact(stride))
    }
}

/// Fills `dst` with bytes `pos..pos + dst.len()` of the stream `prefix ++ frame ++ zeros`.
fn fill(dst: &mut [u8], pos: usize, prefix: &[u8; LEN_PREFIX], frame: &[u8]) {
    let mut out = 0;
    let mut pos = pos;
    if pos < LEN_PREFIX {
        let n = (LEN_PREFIX - pos).min(dst.len());
        dst[..n].copy_from_slice(&prefix[pos..pos + n]);
        out = n;
        pos += n;
        if out == dst.len() {
            return;
        }
    }
    let start = (pos - LEN_PREFIX).min(frame.len());
    let n = (frame.len() - start).min(dst.len() - out);
    dst[out..out + n].copy_from_slice(&frame[start..start + n]);
    dst[out + n..].fill(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datagram::{Datagram, FrameFlags, decode_datagram};

    const INFO: FrameInfo = FrameInfo {
        frame_id: 5,
        flags: FrameFlags::KEYFRAME,
        capture_ts_us: 1234,
    };

    #[test]
    fn parity_rounds_up_and_caps() {
        assert_eq!(parity_count(10, 0), 0);
        assert_eq!(parity_count(1, 5), 1);
        assert_eq!(parity_count(20, 5), 1);
        assert_eq!(parity_count(21, 5), 2);
        assert_eq!(parity_count(100, 25), 25);
        assert_eq!(parity_count(8000, 25), 255);
    }

    #[test]
    fn datagrams_are_equal_size_and_within_the_limit() {
        let frame: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let mut f = Fragmenter::new();
        let dgrams: Vec<&[u8]> = f.fragment(INFO, &frame, 1200, 10).unwrap().collect();
        let len = dgrams[0].len();
        assert!(len <= 1200);
        assert!(dgrams.iter().all(|d| d.len() == len));
        let data = (frame.len() + 4).div_ceil(1200 - 19 - 1);
        assert_eq!(dgrams.len(), data + parity_count(data, 10));
        for (i, d) in dgrams.iter().enumerate() {
            let Ok(Datagram::Video { header, .. }) = decode_datagram(d) else {
                panic!("fragment {i} does not decode");
            };
            assert_eq!(usize::from(header.frag_index), i);
            assert_eq!(usize::from(header.frag_count), dgrams.len());
            assert_eq!(header.frame_id, INFO.frame_id);
        }
    }

    #[test]
    fn empty_frame_is_one_fragment() {
        let mut f = Fragmenter::new();
        assert_eq!(f.fragment(INFO, &[], 1200, 0).unwrap().count(), 1);
    }

    #[test]
    fn limits_are_enforced() {
        let mut f = Fragmenter::new();
        assert_eq!(
            f.fragment(INFO, &[1], 20, 0).err(),
            Some(FragmentError::DatagramTooSmall {
                max_datagram_bytes: 20
            })
        );
        let big = vec![0u8; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            f.fragment(INFO, &big, 1200, 0),
            Err(FragmentError::FrameTooLarge { .. })
        ));
        // Fits in bytes but needs more than MAX_FRAGMENTS fragments of 2 bytes.
        let many = vec![0u8; 2 * usize::from(MAX_FRAGMENTS)];
        assert!(matches!(
            f.fragment(INFO, &many, 21, 0),
            Err(FragmentError::FrameTooLarge { .. })
        ));
    }
}
