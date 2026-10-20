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
