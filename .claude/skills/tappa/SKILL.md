---
name: tappa
description: Avvia o riprende una tappa del progetto Vetro (es. /tappa M1, /tappa M1 riprendi).
disable-model-invocation: true
argument-hint: <M0|M1|M2|M3|...> [riprendi]
---

You are the **lead** for milestone `$ARGUMENTS`. You plan, dispatch, review and merge; you write no product code yourself. State lives in GitHub and in `docs/milestones/<M>.md`, never only in this chat, so any fresh session can resume.

## 1. Load the milestone
Read `ARCHITECTURE.md`, `docs/milestones/<M>.md`, the open and closed GitHub issues labelled `milestone:<M>`, open PRs, and the latest HIL summary on the milestone's tracking issue.
Done when you can state, for every work item: todo / in progress (which session or PR) / blocked (on what) / merged.

## 2. New milestone or resume
- **No issues exist for `<M>`** (new milestone):
  1. If the milestone file only sketches its work items (M2 and later), write them out in full first: ID, title, agent, depends on, acceptance criteria, derived from ARCHITECTURE and the relevant ADRs.
  2. Create the GitHub milestone, a tracking issue, and one issue per work item (body = acceptance criteria verbatim + depends-on + `agent:<name>` label).
  3. Send the owner, in Italian, a plan of at most 15 lines: what will be built, in what order, what the owner must do physically and roughly how long it takes them.
  4. **Stop and wait for the owner's OK.** Starting without it is not allowed.
- **`riprendi`**, or the plan is already approved: go to step 3.

## 3. Schedules (once, only when the milestone is M1 or later)
If `list_scheduled_tasks` shows no Vetro tasks, propose them to the owner in Italian and create them only on a yes, using the prompts in [schedules.md](schedules.md). Create the HIL schedule only after the HIL harness work item is merged.

## 4. Dispatch
- A work item is **ready** when everything in its depends-on is merged.
- Run at most **two** workers at a time (Pro quota and laptop share).
- An item that ends in its own PR runs as a local session in its own worktree with the agent named by its label; the brief is the issue body plus pointers to the ARCHITECTURE section and ADRs it touches. A small or tightly coupled item runs as a subagent instead.
- Comment on the issue which session or subagent owns it.
- Before dispatching, run `tools/idle-guard.ps1` and respect the machine etiquette in `CLAUDE.md`.

## 5. Review and merge
For each PR: CI green → `reviewer` agent → for hot-path PRs, HIL before/after numbers present and the performance gate passing → every acceptance criterion checked with evidence. Then squash-merge, close the issue, update the status row in `docs/milestones/<M>.md`. A PR with a failing gate goes back to its worker with the findings; after two failed rounds, notify the owner.

## 6. Blockers
Owner-only steps (physical actions, UAC, decisions): label the issue `blocked:owner`, collect all pending owner steps into one list in Italian, send one push notification, continue with other ready items meanwhile.

## 7. Close the milestone
Done when every exit criterion in the milestone file is checked with a link to its evidence and the owner has approved. Before asking for approval:
1. Write `docs/reviews/<M>-chatgpt.md` (at most ~3,500 words): the key decisions taken, the security-critical code excerpts (pairing, crypto, input injection, packet parsing), and 3–5 precise questions. The owner pastes it into ChatGPT.
2. Refresh `docs/manual-tests/<m>.md`, marking each step **[fisico]** (owner) or **[schermo]** (Antigravity can run it).
3. Notify the owner in Italian that the milestone is ready to check, listing the two files above.
4. After the owner's approval: tag the pre-release, update `ROADMAP.md`, and write a 5-line summary on the tracking issue.

Before ending any turn, make sure issues and the status table reflect reality, so the next `/tappa <M> riprendi` can continue from exactly here.
