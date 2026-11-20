# Architecture Decision Records

One file per decision. Format: Status · Context · Decision · Consequences · Alternatives. A decision changes only through a new ADR that supersedes the old one (the old file gets `Status: Superseded by ADR-XXXX`).

| ADR | Decision |
|---|---|
| [0001](0001-strategy-new-rust-core.md) | New Rust core that reuses components, not a fork of Sunshine/Apollo |
| [0002](0002-platform-scope.md) | Windows host + Android client first; Linux Wayland in v2 |
| [0003](0003-virtual-display-vdd.md) | Virtual monitor through Virtual Display Driver (MIT) |
| [0004](0004-capture-dda-damage.md) | Desktop Duplication, damage-driven encoding, client-side cursor |
| [0005](0005-encoding.md) | FFmpeg behind an `Encoder` trait; codec negotiation; 4:2:0 first |
| [0006](0006-transport-quic.md) | QUIC (quinn) with datagrams, custom congestion control, FEC |
| [0007](0007-usb-strategy.md) | USB tethering in M1, AOA in M2, failover in M3, no bonding |
| [0008](0008-security-pairing.md) | Mandatory QR pairing and pinned mutual TLS on every transport |
| [0009](0009-input-injection.md) | Native touch and pen injection on a dedicated stream |
| [0010](0010-android-client.md) | Android 11+, Rust core via JNI, zero-copy decode, latency-first |
| [0011](0011-host-processes.md) | Headless engine + Tauri tray UI as separate processes |
| [0012](0012-latency-slo-measurement.md) | Latency SLOs, per-stage telemetry, HIL performance gate |
| [0013](0013-out-of-scope.md) | Out of scope: offloading, audio mixing, bonding, X11 |
| [0014](0014-licensing-distribution.md) | MIT OR Apache-2.0, free forever, signed releases, no telemetry |
| [0015](0015-development-process.md) | Local Claude Code agents with quality gates and an ask-first autopilot |
