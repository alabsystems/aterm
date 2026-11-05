// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Carries lost logins through Claude Code's `/login` dialog ([`super::turn_end`], [`aterm_phase::login`]).
use std::time::{Duration, Instant};

use aterm_phase::wall::WallKind;
use aterm_phase::{LoginMethod, LoginPicker, LoginScreen, Program, PromptV2};

use super::super::config::SupervisorConfig;
use super::approval::{Choice, Decision};
use super::guard::row_guard;
use super::turn_end::{TurnEndAction, TurnEndReading, TurnEndState, WaitKind};

/// Rule ID the loop once selected the session's login method in `/login` under. It selects none
/// now ([`pick_method`]: the pick opens the browser, which aterm never brings forward by itself);
/// the rule stays defined so older books, stats and tapes still read.
pub const RULE_LOGIN_METHOD: &str = "login-method@v1";

/// Rule ID for confirming `Login successful` with Enter.
pub const RULE_LOGIN_DONE: &str = "login-done@v1";

/// Rule ID for backing out of the dialog of the loop's own `/login` with its Esc ([`backs_out`]).
pub const RULE_LOGIN_BACK_OUT: &str = "login-back-out@v1";

/// Rule ID for closing, with the dialog's own Esc, a person's own `/login` left standing over a
/// usage limit whose act is due ([`over_limit_act`]).
pub const RULE_LOGIN_OVER_LIMIT: &str = "login-over-limit@v1";

/// How many Escs aterm presses at a person's `/login` it closes ([`over_limit_act`]) before a
/// dialog still drawn is the person's to close, told to them now ([`over_limit_unclosed`]).
pub const OVER_LIMIT_BACKS_MAX: u32 = 3;

/// Why the `/login` method picker is the person's to answer ([`pick_method`]): which method the
/// session signs in with, as far as aterm reads it, for the escalation's words.
///
/// Every method's pick opens a sign-in only a person can finish, the subscription's in the
/// system browser, which comes forward over whatever the person is doing; aterm never brings a
/// window forward by itself (v6 §14.17, rule 1). So the picker is never answered by the loop:
/// the person picks, and the browser opens on their own key.
fn why_persons(picker: &LoginPicker, method: Option<LoginMethod>) -> String {
    let Some(method) = method else {
        return "nothing says which method this session signs in with".to_string();
    };
    if method != LoginMethod::Subscription {
        return format!(
            "this session signs in with its {} method, which only you choose",
            method.word()
        );
    }
    if picker.index_of(method).is_none() {
        return format!("it draws no `{}` row", method.label());
    }
    format!(
        "its `{}` row opens the sign-in in your browser, which aterm never brings forward by \
         itself",
        method.label()
    )
}

/// WHAT CLAUDE CODE'S SETTINGS MAKE OF `/login` IN THE SESSION (v6 §14.17 rule 1; the
/// post-landing review, 2026-11-02), as the host read them before aterm types it
/// (`IdleHost::login_pin`, `claude_settings::login_pin`). Claude Code 2.1.316 (the store build's
/// own code, read for this rule) reads `forceLoginMethod` from its merged settings, and its
/// `/login` then draws no method picker:
/// `claudeai` or `console` starts the OAuth sign-in one tick after the Enter, and the system
/// browser comes forward over the person's work; a gateway (`forceLoginGatewayUrl`, or
/// `forceLoginMethod` `gateway` in managed settings) draws a form of its own, which aterm's login
/// reader does not read. aterm types `/login` only over [`Self::Picker`]; over every other it tells
/// the person, quietly, to type it themselves ([`Self::leaves_login`]), so the browser opens on
/// their key, never aterm's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoginPin {
    /// Nothing the host read pins a sign-in method: `/login` draws its picker. Also the answer of
    /// a host that reads no settings (a test's, a replay of a tape recorded before the door).
    #[default]
    Picker,
    /// `forceLoginMethod` names a method (`claudeai`, `console`): `/login` opens the browser at once.
    Method,
    /// A gateway sign-in (`forceLoginGatewayUrl`, or `forceLoginMethod` `gateway`): `/login`
    /// draws the gateway's form, which aterm does not read.
    Gateway,
    /// A settings source of the session's could not be read, or its environment could not: aterm
    /// cannot tell whether `/login` opens the browser at once.
    Unread,
}

impl LoginPin {
    /// Every pin, for a tape's words.
    pub const ALL: [Self; 4] = [Self::Picker, Self::Method, Self::Gateway, Self::Unread];

    /// Whether `/login` is the person's to type, not aterm's: anything but [`Self::Picker`].
    #[must_use]
    pub const fn leaves_login(self) -> bool {
        !matches!(self, Self::Picker)
    }

    /// The pin's word on a tape and in a journal line.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Picker => "picker",
            Self::Method => "method",
            Self::Gateway => "gateway",
            Self::Unread => "unread",
        }
    }

    /// The pin [`Self::word`] names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.word() == word)
    }

    /// Why aterm did not type `/login` over the pin (`notice::LOGIN_NOT_RUN`'s `{why}`), `None`
    /// over [`Self::Picker`].
    #[must_use]
    pub const fn why(self) -> Option<&'static str> {
        match self {
            Self::Picker => None,
            Self::Method => Some(super::notice::WHY_PINNED),
            Self::Gateway => Some(super::notice::WHY_GATEWAY),
            Self::Unread => Some(super::notice::WHY_PIN_UNREAD),
        }
    }
}

