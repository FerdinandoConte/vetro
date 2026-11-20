# ADR-0001: New Rust core that reuses components, not a fork

Status: Accepted · 2026-11-20

## Context
Apollo (a GPLv3 Sunshine fork) with SudoVDA and Artemis (a Moonlight Android fork) already stream a Windows desktop to an Android tablet through a virtual display that matches the client's resolution and refresh rate. They are built for game streaming: sessions, app lists, no "plug in and it is a monitor" experience, no zero-config USB. The project must be a portfolio-grade codebase, preferably Rust, built mostly by AI agents with the owner reviewing.

## Decision
Write a new core in Rust and reuse proven components through their public interfaces rather than their source: Virtual Display Driver, FFmpeg, Android MediaCodec, quinn. Before writing code, measure Apollo + Artemis on the reference hardware (milestone M0) and treat their numbers as the baseline to beat.

The product's reason to exist (its wedge): monitor-like UX (plug in, it is a screen), USB without developer options (AOA), first-class pen input, and published per-stage latency.

## Consequences
- Our licence is our choice ([ADR-0014](0014-licensing-distribution.md)); GPL code from Sunshine/Moonlight may be read for ideas but never copied.
- Longer path to a first demo than a fork (estimated 6–12 weeks on a Claude Pro plan), in exchange for an architecture the owner fully understands and can present.
- The protocol is ours and is implemented once in Rust for both sides.

## Alternatives
- **Fork Apollo + Artemis:** fastest first result, but C++/Java, GPLv3, a huge codebase that is not ours, and a weaker portfolio story.
- **Installer/launcher that configures Apollo + Artemis:** days of work, no engineering to show.
