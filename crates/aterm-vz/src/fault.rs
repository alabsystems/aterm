// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The negative controls' fault points (`tests/conformance_lifecycle.rs`): each a planted
//! defect of the lifetime `VzLifecycle` models (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1,
//! §12), armed on one thread for one walk, so the walk shows the conformance seeing it and
//! the model's mutant naming the law it breaks. Test and debug builds only; nothing arms one
//! outside a test.

use std::cell::Cell;

/// A planted defect, and the `VzLifecycle` mutant whose step it makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `helper::take` does not stop at the live disk's held `flock`, so a second helper for
    /// the disk starts while the first runs its VM (`SecondBoots`).
    NoFlock,
    /// [`crate::layers::Layers::prepare`] clones the live disk from a bake's partial file
    /// (`ClonePartial`).
    ClonePartial,
    /// [`crate::layers::Layers::prepare`] hands the sealed baked disk to attach writable
    /// (`AttachBaked`).
    AttachBaked,
    /// `agent::run` reads past a command's `Exit`, so the command ends `Lost` while its VM
    /// runs (`DropExit`).
    DropExit,
    /// The helper's release rule keeps the VM running when the last session closes with
    /// `keep` unset (`KeepRunning`).
    KeepRunning,
    /// The helper stops the VM hard when a session closes and another is left
    /// (`HardUnasked`).
    HardOnClose,
    /// `record::holder` and `record::kill_helper` take a pid that runs for the record's
    /// helper whatever its unique id and start time (`SignalReused`).
    PidOnly,
    /// The helper takes the agent's `Ready` before its hello: a watch dial's reader posts a
    /// `Ready` first, the core takes that dial as the watch connection, and the `Ready` with
    /// no hello (`ReadyUnchecked`). Each check alone stops it.
    ReadyBeforeHello,
    /// The helper opens a command connection while the VM boots, before its `Ready`
    /// (`ExecBooting`).
    ConnectBooting,
    /// The helper waits on a hard stop that never finishes instead of exiting a span later:
    /// no mutant's step, but the walk's `HardTimeout` sees it.
    HardStopForever,
}

thread_local! {
    static ARMED: Cell<Option<Fault>> = const { Cell::new(None) };
}

/// Run `f` with `fault` armed on this thread, disarmed after it even on unwind.
pub fn with<T>(fault: Fault, f: impl FnOnce() -> T) -> T {
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
#[must_use]
pub fn armed(fault: Fault) -> bool {
    ARMED.with(|a| a.get() == Some(fault))
}
