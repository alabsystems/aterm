// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mutant {
    /// Store a peer's pushed evidence before judging it, as if a push were records to take.
    StoresPushFirst,
    /// Decode a legacy log witness within this build's held budget, not the legacy count.
    WitnessAtHeldBudget,
    /// End an `atdb/5` session when its membership rounds leave records for a later one.
    DeferredEndsSession,
    /// Refuse a whole `atdb/5` push at a record its receiver has no room for, rather than
    /// judge the peer on the records that fit.
    CapacityRefusesPush,
    /// Pass a peer while this Mac's membership records do not read, as the peer check did
    /// before X1: the session's view states nothing, so a push is judged from its own
    /// genesis, and the local log check lets the peer through.
    UnreadCustodyAdmits,
    /// Judge a peer by the records this Mac acknowledged alone, as the local log check did
    /// before X1, so a `Remove` made durable here and not yet acknowledged takes no one out.
    AcknowledgedOnly,
    /// Read a busy or unread roster with an empty deny list as no roster, nothing to check,
    /// as the roster gate did before PF4.
    BusyRosterIsNoRoster,
    /// Check a peer once per thread and pass every later check unwalked, as the legacy
    /// (`atdb/2`, `atdb/3`, no ALPN) session did before PF4: one check, then every grant.
    CheckedOnce,
    /// Read an acknowledged log of another lineage as one with no Commons epoch, nothing to
    /// judge by, as `trusted_commons_log`'s `Ok(None)` did for both before PF4.
    LineageIsNoJudgement,
    /// Read `atdb/7`'s ALPN as `atdb/8`, so this build speaks `atdb/8`'s range-reconciled control
    /// and membership frames to a peer that speaks `atdb/7`'s whole lists.
    RangesOnReconcile,
    /// Reconcile the control journal by range alone on `atdb/8`, with no update chain agreed by
    /// its frontier first, as a store with no causal chains does.
    ControlRangesOnly,
    /// State a records bound of format 2 on the frontier wires wherever this Mac's build reads
    /// format 2, whether or not its format mark is stored.
    StatesFormatUnmarked,
    /// State a records bound of a later format to any peer, with no evidence that it reads one.
    StatesFormatBlind,
    /// Read a records bound of a later format in a peer's frame on a Mac that states format 1.
    ReadsFormatUngated,
    /// Exchange with a peer whose bound differs from this Mac's only in its format, so the
    /// peer's records, not its bound, are what this Mac refuses.
    FormatUnjudgedAtBound,
    /// Read a record a root of this Mac's log dropped as one it does not hold: a push that
    /// brings it is not complete, and a reconciliation does not name it.
    DroppedUnheld,
    /// Serve no root on the records wire: a peer behind a cut is sent records alone.
    ServesNoRoot,
    /// Serve a rooted log's roots to every peer of format 2, whatever its reconciled set holds.
    ServesAtTheCut,
    /// Settle a served batch the peer took none of as a refused round of records.
    RefusedRootStrikes,
    /// Set the served bit in every batch header, whatever its format and whether a frame
    /// follows.
    ServedBitUngated,
    /// Read the served bit in a batch header that ends with no format trailer, as one that
    /// says a frame follows.
    ServedBitUnread,
    /// Serve a rooted log's roots again in the session after the peer took nothing of them,
    /// under the same views.
    ServesRefusedRoot,
}

thread_local! {
    static ON: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

/// Run `run` with `mutant` on in this thread.
pub(crate) fn with<T>(mutant: Mutant, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Mutant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ON.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(ON.with(|slot| slot.replace(Some(mutant))));
    run()
}

/// Whether `mutant` is on in this thread.
pub(crate) fn on(mutant: Mutant) -> bool {
    ON.with(|slot| slot.get() == Some(mutant))
}
