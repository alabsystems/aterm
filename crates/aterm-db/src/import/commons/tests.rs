// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    catalog::Directory,
    table::{Session, store::Store, tests::Lab},
};
use aterm_secrets::commons::roster::Role;
use aterm_spec::interp::State;

pub(crate) fn atomic_transition(state: &State, action: &str) -> State {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("accounts.csv");
    let db = Imports::at(lab.paths[0].clone());
    std::fs::write(&path, "email,google\na@example.test,1\n").unwrap();
    let mut stable = true;
    let mut private_writes = 0;
    let phase = match action {
        "Reject" => {
            assert_eq!(state["phase"], 0);
            let private = lab._home.path().join("private");
            crate::local::init(&private, "accounts").unwrap();
            for (source, expected) in [
                (
                    "email, EMAIL ,google\na@example.test,b@example.test,1\n",
                    "CSV has ambiguous identity columns",
                ),
                (
                    "email, EMAIL ,google\na@example.test,a@example.test,1\n",
                    "CSV has ambiguous identity columns",
                ),
                (
                    "email,computer, Computer ,google\na@example.test,m3.2,m100,1\n",
                    "CSV has ambiguous identity columns",
                ),
                (
                    "computer, Computer ,google\nm3.2,m100,1\n",
                    "CSV has no email column",
                ),
            ] {
                std::fs::write(&path, source).unwrap();
                assert_eq!(db.apply(&path, [1; 16]).unwrap_err().to_string(), expected);
                let before = crate::tests::wires(&private);
                assert_eq!(
                    crate::import::apply(&private, &path, [1; 16])
                        .unwrap_err()
                        .to_string(),
                    expected
                );
                let after = crate::tests::wires(&private);
                private_writes += after.len().checked_sub(before.len()).unwrap();
                assert_eq!(after, before);
                let absent = lab._home.path().join("not-created");
                assert_eq!(
                    crate::import::apply(&absent, &path, [2; 16])
                        .unwrap_err()
                        .to_string(),
                    expected
                );
                assert!(!absent.exists());
                assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            }
            1
        }
        "Commit" => {
            assert_eq!(state["phase"], 0);
            db.apply(&path, [1; 16]).unwrap();
            2
        }
        "Retry" => {
            assert_eq!(state["phase"], 2);
            let receipt = db.apply(&path, [1; 16]).unwrap();
            stable = db.apply(&path, [1; 16]).unwrap() == receipt;
            3
        }
        _ => panic!("unknown atomic import action"),
    };
    let effects = db
        .snapshot()
        .map_or(0, |snapshot| snapshot.operations.len())
        + private_writes;
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let published = directory
        .snapshot(&session)
        .unwrap()
        .entries
        .contains_key(&descriptor(&bootstrap).unwrap().scope.digest());
    State::from([
        ("phase", phase),
        ("published", i64::from(published)),
        ("effects", effects as i64),
        ("stable", i64::from(stable)),
    ])
}

#[test]
fn atomic_import_conforms_with_negative_controls() {
    let model = aterm_models_db::import_atomic_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = atomic_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
                let mut wrong = next.clone();
                wrong.insert("stable", 0);
                assert!(!model.check_invariant("OriginalReceipt", &wrong));
                assert!(!model.successors(action.name, &state).contains(&wrong));
                if action.name == "Reject" {
                    let mut written = next;
                    written.insert("effects", 1);
                    assert!(!model.check_invariant("RejectBeforeWrites", &written));
                    assert!(!model.successors(action.name, &state).contains(&written));
                }
            }
        }
    }
}

