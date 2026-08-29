---
name: supervise-agent
description: Persistently supervise a WORKER agent (another Claude Code, a Codex/Gemini CLI, an interactive REPL, or a long build) running in an aterm session — dispatch work, then review each turn against GROUND TRUTH (tests, diff, exit codes — never just the screen), approve or deny its prompts, answer its questions, escalate when unsure, and resume after a restart. YOU are the reviewer. Use when asked to manage, babysit, oversee, or operate another agent or a long-running session, to run a worker unattended under a safety budget, to keep a coding agent going and check its work, or to act as a manager/lead over other sessions.
---

<!-- aterm skill v1 — MANAGED FILE, written by `aterm agents install`.
     Edits are overwritten on update. To keep your own version, remove this
     marker line and aterm will leave the file alone (reported as `foreign`). -->

# Supervising a worker agent

You are the MANAGER of a worker — another agent, a REPL, or a long build — living
in an aterm session. Operate it the way an attentive human lead would: give it
work, watch what it shows, judge the result against ground truth, and step in only
when needed. Indefinitely, and safely.

This is the SUPERVISION layer. The mechanics of reading and driving one session
live in the `drive-aterm` skill and in `aterm ctl --help` (the build-generated,
drift-proof verb list). This file is the loop and the judgment on top of them.

## The one thing that matters: YOU are the reviewer

The loop is dumb; your judgment is the entire value. A supervisor that reads the
worker's screen and says "looks good, continue" is a rubber stamp — *worse* than
no supervisor, because it launders unreviewed work as reviewed. So:

> **Review against GROUND TRUTH, not the screen.**

