// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts;
use crate::harness::relaunch::{AgentRun, Snapshot};
use aterm_uds::process::{Birth, Identity};

fn snapshot() -> Snapshot {
    Snapshot {
        tab: "s-process".into(),
        pid: 4242,
        start: "Mon Oct 26 01:02:03 2026".into(),
        shell: 4241,
        program: "/synthetic/claude".into(),
        argv: vec!["/synthetic/claude".into()],
        session: Some("00000000-0000-0000-0000-000000000001".into()),
        cwd: "/synthetic".into(),
        version: None,
        run: AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
        stored_login: true,
        profile: None,
    }
}

fn read_case(case: i64) -> (bool, bool) {
    let mut snap = snapshot();
    let first = Identity {
        pid: if case == 4 { 4243 } else { 4242 },
        birth: Birth(123_456),
        idversion: (case != 16).then_some(7),
    };
    let second = Identity {
        birth: Birth(if case == 13 { 123_457 } else { 123_456 }),
        idversion: if case == 14 { Some(8) } else { first.idversion },
        ..first
    };
    let mut args = atpkg::caller_shell::ProcArgs {
        exec_path: "/synthetic/claude".into(),
        argv: snap.argv.clone(),
        env: vec!["HOME=/synthetic".into()],
    };
    if case == 2 || case == 15 {
        let login = if case == 15 {
            ""
        } else {
            "/synthetic/retained"
        };
        snap.login = Some(login.into());
        args.env.push(format!("{}={login}", accounts::LOGIN_ENV));
    }
    match case {
        8 => args.env.clear(),
        9 => args.env.push(format!("{}=/changed", accounts::LOGIN_ENV)),
        10 => args.env.push(format!("{}=/changed", accounts::HOME_ENV)),
        11 => args
            .env
            .push(format!("{}=synthetic", accounts::CREDENTIAL_KEYS[0])),
        17 => args.exec_path = "/another/claude".into(),
        18 => args.argv.push("--settings=changed.json".into()),
        19 => {
            args.exec_path = "/usr/bin/node".into();
            args.argv.insert(0, "/usr/bin/node".into());
        }
        _ => {}
    }
    let image = if case == 21 {
        None
    } else {
        Some(PathBuf::from(&args.exec_path))
    };
    if case == 20 {
        args.exec_path = "/alias/claude".into();
    }
    let original = args.env.clone();
    let mut reads = 0;
    let env = snapshot_login_env_with(
        &snap,
        || {
            reads += 1;
            if case == 3 || case == 12 && reads == 2 {
                None
            } else {
                Some(if reads == 1 { first } else { second })
            }
        },
        || match case {
            5 => Some("another start".into()),
            6 => None,
            _ => Some(snap.start.clone()),
        },
        || (case != 7).then_some(args),
        || image,
    );
    (env.is_some(), env.is_none_or(|env| env == original))
}

pub(crate) fn process_transition(
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
        "Read" => {
            let (accepted, intact) = read_case(state["case"]);
            next.insert("phase", 2);
            next.insert("accepted", i64::from(accepted));
            next.insert("intact", i64::from(intact));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn transcript_environment_keeps_the_snapshots_process_and_login() {
    let model = aterm_models_harness::transcript_process_model();
    assert_eq!(model.mutant_dials(), (1..=22).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) >= 21);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = process_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (9, "accepted", 1, "MatchingProcess"),
        (1, "accepted", 0, "MatchingProgress"),
        (1, "intact", 0, "CapturedEnvironment"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = process_transition(&state, "Read");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn transcript_environment_reads_the_live_kernel_identity() {
    let mut snap = snapshot();
    snap.pid = std::process::id();
    snap.start = kernel_start(snap.pid).expect("live process start");
    let args = atpkg::caller_shell::process_args(snap.pid).expect("own environment");
    (snap.program, snap.argv) =
        launched(&args.argv, exe_of(snap.pid).as_deref(), Path::new(""), true)
            .expect("normalized own command");
    snap.login = args.env_var(accounts::LOGIN_ENV).map(str::to_owned);
    snap.home = args.env_var(accounts::HOME_ENV).map(str::to_owned);
    snap.stored_login = accounts::ProcessLogin::of_exec_env(&args.env)
        .expect("own nonempty environment")
        .stored_login;
    assert!(
        snapshot_login_env(&snap).is_some(),
        "the live snapshot must agree"
    );
    snap.start.push_str("-stale");
    assert!(
        snapshot_login_env(&snap).is_none(),
        "a different start must refuse"
    );
}

#[test]
fn codex_snapshot_login_env_verifies_codex_home_and_self_claimed_auth_path() {
    let mut snap = snapshot();
    snap.program = "/synthetic/codex".into();
    snap.argv = vec!["/synthetic/codex".into()];
    snap.stored_login = false;
    snap.run = AgentRun::Codex(crate::harness::relaunch::CodexRun {
        home: "/synthetic/.codex".into(),
        home_env: Some("/synthetic/.codex".into()),
        self_claimed: Some(crate::harness::self_claimed::Resume {
            argv: vec!["/synthetic/codex".into()],
            session: "01900000-0000-7000-8000-000000000001".into(),
            codex_home: Some("/synthetic/.codex".into()),
            codex_auth_path: Some("/synthetic/logins/work/auth.json".into()),
            cwd: Some("/synthetic".into()),
        }),
    });
    let id = Identity {
        pid: 4242,
        birth: Birth(123_456),
        idversion: Some(7),
    };
    let read_env = |env: Vec<String>| {
        snapshot_login_env_with(
            &snap,
            || Some(id),
            || Some(snap.start.clone()),
            || {
                Some(atpkg::caller_shell::ProcArgs {
                    exec_path: "/synthetic/codex".into(),
                    argv: snap.argv.clone(),
                    env,
                })
            },
            || Some(PathBuf::from("/synthetic/codex")),
        )
    };
    let matching = vec![
        "HOME=/synthetic".to_string(),
        "CODEX_HOME=/synthetic/.codex".to_string(),
        "CODEX_AUTH_PATH=/synthetic/logins/work/auth.json".to_string(),
    ];
    assert_eq!(read_env(matching.clone()), Some(matching));
    assert_eq!(read_env(Vec::new()), None, "empty env must refuse");
    assert_eq!(
        read_env(vec![
            "HOME=/synthetic".into(),
            "CODEX_HOME=/other/.codex".into(),
            "CODEX_AUTH_PATH=/synthetic/logins/work/auth.json".into(),
        ]),
        None,
        "changed CODEX_HOME must refuse"
    );
    assert_eq!(
        read_env(vec![
            "HOME=/synthetic".into(),
            "CODEX_HOME=/synthetic/.codex".into(),
            "CODEX_AUTH_PATH=/synthetic/logins/other/auth.json".into(),
        ]),
        None,
        "changed CODEX_AUTH_PATH must refuse"
    );
}
