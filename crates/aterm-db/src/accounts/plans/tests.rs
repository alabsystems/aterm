// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    database::{Candidate, Cell},
    table::Scope,
};
use aterm_secrets::commons::journal::Trust;
use std::collections::BTreeMap;

fn put(snapshot: &mut Snapshot, collection: &str, key: &str, value: Value) {
    snapshot.cells.insert(
        (collection.into(), key.into()),
        Cell {
            stopped: false,
            candidates: vec![Candidate {
                operation: aterm_digest::Sha256::digest(format!("{collection}/{key}").as_bytes()),
                value,
            }],
        },
    );
}

fn fixture() -> Snapshot {
    let mut snapshot = Snapshot {
        scope: Scope::new(
            Trust {
                network: [1; 16],
                authority: [2; 32],
                genesis: [3; 32],
            },
            [4; 16],
            1,
        )
        .unwrap(),
        retained: 0,
        withheld: 0,
        cells: BTreeMap::new(),
        operations: vec![],
    };
    put(
        &mut snapshot,
        "accounts",
        "one",
        json!({"provider":"example", "method":"email-link",
        "label":"m3.2", "auth":[{"kind":"email-link","identity":"alias"}]}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "alias",
        json!({"address":"alias@example.test","alias_of":"primary"}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "primary",
        json!({"address":"primary@example.test","mailbox_id":"inbox"}),
    );
    put(
        &mut snapshot,
        "mailboxes",
        "inbox",
        json!({"name":"inbox", "secret_ref":{"vault":"private","entry":"mail-password"},
        "auth":[{"kind":"phone","identity":"sms"}]}),
    );
    put(
        &mut snapshot,
        "phones",
        "sms",
        json!({"number":"+12025550123"}),
    );
    snapshot
}

fn value<'a>(snapshot: &'a mut Snapshot, collection: &str, key: &str) -> &'a mut Value {
    &mut snapshot
        .cells
        .get_mut(&(collection.into(), key.into()))
        .unwrap()
        .candidates[0]
        .value
}

fn set(snapshot: &mut Snapshot, collection: &str, key: &str, field: &str, item: Value) {
    value(snapshot, collection, key)
        .as_object_mut()
        .unwrap()
        .insert(field.into(), item);
}

