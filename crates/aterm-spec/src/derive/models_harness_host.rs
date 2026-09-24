// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-GUI supervisor host's per-session worker lifecycle: at most ONE
//! supervisor on a session at a time, a failing one restarted within a budget
//! and then off, a session another supervisor holds left alone until a claim
//! is released.

use super::Model;

/// `aterm-gui`'s `harness_host` runs one worker per Claude session. For ONE
/// session: `wanted` is the session's published program being `claude` (and
/// the policy active); `cur` a worker running under the current policy;
/// `old` workers asked to stop (the program left, the policy changed) whose
/// wait has not ended yet; `faults` the SESSION's failed runs in the restart
/// window — across its workers, kept at most `Budget + 1`; `faulted` the
/// session's supervisor off after the budget; `held` another supervisor's
/// claim refused ours.
///
/// * `Arrive`/`Leave` — the program becomes / stops being claude. Leaving asks
///   the worker to stop and forgets `faulted` (its badge clears) and `held`
///   (the host keeps them only for a session it still wants) — but NOT
///   `faults`: a program flap gives a crash-looping engine no fresh budget.
/// * `Start` — the host starts a worker for a wanted session with none of its
///   own, not faulted, not held, and — the property — NO OLD WORKER STILL
///   RUNNING: the host reaps a stopped worker before it starts the next one.
///   It inherits the session's `faults`.
/// * `Exit` — an old worker's wait ends and it is reaped.
/// * `Fail` — the current worker's run failed or panicked: restarted in
///   place while `faults` stays within `Budget`, else the supervisor is off.
/// * `Age` — the oldest failure leaves the hour's window.
/// * `Hold` — the claim was refused: the worker ends; `Release` — a claim
///   was released or lapsed somewhere, so a held session may be tried again.
/// * `Reload` — the `[harness]` policy changed: the worker is asked to stop
///   (its successor starts under the new policy), and `faulted` and `faults`
///   are forgiven.
///
/// `Buggy=1` is a host without its guards, each one a defect with its own
/// witness: it starts the new worker without waiting for the old one to end
/// (two supervisors answering one session's boxes, the double press the claim
/// exists to prevent — `OneSupervisor`), over a faulted session
/// (`FaultedIsOff`) and over another supervisor's claim (`HeldIsOff`), and it
/// restarts a failing worker past the budget, counting on (`FaultBudget`, and
/// `FaultedIsOff` again: faulted, yet running). The shipped guard on `Fail`
/// is `cur == 1` alone — `faults` never passes `Budget + 1` there — and the
/// `faults <= Budget + 1` half only bounds the buggy host's space. Tier-1 (`aterm-gui`'s `harness_host` conformance)
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
                faulted = 0;
                held = 0;
            }
            action Start when (wanted == 1 && cur == 0
                && ((faulted == 0 && held == 0 && old == 0) || Buggy == 1)) {
                cur = 1;
            }
            action Exit when (old > 0) {
                old = old - 1;
            }
            action Fail when (cur == 1 && faults <= Budget + 1) {
                faults = if (faults + 1 > Budget + 1 && Buggy == 0) { Budget + 1 } else { faults + 1 };
                faulted = if (faults + 1 > Budget) { 1 } else { 0 };
                cur = if (faults + 1 > Budget && Buggy == 0) { 0 } else { 1 };
            }
            action Age when (faults > 0) {
                faults = faults - 1;
            }
            action Hold when (cur == 1) {
                cur = 0;
                held = 1;
            }
            action Release when (held == 1) {
                held = 0;
            }
            action Reload when (old + cur <= 1) {
                old = old + cur;
                cur = 0;
                faulted = 0;
                faults = 0;
            }

            invariant OneSupervisor: old + cur <= 1;
            invariant FaultBudget: faults <= Budget + 1;
            invariant FaultedIsOff: faulted == 0 || cur == 0;
            invariant HeldIsOff: held == 0 || cur == 0;
        }
    }
}