#[test]
fn two_offline_imports_repair_without_creating_account_authority() {
    use crate::control::{Endpoint, Repair, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    use std::{net::TcpListener, thread, time::Duration};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let apps = [
        Imports::at(lab.paths[0].clone()),
        Imports::at(lab.paths[1].clone()),
    ];
    for (index, app) in apps.iter().enumerate() {
        let file = lab._home.path().join(format!("source-{index}.csv"));
        std::fs::write(
            &file,
            format!(
                "email,computer,google\na{index}@example.test,m{},1\n",
                index + 1
            ),
        )
        .unwrap();
        app.apply(&file, [1; 16]).unwrap();
    }
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
    for _ in 0..3 {
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
    let snapshots = [apps[0].snapshot().unwrap(), apps[1].snapshot().unwrap()];
    assert_eq!(snapshots[0].cells, snapshots[1].cells);
    assert_eq!(snapshots[0].cells.len(), 2);
    assert!(snapshots.iter().all(|s| s.operations.len() == 2));
    for paths in &lab.paths {
        assert!(
            crate::accounts::proposals::Proposals::at(paths.clone())
                .snapshot()
                .is_err()
        );
    }
}

#[test]
fn import_is_one_atomic_retryable_inert_source_document() {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("accounts.csv");
    let original = "email,computer,google,claude,notes,payment\nOwner+Two@example.test,m3.2,email link,1,alias of private PRIVATE-NOTE,PRIVATE-PAYMENT\nowner@example.test,m100,1,,phone +12025550123,\ninvalid,unknown,1,,PRIVATE-CELL,\n";
    std::fs::write(&path, original).unwrap();
    let db = Imports::at(lab.paths[0].clone());
    let first = db.apply(&path, [1; 16]).unwrap();
    assert_eq!(first["candidate_rows"], json!(3));
    assert_eq!(first["provider_hints"], json!(4));
    assert_eq!(first["email_candidates"], json!(2));
    assert_eq!(first["credentials_activated"], json!(false));
    assert_eq!(db.apply(&path, [1; 16]).unwrap(), first);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let snapshot = db.snapshot().unwrap();
    assert_eq!(snapshot.operations.len(), 1);
    assert_eq!(snapshot.cells.len(), 1);
    let value = &snapshot.cells.values().next().unwrap().candidates[0].value;
    assert_eq!(value["rows"][0]["email"], json!("Owner+Two@example.test"));
    assert_eq!(value["rows"][0]["machine_slot_hint"], json!("m3.2"));
    assert_eq!(value["rows"][1]["machine_slot_hint"], json!("m100"));
    assert!(value["rows"][2]["email"].is_null());
    let encoded = aterm_json::to_string(value).unwrap();
    for private in [
        "PRIVATE-NOTE",
        "PRIVATE-PAYMENT",
        "PRIVATE-CELL",
        "+12025550123",
        "alias of",
        "email link",
    ] {
        assert!(!encoded.contains(private));
    }
    assert_eq!(
        snapshot.operations[0].header.actor,
        lab.bootstrap(0).request().unwrap().binding().actor()
    );
    assert_eq!(
        snapshot.operations[0].receipt.id,
        decode_hex::<32>(first["operation"].as_str().unwrap()).unwrap()
    );
    std::fs::write(&path, "email,google\nchanged@example.test,1\n").unwrap();
    assert!(db.apply(&path, [1; 16]).is_err());
    assert_eq!(db.snapshot().unwrap().operations.len(), 1);
    assert!(
        crate::accounts::proposals::Proposals::at(lab.paths[0].clone())
            .snapshot()
            .is_err(),
        "imports do not create or activate the account proposal table"
    );
}

#[test]
fn import_snapshot_reads_beside_control_catalog_and_application_writers() {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("accounts.csv");
    std::fs::write(&path, "email,google\nreader@example.test,1\n").unwrap();
    let db = Imports::at(lab.paths[0].clone());
    let imported = db.apply(&path, [7; 16]).unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let catalog_scope = directory.catalog_scope();
    let catalog_path = directory.path(catalog_scope).unwrap();
    let catalog = Store::open(&session, &catalog_path, catalog_scope).unwrap();
    let scope = descriptor(&bootstrap).unwrap().scope;
    let table_path = directory.path(scope).unwrap();
    let table = Store::open(&session, &table_path, scope).unwrap();
    let before = std::fs::read(table_path.join("table.astream")).unwrap();
    let snapshot = db.snapshot().unwrap();
    assert_eq!(snapshot.operations.len(), 1);
    assert_eq!(snapshot.retained, 1);
    assert_eq!(snapshot.withheld, 0);
    assert_eq!(
        snapshot.operations[0].header.id,
        decode_hex::<32>(imported["operation"].as_str().unwrap()).unwrap()
    );
    assert_eq!(
        std::fs::read(table_path.join("table.astream")).unwrap(),
        before
    );
    assert_eq!(catalog.scope(), catalog_scope);
    assert_eq!(table.scope(), scope);
}

#[test]
fn invalid_and_oversize_documents_are_refused_before_table_publication() {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("accounts.csv");
    let db = Imports::at(lab.paths[0].clone());
    std::fs::write(&path, "email,email\na@example.test,b@example.test\n").unwrap();
    assert!(db.apply(&path, [1; 16]).is_err());
    let mut csv = "email,google\n".to_owned();
    for index in 0..1500 {
        csv.push_str(&format!("owner{index}@example.test,1\n"));
    }
    std::fs::write(&path, csv).unwrap();
    assert!(db.apply(&path, [2; 16]).is_err());
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    assert!(
        !directory
            .snapshot(&session)
            .unwrap()
            .entries
            .contains_key(&descriptor(&bootstrap).unwrap().scope.digest())
    );
}

#[test]
fn signed_unknown_fields_cannot_smuggle_secrets_into_import_projection() {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("accounts.csv");
    std::fs::write(&path, "email,google\na@example.test,1\n").unwrap();
    let db = Imports::at(lab.paths[0].clone());
    db.apply(&path, [1; 16]).unwrap();
    let snapshot = db.snapshot().unwrap();
    let ((_, key), cell) = snapshot.cells.iter().next().unwrap();
    let mut value = cell.candidates[0].value.clone();
    value
        .as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .insert("password".into(), json!("PRIVATE-CREDENTIAL"));
    let command = Command::Write {
        request: [2; 16],
        changes: vec![Change {
            collection: "sources".into(),
            key: key.clone(),
            value: Some(value),
        }],
    };
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let descriptor = descriptor(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let mut store = Store::open(
        &session,
        &directory.path(descriptor.scope).unwrap(),
        descriptor.scope,
    )
    .unwrap();
    store
        .commit_catalog(&session, &command.encode().unwrap())
        .unwrap();
    drop((store, session));
    let snapshot = db.snapshot().unwrap();
    assert_eq!(snapshot.operations.len(), 2);
    assert!(snapshot.operations.iter().any(|op| op.refusal.is_some()));
    assert!(
        !aterm_json::to_string(&snapshot.cells.values().next().unwrap().candidates[0].value)
            .unwrap()
            .contains("PRIVATE-CREDENTIAL")
    );
    assert!(
        db.apply(&path, [3; 16]).is_err(),
        "refused local history requires recovery"
    );
}