#[test]
fn follows_mailbox_auth_and_alias_dependencies_without_disclosing_values() {
    let snapshot = fixture();
    let report = inspect(&snapshot, "one").unwrap();
    assert_eq!(report["complete"].as_bool(), Some(true));
    let dependencies: BTreeSet<_> = report["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["collection"].as_str().unwrap(),
                row["key"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        dependencies,
        BTreeSet::from([
            ("accounts", "one"),
            ("email-addresses", "alias"),
            ("email-addresses", "primary"),
            ("mailboxes", "inbox"),
            ("phones", "sms")
        ])
    );
    let encoded = aterm_json::to_string(&report).unwrap();
    for secret in [
        "primary@example.test",
        "alias@example.test",
        "+12025550123",
        "mail-password",
        "private",
        "m3.2",
    ] {
        assert!(!encoded.contains(secret));
    }
    assert_eq!(report["authority"].as_str(), Some("none"));
}

#[test]
fn a_reference_in_a_contact_detail_joins_the_transitive_closure() {
    let mut snapshot = fixture();
    set(
        &mut snapshot,
        "phones",
        "sms",
        "details",
        json!({"backup":{"kind":"email-ref","value":"backup"}}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "backup",
        json!({"address":"backup@example.test"}),
    );
    let before = inspect(&snapshot, "one").unwrap();
    assert_eq!(before["complete"].as_bool(), Some(true));
    set(
        &mut snapshot,
        "email-addresses",
        "backup",
        "address",
        json!("new@example.test"),
    );
    assert_ne!(
        before["configuration_digest"],
        inspect(&snapshot, "one").unwrap()["configuration_digest"]
    );
    snapshot
        .cells
        .remove(&("email-addresses".into(), "backup".into()));
    assert_eq!(
        inspect(&snapshot, "one").unwrap()["complete"].as_bool(),
        Some(false)
    );
}

#[test]
fn conflicted_evidence_is_sorted_bounded_and_explicit_about_truncation() {
    let mut snapshot = fixture();
    let key = ("phones".into(), "sms".into());
    let cell = snapshot.cells.get_mut(&key).unwrap();
    for id in 1..=64 {
        cell.candidates.push(Candidate {
            operation: [id; 32],
            value: Value::Null,
        });
    }
    let report = inspect(&snapshot, "one").unwrap();
    snapshot.cells.get_mut(&key).unwrap().candidates.reverse();
    assert_eq!(report, inspect(&snapshot, "one").unwrap());
    let evidence = &report["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["collection"].as_str() == Some("phones") && row["key"].as_str() == Some("sms")
        })
        .unwrap()["evidence"];
    assert_eq!(evidence["operations"].as_array().unwrap().len(), 32);
    assert_eq!(evidence["candidate_count"].as_u64(), Some(65));
    assert_eq!(evidence["truncated"].as_bool(), Some(true));
    assert!(report["configuration_digest"].is_null());
}

#[test]
fn missing_root_tombstones_invalid_values_and_nested_conflicts_withhold_both_digests() {
    for case in 0..7 {
        let mut snapshot = fixture();
        match case {
            0 => {
                snapshot.cells.remove(&("accounts".into(), "one".into()));
            }
            1 => {
                snapshot
                    .cells
                    .get_mut(&("phones".into(), "sms".into()))
                    .unwrap()
                    .stopped = true;
            }
            2 => {
                *value(&mut snapshot, "phones", "sms") = Value::Null;
            }
            3 => {
                *value(&mut snapshot, "mailboxes", "inbox") =
                    json!({"password":"never-print-this"});
            }
            4 => {
                snapshot
                    .cells
                    .get_mut(&("phones".into(), "sms".into()))
                    .unwrap()
                    .candidates
                    .push(Candidate {
                        operation: [99; 32],
                        value: json!({"number":"+12025550124"}),
                    });
            }
            5 => {
                set(
                    &mut snapshot,
                    "mailboxes",
                    "inbox",
                    "auth",
                    json!([{"kind":"phone","identity":"missing"}]),
                );
            }
            _ => {
                put(
                    &mut snapshot,
                    "email-addresses",
                    "duplicate",
                    json!({"address":"primary@example.test"}),
                );
            }
        }
        let report = inspect(&snapshot, "one").unwrap();
        assert_eq!(report["complete"].as_bool(), Some(false), "case {case}");
        assert!(report["proposal_digest"].is_null());
        assert!(report["configuration_digest"].is_null());
        assert!(
            !aterm_json::to_string(&report)
                .unwrap()
                .contains("never-print-this")
        );
    }
    assert!(inspect(&fixture(), "../elsewhere").is_err());
}

#[test]
fn bounded_payload_and_dependency_walk_never_return_partial_fingerprints() {
    let mut snapshot = fixture();
    let mut previous = "sms".to_string();
    for index in 0..MAX_OBJECTS {
        let next = format!("p{index}");
        set(
            &mut snapshot,
            "phones",
            &previous,
            "details",
            json!({"next":{"kind":"phone-ref","value":next}}),
        );
        put(&mut snapshot, "phones", &next, json!({"label":"next"}));
        previous = next;
    }
    let report = inspect(&snapshot, "one").unwrap();
    assert_eq!(report["complete"].as_bool(), Some(false));
    assert!(
        report["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("dependency-limit"))
    );
    assert!(report["dependencies"].as_array().unwrap().len() <= MAX_OBJECTS);

    let mut snapshot = fixture();
    let mut previous = "sms".to_string();
    for index in 0..12 {
        let next = format!("p{index}");
        let mut details: aterm_json::Map = (0..12)
            .map(|i| {
                (
                    format!("n{i}"),
                    json!({"kind":"note","value":"x".repeat(2048)}),
                )
            })
            .collect();
        details.insert("next".into(), json!({"kind":"phone-ref","value":next}));
        set(
            &mut snapshot,
            "phones",
            &previous,
            "details",
            Value::Object(details),
        );
        put(&mut snapshot, "phones", &next, json!({"label":"next"}));
        previous = next;
    }
    let report = inspect(&snapshot, "one").unwrap();
    assert_eq!(report["complete"].as_bool(), Some(false));
    assert!(
        report["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("byte-limit"))
    );
    assert!(report["configuration_digest"].is_null());
}

#[test]
fn benign_dependency_cycles_terminate_but_alias_cycles_are_refused() {
    let mut snapshot = fixture();
    set(
        &mut snapshot,
        "mailboxes",
        "inbox",
        "auth",
        json!([{"kind":"email-link","identity":"alias"}]),
    );
    assert_eq!(
        inspect(&snapshot, "one").unwrap()["complete"].as_bool(),
        Some(true)
    );
    set(
        &mut snapshot,
        "email-addresses",
        "primary",
        "alias_of",
        json!("alias"),
    );
    assert_eq!(
        inspect(&snapshot, "one").unwrap()["complete"].as_bool(),
        Some(false)
    );
}

pub(crate) fn plan_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut snapshot = fixture();
    let before = inspect(&snapshot, "one").unwrap();
    let scenario = match action {
        "Clean" => 1,
        "Label" => {
            set(&mut snapshot, "accounts", "one", "label", json!("m100.2"));
            2
        }
        "Dependency" => {
            set(
                &mut snapshot,
                "phones",
                "sms",
                "number",
                json!("+12025550124"),
            );
            3
        }
        "Alias" => {
            set(
                &mut snapshot,
                "email-addresses",
                "primary",
                "address",
                json!("another@example.test"),
            );
            4
        }
        "Auth" => {
            set(
                &mut snapshot,
                "accounts",
                "one",
                "provider",
                json!("different"),
            );
            5
        }
        "Source" => {
            set(
                &mut snapshot,
                "accounts",
                "one",
                "source",
                json!({"row":{"kind":"source-ref","value":"sheet:row"}}),
            );
            6
        }
        "Rewrite" => {
            snapshot
                .cells
                .get_mut(&("phones".into(), "sms".into()))
                .unwrap()
                .candidates[0]
                .operation = [99; 32];
            7
        }
        "Unrelated" => {
            put(
                &mut snapshot,
                "accounts",
                "other",
                json!({"provider":"other","method":"oauth"}),
            );
            8
        }
        "Conflict" => {
            snapshot
                .cells
                .get_mut(&("phones".into(), "sms".into()))
                .unwrap()
                .candidates
                .push(Candidate {
                    operation: [99; 32],
                    value: Value::Null,
                });
            9
        }
        "Missing" => {
            snapshot.cells.remove(&("mailboxes".into(), "inbox".into()));
            10
        }
        "History" => {
            snapshot.withheld = 1;
            11
        }
        "Limit" => {
            for i in 0..=8192 {
                put(
                    &mut snapshot,
                    "phones",
                    &format!("z{i:05}"),
                    json!({"label":"unused"}),
                );
            }
            12
        }
        "Scope" => {
            snapshot.scope = Scope::new(snapshot.scope.trust(), [5; 16], 1).unwrap();
            13
        }
        "Cycle" => {
            set(
                &mut snapshot,
                "email-addresses",
                "primary",
                "alias_of",
                json!("alias"),
            );
            14
        }
        _ => panic!("unknown plan action"),
    };
    let cells = snapshot.cells.clone();
    let report = inspect(&snapshot, "one").unwrap();
    let complete = report["complete"].as_bool().unwrap();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("complete", i64::from(complete));
    next.insert(
        "proposal",
        i64::from(complete && report["proposal_digest"] != before["proposal_digest"]),
    );
    next.insert(
        "configuration",
        i64::from(complete && report["configuration_digest"] != before["configuration_digest"]),
    );
    next.insert(
        "effects",
        i64::from(cells != snapshot.cells || report["authority"].as_str() != Some("none")),
    );
    next
}

#[test]
fn plans_conform_and_missing_dependency_or_conflict_checks_are_detected() {
    let model = aterm_models_db::account_plan_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = plan_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    for (action, field, law) in [
        ("Dependency", "configuration", "BindConfiguration"),
        ("Conflict", "complete", "WithholdIncomplete"),
    ] {
        let mut unsafe_state = plan_transition(&model.init_state(), action);
        unsafe_state.insert(field, if field == "complete" { 1 } else { 0 });
        assert!(!model.check_invariant(law, &unsafe_state));
        assert!(
            !model
                .successors(action, &model.init_state())
                .contains(&unsafe_state)
        );
    }
}
