# ADR-0004: Desktop Duplication, damage-driven encoding, client-side cursor

Status: Accepted · 2026-11-20

## Context
Encoding 2960×1848 at 120 fps continuously on a thin laptop's iGPU risks thermal throttling, and a second monitor shows static content most of the time. Desktop Duplication (DDA) delivers the exact composed pixels as a GPU texture, reports dirty rectangles, and reports pointer updates separately from frame updates.

## Decision
- Capture the VDD output with DDA on the GPU, lossless, no CPU readback.
- Encode only when something changed: no damage → no frame. "120 Hz" means up to 120 fps while content moves and 0 fps while it is still.
- Send the cursor's shape and position as separate messages and draw it on the client as an overlay, so pointer movement over a still screen needs no video frame.

## Consequences
- Power and heat follow screen activity, not refresh rate.
- The client must handle long gaps between frames (no "stream stalled" false alarms) and keep showing the last frame.
- Secure desktop (UAC, lock screen) cannot be captured by an unprivileged process: the client shows a banner until a privileged service exists (v1.0).

## Alternatives
- Windows.Graphics.Capture: similar quality, less direct dirty-rect information, possible extra frame of latency; kept as a fallback if DDA misbehaves on a driver.
- Frames taken directly from the driver's swap chain: possibly lower latency but requires driver changes; revisit only if measurements demand it.
