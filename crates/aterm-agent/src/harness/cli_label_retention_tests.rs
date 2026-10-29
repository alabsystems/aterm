// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

fn data_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    every_file(root)
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
fn accounts_add_refuses_a_directory_retained_by_another_label() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for historical in [false, true] {
            let tmp = tmp_dir("add-retained-directory");
            let env = env_at(&tmp);
            std::fs::write(env.config.as_ref().unwrap(), "").unwrap();
            let store = env.accounts_store_for(cli).unwrap();
            let dir = store.login_dir("work").unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("retained-credential"), "inert old login").unwrap();
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
                        dir.to_string_lossy().into_owned()
                    }),
                    was: if historical {
                        vec![Some(dir.to_string_lossy().into_owned())]
                    } else {
                        Vec::new()
                    },
                    ..accounts::Binding::default()
                },
            );
            store.save_logins(&logins).unwrap();
            let before = data_files(tmp.path());
            let result = accounts_add(
                cli,
                &env,
                &accounts::Row {
                    label: "work".into(),
                    ..accounts::Row::default()
                },
            );
            assert!(
                result.is_err(),
                "{cli:?}/{historical}: accepted a retained directory"
            );
            assert_eq!(data_files(tmp.path()), before);
        }
    }
}

fn seed(case: i64, store: &accounts::Store, label: &str, now: i64) {
    let mut state = accounts::State::default();
    let mut logins = accounts::Logins::default();
    match case {
        2 | 3 | 18 => {
            logins.labels.insert(
                label.into(),
                accounts::Binding {
                    dir: (case != 3).then(|| {
                        store
                            .logins_dir()
                            .join("retained")
                            .to_string_lossy()
                            .into_owned()
                    }),
                    ..accounts::Binding::default()
                },
            );
            store.save_logins(&logins).unwrap();
            if case != 18 {
                return;
            }
            state
                .accounts
                .insert(label.into(), accounts::Account::default());
        }
        4 => {
            state
                .accounts
                .insert(label.into(), accounts::Account::default());
        }
        5 => {
            state.active = Some(label.into());
        }
        6 => {
            state.previous = Some(label.into());
        }
        7 => {
            store.request_pin(label, now).unwrap();
            return;
        }
        8 => {
            state.sessions.insert(
                "old-session".into(),
                accounts::SessionRow {
                    account: label.into(),
                    at: now,
                    held: None,
                },
            );
        }
        9 => {
            state.probe = Some(accounts::ProbeClaim {
                account: label.into(),
                sid: "old-session".into(),
                at: now,
            });
        }
        10 => {
            state.reset = Some(accounts::ResetClaim {
                account: label.into(),
                until: None,
                window: None,
                sid: "old-session".into(),
                at: now,
                pid: 7,
                start: "old-start".into(),
                done: None,
            });
        }
        11 => {
            state.canary = Some(accounts::Canary {
                sid: "old-session".into(),
                at: now,
                took: false,
            });
            state.undo.push(accounts::Undo {
                previous: Some(label.into()),
                ..accounts::Undo::default()
            });
        }
        12..=17 => {
            store.ensure().unwrap();
            let path = match case {
                12 | 16 => store.state_file(),
                13 | 17 => store.logins_file(),
                14 => store.state_witness_file(),
                15 => store.logins_witness_file(),
                _ => unreachable!(),
            };
            let contents = match case {
                12 | 13 => b"{".to_vec(),
                14 | 15 => Vec::new(),
                16 => vec![b' '; usize::try_from(accounts::MAX_STATE_BYTES + 1).unwrap()],
                17 => vec![b' '; usize::try_from(accounts::MAX_LOGINS_BYTES + 1).unwrap()],
                _ => unreachable!(),
            };
            std::fs::write(path, contents).unwrap();
            return;
        }
        19 => {
            logins.generation_clock = 19;
            store.save_logins(&logins).unwrap();
            store.save(&accounts::State::default()).unwrap();
            std::fs::write(
                store.ledger_path(),
                accounts::Event::new(now, accounts::Kind::Use, label).to_json_line(),
            )
            .unwrap();
            return;
        }
        _ => return,
    }
    store.save(&state).unwrap();
}

