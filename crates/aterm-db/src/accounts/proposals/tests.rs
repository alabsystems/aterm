// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    catalog::Directory,
    control::{Endpoint, Repair, Side},
    table::tests::Lab,
};
use aterm_json::json;
use aterm_net::lan::{Connection, Scope as LinkScope};
use aterm_secrets::commons::roster::Role;
use std::{net::TcpListener, thread, time::Duration};

fn app(lab: &Lab, n: usize) -> Proposals {
    Proposals::at(lab.paths[n].clone())
}
fn account(plan: &str) -> Change {
    Change {
        collection: "accounts".into(),
        key: "one".into(),
        value: Some(
            json!({"provider":"google","method":"email-link","plan":plan,
            "email":"Alias@example.test","auth":[{"kind":"email-link","mailbox":"inbox","requirements":["email-access"]}]}),
        ),
    }
}
fn cell(snapshot: &Snapshot) -> &crate::database::Cell {
    &snapshot.cells[&("accounts".into(), "one".into())]
}
fn rounds(lab: &Lab, count: usize) {
    let endpoints: Vec<_> = (0..2)
        .map(|n| {
            Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
                .unwrap()
        })
        .collect();
    let (mut ra, mut rb) = (Repair::default(), Repair::default());
    for _ in 0..count {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::scope(|threads| {
            let server = threads.spawn(|| {
                endpoints[1].exchange(
                    Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                    Side::Responder,
                    &mut rb,
                )
            });
            endpoints[0]
                .exchange(
                    Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60))
                        .unwrap(),
                    Side::Initiator,
                    &mut ra,
                )
                .unwrap();
            server.join().unwrap().unwrap();
        });
    }
}
fn raw(lab: &Lab, n: usize, payload: &[u8]) -> Receipt {
    let bootstrap = lab.bootstrap(n);
    let session = Session::open(&bootstrap).unwrap();
    let descriptor = descriptor(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[n], bootstrap.trust());
    let mut store = Store::open(
        &session,
        &directory.path(descriptor.scope).unwrap(),
        descriptor.scope,
    )
    .unwrap();
    store.commit_catalog(&session, payload).unwrap()
}

#[test]
fn atomic_contacts_reopen_with_original_receipts_and_explicit_aliases() {
    let lab = Lab::new(&[Role::Writer]);
    let db = app(&lab, 0);
    let mut changes = vec![account("basic")];
    changes.extend([
        Change {
            collection: "mailboxes".into(),
            key: "inbox".into(),
            value: Some(json!({"name":"inbox"})),
        },
        Change {
            collection: "email-addresses".into(),
            key: "alias".into(),
            value: Some(
                json!({"address":"Alias@example.test","mailbox_id":"inbox","alias_of":"primary"}),
            ),
        },
        Change {
            collection: "phones".into(),
            key: "sms".into(),
            value: Some(json!({"number":"+12025550123"})),
        },
        Change {
            collection: "machine-proposals".into(),
            key: "01010101010101010101010101010101".into(),
            value: Some(json!({"name":"m3","legacy_names":["m100"]})),
        },
    ]);
    let first = db.write([1; 16], changes.clone()).unwrap();
    let reopened = app(&lab, 0);
    assert_eq!(reopened.write([1; 16], changes).unwrap(), first);
    assert!(reopened.write([1; 16], vec![account("changed")]).is_err());
    let snapshot = reopened.snapshot().unwrap();
    assert_eq!(snapshot.cells.len(), 5);
    assert!(
        snapshot
            .cells
            .values()
            .all(|cell| cell.candidates[0].operation == first.id)
    );
    assert_eq!(snapshot.operations.len(), 1);
    assert_eq!(snapshot.operations[0].receipt, first);
    assert_eq!(
        snapshot.operations[0].header.actor,
        lab.bootstrap(0).request().unwrap().binding().actor()
    );
    assert_eq!(
        cell(&snapshot).candidates[0].value["email"].as_str(),
        Some("Alias@example.test")
    );
    // A JSON null is not silently converted into the permanent-stop representation.
    assert!(
        db.write(
            [2; 16],
            vec![Change {
                value: Some(aterm_json::Value::Null),
                ..account("unused")
            }]
        )
        .is_err()
    );
    assert_eq!(db.snapshot().unwrap().operations.len(), 1);
    let mailbox = |value| Change {
        collection: "mailboxes".into(),
        key: "private".into(),
        value: Some(value),
    };
    assert!(
        db.write(
            [3; 16],
            vec![mailbox(
                json!({"name":"private","secret_ref":"bearer-content"})
            )]
        )
        .is_err()
    );
    assert!(
        db.write(
            [4; 16],
            vec![mailbox(
                json!({"name":"private","secret_ref":{"vault":"mail","entry":"login"}})
            )]
        )
        .is_ok()
    );
}

