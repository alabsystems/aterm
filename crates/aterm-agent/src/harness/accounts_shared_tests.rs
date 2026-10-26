// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::{Logins, advance_renewal_generations};
use super::*;

fn intent(generation: u64) -> Intent {
    Intent {
        source: Source {
            network: [1; 32],
            account: [2; 32],
        },
        decision: [generation as u8; 32],
        generation,
        configuration: [3; 32],
        request: [4; 16],
        label: "m3.2".into(),
        identity: " Owner@Example.TEST ".into(),
        staging: std::env::temp_dir()
            .join("aterm-shared-login-fixture")
            .to_str()
            .unwrap()
            .into(),
        before_generation: Some(10),
        begun_at: 1_700_000_000,
    }
}

#[test]
fn a_durable_begin_consumes_the_grant_even_after_crash_or_with_a_new_nonce() {
    let mut state = State::default();
    let first = intent(1);
    let token = state.begin(first.clone(), None).unwrap();
    let mut restarted = State::from_json(&state.to_json()).unwrap();
    let mut retry = first.clone();
    retry.request = [5; 16];
    assert!(restarted.begin(retry, None).is_err());
    assert_eq!(
        restarted.latest(first.source).unwrap().outcome,
        Outcome::Pending
    );
    restarted.begin(intent(2), None).unwrap();
    assert!(
        restarted
            .finish(
                token,
                Outcome::Published { generation: 11 },
                Some(("owner@example.test", [8; 32])),
                1_700_000_001
            )
            .is_err()
    );
    assert!(restarted.extends(&state));
    assert!(!state.extends(&restarted));
}

#[test]
fn completion_pins_full_identity_and_never_accepts_a_different_principal_or_email() {
    let mut state = State::default();
    let first = intent(1);
    let token = state.begin(first.clone(), None).unwrap();
    state
        .finish(
            token,
            Outcome::Published { generation: 11 },
            Some(("OWNER@example.test", [8; 32])),
            1_700_000_001,
        )
        .unwrap();
    let row = Row {
        label: "new-label".into(),
        email: Some("owner@EXAMPLE.TEST".into()),
        ..Row::default()
    };
    assert_eq!(state.required(&row).unwrap(), Some(first.source));
    assert_eq!(state.principal_source([8; 32]), Some(first.source));
    let prior = state.clone();
    let token = state.begin(intent(2), None).unwrap();
    assert!(
        state
            .finish(
                token,
                Outcome::Published { generation: 12 },
                Some(("owner@example.test", [9; 32])),
                1_700_000_002
            )
            .is_err()
    );
    assert_eq!(
        state.latest(first.source).unwrap().outcome,
        Outcome::Pending
    );
    assert!(state.extends(&prior));
    let token = state.begin(intent(3), None).unwrap();
    assert!(
        state
            .finish(
                token,
                Outcome::Published { generation: 12 },
                Some(("another@example.test", [8; 32])),
                1_700_000_003
            )
            .is_err()
    );
}

#[test]
fn pin_history_has_hard_limits_and_metadata_transactions_cannot_erase_it() {
    let mut state = State::default();
    for n in 1..=MAX_NAMES {
        let mut next = intent(n as u64);
        next.label = format!("label{n}");
        state.begin(next, None).unwrap();
    }
    let before = state.clone();
    let mut overflow = intent(MAX_NAMES as u64 + 1);
    overflow.label = "overflow".into();
    assert!(state.begin(overflow, None).is_err());
    assert_eq!(state, before);
    let prior = Logins {
        shared: state,
        ..Logins::default()
    };
    let mut after = prior.clone();
    after.shared = State::default();
    assert!(advance_renewal_generations(&prior, &mut after, &[]).is_err());
    let unrelated = Row {
        label: "personal".into(),
        email: Some("personal@example.test".into()),
        ..Row::default()
    };
    assert_eq!(prior.shared.required(&unrelated).unwrap(), None);
}

#[test]
fn shared_schema_is_explicit_and_cannot_disappear_under_a_legacy_tag() {
    let local = Logins::default();
    assert_eq!(
        aterm_json::from_str::<Value>(&local.to_json()).unwrap()["schema"].as_u64(),
        Some(3)
    );
    let mut shared = local;
    shared.generation_clock = 10;
    shared.shared.begin(intent(1), None).unwrap();
    let text = shared.to_json();
    let mut wire: Value = aterm_json::from_str(&text).unwrap();
    assert_eq!(wire["schema"].as_u64(), Some(4));
    assert_eq!(Logins::from_json(&text, 1_700_000_000).unwrap(), shared);
    wire.as_object_mut()
        .unwrap()
        .insert("schema".into(), json!(3));
    assert!(Logins::from_json(&aterm_json::to_string(&wire).unwrap(), 1_700_000_000).is_err());
    wire.as_object_mut()
        .unwrap()
        .insert("schema".into(), json!(4));
    wire.as_object_mut().unwrap().remove("shared");
    assert!(Logins::from_json(&aterm_json::to_string(&wire).unwrap(), 1_700_000_000).is_err());
}

