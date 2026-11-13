// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn maintenance_uses_a_new_operation_only_after_retirement_and_real_app_resume() {
    let app = Lifetime::default();
    let foreign = Lifetime::default();
    let original = app.enter().unwrap();
    let original_id = original.operation_identity();
    let original_status = original.cancellation();
    let owner = original.maintenance_owner();
    assert!(owner.enter().is_none());
    original_status.cancel();
    assert!(owner.enter().is_none());
    drop(original);
    assert!(original_status.retired());
    let pause = app.pause().unwrap();
    assert!(owner.enter().is_none());
    let drained = pause.try_wait().unwrap();
    let foreign_scope = foreign.enter().unwrap();
    assert!(
        owner.enter().is_none(),
        "a foreign open App cannot supply admission"
    );
    drop(foreign_scope);
    assert!(foreign.pause().unwrap().try_wait().is_ok());
    assert!(
        owner.enter().is_none(),
        "sealed admission must remain closed"
    );
    drained.resume().unwrap();
    let maintenance = owner.enter().unwrap();
    assert_ne!(maintenance.operation_identity(), original_id);
    assert!(maintenance.cancellation().owned_by(&app));
    assert!(!maintenance.cancellation().owned_by(&foreign));
    assert!(original_status.cancelled() && original_status.retired());
    let pause = app.pause().unwrap();
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("new maintenance must be actually counted"),
    };
    drop(maintenance);
    assert!(pause.try_wait().is_ok(), "the new operation really retired");
    assert!(owner.enter().is_none());
}

#[test]
fn weak_discovery_admission_is_counted_and_never_borrows_a_foreign_app() {
    let app = Lifetime::default();
    let foreign = Lifetime::default();
    let admission = app.admission_owner();
    let original = admission.enter().unwrap();
    assert!(original.cancellation().owned_by(&app));
    assert!(!original.cancellation().owned_by(&foreign));
    let original_id = original.operation_identity();
    let pause = app.pause().unwrap();
    assert!(admission.enter().is_none());
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("discovery is counted before credential work"),
    };
    let foreign_scope = foreign.enter().unwrap();
    assert!(admission.enter().is_none());
    drop(foreign_scope);
    drop(original);
    let drained = pause.try_wait().unwrap();
    assert!(admission.enter().is_none());
    drained.resume().unwrap();
    let next = admission.enter().unwrap();
    assert_ne!(next.operation_identity(), original_id);
    drop(next);
    drop(app);
    assert!(
        admission.enter().is_none(),
        "foreign App cannot revive a dead owner"
    );
}

#[test]
fn weak_maintenance_locator_does_not_keep_an_app_or_original_scope_alive() {
    let app = Lifetime::default();
    let original = app.enter().unwrap();
    let status = original.cancellation();
    let owner = original.maintenance_owner();
    drop(original);
    assert!(status.retired());
    drop(app);
    assert!(owner.enter().is_none());
}
