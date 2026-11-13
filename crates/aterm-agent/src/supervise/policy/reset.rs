// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Limit-reset policy and confirmation verdict ([`ResetAsk`], [`ResetWord`], [`confirm_verdict`],
//! [`KeepWhy`], [`KeepWhy::Kind`], [`ResetEnd`], [`ResetOutcome`]).

use std::fmt;
use std::time::Duration;

use aterm_phase::{ConfirmRead, ResetKind};

use crate::harness::accounts::OFFER_EXPIRING_S;

/// Grace period for the vendor's own continuation after a reset (`TurnEndState::reset_told`).
pub const RESET_CONTINUES_WAIT: Duration = Duration::from_secs(60);

/// What the loop asks its host before the grant row's first key (`IdleHost::reset_take`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetAsk {
    /// Reset kind offered by the row ([`ResetGo::kind`]).
    pub kind: ResetKind,
    /// The resets of that kind left, as the row counts them.
    pub left: u32,
    /// The grant's last day as the row draws it ([`ResetGo::day`]).
    pub day: String,
    /// The first second of that day (unix seconds).
    pub day_start: Option<i64>,
}

impl ResetAsk {
    /// Build a [`ResetGo`] carrying this ask's kind, count, and day ([`confirm_verdict`]).
    #[must_use]
    pub fn go(&self, until: i64, long: bool, limits: Vec<String>) -> ResetGo {
        ResetGo {
            until,
            long,
            limits,
            kind: self.kind,
            left: self.left,
            day: self.day.clone(),
        }
    }
}

/// The host's go ([`ResetWord::Take`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetGo {
    /// The wall's reset (unix seconds), from this process's own transcript row.
    pub until: i64,
    /// The wall stands at least twelve hours from now.
    pub long: bool,
    /// Names the walled window is drawn by in a confirmation's body.
    pub limits: Vec<String>,
    /// The kind of reset the row offered ([`ResetAsk::kind`]): the one the confirmation must be of.
    pub kind: ResetKind,
    /// The resets left as the row counted them ([`ResetAsk::left`]).
    pub left: u32,
    /// The grant's last day as the row drew it ([`ResetAsk::day`]).
    pub day: String,
}

/// Why the host did not give the go ([`ResetWord::No`]): one stable word each ([`Self::word`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetNo {
    /// `[harness] limit_reset` is off, read live.
    Off,
    /// No host answers here (the trait's default).
    NoHost,
    /// The host could not read what it judges by.
    Unread,
    /// The process is not on the default login.
    NotDefaultLogin,
    /// Another login is in the way.
    OtherLogin,
    /// The last quota rejection in the transcript is not this process's.
    NotOwnWall,
    /// The wall names no reset that is believed and still ahead.
    NoReset,
    /// The walled window is not one a grant clears.
    Window,
    /// Neither a long wall nor a grant whose last day holds a deadline within a day.
    NotQualifying,
    /// Another session's claim for this wall is in flight.
    Held,
    /// This wall has had its one attempt.
    Attempted,
    /// The accounts store did not admit: a refused roster, a state that could not be written.
    State,
    /// macOS's program check is stuck: nothing the resumed agent starts would start.
    ProgramCheck,
}

impl ResetNo {
    /// The refusal's one word: lower case, no space, no account's name.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ResetNo::Off => "off",
            ResetNo::NoHost => "no-host",
            ResetNo::Unread => "unread",
            ResetNo::NotDefaultLogin => "not-default-login",
            ResetNo::OtherLogin => "other-login",
            ResetNo::NotOwnWall => "not-own-wall",
            ResetNo::NoReset => "no-reset",
            ResetNo::Window => "window",
            ResetNo::NotQualifying => "not-qualifying",
            ResetNo::Held => "held",
            ResetNo::Attempted => "attempted",
            ResetNo::State => "state",
            ResetNo::ProgramCheck => "program-check",
        }
    }

    /// Every refusal, in the order [`Self::word`] lists them.
    pub const ALL: [ResetNo; 13] = [
        ResetNo::Off,
        ResetNo::NoHost,
        ResetNo::Unread,
        ResetNo::NotDefaultLogin,
        ResetNo::OtherLogin,
        ResetNo::NotOwnWall,
        ResetNo::NoReset,
        ResetNo::Window,
        ResetNo::NotQualifying,
        ResetNo::Held,
        ResetNo::Attempted,
        ResetNo::State,
        ResetNo::ProgramCheck,
    ];

    /// [`Self::word`] read back (a harness tape's host answer, `supervise::tape::host`).
    #[must_use]
    pub fn of_word(word: &str) -> Option<ResetNo> {
        ResetNo::ALL.into_iter().find(|no| no.word() == word)
    }
}

