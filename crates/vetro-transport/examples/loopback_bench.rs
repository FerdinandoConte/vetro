//! Loopback micro-benchmark of the video datagram path: host fragments and sends a frame,
//! client reassembles it; reports per-frame time from "send_frame called" to "frame complete".
//!
//! Run only when `tools/idle-guard.ps1` says `bench_ok` (CLAUDE.md), otherwise the numbers are
//! noisy:
//!
//! ```text
//! cargo run --release -p vetro-transport --example loopback_bench -- [frames] [bytes] [fec%]
//! ```

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vetro_protocol::datagram::{Datagram, FrameFlags, FrameInfo, decode_datagram};
use vetro_protocol::{Fragmenter, PushOutcome, Reassembler, ReassemblerConfig};
use vetro_transport::{Client, Host, Identity};

fn now_us() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

fn arg(i: usize, default: usize) -> usize {
    std::env::args()
        .nth(i)
        .and_then(|a| a.parse().ok())
        .unwrap_or(default)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let frames = arg(1, 2400) as u32; // 20 s at 120 fps
    let bytes = arg(2, 52_000); // ~50 Mbit/s at 120 fps
    let fec = arg(3, 0) as u8;
    let local: SocketAddr = "127.0.0.1:0".parse().unwrap();

    let host_id = Identity::generate().unwrap();
    let client_id = Identity::generate().unwrap();
    let host = Host::bind(local, &host_id, [client_id.fingerprint()]).unwrap();
    let addr = host.local_addr().unwrap();
    let client = Client::bind(local, &client_id).unwrap();
    let (h, c) = tokio::join!(
        async { host.accept().await.unwrap().establish().await },
        client.connect(addr, host_id.fingerprint(), None),
    );
    let (h, c) = (h.expect("host setup"), c.expect("client setup"));

    let rx = tokio::spawn(async move {
        let mut reassembler = Reassembler::new(ReassemblerConfig::for_refresh(120_000));
        let mut lat = Vec::with_capacity(frames as usize);
        while let Ok(Ok(dgram)) =
            tokio::time::timeout(Duration::from_millis(500), c.read_datagram()).await
        {
            let Ok(Datagram::Video { header, payload }) = decode_datagram(&dgram) else {
                continue;
            };
            let now = now_us();
            if let Ok(PushOutcome::Complete(f)) = reassembler.push(now, &header, payload) {
                lat.push(now - f.info.capture_ts_us);
                if f.info.frame_id + 1 == frames {
                    break;
                }
            }
        }
        (lat, reassembler.lost_frames())
    });

    let frame = vec![0x5au8; bytes];
    let mut fragmenter = Fragmenter::new();
    let mut tick = tokio::time::interval(Duration::from_micros(8333));
    let mut datagrams = 0;
    let mut send_errors = 0;
    for frame_id in 0..frames {
        tick.tick().await;
        let info = FrameInfo {
            frame_id,
            flags: FrameFlags::empty(),
            capture_ts_us: now_us(),
        };
        match h.send_frame(&mut fragmenter, info, &frame, fec) {
            Ok(n) => datagrams = n,
            // E.g. the path MTU shrank mid-frame: the frame is lost, the session goes on.
            Err(e) => {
                send_errors += 1;
                eprintln!("frame {frame_id} not sent: {e}");
            }
        }
    }
    let (mut lat, lost) = rx.await.unwrap();
    lat.sort_unstable();
    let pct = |p: usize| lat.get((lat.len() * p / 100).min(lat.len().saturating_sub(1)));
    println!(
        "frames={frames} bytes={bytes} fec={fec}% datagrams/frame={datagrams} send_errors={send_errors} received={} lost={lost}",
        lat.len()
    );
    println!(
        "send->complete us: p50={:?} p95={:?} p99={:?} max={:?}",
        pct(50),
        pct(95),
        pct(99),
        lat.last()
    );
}
