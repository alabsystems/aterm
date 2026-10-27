// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn exercise(cli: Cli, reader: i64, fault: i64, shape: i64) -> [bool; 4] {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store =
        Store::at_for(&root.path().join("state"), cli, &root.path().join("home")).bounded_by(&[]);
    let path = root.path().join(cli.accounts().roster_file);
    let login = root.path().join("login").to_str().unwrap().to_owned();
    let previous = root.path().join("previous").to_str().unwrap().to_owned();
    let credential = PathBuf::from(&login).join(cli.connector().stores[0]);
    let previous_credential = PathBuf::from(&previous).join(cli.connector().stores[0]);
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::create_dir_all(previous_credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, b"retained credential").unwrap();
    std::fs::write(&previous_credential, b"previous credential").unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(note_renewed(
                state, logins, "main", None, &previous, None, NOW,
            ))
        })
        .unwrap();
    let before_rows: Vec<_> = (0..shape)
        .map(|index| Row {
            label: format!("before-{index}"),
            dir: Some(
                root.path()
                    .join(format!("before-{index}"))
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
            vendor: Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..Vendor::default()
            },
            ..Row::default()
        })
        .collect();
    let raw_before = append_rows_for(cli, "", &before_rows).unwrap();
    std::fs::write(&path, &raw_before).unwrap();
    let mut rows = vec![Row {
        label: "main".into(),
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            ..Vendor::default()
        },
        ..Row::default()
    }];
    let second = root.path().join("second").to_str().unwrap().to_owned();
    if reader == 3 {
        rows.push(Row {
            label: "second".into(),
            dir: Some(second.clone()),
            vendor: Vendor {
                region: (cli == Cli::Kimi).then_some(Region::Global),
                ..Vendor::default()
            },
            ..Row::default()
        });
    }
    let after = append_rows_for(cli, "", &rows).unwrap();
    let witness = store.state_witness_file();
    let retained = witness.with_extension("retained");
    if fault > 1 {
        std::fs::rename(&witness, &retained).unwrap();
        std::fs::create_dir(&witness).unwrap();
    }
    let held = store.acquire().unwrap();
    let written = store.transact_sync_held(
        &held,
        NOW,
        &mut None,
        SyncTarget {
            path: &path,
            before: Some(&raw_before),
            after: Some(&after),
            mode: None,
        },
        |state, logins| {
            Ok(note_renewed(
                state,
                logins,
                "main",
                Some(&previous),
                &login,
                None,
                NOW,
            ))
        },
        || Ok(()),
    );
    assert_eq!(written.is_err(), fault > 1);
    drop(held);
    if fault > 1 {
        std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        std::fs::rename(&retained, &witness).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw_before);
    }
    let _held = (fault == 3).then(|| store.acquire().unwrap());
    if fault == 4 {
        std::fs::write(&path, "# conflicting external edit\n").unwrap();
    }
    if fault == 5 {
        std::fs::write(store.dir.join("sync-commit.json"), "{").unwrap();
    }
    let images = || {
        [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.dir.join("sync-commit.json"),
            path.clone(),
            store.state_witness_file(),
            store.logins_witness_file(),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let read = || match reader {
        0 | 1 => tap_roster((reader == 1).then_some(path.as_path()), &store, NOW).map(|roster| {
            roster
                .rows
                .iter()
                .map(|row| {
                    format!(
                        "{}={}",
                        row.label,
                        row.dir.as_deref().unwrap_or("<default>")
                    )
                })
                .collect::<Vec<_>>()
        }),
        2 => lone_login_at(Some(&path), &store, NOW).map(|login| vec![login]),
        3 | 4 => {
            let logins = listed_logins_at(Some(&path), &store, NOW);
            (!logins.is_empty()).then_some(logins)
        }
        _ => unreachable!(),
    };
    let available = fault <= 2 && (matches!(reader, 1..=3) || reader == 0 && fault == 1);
    let expected = available.then(|| match reader {
        0 => vec![format!(
            "{}=<default>",
            super::super::cli::ACCOUNTS_OWN_LABEL
        )],
        1 => vec![format!("main={login}")],
        2 => vec![login],
        3 => vec![login, previous, second],
        _ => unreachable!(),
    });
    let before = images();
    let got = read();
    let after_read = images();
    let coherent = got == expected
        && (!(fault <= 2 && reader > 0) || std::fs::read_to_string(&path).unwrap() == after);
    let stable = read() == got
        && images() == after_read
        && (reader > 0 && fault == 2 || before == after_read);
    let preserved = std::fs::read(&credential).unwrap() == b"retained credential"
        && std::fs::read(&previous_credential).unwrap() == b"previous credential";
    [got.is_some(), coherent, stable, preserved]
}

pub(crate) fn tap_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherReader" => {
            next.insert("reader", state["reader"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "AnotherShape" => {
            next.insert("shape", state["shape"] + 1);
        }
        "Read" => {
            let flags = exercise(
                Cli::ALL[usize::try_from(state["provider"] - 1).unwrap()],
                state["reader"],
                state["fault"],
                state["shape"],
            );
            next.insert("phase", 1);
            for (field, value) in ["available", "coherent", "stable", "preserved"]
                .into_iter()
                .zip(flags)
            {
                next.insert(field, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn tap_readers_conform_to_coherent_recovery() {
    let model = aterm_models_harness::account_tap_read_model();
    assert_eq!(model.mutant_dials(), (1..=5).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = tap_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    let complete = tap_transition(&model.init_state(), "Read");
    for (field, law) in [
        ("available", "ReplayableReadAvailable"),
        ("coherent", "ExactAccountProjection"),
        ("stable", "StableRepeatAndRefusal"),
        ("preserved", "CredentialsPreserved"),
    ] {
        let mut bad = complete.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad));
    }
    let mut bad = complete;
    bad.insert("fault", 3);
    assert!(!model.check_invariant("UnresolvedReadRefused", &bad));
}

#[test]
fn no_roster_path_never_guesses_a_pending_recovery_target() {
    for cli in Cli::ALL {
        for fault in 1..=5 {
            for shape in 0..=2 {
                let flags = exercise(cli, 0, fault, shape);
                assert_eq!(
                    flags,
                    [fault == 1, true, true, true],
                    "{cli:?}, {fault}, {shape}"
                );
            }
        }
    }
}

#[test]
fn one_account_with_history_is_not_a_multi_account_roster() {
    for cli in Cli::ALL {
        for fault in 1..=5 {
            for shape in 0..=2 {
                assert_eq!(exercise(cli, 4, fault, shape), [false, true, true, true]);
            }
        }
    }
}

#[test]
fn ordinary_tap_reads_create_no_account_storage() {
    for cli in Cli::ALL {
        for contents in [None, Some("")] {
            let root = aterm_tempfile::TempDir::new().unwrap();
            let path = root.path().join(cli.accounts().roster_file);
            if let Some(contents) = contents {
                std::fs::write(&path, contents).unwrap();
            }
            let store = Store::at_for(&root.path().join("state"), cli, &root.path().join("home"));
            for path in [None, Some(path.as_path())] {
                assert_eq!(tap_roster(path, &store, NOW), Some(Roster::own()));
                assert_eq!(lone_login_at(path, &store, NOW), None);
                assert!(listed_logins_at(path, &store, NOW).is_empty());
                assert!(!store.dir.exists());
            }
        }
    }
}
