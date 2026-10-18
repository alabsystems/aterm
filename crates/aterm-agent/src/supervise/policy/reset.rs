// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A limit reset, as the loop, its host and the policies speak of one
//! (`docs/SPEC-claude-accounts-resets-2026-10-02.md`): what the loop asks its
//! host before the grant row's first key ([`ResetAsk`]) and what the host
//! answers ([`ResetWord`]: the go, a moment's wait, or the one word why not);
//! the verdict on the confirmation the row opens ([`confirm_verdict`]); what
//! the loop tells its host when the step ends ([`ResetEnd`]) and what the host
//! read of it ([`ResetOutcome`]).
//!
//! A reset is the owner's scarce purchase, so everything here refuses by
//! default: [`confirm_verdict`] says Enter only where every condition holds,
//! and each that does not has its own word ([`KeepWhy`]), which the ledger's
//! `reset-kept` row carries. Pure: nothing here reads a screen, a clock or a
//! file, and nothing types.
//!
//! None of it has been seen live: its reader's texts (`aterm_phase::reset`)
//! come from Claude Code's 2.1.287 bundle, never a screen, and `[harness]
//! limit_reset` defaults off.

use std::fmt;
use std::time::Duration;

use aterm_phase::ConfirmRead;

use crate::harness::accounts::OFFER_EXPIRING_S;

/// How long the vendor's own continuation is given to start after a reset
/// whose answer line asks for nothing (it queued one itself, and starts it at
/// once): the supervisor's continuation is owed only past it
/// (`TurnEndState::reset_told`).
pub const RESET_CONTINUES_WAIT: Duration = Duration::from_secs(60);

/// What the loop asks its host before the grant row's first key
/// (`IdleHost::reset_take`): the row as read (`aterm_phase::ResetRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetAsk {
    /// The resets left, as the row counts them.
    pub left: u32,
    /// The grant's last day as the row draws it (`Oct 5`): a day, not a
    /// deadline. Handed back in the go ([`ResetGo::day`]).
    pub day: String,
    /// The first second of that day (unix seconds), placed in the wall
    /// notice's zone where the loop holds one, else the local one; `None`
    /// where the day did not place.
    pub day_start: Option<i64>,
}

impl ResetAsk {
    /// The go for this ask: what only the host knows (`until`, `long`,
    /// `limits`), with the row's own count and day carried over unchanged, so
    /// the confirmation is held to the row that was taken
    /// ([`confirm_verdict`]).
    #[must_use]
    pub fn go(&self, until: i64, long: bool, limits: Vec<String>) -> ResetGo {
        ResetGo {
            until,
            long,
            limits,
            left: self.left,
            day: self.day.clone(),
        }
    }
}

/// The host's go ([`ResetWord::Take`]): the claim is this session's, and what
/// the confirmation must agree with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetGo {
    /// The wall's reset (unix seconds), from this process's own transcript
    /// row: with the account, what the claim is keyed on.
    pub until: i64,
    /// The wall stands more than a day from now: the reset is worth taking
    /// whenever its deadline is.
    pub long: bool,
    /// The names the walled window is drawn by in a confirmation's body
    /// (`session`, `weekly`, `Opus`, `Sonnet`): one of them must be among the
    /// limits the body says it refills.
    pub limits: Vec<String>,
    /// The resets left as the row counted them ([`ResetAsk::left`]).
    pub left: u32,
    /// The grant's last day as the row drew it ([`ResetAsk::day`]).
    pub day: String,
}

/// Why the host did not give the go ([`ResetWord::No`]): one stable word each
/// ([`Self::word`]), for the loop's one `SKIPPED` line per dialog mount. None
/// is tried again at that mount, and none spends anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetNo {
    /// `[harness] limit_reset` is off, read live.
    Off,
    /// No host answers here (the trait's default).
    NoHost,
    /// The host could not read what it judges by: no Claude Code in the tab,
    /// no seat, a process whose login did not read.
    Unread,
    /// The process is not on the default login: it names a login directory,
    /// its login is not the stored one in the default home, or its roster row
    /// has a directory.
    NotDefaultLogin,
    /// Another login is in the way: the roster's active row is another one,
    /// or another row's account ran within the vendor's profile window.
    OtherLogin,
    /// The last quota rejection in the transcript is not this process's.
    NotOwnWall,
    /// The wall names no reset that is believed and still ahead.
    NoReset,
    /// The walled window is not one a grant clears.
    Window,
    /// Neither a wall that stands more than a day nor a grant whose last day
    /// could hold a deadline within one.
    NotQualifying,
    /// Another session's claim for this wall is in flight.
    Held,
    /// This wall has had its one attempt.
    Attempted,
    /// The accounts store did not admit: a refused roster, a state that could
    /// not be written.
    State,
}

