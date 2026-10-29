// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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

fn redirect(hooks: &mut Hooks, a: &Accts, field: i64, at_writer: bool) {
    let original = Arc::clone(&hooks.acts.account_seat);
    let foreign = a.tmp.path().join("foreign");
    let calls = AtomicUsize::new(0);
    hooks.acts.account_seat = Arc::new(move || {
        let mut seat = original()?;
        let read = calls.fetch_add(1, Ordering::SeqCst);
        if !at_writer || read > 0 {
            match field {
                0 => seat.state.clone_from(&foreign),
                1 => seat.roster = Some(foreign.clone()),
                2 => seat.home.clone_from(&foreign),
                3 => return None,
                4 => {
                    seat.ladder = &[];
                }
                _ => unreachable!(),
            }
        }
        Some(seat)
    });
}

// Project both the first attempt and its retry, so refusal cannot hide lost work.
fn origin_case(case: i64) -> [i64; 9] {
    let fallback = case >= 17;
    let rejected = case == 2 || (7..=14).contains(&case) || case == 16;
    let (a, mut idle) = if fallback {
        moved_at_wall("s-origin-back")
    } else if rejected {
        let (a, idle, held) = account_immediate::blocked_rejection();
        drop(held.lock().unwrap().take());
        (a, idle)
    } else {
        account_landing::pending(false)
    };
    let original = Arc::clone(&idle.hooks.acts.account_seat);
    let before = images(&a);
    match case {
        3..=6 => redirect(&mut idle.hooks, &a, case - 3, false),
        7..=10 => redirect(&mut idle.hooks, &a, case - 7, false),
        11..=14 => redirect(&mut idle.hooks, &a, case - 11, true),
        15 | 16 => {
            a.now.store(ACCT_NOW + 60, Ordering::SeqCst);
            redirect(&mut idle.hooks, &a, 4, false);
        }
        18..=21 => redirect(&mut idle.hooks, &a, case - 18, false),
        22 => redirect(&mut idle.hooks, &a, 0, true),
        _ => {}
    }
    let held = (case == 23).then(|| a.hold_lock());
    let gate = Arc::clone(&idle.kept.lock().unwrap().account.landing);
    let busy = (case == 24).then(|| gate.lock().unwrap());
    let job = acct_job(&idle.sid, &idle);
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let result = if fallback {
        account_way_back(&job, &idle.hooks, &mut snap);
        None
    } else {
        Some(account_land(
            &idle.sid,
            &idle.kept,
            &idle.hooks,
            Landing::Carried,
        ))
    };
    let pending = |idle: &WorkerIdle| {
        let kept = idle.kept.lock().unwrap();
        if fallback {
            kept.account.moved.is_some()
        } else {
            kept.account.flying.is_some()
        }
    };
    let first_pending = pending(&idle);
    let refused = result.map_or(first_pending, |result| result == Applied::Unread);
    let changed = images(&a) != before;
    let first_notified = a.asked("made:");
    let foreign = a.tmp.path().join("foreign").exists();
    if refused {
        assert!(!changed, "case {case}: no origin write on refusal");
        assert_eq!(first_notified, 0, "case {case}");
        assert_eq!(
            snap.login,
            idle.kept.lock().unwrap().snapshot.as_ref().unwrap().login,
            "case {case}: no fallback before the write"
        );
    }
    if matches!(case, 15 | 16) {
        assert_eq!(a.state().sessions[&idle.sid].at, a.clock());
    }
    drop(held);
    drop(busy);
    idle.hooks.acts.account_seat = original;
    if refused {
        if fallback {
            account_way_back(&job, &idle.hooks, &mut snap);
        } else {
            assert_eq!(
                account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
                if rejected { Applied::No } else { Applied::Yes }
            );
        }
    }
    let state = a.state();
    let correct = if fallback {
        snap.login.is_none() && state.active.is_none()
    } else if rejected {
        state.sessions[&idle.sid].account == "main" && state.active.is_none()
    } else {
        state.sessions[&idle.sid].account == "work-2"
    };
    [
        i64::from(refused),
        i64::from(first_pending),
        i64::from(changed),
        i64::from(foreign),
        i64::try_from(first_notified).unwrap(),
        i64::from(pending(&idle)),
        i64::from(correct),
        i64::try_from(a.asked("restart:")).unwrap(),
        i64::try_from(a.asked("made:")).unwrap(),
    ]
}

