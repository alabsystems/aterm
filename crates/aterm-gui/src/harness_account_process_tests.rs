// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn applied(idle: &WorkerIdle, roster: &accounts::Roster, to: &str, snap: &Snapshot) -> Applied {
    match observed_account(&idle.hooks, roster, snap) {
        Ok(observed) if observed.label.as_deref() == Some(to) => Applied::Yes,
        Ok(_) => Applied::No,
        Err(()) => Applied::Unread,
    }
}

fn images(a: &Accts) -> [Option<Vec<u8>>; 4] {
    let store = a.store();
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .map(|path| std::fs::read(path).ok())
}

fn application_case(case: i64) -> Applied {
    let world = Arc::new(World::default());
    let mut hooks = hooks(&world, parking_body(&world));
    let mut snapshot = snap("s-application");
    snapshot.stored_login = true;
    snapshot.login = Some("/login/wanted".into());
    let roster = accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "main".into(),
                ..Default::default()
            },
            accounts::Row {
                label: "wanted".into(),
                dir: Some("/login/wanted".into()),
                was: vec![Some("/login/old".into())],
                ..Default::default()
            },
            accounts::Row {
                label: "other".into(),
                dir: Some("/login/other".into()),
                ..Default::default()
            },
        ],
    };
    let mut env = vec![
        "PATH=/synthetic".into(),
        format!("{}=/login/wanted", accounts::LOGIN_ENV),
    ];
    let target = if case == 2 { "main" } else { "wanted" };
    match case {
        2 => {
            env.truncate(1);
            snapshot.login = None;
        }
        4 => {
            env[1] = format!("{}=/login/other", accounts::LOGIN_ENV);
            snapshot.login = Some("/login/other".into());
        }
        5 => {
            env.push(format!("{}=synthetic", accounts::CREDENTIAL_KEYS[0]));
            snapshot.stored_login = false;
        }
        6 => {
            env.push(format!("{}=/another/home", accounts::HOME_ENV));
            snapshot.home = Some("/another/home".into());
        }
        9 => {
            env[1] = format!("{}=/login/old", accounts::LOGIN_ENV);
            snapshot.login = Some("/login/old".into());
        }
        10 => env.clear(),
        _ => {}
    }
    let expected = snapshot.clone();
    let called = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&called);
    hooks.acts.account_env = Arc::new(move |seen| {
        assert_eq!(
            seen, &expected,
            "the full saved snapshot reaches the reader"
        );
        calls.fetch_add(1, Ordering::SeqCst);
        (case != 3).then(|| env.clone())
    });
    hooks.acts.account_settings = Arc::new(move |_, _, _| case != 7);
    hooks.acts.route = Arc::new(move |_, _, _| {
        if case == 8 {
            Route::Custom("synthetic override".into())
        } else {
            Route::Default
        }
    });
    let idle = idle_host("s-application", Program::Claude, switched(&on()), hooks);
    let outcome = applied(&idle, &roster, target, &snapshot);
    assert_eq!(
        called.load(Ordering::SeqCst),
        1,
        "a snapshot label cannot bypass the reader"
    );
    outcome
}

