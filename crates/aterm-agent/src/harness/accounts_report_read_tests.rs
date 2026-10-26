// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn pause(store: &Store, publish: impl FnOnce() -> Result<Vec<Event>, String>) {
    let witness = store.state_witness_file();
    let retained = witness.with_extension("retained");
    std::fs::rename(&witness, &retained).unwrap();
    std::fs::create_dir(&witness).unwrap();
    assert!(publish().is_err());
    assert!(store.load(NOW).is_err(), "the pending receipt was admitted");
    std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
    std::fs::rename(&retained, &witness).unwrap();
}

fn exercise(provider: i64, shape: i64, fault: i64) -> [bool; 4] {
    let cli =
        [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok][usize::try_from(provider - 1).unwrap()];
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at_for(root.path(), cli, &root.path().join("home")).bounded_by(&[]);
    let path = root.path().join(cli.accounts().roster_file);
    let old = root.path().join("old").to_str().unwrap().to_owned();
    let new = root.path().join("new").to_str().unwrap().to_owned();
    let credentials = [&old, &new].map(|dir| PathBuf::from(dir).join(cli.connector().stores[0]));
    for credential in &credentials {
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(credential, b"opaque credential").unwrap();
    }
    let row = Row {
        label: "main".into(),
        dir: Some(old.clone()),
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            ..Vendor::default()
        },
        ..Row::default()
    };
    let populated = append_rows_for(cli, "", &[row]).unwrap();
    let raw = if shape == 2 { populated.as_str() } else { "" };
    if shape > 0 {
        std::fs::write(&path, raw).unwrap();
    }
    let renew = |dir: &str| {
        store.transact_logins(NOW, &mut None, |state, logins| {
            Ok(note_renewed(state, logins, "main", None, dir, None, NOW))
        })
    };
    renew(&old).unwrap();
    match fault {
        1 | 3 => pause(&store, || renew(&new)),
        2 => pause(&store, || {
            let held = store.acquire().unwrap();
            store.transact_sync_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &path,
                    before: (shape > 0).then_some(raw),
                    after: &populated,
                    mode: None,
                },
                |state, logins| Ok(note_login_end(state, logins, "main", NOW)),
                || Ok(()),
            )
        }),
        4 => std::fs::write(store.dir.join("sync-commit.json"), "{").unwrap(),
        5 => std::fs::rename(
            store.dir.join("sync-commit.json"),
            store.dir.join("retained-receipt"),
        )
        .unwrap(),
        6 => std::fs::write(store.state_file(), "{").unwrap(),
        7 => std::fs::write(store.logins_file(), "{").unwrap(),
        9 => std::fs::write(&path, "[[account]").unwrap(),
        _ => {}
    }
    let _held = (fault == 3).then(|| store.acquire().unwrap());
    let images = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.dir.join("sync-commit.json"),
            store.state_witness_file(),
            store.logins_witness_file(),
            path.clone(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let configured = (shape > 0).then_some(path.as_path());
    let before = images();
    let got = store.read_account_records_with(configured, NOW, || {
        if fault == 8 {
            renew(&new).unwrap();
        }
    });
    let after = images();
    let available = fault < 2 || fault == 2 && shape > 0 || fault == 9 && shape == 0;
    let matches = |records: &AccountRecords, changed: bool| {
        let dir = if changed { &new } else { &old };
        let rows = usize::from(shape == 2 || fault == 2);
        records.roster_present == (shape > 0)
            && records.roster.rows.len() == rows
            && records.raw_roster.rows.len() == rows
            && (rows == 0 || records.raw_roster.row("main").unwrap().dir.as_ref() == Some(&old))
            && (rows == 0 || records.roster.row("main").unwrap().dir.as_ref() == Some(dir))
            && records.logins.get("main").unwrap().dir.as_ref() == Some(dir)
            && records.logins.generation_clock == if changed { 2 } else { 1 }
            && records.state.account("main").auth
                == if fault == 2 {
                    Auth::SignedOut
                } else {
                    Auth::Ok
                }
    };
    let coherent = if available {
        got.as_ref()
            .is_ok_and(|records| matches(records, fault == 1))
    } else if fault == 9 {
        matches!(got, Err(RosterReadError::Roster(_)))
    } else {
        matches!(got, Err(RosterReadError::Records(_)))
    };
    let again = store.read_account_records(configured, NOW);
    let repeatable = if fault == 8 {
        again.as_ref().is_ok_and(|records| matches(records, true))
    } else {
        again == got
    } && store.read_account_records(configured, NOW) == again
        && images() == after;
    let preserved = credentials
        .iter()
        .all(|path| std::fs::read(path).unwrap() == b"opaque credential")
        && (available || fault == 8 || before == after)
        && (fault == 2 || before[7] == after[7]);
    [got.is_ok(), coherent, repeatable, preserved]
}

