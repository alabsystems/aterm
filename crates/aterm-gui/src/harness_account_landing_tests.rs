// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) fn pending(same_login: bool) -> (Arc<Accts>, WorkerIdle) {
    let a = Accts::new();
    let sid = "s-landing-write";
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    a.unread.store(true, Ordering::SeqCst);
    a.same_login.store(same_login, Ordering::SeqCst);
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    a.readable();
    assert!(refresh_snapshot(sid, &idle.kept, &idle.hooks));
    (a, idle)
}

#[test]
fn a_pending_move_cannot_complete_into_another_store() {
    let (a, mut idle) = pending(false);
    let other = aterm_tempfile::TempDir::new().unwrap();
    let foreign = other.path().join("account-state");
    let original = Arc::clone(&idle.hooks.acts.account_seat);
    let changed = foreign.clone();
    idle.hooks.acts.account_seat = Arc::new(move || {
        original().map(|mut seat| {
            seat.state.clone_from(&changed);
            seat
        })
    });
    let before = a.state();
    let ledger = a.ledger();
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
        Applied::Unread,
        "completion must stay with the store that admitted the move"
    );
    assert!(!foreign.exists(), "the unrelated store remains untouched");
    assert_eq!(a.state(), before);
    assert_eq!(a.ledger(), ledger);
    assert!(idle.kept.lock().unwrap().account.flying.is_some());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(a.asked("made:"), 0);
}

#[test]
fn a_landing_rechecks_account_policy_before_recording_completion() {
    let (a, idle) = pending(false);
    let before = a.state();
    let ledger = a.ledger();
    a.at_seat_read(2, |a| {
        let text = std::fs::read_to_string(a.roster_path()).unwrap();
        let changed = text.replace(
            "label = \"work-2\"",
            "label = \"work-2\"\nemail = \"replacement@example.test\"",
        );
        assert_ne!(text, changed);
        std::fs::write(a.roster_path(), changed).unwrap();
    });
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
        Applied::Unread,
        "an observation of the old account cannot complete under new policy"
    );
    assert_eq!(a.state(), before);
    assert_eq!(a.ledger(), ledger);
    assert!(idle.kept.lock().unwrap().account.flying.is_some());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(a.asked("made:"), 0);
}

#[test]
fn a_landing_waits_for_its_state_write_before_it_is_published() {
    for same_login in [false, true] {
        let (a, idle) = pending(same_login);
        let before = a.state();
        let events = a.ledger();
        let held = a.hold_lock();
        let flight = idle.kept.lock().unwrap().account.flying.clone();
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
        {
            let kept = idle.kept.lock().unwrap();
            assert_eq!(
                kept.account.flying, flight,
                "the unrecorded move remains owed"
            );
            assert!(
                kept.account.moved.is_none(),
                "no completion before its write"
            );
        }
        assert_eq!(a.state(), before);
        assert_eq!(a.ledger(), events);
        assert_eq!(a.asked("made:"), 0);
        drop(held);

        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
        assert!(idle.kept.lock().unwrap().account.flying.is_none());
        assert_eq!(
            a.state().sessions[&idle.sid].account,
            if same_login { "main" } else { "work-2" }
        );
        assert_eq!(a.asked("made:"), usize::from(!same_login));
        let events = a.ledger();
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
        assert_eq!(a.ledger(), events, "one completion");
        assert_eq!(a.asked("made:"), usize::from(!same_login));
    }
}

#[test]
fn an_interrupted_completion_receipt_recovers_without_a_second_strike() {
    let (a, idle) = pending(true);
    let store = a.store();
    let wake = store.wake_path();
    assert!(!wake.exists());
    std::fs::create_dir(&wake).unwrap();
    let before = std::fs::read(store.ledger_path()).unwrap();

    account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
    assert!(idle.kept.lock().unwrap().account.flying.is_some());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(a.asked("made:"), 0);
    assert!(
        store.load(a.clock()).is_err(),
        "an admitted receipt fences readers"
    );
    let written = std::fs::read(store.ledger_path()).unwrap();
    assert_ne!(
        written, before,
        "state and ledger reached disk before the failed wake"
    );

    std::fs::rename(&wake, wake.with_extension("obstruction")).unwrap();
    account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
    assert!(idle.kept.lock().unwrap().account.moved.is_none());
    assert_eq!(
        std::fs::read(store.ledger_path()).unwrap(),
        written,
        "no second strike"
    );
    assert_eq!(a.state().sessions[&idle.sid].account, "main");
    assert_eq!(a.state().active, None);
    assert!(wake.is_file(), "the admitted wake completed");
}

fn landing_case(case: i64) -> [i64; 4] {
    let (a, mut idle) = pending(case == 2);
    let before = std::fs::read(a.store().ledger_path()).unwrap();
    let state = std::fs::read(a.store().state_file()).unwrap();
    let original = Arc::clone(&idle.hooks.acts.account_made);
    let seen = Arc::clone(&a);
    idle.hooks.acts.account_made = Arc::new(move |sid, target, at| {
        assert_eq!(
            seen.state().sessions[sid].account,
            target,
            "written before notification"
        );
        original(sid, target, at);
    });
    let gate = Arc::clone(&idle.kept.lock().unwrap().account.landing);
    let gate_held = (case == 7).then(|| gate.lock().unwrap());
    let held = matches!(case, 3 | 5).then(|| a.hold_lock());
    if matches!(case, 4 | 6 | 9) {
        std::fs::write(a.store().state_file(), "{\n").unwrap();
    }
    if matches!(case, 10 | 11) {
        let kept = Arc::clone(&idle.kept);
        a.at_seat_read(2, move |_| {
            let mut kept = kept.lock().unwrap();
            if case == 10 {
                kept.account.flying = None;
            } else {
                let flying = kept.account.flying.clone();
                kept.account = AccountKept::default();
                kept.account.flying = flying;
            }
        });
    }
    account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
    drop(held);
    if case == 6 {
        std::fs::write(a.store().state_file(), &state).unwrap();
    }
    if matches!(case, 5 | 6 | 8 | 9) {
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
    }
    drop(gate_held);
    let kept = idle.kept.lock().unwrap();
    [
        i64::from(kept.account.flying.is_some()),
        i64::from(kept.account.moved.is_some()),
        i64::from(std::fs::read(a.store().ledger_path()).unwrap() != before),
        i64::try_from(a.asked("made:")).unwrap(),
    ]
}

pub(crate) fn landing_transition(
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
            let [pending, moved, recorded, notified] = landing_case(state["case"]);
            next.insert("phase", 2);
            next.insert("pending", pending);
            next.insert("moved", moved);
            next.insert("recorded", recorded);
            next.insert("notified", notified);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn delayed_landing_conforms_through_writer_faults_and_cancellation() {
    let model = aterm_models_harness::account_landing_model();
    assert_eq!(model.mutant_dials(), (1..=12).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 33);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = landing_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (4, "moved", 1, "PublishedAfterCommit"),
        (7, "notified", 1, "NotifiedAfterCommit"),
        (3, "pending", 0, "RetryRetained"),
        (5, "pending", 1, "SuccessCompletes"),
        (2, "moved", 1, "KnownFailure"),
        (10, "recorded", 1, "CanceledIsQuiet"),
        (8, "notified", 2, "AtMostOnce"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = landing_transition(&state, "Observe");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}
