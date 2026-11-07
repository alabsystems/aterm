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
    fn read(&self, _: Cli, _: &Path, _: Option<&str>, _: i64, _: provider_read::Use) -> Reading {
        panic!("the pool must retain the observed request context")
    }

    fn read_observed(
        &self,
        cli: Cli,
        home: &Path,
        _: Option<&str>,
        now: i64,
        use_token: provider_read::Use,
    ) -> UsageObservation {
        use_token
            .begin_home(provider_read::Read::CodexEndpoint, home, now)
            .unwrap();
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
            poll_fixture(
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
            let result = poll_fixture(
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
    poll_fixture(&seat, Cli::Codex, &before, row, &read).unwrap();
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

// The response is captured under generation 1. Cases 19/20 replace the actual
// file before returning it, without touching the roster or managed binding.
// The stable cases exercise both accepting and rejecting provider responses.
fn credential_response_case(case: i64) -> [i64; 4] {
    let world = World::new("usage-policy-credential");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    wall(&world, Cli::Codex, "main", NOW + HOUR);
    write_context_login(&work, 1);
    let store = world.seat().store(Cli::Codex);
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    let credential = std::sync::Mutex::new(None);
    let answer = |home: &Path| {
        assert_eq!(home, work);
        if case >= 19 {
            write_context_login(home, 2);
        }
        *credential.lock().unwrap() = Some(std::fs::read(home.join("auth.json")).unwrap());
        if case == 18 || case == 20 {
            Reading::SignedOut
        } else {
            plain_usage()
        }
    };
    let read = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let result = super::super::launch_env(&world.seat(), Cli::Codex, &read);
    assert_eq!(
        read.homes.lock().unwrap().as_slice(),
        std::slice::from_ref(&work)
    );
    assert_eq!(
        std::fs::read(work.join("auth.json")).unwrap(),
        credential.into_inner().unwrap().unwrap(),
        "the delayed response must never replace the current credential"
    );
    let after = pool_snapshot(&world.seat(), Cli::Codex).unwrap();
    let account = after.state.account("work");
    [
        i64::from(account.sample.is_some()),
        i64::from(result.as_ref().is_ok_and(|env| env.is_some())),
        match account.current_auth(after.roster.row("work").unwrap()) {
            accounts::Auth::Ok => 1,
            accounts::Auth::SignedOut => 2,
            accounts::Auth::Unseen => 0,
        },
        i64::from(before == files.each_ref().map(|path| std::fs::read(path).ok())),
    ]
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
            if state["case"] >= 17 {
                for (key, value) in ["published", "launched", "auth", "unchanged"]
                    .into_iter()
                    .zip(credential_response_case(state["case"]))
                {
                    next.insert(key, value);
                }
                next.insert("phase", 2);
                return next;
            }
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
            let unchanged = files.each_ref().map(|path| std::fs::read(path).ok())
                == after_read.into_inner().unwrap().unwrap();
            if state["case"] > 2 {
                assert!(unchanged);
                assert_eq!(std::fs::read(work.join("auth.json")).unwrap(), b"{}");
            }
            next.insert("phase", 2);
            next.insert("published", i64::from(published));
            next.insert("launched", i64::from(launched));
            next.insert("unchanged", i64::from(unchanged));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn polling_and_launch_conform_to_current_policy() {
    let model = aterm_models_harness::account_pool_usage_policy_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(model.mutant_dials(), (1..=21).collect::<Vec<_>>());
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
    for case in [19, 20] {
        changed.insert("case", case);
        let actual = usage_transition(&changed, "PollAndChoose");
        let mut forged = actual.clone();
        forged.insert("auth", if case == 19 { 1 } else { 2 });
        assert!(!model.check_invariant("CurrentAuthentication", &forged));
        let mut forged = actual;
        forged.insert("unchanged", 0);
        assert!(!model.check_invariant("RefusalPreservesRecords", &forged));
    }
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
    fn read(
        &self,
        _: Cli,
        home: &Path,
        _: Option<&str>,
        now: i64,
        use_token: provider_read::Use,
    ) -> Reading {
        use_token
            .begin_home(provider_read::Read::CodexEndpoint, home, now)
            .unwrap();
        self.homes.lock().unwrap().push(home.to_path_buf());
        (self.answer)(home)
    }

    fn read_observed(
        &self,
        cli: Cli,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        use_token: provider_read::Use,
    ) -> UsageObservation {
        let context = (cli == Cli::Codex)
            .then(|| crate::harness::codex_app_login::current_context(home, self.user_home))
            .flatten();
        UsageObservation {
            reading: self.read(cli, home, expected_email, now, use_token),
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

/// A roster of this machine's own login alone.
fn lone_main(world: &World) {
    std::fs::write(
        world.cfg.join("accounts-codex.toml"),
        "[[account]]\nlabel='main'\nclass='dedicated'\n",
    )
    .unwrap();
}

/// THE LONE ROW IS THE ROW A LAUNCH STARTS ON, so it is read as any launch's row is: a Codex
/// session that starts on a wall writes no figure for it, so no other record can show the wall.
/// A row that serves launches as it always did, on its login as it is, and its figure is current.
/// NEGATIVE CONTROL: before, a lone row was never read: its figure stayed at the 1% of a sweep long
/// gone while Codex stopped at a weekly wall (m21), and the survey called it `usable`.
#[test]
fn a_lone_row_is_read_as_the_row_a_launch_starts_on() {
    let world = World::new("launch-lone-read");
    lone_main(&world);
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    assert_eq!(
        super::super::launch_env(&world.seat(), Cli::Codex, &reads),
        Ok(None)
    );
    let default_home = world.home.join(Cli::Codex.accounts().default_home);
    assert_eq!(
        *reads.homes.lock().unwrap(),
        std::slice::from_ref(&default_home)
    );
    assert_eq!(
        world
            .load(Cli::Codex)
            .account("main")
            .sample
            .and_then(|sample| sample.session_pct),
        Some(10),
        "what it read is on record"
    );
    assert_eq!(
        super::super::launch_env(&world.seat(), Cli::Codex, &reads),
        Ok(None)
    );
    assert_eq!(
        *reads.homes.lock().unwrap(),
        [default_home],
        "a row read a moment ago is not read again"
    );
}

/// `main` alone with a wall on record, written before the launch that follows.
fn lone_main_walled(world: &World, window: &str) {
    lone_main(world);
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW - 600, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "main",
                window,
                Some(NOW + 3 * 24 * HOUR),
                NOW - 600,
                NOW - 600,
            ))
        })
        .unwrap();
}

/// A WALL ON RECORD CAN LIFT EARLY (a limit reset redeemed in Codex, another account signed in) AND
/// NOTHING ELSE WOULD SAY SO: the launch it would refuse reads the row it starts on, a read showing
/// room lifts the wall, and every surface then agrees. NEGATIVE CONTROL: without the read the
/// launch was refused for days on a login with quota.
#[test]
fn a_read_showing_room_lifts_a_lone_rows_wall_and_its_launch_runs() {
    let world = World::new("launch-lone-lift");
    lone_main_walled(&world, accounts::SESSION_LIMIT);
    let until = world.load(Cli::Codex).account("main").limited_until(NOW);
    assert!(until.is_some(), "the wall stands on record");
    assert!(
        launch_opening(&world.seat(), Cli::Codex, None).is_some(),
        "CONTROL: a launch with no read would hold for it"
    );
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    assert_eq!(
        super::super::launch_env(&world.seat(), Cli::Codex, &reads),
        Ok(None)
    );
    assert_eq!(reads.homes.lock().unwrap().len(), 1, "one read of that row");
    assert_eq!(
        world.load(Cli::Codex).account("main").limited_until(NOW),
        None
    );
    assert_eq!(
        launch_opening(&world.seat(), Cli::Codex, None),
        None,
        "nothing to wait for"
    );
}

/// `main` with a wall on record, listed beside a row for another Mac, whose login is not made here.
fn main_walled_beside_a_row_with_no_login_here(world: &World, window: &str) {
    lone_main_walled(world, window);
    let there = world.root.join("logins").join("codex").join("there");
    std::fs::create_dir_all(&there).expect("login dir");
    let text = format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
         label = \"there\"\nclass = \"dedicated\"\ndir = {}\n",
        toml(&there)
    );
    std::fs::write(world.cfg.join("accounts-codex.toml"), text).unwrap();
}

/// A ROSTER THAT LISTS A ROW FOR ANOTHER MAC IS A LONE ROW HERE: no pick can leave a wall on the one
/// row enrolled here, so a launch reads through it as it does on a roster of one, and a read showing
/// room starts the worker (m22 lists `main` and `codex-m22`, whose login is not made there).
/// NEGATIVE CONTROL: the read looked for a roster of one row, so a wall lifted early refused this
/// launch until the wall's end. CONTROL: a read that shows no room leaves the wall. CONTROL: with a
/// second row enrolled the launch reads that row and never the walled one.
#[test]
fn a_read_showing_room_lifts_the_wall_on_the_one_row_enrolled_here() {
    let room = |_: &Path| plain_usage();
    let spent = |_: &Path| spent_usage();
    let run = |world: &World, answer: &(dyn Fn(&Path) -> Reading + Sync)| {
        let reads = Reads {
            user_home: &world.home,
            homes: std::sync::Mutex::default(),
            answer,
        };
        let launched = super::super::launch_env(&world.seat(), Cli::Codex, &reads);
        let read = reads.homes.lock().unwrap().clone();
        (launched, read)
    };
    let walled = |world: &World| {
        world
            .load(Cli::Codex)
            .account("main")
            .limited_until(NOW)
            .is_some()
    };
    let main_home = |world: &World| world.home.join(Cli::Codex.accounts().default_home);

    let world = World::new("launch-sole-lift");
    main_walled_beside_a_row_with_no_login_here(&world, accounts::SESSION_LIMIT);
    let (launched, read) = run(&world, &room);
    assert!(launched.is_ok(), "{launched:?}");
    assert_eq!(read, [main_home(&world)], "one read, of that row");
    assert!(!walled(&world), "the read lifted it");

    let world = World::new("launch-sole-stays");
    main_walled_beside_a_row_with_no_login_here(&world, accounts::SESSION_LIMIT);
    let (launched, read) = run(&world, &spent);
    let refused = launched.expect_err("the wall stands");
    assert!(refused.contains("no usable account"), "{refused}");
    assert_eq!(read, [main_home(&world)]);
    assert!(walled(&world));

    let world = World::new("launch-sole-two");
    let work = world.two(Cli::Codex);
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW - 600, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "main",
                accounts::SESSION_LIMIT,
                Some(NOW + HOUR),
                NOW - 600,
                NOW - 600,
            ))
        })
        .unwrap();
    let (launched, read) = run(&world, &room);
    assert!(launched.is_ok(), "{launched:?}");
    assert_eq!(read, [work], "the row it starts on, and not the walled one");
    assert!(walled(&world));
}

