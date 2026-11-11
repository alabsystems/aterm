// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    catalog::Directory,
    table::{Session, store::Store, tests::Lab},
};
use aterm_secrets::commons::roster::Role;
use aterm_spec::interp::State;

#[test]
fn causal_joins_preserve_import_sources_and_allow_the_next_import() {
    let lab = Lab::new(&[Role::Writer]);
    let source = lab._home.path().join("first.csv");
    std::fs::write(&source, "email,google\nfirst@example.test,1\n").unwrap();
    let db = Imports::at(lab.paths[0].clone());
    let receipt = db.apply(&source, [41; 16]).unwrap();
    let before = db.snapshot().unwrap();
    let merged = crate::documents::application::with_store(
        &lab.paths[0],
        false,
        descriptor,
        |session, store, _| {
            store.commit_catalog(session, &Command::Merge { request: [42; 16] }.encode()?)
        },
    )
    .unwrap();
    let after = db.snapshot().unwrap();
    assert!(after.history_complete());
    assert_eq!(after.retained, before.retained + 1);
    assert_eq!(after.cells, before.cells);
    assert!(
        after
            .operations
            .iter()
            .any(|operation| operation.header.id == merged.id && operation.eligible)
    );
    assert_eq!(db.apply(&source, [41; 16]).unwrap(), receipt);
    assert_eq!(db.snapshot().unwrap().retained, after.retained);

    let next = lab._home.path().join("second.csv");
    std::fs::write(&next, "email,google\nsecond@example.test,1\n").unwrap();
    db.apply(&next, [43; 16]).unwrap();
    let final_snapshot = db.snapshot().unwrap();
    assert!(final_snapshot.history_complete());
    assert_eq!(final_snapshot.cells.len(), 2);
    for (key, cell) in before.cells {
        assert_eq!(final_snapshot.cells.get(&key), Some(&cell));
    }
}

#[test]
fn structural_fallback_keeps_noncanonical_and_foreign_records_refused() {
    let merge = Command::Merge { request: [46; 16] };
    let mut trailing = merge.encode().unwrap();
    trailing.push(b'\n');
    let mut extended: Value = aterm_json::from_slice(&merge.encode().unwrap()).unwrap();
    extended
        .as_object_mut()
        .unwrap()
        .insert("extra".into(), json!("unrecognized"));
    let foreign = Command::Write {
        request: [47; 16],
        changes: vec![Change {
            collection: "other".into(),
            key: "item".into(),
            value: Some(json!(true)),
        }],
    };
    for payload in [
        trailing,
        aterm_json::to_vec(&extended).unwrap(),
        foreign.encode().unwrap(),
    ] {
        let lab = Lab::new(&[Role::Writer]);
        let source = lab._home.path().join("source.csv");
        std::fs::write(&source, "email,google\nfirst@example.test,1\n").unwrap();
        let db = Imports::at(lab.paths[0].clone());
        db.apply(&source, [48; 16]).unwrap();
        let before = db.snapshot().unwrap();
        let refused = crate::documents::application::with_store(
            &lab.paths[0],
            false,
            descriptor,
            |session, store, _| store.commit_catalog(session, &payload),
        )
        .unwrap();
        let snapshot = db.snapshot().unwrap();
        assert!(!snapshot.history_complete());
        assert_eq!(snapshot.cells, before.cells);
        let operation = snapshot
            .operations
            .iter()
            .find(|op| op.header.id == refused.id)
            .unwrap();
        assert!(!operation.eligible);
        assert_eq!(
            operation.refusal.as_deref(),
            Some(decode(&payload).unwrap_err().to_string().as_str())
        );
        assert!(db.apply(&source, [49; 16]).is_err());
    }
}

fn cli_in_home(home: &Path, source: &Path, command: &str) -> Value {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "import::commons::tests::child_import_cli",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", home)
        .env("ATERM_TEST_IMPORT_SOURCE", source)
        .env("ATERM_TEST_IMPORT_COMMAND", command);
    let output =
        aterm_uds::deadline::output_within(&mut child, std::time::Duration::from_secs(60)).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result = stdout
        .lines()
        .find_map(|line| {
            line.split_once("import-cli-result:")
                .map(|(_, value)| value)
        })
        .expect("child reported its command result");
    aterm_json::from_str(result).unwrap()
}

