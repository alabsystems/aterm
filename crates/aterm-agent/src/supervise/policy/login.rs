// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LOST LOGIN CARRIED THROUGH CLAUDE CODE'S `/login` DIALOG (the owner,
//! 2026-10-19, after v0.106.0 left a lost login to them: the harness must
//! carry the login itself). The wall's own rule types `/login` once
//! ([`super::turn_end`]'s `auth-login@v1`); this module answers each state
//! of the dialog that opens ([`aterm_phase::login`]):
//!
//! | screen | act |
//! | --- | --- |
//! | the method picker | the method this session's login already is, by its exact label ([`RULE_LOGIN_METHOD`]); never a guess, never Console or a platform: unknown, the owner is told |
//! | Console's menu | nothing: its second option mints an API key; the owner is told |
//! | the browser hand-off | nothing (no `c`, no Esc, nothing pasted); the owner is told now: finish the sign-in in the browser |
//! | `Login successful. Press Enter to continue…` | Enter, once ([`RULE_LOGIN_DONE`]): the login is saved already |
//! | a success that stayed up after that Enter | nothing: the login is back, and nothing is told |
//! | an error, a retry, an account on hold | nothing; the owner is told |
//!
//! Every act is aterm's only inside its own carry of a lost login: the
//! picker only behind the `/login` this loop typed
//! ([`super::turn_end::TurnEndState::login_typed_at`]), the success Enter
//! only over a lost login this loop tracks. A `/login` a person opened on a
//! working session is theirs whole.
//!
//! The decisions are pure: [`pick_method`] for the picker (a box, the
//! approval loop's), [`dialog_act`] for every state a turn end reads (no
//! option to choose).

use aterm_phase::wall::WallKind;
use aterm_phase::{LoginMethod, LoginPicker, LoginScreen, Program, PromptV2};

use super::super::config::SupervisorConfig;
use super::approval::{Choice, Decision};
use super::guard::row_guard;
use super::turn_end::{TurnEndAction, TurnEndState};

/// The picker's rule: the session's own method chosen by its label.
pub const RULE_LOGIN_METHOD: &str = "login-method@v1";

/// The success state's rule: its one Enter.
pub const RULE_LOGIN_DONE: &str = "login-done@v1";

/// WHETHER THE PICKER IS aterm'S TO ANSWER, `method` the method this
/// session's login already is (the host's
/// [`crate::supervise::IdleHost::login_method`]; `None` where nothing says):
/// `Ok` with the index of the method's row, else why it is the owner's. Only
/// the subscription is ever chosen: a Console login's menu can mint an API
/// key and a platform's login is its environment's, so either is the
/// owner's — as is a picker that does not draw the method, and one whose
/// focus is not on its first row, where the dialog opens it: a person's key
/// moved it there (to Console, say, to bill a usage limit's work to the
/// API), and the choice is theirs.
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

/// What the picker gets ([`picks`]): the focus moved onto the method's row
/// and Enter once a fresh read shows it there ([`Choice::Focus`]), guarded on
/// the dialog's title row; never a digit. Any other picker is the owner's,
/// told so by the escalation's reason.
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
                "pick the sign-in method in the tab, then finish signing in in the browser \
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
            "pick the sign-in method in the tab, then finish signing in in the browser \
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

/// THE ACT AT A `/login` DIALOG A TURN END READS (no option to choose: the
/// picker is a box, [`pick_method`]'s). Only over a lost login this loop
/// tracks: a dialog with none under it is a person's own `/login` on a
/// working session — switching accounts, say — and every step of it theirs.
/// There the success state gets its Enter once — a second read of it after
/// that Enter is the owner's to look at — and every other state nothing: the
/// hand-off is the person's sign-in in the browser, an error's `Press Enter
/// to retry.` would open the browser again for nobody, and Console's menu
/// can mint an API key. What the owner is told of each is the login's
/// notice (`super::exception::exception_for`), never an escalation of the
/// loop's beside it: one surface.
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

