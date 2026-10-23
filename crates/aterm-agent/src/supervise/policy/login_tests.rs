// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::time::Instant;

use aterm_phase::prompt::fixtures::{
    LOGIN_CONSOLE_MENU, LOGIN_ERROR, LOGIN_HANDOFF, LOGIN_OPENING, LOGIN_PICKER,
    LOGIN_PICKER_CONSOLE, LOGIN_PICKER_WARNED, LOGIN_SUCCESS, screen,
};
use aterm_phase::{LoginScreen, PromptV2};

use super::*;
use crate::supervise::config::Approve;
use crate::supervise::policy::approval::{ApprovalCtx, decide};
use crate::supervise::policy::turn_end::TurnEndReading;
use aterm_phase::wall::WallKind;

fn picker_of(text: &str) -> (Vec<String>, PromptV2, LoginPicker) {
    let rows = screen(text);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    let prompt = read.prompt.clone().expect("the picker is a prompt");
    let Some(LoginScreen::Picker(picker)) = aterm_phase::login_screen(&rows) else {
        panic!("the picker reads as the picker");
    };
    (rows, prompt, picker)
}

fn focus_steps(d: &Decision) -> Option<i32> {
    match d {
        Decision::Approve {
            rule_id: RULE_LOGIN_METHOD,
            choice: Choice::Focus { steps, label },
            ..
        } => {
            assert!(label.contains(LoginMethod::Subscription.label()), "{label}");
            Some(*steps)
        }
        _ => None,
    }
}

fn ctx(approve: Approve) -> ApprovalCtx {
    ApprovalCtx {
        approve,
        ..ApprovalCtx::new(
            PathBuf::from("/private/tmp/claude-502/scratch/work1"),
            Some(PathBuf::from("/Users/_owner")),
            502,
            Some(PathBuf::from("/var/folders/ab/xyz/T")),
            std::sync::Arc::new(crate::supervise::policy::LiveProbe),
        )
    }
}

/// The subscription's row, where the dialog opens the focus (its first row);
/// a focus a person's key moved off it — to Console, say, to bill a usage
/// limit's work to the API — is their choice, never moved back (the review's
/// F2).
#[test]
fn a_subscription_session_chooses_the_subscription_row_where_the_dialog_opens_it() {
    for text in [LOGIN_PICKER, LOGIN_PICKER_WARNED] {
        let (_, prompt, picker) = picker_of(text);
        let d = pick_method(&prompt, &picker, Some(LoginMethod::Subscription));
        assert_eq!(focus_steps(&d), Some(0), "{d:?}");
    }
    let (_, prompt, picker) = picker_of(LOGIN_PICKER_CONSOLE);
    let d = pick_method(&prompt, &picker, Some(LoginMethod::Subscription));
    assert!(matches!(d, Decision::Escalate { .. }), "{d:?}");
}

#[test]
fn an_unknown_console_or_platform_method_is_the_owners() {
    let (_, prompt, picker) = picker_of(LOGIN_PICKER);
    for method in [
        None,
        Some(LoginMethod::Console),
        Some(LoginMethod::Platform),
    ] {
        let d = pick_method(&prompt, &picker, method);
        assert!(matches!(d, Decision::Escalate { .. }), "{method:?}: {d:?}");
    }
}

/// The picker of the `/login` the loop typed into a lost login is part of
/// that act, answered at every power; any other picker — a person's own
/// `/login`, on a working session or not — is theirs at every power (the
/// review's F2, voluntary-login-false-alarm); Console's menu never.
#[test]
fn the_approval_policy_answers_only_the_picker_of_its_own_login() {
    let now = Instant::now();
    let opening_rows = screen(LOGIN_OPENING);
    assert!(dialog_stands(&opening_rows));
    let opening = TurnEndReading {
        phase: crate::supervise::phase::Phase::Busy,
        ..TurnEndReading::of(
            &aterm_phase::read(Some("claude"), &opening_rows, Some(2)),
            &opening_rows,
            false,
            None,
            None,
            None,
        )
    };
    let later_rows = screen(LOGIN_PICKER_WARNED);
    let later_read = aterm_phase::read(Some("claude"), &later_rows, Some(2));
    let at_wall = aterm_phase::login_at_wall(&later_rows).expect("the picker at a lost login");
    for approve in [Approve::None, Approve::Safe, Approve::All] {
        let c = ApprovalCtx {
            login_method: Some(LoginMethod::Subscription),
            login_carry: true,
            ..ctx(approve)
        };
        let rows = screen(LOGIN_PICKER);
        let read = aterm_phase::read(Some("claude"), &rows, Some(2));
        let d = decide(&read, &rows, &c);
        assert_eq!(focus_steps(&d), Some(0), "{approve:?}: {d:?}");

        let unknown = ApprovalCtx {
            login_method: None,
            ..c.clone()
        };
        let d = decide(&read, &rows, &unknown);
        assert!(matches!(d, Decision::Escalate { .. }), "{approve:?}: {d:?}");

        let theirs = ApprovalCtx {
            login_carry: false,
            ..c.clone()
        };
        let d = decide(&read, &rows, &theirs);
        assert!(matches!(d, Decision::Escalate { .. }), "{approve:?}: {d:?}");

        let rows = screen(LOGIN_CONSOLE_MENU);
        let read = aterm_phase::read(Some("claude"), &rows, Some(2));
        let d = decide(&read, &rows, &c);
        assert!(matches!(d, Decision::Escalate { .. }), "{approve:?}: {d:?}");

        for work_started in [false, true] {
            let mut st = TurnEndState::default();
            st.seed_login(std::time::Duration::ZERO, now);
            st.observe(&opening, now);
            if work_started {
                st.took();
            }
            assert!(st.take_up_login(Some(&at_wall), false, now));
            let later = ApprovalCtx {
                login_carry: st.login_typed_at().is_some(),
                ..c.clone()
            };
            let d = decide(&later_read, &later_rows, &later);
            if work_started {
                assert!(matches!(d, Decision::Escalate { .. }), "{approve:?}: {d:?}");
            } else {
                assert_eq!(
                    focus_steps(&d),
                    Some(0),
                    "the browser wait: {approve:?}: {d:?}"
                );
            }
        }
    }
}