impl ResetNo {
    /// The refusal's one word: lower case, no space, no account's name.
    #[must_use]
    pub fn word(self) -> &'static str {
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
        }
    }

    /// Every refusal, in the order [`Self::word`] lists them.
    pub const ALL: [ResetNo; 12] = [
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
    ];

    /// [`Self::word`] read back (a harness tape's host answer,
    /// `supervise::tape::host`); `None` for a word it never gives.
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
    /// Not yet answerable (the wall's row is not written, the store's lock is
    /// held): ask again in a moment.
    Again,
    /// No, and why.
    No(ResetNo),
}

/// Why a reset was kept: the confirmation was not entered (it is Escaped), or
/// the step ended before one stood. One stable word each ([`fmt::Display`]),
/// the reason of the ledger's `reset-kept` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepWhy {
    /// The confirmation did not read exactly, and the first part that did
    /// not (`aterm_phase::ConfirmRead::Inexact`: `title`, `options`, `early`,
    /// `body`, `week`, `count`, `deadline`).
    Inexact(&'static str),
    /// Its body names none of the limits the wall is drawn by.
    OtherLimit,
    /// Its count is not the row's.
    Count,
    /// Its deadline's day is not the row's day.
    Day,
    /// The wall is not a long one, and its deadline did not place or is not
    /// ahead.
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
    /// A loop that did not take the row found the confirmation: it never
    /// enters one.
    Successor,
    /// The confirmation was gone before it was judged.
    Closed,
}

impl fmt::Display for KeepWhy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = match self {
            KeepWhy::Inexact(part) => return write!(f, "inexact-{part}"),
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

/// What is done at a confirmation ([`confirm_verdict`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Enter, on the yes row as read.
    Enter,
    /// Escape, and why.
    Keep(KeepWhy),
}

/// The verdict on a reset confirmation: [`Verdict::Enter`] only where all of
/// these hold, checked in this order; the first that does not is the
/// [`Verdict::Keep`]:
///
/// 1. `read` is [`ConfirmRead::Exact`] ([`KeepWhy::Inexact`] with the
///    reader's word; [`KeepWhy::Closed`] for a box that is no confirmation);
/// 2. a limit its body names is one the wall is drawn by (`go.limits`,
///    compared as drawn) — [`KeepWhy::OtherLimit`];
/// 3. its count is the row's, and one at least — [`KeepWhy::Count`];
/// 4. its deadline's text is the row's day followed by ` at ` and an hour —
///    [`KeepWhy::Day`];
/// 5. the wall is a long one (`go.long`), or the deadline is placed
///    (`deadline`, unix seconds), ahead of `now` and no more than
///    [`OFFER_EXPIRING_S`] away — [`KeepWhy::Deadline`] where it is not
///    placed or not ahead, [`KeepWhy::NotQualifying`] where it is further;
/// 6. the cursor is on the yes — [`KeepWhy::Focus`].
///
/// `deadline` is the confirmation's own day and hour as the loop placed it;
/// a long wall does not consult it.
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
    /// The count and the placed deadline (unix seconds) of a confirmation
    /// that read exactly: the one source of an offer's record.
    pub offer: Option<(u32, Option<i64>)>,
    /// The loop will not ask again: with no answer line read, the host
    /// answers what stands without one.
    pub last: bool,
}

