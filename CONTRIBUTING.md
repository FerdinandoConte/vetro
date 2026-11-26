# Contributing to Vetro

Thanks for helping. Vetro is dual-licensed under MIT OR Apache-2.0; by contributing you agree your work is released under both (see [ADR-0014](docs/adr/0014-licensing-distribution.md)).

## Before you start
- Read [ARCHITECTURE.md](ARCHITECTURE.md) for the area you touch and the relevant [ADRs](docs/adr/README.md). Changing a decision means a new ADR that supersedes the old one.
- Work is tracked as one GitHub issue per work item (see `docs/milestones/`). Comment on the issue before starting; open one first for anything new.
- Features listed in [ADR-0013](docs/adr/0013-out-of-scope.md) are declined.
- Do not copy code from GPL projects (Sunshine, Moonlight, Apollo): reading them for ideas is fine, we write our own ([ADR-0001](docs/adr/0001-strategy-new-rust-core.md)).

## Setup
- Rust stable (the `rust-toolchain.toml` picks it up). On Windows: MSVC target with Visual Studio 2022 Build Tools (C++ workload).
- Android: JDK 17, Android SDK, NDK 28 and `cargo-ndk` (ARCHITECTURE section 14).
- The Gradle wrapper is not committed yet (added in M1-6); until then use a locally installed Gradle 9.6.0 (the version AGP 9.5 requires) in `android/`.
- FFmpeg (LGPL shared build): `tools/fetch-ffmpeg.ps1`.

## Pull requests
1. One issue, one PR. Branch name `m<N>/<work-item-id>-<slug>`, for example `m1/M1-2-protocol`.
2. Commits follow [Conventional Commits](https://www.conventionalcommits.org/) (`feat(protocol): ...`, `fix: ...`, `docs: ...`).
3. Fill in the PR template: every acceptance criterion of the issue is checked with evidence (test name, log, screenshot or numbers).
4. Run locally before pushing:
   ```text
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo deny check
   cargo audit
   ```
   and the Android build (`cargo ndk` + `gradle assembleDebug` in `android/`) when you touch `android/` or `crates/vetro-client`.
5. New logic has tests; parsers have property tests; a hot-path change carries before/after HIL numbers (ARCHITECTURE section 11).
6. Docs follow the code: ARCHITECTURE when behaviour changes, an ADR when a decision changes, the milestone status row always.
7. `main` only changes through a PR with all required CI checks green and a reviewer approval.

Hot-path code follows the real-time rules in ARCHITECTURE section 10. Secrets never enter the repository, logs or issues.

## Security
Do not open public issues for vulnerabilities; see [SECURITY.md](SECURITY.md).

## Conduct
Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
