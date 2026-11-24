# M0-6 — Apollo + Artemis glass-to-glass baseline

Reference point for Vetro: how fast an existing open-source stack (Apollo host → Artemis client) is on the reference hardware. Method: [ARCHITECTURE §11](../ARCHITECTURE.md#11-telemetry-and-measurement), checklist [`docs/manual-tests/m0-baseline.md`](../docs/manual-tests/m0-baseline.md), timer page [`tools/latency-timer.html`](../tools/latency-timer.html).

**Δ** = timer value on the laptop panel − timer value on the tablet, read from the same phone photo (Pro mode, 1/500 s). Each reading was cross-checked against the page's binary squares. A photo counts only when both counters are unambiguous and neither frame square is mid-transition. Δ excludes the laptop panel's own display latency, by design (§4).

## Setup
| | |
|---|---|
| Host | Galaxy Book2 Pro 360, i7-1260P, Iris Xe, Windows 11, on AC |
| Client | Galaxy Tab S8 Ultra (SM-X906B), Android 16, panel 2960×1848 @ 120 Hz |
| Host software | Apollo (SudoVDA virtual display), encoder `hevc_qsv` |
| Client software | Artemis, decoder `c2.qti.hevc.decoder.low_latency`, requested 2960×1848 @ 120 fps, HEVC, 80 Mbps |
| Mode | Extend; Edge full-screen on each panel |
| Date | 2026-11-24 (control session) |

## Results

| Transport | Valid / photos | Δ min | **Δ p50** | **Δ p95** | Δ max | Stream fps (overlay) | Network | Host encode avg (max) | Decode avg |
|---|---|---|---|---|---|---|---|---|---|
| USB tethering (RNDIS) | 14 / 17 | 33 | **33 ms** | **50 ms** | 50 | 53–58 | 3–4 ms | 26–28 ms (36–43) | 12.0–12.3 ms |
| Wi-Fi 5 GHz | 12 / 20 | 33 | **50 ms** | **83 ms** | 83 | 51–60 | 13–21 ms | 16.5–28 ms (38) | 12.6–13.3 ms |

Valid Δ samples (ms):
- USB: 33 ×8, 34 ×2, 50 ×4
- Wi-Fi: 33 ×4, 49, 50 ×4, 51, 67, 83

p95 is nearest-rank. Network drops were 0.00 % in both runs.

**Timer zero test** (both windows side by side on the laptop panel): 10/10 valid, Δ ∈ [−1, 0] ms, mean −0.3 ms. Two Edge windows reloaded together agree to within 1 ms.

## Limits of this baseline (accepted by the owner on 2026-11-24)
1. **Not at 120 Hz.** The virtual display was set to 120 Hz and Apollo logged a 120 fps request. Even so, the tablet-side page advanced about 60 frames/s and the stream ran at 51–60 fps. All Δ values fall in ~16.7 ms steps, so each sample is accurate to ±1 frame at 60 Hz. Leading hypothesis, not verified: Edge paces every window at the vsync of the primary display, which is the laptop panel at 60 Hz. Whether Artemis can hold 120 Hz on this tablet is moved to M1 (risk R7), where Vetro's own client can measure it.
2. **Fewer than 20 valid samples per transport** (14 USB, 12 Wi-Fi). The milestone asked for 20+. The owner chose not to run a third session, so p95 here is coarse.
3. **Discarded first run** (2026-11-22 Wi-Fi, 2026-11-24 USB). Apollo's `output_name` pointed to a stale display id, so Apollo fell back to the laptop panel and streamed a 60 Hz mirror instead of the virtual monitor. Those samples (Wi-Fi p50 17 ms, USB p50 72 ms) measured the wrong output and are not used.

## What this means for Vetro
- At ~60 fps, the bar to beat is **USB p50 33 ms / p95 50 ms** and **Wi-Fi p50 50 ms / p95 83 ms**. The M1 SLOs (USB p50 ≤ 20 ms, Wi-Fi p50 ≤ 30 ms) are well below this.
- Apollo's average encode time on this Iris Xe (26–28 ms) is about 5× the 5 ms encode budget in §4. M0-5 measures `hevc_qsv` directly with low-latency settings to see how much of that is configuration and how much is hardware.
- Decode on the Snapdragon is about 12–13 ms, against a budget of 5 ms. M1 must confirm whether `KEY_LOW_LATENCY` plus the vendor key lowers it.

Photos are kept on the owner's machine only (git-ignored). Evidence trail: issue #7.
