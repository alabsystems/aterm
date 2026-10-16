---
name: supervise-agent
description: Supervise a worker (another Claude Code, Codex, a REPL or a long build) in an aterm session — dispatch work, review each turn against ground truth (tests, diff, exit codes, not the screen), handle its prompts, escalate and resume. Use when asked to manage, babysit or oversee another agent or a long-running session.
---

<!-- aterm skill v2 — MANAGED FILE, written by `aterm agents install`.
     Edits are overwritten on update. To keep your own version, remove this
     marker line and aterm will leave the file alone (reported as `foreign`). -->

# Supervising a worker agent

You are the reviewer of a worker living in another aterm session. The mechanics are in
the drive-aterm skill, `aterm ctl help <verb>` and `aterm drive --help` (SUPERVISING A
WORKER); this file is the loop and the judgment.

## Review against ground truth, not the screen

A worker saying "all tests pass" or "done" is a claim. Run the check yourself (the tests,
the build, `git diff`, the exit code) and decide from that.

## The window already supervises every agent session

Every Claude Code, Codex and agy session in an aterm window is supervised by the window's host
(`status supervisor=aterm-harness@<pid>`), fully automatic by default: it answers boxes and
questions, continues ended jobs (never a person's own question or message outside a job it is
continuing) and escalates what it cannot settle. `aterm help harness` says what it does, `aterm harness ledger @<sid>` what it did.

A worker's report that ends with `ATERM-TASK-DONE` on a line by itself stops those
continuations. The token means every part of the job the worker can still do is done and
verified (built, tested, measured where numbers were asked for, committed and landed), and
what is left needs a person (a credential, a decision, physical access) or cannot be done on
that host, each item named with its blocker; a slow check is not blocked. A report that ends
with it but still names open work (`Still open`, `Left:`, `not run`, `skipped`, …) without a
blocker gets one challenge from the host before the job stops.

One supervisor per session: a `drive watch` you start where the host holds the claim
only watches. To drive a worker the host supervises, hold its lease, `aterm ctl "@$SID"
lease acquire holder=<you> ttl=600000` (renew it; `lease release holder=<you>` when
done). While `status` shows `hand=lease:<you>` the host presses and types nothing there.

## Set up

- Attach: `aterm ctl ls` and take the worker's sid.
- Or spawn your own in a private instance (drive-aterm's, but keep your real `HOME` so the
  worker's login persists) with `spawn identity=worker`, then `turn 'cd <workdir> && exec
  claude'`. The identity gives the worker its own config dir and login, so one account's
  usage limit cannot stop you both; its first sign-in is the human's step.
- Unattended work runs in a disposable clone or `git worktree`, not the user's live tree;
  a branch is not a sandbox. On macOS, `aterm --headless --sandbox` denies the network,
  the secrets and every write outside the temp roots.
- Keep a notes file (objective, worker sid and socket, the ground-truth command, budget
  left, one line per action) and give it to `--notes`; give `watch`/`supervise` a
  `--journal` file so `aterm drive ledger` can replay the run. Keep both under the
  project or `~/`, not the Claude Code scratchpad (`/private/tmp/claude-<uid>/…`): macOS's
  daily `com.apple.tmp_cleaner` deletes files there untouched for about three days.

## The loop

With the fabric on, mail is the channel and the screen is the safety net:

```sh
aterm drive watch "@$SID" --mail --no-continue --no-answer --journal "$JOURNAL"   # under one background monitor
aterm drive task "@$SID" 'run the suite and report'                               # instructions by mail + a one-line nudge
aterm ctl @self inbox get <id>                                                    # the report an EVENT turn line names
```

`EVENT turn … report=<id>` is a worker turn with its report; `EVENT idle-no-report` means
none came, so read `aterm drive report "@$SID" --final`. Either is the worker's account,
not proof. `--no-continue --no-answer` brings the turns and questions to you, and
`--approve safe` limits which boxes it answers.

The same loop by hand:

1. SWEEP: `aterm ctl "@$SID" status`. An unchanged `revision` means nothing new.
2. CLASSIFY: `aterm drive phase "@$SID"`; for a prompt the parsed box follows.
3. REVIEW once the turn is done: run the ground truth, then take one action: next,
   revise, answer, done (the ground truth proves the objective and nothing the worker
   could still do is left), or escalate.
4. WAIT: `aterm drive await-turn "@$SID" --timeout 600000`. Park one waiter; no sleep
   loops.

Without the fabric, drive with one verified turn on one line with balanced quotes and
brackets (an open bracket turns Claude Code's Enter into a newline): `aterm ctl "@$SID"
turn settle=gone:esc.to.interrupt timeout=600000 '<instruction>'`.

## Prompts

- `kind question` is an AskUserQuestion dialog, not a permission box, even one reading
  `Do you want to proceed?`. Answer it from your notes or escalate; pressing it as an
  approval answers the human's question.
- A permission box: judge the command line, not the box (`classify` in `phase`, or
  `aterm drive classify '<command>'`). Read-only and on-task: approve guarded on a row the
  box shows, `aterm ctl "@$SID" key if=Do.you.want.to.proceed 1`. A write: judge that one
  command, option 1 at most; the "don't ask again" scope only when the scope is itself
  read-only. "Switch to auto mode" only when the human said so. Off-task or destructive:
  `key escape` and redirect, or escalate.
- Dim text in the composer with the cursor at column 2 is Claude Code's suggestion, not
  the worker's words.
- `limited`: the worker cannot act until the reset or a model switch, and work sent to it
  hits the same wall. `watch` waits it out and continues (`EXTEND`, `CONTINUED`); the last
  directive is yours to resend.

## Bounds

- Budget: choose a drive budget for the run; when it is spent, escalate with where the
  work stands.
- No progress: two or three drives with no `revision` advance means the worker is wedged.
  Change the input (read the screen, `signal int`, re-brief) or escalate.
- Pass on the owner's direction, never your own caution. A worker builds a driver's
  instruction into its own review gates, so a gate the owner never set ("wait for the
  owner's sign-off") costs review work every round until you withdraw it. When a turn ends
  asking for a person's decision and the owner set no gate, tell the worker to decide.
- Escalate a decision only a human can make with `aterm ctl "@$SID" meta set attention
  '<why>'` and carry on with what you can decide.
- A person at the keyboard (`human_ms=` under 120000) or another lease holder owns the
  input.
- Read an unknown screen (`text`, `image`) before any key: a blind key lands wherever
  that program's focus is.

## Report when you stop

The outcome (done, escalated, budget spent), the ground truth you verified, how many
turns it took, and what a human must decide. Leave the worker in place; tear down only a
session you spawned for a one-off.
