# ADR-0002: Windows host and Android client first

Status: Accepted · 2026-11-20

## Context
Supporting Windows and Linux (X11 and Wayland) hosts from day one doubles every backend: virtual display, capture, encode, input. One developer working part-time cannot ship that. The owner's hardware is a Windows laptop and an Android tablet.

## Decision
- MVP host: Windows 10 22H2+ and Windows 11 (tested on 11).
- MVP client: Android 11+ (tablets and phones).
- Linux host in v2, **Wayland only** (KDE Plasma and GNOME first), through the same backend traits.
- No X11 host, ever: there is no clean way to create a virtual monitor there.
- iPadOS or other clients are not planned; the documented, versioned protocol is the only opening towards them. No abstraction layers are built for hypothetical clients.

## Consequences
- Backend traits (`VirtualDisplay`, `Capture`, `Encoder`, `InputInjector`) exist from day one with a single implementation each.
- The protocol crate stays platform-independent and testable on Linux CI.

## Alternatives
- Cross-platform host from day one: rejected for scope.