#[test]
#[ignore = "child process fixture, invoked by the import preview and conformance tests"]
fn child_import_cli() {
    let home = std::env::var_os("HOME").unwrap();
    assert_eq!(Paths::live().unwrap().home, Path::new(&home));
    let source = std::env::var("ATERM_TEST_IMPORT_SOURCE").unwrap();
    let command = std::env::var("ATERM_TEST_IMPORT_COMMAND").unwrap();
    assert!(matches!(
        command.as_str(),
        "accounts-preview-csv"
            | "accounts-import-csv"
            | "preview-with-request"
            | "accounts-compare-csv"
            | "compare-without-store"
    ));
    let verb = if command == "preview-with-request" {
        "accounts-preview-csv"
    } else if command == "compare-without-store" {
        "accounts-compare-csv"
    } else {
        &command
    };
    let mut args = vec![verb.into(), "--file".into(), source];
    if command == "accounts-import-csv" || command == "preview-with-request" {
        args.extend(["--request".into(), "52".repeat(16)]);
    }
    let value = if command == "preview-with-request" || command == "compare-without-store" {
        assert!(crate::cli::run(&args).is_err());
        json!({"refused":true})
    } else {
        crate::cli::run(&args).unwrap()
    };
    println!(
        "import-cli-result:{}",
        aterm_json::to_string(&value).unwrap()
    );
}

#[test]
fn comparison_refuses_unestablished_commons_without_initializing_it() {
    let settled = Lab::new(&[Role::Writer]);
    let source = settled._home.path().join("source.csv");
    std::fs::write(&source, "email,google\nprivate@example.test,1\n").unwrap();
    let db = Imports::at(settled.paths[0].clone());
    db.apply(&source, [45; 16]).unwrap();
    let before = crate::tests::files(&settled.paths[0].home);
    let report = db.compare(&source).unwrap();
    assert_eq!(
        report["comparisons"][0]["exact_candidate_match"],
        json!(true)
    );
    assert_eq!(report["history_complete"], json!(true));
    assert_eq!(report["authority"], json!("none"));
    assert_eq!(crate::tests::files(&settled.paths[0].home), before);

    let pending = Lab::new(&[Role::Writer]);
    let paths = &pending.paths[0];
    assert!(matches!(
        aterm_secrets::commons::observe(paths).unwrap(),
        aterm_secrets::commons::Observation::Visible(_)
    ));
    let commons = paths.dir.join("commons");
    assert!(!paths.dir.join("commons.claim").exists());
    assert!(!commons.join("default.pin").exists());
    assert!(!commons.join("control").exists());
    let before = crate::tests::files(&paths.home);
    assert!(Imports::at(paths.clone()).compare(&source).is_err());
    assert_eq!(crate::tests::files(&paths.home), before);
    assert!(!paths.dir.join("commons.claim").exists());
    assert!(!commons.join("default.pin").exists());
    assert!(!commons.join("control").exists());
}

#[test]
fn comparison_cli_is_read_only_in_existing_and_unenrolled_homes() {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("source.csv");
    std::fs::write(&path, "email,google\nprivate@example.test,1\n").unwrap();
    let db = Imports::at(lab.paths[0].clone());
    db.apply(&path, [44; 16]).unwrap();
    let before = crate::tests::files(&lab.paths[0].home);
    let report = cli_in_home(&lab.paths[0].home, &path, "accounts-compare-csv");
    assert_eq!(
        report["comparisons"][0]["exact_candidate_match"],
        json!(true)
    );
    assert_eq!(report["history_complete"], json!(true));
    assert_eq!(report["authority"], json!("none"));
    assert!(
        !aterm_json::to_string(&report)
            .unwrap()
            .contains("private@example.test")
    );
    assert_eq!(crate::tests::files(&lab.paths[0].home), before);
    cli_in_home(&lab.paths[0].home, &path, "accounts-import-csv");
    assert_ne!(crate::tests::files(&lab.paths[0].home), before);
    let unenrolled = lab._home.path().join("unenrolled");
    std::fs::create_dir(&unenrolled).unwrap();
    assert_eq!(
        cli_in_home(&unenrolled, &path, "compare-without-store")["refused"],
        json!(true)
    );
    assert_eq!(std::fs::read_dir(unenrolled).unwrap().count(), 0);
}