/// WHOSE CLAUDE CODE'S `/login` METHOD PICKER IS ([`picker_gate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerGate {
    /// Behind the `/login` this loop typed into a lost login: aterm's carry,
    /// answered with the method the session's login already is
    /// ([`pick_method`]).
    Carried,
    /// Behind that `/login`, the login came back in another tab since
    /// ([`LoginShared::Back`]): nothing is left to sign in, so nobody answers
    /// it — a method chosen would open a browser sign-in nobody needs (one
    /// sign-in per login) — and the owner is told to close it.
    Back,
    /// Behind that `/login`, a renewal replaced the login it signs in, and
    /// the tab moves onto the renewed one by itself once the dialog is left
    /// ([`renewal_followed`]): nobody answers it — a method chosen would open
    /// a browser sign-in nobody needs, bringing back the login the renewal
    /// replaced — and the owner is told to close it (acct3-3).
    Renewed,
    /// A person's own `/login`: theirs whole.
    Persons,
}

/// WHOSE THE METHOD PICKER IS: `typed`, the `/login` it belongs to is the
/// one this loop typed into a lost login
/// ([`super::turn_end::TurnEndState::login_typed_at`]); `shared`, the host's
/// word on that login's one sign-in
/// ([`crate::supervise::IdleHost::login_shared`]); `renewed`, a renewal the
/// tab follows by itself stands over that login ([`renewal_followed`]). The
/// approval loop's gate, and the one the login wall's conformance binds.
#[must_use]
pub fn picker_gate(typed: bool, shared: Option<&LoginShared>, renewed: bool) -> PickerGate {
    match (typed, shared) {
        (false, _) => PickerGate::Persons,
        (true, Some(LoginShared::Back { .. })) => PickerGate::Back,
        (true, _) if renewed => PickerGate::Renewed,
        (true, _) => PickerGate::Carried,
    }
}

/// WHETHER A RENEWAL STANDS OVER THE SESSION'S LOST LOGIN THAT THE TAB
/// FOLLOWS BY ITSELF: the process holds a login a renewal replaced
/// (`superseded`, [`crate::supervise::IdleHost::login_superseded`]: asked of
/// a renewable row's login alone), the logins' lifecycle on, and a relaunch
/// in reach (`relaunch` and `resume_limits` on, a host that relaunches the
/// agent: `restartable`). Then the `/login` this loop typed has nothing left
/// to sign in — its dialog is the owner's to close, and the tab moves onto
/// the renewed login once it is left (the review of the login carry,
/// acct3-3: the dialog's notice asked for a browser sign-in nobody needed).
/// Out of reach, the sign-in in the tab is the owner's way on, as before.
#[must_use]
pub fn renewal_followed(superseded: bool, cfg: &SupervisorConfig, restartable: bool) -> bool {
    superseded && cfg.logins && cfg.resume_limits && cfg.relaunch && restartable
}

/// Whether `program`'s lost login is carried here: Claude Code's, whose wall,
/// `/login` dialog and notices this module reads. Exhaustive, so an agent
/// added to [`Program`] does not compile until it is decided here.
#[must_use]
pub const fn carried(program: Program) -> bool {
    match program {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
    }
}

/// ANOTHER TAB'S SIGN-IN OF THIS SESSION'S LOGIN, as the tabs of one host
/// share a login ([`crate::harness::login_latch`], the host's
/// [`crate::supervise::IdleHost::login_shared`]): read at each reading of a
/// lost login of a carried agent
/// ([`super::turn_end::TurnEndReading::login_shared`]). One expiry walls every
/// tab working on the login; one tab's `/login` signs them all in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginShared {
    /// Tab `by` carries the login's one sign-in (its `/login`, its notice):
    /// this tab types none, and waits on it.
    Carried {
        /// The tab that carries it, as the host names it.
        by: String,
    },
    /// The login came back in tab `by` while this tab waited on it: this
    /// tab's turn is delivered again, once
    /// ([`super::turn_end::RULE_LOGIN_BACK`]).
    Back {
        /// The tab it came back in.
        by: String,
    },
}

/// Whether `rows` show `/login`'s dialog short of its success: a busy read of
/// them is the dialog's own (the browser hand-off's spinner and paste field),
/// no agent working, and the sign-in the owner's notice stands for is still
/// theirs to finish. MEASURED in the real-Claude lab on 2.1.303 (2026-10-19):
/// read as the agent working, the hand-off took the owner's notice down while
/// its URL and paste field stood. `Login successful` is the login back.
#[must_use]
pub fn dialog_stands(rows: &[String]) -> bool {
    aterm_phase::login_screen(rows).is_some_and(|s| !matches!(s, LoginScreen::Success { .. }))
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
