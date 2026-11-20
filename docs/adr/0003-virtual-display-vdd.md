# ADR-0003: Virtual monitor through Virtual Display Driver

Status: Accepted · 2026-11-20

## Context
Windows needs a display driver to expose an extra monitor. Writing an Indirect Display Driver means C++/WDF, an EV certificate and Microsoft attestation signing: months of work and recurring cost. VirtualDrivers/Virtual-Display-Driver is MIT-licensed (LICENSE file checked 2026-11-20), signed through SignPath, built on IddCx, and supports arbitrary resolutions and refresh rates up to 500 Hz.

## Decision
Bundle a pinned, signed VDD release in our installer (SHA-256 verified, MIT notice included) and drive it from the engine through VDD's own configuration interface, behind the `VirtualDisplay` trait. We write no driver code.

## Consequences
- Installing the driver needs administrator rights once, at install time.
- We depend on VDD's control interface; the M1 spike documents it and the trait isolates it.
- If VDD becomes unmaintained, the trait allows switching to another IddCx driver (e.g. SudoVDA) without touching the rest of the engine.

## Alternatives
- Own IddCx driver: rejected for cost and signing.
- Parsec VDD: proprietary licence, redistribution unclear.
- Physical HDMI dummy plug: hardware dependency, refresh limited by the plug's EDID.
