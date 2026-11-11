// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Condvar, Mutex, PoisonError};

/// How many [`Hold`]s are alive. A count, not a flag: tests of this binary hold at once, and
/// one test's drop must not release a build another test still holds.
static HELD: Mutex<usize> = Mutex::new(0);
static RELEASED: Condvar = Condvar::new();

/// The hold, released once it and every other hold drop.
pub(crate) struct Hold(());

/// Hold every build until every [`Hold`] drops.
pub(crate) fn hold() -> Hold {
    *HELD.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    Hold(())
}

impl Drop for Hold {
    fn drop(&mut self) {
        *HELD.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        RELEASED.notify_all();
    }
}

/// Whether any [`Hold`] is alive, which keeps a build waiting.
fn held() -> bool {
    *HELD.lock().unwrap_or_else(PoisonError::into_inner) > 0
}

/// Wait while a build is held.
pub(super) fn wait() {
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    while *held > 0 {
        held = RELEASED.wait(held).unwrap_or_else(PoisonError::into_inner);
    }
}

/// Tests of one binary hold at once, each dropping its own hold as it ends: one drop must
/// leave the build another test still holds waiting. A hold that is a flag, not a count,
/// fails here.
#[test]
fn a_hold_dropped_beside_another_keeps_the_other_holding() {
    let first = hold();
    let second = hold();
    drop(first);
    assert!(
        held(),
        "one hold's drop released the build another hold still keeps"
    );
    drop(second);
}