#[test]
fn encrypted_peers_keep_late_concurrent_values_beside_an_offline_revert() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = app(&lab, 0);
    let b = app(&lab, 1);
    a.write([1; 16], vec![account("base")]).unwrap();
    rounds(&lab, 3);
    assert_eq!(cell(&b.snapshot().unwrap()).candidates.len(), 1);
    let changed = a.write([2; 16], vec![account("left")]).unwrap();
    b.write([3; 16], vec![account("right")]).unwrap();
    let reverted = a.revert([4; 16], changed.id).unwrap();
    assert_eq!(a.revert([4; 16], changed.id).unwrap(), reverted);
    rounds(&lab, 4);
    let left = a.snapshot().unwrap();
    let right = b.snapshot().unwrap();
    assert_eq!(left.cells, right.cells);
    assert_eq!(cell(&left).candidates.len(), 2);
    assert!(a.revert([5; 16], changed.id).is_err());
    let resolved = a.write([6; 16], vec![account("resolved")]).unwrap();
    assert_eq!(cell(&a.snapshot().unwrap()).candidates.len(), 1);
    assert!(
        a.revert([7; 16], resolved.id).is_err(),
        "a conflicted preimage needs explicit resolution"
    );
    rounds(&lab, 3);
    assert_eq!(a.snapshot().unwrap().cells, b.snapshot().unwrap().cells);
}

#[test]
fn deletions_are_reversible_and_intervening_changes_refuse_atomic_reverts() {
    let lab = Lab::new(&[Role::Writer]);
    let a = app(&lab, 0);
    let mut second = account("basic");
    second.key = "two".into();
    let both = a.write([1; 16], vec![account("basic"), second]).unwrap();
    a.write([2; 16], vec![account("new")]).unwrap();
    let count = a.snapshot().unwrap().operations.len();
    assert!(a.revert([3; 16], both.id).is_err());
    assert_eq!(a.snapshot().unwrap().operations.len(), count);
    let deletion = a
        .write(
            [4; 16],
            vec![Change {
                value: None,
                ..account("unused")
            }],
        )
        .unwrap();
    assert!(cell(&a.snapshot().unwrap()).candidates[0].value.is_null());
    a.revert([5; 16], deletion.id).unwrap();
    assert_eq!(
        cell(&a.snapshot().unwrap()).candidates[0].value["plan"].as_str(),
        Some("new")
    );
    assert!(a.write([6; 16], vec![account("restart")]).is_ok());
    assert!(!cell(&a.snapshot().unwrap()).stopped);
}

#[test]
fn reverting_creation_is_reversible_absence_and_bad_revert_keys_are_inert() {
    let lab = Lab::new(&[Role::Writer]);
    let db = app(&lab, 0);
    let created = db.write([1; 16], vec![account("base")]).unwrap();
    let reverted = db.revert([2; 16], created.id).unwrap();
    let empty = db.snapshot().unwrap();
    assert!(!cell(&empty).stopped);
    assert!(cell(&empty).candidates[0].value.is_null());
    db.revert([3; 16], reverted.id).unwrap();
    assert_eq!(
        cell(&db.snapshot().unwrap()).candidates[0].value["plan"].as_str(),
        Some("base")
    );
    let mut other = account("malicious");
    other.key = "other".into();
    let forged = raw(
        &lab,
        0,
        &encode(&Command::Revert {
            request: [4; 16],
            operation: created.id,
            changes: vec![other],
        })
        .unwrap(),
    );
    let view = db.snapshot().unwrap();
    assert!(
        view.operations
            .iter()
            .find(|op| op.header.id == forged.id)
            .unwrap()
            .refusal
            .is_some()
    );
    assert!(
        !view
            .cells
            .contains_key(&("accounts".into(), "other".into()))
    );
}