pub(crate) fn atomic_transition(state: &State, action: &str) -> State {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("accounts.csv");
    let db = Imports::at(lab.paths[0].clone());
    std::fs::write(&path, "email,google\na@example.test,1\n").unwrap();
    let mut stable = true;
    let mut private_writes = 0;
    let phase = match action {
        "Preview" => {
            assert_eq!(state["phase"], 0);
            let viewed = cli_in_home(&lab.paths[0].home, &path, "accounts-preview-csv");
            assert_eq!(viewed, preview(&path).unwrap());
            assert_eq!(
                viewed["rows"][0]["providers"][0]["presence_marker"],
                json!(true)
            );
            4
        }
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
                    let mut written = next.clone();
                    written.insert("effects", 1);
                    assert!(!model.check_invariant("RejectBeforeWrites", &written));
                    assert!(!model.successors(action.name, &state).contains(&written));
                }
                if action.name == "Preview" {
                    for (field, law) in [
                        ("published", "PreviewBeforePublication"),
                        ("effects", "PreviewBeforeWrites"),
                    ] {
                        let mut written = next.clone();
                        written.insert(field, 1);
                        assert!(!model.check_invariant(law, &written));
                        assert!(!model.successors(action.name, &state).contains(&written));
                    }
                }
            }
        }
    }
}

