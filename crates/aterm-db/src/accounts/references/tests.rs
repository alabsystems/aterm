// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{database::Candidate, table::Scope};
use aterm_secrets::commons::journal::Trust;

fn empty() -> Snapshot {
    Snapshot {
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
        operations: Vec::new(),
    }
}
fn put(snapshot: &mut Snapshot, collection: &str, id: &str, value: Value) {
    let mut operation = [0; 32];
    operation[..8].copy_from_slice(
        &u64::try_from(snapshot.cells.len() + 1)
            .unwrap()
            .to_le_bytes(),
    );
    snapshot.cells.insert(
        (collection.into(), id.into()),
        Cell {
            stopped: false,
            candidates: vec![Candidate { operation, value }],
        },
    );
}
fn fixture() -> Snapshot {
    let mut snapshot = empty();
    put(
        &mut snapshot,
        "mailboxes",
        "inbox",
        json!({"name":"inbox","secret_ref":{"vault":"private","entry":"mail"}}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "primary",
        json!({"address":"Case+tag@example.test","mailbox_id":"inbox"}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "alias",
        json!({"address":"alias@example.test","alias_of":"primary"}),
    );
    put(
        &mut snapshot,
        "phones",
        "sms",
        json!({"number":"+12025550123"}),
    );
    put(
        &mut snapshot,
        "accounts",
        "account",
        json!({"provider":"example","method":"email-link",
        "identities":[{"kind":"email","id":"alias"},{"kind":"phone","id":"sms"}],
        "auth":[{"kind":"email-link","identity":"alias","mailbox":"inbox"}]}),
    );
    snapshot
}
fn codes(report: &Value) -> BTreeSet<&str> {
    report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["code"].as_str().unwrap())
        .collect()
}

#[test]
fn explicit_aliases_and_optional_metadata_create_no_implied_requirements() {
    let mut snapshot = fixture();
    put(
        &mut snapshot,
        "accounts",
        "minimal",
        json!({"provider":"google","method":"phone"}),
    );
    put(
        &mut snapshot,
        "phones",
        "unspecified",
        json!({"label":"private"}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "unlinked",
        json!({"address":"unknown@example.test"}),
    );
    let report = inspect(&snapshot);
    assert!(codes(&report).is_empty());
    assert_eq!(report["authority"].as_str(), Some("none"));
    assert_eq!(report["scope"].as_str(), Some("observed-local-proposals"));
    assert_eq!(report["truncated"].as_bool(), Some(false));
    let text = aterm_json::to_string(&report).unwrap();
    assert!(!text.contains("Case+tag"));
    assert!(!text.contains("private"));
    assert!(!text.contains("+12025550123"));
}

#[test]
fn exact_addresses_never_infer_aliases_or_fold_case_dots_and_tags() {
    let mut snapshot = fixture();
    for (id, address) in [
        ("case", "case+tag@example.test"),
        ("tag", "Case@example.test"),
        ("dot", "Ca.se+tag@example.test"),
    ] {
        put(
            &mut snapshot,
            "email-addresses",
            id,
            json!({"address":address}),
        );
    }
    assert!(!codes(&inspect(&snapshot)).contains("duplicate-exact-address"));
    put(
        &mut snapshot,
        "email-addresses",
        "duplicate",
        json!({"address":"Case+tag@example.test"}),
    );
    assert!(codes(&inspect(&snapshot)).contains("duplicate-exact-address"));
}

#[test]
fn intermediate_conflicts_deletions_and_mailbox_disagreement_are_explicit() {
    let mut snapshot = fixture();
    let key = ("email-addresses".into(), "primary".into());
    let original = snapshot.cells[&key].clone();
    snapshot
        .cells
        .get_mut(&key)
        .unwrap()
        .candidates
        .push(Candidate {
            operation: [99; 32],
            value: Value::Null,
        });
    let report = inspect(&snapshot);
    let issue = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| {
            issue["key"].as_str() == Some("account")
                && issue["code"].as_str() == Some("conflicted-reference")
        })
        .unwrap();
    assert_eq!(issue["target"]["key"].as_str(), Some("primary"));
    assert_eq!(
        issue["target"]["evidence"]["candidate_count"].as_u64(),
        Some(2)
    );
    snapshot.cells.insert(
        key.clone(),
        Cell {
            stopped: false,
            candidates: vec![Candidate {
                operation: [99; 32],
                value: Value::Null,
            }],
        },
    );
    assert!(codes(&inspect(&snapshot)).contains("deleted-reference"));
    snapshot.cells.insert(key, original);
    put(&mut snapshot, "mailboxes", "other", json!({"name":"other"}));
    put(
        &mut snapshot,
        "email-addresses",
        "alias",
        json!({"address":"alias@example.test","alias_of":"primary","mailbox_id":"other"}),
    );
    assert!(codes(&inspect(&snapshot)).contains("alias-mailbox-conflict"));
}

#[test]
fn diagnostics_and_evidence_remain_bounded_and_input_order_independent() {
    let mut snapshot = empty();
    for i in 0..MAX_OBJECTS + 1 {
        put(
            &mut snapshot,
            "email-addresses",
            &format!("e{i:05}"),
            json!({"address":"same@example.test","alias_of":"missing"}),
        );
    }
    let report = inspect(&snapshot);
    assert_eq!(report["truncated"].as_bool(), Some(true));
    assert!(report["issues"].as_array().unwrap().len() <= MAX_ISSUES);
    assert_eq!(
        report["inspected_objects"].as_u64(),
        Some(MAX_OBJECTS as u64)
    );
    let key = ("email-addresses".into(), "e00000".into());
    let cell = snapshot.cells.get_mut(&key).unwrap();
    for i in 0..MAX_EVIDENCE + 1 {
        cell.candidates.push(Candidate {
            operation: [i as u8; 32],
            value: Value::Null,
        });
    }
    let report = inspect(&snapshot);
    let evidence = &report["issues"][0]["evidence"];
    assert!(evidence["operations"].as_array().unwrap().len() <= MAX_EVIDENCE);
    assert_eq!(evidence["truncated"].as_bool(), Some(true));
    let reversed = Snapshot {
        cells: snapshot
            .cells
            .iter()
            .rev()
            .map(|(k, v)| {
                let mut cell = v.clone();
                cell.candidates.reverse();
                (k.clone(), cell)
            })
            .collect(),
        ..snapshot.clone()
    };
    assert_eq!(report, inspect(&reversed));
}

#[test]
fn a_broken_tail_keeps_prior_mailbox_contradictions_visible() {
    let mut snapshot = fixture();
    put(&mut snapshot, "mailboxes", "other", json!({"name":"other"}));
    put(
        &mut snapshot,
        "email-addresses",
        "alias",
        json!({"address":"alias@example.test","mailbox_id":"other","alias_of":"primary"}),
    );
    put(
        &mut snapshot,
        "email-addresses",
        "primary",
        json!({"address":"Case+tag@example.test","mailbox_id":"inbox","alias_of":"missing"}),
    );
    let report = inspect(&snapshot);
    assert!(codes(&report).contains("missing-reference"));
    assert!(codes(&report).contains("alias-mailbox-conflict"));
    put(
        &mut snapshot,
        "email-addresses",
        "alias",
        json!({"address":"alias@example.test","mailbox_id":"missing-mailbox"}),
    );
    let report = inspect(&snapshot);
    assert!(codes(&report).contains("missing-reference"));
    assert!(codes(&report).contains("route-mailbox-conflict"));
}

#[test]
fn lookup_exhaustion_stops_before_claiming_unvisited_objects() {
    let mut snapshot = empty();
    let routes = (0..16)
        .map(|_| json!({"kind":"oauth","identity":"missing"}))
        .collect::<Vec<_>>();
    for i in 0..2100 {
        put(
            &mut snapshot,
            "accounts",
            &format!("a{i:04}"),
            json!({"provider":"example","method":"oauth","auth":routes}),
        );
    }
    let report = inspect(&snapshot);
    assert!(
        report["limits_hit"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("steps"))
    );
    assert!(report["inspected_objects"].as_u64().unwrap() < snapshot.cells.len() as u64);
    assert_eq!(report["truncated"].as_bool(), Some(true));
}

#[test]
fn empty_and_conflicted_objects_do_not_invent_self_references() {
    let mut snapshot = empty();
    snapshot
        .cells
        .insert(("accounts".into(), "empty".into()), Cell::default());
    let report = inspect(&snapshot);
    assert_eq!(report["issues"][0]["code"].as_str(), Some("empty-object"));
    assert!(report["issues"][0]["target"].is_null());
    put(
        &mut snapshot,
        "accounts",
        "empty",
        json!({"provider":"example","method":"oauth"}),
    );
    snapshot
        .cells
        .get_mut(&("accounts".into(), "empty".into()))
        .unwrap()
        .candidates
        .push(Candidate {
            operation: [99; 32],
            value: Value::Null,
        });
    let report = inspect(&snapshot);
    assert_eq!(
        report["issues"][0]["code"].as_str(),
        Some("conflicted-object")
    );
    assert!(report["issues"][0]["target"].is_null());
}

pub(crate) fn reference_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut snapshot = fixture();
    let scenario = match action {
        "Clean" => 1,
        "Cycle" => {
            put(
                &mut snapshot,
                "email-addresses",
                "primary",
                json!({"address":"Case+tag@example.test","alias_of":"alias"}),
            );
            2
        }
        "Conflict" => {
            snapshot
                .cells
                .get_mut(&("email-addresses".into(), "primary".into()))
                .unwrap()
                .candidates
                .push(Candidate {
                    operation: [99; 32],
                    value: Value::Null,
                });
            3
        }
        "Deleted" => {
            put(&mut snapshot, "mailboxes", "inbox", Value::Null);
            4
        }
        "Route" => {
            put(&mut snapshot, "mailboxes", "other", json!({"name":"other"}));
            put(
                &mut snapshot,
                "accounts",
                "account",
                json!({"provider":"example","method":"email-link",
                "auth":[{"kind":"email-link","identity":"alias","mailbox":"other"}]}),
            );
            5
        }
        "Duplicate" => {
            put(
                &mut snapshot,
                "email-addresses",
                "duplicate",
                json!({"address":"Case+tag@example.test"}),
            );
            6
        }
        "Namespace" => {
            put(
                &mut snapshot,
                "phones",
                "alias",
                json!({"label":"other namespace"}),
            );
            put(
                &mut snapshot,
                "accounts",
                "account",
                json!({"provider":"example","method":"oauth",
                "auth":[{"kind":"oauth","identity":"alias"}]}),
            );
            7
        }
        "Limit" => {
            for i in 0..=MAX_ALIAS_DEPTH {
                put(
                    &mut snapshot,
                    "email-addresses",
                    &format!("chain{i}"),
                    json!({"address":format!("chain{i}@example.test"),"alias_of":format!("chain{}",i+1)}),
                );
            }
            8
        }
        _ => panic!("unknown reference action"),
    };
    let before = snapshot.cells.clone();
    let report = inspect(&snapshot);
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("warning", i64::from(!codes(&report).is_empty()));
    next.insert(
        "truncated",
        i64::from(report["truncated"].as_bool().unwrap()),
    );
    next.insert("effects", i64::from(before != snapshot.cells));
    next
}

#[test]
fn reference_diagnostics_conform_and_missing_checks_are_detected() {
    let model = aterm_models_db::account_references_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = reference_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {:?}",
                    action.name,
                    next
                );
            }
        }
    }
    let mut unsafe_cycle = reference_transition(&model.init_state(), "Cycle");
    unsafe_cycle.insert("warning", 0);
    assert!(!model.check_invariant("ReportReferences", &unsafe_cycle));
    assert!(
        !model
            .successors("Cycle", &model.init_state())
            .contains(&unsafe_cycle)
    );
}
