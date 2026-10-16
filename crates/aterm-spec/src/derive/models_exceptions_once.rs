// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's exceptions inbox raises a cause once: what needs a look but not now is one
//! dismissable item, dismissing it files it in a durable log, the same cause is never raised
//! again across restarts and updates, and no surface shows an entry before its raise line is
//! queued. A dismissal held only in process memory re-raises on every start.
//!
//! State for one key. `st`: 0 no entry, 1 open, 2 filed by the owner. `cause` is what the
//! reporter asserts now (1, then 2 after `Drift`, never back), `ecause` the open entry's, `fcause`
//! the cause the ledger remembers a dismissal for, `disk` how much of the entry the file holds (0
//! nothing, 1 the raise, 2 the raise and the filing), `shown` whether the chip counts it. `dcause`
//! and `reshown` are ghosts: the last cause dismissed, and a dismissed cause shown again.
//!
//! A seamless update overlaps the old process and its successor for seconds. `succ` is 1 while
//! a successor holds, frozen, the snapshot of the file it loaded at launch (`sst`, `sfc`);
//! `Commit` is the parent flushing its queue and the successor taking over by folding the file's
//! tail in (`MessageCenter::catch_up`), never by trusting the snapshot, so a dismissal made in
//! the parent after the successor loaded is honoured.
//!
//! Invariants: `DismissedStaysQuiet` (a dismissed cause is never shown again) and
//! `ShownImpliesQueued` (nothing is shown before its raise is queued).
//!
//! `Buggy = 1` switches on every mutant at once; each also has its own switch and alone
//! falsifies its invariant: `Reopen` (a filed cause is raised again), `Early` (the chip shows
//! before the raise is queued), `Volatile` (the filing never reaches the file, so a restart
//! replays the raise alone), `StaleSuccessor` (the successor trusts its snapshot at Commit),
//! `NoFlush` (the parent execs without flushing, so the dismissal never reaches the file).
//!
//! Tier-1 is the real `MessageCenter` in `aterm-messages/src/family_tests.rs`: a restart replays
//! the encoded log, a successor folds the parent's tail in at Commit, the same cause asserted
//! again is `Suppressed`, and `StaleSuccessor` and `NoFlush` are caught negative controls.

use super::Model;

/// See the module doc.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exceptions_once_model() -> Model {
    crate::ty_model! {
        ExceptionsOnce {
            const Buggy = 0;
            const Reopen = 0;
            const Early = 0;
            const Volatile = 0;
            const StaleSuccessor = 0;
            const NoFlush = 0;
            var st = 0;
            var cause = 1;
            var ecause = 0;
            var fcause = 0;
            var disk = 0;
            var shown = 0;
            var dcause = 0;
            var reshown = 0;
            var succ = 0;
            var sst = 0;
            var sfc = 0;

            // The reporter says its cause: a new entry, or news after a dismissal.
            action Assert when (st == 0 || (st == 2 && (cause > fcause || fcause > cause)) || (st == 2 && (Buggy == 1 || Reopen == 1))) {
                st = 1;
                ecause = cause;
                shown = 0;
                disk = 0;
            }
            // The reporter says a different cause for the entry that is open: it
            // is restated in place, still one entry, and remembered as that cause.
            action Restate when (st == 1 && (ecause > cause || cause > ecause)) {
                ecause = cause;
            }
            // The writer appends the raise line (even after the owner filed it).
            action Log when ((st == 1 || st == 2) && disk == 0) {
                disk = 1;
            }
            // The chip counts it: only once the raise is queued.
            action Show when (st == 1 && (disk > 0 || Buggy == 1 || Early == 1)) {
                shown = 1;
                reshown = if ecause == dcause { 1 } else { reshown };
            }
            // The owner files it (one press files every open entry).
            action Dismiss when (st == 1) {
                st = 2;
                fcause = ecause;
                dcause = ecause;
                shown = 0;
            }
            // The writer appends the filing line; the buggy ledger never does.
            action LogFiled when (st == 2 && disk == 1 && Buggy <= 0 && Volatile <= 0) {
                disk = 2;
            }
            // Its reporter stops asserting it: the cause is gone.
            action Resolve when (st == 1) {
                st = 0;
                shown = 0;
                disk = 0;
            }
            // The cause changes for good (never back).
            action Drift when (cause == 1) {
                cause = 2;
            }
            // A graceful restart: the writer flushes, the new process replays.
            action Restart when (succ == 0) {
                shown = 0;
                st = if st == 0 { 0 } else { if st == 1 { 1 } else { if Buggy <= 0 && Volatile <= 0 { 2 } else { 1 } } };
                fcause = if st == 2 { if Buggy <= 0 && Volatile <= 0 { fcause } else { 0 } } else { fcause };
                disk = if st == 0 { disk } else { if st == 1 { 1 } else { if Buggy <= 0 && Volatile <= 0 { 2 } else { 1 } } };
            }

            // A seamless update: the successor launches and loads the file as it is
            // now (not the parent's memory), then waits, frozen.
            action Handoff when (succ == 0) {
                succ = 1;
                sst = if disk == 2 { 2 } else { if disk == 1 { 1 } else { 0 } };
                sfc = if disk == 2 { fcause } else { 0 };
            }
            // Commit: the parent flushes its queue and the successor takes over,
            // folding the file's tail in (the buggy one trusts its snapshot).
            action Commit when (succ == 1) {
                disk = if NoFlush == 1 || Buggy == 1 { disk } else { if st == 0 { disk } else { if st == 1 { 1 } else { if Volatile <= 0 { 2 } else { 1 } } } };
                st = if StaleSuccessor == 1 || Buggy == 1 { sst } else { if NoFlush == 1 { if disk == 2 { 2 } else { if disk == 1 { 1 } else { 0 } } } else { if st == 0 { if disk == 2 { 2 } else { if disk == 1 { 1 } else { 0 } } } else { if st == 1 { 1 } else { if Volatile <= 0 { 2 } else { 1 } } } } };
                fcause = if StaleSuccessor == 1 || Buggy == 1 { sfc } else { if NoFlush == 1 { if disk == 2 { fcause } else { 0 } } else { fcause } };
                shown = 0;
                succ = 0;
                sst = 0;
                sfc = 0;
            }

            invariant DismissedStaysQuiet: reshown == 0;
            invariant ShownImpliesQueued: shown == 0 || disk > 0;
        }
    }
}
