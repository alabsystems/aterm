// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-GUI supervisor host's per-session machines: the worker lifecycle
//! (at most ONE supervisor on a session at a time, a failing one restarted
//! for ever — badged past a budget, never turned off — a session another
//! supervisor holds left alone until a claim is released) and the relaunch of an agent that left
//! its tab (a person's exit is theirs; any other is relaunched on a growing
//! back-off or said to a person — never dropped).

use super::Model;

/// Two restored tabs in the host's one retry queue. A's first step waits;
/// B still gets its first step before A can retry, even when A's due time
/// passes during its own call. `Buggy=1` is the former nested retry loop,
/// which ran A's second step before B's first. Tier-1 drives the real host's
/// restored worker with these outcomes and validates each observed action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_restored_first_attempt_model() -> Model {
    crate::ty_model! {
        HarnessRestoredFirstAttempt {
            const Buggy = 0;
            var a_first = 0;
            var b_first = 0;
            var a_retry = 0;

            action FirstA when (a_first == 0) {
                a_first = 1;
            }
            action FirstB when (a_first == 1 && b_first == 0) {
                b_first = 1;
            }
            action RetryA when (a_first == 1 && a_retry == 0 && (b_first == 1 || Buggy == 1)) {
                a_retry = 1;
            }

            invariant FirstAttemptsBeforeRetry: a_retry == 0 || b_first == 1;
        }
    }
}

/// `aterm-gui`'s `harness_host` runs one worker per Claude session. For ONE
/// session: `wanted` is the session's published program being `claude` (and
/// the policy active); `cur` a worker running under the current policy;
/// `old` workers asked to stop (the program left, the policy changed) whose
/// wait has not ended yet; `faults` the SESSION's failed runs in the restart
/// window — across its workers, kept at most `Budget + 1`; `faulted` the
/// session badged while its worker waits to restart a supervisor that keeps
/// failing past the budget (the worker still the session's: `cur` stays);
/// `held` another supervisor's claim refused ours.
///
/// * `Arrive`/`Leave` — the program becomes / stops being claude. Leaving asks
///   the worker to stop and forgets `held` (the host keeps it only for a
///   session it still wants) — but NOT `faults`: a program flap gives a
///   crash-looping engine no fresh budget. The badge goes with the worker
///   that raised it, at its `Exit`.
/// * `Start` — the host starts a worker for a wanted session with none of its
///   own, not held, and — the property — NO OLD WORKER STILL RUNNING: the
///   host reaps a stopped worker before it starts the next one. It inherits
///   the session's `faults`.
/// * `Exit` — an old worker's wait ends and it is reaped, its badge cleared
///   on its way out (no new worker starts before it has gone).
/// * `Fail` — the current worker's run failed or panicked: restarted in
///   place, ALWAYS — within `Budget` on the short pauses, past it badged
///   while it waits for the longer one. (Until the philosophy review of
///   2026-09-25 the next failure past the budget turned the supervisor
///   OFF until someone edited `[harness]`: a give-up that waited on a
///   person.)
/// * `Restart` — the pause ends: the next run starts, and the badge goes.
/// * `Age` — the oldest failure leaves the hour's window.
/// * `Hold` — the claim was refused: the worker ends, its badge with it;
///   `Release` — a claim was released or lapsed somewhere, so a held
///   session may be tried again.
/// * `Reload` — the `[harness]` policy changed: the worker is asked to stop
///   (its successor starts under the new policy), and `faults` is forgiven.
///
/// `Buggy=1` is a host without its guards, each one a defect with its own
/// witness: it starts the new worker without waiting for the old one to end
/// (two supervisors answering one session's boxes, the double press the claim
/// exists to prevent — `OneSupervisor`) and over another supervisor's claim
/// (`HeldIsOff`), it counts failures past the window's cap (`FaultBudget`),
/// and it GIVES UP on a supervisor failing past the budget — the worker
/// ended with the badge up, nobody supervising and nothing to restart it
/// (`NeverGivesUp`: a badged session is still supervised, or its badged
/// worker is on its way out). The shipped
/// guard on `Fail` is `cur == 1` alone — `faults` never passes `Budget + 1`
/// there — and the `faults <= Budget + 1` half only bounds the buggy host's
/// space. Tier-1 (`aterm-gui`'s `harness_host` conformance)
/// drives the real host through each action and checks its observed state
/// against the model's after the same actions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_worker_lifecycle_model() -> Model {
    crate::ty_model! {
        HarnessWorkerLifecycle {
            const Buggy = 0;
            const Budget = 5;
            var wanted = 0;
            var cur = 0;
            var old = 0;
            var faults = 0;
            var faulted = 0;
            var held = 0;

            action Arrive when (wanted == 0) {
                wanted = 1;
            }
            action Leave when (wanted == 1 && old + cur <= 1) {
                wanted = 0;
                old = old + cur;
                cur = 0;
                held = 0;
            }
            action Start when (wanted == 1 && cur == 0
                && ((held == 0 && old == 0) || Buggy == 1)) {
                cur = 1;
            }
            action Exit when (old > 0) {
                old = old - 1;
                faulted = 0;
            }
            action Fail when (cur == 1 && faults <= Budget + 1) {
                faults = if (faults + 1 > Budget + 1 && Buggy == 0) { Budget + 1 } else { faults + 1 };
                faulted = if (faults + 1 > Budget) { 1 } else { 0 };
                cur = if (faults + 1 > Budget && Buggy == 1) { 0 } else { 1 };
            }
            action Restart when (cur == 1 && faulted == 1) {
                faulted = 0;
            }
            action Age when (faults > 0) {
                faults = faults - 1;
            }
            action Hold when (cur == 1) {
                cur = 0;
                held = 1;
                faulted = 0;
            }
            action Release when (held == 1) {
                held = 0;
            }
            action Reload when (old + cur <= 1) {
                old = old + cur;
                cur = 0;
                faults = 0;
            }

            invariant OneSupervisor: old + cur <= 1;
            invariant FaultBudget: faults <= Budget + 1;
            invariant NeverGivesUp: faulted == 0 || cur == 1 || old > 0;
            invariant HeldIsOff: held == 0 || cur == 0;
        }
    }
}

