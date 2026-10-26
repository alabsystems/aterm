// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;
use std::time::Instant;

use aterm_phase::prompt::fixtures::{
    LOGIN_CONSOLE_MENU, LOGIN_ERROR, LOGIN_HANDOFF, LOGIN_OPENING, LOGIN_PICKER,
    LOGIN_PICKER_2_1_310, LOGIN_PICKER_AT_EXPIRED_2_1_310, LOGIN_PICKER_CONSOLE,
    LOGIN_PICKER_CONSOLE_2_1_310, LOGIN_PICKER_FULLSCREEN_AGAIN_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310, LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_NO_LOGIN_2_1_310, LOGIN_PICKER_PLATFORM_2_1_310, LOGIN_PICKER_WARNED,
    LOGIN_PICKER_WARNED_2_1_310, LOGIN_SUCCESS, screen,
};
use aterm_phase::{LoginScreen, PromptV2};

use super::*;
use crate::supervise::config::Approve;
use crate::supervise::policy::approval::{ApprovalCtx, decide};
use crate::supervise::policy::turn_end::TurnEndReading;
use aterm_phase::wall::WallKind;

/// Every measured picker as it opens, its focus on the subscription's row: 2.1.303's, then
/// 2.1.310's, the fullscreen renderer's bottom panel last, as it first opens (Claude Code's
/// `/effort` hint in its rule), once that hint is gone, and typed again over a left `/login`
/// (aterm_phase's `login_tests` holds both builds' readings).
const OPENED: [&str; 9] = [
    LOGIN_PICKER,
    LOGIN_PICKER_WARNED,
    LOGIN_PICKER_2_1_310,
    LOGIN_PICKER_WARNED_2_1_310,
    LOGIN_PICKER_NO_LOGIN_2_1_310,
    LOGIN_PICKER_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AGAIN_AT_EXPIRED_2_1_310,
];

/// The pickers of [`OPENED`] drawn in the fullscreen renderer's bottom panel, past whose method
/// pick nothing has been measured: no method's Enter is pressed there ([`picks`]).
const FULLSCREEN: [&str; 3] = [
    LOGIN_PICKER_FULLSCREEN_HINT_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AT_EXPIRED_2_1_310,
    LOGIN_PICKER_FULLSCREEN_AGAIN_AT_EXPIRED_2_1_310,
];

/// Every measured picker whose focus arrow keys moved off the first row (no method picked).
const MOVED: [&str; 3] = [
    LOGIN_PICKER_CONSOLE,
    LOGIN_PICKER_CONSOLE_2_1_310,
    LOGIN_PICKER_PLATFORM_2_1_310,
];

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

/// Selects the subscription row only when focus is still on the opening row of a picker the
/// inline renderer draws. CONTROL: the fullscreen renderer's picker, opened alike, is the
/// person's, since nothing past its method pick has been read in the real-Claude lab.
#[test]
fn a_subscription_session_chooses_the_subscription_row_where_the_dialog_opens_it() {
    for text in OPENED {
        let (_, prompt, picker) = picker_of(text);
        let d = pick_method(&prompt, &picker, Some(LoginMethod::Subscription));
        if FULLSCREEN.contains(&text) {
            assert!(matches!(&d, Decision::Escalate { .. }), "{d:?}: {text}");
            let inline = LoginPicker {
                renderer: aterm_phase::PanelRenderer::Inline,
                ..picker
            };
            let d = pick_method(&prompt, &inline, Some(LoginMethod::Subscription));
            assert_eq!(
                focus_steps(&d),
                Some(0),
                "its frame alone holds it: {d:?}: {text}"
            );
        } else {
            assert_eq!(focus_steps(&d), Some(0), "{d:?}: {text}");
        }
    }
    for text in MOVED {
        let (_, prompt, picker) = picker_of(text);
        let d = pick_method(&prompt, &picker, Some(LoginMethod::Subscription));
        assert!(matches!(d, Decision::Escalate { .. }), "{d:?}: {text}");
    }
}

#[test]
fn an_unknown_console_or_platform_method_is_the_owners() {
    for text in OPENED {
        let (_, prompt, picker) = picker_of(text);
        for method in [
            None,
            Some(LoginMethod::Console),
            Some(LoginMethod::Platform),
        ] {
            let d = pick_method(&prompt, &picker, method);
            assert!(
                matches!(d, Decision::Escalate { .. }),
                "{method:?}: {d:?}: {text}"
            );
        }
    }
}

/// Answers only the picker opened by the loop's own `/login`, never a user's or Console's.
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

