// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::SessionFabric;

fn reversed(fabric: &SessionFabric) -> Option<String> {
    let caught =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fabric.hold_observation()));
    let payload = caught.err()?;
    Some(
        payload
            .downcast_ref::<&str>()
            .map(|said| (*said).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default(),
    )
}

#[test]
fn an_inbox_read_beneath_held_metadata_panics_before_it_can_block() {
    let fabric = SessionFabric::default();
    let meta = MetaLock::new(0_u8);
    let guard = meta.lock().unwrap();
    let said = reversed(&fabric).expect("an Inbox taken beneath metadata is the reversed order");
    assert!(said.contains("lock order"), "{said}");
    drop(guard);
    // Another session's metadata ranks the same: the order is class-wide.
    let other = MetaLock::new(1_u8);
    let guard = other.try_lock().unwrap();
    assert!(
        reversed(&fabric).is_some(),
        "a try_lock guard is held metadata too"
    );
    drop(guard);
}

#[test]
fn the_inbox_then_metadata_order_and_a_released_guard_pass() {
    let fabric = SessionFabric::default();
    let meta = MetaLock::new(0_u8);
    assert!(!fabric.hold_observation().is_held());
    *meta.lock().unwrap() += 1;
    // A task mark's shape: metadata taken while the Inbox is held.
    let marked = fabric.run_unfenced(aterm_spec::wait::MUST_HAPPEN, |_| {
        *meta.lock().unwrap() += 1;
    });
    assert!(marked.is_some());
    // A guard dropped, poisoned or not, no longer counts.
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = meta.lock().unwrap();
        panic!("a holder panics");
    }));
    assert!(poisoned.is_err());
    let recovered = meta.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(*recovered, 2);
    drop(recovered);
    assert!(reversed(&fabric).is_none());
}
