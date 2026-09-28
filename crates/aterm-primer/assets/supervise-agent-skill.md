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

## The window already supervises every agent session

Since 2026-09-24 every Claude Code and Codex session in an aterm window is supervised by
default by the window's own host (one loop per session, the engine `aterm drive watch`
runs, each program read by its own reader — Codex's boxes by what their options do, its
turns continued in its own composer; nothing is installed into the agent), FULLY
AUTOMATIC unless aterm.toml's
`[harness]` says otherwise — nobody is at the keyboard, so nothing waits on a person. It
ledgers every act in `<aterm state>/drive/<sid>.jsonl` (`aterm harness ledger @<sid>`),
with a journal beside it (`<sid>.journal.jsonl`):

- **It answers every box** (`approve = "all"`): a permission box its one-shot allow —
  Bash under any header (`(unsandboxed)`, `(runs on …)`, `· from` a workflow, a
  subagent, a plugin) whatever vendor note it carries, the rm circuit breaker of every
  kind included, and Edit/Write/Read, a workflow, Fetch, a network request, Chrome, a
  skill, Monitor, an MCP tool's box, and a box taller than the pane by the options it
  shows; a model's or extra usage's consent too (never credits the account turned off,
  turned back on); never
  "don't ask again", a session grant or a mode switch, and never a purchase (buy, add
  funds, upgrade, a spend limit raised: the option that waits instead, else the box's
  `No`) — the folder-trust dialog (its form listing the folder's pre-approved
  permissions too: the ledger names them), plan mode's approval (its yes that grants no
  standing mode, `Yes, manually approve edits`), the model-refusal pause (its `Switch
  to <model>`, while `model_fallback` is set). A setup dialog, a proposed
  goal or a Computer Use grant is declined, its yes settling something for good. A box
  a rule below proves keeps that rule in the ledger; every other press carries
  `unproven: <why no rule proved it>`. A press that did not land is pressed again on a
  pause that doubles to a minute, the session badged only once it has missed for 2
  minutes. The `${VAR:?}` guard on an `rm` operand is
  therefore the worker's own: the box no longer stops it. `approve = "safe"` answers only what a rule proves safe (a read-only
  Bash box, a Read box under a trust root or a system/scratch root and outside the secrets
  list, the rm breaker's possibly-empty-variable kind in a bypass session under a scratch
  root, the trust dialog for the session's own folder); `"none"` nothing. It dismisses the session
  survey (`0`, never a rating).