/// THE PICKER OF THE LOOP'S OWN `/login` ONCE THE LOGIN CAME BACK IN ANOTHER
/// TAB (the review's harness-login-misled-dialog): nothing is left to sign
/// in, so the gate gives it to nobody ([`PickerGate::Back`]) and the approval
/// policy escalates it with the step that closes it, at every power and with
/// the method known. NEGATIVE CONTROLS: the login carried there or nowhere,
/// the same picker is aterm's (answered with the method); a person's own
/// picker stays theirs whatever the host says.
#[test]
fn the_picker_of_a_login_back_elsewhere_is_answered_by_nobody() {
    let back = LoginShared::Back {
        by: "@2".to_string(),
    };
    let carried = LoginShared::Carried {
        by: "@2".to_string(),
    };
    assert_eq!(picker_gate(true, Some(&back), false), PickerGate::Back);
    assert_eq!(
        picker_gate(true, Some(&carried), false),
        PickerGate::Carried
    );
    assert_eq!(picker_gate(true, None, false), PickerGate::Carried);
    assert_eq!(picker_gate(false, Some(&back), false), PickerGate::Persons);
    assert_eq!(picker_gate(false, None, false), PickerGate::Persons);
    let rows = screen(LOGIN_PICKER);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    for approve in [Approve::None, Approve::Safe, Approve::All] {
        let at_back = ApprovalCtx {
            login_method: Some(LoginMethod::Subscription),
            login_back: true,
            ..ctx(approve)
        };
        let d = decide(&read, &rows, &at_back);
        assert!(
            matches!(&d, Decision::Escalate { reason, .. } if reason.contains("Esc")),
            "{approve:?}: {d:?}"
        );
        let ours = ApprovalCtx {
            login_carry: true,
            login_back: false,
            ..at_back.clone()
        };
        let d = decide(&read, &rows, &ours);
        assert_eq!(focus_steps(&d), Some(0), "{approve:?}: {d:?}");
    }
}

/// THE PICKER OF THE LOOP'S OWN `/login` ONCE A RENEWAL THE TAB FOLLOWS
/// REPLACED ITS LOGIN (the review of the login carry, acct3-3): nothing is
/// left to sign in — a method chosen opens a browser sign-in that brings back
/// the replaced login — so the gate gives it to nobody
/// ([`PickerGate::Renewed`]) and the approval policy escalates it with the
/// step that closes it, at every power and with the method known. The
/// renewal is followed only with the logins' lifecycle on and a relaunch in
/// reach ([`renewal_followed`]). NEGATIVE CONTROLS: no renewal, or one out of
/// reach (`relaunch` or `resume_limits` off, no host that relaunches, the
/// lifecycle off), and the same picker is aterm's; a person's own picker
/// stays theirs; a login back in another tab is the back gate's.
#[test]
fn the_picker_over_a_renewed_login_is_answered_by_nobody() {
    let cfg = SupervisorConfig::default();
    assert!(renewal_followed(true, &cfg, true));
    assert!(!renewal_followed(false, &cfg, true), "no renewal");
    assert!(!renewal_followed(true, &cfg, false), "no relaunch here");
    for off in ["relaunch", "resume_limits", "logins"] {
        let mut c = cfg.clone();
        match off {
            "relaunch" => c.relaunch = false,
            "resume_limits" => c.resume_limits = false,
            _ => c.logins = false,
        }
        assert!(!renewal_followed(true, &c, true), "{off} off");
    }
    let back = LoginShared::Back {
        by: "@2".to_string(),
    };
    assert_eq!(picker_gate(true, None, true), PickerGate::Renewed);
    assert_eq!(picker_gate(true, None, false), PickerGate::Carried);
    assert_eq!(picker_gate(false, None, true), PickerGate::Persons);
    assert_eq!(picker_gate(true, Some(&back), true), PickerGate::Back);
    let rows = screen(LOGIN_PICKER);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    for approve in [Approve::None, Approve::Safe, Approve::All] {
        let renewed = ApprovalCtx {
            login_method: Some(LoginMethod::Subscription),
            login_renewed: true,
            ..ctx(approve)
        };
        let d = decide(&read, &rows, &renewed);
        assert!(
            matches!(&d, Decision::Escalate { reason, .. } if reason.contains("Esc")),
            "{approve:?}: {d:?}"
        );
        let ours = ApprovalCtx {
            login_carry: true,
            login_renewed: false,
            ..renewed.clone()
        };
        let d = decide(&read, &rows, &ours);
        assert_eq!(focus_steps(&d), Some(0), "{approve:?}: {d:?}");
    }
}

