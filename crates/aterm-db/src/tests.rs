// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};
use database::{Change, Command};
use local::{Credentials, Database};
use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::{Path, PathBuf},
    thread,
};

pub(super) fn tmp() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-db-test-")
        .tempdir()
        .unwrap()
}
pub(super) fn pair() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let root = tmp();
    let a = root.path().join("a");
    let b = root.path().join("b");
    local::init(&a, "test").unwrap();
    let request = local::request(&b).unwrap();
    let credentials = Credentials::load(&a).unwrap();
    local::join(
        &b,
        &credentials.invitation(&request).unwrap(),
        credentials.manifest.root,
    )
    .unwrap();
    (root, a, b)
}
fn write(request: u8, key: &str, value: aterm_json::Value) -> Command {
    Command::Write {
        request: [request; 16],
        changes: vec![Change {
            collection: "example".into(),
            key: key.into(),
            value: Some(value),
        }],
    }
}
pub(super) fn wires(dir: &Path) -> Vec<Vec<u8>> {
    let db = Database::open(dir).unwrap();
    db.history
        .index()
        .inventory()
        .map(|id| db.history.wire(id).unwrap().to_vec())
        .collect()
}
pub(super) fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut all = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            all.extend(files(&entry.path()));
        } else {
            all.insert(entry.path(), std::fs::read(entry.path()).unwrap());
        }
    }
    all
}
fn deliver(to: &Path, records: &[Vec<u8>]) {
    let mut db = Database::open(to).unwrap();
    for record in records.iter().rev() {
        db.ingest(record).unwrap();
        db.ingest(record).unwrap();
    }
}
fn reconcile(a: &Path, b: &Path) {
    let aa = wires(a);
    let bb = wires(b);
    deliver(a, &bb);
    deliver(b, &aa);
}

