# Vetro — Roadmap

Entry rule for every item: it must not worsen the measured p99 latency (ADR-0012). Detailed work items and status live in `docs/milestones/`. The owner's plain-Italian version is `docs/it/GUIDA.md`.

## M0 — Baseline and feasibility
Dev environment, public repository, Iris Xe encode capacity at 2960×1848 @120, Apollo + Artemis glass-to-glass baseline, tablet facts (USB tethering, 120 Hz), AOA-on-Windows driver path. → [`docs/milestones/M0.md`](docs/milestones/M0.md)

## M1 — MVP (Windows → Android)
- Virtual monitor (VDD) at the client's native mode; damage-driven DDA capture; client-side cursor.
- FFmpeg encoding (QSV/NVENC/AMF/MF), codec negotiation AV1 > HEVC > H.264 (AV1 experimental).
- QUIC transport over Wi-Fi and USB tethering; default-route protection.
- Android 11+ client: zero-copy low-latency decode, forced max refresh, full-screen.
- Native multi-touch and pen (pressure, tilt, hover).
- QR pairing, mDNS discovery, mutual TLS on every transport.
- Per-stage latency HUD and HIL performance gate.
- Tauri tray app + headless engine; signed installer and APK on GitHub.
- Known limits: no input into elevated windows, frozen frame during UAC prompts.
→ [`docs/milestones/M1.md`](docs/milestones/M1.md)

## M2 — USB zero-config
AOA transport: plug in, the app opens, no developer options; the same QUIC carried over USB bulk. → [`docs/milestones/M2.md`](docs/milestones/M2.md)

## M3 — Continuity
Automatic USB ↔ Wi-Fi failover without ending the session; measured glitch. → [`docs/milestones/M3.md`](docs/milestones/M3.md)

## v1.0 — Public product
- Privileged Windows service: input into elevated windows, visible UAC prompts.
- Signed auto-update (host) and update check (APK).
- Italian UI alongside English.
- Portrait mode and rotation; mirror mode.
- winget, Google Play, F-Droid; documentation site; demo video; published reproducible benchmark.
- GitHub Sponsors.

## v1.x — Quality and experience
- Perfect text: still-screen 4:4:4 refinement pass.
- Predictive ink: the tablet draws the stroke immediately, the PC frame replaces it.
- Several tablets at once.
- Shared clipboard; tablet keyboard-cover input; file drag and drop.
- "Send this window to the tablet" from the tray.
- AV1 verified on real devices (Snapdragon 8 Gen 2+, RTX 40+, Arc, Ryzen 7040+).

## v2 — Linux
Wayland host (KDE Plasma, GNOME): PipeWire capture, VAAPI encode, compositor virtual outputs, RemoteDesktop portal / libei input. Optional one-way PC → tablet audio, Linux first.

## Parking lot (not promised)
- Desktop client (a second laptop as a monitor), cheap thanks to the shared Rust core.
- Pen-only "graphics tablet" mode without video.
- iPadOS client (network only).
- macOS host (needs non-public virtual display APIs).

## Never (ADR-0013)
Tablet RAM/swap for the PC · compute or LLM offloading · bidirectional audio mixing · USB + Wi-Fi bonding · X11 host.

## Repository quality (continuous)
CONTRIBUTING, CODE_OF_CONDUCT, SECURITY, issue and PR templates · an ADR per decision · CI with build, tests, lint, licence and advisory checks · a write-up "how we reached X ms" with benchmark charts.