pub(crate) fn origin_transition(
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
            let values = origin_case(state["case"]);
            next.insert("phase", 2);
            for (key, value) in [
                "refused",
                "pending",
                "changed",
                "foreign",
                "notified",
                "left",
                "correct",
                "restarts",
                "final_notified",
            ]
            .into_iter()
            .zip(values)
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_origins_conform_through_context_changes_and_retries() {
    let model = aterm_models_harness::account_origin_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 24);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = origin_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (3, "changed", 1, "RefusalPreservesOrigin"),
        (8, "foreign", 1, "NoForeignWrites"),
        (5, "pending", 0, "PendingRetained"),
        (1, "left", 1, "RetryCompletes"),
        (10, "restarts", 2, "OneRestart"),
        (3, "notified", 1, "NoPrematureNotification"),
        (15, "refused", 1, "ClockAndWaitAreNotIdentity"),
        (17, "correct", 0, "RetryCompletes"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = origin_transition(&state, "Observe");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn an_untold_move_remains_with_its_origin() {
    let (a, idle) = account_landing::pending(false);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Asked),
        Applied::Yes
    );
    let look = idle.account_look(&a.roster()).unwrap();
    let mut foreign = a.seat();
    foreign.state = a.tmp.path().join("foreign");
    assert_eq!(
        idle.account_untold(&foreign, &look, usage_wall(), true),
        None
    );
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
    let gate = Arc::clone(&idle.kept.lock().unwrap().account.landing);
    let held = gate.lock().unwrap();
    assert_eq!(
        idle.account_untold(&a.seat(), &look, usage_wall(), true),
        None
    );
    drop(held);
    assert_eq!(
        idle.account_untold(&a.seat(), &look, usage_wall(), true)
            .as_deref(),
        Some("work-2")
    );
}

#[test]
fn fallback_cannot_publish_from_a_replaced_session_generation() {
    let (a, idle) = moved_at_wall("s-origin-replaced");
    let job = acct_job(&idle.sid, &idle);
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let before_snap = snap.clone();
    let before = images(&a);
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |_| {
        let mut kept = kept.lock().unwrap();
        let moved = kept.account.moved.clone();
        kept.account = AccountKept::default();
        kept.account.moved = moved;
    });
    account_way_back(&job, &idle.hooks, &mut snap);
    assert_eq!(snap, before_snap);
    assert_eq!(images(&a), before);
    assert!(idle.kept.lock().unwrap().account.moved.is_some());
}

#[test]
fn interrupted_fallback_recovers_before_consuming_its_receipt() {
    let (a, idle) = moved_at_wall("s-origin-receipt");
    let job = acct_job(&idle.sid, &idle);
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let before = snap.clone();
    let store = a.store();
    let wake = store.wake_path();
    if wake.exists() {
        std::fs::rename(&wake, wake.with_extension("prior")).unwrap();
    }
    std::fs::create_dir(&wake).unwrap();
    account_way_back(&job, &idle.hooks, &mut snap);
    assert_eq!(snap, before, "no fallback before durable completion");
    assert!(idle.kept.lock().unwrap().account.moved.is_some());
    assert!(
        store.load(a.clock()).is_err(),
        "unfinished receipt fences readers"
    );
    let written = std::fs::read(store.ledger_path()).unwrap();

    std::fs::rename(&wake, wake.with_extension("obstruction")).unwrap();
    account_way_back(&job, &idle.hooks, &mut snap);
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert!(snap.login.is_none());
    assert_eq!(
        std::fs::read(store.ledger_path()).unwrap(),
        written,
        "one strike"
    );
    assert!(a.state().active.is_none());
    assert!(wake.is_file());
}

