# ADR-0014: MIT OR Apache-2.0, free forever, signed releases, no telemetry

Status: Accepted · 2026-11-20

## Context
The project is a free, open-source portfolio product. Commercial clones are possible under a permissive licence; the brand should stay the owner's. Unsigned Windows binaries trigger SmartScreen warnings.

## Decision
- Code licensed **MIT OR Apache-2.0** (Rust ecosystem standard). The names "Vetro" and "Vetro Display" and the logo are not licensed for forks (Apache-2.0 §6 grants no trademark rights).
- Free forever, no paid features. GitHub Sponsors from v1.0. A paid store build identical to the free one may be reconsidered only with a large user base (and tax/legal advice first).
- Third-party notices shipped for VDD (MIT) and FFmpeg (LGPL, dynamically linked, replaceable DLLs).
- Releases on GitHub: Windows installer and binaries signed through SignPath Foundation (CI-built), APK signed with the project release key, SHA-256 checksums, changelog, semantic versioning. winget, Google Play and F-Droid after v1.0. Public repository from day one.
- No telemetry leaves the user's PC; diagnostics export is manual.

## Consequences
- `cargo deny` enforces the dependency licence allowlist in CI.
- Codec patent questions must be reviewed by a professional before any paid distribution.
