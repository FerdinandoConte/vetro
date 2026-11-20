# Vetro — operating brief for agents

Vetro turns an Android tablet into an extended monitor for a Windows PC (Wi-Fi and USB), at native resolution and refresh rate, with measured glass-to-glass latency. Rust everywhere, Kotlin only for Android UI. Quality beats speed: every merged line meets the definition of done below.

## Where things are
- `ARCHITECTURE.md` — the system as it must be built. Read the section for your area before changing code in `crates/`, `apps/` or `android/`.
- `docs/adr/` — why each decision was taken. Changing a decision means a new ADR that supersedes the old one, never a silent change.
- `docs/milestones/<M>.md` — work items, acceptance criteria, exit criteria and status of each milestone.
- `ROADMAP.md` — what comes after the current milestone; `docs/adr/0013-out-of-scope.md` — what never comes.
- `docs/it/GUIDA.md` — the owner's plain-Italian guide. Update it in the same PR whenever the process the owner follows changes.

Milestones are driven by the `/tappa` skill (owner-invoked). Work-item agents live in `.claude/agents/`.

## Routing
| Work | Agent | Model |
|---|---|---|
| Hot path (capture, encode, transport, decode, present, input), protocol, `unsafe`/FFI, security, concurrency, performance debugging, HIL harness | `core-engineer` | Opus |
| Well-specified UI (Tauri, Compose), tests written from a clear spec, CI, installer, docs | `ui-engineer` | Sonnet |
| Review of every PR | `reviewer` | Opus |
| Non-code chores: run scripts, collect logs, format reports, update status tables | `chores` | Haiku |

A `ui-engineer` task that turns out to touch the hot path, security or the protocol is handed back to the lead for `core-engineer`.

## Definition of done (every PR)
1. One issue, one PR, branch `m<N>/<work-item-id>-<slug>`, Conventional Commits.
2. Each acceptance criterion of the issue is checked off in the PR body with evidence (test name, log, screenshot or numbers).
3. New logic has tests; parsers have property tests; a hot-path change carries HIL numbers before and after (ARCHITECTURE §11).
4. Green locally and in CI: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, tests, `cargo deny check`, Android build.
5. `reviewer` verdict is APPROVE.
6. Docs follow the code: ARCHITECTURE when behaviour changes, ADR when a decision changes, the milestone status row always.

Hot-path code follows the real-time rules in ARCHITECTURE §10.

## Working with the owner
- The owner is an Italian student, not a low-level expert, studying on the same laptop. Write every message to the owner in Italian, plain words, one short question with a recommended answer. Code, commits, issues and docs are in English, except `docs/it/` and owner checklists.
- Contact the owner only for: a decision that blocks work, a batch of physical steps (cable, pen, camera, UAC prompts), a milestone ready for approval, a performance gate failing twice. Batch requests; one push notification per batch.
- The owner approves each milestone's plan before work starts and each milestone's result before it is tagged.

## Machine etiquette
The laptop is the owner's study machine. Before heavy builds or benchmarks run `tools/idle-guard.ps1` (created in M0-1):
- owner active (input in the last 10 min): below-normal priority, at most 8 build jobs, pause new jobs while free RAM < 3 GB;
- owner idle ≥ 10 min: full power, pause while free RAM < 1 GB;
- benchmarks and HIL only when idle ≥ 10 min, on AC power, CPU < 20 %; otherwise mark the run noisy.

## Hard boundaries
Prepare the exact command, explain it in Italian, and let the owner run it for: anything needing administrator rights, driver installs, software installs (winget/choco/installers), registry or network configuration, signing-account or store-account setup. `main` changes only through a PR with all gates green. Secrets never enter the repo, logs or issues. GPL code from Sunshine, Moonlight or Apollo may be read for ideas; we write our own (ADR-0001). Features listed in ADR-0013 are declined with a link to it. Public files name "Vetro contributors", never the owner's personal name or email.