/// A ROW A LAUNCH COULD NOT START ON IS NOT READ, under the owner's no-polling rule, wall or no: on
/// a preparation or another Mac's lease nothing reads a wall through. NEGATIVE CONTROL: the read
/// looked only at which row was enrolled, so a launch that could only be refused spent a provider
/// read, and could renew that login. CONTROL: the same roster with neither reads it.
#[test]
fn a_launch_reads_no_wall_on_a_row_it_could_not_start_on() {
    let room = |_: &Path| plain_usage();
    let run = |tag: &str, seed: &dyn Fn(&mut accounts::State)| {
        let world = World::new(tag);
        main_walled_beside_a_row_with_no_login_here(&world, accounts::SESSION_LIMIT);
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW - 600, &mut None, |state| {
                seed(state);
                Ok(Vec::new())
            })
            .unwrap();
        let reads = Reads {
            user_home: &world.home,
            homes: std::sync::Mutex::default(),
            answer: &room,
        };
        let launched = super::super::launch_env(&world.seat(), Cli::Codex, &reads);
        let lifted = lift_lone_wall(&world.seat(), Cli::Codex, &reads, None);
        let read = reads.homes.lock().unwrap().len();
        (launched.is_ok(), lifted, read)
    };
    assert_eq!(
        run("launch-sole-prepared", &|state| {
            state.prepared_accounts.insert("main".into());
        }),
        (false, false, 0),
        "only a preparation holds that row"
    );
    assert_eq!(
        run("launch-sole-leased", &|state| {
            state.leases.insert(
                "main".to_owned(),
                accounts::AccountLease {
                    holder: "m9".to_owned(),
                    until: NOW + HOUR,
                    at: NOW - HOUR,
                },
            );
        }),
        (false, false, 0),
        "another Mac holds it"
    );
    assert_eq!(
        run("launch-sole-free", &|_| {}),
        (true, false, 1),
        "CONTROL: the launch's read lifted it, so the command had nothing left to lift"
    );
}

