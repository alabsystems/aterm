// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Update handoffs land on item boundaries (init-on-by-default design §6.7).
//!
//! An item that cannot be repeated safely holds a boundary guard across its critical section:
//! the sign-out click sequence and a clone's publish rename (aterm-machines), a launchd
//! bootstrap of a vault job (aterm-secrets). The registry is `aterm_types::update_boundary`'s
//! process one, which those crates and this one all reach. A handoff latches the registry as it
//! starts ([`latch_handoff`], held for [`HANDOFF_PENDING_BOUND`] at most): from then until it
//! commits or is refused, no item enters its critical section, and an item that waited checks
//! again what it acts on before it enters. An item already in one refuses the handoff, as it
//! starts and again at its Commit, so the Commit is never written over an item in its section:
//! within [`MAX_BOUNDARY_WAIT`], one budget for the try across both, the refusal is activity and
//! the handoff is tried again; past it this try is refused, and the updater's next check tries
//! again with a new budget.

use std::time::Duration;

pub use aterm_types::update_boundary::{Boundary, GuardedItem, HandoffLatch, Held};

/// The longest an update's handoff stays in flight, which bounds how long an item waits on its
/// latch ([`latch_handoff`]): its successor held for a quiet moment (at most four minutes), then
/// its readiness and its proof (at most two minutes each). The latch is released as soon as the
/// handoff is refused or abandoned (`App::settle_update_boundary_latch`), so an item waits this
/// long only behind a handoff that never ends, and then the Commit reads its guard again.
pub const HANDOFF_PENDING_BOUND: Duration = Duration::from_secs(8 * 60);

/// The longest one try of an update's handoff follows items at a boundary, across its start and
/// its Commit. Past it the try is refused, and the updater's next check tries again.
pub const MAX_BOUNDARY_WAIT: Duration = Duration::from_secs(10);

/// The registry the handoff reads and latches. A running aterm has one, the process's, which
/// every item holds. A test of this crate reads a second: no port reaches the handoff to hand it a
/// private one, and every test shares the process's, where the native items run on every parallel
/// run. On it, another test's item would read as activity and a test's latch would refuse another
/// test's item, each at random. A test that makes an item and a handoff meet holds its guard here.
#[must_use]
pub fn registry() -> &'static Boundary {
    #[cfg(test)]
    {
        static UNDER_TEST: Boundary = Boundary::new();
        &UNDER_TEST
    }
    #[cfg(not(test))]
    {
        Boundary::process()
    }
}

/// [`registry`]'s [`Boundary::any_held`].
#[must_use]
pub fn any_held() -> Option<GuardedItem> {
    registry().any_held()
}

/// [`registry`]'s [`Boundary::is_held`].
#[must_use]
pub fn is_held(item: GuardedItem) -> bool {
    registry().is_held(item)
}

/// [`registry`]'s [`Boundary::latch_handoff`].
pub fn latch_handoff(bound: Duration) -> HandoffLatch<'static> {
    registry().latch_handoff(bound)
}

/// [`registry`]'s [`Boundary::handoff_pending`].
#[must_use]
pub fn handoff_pending() -> bool {
    registry().handoff_pending()
}

/// [`registry`]'s [`Boundary::waiting`].
#[must_use]
pub fn waiting() -> u32 {
    registry().waiting()
}

#[cfg(test)]
mod tests {
    use super::{Boundary, GuardedItem, is_held};

    #[test]
    fn an_item_on_the_process_registry_stays_out_of_a_tests_handoff() {
        let _held = Boundary::process()
            .try_hold(GuardedItem::HarnessNativeMutation)
            .expect("no test latches the process's registry");
        assert!(
            !is_held(GuardedItem::HarnessNativeMutation),
            "another test's handoff would read this guard as activity"
        );
    }
}