/// The host's answer to [`ResetAsk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetWord {
    /// The claim is this session's: the row may be pressed.
    Take(ResetGo),
    /// Not yet answerable: ask again in a moment.
    Again,
    /// No, and why.
    No(ResetNo),
}

/// Why a reset was kept rather than entered ([`fmt::Display`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepWhy {
    /// The confirmation did not read exactly.
    Inexact(&'static str),
    /// Confirmation is for a different reset kind than the row taken.
    Kind,
    /// Its body names none of the limits the wall is drawn by.
    OtherLimit,
    /// Its count is not the row's.
    Count,
    /// Its deadline's day is not the row's day.
    Day,
    /// The wall is not a long one, and its deadline did not place or is not ahead.
    Deadline,
    /// The wall is not a long one, and its deadline is more than a day off.
    NotQualifying,
    /// The cursor is not on the yes.
    Focus,
    /// The host no longer answers that the claim is this session's.
    Claim,
    /// The row was pressed and nothing opened.
    NotOpened,
    /// The lookup the row starts outlasted its bound.
    CheckTimeout,
    /// The Enter was written and the confirmation still stands.
    EnterNotTaken,
    /// A loop that did not take the row found the confirmation: it never enters one.
    Successor,
    /// The confirmation was gone before it was judged.
    Closed,
}

impl fmt::Display for KeepWhy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = match self {
            KeepWhy::Inexact(part) => return write!(f, "inexact-{part}"),
            KeepWhy::Kind => "kind",
            KeepWhy::OtherLimit => "other-limit",
            KeepWhy::Count => "count",
            KeepWhy::Day => "day",
            KeepWhy::Deadline => "deadline",
            KeepWhy::NotQualifying => "not-qualifying",
            KeepWhy::Focus => "focus",
            KeepWhy::Claim => "claim",
            KeepWhy::NotOpened => "not-opened",
            KeepWhy::CheckTimeout => "check-timeout",
            KeepWhy::EnterNotTaken => "enter-not-taken",
            KeepWhy::Successor => "successor",
            KeepWhy::Closed => "closed",
        };
        f.write_str(word)
    }
}

impl KeepWhy {
    /// Every word but [`KeepWhy::Inexact`]'s, which carries its part.
    const WORDED: [KeepWhy; 13] = [
        KeepWhy::Kind,
        KeepWhy::OtherLimit,
        KeepWhy::Count,
        KeepWhy::Day,
        KeepWhy::Deadline,
        KeepWhy::NotQualifying,
        KeepWhy::Focus,
        KeepWhy::Claim,
        KeepWhy::NotOpened,
        KeepWhy::CheckTimeout,
        KeepWhy::EnterNotTaken,
        KeepWhy::Successor,
        KeepWhy::Closed,
    ];

    /// Human-readable text for a [`fmt::Display`] word under [`super::approval::RULE_LIMIT_RESET`].
    #[must_use]
    pub fn plain_words(word: &str) -> Option<String> {
        if let Some(part) = word.strip_prefix("inexact-").filter(|p| !p.is_empty()) {
            return Some(format!(
                "its confirmation did not read exactly as expected (its {part})"
            ));
        }
        let why = KeepWhy::WORDED
            .into_iter()
            .find(|why| why.to_string() == word)?;
        let words = match why {
            KeepWhy::Inexact(_) => return None,
            KeepWhy::Kind => "its confirmation was for another kind of reset than the row taken",
            KeepWhy::OtherLimit => "its confirmation named none of the limits the session hit",
            KeepWhy::Count => "its confirmation's count of resets was not the one offered",
            KeepWhy::Day => "its confirmation's deadline was not on the day offered",
            KeepWhy::Deadline => {
                "the limit was a short one, and the reset's deadline could not be placed ahead"
            }
            KeepWhy::NotQualifying => {
                "the limit was a short one, and the reset's deadline more than a day off"
            }
            KeepWhy::Focus => "the cursor was not on the confirmation's yes",
            KeepWhy::Claim => "the reset was no longer this session's to take",
            KeepWhy::NotOpened => "pressing the reset's row opened nothing",
            KeepWhy::CheckTimeout => "the check for a reset took longer than it may",
            KeepWhy::EnterNotTaken => "the confirmation stayed up after its Enter",
            KeepWhy::Successor => {
                "a later loop found the confirmation, and only the loop that opened one enters it"
            }
            KeepWhy::Closed => "the confirmation was gone before it was read",
        };
        Some(words.to_string())
    }
}

