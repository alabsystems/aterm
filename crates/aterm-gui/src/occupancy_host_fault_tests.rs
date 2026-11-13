// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

/// A planted defect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// A holder's re-ask renews its reservation from the ask.
    RenewOnAsk,
    /// A slot ask counts no older hold waiting.
    YoungestFirst,
    /// A holder's re-ask counts its own reservation against it.
    CountOwn,
    /// The fact word's edit moves the record's `rev` too, so every `await task` past a rev
    /// with news standing wakes on it.
    NoteBumpsNews,
    /// An `await wake` answer's slot is asked of the typed lines' pace (`pace_on`), which
    /// under `observe` grants nothing.
    PaceOnAwait,
    /// An `await wake` answer whose need names slots takes no reservation.
    AwaitSkipsReserve,
    /// The wake K clock is fed only from a loop's break, which no loop of this build runs.
    KFromBreakOnly,
    /// A restored book's `pid:` atoms are not armed again: their births are not carried over.
    NoRearm,
}

thread_local! {
    static ARMED: Cell<Option<Fault>> = const { Cell::new(None) };
}

/// Run `f` with `fault` armed on this thread, disarmed after it even on unwind.
pub(crate) fn with<T>(fault: Fault, f: impl FnOnce() -> T) -> T {
    struct Disarm;
    impl Drop for Disarm {
        fn drop(&mut self) {
            ARMED.with(|a| a.set(None));
        }
    }
    ARMED.with(|a| a.set(Some(fault)));
    let _disarm = Disarm;
    f()
}

/// Whether `fault` is armed on this thread.
pub(crate) fn armed(fault: Fault) -> bool {
    ARMED.with(|a| a.get() == Some(fault))
}
