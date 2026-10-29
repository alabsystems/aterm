// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn publish_state(state: &aterm_spec::interp::State) -> [i64; 3] {
    let mut seat = AccountSeat {
        roster: Some(PathBuf::from("response-roster")),
        state: PathBuf::from("response-state"),
        home: PathBuf::from("response-home"),
        now: ACCT_NOW,
        ladder: &[],
    };
    let observed = snap("s-response-model");
    let moved = Moved {
        id: MoveId::default(),
        origin: seat.context(),
        to: "work-2".into(),
        at: AccountAt::Wall,
        login: None,
        process: Snapshot {
            pid: observed.pid + u32::from(state["entry"] == 2),
            ..observed.clone()
        },
        answered: false,
        untold: false,
    };
    let kept = Mutex::new(Kept {
        snapshot: Some(observed.clone()),
        account: AccountKept {
            moved: (state["captured"] == 1).then(|| moved.clone()),
            ..AccountKept::default()
        },
        ..Kept::default()
    });
    let receipt = MoveObservation::capture(&kept);
    {
        let mut k = kept.lock().unwrap();
        if state["generation"] == 2 {
            k.account.landing = Arc::default();
        }
        k.account.moved = (state["present"] == 1).then_some(moved);
        if let Some(moved) = k.account.moved.as_mut() {
            if state["receipt"] == 2 {
                moved.id = MoveId::default();
            }
            moved.untold = state["notice"] == 1;
        }
        let current = k.snapshot.as_mut().unwrap();
        match state["process"] {
            2 => current.pid += 1,
            3 => current.start.push_str(" changed"),
            4 => current.login = Some("another-login".into()),
            _ => {}
        }
        if state["enriched"] == 1 {
            current.version = Some("newly-observed-version".into());
            current.session = Some("newly-observed-session".into());
        }
        if state["pending"] == 1 {
            k.account.flying = Some(Flying {
                origin: seat.context(),
                to: "pool-a".into(),
                at: AccountAt::Wall,
                login: observed.login.clone(),
                pid: observed.pid,
                start: observed.start.clone(),
                rejected: None,
            });
        }
    }
    if state["origin"] == 2 {
        seat.state = PathBuf::from("another-state");
    }
    let published = state["durable"] == 1
        && receipt.is_some_and(|receipt| {
            receipt.publish(
                &kept,
                &seat,
                &observed,
                if state["reply"] == 1 {
                    MoveReply::Served
                } else {
                    MoveReply::Failed
                },
            )
        });
    let k = kept.lock().unwrap();
    [
        i64::from(published),
        i64::from(k.account.moved.is_some()),
        i64::from(k.account.moved.as_ref().is_some_and(|m| m.answered)),
    ]
}