#[test]
fn beginning_shared_preparation_fences_old_unpublished_login_checkpoints() {
    let before = Logins::default();
    let parent = super::super::RenewalParent::capture(&before, "m3.2");
    let mut after = before.clone();
    after.shared.begin(intent(1), None).unwrap();
    advance_renewal_generations(&before, &mut after, &[]).unwrap();
    assert!(!parent.matches(&after, "m3.2"));
    assert_eq!(after.generation_clock, 1);
    assert_eq!(
        after
            .shared
            .latest(intent(1).source)
            .unwrap()
            .intent
            .before_generation,
        Some(1)
    );
}

#[test]
fn fencing_keeps_removed_labels_absent_and_refuses_an_overfull_binding_table() {
    let before = Logins::default();
    let mut first = before.clone();
    first.shared.begin(intent(1), None).unwrap();
    advance_renewal_generations(&before, &mut first, &[]).unwrap();
    let mut removed = first.clone();
    removed.labels.remove("m3.2");
    advance_renewal_generations(&first, &mut removed, &[]).unwrap();
    let mut next = removed.clone();
    let mut wanted = intent(2);
    wanted.label = "new-label".into();
    next.shared.begin(wanted, None).unwrap();
    advance_renewal_generations(&removed, &mut next, &[]).unwrap();
    assert!(!next.labels.contains_key("m3.2"));
    assert!(next.labels.contains_key("new-label"));
    let mut full = Logins::default();
    for n in 0..super::super::MAX_ACCOUNTS {
        full.labels
            .insert(format!("full-{n}"), super::super::Binding::default());
    }
    let mut after = full.clone();
    after.shared.begin(intent(1), None).unwrap();
    assert!(advance_renewal_generations(&full, &mut after, &[]).is_err());
}

#[test]
fn a_different_shared_source_cannot_reuse_a_retained_email_or_provider_identity() {
    let mut state = State::default();
    let token = state.begin(intent(1), None).unwrap();
    state
        .finish(
            token,
            Outcome::Published { generation: 11 },
            Some(("owner@example.test", [8; 32])),
            1_700_000_001,
        )
        .unwrap();
    let mut another = intent(1);
    another.source.account = [9; 32];
    another.label = "another".into();
    assert!(state.begin(another.clone(), None).is_err());
    another.identity = "another@example.test".into();
    let token = state.begin(another, None).unwrap();
    assert!(
        state
            .finish(
                token,
                Outcome::Published { generation: 12 },
                Some(("another@example.test", [8; 32])),
                1_700_000_002
            )
            .is_err()
    );
    assert_eq!(state.principal_source([8; 32]), Some(intent(1).source));
}

#[test]
fn approved_principal_survives_failure_restart_and_an_unconstrained_successor() {
    let mut state = State::default();
    let source = intent(1).source;
    let token = state.begin(intent(1), Some([8; 32])).unwrap();
    assert_eq!(state.principal_source([8; 32]), Some(source));
    assert_eq!(state.latest(source).unwrap().outcome, Outcome::Pending);
    let pending = state.clone();
    let mut state = State::from_json(&state.to_json()).unwrap();
    state
        .finish(token, Outcome::Unknown, None, 1_700_000_001)
        .unwrap();
    assert!(state.extends(&pending));
    let mut state = State::from_json(&state.to_json()).unwrap();
    assert_eq!(state.principal_source([8; 32]), Some(source));
    let before = state.clone();
    assert!(state.begin(intent(2), Some([9; 32])).is_err());
    assert_eq!(state, before);
    state.begin(intent(2), None).unwrap();
    assert_eq!(state.principal_source([8; 32]), Some(source));
    assert!(state.extends(&before));
}

#[test]
fn conflicting_approved_principals_are_rejected_before_consuming_a_decision() {
    let mut empty = State::default();
    assert!(empty.begin(intent(1), Some([0; 32])).is_err());
    assert!(empty.is_empty());
    for outcome in [
        Outcome::Pending,
        Outcome::Unknown,
        Outcome::Published { generation: 11 },
    ] {
        let mut state = State::default();
        let token = state.begin(intent(1), Some([8; 32])).unwrap();
        if outcome != Outcome::Pending {
            state
                .finish(
                    token,
                    outcome,
                    Some(("owner@example.test", [8; 32])),
                    1_700_000_001,
                )
                .unwrap();
        }
        let before = state.clone();
        let mut another = intent(1);
        another.source.account = [9; 32];
        another.label = "another".into();
        another.identity = "another@example.test".into();
        assert!(state.begin(another.clone(), Some([8; 32])).is_err());
        assert_eq!(state, before);
        state.begin(another.clone(), None).unwrap();
        assert_eq!(state.principal_source([9; 32]), None);
        another.generation = 2;
        another.decision = [2; 32];
        state.begin(another.clone(), Some([9; 32])).unwrap();
        assert_eq!(state.principal_source([9; 32]), Some(another.source));
        assert_eq!(State::from_json(&state.to_json()).unwrap(), state);
    }
}
