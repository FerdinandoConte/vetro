//! Micro-benchmark: fragment + reassemble one frame, with and without FEC recovery.
//! `cargo run --release -p vetro-protocol --example fec_bench`. Run only when idle-guard says
//! `bench_ok`; otherwise mark the numbers noisy.
#![allow(clippy::disallowed_types)] // the benchmark needs a clock; the library never does.

use std::time::Instant;
use vetro_protocol::datagram::{Datagram, FrameFlags, FrameInfo, decode_datagram};
use vetro_protocol::{Fragmenter, PushOutcome, Reassembler, ReassemblerConfig};

fn main() {
    const MTU: usize = 1200;
    const ITERS: u32 = 500;
    let mut fragmenter = Fragmenter::new();
    let mut reassembler = Reassembler::new(ReassemblerConfig::for_refresh(120_000));
    let mut next_id = 0u32;
    // 100 KB ≈ one HEVC P-frame at 2960×1848, 100 Mbit/s, 120 fps; 1 MB ≈ a large IDR.
    for frame_bytes in [100_000usize, 1_000_000] {
        let frame: Vec<u8> = (0..frame_bytes).map(|i| (i * 31) as u8).collect();
        for (fec, lose_every) in [(0u8, 0usize), (10, 0), (10, 20), (25, 6)] {
            let (mut t_frag, mut t_reasm) = (0u128, 0u128);
            for i in 0..ITERS {
                let info = FrameInfo {
                    frame_id: next_id,
                    flags: FrameFlags::empty(),
                    capture_ts_us: 0,
                };
                next_id += 1;
                let t0 = Instant::now();
                let dgrams: Vec<&[u8]> = fragmenter
                    .fragment(info, &frame, MTU, fec)
                    .unwrap()
                    .collect();
                t_frag += t0.elapsed().as_nanos();
                let dgrams: Vec<Vec<u8>> = dgrams.into_iter().map(<[u8]>::to_vec).collect();
                let t1 = Instant::now();
                let mut done = false;
                for (k, d) in dgrams.iter().enumerate() {
                    if lose_every > 0 && k % lose_every == 0 {
                        continue;
                    }
                    let Ok(Datagram::Video { header, payload }) = decode_datagram(d) else {
                        unreachable!()
                    };
                    if let Ok(PushOutcome::Complete(f)) =
                        reassembler.push(u64::from(next_id) * 8_333, &header, payload)
                    {
                        assert_eq!(f.data.len(), frame.len());
                        done = true;
                        break;
                    }
                }
                t_reasm += t1.elapsed().as_nanos();
                assert!(done, "frame {i} not recovered");
            }
            let us = |ns: u128| ns as f64 / f64::from(ITERS) / 1000.0;
            let loss = if lose_every > 0 {
                100.0 / lose_every as f64
            } else {
                0.0
            };
            println!(
                "frame {frame_bytes:>8} B  fec {fec:>2}%  loss {loss:>4.1}%  fragment {:>8.1} us  reassemble {:>8.1} us",
                us(t_frag),
                us(t_reasm)
            );
        }
    }
}