/// ONE LAUNCH READS A ROW ONCE, even where the answer leaves it due: a refusal with no figure for a
/// window writes no sample. NEGATIVE CONTROL: read_launch_rows and read_lone_wall each read it, and
/// one launch asked the provider twice. CONTROL: the launcher's next launch may read it again.
#[test]
fn one_launch_reads_a_row_once_where_the_answer_leaves_it_due() {
    let refuses = |_: &Path| {
        Reading::Usage(Usage {
            sample: Sample {
                at: NOW,
                ..Sample::default()
            },
            walls: vec![(accounts::SESSION_LIMIT, Some(NOW + HOUR))],
        })
    };
    let beside_another: fn(&World) = |world| {
        main_walled_beside_a_row_with_no_login_here(world, accounts::SESSION_LIMIT);
        world
            .seat()
            .store(Cli::Codex)
            .transact(NOW - 600, &mut None, |state| {
                let main = state.accounts.get_mut("main").expect("its row");
                main.walls.clear();
                Ok(Vec::new())
            })
            .unwrap();
    };
    for (tag, roster) in [
        ("launch-once-lone", lone_main as fn(&World)),
        ("launch-once-sole", beside_another),
    ] {
        let world = World::new(tag);
        roster(&world);
        let reads = Reads {
            user_home: &world.home,
            homes: std::sync::Mutex::default(),
            answer: &refuses,
        };
        let mut pick = super::super::LaunchPick::for_worker_launch();
        for _ in 0..4 {
            let launched = pick.pick(&world.seat(), Cli::Codex, &reads, None);
            assert!(launched.is_err(), "{tag}: {launched:?}");
        }
        assert_eq!(reads.homes.lock().unwrap().len(), 1, "{tag}");
        let launched = super::super::LaunchPick::for_worker_launch().pick(
            &world.seat(),
            Cli::Codex,
            &reads,
            None,
        );
        assert!(launched.is_err(), "{tag}: {launched:?}");
        assert_eq!(
            reads.homes.lock().unwrap().len(),
            2,
            "a distinct invocation has its own allowance: {tag}"
        );
    }
}