/// `aterm-agent`'s `harness::relaunch` as the host drives it, for ONE
/// session's agent: `running` the agent holds its tab; `asked` the tab is
/// LEFT — the last exit was a person's (a keystroke within `human_grace_s`
/// of it, or a person who came back during the back-off), a holder's (a
/// halt, a lease, a named driver's turn), or the launch's own end — and
/// nothing is typed into it; `pending` a relaunch is due; `badged` the
/// session's attention says the relaunch is limited, cannot be made or keeps
/// failing; `step` the back-off step the next attempt waits
/// (`Relaunches::pause`, `Steps` the last, ten minutes); `misses` the
/// attempts in a row that did not relaunch, capped at `Badge`.
///
/// * `Crash` — the agent left with no person asking: a relaunch is due.
/// * `PersonExit` — a person's `/exit`, ctrl-c twice, or a holder's exit:
///   the tab is theirs.
/// * `Limited` — nobody owns the exit (or the pending relaunch), but the
///   owner's `[harness] relaunch = false`, or an agent the relaunch is not
///   written for, takes the power away: nothing due, and said.
/// * `PersonRuns` — the person starts an agent there themselves: nothing
///   is due, and the relaunch's word goes.
/// * `Land` — an attempt relaunched it: running again, the misses and the
///   badge forgotten, the back-off one step longer.
/// * `Miss` — an attempt that did not (the shell's prompt not back, the
///   relaunch not registered): tried again one step later, and said to a
///   person once `Badge` missed in a row — and still tried. (Another actor
///   on the upgrade lock is no miss: a stutter, the state unchanged.)
/// * `Ended` — an attempt found the exit was the launch's own end (a
///   one-shot `-p` run, a launch that never registered a conversation,
///   nothing readable while it ran) or no exit at all (suspended by
///   someone): the tab is left, nothing said.
/// * `Cannot` — an attempt that never can (argv that cannot resume, the
///   shell gone): said to a person.
/// * `Healthy` — the relaunched agent ran long enough: the back-off starts
///   over.
/// * `PersonReturns` — a person or a holder came back to the tab during the
///   back-off: they have it now, and nothing is typed.
///
/// `Buggy=1` is a host without the rules the owner stated (2026-09-24): it
/// relaunches after a person's own exit (`PersonWins` — a person at the
/// keyboard wins), and it drops a relaunch it cannot make, or one the owner
/// limited, without a word (`NeverSilent` — "never give up silently";
/// escalation is for what configuration limited and what is irreducible).
/// Tier-1 (`the_real_relaunch_decisions_conform_to_the_model`,
/// `aterm-agent`) drives the real `on_exit`, `outcome` and `Relaunches`
/// through every action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_relaunch_on_exit_model() -> Model {
    crate::ty_model! {
        HarnessRelaunchOnExit {
            const Buggy = 0;
            const Steps = 3;
            const Badge = 3;
            var running = 1;
            var asked = 0;
            var pending = 0;
            var badged = 0;
            var step = 0;
            var misses = 0;

            action Crash when (running == 1) {
                running = 0;
                pending = 1;
                asked = 0;
            }
            action PersonExit when (running == 1) {
                running = 0;
                asked = 1;
                pending = if (Buggy == 1) { 1 } else { 0 };
            }
            action Limited when (running == 1 || pending == 1) {
                running = 0;
                pending = 0;
                badged = if (Buggy == 1) { badged } else { 1 };
            }
            action PersonRuns when (running == 0 && pending == 0) {
                running = 1;
                asked = 0;
                badged = 0;
            }
            action Land when (pending == 1) {
                running = 1;
                pending = 0;
                badged = 0;
                misses = 0;
                step = if (step <= Steps - 1) { step + 1 } else { Steps };
            }
            action Miss when (pending == 1) {
                misses = if (misses <= Badge - 1) { misses + 1 } else { Badge };
                badged = if (misses + 1 > Badge - 1) { 1 } else { badged };
                step = if (step <= Steps - 1) { step + 1 } else { Steps };
            }
            action Ended when (pending == 1) {
                pending = 0;
                asked = 1;
            }
            action Cannot when (pending == 1) {
                pending = 0;
                badged = if (Buggy == 1) { badged } else { 1 };
            }
            action Healthy when (running == 1 && step > 0) {
                step = 0;
            }
            action PersonReturns when (pending == 1) {
                pending = 0;
                asked = 1;
            }

            invariant PersonWins: asked == 0 || pending == 0;
            invariant NeverSilent: running == 1 || pending == 1 || badged == 1 || asked == 1;
        }
    }
}

