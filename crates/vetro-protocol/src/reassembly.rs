//! Client side: video fragments → complete frames, with Reed–Solomon recovery.
//!
//! The peer is untrusted, so memory is bounded: at most `max_in_flight` frames are buffered, each
//! at most `max_frame_bytes` (parity included). A frame leaves its slot when it completes, when it
//! is older than a frame that just completed (it could only be decoded out of order), when it is
//! still incomplete `frame_timeout_us` after its first fragment (ARCHITECTURE section 7), or when
//! a newer frame needs its slot (fresh beats complete, section 10). Every frame that leaves without
//! completing counts in [`Reassembler::lost_frames`], and so does every frame id skipped entirely
//! (no fragment ever arrived); [`Reassembler::last_lost_frame`] names the newest one for
//! `KeyframeRequest`.
//!
//! The host numbers frames +1 per frame for the whole connection, also across encoder resets. As
//! a safety net, a new frame id more than [`RESTART_WINDOW`] frames *older* than the newest
//! finished frame is taken as a numbering restart: everything in flight is dropped and
//! [`Reassembler::stream_restarts`] grows (request a keyframe). A smaller backwards step is a late
//! fragment and is ignored.
//!
//! Allocation: slot buffers and the Reed–Solomon work space grow to their high-water mark during
//! the first frames and are reused afterwards, so steady-state reassembly allocates nothing.

use crate::MAX_FRAME_BYTES;
use crate::datagram::{FragmentHeader, FrameInfo};
use crate::fragment::LEN_PREFIX;
use reed_solomon_simd::ReedSolomonDecoder;

/// Reassembly limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReassemblerConfig {
    /// Frames buffered at once, clamped to `1..=16`.
    pub max_in_flight: usize,
    /// Largest `frag_count × payload` of one frame, clamped to [`MAX_FRAME_BYTES`].
    pub max_frame_bytes: usize,
    /// A frame still incomplete this long after its first fragment is dropped: one frame interval.
    pub frame_timeout_us: u64,
}

impl ReassemblerConfig {
    pub const DEFAULT_MAX_IN_FLIGHT: usize = 4;

