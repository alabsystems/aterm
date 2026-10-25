// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Carries lost logins through Claude Code's `/login` dialog ([`super::turn_end`], [`aterm_phase::login`]).
use std::time::Instant;

use aterm_phase::wall::WallKind;
use aterm_phase::{LoginMethod, LoginPicker, LoginScreen, Program, PromptV2};

use super::super::config::SupervisorConfig;
use super::approval::{Choice, Decision};
use super::guard::row_guard;
use super::turn_end::{TurnEndAction, TurnEndReading, TurnEndState};

/// Rule ID for selecting the session's login method in `/login`.
pub const RULE_LOGIN_METHOD: &str = "login-method@v1";

/// Rule ID for confirming `Login successful` with Enter.
pub const RULE_LOGIN_DONE: &str = "login-done@v1";

/// Rule ID for backing out of the dialog of the loop's own `/login` with its Esc ([`backs_out`]).
pub const RULE_LOGIN_BACK_OUT: &str = "login-back-out@v1";

/// Returns whether the `/login` method picker can be answered with `method` ([`crate::supervise::IdleHost::login_method`]).
pub fn picks(picker: &LoginPicker, method: Option<LoginMethod>) -> Result<usize, String> {
    let Some(method) = method else {
        return Err("nothing says which method this session signs in with".to_string());
    };
    if method != LoginMethod::Subscription {
        return Err(format!(
            "this session signs in with its {} method, which only you choose",
            method.word()
        ));
    }
    if picker.focused != Some(0) {
        return Err("its focus was moved off the first row".to_string());
    }
    picker
        .index_of(method)
        .ok_or_else(|| format!("it draws no `{}` row", method.label()))
}

/// Returns the action to select `method` in `picker` ([`picks`], [`Choice::Focus`]).
#[must_use]
pub fn pick_method(
    prompt: &PromptV2,
    picker: &LoginPicker,
    method: Option<LoginMethod>,
) -> Decision {
    let target = match picks(picker, method) {
        Ok(target) => target,
        Err(why) => {
            return Decision::escalate(format!(
                "pick the sign-in method in the tab, then finish the browser sign-in \
                 (Claude Code's login method picker: {why})"
            ));
        }
    };
    let Some(option) = prompt.options.get(target).filter(|o| {
        o.label
            .trim_start()
            .starts_with(LoginMethod::Subscription.label())
    }) else {
        return Decision::escalate(
            "pick the sign-in method in the tab, then finish the browser sign-in \
             (Claude Code's login method picker: its rows read otherwise than its options)",
        );
    };
    let label = option.label.clone();
    Decision::Approve {
        rule_id: RULE_LOGIN_METHOD,
        choice: Choice::Focus {
            steps: i32::try_from(target).unwrap_or(0),
            label: label.clone(),
        },
        guard: row_guard(&picker.title_row),
        subject: label,
        unproven: None,
    }
}

/// Returns the action to back out of the method picker of the loop's own `/login` whose sign-in
/// nobody needs ([`backs_out`], [`PickerGate::backs_out`]): the picker's own Esc (`Esc to
/// cancel`), the focus left where it stands, so no method is picked and no browser sign-in opens.
#[must_use]
pub fn back_out_of_picker(prompt: &PromptV2, picker: &LoginPicker) -> Decision {
    let Some(focused) = prompt.focused() else {
        return Decision::escalate(
            "the /login aterm typed: its picker draws no focused row to cancel from (Claude \
             Code's login method picker)",
        );
    };
    Decision::Approve {
        rule_id: RULE_LOGIN_BACK_OUT,
        choice: Choice::FocusKey {
            steps: 0,
            label: focused.label.clone(),
            key: "esc",
        },
        guard: row_guard(&picker.title_row),
        subject: "the /login aterm typed: Esc, no method picked".to_string(),
        unproven: None,
    }
}

/// WHETHER THE LOOP BACKS OUT OF THE `/login` DIALOG ON `screen` NOW: the `/login` aterm typed
/// into a lost login, whose dialog still stands at its method picker, its browser hand-off or a
/// message of its own, and whose browser sign-in, which only a person could finish, nobody needs
/// ([`nobody_signs_in`]): the login is a roster row's, which aterm's sign-in ladder signs in by
/// itself (`crate::harness::sign_in_ladder`; the tab then waits on the row's sign-in as at a
/// roster row's wall, `super::turn_end::login_wall_waits`), the login came back in another tab
/// (the tab then continues its turn there, `super::turn_end::RULE_LOGIN_BACK`), or a renewal the
/// tab follows replaced it (the tab then relaunches onto the renewed login). The dialog is closed
/// with its own Esc, and no person is told to close it. Once, and again only while the dialog is
/// still drawn once the last Esc had its time to close it ([`TurnEndState::login_closing`]);
/// never a person's own `/login` ([`TurnEndState::login_typed_at`]: one opened after the dialog
/// aterm backed out of was read gone included), Console's menu (which only a person's pick
/// opens), or the success state, whose Enter the loop presses ([`dialog_act`]).
#[must_use]
pub fn backs_out(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    screen: &LoginScreen,
    now: Instant,
) -> bool {
    state.login_typed_at().is_some()
        && nobody_signs_in(state, reading, cfg)
        && state.login_closing(now).is_none()
        && matches!(
            screen,
            LoginScreen::Picker(_) | LoginScreen::Browser | LoginScreen::Failed
        )
}