- **It answers a question dialog** (AskUserQuestion) whatever `approve` says — a question
  is no permission — with its recommended option (`answer-recommended@v1`, `[harness]
  answer_questions`, on by default; Settings: search "question"): the `(Recommended)`
  option, option 1 when none is marked, every recommended option of a multi-select then
  its button, the review tab's `Submit answers` — by Enter on the focused row, the focus
  moved there one row a key, every key fenced, and only once nobody has keyed the
  session for `human_grace_s` (`text --json`'s `"human_ms"`). Never a digit, never `Type
  something.`, `Chat about this` or Cancel; a question a person has begun answering (text
  in its free-text row, a check it did not make), or one it cannot read whole, is
  escalated. Codex's question gets its recommended answer, else its first, by its digit.
  One session's own word outranks the switch: `aterm ctl "@$SID" meta set questions ask`
  (its dialogs come to you — how you take a worker's questions; answer one with `aterm
  drive answer "@$SID" <n|label>`, its token from `aterm drive phase`'s `box` line as
  `--box`) or `recommended`; `meta unset questions` hands it back, and a dialog already
  waiting is answered within 2 s. A dialog its own keys answered is told once: `CHOSE …
  policy=recommended <question → answer>`, `chose` on the band.
- **It types:** `keep going` (or accepts the worker's own continuation suggestion) when
  a turn ends — a worker whose turns keep ending short (under 2 minutes of work), or
  that answers a continuation by saying it is done (`nothing left to do`, `what would you
  like me to work on?`), on a back-off that doubles, 2 minutes to an hour, never
  escalated as "done"; nothing into a session nobody has asked anything yet (its launch
  screen, no turn); `answer_text`
  (decide yourself, prefer reversible steps, keep going) when the worker asks a question
  or for a decision, and only "take the option that deletes, overwrites and force-pushes
  nothing" when it names an irreversible act; after an API error, a try that quotes
  Claude Code's own error line instead of `keep going` (never an ask): a network never
  reached, or a certificate or proxy refused, 1, 2, 5, then every 5 min (in a window,
  within about a minute of the API measured reachable again, and nothing for up to 15
  min while it is measured down), a reply cut off at once, the server's own failure or
  an overload for ever (1, 5, 15, 30, then every 60 min); a continuation after a usage
  or spend limit's reset (never buying anything); on a model-bucket limit a relaunch on the
  fallback model (`--model opus`, session-only — never `/model`, which saves the default
  for every new session) and back at its reset (a bucket asking consent to go on on
  credits is continued first); `/compact` on a full context; `/login` on a lost login.
  A continuation the worker never takes is acted again on the back-off.
- **It brings the agent back:** a Claude Code that crashed (its `sessions/<pid>.json`
  left behind) is relaunched in its tab on its own conversation and told to carry on at
  its first idle point; a newer installed build is taken at an idle point (the notice,
  the agent's READY, the restart, the carry-on — `upgrade = false` takes it away). A
  graceful exit — `/exit` from anyone, ctrl-d, a `kill` — is someone's decision and is
  left alone, as is a launch's own end (`-p`). A Codex exit is said on its tab: its
  relaunch is not built yet.
- **A person wins:** within `human_grace_s` (120 s) of a person's keystroke, click,
  scroll or IME composition in the session through a window (`status human_ms=`, `EVENT <local> human`;
  control-socket writes, yours included, are not a person) — or of a draft in the
  composer last changing — it types and presses nothing, the survey's `0` included; a
  turn a person stopped with Esc is held that long, then continued. In a session with a
  task, a draft left standing past the grace is sent as the next message; in one nobody
  has asked anything it is left where it is (and a live upgrade waits on it).
- **It escalates only what the owner limited, and what nothing can answer** — a box
  beyond `approve`, a question under `answer_questions = false`, a written
  `continue_per_hour` spent, a lost login's browser step: the session's keyed
  `attention` (`owner=supervisor`) reads `claude <kind>: <command, path or question>
  (<why>)`, which is one menu-bar row and one native notification. The host posts no
  mail; a `drive watch --mail` of yours posts one `kind=ask` per point, only while
  `fabric=connected`.
- **See it:** `aterm ctl ls` / `status` show `supervisor=aterm-harness@<pid>` (the live
  claim), `program=`, the server's `agent=` verdict (`busy|prompt|question|idle|
  wall:<kind>|…`) and `human_ms=`; `meta` shows the attention.
- **Turn it off:** the Harness switch in Settings (search "harness"), or `enabled =
  false` under `[harness]` in aterm.toml (each policy has its own key: `aterm help
  harness`; every key can only take power away). **Answer only the proven boxes:**
  "Approve permission boxes" set to `safe` in Settings (search "approve"), or `approve =
  "safe"`. A headless instance supervises too unless `headless = false`.

**One supervisor per session.** A `drive watch` you start on a session the host already
holds WATCHES ONLY (`WATCHING … another supervisor holds this session`): its EVENT lines
still come, it presses and types nothing. One you start first holds the claim, and the
host parks behind it until your loop ends. **To drive a worker the host supervises, put
your hand on it:** `aterm ctl "@$SID" lease acquire holder=<you> ttl=600000` (renew it
while you drive; `lease release holder=<you>` when you are done), or dispatch through a
`turn` that names you. While `status` shows `hand=lease:<you>` or `hand=turn:<id>:<you>`
the host presses and types nothing in that session — it holds as it does for a person at
the keyboard — so it never continues the turns you dispatch or answers the questions
meant for you.

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
  XDG_RUNTIME_DIR=$RUN aterm --headless --control-sock "$SOCK" --columns 120 --lines 40 \
    >"$RUN/gui.log" 2>&1 &
  for _ in $(seq 1 100); do [ -S "$SOCK" ] && break; sleep 0.1; done
  SID=$(aterm ctl --sock "$SOCK" spawn identity=worker | cut -d' ' -f2)  # its OWN agent login
  aterm ctl --sock "$SOCK" "@$SID" turn 'cd <workdir> && exec claude'   # launch the worker
  aterm ctl --sock "$SOCK" "@$SID" status                   # expect detail=claude and identity=worker
  ```
  `identity=worker` gives the worker its own agent identity: `CLAUDE_CONFIG_DIR`/`CODEX_HOME`
  point into `<state>/identities/worker/` (created once, 0700, primed with these skills),
  never at your `$HOME`, so the worker never shares an account with you — **a shared
  account's usage limit stops the manager and the worker together**, and a worker on its own
  login stops alone. Expect the first launch under a fresh identity to ask for a sign-in —
  the human's keystrokes, in that window, never yours (a human-run measurement the docs
  still record as a TODO); aterm never reads the login. `aterm ctl --sock "$SOCK"
  identities` lists the identities and which live sessions carry each; `identities forget
  worker confirm=worker` removes the directory once no session uses it (sign out in the
  agent first — a macOS keychain login is not aterm's to remove). On an older build without
  `identity=`, the spawn is `ERR usage`; fall back to the instance's first session
  (`aterm ctl --sock "$SOCK" ls | awk 'NR==1{print $3}'`) and know the worker then shares
  your login.
  On older builds that compound line reads `detail=cd` (the first word, not the segment
  that runs), so when `detail=` is a shell builtin confirm the worker is up with `text`.
  The worker is anything interactive or long-running — `claude` here, but
  `codex`/`gemini`, a REPL, or `make build` work the same way; substitute the
  launch command. Use a **plain** session — NOT `spawn connected=controller`, which injects
  `ATERM_OBSERVE_SESSION_ID`, a marker the worker can read. Launched plainly, the
  worker sees only the generic in-aterm environment (`CLAUDE*`/`ANTHROPIC_*` are
  stripped from every child; under `identity=` only the agents' own home variables are
  set back, pointed into the identity), so it behaves exactly as if a human started it.

**Sandbox anything you run UNATTENDED.** Use a *disposable checkout* — a separate
clone or a `git worktree` in a throwaway path — never the user's live tree. A
branch is **not** a sandbox: it protects committed history, not the filesystem, so
a stray write or `rm` still hits your real files. Containment mode (`aterm
--headless --sandbox`) closes that hole on macOS by taking writes away: the kernel
Seatbelt profile denies ALL network, denies read+write of the credential set
(`.ssh`, `.aws`, `.gnupg`, `.config/gh`, `.netrc`, …) and the private-user-data set
(Documents, Downloads, media, the Mail/Messages/keychain/cookies/browser stores),
and confines WRITES to the temp roots (`$TMPDIR`, `/private/tmp`), `/dev` and the
shell's history file — **the worker's working directory and `$HOME` are read-only
to it**, so a contained worker that must edit or build works in a copy under
`$TMPDIR`. Off macOS there is no OS sandbox, and `--sandbox` refuses to start
(naming the gap) rather than run unconfined. So a worker that must write your
project gets the disposable checkout; `--sandbox` is for a run that may touch
neither the network, your secrets nor your files (an agent CLI that must reach its
model cannot work there). Your budget and breaker are a discipline, not a sandbox.

Keep a durable **notes file** with what a fresh copy of you needs to resume:
objective, worker sid + socket, the ground-truth command, budget remaining, and
one line per action taken. That file — not your context window — is your memory
across restarts. Point `aterm drive supervise --notes` (or `watch --notes`) at the
same file — but it writes far less than the whole story. It appends one UTC-stamped
line per BOX decision, whatever the box's kind — approved (with its `unproven:` reason
where no rule proved it), handed to the manager (with why), press skipped, press
unanswered — and one each time the loop backspaces a stray digit after a lost
connection; one when it dismisses or hands over a survey; under `--report`, a `report
failed: …` line when the end-of-turn
report could not be posted. A `question` phase, an idle composer, a limit notice: no
line. Everything else in the notes is yours to write.

