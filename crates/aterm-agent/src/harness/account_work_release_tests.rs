// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn paused_release_lifetime_is_distinct_counted_and_sealed_closed() {
    let lifetime = Lifetime::default();
    let pause = lifetime.pause().unwrap();
    assert!(
        lifetime.enter().is_none(),
        "ordinary Scope admission remains closed"
    );
    let (operation, settlement) = lifetime.reserve_release_operation().unwrap();
    let monitor = operation.monitor();
    let actual_resources = operation.clone();
    assert!(monitor.owned_by(&lifetime));
    assert!(!monitor.owned_by(&Lifetime::default()));
    assert_eq!(
        settlement.operation_identity(),
        Some(monitor.operation_identity())
    );
    drop(operation);
    assert!(!monitor.retired(), "actual release resources remain live");
    drop(actual_resources);
    assert!(
        monitor.retired(),
        "weak monitor must not retain the release operation"
    );
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("settlement must remain counted"),
    };
    drop(settlement);
    let drained = pause
        .try_wait()
        .unwrap_or_else(|_| panic!("real release+settlement drop must drain"));
    assert!(
        lifetime.reserve_release_operation().is_none(),
        "no reservation after seal"
    );
    drained
        .resume()
        .unwrap_or_else(|_| panic!("matching epoch must resume"));
    let work = lifetime.enter().unwrap();
    assert!(
        !monitor.operation_identity().matches_scope(&work),
        "release identity is not a fresh work Scope"
    );
}
