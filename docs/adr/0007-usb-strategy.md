# ADR-0007: USB tethering in M1, AOA in M2, failover in M3, no bonding

Status: Accepted · 2026-11-20

## Context
Bandwidth is not the problem: a high-quality 120 fps HEVC stream needs 50–150 Mbit/s and even USB 2.0 has several times that. Zero-config is the problem:
- ADB needs developer options on the tablet.
- USB tethering needs one toggle per connection (apps cannot enable it), may hijack the PC's default route, and may be missing on Wi-Fi-only tablets.
- AOA needs no developer options and opens our app on plug, but Windows must bind WinUSB to the accessory-mode device (`18D1:2D00/2D01`) and something must send the AOA start requests.

Bonding USB and Wi-Fi adds reordering and jitter equal to the slower path.

## Decision
1. **M1:** USB through tethering, which is plain IP and shares all Wi-Fi code. The engine detects the tethering interface and keeps it from becoming the default route.
2. **M2:** AOA as the "plug and go" transport, QUIC over USB bulk. The M0 spike decides the Windows driver path (automatic binding, a SignPath-signed WinUSB INF, or another signed generic driver).
3. **M3:** automatic failover between USB and Wi-Fi via QUIC migration, accepting a 100–300 ms glitch plus a keyframe.
4. Never bond links.

## Consequences
- M1 ships a working cable path quickly, with one toggle of friction; M2 removes the friction.
- If M0 shows the tablet has no USB tethering, AOA moves into M1 and M1 grows accordingly.

## Alternatives
- ADB forward/reverse: kept for development only, never required from users.
- Bonding: rejected (worse jitter).