#[test]
fn authority_and_unknown_payloads_are_inert_without_poisoning_later_data() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = app(&lab, 0);
    let b = app(&lab, 1);
    a.initialize().unwrap();
    let authority = Command::Budget {
        request: [1; 16],
        name: "window".into(),
        total: 10,
        shares: std::collections::BTreeMap::from([(
            lab.bootstrap(0).request().unwrap().binding().actor(),
            10,
        )]),
    };
    raw(&lab, 0, &authority.encode().unwrap());
    raw(&lab, 0, b"{\"kind\":\"unrecognized\"}");
    raw(
        &lab,
        0,
        &encode(&Command::Write {
            request: [2; 16],
            changes: vec![account("valid")],
        })
        .unwrap(),
    );
    assert!(
        a.write([3; 16], vec![account("repair")]).is_err(),
        "a locally corrupted author must recover"
    );
    rounds(&lab, 4);
    let view = b.snapshot().unwrap();
    assert_eq!(
        view.operations
            .iter()
            .filter(|o| o.refusal.is_some())
            .count(),
        2
    );
    assert_eq!(
        cell(&view).candidates[0].value["plan"].as_str(),
        Some("valid")
    );
    assert!(b.write([4; 16], vec![account("another-author")]).is_ok());
}

#[test]
fn repeated_request_contents_are_inert_without_poisoning_following_edits() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = app(&lab, 0);
    a.write([1; 16], vec![account("first")]).unwrap();
    raw(
        &lab,
        0,
        &encode(&Command::Write {
            request: [1; 16],
            changes: vec![account("different")],
        })
        .unwrap(),
    );
    raw(
        &lab,
        0,
        &encode(&Command::Write {
            request: [2; 16],
            changes: vec![account("dependent")],
        })
        .unwrap(),
    );
    let view = a.snapshot().unwrap();
    assert_eq!(
        cell(&view).candidates[0].value["plan"].as_str(),
        Some("dependent")
    );
    assert_eq!(
        view.operations
            .iter()
            .filter(|o| o.refusal.is_some())
            .count(),
        2
    );
    assert!(a.write([3; 16], vec![account("more")]).is_err());
    rounds(&lab, 4);
    let b = app(&lab, 1);
    b.write([4; 16], vec![account("unrelated-writer")]).unwrap();
    assert_eq!(
        cell(&b.snapshot().unwrap()).candidates[0].value["plan"].as_str(),
        Some("unrelated-writer")
    );
}

#[test]
fn reader_can_project_but_cannot_create_and_descriptor_conflicts_refuse_both() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = app(&lab, 0);
    let reader = app(&lab, 1);
    assert!(reader.initialize().is_err());
    a.write([1; 16], vec![account("basic")]).unwrap();
    rounds(&lab, 3);
    assert_eq!(
        reader.snapshot().unwrap().cells,
        a.snapshot().unwrap().cells
    );
    assert!(reader.write([2; 16], vec![account("denied")]).is_err());
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let dir = Directory::new(&lab.paths[0], bootstrap.trust());
    let mut catalog = Store::open(
        &session,
        &dir.path(dir.catalog_scope()).unwrap(),
        dir.catalog_scope(),
    )
    .unwrap();
    catalog
        .commit_catalog(
            &session,
            &Descriptor::application(bootstrap.trust(), "accounts", "proposals", 2)
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
    drop(catalog);
    drop(session);
    assert!(a.snapshot().is_err());
    assert!(a.write([3; 16], vec![account("denied")]).is_err());
}

pub(crate) fn retry_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(&[Role::Writer]);
    let db = app(&lab, 0);
    db.initialize().unwrap();
    let mut receipt = None;
    if state["written"] == 1 {
        receipt = Some(db.write([1; 16], vec![account("base")]).unwrap());
    }
    if state["retried"] == 1 {
        assert_eq!(
            Some(db.write([1; 16], vec![account("base")]).unwrap()),
            receipt
        );
    }
    if state["changed"] == 1 {
        assert!(db.write([1; 16], vec![account("changed")]).is_err());
    }
    let mut next = state.clone();
    match action {
        "Write" => {
            db.write([1; 16], vec![account("base")]).unwrap();
            next.insert("written", 1);
        }
        "Retry" => {
            assert_eq!(
                Some(db.write([1; 16], vec![account("base")]).unwrap()),
                receipt
            );
            next.insert("retried", 1);
        }
        "Change" => {
            assert!(db.write([1; 16], vec![account("changed")]).is_err());
            next.insert("changed", 1);
        }
        _ => panic!("unknown retry action"),
    }
    next.insert("count", db.snapshot().unwrap().operations.len() as i64);
    next
}

#[test]
fn durable_request_retry_conforms_and_unsafe_variants_fail() {
    let model = aterm_models_db::proposal_retry_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = retry_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    let mut unsafe_retry = retry_transition(&model.init_state(), "Write");
    unsafe_retry.insert("count", 2);
    assert!(!model.check_invariant("OneEffect", &unsafe_retry));
    assert!(
        !model
            .successors("Write", &model.init_state())
            .contains(&unsafe_retry)
    );
}

