// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One session's process-name requests on the GUI's single resolver worker.
//! The real queue is bound in `aterm-gui::session_program` tests.

use super::*;

/// A session owns one latest job and at most one channel token. A newer
/// foreground group replaces a queued/in-flight request; after an old lookup
/// finishes, the latest job is queued once. Restart requeues a job retained
/// when the worker panicked. `Buggy=1` sends every replacement to the old
/// unbounded channel, making duplicate tokens reachable.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn program_resolver_queue_model() -> Model {
    crate::ty_model! {
        ProgramResolverQueue {
            const Buggy = 0;
            var requests = 0;
            var latest = 0;
            var queued = 0;
            var inflight = 0;
            var snapshot = 0;
            var completed = 0;
            var worker = 1;
            var active = 0;
            var owned = 0;

            action AskFirst when (requests == 0) {
                requests = 1;
                latest = 1;
                queued = 1;
                active = 1;
                owned = 1;
            }
            action Take when (worker == 1 && queued == 1 && inflight == 0 && active == 1) {
                queued = 0;
                inflight = 1;
                snapshot = latest;
            }
            action DropCancelled when (worker == 1 && queued == 1 && inflight == 0 && active == 0) {
                queued = 0;
                owned = 0;
            }
            action AskNewGroup when (requests == 1 && inflight == 1 && active == 1) {
                requests = 2;
                latest = 2;
                queued = if Buggy == 1 { 1 } else { 0 };
            }
            action AskAgain when (requests == 2 && inflight == 1 && active == 1) {
                requests = 3;
                queued = if Buggy == 1 { queued + 1 } else { queued };
            }
            action Cancel when (requests <= 2 && requests > 0 && owned == 1 && active == 1) {
                active = 0;
            }
            action Reenable when (active == 0 && owned == 1 && requests <= 2) {
                requests = 3;
                latest = 3;
                active = 1;
            }
            action Finish when (worker == 1 && inflight == 1) {
                inflight = 0;
                completed = snapshot;
                queued = if active == 1 && latest > snapshot {
                    if Buggy == 1 { queued } else { 1 }
                } else { 0 };
                owned = if active == 1 && latest > snapshot { 1 } else { 0 };
            }
            action Crash when (worker == 1 && owned == 1) {
                worker = 0;
                inflight = 0;
                queued = 0;
            }
            action Restart when (worker == 0) {
                worker = 1;
                queued = if owned == 1 { 1 } else { 0 };
            }

            invariant OneTokenPerSession: queued + inflight <= 1;
            invariant LatestRequestRemainsOwned:
                if worker == 1 && inflight == 0 && active == 1 && latest > completed {
                    queued == 1
                } else { queued <= 1 };
        }
    }
}

/// One Claude footer resolver watch. Group 1 is the old foreground process,
/// group 2 its replacement. A stop for group 1 removes only that watch; a
/// session stop (retire or status-off) removes either. `IdleRead` is enabled
/// exactly while a watch exists. `Buggy=1` makes the old-group stop erase the
/// replacement, the stale-stop race the GUI's scheduler must refuse, and makes
/// the session stop leave its watch behind — the dormant-refresh class a
/// retired or status-off session read through (`DormantWatchCannotRead`). Both
/// mutants are branches of LIVE actions, not an action only `Buggy` enables:
/// `ty --strict-vacuity` credits a dead action only when it alone supplies its
/// counterexample, and here the stale-stop branch would supply one too.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_footer_watch_model() -> Model {
    crate::ty_model! {
        ClaudeFooterWatch {
            const Buggy = 0;
            var watch = 0;
            var asked_old = 0;
            var asked_new = 0;
            var old_stopped = 0;
            var retired = 0;

            action AskOld when (asked_old == 0 && retired == 0) {
                asked_old = 1;
                watch = 1;
            }
            action AskNew when (asked_old == 1 && asked_new == 0 && retired == 0) {
                asked_new = 1;
                watch = 2;
            }
            action StopOld when (asked_old == 1 && old_stopped == 0 && retired == 0) {
                old_stopped = 1;
                watch = if watch == 1 || Buggy == 1 { 0 } else { watch };
            }
            action StopSession when (asked_old == 1 && retired == 0) {
                retired = 1;
                // Buggy=1: the dormant-refresh class (`b916d37ff`) — the stop
                // leaves its watch behind, so a retired session keeps reading.
                watch = if Buggy == 1 { watch } else { 0 };
            }
            action IdleRead when (watch > 0 && retired == 0) {
                watch = watch;
            }

            invariant StaleStopKeepsReplacement:
                if asked_new == 1 && old_stopped == 1 && retired == 0 {
                    watch == 2
                } else { watch <= 2 };
            invariant DormantWatchCannotRead:
                if retired == 1 || (old_stopped == 1 && asked_new == 0) {
                    watch == 0
                } else { watch <= 2 };
        }
    }
}

