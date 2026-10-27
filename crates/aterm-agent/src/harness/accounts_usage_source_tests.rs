// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn images(store: &Store) -> Vec<Option<Vec<u8>>> {
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .iter()
    .map(|path| std::fs::read(path).ok())
    .collect()
}

pub(crate) fn usage_source_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Record" => {
            let root = aterm_tempfile::TempDir::new().unwrap();
            let store = Store::at(&root.path().join("state")).bounded_by(&[]);
            let other = Store::at(&root.path().join("other-state")).bounded_by(&[]);
            store.save(&State::default()).unwrap();
            other.save(&State::default()).unwrap();
            let case = state["case"];
            let path = if case == 12 {
                root.path().join("not/created/accounts.toml")
            } else {
                root.path().join("accounts.toml")
            };
            let row = Row {
                label: "main".into(),
                email: Some("owner@example.test".into()),
                ..Row::default()
            };
            if !matches!(case, 1 | 2 | 5 | 10 | 12) {
                std::fs::write(
                    &path,
                    if case == 3 {
                        String::new()
                    } else {
                        row_text(&row).unwrap()
                    },
                )
                .unwrap();
            }
            #[cfg(unix)]
            let path = if case == 14 {
                let first = root.path().join("first");
                std::fs::create_dir(&first).unwrap();
                std::fs::rename(&path, first.join("accounts.toml")).unwrap();
                let link = root.path().join("via");
                std::os::unix::fs::symlink(first, &link).unwrap();
                link.join("accounts.toml")
            } else {
                path
            };
            let mut who = ProcessLogin {
                login: None,
                default_home: true,
                stored_login: true,
            };
            let mut logins = Logins::default();
            if case == 15 {
                note_renewed(
                    &mut State::default(),
                    &mut logins,
                    "main",
                    None,
                    "/login/current",
                    None,
                    NOW,
                );
                store.save_logins(&logins).unwrap();
                who.login = Some("/login/current".into());
            }
            let captured = store
                .read_usage_roster((case != 1).then_some(path.as_path()), NOW)
                .unwrap();
            match case {
                5 => std::fs::write(&path, row_text(&Roster::own().rows[0]).unwrap()).unwrap(),
                6 => std::fs::rename(&path, root.path().join("retained.toml")).unwrap(),
                7 => std::fs::write(&path, "").unwrap(),
                8 => std::fs::write(
                    &path,
                    row_text(&Row {
                        email: Some("changed@example.test".into()),
                        ..row.clone()
                    })
                    .unwrap(),
                )
                .unwrap(),
                9 => {
                    note_renewed(
                        &mut State::default(),
                        &mut logins,
                        "main",
                        None,
                        "/login/new",
                        None,
                        NOW,
                    );
                    store.save_logins(&logins).unwrap();
                }
                10 => std::fs::write(&path, "[[invalid").unwrap(),
                14 => {
                    #[cfg(unix)]
                    {
                        let next_path = root.path().join("next");
                        std::fs::create_dir(&next_path).unwrap();
                        std::fs::write(next_path.join("accounts.toml"), row_text(&row).unwrap())
                            .unwrap();
                        // Retain the original link; the source is replaced without deleting it.
                        let link = root.path().join("via");
                        std::fs::rename(&link, root.path().join("retained-link")).unwrap();
                        std::os::unix::fs::symlink(next_path, link).unwrap();
                    }
                    #[cfg(not(unix))]
                    {
                        std::fs::write(&path, "[[invalid").unwrap();
                    }
                }
                15 => std::fs::rename(store.logins_file(), root.path().join("retained-logins"))
                    .unwrap(),
                16 => who.stored_login = false,
                17 => {
                    let target = Row {
                        dir: Some("/login/new".into()),
                        ..row.clone()
                    };
                    let other_row = Row {
                        label: "other".into(),
                        email: Some("other@example.test".into()),
                        ..Row::default()
                    };
                    std::fs::write(
                        &path,
                        format!(
                            "{}{}",
                            row_text(&target).unwrap(),
                            row_text(&other_row).unwrap()
                        ),
                    )
                    .unwrap();
                }
                18 => std::fs::write(
                    &path,
                    row_text(&Row {
                        class: Class::Shared,
                        ..row
                    })
                    .unwrap(),
                )
                .unwrap(),
                _ => {}
            }
            let source_before = std::fs::read(&path).ok();
            let before = images(&store);
            let other_before = images(&other);
            let bindings = store.load_logins(NOW).map(|read| read.0);
            let held = (case == 11).then(|| store.acquire().unwrap());
            let mut written = false;
            let writer = if case == 13 { &other } else { &store };
            let result = writer.observe_usage_within(&[], NOW, &captured, &who, |state| {
                written = true;
                Ok(note_wall(
                    state,
                    "main",
                    "five_hour",
                    Some(NOW + 3600),
                    NOW,
                    NOW,
                ))
            });
            drop(held);
            assert_eq!(result.is_ok(), written, "case {case}: {result:?}");
            next.insert("phase", 2);
            next.insert("written", i64::from(written));
            next.insert(
                "preserved",
                i64::from(written || (before == images(&store) && other_before == images(&other))),
            );
            next.insert(
                "source",
                i64::from(
                    source_before == std::fs::read(&path).ok()
                        && bindings == store.load_logins(NOW).map(|read| read.0),
                ),
            );
            if case == 12 {
                assert!(!root.path().join("not").exists());
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn usage_keeps_its_source_mode_and_account_through_admission() {
    let model = aterm_models_harness::usage_source_model();
    assert_eq!(model.mutant_dials(), (1..=20).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 15);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = usage_source_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (5, "written", 1, "SourceAdmission"),
        (2, "written", 0, "DefaultProgress"),
        (5, "preserved", 0, "RefusalPreserves"),
        (2, "source", 0, "SourceUnchanged"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = usage_source_transition(&state, "Record");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn missing_roster_receipts_recover_only_while_the_source_stays_missing() {
    for conflict in [false, true] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let path = root.path().join("missing/parents/accounts.toml");
        let store = Store::at(&root.path().join("state"));
        store.save(&State::default()).unwrap();
        let captured = store.read_usage_roster(Some(&path), NOW).unwrap();
        let who = ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        let witness = store.state_witness_file();
        let kept = root.path().join("retained-witness");
        std::fs::rename(&witness, &kept).unwrap();
        std::fs::create_dir(&witness).unwrap();
        assert!(
            store
                .observe_usage_within(&[], NOW, &captured, &who, |state| {
                    Ok(note_wall(
                        state,
                        "main",
                        "five_hour",
                        Some(NOW + 3600),
                        NOW,
                        NOW,
                    ))
                })
                .is_err()
        );
        assert!(store.sync_needs_recovery());
        assert!(!path.exists());
        std::fs::rename(&witness, root.path().join("retained-obstruction")).unwrap();
        std::fs::rename(&kept, &witness).unwrap();
        if conflict {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let before = images(&store);
        let result = store.read_usage_roster(Some(&path), NOW);
        if conflict {
            assert!(result.is_err());
            assert_eq!(before, images(&store));
            assert!(store.sync_needs_recovery());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        } else {
            assert!(result.unwrap().implicit());
            assert!(!store.sync_needs_recovery());
            assert!(
                store
                    .load(NOW)
                    .unwrap()
                    .0
                    .account("main")
                    .walls
                    .contains_key("five_hour")
            );
            assert!(!root.path().join("missing").exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn usage_recovers_legacy_receipts_through_the_original_parent_alias() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let path = alias.join("accounts.toml");
    std::fs::write(&path, row_text(&Roster::own().rows[0]).unwrap()).unwrap();
    let store = Store::at(&root.path().join("state"));
    store.save(&State::default()).unwrap();
    let captured = store.read_usage_roster(Some(&path), NOW).unwrap();
    let witness = store.state_witness_file();
    let kept = root.path().join("retained-witness");
    std::fs::rename(&witness, &kept).unwrap();
    std::fs::create_dir(&witness).unwrap();
    assert!(
        store
            .transact_pool_logins_within(&[], &path, NOW, &mut None, |_, state, _| {
                Ok(note_auth(state, "main", Auth::Ok, NOW))
            })
            .is_err()
    );
    std::fs::rename(&witness, root.path().join("retained-obstruction")).unwrap();
    std::fs::rename(&kept, &witness).unwrap();
    let who = ProcessLogin {
        login: None,
        default_home: true,
        stored_login: true,
    };
    store
        .observe_usage_within(&[], NOW, &captured, &who, |state| {
            Ok(note_wall(
                state,
                "main",
                "five_hour",
                Some(NOW + 3600),
                NOW,
                NOW,
            ))
        })
        .unwrap();
    assert!(!store.sync_needs_recovery());
    assert!(
        store
            .load(NOW)
            .unwrap()
            .0
            .account("main")
            .walls
            .contains_key("five_hour")
    );
}

#[cfg(unix)]
#[test]
fn an_absent_roster_cannot_alias_an_uncreated_state_file_through_a_parent_link() {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let store = Store::at(&alias.join("state"));
    let source = real.join(store.state_file().strip_prefix(&alias).unwrap());
    let captured = store.read_usage_roster(Some(&source), NOW).unwrap();
    let who = ProcessLogin {
        login: None,
        default_home: true,
        stored_login: true,
    };
    assert!(
        store
            .observe_usage_within(&[], NOW, &captured, &who, |state| {
                Ok(note_wall(
                    state,
                    "main",
                    "five_hour",
                    Some(NOW + 3600),
                    NOW,
                    NOW,
                ))
            })
            .is_err()
    );
    assert!(!store.state_file().exists());
    assert!(!store.sync_needs_recovery());
}