**Keep a `--journal` too.** `aterm drive watch --journal "$JOURNAL"` (or
`supervise --journal`) appends one JSON object per line the loop prints — every
`EVENT`, `APPROVED`, `DISMISSED`, `RECONNECT`, `TIMEOUT` and `EXIT`, with the
Unix time it was printed, the phase, the seq and the line itself — so the loop
can be replayed after the fact. The notes are what YOU decided; the journal is
what the LOOP decided, and without it nothing can say when the worker stopped or
how long you took to answer (measured 2026-09-14: the `--notes` file held none
of the watcher's own decisions).

## The loop

**Mail is your channel; the screen is the safety net.** With the fabric on (`aterm
fabric`) the whole loop is four commands. Nothing wakes the worker for mail: `task` types
the one-line nudge when its screen is idle, as you would, and the worker may run the
verbs itself (`$ME` is your own sid, `$ATERM_PARENT_SESSION_ID`):

```sh
aterm drive watch "@$SID" --mail --no-continue --no-answer --journal "$JOURNAL"   # under ONE Monitor: one line per worker turn
aterm drive task "@$SID" 'run the suite and report'                  # assign: body by mail + a one-line nudge
aterm ctl @self inbox get <id>                                       # read the report the EVENT turn line names
aterm drive ledger "@$SID" --journal "$JOURNAL"                      # replay the run
```

`watch --mail` parks ONE `await inbox` on YOUR session beside the worker's screen (a
thread with a client of its own; the worker's socket sees nothing more but one screen
read per 20 s step while an idle point is held; no polling) and
prints `MAIL id=<n> off=<o> from=<sid> kind=<k> len=<n> [re=<o>]` per delivery as it
lands. The worker's end-of-turn `report` — one it posts itself, if it does — is folded into the
idle point of the same turn: ONE line, `EVENT turn seq=<n> report=<id> rows=<n>
<summary>`, per worker turn; you read the body with `inbox get <id>` (2 KB) instead of a
`report` of the screen (measured 2026-09-14: 689 rows for the same turn). An idle with no
report inside `--idle-grace` (default 5 s) prints `EVENT idle-no-report …` — THEN fall back to
`aterm drive report "@$SID" --final`, and only then. A report that arrives later still
prints `MAIL`; read its inbox row, but do not assign it to the next turn. `task` types
the one-line `Inbox: task @<off>` as a turn, only when the worker is idle (a busy
one gets the mail alone: re-nudge at its next EVENT).
`task --wait` parks for the `answer|report|ack` that carries `re=<off>` and prints it.
The steps below are the same loop by hand, and what `watch` does for you; an older
`aterm` answers `unknown command 'phase'`.

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
   After `prompt` come `kind <k>` (`bash`, `powershell`, `edit`, `write`, `overwrite`,
   `read`, `workflow`, `fetch`, `network`, `browser`, `skill`, `monitor`, `tool`, `trust`,
   `question`, `plan-enter`, `plan-exit`, `held-message`, `goal-proposal`, `computer-use`,
   `read-outside-setting`, `other` — `question` is never a permission box, see the table
   below), `command <line>`,
   `description <text>`, `classify read-only` or `classify not-read-only <reason>` (Bash
   only), one `option N <text>` per option, and `cancel esc` (every box, footed or
   footerless). `busy` is read only
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
   | `prompt` — a live box, named by its TITLE (`kind <k>` follows: `bash`, `edit`, `read`, `workflow`, `tool`, `trust`, `question`, …), with or without an `Esc to cancel` row (Fetch, the network request, Chrome and the plan, held-message and goal dialogs draw none) | **`kind question` is NOT a permission box**: it is an AskUserQuestion dialog for the human — even one reading `Do you want to proceed?` over `1. Yes / 2. No` — so never press it as an approval (a guard on `Do.you.want.to.proceed` matches it too). The window's host answers it with its recommended option unless `[harness] answer_questions = false`, and so does your own `drive watch` unless `--no-answer`. Otherwise answer it from your notes (pick its option, or `Type something.`) or ESCALATE. Any other kind: read WHAT it asks. Matches the task and is safe → approve GUARDED on a row the box shows: `aterm ctl "@$SID" key if=Do.you.want.to.proceed 1` (the option's number, or `enter`); `OK skipped seq=<n>` = NO VISIBLE ROW matched your regex and nothing was pressed — which is equally true when the box is GONE and when a *different* box is up (an Edit box asks `Do you want to make this edit to …?`, a workflow box `Run a dynamic workflow?`). Re-read the screen and classify what is actually there; re-pressing the same guard just skips again. Surprising, destructive, or off-task → deny (`key escape`) and redirect, or ESCALATE |
   | Claude Code's permission prompt: the command line, then `Do you want to proceed?` with numbered options — `1.` Yes (this once), `2.` Yes and don't ask again for a SCOPE (`git log *`, `allow reading from <dir>`), one option `switch to auto mode`, the last `No`; footer `Esc to cancel · Tab to amend` | classify the COMMAND LINE, not the box — the `classify` line `phase` printed, or `aterm drive classify '<command>'`. Read-only and the scope on option 2 is a read-only grant → option 2 (it removes a whole class of future prompts; see *Auto-approving reads*). A write or delete → judge that one command; option 1 at most, never the scope grant. **Never pick `switch to auto mode` unless the human said so.** Off-task or unsafe → `key escape` and redirect, or ESCALATE |
   | `limited` — its last turn ended on a usage or rate limit notice (`You've hit your session limit · resets 7:30pm`) | the worker cannot act until the limit resets or its model is switched; anything you send it fails. Decide per the human's policy — wait for `reset`, switch with `/model`, or ESCALATE — and never keep driving into the wall |
   | `question` — a prose question, composer idle | answer it with a `turn`, from your notes (the window's host answers it itself with `answer_text` unless `[harness] answer_questions = false`) |
   | `idle` — nothing running, no box, no question | the turn is over → go REVIEW |
   | a non-TUI worker (build/script/REPL) still streaming output (`drive phase` says `busy`; `ctl status` says `phase=running`) | WAIT — for these, completion is a returned shell prompt or `phase=exited`, at which point go REVIEW via the **exit code + expected artifacts**, not a busy indicator |
   | a shell prompt where an *interactive agent* used to be | that agent exited — under the window's host a crashed Claude Code is relaunched on its conversation within seconds (`aterm harness ledger` / the journal's `HOST` lines say so); a graceful exit is not: check why, relaunch + re-brief, or ESCALATE (a build returning to the prompt is normal completion, see the row above) |
   | anything you cannot confidently read | **NEVER type into an unknown screen** — ESCALATE |

   **No hook answers a box.** aterm installs nothing into the worker (decision "B",
   2026-09-22): a box is answered by the supervisor's policy above or by a person, and
   `phase` prints the box's `note` rows (the vendor's own reason) after `description`.
   A box the policy does not press is escalated: the worker's `attention` reads `claude
   <kind>: <command, path or question> (<why no rule approved it>)` (`ls` shows `meta=1`,
   `status` reads `level=attention`), and under `watch --mail` you get ONE kind=ask per
   review point while `fabric=connected` — the same box back after the worker worked is
   asked again; the badge clears when the box is gone — your `key`/`turn` answering it
   is what the loop waits on.

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

   **Read the worker's final message with `aterm drive report "@$SID" --final`, not
   the screen:** `--final` prints only its LAST message block — from its last `⏺`
   message row through the done row that ended the turn — with no tool rows and no
   `⎿` output; `--messages` prints every message block and your own `❯` rows the
   same way. Both keep the header (plus ` view=… kept=<n>`), so `complete=` still
   says whether anything was lost. Measured 2026-09-14: a manager read whole
   reports of 689 and 249 rows to find a final message of about 70 — read the view
   first and the whole report only when you need the tool output.

   **The whole report, when you do need it, is `aterm drive report "@$SID"`, not
   the screen:** Claude Code runs on the alternate screen, so what scrolled off its top
   is gone from `text` — measured, 7 of the 35 message blocks of one worker turn were
   still on the screen; a lost one reported a broken build. `report` joins the rows the
   host kept (`offscreen`) with the screen's, from your turn's `❯` row down, verbatim.
   `report complete=1 …` = nothing was lost; `complete=0 reason=…` = rows may be missing
   (the reason says why) — say so, and lean harder on the ground truth. Either way it is
   the worker's account of its work, not proof of it.

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
aterm drive supervise "@$SID" --approve safe --max-s 1800 --notes "$NOTES"
```

