// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    Bed::tree(root)
        .into_iter()
        .filter(|(path, _)| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("accounts.lock" | "signin.lock")
            )
        })
        .collect()
}

#[test]
fn new_rows_refuse_directories_retained_by_other_bindings() {
    for cli in OTHER_CLIS {
        for historical in [false, true] {
            let bed = Bed::new("retained-directory");
            bed.keep_roster(cli);
            let region = if cli == Cli::Kimi {
                "region='global'\n"
            } else {
                ""
            };
            bed.secret(&cli_fleet_file(cli), &format!(
                "[[account]]\nlabel='restored'\nemail='new@example.test'\nmachines=['m16']\n{region}"
            ));
            let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
            let target = store.login_dir("restored").unwrap();
            fs::create_dir_all(&target).unwrap();
            fs::write(target.join("retained-credential"), "inert old login").unwrap();
            let target = target.to_string_lossy().into_owned();
            let mut logins = accounts::Logins::default();
            logins.labels.insert(
                "old".into(),
                accounts::Binding {
                    dir: Some(if historical {
                        store
                            .logins_dir()
                            .join("current")
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        target.clone()
                    }),
                    was: if historical {
                        vec![Some(target)]
                    } else {
                        Vec::new()
                    },
                    ..accounts::Binding::default()
                },
            );
            store.save_logins(&logins).unwrap();
            let before = snapshot(bed._dir.path());
            let result = bed.sync(cli, None);
            assert!(
                result.is_err(),
                "{cli:?}/{historical}: adopted another login directory: {result:?}"
            );
            assert_eq!(snapshot(bed._dir.path()), before);
        }
    }
}

fn seed(case: i64, store: &accounts::Store, label: &str) {
    let mut state = accounts::State::default();
    let mut logins = accounts::Logins::default();
    match case {
        2 | 3 | 20 => {
            logins.labels.insert(
                label.into(),
                accounts::Binding {
                    dir: (case != 3).then(|| {
                        store
                            .logins_dir()
                            .join("old-login")
                            .to_string_lossy()
                            .into_owned()
                    }),
                    ..accounts::Binding::default()
                },
            );
            store.save_logins(&logins).unwrap();
            if case != 20 {
                return;
            }
            state
                .accounts
                .insert(label.into(), accounts::Account::default());
        }
        4 => {
            state.accounts.insert(
                label.into(),
                accounts::Account {
                    auth: accounts::Auth::Ok,
                    ..accounts::Account::default()
                },
            );
        }
        5 => {
            state.active = Some(label.into());
        }
        6 => {
            state.previous = Some(label.into());
        }
        7 => {
            store.request_pin(label, NOW).unwrap();
            return;
        }
        8 => {
            state.sessions.insert(
                "kept-session".into(),
                accounts::SessionRow {
                    account: label.into(),
                    at: NOW,
                    held: None,
                },
            );
        }
        9 => {
            state.probe = Some(accounts::ProbeClaim {
                account: label.into(),
                sid: "kept-session".into(),
                at: NOW,
            });
        }
        10 => {
            state.reset = Some(accounts::ResetClaim {
                account: label.into(),
                until: None,
                window: None,
                sid: "kept-session".into(),
                at: NOW,
                pid: 8,
                start: "start".into(),
                done: None,
            });
        }
        11 => {
            state.canary = Some(accounts::Canary {
                sid: "kept-session".into(),
                at: NOW,
                took: false,
            });
            state.undo.push(accounts::Undo {
                previous: Some(label.into()),
                ..accounts::Undo::default()
            });
        }
        12 | 13 | 14 | 15 | 23 => {
            store.ensure().unwrap();
            let file = match case {
                12 | 23 => store.state_file(),
                13 => store.logins_file(),
                14 => store.state_witness_file(),
                15 => store.logins_witness_file(),
                _ => unreachable!(),
            };
            fs::write(file, if matches!(case, 14 | 15) { "" } else { "{" }).unwrap();
            return;
        }
        21 => {
            logins.generation_clock = 37;
            store.save_logins(&logins).unwrap();
            store.save(&state).unwrap();
            fs::write(
                store.ledger_path(),
                accounts::Event::new(NOW, accounts::Kind::Use, label).to_json_line(),
            )
            .unwrap();
            return;
        }
        _ => return,
    }
    store.save(&state).unwrap();
}

