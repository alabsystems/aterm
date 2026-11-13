// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn weak_identity_is_exact_and_does_not_retain_the_operation() {
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let other = lifetime.enter().unwrap();
    let identity = scope.operation_identity();
    assert!(identity.matches_scope(&scope.clone()));
    assert!(!identity.matches_scope(&other));
    assert!(
        lifetime
            .release_turn()
            .unwrap()
            .operation_identity()
            .is_none()
    );
    let release = scope.release_turn().unwrap();
    assert_eq!(release.operation_identity(), Some(&identity));
    let cancellation = scope.cancellation();
    let pause = lifetime.pause().unwrap();
    drop(scope);
    assert!(
        cancellation.retired(),
        "weak identity must not keep a Scope alive"
    );
    drop(other);
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("release remains counted"),
    };
    drop(release);
    assert!(
        pause.try_wait().is_ok(),
        "weak identity must not keep the app count alive"
    );
}

#[test]
fn cancelled_scope_release_counts_cleanup_until_actual_drop() {
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let pause = lifetime.pause().unwrap();
    assert!(scope.cancelled());
    let release = scope.release_turn().unwrap();
    drop(scope);
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("release-only custody must remain counted"),
    };
    drop(release);
    let drained = pause
        .try_wait()
        .unwrap_or_else(|_| panic!("actual cleanup drop must drain"));
    assert!(
        lifetime.release_turn().is_none(),
        "sealed admission cannot mint cleanup"
    );
    drained
        .resume()
        .unwrap_or_else(|_| panic!("the exact drained epoch must resume"));
}
