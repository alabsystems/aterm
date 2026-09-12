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
  aterm ctl --sock "$SOCK" "@$SID" status                                # expect detail=claude
  ```
  On older builds that compound line reads `detail=cd` (the first word, not the segment
  that runs), so when `detail=` is a shell builtin confirm the worker is up with `text`.
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
across restarts. Point `aterm drive supervise --notes` (or `watch --notes`) at the
same file: it appends one UTC-stamped line per approval or hand-off.

## The loop

Repeat until **done**, **escalated**, or **budget spent**. `aterm drive` carries the
mechanical steps (`aterm drive --help`, *SUPERVISING A WORKER*); an older `aterm` answers
`unknown command 'phase'`, and the manual form under each step is the same loop by hand.

1. **SWEEP** — cheapest possible change check.
   ```sh
   aterm ctl "@$SID" status        # phase=… revision=…   (revision is the change signal)
   ```
   Same `revision` as the last one you acted on → nothing new; go to WAIT. This is
   what keeps you from re-reviewing (and re-billing) an unchanged screen.

2. **CLASSIFY** — one read, one word:
   ```sh
   aterm drive phase "@$SID"       # busy | prompt | limited | idle | question — for a prompt the parsed box follows
   ```
   After `prompt` come `kind bash|edit|write|read|workflow|other`, `command <line>`,
   `description <text>`, `classify read-only` or `classify not-read-only <reason>` (Bash
   only), one `option N <text>` per option, and `cancel esc|none`. `busy` is read only
   from the LIVE ZONE around Claude Code's composer: the status row — the lowest row above
   its top rule that starts with a spinner glyph, before any transcript row; a tip, a todo
   list, a hint, the session survey or a banner under it never hides it — when it is a
   spinner, `Waiting for N …` a workflow or a background agent, or a done row still
   counting a shell; and the footer under its bottom rule (`esc to interrupt`, `· N
   shell(s) ·`, a workflow's `◯ … agents done`). A monitor still running (`· N monitor(s)
   ·`, `N monitor(s) still running`) is busy too, but soft: a question or a limit notice
   outranks it, since a persistent monitor can run for as long as the worker lives. A
   status row above the transcript is history, not a signal; `phase` prints `reason
   <where>: <rule>` under `busy` so you can check which one fired. The footer does not
   always say `esc to interrupt` while a turn runs, so the status row is the first signal
   — and a prompt wins over busy (a worker blocked on a box cannot proceed however many
   shells its footer counts). `limited` = its last turn ENDED on a usage or rate limit
   notice: the last thing said above the composer is Claude Code's notice under the `⎿`
   gutter (`message <text>` and `reset <text|->` follow) — never the worker's own words
   about limits, and never a notice something was said after. `question` = the last
   thing it said, above the composer, ends in `?`. Manual form:
   `aterm ctl "@$SID" text tail=40` (an older build answers `ERR usage` — read the full
   `text`) and this table:

   | you see | do |
   |---|---|
   | `busy` — a live spinner or `Waiting for N …` row just above the composer, `esc to interrupt`, or a shell or monitor still running | WAIT — not a review point yet. A monitor alone is soft busy: a question or a limit notice under it reads `question` / `limited`, because a monitor can run for hours |
   | the busy indicator that *was* there is now gone | the turn finished → go REVIEW |
   | `prompt` — an approval box (`Do you want…`, `1. Yes / 2. No`, trust-folder prompt) | read WHAT it asks. Matches the task and is safe → approve GUARDED on a row the box shows: `aterm ctl "@$SID" key if=Do.you.want.to.proceed 1` (the option's number, or `enter`); `OK skipped seq=<n>` = the box was already gone and nothing was pressed — re-CLASSIFY. Surprising, destructive, or off-task → deny (`key escape`) and redirect, or ESCALATE |
   | Claude Code's permission prompt: the command line, then `Do you want to proceed?` with numbered options — `1.` Yes (this once), `2.` Yes and don't ask again for a SCOPE (`git log *`, `allow reading from <dir>`), one option `switch to auto mode`, the last `No`; footer `Esc to cancel · Tab to amend` | classify the COMMAND LINE, not the box — the `classify` line `phase` printed, or `aterm drive classify '<command>'`. Read-only and the scope on option 2 is a read-only grant → option 2 (it removes a whole class of future prompts; see *Auto-approving reads*). A write or delete → judge that one command; option 1 at most, never the scope grant. **Never pick `switch to auto mode` unless the human said so.** Off-task or unsafe → `key escape` and redirect, or ESCALATE |
   | `limited` — its last turn ended on a usage or rate limit notice (`You've hit your session limit · resets 7:30pm`) | the worker cannot act until the limit resets or its model is switched; anything you send it fails. Decide per the human's policy — wait for `reset`, switch with `/model`, or ESCALATE — and never keep driving into the wall |
   | `question` — a prose question, composer idle | answer it with a `turn`, from your notes |
   | `idle` — nothing running, no box, no question | the turn is over → go REVIEW |
   | a non-TUI worker (build/script/REPL) still streaming output (`phase` running) | WAIT — for these, completion is a returned shell prompt or `phase=exited`, at which point go REVIEW via the **exit code + expected artifacts**, not a busy indicator |
   | a shell prompt where an *interactive agent* used to be | that agent exited — check why, relaunch + re-brief, or ESCALATE (a build returning to the prompt is normal completion, see the row above) |
   | anything you cannot confidently read | **NEVER type into an unknown screen** — ESCALATE |

   **The placeholder rule.** Text in the composer with the cursor at column 2 is Claude
   Code's DIM suggestion, not typed input and not a question — measured: `❯ m7 is
   reachable as ssh m7, go` was a suggestion, not a human. Typing would have pushed the
   cursor right of the text. Check `aterm ctl "@$SID" cursor` (`OK <row> 2 …`) or `cell
   <row> 2` (attrs `dim`). `phase` classifies from the rows above the composer, so a
   suggestion reads `idle`; `supervise` applies the rule itself before it backspaces a
   stray digit. Never act on a suggestion as if the worker had said it.

3. **REVIEW** (only once the turn is DONE) — run the ground truth, judge against
   it, then take exactly one action:
   - **next** — correct but unfinished → drive the next instruction.
   - **revise** — ground truth fails / work is wrong → drive the correction.
   - **answer** — it asked something → drive the answer.
   - **done** — ground truth PROVES completion for THIS objective (tests pass, exit 0, the expected artifact exists — whatever the objective's check actually is) → STOP.
   - **escalate** — unsafe, surprising, or you cannot tell → STOP for a human.

   Drive with ONE verified human turn, settled on the busy footer LEAVING the screen:
   ```sh
   aterm ctl "@$SID" turn settle=gone:esc.to.interrupt timeout=600000 '<single-line instruction>'
   ```
   Keep the instruction to **one line with balanced quotes and parens**. Claude
   Code's composer treats Enter as a newline (not submit) while a bracket is open,
   so a truncated or unbalanced instruction silently piles up in the input box and
   never runs. The `settle=` pattern is **ONE whitespace-free token**: the client
   joins argv with single spaces and the server re-splits the line on whitespace,
   so shell quotes do not survive the wire — `settle=gone:'esc to interrupt'` arms
   the regex `esc` and TYPES `to interrupt timeout=600000 <instruction>` into the
   worker as the message, with the timeout left at its 240 s default. Write
   `esc.to.interrupt`; `.` is regex, not the shell's business. Neither idle-settle
   nor prompt-matching is a completion signal for an interactive worker. Measured:
   `turn idle=3000` settled after 4.5 s while Claude Code was still thinking — its
   screen sits static for 3+ s mid-turn. And the composer glyph (Claude Code `❯`,
   Codex `»`) is on screen the *entire* time it thinks, so matching it returns
   mid-turn too. The signal that holds is the busy footer DISAPPEARING — `esc to
   interrupt` in Claude Code; substitute the worker's own footer text.
   `settle=gone:` first waits (bounded by `submit_window`, default 2000 ms) for the
   footer to APPEAR after the submit, then for it to LEAVE; a footer never seen in
   that window silently degrades the turn to idle-settle, so WAIT still confirms
   with `await gone`. On a build whose `turn` has no `settle=gone:`, drive with
   `turn idle=2500 …` and treat its return as *submitted*, not *finished*: WAIT
   decides completion.

4. **WAIT** — block on the one session most likely to move next. Never busy-poll;
   never park more than one waiter (each holds a control lane).
   ```sh
   aterm drive await-turn "@$SID" --timeout 600000   # block until the phase is not busy, print it like `phase`; exit 124 = still busy
   ```
   `--timeout` is milliseconds (default: the global `--timeout`, 180000). Inside it is
   `await gone esc.to.interrupt` as the first wait where the host has it, else `await idle
   2000` → read → `await seq <seq>` — never a sleep — in 20 s steps against the deadline,
   returning the moment the screen is `prompt`, `limited`, `idle` or `question`. Exit 124 is an
   answer — still busy — not an error. Manual form:
   ```sh
   aterm ctl "@$SID" await gone esc.to.interrupt timeout=600000   # OK gone <seq> = turn finished | OK timeout = still busy
   ```
   The regex is ONE token (`esc.to.interrupt`): a quoted `'esc to interrupt'` is
   re-split on the wire, arms `esc`, and drops the rest (`aterm ctl` prints a stderr
   `note:` first, and still sends it). `await gone` is
   level-triggered: if the footer is already absent it returns at once, so arm it
   only after the footer is up (right after your `turn`, or after a read showed
   it). `OK timeout` (exit 124) is an answer — still busy — not an error. A build
   with no `gone` answers `ERR usage: await <idle <ms>|seq [<n>]|match <re> …|block|…>`
   — a grammar with no `gone` in it; fall back to this loop until the footer is
   absent:
   ```sh
   aterm ctl "@$SID" await idle 4000 timeout=300000       # settled? exit 124 = still busy = an answer
   aterm ctl "@$SID" text | grep -c 'esc to interrupt'    # read the footer: nonzero = NOT done, idle or not
   aterm ctl "@$SID" await seq timeout=15000              # footer still up + idle → wait for fresh output, re-read
   ```
   Do not skip the middle read: idle alone is exactly the false positive measured
   above.

### Run the loop unattended: `aterm drive supervise`

```sh
aterm drive supervise "@$SID" --auto-reads --max-s 1800 --notes "$NOTES"
```

WAIT and the read-only half of CLASSIFY in one process: it runs `await-turn`; with
`--auto-reads`, a Bash prompt whose command classifies read-only is approved with option 1
— pressed GUARDED, `key if=Do.you.want.to.proceed 1` where the host has it, else read →
press → re-read and backspace a digit that landed in the composer — one line `approved
read-only: <command>` goes to `--notes`, and the loop continues. **Anything else stops it
with exit 0 — your review point**: a write prompt, an Edit/Write/workflow box, a question,
a limit notice, an idle composer, or a read-only command coming back after it was approved twice (`handed
to the manager (<why>): <command>` in the notes). It prints the `phase` lines, a `--` line,
then the prompt box verbatim or the last 28 non-blank rows. `TIMEOUT` / exit 124, then the
last read's lines, once `--max-s` (seconds, default 1800) is spent — nothing is pressed
after it. `--allow-python GLOB`
(repeatable) widens the python rule; the default globs are `scripts/*standing*.py`,
`scripts/*report*.py`, `scripts/*score*.py`. Without `--auto-reads` every prompt is yours.
It presses option 1 only — never the scope grant, never auto mode — and never types text:
REVIEW, drive the next `turn`, call it again. Its `--max-s` bounds ONE call's wall clock;
the drive budget below is still yours to count.

### Let the harness wake you: `aterm drive watch`

```sh
aterm drive watch "@$SID" --auto-reads --notes "$NOTES"   # under your harness's background monitor
```

When your harness can run a long-lived command in the background and wake you once per
line it prints (Claude Code's Monitor tool, a supervisor process), run `watch` there
instead of relaunching `supervise` after every review: you are woken only at decision
points, and between them the worker is still being watched. It is `supervise`'s loop,
except that a review point prints one line and the loop keeps going.

- **Each `EVENT <phase> seq=<n> <summary>` line is a review point** — `prompt` with
  `kind=… classify=… command=…`, `question`/`idle` with the last row the worker said,
  `limited` with `message=… reset=…`. REVIEW it exactly as above, then act with
  `aterm ctl "@$SID" turn …` or `aterm ctl "@$SID" key if=Do.you.want.to.proceed 1` (or
  `key escape`). `watch` picks your action up by itself: it waits for the screen to move
  past the point it reported before it looks again. A point that looks like the last one
  it reported — the same summary and status row, the same last transcript rows up to it,
  the same box — is not reported again unless, in between, it saw the worker busy or
  approved a read; a new box, a new reply, your own `turn` or a retry's new notice
  changes those rows, so a retried command or a second edit to one file is a new EVENT
  however quick the worker was. An identical screen that the worker somehow reached
  again is the one case it stays quiet about: if a line you expected does not come,
  `aterm drive phase` it.
- **`APPROVED seq=<n> <command>` lines are the audit trail** of what it waved through —
  the same reads `supervise --auto-reads` approves, noted in `--notes` too.
- **A `limited` EVENT means the worker cannot act** until its limit resets (`reset=` says
  when, if the notice did) or its model is switched. Decide per the human's policy.
- **A worker without Claude Code's composer** (a build, a REPL) gets an EVENT each time
  its output pauses for 2 s and its last rows changed — for a build you are only waiting
  on, `aterm drive await-turn` or `aterm ctl "@$SID" await block` is the better tool.
- `TIMEOUT` (exit 124, once `--max-s` is spent, default 1800 — nothing is pressed or
  reported after the deadline) or `EXIT <reason>` (exit 1) is its last line: `EXIT
  session gone (…)` when the session ended, otherwise a request or the notes file failed,
  or — before the loop ran — one of its flags or the host (the error is on stderr too).
  Relaunch it or escalate. Like `supervise` it presses option 1 only and never types
  text.

### Auto-approving reads

A permission prompt may be pre-approved WITHOUT a per-prompt judgment only when
the command line is read-only, and **the classifier is the source of truth**:

```sh
aterm drive classify 'git status --short && git pull | tail'   # not-read-only git pull   (exit 1)
aterm drive classify 'git log --oneline -5'                    # read-only                (exit 0)
```

`read-only` (exit 0) or `not-read-only <reason>` (exit 1) — pure, no host needed; the
same function `phase` prints as its `classify` line and `supervise --auto-reads` acts
on. Its rules, each paid for by a misclassification in a real session: quoted strings
are dropped BEFORE the danger scan (a `>` inside a `git --format` string is not a
redirect) and the worker's `perl -e 'alarm N; exec @ARGV'` wrapper is stripped; a danger
token ANYWHERE fails the line — `rm mv cp tee …`, a git write (`push pull commit reset
checkout …`, `stash`/`worktree` other than `list`), a redirect to anything but
`/dev/null` or an fd (`2>&1`, `>&2`), `sed -i`, `python3 -c`, `bash|sh|zsh -c`, `perl -e`, a `python3 - <<`
heredoc, `find -delete`, `find -exec` unless it feeds `du/ls/cat/head/wc/stat/file/grep`,
`xargs rm|mv|cp`; then every segment's head (split on `;`, `&&`, `||`, `|`, `$( … )`)
must be a known read-only program — `git` only with a read-only subcommand, `python3`
only a script on the `--allow-python` globs, an unknown program is not a read. A tie
breaks toward `not-read-only`. `git log && rm -rf .` opens read-only and is refused; so
is `cat $(rm -rf x; echo f)`.

**You still judge everything that is not read-only, every time** — `rm`, `mv`, any
redirect, a git write, builds, package managers, any interpreter with inline code: a
write no matter what it prints. And you judge what the classifier does not see: whether
the read is on-task, whether the scope on option 2 is itself read-only (`git log *`,
`allow reading from <dir>` → option 2; otherwise option 1), and a prompt that is not a
Bash prompt at all (Edit/Write/workflow). `supervise` takes option 1 only. Without
`aterm drive classify`, apply the same rules by hand: split every segment, and one
non-read segment makes the whole line yours.

Log every auto-approval in the notes file — the command line, the option chosen,
and why it qualified — so the human can audit what you waved through; `supervise
--notes` writes `approved read-only: <command>` for you.

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

- The `drive-aterm` skill, `aterm ctl --help` and `aterm drive --help` (*SUPERVISING A
  WORKER*) — the read/drive verbs and subcommands this composes.
- `aterm help`, and (in the aterm source) `docs/OPERATOR.md` — the fuller operator brief.