#[test]
fn the_exit_relaunch_waits_for_fallback_and_updates_its_saved_snapshot() {
    let (a, idle) = moved_at_wall("s-origin-relaunch");
    let job = acct_job(&idle.sid, &idle);
    let mut snap = idle.kept.lock().unwrap().snapshot.clone().unwrap();
    let before = snap.clone();
    idle.kept.lock().unwrap().owed = Some(OwedRelaunch {
        snap: snap.clone(),
        asked: None,
    });
    let calls = AtomicUsize::new(0);
    let act = |snap: &Snapshot| {
        calls.fetch_add(1, Ordering::SeqCst);
        assert!(snap.login.is_none());
        "adopted".to_string()
    };
    let held = a.hold_lock();
    assert_eq!(
        account_relaunch(&job, &idle.hooks, &mut snap, act),
        "wait:account-state"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(snap, before);
    assert!(idle.kept.lock().unwrap().account.moved.is_some());
    assert_eq!(
        idle.kept.lock().unwrap().owed.as_ref().unwrap().snap,
        before
    );
    drop(held);
    assert_eq!(
        account_relaunch(&job, &idle.hooks, &mut snap, act),
        "adopted"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(idle.kept.lock().unwrap().owed.as_ref().unwrap().snap, snap);
    assert_eq!(a.asked("restart:"), 1, "only the original rotation restart");
}

#[test]
fn a_completed_move_cannot_be_consumed_by_an_old_session_generation() {
    let (a, idle) = account_landing::pending(false);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Asked),
        Applied::Yes
    );
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(1, move |_| {
        let mut kept = kept.lock().unwrap();
        let moved = kept.account.moved.clone();
        kept.account = AccountKept::default();
        kept.account.moved = moved;
    });
    let before = images(&a);
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .untold
    );
    assert_eq!(images(&a), before);
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn the_host_exit_loop_retries_a_blocked_account_fallback() {
    let (a, idle) = moved_at_wall("s-origin-exit-loop");
    let job = acct_job(&idle.sid, &idle);
    let held = Arc::new(Mutex::new(Some(a.hold_lock())));
    let mut hooks = a.hooks();
    let read = Arc::clone(&hooks.acts.account_seat);
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    hooks.acts.account_seat = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        read()
    });
    let release = Arc::clone(&held);
    hooks.pause = Arc::new(move |_| {
        if reads.load(Ordering::SeqCst) >= 2 {
            drop(release.lock().unwrap().take());
        }
        Duration::ZERO
    });
    hooks.acts.exit_look = Arc::new(|_, snap| {
        Some(ExitLook {
            running: false,
            record: Some(SessionFile {
                pid: snap.pid,
                session_id: snap.session.clone().unwrap(),
                cwd: snap.cwd.clone(),
                version: "2.1.287".into(),
                status: "busy".into(),
                status_updated_at_ms: 1,
                proc_start: snap.start.clone(),
                kind: "interactive".into(),
                entrypoint: "cli".into(),
                claude_dir: None,
                profile: None,
            }),
            agent: aterm_agent::harness::upgrade::Agent::Claude,
            crashed: None,
            refused: false,
        })
    });
    let relaunched = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&relaunched);
    let world = Arc::clone(&a);
    let writer = Arc::clone(&held);
    hooks.acts.relaunch = Arc::new(move |sid, _, snap, _, _, _| {
        assert!(writer.lock().unwrap().is_none());
        assert!(
            snap.login.is_none(),
            "the loop retried the unrecorded fallback"
        );
        assert!(
            world.state().active.is_none(),
            "rollback committed before relaunch"
        );
        counted.fetch_add(1, Ordering::SeqCst);
        world.exec(sid, ACCT_TALK_A, snap.login.as_deref(), world.clock());
        "adopted".into()
    });
    job.left.hand();
    job.stop.store(true, Ordering::SeqCst);
    on_agent_left(&job, &hooks);
    assert_eq!(relaunched.load(Ordering::SeqCst), 1);
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
}
