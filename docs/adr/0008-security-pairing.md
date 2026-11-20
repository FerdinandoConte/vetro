# ADR-0008: Mandatory QR pairing and pinned mutual TLS on every transport

Status: Accepted · 2026-11-20

## Context
The engine accepts input from the network and injects it into the PC: an unauthenticated engine is a remote-control backdoor. The stream also shows the user's desktop.

## Decision
- Each side generates its own long-term key and self-signed certificate locally.
- Pairing via a QR code shown by the tray UI: host certificate fingerprint, a single-use 128-bit token (5-minute TTL), addresses and port. The client checks the fingerprint, presents its own certificate, sends the token; the host pins the client fingerprint.
- After pairing, every connection on every transport (Wi-Fi, tethering, AOA) is mutual TLS with both fingerprints pinned. No insecure or "performance" mode exists.
- Parsers are fuzzed and property-tested; input is accepted only from an authenticated session.
- Unpairing from the tray UI revokes a device immediately.

## Consequences
- Encryption costs well under 0.1 ms per frame with AES-GCM/ChaCha20 at 20 MB/s; no measurable latency impact.
- First connection requires the tablet camera (or a typed fallback code, to be designed in M1 for devices without a camera).

## Alternatives
- PIN-only pairing (Moonlight-style): weaker against active MITM unless combined with fingerprint display.
- Trust-on-first-use without user confirmation: rejected.