WAIT and the provable half of CLASSIFY in one process: it runs `await-turn`, and the
window's approval policy decides every box under your `[harness]` — capped by
`--approve safe` to the rules listed at the top (read-only Bash, a Read box outside the
secrets list, the rm breaker under a scratch root, the trust dialog for the session's own
folder); without the flag it answers every box, as the host does, and `--approve none`
answers nothing. The press is GUARDED by the row
that was judged, plus the read's generation (`key if=<that row> if-gen=<g> 1`), so a box
swapped between the read and the press matches nothing; one line `approved read-only:
<command>` (`approved (<rule>): …` for the others) goes to `--notes`, and the loop
continues. **Anything else stops it with exit 0 — your review point**: a box the policy
does not answer (under `--approve safe`, a write prompt or an Edit/Write/workflow box),
a question under `--no-answer`, a limit notice, an idle composer, or — under `--approve
safe` — a read coming back after it was approved twice (`handed to the manager (<why>):
<command>` in the notes). It prints the `phase` lines, a `--` line, then the prompt box
verbatim or the last 28 non-blank rows. `TIMEOUT` / exit 124, then the last read's lines, once `--max-s`
(seconds, default 1800; 0 is no budget) is spent — nothing is pressed after it.
`--allow-python GLOB` (repeatable) names the python scripts that count as reads; there
are none by default. With `--approve none` every prompt is yours.
It presses the one-shot option only — never the scope grant, never auto mode — and never
types text: REVIEW, drive the next `turn`, call it again. Its `--max-s` bounds ONE call's
wall clock; the drive budget below is still yours to count. It takes no supervisor claim,
so on a session the window's host already supervises (`status supervisor=` is not `-`)
run it with `--approve none`, or use `watch`, which defers to the holder.

