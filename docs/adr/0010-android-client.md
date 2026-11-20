# ADR-0010: Android 11+, Rust core via JNI, zero-copy decode, latency-first presentation

Status: Accepted · 2026-11-20

## Context
Low-latency decode flags (`KEY_LOW_LATENCY`) and `Surface.setFrameRate` need Android 11 (API 30). The protocol and transport should exist once, in Rust. The app should be F-Droid compatible.

## Decision
- `minSdk 30`. Kotlin + Jetpack Compose for UI only; the Rust `vetro-client` cdylib owns transport, reassembly, FEC, decode and input encoding.
- NDK `AMediaCodec` in async mode decoding straight to the `SurfaceView`'s window (zero-copy), with low-latency and vendor keys when available.
- **Latency-first presentation by default:** release each decoded frame for rendering immediately and accept rare judder from clock drift between the host's 120 Hz and the panel's 120 Hz. A settings toggle switches to smoothness-first (one frame of buffering, about +8 ms).
- Hold the panel at maximum refresh with `setFrameRate(..., FIXED_SOURCE)` and `preferredDisplayModeId`.
- QR scanning with CameraX + ZXing; no Google Play Services.

## Consequences
- One protocol implementation, tested on Linux/Windows CI without a device.
- JNI boundary must stay narrow and well tested (surface handoff, input events, lifecycle).

## Alternatives
- Kotlin-only client: duplicate protocol implementation.
- ML Kit for QR: breaks F-Droid compatibility.