    /// Defaults for a stream at `refresh_mhz` (e.g. 120 000 for 120 Hz): timeout of one frame
    /// interval, 4 frames in flight, [`MAX_FRAME_BYTES`] per frame.
    pub fn for_refresh(refresh_mhz: u32) -> Self {
        Self {
            max_in_flight: Self::DEFAULT_MAX_IN_FLIGHT,
            max_frame_bytes: MAX_FRAME_BYTES,
            frame_timeout_us: 1_000_000_000 / u64::from(refresh_mhz.max(1)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReassemblyError {
    /// The header is not self-consistent (see [`FragmentHeader::check`]); names the field.
    InvalidHeader(&'static str),
    /// The fragment disagrees with earlier fragments of the same frame (count, parity, flags,
    /// timestamp or payload size).
    Inconsistent,
    /// The frame would exceed `max_frame_bytes`, or is too small to hold its length prefix.
    TooLarge,
    /// The frame's length prefix points past its data.
    BadLength,
    /// Reed–Solomon recovery failed.
    Fec(reed_solomon_simd::Error),
}

impl core::fmt::Display for ReassemblyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidHeader(field) => write!(f, "invalid fragment header: {field}"),
            Self::Inconsistent => f.write_str("fragment disagrees with its frame"),
            Self::TooLarge => f.write_str("frame exceeds the size limit"),
            Self::BadLength => f.write_str("frame length prefix out of range"),
            Self::Fec(e) => write!(f, "reed-solomon: {e}"),
        }
    }
}

impl std::error::Error for ReassemblyError {}

/// A complete frame. `data` borrows the reassembler's buffer: hand it to the decoder before the
/// next `push`.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub info: FrameInfo,
    pub data: &'a [u8],
    /// Data fragments rebuilt from parity.
    pub fec_recovered: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PushOutcome<'a> {
    /// Stored; the frame is not complete yet.
    Pending,
    /// This fragment completed its frame.
    Complete(Frame<'a>),
    /// Not needed: a duplicate, or a fragment of a frame that is already complete or dropped.
    Ignored,
}

#[derive(Default)]
struct Slot {
    active: bool,
    info: FrameInfo,
    frag_count: u16,
    parity: u8,
    shard: usize,
    first_seen_us: u64,
    have: Vec<bool>,
    received: u16,
    data_received: u16,
    buf: Vec<u8>,
}

impl Slot {
    fn data_count(&self) -> usize {
        usize::from(self.frag_count - u16::from(self.parity))
    }

    fn start(&mut self, h: &FragmentHeader, shard: usize, now_us: u64) {
        self.active = true;
        self.info = FrameInfo {
            frame_id: h.frame_id,
            flags: h.flags,
            capture_ts_us: h.capture_ts_us,
        };
        self.frag_count = h.frag_count;
        self.parity = h.fec_parity;
        self.shard = shard;
        self.first_seen_us = now_us;
        self.have.clear();
        self.have.reserve_exact(usize::from(h.frag_count));
        self.have.resize(usize::from(h.frag_count), false);
        self.received = 0;
        self.data_received = 0;
        let need = usize::from(h.frag_count) * shard;
        if self.buf.len() < need {
            self.buf.reserve_exact(need - self.buf.len());
            self.buf.resize(need, 0);
        }
    }

    fn matches(&self, h: &FragmentHeader, shard: usize) -> bool {
        self.frag_count == h.frag_count
            && self.parity == h.fec_parity
            && self.info.flags == h.flags
            && self.info.capture_ts_us == h.capture_ts_us
            && self.shard == shard
    }
}

/// A fragment of a frame up to this many frames older than the newest finished one is late and
/// ignored; further back means the host restarted its numbering. 64 frames is over 0.5 s at 120 Hz,
/// far past the one-interval timeout, and bounds the freeze after a host-side reset.
pub const RESTART_WINDOW: u32 = 64;

/// `a` is newer than `b` in wrapping (RFC 1982) order.
fn is_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

pub struct Reassembler {
    cfg: ReassemblerConfig,
    slots: Vec<Slot>,
    decoder: Option<ReedSolomonDecoder>,
    /// Newest frame that left a slot (completed or dropped).
    last_finished: Option<u32>,
    lost: u64,
    last_lost: Option<u32>,
    restarts: u64,
}

impl Reassembler {
    pub fn new(cfg: ReassemblerConfig) -> Self {
        let cfg = ReassemblerConfig {
            max_in_flight: cfg.max_in_flight.clamp(1, 16),
            max_frame_bytes: cfg.max_frame_bytes.min(MAX_FRAME_BYTES),
            frame_timeout_us: cfg.frame_timeout_us,
        };
        Self {
            slots: (0..cfg.max_in_flight).map(|_| Slot::default()).collect(),
            cfg,
            decoder: None,
            last_finished: None,
            lost: 0,
            last_lost: None,
            restarts: 0,
        }
    }

    /// Frames lost since creation: dropped incomplete, or skipped without a single fragment.
    /// When it grows, send `KeyframeRequest { lost_frame_id: last_lost_frame() }`.
    pub fn lost_frames(&self) -> u64 {
        self.lost
    }

    /// Newest lost frame id (wrapping order), if any.
    pub fn last_lost_frame(&self) -> Option<u32> {
        self.last_lost
    }

    /// Times the host's frame numbering jumped backwards (see [`RESTART_WINDOW`]). When it grows,
    /// request a keyframe.
    pub fn stream_restarts(&self) -> u64 {
        self.restarts
    }

    /// Bytes held in frame buffers. Never more than `max_in_flight × (max_frame_bytes +
    /// MAX_FRAGMENTS)`, whatever the peer sends. Not included: the Reed–Solomon decoder work
    /// space, which `reed-solomon-simd` sizes from one frame's shard count and size, so it is
    /// bounded by a small multiple of `max_frame_bytes`.
    pub fn buffered_bytes(&self) -> usize {
        self.slots
            .iter()
            .map(|s| s.buf.capacity() + s.have.capacity())
            .sum()
    }

    /// Drops frames still incomplete `frame_timeout_us` after their first fragment. Call it at
    /// least once per frame interval even when no datagram arrives. Returns how many it dropped.
    pub fn expire(&mut self, now_us: u64) -> u32 {
        let mut dropped = 0;
        for i in 0..self.slots.len() {
            let s = &self.slots[i];
            if s.active && now_us.saturating_sub(s.first_seen_us) > self.cfg.frame_timeout_us {
                self.drop_slot(i);
                dropped += 1;
            }
        }
        dropped
    }

    /// Adds one fragment (from [`crate::datagram::decode_datagram`]). `now_us` is the client's
    /// monotonic clock.
    ///
    /// An `Err` means the peer broke the protocol; the fragment is discarded and, for
    /// `BadLength` and `Fec`, its frame is dropped too.
    pub fn push(
        &mut self,
        now_us: u64,
        header: &FragmentHeader,
        payload: &[u8],
    ) -> Result<PushOutcome<'_>, ReassemblyError> {
        header
            .check(payload.len())
            .map_err(ReassemblyError::InvalidHeader)?;
        self.expire(now_us);
        let shard = payload.len();
        let found = self
            .slots
            .iter()
            .position(|s| s.active && s.info.frame_id == header.frame_id);
        let idx = match found {
            Some(i) => {
                if !self.slots[i].matches(header, shard) {
                    return Err(ReassemblyError::Inconsistent);
                }
                i
            }
            None => {
                if let Some(l) = self.last_finished
                    && !is_newer(header.frame_id, l)
                {
                    if l.wrapping_sub(header.frame_id) <= RESTART_WINDOW {
                        return Ok(PushOutcome::Ignored);
                    }
                    self.restart();
                }
                let count = usize::from(header.frag_count);
                let data = usize::from(header.data_count());
                if count.saturating_mul(shard) > self.cfg.max_frame_bytes
                    || data * shard < LEN_PREFIX
                {
                    return Err(ReassemblyError::TooLarge);
                }
                let Some(i) = self.free_slot(header.frame_id) else {
                    return Ok(PushOutcome::Ignored);
                };
                self.slots[i].start(header, shard, now_us);
                i
            }
        };

        let slot = &mut self.slots[idx];
        let k = usize::from(header.frag_index);
        if slot.have[k] {
            return Ok(PushOutcome::Ignored);
        }
        slot.have[k] = true;
        slot.received += 1;
        let data = slot.data_count();
        if k < data {
            slot.data_received += 1;
        }
        slot.buf[k * shard..(k + 1) * shard].copy_from_slice(payload);

        let missing = data - usize::from(slot.data_received);
        if missing > 0 {
            if usize::from(slot.received) < data {
                return Ok(PushOutcome::Pending);
            }
            if let Err(e) = recover(&mut self.decoder, slot) {
                self.drop_slot(idx);
                return Err(ReassemblyError::Fec(e));
            }
        }

        let slot = &self.slots[idx];
        let region = data * slot.shard;
        let len = u32::from_le_bytes([slot.buf[0], slot.buf[1], slot.buf[2], slot.buf[3]]) as usize;
        if len > region - LEN_PREFIX {
            self.drop_slot(idx);
            return Err(ReassemblyError::BadLength);
        }
        let info = slot.info;
        self.slots[idx].active = false;
        self.retire(info.frame_id);
        // Older frames still in flight could only be decoded out of order now.
        for i in 0..self.slots.len() {
            if self.slots[i].active && is_newer(info.frame_id, self.slots[i].info.frame_id) {
                self.drop_slot(i);
            }
        }
        Ok(PushOutcome::Complete(Frame {
            info,
            data: &self.slots[idx].buf[LEN_PREFIX..LEN_PREFIX + len],
            fec_recovered: missing as u16,
        }))
    }

    /// A free slot for `frame_id`, evicting the oldest frame if all are busy. `None` if
    /// `frame_id` is older than every buffered frame.
    fn free_slot(&mut self, frame_id: u32) -> Option<usize> {
        if let Some(i) = self.slots.iter().position(|s| !s.active) {
            return Some(i);
        }
        let oldest = (0..self.slots.len()).reduce(|a, b| {
            if is_newer(self.slots[a].info.frame_id, self.slots[b].info.frame_id) {
                b
            } else {
                a
            }
        })?;
        if !is_newer(frame_id, self.slots[oldest].info.frame_id) {
            return None;
        }
        self.drop_slot(oldest);
        Some(oldest)
    }

    fn drop_slot(&mut self, i: usize) {
        self.slots[i].active = false;
        self.lost += 1;
        let id = self.slots[i].info.frame_id;
        self.note_lost(id);
        self.retire(id);
    }

    /// `frame_id` left a slot. Ids between the previous newest finished frame and `frame_id`
    /// that never got a slot were lost without a single fragment arriving.
    fn retire(&mut self, frame_id: u32) {
        if let Some(l) = self.last_finished {
            if !is_newer(frame_id, l) {
                return;
            }
            let between = |id: u32| is_newer(id, l) && is_newer(frame_id, id);
            let in_flight = self
                .slots
                .iter()
                .filter(|s| s.active && between(s.info.frame_id))
                .count() as u64;
            // is_newer(frame_id, l) guarantees a difference of at least 1.
            let gap = u64::from(frame_id.wrapping_sub(l) - 1);
            if gap > in_flight {
                self.lost += gap - in_flight;
                let mut id = frame_id.wrapping_sub(1);
                while self.slots.iter().any(|s| s.active && s.info.frame_id == id) {
                    id = id.wrapping_sub(1);
                }
                self.note_lost(id);
            }
        }
        self.last_finished = Some(frame_id);
    }

    fn note_lost(&mut self, id: u32) {
        match self.last_lost {
            Some(l) if !is_newer(id, l) => {}
            _ => self.last_lost = Some(id),
        }
    }

    /// The host's numbering went backwards: forget everything in flight and start over.
    fn restart(&mut self) {
        for s in &mut self.slots {
            if s.active {
                s.active = false;
                self.lost += 1;
            }
        }
        self.last_finished = None;
        self.last_lost = None;
        self.restarts += 1;
    }
}

/// Rebuilds the missing data shards of `slot` from what arrived.
fn recover(
    decoder: &mut Option<ReedSolomonDecoder>,
    slot: &mut Slot,
) -> Result<(), reed_solomon_simd::Error> {
    let data = slot.data_count();
    let parity = usize::from(slot.parity);
    let shard = slot.shard;
    let dec = match decoder {
        Some(d) => {
            d.reset(data, parity, shard)?;
            d
        }
        None => decoder.insert(ReedSolomonDecoder::new(data, parity, shard)?),
    };
    for (i, _) in slot.have.iter().enumerate().filter(|(_, h)| **h) {
        let s = &slot.buf[i * shard..(i + 1) * shard];
        if i < data {
            dec.add_original_shard(i, s)?;
        } else {
            dec.add_recovery_shard(i - data, s)?;
        }
    }
    let result = dec.decode()?;
    for (i, s) in result.restored_original_iter() {
        slot.buf[i * shard..(i + 1) * shard].copy_from_slice(s);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datagram::{Datagram, FrameFlags, decode_datagram};
    use crate::{Fragmenter, MAX_FRAGMENTS};
    use proptest::prelude::*;

    fn info(frame_id: u32) -> FrameInfo {
        FrameInfo {
            frame_id,
            flags: FrameFlags::empty(),
            capture_ts_us: u64::from(frame_id) * 8333,
        }
    }

    fn fragments(id: u32, frame: &[u8], mtu: usize, fec: u8) -> Vec<Vec<u8>> {
        Fragmenter::new()
            .fragment(info(id), frame, mtu, fec)
            .unwrap()
            .map(<[u8]>::to_vec)
            .collect()
    }

    /// Feeds datagrams; returns the frames completed, as owned copies.
    fn feed(r: &mut Reassembler, now: u64, dgrams: &[Vec<u8>]) -> Vec<(FrameInfo, Vec<u8>, u16)> {
        let mut out = Vec::new();
        for d in dgrams {
            let Ok(Datagram::Video { header, payload }) = decode_datagram(d) else {
                panic!("bad datagram");
            };
            if let Ok(PushOutcome::Complete(f)) = r.push(now, &header, payload) {
                out.push((f.info, f.data.to_vec(), f.fec_recovered));
            }
        }
        out
    }

    fn cfg() -> ReassemblerConfig {
        ReassemblerConfig::for_refresh(120_000)
    }

    #[test]
    fn timeout_is_one_frame_interval() {
        assert_eq!(cfg().frame_timeout_us, 8_333);
    }

    #[test]
    fn frames_in_order_all_complete() {
        let mut r = Reassembler::new(cfg());
        for id in 0..10u32 {
            let frame = vec![id as u8; 3000 + id as usize];
            let got = feed(
                &mut r,
                u64::from(id) * 8000,
                &fragments(id, &frame, 1200, 10),
            );
            assert_eq!(got, vec![(info(id), frame, 0)]);
        }
        assert_eq!(r.lost_frames(), 0);
    }

    #[test]
    fn skipped_frame_counts_as_lost() {
        let mut r = Reassembler::new(cfg());
        assert_eq!(feed(&mut r, 0, &fragments(1, &[1; 10], 1200, 0)).len(), 1);
        assert_eq!(r.last_lost_frame(), None);
        assert_eq!(feed(&mut r, 0, &fragments(3, &[3; 10], 1200, 0)).len(), 1);
        assert_eq!(r.lost_frames(), 1);
        assert_eq!(r.last_lost_frame(), Some(2));
    }

    #[test]
    fn skipped_frames_across_the_wrap_count_as_lost() {
        let mut r = Reassembler::new(cfg());
        feed(&mut r, 0, &fragments(u32::MAX - 1, &[1; 10], 1200, 0));
        feed(&mut r, 0, &fragments(1, &[1; 10], 1200, 0));
        assert_eq!(r.lost_frames(), 2);
        assert_eq!(r.last_lost_frame(), Some(0));
    }

    #[test]
    fn skipped_frame_is_not_double_counted_with_an_in_flight_one() {
        let mut r = Reassembler::new(cfg());
        feed(&mut r, 0, &fragments(1, &[1; 10], 1200, 0));
        let two = fragments(2, &[2; 5000], 1200, 0);
        assert!(feed(&mut r, 0, &two[..1]).is_empty());
        // 2 is in flight (dropped when 4 completes), 3 never arrived: two lost, newest 3.
        feed(&mut r, 0, &fragments(4, &[4; 10], 1200, 0));
        assert_eq!(r.lost_frames(), 2);
        assert_eq!(r.last_lost_frame(), Some(3));
    }

    #[test]
    fn host_numbering_restart_is_detected() {
        let mut r = Reassembler::new(cfg());
        for id in 1000..1004 {
            feed(&mut r, 0, &fragments(id, &[1; 10], 1200, 0));
        }
        // Within the window: a late frame, ignored.
        let late = 1003 - RESTART_WINDOW;
        assert!(feed(&mut r, 0, &fragments(late, &[1; 10], 1200, 0)).is_empty());
        assert_eq!(r.stream_restarts(), 0);
        // Further back: the host restarted its numbering; frames flow again.
        assert_eq!(feed(&mut r, 0, &fragments(0, &[7; 10], 1200, 0)).len(), 1);
        assert_eq!(r.stream_restarts(), 1);
        assert_eq!(feed(&mut r, 0, &fragments(1, &[7; 10], 1200, 0)).len(), 1);
        assert_eq!(r.lost_frames(), 0);
    }

    #[test]
    fn frame_ids_wrap() {
        let mut r = Reassembler::new(cfg());
        for id in [u32::MAX - 1, u32::MAX, 0, 1] {
            assert_eq!(
                feed(&mut r, 0, &fragments(id, &[1, 2, 3], 1200, 0)).len(),
                1
            );
        }
    }

    #[test]
    fn late_fragments_of_a_finished_frame_are_ignored() {
        let mut r = Reassembler::new(cfg());
        let d = fragments(1, &[7; 100], 1200, 50);
        assert_eq!(feed(&mut r, 0, &d[..1]).len(), 1);
        let Ok(Datagram::Video { header, payload }) = decode_datagram(&d[1]) else {
            panic!()
        };
        assert_eq!(r.push(0, &header, payload), Ok(PushOutcome::Ignored));
    }

    #[test]
    fn incomplete_frame_times_out() {
        let mut r = Reassembler::new(cfg());
        let d = fragments(1, &[7; 5000], 1200, 0);
        assert!(feed(&mut r, 0, &d[..2]).is_empty());
        assert_eq!(r.expire(8_333), 0);
        assert_eq!(r.expire(8_334), 1);
        assert_eq!(r.lost_frames(), 1);
        // The rest arrives too late.
        assert!(feed(&mut r, 8_400, &d[2..]).is_empty());
    }

    #[test]
    fn completed_frame_drops_older_incomplete_ones() {
        let mut r = Reassembler::new(cfg());
        let one = fragments(1, &[1; 5000], 1200, 0);
        let two = fragments(2, &[2; 100], 1200, 0);
        assert!(feed(&mut r, 0, &one[..2]).is_empty());
        assert_eq!(feed(&mut r, 0, &two).len(), 1);
        assert_eq!(r.lost_frames(), 1);
        assert!(feed(&mut r, 0, &one[2..]).is_empty());
    }

    #[test]
    fn full_slots_evict_the_oldest() {
        let mut r = Reassembler::new(ReassemblerConfig {
            max_in_flight: 2,
            ..cfg()
        });
        let frames: Vec<_> = (1..=3)
            .map(|id| fragments(id, &[0; 5000], 1200, 0))
            .collect();
        for f in &frames {
            assert!(feed(&mut r, 0, &f[..1]).is_empty());
        }
        assert_eq!(r.lost_frames(), 1);
        // Frame 1 was evicted; 2 and 3 still complete.
        assert!(feed(&mut r, 0, &frames[0][1..]).is_empty());
        assert_eq!(feed(&mut r, 0, &frames[1][1..]).len(), 1);
        assert_eq!(feed(&mut r, 0, &frames[2][1..]).len(), 1);
    }

    #[test]
    fn inconsistent_fragment_is_rejected() {
        let mut r = Reassembler::new(cfg());
        let d = fragments(1, &[0; 5000], 1200, 0);
        assert!(feed(&mut r, 0, &d[..1]).is_empty());
        let Ok(Datagram::Video {
            mut header,
            payload,
        }) = decode_datagram(&d[1])
        else {
            panic!()
        };
        header.capture_ts_us += 1;
        assert_eq!(
            r.push(0, &header, payload),
            Err(ReassemblyError::Inconsistent)
        );
        let Ok(Datagram::Video { header, payload }) = decode_datagram(&d[1]) else {
            panic!()
        };
        assert_eq!(
            r.push(0, &header, &payload[..payload.len() - 2]),
            Err(ReassemblyError::Inconsistent)
        );
    }

    #[test]
    fn oversized_frame_is_refused_without_buffering() {
        let mut r = Reassembler::new(ReassemblerConfig {
            max_frame_bytes: 64 * 1024,
            ..cfg()
        });
        let header = FragmentHeader {
            frame_id: 1,
            frag_index: 0,
            frag_count: MAX_FRAGMENTS,
            fec_parity: 0,
            flags: FrameFlags::empty(),
            capture_ts_us: 0,
        };
        assert_eq!(
            r.push(0, &header, &[0; 1200]),
            Err(ReassemblyError::TooLarge)
        );
        assert_eq!(r.buffered_bytes(), 0);
    }

    #[test]
    fn bad_length_prefix_is_rejected() {
        let mut r = Reassembler::new(cfg());
        let header = FragmentHeader {
            frame_id: 1,
            frag_index: 0,
            frag_count: 1,
            fec_parity: 0,
            flags: FrameFlags::empty(),
            capture_ts_us: 0,
        };
        assert_eq!(
            r.push(0, &header, &[5, 0, 0, 0, 1, 2]),
            Err(ReassemblyError::BadLength)
        );
        assert_eq!(r.lost_frames(), 1);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Any order, duplicates, and up to `parity` lost fragments: the exact frame comes out once.
        #[test]
        fn fec_recovers_up_to_parity_losses(
            frame in proptest::collection::vec(any::<u8>(), 0..20_000),
            mtu in 40usize..1500,
            fec in 0u8..=50,
            seed: u64,
            lose: proptest::sample::Index,
            dup: proptest::sample::Index,
        ) {
            let mut d = fragments(9, &frame, mtu, fec);
            let parity = usize::from(match decode_datagram(&d[0]) {
                Ok(Datagram::Video { header, .. }) => header.fec_parity,
                _ => unreachable!(),
            });
            let data = d.len() - parity;
            shuffle(&mut d, seed);
            let lost = lose.index(parity + 1);
            d.truncate(d.len() - lost);
            let again = d[dup.index(d.len())].clone();
            d.push(again);
            let mut r = Reassembler::new(cfg());
            let got = feed(&mut r, 0, &d);
            prop_assert_eq!(got.len(), 1);
            prop_assert_eq!(&got[0].1, &frame);
            prop_assert_eq!(got[0].0, info(9));
            prop_assert!(usize::from(got[0].2) <= parity.min(data));
            prop_assert_eq!(r.lost_frames(), 0);
        }

        /// One loss more than parity: no frame (never a wrong one), and it counts as lost.
        #[test]
        fn too_many_losses_never_yield_a_frame(
            frame in proptest::collection::vec(any::<u8>(), 2_000..20_000),
            fec in 0u8..=30,
            seed: u64,
        ) {
            let mut d = fragments(3, &frame, 1200, fec);
            let parity = usize::from(match decode_datagram(&d[0]) {
                Ok(Datagram::Video { header, .. }) => header.fec_parity,
                _ => unreachable!(),
            });
            shuffle(&mut d, seed);
            d.truncate(d.len() - parity - 1);
            let mut r = Reassembler::new(cfg());
            prop_assert!(feed(&mut r, 0, &d).is_empty());
            prop_assert_eq!(r.expire(1_000_000), 1);
            prop_assert_eq!(r.lost_frames(), 1);
        }

        /// Hostile fragment streams: no panic, memory stays within the bound, frames within limits.
        #[test]
        fn hostile_fragments_stay_bounded(
            frags in proptest::collection::vec(
                (
                    0u32..6, 0u16..40, 0u16..40, 0u8..40, 0u64..3, 0usize..1400, 0u64..20_000,
                    // Mostly no jump; sometimes a large gap forward, half the id space, or backwards.
                    proptest::sample::select(vec![0u32, 0, 0, 0, 1 << 20, 1 << 31, u32::MAX - 100]),
                ),
                0..300,
            ),
            big in any::<bool>(),
        ) {
            let cfg = ReassemblerConfig { max_in_flight: 3, max_frame_bytes: 256 * 1024, frame_timeout_us: 8_333 };
            let mut r = Reassembler::new(cfg);
            let mut now = 0;
            let mut base = 0u32;
            let bound = cfg.max_in_flight * (cfg.max_frame_bytes + usize::from(MAX_FRAGMENTS));
            for (off, idx, count, parity, ts, len, dt, jump) in frags {
                now += dt;
                base = base.wrapping_add(jump);
                let id = base.wrapping_add(off);
                // Small counts collide and complete often; `big` also tries huge announced frames.
                let count = if big && off == 5 { MAX_FRAGMENTS - count } else { count };
                let header = FragmentHeader {
                    frame_id: id,
                    frag_index: idx,
                    frag_count: count,
                    fec_parity: parity,
                    flags: FrameFlags::empty(),
                    capture_ts_us: ts,
                };
                let payload = vec![(idx as u8) | 1; len];
                if let Ok(PushOutcome::Complete(f)) = r.push(now, &header, &payload) {
                    prop_assert!(f.data.len() <= cfg.max_frame_bytes);
                }
                prop_assert!(r.buffered_bytes() <= bound);
            }
        }
    }

    fn shuffle<T>(v: &mut [T], mut seed: u64) {
        for i in (1..v.len()).rev() {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.swap(i, (seed >> 33) as usize % (i + 1));
        }
    }
}
