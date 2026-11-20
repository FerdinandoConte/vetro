# ADR-0009: Native touch and pen injection on a dedicated stream

Status: Accepted · 2026-11-20

## Context
Input costs almost nothing in bandwidth (~10 KB/s) or CPU; its costs are complexity and correctness. `SendInput` cannot express multi-touch or pen pressure. Windows' UIPI blocks injection into elevated windows from an unprivileged process, and nothing can inject into the secure desktop.

## Decision
- Touch as native Windows touch (10 contacts) and pen with pressure, tilt, rotation, hover, barrel and eraser, through `CreateSyntheticPointerDevice` + `InjectSyntheticPointerInput`. No mouse emulation.
- Input travels on its own highest-priority QUIC stream and is injected by its own high-priority thread, never queued behind video.
- Client side: `requestUnbufferedDispatch` for full sensor rate; coordinates normalised to the virtual monitor; palm rejection and system cancels forwarded as cancels.
- MVP is unprivileged: no input into elevated windows. A privileged service lifts this in v1.0.
- Later (v1.x): predictive ink on the tablet (front-buffered local stroke replaced by the real frame).

## Consequences
- Touch and pen are in the MVP, not an optional extra.
- The limitation on elevated windows is documented in the README and shown in the UI.

## Alternatives
- Mouse emulation: loses multi-touch and pressure.
- A virtual HID driver: kernel-mode signing cost, rejected.
