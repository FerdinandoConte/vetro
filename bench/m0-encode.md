# M0-5 — Encode capacity spike (Iris Xe, HEVC 2960×1848)

Question (risk R1): can the reference laptop capture the virtual monitor with DXGI Desktop Duplication and encode HEVC 2960×1848 @120 with the low-latency settings of [ARCHITECTURE §5.3](../ARCHITECTURE.md#53-encoder), within the 5 ms encode budget of §4, for 30 minutes? Also risk R10: does content on the 120 Hz virtual monitor render at 120 Hz while the 60 Hz laptop panel is primary?

**Short answer:** no, not at 120 fps. The best native run gave 78 fps (preset veryfast), and no mode got encode p50 under 8 ms. Native 2960×1848 **@60** held 60 fps in one 3-minute run (encode p50 10.8 ms, p99 21.7 ms). On R10, the 60 Hz cap is confirmed: the virtual monitor composes at **61 fps while the panel is primary** (valid run). That making the virtual monitor primary lifts it to **121 fps** is **provisional**: that run is not valid. **Still to re-run:**
- the 30-minute soak (marked noisy);
- the VDD-primary render run (not valid);
- the BGRA run (the timer page probably did not render).

**Status: incomplete; re-run pending.**

## Setup
| | |
|---|---|
| Host | Galaxy Book2 Pro 360, i7-1260P, Iris Xe (Alder Lake Gen12), Windows 11, on AC |
| Virtual monitor | Virtual Display Driver 25.7.23 (signed, pinned, `tools/vdd-install.ps1`), mode 2960×1848 @120 |
| Capture | FFmpeg `ddagrab` (DXGI Desktop Duplication) on the VDD output, `draw_mouse=0` |
| Encode | `hwmap=derive_device=qsv` (zero-copy D3D11 → QSV), `hevc_qsv` Main, `async_depth=1`, `low_power=1`, `bf=0`, `low_delay_brc=1`, 80 Mbps CBR, VBV of one frame, `g=1000000` (no periodic IDR) |
| FFmpeg | BtbN LGPL shared build (`tools/fetch-ffmpeg.ps1`), oneVPL runtime 2.15 |
| Content | `tools/latency-timer.html` full screen in Edge on the virtual monitor, the whole run (counter, frame counter, flashing squares) |
| Tools | `tools/encode-bench.ps1` (one run), `tools/m0-encode-sequence.ps1` (whole sequence, layout switch and restore) |
| Runs | prefix `m0-20261126-0102`: 2026-11-26 01:03–01:17 (enc-bgra-120, enc-nv12-120, render-panel-primary), 2026-11-27 23:24 – 2026-11-28 00:15 (the rest) |

### Method
- **Sustained fps** = frames per wall-clock second from FFmpeg `-progress` (one block per second), first 5 s dropped. ddagrab runs at `framerate=<fps>` and repeats frames when nothing new arrives, so the encoder always gets that many frames per second to encode. The fps is how many of them it finishes.
- **Per-frame encode time** = `-benchmark_all` real time of `avcodec_send_frame` + `avcodec_receive_packet`. With `async_depth=1`, `send_frame` does not return until that frame is encoded, so the sum is the frame's encode time.
- **Render rate (R10)** = ddagrab with `dup_frames=0` at 240 fps counts only frames DWM really composed on the VDD output, with the timer page animating.
- **Clock monitoring.** Every 2 s:
  - CPU % and CPU performance % (the real clock as % of base; `ProcessorFrequency` is only the static base and is no longer recorded)
  - `PercentPerformanceLimit`
  - thermal-zone temperature, `ThrottleReasons` and passive limit
  - GPU engine utilisation (sum over processes, busiest engine per type)

  Per minute: fps and encode p50/p99, to see drift.

  The Iris Xe video engine (VDBOX, which runs VDENC) appears as **"VideoDecode"** in these counters. **GPU clock is not monitored:** Windows has no non-admin performance counter for the Intel GPU frequency (the GPU Engine counters give only utilisation). This is a known gap.
- **Validity:** a run counts only if all of these hold:
  - `tools/idle-guard.ps1` said `bench_ok` at the start (owner idle ≥ 10 min, AC, CPU < 20 %);
  - the idle/AC check taken about every 40 s stayed good for the whole run;
  - the run had enough of those checks (at least one per 45 s; required from now on).

  Otherwise the run is marked `noisy`. From now on a run with the page on but the GPU 3D engine almost idle is also flagged `page_render_suspect`.
- **Summary format.** Every run's summary JSON is in [`bench/m0-encode/`](m0-encode/). Runs from now on also record:
  - the send/receive split
  - a 2 ms encode-time histogram
  - which idle checks failed
  - the effective rate-control and LowDelayBRC settings
  - `output_size`

  `output_size` replaces the old `captured_size` field, which in encode mode was the *encoded* size, not the captured one. Numbers below marked **(raw log)** come from the uncommitted FFmpeg logs (2–18 MB each, kept on the laptop in `%TEMP%\vetro-bench`), not from the committed summaries.

### Preconditions (learned the hard way)
1. **Screen saver off.** The first unattended launch was refused because of the owner's 3-minute screen saver. While it runs, Desktop Duplication cannot see the desktop. The owner turned it off (now in `docs/it/GUIDA.md` §9).
2. Owner idle ≥ 10 min, AC power, CPU < 20 % (`bench_ok`), checked by the sequence before each step. Another heavy job (an M1 build) delayed the second launch until CPU stayed under 20 % twice in a row, 2 min apart.
3. Launch from the owner's interactive session. A sandboxed agent shell gets `ERROR_ACCESS_DENIED` from `SetDisplayConfig`, and the sequence refuses with exit 2.
4. The VDD starts off the desktop. The sequence extends onto it, sets 2960×1848 @120, and restores the saved layout at the end. If the restore fails, it exits 6 and prints the recovery steps (Win+P, "PC screen only").

## Results

### Encode runs (laptop panel primary, timer page on the VDD)
Each run is one row in the table. Encode times are in ms, temperatures in °C. "Frames over the frame period" means over 8.33 ms at 120 fps, or over 16.7 ms at 60 fps.

| Run | Encoded size @ input fps | Input, preset | Sustained fps p50 (mean · p95) | Encode p50 / p95 / p99 | Frames over the frame period | CPU p50 | GPU 3D p50 | GPU video p50 (max) | Temp p50 / max | Valid |
|---|---|---|---|---|---|---|---|---|---|---|
| [enc-bgra-120](m0-encode/m0-20261126-0102-enc-bgra-120.json) | 2960×1848 @120 | BGRA into encoder, TU4 | 43 (44.3 · 54) | 19.3 / 38.6 / 47.9 | ≥ 99 % | 10 % | **0 %** | 47 % (66) | 72 / 93 | **suspect**: page probably not rendering (see below) |
| [enc-nv12-120](m0-encode/m0-20261126-0102-enc-nv12-120.json) | 2960×1848 @120 | `vpp_qsv` → NV12, TU4 | **63** (64.8 · 77) | **16.3** / 17.5 / **23.1** | ≥ 98 % | 13 % | 47 % | 51 % (66) | 83 / 93 | yes |
| [fb-nv12-120-veryfast](m0-encode/m0-20261126-0102-fb-nv12-120-veryfast.json) | 2960×1848 @120 | NV12, TU7 (`-preset veryfast`) | **78** (85.3 · 118) | **9.0** / 17.0 / **20.8** | 66 % | 14 % | 56 % | 50 % (66) | 84 / 93 | yes |
| [fb-nv12-1920x1200-120](m0-encode/m0-20261126-0102-fb-nv12-1920x1200-120.json) | 1920×1200 @120 (GPU-scaled) | NV12, TU4 | **89** (87.9 · 110) | **8.8** / 17.1 / **24.2** | 58 % | 15 % | 57 % | 45 % (62) | 84 / 89 | yes |
| [fb-nv12-1480x924-120](m0-encode/m0-20261126-0102-fb-nv12-1480x924-120.json) | 1480×924 @120 (GPU-scaled) | NV12, TU4 | **87** (88.3 · 111) | **8.5** / 17.1 / **24.9** | 53 % | 15 % | 61 % | 36 % (52) | 84 / 92 | yes |
| [fb-nv12-2960x1848-60](m0-encode/m0-20261126-0102-fb-nv12-2960x1848-60.json) | 2960×1848 @60 | NV12, TU4 | **60** (60.0 · 62) | **10.8** / 17.5 / **21.7** | 10 % | 14 % | 46 % | 47 % (60) | 83 / 90 | yes |
| [soak-nv12-30min](m0-encode/m0-20261126-0102-soak-nv12-30min.json) | 2960×1848 @120 | NV12, TU4, 1800 s | 75 (72.8 · 89) | 13.3 / 17.2 / 28.7 | 96 % | 16 % | 47 % | 54 % (76) | 82 / 93 | **no (noisy)** |

- The 5 ms encode budget of §4 is missed by ≥ 99 % of frames in every run (`over_budget_5ms_pct` 99.1–100).
- The `fb-*` runs ran right after the soak, so the machine was already warm.
- **enc-bgra-120 is suspect.** Its GPU 3D engine sat at p50 0 % (max 36 %); every other run with the page on shows 46–66 %, and the render-only run 66 %. The timer page was most likely not rendering, or not on the VDD, during that run. Its numbers are not compared with the others until the re-run.

### Effective encoder settings
From the `-v verbose` log of the later runs (2026-11-27/28):
- **VDENC (low-power fixed-function encoder): ON**
- **`GopRefDist: 1`:** no B-frames
- **`IntRefType: 0`:** no intra refresh
- **TargetUsage:** 4, or 7 with `veryfast`
- CBR at 80 Mbps (`TargetKbps 40000 × BRCParamMultiplier 2`), VBV about one frame. (raw log)

The VDENC flag and TargetUsage are also in each summary's `encoder` field. The two runs of 2026-11-26 used the same encoder options but without `-v verbose`, so their effective parameters were not logged.

**Deviation:** the runtime reports **`LowDelayBRC: OFF`** in these runs (raw log), although `-low_delay_brc 1` was passed. With CBR (`maxrate = b:v`) it is not enabled. A 2-second VBR probe on the panel output showed `LowDelayBRC: ON` (probe, not committed). §5.3 asks for it on QSV, so M1 should use VBR with a max rate and measure the effect. The next summaries record `low_delay_brc` and `rate_control`.

### Render rate on the virtual monitor (R10)
| Run | Primary display | VDD composed fps p50 (p5 · mean) | Valid | Source |
|---|---|---|---|---|
| render-panel-primary (2026-11-26 01:14) | laptop panel (1920×1080 @60) | **61** (60 · 60.5) | yes | [summary](m0-encode/m0-20261126-0102-render-panel-primary.json), [progress](m0-encode/m0-20261126-0102-render-panel-primary.progress.txt) |
| render-vdd-primary (2026-11-27 23:24–23:26) | virtual monitor (2960×1848 @120) | 121 (116 · 119.3) | **no: provisional** | [progress](m0-encode/m0-20261126-0102-render-vdd-primary.progress.txt), 14 294 frames in 120 s |

**What happened to render-vdd-primary:** ffmpeg exited 0 and captured 2960×1848 on output 0, which is the VDD once it is primary. The summary step of `encode-bench.ps1` then crashed (a missing-match index in render mode, now fixed). Its in-run idle/AC samples and CPU/GPU samples are lost, so the run cannot be shown to be valid. The sequence did check `bench_ok` right before the step. The fps was recomputed from the `-progress` file the same way as in the summaries: 1 s blocks, first 5 s dropped.

**R10 answer:**
- **Confirmed:** while the 60 Hz laptop panel is primary, DWM composes the 120 Hz virtual monitor at the panel's 60 Hz. So every encode run above got only **60 new frames per second**, repeated up to 120 by ddagrab. A 120 fps stream with the panel primary would show each frame twice, and the §11 timer method sees 60 Hz content.
- **Provisional, re-run needed for validity:** making the virtual monitor primary lifts it to about 120 Hz.

### 30-minute soak
**Not valid: a re-run is needed.** `noisy: true` because the idle/AC check failed at least once in 43 checks (`stayed_idle_on_ac: false`). That summary did not record which check failed; `idle_check_failures` is recorded from now on. A query of the Windows System log showed no power or session event in that window (query, not committed).

Information only, not a verdict:
- **Throttling signals: none seen.** `ThrottleReasons` was 0 in all 636 samples and the passive limit stayed at 100 %. CPU performance p50 was 138 % of base, so turbo was active. `PercentPerformanceLimit` was a flat 85 % in every run, including an idle smoke run, so it is a static platform cap, not thermal.
- **Temperature:** p50 82 °C, max 93 °C.
- **No drift.** Per-minute fps averaged 76.1 in minutes 1–5 and 75.0 in minutes 25–29. Per-minute encode p50 jumped between two levels, about 12 ms and about 16 ms, with no trend.
- **Dips:** minutes 13–14 and 17–18 fell to 54–58 fps with p99 about 40 ms. The cause is unknown and may be the same event that made the run noisy.
- **Not covered:** GPU clock. The soak also used the NV12 input path, not BGRA.

## Where the bottleneck is
1. **The encoder call, not capture or CPU.** FFmpeg runs the filter graph (ddagrab, vpp) and the encoder in separate threads. "Encoder thread busy" below is mean encode time × mean fps, from the summaries:

   | Run | Encoder thread busy |
   |---|---|
   | nv12-120 | 1.00 |
   | soak | 0.99 |
   | 1920×1200 | 0.95 |
   | veryfast | 0.90 |
   | 1480×924 | 0.88 |
   | @60 | 0.71 (keeps up) |

   At 120 fps input the encoder thread is the limit. CPU stays at 10–16 % p50. Almost all of the time is spent inside `send_frame`: receive p50 is about 0.4 ms (raw log).
2. **Below about 2.3 Mpx there is a floor of about 8.5 ms.** At TU4, 1920×1200 (2.3 Mpx) gives p50 8.8 ms and 1480×924 (1.4 Mpx) gives 8.5 ms: 40 % fewer pixels save almost nothing. At native size, TU4 time also depends on the input rate: **16.3 ms at 120 fps input vs 10.8 ms at 60** for the same frame size and preset. The video engine is only 36–54 % busy (p50), never saturated. This points to the latency of one synchronous submit-and-wait on a shared, busy GPU with `async_depth=1`, more than a shortage of VDENC throughput. **Hypothesis**, not proven.
3. **A stall shaped like 60 Hz** (raw log). In the scaled, TU7 and @60 runs, about 12 % of frames take 16–18 ms, which is one 60 Hz period, while most take 6–10 ms. At native TU4 @120, 61 % of frames are in that bin. The 3D engine is 46–61 % busy (DWM composing the VDD at 60 Hz, Edge drawing the timer page, `vpp_qsv`). The likely cause is the encode sync waiting behind that 60 Hz work on the same GPU. **Hypothesis.** GPU clock and per-engine queue times were not sampled, and no encode run captured with the VDD primary. From now on the summaries carry the histogram, so the next runs can confirm or reject this.
4. **BGRA vs NV12 (43 vs 63 fps): anomaly, not concluded.** In the BGRA path the RGB→NV12 conversion happens inside the encoder call. In the NV12 path `vpp_qsv` does it on the 3D engine, in parallel. That could explain the gap: encode p50 +3 ms, and p99 48 vs 23 ms. But the BGRA run is also the one where the timer page probably did not render (3D 0 %), so its load is not comparable. **Hypothesis until the re-run.** NV12 via `vpp_qsv` remains the path used for every other run.
5. **Preset is the biggest single knob measured.** At native 120, TU4 → TU7 takes p50 from 16.3 to 9.0 ms and fps from 63 to 78. p95 of per-second fps reaches 118, so 120 is touched but not held.
6. **The capture cadence caps unique frames at 60 (R10)** with the panel primary, whatever the encoder does.

Cross-check with the M0-6 Apollo baseline (`bench/m0-baseline.md`): the same `hevc_qsv` reported host encode at 16.5–28 ms on average and the stream ran at 51–60 fps. This agrees with the 60-fps-class result here.

## Input for M0-8
- **Native 2960×1848 @120 is not feasible on this Iris Xe with FFmpeg QSV and `async_depth=1`.** It needs encode p99 ≤ 8.3 ms, plus the VDD primary (or another fix for R10). Best measured: p50 9.0 ms, p99 20.8 ms, 78 fps.
- **Candidate M1 target: native 2960×1848 @60.** Full resolution, no scaling blur on text, and R10 does not matter at 60 Hz. Caveats:
  - **Single 3-minute run, on a warm machine.** A 30-min soak at 60 (`soak-nv12-60-30min`) is in the re-run.
  - **The VDD mode was 120 Hz** and ddagrab sampled it at 60 fps. A true 60 Hz VDD mode was not measured.
  - **10 % of frames took longer than the 16.7 ms frame period.** Sustained fps still held at 60.0, because the following frames were faster.
  - **Encode p99 21.7 ms** uses more than half of the USB p99 SLO (40 ms) on its own.
  - `-preset veryfast` should lower these numbers at 60 too, but that is not measured.
- **GPU-scaled 120 Hz modes do not help.** 1920×1200 and 1480×924 still reach only 87–89 fps (p50 8.5–8.8 ms), and they cost sharpness.
- **The §4 budget must be revisited in M0-8.** The encode stage is about 10.8 ms p50 at 60, not 5 ms. With the other stages at budget, the USB total is about 24 ms p50, above the 20 ms SLO.
- Options to reach 120 later, each needing its own measurement:
  - native oneVPL backend (R9)
  - `async_depth=2` (throughput, at the cost of one frame of latency, against §5.3)
  - VDD primary during streaming
  - VBR with LowDelayBRC
  - damage-driven encoding (ADR-0004) lowers average load, not the peak

## Re-run pending
Steps: `enc-bgra-120` (suspect), `render-vdd-primary` (not valid), `soak-nv12-30min` (noisy), and the new `soak-nv12-60-30min` (soak of the candidate M1 mode). Expected duration about 78 min of measuring, after `bench_ok`. Run it from the owner's session with the screen saver off:

```
powershell -NoProfile -ExecutionPolicy Bypass -File tools\m0-encode-sequence.ps1 -Prefix m0-20261126-0102 -Only render-vdd-primary,enc-bgra-120,soak-nv12-30min,soak-nv12-60-30min
```

Raw logs stay local (`bench/**/raw/` is gitignored). The per-run summaries are copied to `bench/m0-encode/`.