/// What the host read of the step's end (`IdleHost::reset_told`): the
/// vendor's one answer line, from the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetOutcome {
    /// No answer line yet: asked again.
    Pending,
    /// The reset was spent and the limits are refilled. `asks`: the line asks
    /// for a message to continue (the vendor queued none of its own); `left`:
    /// the resets left as the line counts them; `cleared_at`: the unix second
    /// the account's walls were cleared at.
    Spent {
        asks: bool,
        left: Option<u32>,
        cleared_at: i64,
    },
    /// The wall had lifted by itself and nothing was spent; `asks` and
    /// `cleared_at` as for [`Self::Spent`].
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
    pub fn word(&self) -> &'static str {
        match self {
            ResetOutcome::Pending => "pending",
            ResetOutcome::Spent { .. } => "used",
            ResetOutcome::Lifted { .. } => "lifted",
            ResetOutcome::Kept => "kept",
            ResetOutcome::Failed { .. } => "failed",
            ResetOutcome::Unread => "unread",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::reset::fixtures as composed;
    use aterm_phase::{ResetConfirm, reset_confirm};

    /// The confirmation on one of the reader's composed screens (never
    /// captured: each fixture's first line says so), read as the loop reads it.
    fn read(fixture: &str) -> ConfirmRead {
        let rows = screen(fixture);
        let reading = aterm_phase::read(Some("claude"), &rows, None);
        let prompt = reading.prompt.as_ref().expect("the confirmation is a box");
        reset_confirm(prompt, &rows)
    }

    fn exact(fixture: &str) -> ResetConfirm {
        match read(fixture) {
            ConfirmRead::Exact(confirm) => confirm,
            other => panic!("not exact: {other:?}"),
        }
    }

    /// The row the composed confirmation follows (`Use your reset now (2
    /// left, until Oct 5)`), taken at a weekly wall that stands for days.
    fn go() -> ResetGo {
        ResetAsk {
            left: 2,
            day: "Oct 5".to_string(),
            day_start: None,
        }
        .go(1_800_000_000, true, vec!["weekly".to_string()])
    }

    const NOW: i64 = 1_790_000_000;
    const HOUR: i64 = 3600;

    /// Enter at the exact confirmation of the row taken, at a long wall,
    /// whatever its deadline; and each [`KeepWhy`] the verdict can give, by one
    /// thing changed from that case.
    #[test]
    fn enter_only_at_the_exact_confirmation_of_the_row_taken() {
        let confirm = exact(composed::CONFIRM);
        assert_eq!(
            (
                confirm.limits.as_slice(),
                confirm.left,
                confirm.deadline.as_str()
            ),
            (["weekly".to_string()].as_slice(), 2, "Oct 5 at 3pm")
        );
        let read_ok = ConfirmRead::Exact(confirm.clone());
        for deadline in [None, Some(NOW - HOUR), Some(NOW + 300 * HOUR)] {
            assert_eq!(
                confirm_verdict(&read_ok, &go(), deadline, NOW),
                Verdict::Enter
            );
        }
        // The 60-column wrap reads the same confirmation.
        assert_eq!(
            confirm_verdict(&read(composed::CONFIRM_60), &go(), None, NOW),
            Verdict::Enter
        );
        // A wall drawn by either of two names, and a body that refills two.
        let either = ResetGo {
            limits: vec!["Sonnet".to_string(), "weekly".to_string()],
            ..go()
        };
        assert_eq!(
            confirm_verdict(&read_ok, &either, None, NOW),
            Verdict::Enter
        );
        let both = ConfirmRead::Exact(ResetConfirm {
            limits: vec!["session".to_string(), "weekly".to_string()],
            ..confirm.clone()
        });
        assert_eq!(confirm_verdict(&both, &go(), None, NOW), Verdict::Enter);

        let with_go = |edit: &dyn Fn(&mut ResetGo)| {
            let mut go = go();
            edit(&mut go);
            go
        };
        let with_read = |edit: &dyn Fn(&mut ResetConfirm)| {
            let mut c = confirm.clone();
            edit(&mut c);
            ConfirmRead::Exact(c)
        };
        let rows: Vec<(&str, ConfirmRead, ResetGo, Option<i64>, KeepWhy)> = vec![
            (
                "the early line (the reader's composed screen)",
                read(composed::CONFIRM_EARLY),
                go(),
                None,
                KeepWhy::Inexact("early"),
            ),
            (
                "a title that did not read",
                ConfirmRead::Inexact("title"),
                go(),
                None,
                KeepWhy::Inexact("title"),
            ),
            (
                "no confirmation at all",
                ConfirmRead::Not,
                go(),
                None,
                KeepWhy::Closed,
            ),
            (
                "the wall is the session's, the body refills the week",
                read_ok.clone(),
                with_go(&|g| g.limits = vec!["session".to_string()]),
                None,
                KeepWhy::OtherLimit,
            ),
            (
                "the wall's name in another case",
                read_ok.clone(),
                with_go(&|g| g.limits = vec!["Weekly".to_string()]),
                None,
                KeepWhy::OtherLimit,
            ),
            (
                "a wall drawn by no name",
                read_ok.clone(),
                with_go(&|g| g.limits = Vec::new()),
                None,
                KeepWhy::OtherLimit,
            ),
            (
                "the row counted one",
                read_ok.clone(),
                with_go(&|g| g.left = 1),
                None,
                KeepWhy::Count,
            ),
            (
                "the confirmation counts three",
                with_read(&|c| c.left = 3),
                go(),
                None,
                KeepWhy::Count,
            ),
            (
                "the row's day is the day after",
                read_ok.clone(),
                with_go(&|g| g.day = "Oct 6".to_string()),
                None,
                KeepWhy::Day,
            ),
            (
                "the deadline's day only opens with the row's",
                with_read(&|c| c.deadline = "Oct 15 at 3pm".to_string()),
                with_go(&|g| g.day = "Oct 1".to_string()),
                None,
                KeepWhy::Day,
            ),
            (
                "the deadline is in another year",
                with_read(&|c| c.deadline = "Oct 5, 2027 at 3pm".to_string()),
                go(),
                None,
                KeepWhy::Day,
            ),
            (
                "the row's day did not read",
                read_ok.clone(),
                with_go(&|g| g.day = String::new()),
                None,
                KeepWhy::Day,
            ),
            (
                "a short wall, the deadline not placed",
                read_ok.clone(),
                with_go(&|g| g.long = false),
                None,
                KeepWhy::Deadline,
            ),
            (
                "a short wall, the deadline two days off",
                read_ok.clone(),
                with_go(&|g| g.long = false),
                Some(NOW + 48 * HOUR),
                KeepWhy::NotQualifying,
            ),
            (
                "the cursor on the no (the reader's composed screen)",
                read(composed::CONFIRM_FOCUS_NO),
                go(),
                None,
                KeepWhy::Focus,
            ),
        ];
        for (what, read, go, deadline, why) in &rows {
            assert_eq!(
                confirm_verdict(read, go, *deadline, NOW),
                Verdict::Keep(*why),
                "{what}"
            );
        }
        // Both counts nought: no reset is left to use.
        let none_left = with_read(&|c| c.left = 0);
        assert_eq!(
            confirm_verdict(&none_left, &with_go(&|g| g.left = 0), None, NOW),
            Verdict::Keep(KeepWhy::Count)
        );
        // Where several do not hold, the first is the word.
        let other_and_unfocused = with_read(&|c| {
            c.limits = vec!["session".to_string()];
            c.yes_focused = false;
        });
        assert_eq!(
            confirm_verdict(&other_and_unfocused, &go(), None, NOW),
            Verdict::Keep(KeepWhy::OtherLimit)
        );
        let short_and_unfocused = with_read(&|c| c.yes_focused = false);
        assert_eq!(
            confirm_verdict(
                &short_and_unfocused,
                &with_go(&|g| g.long = false),
                None,
                NOW
            ),
            Verdict::Keep(KeepWhy::Deadline)
        );
    }

    /// At a wall that is not long, a deadline within a day (to the second) is
    /// entered; one a minute past it, one not placed, one now and one in the
    /// past are kept. Negative control: at a long wall each is entered.
    #[test]
    fn a_short_wall_is_reset_only_where_the_offer_expires_within_a_day() {
        let read = read(composed::CONFIRM);
        let short = ResetGo {
            long: false,
            ..go()
        };
        let keep = |why| Verdict::Keep(why);
        for (what, deadline, verdict) in [
            ("23 h 59 m", Some(NOW + 24 * HOUR - 60), Verdict::Enter),
            ("a day to the second", Some(NOW + 24 * HOUR), Verdict::Enter),
            ("a second ahead", Some(NOW + 1), Verdict::Enter),
            (
                "24 h 1 m",
                Some(NOW + 24 * HOUR + 60),
                keep(KeepWhy::NotQualifying),
            ),
            (
                "a day and a second",
                Some(NOW + 24 * HOUR + 1),
                keep(KeepWhy::NotQualifying),
            ),
            ("now", Some(NOW), keep(KeepWhy::Deadline)),
            ("an hour ago", Some(NOW - HOUR), keep(KeepWhy::Deadline)),
            ("the far past", Some(i64::MIN), keep(KeepWhy::Deadline)),
            ("not placed", None, keep(KeepWhy::Deadline)),
        ] {
            assert_eq!(
                confirm_verdict(&read, &short, deadline, NOW),
                verdict,
                "{what}"
            );
            assert_eq!(
                confirm_verdict(&read, &go(), deadline, NOW),
                Verdict::Enter,
                "{what}, at a long wall"
            );
        }
    }

    /// The trait's defaults are the refusal and `Unread`, so a host that keeps
    /// no accounts never has the grant row pressed.
    #[test]
    fn a_host_that_answers_nothing_takes_no_reset() {
        use crate::supervise::{HostStep, IdleHost};
        #[derive(Debug)]
        struct Silent;
        impl IdleHost for Silent {
            fn wants(&self) -> bool {
                false
            }
            fn at_idle(&self) -> Option<HostStep> {
                None
            }
            fn owns_turn_end(&self) -> bool {
                false
            }
        }
        let ask = ResetAsk {
            left: 2,
            day: "Oct 5".to_string(),
            day_start: Some(NOW),
        };
        let ends = [true, false].map(|entered| ResetEnd {
            entered,
            kept: (!entered).then_some(KeepWhy::Focus),
            offer: Some((2, Some(NOW + HOUR))),
            last: !entered,
        });
        assert_eq!(Silent.reset_take(&ask), ResetWord::No(ResetNo::NoHost));
        for end in &ends {
            assert_eq!(Silent.reset_told(end), ResetOutcome::Unread);
        }
    }
}