/// WHICH MODEL the Claude Code footer names (`aterm_agent::harness::footer`,
/// owner 2026-09-28: "I need to see the current model selection in the footer
/// of aterm"). `runs` is the model the process runs now. `seen` is the newest
/// model statement since the floor, in the current session, that the reader
/// can see: 0 none, 1 an answer, 2 a readable choice (`Set model to `Opus
/// 5.5``), 3 an unreadable one (a `/model` row with no result the build
/// parses). `flag` is the process's own `--model` naming model 1, `card` the
/// model its launch card names (0 none), taken at its worst: naming the
/// starting model even after a choice (the inline renderer draws it once; the
/// fullscreen one redraws it with the new model, measured 2026-09-28).
/// `Read` is the host's resolver: the newest statement since the floor, else
/// what the process carried, else the flag, else the card, else nothing —
/// never an unreadable choice's predecessor. `Flood` appends a row larger than
/// the reader's tail window (the owner's 530 KiB image prompt); the reader's
/// carry keeps what it had seen.
///
/// THE LAUNCH (`launched = 0`, before the reader's first sight of the
/// process): `Flag` gives it `--model` 1; `LaunchResume` launches it onto
/// conversation A with `--resume` — A's rows sit above the floor, and it last
/// answered with `Pred` — and Claude RESTORES `Pred` at launch unless the
/// flag pinned the model (2.1.284: every launch resume runs the in-REPL
/// restore); `DrawCard` draws the launch card, naming the model it starts on.
/// Without `LaunchResume` the process starts on model 1 in a session of its
/// own. The first `Read` is the reader's first sight.
///
/// `Clear` is `/clear`: the SAME process takes a new session id — the
/// registry's `sessionId` rewritten under the same pid, kernel start and
/// `startedAt` — whose transcript has said nothing, while the process runs on
/// what it ran. `carry` is the newest statement the process made in a session
/// it left (0 none, else as `seen`); `Read` takes it where the current session
/// says nothing, before the flag and the card. `known` is whether the reader
/// has read the current session at all (it reads at its first sight of the
/// process, and every refresh after): the reader re-reads the session it LEFT
/// at the switch, so it needs to have seen that session once, not every
/// statement in it — the environment assumption is at most one switch
/// between two refreshes.
///
/// `Resume` is an in-REPL `/resume` into the next conversation — A (`Pred`'s),
/// then B, which last answered with `Other`; a process launched onto A goes
/// on to B — the same switch, into a conversation with
/// rows from before the floor, and Claude RESTORES its model unless the
/// process is `pinned` — by the flag or a choice (1), or by an earlier
/// RESTORE (2): Claude restores by setting the very main-loop override its
/// next restore returns early on, and no switch clears it (2.1.284 `Cbe` →
/// `overrideMainLoopModel`). A restore therefore PINS, and every later
/// `Resume` keeps the model. `resumed` counts the conversations taken up
/// (the launch's included); `restored` says how the newest switch went — 1
/// it restored, 2 only a restore's pin kept the model, 0 otherwise — and
/// `Read` shows the restored model where it restored, the carry where it
/// kept. No `Clear` follows a `Resume`: a `Clear` out of a resumed
/// conversation is the same switch as any `Clear`, and it keeps the space the
/// Tier-1 bind walks small.
///
/// `ResumeLate` is the first in-REPL `/resume` taken instead into D — a
/// conversation ANOTHER process began and answered with `Pred` after this
/// one started, before the reader last saw this one in the session it
/// leaves (`late`; the `/resume` picker lists the newest first). It is a
/// resume like any: Claude restores `Pred` unless pinned, and the restore
/// pins. Its rows from before the switch are D's, never this process's
/// statements, so the reader tells the switch from a `Clear` by WHEN it came
/// — after the reader last saw the process in the session it left — not by
/// the floor, which D's rows are all above. A later `Resume` goes on to B.
/// It needs no card: after a resume the carry, the flag or the restore
/// decides.
///
/// `Buggy=1`, each a branch of a live action: `Read` shows nothing after a
/// readable choice (the pre-2026-09-28 reader, which voided the model at
/// every `/model` until the next answer — `ChoiceShownAtOnce`); reads the
/// resumed conversation's own rows from above the floor as the process's (a
/// reader without the floor, which names `Pred` under a `--model` — and the
/// reader of the second 2026-09-28 round, which took a restore for no pin and
/// so restored `Other` at the second resume — `ShownIsTheRunningModel`);
/// after a `Clear` reads only the new session's empty transcript, falling to
/// the flag, the card or nothing (the reader that kept nothing of the process
/// — `ClearKeepsTheModel`, and `ShownIsTheRunningModel` when the flag names the
/// starting model); where Claude restored, reads the switch as a `Clear` — the
/// process's carried answer, else its card, else nothing (the reader of the
/// first 2026-09-28 round — `ResumeNamesTheRestoredModel`); `Flood` forgets
/// what was seen (no carry), so the starting model stands in for a choice
/// (`ShownIsTheRunningModel`). The reader that judges a switch by the floor
/// alone is the same two mutants on D: under a pin it reads D's own answer
/// as the process's (`Pred`), and unpinned it misses the restore's pin and
/// restores `Other` at the next resume.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn claude_footer_model_model() -> Model {
    crate::ty_model! {
        ClaudeFooterModel {
            const Buggy = 0;
            const Pred = 2;
            const Other = 1;
            var launched = 0;
            var runs = 1;
            var seen = 0;
            var carry = 0;
            var known = 0;
            var flag = 0;
            var card = 0;
            var pinned = 0;
            var resumed = 0;
            var restored = 0;
            var late = 0;
            var shown = 0;
            var synced = 0;

            action Flag when (launched == 0 && flag == 0 && card == 0) {
                flag = 1;
                runs = 1;
                pinned = 1;
                restored = 0;
                synced = 0;
            }
            action LaunchResume when (launched == 0 && resumed == 0 && card == 0) {
                resumed = 1;
                // Claude restores the conversation's model unless pinned —
                // and the restore pins it.
                runs = if flag == 1 { 1 } else { Pred };
                pinned = if flag == 1 { 1 } else { 2 };
                restored = if flag == 1 { 0 } else { 1 };
                synced = 0;
            }
            action DrawCard when (launched == 0 && card == 0) {
                card = runs;
                synced = 0;
            }
            action Answer when (launched == 1 && seen <= 3) {
                seen = 1;
                synced = 0;
            }
            action ChooseA when (launched == 1 && seen <= 3) {
                runs = 1;
                seen = 2;
                pinned = 1;
                synced = 0;
            }
            action ChooseB when (launched == 1 && seen <= 3) {
                runs = 2;
                seen = 2;
                pinned = 1;
                synced = 0;
            }
            action ChooseUnread when (launched == 1 && seen <= 3) {
                runs = if runs == 1 { 2 } else { 1 };
                seen = 3;
                pinned = 1;
                synced = 0;
            }
            action Flood when (seen > 0) {
                // Buggy=1: no carry — the huge row pushes the statement out
                // of the reader's window and it is forgotten.
                seen = if Buggy == 1 { 0 } else { seen };
                synced = 0;
            }
            action Clear when (launched == 1 && seen > 0 && known == 1 && resumed == 0) {
                carry = seen;
                seen = 0;
                known = 0;
                synced = 0;
            }
            action Resume when (launched == 1 && resumed <= 1 && known == 1) {
                // What the process last said — or the model restored at the
                // switch before — is carried.
                carry = if seen > 0 { seen } else { if restored == 1 { 1 } else { carry } };
                // Unpinned, the process has taken up no conversation yet:
                // Claude restores A's model, `Pred`.
                runs = if pinned == 0 { Pred } else { runs };
                restored = if pinned == 0 { 1 } else { if pinned == 2 { 2 } else { 0 } };
                // The restore pins the model.
                pinned = if pinned == 0 { 2 } else { pinned };
                seen = 0;
                known = 0;
                resumed = resumed + 1;
                synced = 0;
            }
            action ResumeLate when (launched == 1 && resumed == 0 && known == 1 && card == 0) {
                // Into D, begun by another process since this one started;
                // it last answered `Pred`, which Claude restores unpinned.
                carry = if seen > 0 { seen } else { carry };
                runs = if pinned == 0 { Pred } else { runs };
                restored = if pinned == 0 { 1 } else { 0 };
                pinned = if pinned == 0 { 2 } else { pinned };
                seen = 0;
                known = 0;
                resumed = 1;
                late = 1;
                synced = 0;
            }
            action Read when (synced == 0) {
                shown = if seen == 1 || seen == 2 {
                    // Buggy=1: the old reader voided a readable choice.
                    if Buggy == 1 && seen == 2 { 0 } else { runs }
                } else if seen == 3 {
                    0
                } else if Buggy == 1 && resumed > 0 && (restored == 0 || restored == 2) {
                    // Buggy=1: the resumed conversation's own model — the
                    // floorless reader's under a pin (and the floor-only
                    // judge's in D), and the reader that took a restore for
                    // no pin at the second resume (or missed D's).
                    if resumed == 2 { Other } else { Pred }
                } else if restored == 1 {
                    // The conversation's own model, which Claude restored.
                    // Buggy=1: the resume read as a clear — the process's
                    // carried answer (its starting model), else its card.
                    if Buggy == 1 {
                        if carry == 1 || carry == 2 { 1 } else { card }
                    } else {
                        runs
                    }
                } else if carry > 0 && Buggy == 0 {
                    // The process's own statement, from the session it left.
                    if carry == 3 { 0 } else { runs }
                } else if flag == 1 {
                    // Buggy=1 after a clear: the flag stands in for a choice.
                    1
                } else {
                    card
                };
                launched = 1;
                known = 1;
                synced = 1;
            }

            invariant ShownIsTheRunningModel:
                if synced == 1 { shown == 0 || shown == runs } else { shown <= 2 };
            invariant ChoiceShownAtOnce:
                if synced == 1 && (seen == 1 || seen == 2) {
                    shown == runs
                } else { shown <= 2 };
            invariant ClearKeepsTheModel:
                if synced == 1 && seen == 0 && (restored == 0 || restored == 2) && (carry == 1 || carry == 2) {
                    shown == runs
                } else { shown <= 2 };
            invariant ResumeNamesTheRestoredModel:
                if synced == 1 && seen == 0 && restored == 1 {
                    shown == runs
                } else { shown <= 2 };
        }
    }
}