/// The gate of the loop's own picker: carried, or backed out of where its sign-in nobody needs
/// (the login back in another tab, a roster row's, or one a followed renewal replaced); a
/// person's own picker is theirs whatever else holds.
#[test]
fn the_picker_gate_backs_out_only_of_its_own_login_nobody_needs() {
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
    for (back, roster, renewed, gate) in [
        (true, false, false, PickerGate::Back),
        (true, true, true, PickerGate::Back),
        (false, true, false, PickerGate::Roster),
        (false, true, true, PickerGate::Roster),
        (false, false, true, PickerGate::Renewed),
        (false, false, false, PickerGate::Carried),
    ] {
        let got = picker_gate(true, back, roster, renewed);
        assert_eq!(got, gate, "{back} {roster} {renewed}");
        assert_eq!(
            got.backs_out(),
            gate != PickerGate::Carried,
            "{back} {roster} {renewed}"
        );
        let persons = picker_gate(false, back, roster, renewed);
        assert_eq!(persons, PickerGate::Persons, "{back} {roster} {renewed}");
        assert!(!persons.backs_out(), "a person's own picker");
    }
}

/// The picker of the `/login` aterm typed whose sign-in nobody needs — the login back in another
/// tab, a roster row's (which aterm's sign-in ladder signs in), or one a followed renewal replaced
/// — is backed out of with the picker's own Esc, the focus where it stands, whatever `approve`
/// says; nobody is asked to close it. Negative controls: the same picker of aterm's own `/login`
/// otherwise gets the method's Enter where it opened in the inline renderer (and is the owner's
/// once arrow keys moved its focus, or in the fullscreen renderer's panel), and a person's own
/// picker is nobody's to answer or close. Both measured builds' pickers (2.1.303, 2.1.310) are
/// judged alike, and the Esc alike in both renderers' frames.
#[test]
fn the_picker_aterm_opened_and_nobody_needs_is_backed_out_of() {
    for text in OPENED.into_iter().chain(MOVED) {
        let opened = OPENED.contains(&text);
        let (rows, _, picker) = picker_of(text);
        let read = aterm_phase::read(Some("claude"), &rows, Some(2));
        for approve in [Approve::None, Approve::Safe, Approve::All] {
            let base = ApprovalCtx {
                login_method: Some(LoginMethod::Subscription),
                ..ctx(approve)
            };
            for gate in ["back", "roster", "renewed"] {
                let nobody = ApprovalCtx {
                    login_back: gate == "back",
                    login_roster: gate == "roster",
                    login_renewed: gate == "renewed",
                    ..base.clone()
                };
                let d = decide(&read, &rows, &nobody);
                let Decision::Approve {
                    rule_id,
                    choice: Choice::FocusKey { steps, key, .. },
                    guard,
                    ..
                } = &d
                else {
                    panic!("{gate} {approve:?}: {d:?}: {text}");
                };
                assert_eq!(
                    (*rule_id, *steps, *key),
                    (RULE_LOGIN_BACK_OUT, 0, "esc"),
                    "{gate} {approve:?}: {text}"
                );
                assert_eq!(
                    guard,
                    &row_guard(&picker.title_row),
                    "{gate} {approve:?}: {text}"
                );
            }
            let ours = ApprovalCtx {
                login_carry: true,
                ..base.clone()
            };
            let d = decide(&read, &rows, &ours);
            if opened && !FULLSCREEN.contains(&text) {
                assert_eq!(focus_steps(&d), Some(0), "{approve:?}: {d:?}: {text}");
            } else {
                assert!(
                    matches!(&d, Decision::Escalate { .. }),
                    "a focus moved off the opening row, or the fullscreen panel: {approve:?}: \
                     {d:?}: {text}"
                );
            }
            let d = decide(&read, &rows, &base);
            assert!(
                matches!(&d, Decision::Escalate { .. }),
                "a person's picker: {approve:?}: {d:?}: {text}"
            );
        }
    }
}