pub(crate) fn report_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherShape" => {
            next.insert("shape", state["shape"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Read" => {
            let flags = exercise(state["provider"], state["shape"], state["fault"]);
            next.insert("phase", 1);
            for (field, flag) in ["available", "coherent", "repeatable", "preserved"]
                .into_iter()
                .zip(flags)
            {
                next.insert(field, i64::from(flag));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_report_reads_conform_to_whole_snapshot_or_refusal() {
    let model = aterm_models_harness::account_report_read_model();
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = report_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} => {next:?}"
                );
            }
        }
    }
    let good = report_transition(&model.init_state(), "Read");
    for (field, law) in [
        ("available", "RecoverableReadAvailable"),
        ("coherent", "WholeSnapshotOrRefusal"),
        ("repeatable", "StableAfterPublication"),
        ("preserved", "CredentialsAndRefusedRecordsPreserved"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = good;
    bad.insert("fault", 8);
    assert!(!model.check_invariant("UnresolvedOrSpanningReadRefused", &bad));
}

#[test]
fn clean_absent_report_records_create_no_storage() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    let missing = root.path().join("absent.toml");
    for path in [None, Some(missing.as_path())] {
        let mut records = store.read_account_records(path, NOW).unwrap();
        // load() may mint an in-memory identity; this test concerns durable storage.
        records.state.machine.clear();
        assert_eq!(records, AccountRecords::default());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[test]
fn a_spanning_publication_dominates_even_a_roster_parse_error() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    let path = root.path().join("accounts.toml");
    std::fs::write(&path, "[[account]").unwrap();
    let read = store.read_account_records_with(Some(&path), NOW, || {
        store
            .transact(NOW, &mut None, |state| {
                Ok(note_auth(state, "main", Auth::Ok, NOW))
            })
            .unwrap();
    });
    assert!(matches!(read, Err(RosterReadError::Records(_))));
    assert!(matches!(
        store.read_account_records(Some(&path), NOW),
        Err(RosterReadError::Roster(_))
    ));
}

#[test]
fn only_the_supplied_roster_target_can_complete_its_pending_report() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(root.path()).bounded_by(&[]);
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(note_renewed(state, logins, "main", None, "/old", None, NOW))
        })
        .unwrap();
    let path = root.path().join("accounts.toml");
    let target = append_row("", "main", Class::Dedicated, Some("/old")).unwrap();
    pause(&store, || {
        let held = store.acquire().unwrap();
        store.transact_sync_held(
            &held,
            NOW,
            &mut None,
            SyncTarget {
                path: &path,
                before: None,
                after: &target,
                mode: None,
            },
            |state, logins| Ok(note_login_end(state, logins, "main", NOW)),
            || Ok(()),
        )
    });
    let receipt = std::fs::read(store.dir.join("sync-commit.json")).unwrap();
    let other = path.with_file_name("other.toml");
    for candidate in [None, Some(other.as_path())] {
        assert!(matches!(
            store.read_account_records(candidate, NOW),
            Err(RosterReadError::Records(_))
        ));
        assert!(!path.exists());
        assert!(!other.exists());
        assert_eq!(
            std::fs::read(store.dir.join("sync-commit.json")).unwrap(),
            receipt
        );
    }
    let read = store.read_account_records(Some(&path), NOW).unwrap();
    assert!(read.roster_present);
    assert_eq!(read.roster.rows.len(), 1);
    assert_eq!(read.state.account("main").auth, Auth::SignedOut);
}
