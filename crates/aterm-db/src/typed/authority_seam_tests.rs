// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The authority rules' controls: a test turns one rule off, or admits every write, for the
//! reads its own thread makes while it holds the guard.

use super::Rules;
use std::cell::Cell;

/// What a control changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// Rule 2 off: an automatic write on an Owner column stands.
    Owner,
    /// Rule 3a off: an automatic write on a Birth column after its row's birth stands.
    AfterBirth,
    /// Rule 3b off: an automatic Birth write beside a write that is not automatic stands.
    Contested,
    /// Rule 4 off: a write to a fixed column after its row's birth stands.
    Fixed,
    /// The revert rule off: a revert's cell stands whatever it restores.
    Restored,
    /// The verb rule judges only the versions the other rules judge.
    VerbFromAuthority,
    /// Every write stands.
    AdmitAll,
}

thread_local! {
    static CONTROL: Cell<Option<Control>> = const { Cell::new(None) };
}

/// While it lives, this thread's reads run under its control.
pub(crate) struct Guard(Option<Control>);

impl Drop for Guard {
    fn drop(&mut self) {
        CONTROL.with(|control| control.set(self.0));
    }
}

/// Run this thread's reads under `control` until the guard drops.
pub(crate) fn control(control: Control) -> Guard {
    Guard(CONTROL.with(|now| now.replace(Some(control))))
}

/// The rules this thread's reads judge now.
pub(super) fn rules() -> Rules {
    let mut rules = Rules::ALL;
    match CONTROL.with(Cell::get) {
        None => {}
        Some(Control::Owner) => rules.owner = false,
        Some(Control::AfterBirth) => rules.after_birth = false,
        Some(Control::Contested) => rules.contested = false,
        Some(Control::Fixed) => rules.fixed = false,
        Some(Control::Restored) => rules.restored = false,
        Some(Control::VerbFromAuthority) => rules.verb_every_version = false,
        Some(Control::AdmitAll) => {
            rules = Rules {
                owner: false,
                after_birth: false,
                contested: false,
                fixed: false,
                restored: false,
                verb: false,
                verb_every_version: false,
            };
        }
    }
    rules
}