The worker will announce "all tests pass" / "done". Do not take its word. Run the
check yourself — the tests, the build, `git diff`, the exit code — and decide from
THAT. The gap between what a worker claims and what the ground truth shows is the
whole reason you are in this loop. (Observed for real: a worker reported "all
tests pass" having written only the implementation and no test file at all.)

## Set up the worker

Two ways in:

- **Attach** to a session the human points you at — its sid is your worker:
  ```sh
  aterm ctl ls            # find it; SID is field 3
  SID=s-...
  ```
- **Spawn** a fresh, isolated one you own (headless = CI-safe; `--window` to watch):
  ```sh
  RUN=$(mktemp -d); SOCK=$RUN/c.sock
  ATERM_CONTROL_SOCK=$SOCK XDG_RUNTIME_DIR=$RUN ATERM_COLUMNS=120 ATERM_LINES=40 \
    aterm-gui --headless >"$RUN/gui.log" 2>&1 &
  for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.1; done
  SID=$(aterm ctl --sock "$SOCK" ls | awk 'NR==1{print $3}')
  aterm ctl --sock "$SOCK" "@$SID" turn 'cd <workdir> && exec claude'   # launch the worker
  ```
  The worker is anything interactive or long-running — `claude` here, but
  `codex`/`gemini`, a REPL, or `make build` work the same way; substitute the
  launch command. Use a **plain** session — NOT `spawn connected=controller`, which injects
  `ATERM_OBSERVE_SESSION_ID`, a marker the worker can read. Launched plainly, the
  worker sees only the generic in-aterm environment (`CLAUDE*`/`ANTHROPIC_*` are
  stripped from every child), so it behaves exactly as if a human started it.

**Sandbox anything you run UNATTENDED.** Use a *disposable checkout* — a separate
clone or a `git worktree` in a throwaway path — never the user's live tree. A
branch is **not** a sandbox: it protects committed history, not the filesystem, so
a stray write or `rm` still hits your real files. For stronger isolation, spawn
the worker with `aterm-gui --headless --sandbox` (containment mode: no network,
writes confined to `/tmp`) when the task fits inside it. Your budget and breaker
are a discipline, not a sandbox.

Keep a durable **notes file** with what a fresh copy of you needs to resume:
objective, worker sid + socket, the ground-truth command, budget remaining, and
one line per action taken. That file — not your context window — is your memory
across restarts.

## The loop

Repeat until **done**, **escalated**, or **budget spent**:

1. **SWEEP** — cheapest possible change check.
   ```sh
   aterm ctl "@$SID" status        # phase=… revision=…   (revision is the change signal)
   ```
   Same `revision` as the last one you acted on → nothing new; go to WAIT. This is
   what keeps you from re-reviewing (and re-billing) an unchanged screen.

2. **CLASSIFY** — from `phase` plus one screen read (`aterm ctl "@$SID" text`):

   | you see | do |
   |---|---|
   | busy indicator still spinning (`esc to interrupt`) | WAIT — not a review point yet |
   | the busy indicator that *was* there is now gone | the turn finished → go REVIEW |
   | an approval box (`Do you want…`, `1. Yes / 2. No`, trust-folder prompt) | read WHAT it asks. Matches the task and is safe → approve (`key enter`, or the number). Surprising, destructive, or off-task → deny (`key escape`) and redirect, or ESCALATE |
   | a prose question, composer idle | answer it with a `turn`, from your notes |
   | a non-TUI worker (build/script/REPL) still streaming output (`phase` running) | WAIT — for these, completion is a returned shell prompt or `phase=exited`, at which point go REVIEW via the **exit code + expected artifacts**, not a busy indicator |
   | a shell prompt where an *interactive agent* used to be | that agent exited — check why, relaunch + re-brief, or ESCALATE (a build returning to the prompt is normal completion, see the row above) |
   | anything you cannot confidently read | **NEVER type into an unknown screen** — ESCALATE |

3. **REVIEW** (only once the turn is DONE) — run the ground truth, judge against
   it, then take exactly one action:
   - **next** — correct but unfinished → drive the next instruction.
   - **revise** — ground truth fails / work is wrong → drive the correction.
   - **answer** — it asked something → drive the answer.
   - **done** — ground truth PROVES completion for THIS objective (tests pass, exit 0, the expected artifact exists — whatever the objective's check actually is) → STOP.
   - **escalate** — unsafe, surprising, or you cannot tell → STOP for a human.

   Drive with ONE verified human turn:
   ```sh
   aterm ctl "@$SID" turn idle=2500 timeout=600000 '<single-line instruction>'
   ```
   Keep the instruction to **one line with balanced quotes and parens**. Claude
   Code's composer treats Enter as a newline (not submit) while a bracket is open,
   so a truncated or unbalanced instruction silently piles up in the input box and
   never runs. Prefer idle-settle over prompt-matching for completion: an
   interactive worker's composer glyph (Claude Code `❯`, Codex `»`) is on screen
   the *entire* time it thinks, so matching it returns mid-turn — and Codex's `»`
   in particular stays visible even while it works, so its presence means nothing.

4. **WAIT** — block on the one session most likely to move next. Never busy-poll;
   never park more than one waiter (each holds a control lane).
   ```sh
   aterm ctl "@$SID" await idle 4000 timeout=300000   # settling? exit 124 = still busy = an answer
   aterm ctl "@$SID" await seq timeout=15000          # idle + nothing new → wait for fresh output
   ```

## Bounded autonomy — this IS the safety floor, not optional

- **Budget.** Pick a max number of drives before you stop and report. Decrement
  per drive; at zero, ESCALATE. Never loop unbounded.
- **No-progress breaker.** If you drive 2–3 times and `revision` never advances,
  the worker is wedged or ignoring you — STOP and escalate; do not keep driving.
- **Escalate** = raise a needs-human flag and stop acting:
  ```sh
  aterm ctl "@$SID" meta set attention '<why a human is needed>'
  ```
  A non-empty `attention` is the typed escalation aterm's menu-bar UI badges.
- **Yield to a human.** You *cannot* distinguish a human typing at the keyboard
  from your own input — they are byte-identical by design — so the handoff is
  explicit: when told to stand down, STOP. Yield to another *socket* driver via
  the lease: if `aterm ctl "@$SID" lease status` shows a holder that is not you,
  do not drive.
- **Never type into a screen you cannot read.** Unknown → escalate, every time.

## Report when you stop

State it plainly: the outcome (done / escalated / budget), the **ground truth you
verified against** (not the worker's self-claim), how many turns it took, and — if
escalated — exactly what a human must decide. Leave the worker where it is for
inspection; only tear down a session you spawned for a one-off.

## See also

- The `drive-aterm` skill and `aterm ctl --help` — the read/drive verbs this composes.
- `aterm help`, and (in the aterm source) `docs/OPERATOR.md` — the fuller operator brief.