fn sync_case(case: i64, cli: Cli) -> (i64, i64, i64) {
    let bed = Bed::new("new-label-policy");
    bed.keep_roster(cli);
    let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
    let raw_path = bed.roster(cli);
    let region = if cli == Cli::Kimi {
        "region='global'\n"
    } else {
        ""
    };
    let account = |label: &str, email: &str| {
        format!("[[account]]\nlabel='{label}'\nemail='{email}'\nmachines=['m16']\n{region}")
    };
    let mut source = account("restored", "new@example.com");
    if case == 17 {
        source.push_str(&account("collision", "collision@example.com"));
    }
    if case == 26 {
        source = account("main", "new@example.com");
    }
    if case == 27 {
        source.clear();
    }
    if case == 28 {
        source.push_str(&account("alias", "kept@example.com"));
    }
    bed.secret(&cli_fleet_file(cli), &source);
    let matched_row = Row {
        label: "restored".into(),
        email: Some("new@example.com".into()),
        dir: Some(
            store
                .login_dir("restored")
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ),
        vendor: accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            ..accounts::Vendor::default()
        },
        ..Row::default()
    };
    let kept = accounts::append_rows_for(
        cli,
        "",
        &[Row {
            label: "kept".into(),
            email: Some("kept@example.com".into()),
            dir: None,
            ..matched_row.clone()
        }],
    )
    .unwrap();
    let matched = accounts::append_rows_for(cli, "", &[matched_row]).unwrap();
    if matches!(case, 19 | 23 | 25) {
        fs::write(&raw_path, &matched).unwrap();
    }
    if case == 28 {
        fs::write(&raw_path, &kept).unwrap();
    }
    let (seed_case, label) = match case {
        16 | 32 => (2, ACCOUNTS_OWN_LABEL),
        17 => (2, "collision"),
        20 => (20, "unrelated"),
        28 => (2, "alias"),
        _ => (case, "restored"),
    };
    seed(seed_case, &store, label);
    let completed = if matches!(case, 24 | 25) {
        let staged = store.claim_staging_dir("restored").unwrap();
        let credential = staged.join(cli.connector().stores[0]);
        fs::create_dir_all(credential.parent().unwrap()).unwrap();
        fs::write(&credential, b"{}").unwrap();
        accounts::write_renewal_checkpoint_for(
            cli,
            (cli == Cli::Kimi).then_some(Region::Global),
            "restored",
            "new@example.com",
            staged.to_str().unwrap(),
            None,
            Some(accounts::Proof::Status),
            NOW - 1,
            &store.load_logins(NOW).unwrap().0,
        )
        .unwrap();
        Some(staged)
    } else {
        None
    };
    let _signing = matches!(case, 18 | 19 | 25 | 27).then(|| store.signin_now().unwrap().unwrap());
    let before = Mutex::new(snapshot(bed._dir.path()));
    let held_writer = Mutex::new(None);
    let fetch = |_: &Env, _: &str| {
        assert_eq!(case, 22);
        fs::write(&raw_path, &matched).unwrap();
        *before.lock().unwrap() = snapshot(bed._dir.path());
        Ok(source.clone())
    };
    let result = sync_cli_with_replan(
        cli,
        (&bed.home, &bed.state, &bed.aterm_toml()),
        ((case == 22).then_some("fixture.toml"), None),
        NOW,
        (&fetch, &[]),
        || {
            let free = store
                .acquire_within(&[])
                .unwrap()
                .expect("first writer released before taking sign-in lock");
            if case == 31 {
                *held_writer.lock().unwrap() = Some(free);
            } else {
                drop(free);
            }
            match case {
                29 => fs::write(&raw_path, &matched).unwrap(),
                30 => seed(2, &store, "restored"),
                32 => fs::write(&raw_path, &kept).unwrap(),
                _ => {}
            }
            *before.lock().unwrap() = snapshot(bed._dir.path());
        },
    );
    drop(held_writer.into_inner().unwrap());
    let after = snapshot(bed._dir.path());
    let before = before.into_inner().unwrap();
    if result.is_err() {
        assert_eq!(
            after, before,
            "case {case}: refusal changed files: {result:?}"
        );
        assert!(
            store.acquire_within(&[]).unwrap().is_some(),
            "writer lock leaked on refusal"
        );
        for label in ["main", "restored", "collision"] {
            assert!(
                !store.login_dir(label).unwrap().exists(),
                "a refused batch created a profile directory for {label}"
            );
        }
    }
    let published = after.get(&raw_path) != before.get(&raw_path);
    let authority = |files: BTreeMap<PathBuf, Vec<u8>>| {
        files
            .into_iter()
            .filter(|(path, _)| path != &raw_path && path != &store.wake_path())
            .collect::<BTreeMap<_, _>>()
    };
    let authority_changed = authority(after) != authority(before);
    if case == 31 {
        assert!(
            store.signin_now().unwrap().is_some(),
            "failed writer reacquisition retained sign-in lock"
        );
    }
    if let Some(staged) = completed.filter(|_| case == 24) {
        let raw = Roster::load_for(cli, &raw_path).unwrap();
        let logins = store.load_logins(NOW).unwrap().0;
        let bound = accounts::bound(&raw, &logins);
        let row = bound.row("restored").unwrap();
        assert_eq!(
            row.dir.as_deref(),
            staged.to_str(),
            "creation must release its sign-in lock before recovery"
        );
        assert_eq!(
            logins.get("restored").unwrap().identity_proof_for(row),
            Some(accounts::Proof::Status)
        );
    }
    if case == 25 {
        assert!(
            store.load_logins(NOW).unwrap().0.labels.is_empty(),
            "busy no-op recovered a live sign-in"
        );
    }
    (
        i64::from(result.is_ok()),
        i64::from(published),
        i64::from(authority_changed),
    )
}

pub(crate) fn sync_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Sync" => {
            let (accepted, published, authority) = sync_case(state["case"], Cli::Codex);
            next.insert("phase", 1);
            next.insert("accepted", accepted);
            next.insert("published", published);
            next.insert("authority", authority);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn additive_sync_conforms_to_new_label_admission() {
    let model = aterm_models_harness::account_sync_new_labels_model();
    assert_eq!(model.mutant_dials(), (1..=36).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = sync_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                checked += usize::from(action.name == "Sync");
            }
        }
    }
    assert_eq!(checked, 32);
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    bad.insert("case", 2);
    bad.insert("accepted", 1);
    assert!(!model.check_invariant("NoUnexpectedAdmission", &bad));
}

#[test]
fn each_additive_provider_obeys_admission_and_releases_recovery() {
    for cli in OTHER_CLIS {
        for case in 1..=32 {
            let good = case == 1 || matches!(case, 19..=25 | 27..=29 | 32);
            let publishes = matches!(case, 1 | 20 | 21 | 24 | 28 | 32);
            assert_eq!(
                sync_case(case, cli),
                (i64::from(good), i64::from(publishes), i64::from(case == 24)),
                "{cli:?} case {case}"
            );
        }
    }
}