/// A LONE WALL'S READ IS ONE OF A LAUNCH'S OWN (the no-polling rule's token): it needs that launch's
/// allowance, and the journal books it to the row with the reason a launch gives. NEGATIVE CONTROL:
/// with the allowance spent nothing reaches the provider and the wall on record stands.
#[test]
fn a_lone_walls_read_takes_a_launchs_allowance_and_is_booked_as_its_read() {
    use crate::harness::provider_read::{self, Account, InUse, Launch, ProbeEvent, Read, Use};

    let world = World::new("lone-read-allowance");
    lone_main_walled(&world, accounts::SESSION_LIMIT);
    let answer = |_: &Path| plain_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let seat = world.seat();
    let mut snapshot = super::super::pool_snapshot(&seat, Cli::Codex).unwrap();
    let enrolled = accounts::enrolled_in(Cli::Codex, &snapshot.roster, seat.home);
    let mut spent = Launch::launch_use(super::super::LaunchUse(()));
    for _ in 0..super::super::LAUNCH_READS {
        let account = Account::in_home(Read::CodexEndpoint, &world.home, Some("another"));
        let _spent = Use::launch_read_use(&mut spent, account).unwrap();
    }
    let (spent_read, events) = provider_read::captured(|| {
        super::super::read_lone_wall(
            &seat,
            Cli::Codex,
            &mut snapshot,
            &enrolled,
            None,
            &reads,
            &mut spent,
        )
    });
    assert_eq!(spent_read, Ok(()));
    assert_eq!(events, [], "no authority, so no probe");
    assert!(reads.homes.lock().unwrap().is_empty(), "and no read");
    assert!(
        world
            .load(Cli::Codex)
            .account("main")
            .limited_until(NOW)
            .is_some(),
        "the wall on record stands"
    );

    let (lifted, events) =
        provider_read::captured(|| lift_lone_wall(&seat, Cli::Codex, &reads, None));
    assert!(lifted, "a fresh allowance reads the row and lifts the wall");
    assert_eq!(
        events,
        [ProbeEvent {
            at: NOW,
            read: Read::CodexEndpoint,
            account: Some("main".to_owned()),
            login: None,
            why: InUse::Launching,
        }]
    );
    assert_eq!(reads.homes.lock().unwrap().len(), 1);
}

