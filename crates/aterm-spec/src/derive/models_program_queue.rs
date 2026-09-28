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
