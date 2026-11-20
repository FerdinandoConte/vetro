# ADR-0011: Headless engine and Tauri tray UI as separate processes

Status: Accepted · 2026-11-20

## Context
The UI must never touch the latency path; agents must be able to run and test the engine without a GUI; a privileged Windows service is planned for v1.0. The UI should look professional and be reusable on Linux.

## Decision
- `vetro-engine.exe`: headless host binary owning the hot path, with CLI flags for tests (`--test-pattern`, `--no-vdd`, `--telemetry-out`).
- `Vetro.exe`: Tauri 2 tray app (pairing QR, devices, settings) talking to the engine over a per-user named pipe with JSON-RPC.
- In v1.0 the engine can run as a Windows service without changing the UI.

## Consequences
- An IPC contract to version and test.
- WebView2 is required by Tauri (present on Windows 11; bootstrapped by the installer on Windows 10).

## Alternatives
- Single process: simpler now, but a UI crash or stall could hit streaming, and the service move would be a rewrite.
- egui: lighter but less polished; WinUI 3: Windows-only with weak Rust bindings.