/// Whether nobody needs the browser sign-in of a `/login` dialog over the session's login: aterm
/// signs that login in by itself ([`signs_in_itself`]), it came back in another tab
/// (`super::turn_end::login_back_elsewhere`), or a renewal the tab follows replaced it
/// ([`renewal_followed`]). Over the `/login` aterm typed, such a dialog is aterm's to close
/// ([`backs_out`]), never a person's to be told to.
#[must_use]
pub fn nobody_signs_in(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> bool {
    signs_in_itself(reading, cfg)
        || super::turn_end::login_back_elsewhere(state, reading)
        || renewal_followed(
            reading.login_superseded,
            cfg,
            reading.restartable || reading.upgrading,
        )
}

/// Whether aterm's sign-in ladder signs the session's login in by itself: Claude Code's, its
/// login a roster row's with an email (`login_label`) and `logins` on. No person is told to sign
/// such a login in, at a `/login` aterm typed or anywhere else
/// (`HarnessLoginWall`'s `NoRosterRowSignedInByAPerson`).
#[must_use]
pub fn signs_in_itself(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    carried(reading.program) && cfg.logins && reading.login_label.is_some()
}

/// The `Esc to cancel` row of the `/login` dialog `rows` end with, short of its success: the row
/// the dialog's Esc is fenced on ([`TurnEndAction::Cancel`]).
#[must_use]
pub fn cancel_row(rows: &[String]) -> Option<&String> {
    if !dialog_stands(rows) {
        return None;
    }
    rows.iter()
        .rev()
        .find(|row| row.trim() == aterm_phase::anchor!("prompt.cancel"))
}

/// Returns the turn-end action for a `/login` dialog screen ([`pick_method`], [`backs_out`]).
#[must_use]
pub fn dialog_act(state: &TurnEndState, screen: &LoginScreen) -> TurnEndAction {
    match screen {
        LoginScreen::Success { row }
            if !state.login_confirmed()
                && (state.wall_attempts(WallKind::Auth).is_some()
                    || state.wall_attempts(WallKind::UsageWeekly).is_some()
                    || state
                        .wall_attempts(WallKind::ModelBucket { consent: false })
                        .is_some()) =>
        {
            TurnEndAction::Confirm {
                rule_id: RULE_LOGIN_DONE,
                row: row.clone(),
            }
        }
        LoginScreen::Success { .. }
        | LoginScreen::Browser
        | LoginScreen::Picker(_)
        | LoginScreen::ConsoleMenu
        | LoginScreen::Failed => TurnEndAction::Nothing,
    }
}

/// Ownership classification for a `/login` method picker ([`picker_gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerGate {
    /// Picker opened by this loop's `/login` into a lost login.
    Carried,
    /// Login already restored in another tab ([`LoginShared::Back`], as the turn end counts it:
    /// [`super::turn_end::TurnEndState::login_back_from`]): the loop backs out of its own `/login`
    /// ([`Self::backs_out`]).
    Back,
    /// The login is a roster row's, which aterm's sign-in ladder signs in: the loop backs out of
    /// its own `/login` ([`Self::backs_out`]).
    Roster,
    /// Login superseded by a background renewal the tab follows ([`renewal_followed`]): the loop
    /// backs out of its own `/login` ([`Self::backs_out`]).
    Renewed,
    /// User-initiated `/login` dialog.
    Persons,
}

impl PickerGate {
    /// Whether the picker is the loop's own `/login` whose sign-in nobody needs
    /// ([`nobody_signs_in`]), which the loop backs out of with the picker's own Esc
    /// ([`back_out_of_picker`]) rather than ask anyone to close it.
    #[must_use]
    pub const fn backs_out(self) -> bool {
        match self {
            Self::Back | Self::Roster | Self::Renewed => true,
            Self::Carried | Self::Persons => false,
        }
    }
}

/// Classifies who owns the `/login` method picker
/// ([`super::turn_end::TurnEndState::login_typed_at`]); `back`: the login came back in another
/// tab as the turn end counts it ([`super::turn_end::TurnEndState::login_back_from`], never the
/// host's raw [`LoginShared::Back`], so the picker is backed out of exactly where the turn end
/// backs out of the hand-off); `roster`: the session's login is a roster row's with `logins` on
/// ([`backs_out`]).
#[must_use]
pub const fn picker_gate(typed: bool, back: bool, roster: bool, renewed: bool) -> PickerGate {
    if !typed {
        PickerGate::Persons
    } else if back {
        PickerGate::Back
    } else if roster {
        PickerGate::Roster
    } else if renewed {
        PickerGate::Renewed
    } else {
        PickerGate::Carried
    }
}

/// Returns whether the session's lost login was superseded by a renewal that the tab will follow.
#[must_use]
pub const fn renewal_followed(superseded: bool, cfg: &SupervisorConfig, restartable: bool) -> bool {
    superseded && cfg.logins && cfg.resume_limits && cfg.relaunch && restartable
}

/// Returns whether `program`'s lost login is carried by this module.
#[must_use]
pub const fn carried(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}

/// Cross-tab state for a shared login ([`crate::harness::login_latch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginShared {
    /// Another tab (`by`) is currently carrying the sign-in.
    Carried {
        /// Tab carrying the sign-in.
        by: String,
    },
    /// Sign-in completed in tab `by` ([`super::turn_end::RULE_LOGIN_BACK`]).
    Back {
        /// Tab where sign-in completed.
        by: String,
    },
}

/// Returns whether `rows` show an active `/login` dialog prior to completion.
#[must_use]
pub fn dialog_stands(rows: &[String]) -> bool {
    aterm_phase::login_screen(rows).is_some_and(|s| !matches!(s, LoginScreen::Success { .. }))
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