/// Returns the decision at the method picker of the loop's own `/login` ([`why_persons`]): the
/// person's, escalated quietly with why, never a key pressed in it.
#[must_use]
pub fn pick_method(picker: &LoginPicker, method: Option<LoginMethod>) -> Decision {
    Decision::escalate(format!(
        "pick the sign-in method in the tab, then finish the browser sign-in (Claude Code's \
         login method picker: {})",
        why_persons(picker, method)
    ))
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

/// HOW LONG A PERSON'S OWN `/login` DIALOG STANDS WITH NO KEY OF THEIRS BEFORE IT IS LEFT
/// ([`over_limit_act`]): from their newest key in the tab, or where none is known from this loop's
/// first read of the dialog. A browser sign-in nobody finished in that time is abandoned (the code
/// it would paste has long expired), and a person still at the tab has seen the limit's reset pass.
pub const OVER_LIMIT_UNUSED: Duration = Duration::from_secs(30 * 60);

/// THE ACT AT A PERSON'S OWN `/login` DIALOG LEFT STANDING OVER A USAGE LIMIT
/// (`login-over-limit@v1`): the dialog covers the composer, so neither aterm's resume nor Claude
/// Code's own armed continue reaches the tab while it stands (measured 2026-10-31 on 2.1.302: a
/// person's browser hand-off stood 33 h past its weekly reset, 55 h in all, Claude Code's countdown
/// under it unfired). Where the act the limit owes comes due
/// ([`super::turn_end::login_over_limit_due`]: the limit's reset and its grace, or at once where
/// the roster advises a move or measured the limit cleared) and the dialog is [`OVER_LIMIT_UNUSED`]
/// left, aterm closes it with its own fenced Esc, as it closes its own `/login` ([`backs_out`]),
/// and the limit's act follows at the composer it uncovers. Until then the tab waits for that
/// moment, so the reset is a wake and no limit is told as unhandled. The dialog's method picker,
/// its browser hand-off and its error state alike ([`over_limit_screen`]); never aterm's own
/// `/login` ([`backs_out`]'s), Console's menu (only a person's pick opens it) or the success state
/// ([`dialog_act`]'s); after [`OVER_LIMIT_BACKS_MAX`] Escs that left it drawn, nothing more is
/// pressed and the person is told now ([`over_limit_unclosed`]). `None`: the dialog is the
/// person's, as before.
#[must_use]
pub fn over_limit_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    screen: &LoginScreen,
    now: Instant,
) -> Option<TurnEndAction> {
    if !over_limit_screen(state, screen) {
        return None;
    }
    let due = super::turn_end::login_over_limit_due(state, reading, cfg, now)?;
    if let Some(until) = state.login_over_limit_closing(now) {
        return Some(TurnEndAction::WaitUntil {
            until,
            why: "the /login dialog aterm closed to close".to_string(),
            wait: WaitKind::Other,
        });
    }
    if state.login_over_limit_backs() >= OVER_LIMIT_BACKS_MAX {
        return None;
    }
    let left = unused_at(state, reading, now);
    let until = left.max(due);
    if now < until {
        return Some(TurnEndAction::WaitUntil {
            until,
            why: if left > due {
                "a person's own /login over the usage limit, to stand unused".to_string()
            } else {
                "the usage limit's act, which a person's own /login covers".to_string()
            },
            wait: WaitKind::Other,
        });
    }
    Some(TurnEndAction::Cancel {
        rule_id: RULE_LOGIN_OVER_LIMIT,
    })
}

/// When the `/login` standing is [`OVER_LIMIT_UNUSED`] left: from the person's newest key or touch
/// in the tab, or where neither is known from this loop's first read of the dialog.
fn unused_at(state: &TurnEndState, reading: &TurnEndReading, now: Instant) -> Instant {
    let touched = reading
        .key
        .into_iter()
        .chain(reading.person)
        .min()
        .and_then(|ago| now.checked_sub(ago));
    let from = touched.or_else(|| state.login_dialog_seen()).unwrap_or(now);
    from.checked_add(OVER_LIMIT_UNUSED).unwrap_or(from)
}

/// Whether `screen` is a person's own `/login` dialog [`over_limit_act`] may close, under no
/// `/login` of aterm's: its method picker (no method picked, so no sign-in has started; the
/// approval loop still tells the person the pick is theirs, [`picker_gate`]), its browser
/// hand-off, or its error state, each drawn with the dialog's own `Esc to cancel` row
/// ([`cancel_row`]). A person one step from the hand-off left the tab as surely as one at it.
/// Console's menu stays theirs: only their pick at the picker opens it.
pub(in crate::supervise) fn over_limit_screen(state: &TurnEndState, screen: &LoginScreen) -> bool {
    state.login_typed_at().is_none()
        && matches!(
            screen,
            LoginScreen::Picker(_) | LoginScreen::Browser | LoginScreen::Failed
        )
}

/// WHETHER A PERSON'S OWN `/login` LEFT OVER A USAGE LIMIT STAYS DRAWN after
/// [`OVER_LIMIT_BACKS_MAX`] Escs of [`over_limit_act`]'s, the limit's act due
/// ([`super::turn_end::login_over_limit_due`]): only a person can close it now, and their one key
/// does (the exception's `Now`).
#[must_use]
pub fn over_limit_unclosed(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> bool {
    carried(reading.program)
        && reading
            .login_screen
            .as_ref()
            .is_some_and(|screen| over_limit_screen(state, screen))
        && state.login_over_limit_backs() >= OVER_LIMIT_BACKS_MAX
        && state.login_over_limit_closing(now).is_none()
        && super::turn_end::login_over_limit_due(state, reading, cfg, now)
            .is_some_and(|due| due <= now)
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
