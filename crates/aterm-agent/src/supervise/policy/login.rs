// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Carries lost logins through Claude Code's `/login` dialog ([`super::turn_end`], [`aterm_phase::login`]).
use aterm_phase::wall::WallKind;
use aterm_phase::{LoginMethod, LoginPicker, LoginScreen, Program, PromptV2};

use super::super::config::SupervisorConfig;
use super::approval::{Choice, Decision};
use super::guard::row_guard;
use super::turn_end::{TurnEndAction, TurnEndState};

/// Rule ID for selecting the session's login method in `/login`.
pub const RULE_LOGIN_METHOD: &str = "login-method@v1";

/// Rule ID for confirming `Login successful` with Enter.
pub const RULE_LOGIN_DONE: &str = "login-done@v1";

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

/// Returns the turn-end action for a `/login` dialog screen ([`pick_method`]).
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
    /// Login already restored in another tab ([`LoginShared::Back`]).
    Back,
    /// Login superseded by a background renewal ([`renewal_followed`]).
    Renewed,
    /// User-initiated `/login` dialog.
    Persons,
}

/// Classifies who owns the `/login` method picker
/// ([`super::turn_end::TurnEndState::login_typed_at`]).
#[must_use]
pub const fn picker_gate(typed: bool, shared: Option<&LoginShared>, renewed: bool) -> PickerGate {
    match (typed, shared) {
        (false, _) => PickerGate::Persons,
        (true, Some(LoginShared::Back { .. })) => PickerGate::Back,
        (true, _) if renewed => PickerGate::Renewed,
        (true, _) => PickerGate::Carried,
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
        | Program::Gemini
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
