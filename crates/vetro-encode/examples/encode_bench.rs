//! Encoder micro-benchmark and the R9 spike (ARCHITECTURE section 17): per-frame wall time of
//! the encode call vs the CPU time the calling thread spent in it.
//!
//! With one frame in flight the call is: copy into the pool surface (GPU), map to QSV, submit,
//! wait for the bitstream. Whatever FFmpeg's wrapper costs is CPU work on this thread, so the
//! thread CPU time per frame is an upper bound on what a native oneVPL backend could save per
//! frame; the rest of the wall time is the wait for the GPU, which native oneVPL pays too.
//!
//! `cargo run --release -p vetro-encode --example encode_bench -- 2960 1848 60 30 paced`
//! (`flat` instead of `paced`: back to back, the throughput floor). Run only when
//! `tools/idle-guard.ps1` says `bench_ok`; otherwise the numbers are noisy.

#[cfg(not(windows))]
fn main() {
    eprintln!("encode_bench needs Windows");
}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};
    use vetro_encode::d3d11::{Bars, Device};
    use vetro_encode::ffmpeg::FfmpegEncoder;
    use vetro_encode::{Encoder, Profile, Settings};
    use windows::Win32::System::Threading::GetCurrentThread;
    use windows::Win32::System::WindowsProgramming::QueryThreadCycleTime;

    /// CPU cycles this thread has run (exact, unlike the 15.6 ms tick of GetThreadTimes).
    fn cycles() -> u64 {
        let mut c = 0;
        // SAFETY: the pseudo-handle of the current thread and one out-parameter.
        unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut c) }.expect("cycle time");
        c
    }

    // Cycles per millisecond: spin 100 ms (the thread runs the whole time).
    let (t, c) = (Instant::now(), cycles());
    while t.elapsed() < Duration::from_millis(100) {}
    let per_ms = (cycles() - c) as f64 / (t.elapsed().as_secs_f64() * 1e3);
    let cpu_ms = move || cycles() as f64 / per_ms;

    fn pct(mut v: Vec<f64>) -> String {
        v.sort_by(f64::total_cmp);
        let p = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        format!("{:.2} / {:.2} / {:.2}", p(0.5), p(0.95), p(0.99))
    }

    let a: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, d: u32| a.get(i).map_or(d, |s| s.parse().expect("number"));
    let (w, h, hz, secs) = (num(0, 2960), num(1, 1848), num(2, 60), num(3, 30));
    let paced = a.get(4).is_none_or(|s| s != "flat");

    let dev = Device::open(None).expect("device");
    let settings = Settings {
        profile: Profile::HevcMain,
        width: w,
        height: h,
        refresh_mhz: hz * 1000,
        bitrate_kbps: 50_000,
    };
    let t = Instant::now();
    let mut enc = FfmpegEncoder::open(&dev, &settings).expect("encoder");
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let bars = Bars::new(&dev, w, h).expect("pattern");
    let interval = Duration::from_secs_f64(1.0 / f64::from(hz));
    let (mut wall, mut cpu, mut bytes) = (Vec::new(), Vec::new(), Vec::new());
    let mut au = Vec::new();
    let start = Instant::now();
    let mut next = start;
    let mut seq = 0u32;
    while start.elapsed() < Duration::from_secs(u64::from(secs)) {
        if paced {
            if let Some(d) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            next = (next + interval).max(Instant::now());
        }
        let (t, c) = (Instant::now(), cpu_ms());
        enc.encode(
            &mut |s| bars.draw(&dev, &s.texture, s.index, seq),
            seq == 0,
            &mut au,
        )
        .expect("encode");
        // Skip the opening second: first IDR, pool warm-up.
        if seq >= hz {
            wall.push(t.elapsed().as_secs_f64() * 1e3);
            cpu.push(cpu_ms() - c);
            bytes.push(au.len() as f64);
        }
        seq += 1;
    }
    let fps = f64::from(seq) / start.elapsed().as_secs_f64();
    let cpu_share = cpu.iter().sum::<f64>() / wall.iter().sum::<f64>() * 100.0;
    println!(
        "{w}x{h} @{hz} {} on {}, {:?}, {secs} s: {fps:.1} frames/s, open {open_ms:.0} ms",
        if paced { "paced" } else { "flat" },
        dev.adapter,
        enc.backend()
    );
    println!("encode wall ms p50/p95/p99: {}", pct(wall));
    println!(
        "encode thread CPU ms p50/p95/p99: {} ({cpu_share:.0} % of wall)",
        pct(cpu)
    );
    println!("bytes/frame p50/p95/p99: {}", pct(bytes));
}
