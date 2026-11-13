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
