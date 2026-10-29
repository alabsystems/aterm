// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_target_profile_changed_after_advice_cannot_start_a_claude_rotation() {
    let a = Accts::new();
    let sid = "s-target-identity";
    change_identity(&a, "work-2");
    let profile = PathBuf::from(a.login_dir("work-2")).join(".claude.json");
    std::fs::write(
        &profile,
        br#"{"oauthAccount":{"emailAddress":"replacement@example.test"}}"#,
    )
    .unwrap();
    a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let wrong = br#"{"oauthAccount":{"emailAddress":"another@example.test"}}"#;
    std::fs::write(&profile, wrong).unwrap();
    let before = std::fs::read(a.store().state_file()).unwrap();
    let ledger = a.ledger();
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(std::fs::read(a.store().state_file()).unwrap(), before);
    assert_eq!(a.ledger(), ledger);
    assert_eq!(std::fs::read(profile).unwrap(), wrong);
}

fn change_identity(a: &Accts, label: &str) {
    let text = std::fs::read_to_string(a.roster_path()).unwrap();
    let before = format!("label = \"{label}\"");
    let after = format!("{before}\nemail = \"replacement@example.test\"");
    let changed = text.replace(&before, &after);
    assert_ne!(text, changed);
    std::fs::write(a.roster_path(), changed).unwrap();
}

fn policy_case(case: i64) -> [i64; 5] {
    let initiation = case >= 10;
    let (a, mut idle) = if initiation {
        let a = Accts::new();
        let sid = "s-policy-admission";
        a.spawn(sid, ACCT_TALK_A, "main");
        a.wall(ACCT_TALK_A, ACCT_NOW - 10);
        let idle = a.worker(sid, &on());
        assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
        (a, idle)
    } else {
        account_landing::pending(false)
    };
    let before = std::fs::read(a.store().state_file()).ok();
    let ledger = a.ledger();
    let restarted = a.asked("restart:");
    let original = Arc::clone(&idle.hooks.acts.account_seat);
    match case {
        2 | 9 | 11 => a.at_seat_read(2, |a| change_identity(a, "work-2")),
        3 | 12 => a.at_seat_read(2, |a| change_identity(a, "pool-a")),
        4 => a.at_seat_read(2, |a| {
            let mut logins = a.store().load_logins(a.clock()).unwrap().0;
            accounts::note_renewed(
                &mut accounts::State::default(),
                &mut logins,
                "work-2",
                Some(&a.login_dir("work-2")),
                &a.login_dir("renewed"),
                None,
                a.clock(),
            );
            a.store().save_logins(&logins).unwrap();
        }),
        5..=8 => {
            let read = AtomicUsize::new(0);
            let original = Arc::clone(&original);
            idle.hooks.acts.account_seat = Arc::new(move || {
                let mut seat = original()?;
                if read.fetch_add(1, Ordering::SeqCst) == 1 {
                    match case {
                        5 => seat.state = seat.state.join("another-store"),
                        6 => seat.roster = Some(seat.state.join("another-roster")),
                        7 => seat.home = seat.home.join("another-home"),
                        8 => return None,
                        _ => unreachable!(),
                    }
                }
                Some(seat)
            });
        }
        _ => {}
    }
    if initiation {
        let step = idle.restart(&move_to("work-2", AccountAt::Wall));
        assert_eq!(
            step.as_deref(),
            Some(if case == 10 {
                "adopted"
            } else {
                "wait:account-state"
            })
        );
    } else {
        let result = account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried);
        if case == 1 {
            assert_eq!(result, Applied::Yes);
        } else {
            assert_eq!(result, Applied::Unread);
            assert_eq!(std::fs::read(a.store().state_file()).ok(), before);
            assert_eq!(a.ledger(), ledger);
            assert_eq!(a.asked("made:"), 0);
            assert!(idle.kept.lock().unwrap().account.flying.is_some());
        }
        if case == 9 {
            a.write_roster();
            assert_eq!(
                account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
                Applied::Yes
            );
        }
    }
    let kept = idle.kept.lock().unwrap();
    [
        i64::from(kept.account.flying.is_some()),
        i64::from(kept.account.moved.is_some()),
        i64::from(std::fs::read(a.store().state_file()).ok() != before),
        i64::try_from(a.asked("restart:") - restarted).unwrap(),
        i64::try_from(a.asked("made:")).unwrap(),
    ]
}

pub(crate) fn policy_transition(
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
            let [pending, moved, written, restarted, notified] = policy_case(state["case"]);
            next.insert("phase", 2);
            next.insert("pending", pending);
            next.insert("moved", moved);
            next.insert("written", written);
            next.insert("restarted", restarted);
            next.insert("notified", notified);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_policy_admission_conforms_through_concurrent_changes() {
    let model = aterm_models_harness::account_policy_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 12);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = policy_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, field, value, law) in [
        (2, "written", 1, "ChangedPolicyIsQuiet"),
        (5, "moved", 1, "ChangedContextIsQuiet"),
        (2, "pending", 0, "PendingSurvives"),
        (11, "restarted", 1, "AdmissionBeforeRestart"),
        (1, "moved", 0, "CurrentProgress"),
        (9, "pending", 1, "CurrentProgress"),
        (2, "notified", 1, "NotificationAfterWrite"),
        (10, "restarted", 0, "CurrentInitiates"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = policy_transition(&state, "Observe");
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn periodic_session_attribution_rechecks_account_metadata() {
    for changed in [false, true] {
        let a = Accts::new();
        let sid = "s-note-policy";
        a.spawn(sid, ACCT_TALK_A, "main");
        let idle = a.worker(sid, &on());
        let (seat, roster, look) = idle.account_ctx().unwrap();
        let state = a.state();
        let before = std::fs::read(a.store().state_file()).ok();
        if changed {
            a.at_seat_read(1, |a| change_identity(a, "main"));
        }
        idle.account_note(&seat, &roster, &look, &state);
        assert_eq!(a.state().sessions.contains_key(sid), !changed);
        if changed {
            assert_eq!(std::fs::read(a.store().state_file()).ok(), before);
            assert!(a.ledger().is_empty());
        }
    }
}
