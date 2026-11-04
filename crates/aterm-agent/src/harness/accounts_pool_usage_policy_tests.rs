// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) fn write_context_login(home: &Path, generation: i64) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store='file'\n",
    )
    .unwrap();
    let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": "fixture@example.test", "iat": generation,
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "fixture-user", "chatgpt_account_id": "fixture-account"
        }
    }));
    std::fs::write(
        home.join("auth.json"),
        aterm_json::to_vec(&aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        }))
        .unwrap(),
    )
    .unwrap();
}

struct ContextRead<'a> {
    user_home: &'a Path,
    signed_out: bool,
    replace: bool,
    contextual: bool,
    calls: std::sync::Mutex<usize>,
}

impl UsageReader for ContextRead<'_> {
    fn read(&self, _: Cli, _: &Path, _: Option<&str>, _: i64) -> Reading {
        panic!("the pool must retain the observed request context")
    }

    fn read_observed(&self, cli: Cli, home: &Path, _: Option<&str>, _: i64) -> UsageObservation {
        assert_eq!(cli, Cli::Codex);
        *self.calls.lock().unwrap() += 1;
        let context = crate::harness::codex_app_login::current_context(home, self.user_home);
        assert!(context.is_some());
        if self.replace {
            write_context_login(home, 2);
        }
        UsageObservation {
            reading: if self.signed_out {
                Reading::SignedOut
            } else {
                plain_usage()
            },
            context: self.contextual.then_some(context).flatten(),
        }
    }
}

#[test]
fn old_usage_and_signed_out_results_cannot_publish_after_same_directory_replacement() {
    for signed_out in [false, true] {
        for replace in [false, true] {
            let world = World::new("usage-credential-replacement");
            world.two(Cli::Codex);
            world.on_main(Cli::Codex, &[]);
            let home = world.home.join(Cli::Codex.accounts().default_home);
            write_context_login(&home, 1);
            let seat = world.seat();
            let store = seat.store(Cli::Codex);
            let seed = ContextRead {
                user_home: &world.home,
                signed_out: !signed_out,
                replace: false,
                contextual: true,
                calls: std::sync::Mutex::new(0),
            };
            let before = pool_snapshot(&seat, Cli::Codex).unwrap();
            poll_usage(
                &seat,
                Cli::Codex,
                &before,
                before.roster.row("main").unwrap(),
                &seed,
            )
            .unwrap();
            assert_eq!(*seed.calls.lock().unwrap(), 1);
            let before = pool_snapshot(&seat, Cli::Codex).unwrap();
            assert_eq!(
                before
                    .state
                    .account("main")
                    .current_auth(before.roster.row("main").unwrap()),
                if signed_out {
                    accounts::Auth::Ok
                } else {
                    accounts::Auth::SignedOut
                }
            );
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let images = files.each_ref().map(|path| std::fs::read(path).ok());
            let auth = std::fs::read(home.join("auth.json")).unwrap();
            let read = ContextRead {
                user_home: &world.home,
                signed_out,
                replace,
                contextual: true,
                calls: std::sync::Mutex::new(0),
            };
            let result = poll_usage(
                &seat,
                Cli::Codex,
                &before,
                before.roster.row("main").unwrap(),
                &read,
            );
            assert_eq!(*read.calls.lock().unwrap(), 1);
            let after = pool_snapshot(&seat, Cli::Codex).unwrap();
            assert_eq!(after.roster, before.roster);
            assert_eq!(after.logins, before.logins);
            if replace {
                assert!(result.is_err());
                assert_ne!(std::fs::read(home.join("auth.json")).unwrap(), auth);
                assert_eq!(
                    files.each_ref().map(|path| std::fs::read(path).ok()),
                    images
                );
                assert_eq!(
                    after.state.account("main").auth,
                    before.state.account("main").auth
                );
                assert_eq!(
                    after.state.account("main").auth_at,
                    before.state.account("main").auth_at
                );
                assert_eq!(
                    after
                        .state
                        .account("main")
                        .current_auth(after.roster.row("main").unwrap()),
                    accounts::Auth::Unseen
                );
            } else {
                assert!(result.is_ok());
                assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
                assert_eq!(
                    after
                        .state
                        .account("main")
                        .current_auth(after.roster.row("main").unwrap()),
                    if signed_out {
                        accounts::Auth::SignedOut
                    } else {
                        accounts::Auth::Ok
                    }
                );
            }
        }
    }
}

