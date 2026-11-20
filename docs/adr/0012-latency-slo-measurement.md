# ADR-0012: Latency SLOs, per-stage telemetry and a HIL performance gate

Status: Accepted · 2026-11-20

## Context
"Near-zero latency" is not testable. The owner wants performance that is excellent and measured accurately. Measurements taken while the laptop is busy (the owner studies on it, agents compile on it) are noise.

## Decision
- SLOs (glass-to-glass): USB p50 ≤ 20 ms, p99 ≤ 40 ms; Wi-Fi 5/6 GHz p50 ≤ 30 ms, p99 ≤ 80 ms. Per-stage budgets in ARCHITECTURE §4.
- Every frame carries timestamps; clocks are aligned NTP-style; per-stage histograms feed a HUD and JSONL.
- HIL runs (real tablet, real PC) compute p50/p95/p99 per stage and gate merges: a p99 regression above 10 % or 1 ms on any stage fails.
- A HIL run is valid only on AC power, after ≥ 10 min without user input and with CPU < 20 %; otherwise it is marked noisy and ignored by the gate.
- Glass-to-glass truth is measured by the owner at each milestone with a high-speed camera and the timer page.

## Consequences
- Performance claims in the README are always backed by a dated, reproducible measurement.
- HIL needs the tablet plugged in and the PC on charger and idle; the nightly run is skipped (not failed) when conditions are not met.

## Alternatives
- Only manual camera tests: too slow to catch regressions.
- Only in-pipeline telemetry: misses display and compositor latency.
