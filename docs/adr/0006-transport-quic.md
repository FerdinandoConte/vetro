# ADR-0006: QUIC (quinn) with datagrams, custom congestion control and FEC

Status: Accepted · 2026-11-20

## Context
The link must carry video (loss-tolerant, latency-critical), input (small, must never wait behind video), control and telemetry, encrypted and authenticated, over Wi-Fi, USB tethering and later raw USB, with a path switch on cable unplug.

## Decision
Use QUIC via `quinn`:
- video in unreliable datagrams with per-frame Reed–Solomon parity (0 % on USB, adaptive on Wi-Fi), no retransmission, keyframe request on loss;
- separate streams for control, input (highest priority) and telemetry (lowest);
- TLS 1.3 with pinned certificates from pairing ([ADR-0008](0008-security-pairing.md));
- a permissive custom congestion controller, with bitrate driven by our own delay-based rate control at the encoder;
- a custom `quinn` socket adapter later carries the same packets over AOA bulk endpoints ([ADR-0007](0007-usb-strategy.md));
- connection migration for the M3 failover.

**Exit plan:** if M1 measurements show QUIC adds more than 1 ms at p99 versus raw UDP, switch the video path to raw UDP with Noise (`snow`) encryption. The transport crate hides this choice from the rest of the code.

## Consequences
- Encryption, multiplexing without head-of-line blocking, and migration come for free.
- Default QUIC congestion control and pacing must be tuned or replaced, and verified by measurement.

## Alternatives
- Custom UDP protocol (Moonlight-style): more control, but encryption, reliable control channel and migration must all be built.
- WebRTC: heavy, and its jitter buffer adds latency.
