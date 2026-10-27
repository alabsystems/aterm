// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn fallback_rechecks_authentication_context_at_the_writer() {
    let (a, idle) = moved_at_wall("s-return-auth");
    let job = acct_job(&idle.sid, &idle);
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let before = snap.clone();
    let state = a.state();
    let ledger = a.ledger();
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |_| {
        kept.lock().unwrap().snapshot.as_mut().unwrap().stored_login = false;
    });
    assert!(!account_way_back(&job, &idle.hooks, &mut snap));
    assert_eq!(snap, before);
    assert_eq!(a.state(), state);
    assert_eq!(a.ledger(), ledger);
    assert!(idle.kept.lock().unwrap().account.moved.is_some());
    assert!(idle.kept.lock().unwrap().account.came_back.is_none());
}

#[test]
fn fallback_rechecks_revocation_at_the_writer() {
    let (a, idle) = moved_at_wall("s-return-revoked");
    let job = acct_job(&idle.sid, &idle);
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let before = snap.clone();
    let state = a.state();
    let revoked = Arc::clone(&job.revoked);
    a.at_seat_read(2, move |_| revoked.store(true, Ordering::SeqCst));
    assert!(!account_way_back(&job, &idle.hooks, &mut snap));
    assert_eq!(snap, before);
    assert_eq!(a.state(), state);
    assert!(idle.kept.lock().unwrap().account.moved.is_some());
}

#[test]
fn accounts_off_cannot_finish_a_revoked_workers_return() {
    let (_, idle) = moved_at_wall("s-return-off");
    let job = acct_job(&idle.sid, &idle);
    job.switches
        .account_controls
        .fetch_and(!ACCOUNTS_MODE, Ordering::SeqCst);
    job.revoked.store(true, Ordering::SeqCst);
    let mut snap = job.kept.lock().unwrap().snapshot.clone().unwrap();
    assert!(!account_way_back(&job, &idle.hooks, &mut snap));
    assert!(job.kept.lock().unwrap().account.moved.is_some());
}

// Every case calls the production commit with a change made after capturing
// the return. No account store, process probe or timing is needed for this boundary.
fn return_case(case: i64) -> [i64; 7] {
    let job = left_job("s-return-model", Program::Claude, &on());
    let mut request = snap(&job.sid);
    request.login = Some("/example/target-login".into());
    let moved = Moved {
        id: MoveId::default(),
        origin: AccountContext {
            roster: Some("/example/accounts.toml".into()),
            state: "/example/state".into(),
            home: "/example".into(),
        },
        to: "target".into(),
        login: None,
        process: request.clone(),
        answered: false,
        untold: false,
    };
    let gate = {
        let mut kept = job.kept.lock().unwrap();
        kept.snapshot = Some(request.clone());
        kept.account.moved = Some(moved.clone());
        kept.account.came_back = Some(Some("/example/earlier-return".into()));
        Arc::clone(&kept.account.landing)
    };
    if matches!(case, 2 | 14) {
        job.revoked.store(true, Ordering::SeqCst);
    }
    {
        let mut kept = job.kept.lock().unwrap();
        match case {
            3 => kept.account.landing = Arc::default(),
            4 => kept.account.moved.as_mut().unwrap().id = MoveId::default(),
            5 => kept.snapshot.as_mut().unwrap().stored_login = !request.stored_login,
            6 => request.home = Some("/example/different-home".into()),
            7 => kept.snapshot = None,
            8 => {
                kept.account.flying = Some(Flying {
                    origin: moved.origin.clone(),
                    to: "newer-target".into(),
                    at: AccountAt::Wall,
                    login: moved.process.login.clone(),
                    pid: moved.process.pid,
                    start: moved.process.start.clone(),
                    rejected: None,
                });
            }
            9 => {
                let current = kept.snapshot.as_mut().unwrap();
                current.cwd = "/example/new-directory".into();
                current.session = Some("new-record".into());
                current.version = Some("new-version".into());
            }
            12 => kept.account.moved = None,
            13 => kept.account.moved.as_mut().unwrap().to = "different-target".into(),
            15 => request.cwd = "/example/new-request-directory".into(),
            _ => {}
        }
        kept.owed = Some(OwedRelaunch {
            snap: request.clone(),
            asked: None,
        });
        if case == 11 {
            kept.owed.as_mut().unwrap().snap.login = Some("/example/newer-request".into());
        }
    }
    let before = {
        let kept = job.kept.lock().unwrap();
        (
            kept.account.moved.clone(),
            kept.account.came_back.clone(),
            kept.owed.clone(),
            request.clone(),
            kept.snapshot.clone(),
        )
    };
    let accepted = account_finish_return(&job, &gate, &moved, &mut request, case != 10);
    let kept = job.kept.lock().unwrap();
    let intact = kept.account.moved == before.0
        && kept.account.came_back == before.1
        && kept.owed == before.2
        && request == before.3;
    [
        i64::from(accepted),
        i64::from(intact),
        i64::from(kept.account.moved.is_none()),
        i64::from(request.login == moved.login),
        i64::from(kept.account.came_back == Some(moved.login.clone())),
        i64::from(kept.owed != before.2),
        i64::from(kept.snapshot != before.4),
    ]
}

pub(crate) fn return_transition(
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
        "Commit" => {
            next.insert("phase", 2);
            for (key, value) in [
                "accepted", "intact", "consumed", "returned", "marker", "owed", "observed",
            ]
            .into_iter()
            .zip(return_case(state["case"]))
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_return_conforms_at_every_local_commit_boundary() {
    let model = aterm_models_harness::account_return_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 15);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = return_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, key, value, law) in [
        (5, "accepted", 1, "Admission"),
        (2, "intact", 0, "RefusalIsIntact"),
        (1, "consumed", 0, "ReceiptConsumed"),
        (1, "returned", 0, "ReturnChoice"),
        (2, "marker", 1, "MarkerTogether"),
        (11, "owed", 1, "OwedBelongsToRequest"),
        (1, "observed", 1, "ObservationPreserved"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = return_transition(&state, "Commit");
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
