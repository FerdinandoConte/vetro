# ADR-0005: FFmpeg encoding behind an Encoder trait, codec negotiation, 4:2:0 first

Status: Accepted · 2026-11-20

## Context
The project targets many PCs (Intel, NVIDIA, AMD; desktops and laptops) and many tablets. Writing against each vendor SDK triples the code. Mobile hardware decoders do not decode HEVC 4:4:4, so colour is subsampled (4:2:0), which slightly blurs small coloured text. Alder Lake cannot encode AV1 and Snapdragon 8 Gen 1 cannot decode it.

## Decision
- Use FFmpeg (LGPL shared build, dynamically loaded) for QSV, NVENC, AMF, and the Media Foundation fallback, behind our own `Encoder` trait. A native vendor backend may be added behind the same trait only where FFmpeg measures worse.
- Negotiate the codec per session: AV1 > HEVC > H.264, picked from host encoders ∩ client decoders. AV1 is labelled experimental until verified on real devices.
- Mandatory low-latency settings on every backend (see ARCHITECTURE §5.3).
- MVP uses 4:2:0. The protocol reserves a "refinement" frame type so a later still-screen 4:4:4 colour pass (in the spirit of RDP's AVC444) can be added without a protocol break.

## Consequences
- One code path covers almost every GPU; vendor quirks become per-backend presets.
- LGPL compliance: dynamic linking, shipped DLLs replaceable, licence notice included, no GPL-only encoders (x264/x265) ever linked.
- Text sharpness on mobile is bounded by the decoder until the refinement pass exists.

## Alternatives
- Native SDKs per vendor (oneVPL, NVENC SDK, AMF): more control, three times the code; kept as targeted fallbacks.
- Media Foundation only: universal but weaker low-latency control.
