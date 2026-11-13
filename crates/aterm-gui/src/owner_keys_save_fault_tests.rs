// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

/// A planted defect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// [`super::Provenance`] takes a control client's save for a person's.
    SaveIgnoresSource,
    /// [`super::Provenance`] takes a buffer a control client, an accessibility client or
    /// another process's input edited, or a recovered draft, for one a person typed.
    SaveIgnoresEdits,
    /// A save queued behind one in flight forgets the control client that asked for it.
    QueueDropsSource,
    /// [`super::is_in_force`] tells a file a save would make, while the aterm.toml in force
    /// is missing, from that file by its name alone.
    CreateIgnoresFolder,
    /// [`super::is_in_force`] compares files alone, never the path in force.
    PathIgnored,
    /// A discard of a buffer an unvouched edit reached keeps its undo history.
    DiscardKeepsHistory,
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