pub(crate) fn response_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Absent" => {
            for key in ["captured", "present", "remaining"] {
                next.insert(key, 0);
            }
        }
        "StaleEntry" => {
            next.insert("entry", 2);
        }
        "Serve" | "Fail" => {
            next.insert("phase", 1);
            next.insert("reply", if action == "Serve" { 1 } else { 2 });
        }
        "NewGeneration" => {
            next.insert("generation", 2);
        }
        "NewReceipt" => {
            next.insert("receipt", 2);
            next.insert("present", 1);
            next.insert("remaining", 1);
        }
        "NewProcess" => {
            next.insert("process", 2);
        }
        "OtherOrigin" => {
            next.insert("origin", 2);
        }
        "StartMove" => {
            next.insert("pending", 1);
        }
        "Notice" => {
            next.insert("notice", 1);
        }
        "Enrich" => {
            next.insert("enriched", 1);
        }
        "Commit" | "Refuse" => {
            next.insert("phase", 2);
            next.insert("durable", i64::from(action == "Commit"));
        }
        "Publish" => {
            next.insert("phase", 3);
            for (key, value) in ["published", "remaining", "answered"]
                .into_iter()
                .zip(publish_state(state))
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_response_publication_conforms_through_replacement_interleavings() {
    let model = aterm_models_harness::account_response_model();
    assert_eq!(model.mutant_dials(), (1..=12).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1000);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = response_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (field, value, law) in [
        ("generation", 2, "SameGeneration"),
        ("receipt", 2, "SameReceipt"),
        ("process", 2, "SameProcess"),
        ("origin", 2, "SameOrigin"),
        ("pending", 1, "NoPendingMove"),
        ("captured", 0, "CapturedMove"),
        ("durable", 0, "DurableFirst"),
        ("entry", 2, "EntryMatches"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 2);
        state.insert("reply", 1);
        state.insert("durable", 1);
        state.insert(field, value);
        let mut bad = response_transition(&state, "Publish");
        assert_eq!(bad["published"], 0, "{field}");
        bad.insert("published", 1);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    for reply in [1, 2] {
        let mut state = model.init_state();
        state.insert("phase", 2);
        state.insert("durable", 1);
        state.insert("reply", reply);
        let good = response_transition(&state, "Publish");
        for (key, wrong, law) in if reply == 1 {
            [
                ("remaining", 1, "ServedConsumes"),
                ("published", 0, "CurrentCompletes"),
            ]
        } else {
            [
                ("answered", 0, "FailedAnswers"),
                ("published", 0, "CurrentCompletes"),
            ]
        } {
            let mut bad = good.clone();
            bad.insert(key, wrong);
            assert!(!model.check_invariant(law, &bad));
        }
        for process in [3, 4] {
            state.insert("process", process);
            assert_eq!(publish_state(&state), [0, 1, 0]);
        }
        state.insert("process", 1);
        state.insert("durable", 0);
        let mut bad = response_transition(&state, "Publish");
        bad.insert("remaining", 0);
        assert!(!model.check_invariant("NoCollateral", &bad));
    }
}

#[test]
fn an_old_served_response_cannot_consume_a_new_generations_move() {
    let (a, idle) = moved_at_wall("s-response-served");
    let before = idle.kept.lock().unwrap().account.moved.clone();
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |_| {
        let mut kept = kept.lock().unwrap();
        let moved = kept.account.moved.clone();
        kept.account = AccountKept::default();
        kept.account.moved = moved;
    });
    idle.account_served();
    assert!(a.state().proven, "the observed account did serve");
    assert_eq!(idle.kept.lock().unwrap().account.moved, before);
}

#[test]
fn an_old_failure_cannot_answer_the_move_created_during_upkeep() {
    let (a, mut idle) = moved_at_wall("s-response-failure");
    let kept = Arc::clone(&idle.kept);
    idle.hooks.acts.accounts_upkeep = AccountsUpkeep::Made(Arc::new(move |_| {
        let mut kept = kept.lock().unwrap();
        let snap = kept.snapshot.as_mut().unwrap();
        snap.pid += 1;
        let pid = snap.pid;
        kept.account.moved.as_mut().unwrap().process.pid = pid;
    }));
    idle.account_answered(AccountBad::SignedOut);
    assert_eq!(a.state().account("work-2").auth, accounts::Auth::SignedOut);
    assert!(
        !idle
            .kept
            .lock()
            .unwrap()
            .account
            .moved
            .as_ref()
            .unwrap()
            .answered
    );
}

#[test]
fn responses_accept_notice_and_snapshot_enrichment_but_not_a_new_move_id() {
    for served in [false, true] {
        for replace in [false, true] {
            let (a, idle) = moved_at_wall("s-response-identity");
            let kept = Arc::clone(&idle.kept);
            a.at_seat_read(2, move |_| {
                let mut k = kept.lock().unwrap();
                let moved = k.account.moved.as_mut().unwrap();
                if replace {
                    moved.id = MoveId::default();
                } else {
                    moved.untold = true;
                }
                k.snapshot.as_mut().unwrap().version = Some("newly-known".into());
            });
            if served {
                idle.account_served();
            } else {
                idle.account_answered(AccountBad::SignedOut);
            }
            let k = idle.kept.lock().unwrap();
            if replace {
                assert!(k.account.moved.as_ref().is_some_and(|m| !m.answered));
            } else if served {
                assert!(k.account.moved.is_none());
            } else {
                assert!(
                    k.account
                        .moved
                        .as_ref()
                        .is_some_and(|m| m.answered && m.untold)
                );
            }
        }
    }
}

#[test]
fn refused_response_writes_retain_the_unanswered_move() {
    for served in [false, true] {
        let (a, idle) = moved_at_wall("s-response-write");
        let before = idle.kept.lock().unwrap().account.moved.clone();
        let held = a.hold_lock();
        if served {
            idle.account_served();
        } else {
            idle.account_answered(AccountBad::SignedOut);
        }
        assert_eq!(idle.kept.lock().unwrap().account.moved, before);
        drop(held);
        assert!(!a.state().proven);
        assert_ne!(a.state().account("work-2").auth, accounts::Auth::SignedOut);
    }
}
