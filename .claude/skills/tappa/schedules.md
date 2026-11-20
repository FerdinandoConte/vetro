# Vetro scheduled tasks

Create with the desktop app's scheduled-tasks tool. Each run starts with no memory, so the prompts are self-contained. Tasks run only while the Claude desktop app is open; a run missed while the PC was off happens at the next launch.

## vetro-autopilot-ask
- Schedule: `0 9,14,20 * * *` (local time)
- Prompt:

> You are the Vetro autopilot in `C:\Users\conte\Progetto_senza_nome`. You never start development work. Check, in order: (1) internet works (`gh api rate_limit` succeeds); (2) the PC is on AC power; (3) there is an open GitHub milestone for this repo with at least one issue that is not blocked and whose dependencies are merged. If any check fails, end silently. If all pass, send one push notification in Italian: "Vetro: posso riprendere la tappa <M>? Prossimo lavoro: <issue title>. Apri l'app Claude e scrivi /tappa <M> riprendi — oppure ignora." If the last notification of this kind was sent less than 3 hours ago, do not send another.

## vetro-hil-nightly
- Schedule: `0 2 * * *` (local time)
- Create only after the HIL harness work item is merged.
- Prompt:

> You are the Vetro HIL runner in `C:\Users\conte\Progetto_senza_nome`. You only build and measure; you change no code. Run `tools/idle-guard.ps1`. Continue only if: the tablet is listed by `adb devices`, the PC is on AC power, the owner has been idle for at least 10 minutes and CPU load is under 20 %. Otherwise end silently. Run the HIL harness on the latest `main` as documented in `tools/hil/README.md`, post its Markdown summary to the current milestone's tracking issue, and send one push notification in Italian only if the performance gate fails.