/// WHAT AN EXIT LEFT, READ AS IT IS SEEN (D2 of the 2026-09-26 live test):
/// the relaunch on exit tells a crash from a graceful exit by Claude Code's
/// own record of the agent (`sessions/<pid>.json`) — a crash cannot remove
/// it, a graceful exit (a person's or an orchestrator's `/exit`, a `kill`)
/// does — and that record has a SECOND writer: any Claude Code that starts
/// removes dead processes' records (measured on 2.1.283: a SIGKILLed agent's
/// record gone 0.79 s after the kill, as a Claude in another tab started).
/// For ONE exit of ONE agent: `exit` 0 it runs, 1 it crashed, 2 it exited
/// gracefully; `record` its record is on disk; `seen` the host's look at the
/// exit (`aterm-agent`'s `relaunch::exit_record`) has decided; `kept` what
/// that look found (1: the record survived); `decided` 0 no attempt yet, 1
/// relaunched, 2 left as a graceful exit.
///
/// * `Crash` / `Graceful` — the agent ends. A graceful exit's record may
///   still be on disk as the exit is seen (it removes it in its last
///   moments: measured gone 0.02 s after the `/exit`).
/// * `OwnRemoval` — the graceful exit removes its own record.
/// * `Look` — the host's look decides, and its answer is KEPT: at once on a
///   removed record, else once `EXIT_SETTLE` (a quarter second) has passed,
///   which outlasts an exit's own removal — so a graceful exit is decided
///   only once its removal has landed.
/// * `Sweep` — another Claude Code starts and removes the dead agent's
///   record: at any time after the look, the relaunch's whole back-off (1 s
///   to ten minutes) included. (One inside the quarter second of the settle
///   is the window this design leaves: the assumption the guard states.)
/// * `Decide` — an attempt (`relaunch::after_exit`), after the back-off
///   (which outlasts an exit's own removal too): relaunched iff the KEPT
///   look found the record.
///
/// `CrashIsRelaunched`: a crash is never left as a graceful exit.
/// `GracefulIsLeft`: a graceful exit is never relaunched.
///
/// `Buggy=1` adds two hosts, each a dead action at `Buggy=0` caught alone:
/// the host of 57a2b7050, whose attempt reads the record as it stands then
/// (`DecideAtAttempt`: `Crash`, `Look`, `Sweep`, `DecideAtAttempt` leaves a
/// crash — `CrashIsRelaunched`), and the naive fix of it, whose look reads
/// at the detection instant, before a graceful exit's own removal
/// (`LookAtInstant`: `Graceful`, `LookAtInstant`, `OwnRemoval`, `Decide`
/// relaunches it — `GracefulIsLeft`). Tier-1
/// (`the_real_exit_read_conforms_to_the_model`, `aterm-agent`'s upgrade
/// tests) replays every path of the model through the real `exit_record`,
/// `look_at_exit` and `after_exit` over a real record of a dead process;
/// the first witness through the real read at the attempt
/// (`ExitRecord::Unread`); and an own removal landing inside the real
/// look's settle, which the real look reads as `OwnRemoval` then `Look` —
/// never as `LookAtInstant`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_exit_record_model() -> Model {
    crate::ty_model! {
        HarnessExitRecord {
            const Buggy = 0;
            var exit = 0;
            var record = 1;
            var seen = 0;
            var kept = 0;
            var decided = 0;

            action Crash when (exit == 0) {
                exit = 1;
            }
            action Graceful when (exit == 0) {
                exit = 2;
            }
            action OwnRemoval when (exit == 2 && record == 1) {
                record = 0;
            }
            action Look when (exit > 0 && seen == 0 && (exit == 1 || record == 0)) {
                seen = 1;
                kept = record;
            }
            action LookAtInstant when (Buggy == 1 && exit > 0 && seen == 0) {
                seen = 1;
                kept = record;
            }
            action Sweep when (exit > 0 && seen == 1 && record == 1) {
                record = 0;
            }
            action Decide when (seen == 1 && decided == 0 && (exit == 1 || record == 0)) {
                decided = if (kept == 1) { 1 } else { 2 };
            }
            action DecideAtAttempt when (Buggy == 1 && seen == 1 && decided == 0 && (exit == 1 || record == 0)) {
                decided = if (record == 1) { 1 } else { 2 };
            }

            invariant CrashIsRelaunched: exit == 0 || exit == 2 || decided <= 1;
            invariant GracefulIsLeft: exit <= 1 || decided == 0 || decided == 2;
        }
    }
}