#[test]
fn cli_exposes_shared_values_receipts_and_reverts_without_an_explicit_directory() {
    let lab = Lab::new(&[Role::Writer]);
    let run = |verb, options| command::run_at(verb, options, lab.paths[0].clone()).unwrap();
    let changes = r#"[{"collection":"accounts","key":"one","value":{"provider":"google","method":"email-link"}}]"#;
    let request = aterm_codec::hex::encode_lower(&[1; 16]);
    let arguments = || {
        std::collections::BTreeMap::from([("--changes", changes), ("--request", request.as_str())])
    };
    let first = run("accounts-write", arguments());
    assert_eq!(first, run("accounts-write", arguments()));
    let history = run("accounts-history", Default::default());
    assert_eq!(history["operations"].as_array().unwrap().len(), 1);
    assert_eq!(history["retained"].as_u64(), Some(1));
    assert!(history["operations"][0]["roster"].as_str().is_some());
    let operation = first["operation"].as_str().unwrap();
    let undo = aterm_codec::hex::encode_lower(&[2; 16]);
    run(
        "accounts-revert",
        std::collections::BTreeMap::from([
            ("--operation", operation),
            ("--request", undo.as_str()),
        ]),
    );
    let got = run(
        "accounts-get",
        std::collections::BTreeMap::from([("--collection", "accounts"), ("--key", "one")]),
    );
    assert_eq!(got["cell"]["stopped"].as_bool(), Some(false));
    assert!(got["cell"]["candidates"][0]["value"].is_null());
}

#[test]
fn a_late_remote_request_conflict_cannot_blame_an_honest_reverter() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = app(&lab, 0);
    let b = app(&lab, 1);
    let first = b.write([1; 16], vec![account("remote")]).unwrap();
    rounds(&lab, 3);
    let undo = a.revert([2; 16], first.id).unwrap();
    raw(
        &lab,
        1,
        &encode(&Command::Write {
            request: [1; 16],
            changes: vec![account("conflicting")],
        })
        .unwrap(),
    );
    rounds(&lab, 4);
    let snapshot = a.snapshot().unwrap();
    let operation = snapshot
        .operations
        .iter()
        .find(|op| op.header.id == undo.id)
        .unwrap();
    assert!(operation.eligible);
    assert!(operation.refusal.is_none());
    assert_eq!(a.revert([2; 16], first.id).unwrap(), undo);
    a.write([3; 16], vec![account("honest-writer-continues")])
        .unwrap();
}

#[test]
fn refused_revert_never_returns_a_successful_retry_receipt() {
    let lab = Lab::new(&[Role::Writer]);
    let a = app(&lab, 0);
    let first = a.write([1; 16], vec![account("base")]).unwrap();
    let mut wrong = account("wrong");
    wrong.key = "other".into();
    raw(
        &lab,
        0,
        &encode(&Command::Revert {
            request: [2; 16],
            operation: first.id,
            changes: vec![wrong],
        })
        .unwrap(),
    );
    assert!(a.revert([2; 16], first.id).is_err());
    assert_eq!(a.snapshot().unwrap().operations.len(), 2);
}

fn withheld_request_conflicts_remain_negative_evidence() -> aterm_spec::interp::State {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = app(&lab, 0);
    let b = app(&lab, 1);
    let first = b.write([1; 16], vec![account("first")]).unwrap();
    rounds(&lab, 3);
    a.write([2; 16], vec![account("parent")]).unwrap();
    rounds(&lab, 3);
    let conflict = raw(
        &lab,
        1,
        &encode(&Command::Write {
            request: [1; 16],
            changes: vec![account("conflicting")],
        })
        .unwrap(),
    );
    assert!(
        b.snapshot()
            .unwrap()
            .operations
            .iter()
            .any(|op| { op.header.id == first.id && !op.eligible && op.refusal.is_some() })
    );
    // The second request descends from A's now-withdrawn record; the first does not.
    lab.rotate(&[Role::Reader, Role::Writer]);
    let snapshot = b.snapshot().unwrap();
    assert!(snapshot.cells.is_empty());
    assert!(
        snapshot
            .operations
            .iter()
            .any(|op| op.header.id == conflict.id && !op.catalog_eligible && op.available)
    );
    assert_eq!(
        snapshot
            .operations
            .iter()
            .filter(|op| op.refusal.is_some())
            .count(),
        2
    );
    let retry = b.write([1; 16], vec![account("first")]).is_ok();
    assert!(b.write([1; 16], vec![account("different-again")]).is_err());
    rounds(&lab, 4);
    let converged = a.snapshot().unwrap().cells == b.snapshot().unwrap().cells;
    evidence_state(1, &snapshot, first.id, conflict.id, retry, false, converged)
}

