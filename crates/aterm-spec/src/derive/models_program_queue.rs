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