#[test]
fn one_machine_is_durable_atomic_encrypted_and_idempotent_after_restart() {
    let root = tmp();
    let path = root.path().join("db");
    local::init(&path, "accounts").unwrap();
    let command = Command::Write {
        request: [1; 16],
        changes: vec![
            Change {
                collection: "example".into(),
                key: "left".into(),
                value: Some(json!("private-account-metadata")),
            },
            Change {
                collection: "example".into(),
                key: "right".into(),
                value: Some(json!(42)),
            },
        ],
    };
    let original = {
        let mut db = Database::open(&path).unwrap();
        let id = db.commit(command.clone()).unwrap();
        assert_eq!(db.view().cells.len(), 2);
        id
    };
    let mut db = Database::open(&path).unwrap();
    assert_eq!(db.commit(command).unwrap(), original);
    assert_eq!(db.history.index().inventory().count(), 1);
    assert!(db.commit(write(1, "different", json!(false))).is_err());
    let bytes = std::fs::read(path.join("operations.astream")).unwrap();
    assert!(
        !bytes
            .windows(b"private-account-metadata".len())
            .any(|p| p == b"private-account-metadata")
    );
    assert!(Database::open(&path).is_err(), "one local durable writer");
    let baseline = db.view().cells;
    drop(db);

    let put = |value: &str| {
        vec![
            "put".into(),
            "--dir".into(),
            path.to_string_lossy().into_owned(),
            "--collection".into(),
            "example".into(),
            "--key".into(),
            "left".into(),
            "--value".into(),
            value.into(),
            "--request".into(),
            aterm_codec::hex::encode_lower(&[2; 16]),
        ]
    };
    assert!(crate::cli::run(&put("null")).is_err());
    let mut db = Database::open(&path).unwrap();
    assert_eq!(db.history.index().inventory().count(), 1);
    assert_eq!(db.view().cells, baseline);
    assert!(
        db.commit(Command::Write {
            request: [3; 16],
            changes: vec![
                Change {
                    collection: "example".into(),
                    key: "left".into(),
                    value: Some(json!("must not replace the existing value")),
                },
                Change {
                    collection: "example".into(),
                    key: "right".into(),
                    value: Some(Value::Null),
                },
            ],
        })
        .is_err()
    );
    assert_eq!(db.history.index().inventory().count(), 1);
    assert_eq!(db.view().cells, baseline);
    drop(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(db.history.index().inventory().count(), 1);
    assert_eq!(db.view().cells, baseline);
    drop(db);

    // A rejected request reserves nothing. Nested JSON null is still an ordinary
    // value; only a whole-value null would alias the v1 permanent-stop encoding.
    let nested = put(r#"{"nested":null}"#);
    let receipt = crate::cli::run(&nested).unwrap();
    assert_eq!(crate::cli::run(&nested).unwrap(), receipt);
    let mut db = Database::open(&path).unwrap();
    assert_eq!(db.history.index().inventory().count(), 2);
    assert_eq!(
        db.view().cells[&("example".into(), "left".into())].candidates[0].value,
        json!({"nested":null})
    );
    db.commit(Command::Write {
        request: [4; 16],
        changes: vec![Change {
            collection: "example".into(),
            key: "left".into(),
            value: None,
        }],
    })
    .unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(db.history.index().inventory().count(), 3);
    assert!(db.view().cells[&("example".into(), "left".into())].stopped);
    assert!(
        db.view().cells[&("example".into(), "left".into())]
            .candidates
            .is_empty()
    );
    assert_eq!(
        db.view().cells[&("example".into(), "right".into())],
        baseline[&("example".into(), "right".into())]
    );
}

#[test]
fn two_offline_writers_keep_conflicts_resolve_causally_and_respect_concurrent_stops() {
    let (_root, a, b) = pair();
    Database::open(&a)
        .unwrap()
        .commit(write(
            1,
            "same",
            json!({"realm":"global","email":"one@example.invalid"}),
        ))
        .unwrap();
    Database::open(&b)
        .unwrap()
        .commit(write(
            2,
            "same",
            json!({"realm":"cn","email":"two@example.invalid"}),
        ))
        .unwrap();
    reconcile(&a, &b);
    let cell = |path: &Path| {
        Database::open(path).unwrap().view().cells[&("example".into(), "same".into())].clone()
    };
    assert_eq!(cell(&a), cell(&b));
    assert_eq!(
        cell(&a).candidates.len(),
        2,
        "whole bundles survive; no synthetic identity"
    );
    Database::open(&a)
        .unwrap()
        .commit(write(3, "same", json!({"resolved":true})))
        .unwrap();
    reconcile(&a, &b);
    assert_eq!(cell(&a).candidates.len(), 1);
    assert_eq!(cell(&a), cell(&b));
    Database::open(&a)
        .unwrap()
        .commit(Command::Write {
            request: [4; 16],
            changes: vec![Change {
                collection: "example".into(),
                key: "same".into(),
                value: None,
            }],
        })
        .unwrap();
    Database::open(&b)
        .unwrap()
        .commit(write(5, "same", json!({"late":true})))
        .unwrap();
    reconcile(&a, &b);
    assert!(cell(&a).stopped);
    assert_eq!(cell(&a), cell(&b));
    assert!(cell(&b).candidates.is_empty());
}

#[test]
fn two_machine_budget_shares_survive_partitions_restarts_and_lost_replies() {
    let (_root, a, b) = pair();
    let aa = Credentials::load(&a).unwrap().capability.actor;
    let bb = Credentials::load(&b).unwrap().capability.actor;
    let grant = Database::open(&a)
        .unwrap()
        .allocate(
            [1; 16],
            "provider-account-window".into(),
            10,
            BTreeMap::from([(aa, 4), (bb, 6)]),
        )
        .unwrap();
    reconcile(&a, &b);
    let first = Database::open(&a)
        .unwrap()
        .spend([2; 16], "provider-account-window", 4)
        .unwrap();
    assert_eq!(
        Database::open(&a)
            .unwrap()
            .spend([2; 16], "provider-account-window", 4)
            .unwrap(),
        first
    );
    assert!(
        Database::open(&a)
            .unwrap()
            .spend([3; 16], "provider-account-window", 1)
            .is_err()
    );
    Database::open(&b)
        .unwrap()
        .spend([4; 16], "provider-account-window", 6)
        .unwrap();
    assert!(
        Database::open(&b)
            .unwrap()
            .spend([5; 16], "provider-account-window", 1)
            .is_err()
    );
    reconcile(&a, &b);
    let db = Database::open(&a).unwrap();
    let view = db.view();
    assert_eq!(view.spent[&(grant, aa)] + view.spent[&(grant, bb)], 10);
    assert!(view.invalid.is_empty());
    drop(db);
    assert!(
        Database::open(&a)
            .unwrap()
            .allocate(
                [6; 16],
                "provider-account-window".into(),
                10,
                BTreeMap::from([(aa, 10)])
            )
            .is_err()
    );
    assert!(
        Database::open(&b)
            .unwrap()
            .allocate([7; 16], "new-window".into(), 10, BTreeMap::from([(bb, 10)]))
            .is_err(),
        "ordinary writer cannot mint rights"
    );
}

#[test]
fn invalid_signatures_foreign_databases_and_torn_wire_are_not_acknowledged() {
    let (_root, a, b) = pair();
    Database::open(&a)
        .unwrap()
        .commit(write(1, "one", json!(1)))
        .unwrap();
    let wire = wires(&a).pop().unwrap();
    let mut db = Database::open(&b).unwrap();
    let mut tampered = wire.clone();
    let n = tampered.len();
    tampered[n - 1] ^= 1;
    assert!(db.ingest(&tampered).is_err());
    for len in 0..wire.len() {
        assert!(db.ingest(&wire[..len]).is_err());
    }
    assert_eq!(db.history.index().inventory().count(), 0);
    drop(db);
    let other = tmp();
    local::init(other.path(), "other").unwrap();
    assert!(Database::open(other.path()).unwrap().ingest(&wire).is_err());
}

#[test]
fn authenticated_lan_exchange_is_symmetric_and_repairs_after_offline_edits() {
    let (_root, a, b) = pair();
    Database::open(&a)
        .unwrap()
        .commit(write(1, "a", json!(1)))
        .unwrap();
    Database::open(&b)
        .unwrap()
        .commit(write(2, "b", json!(2)))
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let serve_dir = b.clone();
    let server = thread::spawn(move || {
        let (tcp, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        let connection =
            aterm_net::lan::Connection::admit(tcp, aterm_net::lan::Scope::Loopback).unwrap();
        network::serve_connection(&serve_dir, connection).unwrap()
    });
    let client = network::sync_round(&a, address).unwrap();
    let server = server.join().unwrap();
    assert_eq!(client.sent, server.received);
    assert_eq!(client.received, server.sent);
    assert_eq!(client.sent, 1);
    assert_eq!(client.received, 1);
    assert_eq!(wires(&a), wires(&b));
}

#[test]
fn aliases_phone_routes_and_machine_name_history_remain_distinct() {
    let root = tmp();
    local::init(root.path(), "accounts").unwrap();
    let mut db = Database::open(root.path()).unwrap();
    let id = "00112233445566778899aabbccddeeff";
    for (request, name) in [(1, "m100"), (2, "m3")] {
        db.commit(Command::Write {
            request: [request; 16],
            changes: vec![Change {
                collection: "machines".into(),
                key: id.into(),
                value: Some(json!({"name":name})),
            }],
        })
        .unwrap();
    }
    let view = db.view();
    assert_eq!(view.machine_names["m100"], view.machine_names["m3"]);
    assert!(view.machine_names["m100"].contains(id));
    for address in ["owner@example.invalid", "owner+ai@example.invalid"] {
        crate::contacts::validate(
            "email-addresses",
            &json!({"address":address,"mailbox_id":"shared-mailbox"}),
        )
        .unwrap();
    }
    crate::accounts::validate(&json!({"provider":"google","method":"email-link","machine_id":id,"slot":2,"label":"m3.2","auth":[{"kind":"email-link","mailbox":"shared-mailbox","requirements":["email-access","human-approval"]}],"details":{"phone":{"kind":"phone-ref","value":"phone-one"}}})).unwrap();
    assert!(crate::accounts::validate(&json!({"provider":"google","method":"email-link","details":{"password":{"kind":"note","value":"secret"}}})).is_err());
    assert!(
        crate::accounts::validate(
            &json!({"provider":"google","method":"email-link","password":"secret"})
        )
        .is_err()
    );
    crate::contacts::validate("mailboxes", &json!({"name":"inbox","provider":"proton"})).unwrap();
    for bad_provider in [
        json!(""),
        json!("bad provider"),
        json!(null),
        json!({"secret":"raw"}),
    ] {
        assert!(
            crate::contacts::validate(
                "mailboxes",
                &json!({"name":"inbox","provider":bad_provider})
            )
            .is_err()
        );
    }
    crate::contacts::validate(
        "phones",
        &json!({"number":"+12025550123","label":"office line"}),
    )
    .unwrap();
    for bad_label in [
        json!(""),
        json!("line\nbreak"),
        json!("x".repeat(129)),
        json!(null),
        json!({"secret":"raw"}),
    ] {
        assert!(
            crate::contacts::validate("phones", &json!({"label":bad_label})).is_err(),
            "rejected bad phone label {bad_label:?}"
        );
    }
    assert!(
        crate::cli::run(&[
            "spend".into(),
            "--dir".into(),
            root.path().to_str().unwrap().into(),
            "--name".into(),
            "window".into(),
            "--cost".into(),
            "+1".into(),
            "--request".into(),
            aterm_codec::hex::encode_lower(&[9; 16]),
        ])
        .is_err()
    );
}

fn rights_state(
    view: &database::View,
    grant: [u8; 32],
    a: astream_wire::sync::Actor,
    b: astream_wire::sync::Actor,
    disconnected: i64,
) -> aterm_spec::interp::State {
    BTreeMap::from([
        (
            "a",
            view.spent.get(&(grant, a)).copied().unwrap_or(0) as i64,
        ),
        (
            "b",
            view.spent.get(&(grant, b)).copied().unwrap_or(0) as i64,
        ),
        ("disconnected", disconnected),
    ])
}

#[test]
fn actual_durable_admission_conforms_to_every_model_spend_and_rejects_overdraws() {
    let model = aterm_models_db::offline_rights_model();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in model
            .actions
            .iter()
            .filter(|a| a.name == "SpendA" || a.name == "SpendB")
        {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let (_root, a, b) = pair();
            let aa = Credentials::load(&a).unwrap().capability.actor;
            let bb = Credentials::load(&b).unwrap().capability.actor;
            let grant = Database::open(&a)
                .unwrap()
                .allocate(
                    [1; 16],
                    "model-window".into(),
                    4,
                    BTreeMap::from([(aa, 2), (bb, 2)]),
                )
                .unwrap();
            reconcile(&a, &b);
            if state["a"] > 0 {
                Database::open(&a)
                    .unwrap()
                    .spend([2; 16], "model-window", state["a"] as u64)
                    .unwrap();
            }
            if state["b"] > 0 {
                Database::open(&b)
                    .unwrap()
                    .spend([3; 16], "model-window", state["b"] as u64)
                    .unwrap();
            }
            let actor = if action.name == "SpendA" { &a } else { &b };
            Database::open(actor)
                .unwrap()
                .spend([4; 16], "model-window", 1)
                .unwrap();
            reconcile(&a, &b);
            let db = Database::open(&a).unwrap();
            let next = rights_state(&db.view(), grant, aa, bb, state["disconnected"]);
            assert!(
                model.successors(action.name, &state).contains(&next),
                "{}: {state:?} -> {next:?}",
                action.name
            );
        }
    }
    // Negative control goes through authenticated durable ingestion, bypassing the
    // local convenience guard. The projection must reject the signed excessive debit.
    let (_root, a, b) = pair();
    let aa = Credentials::load(&a).unwrap().capability.actor;
    let bb = Credentials::load(&b).unwrap().capability.actor;
    let grant = Database::open(&a)
        .unwrap()
        .allocate(
            [1; 16],
            "model-window".into(),
            4,
            BTreeMap::from([(aa, 2), (bb, 2)]),
        )
        .unwrap();
    reconcile(&a, &b);
    let bad = Database::open(&b)
        .unwrap()
        .commit(Command::Spend {
            request: [2; 16],
            grant,
            cost: 3,
            cumulative: 3,
        })
        .unwrap();
    reconcile(&a, &b);
    let view = Database::open(&a).unwrap().view();
    assert!(view.invalid.contains(&bad));
    assert!(!view.spent.contains_key(&(grant, bb)));
}

#[derive(Clone)]
struct VisibleRig {
    history: database::History,
    parent: bool,
    child: bool,
    fork: bool,
}
fn visible_state(rig: &VisibleRig) -> aterm_spec::interp::State {
    let view = rig.history.view();
    BTreeMap::from([
        ("parent", i64::from(rig.parent)),
        ("child", i64::from(rig.child)),
        ("fork", i64::from(rig.fork)),
        (
            "visible_parent",
            i64::from(
                view.cells
                    .contains_key(&("example".into(), "parent".into())),
            ),
        ),
        (
            "visible_child",
            i64::from(view.cells.contains_key(&("example".into(), "child".into()))),
        ),
    ])
}
#[test]
fn authenticated_projection_conforms_to_causal_visibility_and_detects_ignored_fork_evidence() {
    let owner = crypto::random().unwrap();
    let a = crypto::random().unwrap();
    let b = crypto::random().unwrap();
    let key = crypto::random().unwrap();
    let manifest = crypto::Manifest::create(&owner, "model").unwrap();
    let mut aa = [0; 48];
    aa[..32].copy_from_slice(&crypto::public(&a).unwrap());
    aa[32..].copy_from_slice(&[1; 16]);
    let mut bb = [0; 48];
    bb[..32].copy_from_slice(&crypto::public(&b).unwrap());
    bb[32..].copy_from_slice(&[2; 16]);
    let cap_a = crypto::Capability::issue(&manifest, &owner, aa).unwrap();
    let cap_b = crypto::Capability::issue(&manifest, &owner, bb).unwrap();
    let make =
        |seed: &[u8; 32], cap: &crypto::Capability, command: Command, parents: &[[u8; 32]]| {
            let wire = crypto::seal(
                &manifest,
                &key,
                seed,
                cap,
                crypto::Causal {
                    sequence: 1,
                    previous: None,
                    parents,
                },
                &command.encode().unwrap(),
            )
            .unwrap();
            crypto::open(&manifest, &key, &wire).unwrap()
        };
    let parent = make(&a, &cap_a, write(1, "parent", json!(1)), &[]);
    let fork = make(&a, &cap_a, write(2, "fork", json!(2)), &[]);
    let child = make(&b, &cap_b, write(3, "child", json!(3)), &[parent.header.id]);
    let step = |rig: &mut VisibleRig, action: &str, ignore_fork: bool| match action {
        "ReceiveParent" => {
            rig.parent = true;
            rig.history.insert(parent.clone()).unwrap();
        }
        "ReceiveChild" => {
            rig.child = true;
            rig.history.insert(child.clone()).unwrap();
        }
        "ReceiveFork" => {
            rig.fork = true;
            if !ignore_fork {
                rig.history.insert(parent.clone()).unwrap();
                rig.history.insert(fork.clone()).unwrap();
            }
        }
        other => panic!("unknown real action {other}"),
    };
    let model = aterm_models_db::causal_visibility_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let start = VisibleRig {
        history: database::History::new(manifest.root),
        parent: false,
        child: false,
        fork: false,
    };
    let mut visited = std::collections::BTreeSet::from([visible_state(&start)]);
    let mut queue = std::collections::VecDeque::from([start.clone()]);
    while let Some(rig) = queue.pop_front() {
        let before = visible_state(&rig);
        for action in &model.actions {
            if !model.action_enabled(action.name, &before) {
                continue;
            }
            let mut next = rig.clone();
            step(&mut next, action.name, false);
            let after = visible_state(&next);
            assert!(
                model.successors(action.name, &before).contains(&after),
                "{}: {before:?} -> {after:?}",
                action.name
            );
            if visited.insert(after) {
                queue.push_back(next);
            }
        }
    }
    assert_eq!(
        visited.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );
    let mut bad = start;
    for action in ["ReceiveParent", "ReceiveChild", "ReceiveFork"] {
        step(&mut bad, action, true);
    }
    assert!(
        !model.check_invariant("ForkStopsDependents", &visible_state(&bad)),
        "negative control must catch a pipeline discarding fork evidence"
    );
}

#[test]
fn signed_unknown_payloads_are_durable_inert_evidence_and_do_not_poison_repair() {
    let (_root, a, b) = pair();
    let credentials = Credentials::load(&b).unwrap();
    let wire = crypto::seal_at(
        &credentials.manifest,
        &credentials.key,
        &credentials.seed,
        &credentials.capability,
        crypto::Causal {
            sequence: 1,
            previous: None,
            parents: &[],
        },
        br#"{"v":999,"future":"command"}"#,
        u64::MAX,
    )
    .unwrap();
    let id = aterm_digest::Sha256::digest(&wire);
    let mut db = Database::open(&b).unwrap();
    assert!(db.ingest(&wire).unwrap());
    assert!(!db.ingest(&wire).unwrap());
    assert_eq!(db.history.refusals().count(), 1);
    assert!(db.view().cells.is_empty());
    let provenance = db.provenance(&id).unwrap();
    assert_eq!(provenance["authored_at_unix_ms"].as_u64(), Some(u64::MAX));
    assert_eq!(provenance["time_source"], json!("signed-author-claim"));
    let receipt = provenance["received_at_unix_ms"].clone();
    assert!(receipt.as_u64().is_some());
    db.commit(write(1, "after-unknown", json!("works")))
        .unwrap();
    drop(db);
    reconcile(&a, &b);
    for path in [&a, &b] {
        let db = Database::open(path).unwrap();
        assert_eq!(db.history.refusals().count(), 1);
        assert_eq!(
            db.view().cells[&("example".into(), "after-unknown".into())].candidates[0].value,
            json!("works")
        );
    }
    assert_eq!(
        Database::open(&b).unwrap().provenance(&id).unwrap()["received_at_unix_ms"],
        receipt
    );
}

#[test]
fn catalog_revocation_does_not_reclaim_offline_rights_or_poison_the_authority_stream() {
    let (_root, a, b) = pair();
    let ca = Credentials::load(&a).unwrap();
    let cb = Credentials::load(&b).unwrap();
    let grant = Database::open(&a)
        .unwrap()
        .allocate(
            [1; 16],
            "window".into(),
            4,
            BTreeMap::from([(ca.capability.actor, 2), (cb.capability.actor, 2)]),
        )
        .unwrap();
    reconcile(&a, &b);
    let old = Database::open(&b)
        .unwrap()
        .commit(write(2, "old", json!(1)))
        .unwrap();
    reconcile(&a, &b);
    let merged = Database::open(&b)
        .unwrap()
        .commit(Command::Merge { request: [10; 16] })
        .unwrap();
    Database::open(&b)
        .unwrap()
        .commit(write(3, "late", json!(2)))
        .unwrap();
    Database::open(&a)
        .unwrap()
        .revoke_writes([4; 16], cb.capability.actor, Some(old))
        .unwrap();
    // B has not learned the retirement and still owns its immutable two units.
    Database::open(&b)
        .unwrap()
        .spend([5; 16], "window", 2)
        .unwrap();
    reconcile(&a, &b);
    let mut db = Database::open(&b).unwrap();
    let view = db.view();
    assert!(view.cells.contains_key(&("example".into(), "old".into())));
    assert!(!view.cells.contains_key(&("example".into(), "late".into())));
    assert!(!view.causal.ordered.contains(&merged));
    assert_eq!(view.spent[&(grant, cb.capability.actor)], 2);
    assert!(db.commit(write(6, "after-retirement", json!(3))).is_err());
    assert!(db.commit(Command::Merge { request: [11; 16] }).is_err());
    assert!(db.spend([7; 16], "window", 1).is_err());
    drop(db);
    let mut owner = Database::open(&a).unwrap();
    assert!(
        owner
            .allocate(
                [8; 16],
                "window".into(),
                4,
                BTreeMap::from([(ca.capability.actor, 4)])
            )
            .is_err()
    );
    // The catalog's withheld tail cannot enter the independent authority chain.
    owner
        .allocate(
            [9; 16],
            "another-resource-window".into(),
            1,
            BTreeMap::from([(ca.capability.actor, 1)]),
        )
        .unwrap();
}

#[test]
fn forked_receipts_remain_liabilities_and_administrative_keys_cannot_be_data_writers() {
    let (_root, a, b) = pair();
    let cb = Credentials::load(&b).unwrap();
    let mut owner = Database::open(&a).unwrap();
    let grant = owner
        .allocate(
            [1; 16],
            "window".into(),
            4,
            BTreeMap::from([(cb.capability.actor, 4)]),
        )
        .unwrap();
    drop(owner);
    reconcile(&a, &b);
    let mut db = Database::open(&b).unwrap();
    let receipt = db.spend([2; 16], "window", 2).unwrap();
    let first = crypto::open(&cb.manifest, &cb.key, db.history.wire(&receipt).unwrap()).unwrap();
    let fork = crypto::seal_at(
        &cb.manifest,
        &cb.key,
        &cb.seed,
        &cb.capability,
        crypto::Causal {
            sequence: first.header.sequence,
            previous: first.header.previous,
            parents: &first.header.parents,
        },
        &Command::Spend {
            request: [3; 16],
            grant,
            cumulative: 3,
            cost: 3,
        }
        .encode()
        .unwrap(),
        1,
    )
    .unwrap();
    db.ingest(&fork).unwrap();
    let view = db.view();
    assert!(view.causal.quarantined.contains(&cb.capability.actor));
    assert!(view.disputed_shares.contains(&(grant, cb.capability.actor)));
    assert!(view.liability[&(grant, cb.capability.actor)] >= 5);
    assert!(db.spend([4; 16], "window", 1).is_err());
    drop(db);
    reconcile(&a, &b);
    assert!(Database::open(&a).unwrap().view().liability[&(grant, cb.capability.actor)] >= 5);
    let owner = crypto::random().unwrap();
    let manifest = crypto::Manifest::create(&owner, "independent-keys").unwrap();
    assert!(crypto::Capability::issue(&manifest, &owner, manifest.control_actor()).is_err());
}

#[test]
fn revert_is_an_atomic_audited_edit_and_refuses_intervening_changes_or_resource_refunds() {
    let (_root, a, b) = pair();
    let mut db = Database::open(&a).unwrap();
    let create = db.commit(write(1, "record", json!({"value":1}))).unwrap();
    let remove = db.revert([2; 16], create).unwrap();
    assert_eq!(db.revert([2; 16], create).unwrap(), remove);
    assert_eq!(
        db.view().cells[&("example".into(), "record".into())].candidates[0].value,
        Value::Null
    );
    let restore = db.revert([3; 16], remove).unwrap();
    assert_eq!(
        db.provenance(&restore).unwrap()["reverts"],
        json!(aterm_codec::hex::encode_lower(&remove))
    );
    assert_eq!(
        db.view().cells[&("example".into(), "record".into())].candidates[0].value,
        json!({"value":1})
    );
    let edit = db.commit(write(4, "record", json!({"value":2}))).unwrap();
    db.revert([5; 16], edit).unwrap();
    assert_eq!(
        db.view().cells[&("example".into(), "record".into())].candidates[0].value,
        json!({"value":1})
    );
    drop(db);
    reconcile(&a, &b);
    Database::open(&b)
        .unwrap()
        .commit(write(6, "record", json!({"concurrent":true})))
        .unwrap();
    let latest = Database::open(&a)
        .unwrap()
        .commit(write(7, "record", json!({"another":true})))
        .unwrap();
    reconcile(&a, &b);
    let mut db = Database::open(&a).unwrap();
    assert!(db.revert([8; 16], latest).is_err());
    assert_eq!(
        db.view().cells[&("example".into(), "record".into())]
            .candidates
            .len(),
        2
    );
    let actor = db.credentials.capability.actor;
    let grant = db
        .allocate([9; 16], "window".into(), 2, BTreeMap::from([(actor, 2)]))
        .unwrap();
    let debit = db.spend([10; 16], "window", 1).unwrap();
    assert!(db.revert([11; 16], grant).is_err());
    assert!(db.revert([12; 16], debit).is_err());
    assert_eq!(db.view().spent[&(grant, actor)], 1);
    let stop = db
        .commit(Command::Write {
            request: [13; 16],
            changes: vec![Change {
                collection: "example".into(),
                key: "record".into(),
                value: None,
            }],
        })
        .unwrap();
    assert!(db.revert([14; 16], stop).is_err());

    // A wire-crafted Revert naming mismatched keys or an invalid/stopped operation is marked
    // invalid in the view, does not mutate cells, and cannot return a successful retry receipt.
    let kept = db
        .commit(write(15, "kept", json!({"value": "intact"})))
        .unwrap();
    let last_head = db
        .history
        .index()
        .project()
        .ordered
        .into_iter()
        .filter_map(|id| db.history.index().get(&id))
        .filter(|h| h.actor == actor)
        .max_by_key(|h| h.sequence)
        .unwrap()
        .clone();
    let forged = crypto::seal_at(
        &db.credentials.manifest,
        &db.credentials.key,
        &db.credentials.seed,
        &db.credentials.capability,
        crypto::Causal {
            sequence: last_head.sequence + 1,
            previous: Some(last_head.id),
            parents: &[last_head.id],
        },
        &Command::Revert {
            request: [16; 16],
            operation: create,
            changes: vec![Change {
                collection: "example".into(),
                key: "kept".into(),
                value: None,
            }],
        }
        .encode()
        .unwrap(),
        1,
    )
    .unwrap();
    let forged_id = aterm_digest::Sha256::digest(&forged);
    db.ingest(&forged).unwrap();
    assert!(db.view().invalid.contains(&forged_id));
    assert_eq!(
        db.view().cells[&("example".into(), "kept".into())].candidates[0].value,
        json!({"value": "intact"})
    );
    assert!(db.revert([16; 16], create).is_err());
    let _ = kept;
}

#[test]
fn machine_name_cli_retries_the_receipt_and_preserves_semistructured_metadata() {
    let root = tmp();
    let dir = root.path().join("db");
    local::init(&dir, "machines").unwrap();
    let key = "00112233445566778899aabbccddeeff"; // a made-up test key, not a credential: gitleaks:allow
    let run = |request: u8, name: Option<&str>| {
        let mut args = vec![
            "machine-name".to_owned(),
            "--dir".into(),
            dir.to_str().unwrap().into(),
            "--key".into(),
            key.into(),
            "--request".into(),
            aterm_codec::hex::encode_lower(&[request; 16]),
        ];
        if let Some(name) = name {
            args.extend(["--name".into(), name.into()]);
        }
        crate::cli::run(&args)
    };
    let first = run(1, None).unwrap();
    assert_eq!(first["name"], json!("m1"));
    assert_eq!(run(1, None).unwrap(), first);
    assert!(run(1, Some("m2")).is_err());
    Database::open(&dir).unwrap().commit(Command::Write{request:[2;16],changes:vec![Change{collection:"machines".into(),key:key.into(),value:Some(json!({"name":"m1","hostnames":["lab.local"],"details":{"role":{"kind":"note","value":"account worker"}}}))}]}).unwrap();
    let rename = run(3, Some("M100")).unwrap();
    assert_eq!(rename["name"], json!("m100"));
    assert_eq!(run(3, None).unwrap(), rename);
    let db = Database::open(&dir).unwrap();
    let view = db.view();
    let value = &view.cells[&("machines".into(), key.into())].candidates[0].value;
    assert_eq!(value["hostnames"], json!(["lab.local"]));
    assert_eq!(value["details"]["role"]["value"], json!("account worker"));
    assert!(view.machine_names["m1"].contains(key));
    assert!(view.machine_names["m100"].contains(key));
    let history = db.history.machine_history(key);
    let names: Vec<_> = history
        .iter()
        .map(|row| row["value"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["m1", "m1", "m100"]);

    // A non-admin replica that receives control operations cannot claim or retry
    // administrative receipts for machine-name, allocate, or control reverts.
    let (_pair_root, admin_dir, replica_dir) = pair();
    let mut admin = Database::open(&admin_dir).unwrap();
    let actor = admin.credentials.capability.actor;
    admin
        .allocate([10; 16], "tokens".into(), 10, BTreeMap::from([(actor, 10)]))
        .unwrap();
    drop(admin);
    let rename_op = crate::database::decode_hex::<32>(
        crate::cli::run(&[
            "machine-name".into(),
            "--dir".into(),
            admin_dir.to_str().unwrap().into(),
            "--key".into(),
            key.into(),
            "--name".into(),
            "m1".into(),
            "--request".into(),
            aterm_codec::hex::encode_lower(&[11; 16]),
        ])
        .unwrap()["operation"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    Database::open(&admin_dir)
        .unwrap()
        .revert([12; 16], rename_op)
        .unwrap();
    let mut replica = Database::open(&replica_dir).unwrap();
    for wire in wires(&admin_dir) {
        replica.ingest(&wire).unwrap();
    }
    assert!(
        replica
            .allocate([10; 16], "tokens".into(), 10, BTreeMap::from([(actor, 10)]))
            .is_err()
    );
    assert!(replica.revert([12; 16], rename_op).is_err());
    drop(replica);
    assert!(
        crate::cli::run(&[
            "machine-name".into(),
            "--dir".into(),
            replica_dir.to_str().unwrap().into(),
            "--key".into(),
            key.into(),
            "--name".into(),
            "m1".into(),
            "--request".into(),
            aterm_codec::hex::encode_lower(&[11; 16]),
        ])
        .is_err()
    );
}

#[test]
fn local_credentials_and_journal_refuse_control_capabilities_and_hidden_records() {
    let root = tmp();
    let dir = root.path().join("db");
    local::init(&dir, "test").unwrap();
    let creds = Credentials::load(&dir).unwrap();
    let root_seed = {
        let raw = aterm_uds::private_file::read_private(&dir, "database.json", 4096).unwrap();
        let v: Value = aterm_json::from_slice(&raw).unwrap();
        database::decode_hex::<32>(v["root_seed"].as_str().unwrap()).unwrap()
    };
    let control_cap = crypto::Capability::control(&creds.manifest, &root_seed).unwrap();
    let bad_cfg = json!({
        "manifest": aterm_codec::hex::encode_lower(creds.manifest.bytes()),
        "capability": aterm_codec::hex::encode_lower(control_cap.bytes()),
        "key": aterm_codec::hex::encode_lower(&creds.key),
        "root_seed": aterm_codec::hex::encode_lower(&root_seed),
    });
    let cfg_dir = root.path().join("bad-cfg");
    aterm_uds::private_file::create_private_dir(&cfg_dir).unwrap();
    let mut id_bytes = [0u8; 48];
    id_bytes[..32].copy_from_slice(&root_seed);
    id_bytes[32..].copy_from_slice(&creds.manifest.control_actor()[32..]);
    aterm_uds::private_file::write_private(&cfg_dir, "identity", &id_bytes).unwrap();
    aterm_uds::private_file::write_private(
        &cfg_dir,
        "database.json",
        &aterm_json::to_vec(&bad_cfg).unwrap(),
    )
    .unwrap();
    assert!(Credentials::load(&cfg_dir).is_err());

    let join_dir = root.path().join("bad-join");
    aterm_uds::private_file::create_private_dir(&join_dir).unwrap();
    aterm_uds::private_file::write_private(&join_dir, "identity", &id_bytes).unwrap();
    let bad_invite = json!({
        "manifest": aterm_codec::hex::encode_lower(creds.manifest.bytes()),
        "capability": aterm_codec::hex::encode_lower(control_cap.bytes()),
        "key": aterm_codec::hex::encode_lower(&creds.key),
    });
    assert!(
        local::join(
            &join_dir,
            &aterm_json::to_vec(&bad_invite).unwrap(),
            creds.manifest.root,
        )
        .is_err()
    );

    // A pruned prefix in operations.astream must be refused rather than silently skipped.
    Database::open(&dir)
        .unwrap()
        .commit(write(1, "k", json!(1)))
        .unwrap();
    {
        let mut log = astream_broker::BrokerLog::open_with(
            dir.join("operations.astream"),
            astream_broker::Durability::Strict,
        )
        .unwrap();
        log.retain_before(astream_wire::Offset(1)).unwrap();
    }
    assert!(Database::open(&dir).is_err());
}

fn writer_usage_record(actor: u8, sequence: u64, control: bool, valid: bool) -> crypto::Opened {
    let mut id = [actor; 32];
    id[..8].copy_from_slice(&sequence.to_le_bytes());
    crypto::Opened {
        control,
        authored_at_unix_ms: 0,
        header: astream_wire::sync::Header {
            id,
            actor: [actor; 48],
            sequence: 1,
            previous: None,
            parents: Vec::new(),
        },
        plaintext: if valid {
            write(1, "usage", json!(sequence)).encode().unwrap()
        } else {
            vec![0xff]
        },
        wire: vec![actor; 31 + usize::try_from(sequence % 17).unwrap()],
    }
}

#[test]
fn writer_usage_counts_retained_evidence_once_and_survives_clone() {
    // History receives already-opened records; authentication is outside this seam.
    let opened = writer_usage_record;
    let mut history = database::History::new([0; 32]);
    let mut expected = BTreeMap::<_, (usize, usize)>::new();
    for sequence in 0..96 {
        let record = opened(
            1 + u8::try_from(sequence % 3).unwrap(),
            sequence,
            sequence % 4 == 0,
            sequence % 5 != 0,
        );
        let actor = record.header.actor;
        let bytes = record.wire.len();
        assert!(history.insert(record.clone()).unwrap());
        assert!(!history.insert(record).unwrap());
        let usage = expected.entry(actor).or_default();
        usage.0 += 1;
        usage.1 += bytes;
        for (actor, usage) in &expected {
            assert_eq!(history.writer_usage(actor), *usage);
        }
    }
    assert!(history.refusals().count() > 0);
    assert_eq!(history.writer_usage(&[99; 48]), (0, 0));
    let mut malformed = opened(4, 100, false, true);
    malformed.header.sequence = 0;
    assert!(history.insert(malformed).is_err());
    assert_eq!(history.writer_usage(&[4; 48]), (0, 0));
    let mut cloned = history.clone();
    let extra = opened(1, 100, false, true);
    let extra_bytes = extra.wire.len();
    assert!(cloned.insert(extra).unwrap());
    let original = expected[&[1; 48]];
    assert_eq!(history.writer_usage(&[1; 48]), original);
    assert_eq!(
        cloned.writer_usage(&[1; 48]),
        (original.0 + 1, original.1 + extra_bytes)
    );

    let mut capped = database::History::new([0; 32]);
    for sequence in 0..database::MAX_WRITER_OPERATIONS as u64 {
        assert!(
            capped
                .insert(opened(8, sequence, false, sequence % 2 == 0))
                .unwrap()
        );
    }
    assert!(capped.insert(opened(8, 9000, false, true)).is_err());
    assert_eq!(
        capped.writer_usage(&[8; 48]).0,
        database::MAX_WRITER_OPERATIONS
    );
    assert!(capped.insert(opened(9, 9000, false, true)).unwrap());
    #[cfg(unix)]
    {
        // Historical control evidence can replay beyond a writer's ordinary quota.
        let historical = opened(8, 9001, true, false);
        assert!(capped.replay(historical.clone()).unwrap());
        assert!(!capped.replay(historical).unwrap());
        assert_eq!(
            capped.writer_usage(&[8; 48]).0,
            database::MAX_WRITER_OPERATIONS + 1
        );
        assert!(capped.insert(opened(8, 9002, false, true)).is_err());
    }
}

#[test]
#[ignore = "process CPU measurement of opened-history retention; no latency budget"]
fn writer_usage_ingest_measurement() {
    let cpu = || {
        let pid = std::process::id();
        aterm_sysprobe::processes()
            .unwrap()
            .procs
            .into_iter()
            .find(|process| process.pid == pid)
            .unwrap()
            .cpu_ns
            .unwrap()
    };
    const RECORDS: u64 = 8192;
    let records: Vec<_> = (0..RECORDS)
        .map(|sequence| {
            writer_usage_record(
                1 + u8::try_from(sequence % 4).unwrap(),
                sequence,
                false,
                sequence % 2 == 0,
            )
        })
        .collect();
    for round in 0..5 {
        let mut history = database::History::new([0; 32]);
        let before = cpu();
        let start = cpu();
        for record in &records {
            assert!(
                history
                    .insert(std::hint::black_box(record.clone()))
                    .unwrap()
            );
        }
        let elapsed = cpu()
            .saturating_sub(start)
            .saturating_sub(start.saturating_sub(before));
        for actor in 1..=4 {
            assert_eq!(
                history.writer_usage(&[actor; 48]).0,
                usize::try_from(RECORDS / 4).unwrap()
            );
        }
        println!(
            "{}",
            json!({"measurement":"opened_history_ingest", "records":RECORDS, "writers":4, "warmup":round == 0, "process_cpu_ns":elapsed})
        );
    }
}