fn unreadable_quarantined_requests_fence_only_their_author() -> aterm_spec::interp::State {
    use aterm_secrets::{
        client,
        commons::{journal::Journal, node::Edit, roster::Grant},
    };
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let a = app(&lab, 0);
    let b = app(&lab, 1);
    let first = b.write([1; 16], vec![account("first")]).unwrap();
    rounds(&lab, 3);
    let mut healthy = account("healthy-dependent-before-fork");
    healthy.key = "healthy".into();
    let prior = a.write([9; 16], vec![healthy]).unwrap();
    rounds(&lab, 3);
    let bootstraps = [lab.bootstrap(0), lab.bootstrap(1)];
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let journal =
        Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
    let grants = || {
        bootstraps
            .iter()
            .enumerate()
            .map(|(n, boot)| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    boot.request().unwrap().binding().clone(),
                    Role::Writer,
                )
            })
            .collect()
    };
    let left = admin.prepare_commons_rotation(&journal, grants()).unwrap();
    let right = admin.prepare_commons_rotation(&journal, grants()).unwrap();
    drop(journal);
    let ingest = |node: &aterm_secrets::commons::node::Node| {
        for bootstrap in &bootstraps {
            Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
                .unwrap()
                .ingest(node.bytes())
                .unwrap();
        }
    };
    ingest(&left);
    let conflict = raw(
        &lab,
        1,
        &encode(&Command::Write {
            request: [1; 16],
            changes: vec![account("left-branch-conflict")],
        })
        .unwrap(),
    );
    assert!(
        !b.snapshot()
            .unwrap()
            .cells
            .contains_key(&("accounts".into(), "one".into()))
    );
    let recovery = admin
        .seal_commons_restricted(
            bootstraps[0].trust().network,
            Edit::Recover {
                heads: &[left.id(), right.id()],
                selected: right.id(),
            },
            &[],
        )
        .unwrap();
    ingest(&right);
    ingest(&recovery);
    lab.rotate(&[Role::Writer, Role::Writer]);
    let snapshot = b.snapshot().unwrap();
    assert!(
        !snapshot
            .cells
            .contains_key(&("accounts".into(), "one".into())),
        "unknown retained request cannot resurrect its old prefix"
    );
    assert!(snapshot.operations.iter().any(|op| !op.available));
    assert!(
        snapshot
            .operations
            .iter()
            .any(|op| op.header.id == prior.id && op.eligible)
    );
    let retry = b.write([1; 16], vec![account("first")]).is_ok();
    rounds(&lab, 4);
    let converged = a.snapshot().unwrap().cells == b.snapshot().unwrap().cells;
    let peer = a
        .write([3; 16], vec![account("unrelated-current-author")])
        .is_ok();
    evidence_state(2, &snapshot, first.id, conflict.id, retry, peer, converged)
}

fn evidence_state(
    scenario: i64,
    snapshot: &Snapshot,
    first: Digest,
    conflict: Digest,
    retry: bool,
    peer: bool,
    converged: bool,
) -> aterm_spec::interp::State {
    let records: Vec<_> = snapshot
        .operations
        .iter()
        .filter(|op| op.header.id == first || op.header.id == conflict)
        .collect();
    aterm_spec::interp::State::from([
        ("scenario", scenario),
        ("retained", records.len() as i64),
        ("revived", i64::from(records.iter().any(|op| op.eligible))),
        ("retry", i64::from(retry)),
        ("peer", i64::from(peer)),
        ("converged", i64::from(converged)),
    ])
}

pub(crate) fn evidence_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    assert_eq!(state["scenario"], 0);
    match action {
        "Withdraw" => withheld_request_conflicts_remain_negative_evidence(),
        "Quarantine" => unreadable_quarantined_requests_fence_only_their_author(),
        _ => panic!("unknown evidence action"),
    }
}

#[test]
fn retained_request_evidence_conforms_through_withdrawal_and_quarantine() {
    let model = aterm_models_db::proposal_evidence_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for action in &model.actions {
        let next = evidence_transition(&model.init_state(), action.name);
        assert!(
            model
                .successors(action.name, &model.init_state())
                .contains(&next)
        );
        let mut unsafe_revival = next.clone();
        unsafe_revival.insert("revived", 1);
        assert!(!model.check_invariant("NoRevival", &unsafe_revival));
        assert!(
            !model
                .successors(action.name, &model.init_state())
                .contains(&unsafe_revival)
        );
    }
}
