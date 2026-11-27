# Security policy

## Reporting a vulnerability
Please do not open a public issue. Use GitHub's private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**.

Include what you found, how to reproduce it, the affected version or commit, and the impact you expect. You will get an acknowledgement within 7 days and a status update at least every 14 days until it is resolved.

## Supported versions
Vetro is pre-1.0. Only the latest release (and `main`) receives security fixes.

## Scope
In scope: the host engine and tray (`crates/`, `apps/`), the Android client, the pairing flow and the transport (see ARCHITECTURE section 8 for the threat model).

Out of scope: vulnerabilities in third-party components that are not specific to how Vetro uses them (report those upstream), and attacks that need prior administrator access to the PC.

## Disclosure
We fix first and then publish an advisory crediting the reporter, unless you prefer to stay anonymous. Releases are signed and ship with SHA-256 checksums (ADR-0014); no telemetry leaves the user's PC.
