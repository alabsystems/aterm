// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{BOUNDARY_WAIT_SINCE, MAX_BOUNDARY_WAIT, boundary_wait_decision, end_boundary_wait};
use crate::app_update_handoff_boundary::GuardedItem;
use std::time::{Duration, Instant};

/// While a non-idempotent item holds the guard, the handoff is refused, never cutting it in
/// half: as activity within the budget, and for this try at it, which ends the try's wait.
#[test]
fn the_handoff_follows_a_held_item_for_at_most_the_budget() {
    end_boundary_wait();
    let start = Instant::now();
    let item = Some(GuardedItem::ClonePublishRename);
    // The item holds the guard; the handoff follows it at first and as the wait grows, up to
    // just under the budget.
    let first = boundary_wait_decision(item, start).expect("held");
    assert!(first.contains("follows it"), "{first}");
    let near = start + MAX_BOUNDARY_WAIT - Duration::from_millis(1);
    assert!(boundary_wait_decision(item, near).is_some_and(|why| why.contains("follows it")));
    // At the budget this try is refused, even though nothing was cut, and its wait ends.
    let at = start + MAX_BOUNDARY_WAIT;
    let spent = boundary_wait_decision(item, at).expect("never proceeds over a held item");
    assert!(spent.contains("next check"), "{spent}");
    assert_eq!(BOUNDARY_WAIT_SINCE.with(std::cell::Cell::get), None);
    end_boundary_wait();
}

/// The control: no item holds the guard, so the handoff is never held and nothing waits.
#[test]
fn a_free_guard_holds_nothing() {
    end_boundary_wait();
    assert!(boundary_wait_decision(None, Instant::now()).is_none());
    assert_eq!(BOUNDARY_WAIT_SINCE.with(std::cell::Cell::get), None);
}

/// One budget for the whole try: an item the start waited on, then another that took the
/// guard after the start, hold the handoff at most [`MAX_BOUNDARY_WAIT`] together, across the
/// start and the Commit. A free reading between them keeps the try's wait; past the budget the
/// Commit is refused for this try, never given a fresh 10 s, and the next check starts afresh.
/// Before, the start that reached the budget let the handoff go and the Commit began a new
/// budget, so while the item stayed in its section the Commit was never written and the
/// design's "at most 10 s across both" did not hold.
#[test]
fn one_item_holds_the_handoff_at_most_the_budget_across_start_and_commit() {
    end_boundary_wait();
    let t0 = Instant::now();
    let item = Some(GuardedItem::SignOutSequence);
    assert!(
        boundary_wait_decision(item, t0).is_some(),
        "the start follows the item in its section"
    );
    // The item lets go: the start goes on, and the try's wait stands.
    assert!(boundary_wait_decision(None, t0 + Duration::from_secs(4)).is_none());
    // Another item takes the guard after the start: the Commit follows it within what is
    // left of the one budget...
    let commit = boundary_wait_decision(item, t0 + Duration::from_secs(8));
    assert!(
        commit
            .as_deref()
            .is_some_and(|why| why.contains("follows it")),
        "{commit:?}"
    );
    // ...and past it is refused for this try, not held for a fresh budget.
    let commit = boundary_wait_decision(item, t0 + MAX_BOUNDARY_WAIT + Duration::from_secs(2));
    assert!(
        commit
            .as_deref()
            .is_some_and(|why| why.contains("next check")),
        "the Commit held a fresh budget for an item the try already waited the whole budget \
         on: {commit:?}"
    );
    // The updater's next check is a new try, with a new budget.
    let next = t0 + MAX_BOUNDARY_WAIT + Duration::from_secs(30);
    assert!(boundary_wait_decision(item, next).is_some_and(|why| why.contains("follows it")));
    end_boundary_wait();
}
