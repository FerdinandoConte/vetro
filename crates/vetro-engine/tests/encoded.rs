//! The real encoder in the engine (`--test-pattern gpu-bars`): GPU bars → FFmpeg hardware
//! encoder (zero-copy) → QUIC over 127.0.0.1 → reassembly, with codec negotiation and rate
//! control from `LinkFeedback`. Windows with FFmpeg (`FFMPEG_DIR`) and a hardware encoder;
//! elsewhere, or without `FFMPEG_DIR`, the tests print why and pass.
//!
//! `bench_host_stages` (ignored) is the M1-7 micro-benchmark: host stage percentiles of the
//! M1-4 synthetic path against the encoded path at the reference mode. Run it only when
//! `tools/idle-guard.ps1` says `bench_ok`:
//! `cargo test --release -p vetro-engine --test encoded -- --ignored --nocapture`.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vetro_engine::{Config, Engine, TestPattern};
use vetro_protocol::datagram::{Datagram, FrameFlags, decode_datagram};
use vetro_protocol::messages::{
    Capabilities, CodecSupport, ControlMessage, Display, LinkFeedback, Profile, TelemetryMessage,
};
use vetro_protocol::{PushOutcome, Reassembler, ReassemblerConfig};
use vetro_transport::{Client, ClientSession, Identity};

const WAIT: Duration = Duration::from_secs(20);

fn have_ffmpeg() -> bool {
    let ok = std::env::var_os("FFMPEG_DIR").is_some();
    if !ok {
        eprintln!("skipped: FFMPEG_DIR is not set (tools/fetch-ffmpeg.ps1)");
    }
    ok
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vetro-enc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Running {
    session: ClientSession,
    configure: vetro_protocol::messages::Configure,
    stop: tokio::sync::oneshot::Sender<()>,
    run: tokio::task::JoinHandle<Result<(), vetro_transport::Error>>,
    _client: Client,
}

/// Starts an engine with `pattern` and a client that asks for `width`×`height` @ `hz` with
/// `codecs`.
async fn start(
    pattern: TestPattern,
    tele: &Path,
    (width, height, hz): (u32, u32, u32),
    codecs: &[Profile],
) -> Running {
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        telemetry_out: Some(tele.to_owned()),
        pattern,
        ..Config::default()
    };
    let engine = Engine::bind(&cfg, &Identity::generate().unwrap()).unwrap();
    let (addr, fp, token) = (
        engine.local_addr().unwrap(),
        engine.fingerprint(),
        engine.start_pairing().unwrap(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let run = tokio::spawn(engine.run(async {
        let _ = stopped.await;
    }));
    let client = Client::bind(
        "127.0.0.1:0".parse().unwrap(),
        &Identity::generate().unwrap(),
    )
    .unwrap();
    let mut session = client.connect(addr, fp, Some(token)).await.unwrap();
    let caps = Capabilities {
        display: Some(Display {
            width,
            height,
            refresh_mhz: hz * 1000,
            density_dpi: 274,
        }),
        codecs: codecs
            .iter()
            .map(|&profile| CodecSupport {
                profile,
                max_width: 4096,
                max_height: 4096,
                max_refresh_mhz: 240_000,
            })
            .collect(),
    };
    session
        .control_tx
        .send(&ControlMessage::Capabilities(caps))
        .await
        .unwrap();
    let Some(ControlMessage::Configure(configure)) =
        tokio::time::timeout(WAIT, session.control_rx.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("host must answer Capabilities with Configure");
    };
    Running {
        session,
        configure,
        stop,
        run,
        _client: client,
    }
}

impl Running {
    /// Complete frames as `(frame_id, keyframe, bytes)`, until `n` arrived or `for_` passed.
    /// Without `keep` the bytes are dropped (long benchmark runs).
    async fn frames(&mut self, n: usize, for_: Duration, keep: bool) -> Vec<(u32, bool, Vec<u8>)> {
        let mut r = Reassembler::new(ReassemblerConfig {
            frame_timeout_us: 10_000_000,
            ..ReassemblerConfig::for_refresh(self.configure.refresh_mhz)
        });
        let mut got = Vec::new();
        let deadline = Instant::now() + for_;
        while got.len() < n {
            let Ok(d) =
                tokio::time::timeout_at(deadline.into(), self.session.read_datagram()).await
            else {
                break;
            };
            let d = d.unwrap();
            let Ok(Datagram::Video { header, payload }) = decode_datagram(&d) else {
                continue;
            };
            if let PushOutcome::Complete(f) = r.push(0, &header, payload).unwrap() {
                let key = f.info.flags.contains(FrameFlags::KEYFRAME);
                let data = if keep { f.data.to_vec() } else { Vec::new() };
                got.push((f.info.frame_id, key, data));
            }
        }
        got
    }

    async fn feedback(&mut self, delay_gradient_us: i32, received_kbps: u32) {
        let fb = TelemetryMessage::LinkFeedback(LinkFeedback {
            interval_us: 100_000,
            delay_gradient_us,
            loss_permille: 0,
            received_kbps,
            frames_lost: 0,
        });
        self.session.telemetry_tx.send(&fb).await.unwrap();
    }

    async fn stop(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, self.run)
            .await
            .expect("engine stops promptly")
            .unwrap()
            .unwrap();
    }
}

/// HEVC NAL unit types in an Annex-B access unit.
fn nal_types(au: &[u8]) -> Vec<u8> {
    au.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| (w[3] >> 1) & 0x3f)
        .collect()
}