#[test]
fn a_contextless_signed_out_result_cannot_adopt_legacy_history_even_with_a_login_file() {
    let world = World::new("usage-unscoped-auth");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    write_context_login(&world.home.join(Cli::Codex.accounts().default_home), 1);
    let seat = world.seat();
    let store = seat.store(Cli::Codex);
    store
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "main",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .unwrap();
    let before = pool_snapshot(&seat, Cli::Codex).unwrap();
    let row = before.roster.row("main").unwrap();
    assert_eq!(before.state.account("main").auth, accounts::Auth::SignedOut);
    assert_eq!(
        before.state.account("main").current_auth(row),
        accounts::Auth::Unseen
    );
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let images = files.each_ref().map(|path| std::fs::read(path).ok());
    let read = ContextRead {
        user_home: &world.home,
        signed_out: true,
        replace: false,
        contextual: false,
        calls: std::sync::Mutex::new(0),
    };
    poll_usage(&seat, Cli::Codex, &before, row, &read).unwrap();
    assert_eq!(*read.calls.lock().unwrap(), 1);
    let after = pool_snapshot(&seat, Cli::Codex).unwrap();
    assert_eq!(after.state.account("main"), before.state.account("main"));
    assert_eq!(
        after
            .state
            .account("main")
            .current_auth(after.roster.row("main").unwrap()),
        accounts::Auth::Unseen
    );
    assert_eq!(
        files.each_ref().map(|path| std::fs::read(path).ok()),
        images
    );
}

pub(crate) fn usage_transition(
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
        "PollAndChoose" => {
            let world = World::new("usage-policy");
            let work = world.two(Cli::Codex);
            world.on_main(Cli::Codex, &[]);
            wall(&world, Cli::Codex, "main", NOW + HOUR);
            let store = world.seat().store(Cli::Codex);
            if state["case"] >= 13 {
                store
                    .transact_logins(NOW - MIN, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "work",
                            work.to_str(),
                            work.to_str().unwrap(),
                            None,
                            NOW - MIN,
                        ))
                    })
                    .unwrap();
            }
            let path = world.cfg.join(Cli::Codex.accounts().roster_file);
            let raw = Roster::load_for(Cli::Codex, &path).unwrap();
            let after_read = std::sync::Mutex::new(None);
            let files = [
                store.state_file(),
                store.logins_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let read = |_: Cli, home: &Path, _: Option<&str>, _: i64| {
                if home != work {
                    return None;
                }
                match state["case"] {
                    3 | 4 => {
                        store
                            .transact_logins(NOW, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    work.to_str(),
                                    world.root.join("renewed").to_str().unwrap(),
                                    None,
                                    NOW,
                                ))
                            })
                            .unwrap();
                    }
                    5 | 6 => {
                        let mut changed = raw.clone();
                        changed.rows[1].identity_email = Some("changed@example.test".into());
                        std::fs::write(
                            &path,
                            accounts::append_rows_for(Cli::Codex, "", &changed.rows).unwrap(),
                        )
                        .unwrap();
                    }
                    7 | 8 => {
                        std::fs::write(&path, "").unwrap();
                    }
                    9 | 11 => {
                        std::fs::write(&path, "[[broken").unwrap();
                    }
                    10 | 12 => {
                        std::fs::rename(&path, world.root.join("kept-roster")).unwrap();
                    }
                    13 | 14 => {
                        store
                            .transact_logins(NOW, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    work.to_str(),
                                    work.to_str().unwrap(),
                                    None,
                                    NOW,
                                ))
                            })
                            .unwrap();
                    }
                    15 | 16 => {
                        store
                            .transact_logins(NOW - MIN, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    work.to_str(),
                                    work.to_str().unwrap(),
                                    None,
                                    NOW - MIN,
                                ))
                            })
                            .unwrap();
                    }
                    _ => {}
                }
                *after_read.lock().unwrap() =
                    Some(files.each_ref().map(|path| std::fs::read(path).ok()));
                matches!(state["case"], 1 | 3 | 5 | 7 | 9 | 10 | 13 | 15).then(|| Usage {
                    sample: near_wall(),
                    walls: (state["case"] > 2)
                        .then_some(("five_hour", Some(NOW + HOUR)))
                        .into_iter()
                        .collect(),
                })
            };
            let result = super::super::launch_env(&world.seat(), Cli::Codex, &read);
            let published = store.load(NOW).unwrap().0.account("work").sample.is_some();
            let launched = result.as_ref().is_ok_and(|env| env.is_some());
            if state["case"] > 2 {
                assert_eq!(
                    files.each_ref().map(|path| std::fs::read(path).ok()),
                    after_read.into_inner().unwrap().unwrap()
                );
                assert_eq!(std::fs::read(work.join("auth.json")).unwrap(), b"{}");
            }
            next.insert("phase", 2);
            next.insert("published", i64::from(published));
            next.insert("launched", i64::from(launched));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn polling_and_launch_conform_to_current_policy() {
    let model = aterm_models_harness::account_pool_usage_policy_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = usage_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut changed = model.init_state();
    changed.insert("case", 3);
    changed.insert("phase", 1);
    let mut forged = usage_transition(&changed, "PollAndChoose");
    forged.insert("published", 1);
    forged.insert("launched", 1);
    assert!(!model.check_invariant("CurrentPublication", &forged));
    assert!(!model.check_invariant("CurrentLaunch", &forged));
}