fn dialog(text: &str) -> LoginScreen {
    aterm_phase::login_screen(&screen(text)).expect("a login dialog")
}

/// A state that read the lost login: the dialog over it is the loss's.
fn walled() -> TurnEndState {
    let rows = screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    let mut st = TurnEndState::default();
    st.observe(
        &TurnEndReading::of(&read, &rows, false, None, None, None),
        Instant::now(),
    );
    assert!(st.wall_attempts(WallKind::Auth).is_some(), "the lost login");
    st
}

/// Over a lost login, only the success gets a key (its Enter); every other
/// state nothing — the owner is told by the login's notice, never by an
/// escalation beside it (the review's double-telling). A `/login` a person
/// opened on a working session (no lost login) gets nothing at all.
#[test]
fn each_dialog_state_gets_its_act() {
    let fresh = TurnEndState::default();
    let lost = walled();
    for text in [
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_PICKER,
        LOGIN_CONSOLE_MENU,
        LOGIN_ERROR,
        LOGIN_SUCCESS,
    ] {
        assert_eq!(dialog_act(&fresh, &dialog(text)), TurnEndAction::Nothing);
    }
    for text in [
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_PICKER,
        LOGIN_CONSOLE_MENU,
        LOGIN_ERROR,
    ] {
        assert_eq!(dialog_act(&lost, &dialog(text)), TurnEndAction::Nothing);
    }
    let success = dialog(LOGIN_SUCCESS);
    let enter = dialog_act(&lost, &success);
    assert!(
        matches!(
            &enter,
            TurnEndAction::Confirm {
                rule_id: RULE_LOGIN_DONE,
                ..
            }
        ),
        "{enter:?}"
    );
    let mut limit_st = TurnEndState::default();
    let expired = screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
    let mut weekly = TurnEndReading::of(
        &aterm_phase::read(Some("claude"), &expired, Some(2)),
        &expired,
        false,
        None,
        None,
        None,
    );
    weekly.wall = Some(WallKind::UsageWeekly);
    weekly.wall_message = "You've hit your weekly limit · resets Oct 23 at 8am".to_string();
    limit_st.observe(&weekly, Instant::now());
    assert!(
        matches!(
            dialog_act(&limit_st, &success),
            TurnEndAction::Confirm {
                rule_id: RULE_LOGIN_DONE,
                ..
            }
        ),
        "usage limit gets its success Enter"
    );
}

#[test]
fn the_success_enter_goes_once() {
    let rows = screen(LOGIN_SUCCESS);
    let read = aterm_phase::read(Some("claude"), &rows, Some(2));
    let reading = TurnEndReading::of(&read, &rows, false, None, None, None);
    let mut st = walled();
    let now = Instant::now();
    st.observe(&reading, now);
    let success = dialog(LOGIN_SUCCESS);
    let enter = dialog_act(&st, &success);
    assert!(matches!(enter, TurnEndAction::Confirm { .. }), "{enter:?}");
    st.acted(&enter, &reading, now);
    assert!(st.login_confirmed());
    st.observe(&reading, now);
    assert_eq!(dialog_act(&st, &success), TurnEndAction::Nothing);
    let gone = screen(LOGIN_HANDOFF);
    let read = aterm_phase::read(Some("claude"), &gone, Some(2));
    st.observe(
        &TurnEndReading::of(&read, &gone, false, None, None, None),
        now,
    );
    assert!(
        !st.login_confirmed(),
        "another screen ends the confirmed one"
    );
}

/// The dialog short of its success stands (a busy read of it is no agent
/// working); its success, a wall under a composer and an ordinary busy turn
/// do not.
#[test]
fn the_dialog_short_of_its_success_stands() {
    for text in [
        LOGIN_PICKER,
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_CONSOLE_MENU,
        LOGIN_ERROR,
    ] {
        assert!(
            dialog_stands(&screen(text)),
            "{}",
            &text[..text.find('\n').unwrap_or(0)]
        );
    }
    for text in [
        LOGIN_SUCCESS,
        aterm_phase::prompt::fixtures::LOGIN_EXPIRED,
        aterm_phase::prompt::fixtures::LOGIN_INTERRUPTED,
    ] {
        assert!(!dialog_stands(&screen(text)));
    }
}
