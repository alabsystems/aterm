// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_immediate_move_waits_for_its_completion_write() {
    let a = Accts::new();
    let sid = "s-immediate-write";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    let held: Arc<Mutex<Option<accounts::Lock>>> = Arc::default();
    let held_by_restart = Arc::clone(&held);
    let world = Arc::clone(&a);
    let original = Arc::clone(&idle.hooks.acts.restart);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        let step = original(sid, grace, why, selected);
        *held_by_restart.lock().unwrap() = Some(world.hold_lock());
        step
    });
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    {
        let kept = idle.kept.lock().unwrap();
        assert!(kept.account.flying.is_some());
        assert!(kept.account.moved.is_none());
    }
    assert_eq!(a.state().sessions[sid].account, "main");
    assert_eq!(a.ledger(), ["wall", "rotate"]);
    drop(held.lock().unwrap().take());
    account_land(sid, &idle.kept, &idle.hooks, Landing::Asked);
    assert_eq!(a.state().sessions[sid].account, "work-2");
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
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
    assert_eq!(a.asked("restart:"), 1);
    assert_eq!(a.ledger(), ["wall", "rotate", "move"]);
    account_land(sid, &idle.kept, &idle.hooks, Landing::Asked);
    assert_eq!(a.ledger(), ["wall", "rotate", "move"]);
}

fn immediate_case(case: i64) -> [i64; 8] {
    let a = Accts::new();
    let sid = "s-immediate-model";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    let held: Arc<Mutex<Option<accounts::Lock>>> = Arc::default();
    let held_by_restart = Arc::clone(&held);
    let prepared = Arc::new(AtomicBool::new(true));
    let prepared_by_restart = Arc::clone(&prepared);
    let world = Arc::clone(&a);
    let kept = Arc::clone(&idle.kept);
    let original = Arc::clone(&idle.hooks.acts.restart);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        let gate = Arc::clone(&kept.lock().unwrap().account.landing);
        let locked = matches!(gate.try_lock(), Err(std::sync::TryLockError::WouldBlock));
        let rotations = world
            .ledger()
            .iter()
            .filter(|word| **word == "rotate")
            .count();
        prepared_by_restart.fetch_and(
            locked && rotations == world.asked("restart:") + 1,
            Ordering::SeqCst,
        );
        let step = original(sid, grace, why, selected);
        if matches!(case, 2 | 3) {
            *held_by_restart.lock().unwrap() = Some(world.hold_lock());
        }
        if case == 8 {
            kept.lock().unwrap().account = AccountKept::default();
            accounts::record_gone(&world.store(), sid, world.clock()).unwrap();
        }
        step
    });
    if matches!(case, 4 | 5) {
        a.answers.lock().unwrap().push_back("wait:changed");
    }
    if case == 4 {
        a.answers.lock().unwrap().push_back("wait:changed");
    }
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    if case == 7 {
        *held.lock().unwrap() = Some(a.hold_lock());
    }
    let _ = idle.restart(&move_to("work-2", AccountAt::Wall));
    if case == 3 {
        drop(held.lock().unwrap().take());
        account_land(sid, &idle.kept, &idle.hooks, Landing::Asked);
    }
    if matches!(case, 4 | 5) {
        assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
        let _ = idle.restart(&move_to("work-2", AccountAt::Wall));
    }
    let target = if case == 6 {
        idle.account_served();
        a.store()
            .transact(a.clock(), &mut None, |state| {
                state.accounts.get_mut("main").unwrap().walls.clear();
                Ok(Vec::new())
            })
            .unwrap();
        a.now.store(ACCT_NOW + 30, Ordering::SeqCst);
        a.wall(ACCT_TALK_A, ACCT_NOW + 20);
        assert_eq!(moves(&idle.account(usage_wall())), Some("main"));
        let _ = idle.restart(&move_to("main", AccountAt::Wall));
        "main"
    } else {
        "work-2"
    };
    let state = a.state();
    let ledger = a.ledger();
    let kept = idle.kept.lock().unwrap();
    [
        i64::from(prepared.load(Ordering::SeqCst)),
        i64::try_from(a.asked("restart:")).unwrap(),
        i64::try_from(ledger.iter().filter(|word| **word == "rotate").count()).unwrap(),
        i64::try_from(ledger.iter().filter(|word| **word == "struck").count()).unwrap(),
        i64::from(kept.account.moved.is_some()),
        i64::from(kept.account.flying.is_some()),
        i64::from(
            state
                .sessions
                .get(sid)
                .is_some_and(|row| row.account == target),
        ),
        i64::from(state.sessions.contains_key(sid)),
    ]
}