/// `aterm ai review`, a person's command, judges the records and reads no usage itself, so it asks
/// for the read a launch would make of a lone row whose wall would refuse it
/// ([`lift_lone_wall`]). It reads no row it is not refused on: none with no wall, none of a pool
/// with another row, and none under rotation off.
#[test]
fn a_persons_command_lifts_a_lone_wall_as_a_launch_would_and_reads_nothing_else() {
    let room = |_: &Path| plain_usage();
    let spent = |_: &Path| spent_usage();
    let run = |world: &World, answer: &(dyn Fn(&Path) -> Reading + Sync)| {
        let reads = Reads {
            user_home: &world.home,
            homes: std::sync::Mutex::default(),
            answer,
        };
        let lifted = lift_lone_wall(&world.seat(), Cli::Codex, &reads, None);
        let read = reads.homes.lock().unwrap().len();
        (lifted, read)
    };

    let world = World::new("lift-room");
    lone_main_walled(&world, accounts::SESSION_LIMIT);
    assert_eq!(run(&world, &room), (true, 1));
    assert_eq!(
        world.load(Cli::Codex).account("main").limited_until(NOW),
        None
    );

    let world = World::new("lift-spent");
    lone_main_walled(&world, accounts::SESSION_LIMIT);
    assert_eq!(run(&world, &spent), (false, 1), "the wall stands");

    let world = World::new("lift-sole");
    main_walled_beside_a_row_with_no_login_here(&world, accounts::SESSION_LIMIT);
    assert_eq!(
        run(&world, &room),
        (true, 1),
        "a row for another Mac leaves it the row the command starts on"
    );

    let world = World::new("lift-no-wall");
    lone_main(&world);
    assert_eq!(run(&world, &room), (false, 0), "nothing refuses it");

    let world = World::new("lift-off");
    lone_main_walled(&world, accounts::SESSION_LIMIT);
    crate::test_config::write(world.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n").unwrap();
    assert_eq!(run(&world, &room), (false, 0), "rotation off reads nothing");

    let world = World::new("lift-two");
    world.two(Cli::Codex);
    world
        .seat()
        .store(Cli::Codex)
        .transact(NOW - 600, &mut None, |state| {
            Ok(accounts::note_wall(
                state,
                "main",
                accounts::SESSION_LIMIT,
                Some(NOW + HOUR),
                NOW - 600,
                NOW - 600,
            ))
        })
        .unwrap();
    assert_eq!(run(&world, &room), (false, 0), "a pool with another row");
}

/// A read that does not show the wall gone leaves it: one that names it, one that refuses use with
/// no window full, and one with no figure for that window. A row read a moment ago is not read again,
/// so a launcher's retries never become a poll of the provider.
#[test]
fn a_read_that_shows_no_room_leaves_a_lone_rows_wall() {
    let refuses_with_room = || {
        Reading::Usage(Usage {
            sample: Sample {
                session_pct: Some(40),
                at: NOW,
                ..Sample::default()
            },
            walls: vec![(accounts::SESSION_LIMIT, Some(NOW + HOUR))],
        })
    };
    let names: [(&str, &str, &(dyn Fn() -> Reading + Sync)); 3] = [
        ("names it", accounts::SESSION_LIMIT, &spent_usage),
        ("refuses", accounts::SESSION_LIMIT, &refuses_with_room),
        ("no figure", accounts::WEEK_LIMIT, &plain_usage),
    ];
    for (case, window, answer) in names {
        let world = World::new("launch-lone-stays");
        lone_main_walled(&world, window);
        let answer = |_: &Path| answer();
        let reads = Reads {
            user_home: &world.home,
            homes: std::sync::Mutex::default(),
            answer: &answer,
        };
        for _ in 0..2 {
            let refused =
                super::super::launch_env(&world.seat(), Cli::Codex, &reads).expect_err(case);
            assert!(refused.contains("no usable account"), "{case}: {refused}");
        }
        assert_eq!(
            reads.homes.lock().unwrap().len(),
            1,
            "{case}: a row read a moment ago is not read again"
        );
        assert!(
            world
                .load(Cli::Codex)
                .account("main")
                .limited_until(NOW)
                .is_some(),
            "{case}"
        );
        assert!(
            launch_opening(&world.seat(), Cli::Codex, None).is_some(),
            "{case}: the launch holds for the wall"
        );
    }
}

/// A read that shows a wall on the lone row refuses the launch, and offers the wall's end as when
/// the pool opens (a worker's launcher holds for it until the startup deadline stalls it). A launch
/// after the wall runs. A launch that does not read judges the same record ([`launch_now`]).
/// CONTROL: with no wall the same read starts it, above, and rotation off still reads nothing.
#[test]
fn a_wall_on_a_lone_row_holds_its_launch_until_the_wall_ends() {
    let world = World::new("launch-lone-wall");
    lone_main(&world);
    let answer = |_: &Path| spent_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let refused = super::super::launch_env(&world.seat(), Cli::Codex, &reads).expect_err("walled");
    assert!(refused.contains("no usable account"), "{refused}");
    let opens = launch_opening(&world.seat(), Cli::Codex, None).expect("a wall ends");
    assert!(opens > NOW, "held until the wall ends: {opens}");

    let state = world.load(Cli::Codex);
    let roster = world.seat().bound_roster(Cli::Codex).unwrap();
    let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &world.home);
    let judge = |now| {
        launch_now(
            (Cli::Codex, false, Mode::Wall),
            &roster,
            &state,
            &enrolled,
            None,
            now,
        )
    };
    assert_eq!(
        judge(NOW).map(|row| row.map(|row| row.label.clone())),
        Err(refused)
    );
    assert_eq!(
        judge(opens).map(|row| row.map(|row| row.label.clone())),
        Ok(None),
        "the wall ended: it runs on its login as it is"
    );

    let off = World::new("launch-lone-off");
    lone_main(&off);
    crate::test_config::write(off.cfg.join("aterm.toml"), "[harness]\naccounts='off'\n").unwrap();
    let reads = Reads {
        user_home: &off.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    assert_eq!(
        super::super::launch_env(&off.seat(), Cli::Codex, &reads),
        Ok(None)
    );
    assert!(
        reads.homes.lock().unwrap().is_empty(),
        "rotation off reads nothing"
    );
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

/// A Codex pool of six on `main`: `main`, `work`, and `w2` to `w5`, each with a login of its own.
fn six(tag: &str) -> World {
    let world = World::new(tag);
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
    world
}

/// A pool whose every pick turns out spent stops at [`LAUNCH_READS`], so a launch never sweeps the
/// pool. CONTROL: `LAUNCH_READS` rows are read, so the cap is what stopped it.
#[test]
fn a_launch_reads_no_more_than_its_cap() {
    let world = six("launch-cap");
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

/// A cold relaunch's read of the row it ran on counts against the same cap: where every row reads
/// spent, the row it ran on and the serving row's picks are read [`LAUNCH_READS`] times in all.
/// CONTROL: the row it ran on was read first, so the cap, not a skipped read, stopped it.
#[test]
fn a_cold_relaunch_reads_no_more_than_a_launchs_cap() {
    let world = six("cold-cap");
    let ran = world.root.join("logins").join("codex").join("w2");
    let auth = ran.join("auth.json").display().to_string();
    let answer = |_: &Path| spent_usage();
    let reads = Reads {
        user_home: &world.home,
        homes: std::sync::Mutex::default(),
        answer: &answer,
    };
    let _ = cold_launch_env(
        &world.seat(),
        Cli::Codex,
        &[("CODEX_AUTH_PATH", auth.as_str())],
        &reads,
        None,
    );
    let homes = reads.homes.lock().unwrap();
    assert_eq!(
        homes.len(),
        crate::harness::accounts_pool::LAUNCH_READS,
        "{homes:?}"
    );
    assert_eq!(homes[0], ran, "CONTROL: the row it ran on is read first");
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
        poll_fixture(
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
        poll_fixture(
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
