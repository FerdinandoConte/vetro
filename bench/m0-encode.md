# M0-5 — Encode capacity spike (Iris Xe, HEVC 2960×1848)

Question (risk R1): can the reference laptop capture the virtual monitor with DXGI Desktop Duplication and encode HEVC 2960×1848 @120 with the low-latency settings of [ARCHITECTURE §5.3](../ARCHITECTURE.md#53-encoder), within the 5 ms encode budget of §4, for 30 minutes? Also risk R10: does content on the 120 Hz virtual monitor render at 120 Hz while the 60 Hz laptop panel is primary?

**Short answer:** no, not at 120 fps. The best native run gave 78 fps (preset veryfast), and no mode got encode p50 under 8 ms. Native 2960×1848 **@60** holds 60 fps, encode p50 10.8 ms and p99 21.7 ms. R10 is confirmed: the virtual monitor composes at **61 fps while the panel is primary and 121 fps when the virtual monitor is primary**. The 30-minute soak ran but is **not valid** (marked noisy), so it must be re-run.

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
| Runs | prefix `m0-20261126-0102`: 2026-11-26 01:03–01:17 (first three runs), 2026-11-27 23:28 – 2026-11-28 00:15 (the rest) |

### Method
- **Sustained fps** = frames per wall-clock second from FFmpeg `-progress` (one block per second), first 5 s dropped. ddagrab runs at `framerate=120` and repeats frames when nothing new arrives, so the encoder always gets 120 frames per second to encode. The fps is how many of them it finishes.
- **Per-frame encode time** = `-benchmark_all` real time of `avcodec_send_frame` + `avcodec_receive_packet`. With `async_depth=1`, `send_frame` does not return until that frame is encoded, so the sum is the frame's encode time. Almost all of it is in `send_frame` (p50 receive ≈ 0.4 ms).
- **Render rate (R10)** = ddagrab with `dup_frames=0` at 240 fps counts only frames DWM really composed on the VDD output, with the timer page animating.
- **Load and clocks**, sampled every 2 s:
  - CPU % and CPU performance % (actual frequency as % of base)
  - `PercentPerformanceLimit`
  - thermal-zone temperature, `ThrottleReasons` and passive limit
  - GPU engine utilisation (sum over processes, busiest engine per type)

  The Iris Xe video engine (VDBOX, which runs VDENC) appears as **"VideoDecode"** in these counters.
- **Validity:** a run counts only if `tools/idle-guard.ps1` said `bench_ok` at the start (owner idle ≥ 10 min, AC, CPU < 20 %), and the idle/AC check taken every 30 s stayed good for the whole run. Otherwise the run is marked `noisy`.

### Preconditions (learned the hard way)
1. **Screen saver off.** The first unattended launch was refused because of the owner's 3-minute screen saver. While it runs, Desktop Duplication cannot see the desktop. The owner turned it off (now in `docs/it/GUIDA.md` §9).
2. Owner idle ≥ 10 min, AC power, CPU < 20 % (`bench_ok`), checked by the sequence before each step. Another heavy job (an M1 build) delayed the second launch until CPU stayed under 20 % twice in a row, 2 min apart.
3. Launch from the owner's interactive session. A sandboxed agent shell gets `ERROR_ACCESS_DENIED` from `SetDisplayConfig`, and the sequence refuses with exit 2.
4. The VDD starts off the desktop. The sequence extends onto it, sets 2960×1848 @120, and restores the saved layout at the end.

## Results

### Encode runs (laptop panel primary, timer page on the VDD)
Each run is one row in the table. Encode times are in ms, temperatures in °C. "Frames over the frame period" means over 8.33 ms at 120 fps, or over 16.7 ms at 60 fps.

| Run | Encoded size @ input fps | Input, preset | Sustained fps p50 (mean · p95) | Encode p50 / p95 / p99 | Frames over the frame period | CPU p50 | GPU 3D p50 | GPU video p50 (max) | Temp p50 / max | Valid |
|---|---|---|---|---|---|---|---|---|---|---|
| [enc-bgra-120](m0-encode/m0-20261126-0102-enc-bgra-120.json) | 2960×1848 @120 | BGRA into encoder, TU4 | **43** (44.3 · 54) | **19.3** / 38.6 / **47.9** | 100 % | 10 % | 0 % | 47 % (66) | 72 / 93 | yes |
| [enc-nv12-120](m0-encode/m0-20261126-0102-enc-nv12-120.json) | 2960×1848 @120 | `vpp_qsv` → NV12, TU4 | **63** (64.8 · 77) | **16.3** / 17.5 / **23.1** | 99 % | 13 % | 47 % | 51 % (66) | 83 / 93 | yes |
| [fb-nv12-120-veryfast](m0-encode/m0-20261126-0102-fb-nv12-120-veryfast.json) | 2960×1848 @120 | NV12, TU7 (`-preset veryfast`) | **78** (85.3 · 118) | **9.0** / 17.0 / **20.8** | 66 % | 14 % | 56 % | 50 % (66) | 84 / 93 | yes |
| [fb-nv12-1920x1200-120](m0-encode/m0-20261126-0102-fb-nv12-1920x1200-120.json) | 1920×1200 @120 (GPU-scaled) | NV12, TU4 | **89** (87.9 · 110) | **8.8** / 17.1 / **24.2** | 58 % | 15 % | 57 % | 45 % (62) | 84 / 89 | yes |
| [fb-nv12-1480x924-120](m0-encode/m0-20261126-0102-fb-nv12-1480x924-120.json) | 1480×924 @120 (GPU-scaled) | NV12, TU4 | **87** (88.3 · 111) | **8.5** / 17.1 / **24.9** | 53 % | 15 % | 61 % | 36 % (52) | 84 / 92 | yes |
| [fb-nv12-2960x1848-60](m0-encode/m0-20261126-0102-fb-nv12-2960x1848-60.json) | 2960×1848 @60 | NV12, TU4 | **60** (60.0 · 62) | **10.8** / 17.5 / **21.7** | 10 % | 14 % | 46 % | 47 % (60) | 83 / 90 | yes |
| [soak-nv12-30min](m0-encode/m0-20261126-0102-soak-nv12-30min.json) | 2960×1848 @120 | NV12, TU4, 1800 s | 75 (72.8 · 89) | 13.3 / 17.2 / 28.7 | 96 % | 16 % | 47 % | 54 % (76) | 82 / 93 | **no (noisy)** |

The 5 ms encode budget of §4 is missed by 100 % of frames in every run. The fallback runs (`fb-*`) ran right after the soak, so the machine was already warm.

### Effective encoder settings
From the `-v verbose` log of each later run:
- **VDENC (low-power fixed-function encoder): ON**
- **`GopRefDist: 1`:** no B-frames
- **`IntRefType: 0`:** no intra refresh
- **TargetUsage:** 4, or 7 with `veryfast`
- CBR at 80 Mbps (`TargetKbps 40000 × BRCParamMultiplier 2`), VBV about one frame.

**Deviation:** the runtime reports **`LowDelayBRC: OFF`** in these runs, although `-low_delay_brc 1` was passed. With CBR (`maxrate = b:v`) the runtime does not enable it. A probe with VBR showed `LowDelayBRC: ON`. §5.3 asks for it on QSV, so M1 should use VBR with a max rate and measure the effect. The first two runs predate the verbose logging; they used the same command line.

### Render rate on the virtual monitor (R10)
| Run | Primary display | VDD composed fps p50 (p5 · mean) | Source |
|---|---|---|---|
| render-panel-primary | laptop panel (1920×1080 @60) | **61** (60 · 60.5) | [summary](m0-encode/m0-20261126-0102-render-panel-primary.json), [progress](m0-encode/m0-20261126-0102-render-panel-primary.progress.txt) |
| render-vdd-primary | virtual monitor (2960×1848 @120) | **121** (116 · 119.3) | [progress](m0-encode/m0-20261126-0102-render-vdd-primary.progress.txt), 14 294 frames in 120 s |

**About render-vdd-primary:** the capture itself ran correctly; ffmpeg exited 0 and captured 2960×1848 on output 0, which is the VDD once it is primary. The summary step of `encode-bench.ps1` then crashed (a missing-match index in render mode, fixed in this PR). The CPU/GPU samples of that run are lost. The fps above was recomputed from its `-progress` file the same way as in the summaries: 1 s blocks, first 5 s dropped. The sequence checked `bench_ok` right before the step.

**R10 answer:** yes. While the 60 Hz laptop panel is primary, DWM composes the 120 Hz virtual monitor at the panel's 60 Hz. Making the virtual monitor primary lifts it to 120 Hz. So every encode run above got only **60 new frames per second**, repeated up to 120 by ddagrab. A 120 fps stream with the panel primary would show each frame twice, and the §11 timer method sees 60 Hz content.

### 30-minute soak
**Not valid: a re-run is needed.** `noisy: true` because the idle/AC check failed at least once in 43 checks (`stayed_idle_on_ac: false`). The summary did not record which check failed. `encode-bench.ps1` now writes `idle_check_failures` with timestamps. No Windows power or session event was logged in that window.

Information only, not a verdict:
- **Throttling signals: none seen.** `ThrottleReasons` was 0 in all 636 samples and the passive limit stayed at 100 %. CPU performance p50 was 138 % of base, so turbo was active. `PercentPerformanceLimit` was a flat 85 % in every run, including the idle smoke run, so it is a static platform cap, not thermal.
- **Temperature:** p50 82 °C, max 93 °C.
- **No drift.** Per-minute fps averaged 76.1 in minutes 1–5 and 75.0 in minutes 25–29. Per-minute encode p50 jumped between two levels, about 12 ms and about 16 ms, with no trend.
- **Dips:** minutes 13–14 and 17–18 fell to 54–58 fps with p99 about 40 ms. The cause is unknown and may be the same event that made the run noisy.
- **Not sampled:** GPU frequency. The soak also used the NV12 input path, not BGRA.

## Where the bottleneck is
1. **The encoder call, not capture, colour conversion or CPU.** FFmpeg runs the filter graph (ddagrab, vpp) and the encoder in separate threads. "Encoder thread busy" below is mean encode time × mean fps:

   | Run | Encoder thread busy |
   |---|---|
   | bgra-120 | 1.00 |
   | nv12-120 | 1.00 |
   | soak | 0.99 |
   | 1920×1200 | 0.95 |
   | veryfast | 0.90 |
   | 1480×924 | 0.88 |
   | @60 | 0.71 (keeps up) |

   At 120 fps input the encoder thread is the limit. CPU stays at 10–16 % p50.
2. **The encode time barely depends on resolution.** At TU7, encode p50 is 9.0 ms for 5.5 Mpx. At TU4, it is 8.8 ms for 2.3 Mpx and 8.5 ms for 1.4 Mpx. A quarter of the pixels saves almost nothing, so there is a floor of about 8 ms per frame. The video engine is only 36–54 % busy (p50), never saturated. This looks like the latency of one synchronous submit-and-wait on a shared GPU with `async_depth=1`, not a shortage of VDENC throughput.
3. **A stall shaped like 60 Hz.** In the scaled, TU7 and @60 runs, 12 % of frames take 16–18 ms, which is one 60 Hz period, even though most frames take 6–10 ms. At native TU4, 61 % of frames are in that bin. The 3D engine is 46–61 % busy (DWM composing the VDD at 60 Hz, Edge drawing the timer page, `vpp_qsv`). The likely cause is the encode sync waiting behind that 60 Hz work on the same GPU. **This is a hypothesis.** GPU frequency and per-engine queue times were not sampled, and no run captured with the VDD primary during encode.
4. **BGRA vs NV12 (43 vs 63 fps) is the colour conversion.**
   - With BGRA input, the 3D engine is idle (p50 0 %). The RGB→NV12 conversion happens inside the encoder call, in series: encode p50 +3 ms, and the p99 doubles from 23 to 48 ms.
   - With `vpp_qsv` the conversion runs on the 3D engine in the filter thread, in parallel with the previous frame's encode.
   - So the conversion belongs before the encoder (§5.2 already allows the VideoProcessor).
5. **Preset is the biggest single knob.** At native 120, TU4 → TU7 takes p50 from 16.3 to 9.0 ms and fps from 63 to 78. p95 of per-second fps reaches 118, so 120 is touched but not held.
6. **The capture cadence caps unique frames at 60 (R10)** with the panel primary, whatever the encoder does.

Cross-check with the M0-6 Apollo baseline (`bench/m0-baseline.md`): the same `hevc_qsv` reported host encode at 16.5–28 ms on average and the stream ran at 51–60 fps. This agrees with the 60-fps-class result here.

## Input for M0-8
- **Native 2960×1848 @120 is not feasible on this Iris Xe with FFmpeg QSV and `async_depth=1`.** It needs encode p99 ≤ 8.3 ms, plus the VDD primary (or another fix for R10). Best measured: p50 9.0 ms, p99 20.8 ms, 78 fps.
- **Recommended M1 target: native 2960×1848 @60** (60.0 fps sustained, encode p50 10.8 / p99 21.7 ms, at TU4). Full resolution means no scaling blur on text, and R10 does not matter at 60 Hz. `-preset veryfast` should lower the encode time at 60 too, but that is **not measured** at 60. Measure it in M1 before fixing the preset.
- **GPU-scaled 120 Hz modes do not help.** 1920×1200 and 1480×924 still reach only 87–89 fps (p50 8.5–8.8 ms), and they cost sharpness.
- **The §4 budget must be revisited in M0-8.** The encode stage is about 10.8 ms p50 at 60, not 5 ms. With the other stages at budget, the USB total is about 24 ms p50, above the 20 ms SLO.
- Options to reach 120 later, each needing its own measurement:
  - native oneVPL backend (R9)
  - `async_depth=2` (throughput, at the cost of one frame of latency, against §5.3)
  - VDD primary during streaming
  - VBR with LowDelayBRC
  - damage-driven encoding (ADR-0004) lowers average load, not the peak
- **Still to re-run:** the 30-min soak (valid run needed), and render-vdd-primary to get its CPU/GPU samples (its fps is already known).

## Raw data
- Per-run summaries and the sequence result are in [`bench/m0-encode/`](m0-encode/). The large FFmpeg logs (2–18 MB each, with every frame's timing) stay on the reference laptop under `%TEMP%\vetro-bench`. They are not committed.
- Re-run the missing steps:

  ```
  powershell -NoProfile -ExecutionPolicy Bypass -File tools\m0-encode-sequence.ps1 -Only soak-nv12-30min,render-vdd-primary
  ```

  Run it from the owner's session, with the screen saver off.