pub(crate) fn immediate_transition(
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
            let values = immediate_case(state["case"]);
            next.insert("phase", 2);
            for (name, value) in [
                "prepared",
                "restarts",
                "rotations",
                "strikes",
                "moved",
                "pending",
                "bound",
                "present",
            ]
            .into_iter()
            .zip(values)
            {
                next.insert(name, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn immediate_completion_preserves_initiation_and_resolution() {
    let model = aterm_models_harness::account_immediate_model();
    assert_eq!(model.mutant_dials(), (1..=12).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 24);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = immediate_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (1, "prepared", 0, "PreparedBeforeRestart"),
        (2, "moved", 1, "CommittedBeforeMoved"),
        (2, "pending", 0, "RetainUnwritten"),
        (3, "restarts", 2, "RetryWithoutRestart"),
        (4, "rotations", 1, "AttemptsRetained"),
        (4, "strikes", 1, "FailedAttemptsRetained"),
        (7, "restarts", 1, "RefusedBeforeRestart"),
        (8, "present", 1, "RetiredIsQuiet"),
        (1, "moved", 0, "SuccessCompletes"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = immediate_transition(&state, "Observe");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

pub(super) fn blocked_rejection() -> (Arc<Accts>, WorkerIdle, Arc<Mutex<Option<accounts::Lock>>>) {
    let a = Accts::new();
    let sid = "s-rejected-write";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    a.answers.lock().unwrap().push_back("wait:changed");
    let held = Arc::new(Mutex::new(None));
    let held_by_restart = Arc::clone(&held);
    let world = Arc::clone(&a);
    let original = Arc::clone(&idle.hooks.acts.restart);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        let step = original(sid, grace, why, selected);
        *held_by_restart.lock().unwrap() = Some(world.hold_lock());
        step
    });
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    assert!(
        idle.kept
            .lock()
            .unwrap()
            .account
            .flying
            .as_ref()
            .unwrap()
            .rejected
            .is_some()
    );
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(a.ledger(), ["wall", "rotate"]);
    (a, idle, held)
}

#[test]
fn a_rejected_restart_keeps_its_rollback_until_recorded() {
    let (a, idle, held) = blocked_rejection();
    assert_eq!(a.state().active.as_deref(), Some("work-2"));
    drop(held.lock().unwrap().take());
    assert!(matches!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Asked),
        Applied::No
    ));
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(a.state().active, None);
    assert!(a.state().rotations.is_empty());
    assert_eq!(a.ledger(), ["wall", "rotate", "struck"]);
    account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Asked);
    assert_eq!(a.ledger(), ["wall", "rotate", "struck"]);
    assert_eq!(a.asked("made:"), 0);
}

#[test]
fn a_pending_rejection_preserves_new_session_and_canary_state() {
    for change in ["session", "canary", "gone"] {
        let (a, idle, held) = blocked_rejection();
        drop(held.lock().unwrap().take());
        a.store()
            .transact(a.clock(), &mut None, |state| {
                let events = match change {
                    "session" => {
                        accounts::note_session(state, &idle.sid, "pool-a", None, a.clock())
                    }
                    "canary" => {
                        state.canary.as_mut().unwrap().sid = "another-session".into();
                        Vec::new()
                    }
                    "gone" => {
                        accounts::forget_session(state, &idle.sid);
                        Vec::new()
                    }
                    _ => unreachable!(),
                };
                Ok(events)
            })
            .unwrap();
        let before = a.state();
        let ledger = a.ledger();
        assert!(matches!(
            account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Asked),
            Applied::No
        ));
        assert_eq!(a.state(), before, "{change}");
        assert_eq!(a.ledger(), ledger, "{change}");
        assert!(idle.kept.lock().unwrap().account.flying.is_none());
        assert!(idle.kept.lock().unwrap().account.moved.is_none());
    }
}

#[test]
fn a_rejection_receipt_recovers_without_repeating_the_failure() {
    let a = Accts::new();
    let sid = "s-rejection-receipt";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    a.answers.lock().unwrap().push_back("wait:changed");
    let world = Arc::clone(&a);
    let original = Arc::clone(&idle.hooks.acts.restart);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        let step = original(sid, grace, why, selected);
        assert!(!world.store().wake_path().exists());
        std::fs::create_dir(world.store().wake_path()).unwrap();
        step
    });
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    assert!(a.store().load(a.clock()).is_err());
    let written = std::fs::read(a.store().ledger_path()).unwrap();
    let wake = a.store().wake_path();
    std::fs::rename(&wake, wake.with_extension("obstruction")).unwrap();
    assert!(matches!(
        account_land(sid, &idle.kept, &idle.hooks, Landing::Asked),
        Applied::No
    ));
    assert_eq!(std::fs::read(a.store().ledger_path()).unwrap(), written);
    assert_eq!(a.ledger(), ["wall", "rotate", "struck"]);
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(a.asked("made:"), 0);
}