/// What is done at a confirmation ([`confirm_verdict`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Enter, on the yes row as read.
    Enter,
    /// Escape, and why.
    Keep(KeepWhy),
}

/// Verdict on a reset confirmation: [`Verdict::Enter`] on [`ConfirmRead::Exact`] within
/// [`OFFER_EXPIRING_S`], else [`Verdict::Keep`] ([`KeepWhy::Inexact`], [`KeepWhy::Closed`]).
#[must_use]
pub fn confirm_verdict(
    read: &ConfirmRead,
    go: &ResetGo,
    deadline: Option<i64>,
    now: i64,
) -> Verdict {
    let confirm = match read {
        ConfirmRead::Exact(confirm) => confirm,
        ConfirmRead::Inexact(part) => return Verdict::Keep(KeepWhy::Inexact(part)),
        ConfirmRead::Not => return Verdict::Keep(KeepWhy::Closed),
    };
    if confirm.kind != go.kind {
        return Verdict::Keep(KeepWhy::Kind);
    }
    if !confirm.limits.iter().any(|name| go.limits.contains(name)) {
        return Verdict::Keep(KeepWhy::OtherLimit);
    }
    if confirm.left != go.left || confirm.left < 1 {
        return Verdict::Keep(KeepWhy::Count);
    }
    let on_the_day = !go.day.is_empty()
        && confirm
            .deadline
            .strip_prefix(go.day.as_str())
            .and_then(|rest| rest.strip_prefix(" at "))
            .is_some_and(|hour| !hour.is_empty());
    if !on_the_day {
        return Verdict::Keep(KeepWhy::Day);
    }
    if !go.long {
        match deadline.map(|at| at.saturating_sub(now)) {
            Some(ahead) if ahead > OFFER_EXPIRING_S => {
                return Verdict::Keep(KeepWhy::NotQualifying);
            }
            Some(ahead) if ahead > 0 => {}
            _ => return Verdict::Keep(KeepWhy::Deadline),
        }
    }
    if !confirm.yes_focused {
        return Verdict::Keep(KeepWhy::Focus);
    }
    Verdict::Enter
}

/// What the loop tells its host when the step ends (`IdleHost::reset_told`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetEnd {
    /// The confirmation's Enter was written.
    pub entered: bool,
    /// Why it was kept, where the loop kept it.
    pub kept: Option<KeepWhy>,
    /// What a confirmation that read exactly offered: the one source of an offer's record.
    pub offer: Option<ResetOffer>,
    /// The loop will not ask again.
    pub last: bool,
}

/// AN OFFER OF ONE KIND ([`ResetEnd::offer`], and what the host records).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetOffer {
    /// The kind counted.
    pub kind: ResetKind,
    /// The resets of that kind left.
    pub left: u32,
    /// The placed deadline (unix seconds) to use it by, where one was read.
    pub use_by: Option<i64>,
}

/// What the host read of the step's end (`IdleHost::reset_told`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetOutcome {
    /// No answer line yet: asked again.
    Pending,
    /// A reset of `kind` was spent and the limits are refilled.
    Spent {
        kind: ResetKind,
        asks: bool,
        left: Option<u32>,
        cleared_at: i64,
    },
    /// A reset of `kind` was spent while the walled limit still stands ([`Self::Spent`]).
    Walled { kind: ResetKind, left: u32 },
    /// The wall lifted by itself with nothing spent (fields as for [`Self::Spent`]).
    Lifted { asks: bool, cleared_at: i64 },
    /// The reset was kept.
    Kept,
    /// The vendor answered with a failure; `login`: for this login.
    Failed { login: bool },
    /// No line was read, or the claim was not this session's to tell of.
    Unread,
}

impl ResetOutcome {
    /// The outcome's one word, as the journal and the session ledger name it.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            ResetOutcome::Pending => "pending",
            ResetOutcome::Spent { .. } | ResetOutcome::Walled { .. } => "used",
            ResetOutcome::Lifted { .. } => "lifted",
            ResetOutcome::Kept => "kept",
            ResetOutcome::Failed { .. } => "failed",
            ResetOutcome::Unread => "unread",
        }
    }
}

#[cfg(test)]
#[path = "reset_tests.rs"]
mod tests;
