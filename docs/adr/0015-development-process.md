# ADR-0015: Local Claude Code agents with quality gates and an ask-first autopilot

Status: Accepted · 2026-11-20

## Context
The owner is a student with a Claude Pro plan who wants development to progress while studying, without any loss of code quality, with agents running locally (no cloud agent sessions unless truly necessary). ChatGPT is available with few tokens (manual paste only); Antigravity can run mechanical on-screen checklists. The laptop is both the study machine and the test rig, and is not always on or online.

## Decision
- **Roles:** one lead session per milestone (Opus, effort high) that plans, dispatches, reviews and merges but writes no product code; workers as local sessions in git worktrees or as subagents, routed by `.claude/agents/` definitions; a reviewer agent (Opus) on every PR.
- **Model routing for code quality:** Opus for the hot path, `unsafe`/FFI, security, concurrency, protocol, debugging and every review; Sonnet only for well-specified UI, tests-from-spec, CI, installer and docs, always reviewed by Opus; Haiku only for non-code chores.
- **Gates:** CI green, reviewer approval, performance gate ([ADR-0012](0012-latency-slo-measurement.md)), protected `main` (status checks required, admins included). Agents merge only when every gate is green; the owner approves each milestone's plan and its completion.
- **State lives in GitHub** (milestone, issues, PRs) and in `docs/milestones/`, so any fresh session resumes with `/tappa <M> riprendi`.
- **Ask-first autopilot:** scheduled local tasks never start work on their own. At fixed times they check preconditions (online, AC power, an open milestone) and send a push notification asking the owner to resume; missed runs while the PC is off simply ask once at next launch. The HIL run is the exception: it only builds and measures, and runs only when the tablet is plugged in, the PC is on charger and idle — leaving the tablet plugged in is the owner's consent.
- **Resource policy:** when the owner is active, builds run at below-normal priority with half the CPU threads and a 3 GB free-RAM guard; when idle ≥ 10 min, full power with a 1 GB guard; benchmarks only when idle.
- **External reviewers:** at each milestone the lead writes a compact review packet (key decisions + security-critical excerpts) for the owner to paste into ChatGPT; Antigravity runs the on-screen manual checklists; physical steps are batched for the owner once per milestone.
- Cloud CI (GitHub Actions) is used for builds and signing only; no cloud agent sessions.

## Consequences
- Progress speed is bounded by the Pro quota and by the owner's approvals, by design.
- No external orchestration framework; Claude Code's native sessions, subagents, worktrees, scheduled tasks and notifications are enough.