fn field(line: &str, name: &str) -> u64 {
    let key = format!("\"{name}\":");
    let start = line.find(&key).unwrap() + key.len();
    let digits: String = line[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn gpu_bars_stream_real_hevc_and_follow_rate_control() {
    if !have_ffmpeg() {
        return;
    }
    let dir = scratch("gpu");
    let tele = dir.join("t.jsonl");
    // AV1 first: the host must fall back to HEVC where it cannot encode AV1 (Alder Lake).
    let mut e = start(
        TestPattern::GpuBars,
        &tele,
        (1280, 720, 60),
        &[Profile::Av1Main, Profile::HevcMain, Profile::H264High],
    )
    .await;
    let negotiated = e.configure.profile;
    assert!(
        matches!(negotiated, Profile::Av1Main | Profile::HevcMain),
        "{negotiated:?}"
    );

    let first = e.frames(30, WAIT, true).await;
    assert_eq!(first.len(), 30);
    let (id, key, au) = &first[0];
    assert!(*key && *id == 0, "the stream opens with a keyframe");
    if negotiated == Profile::HevcMain {
        let t = nal_types(au);
        assert!(
            [32, 33, 34].iter().all(|n| t.contains(n)),
            "VPS/SPS/PPS in {t:?}"
        );
    }
    // A keyframe without a request only follows a frame the mailbox dropped (an id gap
    // right before it, ARCHITECTURE section 5.5).
    let ids: Vec<(u32, bool)> = first.iter().map(|f| (f.0, f.1)).collect();
    for i in 1..ids.len() {
        if ids[i].1 {
            let gap = (1..=2).any(|k| i > k && ids[i - k].0 != ids[i - k - 1].0 + 1)
                || ids[i].0 != ids[i - 1].0 + 1;
            assert!(gap, "keyframe without a request or a drop: {ids:?}");
        }
    }

    // Queues building at 5 Mbit/s delivered: rate control backs off to 0.85 × 5000 after a
    // replacement encoder is swapped in (QSV), which shows as a keyframe.
    for _ in 0..3 {
        e.feedback(8_000, 5_000).await;
    }
    let later = e.frames(120, Duration::from_secs(3), true).await;
    assert!(
        later.iter().any(|f| f.1),
        "the retargeted encoder starts with an IDR"
    );
    e.stop().await;

    let text = std::fs::read_to_string(&tele).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let rates: Vec<u64> = lines.iter().map(|l| field(l, "bitrate_kbps")).collect();
    assert_eq!(rates[0], 50_000);
    assert!(
        rates.iter().any(|&r| r < 50_000 / 2),
        "bitrate followed the feedback: {:?}",
        rates.last()
    );
    // Encode time is the hardware encoder's, not zero.
    let encode: Vec<u64> = lines
        .iter()
        .map(|l| field(l, "encode_end_us") - field(l, "encode_start_us"))
        .collect();
    assert!(encode.iter().filter(|&&t| t > 200).count() > encode.len() / 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn percentiles(mut v: Vec<u64>) -> [f64; 3] {
    v.sort_unstable();
    let p = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize] as f64 / 1000.0;
    [p(0.5), p(0.95), p(0.99)]
}

/// Host stages (ms) at the reference mode, synthetic (M1-4) vs real encoder (M1-7).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: run when tools/idle-guard.ps1 says bench_ok"]
async fn bench_host_stages() {
    if !have_ffmpeg() {
        return;
    }
    let secs: u64 = std::env::var("VETRO_BENCH_SECS").map_or(30, |s| s.parse().unwrap());
    let hz: u32 = std::env::var("VETRO_BENCH_HZ").map_or(60, |s| s.parse().unwrap());
    let dir = scratch("bench");
    println!(
        "| path | frames/s | encode p50 / p95 / p99 | capture→sent p50 / p95 / p99 | bytes/frame p50 |"
    );
    println!("|---|---|---|---|---|");
    for (name, pattern) in [
        ("M1-4 moving-bars (no codec)", TestPattern::MovingBars),
        ("M1-7 gpu-bars (hevc_qsv)", TestPattern::GpuBars),
    ] {
        let tele = dir.join(format!("{pattern:?}.jsonl"));
        let mut e = start(pattern, &tele, (2960, 1848, hz), &[Profile::HevcMain]).await;
        let t = Instant::now();
        let n = e
            .frames(usize::MAX, Duration::from_secs(secs), false)
            .await
            .len();
        let fps = n as f64 / t.elapsed().as_secs_f64();
        e.stop().await;
        let text = std::fs::read_to_string(&tele).unwrap();
        // The first second warms up (encoder open, first IDR).
        let lines: Vec<&str> = text.lines().skip(hz as usize).collect();
        let col = |a: &str, b: &str| -> Vec<u64> {
            lines.iter().map(|l| field(l, b) - field(l, a)).collect()
        };
        let enc = percentiles(col("encode_start_us", "encode_end_us"));
        let send = percentiles(col("capture_us", "send_us"));
        let bytes = percentiles(
            lines
                .iter()
                .map(|l| field(l, "encoded_bytes") * 1000)
                .collect(),
        );
        println!(
            "| {name} | {fps:.1} | {:.2} / {:.2} / {:.2} | {:.2} / {:.2} / {:.2} | {:.0} |",
            enc[0], enc[1], enc[2], send[0], send[1], send[2], bytes[0]
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