### Let the harness wake you: `aterm drive watch`

```sh
aterm drive watch "@$SID" --mail --no-continue --no-answer --notes "$NOTES" --journal "$JOURNAL"   # under ONE background monitor
aterm drive watch "@$SID" --no-continue --no-answer --notes "$NOTES" --report                      # no fabric: the screen alone
```

When your harness can run a long-lived command in the background and wake you once per
line it prints (Claude Code's Monitor tool, a supervisor process), run `watch` there
instead of relaunching `supervise` after every review: you are woken only at decision
points, and between them the worker is still being watched. It is `supervise`'s loop,
except that a review point prints one line and the loop keeps going, and that it takes the
session's supervisor claim — behind the window's host it watches only (see the top).
Like the host it is fully automatic unless limited: to have the turns come to YOU for
review, pass `--no-continue --no-answer` (and `--approve safe` for the boxes); your
standing rules ride in every continuation it does type from `[harness] rules_file`.

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
- **`EVENT survey seq=<n> dismiss with: aterm ctl @<sid> key 'if=^●.How.is.Claude.doing'
  0` is Claude Code's session survey, and it is never yours to answer.** It is printed
  once when `● How is Claude doing this session?` over `1: Bad    2: Fine   3: Good   0:
  Dismiss` appears above the composer (`aterm drive phase` and `await-turn` end with
  `survey 0` while it is open), and again if it comes back after going. While it is open,
  a turn whose first character is 1, 2 or 3 is taken as the HUMAN's rating of the
  session: never rate it for them. Run the command it names — `0`, guarded on the
  survey's own row, quoted for zsh — before your next `turn`. Unless `[harness]
  dismiss_surveys = false`, the loop presses that `0` itself (never while a box is up)
  and prints `DISMISSED survey seq=<n>` once the survey has gone; if it is still there,
  you get the `EVENT survey` line, and it presses the `0` again on a growing back-off
  (badging the session from the second miss) until the survey goes. A monitor that filters the lines keeps that one too: `aterm drive watch
  … | grep --line-buffered -E '^(EVENT|APPROVED|DISMISSED|TIMEOUT|EXIT)'`.
- **`EVENT context seq=<n> <v>% until auto-compact` means the worker is about to compact;
  `EVENT compacted seq=<n>` means it has.** Claude Code parks `1% until auto-compact`
  right-aligned above its composer as the context runs low (`aterm drive phase` and
  `await-turn` end with `context <n>%` while it is up); a compaction replaces the worker's
  history with a summary, and your standing rules (run nothing heavy while a flag file
  exists, say) can silently drop out of it. `watch` says the first reading at or below
  `--context-warn` (default 10; `0` turns both lines off) once a descent, even mid-turn:
  have the worker bring its handoff notes up to date before it compacts. It says `EVENT
  compacted` once the indicator has gone from above the composer (or jumped back up 30
  points or more): re-send your standing rules in ONE turn. `supervise` says both on
  stderr, but only for what happens during its run — each run starts afresh, so a
  compaction between two runs (while you act on a result) prints nothing: after an `EVENT
  context`, run `aterm drive phase` before the next `supervise`; no `context <n>%` line
  (and no box up) means it compacted. Never type `/compact` or `/clear` into the worker
  for it without the human.
- **`--mail` makes the worker's own report the wake.** `MAIL id=<n> off=<o> from=<sid>
  kind=<k> len=<n> [re=<o>]` is one delivery to YOUR inbox (an `ask` from a human, an
  `answer` from a peer, the worker's `report`); `EVENT turn seq=<n> report=<id> rows=<n>
  <summary>` is one worker turn with its report folded in — `aterm ctl @self inbox get
  <id>` is the whole of what it said, and the REVIEW starts there (ground truth still
  applies: run the check yourself). `EVENT idle-no-report seq=<n> …` is a turn whose
  report never came within `--idle-grace` (default 5 s), or one a prompt or a new
  turn superseded while it was held (the screen is read once a 20 s step under the
  hold, so a prompt after an idle is seen within a step): read `aterm drive report
  "@$SID" --final` for THAT turn only. A report is the turn's when it came after the
  worker was read busy for the turn or after the point; one from before the last
  point handed over never is (a question turn's report does not become the next
  turn's); `--report-window` (default 120 s) bounds only a report from before the turn
  was seen to begin. A question, a prompt and a limit notice are never held for mail.
  `MAIL lane off: <why>` once means the lane could not run (no fabric, an older host)
  and the lines are as without the flag from then on. The process ends with its last
  line: the lane's parked wait is cut short.
- **`--report` counts what was said** when there is no mail: each idle, question and
  limited EVENT carries `complete=<0|1> rows=<n>` before its summary (`EVENT idle
  seq=812 complete=1 rows=57 …`) — the `report` of everything the worker said since
  your turn, which the one-line summary is not. Read it with `aterm drive report
  "@$SID"` before you REVIEW. With `--mail` it rides only on an `idle-no-report` line.
- **`APPROVED seq=<n> <command>` lines are the audit trail** of what it answered — the
  same boxes `supervise` answers under the same `--approve`, noted in `--notes` too; the
  journal adds `UNPROVEN seq=<n> rule=<id> unproven: <why>` for each press no rule
  proved, and `DECLINED seq=<n> rule=<id> <subject> => <text>` for each box refused
  with a reason the worker acts on (under `approve = "safe"`, a tall box whose note
  flags a removal) — an answer, never a point for you.
- **`--journal FILE` records every line it prints**, one JSON object per line
  (see *Set up the worker*), and `aterm drive ledger` replays it: pass it to
  every `watch` so the run can be read back later.
- **`RECONNECT <reason>` and `RECONNECTED after <ms> ms` are informational:** `watch`
  rides through an aterm self-update (the worker keeps its `@sid` on the new instance)
  and carries on from a fresh read — the EVENT it last printed comes once more if it is
  still the screen — so there is nothing to do unless it ends in `EXIT reconnect window
  lapsed: …` (the outage outlasted `--reconnect-s`, default 180 s from its first dropped
  request; a drop that keeps coming back is the same outage). Leave `--socket` unset, or
  name the `aterm.sock` alias: a per-instance `aterm-<pid>.sock` goes with the old
  instance, and every ride-out through it lapses.
- **A `limited` EVENT is the watcher's own decision point.** It does NOT exit: a
  `--max-s` that would end before the reset the notice names is stretched to 10 min past
  it (`EXTEND until=<UTC> reset=<text>`, once). Claude Code's auto-continue notice (`⚠
  Usage limit reached · continuing automatically at 1:50pm · esc to cancel`, or
  `continuing shortly`) names that time as its reset and stays on the screen while the
  worker resumes under it; nothing is typed there and nothing is raised (it is handled),
  unless the worker has not gone on 10 minutes past that time — then it is continued. Otherwise, unless `[harness]
  resume_limits = false`, it CONTINUES the worker, as the owner would: a minute past the
  reset, or as soon as the screen leaves the notice (a `/login`, a `/model` line), `keep
  going` (with `rules_file`'s standing rules), at an idle composer with nothing typed,
  guarded — `CONTINUED seq=<n> rule=usage-resume@v1 <text>`; the notice again after it
  waits 10 min, then 30 — and a limit it waits out raises no badge. Under
  `resume_limits = false` it has set the worker's `attention` to the notice (aterm's menu
  bar badges it), mailed you the same text as `kind=control` while `fabric=connected` and
  journaled `ESCALATED …` — once an episode — and it clears the badge when the worker
  works again (the first busy read after an auto-continue notice; else when it answers).
  It invents no work beyond `keep going`: the LAST DIRECTIVE IS YOURS TO RESEND — the
  journal and `aterm ctl "@$SID" history` show it. Never keep driving into the wall.
- **A worker without Claude Code's composer** (a build, a REPL) gets an EVENT each time
  its output pauses for 2 s and its last rows changed — for a build you are only waiting
  on, `aterm drive await-turn` or `aterm ctl "@$SID" await block` is the better tool.
- `TIMEOUT` (exit 124, once a `--max-s` you gave is spent — by default `watch` has none
  and runs as long as the session does; nothing is pressed or reported after the
  deadline) or `EXIT <reason>` (exit 1) is its last line: `EXIT
  session gone (…)` when the session ended, otherwise a request or the notes file failed,
  or — before the loop ran — one of its flags or the host (the error is on stderr too).
  Relaunch it or escalate. Like `supervise` it presses the one-shot option only (or the
  question dialog's answer), and the only text it types is a continuation, an answer
  (`answer_text`) or a wall's slash command — each taken away by `--no-continue`,
  `--no-answer` or its `[harness]` switch.

### A usage limit — the worker's, and your own

Measured 2026-09-15 16:51 → 2026-09-17 08:55: the manager's and the worker's Claude Code
drew on ONE account. Its weekly limit hit both at once — the worker stopped at an idle
composer under `You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)`,
the manager's workflow lost its last stage, the watcher printed `EVENT limited`, ran out its
`--max-s` and exited — and nobody could act for two days, while a finished A/B sat unread.
When the owner logged in under another account, a one-line question was answered in 23 s. Three
things keep that from happening again:

1. **Run the watcher as a plain process** under your harness's background monitor (Claude
   Code's Monitor tool), never as a stage of a workflow that a limit can end: `aterm drive
   watch "@$SID" --mail --journal "$JOURNAL"`. It lives
   through the reset on its own (above), and its `EXTEND` keeps it up past the reset, so the
   line that wakes you comes when you can act on it.
2. **Keep your standing rules in a file** (the flag files, the ground-truth command, what
   never to run) and name it `[harness] rules_file` in aterm.toml: every continuation — a
   reset's included — carries it (`keep going (standing rules: …)`, line breaks as spaces,
   cut at 400 characters), so the rules survive a limit the way you re-send them after
   `EVENT compacted`. Edit the file as the rules change; it is read when typed. What it
   does NOT resend is the work: the last unfinished directive is yours, from the journal or
   `history`. The window's host and your `watch` read the same key.
3. **Give the worker its own login** — spawn it under `identity=worker` (the recipe above),
   so its `CLAUDE_CONFIG_DIR` and its login live apart from yours and one account's limit
   never stops both of you. The sign-in itself is the owner's call, not yours: `/login` in
   the WORKER's window is a human's keystrokes (never type an account switch for them), and
   the watcher takes the screen leaving the notice as its cue to continue at once.

### Assign work by mail: `aterm drive task`

Use this, not `turn`, for real instructions: a plain `turn` (like `paste`) delivers its
text into the worker's composer as one bracketed paste, which reads to a model worker as
PASTED, not as its human's words, so its injection safeguard may make it ask instead of
act. A task goes in by mail; only a one-line inbox nudge is typed.

```sh
aterm drive task "@$SID" --deadline 1800 'run the suite; report the counts'   # body by mail; nudged when idle
aterm drive task "@$SID" --wait --deadline 600 'which branch is this?'                   # park for the answer
```

The body is posted `kind=task` from your own session (`@self`; `--inbox @sid` when you
are not inside aterm) and never goes through the worker's PTY: it prints `task @<off>
nudged=0|1` once the post LANDED (`off=` is what the worker's `inbox` row shows and its
answer carries as `re=`). It reads the worker's phase once and, ONLY when idle, types
the one-line `Inbox: task @<off>` as a turn (not waited on); a busy worker gets the mail
alone and reads it at its next look — nothing wakes it for mail. A post that did not land
is the error in the server's words:
`queued=1` means it
is in the outbox and WILL land when a bridge drains it (never re-post), `no-bridge=1`
that this instance has no bridge at all. `--wait` parks `await inbox` on your inbox for
the `answer|report|ack` with `re=<off>` (bounded by `--deadline`, else `--timeout`, and
re-armed past the host's 600 s clamp on one wait) and prints its MAIL line and body;
`TIMEOUT …` / exit 124 when none came.

### See how the loop ran: `aterm drive ledger`

```sh
aterm drive ledger "@$SID" --journal "$JOURNAL" --format text          # at your terminal
aterm drive ledger "@$SID" --journal "$JOURNAL" --format html --out ledger.html   # for the human
```

One timeline over four sources: the worker's turn ledger (`history`) — your turns,
when each started and how its `turn` verb settled — the size in rows of the reply
each turn drew (from one `offscreen` read, joined the way `report` joins it), the
watcher's `--journal` lines (`EVENT turn` and `idle-no-report` count as the worker
stopping, and every `MAIL` line is there), and this session's fabric mail with that worker
(`inbox --peek --meta` rows from it and its `timeline`'s posts to it; nothing is
listed or handled). SUMMARY gives the turns, the worker's busy time, YOUR response
latency (median and max from each `EVENT idle`/`question` to your next turn),
approvals, dismissals, context warnings and compactions, reconnects, mail and how
many reports were complete; TIMELINE is one row per item — time, lane (manager |
worker | watcher | fabric), what, duration/latency. `--format html --out PATH`
writes ONE self-contained page (nothing fetched) with the manager, watcher and
worker swimlanes and the fabric's mail on a fourth, turns
as bars and every line on hover: that is the thing to hand a human who asks what
you have been doing. It only reads, it says which source it could not read, and
`--since <time>` narrows it to the last stretch.

### Auto-approving reads

The window's own host answers every permission box by default (`approve`, top of this
page), so on a session it holds you review what was pressed — the ledger — not the box.
What follows is YOUR policy for a loop of your own with the proven rules (`supervise
--approve safe`, or a host set `approve = "safe"`): a permission prompt may be
pre-approved WITHOUT a per-prompt judgment only when the command line is read-only, and
**the classifier is the source of truth**:

```sh
aterm drive classify 'git status --short && git pull | tail'   # not-read-only git pull   (exit 1)
aterm drive classify 'git log --oneline -5'                    # read-only                (exit 0)
```

`read-only` (exit 0) or `not-read-only <reason>` (exit 1) — pure, no host needed; the
same function `phase` prints as its `classify` line and `supervise --approve safe` acts
on. Its rules, each paid for by a misclassification in a real session: the line is read
as the shell reads it first (a `#` comment ends at its newline, a here-document body is
data, and `$'…'` with an escape, a `${…}` holding a quote or a substitution, or an
unterminated quote refuses); quoted strings are then dropped (a `>` inside a `git
--format` string is not a redirect); every segment's HEAD (split on `;`, `&`, `&&`, `||`,
`|`, `$( … )`, a wrapper like `xargs`/`env` seen through) must be a known read-only
program — a program word elsewhere is data (`grep -rn open src` reads), a program named
by a path outside `/bin` and `/usr/bin` is unknown, `git` needs a read-only subcommand and
form, `python3` only a script on the `--allow-python` globs; and anywhere on the line a
redirect to a file, `sed -i`, `python3 -c`, `git -c`/`--output`/`--ext-diff`, `rg --pre`,
`printf -v`, an assignment to PATH, HOME, `GIT_*` or `LD_*`/`DYLD_*` refuses (`aterm drive
--help` has the whole list). A tie breaks toward `not-read-only`. `git log && rm -rf .`
opens read-only and is refused; so is `cat $(rm -rf x; echo f)`. The label is about the
command, not the repository: a git read honours repository config the worker can write
(`core.fsmonitor`, `diff.external`, a textconv driver). The in-window supervisor approves a
git read only after reading the effective config it would load and finding no such key —
with the worker's own git and environment (its `PATH`, `HOME`, `GIT_CONFIG_*`), and where
the worker's Bash tool stands too, which an earlier `cd` moves and the box does not show;
when you approve one by hand, `git config --list --show-origin` in that repository, run
in the worker's environment, is the check.

**You still judge everything that is not read-only, every time** — `rm`, `mv`, any
redirect, a git write, builds, package managers, any interpreter with inline code: a
write no matter what it prints. And you judge what the classifier does not see: whether
the read is on-task, whether the scope on option 2 is itself read-only (`git log *`,
`allow reading from <dir>` → option 2; otherwise option 1), and a prompt that is not a
Bash prompt at all (Edit/Write/workflow). `supervise` takes the one-shot option only. Without
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