/// THE UPGRADE'S LOOKS AT ONE SESSION, as its worker takes them at the
/// loop's idle points (`aterm-gui`'s `harness_host::WorkerIdle::at_idle`,
/// over `aterm-agent`'s `upgrade_drive::due`), and ITS NOTE BEHIND
/// (`harness_host::ask_note`, over `upgrade_drive::note_behind`), from the
/// worker's attach at a fresh launch. For ONE session: `due` it has an
/// upgrade to take; `readable` it can be read (the instance's socket
/// answers, Claude Code's record of the agent is written — at a launch, a
/// moment after the attach); `ready` it was announced to and answered
/// READY, so its next step restarts it; `looks` a look is owed — the loop
/// parked for the session's next idle point, or a later look set on the
/// ladder; `done` the upgrade said its last word; `owed` its note behind is
/// still to be taken; `noted` its upgrade state is minted, and `aged` with
/// its age from the attach.
///
/// * `Look` — a look that reads an upgrade to take asks the owed note first,
///   then steps it: the notice (the READY answer folded in), then the
///   restart and the last word. A step that finds no state mints one, its
///   age from the step.
/// * `NotDue` — a look that reads nothing to take lets the upgrade go (the
///   note, read, has nothing to note).
/// * `Unread` — a look that could not read says so (`wait:no-socket`,
///   `wait:no-record`) and sets a later look on the ladder.
/// * `Note` / `NoteUnread` — the owed note asked (at the attach, at the
///   host's wakes and ladder, at an idle point): read, it mints the state
///   with the attach's age, or finds nothing to note; unread, it stays owed.
/// * `Lose` / `Regain` — the reads fail and come back (the lanes held and
///   given back; the record written).
/// * `Settle` — the upgrade stops being due by another hand (the owner's
///   `--skip`, a person's own restart onto the build).
///
/// `NeverDropped`: an upgrade due and not done always has a look owed — one
/// that answered READY included (the live re-test of 155c72a28, 2026-09-26:
/// every control lane held, the looks' `who` was refused, `due` read "not
/// due", and four of seven sessions were let go for good — one that had
/// answered READY to "aterm will restart this Claude Code" among them).
/// `BehindFromTheAttach`: a state minted is behind from the attach (the same
/// re-test's N3: the note was asked once, at the attach, before the launch's
/// record was written, and the first step minted the state after the first
/// busy turn, its age from then).
///
/// `Buggy=1` is the host of 155c72a28: an unreadable look owes nothing
/// (`Regain`, `Look`, `Lose`, `Unread`: a READY'd session let go), and an
/// unreadable note is given up (`NoteUnread`, `Regain`, `Look`). Tier-1, in
/// two halves: `aterm-gui`'s `the_real_look_conforms_to_the_upgrade_look_model`
/// drives the real `attach`, `WorkerIdle` and `ask_note` through every
/// transition of the model from every reachable state, and projects the
/// worker's park, later look and owed note onto `looks` and `owed`; and
/// `aterm-agent`'s `the_real_classifier_conforms_to_the_upgrade_look_model`
/// drives the real `upgrade_drive::due_among` and `no_record` over every
/// combination of what a look reads of a tab (the record, its group, its
/// process's ids and argv, its launch's age), projecting each onto `due` and
/// `readable` — `readable` is every read the verdict needed — and checks the
/// model enables the action its verdict is.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_look_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeLook {
            const Buggy = 0;
            var due = 1;
            var readable = 0;
            var ready = 0;
            var looks = 1;
            var done = 0;
            var owed = 1;
            var noted = 0;
            var aged = 0;

            action Look when (looks == 1 && done == 0 && due == 1 && readable == 1) {
                ready = 1;
                done = ready;
                looks = if (ready == 1) { 0 } else { 1 };
                owed = 0;
                noted = 1;
                aged = if (noted == 1) { aged } else { owed };
            }
            action NotDue when (looks == 1 && done == 0 && due == 0 && readable == 1) {
                looks = 0;
                owed = 0;
            }
            action Unread when (looks == 1 && done == 0 && readable == 0) {
                looks = if (Buggy == 1) { 0 } else { 1 };
            }
            action Note when (owed == 1 && readable == 1) {
                owed = 0;
                noted = if (due == 1) { 1 } else { noted };
                aged = if (due == 1) { 1 } else { aged };
            }
            action NoteUnread when (owed == 1 && readable == 0) {
                owed = if (Buggy == 1) { 0 } else { 1 };
            }
            action Lose when (readable == 1) {
                readable = 0;
            }
            action Regain when (readable == 0) {
                readable = 1;
            }
            action Settle when (due == 1 && done == 0) {
                due = 0;
            }

            invariant NeverDropped: done == 1 || due == 0 || looks == 1;
            invariant BehindFromTheAttach: noted == 0 || aged == 1;
        }
    }
}
