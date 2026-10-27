// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn set_controls(switches: &Switches, mode: &str, logins: bool) {
    let mut cfg = on();
    cfg.set("accounts", mode).unwrap();
    cfg.set("logins", if logins { "renew" } else { "off" })
        .unwrap();
    switches.set(&cfg, false);
}

#[test]
fn rotation_controls_are_rechecked_at_the_writer() {
    let a = Accts::new();
    let sid = "s-controls-writer";
    let before = a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let state = a.state();
    let ledger = a.ledger();
    let switches = Arc::clone(&idle.switches);
    a.at_seat_read(2, move |_| {
        set_controls(&switches, "off", true);
    });
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.tabs.lock().unwrap().get(sid), Some(&before));
    assert_eq!(a.state(), state);
    assert_eq!(a.ledger(), ledger);
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
}

#[test]
fn rotation_controls_are_rechecked_before_restart() {
    let a = Accts::new();
    let sid = "s-controls-restart";
    let before = a.spawn(sid, ACCT_TALK_A, "main");
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let mut idle = a.worker(sid, &on());
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    let switches = Arc::clone(&idle.switches);
    let original = Arc::clone(&idle.hooks.acts.restart);
    let retry = Arc::clone(&original);
    idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
        set_controls(&switches, "off", true);
        original(sid, grace, why, selected)
    });
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("wait:account-changed")
    );
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.tabs.lock().unwrap().get(sid), Some(&before));
    assert!(idle.kept.lock().unwrap().account.flying.is_none());
    assert!(a.state().active.is_none());
    assert!(a.state().canary.is_none());
    idle.hooks.acts.restart = retry;
    set_controls(&idle.switches, "early", true);
    assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
    assert_eq!(
        idle.restart(&move_to("work-2", AccountAt::Wall)).as_deref(),
        Some("adopted")
    );
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn login_repair_obeys_its_own_control_with_rotation_off() {
    for cancel in [false, true] {
        let a = Accts::new();
        let sid = "s-controls-login";
        let before = a.spawn(sid, ACCT_TALK_A, "main");
        let renewed = renew_own(&a);
        let mut idle = a.worker(sid, &on());
        set_controls(&idle.switches, "off", true);
        assert_eq!(moves(&idle.account(AccountPoint::TurnEnd)), Some("main"));
        let switches = Arc::clone(&idle.switches);
        let original = Arc::clone(&idle.hooks.acts.restart);
        idle.hooks.acts.restart = Arc::new(move |sid, grace, why, selected| {
            if cancel {
                set_controls(&switches, "off", false);
            }
            original(sid, grace, why, selected)
        });
        let step = idle.restart(&move_to("main", AccountAt::Ahead));
        assert_eq!(
            step.as_deref(),
            Some(if cancel {
                "wait:account-changed"
            } else {
                "adopted"
            })
        );
        assert_eq!(a.asked("restart:"), usize::from(!cancel));
        let current = a.tabs.lock().unwrap().get(sid).cloned().unwrap();
        if cancel {
            assert_eq!(current, before);
        } else {
            assert_eq!(current.login.as_deref(), Some(renewed.as_str()));
        }
        assert!(idle.kept.lock().unwrap().account.flying.is_none());
    }
}

pub(crate) fn controls_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "ModeBefore" => {
            next.insert("before_mode", state["before_mode"] + 1);
        }
        "ModeAfter" => {
            next.insert("after_mode", state["after_mode"] + 1);
        }
        "LoginsBefore" => {
            next.insert("before_logins", 1);
        }
        "LoginsAfter" => {
            next.insert("after_logins", 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Observe" => {
            let read = |mode, logins| {
                let switches = Switches::default();
                let mode = [
                    accounts::Mode::Off,
                    accounts::Mode::Wall,
                    accounts::Mode::Early,
                ][usize::try_from(mode).unwrap()];
                set_controls(&switches, mode.as_str(), logins == 1);
                assert_eq!(
                    switches.account_controls(),
                    AccountControls {
                        mode,
                        logins: logins == 1
                    }
                );
                assert_eq!(switches.accounts(), mode);
                assert_eq!(switches.logins(), logins == 1);
                switches
            };
            let selected = read(state["before_mode"], state["before_logins"]).account_controls();
            let current = read(state["after_mode"], state["after_logins"]);
            next.insert("phase", 2);
            next.insert("admitted", i64::from(selected.is_current(&current)));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_controls_conform_for_every_published_pair() {
    let model = aterm_models_harness::account_controls_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3]);
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 108);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = controls_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&next));
            }
        }
    }
    for (field, value, admitted, law) in [
        ("after_mode", 1, 1, "CurrentMode"),
        ("after_logins", 1, 1, "CurrentLogins"),
        ("after_mode", 0, 0, "CurrentProgress"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert(field, value);
        let mut bad = controls_transition(&state, "Observe");
        bad.insert("admitted", admitted);
        assert!(!model.check_invariant(law, &bad));
    }
}