#[test]
fn a_public_usage_poll_with_no_result_still_refuses_changed_policy() {
    let world = World::new("empty-poll-policy");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let seat = world.seat();
    let roster = seat.bound_roster(Cli::Codex).unwrap();
    let enrolled = accounts::enrolled_for(Cli::Codex, &roster);
    let store = seat.store(Cli::Codex);
    let before = std::fs::read(store.state_file()).unwrap();
    let read = |_: Cli, home: &Path, _: Option<&str>, _: i64| {
        if home == work {
            std::fs::write(world.cfg.join(Cli::Codex.accounts().roster_file), "").unwrap();
        }
        None
    };
    assert!(record_usage(&seat, Cli::Codex, &roster, &enrolled, &read).is_err());
    assert_eq!(std::fs::read(store.state_file()).unwrap(), before);
}

/// A reader that keeps the homes it was asked about and answers `answer` for each.
struct Reads<'a> {
    user_home: &'a Path,
    homes: std::sync::Mutex<Vec<PathBuf>>,
    answer: &'a (dyn Fn(&Path) -> Reading + Sync),
}

impl crate::harness::accounts_pool::UsageReader for Reads<'_> {
    fn read(&self, _: Cli, home: &Path, _: Option<&str>, _: i64) -> Reading {
        self.homes.lock().unwrap().push(home.to_path_buf());
        (self.answer)(home)
    }

    fn read_observed(
        &self,
        cli: Cli,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
    ) -> UsageObservation {
        let context = (cli == Cli::Codex)
            .then(|| crate::harness::codex_app_login::current_context(home, self.user_home))
            .flatten();
        UsageObservation {
            reading: self.read(cli, home, expected_email, now),
            context,
        }
    }
}

fn plain_usage() -> Reading {
    Reading::Usage(Usage {
        sample: Sample {
            session_pct: Some(10),
            at: NOW,
            ..Sample::default()
        },
        walls: Vec::new(),
    })
}

fn spent_usage() -> Reading {
    Reading::Usage(Usage {
        sample: Sample {
            session_pct: Some(100),
            at: NOW,
            ..Sample::default()
        },
        walls: vec![(accounts::SESSION_LIMIT, Some(NOW + HOUR))],
    })
}

/// The owner's rule at a launch: `work` is never asked about while `main` serves, and the read is
/// reported with its row and why. CONTROL: `a_read_that_shows_a_wall_...` below reads `work` once
/// `main` cannot serve, so the silence here is the rule and not a reader that never reaches it.
#[test]
fn a_launch_reads_only_the_row_it_starts_on_and_reports_it() {
    let world = World::new("launch-one-row");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let (pick, events) = crate::harness::provider_read::captured(|| {
        launch_env(&world.seat(), Cli::Codex, &reads).unwrap()
    });
    assert_eq!(pick.label, "main");
    assert_eq!(
        *reads.homes.lock().unwrap(),
        [world.home.join(Cli::Codex.accounts().default_home)]
    );
    assert_eq!(
        events,
        [crate::harness::provider_read::ProbeEvent {
            at: NOW,
            read: crate::harness::provider_read::Read::CodexEndpoint,
            account: Some("main".to_owned()),
            login: None,
            why: crate::harness::provider_read::InUse::Launching,
        }]
    );
    assert_eq!(world.load(Cli::Codex).account("work").sample, None);
}