pub(crate) fn application_transition(
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
        "Observe" => {
            let outcome = match application_case(state["case"]) {
                Applied::Yes => 1,
                Applied::No => 2,
                Applied::Unread => 3,
            };
            next.insert("phase", 2);
            next.insert("outcome", outcome);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn an_account_move_needs_current_stored_login_evidence() {
    let model = aterm_models_harness::account_application_model();
    assert_eq!(model.mutant_dials(), (1..=11).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) >= 10);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = application_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, outcome, law) in [
        (5, 1, "CurrentStoredOnly"),
        (1, 2, "AvailableProgress"),
        (3, 2, "UnprovenIsUnknown"),
        (4, 3, "KnownRefusal"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = application_transition(&state, "Observe");
        bad.insert("outcome", outcome);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn an_unreadable_replacement_keeps_the_move_pending() {
    for missing in ["process", "roster", "settings", "route"] {
        let a = Accts::new();
        let sid = "s-pending-process";
        let old = a.spawn(sid, ACCT_TALK_A, "main");
        let mut idle = a.worker(sid, &on());
        let new = a.spawn(sid, ACCT_TALK_A, "work-2");
        let flying = Flying {
            origin: a.seat().context(),
            to: "work-2".into(),
            at: AccountAt::Wall,
            login: None,
            pid: old.pid,
            start: old.start,
            rejected: None,
        };
        {
            let mut kept = idle.kept.lock().unwrap();
            kept.snapshot = Some(new);
            kept.account.flying = Some(flying.clone());
        }
        let original = Arc::clone(&idle.hooks.acts.account_env);
        let settings = Arc::clone(&idle.hooks.acts.account_settings);
        let route = Arc::clone(&idle.hooks.acts.route);
        match missing {
            "process" => idle.hooks.acts.account_env = Arc::new(|_| None),
            "roster" => std::fs::write(a.roster_path(), "rotate = true\n").unwrap(),
            "settings" => idle.hooks.acts.account_settings = Arc::new(|_, _, _| false),
            "route" => idle.hooks.acts.route = Arc::new(|_, _, _| Route::Custom("unread".into())),
            _ => unreachable!(),
        }
        let before = images(&a);
        account_land(sid, &idle.kept, &idle.hooks, Landing::Asked);
        assert_eq!(images(&a), before, "{missing}");
        assert_eq!(idle.kept.lock().unwrap().account.flying, Some(flying));
        idle.hooks.acts.account_env = original;
        idle.hooks.acts.account_settings = settings;
        idle.hooks.acts.route = route;
        a.write_roster();
        account_land(sid, &idle.kept, &idle.hooks, Landing::Asked);
        assert!(idle.kept.lock().unwrap().account.flying.is_none());
        assert_eq!(a.state().sessions[sid].account, "work-2");
    }
}

#[test]
fn a_pending_move_refreshes_a_snapshot_that_no_longer_matches() {
    let a = Accts::new();
    let sid = "s-refreshed-process";
    let old = a.spawn(sid, ACCT_TALK_A, "main");
    let mut idle = a.worker(sid, &on());
    let current = a.spawn(sid, ACCT_TALK_A, "work-2");
    let expected = current.clone();
    let env = a.procs.lock().unwrap()[&current.pid].clone();
    idle.hooks.acts.account_env = Arc::new(move |seen| (seen == &expected).then(|| env.clone()));
    {
        let mut kept = idle.kept.lock().unwrap();
        kept.snapshot = Some(Snapshot {
            login: None,
            ..current.clone()
        });
        kept.account.flying = Some(Flying {
            origin: a.seat().context(),
            to: "work-2".into(),
            at: AccountAt::Wall,
            login: None,
            pid: old.pid,
            start: old.start,
            rejected: None,
        });
    }
    account_land(sid, &idle.kept, &idle.hooks, Landing::Asked);
    let kept = idle.kept.lock().unwrap();
    assert!(kept.account.flying.is_none());
    assert_eq!(kept.snapshot.as_ref().unwrap().login, current.login);
    assert_eq!(a.state().sessions[sid].account, "work-2");
}

#[cfg(target_os = "macos")]
#[test]
fn live_account_decisions_refuse_a_different_snapshot_without_writes() {
    use aterm_uds::spawnfd::{Child, Stdio, command};
    use std::io::BufRead as _;

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            aterm_uds::deadline::kill_within(&mut self.0, Duration::from_secs(60));
        }
    }
    let mut cmd = command(std::env::current_exe().unwrap());
    cmd.args([
        "harness_host::tests::a_stand_in_agent_when_asked",
        "--exact",
    ])
    .env_clear()
    .env(STAND_IN_ENV, "1")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut running = Running(cmd.spawn().expect("stand-in"));
    let stdout = running.0.stdout.take().unwrap();
    static READY: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("account-process-ready");
    let ready = READY
        .run(
            Duration::from_secs(60),
            Duration::from_secs(60),
            move || {
                std::io::BufReader::new(stdout)
                    .lines()
                    .map_while(Result::ok)
                    .any(|line| line == "ready")
            },
        )
        .expect("stand-in startup");
    assert!(ready, "stand-in did not report its startup");

    let pid = running.0.id();
    let args = atpkg::caller_shell::process_args(pid).expect("stand-in environment");
    let image = aterm_sysprobe::process_path(pid).expect("stand-in image");
    let (program, argv) =
        relaunch::launched(&args.argv, Some(&image), Path::new(""), true).unwrap();
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).unwrap()).unwrap();
    let captured = Snapshot {
        pid,
        program,
        argv,
        start: aterm_agent::harness::footer::lstart_utc(birth.seconds),
        stored_login: true,
        ..snap("s-live-process")
    };
    for change in ["same", "start", "login", "command", "credential"] {
        let a = Accts::new();
        let sid = "s-live-process";
        let mut idle = a.worker(sid, &on());
        let production = live_acts(
            crate::session_store::new_store(),
            a.tmp.path().join("no.sock").to_string_lossy().into_owned(),
        );
        idle.hooks.acts.account_env = production.account_env;
        let mut observed = captured.clone();
        match change {
            "start" => observed.start.push_str("-stale"),
            "login" => observed.login = Some("/another/login".into()),
            "command" => observed.argv.push("--settings=another".into()),
            "credential" => observed.stored_login = false,
            _ => {}
        }
        idle.kept.lock().unwrap().snapshot = Some(observed.clone());
        let roster = a.roster();
        let accepted = change == "same";
        assert_eq!(idle.account_look(&roster).is_some(), accepted, "{change}");
        assert_eq!(idle.latch_key().is_some(), accepted, "{change}");
        assert_eq!(
            applied(&idle, &roster, "main", &observed),
            if accepted {
                Applied::Yes
            } else {
                Applied::Unread
            },
            "{change}"
        );
        let before = images(&a);
        account_answered(&idle, AccountBad::SignedOut);
        if !accepted {
            assert_eq!(images(&a), before, "{change}");
        }

        if change == "login" {
            // Negative control: the old PID-only reader attributes this stale snapshot.
            idle.hooks.acts.account_env = Arc::new(|snapshot| {
                atpkg::caller_shell::process_args(snapshot.pid).map(|args| args.env)
            });
            assert!(idle.account_look(&roster).is_some());
        }
    }
}