/// The row the dialog's Esc is fenced on: its own `Esc to cancel`, at the picker, the browser
/// hand-off and the error state; no such row once the dialog has signed in, or with no dialog.
#[test]
fn the_dialogs_esc_is_fenced_on_its_cancel_row() {
    let dialogs = [LOGIN_HANDOFF, LOGIN_ERROR, LOGIN_OPENING];
    for text in OPENED.into_iter().chain(MOVED).chain(dialogs) {
        let row = cancel_row(&screen(text)).map(|r| r.trim().to_string());
        assert_eq!(row.as_deref(), Some("Esc to cancel"), "{text}");
    }
    use aterm_phase::prompt::fixtures as f;
    for text in [
        LOGIN_SUCCESS,
        f::LOGIN_EXPIRED,
        f::LOGIN_EXPIRED_2_1_310,
        f::LOGIN_INTERRUPTED_2_1_310,
        f::LOGIN_INTERRUPTED_AT_WALL_2_1_310,
        f::WALL_NOT_LOGGED_IN_2_1_310,
    ] {
        assert_eq!(cancel_row(&screen(text)), None, "{text}");
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

/// Over a lost login, only `LOGIN_SUCCESS` sends `Enter`; user-initiated dialogs send nothing.
#[test]
fn each_dialog_state_gets_its_act() {
    let fresh = TurnEndState::default();
    let lost = walled();
    let short = [
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_CONSOLE_MENU,
        LOGIN_ERROR,
    ];
    for text in OPENED.into_iter().chain(MOVED).chain(short) {
        assert_eq!(
            dialog_act(&fresh, &dialog(text)),
            TurnEndAction::Nothing,
            "{text}"
        );
        assert_eq!(
            dialog_act(&lost, &dialog(text)),
            TurnEndAction::Nothing,
            "{text}"
        );
    }
    assert_eq!(
        dialog_act(&fresh, &dialog(LOGIN_SUCCESS)),
        TurnEndAction::Nothing
    );
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

/// Pre-success login dialog screens report `dialog_stands` as true.
#[test]
fn the_dialog_short_of_its_success_stands() {
    let dialogs = [
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_CONSOLE_MENU,
        LOGIN_ERROR,
    ];
    for text in OPENED.into_iter().chain(MOVED).chain(dialogs) {
        assert!(
            dialog_stands(&screen(text)),
            "{}",
            &text[..text.find('\n').unwrap_or(0)]
        );
    }
    use aterm_phase::prompt::fixtures as f;
    for text in [
        LOGIN_SUCCESS,
        f::LOGIN_EXPIRED,
        f::LOGIN_INTERRUPTED,
        f::LOGIN_EXPIRED_2_1_310,
        f::LOGIN_INTERRUPTED_2_1_310,
        f::LOGIN_INTERRUPTED_AT_WALL_2_1_310,
        f::LOGIN_INTERRUPTED_AT_EXPIRED_2_1_310,
        f::LOGIN_INTERRUPTED_FULLSCREEN_AT_EXPIRED_2_1_310,
    ] {
        assert!(!dialog_stands(&screen(text)), "{text}");
    }
}

/// NO MEASURED CLAUDE CODE SCREEN OF A LOST LOGIN IS A SIGN-IN MENU
/// ([`TurnEndReading::sign_in_menu_drawn`], which an account move's relaunch is made over): the
/// `/login` dialog of either measured build (2.1.303, 2.1.310), its picker, Console's menu, the
/// browser hand-off, its error and its success, is the dialog's (`dialog_act`), and every lost
/// login with the composer back (`Not logged in`, `Invalid API key`, `OAuth token revoked`, the
/// 401's `Please run /login`, `Login expired`, the footer's own `Not logged in · Run /login`) has
/// a composer to type into. CONTROL: agy's measured sign-in menu, drawn in place of its composer,
/// is one.
#[test]
fn no_measured_claude_login_screen_is_a_sign_in_menu() {
    use aterm_phase::prompt::fixtures as f;
    let claude = Some(aterm_phase::Program::Claude.name());
    let dialogs = [
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_CONSOLE_MENU,
        LOGIN_ERROR,
        LOGIN_SUCCESS,
    ];
    let walls = [
        f::LOGIN_EXPIRED,
        f::LOGIN_INTERRUPTED,
        f::FOOTER_NOT_LOGGED_IN_MEASURED,
        f::LOGIN_EXPIRED_2_1_310,
        f::LOGIN_INTERRUPTED_2_1_310,
        f::LOGIN_INTERRUPTED_AT_WALL_2_1_310,
        f::LOGIN_INTERRUPTED_AT_EXPIRED_2_1_310,
        f::LOGIN_INTERRUPTED_FULLSCREEN_AT_EXPIRED_2_1_310,
        f::WALL_NOT_LOGGED_IN_2_1_310,
        f::WALL_NOT_LOGGED_IN_NO_LOGIN_2_1_310,
        f::WALL_INVALID_API_KEY_2_1_310,
        f::WALL_OAUTH_REVOKED_2_1_310,
        f::WALL_401_RUN_LOGIN_2_1_310,
        f::FOOTER_NOT_LOGGED_IN_2_1_310,
    ];
    for text in OPENED.into_iter().chain(MOVED).chain(dialogs).chain(walls) {
        for col in [None, Some(2)] {
            assert!(
                !TurnEndReading::sign_in_menu_drawn(claude, &screen(text), col),
                "{col:?}: {text}"
            );
        }
    }
    let agy = screen(aterm_phase::agy::fixtures::SIGNED_OUT);
    assert!(TurnEndReading::sign_in_menu_drawn(
        Some(aterm_phase::Program::Agy.name()),
        &agy,
        None
    ));
}
