// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(label: &str, email: &str) -> accounts::Row {
    accounts::Row {
        label: label.into(),
        email: Some(email.into()),
        ..Default::default()
    }
}

#[test]
fn one_batch_keeps_same_label_work_and_retries_in_their_actual_namespaces() {
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let a = accounts::Store::at(Path::new("/synthetic/activity-batch-a"));
    let b = accounts::Store::at(Path::new("/synthetic/activity-batch-b"));
    let unrelated = accounts::Store::at(Path::new("/synthetic/activity-batch-other"));
    let row_a = row("shared-label", "a@example.test");
    let row_b = row("shared-label", "b@example.test");
    let clock = Instant::now();
    {
        let mut held = guard.lock().unwrap();
        held.last_attempt.insert("a-claim".into(), clock);
        held.activity_owners.insert(
            "a-claim".into(),
            LoginOwner::new(&a, Cli::Claude, &row_a.label, 900).for_row(&row_a),
        );
    }
    let active = LoginActivityGuard::begin_row(&guard, &b, Cli::Claude, &row_b, Some("mailbox"));
    let began = guard
        .lock()
        .unwrap()
        .active_login
        .as_ref()
        .unwrap()
        .owner
        .observed_at;
    let batch = login_activity_batch_using(
        guard.as_ref(),
        [Some(&a), Some(&b)],
        Cli::Claude,
        1000,
        || clock,
    )
    .unwrap();
    let a_rows = batch.for_store(&a, Cli::Claude).unwrap();
    let b_rows = batch.for_store(&b, Cli::Claude).unwrap();
    assert_eq!(a_rows.len(), 1);
    assert_eq!(b_rows.len(), 1);
    assert!(!a_rows[0].active);
    assert_eq!(a_rows[0].next_at, Some(1900));
    assert!(a_rows[0].matches_row(&row_a));
    assert!(!a_rows[0].matches_row(&row_b));
    assert!(b_rows[0].active);
    assert_eq!(b_rows[0].route.as_deref(), Some("mailbox"));
    assert_eq!(
        b_rows[0].observed_at, began,
        "the read does not timestamp new work"
    );
    assert_eq!(b_rows[0].next_at, None);
    assert!(b_rows[0].matches_row(&row_b));
    assert!(!b_rows[0].matches_row(&row_a));
    assert!(batch.for_store(&unrelated, Cli::Claude).is_none());
    assert!(batch.for_store(&a, Cli::Codex).is_none());
    drop(active);
    let ended = login_activity_batch_using(
        guard.as_ref(),
        [Some(&a), Some(&b)],
        Cli::Claude,
        1000,
        || clock,
    )
    .unwrap();
    assert!(ended.for_store(&b, Cli::Claude).unwrap().is_empty());
    assert_eq!(ended.for_store(&a, Cli::Claude), Some(a_rows));
    assert!(
        batch.for_store(&b, Cli::Claude).unwrap()[0].active,
        "the batch owns its captured observation"
    );
}

#[test]
fn even_identical_rows_cannot_carry_activity_into_another_store_or_cli() {
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let a = accounts::Store::at(Path::new("/synthetic/activity-batch-a"));
    let b = accounts::Store::at(Path::new("/synthetic/activity-batch-b"));
    let same_row = row("shared-label", "same@example.test");
    let _active = LoginActivityGuard::begin_row(&guard, &a, Cli::Claude, &same_row, None);
    let held = guard.lock().unwrap();
    let batch = login_activity_batch_of(
        &held,
        [Some(&a), Some(&b)],
        Cli::Claude,
        1000,
        Instant::now(),
    );
    assert!(batch.for_store(&a, Cli::Claude).unwrap()[0].matches_row(&same_row));
    assert!(batch.for_store(&b, Cli::Claude).unwrap().is_empty());
    let other_cli = login_activity_batch_of(
        &held,
        [Some(&a), Some(&b)],
        Cli::Codex,
        1000,
        Instant::now(),
    );
    assert!(other_cli.for_store(&a, Cli::Codex).unwrap().is_empty());
    assert!(other_cli.for_store(&a, Cli::Claude).is_none());
}

#[test]
fn a_busy_batch_is_unavailable_as_a_whole_and_both_scopes_share_one_clock() {
    let guard = std::sync::Mutex::new(AutoRenewGuard::default());
    let a = accounts::Store::at(Path::new("/synthetic/activity-batch-a"));
    let b = accounts::Store::at(Path::new("/synthetic/activity-batch-b"));
    let clock = Instant::now();
    let mut held = guard.lock().unwrap();
    for (key, store) in [("a-claim", &a), ("b-claim", &b)] {
        held.last_attempt.insert(key.into(), clock);
        held.activity_owners
            .insert(key.into(), LoginOwner::new(store, Cli::Claude, key, 900));
    }
    assert!(
        login_activity_batch_using(&guard, [Some(&a), Some(&b)], Cli::Claude, 1000, || {
            panic!("a busy read must not proceed to sample or build either observation")
        })
        .is_none()
    );
    drop(held);
    let mut samples = 0;
    let batch = login_activity_batch_using(&guard, [Some(&a), Some(&b)], Cli::Claude, 1000, || {
        samples += 1;
        clock
    })
    .unwrap();
    assert_eq!(samples, 1);
    for store in [&a, &b] {
        let rows = batch.for_store(store, Cli::Claude).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].next_at, Some(1900));
    }
    assert_eq!(guard.lock().unwrap().last_attempt.len(), 2);
    assert_eq!(guard.lock().unwrap().activity_owners.len(), 2);
}

#[test]
fn duplicate_namespace_requests_are_one_observation_and_fixture_scopes_stay_exact() {
    let guard = AutoRenewGuard::default();
    let a = accounts::Store::at(Path::new("/synthetic/activity-batch-a"));
    let b = accounts::Store::at(Path::new("/synthetic/activity-batch-b"));
    for stores in [[Some(&a), Some(&a)], [None, Some(&a)]] {
        let batch = login_activity_batch_of(&guard, stores, Cli::Claude, 1000, Instant::now());
        assert_eq!(batch.scopes.iter().flatten().count(), 1);
        assert_eq!(batch.for_store(&a, Cli::Claude), Some([].as_slice()));
        assert!(batch.for_store(&b, Cli::Claude).is_none());
    }
    let current = row("shared-label", "same@example.test");
    let entry = LoginActivity {
        label: current.label.clone(),
        route: Some("browser".into()),
        active: true,
        observed_at: 700,
        next_at: None,
        diagnostic: None,
        identity: Some(LoginIdentity::of(&current)),
        queued: None,
    };
    let fixture = LoginActivityBatch::for_test(
        [Some((&a, Vec::new())), Some((&b, vec![entry.clone()]))],
        Cli::Claude,
    );
    assert!(fixture.for_store(&a, Cli::Claude).unwrap().is_empty());
    assert_eq!(
        fixture.for_store(&b, Cli::Claude).unwrap(),
        std::slice::from_ref(&entry)
    );
    assert!(fixture.for_store(&b, Cli::Kimi).is_none());
    let same = LoginActivityBatch::for_test(
        [Some((&b, vec![entry.clone()])), Some((&b, vec![entry]))],
        Cli::Claude,
    );
    assert_eq!(same.scopes.iter().flatten().count(), 1);
}
