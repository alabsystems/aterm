// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The record is appended; no marker says so.
    AfterSign,
    /// The marker is written; the intent is still in the outbox.
    AfterMarker,
}

thread_local! {
    static STOP: Cell<Option<Stop>> = const { Cell::new(None) };
    static BLIND: Cell<bool> = const { Cell::new(false) };
    static BEFORE_CLAIM: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `act` once on this thread, in the next pass, after it read the waiting intents and
/// before it claims the next one: where a person's drop meets a pass already under way.
pub(crate) fn before_next_claim(act: impl FnOnce() + 'static) {
    BEFORE_CLAIM.with(|slot| *slot.borrow_mut() = Some(Box::new(act)));
}

pub(crate) fn before_claim() {
    if let Some(act) = BEFORE_CLAIM.with(|slot| slot.borrow_mut().take()) {
        act();
    }
}

/// Kill the next pass on this thread at `stop`, once.
pub(crate) fn kill_at(stop: Stop) {
    STOP.with(|slot| slot.set(Some(stop)));
}

pub(crate) fn stops(stop: Stop) -> bool {
    STOP.with(|slot| {
        if slot.get() == Some(stop) {
            slot.set(None);
            true
        } else {
            false
        }
    })
}

/// While the guard lives, the publisher on this thread writes every edit whatever its
/// register holds.
pub(crate) struct Blind;

pub(crate) fn ignore_expected() -> Blind {
    BLIND.with(|slot| slot.set(true));
    Blind
}

impl Drop for Blind {
    fn drop(&mut self) {
        BLIND.with(|slot| slot.set(false));
    }
}

pub(crate) fn ignores_expected() -> bool {
    BLIND.with(Cell::get)
}