/// A pick the read shows spent gives way to the next, which is then the row being launched.
/// CONTROL: the launch lands on `work`, read once, and `main` is not asked twice.
#[test]
fn a_read_that_shows_a_wall_moves_the_launch_to_the_next_row_and_reads_that() {
    let world = World::new("launch-next-row");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let default_home = world.home.join(Cli::Codex.accounts().default_home);
    let answer = |home: &Path| {
        if home == default_home {
            spent_usage()
        } else {
            plain_usage()
        }
    };
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &reads).unwrap();
    assert_eq!(pick.label, "work");
    assert_eq!(*reads.homes.lock().unwrap(), [default_home, work]);
}

/// A row read a moment ago is not read again. CONTROL: the first launch did read it.
#[test]
fn a_fresh_row_starts_with_no_read() {
    let world = World::new("launch-fresh-row");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    launch_env(&world.seat(), Cli::Codex, &reads).unwrap();
    assert_eq!(reads.homes.lock().unwrap().len(), 1);
    launch_env(&world.seat(), Cli::Codex, &reads).unwrap();
    assert_eq!(reads.homes.lock().unwrap().len(), 1);
}

/// A pool whose every pick turns out spent stops at [`LAUNCH_READS`], so a launch never sweeps the
/// pool. CONTROL: `LAUNCH_READS` rows are read, so the cap is what stopped it.
#[test]
fn a_launch_reads_no_more_than_its_cap() {
    let world = World::new("launch-cap");
    world.two(Cli::Codex);
    let mut text =
        std::fs::read_to_string(world.cfg.join(Cli::Codex.accounts().roster_file)).expect("roster");
    for label in ["w2", "w3", "w4", "w5"] {
        let dir = world.root.join("logins").join("codex").join(label);
        std::fs::create_dir_all(&dir).expect("login dir");
        std::fs::write(dir.join("auth.json"), "{}").expect("login");
        text.push_str(&format!(
            "\n[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\ndir = {}\n",
            toml(&dir)
        ));
    }
    std::fs::write(world.cfg.join(Cli::Codex.accounts().roster_file), text).expect("roster");
    world.on_main(Cli::Codex, &[]);
    let answer = |_: &Path| spent_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let _ = launch_env(&world.seat(), Cli::Codex, &reads);
    assert_eq!(
        reads.homes.lock().unwrap().len(),
        crate::harness::accounts_pool::LAUNCH_READS
    );
}

/// A login signed in again by hand comes back when nothing on record serves, the one row left to
/// try. CONTROL: while another row serves it is not read, and stays signed out.
#[test]
fn a_signed_out_row_is_read_only_when_nothing_else_serves() {
    let world = World::new("launch-signed-out");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    write_context_login(&world.home.join(Cli::Codex.accounts().default_home), 1);
    write_context_login(&work, 1);
    let sign_out = |label: &str| {
        let seat = world.seat();
        let before = pool_snapshot(&seat, Cli::Codex).unwrap();
        let read = ContextRead {
            user_home: &world.home,
            signed_out: true,
            replace: false,
            contextual: true,
            calls: std::sync::Mutex::new(0),
        };
        poll_usage(
            &seat,
            Cli::Codex,
            &before,
            before.roster.row(label).unwrap(),
            &read,
        )
        .unwrap();
    };
    sign_out("main");
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &reads).unwrap();
    assert_eq!(pick.label, "work");
    assert_eq!(*reads.homes.lock().unwrap(), std::slice::from_ref(&work));
    assert_eq!(
        world.load(Cli::Codex).account("main").auth,
        accounts::Auth::SignedOut
    );

    let world = World::new("launch-signed-out-all");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    write_context_login(&world.home.join(Cli::Codex.accounts().default_home), 1);
    write_context_login(&work, 1);
    for label in ["main", "work"] {
        let seat = world.seat();
        let before = pool_snapshot(&seat, Cli::Codex).unwrap();
        let read = ContextRead {
            user_home: &world.home,
            signed_out: true,
            replace: false,
            contextual: true,
            calls: std::sync::Mutex::new(0),
        };
        poll_usage(
            &seat,
            Cli::Codex,
            &before,
            before.roster.row(label).unwrap(),
            &read,
        )
        .unwrap();
    }
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &reads).unwrap();
    assert_eq!(pick.label, "main");
    assert_eq!(
        world.load(Cli::Codex).account("main").auth,
        accounts::Auth::Ok
    );
}
