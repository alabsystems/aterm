// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn dead_constructor_metadata_does_not_own_its_own_retirement() {
    // Count/ownership control only. No process-death, source, installed
    // target, kernel exclusion or credential admission is fabricated.
    let app = crate::harness::account_work::Lifetime::default();
    let scope = app.enter().unwrap();
    let registry = Registry::for_app(&app);
    let connection = registry.fixture_connection();
    let work = Arc::new(Constructor {
        member: "a".repeat(32),
        operation: "b".repeat(32),
        scope: scope.clone(),
        worker: false.into(),
    });
    let captured = work.clone();
    let entries = [(
        "b".repeat(32),
        ConstructorEntry {
            work,
            connection: connection.clone(),
            preload: Some("c".repeat(32)),
            ready: None,
            no_reader: None,
        },
    )]
    .into();
    drop(scope);
    let mut witnesses = Vec::new();
    let retired = retire_constructor_entries(entries, &mut witnesses);
    assert!(captured.scope.cancelled());
    assert!(!witnesses[0].retired());
    let pause = app.pause().unwrap();
    drop(retired);
    let pause = pause
        .try_wait()
        .expect_err("actual captured constructor still owns its Scope");
    assert!(!witnesses[0].retired());
    drop(captured);
    assert!(witnesses[0].retired());
    assert!(pause.try_wait().is_ok());
}