fn creation_case(case: i64, cli: Cli) -> (i64, i64, i64) {
    let tmp = tmp_dir("create-label");
    let env = env_at(&tmp);
    let store = env.accounts_store_for(cli).unwrap();
    let path = env.accounts_roster_for(cli).unwrap();
    let fleet = tmp
        .path()
        .join(crate::harness::accounts_sync::FLEET_ROSTER_FILE);
    std::fs::write(env.config.as_ref().unwrap(), "[fleet]\nmachine='m3'\n").unwrap();
    std::fs::write(&fleet, "opt_in='named'\n[[account]]\nlabel='old'\nemail='old@example.test'\nclass='dedicated'\nmachines=['m3']\n").unwrap();
    let adding = case >= 25;
    let (seed_case, label) = match case {
        18 | 27 => (18, "old"),
        25 => (2, ACCOUNTS_OWN_LABEL),
        26 => (2, "work"),
        _ => (case, "work"),
    };
    seed(seed_case, &store, label, env.now);
    let _signing = matches!(case, 20 | 29).then(|| store.signin_now().unwrap().unwrap());
    let _writer = matches!(case, 21 | 30).then(|| store.acquire().unwrap());
    let before = RefCell::new(data_files(tmp.path()));
    let status = |login: &str| {
        assert!(matches!(case, 22..=24));
        assert!(login.is_empty());
        match case {
            22 => seed(2, &store, "work", env.now),
            23 => seed(4, &store, "work", env.now),
            24 => std::fs::write(&path, "[[account]]\nlabel='other'\nclass='dedicated'\n").unwrap(),
            _ => unreachable!(),
        }
        *before.borrow_mut() = data_files(tmp.path());
        Ok(Captured {
            stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"owner@example.test"}"#
                .to_vec(),
            code: Some(0),
        })
    };
    let host = FakeHost::new(&status);
    let row = accounts::Row {
        label: "work".into(),
        email: Some("new@example.test".into()),
        identity_email: matches!(case, 22..=24).then(|| "owner@example.test".into()),
        vendor: accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
            ..accounts::Vendor::default()
        },
        ..accounts::Row::default()
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    // A lock a case holds stays held for the whole verb, so its stores refuse at the first
    // take: the refusal and the files it leaves are the subject, not the ladder's pauses.
    let success = accounts::with_store_ladder(&[], || {
        if adding {
            let result = accounts_add(cli, &env, &row);
            if let Err(why) = &result {
                err.extend_from_slice(why.as_bytes());
            }
            result.is_ok()
        } else {
            run_accounts_adopt(cli, &env, &host, &row, &mut out, &mut err) == ExitCode::SUCCESS
        }
    });
    let after = data_files(tmp.path());
    let before = before.into_inner();
    if !success {
        assert_eq!(
            after,
            before,
            "refusal changed data: case {case}: {}",
            String::from_utf8_lossy(&err)
        );
    }
    // Neither the roster and its fleet source nor the wake marker a written row replaces
    // (`wake_for_roster`, which tells a running host of the row) carries authority; a refusal
    // leaves every file as it was, the marker too (above).
    let wake = store.wake_path();
    let authority = |files: BTreeMap<PathBuf, Vec<u8>>| {
        files
            .into_iter()
            .filter(|(file, _)| file != &path && file != &fleet && file != &wake)
            .collect::<BTreeMap<_, _>>()
    };
    let authority_changed = authority(after) != authority(before);
    let redirected = if success && !adding {
        store
            .bound(&Roster::load_for(cli, &path).unwrap(), env.now)
            .unwrap()
            .row("work")
            .unwrap()
            .dir
            .is_some()
    } else {
        false
    };
    if matches!(case, 20 | 29) {
        assert!(
            !store.dir.join("accounts.lock").exists(),
            "do not take writer lock while waiting for a sign-in"
        );
    }
    if case == 25 {
        assert!(String::from_utf8_lossy(&err).contains("adopt-default <unused-label>"));
    }
    (
        i64::from(success),
        i64::from(authority_changed),
        i64::from(redirected),
    )
}

pub(crate) fn creation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Create" => {
            let (local, authority, redirected) = creation_case(state["case"], Cli::Claude);
            next.insert("phase", 1);
            next.insert("local", local);
            next.insert("authority", authority);
            next.insert("redirected", redirected);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn new_labels_conform_to_retained_state_and_signin_exclusion() {
    let model = aterm_models_harness::account_create_label_model();
    assert_eq!(model.mutant_dials(), (1..=32).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = creation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                checked += usize::from(action.name == "Create");
            }
        }
    }
    assert_eq!(checked, 30);
    let mut bad = model.init_state();
    bad.insert("case", 2);
    bad.insert("phase", 1);
    bad.insert("local", 1);
    assert!(!model.check_invariant("RetainedLabelsRefused", &bad));
}

#[test]
fn each_provider_refuses_retained_labels_and_accepts_unrelated_records() {
    for cli in Cli::ALL {
        for case in [1, 2, 4, 18, 19, 26, 27, 28] {
            let result = creation_case(case, cli);
            assert_eq!(
                result,
                (i64::from(matches!(case, 1 | 18 | 19 | 27 | 28)), 0, 0),
                "{cli:?} case {case}"
            );
        }
    }
}

#[test]
fn adopting_a_label_with_a_retained_binding_refuses_instead_of_redirecting() {
    let tmp = tmp_dir("adopt-retained-binding");
    let env = env_at(&tmp);
    let store = env.accounts_store();
    let mut logins = accounts::Logins::default();
    logins.labels.insert(
        "work".into(),
        accounts::Binding {
            dir: Some(
                tmp.path()
                    .join("previous-login")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..accounts::Binding::default()
        },
    );
    store.save_logins(&logins).unwrap();
    let (code, out, err) = acc_quiet(
        AccountsCmd::AdoptDefault {
            label: "work".into(),
            email: Some("new@example.test".into()),
            identity_email: None,
            browser: None,
        },
        &env,
    );
    let resolved = Roster::load(&env.accounts_roster().unwrap())
        .ok()
        .and_then(|raw| store.bound(&raw, env.now).ok())
        .and_then(|rows| rows.row("work").and_then(|row| row.dir.clone()));
    assert_eq!(
        code,
        exit(1),
        "{out}{err}; new default resolved to {resolved:?}"
    );
    assert!(!env.accounts_roster().unwrap().exists());
    assert_eq!(store.load_logins(env.now).unwrap().0, logins);
}
