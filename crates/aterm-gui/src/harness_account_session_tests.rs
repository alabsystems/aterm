// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn write_session(state: &aterm_spec::interp::State) -> [i64; 4] {
    let observed = snap("s-session-model");
    let generation: Arc<Mutex<()>> = Arc::default();
    let look = AccountLook {
        who: accounts::ProcessLogin::of_env([]),
        label: Some("work-2".into()),
        snap: observed.clone(),
        generation: Arc::clone(&generation),
    };
    let mut kept = Kept::default();
    kept.account.landing = if state["generation"] == 1 {
        generation
    } else {
        Arc::default()
    };
    kept.snapshot = (state["present"] == 1).then(|| {
        let mut current = observed;
        match state["process"] {
            2 => current.pid += 1,
            3 => current.start.push_str(" changed"),
            _ => {}
        }
        if state["login"] == 2 {
            current.login = Some("other-login".into());
        }
        if state["metadata"] == 1 {
            current.version = Some("newly-observed".into());
            current.session = Some("newly-observed-session".into());
        }
        current
    });
    let mut account = accounts::State::default();
    accounts::note_session(
        &mut account,
        "s-session-model",
        "pool-a",
        Some("newer"),
        ACCT_NOW,
    );
    let before = account.clone();
    let events = look.note_session(
        &Mutex::new(kept),
        &mut account,
        "s-session-model",
        "work-2",
        None,
        ACCT_NOW,
    );
    [
        i64::from(events.is_some()),
        if account.sessions["s-session-model"].account == "work-2" {
            1
        } else {
            2
        },
        i64::from(account != before),
        i64::try_from(events.map_or(0, |events| events.len())).unwrap(),
    ]
}

pub(crate) fn session_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Capture" => {
            next.insert("phase", 1);
        }
        "NewGeneration" => {
            next.insert("generation", 2);
        }
        "NewProcess" => {
            next.insert("process", 2);
        }
        "NewLogin" => {
            next.insert("login", 2);
        }
        "Gone" => {
            next.insert("present", 0);
        }
        "Enrich" => {
            next.insert("metadata", 1);
        }
        "Write" => {
            next.insert("phase", 2);
            for (key, value) in ["applied", "row", "changed", "events"]
                .into_iter()
                .zip(write_session(state))
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn session_attribution_conforms_at_the_local_writer_boundary() {
    let model = aterm_models_harness::account_session_attribution_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 50);
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
    state.insert("process", 3);
    assert_eq!(
        write_session(&state),
        [0, 2, 0, 0],
        "a new start with the same pid"
    );
}

fn replace_session(a: &Accts, kept: &Mutex<Kept>, sid: &str) {
    let next = a.spawn(sid, ACCT_TALK_B, "pool-a");
    kept.lock().unwrap().snapshot = Some(next);
    edited(a, |state| {
        accounts::note_session(state, sid, "pool-a", Some("newer"), a.clock());
    });
}

#[test]
fn a_delayed_success_does_not_reattribute_the_replacement_session() {
    let sid = "s-session-success";
    let (a, idle) = moved_at_wall(sid);
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |a| replace_session(a, &kept, sid));
    idle.account_served();
    let state = a.state();
    assert!(state.proven, "the observed account did serve a response");
    assert_eq!(state.sessions[sid].account, "pool-a");
    assert_eq!(state.sessions[sid].held.as_deref(), Some("newer"));
}

#[test]
fn a_delayed_periodic_note_does_not_reattribute_the_replacement_session() {
    let sid = "s-session-periodic";
    let (a, idle) = moved_at_wall(sid);
    a.now
        .store(ACCT_NOW + ACCOUNT_ROW_STALE_S + 1, Ordering::SeqCst);
    let seat = a.seat();
    let roster = a.roster();
    let look = idle.account_look(&roster).unwrap();
    let before = a.state();
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(1, move |a| replace_session(a, &kept, sid));
    idle.account_note(&seat, &roster, &look, &before);
    let state = a.state();
    assert_eq!(state.sessions[sid].account, "pool-a");
    assert_eq!(state.sessions[sid].held.as_deref(), Some("newer"));
}

#[test]
fn old_session_notes_respect_retirement_and_generation_replacement() {
    for served in [false, true] {
        for retired in [false, true] {
            let sid = "s-session-retired";
            let (a, idle) = moved_at_wall(sid);
            a.now
                .store(ACCT_NOW + ACCOUNT_ROW_STALE_S + 1, Ordering::SeqCst);
            let seat = a.seat();
            let roster = a.roster();
            let look = idle.account_look(&roster).unwrap();
            let before = a.state();
            let kept = Arc::clone(&idle.kept);
            a.at_seat_read(if served { 2 } else { 1 }, move |a| {
                {
                    let mut k = kept.lock().unwrap();
                    k.account = AccountKept::default();
                    if retired {
                        k.snapshot = None;
                    }
                }
                edited(a, |state| {
                    if retired {
                        accounts::forget_session(state, sid);
                    } else {
                        accounts::note_session(state, sid, "pool-a", Some("newer"), a.clock());
                    }
                });
            });
            if served {
                idle.account_served();
            } else {
                idle.account_note(&seat, &roster, &look, &before);
            }
            let state = a.state();
            if retired {
                assert!(!state.sessions.contains_key(sid));
            } else {
                assert_eq!(state.sessions[sid].account, "pool-a");
            }
        }
    }
}

#[test]
fn benign_metadata_updates_still_allow_session_refresh() {
    for served in [false, true] {
        let sid = "s-session-enriched";
        let (a, idle) = moved_at_wall(sid);
        a.now
            .store(ACCT_NOW + ACCOUNT_ROW_STALE_S + 1, Ordering::SeqCst);
        let seat = a.seat();
        let roster = a.roster();
        let look = idle.account_look(&roster).unwrap();
        let before = a.state();
        let kept = Arc::clone(&idle.kept);
        a.at_seat_read(if served { 2 } else { 1 }, move |a| {
            let mut k = kept.lock().unwrap();
            let snap = k.snapshot.as_mut().unwrap();
            snap.version = Some("newly-observed".into());
            snap.session = Some("newly-observed-session".into());
            drop(k);
            edited(a, |state| {
                accounts::note_session(state, sid, "pool-a", Some("older"), a.clock());
            });
        });
        if served {
            idle.account_served();
        } else {
            idle.account_note(&seat, &roster, &look, &before);
        }
        let state = a.state();
        assert_eq!(state.sessions[sid].account, "work-2");
        assert_eq!(state.sessions[sid].held, None);
    }
}

#[test]
fn a_rotation_does_not_commit_from_a_replaced_source_process() {
    let sid = "s-session-source";
    let a = Accts::new();
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let kept = Arc::clone(&idle.kept);
    a.at_seat_read(2, move |a| replace_session(a, &kept, sid));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(a.asked("restart:"), 0);
    let state = a.state();
    assert!(state.rotations.is_empty());
    assert!(state.active.is_none());
    assert_eq!(state.sessions[sid].account, "pool-a");
}