#[test]
fn preview_cli_needs_only_the_source_and_does_not_create_a_database() {
    let root = crate::tests::tmp();
    let path = root.path().join("source.csv");
    let source =
        "email,computer,google,notes\nCase+tag@example.test,m3.2,phone required,PRIVATE-NOTE\n";
    std::fs::write(&path, source).unwrap();
    let home = root.path().join("unenrolled");
    std::fs::create_dir(&home).unwrap();
    let value = cli_in_home(&home, &path, "accounts-preview-csv");
    assert_eq!(value["rows"][0]["email"], json!("Case+tag@example.test"));
    assert_eq!(value["rows"][0]["machine_slot_hint"], json!("m3.2"));
    assert_eq!(value["rows"][0]["providers"], json!([]));
    let encoded = aterm_json::to_string(&value).unwrap();
    assert!(!encoded.contains("PRIVATE-NOTE"));
    assert!(!encoded.contains("phone required"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    assert_eq!(
        cli_in_home(&home, &path, "preview-with-request")["refused"],
        json!(true)
    );
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
}

#[test]
fn preview_process_witness_observes_real_import_writes() {
    let lab = Lab::new(&[Role::Writer]);
    let path = lab._home.path().join("source.csv");
    std::fs::write(&path, "email,google\ncontrol@example.test,1\n").unwrap();
    let db = Imports::at(lab.paths[0].clone());
    assert!(db.snapshot().is_err());
    let value = cli_in_home(&lab.paths[0].home, &path, "accounts-preview-csv");
    assert!(db.snapshot().is_err());
    let receipt = cli_in_home(&lab.paths[0].home, &path, "accounts-import-csv");
    let snapshot = db.snapshot().unwrap();
    assert_eq!(snapshot.operations.len(), 1);
    assert_eq!(
        snapshot.cells.values().next().unwrap().candidates[0].value,
        value
    );
    assert_eq!(
        snapshot.operations[0].receipt.id,
        decode_hex::<32>(receipt["operation"].as_str().unwrap()).unwrap()
    );
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
                    Connection::admit(
                        aterm_uds::accept(
                            &listener,
                            aterm_uds::AcceptWait::Forever,
                            aterm_uds::AcceptedMode::Blocking,
                        )
                        .unwrap()
                        .0,
                        LinkScope::Loopback,
                    )
                    .unwrap(),
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
    let previewed = preview(&path).unwrap();
    let db = Imports::at(lab.paths[0].clone());
    let first = db.apply(&path, [1; 16]).unwrap();
    assert_eq!(first["candidate_rows"], json!(3));
    assert_eq!(first["provider_hints"], json!(3));
    assert_eq!(first["email_candidates"], json!(2));
    assert_eq!(first["credentials_activated"], json!(false));
    assert_eq!(db.apply(&path, [1; 16]).unwrap(), first);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let snapshot = db.snapshot().unwrap();
    assert_eq!(snapshot.operations.len(), 1);
    assert_eq!(snapshot.cells.len(), 1);
    let value = &snapshot.cells.values().next().unwrap().candidates[0].value;
    assert_eq!(
        &previewed, value,
        "preview and publication use the same candidate"
    );
    assert_eq!(value["rows"][0]["email"], json!("Owner+Two@example.test"));
    assert_eq!(value["rows"][0]["machine_slot_hint"], json!("m3.2"));
    assert_eq!(
        value["rows"][0]["providers"],
        json!([{"provider":"anthropic","column":4,"presence_marker":true}]),
        "a free-form Google note is not a negative account declaration"
    );
    assert!(
        value["rows"][0]["unclassified_columns"]
            .as_array()
            .unwrap()
            .contains(&json!(3)),
        "the Google note keeps its source coordinate"
    );
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
    assert_eq!(
        &cli_in_home(&lab.paths[0].home, &path, "accounts-preview-csv"),
        value
    );
    let after_preview = db.snapshot().unwrap();
    assert_eq!(after_preview.cells, snapshot.cells);
    assert_eq!(after_preview.operations.len(), snapshot.operations.len());
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
    assert!(preview(&path).is_err());
    assert!(db.apply(&path, [1; 16]).is_err());
    let mut csv = "email,google\n".to_owned();
    for index in 0..1500 {
        csv.push_str(&format!("owner{index}@example.test,1\n"));
    }
    std::fs::write(&path, csv).unwrap();
    assert!(preview(&path).is_err());
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

/// An `accounts/imports` row 0.146 or 0.147 replicated keeps validating on this build: those
/// builds read `m40-1234` as slot 1234 and machine numbers past six digits as machines, which
/// the narrowed label grammar no longer does, so the validator keeps their grammar. A hint no
/// build ever wrote is still refused.
/// NEGATIVE CONTROL: with the validator reading the hint through `Slot::parse`, the 0.146 row
/// is refused and this fails at `validate(&key, &row("m40-1234"))`: `` called
/// `Result::unwrap()` on an `Err` value ``.
#[test]
fn rows_older_builds_replicated_keep_validating() {
    let key = "ab".repeat(32);
    let row = |hint: &str| {
        json!({"schema": 1, "source_sha256": key, "records": 1, "rows": [{
            "record": 2, "email": null, "email_column": 1, "machine_slot_hint": hint,
            "machine_column": 2, "providers": [], "unclassified_columns": [],
        }]})
    };
    for hint in ["m40-1234", "m1234567.2", "m3", "M27-2", "m3.2", "m0"] {
        validate(&key, &row(hint)).unwrap();
    }
    for hint in ["m3.0", "m27-02", "m40-3fa2", "mac-3", "m3.2.1"] {
        assert!(validate(&key, &row(hint)).is_err(), "{hint}");
    }
}

/// AN IMPORT TABLE NOBODY DECLARED IS NO FAILURE TO READ ONE: `snapshot_if_declared` is `None` on a
/// Mac with no Commons, and in a Commons that declared only another table, where `snapshot` refuses.
/// CONTROL: once an import declares it, it reads what `snapshot` reads.
#[test]
fn an_import_table_nobody_declared_reads_as_none() {
    let lab = Lab::new(&[Role::Writer]);
    let db = Imports::at(lab.paths[0].clone());
    let no_commons = aterm_secrets::client::Paths::for_home(&lab._home.path().join("no-commons"));
    assert!(
        Imports::at(no_commons)
            .snapshot_if_declared()
            .unwrap()
            .is_none(),
        "no Commons"
    );
    crate::accounts::proposals::Proposals::at(lab.paths[0].clone())
        .write(
            [1; 16],
            vec![crate::database::Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(aterm_json::json!({
                    "provider":"google","method":"email-link","plan":"initial",
                    "email":"first@example.com",
                    "auth":[{"kind":"email-link","mailbox":"inbox","requirements":["email-access"]}]
                })),
            }],
        )
        .unwrap();
    assert!(db.snapshot().is_err(), "PRECONDITION: undeclared");
    assert!(
        db.snapshot_if_declared().unwrap().is_none(),
        "another table"
    );
    let source = lab._home.path().join("first.csv");
    std::fs::write(&source, "email,google\nfirst@example.com,1\n").unwrap();
    db.apply(&source, [41; 16]).unwrap();
    let declared = db.snapshot_if_declared().unwrap().expect("declared");
    assert_eq!(declared.cells, db.snapshot().unwrap().cells, "CONTROL");
}
