// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's exceptions inbox raises a cause ONCE: what needs a look but not
//! now is one dismissable item, and dismissing it files it in a durable log;
//! the same cause is never raised again, across the restarts and updates that
//! replaced the process, and no surface shows an entry before its raise line is queued.
//!
//! Three shipped defects are the reason it is a machine and not a comment:
//! * the upgrade stall row re-posted for a stall the owner had already read
//!   (`stall_already_shown`, `seen_through` and `same_stall` exist to stop it,
//!   one reporter at a time);
//! * the Full Disk Access question raised on EVERY process start — eight rows
//!   in four days, six of them within a second of an update landing — because
//!   its only memory of a dismissal was the process's own;
//! * (guarded, not yet shipped) a surface counting an entry whose raise line
//!   is still in the writer's queue: an edge-triggered loss (a crash last
//!   time, scrollback left behind) is gone with the process that saw it.
//!
//! State for ONE key. `st`: 0 no entry, 1 open, 2 filed by the owner. `cause`
//! is what the reporter asserts now (1, then 2 after `Drift`, never back),
//! `ecause` the open entry's, `fcause` the cause the ledger remembers a
//! dismissal for, `disk` how much of the entry the file holds (0 nothing, 1
//! the raise, 2 the raise and the filing), `shown` whether the chip counts it.
//! `dcause` and `reshown` are ghosts: the last cause the owner dismissed, and
//! a dismissed cause shown again.
//!
//! THE UPDATE: a seamless update overlaps the old process and its
//! successor for seconds, and six of the eight Full Disk Access raises came
//! within a second of one. `succ` is 1 while a successor holds a snapshot of the
//! FILE it loaded at launch (`sst`, `sfc`: what it saw), frozen and
//! silent; `Commit` is the parent flushing its queue and the successor taking
//! over — by folding the file's tail in (`MessageCenter::catch_up`), never by
//! trusting the snapshot. A dismissal made in the parent after the successor
//! loaded is in the file by Commit, and the successor must honour it.
//!
//! Invariants: `DismissedStaysQuiet` (a dismissed cause is never shown again)
//! and `ShownImpliesQueued` (nothing is shown before its raise is queued).
//!
//! `Buggy = 1` switches on all three defects at once — each alone falsifies
//! its invariant, so none masks another — and each has its own switch:
//! `Reopen` (a filed cause with the same fingerprint is raised again),
//! `Early` (the chip shows before the raise is queued), `Volatile` (the
//! filing never reaches the file, so a restart replays the raise alone),
//! `StaleSuccessor` (the successor trusts the snapshot it loaded and never
//! folds the file's tail in at Commit, so the dismissal the owner made while
//! the two overlapped is forgotten), `NoFlush` (the parent execs without
//! flushing its queue, so the dismissal never reaches the file).
//!
//! Tier-1 is the real `MessageCenter` (`aterm-messages/src/family_tests.rs`,
//! `crates/aterm-messages/tests/conformance_digest.rs`): a restart replays the
//! ENCODED log, a seamless update's successor loads the file and folds the
//! parent's tail in at Commit (`MessageCenter::catch_up`), and the same cause
//! asserted again is `Suppressed`; `StaleSuccessor` (no catch-up) and
//! `NoFlush` (the parent's dismissal never reached the file) are the caught
//! negative controls.

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

            // A seamless update: the successor launches and loads the FILE as it is
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
