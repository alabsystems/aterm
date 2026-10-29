// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn observe_pool_session(model: &aterm_spec::interp::State) -> [i64; 4] {
    let a = Accts::new();
    let cli = accounts::Cli::Codex;
    let path = a.roster_path().with_file_name(cli.accounts().roster_file);
    std::fs::rename(a.roster_path(), &path).unwrap();
    let login = a.login_dir("work-2");
    std::fs::write(
        Path::new(&login).join("auth.json"),
        r#"{"tokens":{"access_token":"opaque"}}"#,
    )
    .unwrap();
    let sid = "s-pool-session";
    let mut snap = a.spawn(sid, ACCT_TALK_A, "work-2");
    snap.program = "/opt/bin/codex".into();
    snap.argv = vec!["/opt/bin/codex".into()];
    a.tabs.lock().unwrap().insert(sid.into(), snap.clone());
    a.procs
        .lock()
        .unwrap()
        .insert(snap.pid, vec![format!("CODEX_AUTH_PATH={login}/auth.json")]);
    let mut idle = a.worker(sid, &on());
    idle.agent = Program::Codex;
    let seat = a.seat();
    let pool_seat = accounts_pool::Seat {
        config_dir: path.parent().unwrap(),
        state: &seat.state,
        home: &seat.home,
        now: seat.now,
    };
    let roster = pool_seat.bound_roster(cli).unwrap();
    let (look, env) = idle.pool_account_look(cli, &roster).unwrap();
    assert!(
        idle.lost_seen(&look, AccountPoint::TurnEnd, seat.now)
            .is_some()
    );
    if model["present"] == 1 {
        let mut current = snap.clone();
        match model["process"] {
            2 => current.pid += 1,
            3 => current.start.push_str(" replaced"),
            4 => current.argv.push("--new-auth".into()),
            _ => {}
        }
        if model["login"] == 2 {
            current.login = Some("/new/login".into());
        }
        if model["metadata"] == 1 {
            current.cwd = "/new/directory".into();
            current.session = Some("new-metadata".into());
            current.version = Some("new-version".into());
        }
        a.tabs.lock().unwrap().insert(sid.into(), current);
        assert!(refresh_snapshot(sid, &idle.kept, &idle.hooks));
    } else {
        idle.kept.lock().unwrap().snapshot = None;
    }
    if model["generation"] == 2 {
        idle.kept.lock().unwrap().account.landing = Arc::default();
    }
    let store = pool_seat.store(cli);
    store
        .transact(seat.now, &mut None, |state| {
            state.active = Some("work-2".into());
            state.proven = false;
            state.canary = Some(accounts::Canary {
                sid: sid.into(),
                at: seat.now,
                took: false,
            });
            if model["present"] == 1 {
                accounts::note_session(state, sid, "pool-a", Some("newer"), seat.now);
            }
            Ok(Vec::new())
        })
        .unwrap();
    let before = store.load(seat.now).unwrap().0.sessions.get(sid).cloned();
    let pairs = env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .collect::<Vec<_>>();
    let applied = std::cell::Cell::new(false);
    let _ = accounts_pool::session_move(
        &pool_seat,
        &accounts_pool::Session {
            cli,
            sid,
            supervisor: accounts::Supervisor::Aterm,
            env: &pairs,
            sample: Some(accounts::Sample {
                at: seat.now,
                session_pct: Some(42),
                ..accounts::Sample::default()
            }),
            point: accounts::Point::TurnEnd,
            mode: accounts::Mode::Off,
            relaunch: relaunch::PoolRelaunch {
                dialect: upgrade::Dialect::Zsh,
                cd: Some(&look.snap.cwd),
                exe: &look.snap.program,
                flags: &[],
                resume: look.snap.session.as_deref(),
                home: None,
            },
        },
        |note| applied.set(look.note_pool_session(&idle.kept, note)),
    );
    let state = store.load(seat.now).unwrap().0;
    let row = state.sessions.get(sid);
    assert!(
        state.proven && state.canary.is_none(),
        "valid late account proof"
    );
    assert_eq!(
        state.accounts["work-2"]
            .sample
            .as_ref()
            .and_then(|s| s.session_pct),
        Some(42),
        "valid late usage evidence"
    );
    assert_eq!(state.accounts["work-2"].ran_at.is_some(), applied.get());
    let events = store
        .tail(100)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == accounts::Kind::Move && event.sid.as_deref() == Some(sid))
        .count();
    [
        i64::from(applied.get()),
        if row.is_some_and(|row| row.account == "work-2") {
            1
        } else {
            2
        },
        i64::from(row != before.as_ref()),
        i64::try_from(events).unwrap(),
    ]
}

#[test]
fn pooled_session_notes_recheck_the_local_generation_at_the_writer() {
    let model = aterm_models_harness::account_session_attribution_model();
    for case in 0..=7 {
        let mut state = model.init_state();
        state.insert("phase", 1);
        match case {
            1 => {
                state.insert("generation", 2);
            }
            2 => {
                state.insert("process", 2);
            }
            3 => {
                state.insert("process", 3);
            }
            4 => {
                state.insert("login", 2);
            }
            5 => {
                state.insert("present", 0);
            }
            6 => {
                state.insert("metadata", 1);
            }
            7 => {
                state.insert("process", 4);
            }
            _ => {}
        }
        let current = case == 0 || case == 6;
        assert_eq!(
            observe_pool_session(&state),
            [
                i64::from(current),
                if current { 1 } else { 2 },
                i64::from(current),
                i64::from(current)
            ],
            "interleaving {case}: session writes need current context; late account evidence survives"
        );
    }
}

pub(crate) fn session_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    if action != "Write" {
        return account_session::session_transition(state, action);
    }
    let mut next = state.clone();
    next.insert("phase", 2);
    for (key, value) in ["applied", "row", "changed", "events"]
        .into_iter()
        .zip(observe_pool_session(state))
    {
        next.insert(key, value);
    }
    next
}

#[test]
fn pooled_session_attribution_conforms_through_the_durable_writer() {
    let model = aterm_models_harness::account_session_attribution_model();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = session_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (field, value, law) in [
        ("generation", 2, "GenerationAdmitted"),
        ("process", 2, "ProcessAdmitted"),
        ("login", 2, "LoginAdmitted"),
        ("present", 0, "PresentAdmitted"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert(field, value);
        let mut bad = session_transition(&state, "Write");
        assert_eq!(bad["applied"], 0);
        assert_eq!(bad["changed"], 0);
        bad.insert("applied", 1);
        assert!(!model.check_invariant(law, &bad));
        bad.insert("applied", 0);
        bad.insert("row", 1);
        assert!(!model.check_invariant("RefusalPreserves", &bad));
    }
    let mut state = model.init_state();
    state.insert("phase", 1);
    let good = session_transition(&state, "Write");
    for (key, value, law) in [
        ("applied", 0, "CurrentCompletes"),
        ("row", 2, "WritesObservedRow"),
        ("events", 0, "RecordsTransition"),
    ] {
        let mut bad = good.clone();
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
