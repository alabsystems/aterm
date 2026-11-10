// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure supervisor turn-end policy decider (`TurnEndState`, `TurnEndReading`, `TurnEndDecision`).
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

use aterm_phase::phase::{Phase, is_done_row, last_said_row, status_line};
use aterm_phase::wall::WallKind;
use aterm_phase::{ApiCause, Program};

use super::super::config::{Autopilot, SupervisorConfig};
use super::super::limit::ResetClock;
use super::approval::PriorityWord;
use super::exception::FUTILE_AFTER_TRIES;
use super::go_ahead::{self, RULE_GO_AHEAD};
use super::land_check;
use super::login::LoginShared;
use super::needs::{self, NEEDS_ACK, NEEDS_HINT};
use super::reset::{RESET_CONTINUES_WAIT, ResetOutcome};
use super::sleeps::InTurn;
use super::trace::{NoTrace, Outcome, Trace, note};
use crate::harness::acts::{HoldLeg, LandHeld, OpenWord, TaskFace};
use crate::harness::harness_mark;
use crate::harness::hold_said::HoldSaid;

/// A continuation typed at an ordinary turn end ([`decide_turn_end`]).
pub const RULE_CONTINUE: &str = "continue@v1";

/// Recovery from a repeated report or a refused routine decision.
pub const FORWARD_RECOVERY: &str = concat!(
    harness_mark!(),
    " Continue autonomously. This approach is not making progress. \
    Decide on a different safe approach and record the decision for another AI to revisit. \
    If this work cannot proceed, defer it with the reason and advance another useful task. \
    Do not ask the person to unblock routine work. Keep moving."
);
/// Why Cruise waits before [`FORWARD_RECOVERY`] at a repeated report: the words ending its
/// `WAITING` line. The wait is the short back-off ladder's step for the streak its point closed
/// ([`TurnEndState::short_wait`]), which these words do not name, so a reader of the ladder counts
/// it as the step after the one before.
pub const REPEAT_RECOVERY_WAIT: &str = "repeated report; changing approach on the next turn";
/// The [`DONE_CHECK`] typed under Cruise where the worker claims it is done.
pub const RULE_DONE_CHECK: &str = "continue-done-check@v1";
/// RETIRED: the open-items challenge older builds typed where a done report named open work ([`OPEN_ITEMS_HEAD`], [`OPEN_ITEMS_HEADS_BEFORE`]).
pub const RULE_OPEN_ITEMS: &str = "continue-open-items@v1";
/// Retired rule tag for the legacy uncommitted-work continuation challenge.
pub const RULE_UNCOMMITTED: &str = "continue-uncommitted@v1";

/// Whether `rule` is one of the retired challenges ([`RULE_OPEN_ITEMS`], [`RULE_UNCOMMITTED`]).
#[must_use]
pub fn is_challenge_rule(rule: &str) -> bool {
    rule == RULE_OPEN_ITEMS || rule == RULE_UNCOMMITTED
}
/// The journal's name for a person's own exchange left to them ([`TurnEndState::left_to_person`]).
pub const RULE_LEFT: &str = "left-to-person@v1";
/// Why the loop keeps its hands off a session a person typed into within their grace: the reason
/// of the policy's hold for them, and the words that end the `WAITING` line the loop journals for
/// it.
pub const PERSON_TYPING: &str = "a person is typing";
/// CRUISE'S DONE CHECK, ITS SHARED BASE: every act typed under [`RULE_DONE_CHECK`] opens with
/// it, then one of [`DONE_ANGLES`] in turn and [`DONE_TAIL`] ([`done_check_words`]), so every
/// reader keyed on the check's text ([`is_done_check`], the ledger's and the upgrade's) reads each
/// angle, and v1's whole text ([`DONE_CHECK_FIXED_BEFORE`]) too, by this prefix.
pub const DONE_CHECK: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: done is never the end."
);

/// CRUISE'S SIX KINDS OF WORK, one asked at each done claim in turn ([`TurnEndState::checks`]):
/// the widest check, a hostile review, the deferred work, a measurement, the next issue, and the
/// session's own leftovers. Each names only the agent's own work: none says delete, remove, reset,
/// retire, drop or prune, and the sixth leaves every worktree, branch and stash the agent did not
/// make exactly as it is.
pub const DONE_ANGLES: [&str; NO_WORK_ACTS] = [
    "This time, run the widest check this repository has on the merged tree and fix what it finds.",
    "This time, review the diff you landed as a hostile reviewer would: edge cases, error paths, \
     missing tests. Fix what you find.",
    "This time, do what your reports called out of scope, later or a follow-up.",
    "This time, measure what you changed against the main it started from and fix any regression.",
    "This time, take the next open issue or TODO beside your work.",
    "This time, land every worktree, branch and stash you made in this session that no remote \
     main has. Leave every one you did not make exactly as it is.",
];

/// Where [`TurnEndState::checks`] counts round: a whole number of turns of [`DONE_ANGLES`] (42 of
/// them). The count after it is [`CHECKS_ROUND`], never 0, so the angles go on in order across it
/// and a count above 0 always names the angle typed last ([`next_checks`]).
const CHECKS_WRAP: u8 = 252;
const _: () = assert!((CHECKS_WRAP as usize).is_multiple_of(DONE_ANGLES.len()));
/// One whole turn of [`DONE_ANGLES`]: the count a wrap goes back to, and the first a seeded count
/// at or past [`CHECKS_WRAP`] (one no loop writes) is folded onto ([`TurnEndState::seed_checks`]).
const CHECKS_ROUND: u8 = 6;
const _: () = assert!(CHECKS_ROUND as usize == DONE_ANGLES.len());

/// The count once one more check is typed after `checks`: the next, or [`CHECKS_ROUND`] at the
/// wrap, so the angle typed last is still `count - 1`'s.
const fn next_checks(checks: u8) -> u8 {
    match checks.checked_add(1) {
        Some(n) if n < CHECKS_WRAP => n,
        _ => CHECKS_ROUND,
    }
}

/// What follows each of [`DONE_ANGLES`]: record decisions and continue; the agent's reversible work is
/// never one ([`go_ahead::NEVER_A_NEED`], owner 2026-10-29: "how dare you block on me for go
/// ahead!"), in the continuation's words, which every done check typed within the person's hour
/// leaves out as the continuation does ([`go_ahead::unless_held`]); and the defense the owner
/// asked for (2026-10-19) with what it ran, then the hunt goes on.
pub const DONE_TAIL: &str = concat!(
    "Decide, log why for later AI review, and keep moving; defer unavailable work. ",
    go_ahead::never_a_need!(),
    " If this turns up nothing, say what you ran and what it showed in a few lines starting \
     Nothing left:, then look for another kind of gap and close it."
);

/// RETIRED: v1's one fixed done check (until 5a, v6 §14.5), which every done claim got word for
/// word. It opens with [`DONE_CHECK`], so the base's own rule reads it with its suffixes; it is not
/// one of [`RETIRED_DONE_CHECKS`], whose stricter tail would drop them.
pub const DONE_CHECK_FIXED_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: done is never the end. Land every open item, then look harder: \
     review your own work adversarially, widen the checks, fix what they find, close every \
     gap. Put what only a person can give on a line starting NEEDS YOU: and carry on. If you \
     are certain nothing is left, defend it in a few lines starting Nothing left: and keep \
     looking."
);

/// The done check's words at its `n`th claim (`n` counts from 0; the seventh is the first again):
/// [`DONE_CHECK`], the angle, [`DONE_TAIL`], before any suffix or the standing rules. Whole, as
/// typed outside the person's hour: a caller that types them within it cuts
/// [`go_ahead::NEVER_A_NEED`] ([`go_ahead::unless_held`]).
#[must_use]
pub fn done_check_words(n: usize) -> String {
    format!(
        "{DONE_CHECK} {} {DONE_TAIL}",
        DONE_ANGLES[n % DONE_ANGLES.len()]
    )
}

/// RETIRED: the done check of `4bff4848b4`.
pub const DONE_CHECK_COMMITTED_BEFORE: &str = "Before you stop: do every open item, skipped \
     check, follow-up and next step, and verify it (build, test, measure where numbers were \
     asked for), then commit and push it (merged to main where this repository lands work). \
     Report whether the task is complete, the outcome and its verification in ordinary \
     language. It is complete only when nothing of the job is left: nothing uncommitted, \
     untracked, stashed or unpushed, and no open item, follow-up or next step in your report, \
     a blocked one included. Otherwise continue.";

/// Retired ordinary-language done-check prompt from `69a8f68178`.
pub const DONE_CHECK_ORDINARY_BEFORE: &str = "Before you stop: do every open item, \
     skipped check, follow-up and next step you can still do yourself now, and verify it \
     (build, test, measure where numbers were asked for, commit and land where this repository \
     lands work). Report whether the task is complete, the outcome, verification, and any \
     remaining blockers in ordinary language. Stop only when everything left needs a person (a \
     credential, a decision, physical access) or cannot be done on this host, each item named \
     with its blocker; a slow check is not a blocker. Otherwise continue.";

/// RETIRED: the done check of `0b1abdabf3`.
pub const DONE_CHECK_WORDS_BEFORE: &str = "Before you stop: do every open item, skipped check, \
     follow-up and next step you can still do yourself now, and verify it (build, test, measure \
     where numbers were asked for, commit and land where this repository lands work). Close \
     your reply by saying the work is complete, only when everything left needs a person (a \
     credential, a decision, physical access) or cannot be done on this host, each item named \
     with its blocker; a slow check is not a blocker. Otherwise continue.";

/// RETIRED: the last completion check that asked for the token, retained only for reading existing conversations and their task results.
pub const DONE_CHECK_MARKER_BEFORE: &str = "Before you stop: do every open item, skipped \
     check, follow-up and next step you can still do yourself now, and verify it (build, test, \
     measure where numbers were asked for, commit and land where this repository lands \
     work). End your reply with ATERM-TASK-DONE on a line by itself only when everything \
     left needs a person (a credential, a decision, physical access) or cannot be done on \
     this host, each item named with its blocker; a slow check is not a blocker. Otherwise \
     continue.";

/// RETIRED: the primer's definition of finished as main's check asked it before the merge of `f09ef3b15` asked for the token LAST.
pub const DONE_CHECK_OPEN_BEFORE: &str = "Before you stop: do every open item, skipped check, \
     follow-up and next step you can still do yourself now, and verify it (build, test, \
     measure where numbers were asked for, commit and land where this repository lands \
     work). Reply with ATERM-TASK-DONE on a line by itself only when everything left needs \
     a person (a credential, a decision, physical access) or cannot be done on this host, \
     each item named with its blocker; a slow check is not a blocker. Otherwise continue.";

/// RETIRED: the done check's words that asked for the token LAST until the check took the primer's definition of finished.
pub const DONE_CHECK_LAST_BEFORE: &str = "If the task is finished and verified, end your reply \
     with ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";

/// RETIRED: the done check's words from `3e34e2f6d` until the check asked for the token as the reply's LAST line.
pub const DONE_CHECK_TOKEN_BEFORE: &str = "If the task is finished and verified, reply with \
     ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";

/// RETIRED: the done check main's tip typed from `d92d817dc` until the check took the primer's definition of finished.
pub const DONE_CHECK_NOTHING_OPEN_BEFORE: &str = "If the task is finished, verified and has \
     nothing left open, reply with ATERM-TASK-DONE on a line by itself and stop; otherwise \
     continue with what is open.";

/// RETIRED: the done check's words before `3e34e2f6d`.
pub const DONE_CHECK_BEFORE: &str =
    "If the task is finished and verified, reply DONE and stop; otherwise continue.";

/// EVERY DONE CHECK AN OLDER BUILD TYPED, newest first.
pub const RETIRED_DONE_CHECKS: [&str; 9] = [
    DONE_CHECK_COMMITTED_BEFORE,
    DONE_CHECK_ORDINARY_BEFORE,
    DONE_CHECK_WORDS_BEFORE,
    DONE_CHECK_MARKER_BEFORE,
    DONE_CHECK_OPEN_BEFORE,
    DONE_CHECK_NOTHING_OPEN_BEFORE,
    DONE_CHECK_LAST_BEFORE,
    DONE_CHECK_TOKEN_BEFORE,
    DONE_CHECK_BEFORE,
];

/// What follows [`DONE_CHECK`] where the done report also names an irreversible act ([`Said::Irreversible`], D1).
pub const DONE_CHECK_REVERSIBLE: &str =
    "Approve nothing irreversible: delete, overwrite and force-push nothing.";

/// THE RETIRED DONE TOKEN.
pub const FINISHED: &str = "ATERM-TASK-DONE";

/// Managed short-turn allowance before short-turn backoff applies after a done check.
pub const MANAGED_SHORT_TURNS: u32 = 3;

/// THE MARK OF THE WORKER'S LAST WORDS (`tail`, [`TurnEndReading::said_tail`] as its program's reader reads them).
#[must_use]
pub fn said_mark(tail: Option<&str>) -> Option<u64> {
    let letters = said_letters(tail);
    (!letters.is_empty()).then(|| aterm_hash::fnv1a_64(letters.as_bytes()))
}

fn said_letters(tail: Option<&str>) -> String {
    let mut letters = String::new();
    letters.extend(
        tail.unwrap_or_default()
            .chars()
            .filter(|c| c.is_alphanumeric()),
    );
    letters
}

/// THE REPLY A TURN SAID ([`TurnEndState::repeats`]): its words, or a read wall's kind and words, a word with a digit one number, a clock's `AM` or `PM` after it included.
fn reply_mark(reading: &TurnEndReading) -> u64 {
    mark_of(reading, reading.wall)
}

/// The turn's words exactly, numbers kept and only the spacing folded: a blocked reply holds
/// only when they come again ([`blocked_reply`]), since an agent that reports a new count beside
/// the same need worked between.
fn exact_mark(reading: &TurnEndReading) -> u64 {
    let words: Vec<&str> = [reading.turn_said.as_deref(), reading.said_tail.as_deref()]
        .into_iter()
        .flatten()
        .flat_map(str::split_whitespace)
        .collect();
    aterm_hash::fnv1a_64(words.join(" ").as_bytes())
}

/// The turn's words alone, whatever wall it shows: a turn of the program's own changes them, a wall's notice leaving does not.
fn words_mark(reading: &TurnEndReading) -> u64 {
    mark_of(reading, None)
}

fn mark_of(reading: &TurnEndReading, wall: Option<WallKind>) -> u64 {
    let mut letters = String::new();
    let words: [Option<&str>; 2] = match wall {
        Some(kind) => {
            letters.push_str(kind.name());
            [Some(&reading.wall_message), None]
        }
        None => [reading.turn_said.as_deref(), reading.said_tail.as_deref()],
    };
    let mut number = false;
    for word in words.into_iter().flatten().flat_map(str::split_whitespace) {
        let bare: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if word.contains(char::is_numeric) {
            letters.push('#');
            number = true;
        } else if number && (bare.eq_ignore_ascii_case("am") || bare.eq_ignore_ascii_case("pm")) {
            number = false;
        } else {
            letters.push_str(&bare);
            number = false;
        }
    }
    aterm_hash::fnv1a_64(letters.as_bytes())
}

/// HOW A MANAGED WORKER'S WORK ENDED UNFINISHED while its agent stands ([`TurnEndState::gave_up`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaveUp {
    /// Its reply said `Could not finish: <why>` ([`needs::could_not_finish`]).
    CouldNotFinish,
    /// The same reply came back with no work and Land's stop for that took the point
    /// ([`TurnEndState::conclude_repeated`]).
    Repeated,
    /// A usage-limit failure recorded by an earlier harness. New walls wait for quota.
    UsageLimit,
}

impl GaveUp {
    /// Every ending, in the order [`Self::of_word`] and [`Self::of_journal_line`] try them: the
    /// `why=` words a task record's `failed` mark must take ([`Self::word`]).
    pub const ALL: [Self; 3] = [Self::CouldNotFinish, Self::Repeated, Self::UsageLimit];

    /// The task record's `why=` for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::CouldNotFinish => "could-not-finish",
            Self::Repeated => "repeated",
            Self::UsageLimit => "usage-limit",
        }
    }

    /// The ending a record's `why=` names; `None` for any other word.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|ending| ending.word() == word)
    }

    /// What the loop's journal says of it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::CouldNotFinish => "the worker said it could not finish",
            Self::Repeated => "the worker repeated the same reply with no work",
            Self::UsageLimit => {
                "the worker's account is at a usage limit that resets too far off to wait for, \
                 and no move off it is made"
            }
        }
    }

    /// The ending a journal's `FAILED` line names, by its [`Self::words`].
    #[must_use]
    pub fn of_journal_line(line: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|ending| line.contains(ending.words()))
    }
}

/// The decider's check that holds an act at the same reply ([`TurnEndState::repeats`]).
pub const SAME_REPLY: &str = "same-reply";

/// The check that changes approach when the standing go-ahead's routine answer budget is spent.
pub const GO_AHEAD_REFUSED: &str = "go-ahead-refused";

/// THE PERSON'S HOUR HOLDS A PARK ([`TurnEndState::go_ahead_hour_ends`]): the check that waits
/// for the hour's end where the decision would type nothing, or back off past it
/// ([`continue_policy`]), so the point is decided again there.
pub const GO_AHEAD_HOUR: &str = "go-ahead-hour";

/// Why a point waits for the end of the person's hour ([`GO_AHEAD_HOUR`]).
pub const GO_AHEAD_HOUR_WHY: &str =
    "a person's key holds the go-ahead for the hour after it; the park is decided again at its end";

/// The books' row of a person's hour that runs from the end of the turn their key came in
/// (`from=<unix ms>`, [`TurnEndState::take_go_ahead_hour`]): the window's clock keeps the key
/// alone, so a loop after a restart reads the hour's start from it
/// ([`TurnEndState::seed_go_ahead_hour`]).
pub const RULE_GO_AHEAD_HOUR: &str = "go-ahead-hour@v1";

/// Why a person's hour is booked ([`RULE_GO_AHEAD_HOUR`]).
pub const GO_AHEAD_HOUR_BOOKED: &str =
    "a person's key came in a turn that ended later; their hour runs from that end";

/// Why a park on a person's go-ahead escalates where answers are off ([`continue_policy`]).
pub const GO_AHEAD_WHY: &str = "the worker parked its own reversible work on a person's go-ahead";

/// Routine go-ahead attempts before switching to forward recovery ([`TurnEndState::go_ahead_spent`]).
pub const GO_AHEAD_ACTS: u32 = 3;

/// How long after its first go-ahead act a park may stand before aterm changes approach: the
/// first since the agent last moved past its park, by real work or a turn a wall cut
/// ([`TurnEndState::go_ahead_worked`], 2026-10-30 review 9).
pub const GO_AHEAD_SPAN: Duration = Duration::from_secs(60 * 60);

/// THE LONGEST BACK-OFF BEFORE A GO-AHEAD, measured from the last act typed: an agent that wakes
/// itself sooner than the short-turn ladder's wait ([`TurnEndState::short_wait`]) restarts no
/// wait the answer owes (the incident's first twenty hours, 2026-10-27). At a break of the
/// agent's own background work the go-ahead rides the carry's ladder, its wait capped the same
/// from the last carry ([`super::cruise_carry::BackgroundCarries::due_within`]).
pub const GO_AHEAD_WAIT: Duration = Duration::from_secs(15 * 60);

/// How long the person's last key holds the go-ahead ([`TurnEndState::go_ahead_held`]), from the
/// key or the end of the turn it came in: the replies after it may restate a hold the person
/// just gave. No word of theirs is read for a hold; one longer than this is the agent's to keep,
/// and the go-ahead says a hold a person gave still stands.
pub const GO_AHEAD_HELD: Duration = Duration::from_secs(60 * 60);

/// How far apart two reads of one key of the person's may time it: each times it back from its
/// own read of `human_typed_ms`, later the longer the loop took to judge it. A key read more than this
/// past the newest one read is a new key ([`TurnEndState::person_keyed`]).
pub const KEY_DRIFT: Duration = Duration::from_secs(30);

/// HOW MANY REPLIES THE SHORT STREAK KEEPS ([`Replies`]).
pub const REPLIES_KEPT: usize = 3;

/// HOW MANY NO-WORK ACTS IN A ROW HOLD WHATEVER THEIR WORDING ([`Replies::run`]): two through each reply the streak keeps, so the same-reply hold always comes first.
pub const NO_WORK_ACTS: usize = 2 * REPLIES_KEPT;

/// ONE REPLY THE SHORT STREAK KEEPS: its mark ([`reply_mark`]), the no-work answers that settled to it, whether a read wall said it, and those answering the wall's own acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kept {
    pub mark: u64,
    pub answers: usize,
    pub walled: bool,
    pub tries: usize,
}

impl Kept {
    /// Two no-work answers settled to it, the reply came again, or a read wall's tries are spent at it ([`TurnEndState::repeats`]).
    #[must_use]
    pub const fn holds(&self) -> bool {
        self.answers >= 2 || (self.walled && self.tries >= FUTILE_AFTER_TRIES)
    }
}

/// How near two readings of one dated reset land: the loop places it on its clock at each read, and another reset names another minute.
const SAME_RESET: Duration = Duration::from_secs(30);

/// The longest the loop leaves a dialog it backed out of to close before its Esc again ([`TurnEndState::login_closing`]): a dialog still drawn after the Esc is pressed again, never handed to a person.
pub const BACK_OUT_PAUSE_MAX: Duration = Duration::from_secs(10 * 60);

/// Whether `at` is the reset `kept` was, read again ([`SAME_RESET`]).
fn same_reset(kept: Option<Instant>, at: Instant) -> bool {
    kept.is_some_and(|k| k.max(at).duration_since(k.min(at)) < SAME_RESET)
}

/// THE REPLIES THE SHORT STREAK KEEPS, the newest [`REPLIES_KEPT`] last, and how many no-work answers came in a row ([`TurnEndState::repeats`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Replies {
    pub kept: [Option<Kept>; REPLIES_KEPT],
    pub run: usize,
}

impl Replies {
    pub const NONE: Self = Self {
        kept: [None; REPLIES_KEPT],
        run: 0,
    };

    #[must_use]
    pub fn last(&self) -> Option<Kept> {
        self.kept.iter().rev().find_map(|k| *k)
    }

    pub fn kept(&self) -> impl Iterator<Item = Kept> + '_ {
        self.kept.iter().filter_map(|k| *k)
    }

    /// The newest reply came again, or [`NO_WORK_ACTS`] no-work answers came in a row.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.held_by().is_some()
    }

    /// WHICH ARM HOLDS (§14.3): the newest kept reply's own ([`Kept::holds`]: it came again, or a
    /// read wall's tries are spent at it) is [`HeldBy::SameReply`], else [`NO_WORK_ACTS`] no-work
    /// answers in a row is [`HeldBy::NoWork`]; `None` where neither holds.
    ///
    /// [`HeldBy::SameReply`]: crate::harness::acts::HeldBy::SameReply
    /// [`HeldBy::NoWork`]: crate::harness::acts::HeldBy::NoWork
    #[must_use]
    pub fn held_by(&self) -> Option<crate::harness::acts::HeldBy> {
        use crate::harness::acts::HeldBy;
        if self.last().is_some_and(|k| k.holds()) {
            Some(HeldBy::SameReply)
        } else if self.run >= NO_WORK_ACTS {
            Some(HeldBy::NoWork)
        } else {
            None
        }
    }

    /// One more answer that settled to `mark` (`own`: to a wall's own act; `idle`: no work): a reply kept already counts it again.
    fn count(&mut self, mark: u64, walled: bool, own: bool, idle: bool) {
        let before = self.kept().find(|k| k.mark == mark);
        let mut kept: Vec<Kept> = self.kept().filter(|k| k.mark != mark).collect();
        kept.push(Kept {
            mark,
            answers: before.map_or(1, |k| k.answers.saturating_add(1)),
            walled,
            tries: before
                .map_or(0, |k| k.tries)
                .saturating_add(usize::from(own)),
        });
        let run = if idle { self.run.saturating_add(1) } else { 0 };
        *self = Self {
            run,
            ..Self::of(&kept)
        };
    }

    fn has(&self, mark: u64) -> bool {
        self.kept().any(|k| k.mark == mark)
    }

    /// The newest [`REPLIES_KEPT`] of `kept`.
    #[must_use]
    pub fn of(kept: &[Kept]) -> Self {
        let mut out = Self::default();
        for (slot, k) in out
            .kept
            .iter_mut()
            .zip(&kept[kept.len().saturating_sub(REPLIES_KEPT)..])
        {
            *slot = Some(*k);
        }
        out
    }
}

fn words_end_with(tail: Option<&str>, mark: Option<u64>, len: usize) -> bool {
    let Some(mark) = mark else {
        return false;
    };
    let letters = said_letters(tail);
    letters
        .len()
        .checked_sub(len)
        .and_then(|from| letters.get(from..))
        .is_some_and(|end| aterm_hash::fnv1a_64(end.as_bytes()) == mark)
}

/// THE GO-AHEAD A TURN PARKS ON ([`go_ahead::parked`]): `turn_said` (its earlier messages) and
/// `tail` (its last), past the rows of `act`, the act the turn opens with where it does. A turn
/// end ([`TurnEndReading::go_ahead_past`]) and a break of the agent's background work
/// ([`TurnEndState::go_ahead_at_break`]) read it alike. A need of the earlier messages that the
/// turn's last word retracted parks nothing ([`needs::retracted_cut`]): the agent did that work,
/// or no longer needs it, so a go-ahead for it would be a prompt it cannot act on.
fn turn_go_ahead(
    turn_said: Option<needs::Before<'_>>,
    tail: &str,
    act: Option<&str>,
) -> Option<go_ahead::Park> {
    match turn_said {
        Some(before) => go_ahead::parked(
            &needs::retracted_cut(before.past(without_act_rows(before.text(), act)), tail),
            tail,
        ),
        None => go_ahead::parked("", without_act_rows(tail, act)),
    }
}

fn without_act_rows<'a>(text: &'a str, act: Option<&str>) -> &'a str {
    let act = said_letters(act);
    if act.is_empty() {
        return text;
    }
    let mut letters = String::new();
    let mut at = 0;
    let mut cut = None;
    for line in text.split_inclusive('\n') {
        letters.extend(line.chars().filter(|c| c.is_alphanumeric()));
        at += line.len();
        if letters.len() > act.len() {
            break;
        }
        if !letters.is_empty() && act.ends_with(&letters) {
            cut = Some(at);
        }
    }
    cut.map_or(text, |at| text[at..].trim_start_matches('\n'))
}

/// `lines` ([`TurnEndReading::said_lines`], [`TurnEndReading::turn_lines`]) where they hold the
/// same words as `rows`, its text a line a row; else `rows`.
fn lines_of<'a>(rows: Option<&'a str>, lines: Option<&'a str>) -> Option<&'a str> {
    lines
        .filter(|l| said_letters(Some(l)) == said_letters(rows))
        .or(rows)
}

/// THE WORDS OF A TURN THE NEEDS FOLD AND THE GO-AHEAD'S PARK BOTH READ, `(before, tail)`: its
/// earlier messages and its last as drawn (`said`, `(tail, before)`), each read as its measured
/// lines (`lines`, the same pair with each line the program wrapped read as one) where they hold
/// the same words ([`lines_of`]), the earlier messages told apart by `breaks` in the text read. A
/// point reads its turn so ([`TurnEndReading::needs_said`]) and a break of the agent's background
/// work alike, so every line the go-ahead answers is a line the fold stands, never a row of one:
/// the menu bar matches lines exactly ([`Needs::less`]), and a line the go-ahead answers never
/// goes up (DESIGN-harness-autopilot §15.6).
#[must_use]
pub(crate) fn needs_words<'a>(
    (tail, before): (Option<&'a str>, Option<&'a str>),
    (tail_lines, before_lines): (Option<&'a str>, Option<&'a str>),
    breaks: &'a needs::Breaks,
) -> (Option<needs::Before<'a>>, Option<&'a str>) {
    let before = match before_lines.filter(|l| said_letters(Some(l)) == said_letters(before)) {
        Some(lines) => Some(needs::Before::new(lines, &breaks.lines)),
        None => before.map(|t| needs::Before::new(t, &breaks.said)),
    };
    (before, lines_of(tail, tail_lines))
}

/// THE MARK OF A SCREEN'S LETTERS ([`TurnEndReading::screen_mark`]).
#[must_use]
pub fn screen_mark(rows: &[String]) -> u64 {
    let mut letters = String::new();
    for row in rows {
        letters.extend(row.chars().filter(|c| c.is_alphabetic()));
    }
    aterm_hash::fnv1a_64(letters.as_bytes())
}

/// THE MARK OF A READING'S CONVERSATION ([`TurnEndReading::screen_mark`]): the letters of the rows
/// above the composer's caret, each as the screen's transcript reads it
/// ([`aterm_phase::ScreenReader::transcript`]: beside Claude Code's side panel, its words up to
/// the panel's rule, so the panel's text moves no conversation).
#[must_use]
pub fn conversation_mark(reading: &aterm_phase::Reading, rows: &[String]) -> u64 {
    let transcript = reading.program.reader().transcript(rows);
    let rows = transcript.rows();
    screen_mark(
        reading
            .composer
            .as_ref()
            .and_then(|(caret, _)| rows.get(..*caret))
            .unwrap_or(rows),
    )
}

fn reply_stands(reading: &TurnEndReading) -> bool {
    turn_opener(reading).is_some() || (!reading.message_drawn && reading.said_tail.is_some())
}

/// THE MESSAGE THAT OPENED THE TURN `reading`'s last words end, hashed.
#[must_use]
pub fn turn_opener(reading: &TurnEndReading) -> Option<u64> {
    reading.answered.map(|a| a.prompt).or(reading.opened_by)
}

/// Whether one line of a report is [`FINISHED`] alone (Markdown punctuation allowed).
#[must_use]
pub fn is_finished_line(line: &str) -> bool {
    !line.trim_start().starts_with(['>', '▎', '│', '┃'])
        && line.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) == FINISHED
}

/// `text` with its [`is_finished_line`] lines dropped, trimmed.
#[must_use]
pub fn without_finished_lines(text: &str) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !is_finished_line(l)).collect();
    kept.join("\n").trim().to_string()
}

/// The heads a report lists open work under, lowercased ([`open_work`]).
pub const OPEN_HEADS: &[&str] = &[
    "still open",
    "still to do",
    "left open",
    "open items",
    "open issues",
    "open work",
    "open tasks",
    "open",
    "to do",
    "todo",
    "todos",
    "left",
    "not done",
    "not finished",
    "unfinished",
    "remaining",
    "what remains",
    "what's left",
    "what is left",
    "follow-up",
    "follow-ups",
    "follow up",
    "follow ups",
    "followup",
    "followups",
    "next step",
    "next steps",
    "outstanding",
    "pending",
    "deferred",
    "skipped",
    "not run",
    "not verified",
    "not tested",
    "untested",
    "not measured",
    "unmeasured",
];

const HEAD_TAIL: usize = 60;

const OPEN_NOUNS: &[&str] = &[
    "items", "item", "issues", "work", "tasks", "steps", "points", "to do", "yet",
];

/// The words that leave work open wherever they stand in a sentence ([`open_work`]).
pub const OPEN_STATUS: &[&str] = &[
    "not run",
    "skipped",
    "not done",
    "not verified",
    "not tested",
    "untested",
    "not measured",
    "unmeasured",
    "not re-measured",
    "not re-run",
    "not rerun",
    "not checked",
    "unchecked",
    "not compiled",
    "uncompiled",
    "not reproduced",
    "not validated",
    "not observed",
    "unobserved",
    "not exercised",
    "not explained",
    "unexplained",
    "not fixed",
    "unfixed",
    "not confirmed",
    "unconfirmed",
    "not closed",
    "not identified",
    "unidentified",
    "not passed",
    "still open",
    "remains open",
    "left open",
    "left for",
    "follow-up",
    "follow-ups",
    "needs a re-run",
    "need a re-run",
    "needs re-running",
    "did not measure",
    "did not test",
    "did not verify",
    "did not check",
    "did not look",
    "did not find",
    "did not track",
    "did not investigate",
    "did not fix",
    "did not confirm",
    "still fails",
    "still fail",
    "still failing",
];

/// WHAT LAND READS AS OPEN BESIDE [`OPEN_STATUS`] ([`open_work_strict`]).
pub const LAND_STATUS: &[&str] = &[
    "not landed",
    "unlanded",
    "not pushed",
    "unpushed",
    "not merged",
    "unmerged",
    "not committed",
    "uncommitted",
    "parked",
    "wip",
    "work in progress",
    "left on a branch",
    "on its own branch",
    "follow-up",
    "follow-ups",
    "follow up",
    "followup",
    "followups",
    "follow-on",
    "follow-ons",
    "follow on",
    "follow ons",
    "untracked",
    "unstaged",
    "modified files",
    "modified file",
    "without landing",
    "without merging",
    "without pushing",
    "without committing",
    "without being merged",
    "without being applied",
    "without applying",
    "moved again before",
    "moved before i pushed",
    "moved before i could",
    "before i could push",
    "dry run",
    "dry-run",
    "--dry-run",
    "staged for commit",
    "staged for",
    "is staged",
    "are staged",
    "staged changes",
    "is modified",
    "are modified",
    "still modified",
    "left modified",
    "shows m",
    "still on disk",
    "left the branch",
    "left the branches",
    "left the worktree",
    "left a branch",
];

/// A LATER DAY ([`LAND_FAMILIES`]).
pub const LAND_LATER: &[&str] = &[
    "for later",
    "later",
    "tomorrow",
    "tonight",
    "next week",
    "next month",
    "next sprint",
    "next quarter",
    "next round",
    "next pass",
    "next iteration",
    "in the next session",
    "for the next session",
    "a later session",
    "a future session",
    "another session",
    "another day",
    "another time",
    "in the future",
    "future work",
    "eventually",
    "someday",
    "some day",
    "at some point",
    "down the line",
    "for now",
    "for the time being",
    "until later",
    "whenever",
    "scheduled for",
    "planned for",
    "is planned",
    "is scheduled",
    "slated for",
    "are planned",
    "in due course",
    "when time permits",
    "time permitting",
    "pencilled in",
    "penciled in",
    "next release",
    "next restart",
    "next nightly",
    "next deploy",
    "next rebuild",
    "by monday",
    "by tuesday",
    "by wednesday",
    "by thursday",
    "by friday",
    "by the weekend",
    "by end of day",
    "by eod",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "this morning",
    "this afternoon",
    "this evening",
    "in the morning",
    "in the afternoon",
    "in the evening",
    "in a bit",
    "in a while",
    "in a minute",
    "in a few minutes",
    "in a few hours",
    "in a few days",
    "in an hour",
    "shortly",
    "asap",
    "over the weekend",
    "this weekend",
    "next time",
    "next session picks",
    "next session will",
    "next session can",
    "phase 2",
    "phase two",
    "phase ii",
    "post-freeze",
    "after the freeze",
    "next milestone",
    "next drop",
    "set for",
    "penciled for",
    "pencilled for",
    "is in the pipeline",
    "are in the pipeline",
    "still in the pipeline",
    "goes out with",
    "go out with",
    "comes with",
    "come with",
    "lands with",
    "land with",
    "ships with",
    "ship with",
    "when i get a chance",
    "when i'm back",
    "when i am back",
    "bumped to",
    "pushed back",
    "pushed out to",
    "afterwards",
    "afterward",
];

const DATED: &[&str] = &[
    "afterwards",
    "afterward",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "this morning",
    "this afternoon",
    "this evening",
    "in the morning",
    "in the afternoon",
    "in the evening",
    "shortly",
];

const LATER_WITH: &[&str] = &[
    "next",
    "release",
    "drop",
    "train",
    "milestone",
    "sprint",
    "version",
    "update",
    "batch",
    "cut",
    "rc",
    "monday's",
    "tuesday's",
    "wednesday's",
    "thursday's",
    "friday's",
];

#[derive(Clone, Copy, Debug)]
enum Reads {
    Anywhere,
    Alone,
    Beside(&'static [&'static str]),
    Exception,
    Later,
    Intent,
    Until,
    Elided,
    Gerund,
    Still,
    Ref,
    Fraction,
    NotDone,
    Lost,
    Better,
    Concession,
    State,
    End,
    Count,
    OneMore,
    Only,
    Elliptic,
    Absent,
    Nobody,
    Unlanded,
    Undone,
    Onto,
    Most,
    Dropped,
    OnBranch,
    Change,
    Percent,
    Party,
    OnlyTail,
    Qualified,
    Tally,
    Without,
    Approval,
    Rerun,
    Status,
}

impl Reads {
    const fn common(self) -> bool {
        matches!(
            self,
            Self::End
                | Self::Party
                | Self::OnlyTail
                | Self::Qualified
                | Self::Count
                | Self::OneMore
                | Self::Only
                | Self::Elliptic
                | Self::Absent
                | Self::Nobody
                | Self::Unlanded
                | Self::Undone
                | Self::Onto
                | Self::Most
                | Self::OnBranch
                | Self::Change
                | Self::Percent
                | Self::Tally
                | Self::Without
                | Self::Approval
                | Self::Status
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LandFamily {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "named by the defense table's mutation check")
    )]
    pub(crate) name: &'static str,
    rows: &'static [&'static str],
    reads: Reads,
    settled_by: &'static [&'static str],
    says: Option<&'static str>,
    phrases: Phrases,
}

impl LandFamily {
    const fn new(
        name: &'static str,
        rows: &'static [&'static str],
        reads: Reads,
        settled_by: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            rows,
            reads,
            settled_by,
            says: None,
            phrases: Phrases::of(rows),
        }
    }

    const fn saying(self, says: &'static str) -> Self {
        Self {
            says: Some(says),
            ..self
        }
    }
}

const SETTLED: &[&str] = &[
    "merged",
    "landed",
    "done",
    "fixed",
    "resolved",
    "closed",
    "approved",
    "reviewed",
    "re-enabled",
    "reenabled",
    "enabled",
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "removed",
    "deleted",
    "dropped",
    "gone",
    "addressed",
    "finished",
    "completed",
    "complete",
    "stable",
    "deflaked",
    "unblocked",
    "cleared",
    "applied",
    "popped",
    "answered",
    "empty",
    "investigated",
    "explained",
    "succeeded",
    "succeeds",
];

const LANDS: &[&str] = &[
    "merged",
    "landed",
    "squash-merged",
    "rebase-merged",
    "auto-merged",
    "upstreamed",
];

const SHIPS: &[&str] = &[
    "merged",
    "landed",
    "squash-merged",
    "rebase-merged",
    "auto-merged",
    "upstreamed",
    "published",
    "released",
    "shipped",
    "deployed",
];

const MENDS: &[&str] = &[
    "fixed",
    "resolved",
    "addressed",
    "closed",
    "removed",
    "deleted",
    "corrected",
    "done",
    "gone",
    "cleared",
    "finished",
    "completed",
    "complete",
    "investigated",
    "explained",
    "answered",
];

const CHECKS: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "fixed",
    "resolved",
    "succeeded",
    "succeeds",
    "stable",
    "deflaked",
    "re-enabled",
    "reenabled",
    "enabled",
    "removed",
    "deleted",
    "done",
    "finished",
    "completed",
    "complete",
    "investigated",
    "explained",
];

const DEFLAKED: &[&str] = &["fixed", "resolved", "deflaked", "removed", "deleted"];

const FLAKY: &[&str] = &[
    "flaky",
    "flakes",
    "flaking",
    "intermittently",
    "intermittent",
    "sporadically",
    "unstable",
];

const STEPS: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "fixed",
    "resolved",
    "succeeded",
    "succeeds",
    "merged",
    "landed",
];

const REVIEWED: &[&str] = &[
    "approved",
    "reviewed",
    "done",
    "given",
    "answered",
    "received",
    "addressed",
    "resolved",
    "finished",
    "completed",
    "decided",
];

const REVIEW_NOUNS: &[&str] = &[
    "review",
    "reviews",
    "approval",
    "approvals",
    "sign-off",
    "signoff",
    "feedback",
    "question",
    "questions",
    "decision",
    "decisions",
    "answer",
    "comment",
    "comments",
    "thread",
    "threads",
];

const CHANGE_NOUNS: &[&str] = &[
    "pr", "prs", "mr", "mrs", "request", "requests", "branch", "branches", "commit", "commits",
    "patch", "patches", "stash", "draft",
];

const STATUS_BY: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "green",
    "succeeded",
    "succeeds",
    "stable",
    "deflaked",
    "re-enabled",
    "reenabled",
    "enabled",
    "merged",
    "landed",
    "fixed",
    "resolved",
    "addressed",
    "closed",
    "removed",
    "deleted",
    "corrected",
    "done",
    "gone",
    "finished",
    "completed",
    "complete",
    "investigated",
    "explained",
    "answered",
];

const RETIRED: &[&str] = &[
    "removed",
    "cleaned",
    "deleted",
    "dropped",
    "reverted",
    "replaced",
    "gone",
    "retired",
    "completed",
    "complete",
    "finished",
];

const REFRESHED: &[&str] = &[
    "updated",
    "refreshed",
    "replaced",
    "rewritten",
    "corrected",
    "fixed",
    "removed",
    "deleted",
    "regenerated",
];

const WAIT_NOUNS: &[&str] = &[
    "review",
    "reviews",
    "reviewer",
    "reviewers",
    "reviewing",
    "approval",
    "approvals",
    "approve",
    "sign-off",
    "signoff",
    "decision",
    "decide",
    "decides",
    "merge",
    "merging",
    "landing",
    "owner",
    "person",
    "someone",
    "somebody",
    "ci",
    "rebase",
    "rebasing",
    "re-run",
    "rerun",
    "cleanup",
    "follow-up",
    "answer",
    "feedback",
    "go-ahead",
    "confirmation",
    "permission",
    "credential",
    "credentials",
    "qa",
    "sign",
    "signs",
    "major",
    "version",
    "upstream",
    "release",
    "vendor",
    "vendors",
    "maintainer",
    "maintainers",
    "team",
    "#secret",
];

const CHECK_NOUNS: &[&str] = &[
    "ci",
    "build",
    "builds",
    "pipeline",
    "pipelines",
    "check",
    "checks",
    "suite",
    "suites",
    "lane",
    "lanes",
    "workflow",
    "workflows",
    "job",
    "jobs",
    "test",
    "tests",
    "run",
    "runs",
    "benchmark",
    "benchmarks",
    "bench",
    "soak",
    "e2e",
    "snapshot",
    "cell",
    "cells",
    "matrix",
    "lint",
    "lints",
    "warning",
    "warnings",
    "assertion",
    "assertions",
    "threshold",
    "tolerance",
    "timeout",
    "clippy",
    "tippy",
    "linter",
    "rustfmt",
    "fmt",
    "p50",
    "p90",
    "p95",
    "p99",
    "p999",
    "latency",
    "latencies",
    "throughput",
    "perf",
    "performance",
];

const BRANCH_THINGS: &[&str] = &[
    "branch", "branches", "fork", "worktree", "pr", "prs", "mr", "#branch",
];

const LOCAL_THINGS: &[&str] = &[
    "commit", "commits", "branch", "branches", "tag", "tags", "change", "changes", "work", "fix",
    "fixes", "patch", "patches", "release", "stash", "worktree", "checkout", "copy",
];

const MADE_LOCALLY: &[&str] = &[
    "tagged",
    "tag",
    "tags",
    "committed",
    "merged",
    "cut",
    "released",
    "amended",
    "staged",
];

const FAULT_NOUNS: &[&str] = &[
    "bug",
    "bugs",
    "outage",
    "crash",
    "crashes",
    "hang",
    "hangs",
    "leak",
    "leaks",
    "race",
    "regression",
    "regressions",
    "flake",
    "flakes",
    "failure",
    "failures",
    "error",
    "errors",
    "problem",
    "problems",
    "issue",
    "issues",
    "panic",
    "panics",
    "deadlock",
    "stall",
    "stalls",
    "glitch",
    "slowdown",
];

const STEP_NOUNS: &[&str] = &[
    "push",
    "merge",
    "rebase",
    "commit",
    "upload",
    "deploy",
    "release",
    "publish",
    "install",
    "build",
    "builds",
    "test",
    "tests",
    "check",
    "checks",
    "ci",
    "job",
    "jobs",
    "lane",
    "lanes",
    "run",
    "suite",
    "pipeline",
    "hook",
    "signing",
    "notarization",
];

const OPEN_NOUNS_LAND: &[&str] = &[
    "pr",
    "prs",
    "request",
    "requests",
    "mr",
    "mrs",
    "issue",
    "issues",
    "ticket",
    "tickets",
    "question",
    "questions",
    "point",
    "points",
    "decision",
    "decisions",
    "thread",
    "threads",
    "comment",
    "comments",
    "discussion",
    "discussions",
    "case",
    "cases",
    "bug",
    "bugs",
    "gap",
    "gaps",
    "problem",
    "problems",
    "item",
    "items",
    "task",
    "tasks",
    "#n",
];

pub(crate) static LAND_FAMILIES: [LandFamily; 68] = [
    LandFamily::new("unlanded", LAND_STATUS, Reads::Anywhere, LANDS),
    LandFamily::new(
        "on a branch",
        &[
            "on a branch",
            "on its branch",
            "on my branch",
            "on a feature branch",
            "on the feature branch",
            "on a side branch",
            "on a separate branch",
            "on a local branch",
            "my local branch",
            "to my branch",
            "to a branch",
            "to a feature branch",
            "to its branch",
            "in a worktree",
            "in my worktree",
            "commits behind",
            "commit behind",
            "commits ahead",
            "commit ahead",
            "ahead of main",
            "ahead of origin",
            "ahead by",
            "behind my branch",
            "on my fork",
            "to my fork",
            "in my fork",
            "on a fork",
            "merged locally",
            "main locally",
            "local main",
            "keeping the worktree",
            "kept the worktree",
            "keep the worktree",
            "keeping the branch",
            "kept the branch",
            "keep the branch",
            "not on main",
            "not on origin",
            "not on the remote",
            "ahead of",
            "into my branch",
            "into my fork",
            "into a branch",
            "into a feature branch",
            "into the feature branch",
            "in my branch",
            "in a branch",
            "in a feature branch",
            "is ahead",
            "are ahead",
            "ahead again",
            "my main",
            "main in the worktree",
            "main in my worktree",
            "main on my machine",
            "main on this machine",
            "main on this mac",
            "main is unchanged",
            "main unchanged",
            "main is untouched",
            "main untouched",
            "left main alone",
            "revision behind",
            "revisions behind",
            "version behind",
            "versions behind",
            "is behind",
            "are behind",
            "still behind",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "current with main",
        &[
            "up to date with",
            "up-to-date with",
            "in sync with",
            "current with",
            "rebased onto",
            "rebased on",
        ],
        Reads::Beside(BRANCH_THINGS),
        LANDS,
    )
    .saying("on a branch"),
    LandFamily::new(
        "local",
        &[
            "still local",
            "only local",
            "local only",
            "local-only",
            "committed locally",
            "only locally",
            "is local",
            "are local",
            "away from done",
            "merge away",
            "push away",
            "commit away",
            "created locally",
            "tagged locally",
            "locally tagged",
            "kept locally",
            "sits locally",
            "updated locally",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "on my machine",
        &[
            "my machine",
            "my laptop",
            "my computer",
            "my box",
            "my workstation",
            "this machine",
            "this host",
        ],
        Reads::Beside(LOCAL_THINGS),
        LANDS,
    )
    .saying("local"),
    LandFamily::new(
        "made locally",
        &["locally"],
        Reads::Beside(MADE_LOCALLY),
        LANDS,
    )
    .saying("local"),
    LandFamily::new(
        "untouched",
        &["untouched", "not touched", "never touched", "unstarted"],
        Reads::Beside(WORK_ITEMS),
        SHIPS,
    )
    .saying("untouched work"),
    LandFamily::new("without", &["without"], Reads::Without, &[]).saying("work owed"),
    LandFamily::new(
        "approved only",
        &[
            "approved",
            "reviewed",
            "accepted",
            "signed off",
            "lgtm",
            "lgtm'd",
        ],
        Reads::Approval,
        LANDS,
    )
    .saying("approved, not landed"),
    LandFamily::new(
        "stash",
        &["stash", "stashed", "stashes"],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new("draft", &["draft", "drafts"], Reads::Anywhere, LANDS),
    LandFamily::new(
        "left over",
        &[
            "remaining",
            "left over",
            "leftover",
            "leftovers",
            "left to do",
            "left to land",
            "left to be",
            "left to the",
            "left to finish",
            "still to do",
            "still to land",
            "still to be",
            "yet to",
            "still have to",
            "still has to",
            "still need to",
            "still needs to",
            "has to be",
            "have to be",
            "tbd",
            "to be done",
            "to be decided",
            "what remains",
            "thing left",
            "things left",
            "item left",
            "items left",
            "work left",
            "next step",
            "next steps",
            "next task",
            "next tasks",
            "next thing",
            "next item",
            "next items",
            "next job",
            "next change",
            "next pr",
            "next piece",
            "next up",
            "up next",
            "to come",
            "outstanding",
            "should be",
            "must be",
            "needs to be",
            "ought to be",
            "lingering",
            "residual",
            "dangling",
            "stray",
            "last step",
            "final step",
            "last thing",
            "one last",
            "caveat",
            "caveats",
            "as an exercise",
            "for completeness",
            "to go",
            "on my list",
            "on the list",
            "to-do",
            "owed",
            "i owe",
            "still owe",
            "tba",
            "tbc",
            "by hand",
            "manual step",
            "only fixed",
            "only patched",
            "only papered",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "remains",
        &[
            "remains",
            "remain",
            "is left",
            "are left",
            "still left",
            "stays",
            "stay",
            "is next",
            "are next",
            "comes next",
            "come next",
            "can wait",
            "will wait",
            "has to wait",
            "yet",
            "mostly",
            "blocked",
            "missing",
        ],
        Reads::Alone,
        &[],
    ),
    LandFamily::new(
        "exception",
        &[
            "except",
            "apart from",
            "aside from",
            "other than",
            "save for",
            "but for",
            "besides",
            "excluding",
            "with the exception of",
            "barring",
            "minus",
            "modulo",
            "one exception",
            "an exception",
            "a single exception",
            "only exception",
            "excepting",
            "excepted",
            "sans",
            "but",
            "bar",
            "save",
            "less",
            "beyond",
            "without",
            "exception",
            "the exception",
            "one gap",
            "the one gap",
            "only gap",
            "the only gap",
            "single gap",
            "not included",
            "not including",
            "not counting",
            "don't count",
            "do not count",
            "excluded",
            "exclusive of",
            "short of",
            "notwithstanding",
            "leaving out",
            "outside of",
            "not the",
            "omission",
            "omissions",
            "one omission",
            "holdout",
            "holdouts",
            "the holdout",
            "one holdout",
            "well, nearly",
            "not withstanding",
            "short one",
            "short by one",
            "one short",
            "give or take",
        ],
        Reads::Exception,
        &[],
    ),
    LandFamily::new(
        "qualified none",
        &[
            "important",
            "big",
            "urgent",
            "blocking",
            "critical",
            "serious",
            "major",
            "real",
            "hard",
            "significant",
            "pressing",
            "must-have",
            "must-haves",
            "showstopper",
            "showstoppers",
            "show-stopper",
            "show-stoppers",
            "that blocks",
            "that matters",
            "that can't wait",
            "that cannot wait",
            "that can not wait",
            "in scope",
            "critical path",
            "today",
            "beyond",
            "worth",
            "of note",
            "noteworthy",
            "substantial",
            "essential",
            "vital",
            "high-priority",
            "high priority",
            "release-blocking",
            "material",
            "meaningful",
            "large",
            "huge",
            "on my side",
            "on my end",
            "from me",
            "for now",
            "yet",
            "here",
            "in this repo",
            "in this pr",
            "for this pr",
        ],
        Reads::Qualified,
        &[],
    )
    .saying("a qualified none"),
    LandFamily::new(
        "a party's",
        &[
            "owner",
            "the owner",
            "owner's",
            "whoever",
            "you",
            "yours",
            "maintainer",
            "maintainers",
            "the maintainer",
            "reviewer",
            "the reviewer",
            "reviewers",
            "human",
            "a human",
        ],
        Reads::Party,
        &[],
    )
    .saying("someone else"),
    LandFamily::new(
        "only that",
        &[
            "nothing more",
            "nothing else",
            "that's all",
            "that is all",
            "that's it",
            "that is it",
        ],
        Reads::OnlyTail,
        &[],
    )
    .saying("only"),
    LandFamily::new(
        "pending",
        &[
            "pending",
            "awaiting",
            "awaits",
            "on hold",
            "up for review",
            "is for review",
            "are for review",
            "sent for review",
            "submitted for review",
            "posted for review",
            "opened for review",
            "pushed for review",
            "under review",
            "in review",
            "out for review",
            "in the merge queue",
            "queued for",
            "unreviewed",
            "unapproved",
            "ready for review",
            "ready to review",
            "ready to merge",
            "ready for merge",
            "ready to land",
            "ready to be merged",
            "ready to be landed",
            "auto-merge",
            "automerge",
            "auto merge",
            "merge train",
            "merge queue",
            "is queued",
            "are queued",
            "on approval",
            "upon approval",
            "merges on",
            "merges once",
            "merges when",
            "merges after",
            "lands once",
            "lands when",
            "lands after",
            "lands on",
            "merging after",
            "merging once",
            "merging when",
            "landing after",
            "landing once",
            "ready to tag",
            "ready to publish",
            "ready to release",
            "ready to ship",
            "ready to push",
            "ready to deploy",
            "for review",
            "for a review",
            "is ready",
            "are ready",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "waits on",
        &[
            "needs", "need", "requires", "require", "waiting", "waits", "wait",
        ],
        Reads::Beside(WAIT_NOUNS),
        LANDS,
    ),
    LandFamily::new(
        "blocked",
        &[
            "blocked on",
            "blocked by",
            "is blocked",
            "are blocked",
            "still blocked",
            "remains blocked",
            "stays blocked",
        ],
        Reads::Anywhere,
        SHIPS,
    ),
    LandFamily::new(
        "open change",
        &["open", "opened", "reopened", "filed"],
        Reads::Beside(OPEN_NOUNS_LAND),
        MENDS,
    ),
    LandFamily::new(
        "check",
        &[
            "running",
            "red",
            "failing",
            "queued",
            "cut short",
            "aborted",
            "interrupted",
            "stopped early",
            "killed early",
            "ended early",
            "in flight",
            "flaky",
            "flakes",
            "flaking",
            "disabled",
            "ignored",
            "quarantined",
            "muted",
            "skipped",
            "broken",
            "timing out",
            "timed out",
            "hanging",
            "unstable",
            "yellow",
            "suppressed",
            "silenced",
            "commented out",
            "xfail",
            "fails",
            "fail",
            "intermittently",
            "intermittent",
            "sporadically",
            "amber",
            "turned off",
            "switched off",
            "allow",
            "allowed",
            "relaxed",
            "loosened",
            "weakened",
            "not passing",
            "not green",
            "not succeeding",
            "not succeeded",
            "not stable",
            "not clean",
            "error out",
            "errors out",
            "erroring out",
            "errored out",
            "complains",
            "complain",
            "complaining",
            "regressed",
            "silence",
            "xfailed",
            "deselected",
        ],
        Reads::Beside(CHECK_NOUNS),
        CHECKS,
    ),
    LandFamily::new(
        "rerun",
        &[
            "on rerun",
            "on re-run",
            "on a rerun",
            "on a re-run",
            "on the rerun",
            "on retry",
            "on a retry",
            "on the retry",
            "after a rerun",
            "after one rerun",
            "after a retry",
            "after one retry",
            "after two retries",
            "after retrying",
            "with a retry",
            "with one retry",
            "one retry",
            "two retries",
            "retried",
            "second run",
            "2nd run",
            "third run",
            "3rd run",
            "second attempt",
            "2nd attempt",
            "third attempt",
            "3rd attempt",
            "second try",
            "2nd try",
            "third try",
            "3rd try",
            "ran it again",
            "ran them again",
            "run it again",
            "running it again",
            "flaked",
            "rerun clean",
            "rerun green",
            "rerun passed",
            "re-run clean",
            "re-run green",
            "reran clean",
            "reran green",
            "re-ran clean",
            "re-ran green",
            "passed this time",
            "green this time",
            "passes this time",
        ],
        Reads::Rerun,
        &[],
    )
    .saying("a rerun"),
    LandFamily::new(
        "status item",
        &[
            "skip",
            "skips",
            "named skip",
            "named skips",
            "none run",
            "not read",
            "unread",
            "next session",
            "to do",
            "n/a",
        ],
        Reads::Status,
        &[],
    ),
    LandFamily::new("later", LAND_LATER, Reads::Later, LANDS),
    LandFamily::new("until", &["until", "till"], Reads::Until, &[]),
    LandFamily::new(
        "intent",
        &[
            "i will",
            "i'll",
            "we will",
            "we'll",
            "i am going to",
            "i'm going to",
            "plan to",
            "planning to",
            "intend to",
            "will land",
            "will be landed",
            "will merge",
            "will be merged",
            "will push",
            "will be pushed",
            "will be done",
            "will be fixed",
            "will do",
            "will add",
            "will write",
            "will follow",
            "should still",
            "still should",
            "we should",
            "i should",
            "ought to",
            "would be good to",
            "would be nice to",
            "would be nice",
            "would be good",
            "would help",
            "would be better",
            "nice to have",
            "nice-to-have",
            "worth adding",
            "can be done",
            "could be done",
            "can be added",
            "could be added",
            "we can",
            "we could",
            "i'd like",
            "i would like",
            "would like",
            "second pair of eyes",
            "a second look",
            "another look",
            "a closer look",
            "a final look",
            "whether to",
            "whether we should",
            "if we should",
            "asked the owner",
            "asked the user",
            "i'm leaving",
            "i am leaving",
            "i'm keeping",
            "i am keeping",
            "will be",
            "will happen",
            "will update",
            "will rebuild",
            "will refresh",
            "will regenerate",
            "will redeploy",
            "will deploy",
            "will publish",
            "will roll",
            "will pick",
            "will go",
            "will get",
            "will run",
            "will need",
            "will come",
            "will ship",
            "should i",
            "should we",
            "shall i",
            "shall we",
            "want me to",
            "do you want",
            "let me know",
            "happy to",
            "i can also",
            "if you want",
            "if you'd like",
            "if you like",
            "if needed",
            "if desired",
            "when you are",
            "when you're",
            "let's",
            "let us",
            "worth considering",
            "consider",
        ],
        Reads::Intent,
        &[],
    ),
    LandFamily::new(
        "todo",
        &["todo", "todos", "fixme", "fixmes"],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "check off in code",
        &["check ignored", "lint allowed"],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "could not",
        &[
            "could not",
            "couldn't",
            "not able to",
            "unable to",
            "cannot run",
            "cannot test",
            "cannot verify",
            "cannot check",
            "cannot measure",
            "cannot confirm",
            "can't run",
            "can't test",
            "can't verify",
            "can't check",
            "can't measure",
            "can't confirm",
            "did not get to",
            "ran out of",
            "ran out",
            "out of time",
            "ended before",
            "stopped before",
            "unverified",
            "only type-checked",
        ],
        Reads::Anywhere,
        CHECKS,
    ),
    LandFamily::new(
        "not done",
        &[
            "not written",
            "not reviewed",
            "not approved",
            "not released",
            "not deployed",
            "not published",
            "not tagged",
            "not documented",
            "not implemented",
            "not started",
            "not finished",
            "not complete",
            "not completed",
            "not resolved",
            "not addressed",
            "not handled",
            "not updated",
            "not corrected",
            "not driven end to end",
            "not wired",
            "not hooked up",
            "not hooked in",
            "not plugged in",
            "not integrated",
            "not ported",
            "not migrated",
            "not rebased",
            "not signed",
            "not land",
            "not merge",
            "not push",
            "not commit",
            "not all",
            "not removed",
            "not deleted",
            "not cleaned up",
            "not reverted",
            "not dropped",
            "not investigated",
            "not looked at",
            "not looked into",
            "not profiled",
            "not diagnosed",
            "not understood",
            "not root-caused",
            "uninvestigated",
            "did not update",
            "did not add",
            "did not write",
            "did not push",
            "did not merge",
            "did not land",
            "did not finish",
            "did not do",
            "did not address",
            "did not handle",
            "did not document",
            "did not commit",
            "did not remove",
            "did not delete",
            "did not rebase",
            "not covered",
            "uncovered",
            "not tried",
            "untried",
            "not attempted",
            "did not apply",
            "stopped there",
            "gave up",
            "did not have time",
            "had no time",
            "no time for",
            "forgot to",
            "overlooked",
            "did not get into",
            "did not dig into",
            "not get around",
            "unfinished",
            "unaddressed",
            "unresolved",
            "unhandled",
            "unimplemented",
            "unwritten",
            "undocumented",
            "unreleased",
            "unpublished",
            "untagged",
            "unanswered",
            "unsolved",
            "unstarted",
            "never written",
            "never tested",
            "never run",
            "never ran",
            "never reviewed",
            "never documented",
            "never finished",
            "never implemented",
            "never released",
            "never published",
            "never tagged",
            "never verified",
            "never checked",
            "never measured",
            "not so much",
            "left out",
            "missed the merge",
            "missed the release",
            "missed the cut",
            "missed the train",
            "missed the window",
            "missed the deadline",
            "to be written",
            "to be added",
            "to be updated",
            "to be fixed",
            "to be ported",
            "still to write",
        ],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "partly",
        &[
            "only partly",
            "partly",
            "partially",
            "partial",
            "half done",
            "half-done",
            "half written",
            "half-written",
            "half finished",
            "half-finished",
            "half implemented",
            "half-implemented",
            "half merged",
            "halfway",
            "half-way",
            "half way",
            "mostly done",
            "mostly merged",
            "mostly landed",
            "almost done",
            "nearly done",
            "incomplete",
            "incompletely",
            "not fully",
            "not completely",
            "not entirely",
            "spot-checked",
            "spot-check",
            "spot checked",
            "only checked",
            "only tested",
            "only ran",
            "only verified",
            "only tried",
            "smoke-tested",
            "eyeballed",
            "some of the",
            "a few of the",
            "several of the",
            "half of",
            "half fixed",
            "mostly fixed",
            "fixed only for",
            "a quarter of",
            "a third of",
            "two thirds of",
            "three quarters of",
            "a fraction of",
            "not the real",
            "against a mock",
            "staging only",
            "on staging only",
            "only on staging",
            "only in staging",
            "not zero",
            "only some",
            "only part",
            "done-ish",
            "merged-ish",
            "finished-ish",
            "landed-ish",
            "complete-ish",
            "mostly-done",
        ],
        Reads::Anywhere,
        RETIRED,
    ),
    LandFamily::new(
        "stopgap",
        &[
            "workaround",
            "workarounds",
            "temporary",
            "temporarily",
            "stopgap",
            "stop-gap",
            "hack",
            "hacks",
            "hacky",
            "interim",
            "band-aid",
            "quick fix",
            "old code path",
            "old path",
            "behind a flag",
            "behind a cfg",
            "behind cfg",
            "behind a feature gate",
            "behind a gate",
            "left in place",
            "in place behind",
            "behind a feature flag",
            "behind the flag",
            "off by default",
            "works around",
            "work around",
            "worked around",
            "working around",
            "rather than fixing",
            "instead of fixing",
            "rather than solving",
            "instead of solving",
            "as a fallback",
            "as a backup",
            "rather than deleted",
            "rather than deleting",
            "rather than removed",
            "rather than removing",
            "instead of deleting",
            "instead of removing",
            "placeholder",
            "placeholders",
            "stubbed out",
            "is a stub",
            "just a stub",
            "only a stub",
            "kept around",
            "keep around",
            "keeping around",
        ],
        Reads::Anywhere,
        RETIRED,
    ),
    LandFamily::new(
        "needs doing",
        &[
            "needs",
            "need",
            "requires",
            "require",
            "still needs",
            "wants",
        ],
        Reads::Gerund,
        &[],
    ),
    LandFamily::new("still", &["still"], Reads::Still, &[]),
    LandFamily::new(
        "remote branch",
        &["origin", "upstream", "--no-merged"],
        Reads::Ref,
        LANDS,
    ),
    LandFamily::new("part of a count", &["of", "only"], Reads::Fraction, &[]),
    LandFamily::new(
        "hedge",
        &[
            "not sure",
            "unsure",
            "uncertain",
            "unclear whether",
            "unclear if",
            "not certain",
            "not convinced",
            "not confident",
            "i think",
            "i believe",
            "we think",
            "we believe",
            "i doubt",
            "doubt that",
            "doubtful",
            "i suspect",
            "i guess",
            "i hope",
            "i imagine",
            "probably",
            "should pass",
            "should work",
            "should be fine",
            "likely",
            "hopefully",
            "a guess",
            "guessed",
            "guesswork",
            "i assume",
            "presumably",
            "seems to",
            "appears to",
            "from my laptop",
            "from my machine",
            "tested on my",
            "measured on my",
            "benchmarked on my",
            "only locally",
            "locally only",
            "just locally",
            "only on my",
            "assuming",
            "fairly sure",
            "pretty sure",
            "reasonably sure",
            "quite sure",
            "fairly confident",
            "pretty confident",
            "if i recall",
            "if i remember",
            "iirc",
            "afaict",
            "afaik",
            "as far as i can tell",
            "as far as i know",
            "to my knowledge",
            "looks merged",
            "looks landed",
            "looks done",
            "looks good",
            "looks fine",
            "looks like it",
            "looks like everything",
            "looks like all",
            "it looks like",
            "provided",
            "providing",
            "fingers crossed",
            "should go",
            "should land",
            "should merge",
            "should be merged",
            "should be green",
            "unknown",
            "no idea",
            "no data",
            "not known",
        ],
        Reads::Anywhere,
        &[],
    ),
    LandFamily::new(
        "failed step",
        &[
            "failed",
            "errored",
            "rejected",
            "refused",
            "denied",
            "bounced",
            "conflict",
            "conflicts",
            "conflicted",
            "conflicting",
        ],
        Reads::Beside(STEP_NOUNS),
        STEPS,
    ),
    LandFamily::new(
        "elided",
        &[
            "is not", "are not", "has not", "have not", "was not", "were not", "did not",
            "does not", "do not",
        ],
        Reads::Elided,
        &[],
    ),
    LandFamily::new(
        "did not",
        &["did not", "have not", "has not", "had not", "never got to"],
        Reads::NotDone,
        &[],
    ),
    LandFamily::new(
        "no longer works",
        &[
            "doesn't",
            "does not",
            "don't",
            "do not",
            "no longer",
            "won't",
            "will not",
            "cannot",
            "can't",
        ],
        Reads::Lost,
        &[],
    ),
    LandFamily::new(
        "could be better",
        &["could be", "might be"],
        Reads::Better,
        &[],
    ),
    LandFamily::new(
        "concession",
        &["though", "although", "however", "but", "even so"],
        Reads::Concession,
        &[],
    ),
    LandFamily::new(
        "wrong",
        &[
            "is wrong",
            "are wrong",
            "still wrong",
            "is incorrect",
            "are incorrect",
            "is inaccurate",
            "is misleading",
            "is buggy",
            "is broken",
            "are broken",
            "will break",
            "will fail",
            "would break",
            "only handles",
            "only covers",
            "only supports",
            "only works",
            "is missing",
            "are missing",
            "still missing",
            "lacks",
            "still lacks",
            "lacking",
            "same bug",
            "same problem",
            "same issue",
            "nothing covers",
            "no test covers",
            "no tests cover",
            "says nothing about",
            "does not mention",
            "doesn't mention",
            "no mention of",
            "got worse",
            "gets worse",
            "is worse",
            "are worse",
            "got slower",
            "is slower",
            "are slower",
            "is dirty",
            "are dirty",
            "still dirty",
            "regression in",
            "regressions in",
            "is a regression",
            "there is a regression",
            "introduces a regression",
            "introduced a regression",
            "regressed",
            "main is red",
            "main is broken",
            "main is failing",
            "red on main",
        ],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "back again",
        &[
            "is back",
            "are back",
            "came back",
            "comes back",
            "is back again",
            "resurfaced",
            "reappeared",
            "recurs",
            "recurred",
            "returned",
            "still reproduces",
        ],
        Reads::Beside(FAULT_NOUNS),
        MENDS,
    ),
    LandFamily::new(
        "stale",
        &[
            "is stale",
            "are stale",
            "still stale",
            "now stale",
            "outdated",
            "out of date",
            "out-of-date",
            "still describes",
            "still mentions",
            "still refers to",
            "still points to",
            "still documents",
        ],
        Reads::Anywhere,
        REFRESHED,
    ),
    LandFamily::new(
        "known issue",
        &[
            "known issue",
            "known issues",
            "known bug",
            "known bugs",
            "known limitation",
            "known limitations",
            "known gap",
            "known gaps",
            "known problem",
            "known problems",
            "known regression",
            "known regressions",
            "known crash",
            "known crashes",
            "known hang",
            "known flake",
            "known failure",
            "known failures",
            "nit",
            "nits",
        ],
        Reads::Anywhere,
        MENDS,
    ),
    LandFamily::new(
        "someone else",
        &[
            "someone else",
            "somebody else",
            "someone will",
            "somebody will",
            "someone should",
            "somebody should",
            "someone can",
            "someone could",
            "someone must",
            "someone needs to",
            "until someone",
            "until somebody",
            "up to the owner",
            "left to the owner",
            "for the owner to",
            "the owner will",
            "the owner must",
            "the owner has to",
            "the owner needs to",
            "the owner's call",
            "up to you",
            "for you to",
            "your call",
            "another agent",
            "another worker",
            "another person",
            "the next agent",
            "the next worker",
            "a later worker",
            "owner's to do",
            "yours to do",
            "the owner's job",
            "the owner's step",
            "the owner's turn",
            "in the owner's hands",
            "in your hands",
            "on my side",
            "on my end",
            "leave to",
            "leave it to",
            "i leave",
            "leaving it to",
            "hand off",
            "handed off",
            "handoff",
            "hand-off",
            "hand-offs",
            "handing off",
            "handing it off",
            "handing over",
            "handed over",
            "you can",
            "you may",
            "you could",
            "you might",
            "feel free",
            "over to you",
            "your court",
            "your move",
            "your turn",
            "on you to",
            "is yours",
            "are yours",
            "yours to",
            "with the owner",
            "with you",
            "sits with",
            "rests with",
            "lies with",
            "has to sign",
            "must sign",
            "needs to sign",
            "to sign off",
            "for sign-off",
            "for signoff",
            "for your review",
            "for your approval",
            "on me",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "asks you",
        &[
            "please",
            "kindly",
            "i need you",
            "we need you",
            "need you to",
            "needs you to",
            "need from you",
            "needed from you",
            "need your",
            "needs your",
            "requires you",
            "require you",
            "requires your",
            "require your",
            "you need to",
            "you'll need to",
            "you will need to",
            "you'd need to",
            "you would need to",
            "you'll have to",
            "you will have to",
            "you have to",
            "you must",
            "you should",
            "could you",
            "can you",
            "would you",
            "will you",
            "ask you",
            "asking you",
            "action for you",
            "actions for you",
            "thing for you",
            "things for you",
            "item for you",
            "items for you",
            "task for you",
            "step for you",
            "steps for you",
            "left for you",
            "is for you",
            "are for you",
            "waiting on you",
            "waiting for you",
            "waits on you",
            "waits for you",
            "depends on you",
            "on you",
            "owner action",
            "action needed",
            "action required",
            "needs the owner",
            "requires the owner",
            "require the owner",
            "only a person can",
        ],
        Reads::Anywhere,
        &[],
    ),
    LandFamily::new(
        "deferred",
        &[
            "deferred",
            "postponed",
            "put off",
            "punted",
            "shelved",
            "backlogged",
            "in the backlog",
            "tracked in issue",
            "tracked in an issue",
            "tracked in a ticket",
            "tracked as issue",
            "tracked as an issue",
            "separately",
            "separate pr",
            "separate prs",
            "separate change",
            "separate job",
            "separate task",
            "separate step",
            "separate issue",
            "separate effort",
            "its own pr",
            "another pr",
            "a second pr",
            "second pr",
            "a later pr",
            "subsequent pr",
            "subsequent prs",
            "subsequent change",
            "subsequent changes",
            "subsequent commit",
            "subsequent session",
            "subsequent release",
            "subsequent version",
            "is coming",
            "are coming",
            "coming in",
            "coming soon",
            "in v2",
            "for v2",
            "next version",
            "a future version",
            "a later version",
            "out of scope",
            "outside my scope",
            "beyond scope",
            "beyond the scope",
            "not in scope",
            "another ticket",
            "another issue",
            "a separate ticket",
            "a follow-up ticket",
            "its own ticket",
            "another task",
            "tracked in",
            "tracked by",
            "tracked as",
            "tracked under",
            "tracked separately",
            "tracking issue",
            "on the roadmap",
            "on our roadmap",
            "in the roadmap",
            "can follow",
            "could follow",
            "may follow",
            "might follow",
            "after this lands",
            "once this lands",
            "after this merges",
            "once this merges",
            "after this is merged",
            "once this is merged",
            "second step",
        ],
        Reads::Anywhere,
        LANDS,
    ),
    LandFamily::new(
        "in progress",
        &[
            "in progress",
            "underway",
            "under way",
            "paused",
            "on pause",
            "is in flight",
            "are in flight",
            "still in flight",
            "abandoned",
            "omitted",
            "skipping",
            "ongoing",
            "stalled",
            "stuck",
            "suspended",
            "on the back burner",
            "on ice",
            "about to",
            "wrapping up",
            "almost through",
            "nearly through",
            "going through",
            "going in",
            "merging now",
            "pushing now",
            "landing now",
            "rolling out",
            "being rolled out",
            "being merged",
            "being pushed",
            "in the works",
            "slipped",
            "descoped",
            "de-scoped",
            "fell through",
            "fell behind",
            "fell off",
            "lag behind",
            "lags behind",
            "lagging behind",
            "behind the code",
        ],
        Reads::State,
        MENDS,
    ),
    LandFamily::new(
        "end of a clause",
        &[
            "soon",
            "next",
            "aside",
            "one thing",
            "one more thing",
            "to follow",
            "one item",
            "one small thing",
            "one small item",
            "one last item",
            "a single item",
            "coming",
            "forthcoming",
            "upcoming",
            "nearly",
            "almost",
            "absent",
            "to write",
            "to add",
            "to update",
            "to fix",
            "to port",
            "to land",
            "to merge",
            "to push",
            "to tag",
            "to review",
            "to publish",
            "to release",
            "last bit",
            "the last bit",
            "local",
            "behind",
            "on origin",
            "on the remote",
        ],
        Reads::End,
        &[],
    ),
    LandFamily::new("counted left", &["left"], Reads::Count, &[]),
    LandFamily::new("one more", &["one more"], Reads::OneMore, &[]),
    LandFamily::new("only", &["only", "just", "merely"], Reads::Only, &[]),
    LandFamily::new("after an event", &["after", "once"], Reads::Elliptic, &[]).saying("later"),
    LandFamily::new(
        "absent",
        &[
            "no test",
            "no tests",
            "no unit test",
            "no unit tests",
            "no new test",
            "no new tests",
            "no regression test",
            "no docs",
            "no doc",
            "no documentation",
            "no coverage",
            "no test coverage",
            "no benchmark",
            "no benchmarks",
            "no changelog",
            "no changelog entry",
            "no example",
            "no examples",
            "no readme",
            "no release notes",
            "no release note",
            "no migration",
            "no migrations",
            "zero tests",
            "zero test",
            "0 tests",
            "no test case",
            "no test cases",
            "no integration test",
            "no integration tests",
            "no run",
            "no runs",
            "no result",
            "no results",
            "no numbers",
            "no measurement",
            "no measurements",
        ],
        Reads::Absent,
        &[],
    ),
    LandFamily::new(
        "nobody",
        &["no one", "no-one", "noone", "nobody", "nothing"],
        Reads::Nobody,
        &[],
    ),
    LandFamily::new(
        "landing denied",
        &[
            "merged",
            "landed",
            "pushed",
            "reached",
            "committed",
            "on origin/main",
            "on main",
            "on remote main",
            "on the remote main",
            "in origin/main",
            "on upstream/main",
            "made it",
            "push",
            "merge",
        ],
        Reads::Unlanded,
        &[],
    )
    .saying("not landed"),
    LandFamily::new(
        "work denied",
        &[
            "written",
            "run",
            "tested",
            "done",
            "finished",
            "implemented",
            "documented",
            "reviewed",
        ],
        Reads::Undone,
        &[],
    )
    .saying("not done"),
    LandFamily::new(
        "onto a branch",
        &[
            "pushed",
            "merged",
            "landed",
            "committed",
            "cherry-picked",
            "cherry picked",
            "backported",
            "picked",
        ],
        Reads::Onto,
        &[],
    )
    .saying("to a branch"),
    LandFamily::new(
        "most",
        &[
            "most",
            "most of",
            "half the",
            "the bulk of",
            "the majority of",
            "almost all",
            "almost everything",
            "nearly all",
            "nearly everything",
            "virtually all",
            "practically all",
            "some of it",
            "some of them",
            "part of it",
            "parts of it",
            "half of it",
            "half of them",
            "much of",
            "the majority",
            "largely",
            "virtually everything",
            "practically everything",
            "the better part of",
            "a good part of",
            "pretty much",
            "more or less",
            "essentially",
            "basically",
            "practically",
            "lion's share",
            "the lion's share",
            "tenths",
            "nine tenths",
            "the lion's share of",
            "most commits",
        ],
        Reads::Most,
        &[],
    ),
    LandFamily::new(
        "dropped",
        &[
            "was dropped",
            "were dropped",
            "got dropped",
            "been dropped",
            "is dropped",
            "are dropped",
            "reverted",
            "rolled back",
            "backed out",
            "backed off",
            "scrapped",
            "was cut",
            "were cut",
            "got cut",
            "been cut",
            "cut from",
        ],
        Reads::Dropped,
        &[],
    ),
    LandFamily::new(
        "on a named branch",
        &[
            "on",
            "in",
            "has everything",
            "has it all",
            "has all of it",
            "holds everything",
            "carries everything",
        ],
        Reads::OnBranch,
        &[],
    )
    .saying("on a branch"),
    LandFamily::new(
        "change",
        &[
            "pr",
            "prs",
            "pull request",
            "pull requests",
            "mr",
            "mrs",
            "merge request",
            "merge requests",
            "the second one",
            "the other one",
        ],
        Reads::Change,
        SHIPS,
    )
    .saying("an unmerged change"),
    LandFamily::new(
        "percent",
        &["percent", "per cent", "pct"],
        Reads::Percent,
        &[],
    )
    .saying("a part"),
    LandFamily::new(
        "counted failures",
        &[
            "errors", "error", "failures", "failure", "panicked", "panics", "panic", "crashes",
            "crash", "crashed", "timeouts", "timeout", "warnings", "warning", "xfailed", "xfail",
            "flakes", "flake",
        ],
        Reads::Tally,
        CHECKS,
    ),
];

const CONTRACTED: &[&str] = &[
    "did", "was", "were", "has", "have", "had", "is", "are", "does", "do",
];

const OPEN_NEGATIONS: &[&str] = &[
    "none", "nothing", "no", "n/a", "nil", "never", "zero", "0", "without", "neither", "nor",
];

const OPEN_MODALS: &[&str] = &[
    "do", "does", "don't", "doesn't", "must", "should", "shall", "will", "would", "can", "may",
    "might", "need", "needs",
];

/// What a report's own words leave open, in [`OPEN_HEADS`]' and [`OPEN_STATUS`]' spelling, each once, in the order found.
#[must_use]
pub fn open_work(tail: Option<&str>) -> Vec<&'static str> {
    tail.map_or_else(Vec::new, |t| open_work_read(t, None))
}

/// LAND'S STRICT READING of open work.
#[must_use]
pub fn open_work_strict(tail: Option<&str>) -> Vec<&'static str> {
    tail.map_or_else(Vec::new, |t| open_work_read(t, Some(&LAND_FAMILIES)))
}

fn open_work_read(tail: &str, land: Option<&[LandFamily]>) -> Vec<&'static str> {
    open_work_read_as(tail, land, true)
}

fn open_work_read_as(tail: &str, land: Option<&[LandFamily]>, defense: bool) -> Vec<&'static str> {
    once(open_work_found(tail, land, defense))
}

/// Each found word once, in the order found.
fn once(found: Vec<Found>) -> Vec<&'static str> {
    let mut once: Vec<&'static str> = Vec::with_capacity(found.len());
    for word in found.into_iter().map(|f| f.word) {
        if !once.contains(&word) {
            once.push(word);
        }
    }
    once
}

/// A piece of open work a report names: the reader's word for it, and the sentence it stood in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The reader's word, as [`open_work`] and [`open_work_strict`] name it.
    pub word: &'static str,
    /// The sentence the word stood in, in the agent's own words, with markdown's emphasis, code
    /// ticks and the marks the reader dropped left out and its spaces closed up; for a head
    /// over items `<head>: <first item>`, for an unticked line `to do: <item>`, and for a word
    /// a code block reads, the block's line that held it. At most [`SAID_CAP`] bytes: a longer
    /// one is cut around its match, which it keeps whole, with `…` at each cut end.
    pub said: String,
    /// The bytes of `said` that matched: the phrase, the head, `to do`, a count beside none
    /// with the word that places it, or a code block's whole line.
    pub matched: Range<usize>,
}

/// The most bytes a [`Found::said`] holds.
pub const SAID_CAP: usize = 160;

/// What a [`Found::said`] leads an unticked line with.
const TO_DO: &str = "to do";

impl Found {
    /// `word`, matched at `matched` in the sentence `held` of `line`'s words.
    fn of(
        line: &ReportLine,
        held: Range<usize>,
        word: &'static str,
        matched: Range<usize>,
    ) -> Self {
        let (said, matched) = line.said(held, matched);
        Self::cut(word, &said, matched)
    }

    /// An unticked line: `to do: <item>`.
    fn to_do(line: &ReportLine) -> Self {
        let (item, _) = line.said(0..line.words.len(), 0..0);
        let said = if item.is_empty() {
            TO_DO.to_string()
        } else {
            format!("{TO_DO}: {item}")
        };
        Self::cut(UNCHECKED, &said, 0..TO_DO.len())
    }

    /// A head over items: `<head>: <first item>`, the first item that is not a nil one.
    fn over(
        line: &ReportLine,
        held: Range<usize>,
        word: &'static str,
        head: Range<usize>,
        item: &ReportLine,
    ) -> Self {
        let (lead, matched) = line.said(held, head);
        let lead = lead.trim_end_matches(|c: char| c == ':' || c.is_whitespace());
        let matched = matched.start.min(lead.len())..matched.end.min(lead.len());
        let (item, _) = item.said(0..item.words.len(), 0..0);
        let said = if item.is_empty() {
            lead.to_string()
        } else {
            format!("{lead}: {item}")
        };
        Self::cut(word, &said, matched)
    }

    fn cut(word: &'static str, said: &str, matched: Range<usize>) -> Self {
        let (said, matched) = cut_around(said, matched, SAID_CAP);
        Self {
            word,
            said,
            matched,
        }
    }
}

impl ReportLine {
    /// The agent's own words for `words[held]`, with where `words[matched]` stands in them.
    fn said(&self, held: Range<usize>, matched: Range<usize>) -> (String, Range<usize>) {
        if self.block {
            // A block is quoted by its lines: a match by the line that held it, which is the
            // match, and an item by the block's first line.
            let line = if matched.is_empty() {
                0
            } else {
                self.from.get(matched.start).copied().unwrap_or(0)
            };
            let line = self.shown.get(line..).unwrap_or("");
            let line = line.split('\n').next().unwrap_or("");
            let (said, _) = plain(line, 0..line.len(), 0..0, None);
            let at = if matched.is_empty() {
                0..0
            } else {
                0..said.len()
            };
            return (said, at);
        }
        let (said, at) = plain(
            &self.shown,
            self.shown_at(held.clone()),
            self.shown_match(matched),
            Some(&self.from[..]),
        );
        if said.is_empty() {
            // Nothing the agent wrote stands for it (a lone tick): the words read do, but for a
            // code span's placeholder, which the agent never wrote.
            let words: Vec<&str> = self.words[held]
                .split_whitespace()
                .filter(|w| *w != CODE)
                .collect();
            return (words.join(" "), 0..0);
        }
        (said, at)
    }
}

/// `shown[range]` as said: markdown's emphasis and code ticks dropped and its spaces closed up,
/// with where `matched`, bytes of `shown`, stands in it. Given `from` (for each byte the reader
/// read, the byte of `shown` it came from), the underscores and box marks outside code that
/// the reader dropped go too.
fn plain(
    shown: &str,
    range: Range<usize>,
    matched: Range<usize>,
    from: Option<&[usize]>,
) -> (String, Range<usize>) {
    let dropped = |at: usize, len: usize| {
        from.is_some_and(|from| {
            let first = from.partition_point(|&f| f < at);
            from.get(first).is_none_or(|&f| f >= at + len)
        })
    };
    let mut code = shown[..range.start].matches('`').count() % 2 == 1;
    let mut out = String::with_capacity(range.len());
    let (mut start, mut end) = (None, None);
    let mut space = false;
    let mut past = range.start;
    for (at, c) in shown[range.clone()].char_indices() {
        let at = range.start + at;
        if at < past {
            continue;
        }
        if c == '`' {
            code = !code;
            continue;
        }
        if c == '*' && !code {
            continue;
        }
        if !code && from.is_some() {
            let mark = if c == '_' {
                Some(1)
            } else if c == '[' || !c.is_ascii() {
                UNCHECKED_BOXES
                    .iter()
                    .chain(CHECKED_BOXES)
                    .filter(|b| shown[at..].starts_with(**b))
                    .map(|b| b.len())
                    .max()
            } else {
                None
            };
            if let Some(len) = mark.filter(|&len| dropped(at, len)) {
                past = at + len;
                continue;
            }
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if std::mem::take(&mut space) {
            out.push(' ');
        }
        if matched.contains(&at) {
            start.get_or_insert(out.len());
            end = Some(out.len() + c.len_utf8());
        }
        out.push(c);
    }
    let at = start.zip(end).map_or(0..0, |(s, e)| s..e);
    (out, at)
}

/// `said` cut to at most `cap` bytes around `matched`, which it keeps whole, at a word's edge
/// where one stands outside the match, with `…` at each cut end. A match longer than the room
/// keeps its start and as many of its whole words as fit.
pub(super) fn cut_around(said: &str, matched: Range<usize>, cap: usize) -> (String, Range<usize>) {
    const CUT: &str = "…";
    let matched = matched.start.min(said.len())..matched.end.min(said.len());
    if said.len() <= cap {
        return (said.to_string(), matched);
    }
    let room = cap.saturating_sub(CUT.len());
    let (from, to) = if matched.end <= room {
        (0, room)
    } else if said.len() - matched.start <= room {
        (said.len() - room, said.len())
    } else {
        let room = cap.saturating_sub(2 * CUT.len());
        let spare = room.saturating_sub(matched.len());
        let from = matched.start - (spare / 2).min(matched.start);
        (from, (from + room).min(said.len()))
    };
    let mut from = from;
    while !said.is_char_boundary(from) {
        from += 1;
    }
    let mut to = to;
    while !said.is_char_boundary(to) {
        to -= 1;
    }
    if from > 0
        && let Some(space) = said[from..matched.start.max(from)].find(' ')
    {
        from += space + 1;
    }
    // The cut's end goes back to a word's edge: past a match that fits, and inside one that
    // does not, which then keeps its start and whole words of it.
    let (floor, inside) = if matched.end <= to {
        (matched.end, false)
    } else {
        (matched.start.max(from), true)
    };
    if to < said.len()
        && let Some(space) = said
            .get(floor..to)
            .and_then(|s| s.rfind(' '))
            .filter(|&s| !inside || s > 0)
    {
        to = floor + space;
    }
    let mut out = String::with_capacity(cap);
    if from > 0 {
        out.push_str(CUT);
    }
    let shift = out.len();
    out.push_str(&said[from..to]);
    if to < said.len() {
        out.push_str(CUT);
    }
    let start = (matched.start.max(from) - from + shift).min(out.len());
    let end = (matched.end.min(to).max(from) - from + shift).clamp(start, out.len());
    (out, start..end)
}

/// What a report's own words leave open, each with the sentence it stood in, in the order found.
fn open_work_found(tail: &str, land: Option<&[LandFamily]>, defense: bool) -> Vec<Found> {
    let lines = report_lines(tail, land.is_some());
    let said_main = land.is_some()
        && (!defense
            || lines
                .iter()
                .any(|l| main_named(&l.words) || main_claimed(&l.words)));
    let mut found: Vec<Found> = Vec::new();
    let mut i = 0;
    while let Some(line) = lines.get(i) {
        if line.unchecked {
            found.push(Found::to_do(line));
        }
        let words = line.words.as_str();
        let starts = sentence_starts(words);
        let end = |k: usize| starts.get(k + 1).copied().unwrap_or(words.len());
        let mut next = i + 1;
        for k in 0..starts.len() {
            let held = starts[k]..end(k);
            let sentence = &words[held.clone()];
            let head = open_head(sentence)
                .or_else(|| land.and_then(|_| head_in(sentence, LAND_HEADS)))
                .or_else(|| {
                    (k == 0 && line.heading)
                        .then(|| heading_head(sentence))
                        .flatten()
                });
            let Some((head, rest)) = head else {
                let (line_on, len) = (&words[starts[k]..], end(k) - starts[k]);
                let hit = match land {
                    None => open_status(line_on, len, &STATUS_PHRASES),
                    Some(families) => {
                        land_status(line_on, len, &words[..starts[k]], families, said_main)
                    }
                };
                if let Some(hit) = hit {
                    let at = starts[k] + hit.at..starts[k] + hit.at + hit.len;
                    found.push(Found::of(line, held, hit.word, at));
                }
                continue;
            };
            let head_at = starts[k]..starts[k] + head.len();
            let nil = |item: &str| {
                if land.is_some() {
                    nil_list(item)
                } else {
                    negated(item)
                }
            };
            let (qualifier, inline) = qualified(rest);
            let cleared = nil(qualifier);
            let one_sentence = sentence_starts(inline).len() == 1;
            if inline.is_empty() || (one_sentence && inline.ends_with(':')) {
                let mut j = i + 1;
                let mut items: Vec<&ReportLine> = Vec::new();
                while let Some(under) = lines.get(j) {
                    if under.block {
                        // Land's reading: a code block right under a bare
                        // head is its item (`Still to do:` over a command).
                        if items.is_empty() {
                            items.push(under);
                        }
                        break;
                    }
                    if under.separated
                        || open_head(&under.words).is_some()
                        || !(under.item || items.is_empty())
                    {
                        break;
                    }
                    items.push(under);
                    j += 1;
                }
                next = j;
                if !cleared && let Some(first) = items.iter().find(|item| !nil(&item.words)) {
                    found.push(Found::over(line, held, head, head_at, first));
                }
                if let Some(families) = land {
                    found.extend(items.iter().filter_map(|item| {
                        let hit = aside_status(&item.words, families, said_main)?;
                        Some(Found::of(item, 0..item.words.len(), hit.word, hit.range()))
                    }));
                }
                break;
            }
            if !cleared && !nil(inline) {
                found.push(Found::of(line, held.clone(), head, head_at));
            }
            if let Some(families) = land
                && let Some(hit) = aside_status(inline, families, said_main)
            {
                let base = starts[k] + offset_in(sentence, inline);
                let at = base + hit.at..base + hit.at + hit.len;
                found.push(Found::of(line, held, hit.word, at));
            }
        }
        i = next;
    }
    found
}

const UNCHECKED_BOXES: &[&str] = &[
    "[ ]", "[  ]", "[]", "[-]", "[~]", "☐", "⬜", "◻️", "◻", "□", "🔲", "⏳", "⌛", "🚧", "❌",
    "✗", "✘", "✕", "✖", "✖️", "⚠️", "⚠",
];

const CHECKED_BOXES: &[&str] = &[
    "[x]", "[X]", "[✓]", "[✔]", "[v]", "☑️", "☑", "✅", "✔️", "✔", "✓",
];

const UNCHECKED: &str = "unchecked box";

const CODE: &str = "codespan";

const CODE_MARKS: &[(&str, &str)] = &[
    ("todo!(", "todo"),
    ("unimplemented!(", "todo"),
    ("#[ignore", "check ignored"),
    ("continue-on-error", "check ignored"),
    ("allow(", "lint allowed"),
    ("stash@{", "stash"),
    ("--dry-run", "dry run"),
];

fn code_words(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let lower = lower.as_str();
    if let Some(word) = code_work(lower).first() {
        return (*word).to_string();
    }
    let whole = lower.trim().trim_matches(|c: char| !c.is_alphanumeric());
    if let Some(word) = SPAN_STATUS
        .iter()
        .find(|(said, _)| *said == whole)
        .map(|(_, word)| *word)
    {
        return word.to_string();
    }
    if let Some(word) = output_status(raw).first() {
        return (*word).to_string();
    }
    let mut tokens = lower.split_whitespace();
    if let (Some(one), None) = (tokens.next(), tokens.next()) {
        // A refspec names where it pushes (`HEAD:main` is main).
        if let Some((_, dst)) = one.split_once(':')
            && matches!(
                dst,
                "main" | "master" | "refs/heads/main" | "refs/heads/master"
            )
        {
            return "main".to_string();
        }
        let named = one.contains(['-', '_'])
            && one
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        if code_ref(one)
            || matches!(one, "main" | "master")
            || BRANCHES.contains(&one)
            || (named && one.starts_with(|c: char| c.is_ascii_alphabetic()))
        {
            return one.to_string();
        }
    }
    let mut out = CODE.to_string();
    for t in lower.split_whitespace().filter(|t| code_ref(t)) {
        out.push(' ');
        out.push_str(t);
        if t.ends_with("..") {
            out.push_str("head");
        }
    }
    out
}

const SPAN_NOUNS: &[&str] = &[
    "line", "lines", "row", "rows", "label", "labels", "word", "words", "status", "marker",
    "markers", "entry", "entries", "prefix", "column", "columns", "case", "cases", "state",
    "states", "outcome", "outcomes", "result", "results", "output", "colour", "color", "text",
];

const SPAN_STATUS: &[(&str, &str)] = &[
    ("skip", "skipped"),
    ("skips", "skipped"),
    ("skipped", "skipped"),
    ("fail", "check failed"),
    ("failed", "check failed"),
    ("failing", "check failed"),
    ("not run", "not run"),
    ("pending", "pending"),
];

fn output_status(raw: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let t = raw.trim();
    let mut tokens = t.split_whitespace();
    let first = tokens
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| matches!(c, '[' | ']' | ':' | '-'));
    let second = tokens.next();
    let named = second.is_some_and(|w| w.starts_with(|c: char| c.is_alphanumeric()));
    if matches!(first, "SKIP" | "SKIPPED") || (first.eq_ignore_ascii_case("skip") && named) {
        out.push("skipped");
    }
    if matches!(first, "FAIL" | "FAILED") || t.contains("test result: FAILED") {
        out.push("check failed");
    }
    if first == "NOT" && second == Some("RUN") {
        out.push("not run");
    }
    let words: Vec<String> = t
        .split_whitespace()
        .map(|w| bare(w).to_lowercase())
        .collect();
    for pair in words.windows(2) {
        if count(&pair[0]).is_some_and(|n| n > 0) {
            match pair[1].as_str() {
                "skip" | "skips" | "skipped" => out.push("skipped"),
                "fail" | "fails" | "failed" | "failures" | "failing" => out.push("check failed"),
                _ => {}
            }
        }
    }
    if t.starts_with("## ") && t.contains("[ahead ") {
        out.push("commits ahead");
    }
    let bytes = raw.trim_end().as_bytes();
    let state = |b: u8| matches!(b, b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U');
    if raw.starts_with("?? ") && raw.len() > 3 {
        out.push("untracked");
    } else if bytes.len() > 3
        && state(bytes[0])
        && state(bytes[1])
        && bytes[..2] != *b"  "
        && bytes[2] == b' '
        && bytes[3] != b' '
        && raw[3..].contains(['.', '/'])
    {
        out.push("modified files");
    }
    out.dedup();
    out
}

fn code_ref(t: &str) -> bool {
    let plain = t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'));
    let file = t.rsplit('/').next().is_some_and(|last| {
        last.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        })
    });
    plain
        && t.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !t.ends_with('/')
        && (t.contains("..") || (t.contains('/') && !file))
}

fn table_row(t: &str) -> Option<String> {
    let inner = t.strip_prefix('|')?.trim_end().strip_suffix('|')?;
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    match cells.as_slice() {
        [label, item] if !label.is_empty() && !item.is_empty() => Some(format!("{label}: {item}")),
        _ => None,
    }
}

fn code_work(raw: &str) -> Vec<&'static str> {
    let lower = raw.to_lowercase();
    let mut out: Vec<&'static str> = CODE_MARKS
        .iter()
        .filter(|(mark, _)| lower.contains(mark))
        .map(|(_, word)| *word)
        .collect();
    if lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| matches!(w, "todo" | "fixme"))
    {
        out.push("todo");
    }
    out
}

struct ReportLine {
    separated: bool,
    block: bool,
    unchecked: bool,
    item: bool,
    heading: bool,
    capital: bool,
    words: String,
    /// The line as the agent wrote it, past its marks (the quote, the bullet, the box, the
    /// heading's hashes): the text `words` was read from. A code block's is its first line and
    /// then each line it read a word in, a `\n` before each.
    shown: String,
    /// For each byte of `words`, the byte of `shown` it was read from; never decreasing.
    from: Vec<usize>,
}

impl ReportLine {
    fn of(raw: &str, land: bool) -> Self {
        let lead = raw.trim_start();
        let marks = lead.trim_start_matches('#');
        let heading = lead.starts_with('#') && !(land && !marks.starts_with(char::is_whitespace));
        let t = raw.trim().trim_start_matches(|c: char| {
            c == '>' || (c == '#' && heading) || (c == '#' && !land) || c.is_whitespace()
        });
        let marked = ["- ", "* ", "+ ", "• ", "◦ ", "‣ "]
            .iter()
            .find_map(|m| t.strip_prefix(m))
            .or_else(|| {
                let digits = t.bytes().take_while(u8::is_ascii_digit).count();
                let rest = t.get(digits..).filter(|_| (1..=3).contains(&digits))?;
                rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
            });
        let (item, t) = marked.map_or((false, t), |r| (true, r.trim_start()));
        let boxed = UNCHECKED_BOXES.iter().chain(CHECKED_BOXES).find(|b| {
            land && t
                .strip_prefix(**b)
                .is_some_and(|r| r.is_empty() || r.starts_with(' '))
        });
        let mut unchecked = boxed.is_some_and(|b| UNCHECKED_BOXES.contains(b));
        let t = boxed.map_or(t, |b| t[b.len()..].trim_start());
        let cells = land.then(|| table_row(t)).flatten();
        let t = cells.as_deref().unwrap_or(t);
        let capital = t
            .chars()
            .find(|&c| c != '*' && c != '_')
            .is_some_and(char::is_uppercase);
        // Each step keeps, beside its text, the byte of `t` each of its bytes came from.
        let mut flat = String::with_capacity(t.len());
        let mut flat_from: Vec<usize> = Vec::with_capacity(t.len());
        let mut code = false;
        let mut span = String::new();
        let mut span_at = 0;
        for (i, c) in t.char_indices() {
            match c {
                '`' => {
                    if code && land {
                        flat.push(' ');
                        flat_from.resize(flat.len(), span_at);
                        let named = t[i + 1..]
                            .split_whitespace()
                            .next()
                            .map(bare)
                            .is_some_and(|w| SPAN_NOUNS.contains(&w.to_lowercase().as_str()));
                        let words = code_words(&span);
                        if named && SPAN_STATUS.iter().any(|(_, w)| *w == words) {
                            flat.push_str(CODE);
                            flat_from.resize(flat.len(), span_at);
                        } else {
                            flat.push_str(&words);
                            flat_from.extend(span_words_from(&span, span_at, &words));
                        }
                        span.clear();
                    }
                    if !code {
                        span_at = i;
                    }
                    code = !code;
                    flat.push(' ');
                }
                _ if code => span.push(c),
                '*' => {}
                '’' => flat.push('\''),
                c => flat.extend(c.to_lowercase()),
            }
            flat_from.resize(flat.len(), i);
        }
        if land {
            let mut padded = format!(" {flat} ");
            let mut padded_from = Vec::with_capacity(padded.len());
            padded_from.push(0);
            padded_from.append(&mut flat_from);
            padded_from.push(t.len());
            for b in UNCHECKED_BOXES.iter().chain(CHECKED_BOXES) {
                let mark = format!(" {}", b.to_lowercase());
                let mut from = 0;
                while let Some(found) = padded[from..].find(&mark).map(|i| from + i) {
                    let past = found + mark.len();
                    if padded[past..].starts_with([' ', '.', ',', ';', ':', ')', '!']) {
                        unchecked |= UNCHECKED_BOXES.contains(b);
                        padded.replace_range(found..past, " ");
                        padded_from.drain(found + 1..past);
                        from = found;
                    } else {
                        from = past;
                    }
                }
            }
            (flat, flat_from) = replaced(&padded, &padded_from, "#[ignore]", " check ignored ");
        }
        let (flat, flat_from) = without_underscore_emphasis(&flat, &flat_from);
        let mut words = String::with_capacity(flat.len());
        let mut from: Vec<usize> = Vec::with_capacity(flat.len());
        let mut after_not = false;
        let mut gap = 0;
        for (at, token) in tokens_at(&flat) {
            if after_not && matches!(token, "yet" | "been") {
                continue;
            }
            if !words.is_empty() {
                words.push(' ');
                from.push(gap);
            }
            let source = &flat_from[at..at + token.len()];
            let spelled = token.split_once("n't").filter(|(verb, rest)| {
                CONTRACTED.contains(verb) && !rest.contains(char::is_alphanumeric)
            });
            if let Some((verb, rest)) = spelled {
                words.push_str(verb);
                from.extend_from_slice(&source[..verb.len()]);
                words.push_str(" not");
                from.resize(words.len(), source[verb.len()]);
                words.push_str(rest);
                from.extend_from_slice(&source[verb.len() + "n't".len()..]);
                after_not = rest.is_empty();
            } else if let Some((n, m)) = land.then(|| ratio(token)).flatten() {
                words.push_str(&format!("{n} of {m}"));
                from.resize(words.len(), source[0]);
                let digits = token.trim_end_matches(|c: char| !c.is_ascii_digit()).len();
                words.push_str(&token[digits..]);
                from.extend_from_slice(&source[digits..]);
                after_not = false;
            } else {
                words.push_str(token);
                from.extend_from_slice(source);
                after_not = token == "not";
            }
            gap = flat_from.get(at + token.len()).copied().unwrap_or(t.len());
        }
        let mut shown = t.to_string();
        if from.len() != words.len() {
            // A byte the steps above lost would misplace every quote after it. The tests read
            // every corpus row with this held; a build quotes such a line in the words it read.
            if cfg!(test) {
                panic!("{t:?} reads {words:?} from {from:?}");
            }
            shown.clone_from(&words);
            from = (0..words.len()).collect();
        }
        Self {
            separated: false,
            block: false,
            unchecked,
            item,
            heading,
            capital,
            words,
            shown,
            from,
        }
    }

    /// The bytes of `shown` that `words[range]` was read from.
    fn shown_at(&self, range: Range<usize>) -> Range<usize> {
        let at = |b: usize| {
            let mut at = self
                .from
                .get(b)
                .copied()
                .unwrap_or(self.shown.len())
                .min(self.shown.len());
            while !self.shown.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        let start = at(range.start);
        start..at(range.end).max(start)
    }

    /// The bytes of `shown` that the match `words[range]` was read from: from its first byte's
    /// source to one character past its last byte's, so a mark the reader dropped after it
    /// (`_still open_`) stays out. Where its last word stands for a token of the agent's that
    /// reads otherwise (`3/5` read as `3 of 5`, `n't` as `not`, a code span as its status),
    /// the match runs to that token's end.
    fn shown_match(&self, range: Range<usize>) -> Range<usize> {
        let at = self.shown_at(range.clone());
        let last = self
            .words
            .get(range.clone())
            .and_then(|m| m.char_indices().next_back());
        let Some((last, read)) = last.map(|(b, c)| (range.start + b, c)) else {
            return at;
        };
        let source = self.shown_at(last..last).start;
        let Some(said) = self.shown[source..].chars().next() else {
            return at;
        };
        let rewritten = !(said.to_lowercase().any(|c| c == read) || (said == '’' && read == '\''))
            || (last > range.start && self.from.get(last - 1) == self.from.get(last));
        let end = if !rewritten {
            source + said.len_utf8()
        } else if said == '`' {
            // A code span read as a word it does not hold: the whole span.
            self.shown[source + 1..]
                .find('`')
                .map_or(self.shown.len(), |close| source + close + 2)
        } else {
            let token = &self.shown[source..];
            let token = &token[..token.find(char::is_whitespace).unwrap_or(token.len())];
            source
                + token
                    .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '_', '*'])
                    .len()
        };
        at.start..end.max(at.start)
    }

    /// Joins `line`, which carries this one on, to this one.
    fn join(&mut self, line: Self) {
        self.unchecked |= line.unchecked;
        self.words.push(' ');
        self.words.push_str(&line.words);
        self.from.push(self.shown.len());
        self.shown.push(' ');
        let base = self.shown.len();
        self.shown.push_str(&line.shown);
        self.from.extend(line.from.iter().map(|f| f + base));
    }
}

/// Where each byte of the words a code span read ([`code_words`]) stands in its line: a word
/// the span holds where it holds it, and any other (`codespan`, `skipped`) at the opening `tick`.
fn span_words_from(span: &str, tick: usize, words: &str) -> Vec<usize> {
    let lower = span.to_lowercase();
    let mut from = Vec::with_capacity(words.len());
    let (mut past, mut last) = (0, tick);
    for (at, token) in tokens_at(words) {
        from.resize(at, last);
        let held = (lower.len() == span.len())
            .then(|| lower[past..].find(token))
            .flatten();
        if let Some(p) = held {
            let start = tick + 1 + past + p;
            from.extend(start..start + token.len());
            past += p + token.len();
            last = start + token.len();
        } else {
            from.resize(at + token.len(), last);
        }
    }
    from.resize(words.len(), last);
    from
}

/// `text`'s whitespace-separated tokens, as `split_whitespace` gives them, each with its byte.
fn tokens_at(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut past = 0;
    std::iter::from_fn(move || {
        let rest = &text[past..];
        let start = past + (rest.len() - rest.trim_start().len());
        if start >= text.len() {
            return None;
        }
        let len = text[start..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - start);
        past = start + len;
        Some((start, &text[start..past]))
    })
}

/// `str::replace` that keeps, for each byte, where it came from.
fn replaced(text: &str, from: &[usize], pattern: &str, with: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut out_from = Vec::with_capacity(from.len());
    let mut kept = 0;
    for (at, _) in text.match_indices(pattern) {
        out.push_str(&text[kept..at]);
        out_from.extend_from_slice(&from[kept..at]);
        out.push_str(with);
        out_from.resize(out.len(), from[at]);
        kept = at + pattern.len();
    }
    out.push_str(&text[kept..]);
    out_from.extend_from_slice(&from[kept..]);
    (out, out_from)
}

fn without_underscore_emphasis(text: &str, from: &[usize]) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut out_from = Vec::with_capacity(from.len());
    let mut keep = |range: Range<usize>| {
        out.push_str(&text[range.clone()]);
        out_from.extend_from_slice(&from[range]);
    };
    let mut kept = 0;
    let mut cursor = 0;
    let mut open = None;
    while let Some(offset) = text[cursor..].find('_') {
        let at = cursor + offset;
        let width = text[at..].bytes().take_while(|&b| b == b'_').count();
        cursor = at + width;
        let before = text[..at].chars().next_back();
        let after = text[cursor..].chars().next();
        let closes =
            before.is_some_and(|c| !c.is_whitespace()) && after.is_none_or(|c| !word_char(c));
        if let Some((start, marks)) = open
            && marks == width
            && closes
        {
            keep(kept..start);
            keep(start + marks..at);
            kept = cursor;
            open = None;
        } else if open.is_none()
            && before.is_none_or(|c| !word_char(c))
            && after.is_some_and(|c| !c.is_whitespace())
        {
            open = Some((at, width));
        }
    }
    keep(kept..text.len());
    (out, out_from)
}

fn ratio(token: &str) -> Option<(u32, u32)> {
    let t = token.trim_end_matches(['.', ',', ';', ':', ')']);
    let (n, m) = t.split_once('/')?;
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !(digits(n) && digits(m)) {
        return None;
    }
    let (n, m): (u32, u32) = (n.parse().ok()?, m.parse().ok()?);
    (n < m && m <= 1000).then_some((n, m))
}

fn table_header(raw: &str, next: Option<&str>) -> bool {
    raw.trim_start().starts_with('|') && next.is_some_and(table_rule)
}

/// A table's rule row (`|---|:--|`).
fn table_rule(raw: &str) -> bool {
    let t = raw.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn report_lines(tail: &str, land: bool) -> Vec<ReportLine> {
    let mut out: Vec<ReportLine> = Vec::new();
    let mut fence = None;
    let mut separated = false;
    let lines: Vec<&str> = tail.lines().collect();
    for (i, raw) in lines.iter().copied().enumerate() {
        if let Some((mark, width)) = fence {
            if needs::code_fence(raw)
                .is_some_and(|(m, n, rest)| m == mark && n >= width && rest.trim().is_empty())
            {
                fence = None;
            } else if let Some(block) = out.last_mut().filter(|b| land && b.block) {
                // A block shows its first line, and then, a line apiece, each line it reads a
                // word in: the word maps to its line, which is what a find of it quotes.
                let text = raw.trim();
                let first = block.shown.is_empty();
                if first {
                    text.clone_into(&mut block.shown);
                }
                let mut held = first.then_some(0);
                for word in code_work(raw).into_iter().chain(output_status(raw)) {
                    if !block.words.split(' ').any(|w| w == word) {
                        let at = *held.get_or_insert_with(|| {
                            block.shown.push('\n');
                            let at = block.shown.len();
                            block.shown.push_str(text);
                            at
                        });
                        block.words.push(' ');
                        block.words.push_str(word);
                        block.from.resize(block.words.len(), at);
                    }
                }
            }
            continue;
        }
        if let Some((mark, width, _)) = needs::code_fence(raw) {
            fence = Some((mark, width));
            separated = true;
            if land {
                let mut block = ReportLine::of(CODE, true);
                block.block = true;
                block.separated = true;
                block.shown.clear();
                block.from.fill(0);
                out.push(block);
            }
            continue;
        }
        if is_finished_line(raw) || (land && table_header(raw, lines.get(i + 1).copied())) {
            continue;
        }
        let mut line = ReportLine::of(raw, land);
        if line.words.is_empty() && !line.unchecked {
            continue;
        }
        if land && table_rule(raw) {
            // A table's rule reads as words but shows none, so a quote across it keeps to the
            // rows.
            line.shown.clear();
            line.from.fill(0);
        }
        line.separated = std::mem::take(&mut separated);
        match out.last_mut() {
            Some(above) if !line.separated && !line.item && !line.capital => above.join(line),
            _ => out.push(line),
        }
    }
    out
}

fn sentence_starts(words: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (at, c) in words.char_indices() {
        if matches!(c, '.' | ';' | '!' | '?') && words[at + 1..].starts_with(' ') {
            starts.push(at + 2);
        }
    }
    starts.retain(|&s| s < words.len() || s == 0);
    starts
}

fn open_head(text: &str) -> Option<(&'static str, &str)> {
    head_in(text, OPEN_HEADS)
}

const LAND_HEADS: &[&str] = &[
    "blockers",
    "blocker",
    "blocking",
    "known issues",
    "known issue",
    "open questions",
    "unresolved",
    "fixmes",
    "fixme",
    "gaps",
    "possible improvements",
    "potential improvements",
    "further improvements",
    "future improvements",
    "improvement ideas",
    "worth considering",
    "optional",
    "optional follow-up",
    "optional follow-ups",
    "ideas",
    "one idea",
    "an idea",
    "suggestions",
    "suggestion",
    "suggested follow-ups",
    "possible follow-ups",
    "nice to have",
    "nice to haves",
    "nice-to-haves",
    "for later",
    "owner",
    "for the owner",
    "for you",
    "action for you",
    "actions for you",
    "action needed",
    "action required",
    "ask",
    "asks",
    "one ask",
    "an ask",
    "my ask",
    "one request",
    "a request",
    "your decision",
    "your input",
    "your action",
    "your actions",
    "decision needed",
    "decision required",
    "owner action",
    "owner actions",
    "owner step",
    "owner steps",
    "owner action needed",
    "owner action required",
    "blocking on you",
    "next session",
    "next time",
    "later",
    "regression",
    "regressions",
];

fn head_in<'t>(text: &'t str, heads: &[&'static str]) -> Option<(&'static str, &'t str)> {
    let ends = |rest: &str| {
        rest.is_empty()
            || rest.starts_with([':', '('])
            || [" -", " –", " —", " ("].iter().any(|p| rest.starts_with(p))
    };
    heads
        .iter()
        .filter_map(|&head| {
            let rest = text.strip_prefix(head)?;
            let rest = OPEN_NOUNS
                .iter()
                .find_map(|noun| {
                    rest.strip_prefix(' ')?
                        .strip_prefix(noun)
                        .filter(|r| ends(r))
                })
                .unwrap_or(rest);
            if ends(rest) {
                Some((head, rest))
            } else {
                head_tail(rest).map(|rest| (head, rest))
            }
        })
        .max_by_key(|(head, _)| head.len())
}

fn head_tail(rest: &str) -> Option<&str> {
    let tail = [", ", " from ", " for "]
        .iter()
        .find_map(|p| rest.strip_prefix(p))?;
    let colon = tail.find(':').filter(|&at| at <= HEAD_TAIL)?;
    Some(&tail[colon..])
}

fn heading_head(text: &str) -> Option<(&'static str, &str)> {
    OPEN_HEADS
        .iter()
        .filter(|&&head| text.strip_prefix(head).is_some_and(|r| r.starts_with(',')))
        .max_by_key(|head| head.len())
        .map(|&head| (head, ""))
}

fn qualified(rest: &str) -> (&str, &str) {
    let rest = rest.trim_start();
    let (qualifier, after) = rest
        .strip_prefix('(')
        .and_then(|r| r.split_once(')'))
        .unwrap_or(("", rest));
    let item = after
        .trim_start_matches(|c: char| matches!(c, ':' | '-' | '–' | '—') || c.is_whitespace())
        .trim_end();
    (qualifier, item)
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

#[derive(Clone, Copy)]
struct Phrases {
    table: &'static [&'static str],
    first: [bool; 256],
}

impl Phrases {
    const fn of(table: &'static [&'static str]) -> Self {
        let mut first = [false; 256];
        let mut i = 0;
        while i < table.len() {
            first[table[i].as_bytes()[0] as usize] = true;
            i += 1;
        }
        Self { table, first }
    }
}

const STATUS_PHRASES: Phrases = Phrases::of(OPEN_STATUS);

fn phrases_in<'t>(
    text: &'t str,
    phrases: &'t Phrases,
) -> impl Iterator<Item = (usize, &'static str)> + 't {
    let mut before = None;
    text.char_indices()
        .filter_map(move |(at, c)| {
            let starts = word_char(c)
                && !before.is_some_and(word_char)
                && phrases.first[usize::from(text.as_bytes()[at])];
            before = Some(c);
            starts.then_some(at)
        })
        .flat_map(move |at| {
            phrases
                .table
                .iter()
                .copied()
                .filter(move |phrase| {
                    text.as_bytes().get(at) == phrase.as_bytes().first()
                        && text[at..].starts_with(phrase)
                        && !text[at + phrase.len()..]
                            .chars()
                            .next()
                            .is_some_and(word_char)
                })
                .map(move |phrase| (at, phrase))
        })
}

fn negated(item: &str) -> bool {
    item.trim_start_matches(|c: char| !c.is_alphanumeric())
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | ';' | ':' | '!' | ')'))
        .next()
        .is_some_and(|first| OPEN_NEGATIONS.contains(&first))
        || item.starts_with("n/a")
}

/// Where a reader found open work in a sentence: its word, and the bytes that matched.
#[derive(Clone, Copy, Debug)]
struct Hit {
    word: &'static str,
    at: usize,
    len: usize,
}

impl Hit {
    const fn range(self) -> Range<usize> {
        self.at..self.at + self.len
    }
}

fn open_status(line: &str, len: usize, phrases: &Phrases) -> Option<Hit> {
    let sentence = &line[..len];
    let stands: Vec<(usize, &'static str)> = phrases_in(sentence, phrases).collect();
    phrases.table.iter().copied().find_map(|word| {
        stands
            .iter()
            .filter(|&&(_, w)| w == word)
            .find(|&&(at, _)| {
                let clause = sentence[..at]
                    .rsplit([',', ';', ':', '(', ')', '—', '–'])
                    .next()
                    .unwrap_or("");
                let before: Vec<&str> = clause.split_whitespace().collect();
                let prev = before.last().copied();
                !before.iter().any(|w| OPEN_NEGATIONS.contains(w))
                    && !(word.starts_with("not ") && prev.is_some_and(|p| OPEN_MODALS.contains(&p)))
                    && !(word == "skipped"
                        && prev.is_some_and(|p| ["is", "are", "be"].contains(&p)))
                    && !quoted(sentence, at)
                    && !nothing_to_do(&line[at + word.len()..])
            })
            .map(|&(at, _)| Hit {
                word,
                at,
                len: word.len(),
            })
    })
}

struct Stand<'f> {
    at: usize,
    word: &'static str,
    family: Option<&'f LandFamily>,
    rank: (usize, usize),
    len: usize,
}

impl Stand<'_> {
    const fn end(&self) -> usize {
        self.at + self.len
    }

    fn reads(&self) -> Reads {
        self.family.map_or(Reads::Anywhere, |f| f.reads)
    }

    fn settled_by(&self) -> &'static [&'static str] {
        self.family.map_or(STATUS_BY, |f| f.settled_by)
    }

    fn lands_by_place(&self) -> bool {
        match self.family {
            Some(f) => f.settled_by == LANDS || f.settled_by == SHIPS,
            None => matches!(
                self.word,
                "follow-up"
                    | "follow-ups"
                    | "still open"
                    | "remains open"
                    | "left open"
                    | "left for"
            ),
        }
    }
}

fn land_status(
    line: &str,
    len: usize,
    prior: &str,
    families: &[LandFamily],
    said_main: bool,
) -> Option<Hit> {
    let s = &line[..len];
    let rank =
        |table: &[&str], word: &str| table.iter().position(|w| *w == word).unwrap_or(usize::MAX);
    let mut stands: Vec<Stand<'_>> = phrases_in(s, &STATUS_PHRASES)
        .map(|(at, word)| Stand {
            at,
            word,
            family: None,
            rank: (0, rank(OPEN_STATUS, word)),
            len: word.len(),
        })
        .collect();
    for (k, family) in families.iter().enumerate() {
        stands.extend(phrases_in(s, &family.phrases).map(|(at, word)| Stand {
            at,
            word,
            family: Some(family),
            rank: (k + 1, rank(family.rows, word)),
            len: word.len(),
        }));
        stands.extend(
            pattern_stands(s, family.reads)
                .into_iter()
                .map(|(at, len, word)| Stand {
                    at,
                    word,
                    family: Some(family),
                    rank: (k + 1, 0),
                    len,
                }),
        );
    }
    stands.sort_by_key(|st| st.rank);
    let open = stands
        .iter()
        .find(|st| land_open(line, s, (prior, said_main), st, &stands))
        .map(|st| Hit {
            word: st.family.and_then(|f| f.says).unwrap_or(st.word),
            at: st.at,
            len: st.len,
        });
    if !asks(s) {
        return open;
    }
    let leftover = match stated(s) {
        Some(t) if open.is_some() && reply(&line[len..]) == Some(Answer::Yes) => {
            land_status(&t, t.len(), prior, families, said_main).is_some()
        }
        _ => open.is_some(),
    };
    let mark = s.trim_end().len() - 1;
    question_open(s, &line[len..], leftover).then(|| {
        open.unwrap_or(Hit {
            word: QUESTION,
            at: mark,
            len: 1,
        })
    })
}

fn stated(s: &str) -> Option<String> {
    let t = s.trim_end().strip_suffix('?')?;
    let head = t.rfind(':').map_or(0, |i| i + 1);
    let words: Vec<&str> = t[head..].split_whitespace().collect();
    let state = |w: &str| {
        let w = bare(w);
        verbish(w) || matches!(w, "empty" | "clean" | "gone" | "green" | "red")
    };
    let place = |k: usize| {
        let rest: Vec<&str> = words[k..].iter().map(|w| bare(w)).collect();
        onto_main(&rest)
    };
    let first = bare(words.first()?);
    let (aux, subject, p) = if matches!(first, "is" | "are" | "was" | "were") {
        let p = (2..words.len()).find(|&k| state(words[k]) || place(k))?;
        (first, 1, p)
    } else {
        let adjective = |w: &str| {
            let w = bare(w);
            past_form(w) || matches!(w, "empty" | "clean" | "gone" | "green" | "red")
        };
        let p = (1..words.len()).find(|&k| adjective(words[k]) || place(k))?;
        if words[1..p].iter().any(|w| verbish(bare(w))) {
            return None;
        }
        ("is", 0, p)
    };
    Some(format!(
        "{} {} {aux} {}.",
        t[..head].trim_end(),
        words[subject..p].join(" "),
        words[p..].join(" ")
    ))
}

fn pattern_stands(s: &str, reads: Reads) -> Vec<(usize, usize, &'static str)> {
    if matches!(reads, Reads::Absent) {
        let words: Vec<(usize, &str)> = words_at(s, 0, s.len()).collect();
        return words
            .windows(3)
            .filter(|w| {
                bare(w[0].1) == "no"
                    && bare(w[1].1).contains('-')
                    && !w[1].1.ends_with([',', ';', ':', '.'])
                    && matches!(bare(w[2].1), "test" | "tests")
            })
            .map(|w| {
                let at = w[0].0 + w[0].1.find("no").unwrap_or(0);
                let (raw, noun) = (w[2].1, bare(w[2].1));
                let end = w[2].0 + raw.find(noun).unwrap_or(0) + noun.len();
                (at, end - at, "no test")
            })
            .collect();
    }
    if !matches!(reads, Reads::Change | Reads::Percent) {
        return Vec::new();
    }
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    words_at(s, 0, s.len())
        .filter_map(|(p, raw)| {
            let lead = raw.len() - raw.trim_start_matches('(').len();
            let t = raw[lead..].trim_end_matches(['.', ',', ';', ':', ')', '!', '?']);
            let at = p + lead;
            match reads {
                Reads::Change => {
                    let number = t
                        .strip_prefix('#')
                        .or_else(|| t.strip_prefix('!'))
                        .or_else(|| t.strip_prefix("gh-"))
                        .or_else(|| {
                            ["/pull/", "/pulls/", "/merge_requests/"]
                                .iter()
                                .find_map(|p| t.split_once(p))
                                .filter(|(site, _)| site.contains("://"))
                                .map(|(_, n)| n.trim_end_matches('/'))
                        });
                    number
                        .filter(|n| digits(n))
                        .map(|_| (at, t.len(), CHANGE_NUMBER))
                }
                Reads::Percent => t
                    .strip_suffix('%')
                    .filter(|n| below_hundred(n))
                    .map(|_| (at, t.len(), PERCENT)),
                _ => None,
            }
        })
        .collect()
}
const QUESTION: &str = "a question";

const CHANGE_NUMBER: &str = "#n";

const PERCENT: &str = "%";

const COUNT_BESIDE_NONE: &str = "a count beside none";

fn asks(s: &str) -> bool {
    let t = s.trim_end();
    t.ends_with('?') && !quoted(t, t.len() - 1)
}

fn reply(rest: &str) -> Option<Answer> {
    let t = reply_text(rest);
    let end = t.len();
    let words: Vec<&str> = t
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let first = *words.first()?;
    let bare_word = t.strip_prefix(first).is_some_and(|r| {
        r.is_empty()
            || r.starts_with(['.', ',', ';', ':', '!', '—', '–', ')'])
            || [" —", " –", " - "].iter().any(|p| r.starts_with(p))
    });
    let empty = words
        .windows(2)
        .any(|w| matches!(w, ["is" | "are" | "was" | "were", "empty"]));
    Some(
        if bare_word
            && matches!(
                first,
                "yes" | "yep" | "yeah" | "yup" | "correct" | "confirmed" | "affirmative" | "indeed"
            )
        {
            let main_too = words.get(1) == Some(&"and")
                && main_claimed(&words[2..].join(" "))
                && !words
                    .iter()
                    .any(|w| negation(w) || UNSETTLED.contains(w) || DEFICITS.contains(w));
            if yes_tail(&words[1..]) || main_too {
                Answer::Yes
            } else {
                Answer::Said
            }
        } else if (bare_word
            && matches!(first, "no" | "nope" | "none" | "nothing" | "nil" | "zero"))
            || nil_words(&t[..end])
            || empty
        {
            Answer::No
        } else {
            Answer::Said
        },
    )
}

fn reply_text(rest: &str) -> &str {
    let t = rest.trim_start();
    let t = t
        .strip_prefix("a:")
        .or_else(|| t.strip_prefix("answer:"))
        .unwrap_or(t)
        .trim_start();
    &t[..sentence_starts(t).get(1).copied().unwrap_or(t.len())]
}

fn yes_tail(tail: &[&str]) -> bool {
    let mut k = 0;
    while k < tail.len() {
        let w = tail[k];
        let run = matches!(w, "run" | "build" | "job" | "pipeline")
            && tail
                .get(k + 1)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let every = matches!(w, "every" | "each")
            && tail
                .get(k + 1)
                .is_some_and(|n| matches!(*n, "lane" | "check" | "job" | "cell" | "one" | "test"));
        if run || every {
            k += 2;
        } else if YES_TAIL.contains(&w) || sha(w) {
            k += 1;
        } else {
            return false;
        }
    }
    true
}

const YES_TAIL: &[&str] = &[
    "as",
    "at",
    "on",
    "in",
    "to",
    "into",
    "the",
    "remote",
    "main",
    "origin/main",
    "upstream/main",
    "both",
    "all",
    "of",
    "them",
    "it",
    "is",
    "are",
    "merged",
    "pushed",
    "and",
];

fn sha(w: &str) -> bool {
    (6..=40).contains(&w.len())
        && w.bytes().all(|b| b.is_ascii_hexdigit())
        && w.bytes().any(|b| b.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Answer {
    Yes,
    No,
    Said,
}

const DEFICITS: &[&str] = &[
    "need",
    "needs",
    "needed",
    "required",
    "requires",
    "missing",
    "left",
    "pending",
    "open",
    "broken",
    "fail",
    "fails",
    "failed",
    "failing",
    "skipped",
    "flaky",
    "blocked",
    "outstanding",
    "unmerged",
    "unpushed",
    "uncommitted",
    "unlanded",
    "stale",
    "red",
    "deferred",
    "disabled",
    "waiting",
    "owed",
];

const UNLANDED_DEFICITS: &[&str] = &["unmerged", "unpushed", "uncommitted", "unlanded"];

fn question_open(s: &str, rest: &str, leftover: bool) -> bool {
    let q = s.rsplit_once(':').map_or(s, |(_, q)| q);
    let words: Vec<&str> = q
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let first = words.first().copied().unwrap_or("");
    let second = words.get(1).copied().unwrap_or("");
    let addressed = words.contains(&"or")
        || first == "shall"
        || words.windows(2).any(|w| {
            matches!(
                w,
                [
                    "should" | "shall" | "may" | "can" | "could" | "would" | "will" | "do" | "did",
                    "i" | "we" | "you"
                ] | ["want", "me"]
                    | ["like", "me"]
            )
        });
    if addressed {
        return true;
    }
    let Some(answer) = reply(rest) else {
        return true;
    };
    let wh = matches!(
        first,
        "what"
            | "what's"
            | "which"
            | "who"
            | "who's"
            | "whom"
            | "whose"
            | "when"
            | "where"
            | "where's"
            | "why"
            | "how"
    );
    let change = words
        .iter()
        .any(|w| CHANGE_NOUNS.contains(w) || w.starts_with('#'))
        || q.contains("pull request");
    let lands = |w: &[&str]| {
        w.iter().any(|x| SHIPS.contains(x) || LANDED.contains(x))
            || (0..w.len()).any(|i| onto_main(&w[i..]))
    };
    let told: Vec<&str> = reply_text(rest)
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if wh {
        if second == "about" {
            // `What about the docs? Merged with the fix.`: a thing's state,
            // which only its landing answers (the answer is read on its own).
            return leftover || !lands(&told);
        }
        let remote = |w: &&str| matches!(*w, "origin/main" | "upstream/main" | "origin/master");
        let placed = matches!(first, "where" | "where's")
            && told.iter().any(remote)
            && told
                .iter()
                .all(|w| remote(w) || YES_TAIL.contains(w) || sha(w));
        return !((matches!(first, "why" | "how") || placed) && !leftover) && answer != Answer::No;
    }
    let settled_word = |w: &&str| SETTLED.contains(w) || matches!(*w, "clean" | "gone" | "empty");
    let exists = second == "there"
        || !(words.iter().any(|w| verbish(w) || settled_word(w)) || lands(&words))
        || words.iter().any(|w| {
            matches!(
                *w,
                "any"
                    | "anything"
                    | "anyone"
                    | "anybody"
                    | "something"
                    | "else"
                    | "left"
                    | "remaining"
                    | "outstanding"
            )
        });
    if exists {
        return answer != Answer::No;
    }
    let deficit = words.iter().find(|w| DEFICITS.contains(w));
    match answer {
        Answer::Yes => {
            let gone = |w: &&str| GONE.contains(w) || matches!(*w, "gone" | "empty");
            leftover
                || deficit.is_some()
                || !(lands(&words)
                    || (!change && words.iter().any(settled_word))
                    || words.iter().any(gone))
        }
        Answer::No => {
            !((leftover || deficit.is_some())
                && (!change
                    || deficit.is_some_and(|d| UNLANDED_DEFICITS.contains(d))
                    || lands(&told)))
        }
        Answer::Said => true,
    }
}

fn land_open(
    line: &str,
    s: &str,
    (prior, said_main): (&str, bool),
    st: &Stand<'_>,
    stands: &[Stand<'_>],
) -> bool {
    let (at, end) = (st.at, st.end());
    if quoted(s, at) || in_path(s, at, end) || nil_item(s, at, end) || needs::token_you(s, at) {
        return false;
    }
    if matches!(st.reads(), Reads::Exception) {
        return nothing_but(s, st) || (excepted(s, st) && excepting(s, stands));
    }
    if matches!(st.reads(), Reads::Qualified) {
        return qualified_nil(s, st);
    }
    if matches!(st.reads(), Reads::OnlyTail) {
        return only_item(s, st);
    }
    if matches!(st.reads(), Reads::Party) {
        let rest = &s[st.end()..];
        let clause: Vec<&str> = rest[..clause_end(rest, 0)]
            .split_whitespace()
            .map(bare)
            .collect();
        let owed = |modal: usize, verb: usize| {
            clause
                .get(verb)
                .is_some_and(|w| matches!(*w, "decide" | "fix" | "choose" | "rule"))
                && !clause[..modal].iter().any(|w| negation(w))
                && !clause
                    .get(verb + 1)
                    .is_some_and(|w| matches!(*w, "nothing" | "none"))
        };
        if (0..clause.len()).any(|i| {
            (matches!(clause[i], "has" | "have" | "needs" | "need")
                && clause.get(i + 1) == Some(&"to")
                && owed(i, i + 2))
                || (matches!(clause[i], "should" | "must") && owed(i, i + 1))
        }) {
            return true;
        }
        return s[..st.at]
            .trim_end()
            .strip_suffix(':')
            .is_some_and(|label| !label.trim().is_empty())
            && rest[..clause_end(rest, 0)]
                .split_whitespace()
                .map(bare)
                .all(|w| w.is_empty() || matches!(w, "only" | "alone" | "now"));
    }
    if matches!(
        st.word,
        "stash" | "stashes" | "draft" | "drafts" | "wip" | "worktree"
    ) && s[end..]
        .split_whitespace()
        .next()
        .map(|w| bare(w).trim_end_matches("'s"))
        .is_some_and(|w| CODE_PARTS.contains(&w))
    {
        return false;
    }
    if s[end..]
        .split_whitespace()
        .next()
        .map(bare)
        .is_some_and(|w| matches!(w, "nothing" | "none"))
    {
        return false;
    }
    let from = clause_start(s, at);
    let to = clause_end(s, end);
    let before: Vec<&str> = s[from..at].split_whitespace().map(bare).collect();
    let after: Vec<&str> = s[end..to]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let prev = before.last().copied();
    let next = after.first().copied();
    if st.word.starts_with("not ") && prev.is_some_and(|p| OPEN_MODALS.contains(&p)) {
        return false;
    }
    if st.word == "unstaged" && next.is_some_and(|n| matches!(n, "it" | "them" | "those" | "these"))
    {
        return false;
    }
    if (matches!(st.word, "up to date with" | "up-to-date with") && before.contains(&"your"))
        || (matches!(st.word, "provided" | "providing" | "unknown")
            && prev.is_some_and(|p| DETERMINERS.contains(&p) || p == "as"))
    {
        return false;
    }
    if matches!(st.word, "could not" | "couldn't") && cannot_happen(&s[end..to]) {
        return false;
    }
    let proof_row = PROOF_ROWS.contains(&st.word)
        || (st.word == "still" && next.is_some_and(|n| PROOF_ROWS.contains(&n)));
    let failed_proof = matches!(st.word, "fail" | "fails" | "failed" | "failing")
        && next == Some("to")
        && !after
            .get(1)
            .is_some_and(|w| matches!(*w, "pass" | "succeed"))
        && (before
            .windows(2)
            .any(|w| matches!(w, ["negative", "control" | "controls"]))
            || before
                .iter()
                .any(|w| matches!(*w, "mutation" | "mutations" | "mutant" | "mutants")));
    if proof_row && !failed_proof && proves_by_failing(s, (from, to), (at, end)) {
        return false;
    }
    if st.family.is_none()
        && st.word == "skipped"
        && prev.is_some_and(|p| ["is", "are", "be"].contains(&p))
        && !before.iter().any(|w| TASK_ITEMS.contains(w))
    {
        return false;
    }
    if described(&before) || behind(&before) {
        return false;
    }
    if matches!(st.reads(), Reads::Unlanded | Reads::Undone) {
        return landing_denied(s, from, at, &after, st, stands);
    }
    if aside_settled(s, st) || also_landed(s, (from, to), st, stands) {
        return false;
    }
    let settles = match past_aside(s, to) {
        Some((joined, to)) => settled(&joined, from, to, st),
        None => settled(s, from, to, st),
    };
    if settles || (st.word != "yet" && denied(s, from, at, st, stands)) {
        return false;
    }
    if failed_proof {
        return true;
    }
    match st.reads() {
        Reads::Anywhere if matches!(st.word, "local main" | "main locally") => {
            !even_with_remote(&after)
        }
        Reads::Anywhere
            if st.word == "partial" && next.is_some_and(|n| PARTIAL_TERMS.contains(&n)) =>
        {
            false
        }
        Reads::Anywhere if matches!(st.word, "for review" | "for a review") => {
            before.iter().any(|w| {
                matches!(
                    *w,
                    "pushed" | "force-pushed" | "sent" | "posted" | "submitted" | "up" | "put"
                ) || CHANGE_NOUNS.contains(w)
                    || branch_name(w)
            })
        }
        Reads::Anywhere
            if st.word == "separately"
                && before.iter().any(|w| TESTED.contains(w))
                && !before.iter().any(|w| negation(w) || UNSETTLED.contains(w)) =>
        {
            false
        }
        Reads::Anywhere
        | Reads::Exception
        | Reads::Party
        | Reads::OnlyTail
        | Reads::Qualified
        | Reads::Unlanded
        | Reads::Undone => true,
        Reads::Alone => {
            alone(&s[end..to])
                && !prev.is_some_and(|p| matches!(p, "main" | "origin/main" | "master"))
        }
        Reads::Beside(nouns) => {
            let (a, b) = stretch(s, from, to, (at, end), st.settled_by());
            let counted = prev.and_then(count).is_some_and(|n| n > 0);
            let tally: Vec<&str> = s[..at].split_whitespace().map(bare).collect();
            let tail = s[end..].trim_start();
            let tallied = st.word == "ignored"
                && (tail.starts_with(')') || tail.split_whitespace().all(|w| bare(w).is_empty()))
                && counted
                && tally
                    .windows(2)
                    .any(|w| count(w[0]).is_some() && w[1] == "passed")
                && tally
                    .windows(2)
                    .any(|w| count(w[0]) == Some(0) && w[1] == "failed");
            if tallied {
                return false;
            }
            let turned_off = prev.is_some_and(|p| matches!(p, "i" | "we"))
                && next.is_some_and(|n| matches!(n, "it" | "them"))
                && matches!(
                    st.word,
                    "disabled"
                        | "ignored"
                        | "quarantined"
                        | "muted"
                        | "skipped"
                        | "suppressed"
                        | "silenced"
                        | "turned off"
                        | "switched off"
                        | "commented out"
                );
            let code_mark = matches!(
                (prev, st.word),
                (Some("check"), "ignored") | (Some("lint"), "allowed")
            );
            !code_mark
                && !next.is_some_and(|n| CONTROLS.contains(&n))
                && (counted
                    || turned_off
                    || nouns_in(s, a, b, nouns)
                    || labelled(s, from, &before, nouns))
        }
        Reads::Later => {
            let with = st.word.ends_with(" with")
                && !after
                    .iter()
                    .find(|w| !matches!(**w, "the" | "a" | "an"))
                    .is_some_and(|w| {
                        LATER_WITH.contains(w)
                            || (w.starts_with('v')
                                && w[1..].starts_with(|c: char| c.is_ascii_digit()))
                            || w.starts_with(|c: char| c.is_ascii_digit())
                    });
            let dated = DATED.contains(&st.word)
                && before.iter().any(|w| {
                    past_form(w) || matches!(*w, "came" | "went" | "ran" | "got" | "was" | "were")
                })
                && !before.iter().enumerate().any(|(i, w)| {
                    let put_off = *w == "out"
                        && i.checked_sub(1).is_some_and(|p| {
                            matches!(
                                before[p],
                                "pushed" | "moved" | "bumped" | "put" | "kicked" | "punted"
                            )
                        });
                    put_off
                        || negation(w)
                        || (UNSETTLED.contains(w) && !(*w == "to" && onto_main(&before[i..])))
                        || matches!(
                            *w,
                            "back"
                                | "until"
                                | "till"
                                | "by"
                                | "for"
                                | "moved"
                                | "bumped"
                                | "rescheduled"
                                | "delayed"
                                | "postponed"
                                | "deferred"
                                | "planned"
                                | "scheduled"
                                | "slated"
                                | "set"
                                | "booked"
                                | "requested"
                                | "asked"
                                | "pencilled"
                                | "penciled"
                        )
                });
            !with
                && !dated
                && !AUTOMATIC.iter().any(|p| s.contains(p))
                && !past_attributive(st, prev, &after)
        }
        Reads::Until => {
            !before
                .iter()
                .any(|w| MECHANISMS.contains(w) && !(WAITS.contains(w) && waits_on_work(&before)))
                && !AUTOMATIC.iter().any(|p| s.contains(p))
        }
        Reads::Elided => next.is_none(),
        Reads::Intent => {
            (st.word != "consider" || s[..at].trim().is_empty())
                && !ends_intent(&s[end..], line[s.len()..].trim().is_empty())
                && !after.iter().any(|w| DENIALS.contains(w))
        }
        Reads::Gerund => next.is_some_and(|n| {
            (n.len() > 4 && n.ends_with("ing") && !n.ends_with("thing") && n != "during")
                || matches!(n, "a" | "an" | "another" | "more" | "some")
                || (n == "to"
                    && (s[..at].trim().is_empty()
                        || prev.is_some_and(|p| matches!(p, "i" | "we" | "also" | "just"))))
        }),
        Reads::Still => {
            let rest: Vec<&str> = s[end..to].split_whitespace().map(bare).collect();
            if rest
                .windows(2)
                .any(|w| matches!(w, ["on", "purpose"] | ["by", "design"]))
                && !before.iter().any(|w| FAULT_NOUNS.contains(w))
            {
                return false;
            }
            let (first, second) = (rest.first().copied(), rest.get(1).copied());
            if matches!(first, Some("catches" | "catch")) {
                return !rest[1..].iter().any(|w| FAULT_NOUNS.contains(w));
            }
            if matches!(first, Some("pass" | "passes"))
                && rest.iter().enumerate().any(|(i, w)| {
                    let later = &rest[i + 1..];
                    (matches!(*w, "if" | "when")
                        && later
                            .iter()
                            .any(|w| matches!(*w, "removed" | "reverted" | "undone")))
                        || (*w == "without"
                            && later
                                .iter()
                                .take(3)
                                .any(|w| matches!(*w, "fix" | "change" | "patch")))
                })
            {
                return true;
            }
            let ok = match rest.get(1..) {
                Some(["with", code]) | Some(["with", "a" | "an", code]) => ok_status(code),
                _ => false,
            };
            let more = s[to..]
                .strip_prefix(',')
                .filter(|_| rest.len() == 1)
                .map(|r| {
                    let words: Vec<&str> = r[..clause_end(r, 0)]
                        .split_whitespace()
                        .map(bare)
                        .filter(|w| !w.is_empty())
                        .collect();
                    (
                        !words.is_empty(),
                        !words.is_empty() && !words.iter().any(|w| verbish(w)),
                    )
                });
            let (any_more, fragment) = more.unwrap_or((false, false));
            let np = before
                .iter()
                .position(|w| PREPOSITIONS.contains(w))
                .unwrap_or(before.len());
            let head = before[..np]
                .iter()
                .rfind(|w| !matches!(**w, "is" | "are" | "was" | "were"));
            let fault = head.is_some_and(|w| FAULT_NOUNS.contains(w));
            fault
                || s[to..].starts_with(':')
                || first.is_none_or(|n| {
                    !((STILL_GOOD.contains(&n) && !fragment)
                        || (STILL_HOLDS.contains(&n)
                            && !any_more
                            && (second.is_none_or(|w| {
                                matches!(w, "for" | "to" | "as" | "on" | "in" | "after" | "from")
                                    || STILL_MANNER.contains(&w)
                            }) || ok))
                        || (matches!(n, "exists" | "exist") && second == Some("as"))
                        || n.starts_with(|c: char| c.is_ascii_digit())
                        || (DETERMINERS.contains(&n) && !second.is_some_and(|w| OLD.contains(&w))))
                })
        }
        Reads::Ref if st.word == "--no-merged" => !lists_none(&s[end..]),
        Reads::Ref => off_main(&s[end..]),
        Reads::Fraction if st.word == "only" => {
            (before.is_empty() && only_part(&after))
                || (after.is_empty()
                    && !before.iter().any(|w| matches!(*w, "was" | "were"))
                    && before.iter().any(|w| {
                        matches!(
                            *w,
                            "covered" | "tested" | "exercised" | "checked" | "verified"
                        )
                    }))
        }
        Reads::Fraction => {
            (short_count(&before, &s[end..to]) || leading_part(&before, &s[end..]))
                && !rest_settled(&line[end..])
        }
        Reads::NotDone => next.is_some_and(|n| {
            !AVOIDED.contains(&n)
                && !negation(n)
                && !(STEADY.contains(&n) && !prev.is_some_and(|p| matches!(p, "i" | "we")))
        }),
        Reads::Lost => next.is_some_and(|n| WORKS.contains(&n)),
        Reads::Better => {
            next.is_some_and(|n| (n.len() > 4 && n.ends_with("ed")) || COMPARATIVES.contains(&n))
        }
        Reads::Concession => conceding(&format!("{prior}{}", &s[..at]), &s[end..]),
        Reads::State => {
            !next.is_some_and(|n| CONTROLS.contains(&n) || n == "while")
                && !prev.is_some_and(|p| STOPS.contains(&p))
                && !after.iter().take(3).any(|w| ATTEMPTS.contains(w))
        }
        Reads::End => {
            let kept_off_main = matches!(st.word, "on origin" | "on the remote" | "local")
                && (prev.is_some_and(|p| MAINS.contains(&p))
                    || before.iter().any(|w| {
                        GONE.contains(w)
                            || matches!(*w, "tag" | "tags" | "deleted" | "cleaned")
                            || version(w)
                    }));
            next.is_none()
                && !kept_off_main
                && !prev.is_some_and(|p| matches!(p, "too" | "as" | "so" | "how"))
                && !(st.word == "aside" && before.is_empty() && s[to..].starts_with(':'))
        }
        Reads::Count => {
            (next.is_none() || next == Some("to"))
                && before.iter().rev().take(3).any(|w| counted(w))
        }
        Reads::OneMore => {
            let settles = |w: &&str| {
                ((SETTLED.contains(w) || LANDED.contains(w))
                    && (past_form(w) || matches!(*w, "done" | "green" | "clean")))
                    || negation(w)
            };
            !next.is_some_and(|n| matches!(n, "time" | "times"))
                && !before
                    .iter()
                    .any(|w| SETTLED.contains(w) || LANDED.contains(w) || negation(w))
                && !after
                    .iter()
                    .skip(1)
                    .take_while(|w| {
                        !CONDITIONS.contains(w) && !matches!(**w, "before" | "so" | "to")
                    })
                    .any(settles)
        }
        Reads::Only => {
            (s[..at].trim().is_empty()
                && s.split_whitespace().count() <= 5
                && !s[end..].split_whitespace().map(bare).any(verbish))
                || (before.is_empty()
                    && s[..from].trim_end().ends_with(',')
                    && s[clause_start(s, from.saturating_sub(1))..from]
                        .split_whitespace()
                        .map(bare)
                        .next()
                        .is_some_and(|w| matches!(w, "nothing" | "none" | "no" | "zero")))
        }
        Reads::Elliptic => {
            (1..=3).contains(&before.len())
                && !before.iter().any(|w| {
                    verbish(w)
                        || negation(w)
                        || CUTS.contains(w)
                        || PREPOSITIONS.contains(w)
                        || LEADS.contains(w)
                })
                && !s[end..].split_whitespace().map(bare).any(|w| {
                    SETTLED.contains(&w)
                        || LANDED.contains(&w)
                        || (st.word == "after" && verbish(w))
                })
        }
        Reads::Absent => {
            let past = prev.is_some_and(|p| matches!(p, "was" | "were" | "had"));
            let kind = st.len != st.word.len();
            (next.is_none_or(|n| GAP_PREPOSITIONS.contains(&n)) && !(kind && past))
                || (next == Some("that")
                    && !past
                    && after.get(1).is_some_and(|w| {
                        matches!(*w, "a" | "an" | "the" | "its" | "each" | "every")
                    }))
        }
        Reads::Nobody => {
            let nothing = st.word == "nothing";
            let filler = |w: &str| {
                matches!(
                    w,
                    "has" | "have" | "had" | "yet" | "ever" | "actually" | "else"
                )
            };
            let Some(k) = after.iter().position(|w| !filler(w)) else {
                return false;
            };
            let w = after[k];
            if nothing {
                matches!(
                    w,
                    "ran" | "run" | "tested" | "checked" | "verified" | "measured"
                ) || (matches!(w, "tests" | "covers" | "exercises")
                    && after
                        .get(k + 1)
                        .is_some_and(|o| matches!(*o, "it" | "them" | "this" | "that")))
            } else {
                CHECKED.contains(&w)
            }
        }
        Reads::Onto => {
            let bracketed: Vec<&str> = s[end..]
                .trim_start()
                .strip_prefix('(')
                .and_then(|r| r.split_once(')'))
                .map(|(inner, _)| inner.split_whitespace().map(bare).collect())
                .unwrap_or_default();
            ((onto_branch(&after) || (after.is_empty() && onto_branch(&bracketed)))
                && !main_claimed(s))
                || (matches!(st.word, "pushed" | "committed")
                    && unplaced(s, (prior, said_main), &before, &after))
        }
        Reads::Dropped => !disposable(st.word, &before, &after),
        Reads::OnBranch if !matches!(st.word, "on" | "in") => prev.is_some_and(|p| {
            (branch_name(p) && !p.split_once('/').is_some_and(|(_, m)| MAINS.contains(&m)))
                || matches!(p, "branch" | "worktree")
        }),
        Reads::OnBranch => {
            on_branch(&after, prev.is_some_and(|p| LANDING_VERBS.contains(&p))) && !main_claimed(s)
        }
        Reads::Change => change_open(s, (from, to), st, &before, &after),
        Reads::Percent => {
            let rolling = before.iter().any(|w| {
                matches!(
                    *w,
                    "rollout"
                        | "rolled"
                        | "rolling"
                        | "canary"
                        | "deploy"
                        | "deployed"
                        | "ramp"
                        | "ramped"
                        | "staged"
                )
            });
            let label = s[..at]
                .trim_end()
                .strip_suffix(':')
                .and_then(|l| l.split_whitespace().last())
                .is_some_and(|l| WORK_LABELS.contains(&bare(l)));
            (st.word == "%" || prev.is_some_and(below_hundred))
                && (rolling || percent_part(&s[end..]) || (label && after.is_empty()))
        }
        Reads::Without => after
            .iter()
            .find(|w| !DETERMINERS.contains(w) && !matches!(**w, "any" | "its" | "their"))
            .is_some_and(|w| OWED_WITHOUT.contains(w)),
        Reads::Approval => {
            let (a, b) = stretch(s, from, to, (at, end), LANDS);
            let landed_after = {
                let rest = line[to..].trim_start_matches([',', ';', '—', '–', ' ']);
                let words: Vec<&str> = rest[..clause_end(rest, 0)]
                    .split_whitespace()
                    .map(bare)
                    .filter(|w| !w.is_empty())
                    .collect();
                to < line.len()
                    && settles_with(&words, SHIPS, true)
                    && words.first().is_some_and(|w| {
                        SHIPS.contains(w)
                            || LANDING_VERBS.contains(w)
                            || matches!(*w, "then" | "and" | "since" | "now")
                    })
            };
            let upstream = st.word == "accepted" && next == Some("upstream");
            nouns_in(s, a, b, APPROVED_THINGS) && !landed_after && !upstream
        }
        Reads::Tally => {
            let one = prev.is_some_and(|p| matches!(p, "a" | "an" | "one" | "single"))
                && before.len().checked_sub(2).is_none_or(|i| {
                    matches!(
                        before[i],
                        "has"
                            | "have"
                            | "had"
                            | "shows"
                            | "showed"
                            | "hit"
                            | "hits"
                            | "got"
                            | "saw"
                            | "sees"
                            | "is"
                            | "are"
                            | "was"
                            | "were"
                            | "still"
                            | "remains"
                            | "with"
                            | "there's"
                            | "left"
                    )
                });
            let labelled = next.is_none()
                && s[to..].strip_prefix(':').is_some_and(|r| {
                    let mut item = r[..clause_end(r, 0)].split_whitespace().map(bare);
                    item.next().and_then(count).is_some_and(|n| n > 0) && item.next().is_none()
                });
            labelled
                || ((prev.and_then(count).is_some_and(|n| n > 0) || one)
                    && next.is_none_or(|n| {
                        PREPOSITIONS.contains(&n) || JOINS.contains(&n) || verbish(n) || n == "left"
                    }))
        }
        Reads::Rerun => {
            let mechanism = st.word == "retried"
                && prev.is_some_and(|p| matches!(p, "is" | "are" | "gets" | "get"))
                && !before.iter().any(|w| CHECK_NOUNS.contains(w))
                && !s.split_whitespace().map(bare).any(|w| {
                    matches!(
                        w,
                        "pass"
                            | "passes"
                            | "passed"
                            | "passing"
                            | "green"
                            | "succeeds"
                            | "succeeded"
                            | "fail"
                            | "fails"
                            | "failed"
                            | "failing"
                            | "red"
                            | "second"
                            | "until"
                    )
                });
            !mechanism
                && !s[..at]
                    .split_whitespace()
                    .map(bare)
                    .any(|w| RETRIED_STEPS.contains(&w))
        }
        Reads::Status => {
            let label_text = s[..at]
                .trim_end_matches(|c: char| {
                    c.is_whitespace() || !(c.is_ascii() || c.is_alphanumeric())
                })
                .strip_suffix(':');
            let label = label_text.is_some_and(|l| l.chars().any(char::is_alphanumeric));
            let counted = prev.and_then(count).is_some_and(|n| n > 0);
            let opens = matches!(st.word, "skip" | "skips")
                && s[..at].trim().is_empty()
                && !after.iter().any(|w| verbish(w));
            if st.word == "n/a" {
                label
                    && after.is_empty()
                    && label_text
                        .and_then(|l| l.split_whitespace().last())
                        .is_some_and(|w| CHECK_LABELS.contains(&bare(w)))
            } else {
                (label && after.is_empty()) || counted || opens
            }
        }
        Reads::Most => {
            let object = prev.is_some_and(|p| {
                SHIPS.contains(&p) || matches!(p, "pushed" | "committed" | "landed" | "done")
            });
            object
                || !prev.is_some_and(|p| {
                    matches!(p, "the" | "at" | "a" | "an" | "which" | "what" | "whose")
                }) && (after
                    .iter()
                    .any(|w| SETTLED.contains(w) || LANDED.contains(w))
                    || (0..after.len()).any(|i| onto_main(&after[i..])))
        }
    }
}

const CHECK_LABELS: &[&str] = &[
    "appkit", "measure", "verify", "check", "checks", "test", "tests", "lab", "lane", "lanes",
    "fmt", "tippy", "census", "spec", "perf", "cells", "cell", "suite", "e2e",
];

const RETRIED_STEPS: &[&str] = &[
    "push", "pushed", "pushes", "pull", "fetch", "fetched", "clone", "download", "upload",
];

fn verbish(w: &str) -> bool {
    VERBS.contains(&w)
        || AUX.contains(&w)
        || SETTLED.contains(&w)
        || STILL_GOOD.contains(&w)
        || MORE_VERBS.contains(&w)
        || past_form(w)
}

fn past_attributive(st: &Stand<'_>, prev: Option<&str>, after: &[&str]) -> bool {
    st.word == "later"
        && prev.is_some_and(|p| matches!(p, "a" | "an" | "the"))
        && after.first().is_some_and(|n| !verbish(n))
        && after
            .get(1)
            .is_some_and(|v| past_form(v) || matches!(*v, "was" | "were" | "had" | "ran"))
}

const STILL_HOLDS: &[&str] = &[
    "stable",
    "responds",
    "respond",
    "load",
    "taken",
    "served",
    "serves",
    "reachable",
    "usable",
    "readable",
    "unchanged",
    "byte-identical",
    "identical",
    "restore",
    "restores",
    "resume",
    "resumes",
    "reconnect",
    "reconnects",
];

const STILL_MANNER: &[&str] = &["unchanged", "fine", "correctly", "normally", "identically"];

fn ok_status(w: &str) -> bool {
    matches!(w, "ok" | "success")
        || (w.len() == 3 && w.starts_with('2') && w.bytes().all(|b| b.is_ascii_digit()))
}

const MORE_VERBS: &[&str] = &[
    "exists", "exist", "matters", "counts", "differs", "changes", "change", "uses", "use", "calls",
    "touches", "stays", "stay", "lives", "live", "took", "brought", "carried",
];

const NOT_PAST: &[&str] = &[
    "need", "embed", "speed", "feed", "seed", "proceed", "succeed", "exceed", "indeed", "hundred",
    "sacred", "naked", "wicked", "kindred",
];

fn past_form(w: &str) -> bool {
    w.len() > 3 && w.ends_with("ed") && !NOT_PAST.contains(&w)
}

const ATTEMPTS: &[&str] = &[
    "approach",
    "attempt",
    "attempts",
    "idea",
    "plan",
    "design",
    "experiment",
    "try",
    "prototype",
];

const STOPS: &[&str] = &[
    "stops", "stop", "stopped", "avoids", "avoid", "avoided", "prevents", "prevent",
];

fn counted(w: &str) -> bool {
    count(w).is_some_and(|n| n > 0)
        || matches!(
            w,
            "a" | "an" | "few" | "some" | "several" | "more" | "many" | "couple" | "only" | "last"
        )
}

const LEADS: &[&str] = &[
    "even", "right", "just", "only", "soon", "long", "shortly", "day", "week", "shortly",
];

const GAP_PREPOSITIONS: &[&str] = &[
    "for", "on", "of", "in", "at", "around", "about", "covering", "against", "under", "yet",
];

const CHECKED: &[&str] = &[
    "run",
    "ran",
    "tested",
    "test",
    "reviewed",
    "review",
    "checked",
    "check",
    "verified",
    "verify",
    "tried",
    "try",
    "looked",
    "measured",
    "confirmed",
    "approved",
    "signed",
    "merged",
    "pushed",
    "landed",
    "released",
    "published",
];

const WAITS: &[&str] = &["waits", "wait", "waiting"];

const WORK_ITEMS: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "pr",
    "prs",
    "release",
    "changelog",
    "migration",
    "tag",
    "readme",
    "port",
    "upgrade",
    "cleanup",
    "rest",
    "remainder",
    "rollout",
    "deploy",
];

fn waits_on_work(before: &[&str]) -> bool {
    before
        .iter()
        .take_while(|w| !WAITS.contains(w))
        .any(|w| WORK_ITEMS.contains(w))
}

fn ends_intent(rest: &str, last: bool) -> bool {
    let words: Vec<&str> = rest
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    last && match words.as_slice() {
        ["stop" | "end"] => true,
        ["stop" | "end", w] => matches!(*w, "here" | "now" | "there"),
        [
            "stop" | "end",
            "here" | "now" | "there",
            "since" | "as" | "because",
            reason @ ..,
        ] => {
            (0..reason.len()).any(|i| onto_main(&reason[i..]))
                && !reason
                    .iter()
                    .any(|w| negation(w) || UNSETTLED.contains(w) || DEFICITS.contains(w))
        }
        ["leave", "it" | "things", "there"] | ["leave", "it", "at", "that"] => true,
        ["wrap", "up", w] | ["wrap", "it" | "things", "up", w] => matches!(*w, "here" | "there"),
        _ => false,
    }
}

const NIL: &[&str] = &["none", "nothing", "n/a", "nil", "zero", "0", "empty", "no"];

const NIL_TAIL: &[&str] = &[
    "left",
    "remaining",
    "found",
    "open",
    "pending",
    "outstanding",
    "at",
    "all",
    "whatsoever",
];

fn nil_words(item: &str) -> bool {
    let mut words = item.split_whitespace().map(bare).filter(|w| !w.is_empty());
    words.next().is_some_and(|w| NIL.contains(&w)) && words.all(|w| NIL_TAIL.contains(&w))
}

fn nil_item(s: &str, at: usize, end: usize) -> bool {
    if s[clause_start(s, at)..at].split_whitespace().count() > 1 {
        return false;
    }
    let colon = s[end..].find(':').map(|c| (end + c, 1));
    let dash = [" — ", " – "]
        .iter()
        .filter_map(|d| s[end..].find(d).map(|c| (end + c, d.len())))
        .min();
    let Some((colon, mark)) = [colon, dash].into_iter().flatten().min() else {
        return false;
    };
    let label = &s[end..colon];
    if label.contains(CLAUSE_MARKS) || label.split_whitespace().count() > 2 {
        return false;
    }
    nil_list(&s[colon + mark..])
}

fn nil_list(item: &str) -> bool {
    if nil_aside(item).is_some_and(|(_, inner)| placed_count(inner).is_some()) {
        return false;
    }
    let item = nil_aside(item).map_or(item, |(nil, _)| nil);
    let item = item.trim().trim_end_matches(['.', ';', '!', ' ']);
    if matches!(item, "-" | "—" | "–") {
        // A dash for none, as a list or a table writes it (`Open PRs: —`).
        return true;
    }
    let parts: Vec<&str> = item.split([',', ';']).collect();
    let fresh = |part: &str| {
        let words: Vec<&str> = part
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        matches!(words.as_slice(), [n, rest @ ..] if NIL.contains(n)
            && !rest.is_empty()
            && rest.iter().all(|w| matches!(*w, "added" | "new" | "introduced")))
    };
    let labelled = |part: &str| {
        let split = part
            .split_once(':')
            .or_else(|| part.split_once(" — "))
            .or_else(|| part.split_once(" – "));
        match split {
            Some((label, value)) => {
                label.split_whitespace().count() <= 3
                    && !label.contains(CLAUSE_MARKS)
                    && nil_words(value)
            }
            None => false,
        }
    };
    let first = parts[0];
    (nil_words(first) || (fresh(first) && parts[1..].iter().any(|p| nil_words(p))))
        && parts[1..]
            .iter()
            .all(|part| labelled(part) || nil_words(part) || fresh(part))
}

fn nil_aside(item: &str) -> Option<(&str, &str)> {
    let t = item.trim().trim_end_matches(['.', ';', '!', ' ']);
    t.strip_suffix(')')?.split_once('(')
}

/// Where a `none (…)` aside opens on a count, the bytes of `inner` that hold it and the word
/// that places it (`3 open`).
fn placed_count(inner: &str) -> Option<Range<usize>> {
    let words: Vec<(usize, &str)> = tokens_at(inner)
        .map(|(at, raw)| {
            let word = bare(raw);
            (at + offset_in(raw, word), word)
        })
        .filter(|(_, w)| !w.is_empty())
        .collect();
    match words.as_slice() {
        [(at, n), rest @ ..]
            if (count(n).is_some_and(|c| c > 0) || matches!(*n, "some" | "few"))
                && rest
                    .first()
                    .is_none_or(|(_, w)| ELSEWHERE.contains(w) || CHANGE_NOUNS.contains(w)) =>
        {
            let end = rest
                .first()
                .map_or(at + n.len(), |(next, w)| next + w.len());
            Some(*at..end)
        }
        _ => None,
    }
}

const ELSEWHERE: &[&str] = &[
    "upstream",
    "elsewhere",
    "on",
    "in",
    "open",
    "pending",
    "left",
    "remaining",
    "outstanding",
    "unmerged",
    "unpushed",
    "of",
    "more",
    "still",
    "here",
    "there",
];

/// An item's `none (…)` aside that leaves work open; the hit stands in `item`.
fn aside_status(item: &str, families: &[LandFamily], said_main: bool) -> Option<Hit> {
    let (nil, inner) = nil_aside(item)?;
    if !nil_list(nil) {
        return None;
    }
    let base = offset_in(item, inner);
    if let Some(count) = placed_count(inner) {
        return Some(Hit {
            word: COUNT_BESIDE_NONE,
            at: base + count.start,
            len: count.len(),
        });
    }
    land_status(inner, inner.len(), "", families, said_main).map(|hit| Hit {
        at: base + hit.at,
        ..hit
    })
}

/// Where `inner`, a slice of `outer`, starts in it.
fn offset_in(outer: &str, inner: &str) -> usize {
    let at = inner.as_ptr().addr().wrapping_sub(outer.as_ptr().addr());
    debug_assert!(
        at + inner.len() <= outer.len(),
        "{inner:?} is not in {outer:?}"
    );
    at.min(outer.len())
}

fn behind(before: &[&str]) -> bool {
    before.iter().enumerate().any(|(r, w)| {
        matches!(*w, "where" | "which" | "that" | "when" | "whereby")
            && before[r.saturating_sub(4)..r]
                .iter()
                .any(|v| FIXERS.contains(v))
            && !before[r + 1..].iter().any(|v| PRESENT.contains(v))
    })
}

const PRESENT: &[&str] = &[
    "is", "are", "am", "has", "have", "remains", "remain", "stays", "stay", "will", "can", "does",
    "do",
];

const FIXERS: &[&str] = &[
    "fixed",
    "resolved",
    "removed",
    "addressed",
    "closed",
    "eliminated",
    "dropped",
    "deleted",
    "retired",
    "replaced",
    "reverted",
    "rewrote",
    "rewritten",
    "solved",
];

const AVOIDED: &[&str] = &[
    "change",
    "changed",
    "touch",
    "touched",
    "break",
    "broken",
    "broke",
    "alter",
    "altered",
    "modify",
    "modified",
    "affect",
    "affected",
    "need",
    "needed",
    "regress",
    "regressed",
    "fail",
    "failed",
    "introduce",
    "introduced",
    "start",
    "started",
    "expand",
    "expanded",
    "widen",
    "widened",
    "force-push",
    "force-pushed",
    "delete",
    "deleted",
    "leave",
    "left",
    "find",
    "found",
    "see",
    "seen",
    "hit",
    "notice",
    "noticed",
    "add",
    "added",
];

const STEADY: &[&str] = &[
    "move",
    "moved",
    "budge",
    "budged",
    "shift",
    "shifted",
    "grow",
    "grew",
    "degrade",
    "degraded",
    "crash",
    "crashed",
    "hang",
    "hung",
    "leak",
    "leaked",
    "recur",
    "recurred",
    "reappear",
    "reappeared",
];

const WORKS: &[&str] = &[
    "compile", "compiles", "build", "builds", "work", "works", "pass", "passes", "run", "runs",
    "parse", "parses", "load", "loads", "start", "starts", "handle", "handles", "cover", "covers",
    "support", "supports", "render", "renders", "apply", "applies", "match", "matches", "link",
    "links", "install", "installs", "launch", "launches",
];

const COMPARATIVES: &[&str] = &[
    "better", "cleaner", "faster", "simpler", "nicer", "tidier", "smaller", "safer", "clearer",
];

const OWED_WITHOUT: &[&str] = &[
    "docs",
    "doc",
    "documentation",
    "changelog",
    "readme",
    "tag",
    "tags",
    "tests",
    "test",
    "coverage",
    "release",
    "notes",
    "migration",
    "port",
    "backport",
    "updating",
    "adding",
    "writing",
    "tagging",
    "porting",
    "testing",
    "publishing",
    "pushing",
    "merging",
    "releasing",
    "documenting",
    "running",
    "rerunning",
    "verifying",
    "reviewing",
    "signing",
    "bumping",
];

const APPROVED_THINGS: &[&str] = &[
    "change",
    "changes",
    "fix",
    "fixes",
    "patch",
    "patches",
    "work",
    "feature",
    "features",
    "refactor",
    "rewrite",
    "migration",
    "port",
    "docs",
    "update",
    "updates",
    "diff",
    "commit",
    "commits",
    "implementation",
];

const PARTIAL_TERMS: &[&str] = &[
    "index",
    "indexes",
    "indices",
    "application",
    "function",
    "functions",
    "order",
    "ordering",
    "match",
    "matches",
    "matching",
    "sum",
    "sums",
    "eq",
    "ord",
    "view",
    "views",
    "mock",
    "mocks",
    "template",
    "templates",
    "specialization",
    "key",
    "keys",
    "derivative",
    "evaluation",
    "line",
    "lines",
];

const TESTED: &[&str] = &[
    "tested",
    "ran",
    "checked",
    "verified",
    "benchmarked",
    "measured",
    "built",
    "profiled",
];

const COSTS: &[&str] = &[
    "it",
    "ci",
    "the",
    "took",
    "a",
    "an",
    "few",
    "couple",
    "of",
    "several",
    "runs",
    "run",
    "tries",
    "try",
    "attempts",
    "retries",
    "reruns",
    "rounds",
    "iterations",
    "while",
    "longer",
    "than",
    "expected",
    "hours",
    "hour",
    "minutes",
    "days",
    "to",
    "go",
    "get",
    "turn",
    "green",
    "pass",
    "lane",
    "job",
    "build",
    "builds",
    "pipeline",
];

const LANDED: &[&str] = &[
    "pushed",
    "committed",
    "main",
    "origin/main",
    "upstream/main",
    "shipped",
    "already",
];

fn conceding(before: &str, after: &str) -> bool {
    let claims = |text: &str| {
        text.split_whitespace()
            .map(bare)
            .any(|w| SETTLED.contains(&w) || LANDED.contains(&w))
    };
    let words: Vec<&str> = after
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let cost = words.contains(&"took")
        && words
            .iter()
            .all(|w| COSTS.contains(w) || count(w).is_some_and(|n| n > 0));
    claims(before) && !claims(after) && !words.iter().any(|w| negation(w)) && !cost
}

const CONTROLS: &[&str] = &[
    "without", "if", "when", "unless", "whenever", "against", "before",
];

const OLD: &[&str] = &[
    "old", "previous", "former", "outdated", "stale", "earlier", "last",
];

const STILL_GOOD: &[&str] = &[
    "passes",
    "pass",
    "passed",
    "passing",
    "works",
    "work",
    "working",
    "builds",
    "build",
    "compiles",
    "compile",
    "parses",
    "parse",
    "holds",
    "hold",
    "green",
    "merged",
    "landed",
    "pushed",
    "on",
    "clean",
    "runs",
    "run",
    "fine",
    "correct",
    "valid",
    "intact",
    "supported",
    "matches",
    "match",
    "reads",
    "read",
    "fits",
    "fit",
    "applies",
    "apply",
    "opens",
    "starts",
    "loads",
    "renders",
    "behaves",
    "succeeds",
    "exits",
    "returns",
    "accepts",
    "handles",
    "accepted",
    "compatible",
    "backward",
    "backwards",
    "available",
    "honoured",
    "honored",
    "respected",
    "recognised",
    "recognized",
];

fn off_main(after: &str) -> bool {
    let Some(name) = after.strip_prefix('/') else {
        return false;
    };
    let token = name
        .split(|c: char| !(word_char(c) || c == '/' || c == '.'))
        .next()
        .unwrap_or("");
    if let Some((left, _)) = token.split_once("..") {
        return matches!(left, "main" | "master") && !lists_none(&name[token.len()..]);
    }
    let name = token.trim_end_matches('.');
    !name.is_empty() && !matches!(name, "main" | "master" | "head")
}

fn lists_none(rest: &str) -> bool {
    rest[..clause_end(rest, 0)]
        .split_whitespace()
        .map(bare)
        .any(|w| matches!(w, "nothing" | "empty" | "none" | "no" | "0" | "zero"))
}

const REMAINDER: &[&str] = &[
    "the other",
    "the others",
    "the rest",
    "the remaining",
    "the last",
    "the second",
    "the third",
    "the fourth",
    "the fifth",
];

fn rest_settled(after: &str) -> bool {
    REMAINDER.iter().any(|r| {
        after.find(r).is_some_and(|at| {
            after[at + r.len()..]
                .split_whitespace()
                .map(bare)
                .any(|w| SETTLED.contains(&w) || LANDED.contains(&w))
        })
    })
}

fn count(w: &str) -> Option<u32> {
    const WORDS: [&str; 12] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    crate::parse_uint(w).or_else(|| {
        WORDS
            .iter()
            .position(|n| *n == w)
            .and_then(|i| u32::try_from(i + 1).ok())
    })
}

fn short_count(before: &[&str], after: &str) -> bool {
    let mut words = after
        .split_whitespace()
        .map(bare)
        .skip_while(|w| *w == "the");
    let prev = match before {
        [.., "every" | "each", "one"] => None,
        [.., n, "out"] => Some(*n),
        [.., n] => Some(*n),
        [] => None,
    };
    let (Some(n), Some(m)) = (prev.and_then(count), words.next().and_then(count)) else {
        return false;
    };
    n < m
}

const PART_DONE: &[&str] = &[
    "done",
    "converted",
    "ported",
    "migrated",
    "implemented",
    "finished",
    "complete",
    "completed",
    "wired",
    "covered",
    "handled",
    "supported",
    "fixed",
    "merged",
    "landed",
    "written",
];

fn leading_part(before: &[&str], after: &str) -> bool {
    let Some((whole, part)) = after.split_once(',') else {
        return false;
    };
    let whole = whole
        .split_whitespace()
        .map(bare)
        .find(|w| *w != "the" && *w != "all")
        .and_then(count);
    let part: Vec<&str> = part.split_whitespace().map(bare).collect();
    before.is_empty()
        && whole
            .zip(part.first().and_then(|w| count(w)))
            .is_some_and(|(m, n)| n < m)
        && only_part(&part[1..])
}

fn only_part(after: &[&str]) -> bool {
    after
        .windows(2)
        .take(8)
        .any(|w| AUX.contains(&w[0]) && PART_DONE.contains(&w[1]))
}

const CLAUSE_MARKS: [char; 7] = [',', ';', ':', '(', ')', '—', '–'];

fn clause_mark(s: &str, i: usize, c: char) -> bool {
    let (before, after) = (&s[..i], &s[i + c.len_utf8()..]);
    CLAUSE_MARKS.contains(&c)
        && !(c == ','
            && before.ends_with(|d: char| d.is_ascii_digit())
            && after.starts_with(|d: char| d.is_ascii_digit()))
        && !(c == ':' && (before.ends_with(':') || after.starts_with(':')))
}

fn clause_start(s: &str, at: usize) -> usize {
    s[..at]
        .char_indices()
        .rev()
        .find(|&(i, c)| clause_mark(s, i, c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

fn clause_end(s: &str, from: usize) -> usize {
    s[from..]
        .char_indices()
        .find(|&(i, c)| clause_mark(s, from + i, c))
        .map_or(s.len(), |(i, _)| from + i)
}

fn bare(w: &str) -> &str {
    w.trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '-' | '_' | '/')))
}

fn words_at(s: &str, a: usize, b: usize) -> impl Iterator<Item = (usize, &str)> {
    let mut at = a;
    s[a..b]
        .split(' ')
        .map(move |w| {
            let p = at;
            at += w.len() + 1;
            (p, w)
        })
        .filter(|(_, w)| !w.is_empty())
}

fn in_path(s: &str, at: usize, end: usize) -> bool {
    s[..at].ends_with(['/', '.'])
        || s[end..]
            .strip_prefix('.')
            .is_some_and(|r| r.starts_with(char::is_alphanumeric))
}

const PAST_MARKERS: &[&str] = &[
    "previously",
    "formerly",
    "once",
    "earlier",
    "initially",
    "originally",
];

fn described(before: &[&str]) -> bool {
    let back = |k: usize| {
        before
            .len()
            .checked_sub(k)
            .and_then(|i| before.get(i))
            .copied()
    };
    let relative = |k: usize| back(k).is_some_and(|w| matches!(w, "that" | "which" | "who"));
    back(1).is_some_and(|w| PAST_MARKERS.contains(&w))
        || (matches!(back(1), Some("was" | "were" | "had")) && relative(2))
        || (matches!(back(1), Some("was" | "were")) && back(2) == Some("said"))
        || (back(1) == Some("been") && back(2) == Some("had") && relative(3))
}

const DETERMINERS: &[&str] = &[
    "the", "a", "an", "this", "that", "these", "those", "each", "every", "all", "both", "its",
    "my", "our", "their", "his", "her", "your", "any", "some", "one", "two", "three", "four",
    "five", "six", "seven", "eight", "nine", "ten",
];

const UNSETTLED: &[&str] = &[
    "to", "be", "will", "would", "can", "could", "should", "must", "may", "might", "shall",
    "needs", "need", "not",
];

const PAST_AUX: &[&str] = &["was", "were", "got", "had", "been"];

const CONDITIONS: &[&str] = &["once", "when", "if", "unless", "until"];

const AUX: &[&str] = &[
    "is", "was", "are", "were", "has", "have", "had", "been", "got", "now", "then", "since",
    "also", "finally", "already", "just", "all", "both",
];

const CUTS: &[&str] = &[
    "and", "but", "while", "whereas", "though", "although", "so", "yet", "until", "unless",
    "before", "because",
];

fn stretch(s: &str, from: usize, to: usize, span: (usize, usize), by: &[&str]) -> (usize, usize) {
    let (mut a, mut b) = (from, to);
    for (p, w) in words_at(s, from, to) {
        if !CUTS.contains(&bare(w)) {
            continue;
        }
        let past = p + w.len();
        if past <= span.0 {
            a = past;
        } else if p >= span.1 && !continues(&s[past..to], by) {
            b = p;
            break;
        }
    }
    (a, b)
}

fn continues(rest: &str, by: &[&str]) -> bool {
    let words: Vec<&str> = rest.split_whitespace().map(bare).collect();
    settles_at(&words, by, true)
}

const LANDING_VERBS: &[&str] = &[
    "pushed",
    "committed",
    "landed",
    "merged",
    "shipped",
    "squash-merged",
    "rebase-merged",
];

fn settles_at(words: &[&str], by: &[&str], place: bool) -> bool {
    let rest: Vec<&str> = words
        .iter()
        .copied()
        .skip_while(|w| AUX.contains(w))
        .collect();
    rest.first().is_some_and(|w| by.contains(w))
        || (place
            && (onto_main(&rest)
                || (rest.first().is_some_and(|w| {
                    LANDING_VERBS.contains(w) || matches!(*w, "lives" | "sits" | "went")
                }) && onto_main(&rest[1..]))
                || (rest.first().is_some_and(|w| {
                    LANDING_VERBS.contains(w) || matches!(*w, "took" | "brought" | "carried")
                }) && rest.get(1).is_some_and(|o| matches!(*o, "it" | "them"))
                    && onto_main(&rest[2..]))))
}

fn onto_main(words: &[&str]) -> bool {
    let Some((&first, rest)) = words.split_first() else {
        return false;
    };
    let lead = rest
        .iter()
        .take_while(|w| matches!(**w, "the" | "remote"))
        .count();
    let remote = rest[..lead].contains(&"remote");
    let mut names = rest[lead..].iter().copied();
    let remote_main = |n: &str| {
        matches!(
            n,
            "origin/main" | "origin/master" | "upstream/main" | "upstream/master"
        )
    };
    match (first, names.next()) {
        ("on" | "to" | "into" | "onto" | "in", Some(n)) => {
            let repo = n.split_once('/').is_some_and(|(owner, name)| {
                !owner.is_empty() && !name.is_empty() && !name.contains(['/', '.'])
            });
            remote_main(n)
                || matches!(n, "main" | "master" | "trunk")
                || ((matches!(n, "origin" | "upstream") || repo)
                    && names.next().is_some_and(|m| matches!(m, "main" | "master")))
        }
        ("matches" | "match" | "equals" | "at" | "with", Some(n)) => {
            remote_main(n) || (remote && matches!(n, "main" | "master"))
        }
        _ => false,
    }
}

const HOLDS: &[&str] = &["has", "contains", "includes", "carries", "got", "holds"];

fn main_claimed(s: &str) -> bool {
    let words: Vec<&str> = s.split_whitespace().map(bare).collect();
    (0..words.len()).any(|i| {
        let undenied = i
            .checked_sub(1)
            .and_then(|j| words.get(j))
            .is_none_or(|w| *w != "not" && !negation(w));
        let carried = || {
            let prev = i.checked_sub(1).and_then(|j| words.get(j)).copied();
            let back = &words[i.saturating_sub(3)..i];
            if back
                .iter()
                .any(|w| UNSETTLED.contains(w) || CHANGE_NOUNS.contains(w) || *w == "date")
            {
                return false;
            }
            prev.is_some_and(|p| AUX.contains(&p) && !matches!(p, "all" | "both"))
                || back
                    .iter()
                    .rev()
                    .take_while(|w| LANDING_VERBS.contains(w) || !verbish(w))
                    .any(|w| LANDING_VERBS.contains(w) || *w == "went")
        };
        undenied
            && ((onto_main(&words[i..]) && carried())
                || (matches!(
                    words[i],
                    "main" | "origin/main" | "upstream/main" | "master"
                ) && words.get(i + 1).is_some_and(|w| HOLDS.contains(w))))
    })
}

const REMOTES: &[&str] = &[
    "the", "remote", "origin", "upstream", "github", "gitlab", "our",
];

const MAINS: &[&str] = &[
    "main",
    "master",
    "trunk",
    "head",
    "default",
    "origin/main",
    "origin/master",
    "upstream/main",
    "upstream/master",
    "origin/head",
];

const BRANCHES: &[&str] = &[
    "develop",
    "development",
    "dev",
    "staging",
    "next",
    "beta",
    "stable",
    "release",
    "integration",
];

fn onto_branch(after: &[&str]) -> bool {
    let Some(k) = after
        .iter()
        .take(6)
        .position(|w| matches!(*w, "to" | "into" | "onto"))
    else {
        return false;
    };
    let mut names = after[k + 1..]
        .iter()
        .copied()
        .skip_while(|w| REMOTES.contains(w));
    let Some(name) = names.next() else {
        return false;
    };
    let then = names.next();
    let file = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphabetic()));
    if MAINS.contains(&name)
        || DETERMINERS.contains(&name)
        || count(name).is_some()
        || file
        || !name.starts_with(char::is_alphabetic)
    {
        return false;
    }
    if then.is_some_and(|w| matches!(w, "main" | "master")) {
        // An integration branch's own main (`pushed to the staging main`)
        // is off main; a repository's is main (`pushed to
        // example-org/codex-alab main`).
        return BRANCHES.contains(&name);
    }
    then == Some("branch")
        || name.contains(['/', '-', '_'])
        || main_grown(name)
        || (BRANCHES.contains(&name) && then.is_none_or(|w| JOINS.contains(&w)))
}

fn main_grown(name: &str) -> bool {
    ["main", "master"].iter().any(|m| {
        name.strip_prefix(m).is_some_and(|rest| {
            !rest.is_empty()
                && !matches!(rest, "ly" | "s")
                && !rest.starts_with("tain")
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
    })
}

const DISPOSABLE: &[&str] = &[
    "workaround",
    "workarounds",
    "hack",
    "hacks",
    "stopgap",
    "stopgaps",
    "stop-gap",
    "band-aid",
    "shim",
    "shims",
    "kludge",
    "approach",
    "attempt",
    "attempts",
    "idea",
    "experiment",
    "experiments",
    "prototype",
    "spike",
    "noise",
    "churn",
    "hotfix",
    "hotfixes",
    "copy",
    "copies",
    "duplicate",
    "duplicates",
];

const DISPOSABLE_MODS: &[&str] = &[
    "temporary",
    "interim",
    "hacky",
    "old",
    "outdated",
    "stale",
    "dead",
    "unused",
    "obsolete",
    "legacy",
    "deprecated",
    "redundant",
    "duplicate",
    "duplicated",
    "bad",
    "broken",
    "faulty",
    "buggy",
    "offending",
    "wrong",
    "debug",
    "accidental",
    "unrelated",
    "stray",
    "unintended",
    "bogus",
    "spurious",
    "vendored",
];

const CUT_ROWS: &[&str] = &["was cut", "were cut", "got cut", "been cut", "cut from"];

const CUT_THINGS: &[&str] = &[
    "release",
    "releases",
    "tag",
    "tags",
    "rc",
    "branch",
    "version",
    "build",
    "candidate",
];

fn disposable(word: &str, before: &[&str], after: &[&str]) -> bool {
    if after.iter().take(4).any(|w| {
        matches!(
            *w,
            "relanded" | "re-landed" | "remerged" | "re-merged" | "reapplied" | "re-applied"
        )
    }) {
        return true;
    }
    let passive = word.split(' ').next().is_some_and(|w| AUX.contains(&w))
        || CUT_ROWS.contains(&word)
        || before
            .last()
            .is_some_and(|w| matches!(*w, "was" | "were" | "is" | "are" | "got" | "been" | "be"));
    let np: Vec<&str> = if passive {
        let subject = before
            .iter()
            .rposition(|w| verbish(w) && !AUX.contains(w))
            .map_or(before, |i| &before[i + 1..]);
        subject
            .iter()
            .copied()
            .filter(|w| !AUX.contains(w))
            .collect()
    } else {
        after
            .iter()
            .copied()
            .take_while(|w| !PREPOSITIONS.contains(w) && !CUTS.contains(w) || *w == "and")
            .collect()
    };
    let mut things: Vec<Vec<&str>> = vec![Vec::new()];
    for w in np {
        if matches!(w, "and" | "or" | "plus") {
            things.push(Vec::new());
        } else if let Some(thing) = things.last_mut() {
            thing.push(w);
        }
    }
    let things: Vec<Vec<&str>> = things
        .into_iter()
        .map(|t| {
            t.into_iter()
                .take_while(|w| !PREPOSITIONS.contains(w) && *w != "of")
                .filter(|w| !DETERMINERS.contains(w) && count(w).is_none())
                .collect::<Vec<&str>>()
        })
        .filter(|t| !t.is_empty())
        .take_while(|t| {
            !t.first()
                .is_some_and(|w| verbish(w) && !DISPOSABLE_MODS.contains(w))
        })
        .collect();
    !things.is_empty()
        && things.iter().all(|t| {
            let head = t[t.len() - 1];
            let modifier = t.len().checked_sub(2).map(|i| t[i]);
            DISPOSABLE.contains(&head)
                || modifier.is_some_and(|m| DISPOSABLE_MODS.contains(&m))
                || (CUT_ROWS.contains(&word) && CUT_THINGS.contains(&head))
        })
}

const BRANCH_PREFIXES: &[&str] = &[
    "fix", "feat", "feature", "features", "bugfix", "hotfix", "release", "releases", "chore",
    "refactor", "wip", "topic", "exp", "dev", "docs", "test", "tmp",
];

fn branch_name(name: &str) -> bool {
    let Some((prefix, rest)) = name.split_once('/') else {
        return false;
    };
    let code = prefix.len() <= 3
        && prefix.starts_with(char::is_alphabetic)
        && prefix.bytes().skip(1).all(|b| b.is_ascii_digit());
    let round = (1..=3).contains(&prefix.len())
        && prefix.bytes().all(|b| b.is_ascii_lowercase())
        && rest.starts_with(|c: char| c.is_ascii_lowercase())
        && rest.bytes().any(|b| b.is_ascii_digit())
        && rest.bytes().all(|b| b.is_ascii_alphanumeric());
    !rest.is_empty()
        && !name.contains("://")
        && (BRANCH_PREFIXES.contains(&prefix)
            || code
            || round
            || rest.contains(['-', '_'])
            || matches!(prefix, "origin" | "upstream"))
}

fn on_branch(after: &[&str], landed: bool) -> bool {
    let mut names = after.iter().copied().skip_while(|w| REMOTES.contains(w));
    let Some(name) = names.next() else {
        return false;
    };
    let name = name.strip_suffix("'s").unwrap_or(name);
    let then = names.next();
    let file = name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphabetic()));
    if name == "branch" {
        return then.is_some_and(|n| {
            branch_name(n) || BRANCHES.contains(&n) || (n.contains(['-', '_']) && !n.contains('.'))
        });
    }
    if name.split_once('/').is_some_and(|(remote, m)| {
        MAINS.contains(&m) && !REMOTES.contains(&remote) && !MAINS.contains(&name)
    }) {
        return true;
    }
    if landed
        && !file
        && !MAINS.contains(&name)
        && name.starts_with(char::is_alphabetic)
        && (main_grown(name)
            || (then.is_some_and(|w| matches!(w, "main" | "master")) && BRANCHES.contains(&name))
            || ((BRANCHES.contains(&name) || name.contains(['-', '_']))
                && then.is_none_or(|w| JOINS.contains(&w))))
    {
        return true;
    }
    if MAINS.contains(&name)
        || DETERMINERS.contains(&name)
        || count(name).is_some()
        || file
        || !name.starts_with(char::is_alphabetic)
        || name
            .split_once('/')
            .is_some_and(|(_, m)| MAINS.contains(&m))
    {
        return false;
    }
    then == Some("branch")
        || branch_name(name)
        || (matches!(name, "develop" | "development" | "integration")
            && then.is_none_or(|w| JOINS.contains(&w)))
}

const CODE_PARTS: &[&str] = &[
    "command",
    "commands",
    "subcommand",
    "flag",
    "flags",
    "option",
    "options",
    "api",
    "module",
    "function",
    "method",
    "code",
    "logic",
    "parser",
    "handler",
    "helper",
    "implementation",
    "type",
    "struct",
    "enum",
    "crate",
    "help",
];

const CHANGE_PARTS: &[&str] = &[
    "description",
    "title",
    "body",
    "template",
    "comment",
    "comments",
    "thread",
    "threads",
    "link",
    "url",
    "number",
    "author",
    "label",
    "labels",
    "summary",
    "text",
];

fn unplaced(s: &str, (prior, said_main): (&str, bool), before: &[&str], after: &[&str]) -> bool {
    let object = &after[..after.len().min(4)];
    let a_branch =
        |w: &&str| branch_name(w) && !w.split_once('/').is_some_and(|(_, m)| MAINS.contains(&m));
    if (object.iter().any(|w| matches!(*w, "branch" | "branches"))
        && !object.iter().any(|w| MAINS.contains(w)))
        || before.iter().any(a_branch)
    {
        return !main_claimed(s);
    }
    let placed = before.iter().any(|w| MAINS.contains(w))
        || after
            .iter()
            .find(|w| !matches!(**w, "the" | "it" | "them"))
            .is_some_and(|w| MAINS.contains(w))
        || (0..after.len()).any(|k| {
            onto_main(&after[k..])
                && !after[..k]
                    .iter()
                    .any(|w| CHANGE_NOUNS.contains(w) || UNSETTLED.contains(w))
        });
    let tagged = before
        .iter()
        .chain(after)
        .any(|w| matches!(*w, "tag" | "tags") || version(w))
        || s.split_whitespace().next().map(bare).is_some_and(version);
    !placed
        && !tagged
        && !said_main
        && ![s, prior]
            .iter()
            .any(|t| landing_claimed(t) || main_named(t))
}

fn version(w: &str) -> bool {
    let n = w.strip_prefix('v').unwrap_or(w);
    n.starts_with(|c: char| c.is_ascii_digit())
        && n.contains('.')
        && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn main_named(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().map(bare).collect();
    (0..words.len()).any(|i| {
        let back = &words[i.saturating_sub(3)..i];
        !back.iter().any(|w| {
            negation(w)
                || matches!(
                    *w,
                    "not"
                        | "never"
                        | "without"
                        | "before"
                        | "nearly"
                        | "almost"
                        | "rebased"
                        | "rebasing"
                        | "rebase"
                        | "based"
                        | "pr"
                        | "prs"
                        | "mr"
                        | "request"
                        | "requests"
                        | "be"
                        | "to"
                        | "date"
                        | "sync"
                        | "against"
                        | "targeting"
                        | "targets"
                        | "opened"
                        | "will"
                )
        }) && (onto_main(&words[i..])
            || (matches!(words[i], "fast-forwarded" | "ff-merged")
                && words.get(i + 1).is_some_and(|m| MAINS.contains(m))))
    })
}

fn change_open(
    s: &str,
    (from, to): (usize, usize),
    st: &Stand<'_>,
    before: &[&str],
    after: &[&str],
) -> bool {
    let lead = s[..from].trim_end();
    if matches!(
        st.word,
        "the second one" | "the other one" | "the second" | "the other"
    ) {
        let named = s[..st.at]
            .split_whitespace()
            .map(bare)
            .any(|w| CHANGE_NOUNS[..4].contains(&w) || w.starts_with('#'))
            || s[..st.at].contains("pull request");
        if !named {
            return false;
        }
    }
    let included = after.len() <= 2 && after.last() == Some(&"included") && landing_claimed(lead);
    let bracketed = lead.ends_with('(')
        && s[to..].strip_prefix(')').is_none_or(|r| {
            r.split_whitespace()
                .map(bare)
                .find(|w| !w.is_empty())
                .is_none_or(|w| !verbish(w))
        });
    if included
        || bracketed
        || (lead.ends_with(',')
            && before.is_empty()
            && after.is_empty()
            && !s[to..].starts_with(','))
        || after.first().is_some_and(|w| CHANGE_PARTS.contains(w))
    {
        return false;
    }
    let names = |words: &[&str]| {
        let landing = words
            .iter()
            .rposition(|w| verbish(w) || CUTS.contains(w))
            .is_some_and(|k| {
                (SHIPS.contains(&words[k])
                    || LANDING_VERBS.contains(&words[k])
                    || words[k] == "rebase-merged")
                    && !words[..k]
                        .iter()
                        .rev()
                        .take(2)
                        .any(|w| UNSETTLED.contains(w) || negation(w))
            });
        landing
            || words.windows(2).any(|w| {
                matches!(w[1], "as" | "in")
                    && (SHIPS.contains(&w[0]) || LANDING_VERBS.contains(&w[0]))
            })
            || words.iter().enumerate().any(|(i, w)| {
                !words[i + 1..]
                    .iter()
                    .any(|x| verbish(x) || CUTS.contains(x))
                    && matches!(
                        *w,
                        "no" | "none"
                            | "zero"
                            | "0"
                            | "via"
                            | "see"
                            | "per"
                            | "fixes"
                            | "closes"
                            | "resolves"
                            | "refs"
                            | "addresses"
                            | "supersedes"
                            | "fix"
                            | "close"
                            | "resolve"
                    )
            })
    };
    let closed = st.word == "#n"
        && before
            .iter()
            .rposition(|w| verbish(w))
            .is_some_and(|k| matches!(before[k], "closed" | "fixed" | "resolved" | "addressed"));
    let same = before.last().is_some_and(|w| *w == "same");
    let run = st.word == "#n"
        && before
            .last()
            .is_some_and(|w| matches!(*w, "run" | "build" | "job" | "pipeline" | "workflow"));
    if run {
        return false;
    }
    let record = matches!(before, [.., "in" | "on", "the"])
        && before.iter().any(|w| {
            matches!(
                *w,
                "numbers"
                    | "results"
                    | "details"
                    | "notes"
                    | "logs"
                    | "benchmarks"
                    | "measurements"
                    | "figures"
                    | "writeup"
                    | "write-up"
                    | "rationale"
                    | "discussion"
                    | "context"
            )
        });
    if closed || same || record || names(before) {
        return false;
    }
    let mut f = from;
    while !after.iter().any(|w| verbish(w)) {
        let lead = s[..f].trim_end();
        let Some(comma) = lead.strip_suffix(',').map(str::len) else {
            break;
        };
        let start = clause_start(s, comma);
        let words: Vec<&str> = s[start..comma].split_whitespace().map(bare).collect();
        if names(&words) {
            return false;
        }
        f = start;
    }
    if !after.iter().any(|w| verbish(w)) && list_landed(s, to) {
        return false;
    }
    let (a, b) = stretch(s, from, to, (st.at, st.end()), SHIPS);
    let words: Vec<&str> = s[a..b]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let shared = before.iter().all(list_item) && {
        let v = after.iter().take_while(|w| list_item(w)).count();
        after[..v].contains(&"and") && settles_with(&after[v..], SHIPS, true)
    };
    !shared && !lands_in(&words) && !landed_again(s, st, b)
}

fn landed_again(s: &str, st: &Stand<'_>, to: usize) -> bool {
    let name = &s[st.at..st.end()];
    if st.word != "#n" || !name.starts_with('#') {
        return false;
    }
    let mut from = st.end();
    while let Some(k) = s[from..].find(name).map(|k| from + k) {
        let end = k + name.len();
        from = end;
        if s[end..].starts_with(|c: char| c.is_alphanumeric() || c == '\'' || c == '’') {
            continue;
        }
        let (a, b) = stretch(s, clause_start(s, k), clause_end(s, end), (k, end), SHIPS);
        let words: Vec<&str> = s[a..b]
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        if k >= to && lands_in(&words) {
            return true;
        }
    }
    false
}

fn list_item(w: &&str) -> bool {
    w.starts_with('#')
        || w.bytes().all(|b| b.is_ascii_digit())
        || matches!(
            *w,
            "and"
                | "or"
                | "the"
                | "its"
                | "pr"
                | "prs"
                | "mr"
                | "mrs"
                | "pull"
                | "request"
                | "requests"
                | "follow-up"
                | "both"
                | "all"
        )
}

fn lands_in(words: &[&str]) -> bool {
    (0..words.len()).any(|k| {
        let w = words[k];
        let landing =
            SHIPS.contains(&w) || w == "rebase-merged" || settles_at(&words[k..], &[], true);
        landing
            && !words[..k]
                .iter()
                .rev()
                .take(3)
                .any(|x| UNSETTLED.contains(x) || negation(x) || *x == "without")
            && !words[k..].iter().any(|x| CONDITIONS.contains(x))
    })
}

fn list_landed(s: &str, mut to: usize) -> bool {
    let item = list_item;
    while s[to..].starts_with([',', '(', ')', ':']) {
        let start = to + 1;
        let end = clause_end(s, start);
        let words: Vec<&str> = s[start..end]
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        match words.iter().position(|w| starts_predicate(w, SHIPS)) {
            Some(v) => {
                return words[..v].iter().all(item) && settles_with(&words[v..], SHIPS, true);
            }
            None if words.iter().all(item) => to = end,
            None => return false,
        }
    }
    false
}

fn below_hundred(n: &str) -> bool {
    const TENS: [&str; 9] = [
        "ten", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    let decimal = !n.is_empty()
        && n.starts_with(|c: char| c.is_ascii_digit())
        && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && n.parse::<f64>().is_ok_and(|v| v < 100.0);
    let spelled = !n.starts_with(|c: char| c.is_ascii_digit()) && count(n).is_some();
    decimal
        || spelled
        || TENS.contains(&n)
        || n.split_once('-')
            .is_some_and(|(t, u)| TENS.contains(&t) && count(u).is_some())
}

const WORK_LABELS: &[&str] = &[
    "docs",
    "documentation",
    "tests",
    "migration",
    "port",
    "porting",
    "conversion",
    "progress",
    "done",
    "completion",
    "implementation",
    "rewrite",
    "refactor",
    "work",
    "readers",
    "lanes",
    "status",
];

fn percent_part(after: &str) -> bool {
    after
        .split_whitespace()
        .map(bare)
        .find(|w| !w.is_empty())
        .is_some_and(|w| {
            w == "of"
                || SETTLED.contains(&w)
                || LANDED.contains(&w)
                || matches!(
                    w,
                    "coverage" | "covered" | "migrated" | "ported" | "tested" | "there"
                )
        })
}

const JOINS: &[&str] = &[
    "and", "but", "then", "only", "too", "first", "so", "while", "now", "with", "for", "on",
];

fn settled(s: &str, from: usize, to: usize, st: &Stand<'_>) -> bool {
    if ["not ", "never ", "did not ", "could", "can", "unable"]
        .iter()
        .any(|p| st.word.starts_with(p))
    {
        // A lack negates its own clause: `not all of the comments were
        // addressed` settles nothing.
        return false;
    }
    let (at, end) = (st.at, st.end());
    let mut back: Vec<&str> = s[from..at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if let Some(k) = back.iter().rposition(|w| *w == "that")
        && k > 0
        && CONFIRMED.contains(&back[k - 1])
        && !back[..k]
            .iter()
            .any(|w| negation(w) || UNSETTLED.contains(w))
    {
        back.drain(..=k);
    }
    let whole: Vec<&str> = s[end..to]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let np_end = whole
        .iter()
        .position(|w| starts_predicate(w, &[]))
        .unwrap_or(whole.len());
    let np: Vec<&str> = back.iter().chain(&whole[..np_end]).copied().collect();
    let by = settlers(st, &np);
    if by.is_empty() {
        return false;
    }
    let place = st.lands_by_place()
        || CHANGE_NOUNS.contains(&st.word)
        || np.iter().any(|w| CHANGE_NOUNS.contains(w));
    if let Some(v) = object_verb(&back, st) {
        let did = worker_did(&back[..v]) && !whole.iter().any(|w| UNDOES.contains(w));
        if did
            && (by.contains(&back[v])
                || (place
                    && GONE.contains(&back[v])
                    && (landing_claimed(s) || merged_first(&whole))))
        {
            return true;
        }
    }
    if matches!(st.reads(), Reads::Change) && np_end == whole.len() && s[to..].starts_with(':') {
        let item: Vec<&str> = s[to + 1..clause_end(s, to + 1)]
            .split_whitespace()
            .map(bare)
            .filter(|w| !w.is_empty())
            .collect();
        return settles_once(&item, &by, place)
            && !item
                .iter()
                .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without");
    }
    let (_, b) = stretch(s, from, to, (at, end), &by);
    let mut fwd: Vec<&str> = s[end..b]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if whole.first() == Some(&"and") {
        let shared = (2..=4).find(|&k| {
            whole
                .get(k)
                .is_some_and(|w| AUX.contains(w) && !matches!(*w, "all" | "both"))
                && whole[1..k].iter().all(|w| !verbish(w))
        });
        if let Some(k) = shared {
            fwd = whole[k..].to_vec();
        }
    }
    if b == to
        && fwd
            .iter()
            .any(|w| AUX.contains(w) && !matches!(*w, "all" | "both"))
    {
        let mut t = to;
        while s[t..].starts_with(',') {
            let next = clause_end(s, t + 1);
            let words: Vec<&str> = s[t + 1..next]
                .split_whitespace()
                .map(bare)
                .filter(|w| !w.is_empty())
                .collect();
            if !words.first().is_some_and(|w| past_form(w))
                || words.iter().any(|w| negation(w) || UNSETTLED.contains(w))
            {
                break;
            }
            fwd.push("and");
            fwd.extend(words);
            t = next;
        }
    }
    let lead = st.word.split(' ').next().unwrap_or("");
    let relative = past_form(lead)
        && !st.word.contains(' ')
        && back
            .iter()
            .position(|w| matches!(*w, "i" | "we" | "you" | "they"))
            .is_some_and(|p| {
                p >= 2
                    && DETERMINERS.contains(&back[0])
                    && !back[1..p].iter().any(|w| verbish(w))
                    && back[p + 1..]
                        .iter()
                        .all(|w| matches!(*w, "had" | "have" | "has"))
            });
    if relative {
        return whole
            .first()
            .is_some_and(|w| AUX.contains(w) && !matches!(*w, "all" | "both"))
            && !whole
                .iter()
                .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without")
            && settles_with(&whole, &by, place);
    }
    let named =
        !(AUX.contains(&lead) || FINITE.contains(&lead) || lead == "still" || lead == "only")
            && (back.last().is_none_or(|w| DETERMINERS.contains(w)) || !past_form(lead));
    if back.iter().any(|w| verbish(w)) || !named {
        let now = back.iter().any(|w| matches!(*w, "was" | "were"))
            && s[to..].strip_prefix(", now ").is_some_and(|r| {
                let words: Vec<&str> = r[..clause_end(r, 0)]
                    .split_whitespace()
                    .map(bare)
                    .filter(|w| !w.is_empty())
                    .collect();
                settles_once(&words, &by, place)
            });
        return now
            || fwd.iter().enumerate().any(|(i, w)| {
                matches!(*w, "and" | "then") && settles_once(&fwd[i + 1..], &by, place)
            });
    }
    let head = usize::from(
        past_form(lead)
            && back.last().is_some_and(|w| DETERMINERS.contains(w))
            && fwd
                .first()
                .is_some_and(|w| !AUX.contains(w) && !by.contains(w) && !LANDING_VERBS.contains(w)),
    );
    let mut starts = fwd
        .iter()
        .enumerate()
        .skip(head)
        .filter(|(_, w)| starts_predicate(w, &by))
        .map(|(k, _)| k);
    if let ["no", "longer", "exists" | "exist", rest @ ..] = &fwd[head.min(fwd.len())..] {
        return by.contains(&"gone")
            && rest
                .iter()
                .all(|w| matches!(*w, "anymore" | "now" | "at" | "all"));
    }
    let branch = np
        .iter()
        .any(|w| matches!(*w, "branch" | "branches" | "worktree" | "worktrees"));
    let gone = fwd.iter().position(|w| AUX.contains(w)).is_some_and(|v| {
        !fwd[..v].iter().any(|w| verbish(w)) && fwd.get(v + 1).is_some_and(|w| GONE.contains(w))
    });
    if branch && gone && landing_claimed(s) {
        return true;
    }
    let Some(mut k) = starts.next() else {
        return false;
    };
    if fwd
        .first()
        .is_some_and(|w| matches!(*w, "i" | "we" | "you" | "they" | "that" | "which" | "who"))
    {
        let Some(next) = starts.next() else {
            return false;
        };
        k = next;
    }
    k <= 5
        && !fwd[..k]
            .iter()
            .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without")
        && (settles_with(&fwd[k..], &by, place) || empty_now(&fwd[k..]))
}

const CONFIRMED: &[&str] = &[
    "checked",
    "double-checked",
    "confirmed",
    "verified",
    "ensured",
    "saw",
];

fn empty_now(words: &[&str]) -> bool {
    let skip = words.iter().take_while(|w| AUX.contains(w)).count();
    skip > 0
        && match &words[skip..] {
            ["empty"] => true,
            ["empty", w, ..] => matches!(*w, "and" | "now" | "too" | "again"),
            _ => false,
        }
}

const FINITE: &[&str] = &[
    "is", "are", "am", "was", "were", "has", "have", "had", "remains", "remain", "stays", "stay",
    "will", "would", "can", "could", "should", "must", "may", "might", "does", "do", "needs",
    "need", "requires", "require", "lacks", "wants", "waits", "wait", "awaits",
];

fn settlers(st: &Stand<'_>, np: &[&str]) -> Vec<&'static str> {
    let by = st.settled_by();
    if by.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&'static str> = if FLAKY.contains(&st.word) {
        DEFLAKED.to_vec()
    } else {
        by.to_vec()
    };
    let change = np.iter().any(|w| CHANGE_NOUNS.contains(w)) || CHANGE_NOUNS.contains(&st.word);
    if st.word == "stray" {
        // A stray thing is junk, a stray commit too: gone is done (`the stray
        // debug print is removed`, `the stray debug commit was dropped`).
        out.extend(["removed", "deleted", "gone", "dropped"]);
    } else if change {
        out = SHIPS.to_vec();
    } else if st.word == "#n"
        || st
            .word
            .split(' ')
            .any(|w| matches!(w, "issue" | "issues" | "ticket" | "tickets"))
    {
        // An issue (`the tracking issue`), or a bare number, which names
        // one as often as a change: its fix or its close settles it (`#45
        // is closed`); a number a change noun names (`PR #45 is closed`) is
        // a change, settled only by landing.
        out.extend(["fixed", "resolved", "closed"]);
    } else {
        if np.iter().any(|w| REVIEW_NOUNS.contains(w)) {
            out.extend(REVIEWED);
        }
        if matches!(st.word, "remaining" | "leftover" | "outstanding" | "paused")
            && np.iter().any(|w| CHECK_NOUNS.contains(w))
        {
            out.extend(CHECKS);
        }
    }
    out
}

fn starts_predicate(w: &str, by: &[&str]) -> bool {
    (AUX.contains(&w) && !matches!(w, "all" | "both"))
        || by.contains(&w)
        || LANDING_VERBS.contains(&w)
        || (verbish(w) && !PREPOSITIONS.contains(&w) && w != "on")
}

fn settles_with(words: &[&str], by: &[&str], place: bool) -> bool {
    settles_once(words, by, place)
        || words
            .iter()
            .enumerate()
            .any(|(i, w)| matches!(*w, "and" | "then") && settles_once(&words[i + 1..], by, place))
}

fn settles_once(words: &[&str], by: &[&str], place: bool) -> bool {
    let skip = words.iter().take_while(|w| AUX.contains(w)).count();
    let past = skip
        .checked_sub(1)
        .and_then(|i| words.get(i))
        .is_some_and(|w| PAST_AUX.contains(w));
    settles_at(words, by, place) && (past || !words[skip..].iter().any(|w| CONDITIONS.contains(w)))
}

fn object_verb(back: &[&str], st: &Stand<'_>) -> Option<usize> {
    let own = st
        .word
        .split(' ')
        .next()
        .is_some_and(|w| DETERMINERS.contains(&w) || w == "local");
    let determiner = |w: &str| {
        DETERMINERS.contains(&w) || (!w.is_empty() && w.bytes().all(|c| c.is_ascii_digit()))
    };
    let d = match back {
        _ if own => back.len(),
        [.., d] if determiner(d) => back.len() - 1,
        [.., d, w] if determiner(d) && !CUTS.contains(w) && !negation(w) => back.len() - 2,
        _ => return None,
    };
    d.checked_sub(1)
}

fn worker_did(pre: &[&str]) -> bool {
    let from = pre
        .iter()
        .rposition(|w| matches!(*w, "and" | "then"))
        .map_or(0, |i| i + 1);
    pre[from..].iter().all(|w| {
        matches!(
            *w,
            "i" | "we"
                | "then"
                | "also"
                | "finally"
                | "just"
                | "so"
                | "have"
                | "had"
                | "has"
                | "now"
                | "already"
                | "since"
        )
    })
}

const GONE: &[&str] = &["deleted", "removed", "dropped", "pruned"];

fn merged_first(whole: &[&str]) -> bool {
    matches!(
        whole,
        ["after", "merging" | "landing"] | ["after", "merging" | "landing", "it" | "them" | "both"]
    )
}

const UNDOES: &[&str] = &[
    "without", "before", "instead", "unmerged", "unlanded", "unpushed", "never", "not",
];

fn landing_claimed(s: &str) -> bool {
    let words: Vec<&str> = s.split_whitespace().map(bare).collect();
    main_claimed(s)
        || words.iter().enumerate().any(|(i, w)| {
            matches!(*w, "merged" | "landed")
                && !words[..i]
                    .iter()
                    .rev()
                    .take(3)
                    .any(|x| negation(x) || matches!(*x, "not" | "without" | "before"))
        })
}

fn past_aside(s: &str, to: usize) -> Option<(String, usize)> {
    let rest = &s[to..];
    for lead in [", then ", ", and then ", ", but then "] {
        if let Some(r) = rest.strip_prefix(lead) {
            let joined = format!("{} then {r}", &s[..to]);
            let end = clause_end(&joined, to + 1);
            return Some((joined, end));
        }
    }
    let mark = rest.chars().next()?;
    let (close, most) = match mark {
        ',' => (',', 4),
        '—' | '–' => (mark, 6),
        '(' => (')', 8),
        _ => return None,
    };
    let inner_at = to + mark.len_utf8();
    let len = s[inner_at..].find(close)?;
    let inner = &s[inner_at..inner_at + len];
    let after = &s[inner_at + len + close.len_utf8()..];
    let words: Vec<&str> = inner.split_whitespace().map(bare).collect();
    let first = after.split_whitespace().map(bare).find(|w| !w.is_empty());
    let participle = match words.as_slice() {
        ["as", w] => past_form(w),
        [w, rest @ ..] => {
            past_form(w)
                && !rest
                    .iter()
                    .any(|r| verbish(r) && !PREPOSITIONS.contains(r) && *r != "on")
        }
        [] => false,
    } && !words.iter().any(|w| negation(w) || UNSETTLED.contains(w));
    if words.is_empty()
        || words.len() > most
        || inner.contains(CLAUSE_MARKS)
        || (words.iter().any(|w| verbish(w))
            && !participle
            && !words
                .first()
                .is_some_and(|w| matches!(*w, "which" | "who" | "that")))
        || !first.is_some_and(verbish)
    {
        return None;
    }
    let joined = format!("{}{after}", &s[..to]);
    let end = clause_end(&joined, to);
    Some((joined, end))
}

fn even_with_remote(after: &[&str]) -> bool {
    let remote = |m: &str| matches!(m, "origin/main" | "origin/master" | "upstream/main");
    if let [
        "is",
        "even" | "level" | "identical" | "equal",
        "with" | "to",
        m,
    ]
    | ["is", "in", "sync", "with", m]
    | ["is", "at", m] = after
    {
        return remote(m);
    }
    let rest = match after {
        ["and", m, rest @ ..]
            if matches!(*m, "origin/main" | "origin/master" | "upstream/main") =>
        {
            rest
        }
        [verb, ..] if matches!(*verb, "matches" | "equals") => after,
        _ => return false,
    };
    matches!(
        rest,
        ["point" | "are", "at", "the", "same", "commit" | "sha"]
            | ["point", "to", "the", "same", "commit" | "sha"]
            | ["match" | "agree"]
            | ["matches" | "equals", "origin/main" | "upstream/main"]
            | ["are", "in", "sync" | "step"]
            | ["are", "identical" | "even" | "equal"]
    )
}

fn also_landed(s: &str, (from, to): (usize, usize), st: &Stand<'_>, stands: &[Stand<'_>]) -> bool {
    let words: Vec<&str> = s[from..to]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let lead = s[..from].trim_end();
    let added = matches!(words.last(), Some(&"too" | &"also" | &"included"))
        || words.ends_with(&["as", "well"]);
    added
        && words.len() <= 6
        && matches!(
            st.word,
            "follow-up"
                | "follow-ups"
                | "followup"
                | "followups"
                | "pr"
                | "prs"
                | "pull request"
                | "pull requests"
                | "mr"
                | "mrs"
                | "#n"
        )
        && lead.ends_with(',')
        && landing_claimed(lead)
        && !words.iter().any(|w| verbish(w) && *w != "included")
        && !stands.iter().any(|o| {
            (from..to).contains(&o.at) && o.at != st.at && !matches!(o.reads(), Reads::Change)
        })
}

const BEFORE_NOW: &[&str] = &[
    "earlier",
    "before",
    "previously",
    "initially",
    "originally",
    "at",
    "first",
    "once",
    "yesterday",
];

fn aside_settled(s: &str, st: &Stand<'_>) -> bool {
    let (at, end) = (st.at, st.end());
    let Some(open_at) = s[..at].rfind(CLAUSE_MARKS) else {
        return false;
    };
    let Some(mark) = s[open_at..].chars().next() else {
        return false;
    };
    let close = match mark {
        ',' | '—' | '–' => mark,
        '(' => ')',
        _ => return false,
    };
    let Some(close_at) = s[end..].find(close).map(|c| end + c) else {
        return false;
    };
    let inner: Vec<&str> = s[open_at + mark.len_utf8()..close_at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let row: Vec<&str> = st.word.split(' ').collect();
    let relative = inner
        .first()
        .is_some_and(|w| matches!(*w, "which" | "who" | "that"));
    let mut i = 0;
    if relative {
        if !inner
            .get(1)
            .is_some_and(|w| matches!(*w, "was" | "were" | "had"))
        {
            return false;
        }
        i = 2;
        if inner.get(i) == Some(&"been") {
            i += 1;
        }
    }
    if inner.get(i..i + row.len()) != Some(row.as_slice()) {
        return false;
    }
    let dated = &inner[i + row.len()..];
    let on: Vec<&str> = dated
        .iter()
        .copied()
        .filter(|w| !BEFORE_NOW.contains(w))
        .collect();
    let waited_on = on.is_empty()
        || (on.len() <= 3
            && matches!(on[0], "on" | "by" | "in" | "for" | "behind")
            && dated.iter().any(|w| BEFORE_NOW.contains(w) && *w != "at")
            && !on
                .iter()
                .any(|w| negation(w) || UNSETTLED.contains(w) || matches!(*w, "still" | "now")));
    if !waited_on || (dated.is_empty() && !relative) {
        return false;
    }
    let from = clause_start(s, open_at);
    let subject: Vec<&str> = s[from..open_at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    if subject.is_empty() || subject.iter().any(|w| verbish(w)) {
        return false;
    }
    let rest = &s[close_at + close.len_utf8()..];
    let pred: Vec<&str> = rest[..clause_end(rest, 0)]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let by = match settlers(st, &subject) {
        by if by.is_empty() => SHIPS.to_vec(),
        by => by,
    };
    let place = st.lands_by_place() || subject.iter().any(|w| CHANGE_NOUNS.contains(w));
    pred.first().is_some_and(|w| starts_predicate(w, &by))
        && !pred
            .iter()
            .any(|w| UNSETTLED.contains(w) || negation(w) || *w == "without")
        && settles_with(&pred, &by, place)
}

fn nouns_in(s: &str, a: usize, b: usize, nouns: &[&str]) -> bool {
    let mut prev = None;
    s[a..b].split_whitespace().any(|raw| {
        let hit = noun_in(prev, raw, nouns);
        prev = Some(raw);
        hit
    })
}

const SECRETS: &[&str] = &[
    "token",
    "tokens",
    "key",
    "keys",
    "password",
    "passwords",
    "secret",
    "secrets",
    "passphrase",
];

const SECRET_KINDS: &[&str] = &[
    "crates.io",
    "npm",
    "pypi",
    "api",
    "access",
    "auth",
    "github",
    "gitlab",
    "release",
    "publish",
    "publishing",
    "signing",
    "deploy",
    "personal",
    "apple",
    "notary",
    "ssh",
    "gpg",
    "admin",
];

const NOT_POSSESSIVE: &[&str] = &[
    "it's", "that's", "what's", "there's", "here's", "let's", "he's", "she's", "who's", "one's",
];

fn noun_in(prev: Option<&str>, raw: &str, nouns: &[&str]) -> bool {
    let w = bare(raw);
    let numbered = || {
        let t = raw.trim_matches(|c: char| matches!(c, ',' | '.' | ':' | ';' | '(' | ')'));
        t.len() > 1 && t.starts_with(['#', '!']) && t[1..].bytes().all(|b| b.is_ascii_digit())
    };
    let secret = || {
        SECRETS.contains(&w)
            && prev.map(bare).is_some_and(|p| {
                (p.ends_with("'s") && !NOT_POSSESSIVE.contains(&p)) || SECRET_KINDS.contains(&p)
            })
    };
    nouns.contains(&w)
        || w.strip_suffix("'s").is_some_and(|w| nouns.contains(&w))
        || (nouns.contains(&"tests") && (w.contains("tests::") || w.ends_with("::tests")))
        || (nouns.contains(&"#n") && numbered())
        || (nouns.contains(&"#secret") && secret())
        || (nouns.contains(&"#branch")
            && branch_name(w)
            && !w.split_once('/').is_some_and(|(_, m)| MAINS.contains(&m)))
}

fn labelled(s: &str, from: usize, before: &[&str], nouns: &[&str]) -> bool {
    if !before.iter().all(|w| AUX.contains(w)) {
        return false;
    }
    let lead = s[..from].trim_end();
    let label_end = if let Some(l) = lead.strip_suffix(':') {
        l.len()
    } else if lead.ends_with(',') {
        match lead.rfind(':') {
            Some(colon) if !lead[colon..].contains([';', '(', ')']) => colon,
            _ => return false,
        }
    } else if lead.ends_with(')') {
        match lead.rfind('(') {
            Some(open) => open,
            None => return false,
        }
    } else {
        return false;
    };
    nouns_in(s, clause_start(s, label_end), label_end, nouns)
}

fn negation(w: &str) -> bool {
    OPEN_NEGATIONS.contains(&w) || matches!(w, "nobody" | "noone" | "no-one")
}

const CONTRASTS: &[&str] = &[
    "but", "while", "whereas", "though", "although", "yet", "however",
];

const GLUE: &[&str] = &[
    "and", "or", "nor", "a", "an", "any", "also", "either", "neither",
];

const VERBS: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "has", "have", "had", "will", "would", "can",
    "could", "do", "does", "did", "remains", "remain", "stays", "needs", "awaits", "waits",
];

const STRONG: &[&str] = &[
    "the", "this", "that", "these", "those", "its", "my", "our", "their", "his", "her", "your",
];

fn denied(s: &str, from: usize, at: usize, st: &Stand<'_>, stands: &[Stand<'_>]) -> bool {
    let covered = |p: usize| stands.iter().any(|o| (o.at..o.end()).contains(&p));
    let before: Vec<(usize, &str)> = words_at(s, from, at).map(|(p, w)| (p, bare(w))).collect();
    let reach = reach(&before, stands);
    let near = &before[reach..];
    let has = |w: &str| near.iter().any(|&(_, x)| x == w);
    if near.iter().any(|&(_, w)| negation(w))
        || (!st.word.starts_with("not ") && near.last().is_some_and(|&(_, w)| w == "not"))
        || (has("not")
            && ["any", "anything", "anyone", "anybody"]
                .iter()
                .any(|w| has(w)))
    {
        return true;
    }
    let to = clause_end(s, st.end());
    if reach > 0 || s[to..].starts_with(':') {
        return false;
    }
    let tail: Vec<(usize, &str)> = words_at(s, st.end(), to)
        .map(|(p, w)| (p, bare(w)))
        .filter(|(_, w)| !w.is_empty())
        .collect();
    let listed =
        |words: &[(usize, &str)]| words.iter().all(|&(p, w)| covered(p) || GLUE.contains(&w));
    let mut cut = from;
    let mut current = before;
    let mut first = true;
    loop {
        if cut == 0 || !s[..cut].ends_with(',') {
            return false;
        }
        let start = clause_start(s, cut - 1);
        if s[..start].trim_end().ends_with(':') {
            return false;
        }
        let previous: Vec<(usize, &str)> = words_at(s, start, cut - 1)
            .map(|(p, w)| (p, bare(w)))
            .collect();
        let predicates = listed(&current) && (!first || listed(&tail));
        let joined = current
            .iter()
            .chain(if first { tail.iter() } else { [].iter() })
            .any(|&(_, w)| matches!(w, "or" | "nor" | "and"));
        let subjects = previous.len() >= 2
            && matches!(previous[0].1, "no" | "neither" | "zero")
            && !previous.iter().any(|&(_, w)| VERBS.contains(&w))
            && joined
            && !current
                .iter()
                .any(|&(_, w)| VERBS.contains(&w) || STRONG.contains(&w) || CONTRASTS.contains(&w));
        if !(predicates || subjects) {
            return false;
        }
        if previous.iter().any(|&(_, w)| negation(w)) {
            return true;
        }
        current = previous;
        cut = start;
        first = false;
    }
}

const REACH_BREAKS: &[&str] = &[
    "so",
    "because",
    "since",
    "as",
    "then",
    "therefore",
    "hence",
    "which",
    "until",
    "unless",
    "when",
    "if",
];

fn reach(before: &[(usize, &str)], stands: &[Stand<'_>]) -> usize {
    let covered = |p: usize| stands.iter().any(|o| (o.at..o.end()).contains(&p));
    before
        .iter()
        .enumerate()
        .rev()
        .find(|&(i, &(_, w))| {
            CONTRASTS.contains(&w)
                || REACH_BREAKS.contains(&w)
                || (w == "and"
                    && before
                        .get(i + 1)
                        .is_some_and(|&(p, x)| !(covered(p) || GLUE.contains(&x) || negation(x))))
        })
        .map_or(0, |(i, _)| i + 1)
}

const WORK_NOUNS: &[&str] = &[
    "pr", "prs", "mr", "mrs", "change", "changes", "commit", "commits", "branch", "branches",
    "fix", "fixes", "work", "code", "patch", "patches", "push", "merge", "part", "piece",
];

const BESIDE_LANDING: &[&str] = &[
    "else",
    "new",
    "more",
    "other",
    "further",
    "extra",
    "but",
    "except",
    "besides",
    "apart",
    "without",
    "since",
    "after",
    "afterwards",
    "anywhere",
    "elsewhere",
    "outside",
    "than",
];

const MERGE_THINGS: &[&str] = &[
    "conflict",
    "conflicts",
    "commit",
    "commits",
    "request",
    "requests",
    "queue",
    "train",
    "base",
    "tool",
    "button",
    "strategy",
];

fn landing_denied(
    s: &str,
    from: usize,
    at: usize,
    after: &[&str],
    st: &Stand<'_>,
    stands: &[Stand<'_>],
) -> bool {
    let before: Vec<(usize, &str)> = words_at(s, from, at).map(|(p, w)| (p, bare(w))).collect();
    let near: Vec<&str> = before[reach(&before, stands)..]
        .iter()
        .map(|&(_, w)| w)
        .collect();
    if near.last().is_some_and(|w| UNSETTLED.contains(w))
        || near.iter().chain(after).any(|w| BESIDE_LANDING.contains(w))
    {
        return false;
    }
    if matches!(st.word, "push" | "merge")
        && (!near.last().is_some_and(|w| negation(w))
            || after.first().is_some_and(|w| MERGE_THINGS.contains(w)))
    {
        return false;
    }
    let undone = matches!(st.reads(), Reads::Undone);
    near.iter().enumerate().any(|(i, w)| {
        let work = || {
            near[i + 1..]
                .iter()
                .any(|x| WORK_NOUNS.contains(x) || (undone && OWED.contains(x)))
                || WORK_NOUNS.contains(&st.word)
        };
        let any = matches!(
            *w,
            "nothing" | "none" | "never" | "nobody" | "noone" | "no-one" | "neither" | "nor"
        );
        (any && !undone) || ((any || matches!(*w, "no" | "zero" | "0")) && work())
    })
}

const OWED: &[&str] = &[
    "test",
    "tests",
    "docs",
    "doc",
    "documentation",
    "benchmark",
    "benchmarks",
    "changelog",
    "example",
    "examples",
    "readme",
    "migration",
    "port",
    "suite",
    "check",
    "checks",
    "review",
];

const WHOLE: &[&str] = &[
    "done",
    "merged",
    "landed",
    "pushed",
    "finished",
    "complete",
    "everything",
    "all",
    "nothing",
    "main",
    "origin/main",
    "master",
];

fn excepted(s: &str, st: &Stand<'_>) -> bool {
    let before = s[..st.at].trim_end();
    let prev = before.split_whitespace().last().map(bare);
    let near: Vec<&str> = before.split_whitespace().rev().take(4).map(bare).collect();
    if near.windows(2).any(|w| {
        matches!(w[0], "text" | "file" | "copy") && matches!(w[1], "main's" | "origin/main's")
    }) {
        return false;
    }
    if st.word == "not the" {
        let next = s[st.end()..].split_whitespace().next().map(bare);
        return before.ends_with(',')
            && landing_claimed(before)
            && !next.is_some_and(|n| {
                matches!(
                    n,
                    "branch" | "fork" | "worktree" | "feature" | "side" | "local" | "old" | "stale"
                )
            });
    }
    if matches!(
        st.word,
        "omission" | "omissions" | "holdout" | "holdouts" | "excluded" | "excepted"
    ) && prev.is_some_and(|p| matches!(p, "no" | "nothing" | "none" | "without" | "zero"))
    {
        // `with no omissions`, `nothing excluded`: none is made.
        return false;
    }
    if matches!(st.word, "exception" | "the exception") {
        let next = s[st.end()..].split_whitespace().next().map(bare);
        return !prev.is_some_and(|p| matches!(p, "without" | "no" | "any"))
            && next.is_none_or(|n| {
                matches!(
                    n,
                    "" | "of"
                        | "to"
                        | "is"
                        | "was"
                        | "are"
                        | "were"
                        | "being"
                        | "here"
                        | "there"
                        | "remains"
                        | "stays"
                )
            });
    }
    if !matches!(
        st.word,
        "but" | "bar" | "save" | "less" | "beyond" | "without"
    ) {
        return true;
    }
    let next = s[st.end()..].split_whitespace().next().map(bare);
    if st.word == "without" {
        // Only a thing left out after a comma: `all on main, without the
        // docs`, never `ran on main without a crash`.
        return before.ends_with(',') && next.is_some_and(|n| STRONG.contains(&n));
    }
    let whole = prev.is_some_and(|p| WHOLE.contains(&p) && !before.ends_with(','));
    let after_comma = st.word != "but" && before.ends_with(',');
    (whole || after_comma)
        && next.is_some_and(|n| {
            DETERMINERS.contains(&n) || (!n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        })
}

const NIL_GLUE: &[&str] = &[
    "else",
    "more",
    "really",
    "truly",
    "too",
    "very",
    "particularly",
    "terribly",
    "super",
    "of",
    "on",
    "the",
    "remaining",
    "items",
    "item",
    "issues",
    "issue",
    "left",
    "is",
    "are",
    "was",
    "were",
    "still",
    "open",
    "pending",
    "outstanding",
    "work",
    "tasks",
    "task",
    "things",
    "thing",
    "it",
    "them",
    "that",
    "which",
    "seems",
    "seem",
    "looks",
    "remains",
    "remain",
    "at",
    "all",
];

fn qualified_nil(s: &str, st: &Stand<'_>) -> bool {
    let words: Vec<&str> = s[clause_start(s, st.at)..st.at]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let next = s[st.end()..].split_whitespace().next().map(bare);
    if st.word == "major" && next.is_some_and(|n| matches!(n, "version" | "release" | "bump")) {
        return false;
    }
    words.first().is_some_and(|w| {
        matches!(*w, "nothing" | "none" | "no" | "zero" | "nil")
            || (*w == "not" && words.get(1) == Some(&"much"))
    }) && words.len() <= 7
        && words[1..].iter().all(|w| NIL_GLUE.contains(w))
}

fn only_item(s: &str, st: &Stand<'_>) -> bool {
    let from = clause_start(s, st.at);
    if !s[from..st.at].trim().is_empty() {
        return false;
    }
    let Some(lead) = s[..from].trim_end().strip_suffix(',') else {
        return false;
    };
    let words: Vec<&str> = lead[clause_start(lead, lead.len())..]
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    !words.is_empty()
        && words.len() <= 5
        && !words.iter().any(|w| {
            verbish(w)
                || SHIPS.contains(w)
                || LANDED.contains(w)
                || MAINS.contains(w)
                || negation(w)
                || matches!(*w, "all" | "everything" | "both")
        })
        && !(0..words.len()).any(|i| onto_main(&words[i..]))
}

const LEFTOVER: &[&str] = &[
    "remains",
    "remain",
    "left",
    "pending",
    "outstanding",
    "open",
    "do",
    "land",
];

fn nothing_but(s: &str, st: &Stand<'_>) -> bool {
    if st.word != "but" {
        return false;
    }
    let lead = s[clause_start(s, st.at)..st.at].trim_end();
    let words: Vec<&str> = lead.split_whitespace().map(bare).collect();
    let next = s[st.end()..]
        .split_whitespace()
        .map(bare)
        .find(|w| !w.is_empty());
    !lead.ends_with(',')
        && matches!(words.first(), Some(&("nothing" | "none")))
        && words.iter().any(|w| LEFTOVER.contains(w))
        && next.is_some_and(|n| !negation(n))
}

const FAULTS: &[&str] = &[
    "recur", "happen", "occur", "deadlock", "race", "crash", "hang", "leak", "break", "fail",
    "regress", "overflow", "panic",
];

const PROOF_ROWS: &[&str] = &[
    "fail",
    "fails",
    "failed",
    "failing",
    "still fail",
    "still fails",
    "still failing",
    "reverted",
];

const FIX_NOUNS: &[&str] = &[
    "fix", "fixes", "change", "patch", "commit", "guard", "rule", "check",
];

const OLD_CODE: &[&str] = &[
    "code",
    "rule",
    "rules",
    "reader",
    "filter",
    "logic",
    "implementation",
    "parser",
    "guard",
];

fn proves_by_failing(s: &str, (from, to): (usize, usize), (at, end): (usize, usize)) -> bool {
    let sentence: Vec<&str> = s
        .split_whitespace()
        .map(bare)
        .filter(|w| !w.is_empty())
        .collect();
    let pair = |a: &str, b: &[&str]| sentence.windows(2).any(|w| w[0] == a && b.contains(&w[1]));
    let flake = sentence.iter().any(|w| {
        matches!(
            *w,
            "flaky"
                | "flake"
                | "flakes"
                | "flaked"
                | "flaking"
                | "intermittent"
                | "intermittently"
                | "sporadically"
                | "rerun"
                | "re-run"
                | "retry"
                | "retried"
                | "isolation"
        )
    }) || pair("under", &["load"])
        || pair("run", &["alone"])
        || pair("ran", &["alone"])
        || ["fail", "fails", "failed", "passed", "passes"]
            .iter()
            .any(|f| pair(f, &["once", "alone"]));
    if flake {
        return false;
    }
    let raw: Vec<(usize, &str)> = words_at(s, from, to).collect();
    let w: Vec<&str> = raw.iter().map(|&(_, r)| bare(r)).collect();
    let fail = |x: &str| matches!(x, "fail" | "fails" | "failed" | "failing");
    let (Some(first), Some(last)) = (
        raw.iter().position(|&(p, r)| p + r.len() > at),
        raw.iter().rposition(|&(p, _)| p < end),
    ) else {
        return false;
    };
    let f = if fail(w[last]) || w[last] == "reverted" {
        last
    } else if w.get(last + 1).is_some_and(|n| fail(n)) {
        last + 1
    } else {
        return false;
    };
    let joined = |a: usize, b: usize| {
        w[a.min(b) + 1..a.max(b)]
            .iter()
            .any(|x| CONTRASTS.contains(x) || matches!(*x, "and" | "or" | "so" | "then"))
    };
    let undone = |x: &str| matches!(x, "reverted" | "removed" | "undone");
    let taken_out = (0..w.len()).any(|i| {
        matches!(w[i], "when" | "if" | "once")
            && (i + 1..w.len().min(i + 7)).any(|r| {
                undone(w[r])
                    && w[i + 1..r].iter().any(|x| FIX_NOUNS.contains(x))
                    && (0..i).any(|g| fail(w[g]) && (g == f || r == f) && !joined(g, r))
            })
    }) || matches!(
        w.get(f + 1..),
        Some(["without", "the" | "its" | "that" | "this", n, ..]) if FIX_NOUNS.contains(n)
    ) || matches!(
        w.get(f + 1..),
        Some(["with", "the" | "its" | "that" | "this", n, u, ..])
            if FIX_NOUNS.contains(n) && undone(u)
    );
    if w[f] == "reverted" {
        return taken_out;
    }
    let check = |i: usize| {
        w.get(i + 1).is_some_and(|n| {
            matches!(
                *n,
                "test" | "tests" | "job" | "jobs" | "suite" | "check" | "checks" | "lane"
            )
        }) && w.iter().skip(i + 2).take(2).any(|x| fail(x))
    };
    let agent = (0..first).any(|i| {
        let noun = matches!(w[i], "mutation" | "mutations" | "mutant" | "mutants")
            || (matches!(w[i], "control" | "controls") && i > 0 && w[i - 1] == "negative");
        noun && !check(i) && !joined(i, first)
    });
    let seen = (0..w.len().saturating_sub(2)).any(|i| {
        matches!(w[i], "seen" | "shown" | "watched")
            && w[i + 1] == "to"
            && w[i + 2] == "fail"
            && (i + 2 == f || (f < i && !joined(f, i)))
    });
    let after_fix = sentence.windows(4).any(|x| {
        matches!(x[0], "pass" | "passes")
            && matches!(x[1], "after" | "with")
            && matches!(x[2], "the" | "its" | "this")
            && x[3] == "fix"
    });
    let on_old = match w.get(f + 1..) {
        Some(["on" | "against", "the", old, code, ..]) => {
            matches!(
                *old,
                "old" | "unfixed" | "original" | "previous" | "pre-fix"
            ) && OLD_CODE.contains(code)
        }
        Some(["on" | "against", "main's" | "origin/main's", "code", ..]) => after_fix,
        _ => false,
    };
    agent || seen || on_old || taken_out
}

const TASK_ITEMS: &[&str] = &["item", "items", "finding", "findings"];

fn cannot_happen(after: &str) -> bool {
    let mut words = after.split_whitespace().map(bare);
    words.next().is_some_and(|w| FAULTS.contains(&w))
}

const IDLE: &[&str] = &["else", "more", "other", "at", "all", "of", "it", "them", ""];

fn excepting(s: &str, stands: &[Stand<'_>]) -> bool {
    if stands
        .iter()
        .any(|st| !matches!(st.reads(), Reads::Exception) && !st.reads().common())
    {
        return true;
    }
    !s.split(CLAUSE_MARKS).any(|clause| {
        let words: Vec<&str> = clause.split_whitespace().map(bare).collect();
        words.iter().enumerate().any(|(i, w)| {
            negation(w)
                && *w != "without"
                && !words[..i]
                    .iter()
                    .any(|x| matches!(*x, "that" | "which" | "who"))
                && words[i + 1..].iter().any(|x| !IDLE.contains(x))
        })
    })
}

const PREPOSITIONS: &[&str] = &[
    "in", "on", "for", "to", "with", "at", "from", "until", "till", "after", "before", "behind",
    "over", "under",
];

fn alone(after: &str) -> bool {
    after
        .split_whitespace()
        .map(bare)
        .find(|w| !w.is_empty())
        .is_none_or(|w| PREPOSITIONS.contains(&w))
}

const MECHANISMS: &[&str] = &[
    "waits", "wait", "waiting", "blocks", "block", "blocking", "retries", "retry", "spins", "spin",
    "loops", "loop", "sleeps", "sleep", "polls", "poll", "holds", "hold", "buffers", "queues",
    "reads", "scrolls", "runs", "repeats", "backs",
];

const AUTOMATIC: &[&str] = &[
    "on its own",
    "on their own",
    "by itself",
    "by themselves",
    "automatically",
    "unattended",
];

const DENIALS: &[&str] = &["no", "not", "nothing", "never", "none", "nobody"];

fn quoted(text: &str, at: usize) -> bool {
    let before = &text[..at];
    before.matches('"').count() % 2 == 1 || before.rfind('“') > before.rfind('”')
}

fn nothing_to_do(after: &str) -> bool {
    let after = after.trim_start();
    let explained = after.starts_with([':', ';', '(', '.']);
    let reason = after.trim_start_matches(|c: char| {
        matches!(c, ',' | ':' | ';' | '(' | '.') || c.is_whitespace()
    });
    let because = ["because ", "since ", "as "]
        .iter()
        .find_map(|w| reason.strip_prefix(w));
    let clause = because
        .unwrap_or(reason)
        .split([',', ';', '.'])
        .next()
        .unwrap_or("");
    if reason_words(clause).any(|w| ONLY_SLOW.contains(&w) || LACKING.contains(&w)) {
        return false;
    }
    if because.is_some() {
        return reason_words(clause).any(|w| OPEN_NEGATIONS.contains(&w))
            || clause
                .split(|c: char| !word_char(c))
                .any(|w| w == "already");
    }
    explained
        && reason_words(reason)
            .next()
            .is_some_and(|w| OPEN_NEGATIONS.contains(&w))
}

fn reason_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ';' | ':' | '(' | ')'))
        .filter(|w| !w.is_empty())
}

const ONLY_SLOW: &[&str] = &["time", "slow", "long", "minutes", "hours"];

const LACKING: &[&str] = &[
    "available",
    "allowed",
    "permitted",
    "installed",
    "here",
    "host",
    "machine",
    "access",
    "window",
    "windows",
    "mount",
    "gpu",
    "credential",
    "credentials",
    "memory",
    "ram",
    "disk",
];

/// AUTOPILOT LAND's instruction, typed at the first point of a Land episode where the continuation or the done check would go.
pub const RULE_LAND: &str = "land@v1";

/// WHAT A NOTE RESTATING A LANDING AFTER THE TASK'S OWN WAKE SAYS ([`TurnEndState::take_wake_restated`]).
pub const WAKE_KEPT: &str = "the landing holds through the task's own wake";
/// A nudge after the land instruction ([`LAND_NUDGE`]).
pub const RULE_LAND_NUDGE: &str = "land-nudge@v1";
/// The answer a question gets under Land ([`LAND_ANSWER`]).
pub const RULE_LAND_ANSWER: &str = "land-answer@v1";

pub use super::land_check::{LAND_CHECK, LAND_CHECK_HEAD, RULE_LAND_CHECK};

/// What Land's instruction and nudge say of files no git repository holds ([`NO_REPOSITORY`]).
macro_rules! no_repository {
    () => {
        "Files no git repository holds land where they are once verified."
    };
}
pub(crate) use no_repository;

/// FILES NO REPOSITORY HOLDS LAND WHERE THEY ARE: [`LAND`]'s and [`LAND_NUDGE`]'s sentence for them. It names the files, not the folder, so a worker whose tab is no repository still pushes the work it did in one; the probe still reads the session's repository at a defense ([`repo_work_suffix`]). It is short so the standing rules keep room on Land's first act with both its clauses ([`VERBATIM_UNITS`]).
pub const NO_REPOSITORY: &str = no_repository!();

/// LAND'S CLOSE, one source for every Land act that asks for the landing ([`LAND`],
/// [`LAND_NUDGE`], [`land_contradicted`]): its [`NO_REPOSITORY`] sentence, what a person can give
/// on a `NEEDS YOU:` line, and the defense owed once it is all on remote main.
macro_rules! land_close {
    () => {
        concat!(
            no_repository!(),
            " Record unavailable work and the decision to defer it, then land what can proceed. \
             Once it is all on remote main, defend that in a few lines starting Nothing left:"
        )
    };
}

/// [`land_close!`] as a constant.
const LAND_CLOSE: &str = land_close!();

/// What Land types at an episode's first point ([`RULE_LAND`]). Its [`NO_REPOSITORY`] sentence keeps a worker whose files no repository holds from asking a person for a remote to push to: on 2026-10-24 a real model's work in a plain folder answered the words without it with a NEEDS YOU for one in 3 runs of 3 (one a live `spawn agent=codex` worker), and with it with a clean `Nothing left:` in 3 of 3; Land nudges with no bound, so each nudge would have asked again.
pub const LAND: &str = concat!(
    harness_mark!(),
    " Autopilot is Land: land ALL of this work into the remote main branch, no exceptions. \
     Finish every open item, verify it, commit, merge and push it, and remove your worktrees \
     and merged branches. Leave nothing unlanded, parked on a branch or for later; chew \
     through every wall with safe, reversible fixes. Start no new work. ",
    land_close!()
);

/// What a Land nudge types ([`RULE_LAND_NUDGE`]) at every free point after the instruction until the work is landed, but where it contradicts a defense ([`land_contradicted`]).
pub const LAND_NUDGE: &str = concat!(
    harness_mark!(),
    " Still landing: get everything onto the remote main branch, no exceptions, with nothing \
     unlanded, parked on a branch or for later. ",
    land_close!()
);

/// LAND'S CONTRADICTION'S FIXED HEAD (v6 §14.4's slice 4c): what Land's nudge opens with where the
/// reply it answers defends the work with `Nothing left:` while git still finds work no remote
/// main has, or while the agent's own list of what is left still stands ([`land_contradicted`]).
/// Every reader of a typed Land act knows it by these words (limit 4).
pub const LAND_CONTRADICTED_HEAD: &str = concat!(
    harness_mark!(),
    " Still landing: your last reply says Nothing left, but"
);

/// The contradiction's measured fact, before git's finding in parentheses ([`repo_finding`]).
pub const CONTRADICTED_GIT: &str = "git shows work no remote main has";

/// What the contradiction asks of the work git found: to land it, and never to delete it. It
/// offers no other way out (no "ignore it", "move it out" or "say it is not open"), since each
/// would clear the probe without landing anything.
pub const CONTRADICTED_GIT_ASK: &str =
    "Commit, merge and push it to the remote main; never delete work.";

/// The contradiction's quote of the agent's own list ([`TurnEndState::land_listed`]), the words
/// Land's nudge quotes it with ([`land_check::LISTED_OPEN`]) after the head's `but`.
pub const CONTRADICTED_LISTED: &str = concat!("you ", land_check::said_left!());

/// What the contradiction asks of the agent's own list: to finish it and land it, and never to
/// delete work.
pub const CONTRADICTED_LISTED_ASK: &str =
    "Finish it and land it on the remote main; never delete work.";

/// The most UTF-16 units of the agent's list the contradiction quotes, its first item first.
pub const CONTRADICTED_QUOTE_UNITS: usize = 120;

/// What a question gets under Land ([`RULE_LAND_ANSWER`]).
pub const LAND_ANSWER: &str = "Nobody is here to choose: take the option that lands all of this \
                               work on the remote main soonest and start no new work. Pushing \
                               and merging are fine; never force-push or delete work.";

/// What opens Land's instruction where the person's switch over a held tab types it (v6 §14.17
/// item 3), before the held quote ([`switch_why`]).
pub const SWITCH_WHY_HEAD: &str = "You stopped at: \u{201c}";

/// What follows the held quote in the switch's why ([`switch_why`]).
pub const SWITCH_WHY_TAIL: &str = "\u{201d}. The person switched this tab to Land, so land what \
                                   you can, record unavailable work and the decision to defer it, \
                                   and keep moving.";

/// The most UTF-16 units the held quote takes of the switch's ask ([`switch_act`]), less where
/// the act's fixed words leave less of [`VERBATIM_UNITS`].
pub const SWITCH_QUOTE_UNITS: usize = 120;

/// THE SWITCH'S WHY, from one source (v6 §14.17 item 3): `You stopped at: “<quote>”. The person
/// switched this tab to Land, so land what you can and name in a NEEDS YOU: line only what a
/// person must give.`
#[must_use]
pub fn switch_why(quote: &str) -> String {
    format!("{SWITCH_WHY_HEAD}{quote}{SWITCH_WHY_TAIL}")
}

/// THE HELD QUOTE: what the agent stopped at, read from its held reply `tail` — the paragraph of
/// its `Nothing left:` defense where it gave one, else its last paragraph — on one line, its
/// curly quotes made straight so the why's own close its quote, no control character, and its
/// one closing full stop left to the why's (an ellipsis kept whole).
/// [`switch_act`] cuts it to its room.
#[must_use]
pub fn held_quote(tail: &str) -> String {
    let lines: Vec<&str> = tail.lines().collect();
    let blank = |l: &&str| l.trim().is_empty();
    let start = needs::defense_line(tail)
        .and_then(|d| lines.iter().position(|l| l.trim() == d))
        .or_else(|| {
            let end = lines.iter().rposition(|l| !blank(l))?;
            Some(lines[..end].iter().rposition(blank).map_or(0, |b| b + 1))
        })
        .unwrap_or(lines.len());
    let said: String = lines[start..]
        .iter()
        .take_while(|l| !blank(l))
        .flat_map(|l| l.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| match c {
            '\u{201c}' | '\u{201d}' => '"',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    // The why closes its quote with its own full stop (`“…All done”. The person …`).
    match said.strip_suffix('.') {
        Some(cut) if !cut.ends_with('.') => cut.to_string(),
        _ => said,
    }
}

/// LAND'S INSTRUCTION AS THE PERSON'S SWITCH OVER A HELD TAB TYPES IT (v6 §14.17 item 3): still
/// `land@v1`, after the harness's mark the switch's why ([`switch_why`]) quoting `quote` cut to
/// what [`VERBATIM_UNITS`] leaves past the instruction and `after` (at most
/// [`SWITCH_QUOTE_UNITS`]; `…` where none is left), then [`LAND`]'s own words, `after` and the
/// standing `rules` ([`act_words`]).
fn switch_act(quote: &str, after: &[&str], rules: Option<&str>) -> String {
    let fixed = units(LAND)
        + 1
        + units(SWITCH_WHY_HEAD)
        + units(SWITCH_WHY_TAIL)
        + after
            .iter()
            .filter(|a| !a.is_empty())
            .map(|a| 1 + units(a))
            .sum::<usize>();
    let room = VERBATIM_UNITS.saturating_sub(fixed).min(SWITCH_QUOTE_UNITS);
    let cut = fit_units(quote, room);
    let quote = if cut.is_empty() { "\u{2026}" } else { &cut };
    let mark = harness_mark!();
    let base = format!(
        "{mark} {}{}",
        switch_why(quote),
        LAND.strip_prefix(mark).unwrap_or(LAND)
    );
    act_words(&base, after, rules)
}

/// `text` WITHOUT THE SWITCH'S WHY where it opens with it ([`switch_act`]), the harness's mark
/// kept: Land's instruction as every reader keyed on its words reads it. Any other text is
/// itself.
#[must_use]
pub fn past_switch_why(text: &str) -> Cow<'_, str> {
    let mark = harness_mark!();
    let Some(rest) = text
        .strip_prefix(mark)
        .and_then(|r| r.strip_prefix(' '))
        .and_then(|r| r.strip_prefix(SWITCH_WHY_HEAD))
    else {
        return Cow::Borrowed(text);
    };
    match rest.find(SWITCH_WHY_TAIL) {
        Some(at) => Cow::Owned(format!("{mark}{}", &rest[at + SWITCH_WHY_TAIL.len()..])),
        None => Cow::Borrowed(text),
    }
}

/// Whether `text` opens with the switch's why ([`switch_act`]): Land's one ask at the person's
/// switch over a held tab.
#[must_use]
pub fn is_switch_ask(text: &str) -> bool {
    matches!(past_switch_why(text), Cow::Owned(_))
}

/// WHETHER THE REPLY `tail` IS THE TAB'S LOOP'S TO READ, not a relaunch's to carry on from: a clean
/// defense ([`clean_defense`]) or a `Could not finish:` ([`needs::could_not_finish`]).
#[must_use]
pub fn reply_for_the_loop(tail: Option<&str>) -> bool {
    clean_defense(tail) || tail.and_then(needs::could_not_finish).is_some()
}

/// WHETHER THE REPLY `tail` LANDS THE WORK, read at a land act's yield ([`TurnEndState::observe`]).
#[must_use]
pub fn clean_defense(tail: Option<&str>) -> bool {
    tail.is_some_and(|t| {
        needs::defense_line(t).is_some()
            && needs::needs_you(t).is_empty()
            && open_work_strict(Some(&needs::defense_body(t))).is_empty()
    })
}

fn land_open_words(tail: Option<&str>) -> Vec<&'static str> {
    once(land_open_found(tail))
}

/// What Land's reading of the reply `tail` finds open at a land act's yield, each with the
/// sentence it stood in ([`Found`]), in the order found: the words a hold names.
#[must_use]
pub fn land_open_found(tail: Option<&str>) -> Vec<Found> {
    match tail {
        Some(t) if needs::defense_line(t).is_some() => {
            open_work_found(&needs::defense_body(t), Some(&LAND_FAMILIES), true)
        }
        Some(t) if says_done(Some(t)) => {
            open_work_found(&needs::without_needs(t), Some(&LAND_FAMILIES), false)
        }
        _ => Vec::new(),
    }
}

/// WHY LAND HOLDS AT A LAND ACT'S YIELD: at the land check's (`answer`), the legs its answer
/// leaves (its needs, the agent's own list as the Open leg, the repository), and at any other
/// the reply's (its needs, the repository, no defense, a cut) beside the list the agent gave the
/// check, or its newest rewording since, while it `stands` (no real work and no new check since:
/// [`TurnEndState::land_listed`]), which is still its answer and so still the Open leg, a defense
/// owed by it no longer. The word
/// reader never sets the Open leg: it only describes the agent's list there
/// ([`LandHeld::open`]). Beside it, the hold's own words (§14.17 item 1, slice 4b): the agent's
/// list, quoted, and the files the repository's probe named ([`HoldSaid::of`]). `left` is how many
/// of the agent's left rows hold it ([`HoldLeg::Left`], [`TurnEndState::left_at_yield`]).
fn hold_at_yield(
    reading: &TurnEndReading,
    answer: Option<&land_check::CheckAnswer>,
    stands: Option<&str>,
    left: u16,
) -> (LandHeld, HoldSaid) {
    let tail = reading.said_tail.as_deref();
    let said_need = reading.names_need();
    let listed = match answer {
        Some(land_check::CheckAnswer::Left(list)) => Some(list.as_str()),
        Some(land_check::CheckAnswer::Needs { rest_landed: false }) => tail,
        Some(land_check::CheckAnswer::Landed | land_check::CheckAnswer::Needs { .. }) => None,
        None => stands,
    }
    .map(needs::without_needs)
    .filter(|l| !l.trim().is_empty());
    let words = listed
        .as_deref()
        .map_or_else(Vec::new, |l| open_work_strict(Some(l)));
    let mut open = [None; 4];
    for (slot, word) in open
        .iter_mut()
        .zip(words.iter().filter_map(|w| OpenWord::of(w)))
    {
        *slot = Some(word);
    }
    let repo = reading.repo.found();
    let legs = [
        (HoldLeg::Needs, said_need),
        (HoldLeg::Repo, repo.is_some()),
        (HoldLeg::Unread, reading.repo.is_unread()),
        (HoldLeg::Left, left > 0),
        (HoldLeg::Open, listed.is_some()),
        (
            HoldLeg::Defense,
            answer.is_none() && listed.is_none() && tail.and_then(needs::defense_line).is_none(),
        ),
        (HoldLeg::Cut, reading.said_cut),
    ]
    .into_iter()
    .filter(|&(_, held)| held)
    .fold(0, |bits, (leg, _)| bits | leg.bit());
    let count = |n: fn(&crate::harness::repo_work::RepoWork) -> usize| {
        repo.map_or(0, |r| u16::try_from(n(r)).unwrap_or(u16::MAX))
    };
    let said = HoldSaid::of(
        listed.as_deref(),
        repo.map_or(&[][..], |r| r.paths.as_slice()),
    );
    let held = LandHeld {
        since_ms: None,
        nudges: 0,
        read: true,
        legs,
        open,
        changed: count(|r| r.changed),
        untracked: count(|r| r.untracked),
        stashes: count(|r| r.stashes),
        unpushed: count(|r| r.unpushed),
        unlanded: count(|r| r.unlanded),
        sub_unpushed: count(|r| r.sub_unpushed),
        parked: count(|r| r.parked),
        left,
    };
    (held, said)
}

fn open_words() -> impl Iterator<Item = &'static str> {
    OPEN_HEADS
        .iter()
        .chain(LAND_HEADS)
        .chain(OPEN_STATUS)
        .copied()
        .chain(
            LAND_FAMILIES
                .iter()
                .flat_map(|f| f.says.into_iter().chain(f.rows.iter().copied())),
        )
        .chain([
            UNCHECKED,
            QUESTION,
            CHANGE_NUMBER,
            PERCENT,
            COUNT_BESIDE_NONE,
        ])
}

/// The index of `word` among [`open_words`], counted from 1 (its first place).
#[must_use]
pub fn open_word_index(word: &str) -> Option<std::num::NonZeroU16> {
    let at = open_words().position(|w| w == word)?;
    u16::try_from(at + 1)
        .ok()
        .and_then(std::num::NonZeroU16::new)
}

/// The word at `index` ([`open_word_index`]).
#[must_use]
pub fn open_word_at(index: std::num::NonZeroU16) -> Option<&'static str> {
    open_words().nth(usize::from(index.get()) - 1)
}

fn wake_left_open(reading: &TurnEndReading) -> bool {
    reading.said_cut || wake_words_open(reading)
}

fn wake_words_open(reading: &TurnEndReading) -> bool {
    reading.names_need()
        || [reading.turn_said.as_deref(), reading.said_tail.as_deref()]
            .into_iter()
            .flatten()
            .any(|t| !open_work_strict(Some(&needs::without_needs(t))).is_empty())
}

/// RETIRED: [`LAND`]'s words of `4bff4848b4`, as [`DONE_CHECK_COMMITTED_BEFORE`].
pub const LAND_COMMITTED_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed and pushed \
     (merged to main where this repository lands work), with nothing uncommitted, untracked or \
     stashed. Write down what is left in the repository and commit that too, so your summary \
     names nothing left. Then write a short summary of whether the work is complete and what \
     is done."
);

/// RETIRED: [`LAND`]'s words from `69a8f68178` until `4bff4848b4`.
pub const LAND_ORDINARY_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of whether the work is \
     complete, what is done and what is left."
);

/// RETIRED: [`LAND`]'s words of `0b1abdabf3` until `69a8f68178`, as [`DONE_CHECK_WORDS_BEFORE`].
pub const LAND_WORDS_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of what is done and what \
     is left, and close by saying the work is complete."
);

/// RETIRED: [`LAND`]'s words while it asked for the [`FINISHED`] token.
pub const LAND_MARKER_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is set to Land: wrap up this work cleanly. Finish only the step in hand and \
     start nothing new. Make sure what is done is verified, and leave it committed (and pushed, \
     if that is how this repository works). Then write a short summary of what is done and what \
     is left, and reply with ATERM-TASK-DONE on a line by itself."
);

/// RETIRED: [`LAND`]'s words of `a355109905` (FOREVER LAND), before its [`NO_REPOSITORY`] sentence.
pub const LAND_REMOTE_MAIN_BEFORE: &str = concat!(
    harness_mark!(),
    " Autopilot is Land: land ALL of this work into the remote main branch, no exceptions. \
     Finish every open item, verify it, commit, merge and push it, and remove your worktrees \
     and merged branches. Leave nothing unlanded, parked on a branch or for later; chew \
     through every wall with safe, reversible fixes. Start no new work. Put what only a person \
     can give on a line starting NEEDS YOU: and land the rest. Once it is all on remote main, \
     defend that in a few lines starting Nothing left:"
);

/// EVERY LAND INSTRUCTION AN OLDER BUILD TYPED, newest first.
pub const RETIRED_LANDS: [&str; 5] = [
    LAND_REMOTE_MAIN_BEFORE,
    LAND_COMMITTED_BEFORE,
    LAND_ORDINARY_BEFORE,
    LAND_WORDS_BEFORE,
    LAND_MARKER_BEFORE,
];

/// RETIRED: [`LAND_NUDGE`]'s words of `4bff4848b4`, as [`LAND_COMMITTED_BEFORE`].
pub const LAND_NUDGE_COMMITTED_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, write down what is left in the \
     repository and commit it, then summarize whether the work is complete and what is done."
);

/// RETIRED: [`LAND_NUDGE`]'s words from `69a8f68178` until `4bff4848b4`, as [`LAND_ORDINARY_BEFORE`].
pub const LAND_NUDGE_ORDINARY_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then summarize whether the work \
     is complete, what is done and what is left."
);

/// RETIRED: [`LAND_NUDGE`]'s words of `0b1abdabf3` until `69a8f68178`, as [`LAND_WORDS_BEFORE`].
pub const LAND_NUDGE_WORDS_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then close by saying the work is \
     complete."
);

/// RETIRED: [`LAND_NUDGE`]'s words while it asked for the [`FINISHED`] token.
pub const LAND_NUDGE_MARKER_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: finish only what leaves the work clean, then reply with ATERM-TASK-DONE on \
     a line by itself."
);

/// RETIRED: [`LAND_NUDGE`]'s words of `a355109905`, as [`LAND_REMOTE_MAIN_BEFORE`].
pub const LAND_NUDGE_REMOTE_MAIN_BEFORE: &str = concat!(
    harness_mark!(),
    " Still landing: get everything onto the remote main branch, no exceptions, with nothing \
     unlanded, parked on a branch or for later. Put what only a person can give on a line \
     starting NEEDS YOU: and land the rest. Once it is all on remote main, defend that in a \
     few lines starting Nothing left:"
);

/// EVERY LAND NUDGE AN OLDER BUILD TYPED, newest first.
pub const RETIRED_LAND_NUDGES: [&str; 5] = [
    LAND_NUDGE_REMOTE_MAIN_BEFORE,
    LAND_NUDGE_COMMITTED_BEFORE,
    LAND_NUDGE_ORDINARY_BEFORE,
    LAND_NUDGE_WORDS_BEFORE,
    LAND_NUDGE_MARKER_BEFORE,
];

/// RETIRED: the [`LAND_ANSWER`] every build before Autopilot Land typed.
pub const LAND_ANSWER_BEFORE: &str = "Nobody is here to choose: take the option that finishes \
                                      this work cleanly soonest, start nothing new, and change \
                                      nothing irreversible.";

/// EVERY LAND ANSWER AN OLDER BUILD TYPED.
pub const RETIRED_LAND_ANSWERS: [&str; 1] = [LAND_ANSWER_BEFORE];

/// [`LAND`], with the standing rules when there are any.
#[must_use]
pub fn land_text(rules: Option<&str>) -> String {
    with_rules(LAND, rules)
}

/// [`LAND_NUDGE`], with the standing rules when there are any.
#[must_use]
pub fn land_nudge_text(rules: Option<&str>) -> String {
    with_rules(LAND_NUDGE, rules)
}

/// [`LAND_ANSWER`], with the standing rules when there are any.
#[must_use]
pub fn land_answer_text(rules: Option<&str>) -> String {
    with_rules(LAND_ANSWER, rules)
}

/// Whether `rule` is one of Land's four acts (its instruction, nudge, answer and land check).
#[must_use]
pub fn is_land_rule(rule: &str) -> bool {
    matches!(
        rule,
        RULE_LAND | RULE_LAND_NUDGE | RULE_LAND_ANSWER | RULE_LAND_CHECK
    )
}

/// RETIRED AS AN ACT: the worker's own suggestion accepted, as builds before v6's 5a did under Cruise. Kept so older books, the stats and the tape corpus read it; Cruise types [`RULE_CONTINUE`] over a suggestion now.
pub const RULE_SUGGESTION: &str = "continue-suggestion@v1";
/// A continuation after a refused request's wait. Before v6's 12b it was an outage wall's typed
/// retry too, so older books, the stats and the tape corpus read it there.
pub const RULE_API_RETRY: &str = "api-retry@v1";
/// RETIRED AS AN ACT: a continuation once the host measured the API reachable again, after an API
/// error that never reached it. Kept so older books, the stats and the tape corpus read it; an
/// outage wall's measured clearing types [`RULE_OUTAGE_RESUME`] now.
pub const RULE_API_BACK: &str = "api-back@v1";
/// A continuation after a reply the connection cut off.
pub const RULE_API_CUTOFF: &str = "api-cutoff@v1";
/// The one line that resumes an outage wall's wait (v6 §14.17.5.2): measured once the route is
/// back, or tried on the outage's doubling back-off where nothing measures its end.
pub const RULE_OUTAGE_RESUME: &str = "outage-resume@v1";
/// A LOST LOGIN'S TURN DELIVERED AGAIN once the login came back in another tab of the host ([`super::login::LoginShared::Back`]).
pub const RULE_LOGIN_BACK: &str = "login-back@v1";

/// How often a tab waiting on another tab's sign-in of its login reads that sign-in again ([`super::login::LoginShared::Carried`]).
pub const LOGIN_SHARED_LOOK: Duration = Duration::from_secs(15);

/// [`RULE_LOGIN_BACK`]'s words: the expired login's line ([`cleared_text`]).
#[must_use]
pub fn login_back_text() -> String {
    cleared_text(Cleared::Login, Clearing::Measured).to_string()
}

/// THE CONDITION A SYSTEM MESSAGE ENDED THE AGENT'S TURN ON, as the line that resumes it past the
/// condition names it ([`cleared_text`]; v6 §14.17.5, slice 12a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleared {
    /// A usage limit: a session's, a week's, a model's or the credits'.
    UsageLimit,
    /// A lost login, signed in again.
    Login,
    /// A model's limit Claude Code goes past on usage credits, on the prompt this line is.
    Credits,
    /// An outage wall waited on ([`outage_of`]; v6 §14.17.5.2, slice 12b).
    Outage(Outage),
}

/// HOW A CONDITION'S CLEARING IS KNOWN ([`cleared_text`]): measured (the reset it named passed, the
/// accounts probe or the login says it cleared, or the session moved off it), or tried blind on a
/// back-off, where it named no reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clearing {
    /// The clearing was seen: *has cleared*.
    Measured,
    /// The resume is a try on a back-off: *may have cleared*.
    Blind,
}

/// THE ONE LINE A RESUME TYPES PAST A SYSTEM MESSAGE (v6 §14.17.5, slice 12a): a limit or a lost
/// login ended the agent's turn, not the agent, so the resume says what cleared
/// and nothing of Cruise's or Land's prompt (owner rule 2026-10-25: a prompt into a turn the agent
/// did not end is spam). Cruise and Land speak at the agent's own next turn end; a prompt the
/// condition cut is the agent's still, and its answer is judged as that prompt's
/// ([`TurnEndState::acted`]'s cut carry).
#[must_use]
pub const fn cleared_text(cleared: Cleared, clearing: Clearing) -> &'static str {
    macro_rules! line {
        ($what:literal) => {
            match clearing {
                Clearing::Measured => concat!(
                    harness_mark!(),
                    " The ",
                    $what,
                    " has cleared; continue where you stopped."
                ),
                Clearing::Blind => concat!(
                    harness_mark!(),
                    " The ",
                    $what,
                    " may have cleared; continue where you stopped."
                ),
            }
        };
    }
    match cleared {
        Cleared::UsageLimit => line!("usage limit"),
        Cleared::Login => line!("expired login"),
        Cleared::Outage(Outage::Overload) => line!("API overload"),
        Cleared::Outage(Outage::ApiError) => line!("API error"),
        Cleared::Outage(Outage::Connection) => line!("connection error"),
        Cleared::Credits => concat!(
            harness_mark!(),
            " The model's usage limit is passed on usage credits; continue where you stopped."
        ),
    }
}

/// Whether `text` opens on one of [`cleared_text`]'s lines.
#[must_use]
pub fn is_cleared_line(text: &str) -> bool {
    const CLEARED: [Cleared; 6] = [
        Cleared::UsageLimit,
        Cleared::Login,
        Cleared::Credits,
        Cleared::Outage(Outage::Overload),
        Cleared::Outage(Outage::ApiError),
        Cleared::Outage(Outage::Connection),
    ];
    let text = text.trim_start();
    CLEARED.iter().any(|&c| {
        [Clearing::Measured, Clearing::Blind]
            .iter()
            .any(|&how| text.starts_with(cleared_text(c, how)))
    })
}

/// The condition a resume of `rule` types past ([`cleared_text`]): the consent a model's limit
/// asks for, the outage an outage wall's resume waited on (the wall `reading` shows, else the one
/// `state`'s track stands for), or a limit's resume past the limit, or past the lost login where
/// that is the wall `state`'s track stands for. `/compact`'s owed continuation has none: the
/// compaction took the agent's context, so the mode's own continuation tells it again what it is
/// doing.
fn cleared_of(state: &TurnEndState, reading: &TurnEndReading, rule: &str) -> Option<Cleared> {
    Some(match rule {
        RULE_COMPACT => return None,
        RULE_CONSENT => Cleared::Credits,
        RULE_OUTAGE_RESUME => {
            let track = state.wall.as_ref().filter(|t| t.class == WallClass::Retry);
            Cleared::Outage(
                reading
                    .wall
                    .or_else(|| state.wall.as_ref().and_then(|t| t.kind))
                    .and_then(|kind| {
                        outage_of(kind, measured_down(reading, track), reading.program)
                    })
                    .unwrap_or(Outage::ApiError),
            )
        }
        _ if state
            .wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth) =>
        {
            Cleared::Login
        }
        _ => Cleared::UsageLimit,
    })
}

const CARRY_ON: &str = concat!(
    "Carry on from where you stopped; if the result of your last step is missing, check \
     whether it ran before you repeat it. ",
    super::needs::needs_hint!()
);

/// HOW `program`'S AGENT SIGNS IN AGAIN, in the words its lost login's escalation names (the wall's
/// `message` read for Muse Code's own remedy): `/login` where the program has one, else what the
/// person does in its place. Public for the system-message lab's expired-login judge
/// (`aterm-lab`'s `forever::sysmsg::login`), which reads the escalation for these words.
#[must_use]
pub fn sign_in_how(program: Program, message: &str) -> String {
    match program {
        Program::Claude | Program::Grok | Program::Kimi | Program::Generic => {
            "sign in with /login".to_string()
        }
        Program::Codex => "sign in to Codex again with codex login".to_string(),
        Program::Agy => {
            "quit agy and run it again to sign in, or give it a valid Gemini API key".to_string()
        }
        Program::Muse => aterm_phase::muse::remedy(message).map_or_else(
            || "sign in to Muse Code again with muse login in a shell".to_string(),
            |r| format!("as Muse Code says, {r}"),
        ),
    }
}

/// The cells of an escalation's reason the person's badge keeps (`supervise::run::escalate`): the
/// rest is cut behind an ellipsis.
pub const REASON_CELLS: usize = 120;

/// A lost login's escalation for `program`: the wall's words `msg` first, then `aside` and how
/// the program signs in again (`how`); how it signs in goes AHEAD of the wall's words wherever the
/// badge could cut it off behind them (agy's 401 quotes Google's message whole, Muse Code's
/// rejected key its own remedy again): past [`REASON_CELLS`], or past the bytes the badge keeps
/// for a reason beside the wall's own label (`<program> wall: <its words>`, all of it within
/// `META_ATTENTION_KEYED_MAX` bytes), counting the label at the most its words can take.
fn login_gone(program: Program, msg: &str, aside: Option<&str>, how: &str) -> String {
    let trailing = match aside {
        Some(aside) => format!("the login is gone: {msg} ({aside}; {how})"),
        None => format!("the login is gone: {msg} ({how})"),
    };
    let cells: usize = trailing.chars().map(aterm_grapheme::char_width).sum();
    let label = program.name().len() + " wall: ".len() + msg.len() + '…'.len_utf8();
    if cells <= REASON_CELLS
        && label + " ()".len() + trailing.len()
            <= aterm_types::control_verbs::META_ATTENTION_KEYED_MAX
    {
        return trailing;
    }
    match aside {
        Some(aside) => format!("the login is gone, {how} ({aside}): {msg}"),
        None => format!("the login is gone, {how}: {msg}"),
    }
}

/// How a session whose vault token Claude Code refused gets a working one
/// (`super::login::LoginPin::Token`; `docs/DESIGN-claude-setup-token-2026-11-05.md` §5.6).
const TOKEN_HOW: &str =
    "make its account a new token: aterm harness accounts token <account> --replace";

const fn login_command(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude => Some("/login"),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => None,
    }
}

const fn compact_command(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude | Program::Muse => Some("/compact"),
        Program::Codex | Program::Grok | Program::Kimi | Program::Agy | Program::Generic => None,
    }
}

/// Text typed at an API wall's act (`rule`, at a wall of `kind` drawn by `program`).
#[must_use]
pub fn wall_retry_text(rule: &str, kind: WallKind, program: Program) -> String {
    let who = program.product();
    let what = api_wall_words(kind);
    let cause = match kind {
        WallKind::ApiError { cause, .. } => cause,
        _ => ApiCause::Server,
    };
    match (rule, cause) {
        (RULE_API_BACK, _) => {
            format!("{who} reported {what}, and the API is reachable again now. {CARRY_ON}")
        }
        (_, ApiCause::CutOff) => {
            format!("{who} reported {what}, so your last reply may be incomplete. {CARRY_ON}")
        }
        (_, ApiCause::Unreachable | ApiCause::Config) => {
            format!("{who} reported {what}; trying again. {CARRY_ON}")
        }
        (_, ApiCause::Overlong) => format!(
            "{who} reported {what}, so your last reply was cut off. Write long files and \
             outputs across several smaller replies. {CARRY_ON}"
        ),
        (_, ApiCause::Server | ApiCause::Request) => format!("{who} reported {what}. {CARRY_ON}"),
    }
}

/// An API wall in aterm's words, from its kind alone.
#[must_use]
pub fn api_wall_words(kind: WallKind) -> String {
    let (code, why) = match kind {
        WallKind::Overloaded => (None, Some("overloaded")),
        WallKind::ApiError { code, cause, .. } => (
            code,
            match cause {
                ApiCause::Server => None,
                ApiCause::Unreachable => Some("the API could not be reached"),
                ApiCause::CutOff => Some("the reply was cut off"),
                ApiCause::Config => Some("a certificate or proxy refusal"),
                ApiCause::Request => Some("the request was refused"),
                ApiCause::Overlong => Some("the reply was over its output token maximum"),
            },
        ),
        other => return format!("{} wall", super::a_or_an(other.name())),
    };
    match (code, why) {
        (Some(code), Some(why)) => format!("an API error ({code}, {why})"),
        (Some(code), None) => format!("an API error ({code})"),
        (None, Some(why)) => format!("an API error ({why})"),
        (None, None) => "an API error".to_string(),
    }
}

/// What the session's host last measured of the agent's route to its API ([`crate::supervise::IdleHost::reach`]), never read off the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    #[default]
    Unknown,
    Down {
        since: Instant,
    },
    Up {
        since: Instant,
    },
}

/// The most acts of one wall rule this policy types in its window ([`TurnEndTiming::window`], an hour).
pub const WALL_ACTS_PER_WINDOW: usize = 12;

fn is_wall_act(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY
            | RULE_API_BACK
            | RULE_API_CUTOFF
            | RULE_OUTAGE_RESUME
            | RULE_LIMIT_RESUME
            | RULE_CONSENT
            | RULE_COMPACT
            | RULE_LOGIN
    )
}

/// WHETHER `rule` RESUMES A WAIT (v6 §14.17.5, slice 12a's G6): a wall's retry past its
/// condition ([`is_wall_retry`]) but the compaction, which answers a full context the agent's
/// own work filled, held through a dark wake between sleeps (the loop's dark-wake gate,
/// [`super::super::Session`]'s `turn_end_execute`).
#[must_use]
pub fn is_wait_resume(rule: &str) -> bool {
    is_wall_retry(rule) && rule != RULE_COMPACT
}

pub(crate) fn is_wall_retry(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY
            | RULE_API_BACK
            | RULE_API_CUTOFF
            | RULE_OUTAGE_RESUME
            | RULE_LIMIT_RESUME
            | RULE_COMPACT
            | RULE_LOGIN_BACK
    )
}
/// A continuation past a usage window's reset.
pub const RULE_LIMIT_RESUME: &str = "usage-resume@v1";
/// The agent relaunched on the fallback model on a model bucket.
pub const RULE_MODEL_FALLBACK: &str = "model-fallback@v1";
/// The agent relaunched on the bucket's model again at the bucket's reset.
pub const RULE_MODEL_RESTORE: &str = "model-restore@v1";
/// The agent relaunched under another of the owner's Claude Code accounts.
pub const RULE_ACCOUNT: &str = "account-rotate@v1";
/// The accounts probe.
pub const RULE_USAGE_PROBE: &str = "usage-probe@v1";

/// The gate that shut on a due accounts probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeGate {
    NotDue,
    NotIdle,
    NotAuthoritative,
    Survey,
    Wall,
    Composer,
    PendingInput,
    Interrupted,
    Question,
    NotTurnEnd,
    ActOwed,
    /// Land owes its land check at this point (`TurnEndState::land_check_owed`): the check goes first,
    /// and the probe waits for a later turn end.
    LandCheck,
    Person,
    Watching,
    NoLedger,
    ProbesOff,
    PanelOwed,
    NotLive,
    NotNew,
    Escalated,
    FoldLatch,
    HostWants,
    Stalled,
    WakeSettle,
    NoFence,
    NotClaude,
    NotFullscreen,
    Claim,
}

impl ProbeGate {
    /// Every gate, for a reader of the words.
    pub const ALL: [ProbeGate; 28] = [
        ProbeGate::NotDue,
        ProbeGate::NotIdle,
        ProbeGate::NotAuthoritative,
        ProbeGate::Survey,
        ProbeGate::Wall,
        ProbeGate::Composer,
        ProbeGate::PendingInput,
        ProbeGate::Interrupted,
        ProbeGate::Question,
        ProbeGate::NotTurnEnd,
        ProbeGate::ActOwed,
        ProbeGate::LandCheck,
        ProbeGate::Person,
        ProbeGate::Watching,
        ProbeGate::NoLedger,
        ProbeGate::ProbesOff,
        ProbeGate::PanelOwed,
        ProbeGate::NotLive,
        ProbeGate::NotNew,
        ProbeGate::Escalated,
        ProbeGate::FoldLatch,
        ProbeGate::HostWants,
        ProbeGate::Stalled,
        ProbeGate::WakeSettle,
        ProbeGate::NoFence,
        ProbeGate::NotClaude,
        ProbeGate::NotFullscreen,
        ProbeGate::Claim,
    ];

    /// The word the journal's `SKIPPED … gate=<word>` line carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ProbeGate::NotDue => "not-due",
            ProbeGate::NotIdle => "not-idle",
            ProbeGate::NotAuthoritative => "not-authoritative",
            ProbeGate::Survey => "survey",
            ProbeGate::Wall => "wall",
            ProbeGate::Composer => "composer",
            ProbeGate::PendingInput => "pending-input",
            ProbeGate::Interrupted => "interrupted",
            ProbeGate::Question => "question",
            ProbeGate::NotTurnEnd => "not-turn-end",
            ProbeGate::ActOwed => "act-owed",
            ProbeGate::LandCheck => "land-check",
            ProbeGate::Person => "person",
            ProbeGate::Watching => "watching",
            ProbeGate::NoLedger => "no-ledger",
            ProbeGate::ProbesOff => "probes-off",
            ProbeGate::PanelOwed => "panel-owed",
            ProbeGate::NotLive => "not-live",
            ProbeGate::NotNew => "not-new",
            ProbeGate::Escalated => "escalated",
            ProbeGate::FoldLatch => "fold-latch",
            ProbeGate::HostWants => "host-wants",
            ProbeGate::Stalled => "stalled",
            ProbeGate::WakeSettle => "wake-settle",
            ProbeGate::NoFence => "no-fence",
            ProbeGate::NotClaude => "not-claude",
            ProbeGate::NotFullscreen => "not-fullscreen",
            ProbeGate::Claim => "claim",
        }
    }
}
/// `/compact` on a full context, and the continuation after it.
pub const RULE_COMPACT: &str = "context-compact@v1";
/// `/login` on a lost login.
pub const RULE_LOGIN: &str = "auth-login@v1";
/// `cfg.answer_text`, typed where a worker asked a person.
pub const RULE_ANSWER: &str = "answer@v1";
/// A model bucket's consent to go on on usage credits, accepted.
pub const RULE_CONSENT: &str = "consent-accept@v1";
/// THE CREDITS HOLD ([`credits_hold`]).
pub const RULE_CREDITS_HOLD: &str = "credits-hold@v1";
/// The credits hold's escalation where no reset is named, told once an episode.
pub const CREDITS_HOLD_TOLD: &str = "usage credits carry the session and name no reset: \
     nothing is typed on them (credits-hold@v1) but one turn when the five-hour window must \
     have reset; `[harness] usage_credits = true` continues on credits";
/// The same where the credits carry a MODEL's requests, none named ([`CreditsEpisode::model`]).
pub const CREDITS_HOLD_MODEL_TOLD: &str = "usage credits carry the requests of the model \
     this session runs, with no reset named: nothing is typed on them (credits-hold@v1) until \
     it runs another (`[harness] model_fallback`, or a relaunch); `[harness] usage_credits = \
     true` continues on credits";
/// The agent restarted in its tab for Claude Code's critical-memory banner.
pub const RULE_MEMORY_RESTART: &str = "memory-restart@v1";
/// The agent restarted in its tab because it read the line the loop wrote to it and drew none of
/// it a minute on, at rest ([`Restart::InputLost`]): the line the loop holds in custody
/// (`turn_end_loop`'s `Custody`) is lost with its process, and the point resumed is decided and
/// written once.
pub const RULE_INPUT_LOST: &str = "input-lost-restart@v1";
/// A turn the session's host typed (upgrade notice, carry-on; [`TurnEndState::host_typed`]).
pub const RULE_HOST_TURN: &str = "harness-turn@v1";
/// The agent relaunched with `--resume` onto its account's vault token, or back onto the row's
/// stored login ([`Restart::Route`]; `docs/DESIGN-claude-setup-token-2026-11-05.md` §5.5).
pub const RULE_TOKEN_ROUTE: &str = "token-route@v1";

/// Which turn the session's host typed, told by the host from its own step, never read off the screen or the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostTurn {
    Notice,
    /// The one line a restart types where the resumed build warned something was lost: its reply
    /// is read as a [`Self::Notice`]'s, and a short one moves the back-off as a [`Self::CarryOn`]'s
    /// does ([`TurnEndState::back_off_from_restart_reply`]).
    Restore,
    CarryOn,
    Check,
    /// The occupancy door's line at a break ([`TurnEndState::fact_typed`]): its answer is never a done
    /// report, never a short turn, and moves no streak.
    Fact,
}

/// Why the session's host restarts the agent in its tab ([`TurnEndAction::Restart`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    Memory,
    /// The worker read the line the loop wrote to it and drew none of it a minute on, at rest
    /// ([`RULE_INPUT_LOST`]): a restart of `of`, the agent's process the line was written to as
    /// the host named it, and of no other process; `(None, None)` where it named none, a restart
    /// of the tab's agent then. `line`: the line it is made for, the same at every ask of that
    /// line's restart (`custody::LineAsk`), whose ask ([`RestartAsk::of`]) is the line's.
    InputLost {
        of: crate::supervise::custody::Process,
        line: crate::supervise::custody::LineAsk,
    },
    Model {
        to: String,
    },
    ModelBack {
        to: Option<String>,
    },
    Account {
        to: String,
        at: AccountAt,
    },
    /// The session's own account, onto another credential (§5.5 of
    /// `docs/DESIGN-claude-setup-token-2026-11-05.md`): onto the account's vault token, now in use
    /// on this Mac, from its stored login; or back onto the row's stored login from a token that
    /// was refused, removed, ended or mismatched. The host offers it ([`TurnEndReading::route`])
    /// and the claims ring decides first: a yield's account move is asked ahead of it.
    Route {
        to: RouteTo,
    },
    /// A person's click on the chip of a Claude Code tab (`docs/DESIGN-yolo-binary-2026-11-04.md`
    /// §3): the session restarted on its conversation `conversation`, its line rid of every
    /// permission flag and given the one `to` names. Only the click's lane asks it
    /// (`harness::permission_click`); the turn-end loop never does.
    Permission {
        to: PermissionTo,
        conversation: String,
    },
}

/// Which line a click on the chip asks for ([`Restart::Permission`], the design's §3 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionTo {
    /// A click on `yolo`: no permission flag on the line, so the session lands in the mode its
    /// own settings choose.
    Settings,
    /// A click on `lame`: `--dangerously-skip-permissions` on the line, so it lands in bypass.
    Bypass,
}

impl PermissionTo {
    /// The one word a ledger names it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Bypass => "bypass",
        }
    }
}

/// Which credential a route move puts the session on ([`Restart::Route`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTo {
    /// The account's vault token, through aterm's wrapper.
    Token,
    /// The row's stored login.
    Stored,
}

impl RouteTo {
    /// The one word a ledger and a cause name it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::Stored => "stored",
        }
    }

    /// The route [`Self::word`] names (`None`: a word this build does not know).
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        [Self::Token, Self::Stored]
            .into_iter()
            .find(|to| to.word() == word)
    }
}

/// Where an account move is asked ([`Restart::Account`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAt {
    Wall,
    Ahead,
}

/// WHAT AN ACCOUNT MOVE THE HOST COULD NOT MAKE AT A WALL SAYS OF ASKING IT AGAIN ([`TurnEndState::account_refusal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmade {
    Again,
    NoShell,
    Launch,
}

impl Unmade {
    /// The class of the host's `step` word for a restart it did not make.
    #[must_use]
    pub fn of(step: Option<&str>) -> Self {
        match step {
            Some("refused:not-a-shell-job") => Self::NoShell,
            Some(step) if step.starts_with("refused:") || step.starts_with("failed:") => {
                Self::Launch
            }
            _ => Self::Again,
        }
    }
}

impl Restart {
    /// A lost line's restart's [`Self::word`] ([`Restart::InputLost`]).
    pub const INPUT_LOST: &'static str = "input-lost";
    /// A model's way back's [`Self::word`] ([`Restart::ModelBack`]).
    pub const MODEL_BACK: &'static str = "model-back";
    /// An account move ahead's [`Self::word`] ([`Restart::Account`] at [`AccountAt::Ahead`]).
    pub const ACCOUNT_AHEAD: &'static str = "account-ahead";

    /// A model's move onto its fallback's [`Self::word`] ([`Restart::Model`]).
    pub const MODEL: &'static str = "model";
    /// An account move at a wall's [`Self::word`] ([`Restart::Account`] at [`AccountAt::Wall`]).
    pub const ACCOUNT: &'static str = "account";
    /// A move onto the account's vault token's [`Self::word`] ([`Restart::Route`] to
    /// [`RouteTo::Token`]).
    pub const ROUTE_TOKEN: &'static str = "route-token";
    /// A move back onto the row's stored login's [`Self::word`] ([`Restart::Route`] to
    /// [`RouteTo::Stored`]).
    pub const ROUTE_STORED: &'static str = "route-stored";
    /// A person's click on the chip's [`Self::word`] ([`Restart::Permission`]), either way.
    pub const PERMISSION: &'static str = "permission";

    /// THE KINDS ([`Self::word`]) OF THE MOVES PUT OFF TO A QUIET MOMENT: a model's way back, an
    /// account move ahead and a route move, which the turn-end state owes until the move is made
    /// or refused, never a screen. Where its host answers a wait (`wait:…`,
    /// `busy:another-sweep`), the point goes on with the act it would have taken (`DEFERRED`) and
    /// the move is asked again at a later turn end, under the same ask ([`RestartAsk`]) while its
    /// run holds a live claim of the conversation's record ([`RestartClaim`], [`Self::moves`]).
    pub const PUT_OFF: [&'static str; 4] = [
        Self::MODEL_BACK,
        Self::ACCOUNT_AHEAD,
        Self::ROUTE_TOKEN,
        Self::ROUTE_STORED,
    ];

    /// Whether a restart of the kind `word` ([`Self::word`]) is a move put off to a quiet moment
    /// ([`Self::PUT_OFF`]).
    #[must_use]
    pub fn put_off(word: &str) -> bool {
        Self::PUT_OFF.contains(&word)
    }

    /// THE KINDS ([`Self::word`]) OF THE MOVES: a model's move onto its fallback and its way back,
    /// an account move at a wall or ahead of one, and a move onto another credential of the same
    /// account (a route move). A move is the session's, not its process's:
    /// asked again after the loop's other acts it is the same move, so while its run holds a live
    /// claim of the conversation's record ([`RestartClaim`]) the loop keeps that ask until the move
    /// is made, refused or another restart's run takes its place, and the re-ask meets the record
    /// it claimed (carried while its relaunch is in flight, made once that is carried on). A memory
    /// restart's need and a lost line's are the process's: neither is a move.
    pub const MOVES: [&'static str; 6] = [
        Self::MODEL,
        Self::MODEL_BACK,
        Self::ACCOUNT,
        Self::ACCOUNT_AHEAD,
        Self::ROUTE_TOKEN,
        Self::ROUTE_STORED,
    ];

    /// Whether a restart of the kind `word` ([`Self::word`]) is a move ([`Self::MOVES`]).
    #[must_use]
    pub fn moves(word: &str) -> bool {
        Self::MOVES.contains(&word)
    }

    /// The one word the journal names it by (`restart:<word>`).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Restart::Memory => "memory",
            Restart::InputLost { .. } => Self::INPUT_LOST,
            Restart::Model { .. } => Self::MODEL,
            Restart::ModelBack { .. } => Self::MODEL_BACK,
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => Self::ACCOUNT,
            Restart::Account {
                at: AccountAt::Ahead,
                ..
            } => Self::ACCOUNT_AHEAD,
            Restart::Route { to: RouteTo::Token } => Self::ROUTE_TOKEN,
            Restart::Route {
                to: RouteTo::Stored,
            } => Self::ROUTE_STORED,
            Restart::Permission { .. } => Self::PERMISSION,
        }
    }

    /// Whether `word` is [`Self::word`] of a move onto another account.
    #[must_use]
    pub fn moves_account(word: &str) -> bool {
        matches!(word, "account" | "account-ahead")
    }
}

/// WHICH ASK OF A RESTART IN PLACE THE HOST IS ASKED (`IdleHost::restart`'s `ask`): the same at
/// every ask of one restart, its first and each asked again after a deferral, and another at any
/// other restart's. A lost line's is its line's ([`Restart::InputLost`]'s `line`), the same while
/// the line is held, whatever other restart is asked meanwhile; any other restart's is its run's
/// (`Session::restart_run`): its kind and its run's Unix second, the second the loop first asked
/// it, or one past the last run's where that run's was this second or later
/// (`Session::restart_floor`), so no two runs of one loop share one; a loop's first run is past
/// the newest run's second its journal's restart notes name (each names its run's ask). It is
/// kept while the loop asks that kind again on its back-off, until the restart is made or refused
/// or the loop decides another act; a lost line's restart's run outlives that act while its line
/// stands held, and a move's ([`Restart::moves`]) while the run holds a live claim of the
/// conversation's record ([`RestartClaim`]: from the answer that took it until one that read the
/// record finds none), until the move is made or refused or another restart's run takes its place:
/// its next ask, at a later turn end or on its back-off, meets the record its ask claimed. A move
/// whose run holds no claim ends its run at the act, so a later ask that claims the record is
/// another run's; either way the move is dated by the second its claim was taken
/// (`approvals::model_switch_reason`), never by an ask that claimed nothing or a claim that
/// lapsed. A later loop restores the run of a move its journal's last restart note shows deferred
/// with its claim held, which outlives the acts there too. The
/// relaunch's record keeps the ask that claimed it (`St::ask`), and a restart asked again meets
/// its own record by that ask, never by its cause alone, so a restart asked of the process an
/// earlier restart of its cause started is made of it (`relaunch::its_own`); no account move's
/// record is its own, whatever its ask (`relaunch::carried_word` answers one in flight), so no
/// account move's answer holds or takes a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartAsk(String);

impl RestartAsk {
    /// The ask of `why` in the loop's restart run of Unix second `run_at`; a click's
    /// ([`Restart::Permission`]) is of the second it was clicked, the same at every look of its
    /// lane.
    #[must_use]
    pub fn of(why: &Restart, run_at: i64) -> Self {
        match why {
            Restart::InputLost { line, .. } => Self(format!("line:{}", line.word())),
            Restart::Memory
            | Restart::Model { .. }
            | Restart::ModelBack { .. }
            | Restart::Account { .. }
            | Restart::Route { .. }
            | Restart::Permission { .. } => Self(format!("{}@{run_at}", why.word())),
        }
    }

    /// The word the relaunch's record keeps (`St::ask`).
    #[must_use]
    pub fn word(&self) -> &str {
        &self.0
    }
}

/// THE HOST'S ANSWER TO AN ASK OF A RESTART IN PLACE (`IdleHost::restart`): `step`, its word for
/// what it did (`adopted`, `relaunched`, `continued…` or `done:…` that the restart is made;
/// `wait:…` or `busy:another-sweep` that it defers it; any other `busy:…`, `refused:…` or
/// `failed:…` that it made none), and `claim`, what it read of the conversation's record once it
/// answered ([`RestartClaim`]). The window host makes the claim in one place, from the record as
/// its restart left it (`relaunch::restart_as`). A deferral may come before the claim (a person's
/// hand on the tab, another sweep's lock, the record unwritable) or after it, its signal gone and
/// its relaunch landed (`wait:resume`, `wait:exiting`, `wait:shell-prompt`), and the words do not
/// tell the two apart (`wait:held` is said at either), so the loop reads the claim from this field
/// alone (`Session::restart_agent`): a run holds a claim from the answer that took it until an
/// answer that read the record finds it no live claim of the run's or the run ends, which keeps a
/// move's ask past its other acts (`Session::run_outlives_act`); the move is dated by the second
/// the claim was taken; and the run's claim is journaled (`HOST … restart:<kind> ask=<ask>
/// claimed=<second|no> step=<step>`), which a later loop reads back
/// (`turn_end_loop::journaled_open_moves`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartAnswer {
    /// The host's word for what it did.
    pub step: String,
    /// What the host read of the conversation's record once it answered.
    pub claim: RestartClaim,
}

impl RestartAnswer {
    /// `step`, where the answer read no record ([`RestartClaim::Unread`]).
    #[must_use]
    pub fn unread(step: impl Into<String>) -> Self {
        Self {
            step: step.into(),
            claim: RestartClaim::Unread,
        }
    }
}

/// WHAT A RESTART'S ANSWER READ OF THE CONVERSATION'S RECORD ([`RestartAnswer::claim`]), read back
/// under the sweep's lock as the restart left it (`relaunch::restart_as`). The asked restart's own
/// record is the one an ask of that restart claimed ([`RestartAsk`] against `St::ask`), of its
/// cause (`relaunch::its_own`), in the asker's tab; its claim is LIVE while its relaunch is in
/// flight, which a re-ask under the same ask carries, or once it is carried on, which a re-ask
/// answers made where it landed in the asker (`relaunch::already_made`). A record whose relaunch
/// stopped or failed unmade holds no live claim: a re-ask over it restarts afresh, a new claim. No
/// account move's record is its own, so no account move's answer holds or takes a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartClaim {
    /// The answer read no record (an account move not listed or not admitted, another sweep's
    /// lock, a snapshot of another tab or none, a process the restart was not asked of or with no
    /// shell job, no session record of the asker): it says nothing of the run's claim.
    Unread,
    /// The record read holds no live claim of the asked restart's: it is none, another restart's
    /// or another tab's, its own relaunch stopped or failed unmade, its own carried on in another
    /// process than the asker, or its own claim went unwritten or was restored.
    Unclaimed,
    /// The record is the asked restart's own, its claim live, as an earlier answer took it: this
    /// answer carried it in flight, or found it made in the asker.
    Held,
    /// This answer took the claim: it wrote the record as the asked restart's own, and its claim
    /// is live once it answered.
    Taken,
}

impl RestartClaim {
    /// Every claim, in the order [`Self::word`] names them.
    pub const ALL: [Self; 4] = [Self::Unread, Self::Unclaimed, Self::Held, Self::Taken];

    /// The one word a tape names it by.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Unclaimed => "unclaimed",
            Self::Held => "held",
            Self::Taken => "taken",
        }
    }

    /// The claim [`Self::word`] names `word` (`None`: no claim this build knows).
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|claim| claim.word() == word)
    }
}

/// Roster classification of the session's login at a weekly usage limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterWord {
    Unlisted,
    Row {
        next: Option<SignInNext>,
        ready: Option<Ready>,
        held: Option<HeldBy>,
    },
    Unread,
}

/// Another roster row ready to run the tab now ([`RosterWord::Row`]'s `ready`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub label: String,
}

/// WHY THE ROSTER DOES NOT MOVE A TAB AT ITS USAGE LIMIT ONTO A ROW READY ([`RosterWord::Row`]'s `held`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldBy {
    Earlier,
    Pin,
    Reset,
}

impl HeldBy {
    /// The clause [`super::notice::LIMIT_USE`]'s `{why}` is filled with.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Earlier => super::notice::WHY_USE_EARLIER,
            Self::Pin => super::notice::WHY_USE_PIN,
            Self::Reset => super::notice::WHY_USE_RESET,
        }
    }
}

/// The roster row a sign-in by name would make ready ([`RosterWord::Row`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInNext {
    pub label: String,
    pub renew: bool,
}

/// What the session's host advises about its account at one point ([`TurnEndReading::account`], from `crate::supervise::IdleHost::account`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountAdvice {
    Probe {
        otherwise: Option<Box<AccountAdvice>>,
    },
    /// `yields`: where the session's own account is another Mac's to use (F19), this move is how
    /// it yields it, and a turn end that does not make the move holds what it would type until
    /// then ([`account_hold`]), as at [`AccountAdvice::Hold`].
    Move {
        to: String,
        yields: Option<Instant>,
    },
    Wait {
        until: Instant,
    },
    /// At a turn end, the session's own account is another Mac's to use (its senior live claim,
    /// `docs/DESIGN-fluid-swarm-2026-10-28.md` F19) and the session cannot move off it: no row can
    /// take it, or a limit reset in flight on the account bars the move (R18 of
    /// `docs/SPEC-claude-accounts-resets-2026-10-02.md`). What the turn end would type waits until
    /// then, when the host is asked again ([`account_hold`]).
    Hold {
        until: Instant,
    },
    /// The host cannot advise yet and is asked again at `until`, for `why`: a wait as
    /// [`AccountAdvice::Wait`]'s, but no other account's opening. The roster never advises it
    /// ([`Self::of_roster_at`]).
    Again {
        until: Instant,
        why: AskAgain,
    },
    Resume {
        cleared_at: i64,
    },
}

/// Why the host asks again in place of advising ([`AccountAdvice::Again`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskAgain {
    /// The session's limit record is not written yet.
    Unwritten,
    /// A limit reset is in flight on the session's account.
    Reset,
}

impl AskAgain {
    /// The reason a wait until the next ask records.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Unwritten => "the limit's record is not written yet: asked again",
            Self::Reset => "a limit reset is in flight: asked again",
        }
    }
}

impl AccountAdvice {
    /// The advice the decider acts on.
    #[must_use]
    pub fn past_probe(&self) -> Option<&AccountAdvice> {
        let mut advice = self;
        while let AccountAdvice::Probe { otherwise } = advice {
            advice = otherwise.as_deref()?;
        }
        Some(advice)
    }

    /// The roster's advice (`crate::harness::accounts::Advice`) as the policy takes it, `clock` being the `Instant` of the unix second `now`.
    #[must_use]
    pub fn of_roster_at(
        advice: &crate::harness::accounts::Advice,
        past: Option<&crate::harness::accounts::Advice>,
        state: &crate::harness::accounts::State,
        opening: Option<i64>,
        at_wall: bool,
        now: i64,
        clock: Instant,
    ) -> Option<AccountAdvice> {
        use crate::harness::accounts::{Advice, StayWhy, hold_until};
        let at = |until: i64| {
            let secs = u64::try_from(until.saturating_sub(now))
                .unwrap_or(0)
                .min(86_400 * 365 * 10);
            later(clock, Duration::from_secs(secs))
        };
        let plain = |advice: &Advice| match advice {
            Advice::Move { to, yields, .. } => Some(AccountAdvice::Move {
                to: to.clone(),
                yields: yields.map(at),
            }),
            Advice::Wait { until } if at_wall => Some(AccountAdvice::Wait { until: at(*until) }),
            Advice::Wait { until } => Some(AccountAdvice::Hold { until: at(*until) }),
            Advice::Resume { cleared_at } => Some(AccountAdvice::Resume {
                cleared_at: *cleared_at,
            }),
            Advice::Stay(StayWhy::Breaker { until }) if !at_wall => {
                Some(AccountAdvice::Wait { until: at(*until) })
            }
            Advice::Stay(why @ (StayWhy::Canary | StayWhy::Breaker { .. })) if at_wall => {
                hold_until(*why, state, opening, now)
                    .map(|until| AccountAdvice::Wait { until: at(until) })
            }
            Advice::Stay(_) | Advice::Probe => None,
        };
        match advice {
            Advice::Probe => {
                let otherwise = past.and_then(plain);
                if at_wall {
                    otherwise
                } else {
                    Some(AccountAdvice::Probe {
                        otherwise: otherwise.map(Box::new),
                    })
                }
            }
            other => plain(other),
        }
    }

    /// [`Self::of_roster_at`] for a caller that has no roster's opening and no advice past a probe to hand over.
    #[must_use]
    pub fn of_roster(
        advice: &crate::harness::accounts::Advice,
        state: &crate::harness::accounts::State,
        at_wall: bool,
        now: i64,
        clock: Instant,
    ) -> Option<AccountAdvice> {
        Self::of_roster_at(advice, None, state, None, at_wall, now, clock)
    }
}

/// The point the host is asked about ([`crate::supervise::IdleHost::account`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPoint {
    Wall { kind: WallKind },
    LoginEnded,
    TurnEnd,
}

impl AccountPoint {
    /// Whether the session stands at a wall here, not at a plain turn end.
    #[must_use]
    pub const fn at_wall(self) -> bool {
        !matches!(self, AccountPoint::TurnEnd)
    }
}

/// What the first turn after this session's own account move was answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountBad {
    Wall,
    SignedOut,
}

/// [`TurnEndState::account_answer`]: how the move was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAnswer {
    Served,
    Bad(AccountBad),
}

/// An account move this session's host made and no point has answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMove {
    pub to: String,
    pub at: Instant,
    pub wall: bool,
}

/// The most account moves one session makes in its window ([`TurnEndTiming::window`], an hour).
pub const ACCOUNT_MOVES_PER_WINDOW: usize = 4;

const ACCOUNT_WAIT_FLOOR: Duration = Duration::from_secs(60);

/// The words a worker says it is DONE with.
pub const DONE_PHRASES: &[&str] = &[
    "nothing left to do",
    "nothing else to do",
    "nothing more to do",
    "nothing further to do",
    "nothing left to work on",
    "no further work",
    "no remaining work",
    "no more work",
    "all done",
    "all tasks are complete",
    "all tasks complete",
    "everything is done",
    "everything is already done",
    "everything is complete",
    "already complete",
    "work is complete",
    "work is done",
    "task is complete",
    "workflow is finished",
    "workflow is complete",
    "all tasks are completed",
    "all tasks completed",
    "everything is completed",
    "work is completed",
    "task is completed",
    "workflow is completed",
    "everything was completed",
    "everything is already completed",
    "everything was already completed",
    "everything has already been completed",
    "everything had already been completed",
    "everything was already done",
    "all tasks were completed",
    "all tasks are already completed",
    "all tasks were already completed",
    "all tasks have already been completed",
    "all tasks had already been completed",
    "work was completed",
    "work is already completed",
    "work was already completed",
    "work has already been completed",
    "work had already been completed",
    "task was completed",
    "task is already completed",
    "task was already completed",
    "task has already been completed",
    "task had already been completed",
    "completed successfully",
    "what would you like me to work on",
    "what would you like me to do",
    "what should i work on",
    "what else would you like",
    "let me know if you need anything",
    "let me know if there's anything else",
    "what's next?",
    "anything else?",
    "i'm stopping here",
    "i am stopping here",
    "i'll stop here",
    "i will stop here",
    "i've stopped.",
    "i have stopped.",
    "so the next session can",
    "everything i can do is",
    "everything this machine can do",
    "nothing more i can do",
    "nothing else i can do",
];

/// WHETHER THE WORKER'S LAST WORDS CLAIM IT IS DONE.
#[must_use]
pub fn says_done(tail: Option<&str>) -> bool {
    let Some(t) = tail else {
        return false;
    };
    thread_local! {
        static LAST: LastTail<bool> = const { LastTail::new() };
    }
    LAST.with(|last| {
        last.get_or(t, || {
            needs::defense_line(t).is_some() || says_done_read(&needs::without_needs(t))
        })
    })
}

const CHECK_ECHO: [&str; 3] = [
    "nothing of the job is left",
    "everything left needs a person",
    "finished and verified",
];

fn says_done_read(t: &str) -> bool {
    if says_done_outright(t) {
        return true;
    }
    let flat = done_flat(t);
    says_idle(&flat) || CHECK_ECHO.iter().any(|echo| said_as_report(&flat, echo))
}

fn said_as_report(flat: &str, echo: &str) -> bool {
    flat.match_indices(echo).any(|(at, _)| {
        !negated_before(flat, at)
            && !conditional_at(flat, at, echo)
            && !partial_subject_at(flat, at, echo)
    })
}

const CLAUSE_ENDS: &[char] = &[',', ';', ':', '.', '!', '?', '—', '–', '(', ')'];

fn clause_before(flat: &str, at: usize) -> Vec<&str> {
    flat[..at]
        .rsplit(CLAUSE_ENDS)
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect()
}

fn conditional_at(flat: &str, at: usize, phrase: &str) -> bool {
    const IF: &[&str] = &["if", "when", "whenever", "unless", "once"];
    const STATES: &[&[&str]] = &[
        &["provided"],
        &["providing"],
        &["assuming"],
        &["supposing"],
        &["as", "soon", "as"],
        &["as", "long", "as"],
    ];
    const LEADS: &[&[&str]] = &[
        &["after"],
        &["before"],
        &["by", "the", "time"],
        &["in", "case"],
        &["whether"],
    ];
    let after = if phrase.ends_with(CLAUSE_ENDS) {
        ""
    } else {
        flat[at + phrase.len()..]
            .split(CLAUSE_ENDS)
            .next()
            .unwrap_or("")
    };
    let mut words = clause_before(flat, at);
    let p = words.len();
    words.push("\0");
    words.extend(after.split_whitespace());
    let word = |j: Option<usize>| j.and_then(|j| words.get(j)).copied().unwrap_or("");
    (0..words.len()).any(|i| {
        let w = words[i];
        let (next, prev, prev2) = (
            word(i.checked_add(1)),
            word(i.checked_sub(1)),
            word(i.checked_sub(2)),
        );
        let count = w == "once"
            && (matches!(next, "more" | "again")
                || prev == "at"
                || (prev == "than" && prev2 == "more"));
        let rest = &words[i..];
        let leads = i < p
            && (LEADS.iter().any(|s| words[i..p].starts_with(s))
                || (matches!(w, "until" | "till") && waits_at(&words, i)));
        (IF.contains(&w) && !count) || STATES.iter().any(|s| rest.starts_with(s)) || leads
    })
}

fn partial_subject_at(flat: &str, at: usize, phrase: &str) -> bool {
    const WHOLE: &[&str] = &[
        "the", "this", "that", "my", "your", "our", "all", "whole", "entire",
    ];
    const PART: &[&str] = &[
        "half",
        "most",
        "some",
        "part",
        "much",
        "almost",
        "nearly",
        "mostly",
        "partly",
        "largely",
        "practically",
        "virtually",
        "of",
    ];
    let noun = phrase.starts_with("task ") || phrase.starts_with("work ");
    if !(noun || phrase.starts_with("all ") || phrase.starts_with("everything ")) {
        return false;
    }
    let before = clause_before(flat, at);
    let mut i = before.len();
    if noun {
        while i > 0 && WHOLE.contains(&before[i - 1]) {
            i -= 1;
        }
        if i == before.len() && i > 0 {
            // A modifier, not a determiner, just before the noun.
            return true;
        }
    }
    match i.checked_sub(1).map(|j| before[j]) {
        Some("of") => i < 2 || before[i - 2] != "all",
        Some(w) => PART.contains(&w),
        None => false,
    }
}

#[must_use]
pub(crate) fn says_done_outright(t: &str) -> bool {
    if t.lines().any(is_finished_line) || t.trim_matches(|c: char| !c.is_alphanumeric()) == "DONE" {
        return true;
    }
    let blocks: Vec<String> = done_blocks(&without_code(t))
        .iter()
        .map(|b| done_flat(b))
        .collect();
    let last = blocks.len().saturating_sub(1);
    DONE_PHRASES.iter().any(|p| {
        blocks
            .iter()
            .enumerate()
            .any(|(i, b)| said_done_phrase(b, p, i == last))
    })
}

fn without_code(t: &str) -> String {
    let mut block = None;
    let mut out: Vec<String> = Vec::new();
    for line in t.lines() {
        if needs::fenced_line(line, &mut block) {
            out.push(String::new());
            continue;
        }
        let parts: Vec<&str> = line.split('`').collect();
        let last = parts.len().saturating_sub(1);
        let kept: Vec<&str> = parts
            .iter()
            .enumerate()
            .filter(|&(i, _)| i % 2 == 0 || (last % 2 == 1 && i == last))
            .map(|(_, p)| *p)
            .collect();
        out.push(kept.join(" "));
    }
    out.join("\n")
}

fn done_blocks(t: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut open = false;
    let mut introduced = false;
    for line in t.lines() {
        let l = line.trim();
        if l.is_empty() {
            open = false;
            continue;
        }
        if !open || (opens_item(l) && !introduced) {
            blocks.push(String::new());
            open = true;
            introduced = false;
        }
        let b = blocks.last_mut().expect("a block was opened");
        b.push(' ');
        b.push_str(l);
        introduced |= l.ends_with(':');
    }
    blocks
}

fn opens_item(l: &str) -> bool {
    let rest = if let Some(r) = l.strip_prefix(['-', '*', '•']) {
        r
    } else if l.starts_with('#') {
        l.trim_start_matches('#')
    } else {
        let digits = l.trim_start_matches(|c: char| c.is_ascii_digit());
        if digits.len() == l.len() {
            return false;
        }
        match digits.strip_prefix(['.', ')']) {
            Some(r) => r,
            None => return false,
        }
    };
    rest.starts_with(char::is_whitespace)
}

const BUSY: &[&str] = &[
    "still running",
    "still going",
    "still working",
    "still in progress",
    "is running:",
    "are running:",
    "next step",
    "is next",
];

const WAIT_WORDS: &[&str] = &["until", "till", "once"];

const WAIT_PHRASES: &[&str] = &[
    "will notify",
    "notify me",
    "notifies me",
    "waiting on",
    "waiting for",
    "reports back",
    "report back",
];

const ASKS_NEXT: &[&str] = &["what's next?", "anything else?"];

fn said_done_phrase(flat: &str, phrase: &str, last: bool) -> bool {
    flat.match_indices(phrase).any(|(at, _)| {
        if !whole_words_at(flat, at, phrase) {
            return false;
        }
        if negated_before(flat, at)
            || conditional_at(flat, at, phrase)
            || partial_subject_at(flat, at, phrase)
        {
            return false;
        }
        let (start, end) = sentence_of(flat, at, phrase);
        if waits_in(&flat[start..end]) {
            return false;
        }
        !(ASKS_NEXT.contains(&phrase)
            && (!last || flat[at + phrase.len()..].chars().any(char::is_alphanumeric)))
    })
}

fn whole_words_at(flat: &str, at: usize, phrase: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let end = at + phrase.len();
    !word(flat[..at].chars().next_back())
        && !(phrase.ends_with(char::is_alphanumeric) && word(flat[end..].chars().next()))
}

fn sentence_of(flat: &str, at: usize, phrase: &str) -> (usize, usize) {
    const ENDS: [&str; 3] = [". ", "! ", "? "];
    let start = ENDS
        .iter()
        .filter_map(|e| flat[..at].rfind(e).map(|i| i + e.len()))
        .max()
        .unwrap_or(0);
    let from = at + phrase.trim_end_matches(['.', '?', '!']).len();
    let end = ENDS
        .iter()
        .filter_map(|e| flat[from..].find(e).map(|i| from + i + 1))
        .min()
        .unwrap_or(flat.len());
    (start, end)
}

fn waits_in(sentence: &str) -> bool {
    if WAIT_PHRASES
        .iter()
        .chain(BUSY)
        .any(|w| sentence.contains(w))
    {
        return true;
    }
    let words: Vec<&str> = sentence
        .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    (0..words.len()).any(|i| WAIT_WORDS.contains(&words[i]) && waits_at(&words, i))
}

fn waits_at(words: &[&str], i: usize) -> bool {
    const PERSON: &[&str] = &["you", "you're", "you've", "someone", "somebody"];
    const PERSON_NOUN: &[&str] = &["owner", "user", "person", "owner's", "user's", "person's"];
    const PERSONS_ACT: &[&str] = &[
        "review",
        "approval",
        "go",
        "decision",
        "reply",
        "answer",
        "input",
        "feedback",
        "sign",
        "confirmation",
        "ok",
        "okay",
        "say",
        "permission",
        "instructions",
        "response",
        "call",
    ];
    let word = |j: Option<usize>| j.and_then(|j| words.get(j)).copied().unwrap_or("");
    let (next, next2) = (word(i.checked_add(1)), word(i.checked_add(2)));
    let (prev, prev2) = (word(i.checked_sub(1)), word(i.checked_sub(2)));
    let person = PERSON.contains(&next)
        || (matches!(next, "the" | "a") && PERSON_NOUN.contains(&next2))
        || (next == "your"
            && next2
                .split('-')
                .next()
                .is_some_and(|w| PERSONS_ACT.contains(&w)));
    let count = words[i] == "once"
        && (matches!(next, "more" | "again")
            || prev == "at"
            || (prev == "than" && prev2 == "more"));
    !(person || count)
}

fn said_unnegated(flat: &str, phrase: &str) -> bool {
    flat.match_indices(phrase)
        .any(|(at, _)| !negated_before(flat, at))
}

fn negated_before(flat: &str, at: usize) -> bool {
    flat[..at]
        .split(' ')
        .rev()
        .filter(|w| !w.is_empty())
        .take(2)
        .take_while(|w| !w.ends_with(['.', ',', ':', ';', '!', '?']))
        .any(|w| w == "not" || w == "cannot" || w.ends_with("n't"))
}

fn done_flat(t: &str) -> String {
    t.to_lowercase()
        .replace('’', "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn says_idle(flat: &str) -> bool {
    const EMPTY_QUEUE: &[&str] = &["all queued work has landed", "all queued work is landed"];
    if BUSY.iter().chain(STOP_PHRASES).any(|b| flat.contains(b)) {
        return false;
    }
    if EMPTY_QUEUE.iter().any(|p| said_unnegated(flat, p)) {
        return true;
    }
    let words: Vec<(&str, bool)> = flat
        .split(' ')
        .map(|w| {
            let end = &w[w.trim_end_matches(|c: char| !c.is_alphanumeric()).len()..];
            (
                w.trim_matches(|c: char| !c.is_alphanumeric()),
                end.contains(['.', ',', ':', ';', '!', '?']),
            )
        })
        .collect();
    let one_clause = |w: &[(&str, bool)]| w[..w.len() - 1].iter().all(|&(_, ends)| !ends);
    let be = |w: &str| matches!(w, "is" | "are");
    let idle = |w: &str| matches!(w, "running" | "queued");
    let none_of = |w: &[(&str, bool)]| w[0].0 == "no" || (w[0].0 == "nothing" && w[1].0 == "else");
    let of_one = words
        .windows(4)
        .any(|w| one_clause(w) && none_of(w) && be(w[2].0) && idle(w[3].0));
    of_one
        || words
            .windows(3)
            .any(|w| one_clause(w) && w[0].0 == "nothing" && be(w[1].0) && idle(w[2].0))
}

pub(crate) fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE
        || rule == RULE_GO_AHEAD
        || rule == RULE_SUGGESTION
        || is_answer_rule(rule)
        || rule == RULE_DONE_CHECK
        || is_challenge_rule(rule)
        || is_land_rule(rule)
}

/// Whether `rule` answers a question the worker asked in prose.
#[must_use]
pub fn is_answer_rule(rule: &str) -> bool {
    PriorityWord::ALL
        .into_iter()
        .any(|p| p.prose_rule_id() == rule)
}

/// Claude Code's own `/model` aliases: the only names a switch back is typed with.
pub const MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];

/// Pieces a suggestion may be made of to be a continuation, joined by `,`, `;`, `.`, ` and ` or ` then `: what builds before v6's 5a accepted bare under [`RULE_SUGGESTION`] ([`suggestion_allowed`]). Cruise now types its own continuation over any suggestion.
pub const SUGGESTION_ALLOW: &[&str] = &[
    "keep going",
    "continue",
    "keep fixing forward",
    "push it when green",
    "go on",
    "carry on",
    "keep at it",
    "proceed",
    "go ahead",
];

/// Phrases a worker asks a person for a decision with.
pub const STOP_PHRASES: &[&str] = &[
    "need your decision",
    "needs your decision",
    "need a decision",
    "your decision on",
    "blocked on",
    "i'm blocked",
    "i am blocked",
    "waiting on you",
    "waiting for you",
    "waiting on your",
    "waiting for your",
    "your call",
    "need your input",
    "need your approval",
    "need you to",
    "please confirm",
    "can you confirm",
    "please advise",
    "how would you like me to proceed",
    "cannot proceed without",
    "can't proceed without",
    "your go-ahead",
    "your go ahead",
    "the go-ahead",
    "a go-ahead",
    "sign off",
    "sign-off",
    "signoff",
    "for your review",
    "ready for review",
    "ready for your review",
    "please review",
    "your approval",
    "needs approval",
    "need approval",
    "how you'd like",
    "how you would like",
    "how would you like",
    "what you'd like me to",
    "let me know how",
    "let me know whether",
    "i'll wait",
    "i will wait",
    "i'll hold off",
    "holding off until",
    "before i proceed",
    "before i continue",
    "before i go ahead",
    "before i run",
    "before i push",
    "before i force",
    "before i delete",
    "before i merge",
    "before i deploy",
    "before i release",
];

/// Phrases a worker asks a person to choose with.
pub const CHOICE_PHRASES: &[&str] = &[
    "which option",
    "which would you prefer",
    "which do you prefer",
    "let me know which",
];

/// Words that make an offer, stop, choice or question one a person must answer.
pub const DESTRUCTIVE_WORDS: &[&str] = &[
    "force",
    "force-push",
    "--force",
    "push -f",
    "force-with-lease",
    "delete",
    "deleting",
    "deletion",
    "drop",
    "dropping",
    "remove",
    "removing",
    "removal",
    "rm",
    "deploy",
    "deploying",
    "migrate",
    "migrating",
    "publish",
    "publishing",
    "release",
    "releasing",
    "wipe",
    "purge",
    "reset",
    "overwrite",
    "truncate",
    "destroy",
    "uninstall",
    "revert",
    "rewrite history",
];

/// Words that make a question an offer to go on (it counts as a continuation).
pub const OFFER_PHRASES: &[&str] = &[
    "want me to",
    "shall i",
    "should i continue",
    "should i proceed",
    "should i keep going",
    "should i go ahead",
    "should i also",
    "would you like me to",
    "next steps:",
    "next step:",
    "i can also",
    "if you'd like, i can",
    "let me know if you'd like",
    "happy to",
];

/// Words that recommend one option.
pub const RECOMMEND_PHRASES: &[&str] = &[
    "i recommend",
    "i'd recommend",
    "i would recommend",
    "recommended",
    "my recommendation",
    "i suggest",
    "i'd suggest",
    "i'd go with",
    "i'll go with",
    "i'd pick",
    "i lean",
];

/// The busy work that makes a turn real work by default ([`TurnEndTiming::min_work`]).
pub const MIN_WORK: Duration = Duration::from_secs(2 * 60);

/// The policy's timings; tests shorten them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndTiming {
    pub min_work: Duration,
    pub short_backoff: Duration,
    pub short_backoff_max: Duration,
    pub retry_backoff: Vec<Duration>,
    pub net_backoff: Vec<Duration>,
    /// An outage wall's first blind try comes this long after the wall ([`outage_wait`]), each
    /// later one twice as long after its own wall's re-read, up to [`Self::outage_cap`].
    pub outage_first: Duration,
    /// The longest gap between an outage wall's blind tries, the period a wait the host
    /// measures down decides again at, and how long a measured outage holds what aterm would
    /// type where no wall stands, or the try of a wall only the measure makes the connection's
    /// ([`Self::outage_holds_until`]).
    pub outage_cap: Duration,
    pub progress: Duration,
    pub reset_grace: Duration,
    pub reset_jitter: Duration,
    /// How long a minute of the time from a wall's read to the reset its words name lasts on the
    /// harness's clock ([`Self::reset_placed`]): [`RESET_MINUTE`], unless a development build's
    /// [`RESET_SPAN_SCALE_SEAM`] shortens it.
    pub reset_minute: Duration,
    pub auto_resume_grace: Duration,
    pub limit_backoff: Vec<Duration>,
    pub entitlement_backoff: Vec<Duration>,
    pub refused_backoff: Vec<Duration>,
    pub window: Duration,
    pub take_within: Duration,
    pub restart_backoff: Vec<Duration>,
    pub credits_bound: Duration,
}

const NO_AUTO_CONTINUE_GRACE: Duration = Duration::from_secs(60);

/// A minute, as the shipped harness runs the time to a wall's reset: the reset sits where its
/// words name it ([`TurnEndTiming::reset_minute`]).
pub const RESET_MINUTE: Duration = Duration::from_secs(60);

impl TurnEndTiming {
    /// Until when `reach`, read at `now`, holds what aterm would type where no wall stands (a
    /// point's act, [`outage_hold`]; a line at a break, through `Guard::Reach`), and the blind
    /// try of a wall whose own words name no lost route, which only this measure makes the
    /// connection's outage (a code-less server error, [`outage_wait`]): a measured outage
    /// ([`Reach::Down`]) holds for [`Self::outage_cap`] from when it began, and then nothing. The
    /// cap runs once per outage, not per point or line, because nothing but the probe says the
    /// route is lost (the agent's own requests got through for its last turn to end, or its wall
    /// names no lost route), so a probe that reads Down while they still get through (a route of
    /// its own, a proxy, a firewall that blocks only the probe) costs the session one cap, never
    /// one act per cap; typed into a real outage, the line's answer is the agent's own network
    /// wall, which [`outage_wait`] waits out on the measured reach with no cap where its words
    /// name the lost route. `None` when the reach is not Down or the cap has passed.
    #[must_use]
    pub fn outage_holds_until(&self, reach: Reach, now: Instant) -> Option<Instant> {
        let Reach::Down { since } = reach else {
            return None;
        };
        let until = later(since, self.outage_cap);
        (now < until).then_some(until)
    }

    /// The grace past a wall's reset for the program the wall is in.
    #[must_use]
    pub fn grace_for(&self, program: Program) -> Duration {
        if auto_continues(program) {
            self.reset_grace
        } else {
            self.reset_grace.min(NO_AUTO_CONTINUE_GRACE)
        }
    }
}

const fn auto_continues(program: Program) -> bool {
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

impl Default for TurnEndTiming {
    fn default() -> Self {
        let min = |m: u64| Duration::from_secs(m * 60);
        Self {
            min_work: MIN_WORK,
            short_backoff: min(2),
            short_backoff_max: min(60),
            retry_backoff: vec![min(1), min(5), min(15), min(30), min(60)],
            net_backoff: vec![min(1), min(2), min(5)],
            outage_first: min(2),
            outage_cap: min(60),
            progress: min(5),
            reset_grace: Duration::from_secs(150),
            reset_jitter: Duration::from_secs(60),
            reset_minute: RESET_MINUTE,
            auto_resume_grace: min(10),
            limit_backoff: vec![min(10), min(30)],
            entitlement_backoff: vec![min(30), min(120), min(360)],
            refused_backoff: vec![min(1), min(30), min(120), min(360)],
            window: min(60),
            take_within: Duration::from_secs(30),
            restart_backoff: vec![Duration::from_secs(10), min(1), min(5), min(10)],
            credits_bound: min(300),
        }
    }
}

/// THE LADDER'S DEVELOPMENT SEAM.
pub const LADDER_SCALE_SEAM: &str = "ATERM_DEV_LADDER_SCALE";

/// THE RESET WAIT'S DEVELOPMENT SEAM: a factor in `(0, 1]` on the grace and jitter the harness
/// waits past a wall's reset ([`TurnEndTiming::reset_scaled`]), so a world test's carry-on comes
/// at the wall's own reset and its wait is that reset and the hang detector. Only a development
/// build reads it.
pub const RESET_SCALE_SEAM: &str = "ATERM_DEV_RESET_SCALE";

/// THE RESET SPAN'S DEVELOPMENT SEAM: a factor in `(0, 1]` on the time from a wall's read to the
/// reset its words name ([`TurnEndTiming::reset_span_scaled`]), so a world test waits out a wall
/// that says `in 1m` in seconds while the carry-on still comes after its reset. The wall's words
/// are not scaled. Only a development build reads it.
pub const RESET_SPAN_SCALE_SEAM: &str = "ATERM_DEV_RESET_SPAN_SCALE";

impl TurnEndTiming {
    /// The same timing, its short back-off ladder and an outage wall's ladder scaled by `factor`.
    #[must_use]
    pub fn ladder_scaled(mut self, factor: f64) -> Self {
        self.short_backoff = self.short_backoff.mul_f64(factor);
        self.short_backoff_max = self.short_backoff_max.mul_f64(factor);
        self.outage_first = self.outage_first.mul_f64(factor);
        self.outage_cap = self.outage_cap.mul_f64(factor);
        self
    }

    /// The back-off before the next act after a streak of `streak` short turns
    /// ([`TurnEndState::short_wait`]): [`Self::short_backoff`] after the first, doubled at each
    /// turn past it up to [`Self::short_backoff_max`]; `None` before any.
    #[must_use]
    pub fn short_wait_at(&self, streak: u32) -> Option<Duration> {
        let n = streak.checked_sub(1)?;
        Some(crate::supervise::ladder::doubling(
            self.short_backoff,
            n,
            self.short_backoff_max,
        ))
    }

    /// The factor [`LADDER_SCALE_SEAM`]'s value names.
    #[must_use]
    pub fn ladder_scale(raw: Option<&std::ffi::OsStr>) -> Option<f64> {
        let s = raw?.to_str()?.trim();
        if s.starts_with('+') {
            return None;
        }
        let f: f64 = s.parse().ok()?;
        (f.is_finite() && f > 0.0 && f <= 1.0).then_some(f)
    }

    /// The same timing, the wait past a wall's reset ([`Self::reset_grace`] and
    /// [`Self::reset_jitter`]) scaled by `factor`.
    #[must_use]
    pub fn reset_scaled(mut self, factor: f64) -> Self {
        self.reset_grace = self.reset_grace.mul_f64(factor);
        self.reset_jitter = self.reset_jitter.mul_f64(factor);
        self
    }

    /// The same timing, the time from a wall's read to the reset its words name
    /// ([`Self::reset_minute`]) scaled by `factor`.
    #[must_use]
    pub fn reset_span_scaled(mut self, factor: f64) -> Self {
        self.reset_minute = self.reset_minute.mul_f64(factor);
        self
    }

    /// Whether a wall's reset sits where its words name it ([`Self::reset_minute`] is
    /// [`RESET_MINUTE`]), as in every shipped build.
    #[must_use]
    pub fn reset_as_named(&self) -> bool {
        self.reset_minute == RESET_MINUTE
    }

    /// `at`, the reset a wall's words name in unix seconds, read at `now_ms` in unix
    /// milliseconds, where the harness waits to it: the time from the read to `at` lasts
    /// [`Self::reset_minute`] a minute, and the reset falls on the first whole second at or after
    /// that, so it never comes before the scaled time. A reset as named
    /// ([`Self::reset_as_named`]), one at or before the read, and one too far off to scale stay
    /// at `at`.
    #[must_use]
    pub fn reset_placed(&self, at: i64, now_ms: i64) -> i64 {
        if self.reset_as_named() {
            return at;
        }
        scaled_reset(at, now_ms, self.reset_minute).unwrap_or(at)
    }

    /// Construct timing parameters for a session started by this build.
    #[must_use]
    pub fn for_build() -> Self {
        Self::for_seams(
            aterm_types::dev_seam!(LADDER_SCALE_SEAM).as_deref(),
            aterm_types::dev_seam!(RESET_SCALE_SEAM).as_deref(),
            aterm_types::dev_seam!(RESET_SPAN_SCALE_SEAM).as_deref(),
        )
    }

    /// [`Self::for_build`] under the values its three development seams read
    /// ([`LADDER_SCALE_SEAM`], [`RESET_SCALE_SEAM`], [`RESET_SPAN_SCALE_SEAM`]), each a factor in
    /// `(0, 1]`.
    #[must_use]
    pub fn for_seams(
        ladder: Option<&std::ffi::OsStr>,
        reset: Option<&std::ffi::OsStr>,
        span: Option<&std::ffi::OsStr>,
    ) -> Self {
        let mut timing = Self::default();
        if let Some(factor) = Self::ladder_scale(ladder) {
            timing = timing.ladder_scaled(factor);
        }
        if let Some(factor) = Self::ladder_scale(reset) {
            timing = timing.reset_scaled(factor);
        }
        if let Some(factor) = Self::ladder_scale(span) {
            timing = timing.reset_span_scaled(factor);
        }
        timing
    }
}

/// [`TurnEndTiming::reset_placed`]'s arithmetic: the first whole second at or after `now_ms` plus
/// the span from it to `at` at `minute` a minute; `None` for a reset at or before the read, or one
/// too far off to scale.
fn scaled_reset(at: i64, now_ms: i64, minute: Duration) -> Option<i64> {
    let span_ms = i128::from(at)
        .checked_mul(1000)?
        .checked_sub(i128::from(now_ms))?;
    if span_ms <= 0 {
        return None;
    }
    // The read and the scaled span, both in milliseconds times a minute's nanoseconds, so one
    // division by a second's worth, rounded up, places the reset.
    let named = i128::try_from(RESET_MINUTE.as_nanos()).ok()?;
    let rate = i128::try_from(minute.as_nanos()).ok()?;
    let placed = i128::from(now_ms)
        .checked_mul(named)?
        .checked_add(span_ms.checked_mul(rate)?)?;
    let second = named.checked_mul(1000)?;
    i64::try_from(placed.div_euclid(second) + i128::from(placed.rem_euclid(second) != 0)).ok()
}

/// The composer as the reading found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composer {
    Absent,
    Unknown,
    Empty,
    Placeholder(String),
    Typed,
    Own,
    Diverted(Diversion),
    /// A draft its program would run as a command on Enter, not take as a prompt
    /// ([`aterm_phase::draft_command`], or Claude Code's slash menu whose Enter runs an entry):
    /// a person's, it stands as their hold, never sent by aterm (v6 §14.17.5.8,
    /// [`COMMAND_STANDS`]), whatever a menu over it does. The loop's own line equal to it, left
    /// with no person's typing since, reads as the menu over it says ([`CommandEnter::own`]).
    Command(aterm_phase::DraftCommand, CommandEnter),
}

/// WHAT ENTER DOES WITH A COMMAND STANDING ([`Composer::Command`]) UNDER THE COMPLETION MENU DRAWN
/// OVER IT: nothing to a person's command, which stands whatever it says, and the composer a line
/// of the loop's own reads as ([`Self::own`]), so no Enter runs an entry the loop never wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEnter {
    /// Enter runs the line as typed: no menu is drawn, or its Enter runs this command.
    Runs,
    /// The menu's Enter runs or completes another entry ([`Diversion::Menu`]).
    Diverted,
    /// The menu's selection could not be read: its Enter may run the line or another entry.
    Unread,
}

impl CommandEnter {
    /// What a menu read `menu` (`None` where Enter sends the draft as typed; else
    /// [`Composer::Diverted`] or [`Composer::Unknown`]) says of a command's Enter.
    #[must_use]
    pub const fn of(menu: Option<&Composer>) -> Self {
        match menu {
            None => Self::Runs,
            Some(Composer::Diverted(_)) => Self::Diverted,
            Some(_) => Self::Unread,
        }
    }

    /// The composer the loop's own line reads as under it: a line its Enter runs as typed
    /// ([`Composer::Typed`]), else what stands over it, as for any draft.
    #[must_use]
    pub const fn own(self) -> Composer {
        match self {
            Self::Runs => Composer::Typed,
            Self::Diverted => Composer::Diverted(Diversion::Menu),
            Self::Unread => Composer::Unknown,
        }
    }
}

impl Composer {
    /// Whether the composer is no empty one to type into: a draft, aterm's own words, or something
    /// standing over it (whose row may hold no text at all, as the prompt-history search's over an
    /// empty composer).
    #[must_use]
    pub const fn drafted(&self) -> bool {
        matches!(
            self,
            Composer::Typed | Composer::Own | Composer::Diverted(_) | Composer::Command(..)
        )
    }

    /// The question a point standing on this composer asks before any act of its, whatever the
    /// act: aterm's own words standing ([`OWN_WORDS_STAND`]), or what diverts the composer's Enter
    /// ([`Diversion::reason`]). `None` for a composer a person's draft (their command included)
    /// or nothing stands in.
    #[must_use]
    pub const fn asks(&self) -> Option<&'static str> {
        match self {
            Composer::Own => Some(OWN_WORDS_STAND),
            Composer::Diverted(by) => Some(by.reason()),
            Composer::Absent
            | Composer::Unknown
            | Composer::Empty
            | Composer::Placeholder(_)
            | Composer::Typed
            | Composer::Command(..) => None,
        }
    }
}

/// WHAT KEEPS THE COMPOSER'S ENTER FROM SENDING A PERSON'S DRAFT ([`Composer::Diverted`]): something
/// standing over it, or an old prompt a search left in it. The point is asked about
/// ([`Self::reason`]) once a person's grace is out, and decided again when it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Diversion {
    /// Claude Code's completion menu, open under a draft: Enter may run or complete its entry.
    Menu,
    /// The program's prompt-history search, open over the composer
    /// (`aterm_phase::ScreenReader::history_search`): keys type into its query and Enter sends the
    /// old prompt it matched, whatever the composer's row shows.
    Search,
    /// The old prompt the search matched, left in the composer unedited as the search went: Esc
    /// accepts the match in Claude Code 2.1.318 and Muse Code 1.4.14 alike (measured,
    /// `claude-2.1.318-history-search-left`, `muse-1.4.14-R7309.1-history-search-left`), so a
    /// person backing out of the search leaves it there, drawn as typed text. It is no draft of
    /// theirs to send: their edit makes it one.
    Recall,
}

impl Diversion {
    /// The escalation's reason.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Menu => MENU_DIVERTS,
            Self::Search => SEARCH_DIVERTS,
            Self::Recall => RECALL_STANDS,
        }
    }

    /// What the person does in the tab, for a notice about the draft.
    #[must_use]
    pub const fn todo(self) -> &'static str {
        match self {
            Self::Menu => "Close the completion menu, then send the draft in the tab.",
            Self::Search => "Close the prompt-history search in the tab.",
            Self::Recall => "Send, edit or clear the old prompt the search left in the tab.",
        }
    }
}

/// WHY A PERSON'S COMMAND HOLDS THE POINT ([`Composer::Command`], v6 §14.17.5.8): the one journal
/// line its hold writes, no alert and no badge (`DESIGN-continuous-forward-2026-11-01.md`).
pub const COMMAND_STANDS: &str = "stands unsent in the prompt: aterm's Enter sends no command it \
     did not write, so it is their hold until they send or clear it";

/// Escalation reason while aterm's own words stand unsent in the prompt ([`Composer::Own`]).
pub const OWN_WORDS_STAND: &str =
    "aterm's own words stand unsent in the prompt; clear them and the session goes on";

/// Escalation reason while a draft stands over Claude Code's open completion menu ([`Diversion::Menu`]).
pub const MENU_DIVERTS: &str = "a draft stands with Claude Code's completion menu open: Enter \
     may run or complete the menu's highlighted entry, not send the draft; send or clear it \
     and the session goes on";

/// Escalation reason while the prompt-history search stands over the composer ([`Diversion::Search`]).
pub const SEARCH_DIVERTS: &str = "the prompt-history search (ctrl+r) is open over the composer: \
     Enter would send the old prompt it found and typing would go into its query, and leaving it \
     with Esc keeps that prompt in the composer; send, edit or clear what it leaves, or leave a \
     search that found nothing, and the session goes on";

/// Escalation reason while an old prompt the search left stands unedited ([`Diversion::Recall`]).
pub const RECALL_STANDS: &str = "an old prompt the prompt-history search (ctrl+r) found stands \
     unedited in the composer, where leaving the search with Esc keeps it: aterm sends no prompt \
     it did not see typed; send, edit or clear it and the session goes on";

/// Suffix of an escalated question's reason when `answer_questions` holds it (`<why>; answer_questions is off`).
pub const ANSWERS_OFF: &str = "answer_questions is off";

/// What the loop read of the composer beyond its rows, for [`TurnEndReading::of`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerRead {
    pub typed: Option<bool>,
    pub menu_fg: Option<Vec<Option<String>>>,
}

/// USAGE CREDITS CARRY THE SESSION, by Claude Code's own notice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditsCarry {
    pub reset_at: Option<Instant>,
    pub since: Option<Instant>,
    pub model: Option<String>,
    pub kept: bool,
}

/// The word of a model's family a credits notice names ([`CreditsCarry::model`]).
#[must_use]
pub fn credits_model(name: &str) -> Option<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .map(str::to_lowercase)
}

impl From<Option<bool>> for ComposerRead {
    fn from(typed: Option<bool>) -> Self {
        Self {
            typed,
            menu_fg: None,
        }
    }
}

impl From<bool> for ComposerRead {
    fn from(typed: bool) -> Self {
        Some(typed).into()
    }
}

/// The conversation evidence of a real supervising context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conversation {
    Hostless,
    Unknown,
    Named(String),
}

impl Conversation {
    pub(crate) fn fresh_after(&self, before: &Self) -> Self {
        match (self, before) {
            (Self::Hostless, Self::Unknown | Self::Named(_)) => Self::Unknown,
            _ => self.clone(),
        }
    }

    pub(crate) const fn unread(host: bool, program: Program) -> Self {
        if host && !screen_alone(program) {
            Self::Unknown
        } else {
            Self::Hostless
        }
    }
}

#[must_use]
pub(crate) const fn reads_conversation(program: Program) -> bool {
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

#[must_use]
pub(crate) const fn screen_alone(program: Program) -> bool {
    match program {
        Program::Agy | Program::Muse => true,
        Program::Claude | Program::Codex | Program::Grok | Program::Kimi | Program::Generic => {
            false
        }
    }
}

#[must_use]
pub(crate) fn met_under_another(met: Option<Program>, here: Program) -> bool {
    let decided = |p: Program| reads_conversation(p) || screen_alone(p);
    met.is_some_and(|met| met != here && decided(met) && decided(here))
}

/// THE SEAT A SCREEN NAMES: the model and the login a screen-alone agent
/// draws ([`screen_alone`]) — agy's footer model and its launch card's login
/// row (`Gemini API key`, or the signed-in account).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Seat {
    pub model: Option<String>,
    pub login: Option<String>,
}

impl Seat {
    /// The seat `program`'s screen `rows` name.
    #[must_use]
    pub fn of(program: Program, rows: &[String]) -> Self {
        match program {
            Program::Agy => Self {
                model: aterm_phase::agy::launch_card(rows).map(|c| c.model),
                login: aterm_phase::agy::launch_card_login(rows).map(str::to_owned),
            },
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Muse
            | Program::Generic => Self::default(),
        }
    }

    /// The fields `later` names, each over this one's.
    fn update(&mut self, later: &Self) {
        if later.model.is_some() {
            self.model.clone_from(&later.model);
        }
        if later.login.is_some() {
            self.login.clone_from(&later.login);
        }
    }

    /// This seat with the fields `later` names over it.
    fn merged(&self, later: &Self) -> Self {
        let mut seat = self.clone();
        seat.update(later);
        seat
    }

    /// WHETHER `now` IS THIS SEAT'S ACCOUNT BY THE SCREEN'S OWN WORD: both
    /// name the login, alike, and it names an account — an address
    /// (`user@example.com (Google AI Pro)`, `Signed in with Google:
    /// user@example.com`) — never a key's label (`Gemini API key`), which
    /// every key draws alike: a relaunch under another key draws the same
    /// words.
    fn same_account(&self, now: &Self) -> bool {
        matches!((&self.login, &now.login), (Some(a), Some(b)) if a == b && a.contains('@'))
    }

    /// Whether `now` names another login than this seat, or — where the
    /// limit is a model's (`per_model`) — another model: both named, and
    /// different.
    fn moved(&self, now: &Self, per_model: bool) -> bool {
        let differs =
            |a: &Option<String>, b: &Option<String>| matches!((a, b), (Some(a), Some(b)) if a != b);
        differs(&self.login, &now.login) || (per_model && differs(&self.model, &now.model))
    }
}

/// WHETHER A LIMIT ON `program`'s SCREEN IS A MODEL'S: agy's (`You have
/// exhausted your quota on this model.`, its 429's `QuotaFailure` per
/// model).
const fn limit_per_model(program: Program) -> bool {
    match program {
        Program::Agy => true,
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => false,
    }
}

/// What one screen says, as the turn-end policy reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndReading {
    pub phase: Phase,
    pub authoritative: bool,
    pub survey: bool,
    pub wall: Option<WallKind>,
    pub wall_message: String,
    pub reset_at: Option<Instant>,
    pub reset_clock: Option<ResetClock>,
    pub jitter: Duration,
    pub resumes_by_itself: bool,
    pub composer: Composer,
    pub said_tail: Option<String>,
    pub said_cut: bool,
    pub opens_cut: bool,
    pub turn_said: Option<String>,
    /// [`Self::said_tail`], each line the program wrapped across rows read as one
    /// ([`aterm_phase::reader::ScreenReader::said_lines`]): the words the needs fold reads.
    pub said_lines: Option<String>,
    /// [`Self::turn_said`], read as [`Self::said_lines`] is.
    pub turn_lines: Option<String>,
    /// Where each of [`Self::turn_said`]'s messages after the first opens, in it and in
    /// [`Self::turn_lines`] ([`needs::Breaks`]): the readers of the turn's needs read a none in
    /// an earlier message for that message alone ([`needs::standing_before`]).
    pub turn_breaks: needs::Breaks,
    pub opened_by: Option<u64>,
    pub message_drawn: bool,
    pub submits: Option<u64>,
    pub inputs_before: Option<u64>,
    pub screen_mark: Option<u64>,
    pub worked: Option<Duration>,
    pub worked_doubt: Duration,
    /// The turn ran a tool ([`aterm_phase::reader::ScreenReader::turn_ran_tools`]), read on the whole turn, or the history to its start, where it opens above the screen, or may have, neither read: work, however short ([`TurnEndState::repeats`]).
    pub ran_tools: bool,
    /// [`Self::ran_tools`] only as may have: the turn opens above the screen and neither its whole nor the history to its start was read.
    pub tools_unread: bool,
    pub rules: Option<String>,
    /// The `aterm:` fact the act typed here may carry ([`FACT_RIDES`],
    /// [`crate::supervise::facts`]): the session's untold one, filled by the loop only at the
    /// final pass of an act it rides, with `[harness] facts` on; `None` everywhere else.
    pub fact: Option<crate::supervise::facts::Fact>,
    /// The declared wake's fact the act typed here may carry where [`Self::fact`] carries none
    /// (the slice-2 plan's D3: the one fact slot): filled by the loop at the same final pass,
    /// under the owner's `prompt` alone ([`RideFact::new`]); `None` everywhere else.
    pub wake: Option<RideFact>,
    pub pending_input: bool,
    pub interrupted: bool,
    /// The program's own notice that it stopped the turn, where only a person clears what it
    /// names ([`aterm_phase::reader::ScreenReader::stopped_turn`]): the point is handed over.
    pub fault: Option<String>,
    pub restartable: bool,
    pub relaunch_off: bool,
    pub resume: Option<String>,
    pub upgrading: bool,
    pub taskless: bool,
    pub finished_record: Option<bool>,
    pub left_record: bool,
    pub left_work: Option<Duration>,
    pub login_back: bool,
    pub auto_continue: Option<aterm_phase::AutoContinue>,
    pub since_limit: Option<aterm_phase::SinceLimit>,
    pub served_in_transcript: bool,
    pub chosen_in_transcript: Option<String>,
    pub conversation: Conversation,
    pub notice_elsewhere: bool,
    pub wall_remembered: bool,
    pub reach: Reach,
    /// THE WITNESS ON THE SESSION'S ROUTE (v6 §14.17.5.4, slice 12c): when another session of
    /// this aterm on the same route (the same program and the endpoint its launch names,
    /// [`crate::harness::witness::RouteKey`]) last finished a turn with a real reply, read by the
    /// loop only at an outage wall (`IdleHost::witness`); `None` elsewhere, with no route named,
    /// or no such turn. One newer than the wall's last try is the outage's clearing, seen
    /// ([`outage_wait`]).
    pub witness: Option<Instant>,
    pub person: Option<Duration>,
    /// How long before this read the person's last measured key came: the window's
    /// `human_typed_ms` (`status`'s or the screen's: their typing, never a scroll or a click,
    /// counted through the Mac's sleep; `human_ms` from a window that reports none) or another
    /// driver's hand a `status` read saw, kept after it lifts (a fleet member's `turn` names no
    /// driver, as an owner's own does not, so it is none), never a draft the loop saw in the
    /// composer, which may be the harness's own words before their Enter. It alone holds the
    /// go-ahead ([`TurnEndState::person_keyed`]).
    pub key: Option<Duration>,
    pub program: Program,
    /// The model and the login the screen names, where it is all the
    /// evidence ([`Seat::of`]): what a limit met under one of them binds.
    pub seat: Seat,
    pub account: Option<AccountAdvice>,
    /// THE ROUTE MOVE THE HOST OFFERS (`IdleHost::route_move`, §5.5 of
    /// `docs/DESIGN-claude-setup-token-2026-11-05.md`): [`RouteTo::Token`] where the session runs
    /// on its row's stored login and its account's vault token is in use on this Mac,
    /// [`RouteTo::Stored`] where it runs on a token this Mac saw refused, or that was removed,
    /// ended or mismatched, and the row has a stored login here. Read at the points the host's
    /// account advice is read ([`Self::account`]); `None` everywhere else, and for every program
    /// but Claude Code.
    pub route: Option<RouteTo>,
    pub autopilot: Autopilot,
    /// Whether a person set the session's word ([`Self::autopilot`]) where it stands: `meta`'s
    /// `autopilot_by=human:…`, a press on the chip, the Autopilot menu, the composer border or
    /// Settings ▸ Harness. Only the person's own switch to Land over a held tab asks once (v6
    /// §14.17 item 3); a lead's `aterm ctl` write, the timer's fire or the harness's own keeps
    /// limit 1.
    pub autopilot_by_person: bool,
    pub login_label: Option<String>,
    pub login_superseded: bool,
    pub login_notice: Option<u32>,
    pub login_screen: Option<aterm_phase::LoginScreen>,
    pub login_interrupted: bool,
    pub login_at_wall: Option<aterm_phase::LoginAtWall>,
    pub login_method: Option<aterm_phase::LoginMethod>,
    pub login_shared: Option<super::login::LoginShared>,
    /// WHAT CLAUDE CODE'S SETTINGS MAKE OF `/login` here (`IdleHost::login_pin`, read at a lost
    /// login before aterm types it): a pinned sign-in method opens the browser at once, so
    /// `/login` is then the person's to type ([`super::login::LoginPin::leaves_login`]).
    pub login_pin: super::login::LoginPin,
    /// THE LOGIN MOVED OFF THE LIMIT'S ACCOUNT: the session's Claude Code
    /// profile names another account than the one its standing limit was
    /// read under (`IdleHost::login_account`), on the one reading that
    /// first sees it ([`TurnEndState::login_resume`]).
    pub login_moved: bool,
    pub roster: Option<RosterWord>,
    pub answered: Option<ScreenAnswer>,
    pub record_turn: Option<RecordTurn>,
    pub credits: Option<CreditsCarry>,
    pub repo: crate::harness::repo_work::RepoRead,
    /// THE PROCESSES THE SESSION'S AGENT STARTED THAT STILL RUN, as the host read and re-proved
    /// them at this yield (`IdleHost::leftover`): read by the loop only where it reads
    /// [`Self::repo`] (a done claim, a finished record, an awaited land check, a Land go-ahead's
    /// park), fresh at each, and not asked anywhere else, nor by a tape recorded without the door.
    /// Under Land an unkept row holds the landing ([`HoldLeg::Left`]); a view that cannot say holds
    /// nothing.
    pub leftover: crate::harness::leftover::LeftRead,
}

/// THE TURN A SCREEN'S LAST WORDS CLOSE ([`TurnEndReading::answered`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenAnswer {
    pub prompt: u64,
    pub work: Option<Duration>,
    /// The prompt opens with [`crate::harness::upgrade::HARNESS_MARK`], as every turn the harness
    /// types does (a restart prompt, an upgrade's notice): read where the words of a turn the host
    /// typed are not this policy's to compare ([`TurnEndState::host_typed`]).
    pub harness: bool,
}

impl ScreenAnswer {
    /// The turn Claude Code's `rows` close ([`aterm_phase::answered_prompt`]).
    #[must_use]
    pub fn of(rows: &[String]) -> Option<Self> {
        aterm_phase::answered_prompt(rows).map(|said| Self {
            prompt: crate::supervise::approvals::words_hash(&said),
            work: done_row_work(rows),
            harness: said.starts_with(crate::harness::upgrade::HARNESS_MARK),
        })
    }
}

/// THE TURN THE CONVERSATION'S RECORD ENDS ON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordTurn {
    pub prompt: u64,
    pub age: Duration,
    pub ended: Option<Duration>,
}

impl RecordTurn {
    /// The record's last turn `t`, read at `now_ms` (unix milliseconds).
    #[must_use]
    pub fn of(t: &crate::harness::upgrade::LastTurn, now_ms: i64) -> Option<Self> {
        let at = i64::try_from(t.opened_ms?).ok()?;
        Some(Self {
            prompt: t.opened_said?,
            age: Duration::from_millis(now_ms.saturating_sub(at).max(0).unsigned_abs()),
            ended: t.ended,
        })
    }
}

/// How much earlier than the act's own stamp the record may date the prompt that act typed ([`TurnEndState::observe`]'s fast answer).
pub const RECORD_SLACK: Duration = Duration::from_secs(5);

/// WHETHER `phase` IS A TURN'S END: no turn running and no box (idle, a question, a limit's
/// notice), the phases a point is decided at ([`TurnEndReading`]'s point) and so the phases the
/// policy writes a line at, and on which a line held is judged at rest
/// (`custody::Rest::turn_ended`, `Session::custody_at_rest`).
#[must_use]
pub(crate) const fn turn_ended(phase: &Phase) -> bool {
    matches!(phase, Phase::Idle | Phase::Question | Phase::Limited { .. })
}

impl TurnEndReading {
    /// The advice the decider acts on ([`AccountAdvice::past_probe`] of [`Self::account`]), read the same at every deciding site.
    #[must_use]
    pub fn advised(&self) -> Option<&AccountAdvice> {
        self.account.as_ref().and_then(AccountAdvice::past_probe)
    }

    /// A person's hand in the turn that ended here.
    #[must_use]
    pub fn person_in_turn(&self) -> bool {
        self.in_turn(self.person)
    }

    /// Whether a person's input `ago` before this read came in the turn that ended here: within
    /// its work and [`PERSON_TURN_SLACK`].
    fn in_turn(&self, ago: Option<Duration>) -> bool {
        ago.is_some_and(|ago| {
            ago <= self
                .worked
                .unwrap_or_default()
                .saturating_add(self.worked_doubt)
                .saturating_add(PERSON_TURN_SLACK)
        })
    }

    /// THE SCREEN SHOWS A STANDING WALL.
    #[must_use]
    pub const fn wall_drawn(&self) -> bool {
        self.wall.is_some() && !(self.wall_remembered && self.since_limit.is_none())
    }

    /// NOTHING OF A CONVERSATION STANDS.
    #[must_use]
    pub fn holds_no_conversation(&self) -> bool {
        self.phase == Phase::Idle
            && !self.message_drawn
            && self.worked.is_none()
            && self.said_tail.is_none()
            && self.turn_said.is_none()
            && self.opened_by.is_none()
            && self.answered.is_none()
            && !self.pending_input
            && !self.composer.drafted()
            && self.composer != Composer::Unknown
            && self.wall.is_none()
            && self.login_screen.is_none()
            && !self.interrupted
    }

    /// THE WORKER'S WORDS SINCE A USAGE LIMIT, where the screen orders them after it ([`limit_end`]).
    #[must_use]
    pub fn said_since_limit(&self) -> Option<&str> {
        let claude = match self.program {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
        };
        let after = self
            .since_limit
            .as_ref()
            .is_none_or(|s| s.served || s.fired);
        if !claude || self.wall_drawn() || self.pending_input || self.interrupted || !after {
            return None;
        }
        self.said_tail.as_deref()
    }

    /// What the worker asks of a person here and whether it names an irreversible act ([`Said::Irreversible`]).
    #[must_use]
    pub fn asked(&self) -> Option<(String, bool)> {
        if self.interrupted {
            None
        } else if self.phase == Phase::Question && self.said_tail.is_none() {
            Some((
                "the worker asked something its last words do not show".to_string(),
                false,
            ))
        } else {
            asked_by(self.said_tail.as_deref())
        }
    }

    /// THE TURN'S EARLIER MESSAGES AS ITS LAST WORD LEAVES THEM for needs
    /// ([`needs::standing_before`]): [`Self::turn_said`] without the needs each message's own later
    /// none retracted, or none where the reply ([`Self::said_tail`]) says it. Every reader of the
    /// needs a turn named reads them here, beside the reply's own, but the go-ahead reader, which
    /// reads the same rule's cut ([`needs::retracted_cut`]) for its prose (the post-landing review,
    /// 2026-11-01).
    #[must_use]
    pub fn needs_before(&self) -> Option<std::borrow::Cow<'_, str>> {
        needs::standing_before(self.before(), self.said_tail.as_deref().unwrap_or_default())
    }

    /// Whether [`Self::needs_before`] names a need.
    #[must_use]
    pub fn need_before(&self) -> bool {
        self.needs_before()
            .is_some_and(|t| !needs::needs_you(&t).is_empty())
    }

    /// [`Self::turn_said`], its messages told apart ([`Self::turn_breaks`]).
    #[must_use]
    pub fn before(&self) -> Option<needs::Before<'_>> {
        self.turn_said
            .as_deref()
            .map(|t| needs::Before::new(t, &self.turn_breaks.said))
    }

    /// THE WORDS OF THE TURN THE NEEDS FOLD READS ([`TurnEndState::observe`]), `(before, tail)`:
    /// its earlier messages ([`Self::turn_lines`] where they hold the same words as
    /// [`Self::turn_said`], else [`Self::before`]) and its last ([`Self::said_lines`] where they
    /// hold the same words as [`Self::said_tail`], else the tail), each line the program wrapped
    /// read as one ([`needs_words`]). The go-ahead's park reads these words too
    /// ([`Self::go_ahead_past`]), so every line it answers is a line the fold stands, never a row
    /// of one (DESIGN-harness-autopilot §15.6).
    #[must_use]
    pub fn needs_said(&self) -> (Option<needs::Before<'_>>, Option<&str>) {
        needs_words(
            (self.said_tail.as_deref(), self.turn_said.as_deref()),
            (self.said_lines.as_deref(), self.turn_lines.as_deref()),
            &self.turn_breaks,
        )
    }

    /// WHETHER A NEED THE TURN NAMED STANDS: the reply's own, or one its earlier messages named
    /// that the reply did not retract ([`Self::need_before`]).
    #[must_use]
    pub fn names_need(&self) -> bool {
        self.need_before()
            || self
                .said_tail
                .as_deref()
                .is_some_and(|t| !needs::needs_you(t).is_empty())
    }

    /// THE GO-AHEAD THE REPLY PARKS ON ([`go_ahead::parked`]): the turn's earlier messages and its
    /// last, as the needs fold reads them ([`Self::needs_said`]), a need the turn's last word
    /// retracted cut ([`needs::retracted_cut`]); `None` where the turn was interrupted.
    #[must_use]
    pub fn go_ahead(&self) -> Option<go_ahead::Park> {
        self.go_ahead_past(None)
    }

    /// [`Self::go_ahead`] past the rows of `act`, the act the turn opens with where it does.
    #[must_use]
    pub fn go_ahead_past(&self, act: Option<&str>) -> Option<go_ahead::Park> {
        if self.interrupted {
            return None;
        }
        let (before, tail) = self.needs_said();
        turn_go_ahead(before, tail.unwrap_or_default(), act)
    }

    /// Reads one screen.
    #[must_use]
    pub fn of(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: impl Into<ComposerRead>,
        worked: Option<Duration>,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        Self::of_slept(
            reading,
            rows,
            typed,
            worked,
            InTurn::default(),
            reset_at,
            rules,
        )
    }

    /// [`Self::of`] where the machine slept `asleep` inside the turn the screen closes.
    #[must_use]
    pub fn of_slept(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: impl Into<ComposerRead>,
        worked: Option<Duration>,
        asleep: InTurn,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
        };
        let own = reading
            .composer
            .as_ref()
            .is_some_and(|(_, lines)| crate::harness::upgrade::own_draft(&lines.concat()));
        let read = typed.into();
        let over_menu = reading.composer.as_ref().and_then(|(_, lines)| {
            let menu = claude
                .then(|| aterm_phase::phase::completion_menu(rows))
                .flatten()?;
            let fg = read.menu_fg.as_deref();
            let runs = menu.runs(fg);
            let command = lines.first().and_then(|l| l.split_whitespace().next());
            let sends = |enter: &aterm_phase::phase::MenuEnter<'_>| match *enter {
                aterm_phase::phase::MenuEnter::Run(entry) => Some(entry) == command,
                aterm_phase::phase::MenuEnter::Complete(_) => false,
                aterm_phase::phase::MenuEnter::Draft => true,
            };
            let selection_read = !menu.colour_only()
                || fg.is_some_and(|fg| {
                    fg.len() == menu.entries.len() && fg.iter().all(Option::is_some)
                });
            if runs.iter().all(sends) {
                None
            } else if selection_read || !runs.iter().any(sends) {
                Some(Composer::Diverted(Diversion::Menu))
            } else {
                Some(Composer::Unknown)
            }
        });
        // A DRAFT ITS PROGRAM RUNS AS A COMMAND (v6 §14.17.5.8): by its own forms
        // ([`aterm_phase::draft_command`]), or under Claude Code's slash menu, whose Enter runs an
        // entry ([`aterm_phase::phase::MenuEnter::Run`]) whatever the draft says.
        let command = reading.composer.as_ref().and_then(|(_, lines)| {
            aterm_phase::draft_command(reading.program, lines).or_else(|| {
                claude
                    .then(|| aterm_phase::phase::completion_menu(rows))
                    .flatten()
                    .filter(|menu| {
                        menu.runs(read.menu_fg.as_deref())
                            .iter()
                            .any(|enter| matches!(enter, aterm_phase::phase::MenuEnter::Run(_)))
                    })
                    .map(|_| aterm_phase::DraftCommand::Slash)
            })
        });
        // A HISTORY SEARCH OVER THE COMPOSER: what its row shows (a match drawn as typed, nothing, a
        // suggestion) is no draft Enter would send, whatever its cell says.
        let searching = reading.program.reader().history_search(rows);
        let composer = match (reading.composer.as_ref(), read.typed) {
            (None, _) => Composer::Absent,
            (Some(_), _) if searching => Composer::Diverted(Diversion::Search),
            (Some(_), None) => Composer::Unknown,
            (Some(_), Some(true)) if own => Composer::Own,
            (Some(_), Some(true)) => match command {
                Some(form) => Composer::Command(form, CommandEnter::of(over_menu.as_ref())),
                None => over_menu.unwrap_or(Composer::Typed),
            },
            (Some(_), Some(false)) => match &reading.suggestion {
                Some(s) => Composer::Placeholder(s.clone()),
                None => Composer::Empty,
            },
        };
        let message = reading
            .wall
            .as_ref()
            .map(|w| w.message.clone())
            .unwrap_or_default();
        let auto_continue = claude
            .then(|| aterm_phase::auto_continue_end(rows))
            .flatten();
        let recorded = claude
            && reading.composer.is_some()
            && !matches!(
                auto_continue,
                Some(aterm_phase::AutoContinue::Armed | aterm_phase::AutoContinue::Fired)
            )
            && reading
                .wall
                .as_ref()
                .is_some_and(|w| w.placement == aterm_phase::Placement::ErrorRow);
        let done = claude
            .then(|| done_row_work(rows))
            .flatten()
            .unwrap_or_default();
        let less = |w: Duration, asleep: Duration| w.max(done.saturating_sub(asleep));
        let (turn_said, said_breaks) =
            needs::joined(reading.program.reader().turn_messages_before(rows));
        let (turn_lines, lines_breaks) = needs::joined(
            reading
                .program
                .reader()
                .turn_message_lines_before(rows, needs::apart),
        );
        Self {
            phase: reading.phase.clone(),
            authoritative: reading.phase_authoritative,
            survey: reading.survey,
            wall: reading.wall.as_ref().map(|w| w.kind),
            resumes_by_itself: super::super::limit::resumes_by_itself(&message)
                && auto_continue != Some(aterm_phase::AutoContinue::Unkept)
                && !recorded,
            wall_message: message,
            reset_at,
            reset_clock: reading
                .wall
                .as_ref()
                .and_then(|w| ResetClock::of(w.reset.as_deref())),
            jitter: Duration::ZERO,
            composer,
            said_tail: reading.said_tail.clone(),
            said_cut: reading.said_tail.is_some() && !reading.program.reader().said_whole(rows),
            opens_cut: reading.said_tail.is_some()
                && !reading.program.reader().bounds_messages()
                && !reading.program.reader().turn_whole(rows),
            turn_said,
            said_lines: reading.program.reader().said_lines(rows, needs::apart),
            turn_lines,
            turn_breaks: needs::Breaks {
                said: said_breaks,
                lines: lines_breaks,
            },
            opened_by: reading
                .program
                .reader()
                .turn_opened_by(rows)
                .map(|t| crate::supervise::approvals::words_lead(&t)),
            message_drawn: reading.program.reader().message_drawn(rows),
            submits: None,
            inputs_before: None,
            screen_mark: Some(conversation_mark(reading, rows)),
            worked: worked.map(|w| less(w, asleep.most)),
            worked_doubt: worked.map_or(Duration::ZERO, |w| {
                less(w, asleep.least).saturating_sub(less(w, asleep.most))
            }),
            ran_tools: reading.program.reader().turn_ran_tools(rows),
            tools_unread: false,
            rules,
            fact: None,
            wake: None,
            pending_input: claude
                && last_said_row(rows).is_some_and(|r| r.trim_start().starts_with('❯')),
            interrupted: reading.interrupted,
            fault: reading.program.reader().stopped_turn(rows),
            restartable: false,
            relaunch_off: false,
            resume: None,
            upgrading: false,
            taskless: reading.fresh,
            finished_record: None,
            left_record: false,
            left_work: None,
            person: None,
            key: None,
            login_back: claude && aterm_phase::login_restored(rows),
            auto_continue,
            since_limit: claude.then(|| aterm_phase::since_limit(rows)).flatten(),
            served_in_transcript: false,
            chosen_in_transcript: None,
            conversation: Conversation::Hostless,
            notice_elsewhere: false,
            wall_remembered: false,
            reach: Reach::Unknown,
            witness: None,
            program: reading.program,
            seat: Seat::of(reading.program, rows),
            account: None,
            route: None,
            autopilot: Autopilot::Cruise,
            autopilot_by_person: false,
            login_label: None,
            login_superseded: false,
            login_notice: claude
                .then(|| aterm_phase::login_expires_in(rows))
                .flatten(),
            login_screen: claude.then(|| aterm_phase::login_screen(rows)).flatten(),
            login_interrupted: claude && aterm_phase::login_interrupted(rows),
            login_at_wall: claude.then(|| aterm_phase::login_at_wall(rows)).flatten(),
            login_method: None,
            login_shared: None,
            login_pin: super::login::LoginPin::Picker,
            login_moved: false,
            roster: None,
            answered: claude.then(|| ScreenAnswer::of(rows)).flatten(),
            record_turn: None,
            credits: reading.credits.as_ref().map(|c| CreditsCarry {
                model: c.model.as_deref().and_then(credits_model),
                ..CreditsCarry::default()
            }),
            repo: crate::harness::repo_work::RepoRead::Clean,
            leftover: crate::harness::leftover::LeftRead::NotAsked,
        }
    }

    const fn at_point(&self, stale_input: bool) -> bool {
        turn_ended(&self.phase)
            && self.authoritative
            && !self.survey
            && !matches!(self.composer, Composer::Absent | Composer::Unknown)
            && (!self.pending_input || stale_input)
    }

    /// A LOST LOGIN DRAWN WITH NO COMPOSER, read idle and authoritative, and no `/login` dialog:
    /// agy's or Muse Code's sign-in menu in place of the composer. No point
    /// ([`Self::at_point`]): nothing is typed into it. Claude Code's measured `/login` dialog
    /// (its browser hand-off, error and success states) has no composer either; it is
    /// [`super::login::dialog_act`]'s, and no sign-in menu. Claude Code reads so only where its
    /// lost login's row stands with no composer frame drawn under it (its launch card and the
    /// row, say), which no measured screen of it shows.
    const fn at_sign_in_menu(&self) -> bool {
        matches!(self.wall, Some(WallKind::Auth))
            && matches!(self.composer, Composer::Absent)
            && matches!(self.phase, Phase::Idle)
            && self.authoritative
            && self.login_screen.is_none()
    }

    /// THE PROGRAM'S OWN STOP NOTICE an idle point with a task ends on ([`Self::fault`]): the
    /// answer to whatever turn was asked, an act of this policy's included, and no point.
    fn stopped_turn(&self) -> Option<&str> {
        self.fault
            .as_deref()
            .filter(|_| self.phase == Phase::Idle && !self.taskless)
    }

    /// Whether `rows`, read as `program` draws them (`None`: as the screen names its program),
    /// are a sign-in menu as the supervisor reads one (`Self::at_sign_in_menu`): the screen an
    /// account move's relaunch is made over in place of an empty composer
    /// (`crate::harness::relaunch`), since nothing typed stands there to lose.
    #[must_use]
    pub fn sign_in_menu_drawn(
        program: Option<&str>,
        rows: &[String],
        cursor_col: Option<usize>,
    ) -> bool {
        let read = aterm_phase::read(program, rows, cursor_col);
        Self::of(&read, rows, false, None, None, None).at_sign_in_menu()
    }

    const fn answered_point(&self) -> bool {
        matches!(self.phase, Phase::Idle | Phase::Question)
            && self.authoritative
            && !self.survey
            && self.wall.is_none()
            && !self.pending_input
            && !self.interrupted
            && matches!(self.composer, Composer::Empty | Composer::Placeholder(_))
    }

    /// AN ANSWERED POINT THAT SAYS NOTHING ([`Self::answered_point`]'s shape, a conversation
    /// drawn, no stop notice, no wall or limit notice drawn): no reply read, or one of whitespace
    /// alone, none of it cut above the screen. A land check answered so has no audit to read
    /// ([`TurnEndState::observe`]); one drawn at an idle composer may still draw its words
    /// ([`Self::words_may_follow`]).
    fn unsaid(&self) -> bool {
        self.answered_point()
            && self.fault.is_none()
            && !self.wall_drawn()
            && self.since_limit.is_none()
            && !self.taskless
            && !self.said_cut
            && self
                .said_tail
                .as_deref()
                .is_none_or(|t| t.trim().is_empty())
    }

    /// [`Self::unsaid`] AT AN IDLE COMPOSER: the frame agy 1.5.1 on a loaded Mac drew under an
    /// act's message before the words it then drew (L19), so a Land act's answer read so has not
    /// answered it yet ([`TurnEndState::unsaid_reading`]). A question box is drawn on purpose,
    /// never ahead of its words, so one that says nothing is judged where it is read.
    fn words_may_follow(&self) -> bool {
        self.phase == Phase::Idle && self.unsaid()
    }
}

/// THE HARNESS'S OWN ESC ([`TurnEndState::woken_esc`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WokenEsc {
    /// The turn it stopped was the vendor's continuation after a usage limit.
    resume: bool,
}

/// The turn running now, as the loop's busy reads track it (the busy-read twin of [`TurnEndReading::person_in_turn`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningTurn {
    since: Option<Instant>,
}

impl RunningTurn {
    /// A busy read of the worker's own turn at `now`.
    pub fn busy(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// The point ending the turn, at `now`.
    pub fn point(&mut self, now: Instant) -> Option<Duration> {
        self.since.take().map(|t| now.saturating_duration_since(t))
    }

    /// Whether a busy read showed the worker's turn running since its last point.
    #[must_use]
    pub const fn runs(&self) -> bool {
        self.since.is_some()
    }
}

/// What a typed command leaves owed for the next point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    Nothing,
    Continue,
}

/// WHAT KIND OF WAIT A [`TurnEndAction::WaitUntil`] IS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitKind {
    Retry,
    BackOff {
        short: u8,
    },
    /// An outage wall waited on (v6 §14.17.5.2, slice 12b), or a measured outage at a point no
    /// wall stands at ([`outage_hold`]): nothing is typed while it stands, and the window draws
    /// `waiting: <words>` on the tab's state line. `tries`: a try comes at the wait's end (an
    /// outage wall's blind line, or the act a wall-less point's hold held); false while the host
    /// measures an outage wall's route down, when only its edge up resumes.
    Outage {
        wall: Outage,
        tries: bool,
    },
    Other,
}

/// WHICH OUTAGE AN [`WaitKind::Outage`] WAITS ON: the condition its state line and its resume name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outage {
    /// The API is overloaded (`Overloaded`).
    Overload,
    /// The API answers with a server error (`ApiError` cause `Server`, but not one with no status
    /// while the route is measured down).
    ApiError,
    /// The API cannot be reached (`ApiError` cause `Unreachable` or `Config`, or a `CutOff` or a
    /// code-less `Server` one while the route is measured down).
    Connection,
}

impl Outage {
    /// The condition in words, as the state line and the resume line say it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Outage::Overload => "API overload",
            Outage::ApiError => "API error",
            Outage::Connection => "connection error",
        }
    }

    /// The condition's one-word name, as `aterm ctl status` says it (`waiting=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Outage::Overload => "overload",
            Outage::ApiError => "api-error",
            Outage::Connection => "connection",
        }
    }
}

impl WaitKind {
    /// Whether the loop tells the host of a wait of this kind.
    #[must_use]
    pub const fn told(self) -> bool {
        !matches!(self, WaitKind::Other)
    }
}

/// What [`decide_turn_end`] says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndAction {
    Nothing,
    WaitUntil {
        until: Instant,
        why: String,
        wait: WaitKind,
    },
    Type {
        text: String,
        rule_id: &'static str,
    },
    TypeCommand {
        command: String,
        rule_id: &'static str,
        then: Then,
    },
    Submit {
        rule_id: &'static str,
    },
    Confirm {
        rule_id: &'static str,
        row: String,
    },
    /// A fenced Esc at the dialog's own `Esc to cancel` row, out of a dialog the loop opened
    /// ([`super::login::backs_out`]).
    Cancel {
        rule_id: &'static str,
    },
    Escalate {
        reason: String,
    },
    Restart {
        why: Restart,
        rule_id: &'static str,
        otherwise: Box<TurnEndAction>,
    },
}

impl TurnEndAction {
    /// The rule an act is taken under (`None` for no act).
    #[must_use]
    pub const fn rule_id(&self) -> Option<&'static str> {
        match self {
            TurnEndAction::Type { rule_id, .. }
            | TurnEndAction::TypeCommand { rule_id, .. }
            | TurnEndAction::Submit { rule_id }
            | TurnEndAction::Confirm { rule_id, .. }
            | TurnEndAction::Cancel { rule_id }
            | TurnEndAction::Restart { rule_id, .. } => Some(rule_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallClass {
    Retry,
    Usage,
    Model,
    Context,
    Auth,
    Memory,
}

const fn class_of(kind: WallKind) -> WallClass {
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => WallClass::Retry,
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => WallClass::Usage,
        WallKind::ModelBucket { .. } => WallClass::Model,
        WallKind::Context => WallClass::Context,
        WallKind::Auth => WallClass::Auth,
        WallKind::Memory => WallClass::Memory,
    }
}

const fn named_class(class: WallClass) -> bool {
    matches!(class, WallClass::Usage | WallClass::Model)
}

/// THE ACTS OF A LIMIT'S PLACE, the rule half of [`TurnEndState::in_limits_place`]: the done
/// check, the go-ahead, an answer and a land act, each of which a usage window's or a model
/// bucket's standing track turns into the limit's own try. The one list aterm's loop and a proven
/// self-claim's fact read (`harness fact`'s `why=limit`) both judge by.
#[must_use]
pub fn limits_place_rule(rule: &str) -> bool {
    rule == RULE_DONE_CHECK || rule == RULE_GO_AHEAD || is_answer_rule(rule) || is_land_rule(rule)
}

/// Whether `t` is a limit a usage tap's window times (a Claude Code session or weekly limit) whose
/// reset no login dropped since its notice was read ([`WallTrack::reset_dropped`]).
fn tapped_limit(t: &WallTrack) -> bool {
    let tapped = match t.program {
        Some(Program::Claude) => true,
        Some(
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic,
        )
        | None => false,
    };
    tapped
        && !t.reset_dropped
        && matches!(t.kind, Some(WallKind::UsageSession | WallKind::UsageWeekly))
}

/// THE LIMIT'S TRY [`TurnEndState::acted`] last recorded ([`TurnEndState::limit_try`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitTry {
    pub bucket: bool,
    pub in_place: bool,
    pub attempts: usize,
    pub entitlement: bool,
}

/// HOW FAR APART A LIMIT'S TRY AND ITS ANSWER STAND, at most, where the limit answered it at once ([`TurnEndState::date_unread_try`]).
pub const TRY_ANSWER_SLACK: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnreadTry {
    at: Instant,
    limit: LimitTry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WallTrack {
    class: WallClass,
    since: Instant,
    attempts: usize,
    ups: usize,
    exhausted: bool,
    fill: Option<ContextFill>,
    named: Option<NamedReset>,
    pointed: bool,
    said: String,
    kind: Option<WallKind>,
    seeded: bool,
    entitlement: bool,
    owed: bool,
    /// The wall was read while the host measured the route down: a cut-off reply's, or a
    /// code-less server error's, is the connection's outage from then until the agent works again
    /// ([`outage_of`]), so the route's edge up resumes it as measured, never as a one-off cut or
    /// blind.
    down: bool,
    /// A resume was typed here on a witness (v6 §14.17.5.4, [`witness_clears`]): another
    /// session's turn on the route went through, and the line went into this wall. Its later
    /// witnesses clear nothing until the agent works again, and the ladder tries it, so a route-mate
    /// whose turns go through where this one's do not (a partial outage) costs one line, never a
    /// line at each of its turns.
    witnessed: bool,
    login: LoginBooks,
    conversation: Conversation,
    program: Option<Program>,
    /// THE AGENT RELAUNCHED UNDER THE LIMIT ([`TurnEndState::relaunch_startup`]).
    relaunch: RelaunchBooks,
    /// The seat the limit was met under ([`Seat`]: the model and login its
    /// screen named where the wall was read).
    seat: Seat,
    /// A later screen named another seat ([`Seat::moved`]), or the agent
    /// was relaunched on a seat nothing vouches for
    /// ([`TurnEndState::relaunch_startup`]): the limit binds the session no
    /// longer — its reset dropped, the point going at once — and it holds no
    /// host's step ([`TurnEndState::limit_holds_host`]).
    seat_moved: bool,
    /// The limit's reset was dropped for a login that came back or moved off the limit's
    /// account since its notice was last read: a reset the usage tap read before is that
    /// login's, and names the limit no more ([`TurnEndState::tracks_tapped_limit`]) until its
    /// notice is read again.
    reset_dropped: bool,
}

/// WHAT A LIMIT'S TRACK KEEPS OF ITS AGENT'S RELAUNCH ([`WallTrack::relaunch`],
/// [`TurnEndState::relaunch_startup`]): a limit met under an agent whose
/// screen is all its evidence ([`screen_alone`]: Muse Code, agy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RelaunchBooks {
    /// The last point of the limit's own program read while the track
    /// stood; `None`: none since the track opened ([`WallTrack::since`]).
    read: Option<Instant>,
    /// A point of no agent's was read since (a shell's prompt: the agent
    /// left the tab, crashed or upgraded), as no host's start says.
    away: bool,
    /// THE RELAUNCH'S START-UP STANDS: the agent's process came back since
    /// the limit was last read and no turn has run since — the limit is the
    /// account's still, whatever the new process drew (a fresh banner with no
    /// task included: [`TurnEndReading::taskless`] ends no such track).
    startup: bool,
}

/// A `/login` dialog this loop reads standing, and aterm's Escs at it where it is a person's own
/// left over a usage limit ([`super::login::over_limit_act`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StandingLogin {
    /// When this loop first read the dialog, since no screen read none.
    seen: Instant,
    /// When aterm's newest Esc at it went in.
    backed_out: Option<Instant>,
    /// aterm's Escs at it.
    backs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LoginBooks {
    cancelled: bool,
    /// When the loop last pressed the dialog's own Esc to back out of the dialog of the `/login`
    /// it typed ([`TurnEndAction::Cancel`], [`super::login::backs_out`]).
    backed_out: Option<Instant>,
    /// How many of those Escs the loop pressed: each spaces the next one further
    /// ([`TurnEndState::login_closing`]).
    backs: u32,
    /// The dialog the loop backed out of was read gone since its Esc (no dialog drawn, the
    /// composer back): that `/login` was left ([`TurnEndState::login_typed_left`]), and a dialog
    /// drawn after it is a person's own.
    closed: bool,
    hand: bool,
    prompt_at: Option<Instant>,
    prompt: Option<u64>,
    typed_at: Option<Instant>,
    back_typed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arrival {
    hand: bool,
    seen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamedReset {
    kind: WallKind,
    at: Instant,
    jitter: Duration,
    by_itself: bool,
    said: Option<ResetClock>,
    first: Instant,
    written: Option<i64>,
}

impl NamedReset {
    fn served_under(&self, since: Option<&aterm_phase::SinceLimit>) -> bool {
        since.is_some_and(|s| s.served) && notice_of(since, self.kind, self.said)
    }

    fn continued_under(
        &self,
        since: Option<&aterm_phase::SinceLimit>,
        now: Option<Instant>,
    ) -> bool {
        self.said.is_none()
            || (notice_of(since, self.kind, self.said)
                && now.is_none_or(|now| later(now, FIRED_EARLY) >= self.at))
    }

    fn moved_off_under(&self, reading: &TurnEndReading, limited: &str) -> bool {
        let since = reading.since_limit.as_ref();
        let chosen = if notice_of(since, self.kind, self.said) {
            since
                .filter(|s| s.model_set)
                .and_then(|s| s.chosen_model.as_deref())
        } else if since.is_none() && self.said.is_some() {
            reading.chosen_in_transcript.as_deref()
        } else {
            None
        };
        chosen
            .and_then(bucket_model)
            .zip(bucket_model(limited))
            .is_some_and(|(chosen, limited)| chosen != limited)
    }
}

/// How long before a limit's reset.
pub const FIRED_EARLY: Duration = Duration::from_secs(60);

fn notice_of(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| {
        let notice = if s.generic && named_class(class_of(kind)) {
            kind
        } else {
            s.kind
        };
        names_reset(notice, s.reset.as_deref(), kind, said)
    })
}

fn names_reset(
    notice: WallKind,
    reset: Option<&str>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    class_of(notice) == class_of(kind) && said.is_some() && ResetClock::of(reset) == said
}

/// THE LIMIT THIS POLICY WAITS ON, by the reset its notice named ([`TurnEndState::waited_limit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitedLimit {
    kind: WallKind,
    pub at: Instant,
    said: Option<ResetClock>,
}

impl WaitedLimit {
    /// The limit's kind.
    #[must_use]
    pub const fn kind(&self) -> WallKind {
        self.kind
    }

    /// Whether a limit notice of `kind` whose words name the reset `reset` names THIS limit's reset: of its class, the same clock.
    #[must_use]
    pub fn named_by(&self, kind: WallKind, reset: Option<&str>) -> bool {
        names_reset(kind, reset, self.kind, self.said)
    }
}

/// Return whether the screen's newest limit notice ended under a turn or retry.
#[must_use]
pub fn ended_under(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| s.served || s.fired) && notice_of(since, kind, said)
}

const fn work_is_its_cause(kind: WallKind) -> bool {
    matches!(
        kind,
        WallKind::ApiError {
            cause: ApiCause::Overlong,
            ..
        }
    )
}

fn compaction_exhausted_row(message: &str) -> bool {
    let lower = message
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let anchor = |id: &str| aterm_phase::anchor(id).to_lowercase();
    lower.starts_with(&anchor("wall.compaction_failed"))
        || lower.starts_with(&anchor("wall.not_enough_to_compact"))
        || lower.contains(&anchor("wall.cannot_be_compacted"))
}

/// WHAT FILLS A FULL CONTEXT beyond the conversation's words, by the vendor's own notice (2.1.300).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFill {
    Images,
    Attachments,
}

impl ContextFill {
    /// The fill notice `message` names, by the anchor table's words.
    #[must_use]
    pub fn of(message: &str) -> Option<Self> {
        let lower = message.to_lowercase();
        let names = |id: &str| lower.contains(&aterm_phase::anchor(id).to_lowercase());
        if names("wall.compact_attachments") {
            Some(Self::Attachments)
        } else if names("wall.compact_images") {
            Some(Self::Images)
        } else {
            None
        }
    }
}

/// A model switch this policy made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitch {
    pub from: Option<String>,
    pub to: String,
    pub back_at: Option<Instant>,
    /// The agent process the switch is in ([`TurnEndState::agent_process`]); `None` until the
    /// host names one.
    pub proc: Option<SwitchProc>,
    /// The host names another process than [`Self::proc`] and no conversation of it yet: the
    /// switch's return waits until it does, as that process may be another program.
    pub unproven: bool,
}

/// The agent process a model switch is in, as its host names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchProc {
    pub pid: u32,
    /// When it started, unix seconds.
    pub start: i64,
    /// The conversation it holds, once the host names one.
    pub conversation: Option<String>,
}

/// What [`TurnEndState::agent_process`] did to the model switch in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchScope {
    /// The switch is in this process now: the first the host named, a relaunch that resumed its
    /// conversation, one the harness started or owes a continuation for, or a conversation its
    /// process opened (a `/clear`).
    Moved(SwitchProc),
    /// Another process on another conversation, which the harness did not start, runs in the tab
    /// (a program the person started): the switch is over.
    Closed,
}

/// How far past a turn's busy work a person's keystroke still counts as their hand in it.
pub const PERSON_TURN_SLACK: Duration = Duration::from_secs(30);

/// The longest answer that is still a person's own exchange ([`TurnEndState::left_to_person`]).
pub const EXCHANGE_WORK: Duration = Duration::from_secs(10 * 60);

/// How long after a relaunch the loop made a taskless screen is read as that resume's banner
/// ([`TurnEndState::restarted`]): the resumed program draws its transcript well within it, and one
/// a person starts later is a program started fresh.
pub const RESUME_DRAWN: Duration = Duration::from_secs(60);

/// What [`crate::supervise::IdleHost::agent_resumed`] reads of the foreground agent in the tab: its
/// process id, its kernel start in Unix seconds when read, and whether it resumes a conversation
/// (aterm's relaunch, or its own arguments: `relaunch::resumes_conversation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentResumed {
    pub pid: u32,
    pub started: Option<i64>,
    pub resumed: bool,
}

/// A line the occupancy door typed at a break ([`TurnEndState::fact_typed`]): whether a read has
/// seen the agent take it ([`TurnEndState::took`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FactAwaited {
    taken: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Awaited {
    rule: &'static str,
    command: bool,
    at: Instant,
    typed: Instant,
    unseen_since: Option<Instant>,
    taken: bool,
    met: bool,
    left: Option<LeftKey>,
    host: Option<HostTurn>,
    off_stood: bool,
    wall_retry: bool,
    /// A wall's own act: its retry, or the continuation its command owed ([`Kept::tries`]).
    own: bool,
    said: Option<u64>,
    over: Option<u64>,
    lead: Option<u64>,
    opened: Option<u64>,
    submitted: bool,
    submits: Option<u64>,
    screen: Option<u64>,
    /// THE LAND ACT'S TURN RAN AND ENDED SAYING NOTHING YET ([`TurnEndState::unsaid_reading`]):
    /// the work it ran for, which the reading its words are drawn on is judged with.
    ran: Option<Duration>,
}

impl Awaited {
    fn submitted_since(&self, reading: &TurnEndReading) -> bool {
        matches!((reading.submits, self.submits), (Some(now), Some(then)) if now > then)
            && reading.screen_mark.is_some()
            && reading.screen_mark != self.screen
    }
}

type LeftKey = (WallClass, Option<ResetClock>);

fn left_key(
    since: Option<&aterm_phase::SinceLimit>,
    auto: Option<aterm_phase::AutoContinue>,
) -> Option<LeftKey> {
    since
        .filter(|s| {
            auto == Some(aterm_phase::AutoContinue::Unkept)
                && !s.served
                && s.reset.is_some()
                && named_class(class_of(s.kind))
        })
        .map(|s| (class_of(s.kind), ResetClock::of(s.reset.as_deref())))
}

/// A limit a cancel left, read as the answer to a turn asked since the policy last saw the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftAnswer {
    pub wall: aterm_phase::Wall,
    pub since: Instant,
    pub screen_tells: bool,
}

impl Awaited {
    fn due(&self, timing: &TurnEndTiming) -> Instant {
        later(self.unseen_since.unwrap_or(self.at), timing.take_within)
    }
}

/// THE FIRST READ AFTER AN ACT A LOOP BEFORE THIS ONE TYPED, as its ledger shows it ([`TurnEndState::seed_awaited`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRead {
    Read(Duration),
    Unread,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetResume {
    told: Instant,
    due: Instant,
}

/// THE PERSON'S SWITCH TO LAND OVER A HELD TAB (v6 §14.17 item 3): their switch counts as their
/// turn, so Land types its instruction once more, opened by why ([`switch_act`]), and the same
/// answer holds again with no second prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SwitchAsk {
    /// No switch's ask stands.
    #[default]
    None,
    /// The word moved to Land from another while the replies held (`TurnEndState::note_word`):
    /// the ask is owed at the next decision while the hold stands ([`TurnEndState::switch_asks`]).
    Owed,
    /// The ask was typed ([`TurnEndState::acted`]); its answer is not read yet.
    Typed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LandEpisode {
    typed: bool,
    nudges: u32,
    hold: HoldStart,
    /// The rule the act that began the hold went out under, until its row stamps the hold
    /// ([`TurnEndState::ledgered`]).
    unstamped: Option<&'static str>,
    legs: Option<LandHeld>,
    /// THE LAND CHECK IS OWED ([`RULE_LAND_CHECK`]): this point's reply defends the work over a
    /// clean repository read, and that defense ([`Self::owed_mark`]) was not checked since the
    /// last work.
    check_owed: bool,
    /// The land check was Land's last act: the next judged answer is the check's
    /// ([`super::land_check::land_check_answer`]), a wall's cut and its retry kept.
    check_pending: bool,
    /// The reply marks of the defenses checked since the last real work or conclusion, newest
    /// last: one check per defense. A defense counts as checked once the check's answer is read
    /// ([`Self::sent`]), never on the typing alone: a check the agent never took leaves its
    /// defense owed the check again.
    audited: [Option<u64>; 3],
    /// The defense the check typed last audits ([`Self::owed_mark`] as it was typed), checked
    /// ([`Self::audit`]) where that check's answer is read.
    sent: Option<u64>,
    /// THE CHECK WAS ANSWERED WITH A LIST (or a need beside one), and no act or turn came since:
    /// Land's next prompt quotes it ([`TurnEndState::land_listed`]). The answer is read literally,
    /// so the question reader never takes it for a choice put to the person
    /// ([`RULE_LAND_ANSWER`]'s "Nobody is here to choose", which talked an agent out of its own
    /// need in the real-Claude drive).
    answered: bool,
    /// The owed check's defense.
    owed_mark: Option<u64>,
    /// RESTING ON A NEED: the check's answer named only what a person must give (or that and
    /// `LANDED`), over a clean repository. Land types nothing until someone else's turn.
    rest: bool,
    /// The rest was journaled ([`TurnEndState::rest_untold`]).
    rest_told: bool,
    /// THE TOKENS THE EPISODE'S DEFENSES KEPT RUNNING (each `keep <token>` line under a `Nothing
    /// left:`, [`needs::kept_tokens`]) that still run ([`Self::keep`]): the rows they name hold no
    /// landing for the rest of the episode, at the check's `LANDED`, which has no defense to read,
    /// as at the defense itself.
    kept: [Option<aterm_uds::process::Identity>; KEPT_TOKENS],
    /// A DEFENSE KEPT MORE RUNNING ROWS THAN [`Self::kept`] HOLDS: the episode no longer knows
    /// which rows its defenses kept, so the left leg holds nothing for the rest of it and a
    /// landing over a row it reads unkept says `left=unchecked`.
    kept_over: bool,
    /// HOW MANY YIELDS THE LEFT LEG HELD IN THIS EPISODE ([`HoldLeg::Left`]): from
    /// [`LEFT_HOLDS`] on it holds no more, so a program that starts again whatever the agent ends
    /// cannot keep Land typing.
    left_holds: u8,
    /// THE REPLY WHOSE YIELD THE LEFT LEG COUNTED AND NOTHING SETTLED YET ([`Self::left_holds`],
    /// its [`reply_mark`]): an act typed, or a turn run since the yield, settles it
    /// ([`TurnEndState::settle_left`]). Until then the same yield read again before its act takes
    /// its hold back first ([`TurnEndState::uncount_left`]), as does the work's conclusion, after
    /// which no act is typed for it ([`TurnEndState::conclude`]), and the books a loop that follows
    /// seeds leave it out ([`TurnEndState::land_books`]): that loop counts it again where it judges
    /// the yield again and still holds, and never where it reads a turn run since, so each yield
    /// counts once, and only as it was last judged.
    left_counted: Option<u64>,
}

/// The most running rows a land episode keeps at once ([`LandEpisode::kept`]).
pub const KEPT_TOKENS: usize = 16;

/// HOW MANY TIMES THE LEFT LEG HOLDS A LAND EPISODE ([`LandEpisode::left_holds`]), as many as
/// the same reply is let hold: past them a landing says the rows it left (`left=<n>`).
pub const LEFT_HOLDS: u8 = 3;

/// THE LAND CHECK'S BOOKS A LOOP THAT FOLLOWS KEEPS (v6 §14.17.13, the post-landing review,
/// 2026-11-01): the defenses the check audited since the last real work ([`LandEpisode::audited`])
/// and the one the check typed last audits once its answer is read ([`LandEpisode::sent`]). They
/// ride the ledger's point and act rows ([`crate::supervise::approvals::Carried::land`]), so a
/// restarted loop checks no defense twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LandBooks {
    pub audited: [Option<u64>; 3],
    pub sent: Option<u64>,
    /// The tokens the episode's defenses kept ([`LandEpisode::kept`]).
    pub kept: [Option<aterm_uds::process::Identity>; KEPT_TOKENS],
    /// A defense kept more than they hold ([`LandEpisode::kept_over`]).
    pub kept_over: bool,
    /// The yields the left leg held, the one nothing settled yet left out
    /// ([`LandEpisode::left_holds`], [`LandEpisode::left_counted`]).
    pub left_holds: u8,
}

/// THE WALL'S PASS A LOOP THAT FOLLOWS KEEPS (v6 §14.17.5.7): whether a cut act went uncounted
/// at this rung of the short streak ([`TurnEndState::cut_pass_spent`]), and whether a continue
/// act a wall cut is still owed its delivery ([`TurnEndState::cut_uncounted`]). They ride the
/// ledger's point and act rows ([`crate::supervise::approvals::Carried::pass`]), so a restarted
/// loop counts a wall's next cut act, and the vendor's continuation of the cut one, as the loop
/// before it would have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WallPass {
    pub spent: bool,
    pub owed: bool,
}

/// WHAT THE PARK OF AN OFF KEPT, by who set that Off ([`TurnEndState::park_off_held`]), as its
/// ledger row says it ([`crate::supervise::approvals::RULE_OFF_STOOD`]'s reason, read back by
/// `approvals::off_kept_in` for a loop the host starts as the Off lifts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffStood {
    /// The person set it: what the park ends is over (R1-8).
    Person,
    /// No person set it: the replies that hold, and the ask spent over them, stand.
    Held,
    /// No person set it over Land's rest on a need: the replies that hold, the land episode and
    /// the needs standing stand too.
    Resting,
}

impl OffStood {
    /// The reason its ledger row gives.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        use crate::supervise::approvals::{OFF_STOOD, OFF_STOOD_HELD, OFF_STOOD_RESTING};
        match self {
            OffStood::Person => OFF_STOOD,
            OffStood::Held => OFF_STOOD_HELD,
            OffStood::Resting => OFF_STOOD_RESTING,
        }
    }

    /// The park a row's `reason` says: a person's where it names neither of the others.
    #[must_use]
    pub fn of_reason(reason: Option<&str>) -> Self {
        [OffStood::Held, OffStood::Resting]
            .into_iter()
            .find(|k| reason == Some(k.reason()))
            .unwrap_or(OffStood::Person)
    }
}

impl LandEpisode {
    fn audit(&mut self, mark: u64) {
        if !self.audited.contains(&Some(mark)) {
            self.audited.rotate_left(1);
            self.audited[self.audited.len() - 1] = Some(mark);
        }
    }

    /// Keeps `tokens` running for the rest of the episode ([`Self::kept`]), newest last: first the
    /// kept tokens the yield's rows (`running`) no longer list are let go, since a token names one
    /// process and a gone one keeps nothing again, then each new token joins them. Past
    /// [`KEPT_TOKENS`] the new tokens no row read names go first, oldest first, so only kept rows
    /// that run fill the books (a new one no row names, a process gone or one this yield read as no
    /// row of the agent's, stays while there is room); past it still, the oldest are dropped and
    /// [`Self::kept_over`] is set. With no rows read (`running` is `None`) none is let go.
    fn keep(
        &mut self,
        tokens: &[aterm_uds::process::Identity],
        running: Option<&[aterm_uds::process::Identity]>,
    ) {
        let runs =
            |token: &aterm_uds::process::Identity| running.is_none_or(|rows| rows.contains(token));
        let mut kept: Vec<aterm_uds::process::Identity> = self
            .kept
            .iter()
            .flatten()
            .filter(|token| runs(token))
            .copied()
            .collect();
        for token in tokens {
            if !kept.contains(token) {
                kept.push(*token);
            }
        }
        let mut spare = kept.len().saturating_sub(KEPT_TOKENS);
        kept.retain(|token| {
            let drop = spare > 0 && !runs(token);
            spare -= usize::from(drop);
            !drop
        });
        if kept.len() > KEPT_TOKENS {
            self.kept_over = true;
            kept.drain(..kept.len() - KEPT_TOKENS);
        }
        self.kept = [None; KEPT_TOKENS];
        let from = KEPT_TOKENS - kept.len();
        for (slot, token) in self.kept[from..].iter_mut().zip(kept) {
            *slot = Some(token);
        }
    }

    fn audited(&self, mark: u64) -> bool {
        self.audited.contains(&Some(mark))
    }
}

/// WHERE THE HOLD BEGAN in a Land episode ([`TurnEndState::land_held`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldStart {
    pub since_ms: Option<i64>,
    pub base: u32,
    pub relanded: bool,
}

impl HoldStart {
    /// The hold a landing leaves: none, until Land's next act begins one.
    pub const LANDED: HoldStart = HoldStart {
        since_ms: None,
        base: 0,
        relanded: true,
    };
}

/// THE NEEDS STANDING AND WHAT THE MENU BAR SHOWS OF THEM (DESIGN-harness-autopilot §15.6): the
/// lines the owner's standing go-ahead answers ([`go_ahead::Park::needs`]) are held off it from
/// the decision that types the go-ahead, or waits out its back-off (`answering`), and dropped for
/// good once that act was submitted, or a reply that names no need was read before it
/// (`answered`). Any other decision on a reply that names them, the go-ahead's refusal, an
/// unsubmitted act and every reset of the needs put them back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Needs {
    standing: Vec<String>,
    key: u64,
    acked: Option<u64>,
    /// The lines a submitted go-ahead answered, and those of one decided but not typed before a
    /// reply that names no need was read.
    answered: Vec<String>,
    /// The lines the go-ahead decided at this point answers, not yet submitted.
    answering: Vec<String>,
    /// The needs the last reply read named, while no go-ahead was submitted since: every line
    /// standing after a report, only the turn's own after one a wall cut ([`Self::join`]).
    named: Vec<String>,
    /// The needs standing less the lines answered or being answered: what the menu bar shows.
    shown: Vec<String>,
    /// The key of `shown` ([`needs::needs_key`]).
    shown_key: u64,
}

impl Needs {
    /// The standing needs less `held`, in their order.
    fn less(&self, held: &[&[String]]) -> Vec<String> {
        self.standing
            .iter()
            .filter(|n| !held.iter().any(|h| h.contains(n)))
            .cloned()
            .collect()
    }

    /// Recompute what the menu bar shows.
    fn reshow(&mut self) {
        self.shown = self.less(&[self.answered.as_slice(), self.answering.as_slice()]);
        self.shown_key = needs::needs_key(&self.shown);
    }

    /// A JUDGMENT OF THE REPLY LAST READ: of the lines answered before that it names
    /// ([`Self::named`]), only those its own park answers (`parks`, [`go_ahead::Park::needs`]) stay
    /// off the menu bar. The hidden set is matched by cleaned words, so a later reply's need that
    /// cleans to a line an earlier go-ahead answered, and that this reply's park does not answer
    /// (a person's need opening with the same words), shows (2026-10-30 review 9, second pass).
    fn keep_answered(&mut self, parks: &[String]) {
        let named = &self.named;
        self.answered
            .retain(|line| parks.contains(line) || !named.contains(line));
    }

    /// The lines answered that the reply last read does not name ([`Self::named`]): off the menu
    /// bar whatever a decision on it is ([`Self::keep_answered`]).
    fn answered_unnamed(&self) -> Vec<String> {
        self.answered
            .iter()
            .filter(|line| !self.named.contains(line))
            .cloned()
            .collect()
    }

    /// A TURN A WALL CUT NAMED `named` ([`TurnEndState::observe`]): those not standing join the
    /// set after the needs standing, in the turn's order, and none is taken down, for the report
    /// that would have named every need again never came. The lines answered stay answered, and
    /// only those it names again are the last reply's ([`Self::named`]), so a decision on it
    /// other than the go-ahead's puts up only those, as it would on a reply that named them.
    fn join(&mut self, named: Vec<String>) {
        if named.is_empty() {
            return;
        }
        for line in &named {
            if !self.standing.contains(line) {
                self.standing.push(line.clone());
            }
        }
        self.key = needs::needs_key(&self.standing);
        self.named = named;
        self.reshow();
    }

    /// The standing needs a word of the agent's took back ([`needs::retracted_in`]) are no
    /// longer standing.
    fn retract(&mut self, lines: &[String]) {
        if lines.is_empty() || !self.standing.iter().any(|n| lines.contains(n)) {
            return;
        }
        self.standing.retain(|n| !lines.contains(n));
        self.answered.retain(|n| !lines.contains(n));
        self.answering.retain(|n| !lines.contains(n));
        self.named.retain(|n| !lines.contains(n));
        if self.standing.is_empty() {
            *self = Self::default();
            return;
        }
        self.key = needs::needs_key(&self.standing);
        self.reshow();
    }

    /// The lines being answered, and `past`, are answered: off the menu bar for good, until a
    /// later reply names them under another decision.
    fn answer(&mut self, past: &[String]) {
        let answering = std::mem::take(&mut self.answering);
        for line in answering.into_iter().chain(past.iter().cloned()) {
            if !self.answered.contains(&line) {
                self.answered.push(line);
            }
        }
        self.named.clear();
        self.reshow();
    }
}

/// THE GO-AHEAD'S COUNT (owner, 2026-10-29): the [`RULE_GO_AHEAD`] acts typed on the standing
/// park, at turn ends and breaks alike, and when the first since the agent last moved past it
/// went out ([`TurnEndState::go_ahead_worked`]). The standing park is every park read since the
/// last read that parked nothing, whatever word or prose each asks for
/// ([`go_ahead::Park::token`] words the answer only): an agent that alternates its token, or its
/// need and its prose, is one park (2026-10-30 review 9). A reply that parks no more and a new
/// key of the person's ([`TurnEndState::person_keyed`]) reset it; a host's act (`disk-free@v1`,
/// an upgrade's tell) does not, nor does the work the agent ran between its parks or a sleep of
/// the Mac (each starts only the span again), nor a room the answer told it to wait for: the
/// answer says to drop the line now, and an agent that waits for room as told drops it and parks
/// nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GoAheads {
    acts: u32,
    first: Option<Instant>,
}

/// WHAT A DECISION DID WITH THE GO-AHEAD'S LINES ([`TurnEndState::go_ahead_verdict`],
/// [`TurnEndState::go_ahead_judged`]; DESIGN-harness-autopilot §15.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoAheadVerdict {
    /// The decision types the owner's standing go-ahead ([`RULE_GO_AHEAD`]) or Land's instruction
    /// over its park, itself, as a restart's `otherwise` or by the Enter of the loop's own line
    /// of it, or waits out the go-ahead's back-off: the lines it answers
    /// ([`go_ahead::Park::needs`]) are off the menu bar, and they are all the act answers
    /// ([`TurnEndState::acted`]).
    Answers(Vec<String>),
    /// The go-ahead is spent and the person is told now
    /// ([`super::exception::Exception::refuses_go_ahead`]): every line is back.
    Refused,
    /// Any other decision: a reply read since the go-ahead's lines were answered (its act
    /// submitted, or a reply naming no need read before it) that names needs shows them again.
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CreditsEpisode {
    reset: Option<Instant>,
    since: Option<Instant>,
    model: Option<String>,
    passed: Option<Instant>,
    told: bool,
}

const SAME_CREDITS_RESET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreditsHeld {
    reset: Option<Instant>,
    model: Option<String>,
    lifts: Option<Instant>,
    told: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompactOwes {
    check: bool,
    land: bool,
}

/// An act's answer read at a wall, settled again by the next point's read ([`TurnEndState::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Settling {
    before: (Replies, Option<Instant>),
    at: Instant,
    /// The reset the answer showed.
    reset: Option<Instant>,
    seen: bool,
    own: bool,
}

/// The act a loop before this one typed last, its answer this loop's to read ([`TurnEndState::seed_yield`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct YieldSeed {
    continues: bool,
    typed: bool,
    own: bool,
}

/// What the policy remembers of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndState {
    pub timing: TurnEndTiming,
    login_confirmed: bool,
    /// THE `/login` DIALOG STANDING since a screen last read none, and aterm's Escs at it where it
    /// is a person's own left over a usage limit ([`super::login::over_limit_act`]).
    standing_login: Option<StandingLogin>,
    acts: VecDeque<Instant>,
    wall_acts: VecDeque<(Instant, &'static str)>,
    awaiting: Option<Awaited>,
    /// The occupancy door's line at a break ([`Self::fact_typed`]), beside the act awaited.
    fact: Option<FactAwaited>,
    followed: Option<&'static str>,
    point_worked: Duration,
    short: u32,
    /// The named program whose turns the short streak counts; a Generic shell keeps it.
    short_program: Option<Program>,
    /// The replies no-work answers to acts settled to ([`Self::repeats`]), and when the newest did.
    replies: Replies,
    replies_at: Option<Instant>,
    /// The reset the newest answer counted showed, so a reset moving forward with each answer keeps the wall's tries.
    replies_reset: Option<Instant>,
    /// The exact words ([`exact_mark`]) of the blocked reply the replies keep last, so only the
    /// same words again count it again ([`blocked_reply`]).
    blocked_words: Option<u64>,
    /// The person's newest input a point read, so one input lifts the replies once.
    person_seen: Option<Instant>,
    settling: Option<Settling>,
    point_at: Option<Instant>,
    points: u64,
    done_at: Option<Instant>,
    esc_at: Option<Instant>,
    /// THE HARNESS'S OWN ESC on a turn the Mac slept through in the vendor's API retry
    /// ([`Self::woken_esc`]), until the point it stopped is decided.
    woken_esc: Option<WokenEsc>,
    wall: Option<WallTrack>,
    notice_typed: Option<String>,
    agent_start: Option<Instant>,
    agent_start_unix: Option<i64>,
    after: Option<&'static str>,
    model: Option<ModelSwitch>,
    /// The harness started a new agent process while a model switch stood, and the host has not
    /// named it yet: a step of the host's whose word says so, or this loop's own restart
    /// ([`Self::harness_started`]). It carries the switch, as every relaunch keeps `--model`.
    started_by_harness: bool,
    fallback_unmade: bool,
    fallback_unasked: bool,
    fallback_unix: Option<i64>,
    fallback_started: Option<i64>,
    notice_unix: Option<i64>,
    account: Option<AccountMove>,
    account_asked: bool,
    account_told: bool,
    account_startup: bool,
    /// THE PROMPT THE MOVE'S OWN POINT CLOSED (its reading's answered prompt, `None` where it
    /// showed none): the resumed transcript closes it too, so at the restart's first point only
    /// another prompt answered is a turn under the move ([`Self::account_answer_unseen`]).
    account_closed: Option<u64>,
    account_resume: bool,
    account_resume_seed: bool,
    /// A LIMIT'S RESUME OWED TO A LOGIN MOVED ELSEWHERE
    /// ([`TurnEndReading::login_moved`]): a usage limit or model bucket
    /// stood when the session's login moved off its account, so its reset
    /// binds no longer — the reset dropped and one resume owed, as after a
    /// move this session's host made ([`Self::account_resume`]), until an
    /// act is taken, a turn is served or a prompt answered, or the limit's
    /// track ends or gives way to another wall.
    login_resume: bool,
    account_resumed: Option<i64>,
    limit_try: Option<LimitTry>,
    limit_ended: bool,
    /// That end was ANOTHER PROGRAM'S SCREEN read ([`met_under_another`],
    /// [`Self::limit_ended_elsewhere`]).
    limit_ended_elsewhere: bool,
    /// THE SEAT THE TAB'S SCREEN-ALONE AGENT LAST NAMED ([`Seat`], each field
    /// kept from the last read that drew it; [`Self::seat_seen`]): the login
    /// its launch card or banner drew at the session's start, which scroll
    /// off long before a limit is met, is the one a limit met later binds.
    seat_seen: Option<(Program, Seat)>,
    reset_resume: Option<ResetResume>,
    account_unmade: Option<Unmade>,
    account_unmade_seed: Option<Unmade>,
    account_unmade_ms: Option<i64>,
    account_ahead_unmade: Option<(String, Instant)>,
    /// The route move the host could not make, and when ([`Self::route_refused`]).
    route_unmade: Option<(RouteTo, Instant)>,
    account_moves: VecDeque<Instant>,
    stale_input: bool,
    done: u8,
    /// HOW MANY DONE CHECKS THIS CRUISE RUN TYPED ([`Self::checks`]), so the next asks the next
    /// of [`DONE_ANGLES`]: bumped as one is typed ([`Self::acted`]), carried across loops in the
    /// streak seed's note ([`Self::seed_checks`]), and reset at someone's turn and at a word other
    /// than Cruise.
    checks: u8,
    checking: bool,
    seeded_check: Option<&'static str>,
    check_point_seed: Option<&'static str>,
    seeded_land: Option<&'static str>,
    land_point_seed: bool,
    open: Vec<&'static str>,
    repo: crate::harness::repo_work::RepoRead,
    /// HOW MANY PROCESSES THE ACT'S LEFT SUFFIX NAMES ([`Suffix::Left`]): under Land the unkept
    /// rows of a yield whose left leg held ([`HoldLeg::Left`]), under Cruise those of its done
    /// claim; none elsewhere.
    left_said: u16,
    /// THE DONE LINE'S `left=` WORD of the landing this loop made ([`crate::harness::leftover::LeftWord`]):
    /// what its yield read of the agent's left rows; `None` where nothing was asked or no landing
    /// was made here.
    landed_left: Option<crate::harness::leftover::LeftWord>,
    needs: Needs,
    autopilot: Option<Autopilot>,
    /// The word the replies were last read under, for the person's switch to Land
    /// ([`Self::note_word`]): the word before the one a point reads, which [`Self::observe`] has
    /// already put in [`Self::autopilot`] when it asks.
    word_seen: Option<Autopilot>,
    /// The person's switch to Land over a held tab, and its one ask ([`SwitchAsk`]).
    switch_ask: SwitchAsk,
    /// A switch to Land seen and not yet weighed against a hold ([`Self::weigh_switch`]).
    switch_seen: bool,
    /// Whether the host was last told the switch's ask stands ([`Self::switch_moved`]).
    switch_told: bool,
    /// THE HELD REPLY THE SWITCH'S ASK WAS SPENT ON (its [`Kept::mark`]): the same answer holds
    /// again, and a later switch over a hold at that reply keeps limit 1 (v6 §14.17 rule 2: a
    /// prompt the agent already answered the same way is spam). Dropped once no kept reply has
    /// it ([`Self::observe`]).
    switch_spent: Option<u64>,
    land: LandEpisode,
    /// The hinted check's clause for the owed check ([`super::land_check::hint_clause`]).
    land_hint: Option<String>,
    /// The owed check's defense, as the reply said it ([`super::land_check::audited`]).
    land_owed_tail: Option<String>,
    /// THE DEFENSE THE LAST LAND CHECK AUDITED ([`Self::land_audited`]): the landing's receipt
    /// quotes it, since the landing's own reply is the word `LANDED`.
    land_audited: Option<String>,
    /// THE AGENT'S OWN LIST OF WHAT IS LEFT, its last answer to the check that was not `LANDED`,
    /// or its newest rewording of it in a reply to Land's nudge ([`Self::land_relisted`]): Land's
    /// next prompt quotes it ([`super::land_check::listed_suffix`]).
    land_listed: Option<String>,
    /// THE HOLD'S OWN WORDS at the yield whose legs [`LandEpisode::legs`] keeps, read with them
    /// ([`hold_at_yield`]): shown only while those legs stand ([`Self::land_said`]).
    land_said: HoldSaid,
    landing: bool,
    landing_act: Option<(u64, u64, Instant)>,
    landed: bool,
    /// THE WORK ENDED UNFINISHED ([`Self::gave_up`]): [`Self::done`] stands at `2` with it, so
    /// nothing is typed, until a turn runs.
    gave_up: Option<GaveUp>,
    /// The point judged last read `Could not finish:` under every gate but the manager's, which a
    /// point judged with no record read back has not read yet ([`Self::set_managed`]).
    could_not_finish_read: bool,
    /// THE FAILED CONCLUSION A LOOP BEFORE THIS ONE JOURNALED, and the mark of the reply it ended on
    /// ([`Self::seed_failed`]), until this loop's first point takes it up or drops it.
    gave_up_seed: Option<(GaveUp, Option<u64>)>,
    /// No turn ran at the point judged last, so a failed conclusion a record holds still stands
    /// there ([`Self::seed_gave_up`]).
    point_turnless: bool,
    landed_mark: Option<u64>,
    off_landing: Option<Option<u64>>,
    point_mark: Option<u64>,
    point_mark_len: usize,
    point_words: Option<u64>,
    point_opener: Option<u64>,
    point_submits: Option<u64>,
    point_screen: Option<u64>,
    point_tasked: bool,
    point_inputs: Option<u64>,
    point_clean: bool,
    last_prompt: Option<u64>,
    /// The prompt the last screen read showed answered ([`TurnEndReading::answered`]), judged
    /// as a point or not: what a turn the host types next is answered past
    /// ([`Self::host_typed`]).
    screen_prompt: Option<u64>,
    cut_check: bool,
    cut_land: bool,
    cut_text: Option<String>,
    act_words: Option<String>,
    compact_owes: Option<CompactOwes>,
    managed: bool,
    cut_reply: bool,
    /// A continue act a wall cut, not yet delivered by a wall-free point. The vendor's own short
    /// continuation that delivers it again past the wall adds nothing to the short streak (v6
    /// §14.17.5.7), as the loop's own resume does not: the act added nothing where it was the
    /// wall's one pass ([`Self::cut_pass_spent`]) and was counted already where it was not. Real
    /// work in that continuation still ends the streak.
    cut_uncounted: bool,
    /// THE WALL'S ONE PASS AT THIS RUNG OF THE STREAK IS SPENT (v6 §14.17.5.7): a continue act a
    /// wall cut went uncounted since the agent's own short answer last moved the streak or real
    /// work ended it. A wall that cuts the next act too, before either, counts it, so a wall that
    /// cuts every act still climbs the back-off ladder rather than prompting at one pace forever
    /// (§14.17: never spam the agent).
    cut_pass_spent: bool,
    exchange: bool,
    exchange_work: Duration,
    worked_before: bool,
    /// The person's exchange last left to them, by a loop before this one
    /// ([`Self::seed_left_after_work`]) or by this one ([`Self::observe`]), was left after the
    /// session worked.
    left_after_work: bool,
    /// A program started fresh in the tab since the last point (a taskless screen: its banner,
    /// no task; or the host's relaunch afresh, [`Self::relaunched_afresh`]): the next exchange
    /// opens its session, whatever the tab's books say.
    opened_fresh: bool,
    /// The session ran under Off ([`Self::seed_off_stood`]).
    off_stood: bool,
    /// The point in hand shows the program's opening exchange whole ([`Self::opening_read`]).
    opening_drawn: bool,
    /// When this loop last relaunched its program on the session's conversation, no point since
    /// ([`Self::restarted`]): a taskless screen within [`RESUME_DRAWN`] of it is that resume's,
    /// drawn before its transcript, not a program started fresh ([`Self::opened_fresh`]).
    resumed: Option<Instant>,
    /// What [`IdleHost::agent_resumed`] last reported for the foreground agent, so a relaunch
    /// made outside the loop (or one that finished after a `wait:` answer, or drew past
    /// [`RESUME_DRAWN`]) keeps its banner from setting [`Self::opened_fresh`] until that process
    /// has drawn a non-taskless screen ([`Self::transcript_drawn`]).
    resumed_agent: Option<AgentResumed>,
    /// Whether the current foreground process has drawn a non-taskless non-busy screen, so a
    /// later taskless screen of the same process (`/clear`) sets [`Self::opened_fresh`].
    transcript_drawn: bool,
    transcript_pid: Option<u32>,
    transcript_start: Option<i64>,
    /// The person's exchange in hand opens the session ([`Self::exchange_opens`]).
    exchange_opens: bool,
    carrying: bool,
    carry_at_attach: bool,
    looked: Option<(Instant, Option<LeftKey>)>,
    other_turn: bool,
    quiet_wake: bool,
    wake_repo: Option<crate::harness::repo_work::RepoRead>,
    landed_seed: Option<bool>,
    landed_words: Option<u64>,
    seed_by_wake: bool,
    by_wake: bool,
    /// The landing this state holds stood on the land check's `LANDED` (slice 13a), read at
    /// this loop's own point ([`Self::landed_by_check`]).
    by_check: bool,
    wake_restated: bool,
    /// Cruise unlanded the books at the point folded last ([`Self::unlanded`]).
    unlanded: bool,
    seed_noted_ms: Option<i64>,
    restating: Option<(TaskFace, i64)>,
    face_told: Option<TaskFace>,
    held_told: Option<Option<LandHeld>>,
    /// The hold's words last told the host ([`Self::said_moved`]); `None` where none stand told
    /// (a host starts with none, so a hold without words is never told any).
    said_told: Option<HoldSaid>,
    try_over: bool,
    unread_try: Option<UnreadTry>,
    seed_answered: bool,
    point_seed: Option<Instant>,
    ran_turn_seed: Option<(Instant, Instant)>,
    esc_seed: bool,
    point_moved: bool,
    login_returned: bool,
    login_seed: Option<Instant>,
    login_retaken: bool,
    yield_seed: Option<YieldSeed>,
    /// The needs set a loop before this one typed the ack for ([`Self::seed_needs_acked`]).
    acked_seed: u64,
    left_judged: bool,
    point_last: bool,
    ran: bool,
    gap: bool,
    /// The point the gap was taken at answers a turn no loop timed ([`Self::take_gap`]).
    gap_turn: bool,
    credits: Option<CreditsEpisode>,
    go_aheads: GoAheads,
    /// Where the person's last key holds the go-ahead from ([`Self::person_keyed`]): for
    /// [`GO_AHEAD_HELD`] after it no go-ahead answers the replies, which may agree to or restate
    /// a hold the person gave.
    go_ahead_person: Option<Instant>,
    /// The newest key of the person's read that held the go-ahead ([`Self::person_keyed`]).
    person_key: Option<Instant>,
    /// The start of the person's hour last booked or seeded ([`Self::take_go_ahead_hour`]).
    go_ahead_booked: Option<Instant>,
}

impl Default for TurnEndState {
    fn default() -> Self {
        Self::new(TurnEndTiming::default())
    }
}

impl TaskFace {
    /// THE TASK'S FACE, a pure function of the policy's memory, taken in order.
    #[must_use]
    pub fn of(state: &TurnEndState, other_turn: bool) -> TaskFace {
        if other_turn {
            return TaskFace::Open;
        }
        if state.left_to_person() {
            return TaskFace::Theirs;
        }
        // The loop's own failed conclusion ([`TurnEndState::gave_up`]) stands until a turn runs or
        // Cruise lifts it: the harness types nothing meanwhile, and no landing is under way.
        if state.gave_up.is_some() {
            return TaskFace::Failed;
        }
        if state.landed() || state.landed_seed.is_some() {
            TaskFace::Landed
        } else {
            TaskFace::Open
        }
    }
}

impl TurnEndState {
    #[must_use]
    pub fn new(timing: TurnEndTiming) -> Self {
        Self {
            timing,
            login_confirmed: false,
            standing_login: None,
            acts: VecDeque::new(),
            wall_acts: VecDeque::new(),
            awaiting: None,
            fact: None,
            followed: None,
            point_worked: Duration::ZERO,
            short: 0,
            short_program: None,
            replies: Replies::default(),
            replies_at: None,
            replies_reset: None,
            blocked_words: None,
            person_seen: None,
            settling: None,
            point_at: None,
            points: 0,
            done_at: None,
            esc_at: None,
            woken_esc: None,
            wall: None,
            notice_typed: None,
            agent_start: None,
            agent_start_unix: None,
            after: None,
            model: None,
            started_by_harness: false,
            fallback_unmade: false,
            fallback_unasked: false,
            fallback_unix: None,
            fallback_started: None,
            notice_unix: None,
            account: None,
            account_asked: false,
            account_told: false,
            account_startup: false,
            account_closed: None,
            account_resume: false,
            account_resume_seed: false,
            login_resume: false,
            account_resumed: None,
            limit_try: None,
            limit_ended: false,
            limit_ended_elsewhere: false,
            seat_seen: None,
            reset_resume: None,
            account_unmade: None,
            account_unmade_seed: None,
            account_unmade_ms: None,
            account_ahead_unmade: None,
            route_unmade: None,
            account_moves: VecDeque::new(),
            stale_input: false,
            done: 0,
            checks: 0,
            checking: false,
            seeded_check: None,
            check_point_seed: None,
            seeded_land: None,
            land_point_seed: false,
            open: Vec::new(),
            repo: crate::harness::repo_work::RepoRead::Clean,
            left_said: 0,
            landed_left: None,
            needs: Needs::default(),
            autopilot: None,
            word_seen: None,
            switch_ask: SwitchAsk::None,
            switch_seen: false,
            switch_told: false,
            switch_spent: None,
            land: LandEpisode::default(),
            land_hint: None,
            land_owed_tail: None,
            land_audited: None,
            land_listed: None,
            land_said: HoldSaid::default(),
            landing: false,
            landing_act: None,
            landed: false,
            gave_up: None,
            could_not_finish_read: false,
            gave_up_seed: None,
            point_turnless: false,
            landed_mark: None,
            off_landing: None,
            point_mark: None,
            point_mark_len: 0,
            point_words: None,
            point_opener: None,
            point_submits: None,
            point_screen: None,
            point_tasked: false,
            point_inputs: None,
            point_clean: false,
            last_prompt: None,
            screen_prompt: None,
            cut_check: false,
            cut_land: false,
            cut_text: None,
            act_words: None,
            compact_owes: None,
            managed: false,
            cut_reply: false,
            cut_uncounted: false,
            cut_pass_spent: false,
            exchange: false,
            exchange_work: Duration::ZERO,
            worked_before: false,
            left_after_work: false,
            opened_fresh: false,
            off_stood: false,
            opening_drawn: false,
            resumed: None,
            resumed_agent: None,
            transcript_drawn: false,
            transcript_pid: None,
            transcript_start: None,
            exchange_opens: false,
            carrying: false,
            carry_at_attach: false,
            looked: None,
            other_turn: false,
            quiet_wake: false,
            wake_repo: None,
            landed_seed: None,
            landed_words: None,
            seed_by_wake: false,
            by_wake: false,
            by_check: false,
            wake_restated: false,
            unlanded: false,
            seed_noted_ms: None,
            restating: None,
            face_told: None,
            held_told: None,
            said_told: None,
            try_over: false,
            unread_try: None,
            seed_answered: false,
            point_seed: None,
            ran_turn_seed: None,
            esc_seed: false,
            point_moved: false,
            login_returned: false,
            login_seed: None,
            login_retaken: false,
            yield_seed: None,
            acked_seed: 0,
            left_judged: false,
            point_last: true,
            ran: false,
            gap: false,
            gap_turn: false,
            credits: None,
            go_aheads: GoAheads::default(),
            go_ahead_person: None,
            person_key: None,
            go_ahead_booked: None,
        }
    }

    /// A model switch a previous loop on the session made and did not undo (the approval ledger's open `/model` row).
    pub fn seed_model(&mut self, switch: ModelSwitch) {
        if self.model.is_none() {
            self.model = Some(switch);
        }
    }

    /// The account moves a previous loop on the session made, `ages` ago (the approval ledger's `typed` rows of [`RULE_ACCOUNT`]).
    pub fn seed_account_moves(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.account_moves.drain(..));
        at.sort_unstable();
        self.account_moves = at.into();
        while self.account_moves.len() > 4 * ACCOUNT_MOVES_PER_WINDOW {
            self.account_moves.pop_front();
        }
    }

    /// An account move a previous loop made.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_account(
        &mut self,
        to: String,
        at: AccountAt,
        ago: Duration,
        asked: bool,
        told: bool,
        told_closes: bool,
        now: Instant,
    ) {
        if self.account.is_none() && ((told && told_closes) || ago < self.timing.window) {
            self.account = Some(AccountMove {
                to,
                at: now.checked_sub(ago).unwrap_or(now),
                wall: at == AccountAt::Wall,
            });
            self.account_asked = asked || told;
            self.account_told = told;
            self.account_startup = told || !asked;
            self.account_closed = None;
            self.account_resume = at == AccountAt::Wall && !asked && !told;
            self.account_resume_seed = self.account_resume;
        }
    }

    /// An account move a previous loop asked for could not be made.
    pub fn seed_account_unmade(&mut self, unmade: Unmade, written_ms: Option<i64>) {
        self.account_unmade = Some(unmade);
        self.account_unmade_seed = Some(unmade);
        self.account_unmade_ms = written_ms;
        self.drop_gone_launch_latch();
    }

    fn drop_gone_launch_latch(&mut self) {
        let gone = matches!(self.account_unmade, Some(Unmade::NoShell | Unmade::Launch))
            && self
                .account_unmade_ms
                .zip(self.agent_start_unix)
                .is_some_and(|(written, start)| written < start.saturating_mul(1000));
        if gone {
            self.account_unmade = None;
            self.account_unmade_seed = None;
            self.account_unmade_ms = None;
        }
    }

    /// Acts a previous loop on the session typed, `ages` ago (the approval ledger's `typed` rows).
    pub fn seed_acts(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.acts.drain(..));
        at.sort_unstable();
        self.acts = at.into();
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
        self.carrying |= !self.acts.is_empty();
    }

    /// THE CARRY THE SEEDED ACTS GAVE IS OVER (`crate::supervise::approvals::carry_ended_in`).
    pub const fn seed_carry_over(&mut self) {
        self.carrying = false;
    }

    /// Seed a usage-resume attempt recorded by a previous supervisor loop.
    pub fn seed_usage_attempt(&mut self, ago: Duration, limit: LimitTry, now: Instant) {
        self.seed_try(ago, limit, false, now);
    }

    /// THE TRY A LOOP BEFORE THIS ONE MADE, MET AGAIN.
    pub fn seed_met_attempt(&mut self, ago: Duration, limit: LimitTry, now: Instant) {
        self.seed_try(ago, limit, true, now);
    }

    fn seed_try(&mut self, ago: Duration, limit: LimitTry, met: bool, now: Instant) {
        if self.wall.is_some() {
            return;
        }
        let at = now.checked_sub(ago).unwrap_or(now);
        if !met {
            self.unread_try = Some(UnreadTry { at, limit });
        }
        let next = if limit.entitlement {
            crate::supervise::ladder::Ladder(&self.timing.entitlement_backoff).at(limit.attempts)
        } else {
            self.timing.limit_backoff.last().copied()
        };
        if !met && next.is_none_or(|next| ago >= next) {
            self.try_over = next.is_some();
            return;
        }
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            reset_dropped: false,
            class: if limit.bucket {
                WallClass::Model
            } else {
                WallClass::Usage
            },
            since: at,
            attempts: limit.attempts.max(1),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: String::new(),
            kind: None,
            seeded: !met,
            entitlement: limit.entitlement,
            owed: false,
            down: false,
            witnessed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program: None,
        });
    }

    /// THE RETRY LADDER OF THE API ERROR'S WALL THIS LOOP WAITS AT, and whether the wall was read
    /// while the host measured the route down ([`WallTrack::down`]).
    #[must_use]
    pub fn retry_track(&self) -> Option<(WallKind, usize, usize, Instant, bool)> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Retry)
            .and_then(|t| Some((t.kind?, t.attempts, t.ups, t.since, t.down)))
    }

    /// Whether the retry track's outage resumed on a witness that met the wall again
    /// ([`WallTrack::witnessed`]), for the loop that follows ([`Self::seed_retry_witnessed`]).
    #[must_use]
    pub fn retry_witnessed(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Retry && t.witnessed)
    }

    /// A loop before this one resumed the seeded retry track's outage on a witness
    /// ([`Self::retry_witnessed`], its row's ` witnessed=yes`): the ladder tries it until the agent
    /// works again, as that loop's would have (the post-landing review, 2026-11-02).
    pub fn seed_retry_witnessed(&mut self) {
        if let Some(t) = self.wall.as_mut().filter(|t| t.class == WallClass::Retry) {
            t.witnessed = true;
        }
    }

    /// THE WITNESS AN OUTAGE WAIT'S RESUME AT `reading` IS TYPED ON (v6 §14.17.5.4, slice 12c):
    /// when another session's turn on the route finished, where it clears the standing API wall's
    /// track ([`witness_clears`]); `None` otherwise. The loop journals it with the resume.
    #[must_use]
    pub fn witness_clearing(&self, reading: &TurnEndReading) -> Option<Instant> {
        let t = self.wall.as_ref().filter(|t| t.class == WallClass::Retry)?;
        witness_clears(Some(t), reading, t.kind?)
    }

    /// THE WITNESS `action` RESUMED THE OUTAGE ON (v6 §14.17.5.4): [`Self::witness_clearing`] at
    /// the point's `reading` as decided (`Check::AsDecided`, its witness the decision's), for an
    /// outage's resume of any text but the blind try's line: the measured line, Land's instruction
    /// or the person's draft sent on the witness (the post-landing review, 2026-11-02); `None` for
    /// the blind line, whatever witness landed while it was typed, and for every other act.
    #[must_use]
    pub fn witness_resumed(
        &self,
        action: &TurnEndAction,
        reading: &TurnEndReading,
    ) -> Option<Instant> {
        if action.rule_id() != Some(RULE_OUTAGE_RESUME) || types_blind_outage_line(action) {
            return None;
        }
        self.witness_clearing(reading)
    }

    /// THE RETRY LADDER A LOOP BEFORE THIS ONE WAS ON at the API error's wall `kind` ([`Self::retry_track`], its last row's mark).
    #[allow(clippy::too_many_arguments)]
    pub fn seed_retry(
        &mut self,
        kind: WallKind,
        attempts: usize,
        ups: usize,
        ago: Duration,
        owed: bool,
        down: bool,
        now: Instant,
    ) {
        if self.wall.is_some() || class_of(kind) != WallClass::Retry {
            return;
        }
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            reset_dropped: false,
            class: WallClass::Retry,
            since: now.checked_sub(ago).unwrap_or(now),
            attempts,
            ups,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: String::new(),
            kind: Some(kind),
            seeded: false,
            entitlement: false,
            owed,
            down,
            witnessed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program: None,
        });
    }

    /// THE PERSON'S LAST KEY, `key`: the window's measured typing (`human_typed_ms`) or another
    /// driver's hand, never a write of the harness's (the window stamps none for a control
    /// write). It holds the go-ahead for [`GO_AHEAD_HELD`] from the later of the key and `ended`,
    /// the end of the turn it came in where it came in one ([`Self::go_ahead_held`]); a hold
    /// timed later stands. A key that holds nothing at `now` changes nothing. One read more than
    /// [`KEY_DRIFT`] past the newest one read is a new key: the person came back, and the
    /// go-ahead's count starts again. No word of theirs is read: a hold that outlasts the hour is
    /// the agent's to keep, and the go-ahead says so ([`go_ahead::GO_AHEAD_ANSWER`]).
    fn person_keyed(&mut self, key: Instant, ended: Option<Instant>, now: Instant) {
        let from = ended.map_or(key, |end| key.max(end));
        if now >= later(from, GO_AHEAD_HELD) {
            return;
        }
        if self
            .person_key
            .is_none_or(|newest| key > later(newest, KEY_DRIFT))
        {
            self.go_aheads = GoAheads::default();
        }
        self.person_key = Some(self.person_key.map_or(key, |newest| newest.max(key)));
        self.go_ahead_person = Some(self.go_ahead_person.map_or(from, |at| at.max(from)));
    }

    /// THE PERSON'S LAST KEY READ AT A SETTLED BREAK of the agent's own background work that
    /// began at `began` ([`Self::person_keyed`]), the turn it ends opened at `opened` (none read
    /// live: `None`): a key past the last point judged and act typed, and no more than
    /// [`PERSON_TURN_SLACK`] before the turn opened, came in that turn or at the break, so its
    /// hour runs from the later of the key and the break's start, as a key in a point's turn runs
    /// from the point ([`Self::came_in_turn`]). Any other key's runs from the key: one older than
    /// the turn, a key the loop's first look or a restart reads, and a key read again at the
    /// later breaks of the agent's own wakes, each a turn opened after it.
    pub fn break_keyed(
        &mut self,
        key: Instant,
        began: Instant,
        opened: Option<Instant>,
        now: Instant,
    ) {
        let in_turn = opened.is_some_and(|from| later(key, PERSON_TURN_SLACK) >= from)
            && self
                .point_at
                .max(self.acts.back().copied())
                .is_none_or(|since| key > since);
        self.person_keyed(key, in_turn.then_some(began), now);
    }

    /// THE PERSON'S HOUR TO BOOK ([`RULE_GO_AHEAD_HOUR`]): its start where it runs from the end
    /// of the turn their key came in, more than [`KEY_DRIFT`] after the newest key read (the
    /// window's clock keeps the key, not that end), and more than [`KEY_DRIFT`] past the start
    /// last booked or seeded. Taken, it is the one booked.
    pub fn take_go_ahead_hour(&mut self) -> Option<Instant> {
        let from = self.go_ahead_person?;
        let past = |at: Option<Instant>| at.is_none_or(|at| from > later(at, KEY_DRIFT));
        if !(past(self.person_key) && past(self.go_ahead_booked)) {
            return None;
        }
        self.go_ahead_booked = Some(from);
        Some(from)
    }

    /// THE PERSON'S HOUR A LOOP BEFORE THIS ONE BOOKED, starting `ago` before `now`
    /// ([`Self::take_go_ahead_hour`]): it holds the go-ahead for the rest of that hour
    /// ([`Self::person_holds`]), as it would have in the loop that booked it. One already over
    /// holds nothing.
    pub fn seed_go_ahead_hour(&mut self, ago: Duration, now: Instant) {
        if ago >= GO_AHEAD_HELD {
            return;
        }
        let Some(from) = now.checked_sub(ago) else {
            return;
        };
        self.go_ahead_booked = Some(from);
        self.go_ahead_person = Some(self.go_ahead_person.map_or(from, |at| at.max(from)));
    }

    /// THE `/login` A LOOP BEFORE THIS ONE TYPED, `ago` ago, as the ledger says it.
    pub fn seed_login(&mut self, ago: Duration, now: Instant) {
        self.login_seed = Some(now.checked_sub(ago).unwrap_or(now));
    }

    /// Record or adopt an in-progress `/login` attempt at a lost-login wall.
    pub fn take_up_login(
        &mut self,
        at_wall: Option<&aterm_phase::LoginAtWall>,
        interrupted: bool,
        now: Instant,
    ) -> bool {
        let Some(at_wall) = at_wall else {
            return false;
        };
        if self
            .wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth)
        {
            return false;
        }
        let typed_at = self.login_seed.take();
        let cancelled = interrupted || at_wall.left_before;
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            reset_dropped: false,
            class: WallClass::Auth,
            since: now,
            attempts: usize::from(typed_at.is_some()),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: at_wall.message.clone(),
            kind: Some(WallKind::Auth),
            seeded: false,
            entitlement: false,
            owed: false,
            down: false,
            witnessed: false,
            conversation: Conversation::Hostless,
            program: None,
            login: LoginBooks {
                cancelled,
                typed_at,
                ..LoginBooks::default()
            },
        });
        self.login_retaken |= typed_at.is_some() && !cancelled;
        true
    }

    /// Seed a named usage-limit or model-bucket reset wait from a previous loop.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_named_reset(
        &mut self,
        kind: WallKind,
        message: &str,
        at: Instant,
        jitter: Duration,
        by_itself: bool,
        said: Option<ResetClock>,
        since: Instant,
    ) {
        let class = class_of(kind);
        if !named_class(class) {
            return;
        }
        let named = NamedReset {
            kind,
            at,
            jitter,
            by_itself: by_itself && !self.countdown_gone(since),
            said,
            first: since,
            written: None,
        };
        let (since, attempts, named, program) = match &self.wall {
            Some(t) if t.class == class => {
                (t.since, t.attempts, t.named.unwrap_or(named), t.program)
            }
            _ => (since, 0, named, None),
        };
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            reset_dropped: false,
            class,
            since,
            attempts,
            ups: 0,
            exhausted: false,
            fill: None,
            named: Some(named),
            pointed: true,
            said: message.to_string(),
            kind: None,
            seeded: false,
            entitlement: false,
            owed: false,
            down: false,
            witnessed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program,
        });
    }

    /// A LIMIT WHOSE NOTICE PLACED NO RESET a previous loop on the session waited on and no loop has ended.
    pub fn seed_unnamed_wall(&mut self, kind: WallKind, message: &str, since: Instant) {
        let class = class_of(kind);
        if self.wall.is_some() || self.try_over || !named_class(class) {
            return;
        }
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            reset_dropped: false,
            class,
            since,
            attempts: 0,
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: message.to_string(),
            kind: Some(kind),
            seeded: false,
            entitlement: entitlement_ladder(false, kind),
            owed: false,
            down: false,
            witnessed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program: None,
        });
    }

    /// AN ACT A LOOP BEFORE THIS ONE TYPED IS STILL INSIDE ITS TAKE WINDOW (typed `ago` ago, under [`TurnEndTiming::take_within`]).
    pub fn seed_awaited(
        &mut self,
        rule: &'static str,
        ago: Duration,
        read: FirstRead,
        wall_retry: bool,
        met: bool,
        now: Instant,
    ) -> bool {
        let take = self.timing.take_within;
        let (open, read_ago) = match read {
            FirstRead::Read(r) => (r < take, Some(r)),
            FirstRead::Unread => (true, None),
            FirstRead::Unknown => (ago < take, None),
        };
        if !open || self.awaiting.is_some() {
            return false;
        }
        let at = now.checked_sub(ago).unwrap_or(now);
        self.awaiting = Some(Awaited {
            rule,
            command: false,
            at,
            typed: at,
            unseen_since: read_ago.map(|r| now.checked_sub(r).unwrap_or(now)),
            taken: false,
            met,
            left: None,
            host: None,
            off_stood: false,
            wall_retry,
            own: wall_retry,
            said: None,
            over: None,
            lead: None,
            opened: None,
            submitted: false,
            submits: None,
            screen: None,
            ran: None,
        });
        true
    }

    /// THE DONE CHECK A LOOP BEFORE THIS ONE ASKED IS OWED ITS ANSWER ([`crate::supervise::approvals::done_check_owed`]).
    pub const fn seed_done_check(&mut self, rule: &'static str, cut: bool, answered: bool) {
        self.seeded_check = Some(rule);
        if cut {
            self.cut_check = true;
        } else {
            self.checking = true;
            self.seed_answered |= answered;
        }
    }

    /// THE DONE CHECK A FULL CONTEXT CUT.
    pub const fn seed_compact_check(&mut self, rule: &'static str) {
        self.seeded_check = Some(rule);
        self.compact_owes = Some(CompactOwes {
            check: true,
            land: false,
        });
        self.after = Some(RULE_COMPACT);
    }

    /// THE CHECK WHOSE ANSWER THE POINT A LOOP BEFORE THIS ONE JUDGED LAST WAS.
    pub const fn seed_check_point(&mut self, rule: &'static str) {
        self.check_point_seed = Some(rule);
    }

    /// THE LAND ACT A LOOP BEFORE THIS ONE TYPED.
    pub const fn seed_land_owed(&mut self, rule: &'static str) {
        self.seeded_land = Some(rule);
    }

    /// …AND IT WAS THE LAND CHECK ([`crate::supervise::approvals::LandSeed::checked`]): the next
    /// judged answer is the check's, so a `LANDED` read there lands.
    pub const fn seed_land_checked(&mut self) {
        self.land.check_pending = true;
    }

    /// THE LAND ACT A FULL CONTEXT CUT.
    pub const fn seed_compact_land(&mut self) {
        self.compact_owes = Some(CompactOwes {
            check: false,
            land: true,
        });
        self.after = Some(RULE_COMPACT);
    }

    /// THE LAND ACT WHOSE ANSWER THE POINT A LOOP BEFORE THIS ONE JUDGED LAST WAS.
    pub const fn seed_land_point(&mut self) {
        self.land_point_seed = true;
    }

    fn leave_land_episode(&mut self) {
        self.land = LandEpisode::default();
        self.land_hint = None;
        self.land_owed_tail = None;
        self.land_listed = None;
        self.landing = false;
        self.landing_act = None;
        self.cut_land = false;
        self.seeded_land = None;
        self.land_point_seed = false;
        if self.cut_text.as_deref().is_some_and(is_land_words) {
            self.cut_text = None;
        }
        self.compact_owes = self.compact_owes.and_then(|mut owed| {
            owed.land = false;
            owed.check.then_some(owed)
        });
    }

    fn leave_cruise_episode(&mut self) {
        // Out of Cruise: the next Cruise run's first done claim asks the first angle, and any cut
        // or compacted check payload is revoked.
        self.checks = 0;
        self.cut_check = false;
        self.check_point_seed = None;
        if !self.checking {
            self.seeded_check = None;
        }
        if self
            .cut_text
            .as_deref()
            .is_some_and(|t| is_done_check(t) || is_challenge_text(t))
        {
            self.cut_text = None;
        }
        self.compact_owes = self.compact_owes.and_then(|mut owed| {
            owed.check = false;
            owed.land.then_some(owed)
        });
    }

    /// A TURN WAS READ RUNNING after the point a loop before this one recorded last ([`crate::supervise::approvals::StreakSeed::ran`]).
    pub fn seed_ran(&mut self) {
        if std::mem::take(&mut self.seed_answered) {
            self.checking = false;
            self.landing = false;
        }
        self.gap = true;
    }

    /// THE LIMIT'S TRY A LOOP BEFORE THIS ONE TYPED LAST WAS TAKEN.
    pub const fn seed_try_taken(&mut self) {
        self.gap = true;
    }

    pub(crate) fn take_gap(&mut self) -> bool {
        let gap = std::mem::take(&mut self.gap) && self.awaiting.is_none();
        self.gap_turn |= gap;
        gap
    }

    /// The act a loop before this one typed last, its answer unread ([`Self::seed_yield`]).
    #[must_use]
    pub fn seeded_act_typed(&self) -> Option<Instant> {
        self.yield_seed.and(self.acts.back().copied())
    }

    pub(crate) const fn launched_since_books(&mut self) {
        self.gap = false;
        self.gap_turn = false;
        self.ran_turn_seed = None;
        self.replies = Replies::NONE;
        self.replies_at = None;
    }

    /// …AND WHEN.
    pub fn seed_ran_turn(&mut self, ran_ago: Duration, point_ago: Duration, now: Instant) {
        self.ran_turn_seed = Some((
            now.checked_sub(ran_ago).unwrap_or(now),
            now.checked_sub(point_ago).unwrap_or(now),
        ));
    }

    fn ran_turn_reading(
        &mut self,
        reading: &TurnEndReading,
        now: Instant,
    ) -> Option<TurnEndReading> {
        let (ran_at, point_at) = self.ran_turn_seed.take()?;
        if reading.worked.is_some() || self.awaiting.is_some() || self.point_at.is_some() {
            return None;
        }
        let worked = now.saturating_duration_since(ran_at);
        let at = reading.person.and_then(|ago| now.checked_sub(ago))?;
        let since = self
            .acts
            .back()
            .copied()
            .map_or(point_at, |a| a.max(point_at));
        let turn = TurnEndReading {
            worked: Some(worked),
            ..reading.clone()
        };
        (at > since && turn.person_in_turn()).then_some(turn)
    }

    /// THE LAND EPISODE A LOOP BEFORE THIS ONE HAD IN HAND ([`crate::supervise::approvals::land_episode`]).
    pub fn seed_land(&mut self, nudges: u32, landing: bool, cut: bool, answered: bool) {
        self.land = LandEpisode {
            typed: true,
            nudges,
            ..LandEpisode::default()
        };
        self.landing = landing;
        self.cut_land = cut;
        self.seed_answered |= landing && answered;
    }

    /// THE WALL'S PASS this state keeps ([`WallPass`]), for the ledger's rows.
    #[must_use]
    pub const fn wall_pass(&self) -> WallPass {
        WallPass {
            spent: self.cut_pass_spent,
            owed: self.cut_uncounted,
        }
    }

    /// Seed the wall's pass a loop before this one booked ([`Self::wall_pass`]).
    pub const fn seed_wall_pass(&mut self, pass: WallPass) {
        self.cut_pass_spent = pass.spent;
        self.cut_uncounted = pass.owed;
    }

    /// THE LAND CHECK'S BOOKS this state keeps ([`LandBooks`]), for the ledger's rows: the left
    /// hold of a yield nothing settled yet left out ([`LandEpisode::left_counted`]), since the
    /// yield may yet be judged again before its act, and a loop that follows judges it again or
    /// reads a turn run since.
    #[must_use]
    pub fn land_books(&self) -> LandBooks {
        let unsettled = u8::from(self.land.left_counted.is_some());
        LandBooks {
            audited: self.land.audited,
            sent: self.land.sent,
            kept: self.land.kept,
            kept_over: self.land.kept_over,
            left_holds: self.land.left_holds.saturating_sub(unsettled),
        }
    }

    /// …AND THOSE A LOOP BEFORE THIS ONE KEPT, in the land episode it handed on
    /// ([`Self::seed_land`], which comes first).
    pub const fn seed_land_books(&mut self, books: LandBooks) {
        if self.land.typed {
            self.land.audited = books.audited;
            self.land.sent = books.sent;
            self.land.kept = books.kept;
            self.land.kept_over = books.kept_over;
            self.land.left_holds = books.left_holds;
        }
    }

    /// …AND THAT EPISODE'S LAST ACT, as the ledger holds it ([`crate::supervise::approvals::LandAct`]).
    pub fn seed_land_act(&mut self, said: u64, lead: u64, ago: Duration, now: Instant) {
        self.landing_act = Some((said, lead, now.checked_sub(ago).unwrap_or(now)));
    }

    /// THE LAND ACT A LOOP BEFORE THIS ONE TYPED ([`Self::seed_land_act`]) IS WHAT THIS READING
    /// ANSWERS: its turn opened by the act (agy, Muse Code) or its prompt the act's words (Claude
    /// Code); with no act seeded, any reading is.
    fn act_seen(
        landing_act: Option<(u64, u64, Instant)>,
        reading: &TurnEndReading,
        now: Instant,
    ) -> bool {
        let act_speaks = match reading.program {
            aterm_phase::Program::Claude => true,
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Generic => false,
        };
        let act_opens = match reading.program {
            aterm_phase::Program::Agy | aterm_phase::Program::Muse => true,
            aterm_phase::Program::Claude
            | aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Generic => false,
        };
        match landing_act {
            Some((_, lead, _)) if act_opens => reading.opened_by.is_none_or(|o| o == lead),
            Some((said, _, typed)) if act_speaks => {
                reading.answered.is_some_and(|a| a.prompt == said)
                    || reading.record_turn.is_some_and(|t| {
                        t.prompt == said
                            && t.ended.is_some()
                            && now
                                .checked_sub(t.age)
                                .is_some_and(|recorded| recorded + RECORD_SLACK >= typed)
                    })
            }
            Some(_) | None => true,
        }
    }

    /// A LAND ACT A LOOP BEFORE THIS ONE TYPED IS OWED ITS ANSWER at this state's first point ([`Self::seed_land_act`]).
    #[must_use]
    pub const fn land_act_seeded(&self) -> bool {
        self.landing && self.landing_act.is_some() && self.point_at.is_none()
    }

    /// THE TURN ENDING AT `r` IS OWED ITS WHOLE WORDS, whatever its last ones claim.
    #[must_use]
    pub fn reads_turn_whole(&self, r: &TurnEndReading, worked: bool) -> bool {
        let first_point = self.point_at.is_none() && !r.program.reader().bounds_messages();
        // A kept reply is compared at every point ([`Self::repeats`]), so as it was read at its answer.
        let compared = self.replies.last().is_some();
        if !(worked || first_point || compared) || matches!(r.phase, Phase::Busy | Phase::Prompt) {
            return false;
        }
        // A turn a wall cut folds its needs too ([`Self::observe`]), so it is read whole for them.
        let folds_needs = !r.pending_input;
        let keeps_landing =
            r.finished_record == Some(true) && (self.landed() || self.landed_seed.is_some());
        folds_needs || keeps_landing
    }

    /// WHETHER THE WORKER'S LAST WORDS `tail` ARE A LANDING'S OWN, READ AGAIN.
    #[must_use]
    pub fn landing_words_read(&self, tail: Option<&str>) -> bool {
        let mark = said_mark(tail);
        mark.is_some()
            && self.awaiting.is_none()
            && ((self.landed_seed == Some(true) && self.landed_words == mark)
                || (self.landed() && self.off_landing.is_none() && self.landed_mark == mark))
    }

    /// Seed the consecutive short-turn streak recorded by a previous loop.
    pub const fn seed_short(&mut self, short: u32) {
        self.short = short;
        self.point_last = false;
    }

    /// Seed the program named by the previous loop's counted point.
    pub const fn seed_short_program(&mut self, program: Program) {
        self.short_program = Some(program);
    }

    /// Seed the replies a previous loop recorded `ago` ([`Self::replies`]).
    pub fn seed_replies(&mut self, said: Replies, ago: Duration, now: Instant) {
        self.replies = said;
        self.replies_at = (said != Replies::default()).then(|| now.checked_sub(ago).unwrap_or(now));
    }

    /// THE HELD REPLY A LOOP BEFORE THIS ONE SPENT THE SWITCH'S ONE ASK ON ([`Self::switch_spent`],
    /// the post-landing review, 2026-11-01), kept only while a seeded reply has it
    /// ([`Self::seed_replies`] first): the person's flip back to Land over the same reply asks
    /// nothing again across a restart.
    pub fn seed_switch_spent(&mut self, mark: Option<u64>) {
        self.switch_spent = mark.filter(|m| self.replies.has(*m));
    }

    /// THE DONE CHECKS A LOOP BEFORE THIS ONE TYPED IN THIS CRUISE RUN, as its note says
    /// ([`Self::checks`]): this loop's next asks the angle after them.
    pub const fn seed_checks(&mut self, checks: u8) {
        self.checks = if checks < CHECKS_WRAP {
            checks
        } else {
            CHECKS_ROUND + checks % CHECKS_ROUND
        };
    }

    /// How many done checks this Cruise run typed: the next one asks [`DONE_ANGLES`]'s angle at
    /// this count, the seventh the first again ([`done_check_words`]).
    #[must_use]
    pub const fn checks(&self) -> u8 {
        self.checks
    }

    /// THE RESET THE NEWEST REPLY SEEDED SHOWED, as its record says ([`Self::replies_reset`]).
    pub const fn seed_replies_reset(&mut self, at: Option<Instant>) {
        self.replies_reset = at;
    }

    /// THE STREAK SEEDED WAS RECORDED AT A POINT a loop before this one judged `ago` ago ([`crate::supervise::approvals::RULE_POINT`]).
    pub fn seed_point(&mut self, ago: Duration, now: Instant) {
        self.point_seed = Some(now.checked_sub(ago).unwrap_or(now));
        self.point_last = true;
    }

    /// THE SESSION WORKED BEFORE THIS LOOP: its books hold a point a loop before this one judged or
    /// an act it typed for the session, or the session ran under Off ([`Self::seed_off_stood`]).
    /// A person's message is then never the one that opens the session, so under Land it
    /// interrupts the work rather than being theirs ([`Self::observe`]).
    pub const fn seed_worked(&mut self) {
        self.worked_before = true;
    }

    /// THE SESSION RAN UNDER OFF, its points unjudged (this loop lifted off it, or its books hold
    /// the lift's `autopilot-off@v1` row): a person may have driven it by hand, so it worked
    /// ([`Self::seed_worked`]) unless its first point's screen shows its opening exchange
    /// ([`Self::opening_read`]).
    pub const fn seed_off_stood(&mut self) {
        self.worked_before = true;
        self.off_stood = true;
    }

    /// THE EXCHANGE A LOOP BEFORE THIS ONE LEFT TO THE PERSON CAME AFTER WORK: its books hold an
    /// act typed, or a point recorded at an earlier screen, before the record
    /// (`approvals::left_after_work_in`). Under Land that record is an older build's, under the
    /// superseded rule, and the work goes on ([`Self::observe`]).
    pub const fn seed_left_after_work(&mut self) {
        self.left_after_work = true;
    }

    /// Whether the person's exchange in hand opens the session: no point judged or seeded, no work
    /// in the books before it and no conversation resumed, or a program started fresh since the
    /// last point (its banner read, or begun in the turn: [`Self::launched_in_turn`]). The loop
    /// writes it into the exchange's `LEFT` row (`approvals::left_reason`), so a loop that
    /// follows keeps that record the person's whatever the tab's books read before it.
    #[must_use]
    pub const fn exchange_opens(&self) -> bool {
        self.exchange_opens
    }

    /// Whether a loop before this one worked the session ([`Self::seed_worked`]).
    #[must_use]
    pub const fn worked_before(&self) -> bool {
        self.worked_before
    }

    /// A PERSON'S ESC STOOD AT THE POINT SEEDED ([`Self::seed_point`]), as its record says (`approvals::StreakSeed::esc`).
    pub const fn seed_esc(&mut self) {
        self.esc_seed = true;
    }

    /// Whether a person's Esc stands at the point now ([`Self::esc_at`]).
    #[must_use]
    pub const fn persons_esc_stands(&self) -> bool {
        self.esc_at.is_some()
    }

    /// THE HARNESS PRESSED ESC on a turn the Mac slept through in the vendor's API retry
    /// (`supervise::run::woken_turn`): the point it stops is no person's Esc, so the person's
    /// grace holds nothing there; `resume`: the turn was the vendor's own continuation after a
    /// usage limit, so the point's act is the limit's resume ([`RULE_LIMIT_RESUME`]). A live read
    /// ([`Self::took`]), a box ([`Self::box_up`]) or a point no Esc stopped ends it.
    pub const fn woken_esc(&mut self, resume: bool) {
        self.woken_esc = Some(WokenEsc { resume });
    }

    /// THE `Interrupted` ROW A RESUME DRAWS IS THE HARNESS'S RESTART'S OWN: a relaunch ended the
    /// agent with a tool call running (its own `aterm ctl @self restart`, as a rule), and the resumed
    /// process restores that call as `Interrupted · What should Claude do instead?`. Read while its
    /// carry-on is owed ([`crate::supervise::run::IdleHost::carry_owed`]), it is no person's Esc, as
    /// [`Self::woken_esc`]'s is not, and it ends as that one does.
    pub const fn restored_cut(&mut self) {
        self.woken_esc(false);
    }

    /// Whether the harness's own Esc stands at the point now ([`Self::woken_esc`]).
    #[must_use]
    pub const fn woken_esc_stands(&self) -> bool {
        self.woken_esc.is_some()
    }

    /// THE STREAK SEEDED IS THE ONE THE ACT `rule` A LOOP BEFORE THIS ONE TYPED LAST WAS TYPED AFTER.
    pub fn seed_yield(&mut self, rule: &str) {
        self.yield_seed = Some(YieldSeed {
            continues: is_continue_rule(rule),
            typed: !matches!(rule, RULE_COMPACT | RULE_LOGIN),
            own: is_wall_retry(rule),
        });
    }

    pub(crate) fn take_point_moved(&mut self) -> bool {
        let moved = std::mem::take(&mut self.point_moved);
        self.point_last |= moved;
        moved
    }

    pub(crate) fn take_login_returned(&mut self) -> bool {
        std::mem::take(&mut self.login_returned)
    }

    pub(crate) fn take_login_retaken(&mut self) -> bool {
        std::mem::take(&mut self.login_retaken)
    }

    pub(crate) fn take_ran(&mut self) -> bool {
        std::mem::take(&mut self.ran)
    }

    #[must_use]
    pub(crate) const fn left_judged(&self) -> bool {
        self.left_judged
    }

    pub(crate) fn take_try_over(&mut self) -> bool {
        std::mem::take(&mut self.try_over)
    }

    /// WHEN THE LIMIT'S TRY NO LOOP HAS READ WAS TYPED (on the policy's clock, [`UnreadTry::at`]), while this loop has not read it either.
    #[must_use]
    pub fn unread_try(&self) -> Option<Instant> {
        self.unread_try
            .map(|u| u.at)
            .filter(|&at| self.awaits_unread(at))
    }

    fn awaits_unread(&self, at: Instant) -> bool {
        self.awaiting.is_some_and(|a| {
            a.wall_retry && !a.taken && !a.met && a.unseen_since.is_none() && a.typed == at
        })
    }

    /// THE ANSWER OF A TRY NO LOOP READ.
    pub fn date_unread_try(
        &mut self,
        wall: Option<WallKind>,
        answered: Option<Duration>,
    ) -> Option<Instant> {
        let u = self.unread_try.take()?;
        let answered = answered?;
        if !self.awaits_unread(u.at) {
            return None;
        }
        let class = if u.limit.bucket {
            WallClass::Model
        } else {
            WallClass::Usage
        };
        if wall.map(class_of) != Some(class) {
            return None;
        }
        self.awaiting = None;
        self.try_over = false;
        let read = later(u.at, answered);
        let program = self.wall.as_ref().and_then(|t| t.program);
        self.wall = Some(WallTrack {
            relaunch: RelaunchBooks::default(),
            seat: Seat::default(),
            seat_moved: false,
            reset_dropped: false,
            class,
            since: later(read, self.timing.take_within),
            attempts: u.limit.attempts.max(1),
            ups: 0,
            exhausted: false,
            fill: None,
            named: None,
            pointed: true,
            said: String::new(),
            kind: None,
            seeded: false,
            entitlement: u.limit.entitlement,
            owed: false,
            down: false,
            witnessed: false,
            login: LoginBooks::default(),
            conversation: Conversation::Hostless,
            program,
        });
        Some(read)
    }

    pub(crate) fn seed_limit_conversation(
        &mut self,
        conversation: Conversation,
        program: Option<Program>,
    ) {
        if let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class)) {
            t.conversation = conversation.fresh_after(&t.conversation);
            t.program = program.or(t.program);
        }
    }

    pub(crate) fn rescope_limit_conversation(&mut self, from: &Conversation, to: &Conversation) {
        if let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class))
            && t.conversation == *from
        {
            t.conversation = to.clone();
        }
    }

    pub(crate) fn limit_conversation(&self) -> Option<&Conversation> {
        self.wall
            .as_ref()
            .filter(|t| named_class(t.class))
            .map(|t| &t.conversation)
    }

    pub(crate) fn limit_program(&self) -> Option<Program> {
        self.wall
            .as_ref()
            .filter(|t| named_class(t.class))
            .and_then(|t| t.program)
    }

    pub(crate) fn has_limit_track(&self) -> bool {
        self.wall.as_ref().is_some_and(|t| named_class(t.class))
    }

    /// Whether the limit tracked is one a usage tap's window times (a Claude Code session or weekly
    /// limit), its reset named or not, and not dropped for a login since
    /// ([`WallTrack::reset_dropped`]).
    #[must_use]
    pub(crate) fn tracks_tapped_limit(&self) -> bool {
        self.wall.as_ref().is_some_and(tapped_limit)
    }

    /// THE SEAT `program`'s SCREEN NAMES NOW ([`WallTrack::seat`]): the
    /// fields `here` (this screen's [`Seat::of`]) names, over those the tab's
    /// earlier screens of the same agent named ([`TurnEndState::seat_seen`]:
    /// the launch card's or banner's login, read at the session's start and
    /// scrolled off since).
    #[must_use]
    pub(crate) fn seat_now(&self, program: Program, here: &Seat) -> Seat {
        match &self.seat_seen {
            Some((seen_by, seen)) if *seen_by == program => seen.merged(here),
            _ => here.clone(),
        }
    }

    /// The seat `reading`'s screen names, kept ([`TurnEndState::seat_seen`]):
    /// a screen-alone agent's fields over what the tab's earlier screens of
    /// it named; another agent's screen replaces it (its seat is none of
    /// this one's); a screen no reader names (a shell, a frame before the
    /// agent draws) keeps it.
    fn note_seat(&mut self, reading: &TurnEndReading) {
        let program = reading.program;
        if program == Program::Generic {
            return;
        }
        if !screen_alone(program) {
            self.seat_seen = None;
            return;
        }
        match &mut self.seat_seen {
            Some((seen_by, seen)) if *seen_by == program => seen.update(&reading.seat),
            _ => self.seat_seen = Some((program, reading.seat.clone())),
        }
    }

    /// WHETHER A LIMIT THIS POLICY WAITS ON HOLDS THE HOST'S STEPS at `now`
    /// (`Session::host_steps_here`, `host_steps_in_background`: a relaunch's
    /// carry-on, an upgrade's notice): a usage limit, spend limit or model
    /// bucket ([`named_class`]) met under an agent whose screen is all its
    /// evidence ([`screen_alone`]), its track standing — through the
    /// agent's relaunch ([`Self::relaunch_startup`]), where the loop's own
    /// limit episode closed on the relaunched screen — until `grace` past the
    /// reset it named, and for good where it names none (a reset is never
    /// guessed: the policy's own tries, or a person's turn, end it).
    #[must_use]
    pub fn limit_holds_host(&self, now: Instant, grace: Duration) -> bool {
        let Some(t) = self.wall.as_ref().filter(|t| named_class(t.class)) else {
            return false;
        };
        if !t.program.is_some_and(screen_alone) || t.seat_moved {
            return false;
        }
        t.named.is_none_or(|n| now < later(n.at, grace))
    }

    /// Whether a usage limit or a model bucket whose notice named its reset is waited on ([`named_class`]).
    #[must_use]
    pub fn waits_on_named_reset(&self) -> bool {
        self.waited_limit().is_some()
    }

    /// The standing retry wall's kind when its notice left the screen ([`left_retry_kind`]).
    #[must_use]
    pub(crate) fn standing_retry_wall(&self) -> Option<WallKind> {
        let retry_taken = self.awaiting.is_some_and(|a| a.wall_retry && a.taken);
        self.wall
            .as_ref()
            .filter(|t| !retry_taken && !t.owed)
            .and_then(left_retry_kind)
    }

    fn limit_elsewhere(&self, reading: &TurnEndReading) -> bool {
        if self.wall.as_ref().is_some_and(|t| {
            matches!((&t.conversation, &reading.conversation),
                (Conversation::Named(ours), Conversation::Named(theirs)) if ours != theirs)
        }) {
            return true;
        }
        let Some(n) = self.waited_limit() else {
            return false;
        };
        reading.notice_elsewhere
            || (n.said.is_some()
                && reading.since_limit.is_some()
                && !notice_of(reading.since_limit.as_ref(), n.kind, n.said))
    }

    /// THE CLOCK THE LIMIT THIS POLICY WAITS ON NAMED, while its reset is still ahead of `now`.
    #[must_use]
    pub fn named_reset_clock(&self, now: Instant) -> Option<ResetClock> {
        let n = self.wall.as_ref().filter(|t| named_class(t.class))?.named?;
        n.said.filter(|_| n.at > now)
    }

    /// The limit [`Self::waits_on_named_reset`] waits on.
    #[must_use]
    pub fn waited_limit(&self) -> Option<WaitedLimit> {
        let n = self.wall.as_ref().filter(|t| named_class(t.class))?.named?;
        Some(WaitedLimit {
            kind: n.kind,
            at: n.at,
            said: n.said,
        })
    }

    #[must_use]
    pub(crate) fn read_remembered_wall(&self, mut reading: TurnEndReading) -> TurnEndReading {
        let plain = TurnEndReading {
            phase: Phase::Idle,
            wall: None,
            wall_message: String::new(),
            reset_at: None,
            reset_clock: None,
            resumes_by_itself: false,
            ..reading.clone()
        };
        let moved = self.wall.as_ref().is_some_and(|t| {
            (t.class == WallClass::Model
                && limit_conversation_ours(t, &reading)
                && t.named
                    .is_some_and(|n| n.moved_off_under(&reading, &t.said)))
                || (named_class(t.class)
                    && (reading.login_back
                        || reading.login_interrupted
                        || reading.login_screen.is_some()))
        });
        if reading.taskless
            || moved
            || self.limit_went_on(&plain, None)
            || self.limit_elsewhere(&plain)
        {
            return plain;
        }
        reading.resumes_by_itself &=
            reading.auto_continue == Some(aterm_phase::AutoContinue::Armed);
        reading.wall_remembered = true;
        reading
    }

    /// Seed a post-rotation usage-resume timestamp recorded by a previous loop.
    pub fn seed_account_resumed(&mut self, typed_unix: i64) {
        self.account_resumed = Some(
            self.account_resumed
                .map_or(typed_unix, |typed| typed.max(typed_unix)),
        );
    }

    /// The typed acts within the window ending at `now`.
    #[must_use]
    pub fn acts_in_window(&self, now: Instant) -> usize {
        self.acts
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) < self.timing.window)
            .count()
    }

    /// When the typing budget `cap` (`continue_per_hour`) next allows an act.
    #[must_use]
    pub fn budget_frees_at(&self, cap: u32, now: Instant) -> Instant {
        let window = self.timing.window;
        let mut within: Vec<Instant> = self
            .acts
            .iter()
            .copied()
            .filter(|&t| now.saturating_duration_since(t) < window)
            .collect();
        within.sort_unstable();
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        if cap == 0 || within.len() < cap {
            return now;
        }
        within
            .get(within.len() - cap)
            .map_or(now, |&t| (t + window).max(now))
    }

    /// How many acts of wall rule `rule` this policy typed within the window ending at `now`, and when the oldest of them leaves it.
    #[must_use]
    pub fn wall_acts_of(&self, rule: &str, now: Instant) -> (usize, Option<Instant>) {
        let window = self.timing.window;
        let mut n = 0;
        let mut oldest = None;
        for &(t, r) in &self.wall_acts {
            if r == rule && now.saturating_duration_since(t) < window {
                n += 1;
                oldest = oldest.or(Some(t + window));
            }
        }
        (n, oldest)
    }

    /// Short turns in a row.
    #[must_use]
    pub const fn short_streak(&self) -> u32 {
        self.short
    }

    /// The named program whose turns this short streak counts.
    #[must_use]
    pub const fn short_program(&self) -> Option<Program> {
        self.short_program
    }

    /// The replies no-work answers to acts settled to ([`reply_mark`]), the newest last.
    #[must_use]
    pub const fn replies(&self) -> Replies {
        self.replies
    }

    /// Which arm of ruling F's hold holds the acts now ([`Replies::held_by`]), whether or not a
    /// decision at this point reads it.
    #[must_use]
    pub fn held_by(&self) -> Option<crate::harness::acts::HeldBy> {
        self.replies.held_by()
    }

    /// The reset the newest answer counted showed ([`Self::repeats`]).
    #[must_use]
    pub const fn replies_reset(&self) -> Option<Instant> {
        self.replies_reset
    }

    /// Whether the next point's [`TurnEndReading::ran_tools`] weighs ([`Self::observe`]): an act's answer is read (an act awaited, a yield typed, a wall's answer settling), or a reply is kept.
    #[must_use]
    pub fn weighs_tools(&self) -> bool {
        self.awaiting.is_some_and(|a| !a.command)
            || self.yield_seed.is_some_and(|y| y.typed)
            || self.settling.is_some()
            || self.replies.last().is_some()
    }

    /// THE SAME REPLY AGAIN, a read wall's once its tries are spent, or [`NO_WORK_ACTS`] no-work answers in a row (past a read wall's own tries), the screen showing the newest kept: the policy chooses a paced recovery under Cruise. The route back since the last answer lifts both, and so does a date or clock reset since it, the one the last answer showed only while the wall's tries last (a reset moving forward with each answer keeps them); a relative reset, restated with each answer, lifts only the first while the wall's tries last.
    #[must_use]
    pub fn repeats(&self, reading: &TurnEndReading) -> bool {
        // AN OUTAGE WALL HOLDS NOTHING (v6 §14.17.5.2): while the screen shows it, it is waited
        // out, its answers no reply the agent gave again. A reply that shows no outage wall is
        // weighed as any other, a retry track standing behind it or not.
        if outage_shown(self, reading).is_some() {
            return false;
        }
        let Some(k) = self
            .replies
            .last()
            .filter(|k| k.mark == reply_mark(reading))
        else {
            return false;
        };
        let since = |r: Instant| self.replies_at.is_none_or(|at| at < r);
        let tries = k.tries.max(self.wall.as_ref().map_or(0, |t| t.attempts));
        let (dated, relative) = match self.reset_known(reading) {
            // The answers' own reset lifts only while the wall's tries last; a new one is a new wall.
            Some((r, true)) => (
                since(r) && (tries < FUTILE_AFTER_TRIES || !same_reset(self.replies_reset, r)),
                false,
            ),
            Some((r, false)) => (false, since(r) && tries < FUTILE_AFTER_TRIES),
            None => (false, false),
        };
        if dated || self.route_back(reading).is_some_and(since) {
            return false;
        }
        let spent = !k.walled || tries >= FUTILE_AFTER_TRIES;
        let run = if spent {
            NO_WORK_ACTS
        } else {
            NO_WORK_ACTS + FUTILE_AFTER_TRIES
        };
        (!relative && spent && k.holds()) || self.replies.run >= run
    }

    /// Whether the decision's arm for replies that repeat with no work takes the point at `reading`
    /// ([`Self::repeats`], a go-ahead owed or a switch's ask going ahead of it).
    fn replies_repeat(&self, reading: &TurnEndReading, now: Instant) -> bool {
        !self.login_resume
            && ((self.repeats(reading)
                && !self.go_ahead_untried(reading, now)
                && !self.switch_asks(reading))
                || self
                    .wall
                    .as_ref()
                    .and_then(|t| still_shown(t, reading).or_else(|| left_retry(t, reading)))
                    .is_some_and(|(_, shown)| self.repeats(&shown)))
    }

    /// Whether that arm gives the repeated reply another turn: an ordinary reply under Cruise, no
    /// wall behind it and no person's hour on the go-ahead.
    fn repeat_gets_a_turn(&self, reading: &TurnEndReading, now: Instant) -> bool {
        reading.autopilot == Autopilot::Cruise
            && self.wall.is_none()
            && self.replies.last().is_some_and(|reply| !reply.walled)
            && !self.go_ahead_held_at(reading, now)
    }

    /// WHETHER THE STOP FOR REPLIES THAT REPEAT WITH NO WORK HOLDS THE LOOP'S TYPING AT `reading`:
    /// what the decision's own arm does, so a host's step at this point reads what the loop itself
    /// would do. A repeat that gets another turn stops nothing.
    #[must_use]
    pub fn replies_stop(&self, reading: &TurnEndReading, now: Instant) -> bool {
        self.replies_repeat(reading, now) && !self.repeat_gets_a_turn(reading, now)
    }

    /// The parsed reset of the limit at this point, the read wall's, else the one its track names, and whether it is a date or clock rather than a time from its notice (`in 45s`).
    fn reset_known(&self, reading: &TurnEndReading) -> Option<(Instant, bool)> {
        match reading.reset_at {
            Some(at) => Some((at, reading.wall.is_none() || reading.reset_clock.is_some())),
            None => self
                .wall
                .as_ref()
                .and_then(|t| left_named(t, reading))
                .map(|n| (n.at, n.said.is_some())),
        }
    }

    /// When the host measured the route up ([`Reach::Up`]), at an API error the route caused.
    fn route_back(&self, reading: &TurnEndReading) -> Option<Instant> {
        let kind = reading
            .wall
            .or_else(|| self.wall.as_ref().and_then(|t| t.kind));
        match (kind, reading.reach) {
            (
                Some(WallKind::ApiError {
                    cause: ApiCause::Unreachable | ApiCause::CutOff,
                    ..
                }),
                Reach::Up { since },
            ) => Some(since),
            _ => None,
        }
    }

    /// Where the task stands.
    #[must_use]
    pub const fn done_reports(&self) -> u8 {
        self.done
    }

    /// Whether the conversation's record is read at `r` ([`TurnEndReading::finished_record`]).
    #[must_use]
    pub fn reads_record(&self, r: &TurnEndReading) -> bool {
        self.off_landing
            .is_some_and(|mark| said_mark(r.said_tail.as_deref()) != mark)
            || r.worked.is_some()
            || r.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            || self.point_at.is_none()
            || (r.wall.is_none() && self.wall.as_ref().is_some_and(|t| named_class(t.class)))
            || self.untaken().is_some()
    }

    /// THE TASK IS DONE.
    #[must_use]
    pub const fn task_done(&self) -> bool {
        self.done >= 2
    }

    /// A LOST LOGIN'S `/login` WAS LEFT BEFORE IT SIGNED IN (`⎿  Login interrupted` read while its
    /// track stands, [`LoginBooks::cancelled`]).
    #[must_use]
    pub fn login_cancelled(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Auth && t.login.cancelled)
    }

    /// A PERSON'S `/login` AT A USAGE OR MODEL LIMIT WAS LEFT BEFORE IT SIGNED IN (`⎿  Login interrupted` read while the limit's track stands).
    #[must_use]
    pub fn login_cancelled_on_limit(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && t.login.cancelled)
    }

    /// WHETHER THE HOST'S CROSS-TAB READ `shared` SAYS THE SESSION'S LOGIN CAME BACK IN ANOTHER TAB FOR THIS TAB ([`LoginShared::Back`]): not once the lost login's track delivered its turn again ([`RULE_LOGIN_BACK`]), after which the wall met again is signed in here and a `Back` still read is the one already delivered. The turn end and the approval loop's picker gate ([`super::login::picker_gate`]) both read it here, so the picker and the hand-off of one `/login` are judged alike.
    #[must_use]
    pub fn login_back_from(&self, shared: Option<&LoginShared>) -> bool {
        matches!(shared, Some(LoginShared::Back { .. }))
            && !self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth && t.login.back_typed)
    }

    /// WHEN THIS LOOP TYPED `/login` INTO THE LOST LOGIN STANDING ([`LoginBooks::typed_at`]), its sign-in not left ([`Self::login_typed_left`]): a dialog still drawn after the loop's Esc out of it is still the `/login` the loop typed, and one drawn after that dialog was read gone is a person's own.
    #[must_use]
    pub fn login_typed_at(&self) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| {
                t.class == WallClass::Auth
                    && t.attempts > 0
                    && !t.login.cancelled
                    && !t.login.closed
            })
            .and_then(|t| t.login.typed_at)
    }

    /// UNTIL WHEN THE DIALOG THE LOOP BACKED OUT OF MAY STILL BE CLOSING: [`TurnEndTiming::take_within`] from its first Esc, doubled at each Esc since and at most [`BACK_OUT_PAUSE_MAX`], while the dialog is not read gone. Past it, a dialog still drawn gets the Esc again ([`super::login::backs_out`]).
    #[must_use]
    pub fn login_closing(&self, now: Instant) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth && !t.login.cancelled && !t.login.closed)
            .and_then(|t| {
                let pause = super::super::ladder::doubling(
                    self.timing.take_within,
                    t.login.backs.saturating_sub(1),
                    BACK_OUT_PAUSE_MAX,
                );
                t.login.backed_out.map(|at| later(at, pause))
            })
            .filter(|until| now < *until)
    }

    /// UNTIL WHEN A PERSON'S OWN `/login` aterm closed with its Esc
    /// ([`super::login::over_limit_act`]) may still be closing: [`TurnEndTiming::take_within`] from
    /// the Esc, doubled at each Esc since the dialog was last read gone and at most
    /// [`BACK_OUT_PAUSE_MAX`]. Past it, a dialog still drawn gets the Esc again, up to
    /// [`super::login::OVER_LIMIT_BACKS_MAX`].
    #[must_use]
    pub fn login_over_limit_closing(&self, now: Instant) -> Option<Instant> {
        let left = self.standing_login.as_ref()?;
        let pause = super::super::ladder::doubling(
            self.timing.take_within,
            left.backs.saturating_sub(1),
            BACK_OUT_PAUSE_MAX,
        );
        left.backed_out
            .map(|at| later(at, pause))
            .filter(|until| now < *until)
    }

    /// When this loop first read the `/login` dialog standing now, since a screen last read none.
    #[must_use]
    pub fn login_dialog_seen(&self) -> Option<Instant> {
        self.standing_login.as_ref().map(|left| left.seen)
    }

    /// The Escs aterm pressed at a person's own `/login` ([`super::login::over_limit_act`]) since a
    /// screen last read no `/login` dialog.
    #[must_use]
    pub fn login_over_limit_backs(&self) -> u32 {
        self.standing_login.as_ref().map_or(0, |left| left.backs)
    }

    /// The loop's Esc out of the dialog of the `/login` it typed went in at `now`: the turn end's
    /// ([`TurnEndAction::Cancel`]), or the approval loop's at the method picker
    /// ([`super::login::back_out_of_picker`]).
    pub fn login_backed_out_at(&mut self, now: Instant) {
        if let Some(t) = self
            .wall
            .as_mut()
            .filter(|t| t.class == WallClass::Auth && t.attempts > 0)
        {
            t.login.backed_out = Some(now);
            t.login.backs = t.login.backs.saturating_add(1);
        }
    }

    /// WHETHER THE LOOP'S OWN ESC LEFT THE `/login` IT TYPED, at `since` or later: it backed out
    /// of that `/login`'s dialog then ([`Self::login_backed_out_at`]) and the `/login` was left
    /// since ([`Self::login_typed_left`]). The login exception that then ends is cleared by that
    /// back-out ([`super::exception::LOGIN_BACKED_OUT`]), not by a sign-in or the agent working.
    #[must_use]
    pub fn login_backed_out_since(&self, since: Instant) -> bool {
        self.login_typed_left()
            && self
                .wall
                .as_ref()
                .and_then(|t| t.login.backed_out)
                .is_some_and(|at| at >= since)
    }

    /// THE `/login` THIS LOOP TYPED INTO THE LOST LOGIN STANDING WAS LEFT: the dialog the loop
    /// backed out of read gone ([`LoginBooks::closed`]), or `⎿  Login interrupted` read, whether
    /// the loop's Esc or a person's closed it ([`LoginBooks::cancelled`]). Over a roster row's
    /// login the tab then waits on the row's own sign-in ([`login_wall_waits`]).
    #[must_use]
    pub fn login_typed_left(&self) -> bool {
        self.wall.as_ref().is_some_and(|t| {
            t.class == WallClass::Auth && t.attempts > 0 && (t.login.cancelled || t.login.closed)
        })
    }

    /// UNTIL WHEN THE `/login` THIS LOOP TYPED MAY STILL BE OPENING ITS DIALOG ([`TurnEndTiming::take_within`] from the keys).
    #[must_use]
    pub fn login_opening(&self, now: Instant) -> Option<Instant> {
        self.login_typed_at()
            .map(|at| later(at, self.timing.take_within))
            .filter(|until| now < *until)
    }

    /// `/login`'s success state was answered with its Enter and has not left since.
    #[must_use]
    pub const fn login_confirmed(&self) -> bool {
        self.login_confirmed
    }

    /// The open work the current point's done claim names, which its act names as a suffix ([`open_work_suffix`]; empty: none).
    #[must_use]
    pub fn open_items(&self) -> &[&'static str] {
        &self.open
    }

    /// What the session's repository held that no remote main has at the current point's done claim ([`Self::repo`]; `None`: nothing found, or the probe could not read the repository, [`Self::repo_unread`]).
    #[must_use]
    pub const fn repo_owed(&self) -> Option<&crate::harness::repo_work::RepoWork> {
        self.repo.found()
    }

    /// Whether the repository probe at the current point's done claim could not read the repository ([`crate::harness::repo_work::RepoRead::Unread`]).
    #[must_use]
    pub const fn repo_unread(&self) -> bool {
        self.repo.is_unread()
    }

    /// THE NEEDS YOU STANDING ([`needs::needs_you`], [`Self::needs`]), as the worker's last report named them.
    #[must_use]
    pub fn standing_needs(&self) -> &[String] {
        &self.needs.standing
    }

    /// The key of the needs standing ([`needs::needs_key`]; `0` for none).
    #[must_use]
    pub const fn needs_key(&self) -> u64 {
        self.needs.key
    }

    /// A LOOP BEFORE THIS ONE LEFT `needs` STANDING at a handover, as the window was told them
    /// ([`crate::supervise::IdleHost::needs_told`]): they stand, and the menu bar shows them, as
    /// its last tell named them, and the ack is owed for them again unless its books show an act
    /// typed it ([`Self::seed_needs_acked`]).
    pub fn seed_needs(&mut self, needs: &[String]) {
        let key = needs::needs_key(needs);
        self.needs = Needs {
            standing: needs.to_vec(),
            key,
            acked: (key != 0 && self.acked_seed == key).then_some(key),
            ..Needs::default()
        };
        self.needs.reshow();
    }

    /// THE NEEDS SET WHOSE ACK A LOOP BEFORE THIS ONE TYPED, as its books' note says
    /// ([`Self::needs_acked`]; `0` for none): a set adopted from the window that is the one owes
    /// no ack again ([`Self::seed_needs`]).
    pub const fn seed_needs_acked(&mut self, key: u64) {
        self.acked_seed = key;
    }

    /// The key of the needs whose ack an act typed ([`NEEDS_ACK`]): the set
    /// [`Self::needs_ack_owed`] judges, while no ack is owed for it; `0` while none stands or its
    /// ack is owed.
    #[must_use]
    pub fn needs_acked(&self) -> u64 {
        let held = self.needs.answered_unnamed();
        let key = needs::needs_key(&self.needs.less(&[held.as_slice()]));
        self.needs
            .acked
            .filter(|&acked| key != 0 && acked == key)
            .unwrap_or(0)
    }

    /// A NEW SET OF NEEDS STANDS UNACKED: the needs an act other than the go-ahead leaves on the
    /// menu bar ([`GoAheadVerdict::Other`]) are not the set last acked.
    #[must_use]
    pub fn needs_ack_owed(&self) -> bool {
        let held = self.needs.answered_unnamed();
        self.ack_owed_of(&self.needs.less(&[held.as_slice()]))
    }

    /// Whether `shown` is a set of needs not acked.
    fn ack_owed_of(&self, shown: &[String]) -> bool {
        let key = needs::needs_key(shown);
        key != 0 && self.needs.acked != Some(key)
    }

    /// THE NEEDS ON THE MENU BAR (DESIGN-harness-autopilot §15.6): those standing
    /// ([`Self::standing_needs`]) less the lines the owner's standing go-ahead answers.
    #[must_use]
    pub fn shown_needs(&self) -> &[String] {
        &self.needs.shown
    }

    /// The key of the needs on the menu bar ([`Self::shown_needs`]; `0` for none).
    #[must_use]
    pub const fn shown_key(&self) -> u64 {
        self.needs.shown_key
    }

    /// How many needs standing the go-ahead answers ([`Self::shown_needs`]).
    #[must_use]
    pub fn needs_answered(&self) -> usize {
        self.needs
            .standing
            .len()
            .saturating_sub(self.needs.shown.len())
    }

    /// [`Self::needs_ack_owed`] for the go-ahead that answers `past`: the needs it leaves on the
    /// menu bar are not the set last acked.
    #[must_use]
    pub fn needs_ack_owed_past(&self, past: &[String]) -> bool {
        self.ack_owed_of(&self.needs.less(&[self.needs.answered.as_slice(), past]))
    }

    /// WHAT `action`, decided on `reading` under `cfg` at `now`, DID WITH THE GO-AHEAD'S LINES
    /// ([`GoAheadVerdict`]): `refused` where the exception it raises is the go-ahead's refusal
    /// ([`super::exception::Exception::refuses`]). A point answers them where it types the
    /// go-ahead owed ([`Self::go_ahead_owed`]) or waits out the back-off before it, and where it
    /// types Land's instruction over the park the go-ahead answers once it is typed
    /// ([`Self::go_ahead_first`], 2026-10-30 review 9). A decision is judged on the act it types:
    /// a restart on its `otherwise`, which the loop types where the restart waits for a quiet
    /// moment or is not made (a restart made types none and puts the lines back,
    /// [`Self::restarted`]); a draft's Enter ([`TurnEndAction::Submit`]) on the act it sends in
    /// place of, which is the loop's own line of that act where it wrote one and left it
    /// unsent. So the lines a go-ahead answers are always ones its decision judged
    /// ([`Self::acted`]).
    #[must_use]
    pub fn go_ahead_verdict(
        &self,
        action: &TurnEndAction,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        refused: bool,
        now: Instant,
    ) -> GoAheadVerdict {
        if refused {
            return GoAheadVerdict::Refused;
        }
        if let TurnEndAction::Restart { otherwise, .. } = action {
            return self.go_ahead_verdict(otherwise, reading, cfg, false, now);
        }
        let park = match action {
            TurnEndAction::Type { rule_id, .. } | TurnEndAction::Submit { rule_id }
                if *rule_id == RULE_GO_AHEAD =>
            {
                self.go_ahead_owed(reading, now)
            }
            TurnEndAction::WaitUntil {
                wait: WaitKind::BackOff { .. },
                ..
            } => self.go_ahead_owed(reading, now),
            TurnEndAction::Type { rule_id, .. } | TurnEndAction::Submit { rule_id }
                if *rule_id == RULE_LAND =>
            {
                self.go_ahead_first(reading, cfg, now)
            }
            _ => None,
        };
        match park.filter(|_| !self.go_ahead_spent(now)) {
            Some(park) => GoAheadVerdict::Answers(park.needs),
            None => GoAheadVerdict::Other,
        }
    }

    /// THE DECISION'S [`GoAheadVerdict`], judged on the needs the menu bar shows: where the reply
    /// names needs, the lines answered before stay off only where this decision answers them too
    /// (`Needs::keep_answered`).
    pub fn go_ahead_judged(&mut self, verdict: GoAheadVerdict) {
        match verdict {
            GoAheadVerdict::Answers(lines) => {
                self.needs.keep_answered(&lines);
                self.needs.answering = lines;
            }
            GoAheadVerdict::Refused => {
                self.needs.answered.clear();
                self.needs.answering.clear();
            }
            GoAheadVerdict::Other => {
                self.needs.answering.clear();
                self.needs.keep_answered(&[]);
            }
        }
        self.needs.reshow();
    }

    /// A BREAK THAT JUDGES NO GO-AHEAD (no carry can go there: not Cruise, the continuation off,
    /// an attended run, no window host): nothing is being answered, and where its turn's reply
    /// names needs, the lines answered before stay off only where `park`, the park the go-ahead
    /// would answer on that turn ([`Self::go_ahead_at_break`], past an irreversible ask, the
    /// person's hold and answers off), still answers them (`Needs::keep_answered`; 2026-10-30
    /// review 9, second pass). A turn that restates the park keeps its line off, as nothing types
    /// at such a break to answer it again; a person's need that cleans to it shows.
    pub fn go_ahead_unjudged(&mut self, park: Option<&go_ahead::Park>) {
        self.needs.answering.clear();
        self.needs
            .keep_answered(park.map_or(&[][..], |p| p.needs.as_slice()));
        self.needs.reshow();
    }

    /// THE GO-AHEAD DECIDED WAS NOT SUBMITTED: the lines it would have answered are back on the
    /// menu bar. Whether any were held for it.
    pub fn go_ahead_unwritten(&mut self) -> bool {
        if self.needs.answering.is_empty() {
            return false;
        }
        self.needs.answering.clear();
        self.needs.reshow();
        true
    }

    /// THE GO-AHEAD DECIDED WAS SUBMITTED on a park that answers `past`: the lines it answers are
    /// off the menu bar for good, until a later reply names them under another decision.
    fn go_ahead_answered(&mut self, past: &[String]) {
        self.needs.answer(past);
    }

    fn needs_destroy(&self) -> bool {
        self.needs
            .standing
            .iter()
            .any(|n| destructive_word(&n.to_lowercase()).is_some())
    }

    /// The task was landed.
    #[must_use]
    pub const fn landed(&self) -> bool {
        self.task_done() && self.landed
    }

    /// The Land episode in hand has had its instruction ([`LAND`]) typed, and how many nudges ([`LAND_NUDGE`]) since.
    #[must_use]
    pub const fn land_episode(&self) -> (bool, u32) {
        (self.land.typed, self.land.nudges)
    }

    /// WHY LAND STILL HOLDS ([`LandHeld`]).
    #[must_use]
    pub fn land_held(&self) -> Option<LandHeld> {
        if !self.land.typed
            || self.land.hold.relanded
            || (self.gave_up.is_some() && !self.other_turn)
            || self.face() != TaskFace::Open
        {
            return None;
        }
        let base = LandHeld {
            since_ms: self.land.hold.since_ms,
            nudges: self.land.nudges.saturating_sub(self.land.hold.base),
            ..LandHeld::default()
        };
        // A need the go-ahead answers holds nothing the menu bar shows (§15.6).
        let answered = !self.needs.standing.is_empty() && self.needs.shown.is_empty();
        Some(self.land.legs.map_or(base, |legs| LandHeld {
            read: true,
            legs: if answered {
                legs.legs & !HoldLeg::Needs.bit()
            } else {
                legs.legs
            },
            open: legs.open,
            changed: legs.changed,
            untracked: legs.untracked,
            stashes: legs.stashes,
            unpushed: legs.unpushed,
            unlanded: legs.unlanded,
            sub_unpushed: legs.sub_unpushed,
            parked: legs.parked,
            left: legs.left,
            ..base
        }))
    }

    /// THE HOLD TO TELL THE HOST.
    pub fn held_moved(&mut self, here: bool) -> Option<Option<LandHeld>> {
        let held = self.land_held().filter(|_| here);
        (self.held_told.replace(held) != Some(held)).then_some(held)
    }

    /// THE LOOP ENDS (`Session::tell_hold_ended`).
    pub const fn held_ended(&mut self) -> bool {
        matches!(self.held_told.replace(None), Some(Some(_)))
    }

    /// THE HOLD'S OWN WORDS (§14.17 item 1, slice 4b): what the agent said is left, quoted, and
    /// the files git found, at the yield whose legs the hold reads; `None` where the hold reads
    /// no yield's legs or holds no word. Its own comparison, apart from [`LandHeld`]'s, so an
    /// item the agent rewords within one word family refreshes the quote.
    #[must_use]
    pub fn land_said(&self) -> Option<&HoldSaid> {
        self.land_held()
            .filter(|held| held.read)
            .map(|_| &self.land_said)
            .filter(|said| !said.is_empty())
    }

    /// THE HOLD'S WORDS TO TELL THE HOST, where they moved since the last tell
    /// ([`crate::supervise::IdleHost::land_said`]).
    pub fn said_moved(&mut self, here: bool) -> Option<Option<HoldSaid>> {
        let said = self.land_said().filter(|_| here).cloned();
        (self.said_told != said).then(|| {
            self.said_told.clone_from(&said);
            said
        })
    }

    /// THE LOOP ENDS: whether words were told that the host must now drop.
    pub fn said_ended(&mut self) -> bool {
        self.said_told.take().is_some()
    }

    /// The legs and words of the land act's yield read now ([`hold_at_yield`]); none once landed.
    /// A yield the left leg holds counts toward [`LEFT_HOLDS`] ([`LandEpisode::left_counted`]),
    /// and its rows are what the act's left suffix names ([`Self::left_said`]).
    fn set_legs(&mut self, reading: &TurnEndReading, answer: Option<&land_check::CheckAnswer>) {
        if self.landed() {
            self.land.legs = None;
        } else {
            let left = self.left_at_yield(reading, answer.is_some());
            let (held, said) = hold_at_yield(reading, answer, self.land_listed.as_deref(), left);
            let counted = held.holds(HoldLeg::Left);
            if counted {
                self.land.left_holds = self.land.left_holds.saturating_add(1);
            }
            self.land.left_counted = counted.then(|| reply_mark(reading));
            if reading.autopilot == Autopilot::Land {
                self.left_said = held.left;
            }
            self.land.legs = Some(held);
            self.land_said = said;
        }
    }

    /// A YIELD READ AGAIN BEFORE ITS ACT is judged as it was the first time: the hold the left leg
    /// counted for it and nothing settled yet ([`LandEpisode::left_counted`]) is taken back, and
    /// [`Self::set_legs`] counts it again where it still holds.
    fn uncount_left(&mut self, reading: &TurnEndReading) {
        if self.land.left_counted == Some(reply_mark(reading)) {
            self.land.left_holds = self.land.left_holds.saturating_sub(1);
            self.land.left_counted = None;
        }
    }

    /// THE LEFT HOLD NOTHING SETTLED YET IS SETTLED ([`LandEpisode::left_counted`]): an act typed,
    /// or a turn run since its yield, leaves that yield nothing to judge again, so its hold stands
    /// counted, in the books a loop that follows seeds too.
    const fn settle_left(&mut self) {
        self.land.left_counted = None;
    }

    /// WHAT KEEPS A ROW RUNNING AT `reading`: a `keep <token>` line under one of the episode's
    /// defenses ([`LandEpisode::kept`]) or under the reply's own ([`needs::kept_tokens`]).
    fn left_kept(
        &self,
        reading: &TurnEndReading,
    ) -> impl Fn(&aterm_uds::process::Identity) -> bool {
        let said = reading
            .said_tail
            .as_deref()
            .filter(|_| !reading.leftover.rows().is_empty())
            .map(needs::kept_tokens)
            .unwrap_or_default();
        let episode = self.land.kept;
        move |row| episode.contains(&Some(*row)) || said.contains(row)
    }

    /// The agent's left rows at `reading` nothing keeps running ([`Self::left_kept`]).
    fn left_unkept(&self, reading: &TurnEndReading) -> u16 {
        let n = reading.leftover.unkept(self.left_kept(reading)).count();
        u16::try_from(n).unwrap_or(u16::MAX)
    }

    /// WHETHER THE LEFT LEG HOLDS LAND AT `reading` ([`HoldLeg::Left`]): under Land, a row of the
    /// agent's nothing keeps, while the episode's left holds are fewer than [`LEFT_HOLDS`]. A view
    /// that could not say, none asked, or an episode that lost what its defenses kept
    /// ([`LandEpisode::kept_over`]) holds nothing.
    fn left_holds(&self, reading: &TurnEndReading) -> bool {
        reading.autopilot == Autopilot::Land
            && !self.land.kept_over
            && self.land.left_holds < LEFT_HOLDS
            && self.left_unkept(reading) > 0
    }

    /// HOW MANY OF THE AGENT'S LEFT ROWS HOLD THE LAND ACT'S YIELD AT `reading`: where the leg holds
    /// ([`Self::left_holds`]) at a `Nothing left:` defense, where it would owe the check, or at the
    /// check's answer (`answered`), where its `LANDED` lands nothing over them; none at any other
    /// reply, whose own legs hold it.
    fn left_at_yield(&self, reading: &TurnEndReading, answered: bool) -> u16 {
        let defends = reading
            .said_tail
            .as_deref()
            .and_then(needs::defense_line)
            .is_some();
        if (answered || defends) && self.left_holds(reading) {
            self.left_unkept(reading)
        } else {
            0
        }
    }

    /// THE DONE LINE'S `left=` WORD of the landing this loop made: what its yield read of the
    /// agent's left rows, the episode's kept tokens kept; `None` where nothing was asked there.
    #[must_use]
    pub const fn landed_left(&self) -> Option<crate::harness::leftover::LeftWord> {
        self.landed_left
    }

    /// THE SESSION'S WORD AS A POINT READ IT (v6 §14.17 item 3), the word a wake reported riding
    /// the point's reading, and whether a person set it (`by_person`): a person's move to Land
    /// from another word while the replies hold ([`Self::held_by`]) is their switch over a held
    /// tab, which counts as their turn, so Land's one ask is owed ([`SwitchAsk::Owed`]). A move no
    /// person made (a lead's `aterm ctl` write, the timer's fire) keeps limit 1. Any other word
    /// drops a switch's ask, owed or typed. A loop's first word read is no switch: no move was
    /// seen.
    fn note_word(&mut self, word: Autopilot, by_person: bool) {
        let before = self.word_seen.replace(word);
        if word != Autopilot::Land {
            self.switch_ask = SwitchAsk::None;
            self.switch_seen = false;
            return;
        }
        let moved = by_person && before.is_some_and(|b| b != Autopilot::Land);
        if moved {
            // The person's own move to Land begins a Land episode: one an Off no person set kept
            // over its rest ([`OffStood::Resting`]) is over, as at their switch from Cruise.
            self.leave_land_episode();
        }
        self.switch_seen |= moved;
        self.weigh_switch();
    }

    /// A SWITCH TO LAND SEEN ([`Self::note_word`]) OVER A HOLD: Land's one ask is owed, unless it
    /// was spent on the reply that holds ([`Self::switch_spent`]). A switch made while the agent
    /// worked is weighed again at the point its answer is counted ([`Self::observe`]), and then
    /// dropped: where that answer holds, Land's instruction never reached the agent either, so the
    /// switch counts as the person's turn all the same; where it does not, Land types its
    /// instruction as at any switch (v6 §14.10).
    fn weigh_switch(&mut self) {
        if self.switch_seen && self.switch_ask == SwitchAsk::None && self.held_by().is_some() {
            if self.replies.last().map(|k| k.mark) != self.switch_spent {
                self.switch_ask = SwitchAsk::Owed;
            }
            self.switch_seen = false;
        }
    }

    /// WHETHER THE PERSON'S SWITCH TO LAND WOULD ASK OVER THE HOLD AT `reading` (v6 §14.17 item
    /// 3), for the stop's words, which name the switch only here: the replies hold the agent's own
    /// reply, which no read wall said and no ask was spent on, and the word is not Land already
    /// (no switch to make). A wall's hold and a go-ahead's are kept by their own arms, which
    /// answer a switch with nothing, so their exceptions never name it
    /// ([`super::exception::Exception::lifted_by_switch`]).
    #[must_use]
    pub fn switch_would_ask(&self, reading: &TurnEndReading) -> bool {
        reading.autopilot != Autopilot::Land
            && self.switch_ask == SwitchAsk::None
            && self.repeats(reading)
            && self
                .replies
                .last()
                .is_some_and(|k| !k.walled && Some(k.mark) != self.switch_spent)
    }

    /// The person's switch to Land over a held tab and its one ask ([`SwitchAsk`]).
    #[must_use]
    pub const fn switch_ask(&self) -> SwitchAsk {
        self.switch_ask
    }

    /// WHETHER THE DECISION AT `reading` TYPES THE SWITCH'S ONE ASK (v6 §14.17 item 3): owed under
    /// Land while the replies still hold the agent's own reply, the screen showing it. A wall's
    /// hold keeps limit 1 without asking here: a wall on the screen (a limit, an outage) is
    /// decided, and its repeat answered with nothing, before any act past the walls, as is its
    /// notice once it leaves while its track stands (`act_past_walls`' wall arm).
    #[must_use]
    pub fn switch_asks(&self, reading: &TurnEndReading) -> bool {
        self.switch_ask == SwitchAsk::Owed
            && reading.autopilot == Autopilot::Land
            && self.repeats(reading)
    }

    /// THE SWITCH'S ASK TO TELL THE HOST: `Some(standing)` where it moved since the host was last
    /// told; standing from its act until its answer is read ([`SwitchAsk::Typed`]).
    pub fn switch_moved(&mut self) -> Option<bool> {
        let now = self.switch_ask == SwitchAsk::Typed;
        (std::mem::replace(&mut self.switch_told, now) != now).then_some(now)
    }

    /// THE LOOP ENDS: whether the host was told a switch's ask stands, which it no longer does.
    pub const fn switch_ended(&mut self) -> bool {
        std::mem::replace(&mut self.switch_told, false)
    }

    /// AN ACT TYPED UNDER `rule` WAS LEDGERED at `at_ms` (unix ms, the row's own stamp).
    pub fn ledgered(&mut self, rule: &str, at_ms: i64) {
        if self
            .land
            .unstamped
            .is_some_and(|by| by == rule || is_land_rule(rule))
        {
            self.land.hold.since_ms = Some(at_ms);
            self.land.unstamped = None;
        }
    }

    /// …AND WHERE THAT EPISODE'S HOLD BEGAN.
    pub const fn seed_land_hold(&mut self, hold: HoldStart) {
        if self.land.typed {
            self.land.hold = hold;
        }
    }

    const fn hold_landed(&mut self) {
        self.land.hold = HoldStart::LANDED;
        self.land.unstamped = None;
        self.land.legs = None;
    }

    /// THE LAND CHECK'S ANSWER, read at its yield ([`land_check::land_check_answer`]): `LANDED`
    /// landed the task where the repository read clean (else Land's nudge carries what the
    /// repository holds); a need alone over a clean repository is a rest, Land typing nothing
    /// until someone else's turn; anything else is the agent's own list, which Land's next
    /// prompt quotes.
    fn land_answered(&mut self, answer: &land_check::CheckAnswer, reading: &TurnEndReading) {
        self.land.check_owed = false;
        self.land.owed_mark = None;
        self.land_hint = None;
        self.land.answered = matches!(
            answer,
            land_check::CheckAnswer::Left(_)
                | land_check::CheckAnswer::Needs { rest_landed: false }
        );
        self.land_listed = match answer {
            // `LANDED` lands the work and retracts no need: a need stands until the agent's own
            // `NEEDS YOU: nothing` takes it down (its last word, [`needs::standing_before`]), so
            // one its defense raised stays quietly on the menu bar beside the landing, as it
            // stands over a repository read dirty, where nothing lands.
            land_check::CheckAnswer::Landed => None,
            land_check::CheckAnswer::Needs { rest_landed: true } => {
                self.land.rest = reading.repo.is_clean() && !self.landed();
                self.land.rest_told = false;
                None
            }
            land_check::CheckAnswer::Needs { rest_landed: false } => reading
                .said_tail
                .as_deref()
                .map(|t| needs::without_needs(t).trim().to_string())
                .filter(|l| !l.is_empty()),
            land_check::CheckAnswer::Left(list) => (!list.is_empty()).then(|| list.clone()),
        };
    }

    /// A LIST REWORDED IN A REPLY TO LAND'S NUDGE (§14.17 item 1, slice 4b): while the agent's
    /// list stands ([`Self::land_listed`]: no real work and no new check since), a land act's
    /// reply that is not the check's answer, holds no `Nothing left:` defense and is not
    /// `LANDED` is the agent's newest answer, so its body, its needs taken out, becomes the list
    /// Land's next prompt quotes and the hold quotes: said again, Land holds on it, quoted. A
    /// defense, `LANDED`, a reply of needs alone or a question keeps the standing list: Land
    /// answers a question, which says nothing of what is left.
    fn land_relisted(&mut self, reading: &TurnEndReading) {
        if self.land_listed.is_none() {
            return;
        }
        let Some(tail) = reading.said_tail.as_deref() else {
            return;
        };
        if needs::defense_line(tail).is_some()
            || land_check::landed_reply(Some(tail))
            || reading.asked().is_some()
        {
            return;
        }
        let body = needs::without_needs(tail);
        let body = body.trim();
        if !body.is_empty() {
            self.land_listed = Some(body.to_string());
        }
    }

    /// A DEFENSE AT A LAND ACT'S YIELD owes the land check, once per defense, over a clean
    /// repository read and no row of the agent's left running unkept ([`Self::left_holds`]); a
    /// dirty or unread repository gets the repository's facts instead, and a left row the left
    /// suffix. The tokens its keep lines name stay kept for the rest of the episode while they run
    /// ([`LandEpisode::keep`]).
    fn land_defended(&mut self, reading: &TurnEndReading) {
        let tail = reading.said_tail.as_deref();
        let mark = reply_mark(reading);
        let running = match &reading.leftover {
            crate::harness::leftover::LeftRead::Rows(rows) => Some(rows.as_slice()),
            crate::harness::leftover::LeftRead::NotAsked
            | crate::harness::leftover::LeftRead::Unchecked => None,
        };
        self.land
            .keep(&tail.map(needs::kept_tokens).unwrap_or_default(), running);
        let left = self.left_holds(reading);
        let owed = !self.landed()
            && reading.repo.is_clean()
            && !left
            && tail.and_then(needs::defense_line).is_some()
            && !self.land.audited(mark);
        self.land.check_owed = owed;
        self.land.answered = false;
        self.land.owed_mark = owed.then_some(mark);
        self.land_hint = owed
            .then(|| tail.and_then(land_check::hint_clause))
            .flatten();
        self.land_owed_tail = owed.then(|| tail.map(land_check::audited)).flatten();
    }

    /// THE LAND CHECK IS OWED at this point ([`RULE_LAND_CHECK`]).
    #[must_use]
    pub const fn land_check_owed(&self) -> bool {
        self.land.check_owed
    }

    /// THE LAND CHECK WAS LAND'S LAST ACT: the next judged reply is its answer, whose yield the
    /// repository is read at ([`super::super::turn_end_loop`]'s probe).
    #[must_use]
    pub const fn land_check_awaited(&self) -> bool {
        self.land.check_pending
    }

    /// LAND RESTS ON A NEED: the check's answer named only what a person must give.
    #[must_use]
    pub const fn land_resting(&self) -> bool {
        self.land.rest
    }

    /// THE REST IS UNTOLD: true once per rest, for the loop's one journal line.
    pub const fn rest_untold(&mut self) -> bool {
        let untold = self.land.rest && !self.land.rest_told;
        if untold {
            self.land.rest_told = true;
        }
        untold
    }

    /// THE DEFENSE THE LAST LAND CHECK AUDITED, as the reply said it (at most
    /// [`land_check::AUDITED_CAP`] bytes): what the landing's receipt quotes.
    #[must_use]
    pub fn land_audited(&self) -> Option<&str> {
        self.land_audited.as_deref()
    }

    /// THE AGENT'S OWN LIST OF WHAT IS LEFT, its last answer to the check ([`land_check`]) or its
    /// newest rewording of it in a reply to Land's nudge.
    #[must_use]
    pub fn land_listed(&self) -> Option<&str> {
        self.land_listed.as_deref()
    }

    pub(crate) fn park_off(&mut self) {
        self.park(false);
    }

    /// [`Self::park_off`], Land's episode and the needs standing kept where `keep_land`
    /// ([`OffStood::Resting`]).
    fn park(&mut self, keep_land: bool) {
        // A landing holds past the park only while its words do
        // ([`Self::off_landing`]).
        if self.landed() {
            self.off_landing = Some(self.landed_mark);
        }
        if !keep_land {
            self.needs = Needs::default();
            self.leave_land_episode();
        }
        self.leave_cruise_episode();
        self.replies = Replies::default();
        self.switch_ask = SwitchAsk::None;
        self.switch_seen = false;
        self.switch_spent = None;
        self.replies_at = None;
        self.settling = None;
        self.carrying = false;
        self.carry_at_attach = false;
        if let Some(episode) = &mut self.landed_seed {
            *episode = false;
        }
        if let Some(a) = &mut self.awaiting {
            a.off_stood = true;
        }
    }

    /// OFF SET BY NO PERSON (`autopilot_by=` not `human:…`: a lead's `meta set`, the Land timer, a
    /// harness): [`Self::park_off`], the hold kept (the post-landing review, 2026-11-01). Only the
    /// person's own switch lifts a hold (v6 §14.17 item 3), so the replies that hold and the ask
    /// spent over them survive the park, and so does Land's rest on a need
    /// ([`Self::land_resting`]), with its land episode and the needs standing
    /// ([`OffStood::Resting`]): the same reply read again past Off types nothing, under Land or
    /// Cruise, the need stays on the menu bar, and the person's own switch to Land over the hold
    /// is a move from Off ([`Self::note_word`]), which asks once over held replies and lifts the
    /// rest as their own switch does. Returns what the park kept, for its ledger row.
    pub(crate) fn park_off_held(&mut self) -> OffStood {
        let kept = if self.land.rest {
            OffStood::Resting
        } else {
            OffStood::Held
        };
        self.park_kept(kept);
        kept
    }

    fn park_kept(&mut self, kept: OffStood) {
        if kept == OffStood::Person {
            self.park_off();
            return;
        }
        let (replies, replies_at, spent) = (self.replies, self.replies_at, self.switch_spent);
        self.park(kept == OffStood::Resting);
        self.replies = replies;
        self.replies_at = replies_at;
        self.switch_spent = spent;
        self.word_seen = Some(Autopilot::Off);
    }

    /// A LOOP THE HOST STARTS AS THE SESSION'S OFF LIFTS: what Off's park ends is over, what the
    /// park of an Off no person set kept kept (`off`, as its row says: [`Self::park_off_held`];
    /// over a rest, the land episode this loop seeded, whose seeded point is read again as the
    /// check's answer), and then the word it lifts to is a move from Off ([`Self::note_word`]),
    /// so only the person's own lift to Land over the hold asks.
    pub(crate) fn start_lifted_off(&mut self, off: OffStood) {
        let seeded = self.carrying;
        self.park_kept(off);
        self.carry_at_attach = seeded;
        self.seed_off_stood();
    }

    /// Whether [`Self::opening_read`] weighs the point in hand: the session's first under this loop,
    /// after it ran under Off ([`Self::seed_off_stood`]).
    #[must_use]
    pub(crate) const fn reads_opening(&self) -> bool {
        self.off_stood && self.point_at.is_none()
    }

    /// THE POINT'S SCREEN SHOWS THE PROGRAM'S OPENING EXCHANGE WHOLE, its launch card over the one
    /// message ([`aterm_phase::ScreenReader::opening`]): after Off that message opens the session,
    /// whatever Off and the tab's books say ([`Self::observe`]).
    pub(crate) const fn opening_read(&mut self, drawn: bool) {
        self.opening_drawn = drawn;
    }

    /// THE LANDING A LOOP BEFORE THIS ONE LANDED.
    pub const fn seed_landed(&mut self, words: Option<u64>) {
        self.landed_seed = Some(true);
        self.landed_words = words;
        self.seed_by_wake = false;
    }

    /// THE SEEDED LANDING ([`Self::seed_landed`]) WAS KEPT THROUGH THE TASK'S OWN WAKE by the loop before this one.
    pub const fn seed_landed_by_wake(&mut self) {
        if self.landed_seed.is_some() {
            self.seed_by_wake = true;
        }
    }

    /// The landing this state holds was kept through the task's own wake ([`Self::seed_landed_by_wake`]).
    #[must_use]
    pub const fn landed_by_wake(&self) -> bool {
        self.landed && self.by_wake
    }

    /// THE LANDING THIS STATE HOLDS STOOD ON THE LAND CHECK'S `LANDED` (`land-check@v1`, slice
    /// 13a), read at this loop's own point: the landing's receipt says the check was answered
    /// and quotes the defense it audited ([`Self::land_audited`]). A landing seeded from a loop
    /// before this one, kept through the task's wake, or a task record's did not.
    #[must_use]
    pub const fn landed_by_check(&self) -> bool {
        self.landed && self.by_check
    }

    /// A WAKE KEPT THE LANDING AT THE POINT FOLDED LAST, asked once by the loop after the fold.
    pub fn take_wake_restated(&mut self) -> bool {
        std::mem::take(&mut self.wake_restated)
    }

    /// CRUISE UNLANDED THE BOOKS AT THE POINT FOLDED LAST: it never parks, so the task is open
    /// again whatever act that point followed. The next point folded unlands nothing of its own.
    #[must_use]
    pub const fn unlanded(&self) -> bool {
        self.unlanded
    }

    /// THE LOOP STARTS UNDER `mode`, the session's word as the loop read it before it tells its start face ([`Self::face_start`]).
    pub fn start_under(&mut self, mode: Autopilot) {
        if mode == Autopilot::Cruise {
            self.landed_seed = None;
            self.landed_words = None;
            self.seed_by_wake = false;
            self.seed_noted_ms = None;
        }
    }

    /// A landing seeded from a loop before this one is armed, not yet taken up or dropped ([`Self::seed_landed`]).
    #[must_use]
    pub const fn landed_seed_armed(&self) -> bool {
        self.landed_seed.is_some()
    }

    /// THE TIME THE SEEDED LANDING WAS NOTED.
    pub const fn seed_noted_at(&mut self, noted_ms: i64) {
        self.seed_noted_ms = Some(noted_ms);
    }

    /// The time to tell with the face at the loop's start ([`Self::face_start`]).
    #[must_use]
    pub fn face_start_noted(&self) -> Option<i64> {
        let restated = match self.face_told? {
            TaskFace::Landed => self.landed_seed.is_some(),
            TaskFace::Open | TaskFace::Theirs | TaskFace::Failed => false,
        };
        self.seed_noted_ms.filter(|_| restated)
    }

    /// THE LANDING THIS POLICY HOLDS IS A SEED RESTATED.
    #[must_use]
    pub fn restated_noted_ms(&self) -> Option<i64> {
        self.restating
            .filter(|(face, _)| *face == self.face())
            .map(|(_, at)| at)
    }

    /// THE LAST REPLY SAID DONE, as this policy read it at its last point.
    #[must_use]
    pub fn said_done(&self) -> bool {
        !self.other_turn
            && !self.cut_reply
            && !self.left_to_person()
            && (self.done >= 1 || self.landed_seed.is_some())
    }

    /// A live turn of someone else's has been read since the last point ([`Self::other_turn`]).
    #[must_use]
    pub const fn other_turn(&self) -> bool {
        self.other_turn
    }

    /// THE TASK'S FACE NOW.
    #[must_use]
    pub fn face(&self) -> TaskFace {
        TaskFace::of(self, self.other_turn)
    }

    /// THE FACE TO TELL THE HOST (`IdleHost::task_face`), where it moved since the loop last told one.
    pub fn face_moved(&mut self) -> Option<TaskFace> {
        let face = self.face();
        let moved = (self.face_told.replace(face) != Some(face)).then_some(face);
        if moved.is_some() {
            self.restating = None;
        }
        moved
    }

    /// The face at the loop's start, told whatever it is.
    pub fn face_start(&mut self) -> TaskFace {
        let face = self.face();
        self.face_told = Some(face);
        face
    }

    /// The task is done because the turn was a person's own exchange.
    #[must_use]
    pub const fn left_to_person(&self) -> bool {
        self.task_done() && self.exchange
    }

    /// Whether `autopilot` leaves a person's own exchange to them. CRUISE NEVER PARKS (owner ruling
    /// 2026-10-26); under Land only the exchange that opens the session is theirs
    /// (2026-10-27, [`Self::exchange_opens`]), and never a managed worker's
    /// ([`Self::set_managed`]): its opening message is the task aterm or its manager typed, so a
    /// person's gesture in its first turn (a box they answered, a click, a scroll) makes the work
    /// no exchange of theirs, and Land goes on to conclude it (2026-11-04); Off leaves every one.
    const fn mode_leaves_exchange(&self, autopilot: Autopilot) -> bool {
        match autopilot {
            Autopilot::Cruise => false,
            Autopilot::Land => self.exchange_opens && !self.managed,
            Autopilot::Off => true,
        }
    }

    /// An exchange the word in hand would not leave ([`Self::mode_leaves_exchange`]) is one a run
    /// under another word left (the autopilot switched since: Land to Cruise, or Off to Land
    /// after the session worked). It is taken back here, and the point it held is a report again:
    /// a done claim where its words say done ([`says_done`]), else an ordinary turn end.
    fn mode_takes_exchange(&mut self, reading: &TurnEndReading) {
        if !self.exchange || self.mode_leaves_exchange(reading.autopilot) {
            return;
        }
        self.exchange = false;
        if self.done >= 2 && !self.landed {
            self.done = u8::from(says_done(reading.said_tail.as_deref()) && reading.wall.is_none());
            self.done_at = None;
        }
    }

    /// The act the point last judged followed.
    #[must_use]
    pub const fn followed(&self) -> Option<&'static str> {
        self.followed
    }

    /// Which point this policy judged last ([`Self::observe`]), counted from `1`.
    #[must_use]
    pub fn point_number(&self) -> Option<u64> {
        (self.points > 0).then_some(self.points)
    }

    /// WHEN THE DONE STANDING NOW WAS DECIDED, on the loop's clock.
    #[must_use]
    pub const fn done_shown(&self) -> Option<Instant> {
        self.done_at
    }

    /// A MANAGER WAITS ON THIS SESSION (`managed`). The loop reads the record before its first
    /// point is judged; a point judged while no read had come back (a lost one) keeps its
    /// `Could not finish:` word here for this.
    pub fn set_managed(&mut self, managed: bool) {
        self.managed = managed;
        if managed && std::mem::take(&mut self.could_not_finish_read) {
            self.conclude(GaveUp::CouldNotFinish);
        }
    }

    /// HOW THE WORK ENDED UNFINISHED, once it did ([`GaveUp`]): the task is done
    /// ([`Self::task_done`]), not landed, and nothing is typed until a turn runs.
    #[must_use]
    pub const fn gave_up(&self) -> Option<GaveUp> {
        self.gave_up
    }

    /// The work ends unfinished at the point judged last: Land's rest takes it, and what the books
    /// owed a defense (the land check, its list, the defenses it read) goes with it, since the
    /// check's words have no answer to it: once a turn lifts the conclusion, the same defense is
    /// owed the check again, said in a turn of real work or not. So does a left hold nothing
    /// settled yet ([`LandEpisode::left_counted`]): no act will be typed for it, so it held
    /// nothing, and the episode's left holds ([`LEFT_HOLDS`]) are not spent on it.
    fn conclude(&mut self, ending: GaveUp) {
        self.gave_up = Some(ending);
        self.done = 2;
        self.landed = false;
        self.by_check = false;
        self.by_wake = false;
        self.done_at = self.point_at;
        self.exchange = false;
        self.carrying = false;
        self.checking = false;
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        self.land.check_pending = false;
        self.land.check_owed = false;
        self.land.sent = None;
        self.land.audited = [None; 3];
        self.land_listed = None;
        if self.land.left_counted.take().is_some() {
            self.land.left_holds = self.land.left_holds.saturating_sub(1);
        }
    }

    /// THE SAME REPLY ENDS A MANAGED WORKER'S WORK: at a Land point whose decision typed nothing
    /// because the same reply came back with no work ([`Self::replies_stop`]), the rest it took has
    /// no end but someone else's turn, so it is a conclusion and a waiting manager hears it. A
    /// reply a wall or a standing need explains is not one: a wall has its own recovery, and a need
    /// already tells them. A wordless reply is tried again later, so it is not one yet either.
    pub fn conclude_repeated(&mut self, reading: &TurnEndReading, now: Instant) {
        if reading.autopilot != Autopilot::Land
            || !self.managed
            || self.task_done()
            || self.wall.is_some()
            || reading.wall.is_some()
            || reading.taskless
            || !reading.at_point(self.stale_input)
            || !self.needs.standing.is_empty()
            || self.land.rest
            || self.wordless_probe(reading).is_some()
            || !self.replies_stop(reading, now)
        {
            return;
        }
        self.conclude(GaveUp::Repeated);
    }

    /// THE FAILED CONCLUSION A LOOP BEFORE THIS ONE JOURNALED: the rest a journal's landing seeds
    /// ([`Self::seed_landed`]), since a relaunch restores the session's Land and no task record. It
    /// takes this loop's first point where that point holds the same reply (`words`, where the
    /// journal has its mark) under Land, by [`Self::seed_gave_up`]'s own conditions; a live turn
    /// drops it, and so does any other point.
    pub const fn seed_failed(&mut self, ending: GaveUp, words: Option<u64>) {
        self.gave_up_seed = Some((ending, words));
    }

    /// THE WORK CONCLUDED: it landed, or it ended unfinished, and nothing is typed until a turn
    /// runs. A relaunch's owed carry-on is settled with nothing typed at such a point
    /// (`IdlePoint::done_under_land`).
    #[must_use]
    pub const fn concluded(&self) -> bool {
        self.landed() || self.gave_up.is_some()
    }

    /// THE RECORD SAYS THE WORK ENDED UNFINISHED: whether that stands here, as
    /// [`Self::seed_task_done`] asks of a landing. A turn that ran at the first point, an act in
    /// flight or a wall means it no longer does, and the record is opened again. A usage-limit
    /// failure from an earlier harness never concludes the successor.
    pub fn seed_gave_up(&mut self, ending: GaveUp) -> bool {
        if self.gave_up.is_some() {
            return true;
        }
        if ending == GaveUp::UsageLimit {
            return false;
        }
        if self.autopilot == Some(Autopilot::Cruise)
            || !self.point_turnless
            || self.task_done()
            || self.wall.is_some()
            || self.awaiting.is_some()
            || self.after.is_some()
        {
            return false;
        }
        self.conclude(ending);
        true
    }

    /// Whether a manager waits on this session ([`Self::set_managed`]).
    #[must_use]
    pub const fn managed(&self) -> bool {
        self.managed
    }

    /// THE TASK IS LANDED BY ITS RECORD.
    pub fn seed_task_done(&mut self) -> bool {
        if self.task_done() {
            return true;
        }
        if self.autopilot == Some(Autopilot::Cruise)
            || self.wall.is_some()
            || self.awaiting.is_some()
            || self.after.is_some()
        {
            return false;
        }
        self.done = 2;
        self.landed = true;
        self.by_check = false;
        self.hold_landed();
        self.landed_mark = self.point_mark;
        self.done_at = self.point_at;
        self.exchange = false;
        self.carrying = false;
        self.checking = false;
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        true
    }

    /// WHETHER A LANDING THE TASK RECORD NAMES CAN STAND AT THE POINT JUDGED LAST (`supervise::task_mark`'s seed, before [`Self::seed_task_done`]).
    #[must_use]
    pub const fn record_landing_holds(&self) -> bool {
        self.point_clean
    }

    /// The work of the person's exchange in all so far.
    #[must_use]
    pub const fn exchange_work(&self) -> Duration {
        self.exchange_work
    }

    /// A PERSON'S OWN EXCHANGE STANDS AT A WALL.
    #[must_use]
    pub const fn exchange_at_wall(&self) -> bool {
        self.exchange && self.wall.is_some() && !self.task_done()
    }

    fn hand(&self, reading: &TurnEndReading, now: Instant) -> bool {
        self.came_in_turn(reading, reading.person, now)
    }

    /// Whether a person's input `ago` before `now` came in the turn that ended at `now`
    /// ([`TurnEndReading::in_turn`]), past the last point judged and act typed.
    fn came_in_turn(&self, reading: &TurnEndReading, ago: Option<Duration>, now: Instant) -> bool {
        let Some(at) = ago.and_then(|ago| now.checked_sub(ago)) else {
            return false;
        };
        reading.in_turn(ago)
            && self
                .point_at
                .max(self.acts.back().copied())
                .is_none_or(|since| at > since)
    }

    /// The back-off before the next act after the short streak.
    #[must_use]
    pub fn short_wait(&self) -> Option<Duration> {
        self.timing.short_wait_at(self.short)
    }

    /// WHEN A HOLD ON A WORDLESS REPLY TRIES AGAIN ([`Self::repeats`]): agy's turn at `reading`
    /// said nothing and showed no wall its reader names, and that empty reply is the one held.
    /// agy's reader reads every reply it draws, so a turn of agy's with no words is a model call
    /// that failed (a quota or a server error in words no reader knows yet), not a reply repeated:
    /// the hold is no stop, but one act more at [`TurnEndTiming::outage_cap`] from the last answer
    /// counted, doubling with each answer past the hold up to eight caps. `None` for another
    /// program (a reader that reads no words, or Claude Code's rows it does not name, keep the
    /// hold) or where the held reply said something.
    #[must_use]
    pub fn wordless_probe(&self, reading: &TurnEndReading) -> Option<Instant> {
        match reading.program {
            Program::Agy => {}
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Muse
            | Program::Generic => return None,
        }
        if reading.wall.is_some()
            || !said_letters(reading.turn_said.as_deref()).is_empty()
            || !said_letters(reading.said_tail.as_deref()).is_empty()
        {
            return None;
        }
        let k = self
            .replies
            .last()
            .filter(|k| k.mark == reply_mark(reading))?;
        let cap = self.timing.outage_cap;
        let past = u32::try_from(k.answers.saturating_sub(2)).unwrap_or(u32::MAX);
        let wait = crate::supervise::ladder::doubling(cap, past, cap.saturating_mul(8));
        Some(later(self.replies_at?, wait))
    }

    /// Return whether a retry wall still stands after its notice left the screen.
    #[must_use]
    pub fn holds_left_retry(&self) -> bool {
        self.wall.as_ref().and_then(left_retry_kind).is_some()
    }

    /// An act of this policy's is in flight.
    #[must_use]
    pub fn act_in_flight(&self) -> bool {
        self.awaiting.is_some_and(|a| a.rule != RULE_HOST_TURN) || self.after.is_some()
    }

    /// The account move's answer waits on the act awaited.
    #[must_use]
    pub const fn account_answer_awaited(&self, reading: &TurnEndReading) -> bool {
        self.account_answer_open(reading) && self.awaiting.is_some()
    }

    /// The account move is not answered yet and no read at this point saw a turn run, so only a
    /// turn answered unseen can answer it ([`Self::account_answer_unseen`]).
    #[must_use]
    pub const fn account_answer_open(&self, reading: &TurnEndReading) -> bool {
        self.account.is_some() && !self.account_told && reading.worked.is_none()
    }

    /// An act of this policy's the agent has not taken yet (no live turn seen since, [`Self::took`]), or a command's continuation owed.
    #[must_use]
    pub fn act_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.rule != RULE_HOST_TURN && !a.taken)
            || self.after.is_some()
            || self.fact_untaken()
    }

    /// A TURN THE HARNESS TYPED THAT THE AGENT HAS NOT TAKEN YET.
    #[must_use]
    pub fn typed_untaken(&self) -> bool {
        self.awaiting.is_some_and(|a| !a.taken) || self.fact_untaken()
    }

    /// THE OCCUPANCY DOOR TYPED ITS LINE AT A BREAK ([`HostTurn::Fact`]): kept in a record of its own, so an act awaited before it keeps its own (a taken
    /// continuation's point judges that act's answer), and untaken until a read sees the agent
    /// take it ([`Self::took`]), which holds the next break ([`Self::act_untaken`]) and reads
    /// that turn as the harness's ([`Self::typed_untaken`]).
    pub const fn fact_typed(&mut self) {
        self.keyed_by_harness();
        self.fact = Some(FactAwaited { taken: false });
    }

    /// CRUISE'S CARRY WENT AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK at `now`
    /// ([`super::cruise_carry::RULE_BACKGROUND`]): kept as the door's line is
    /// ([`Self::fact_typed`]), so its answer is the harness's turn and the next break waits on
    /// its take, and counted in the hour's acts (`continue_per_hour`, [`Self::acts_in_window`]).
    /// Its `text` is the act the next turn opens with, whose own rows a cut opening drops
    /// ([`without_act_rows`]).
    pub fn background_carried(&mut self, now: Instant, text: &str) {
        self.act_words = Some(text.to_string());
        self.fact_typed();
        self.acts.push_back(now);
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
    }

    /// The occupancy door's line stands untaken ([`Self::fact_typed`]).
    #[must_use]
    pub fn fact_untaken(&self) -> bool {
        self.fact.is_some_and(|f| !f.taken)
    }

    /// A BREAK OF THE AGENT'S OWN BACKGROUND WORK READ AFTER THE DOOR'S LINE WAS TAKEN: its
    /// answer ended there (`OccupancyDoor`'s `Break`), so the point after it is the agent's own.
    pub fn fact_answered(&mut self) {
        if self.fact.is_some_and(|f| f.taken) {
            self.fact = None;
        }
    }

    /// Whether an act under `rule` typed now would be typed IN A LIMIT'S PLACE ([`Self::acted`]
    /// records it so): an act of a limit's place ([`limits_place_rule`]) while the track of a
    /// usage window or a model bucket stands that no loop before this one only seeded.
    #[must_use]
    pub fn in_limits_place(&self, rule: &str) -> bool {
        limits_place_rule(rule)
            && self
                .wall
                .as_ref()
                .is_some_and(|t| named_class(t.class) && !t.seeded)
    }

    /// THE GO-AHEAD THIS POINT IS OWED (owner, 2026-10-29: "how dare you block on me for go
    /// ahead!"): the reading's park on a person's go-ahead for its own reversible work
    /// ([`TurnEndReading::go_ahead_past`]), where Cruise answers it, or Land once its instruction
    /// was typed and the work is unlanded: git measures work no remote main has, or the park is
    /// landing work by the agent's own words (it names a landing act, or its turn names the
    /// branch, a worktree or main: [`go_ahead::Park::lands`]), which may stand in a worktree the
    /// session's folder does not show. Where Land's instruction is untyped, a park
    /// gets the instruction first ([`continue_policy`]). Never while the person's last key holds
    /// it ([`Self::key_hour_ends`]: a key a point observed, or the reading's own, which the
    /// decision's `status` read can find first; the replies may agree to or restate their hold),
    /// nor where the ask names an irreversible act ([`REVERSIBLE_ANSWER`] answers that) other
    /// than the agent's own cleanup on a need's wrapped rows ([`go_ahead_answers`]), read in the
    /// rows as drawn and in the words the park read ([`go_ahead_answers_on`]).
    #[must_use]
    pub fn go_ahead_owed(&self, reading: &TurnEndReading, now: Instant) -> Option<go_ahead::Park> {
        self.owed_unless(reading, self.key_hour_ends(reading, now).is_some())
    }

    /// [`Self::go_ahead_owed`], `held` whether the person's last key holds the go-ahead.
    fn owed_unless(&self, reading: &TurnEndReading, held: bool) -> Option<go_ahead::Park> {
        if reading.autopilot == Autopilot::Land && !self.land.typed {
            return None;
        }
        self.parked_unless(reading, held)
    }

    /// [`Self::go_ahead_owed`] as if Land's instruction were typed: the park Land answers with
    /// its instruction first, and the go-ahead after it. The person's hour, the same reply's hold
    /// and its exception read this one (2026-10-30 review 9), so a park before the instruction is
    /// held, let through and told as the go-ahead owed is.
    #[must_use]
    pub fn go_ahead_parked(
        &self,
        reading: &TurnEndReading,
        now: Instant,
    ) -> Option<go_ahead::Park> {
        self.parked_unless(reading, self.key_hour_ends(reading, now).is_some())
    }

    /// [`Self::go_ahead_parked`], `held` whether the person's last key holds the go-ahead.
    fn parked_unless(&self, reading: &TurnEndReading, held: bool) -> Option<go_ahead::Park> {
        if held || reading.autopilot == Autopilot::Off {
            return None;
        }
        let act = self.act_words.as_deref().filter(|_| reading.opens_cut);
        let park = reading.go_ahead_past(act)?;
        let unlanded =
            reading.autopilot == Autopilot::Cruise || reading.repo.found().is_some() || park.lands;
        // The guard reads the report as drawn and as the park read it ([`go_ahead_answers_on`]).
        let guarded = go_ahead_answers_on(reading.said_tail.as_deref(), reading.needs_said().1);
        (unlanded && guarded).then_some(park)
    }

    /// THE GO-AHEAD A BREAK OF THE AGENT'S BACKGROUND WORK PARKS ON: the break's turn read as a
    /// turn end reads it ([`TurnEndReading::go_ahead_past`]), `turn_said` its earlier messages
    /// and `tail` its last, in the words the break's needs fold read ([`Self::fold_break_needs`],
    /// [`needs_words`]), past the act the turn opens with where `opens_cut`. So a plan in an
    /// earlier message (`delete the release tags, then push`) parks nothing at a break either.
    #[must_use]
    pub fn go_ahead_at_break(
        &self,
        turn_said: Option<needs::Before<'_>>,
        tail: &str,
        opens_cut: bool,
    ) -> Option<go_ahead::Park> {
        turn_go_ahead(
            turn_said,
            tail,
            self.act_words.as_deref().filter(|_| opens_cut),
        )
    }

    /// THE PARK LAND'S INSTRUCTION ANSWERS FIRST: under Land before its instruction was typed,
    /// with answers on (`cfg`), the park the go-ahead answers once it is
    /// ([`Self::go_ahead_parked`]). The instruction typed over it is the act that answers the
    /// park, so its lines are off the menu bar from that decision and its ack owes none of them
    /// (2026-10-30 review 9); a park after it gets the go-ahead.
    #[must_use]
    pub fn go_ahead_first(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> Option<go_ahead::Park> {
        (reading.autopilot == Autopilot::Land && !self.land.typed && cfg.answer_questions)
            .then(|| self.go_ahead_parked(reading, now))
            .flatten()
    }

    /// Whether the person's last key holds the go-ahead at `now`: it came, or the turn it came in
    /// ended, within [`GO_AHEAD_HELD`] before it ([`Self::person_keyed`]).
    fn person_holds(&self, now: Instant) -> bool {
        self.go_ahead_person
            .is_some_and(|at| now < later(at, GO_AHEAD_HELD))
    }

    /// WHEN THE HOUR THE PERSON'S LAST KEY HOLDS THE GO-AHEAD FOR ENDS, where `reading` is decided
    /// at `now`: the later of the hour of a key a point observed ([`Self::person_holds`]) and the
    /// hour of the reading's own key ([`TurnEndReading::key`], from the key): the `status` a
    /// decision reads before it types can find a key no point observed yet (a loop that followed,
    /// whose first point waited), and it holds the hour as the point that observes it will
    /// ([`Self::person_keyed`]). `None` where neither holds at `now`. The go-ahead
    /// ([`Self::go_ahead_owed`]) and the sentence that says it is no need
    /// ([`Self::go_ahead_held_at`]) both read it, so the two never disagree on one reading.
    fn key_hour_ends(&self, reading: &TurnEndReading, now: Instant) -> Option<Instant> {
        let observed = self.go_ahead_person.map(|at| later(at, GO_AHEAD_HELD));
        let read = reading
            .key
            .and_then(|ago| GO_AHEAD_HELD.checked_sub(ago))
            .map(|left| later(now, left));
        observed.max(read).filter(|&ends| now < ends)
    }

    /// WHEN THE PERSON'S HOUR THAT HOLDS THIS POINT'S PARK ENDS: the end of the hour their last key
    /// holds the go-ahead for ([`Self::key_hour_ends`]), where it holds at `now` and the reading's
    /// park is owed once it ends, or Land's instruction first where it is untyped
    /// ([`Self::go_ahead_parked`], 2026-10-30 review 9). The decision waits for it where it
    /// would type nothing ([`GO_AHEAD_HOUR`]), so the loop decides the point again there, though
    /// the parked agent ends no other turn.
    #[must_use]
    pub fn go_ahead_hour_ends(&self, reading: &TurnEndReading, now: Instant) -> Option<Instant> {
        let ends = self.key_hour_ends(reading, now)?;
        self.parked_unless(reading, false).is_some().then_some(ends)
    }

    /// Whether the person holds the go-ahead at `now`: their last key does
    /// ([`Self::person_holds`]), or their own exchange stands. A carry at a background break
    /// ([`super::cruise_carry`]) then types its own words, and neither it nor Cruise's
    /// continuation says the go-ahead is no need ([`go_ahead::unless_held`]).
    #[must_use]
    pub fn go_ahead_held(&self, now: Instant) -> bool {
        self.person_holds(now) || self.exchange
    }

    /// Whether the person holds the go-ahead where `reading` is decided at `now`: their last key
    /// does, a point's or the reading's own ([`Self::key_hour_ends`]), or their own exchange
    /// stands ([`Self::go_ahead_held`]). Cruise's continuation and done check then leave
    /// [`go_ahead::NEVER_A_NEED`] out ([`go_ahead::unless_held`]).
    #[must_use]
    pub fn go_ahead_held_at(&self, reading: &TurnEndReading, now: Instant) -> bool {
        self.key_hour_ends(reading, now).is_some() || self.exchange
    }

    /// Whether the continuation a `/compact` owes leaves [`go_ahead::NEVER_A_NEED`] out where
    /// `reading` is decided at `now`: the person holds the go-ahead ([`Self::go_ahead_held_at`]),
    /// or the reading parks on a person's go-ahead ([`Self::go_ahead_parked`]) that answers off
    /// (`cfg`) leave to the person, as the continuation owed at a break leaves it out
    /// ([`Self::owed_words`]). The owed act goes ahead of the escalation answers off give such a
    /// park ([`act_past_walls`]), so it never says the go-ahead is no need over it.
    fn owed_held_at(&self, reading: &TurnEndReading, cfg: &SupervisorConfig, now: Instant) -> bool {
        self.go_ahead_held_at(reading, now)
            || (!cfg.answer_questions && self.go_ahead_parked(reading, now).is_some())
    }

    /// Whether the go-ahead is owed at this point, or Land's instruction before it
    /// ([`Self::go_ahead_parked`], 2026-10-30 review 9), and none was typed on the standing park
    /// yet ([`GoAheads`]): the same-reply hold lets the point through (`act_past_walls`), the
    /// instruction and then the go-ahead, after which the hold and the go-ahead's own bound count
    /// again.
    #[must_use]
    pub fn go_ahead_untried(&self, reading: &TurnEndReading, now: Instant) -> bool {
        self.go_aheads.acts == 0 && self.go_ahead_parked(reading, now).is_some()
    }

    /// Whether the go-ahead on the standing park is spent ([`GoAheads`]): [`GO_AHEAD_ACTS`] acts
    /// typed on it, whatever word each reply asked for and whatever work ran between them, or
    /// [`GO_AHEAD_SPAN`] since the first since the agent last moved past it
    /// ([`Self::go_ahead_worked`]) or the Mac last slept ([`Self::slept`]). The policy then
    /// switches to paced forward recovery and records the refusal.
    #[must_use]
    pub fn go_ahead_spent(&self, now: Instant) -> bool {
        self.go_aheads.acts >= GO_AHEAD_ACTS
            || self
                .go_aheads
                .first
                .is_some_and(|first| now >= later(first, GO_AHEAD_SPAN))
    }

    /// THE GO-AHEAD'S COUNT AFTER A READ of a turn end or a break whose turn `parks`
    /// ([`go_ahead::Park`]): it starts again where the turn parks no more, whatever the act typed
    /// after; a park on any word or in prose keeps it ([`GoAheads`]).
    pub fn go_ahead_read(&mut self, parks: bool) {
        if !parks {
            self.go_aheads = GoAheads::default();
        }
    }

    /// THE AGENT MOVED PAST ITS PARK in the turn a read ends: it worked `worked` (a point's
    /// [`TurnEndReading::worked`], or a settled break's busy time), real work at least
    /// [`TurnEndTiming::min_work`], or a wall cut the turn (`walled`), whose wait is the limit's,
    /// not the park's. The go-ahead's span ([`GO_AHEAD_SPAN`]) then runs again from its next act;
    /// its acts stand ([`GO_AHEAD_ACTS`]), so an agent that parks again after work is bounded all
    /// the same (2026-10-30 review 9: one that obeyed, worked past the hour and parked again was
    /// refused after a single go-ahead).
    pub fn go_ahead_worked(&mut self, worked: Duration, walled: bool) {
        if walled || worked >= self.timing.min_work {
            self.go_aheads.first = None;
        }
    }

    /// THE GO-AHEAD WENT ON `park` at `now` at a break ([`RULE_GO_AHEAD`]; a turn end's is booked
    /// by [`Self::acted`]): one more act on the standing park, or its first, and the lines it
    /// answers are off the menu bar for good ([`GoAheadVerdict::Answers`]).
    pub fn go_ahead_typed(&mut self, park: &go_ahead::Park, now: Instant) {
        self.go_ahead_counted(&park.needs, now);
    }

    /// One act of the go-ahead's count at `now` ([`GoAheads`]), answering the lines being
    /// answered and those `past` names: the act counts whatever lines it names.
    fn go_ahead_counted(&mut self, past: &[String], now: Instant) {
        self.go_ahead_answered(past);
        self.go_aheads.first.get_or_insert(now);
        self.go_aheads.acts = self.go_aheads.acts.saturating_add(1);
    }

    /// THE FACT `fact` RODE THE ACT [`Self::acted`] LAST RECORDED ([`FACT_RIDES`]): the act's
    /// own text, kept for a `/compact`'s continuation to type again, is kept without it, so a
    /// fact told once never rides a retyped act.
    pub fn fact_rode(&mut self, fact: &crate::supervise::facts::Fact) {
        let suffix = fact_suffix(fact);
        if let Some(text) = &mut self.cut_text
            && let Some(kept) = text.strip_suffix(&suffix)
        {
            *text = kept.to_string();
        }
    }

    fn compact_owed_text(
        &self,
        rule: &str,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> Option<String> {
        self.compact_owed_words(rule, reading.autopilot, reading.rules.as_deref())
            .map(|words| {
                go_ahead::unless_held(&words, self.owed_held_at(reading, cfg, now)).into_owned()
            })
    }

    /// A COMMAND'S CONTINUATION OWED ([`Self::act_untaken`]'s `after`) WITH NOTHING THE HARNESS
    /// TYPED STANDING UNTAKEN: the `/compact` ran, so its continuation is the next line, which a
    /// break of the agent's own background work types in Cruise's carry's place
    /// (`Session::cruise_carry`), as no point may come while that work runs.
    #[must_use]
    pub fn owed_at_break(&self) -> Option<&'static str> {
        self.after.filter(|_| !self.typed_untaken())
    }

    /// The words of the continuation `rule` owes under Cruise at `now`, as a point types them
    /// ([`act_past_walls`]): the act a full context cut, else Cruise's continuation, each without
    /// [`go_ahead::NEVER_A_NEED`] while the person's hour holds the go-ahead
    /// ([`go_ahead::unless_held`]), or where the break's turn `parks` on a person's go-ahead that
    /// answers off (`cfg`) leave to the person, as the carry goes without it there and a point
    /// types the owed act without it ([`Self::owed_held_at`]).
    #[must_use]
    pub fn owed_words(
        &self,
        rule: &str,
        cfg: &SupervisorConfig,
        rules: Option<&str>,
        parks: bool,
        now: Instant,
    ) -> String {
        let words = self
            .compact_owed_words(rule, Autopilot::Cruise, rules)
            .unwrap_or_else(|| continuation_text(cfg, rules));
        let held = self.go_ahead_held(now) || (parks && !cfg.answer_questions);
        go_ahead::unless_held(&words, held).into_owned()
    }

    /// THE DONE CHECK [`words_act`] TYPES AT THIS POINT: [`fitted_check`] at the count's angle,
    /// with the open work, the repository's own work to land, the reversible clause where
    /// `irreversible` or a destructive need rides, and NEEDS YOU's acknowledgment where it is
    /// owed; without [`go_ahead::NEVER_A_NEED`] where `held`.
    fn done_check_at(&self, irreversible: bool, held: bool, rules: Option<&str>) -> String {
        let ack = self.needs_ack_owed();
        fitted_check(
            usize::from(self.checks),
            held,
            &[
                Suffix::Open(&self.open),
                Suffix::Repo(&self.repo, RepoAsk::Own),
                Suffix::Left(self.left_said, RepoAsk::Own),
                Suffix::Fixed(reversible_clause(
                    irreversible || (ack && self.needs_destroy()),
                )),
                Suffix::Fixed(ack_clause(ack)),
            ],
            rules,
        )
    }

    /// THE WORDS [`Self::cut_text`] KEEPS FOR `action`, read before [`Self::acted`] moves the
    /// state they were decided on: the act's typed text, but whole where the person's hour cut
    /// [`go_ahead::NEVER_A_NEED`] out of it: a done check ([`Self::done_check_at`] past the hour,
    /// with the reversible clause where the typed one carries it), or a `/compact`'s retype of
    /// the act a full context cut ([`Self::compact_owed_words`]). A retype cuts them as the hour
    /// has it then ([`Self::compact_owed_text`], [`Self::owed_words`]), so a check typed within
    /// the hour and again past it says the sentence, as the check a seeded loop types does. An
    /// act typed in neither's words keeps its own, a fact riding it included ([`Self::fact_rode`]
    /// takes that off).
    fn kept_words(&self, action: &TurnEndAction, reading: &TurnEndReading) -> Option<String> {
        let TurnEndAction::Type { text, rule_id } = action else {
            return None;
        };
        let rules = reading.rules.as_deref();
        let whole = match *rule_id {
            RULE_DONE_CHECK => {
                let irreversible = text.contains(DONE_CHECK_REVERSIBLE);
                let cut = self.done_check_at(irreversible, true, rules);
                text.starts_with(&cut)
                    .then(|| self.done_check_at(irreversible, false, rules))
            }
            RULE_COMPACT => self
                .compact_owed_words(RULE_COMPACT, reading.autopilot, rules)
                .filter(|words| text.starts_with(go_ahead::unless_held(words, true).as_ref())),
            _ => None,
        };
        Some(whole.unwrap_or_else(|| text.clone()))
    }

    fn compact_owed_words(
        &self,
        rule: &str,
        autopilot: Autopilot,
        rules: Option<&str>,
    ) -> Option<String> {
        let owes = self.compact_owes.filter(|_| rule == RULE_COMPACT)?;
        match autopilot {
            Autopilot::Land if owes.land => Some(
                self.cut_text
                    .as_deref()
                    .filter(|t| is_land_words(t))
                    .map(str::to_string)
                    .unwrap_or_else(|| match self.seeded_land {
                        Some(RULE_LAND) => land_text(rules),
                        Some(RULE_LAND_ANSWER) => land_answer_text(rules),
                        Some(RULE_LAND_CHECK) => land_check::land_check_text(None, false, rules),
                        _ => land_nudge_text(rules),
                    }),
            ),
            Autopilot::Cruise if owes.check => Some(
                self.cut_text
                    .as_deref()
                    .filter(|t| !is_land_words(t))
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        // The check the cut took, by its angle: the count's last (a loop before this
                        // one that counted none typed the first, or v1's fixed check).
                        let typed = usize::from(self.checks.saturating_sub(1));
                        act_words(&done_check_words(typed), &[reversible_clause(true)], rules)
                    }),
            ),
            _ => None,
        }
    }

    /// A `/compact` of this policy's is running.
    #[must_use]
    pub fn compacting(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.command && a.rule == RULE_COMPACT)
    }

    /// THE WALL'S RETRY AWAITED ([`Awaited::wall_retry`]: the limit's try among them) WAS ANSWERED BY A WALL inside its take window.
    #[must_use]
    pub fn retry_met_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.wall_retry && a.met && !a.taken)
    }

    #[cfg(test)]
    pub(crate) fn untaken_act_seen(&self) -> Option<Instant> {
        self.awaiting
            .as_ref()
            .filter(|a| !a.taken)
            .and_then(|a| a.unseen_since)
    }

    /// When the act this policy typed last went, while no read since saw the agent's live turn ([`Self::took`]).
    #[must_use]
    pub fn untaken_act(&self) -> Option<Instant> {
        self.untaken().map(|a| a.at)
    }

    /// When [`Self::untaken_act`]'s act was typed on the policy's clock ([`Awaited::typed`]).
    #[must_use]
    pub fn untaken_act_typed(&self) -> Option<Instant> {
        self.untaken().map(|a| a.typed)
    }

    fn untaken(&self) -> Option<&Awaited> {
        self.awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken)
    }

    /// The limit a cancel left, answering a turn no read saw answered.
    #[must_use]
    pub fn left_answer(&self, rows: &[String]) -> Option<LeftAnswer> {
        let wall = aterm_phase::left_wall(rows)?;
        let key = left_key(
            aterm_phase::since_limit(rows).as_ref(),
            aterm_phase::auto_continue_end(rows),
        )?;
        let own = self
            .awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken);
        let (since, seen) = match own {
            Some(a) if a.met => return None,
            Some(a) => (a.at, a.left),
            None if self.waits_on_named_reset() => return None,
            None => self.looked?,
        };
        Some(LeftAnswer {
            wall,
            since,
            screen_tells: seen != Some(key),
        })
    }

    /// WHETHER A LIVE TURN READ NOW MAY BE A LANDED TASK'S OWN WAKE.
    #[must_use]
    pub fn wake_may_keep_landing(&self) -> bool {
        (self.landed() && self.autopilot == Some(Autopilot::Land)
            || self.landed_seed.is_some() && self.autopilot.is_none_or(|a| a == Autopilot::Land))
            && self.awaiting.is_none()
            && self.after.is_none()
            && !self.other_turn
            && !self.quiet_wake
            && self.fact.is_none()
    }

    /// [`Self::took`] for A LANDED TASK'S OWN WAKE.
    pub fn took_quiet(&mut self) {
        self.quiet_wake = true;
        self.took();
    }

    /// WHAT THE SESSION'S REPOSITORY HELD AS A LANDED TASK'S OWN WAKE BEGAN ([`Self::took_quiet`]).
    pub fn wake_began_over(&mut self, found: crate::harness::repo_work::RepoRead) {
        if self.wake_start_owed() {
            self.wake_repo = Some(found);
        }
    }

    /// A LANDED TASK'S OWN WAKE IS READ AT ITS START ([`Self::took_quiet`]) and THIS STATE SAW THE POINT IT BEGAN AFTER.
    #[must_use]
    pub const fn wake_start_owed(&self) -> bool {
        self.quiet_wake && self.point_at.is_some() && self.wake_repo.is_none()
    }

    /// A read saw the agent's live turn (its spinner or busy footer, never a break of its own background work).
    pub fn took(&mut self) {
        // The door's line's answer is the harness's turn, no one else's.
        if self.awaiting.is_none()
            && self.after.is_none()
            && !self.quiet_wake
            && self.fact.is_none()
        {
            self.other_turn = true;
            self.land.legs = None;
        }
        self.ran_turn_seed = None;
        self.login_seed = None;
        if !self.quiet_wake {
            self.landed_seed = None;
            self.landed_words = None;
            self.seed_by_wake = false;
            self.seed_noted_ms = None;
            self.gave_up_seed = None;
        }
        self.restating = None;
        self.ran |= std::mem::take(&mut self.point_last);
        self.gap = false;
        self.esc_at = None;
        self.esc_seed = false;
        self.woken_esc = None;
        if let Some(a) = &mut self.awaiting {
            a.taken = true;
            a.met = false;
        }
        if let Some(f) = &mut self.fact {
            f.taken = true;
        }
        if std::mem::take(&mut self.seed_answered) {
            self.checking = false;
            self.landing = false;
        }
        self.reset_resume = None;
        if std::mem::take(&mut self.carry_at_attach) {
            self.carrying = true;
        }
    }

    const fn new_point(&mut self, now: Instant) {
        self.point_at = Some(now);
        self.points = self.points.saturating_add(1);
        self.opened_fresh = false;
        self.resumed = None;
    }

    /// A RESTART PROMPT'S SHORT REPLY MEASURES ITS BACK-OFF FROM THE REPLY ([`HostTurn::CarryOn`],
    /// [`HostTurn::Restore`], or [`HostTurn::Check`] where the agent's last words claimed the work
    /// done): the prompt may have waited minutes behind a draft or under Off, and the point before
    /// it would have the loop's own prompt straight after this one. A reply nobody saw work is
    /// measured from the prompt, which is where an unheld prompt's own wait starts. One prompt per
    /// relaunch moves it, so no run of them can hold the loop's own prompt back. An upgrade's
    /// notice ([`HostTurn::Notice`]) asks again at its own pace and leaves the clock where it is.
    fn back_off_from_restart_reply(
        &mut self,
        typed: Option<Instant>,
        worked: Duration,
        now: Instant,
    ) {
        if let Some(typed) = typed {
            let reply = later(typed, worked).min(now);
            self.point_at = self.point_at.map(|at| at.max(reply));
        }
    }

    /// A BOX WAS READ UP (a permission box, a dialog).
    pub fn box_up(&mut self) {
        self.esc_at = None;
        self.esc_seed = false;
        self.woken_esc = None;
        self.ran |= std::mem::take(&mut self.point_last);
        self.gap = false;
    }

    #[cfg(test)]
    pub(crate) fn esc_read(&self) -> Option<Instant> {
        self.esc_at
    }

    /// The Mac slept `by`, as the loop measured it (`Instant` stops while the Mac sleeps; the wall clock does not).
    pub fn slept(&mut self, by: Duration) {
        let back = |t: &mut Instant| {
            if let Some(earlier) = t.checked_sub(by) {
                *t = earlier;
            }
        };
        for t in &mut self.acts {
            back(t);
        }
        if let Some((_, t)) = &mut self.account_ahead_unmade {
            back(t);
        }
        if let Some((_, t)) = &mut self.route_unmade {
            back(t);
        }
        if let Some(t) = &mut self.point_at {
            back(t);
        }
        if let Some(t) = &mut self.done_at {
            back(t);
        }
        // THE PERSON'S HOUR COUNTS THE SLEEP (2026-10-30 review 9): their key, the hour it holds
        // and the one booked, as the window's `human_typed_ms` counts it.
        for t in [
            &mut self.go_ahead_person,
            &mut self.person_key,
            &mut self.go_ahead_booked,
        ]
        .into_iter()
        .flatten()
        {
            back(t);
        }
        // A SLEEP IS NO PARK STANDING (review 9, second pass): the Mac's wait, not the agent's on
        // its park, as a wall's is ([`Self::go_ahead_worked`]): the go-ahead's span runs again
        // from its next act, and its acts stand ([`GO_AHEAD_ACTS`]).
        self.go_aheads.first = None;
        if let Some(t) = &mut self.point_seed {
            back(t);
        }
        if let Some(t) = &mut self.esc_at {
            back(t);
        }
        if let Some(a) = &mut self.awaiting {
            back(&mut a.typed);
        }
        if let Some(u) = &mut self.unread_try {
            back(&mut u.at);
        }
        if let Some(w) = &mut self.wall {
            back(&mut w.since);
            if let Some(n) = &mut w.named {
                back(&mut n.at);
            }
            for t in [
                &mut w.login.prompt_at,
                &mut w.login.typed_at,
                &mut w.login.backed_out,
            ]
            .into_iter()
            .flatten()
            {
                back(t);
            }
        }
        if let Some(t) = self.model.as_mut().and_then(|m| m.back_at.as_mut()) {
            back(t);
        }
        if let Some(c) = &mut self.credits {
            for t in [&mut c.reset, &mut c.passed, &mut c.since]
                .into_iter()
                .flatten()
            {
                back(t);
            }
        }
    }

    /// The loop's limit-reset step ended and its host read `outcome` (`crate::supervise::IdleHost::reset_told`).
    pub fn reset_told(&mut self, outcome: &ResetOutcome, now: Instant) {
        let (asks, cleared_at) = match *outcome {
            ResetOutcome::Pending => return,
            ResetOutcome::Spent {
                asks, cleared_at, ..
            }
            | ResetOutcome::Lifted { asks, cleared_at } => (asks, Some(cleared_at)),
            ResetOutcome::Kept
            | ResetOutcome::Failed { .. }
            | ResetOutcome::Unread
            | ResetOutcome::Walled { .. } => (true, None),
        };
        self.reset_resume = Some(ResetResume {
            told: now,
            due: if asks {
                now
            } else {
                later(now, RESET_CONTINUES_WAIT)
            },
        });
        if let Some(cleared_at) = cleared_at {
            self.account_resumed = Some(
                self.account_resumed
                    .map_or(cleared_at, |typed| typed.max(cleared_at)),
            );
        }
    }

    /// From when the continuation a limit reset left is typed, while one is owed.
    #[must_use]
    pub fn reset_resume_owed(&self) -> Option<Instant> {
        self.reset_resume.map(|owed| owed.due)
    }

    /// The model switch in force, if any.
    #[must_use]
    pub const fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
    }

    /// Whether the act [`Self::acted`] last recorded (a typed act; a restart is [`Self::restarted`]'s) was typed IN A LIMIT'S PLACE.
    #[must_use]
    pub fn acted_in_limits_place(&self) -> bool {
        self.limit_try.is_some_and(|l| l.in_place)
    }

    /// THE LIMIT'S TRY the act [`Self::acted`] last recorded was, if it was one ([`LimitTry`]).
    #[must_use]
    pub const fn limit_try(&self) -> Option<LimitTry> {
        self.limit_try
    }

    /// A LIMIT'S TRACK ENDED at the last fold ([`Self::observe`]), by no act of this policy's.
    #[must_use]
    pub const fn limit_ended(&self) -> bool {
        self.limit_ended
    }

    /// The limit's track ended at the last fold ([`Self::limit_ended`])
    /// because ANOTHER PROGRAM'S SCREEN was read: a limit met under one
    /// agent the loop decides on, ended where another such agent runs in
    /// the tab now ([`met_under_another`]), not by the worker's work or
    /// another wall.
    #[must_use]
    pub const fn limit_ended_elsewhere(&self) -> bool {
        self.limit_ended && self.limit_ended_elsewhere
    }

    /// The acts made at the wall being worked, if it is of `kind`'s class
    /// ([`exception_for`](super::exception::exception_for)'s and the conformance walks'
    /// projection).
    #[must_use]
    pub fn wall_attempts(&self, kind: WallKind) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.attempts)
    }

    /// Return whether this state needs the account roster to choose the owner's notice.
    #[must_use]
    pub fn reads_roster(&self, reading: &TurnEndReading) -> bool {
        reading.wall == Some(WallKind::UsageWeekly)
            || (self.wall_attempts(WallKind::UsageWeekly).is_some()
                && (reading.login_interrupted || self.login_cancelled_on_limit()))
    }

    /// Since when the wall being worked shows, if it is of `kind`'s class.
    #[must_use]
    pub fn wall_since(&self, kind: WallKind) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.since)
    }

    /// Whether the full context being worked has met the vendor's own word that a `/compact` could not lower it.
    #[must_use]
    pub fn compaction_exhausted(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Context && t.exhausted)
    }

    /// What the full context being worked was said to be filled by, by its own notice this appearance ([`ContextFill::of`]).
    #[must_use]
    pub fn context_fill(&self) -> Option<ContextFill> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Context)
            .and_then(|t| t.fill)
    }

    /// THE TYPED KIND OF THE TRANSCRIPT'S NOTICE at this decision's read, told by the loop (`supervise/wall_confirm.rs`).
    pub fn notice_typed_was(&mut self, typed: Option<String>) {
        self.notice_typed = typed;
    }

    /// The typed kind of the notice the transcript ended on at the last decision's read.
    #[must_use]
    pub fn notice_typed(&self) -> Option<&str> {
        self.notice_typed.as_deref()
    }

    /// `/compact`'S OWN ANSWER.
    pub fn compact_answered(&mut self, said: &str) {
        if self
            .awaiting
            .is_some_and(|a| a.rule == RULE_COMPACT && a.command)
            && compaction_exhausted_row(said)
            && let Some(t) = self.wall.as_mut().filter(|t| t.class == WallClass::Context)
        {
            t.exhausted = true;
        }
    }

    /// Whether the fallback relaunch for the bucket being worked was refused for good ([`Self::restarted`] with `made` false).
    #[must_use]
    pub const fn fallback_refused(&self) -> bool {
        self.fallback_unmade
    }

    /// The account move made and not answered yet, if any.
    #[must_use]
    pub const fn account_move(&self) -> Option<&AccountMove> {
        self.account.as_ref()
    }

    /// Whether an account move asked for at the wall being worked could not be made ([`Self::restarted`] with `made` false).
    #[must_use]
    pub const fn account_refused(&self) -> bool {
        self.account_unmade.is_some()
    }

    /// What the host's word for that move says of asking it again ([`Unmade`]; [`Self::restart_unmade`]).
    #[must_use]
    pub const fn account_refusal(&self) -> Option<Unmade> {
        self.account_unmade
    }

    /// The accounts' news reached this session: both refusal latches end and the next point asks again.
    pub fn account_news(&mut self) {
        self.account_unmade = None;
        self.account_unmade_seed = None;
        self.account_unmade_ms = None;
        self.account_ahead_unmade = None;
        self.route_unmade = None;
    }

    /// Whether a limit's resume is owed to a login moved elsewhere ([`Self::login_resume`]).
    #[must_use]
    pub const fn login_resume_owed(&self) -> bool {
        self.login_resume
    }

    /// Whether the wall's resume is owed after a move this session's host made at a wall ([`Self::account_resume`]).
    #[must_use]
    pub const fn account_resume_owed(&self) -> bool {
        self.account_resume
    }

    /// Whether the host could not make a route move onto `to` within the window ending at `now`
    /// ([`TurnEndTiming::window`]): it is not asked again until then, or until the accounts' news
    /// reaches the session ([`Self::account_news`]).
    fn route_refused(&self, to: RouteTo, now: Instant) -> bool {
        self.route_unmade.as_ref().is_some_and(|(was, at)| {
            *was == to && now.saturating_duration_since(*at) < self.timing.window
        })
    }

    fn account_ahead_refused(&self, to: &str, now: Instant) -> bool {
        self.account_ahead_unmade
            .as_ref()
            .is_some_and(|(label, at)| {
                label == to && now.saturating_duration_since(*at) < self.timing.window
            })
    }

    /// How many account moves this session made within the window ending at `now`, and when the oldest of them leaves it.
    #[must_use]
    pub fn account_moves_of(&self, now: Instant) -> (usize, Option<Instant>) {
        let window = self.timing.window;
        let mut n = 0;
        let mut oldest = None;
        for &t in &self.account_moves {
            if now.saturating_duration_since(t) < window {
                n += 1;
                oldest = oldest.or(Some(later(t, window)));
            }
        }
        (n, oldest)
    }

    /// What this point says of the account move not answered yet. A sign-in menu drawn in place
    /// of the composer (`TurnEndReading::at_sign_in_menu`) is no point, but it answers the move
    /// all the same: no transcript stands there, so its lost login is the moved-to process's own
    /// ([`AccountBad::SignedOut`]), unless an act is still awaited.
    #[must_use]
    pub fn account_answer(&self, reading: &TurnEndReading) -> Option<AccountAnswer> {
        self.account_answer_as(reading, self.account_asked, self.account_startup)
    }

    /// [`Self::account_answer`], with `asked` saying whether a turn was asked under the move and
    /// `startup` whether this point is the moved-to process's start.
    fn account_answer_as(
        &self,
        reading: &TurnEndReading,
        asked: bool,
        startup: bool,
    ) -> Option<AccountAnswer> {
        self.account.as_ref()?;
        if !reading.at_point(self.stale_input) {
            return (reading.at_sign_in_menu()
                && moves_accounts(reading.program)
                && !reading.survey
                && !self.account_told
                && !(self.awaiting.is_some() && reading.worked.is_none()))
            .then_some(AccountAnswer::Bad(AccountBad::SignedOut));
        }
        if self.account_told {
            let worked = reading.worked.is_some() || self.limit_went_on(reading, None);
            return (reading.wall.is_none()
                && worked
                && !reading.pending_input
                && !startup
                && !self.login_drew(reading))
            .then_some(AccountAnswer::Served);
        }
        let wall = reading.wall.or_else(|| {
            self.wall
                .as_ref()
                .filter(|t| reading.worked.is_none() && left_waited_login(self, t, reading))
                .map(|_| WallKind::Auth)
        });
        match wall {
            Some(WallKind::Auth)
                if (!self.login_left(reading) || self.ran_under_move(reading))
                    && !(self.awaiting.is_some() && reading.worked.is_none()) =>
            {
                Some(AccountAnswer::Bad(AccountBad::SignedOut))
            }
            Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
                if asked && !(self.awaiting.is_some() && reading.worked.is_none()) =>
            {
                Some(AccountAnswer::Bad(AccountBad::Wall))
            }
            None if asked
                && reading.worked.is_some()
                && !reading.interrupted
                && !startup
                && !self.login_drew(reading)
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountAnswer::Served)
            }
            _ => None,
        }
    }

    /// What a turn under the move says of it where no read saw that turn run. The act asked
    /// under the move, answered on screen or in the record before any read saw it busy
    /// ([`Self::answered_unseen`], read at `now`), is a turn the moved-to login served as much as
    /// one a read saw busy, so a login that answers at once has served ([`Self::account_answer`]
    /// of the point with that work). Where no act of this policy's awaits its answer (a move
    /// ahead of a wall, made at a turn end, asks none), a prompt answered on screen that no point
    /// before this one closed (another than the last prompt a point read) is a turn someone asked
    /// under the move, and its answer is the move's the same way. At the restart's first point the
    /// screen is the resumed transcript, whose prompts the login before the move answered: a
    /// prompt counts there only against the one the move's own point closed, and nothing does
    /// where that point closed none. `None` where a read saw work, nothing answered under the
    /// move, or no move awaits its answer.
    #[must_use]
    pub fn account_answer_unseen(
        &self,
        reading: &TurnEndReading,
        now: Instant,
    ) -> Option<AccountAnswer> {
        if reading.worked.is_some() {
            return None;
        }
        if let Some(work) = self.answered_unseen(reading, now) {
            return self.account_answer(&TurnEndReading {
                worked: Some(work),
                ..reading.clone()
            });
        }
        if self.untaken().is_some() {
            return None;
        }
        let answered = reading
            .answered
            .filter(|a| self.last_prompt.is_some_and(|last| last != a.prompt))?;
        if self.account_startup
            && self
                .account_closed
                .is_none_or(|closed| closed == answered.prompt)
        {
            return None;
        }
        self.account_answer_as(
            &TurnEndReading {
                worked: Some(answered.work.unwrap_or_default()),
                ..reading.clone()
            },
            true,
            false,
        )
    }

    /// THE LOST LOGIN LEFT TO THE PERSON OVER A PIN WHOSE NOTICE LEFT THE SCREEN: `reading` as the
    /// point read it (its notice, from the track, as [`still_shown`] puts it back), where the
    /// login's track stands with no `/login` typed and the agent's settings leave `/login` to the
    /// person ([`super::login::LoginPin::leaves_login`]), so the person is told of it as at the
    /// notice on screen, until the login is back (the review of fix/landed-review-2, 2026-11-02).
    #[must_use]
    pub(super) fn pinned_login_left(&self, reading: &TurnEndReading) -> Option<TurnEndReading> {
        let t = self
            .wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth && t.attempts == 0)?;
        (reading.wall.is_none() && reading.login_pin.leaves_login() && !reading.login_back).then(
            || TurnEndReading {
                wall: Some(WallKind::Auth),
                wall_message: t.said.clone(),
                ..reading.clone()
            },
        )
    }

    /// THE LOST LOGIN A MOVE MADE AT A WALL LEFT: the resumed transcript still shows the wall the
    /// session moved off, until the moved-to process is asked a turn or the move's answer is told.
    /// Never at a sign-in menu ([`TurnEndReading::at_sign_in_menu`]): no transcript is drawn
    /// there, so its lost login is the moved-to process's own.
    #[must_use]
    pub(super) fn login_left(&self, reading: &TurnEndReading) -> bool {
        reading.wall == Some(WallKind::Auth)
            && !reading.at_sign_in_menu()
            && self.account.as_ref().is_some_and(|m| m.wall)
            && !self.account_asked
            && !self.account_told
    }

    const fn login_drew(&self, reading: &TurnEndReading) -> bool {
        self.wall.is_some()
            && reading.worked.is_some()
            && (reading.login_screen.is_some() || reading.login_interrupted || reading.login_back)
    }

    /// A BUCKET THE SESSION LEFT.
    #[must_use]
    pub fn bucket_left(&self, reading: &TurnEndReading) -> bool {
        reading
            .wall
            .is_some_and(|kind| self.left_bucket(kind, &reading.wall_message))
    }

    /// [`Self::bucket_left`] for a wall of `kind` in `message`'s words.
    #[must_use]
    pub fn left_bucket(&self, kind: WallKind, message: &str) -> bool {
        self.fallback_unasked
            && matches!(kind, WallKind::ModelBucket { .. })
            && self
                .model
                .as_ref()
                .and_then(|m| m.from.as_deref())
                .is_some_and(|from| bucket_model(message).as_deref() == Some(from))
            && !self
                .notice_unix
                .zip(self.fallback_made())
                .is_some_and(|(written, made)| written > made)
    }

    fn relaunched_off(&self, t: &WallTrack) -> bool {
        t.class == WallClass::Model
            && t.named.is_none()
            && self
                .model
                .as_ref()
                .and_then(|m| m.from.as_deref())
                .is_some_and(|from| bucket_model(&t.said).as_deref() == Some(from))
    }

    fn fallback_made(&self) -> Option<i64> {
        self.fallback_started.or(self.fallback_unix)
    }

    /// THE FALLBACK'S RELAUNCH WAS MADE at `unix` (seconds), told by the loop after [`Self::restarted`] made it.
    pub const fn fallback_made_at(&mut self, unix: i64) {
        self.fallback_unix = Some(unix);
        self.fallback_started = None;
    }

    /// WHEN THE TRANSCRIPT'S NOTICE WAS WRITTEN, at this decision's read, told by the loop (`supervise/wall_confirm.rs`).
    pub const fn notice_written_at(&mut self, unix: Option<i64>) {
        self.notice_unix = unix;
    }

    /// When the transcript's notice the last decision read was written.
    #[must_use]
    pub const fn notice_written(&self) -> Option<i64> {
        self.notice_unix
    }

    /// THE FALLBACK'S RELAUNCH WAS MADE AND NO TURN ASKED since ([`Self::bucket_left`]'s latch).
    #[must_use]
    pub const fn fallback_unasked(&self) -> bool {
        self.fallback_unasked
    }

    /// Seed a fallback relaunch from a previous loop where no turn has been asked yet.
    pub const fn seed_fallback_unasked(&mut self, made_at: i64) {
        self.fallback_unasked = self.model.is_some();
        self.fallback_unix = Some(made_at);
        self.fallback_started = None;
    }

    const fn ran_under_move(&self, reading: &TurnEndReading) -> bool {
        self.account.is_some()
            && reading.worked.is_some()
            && !reading.pending_input
            && !self.account_startup
    }

    /// The point the host is asked about ([`AccountPoint`]): where no move or follow is asked
    /// here, a roster row's own lost login still is (`login_end_told`).
    #[must_use]
    pub fn account_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
    ) -> Option<AccountPoint> {
        self.account_point_moving(reading, cfg).or_else(|| {
            self.login_end_told(reading, cfg)
                .then_some(AccountPoint::LoginEnded)
        })
    }

    /// A ROSTER ROW'S OWN LOST LOGIN, TOLD THE HOST WHATEVER HOLDS THE TAB'S MOVE OFF IT: the
    /// login wall of a session whose login is a roster row's (`login_label`, `logins` on) is
    /// [`AccountPoint::LoginEnded`] though no relaunch reaches the tab, a draft stands, the tab
    /// has no task or its task is done, so the host marks the row signed out and aterm's sign-in
    /// ladder signs it in (`crate::harness::accounts_sync`'s tier 0) — the one route that asks no
    /// person. At a point, and at a sign-in menu drawn in place of the composer
    /// ([`TurnEndReading::at_sign_in_menu`]), which is no point; past a `/login` aterm typed and
    /// left ([`Self::login_typed_left`]) too, whose wall its `Login interrupted` stands below. Not
    /// over a move whose wall the session left, an answer still awaited, a login back in another
    /// tab, or an upgrade's restart.
    fn login_end_told(&self, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
        cfg.logins
            && reading.login_label.is_some()
            && moves_accounts(reading.program)
            && (reading.wall == Some(WallKind::Auth)
                || (reading.wall.is_none()
                    && reading.login_screen.is_none()
                    && !reading.login_back
                    && self.login_typed_left()))
            && (reading.at_point(self.stale_input)
                || (reading.at_sign_in_menu() && !reading.survey))
            && !reading.upgrading
            && !self.login_left(reading)
            && !self.account_answer_awaited(reading)
            && !login_back_elsewhere(self, reading)
    }

    fn account_point_moving(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
    ) -> Option<AccountPoint> {
        let rotates = account_rotates(self, reading, cfg);
        let follows = login_follows(self, reading, cfg);
        let lost = follows
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth);
        if !reading.at_point(self.stale_input)
            || reading.taskless
            || reading.upgrading
            || (self.task_done() && !lost)
            || self.reset_resume.is_some()
            || !(rotates || follows)
        {
            return None;
        }
        let shown = reading.wall.filter(|_| !self.bucket_left(reading));
        let wall = shown.or_else(|| {
            let t = self.wall.as_ref()?;
            left_named(t, reading)
                .map(|n| n.kind)
                .or_else(|| left_waited_login(self, t, reading).then_some(WallKind::Auth))
        });
        match wall {
            Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
                if self.account_resume =>
            {
                None
            }
            Some(
                kind @ (WallKind::UsageSession
                | WallKind::UsageWeekly
                | WallKind::ModelBucket { .. }),
            ) if rotates => Some(AccountPoint::Wall { kind }),
            Some(WallKind::Auth)
                if self.login_left(reading) || self.account_answer_awaited(reading) =>
            {
                None
            }
            Some(kind @ WallKind::Auth) if self.account.is_some() && rotates => {
                Some(AccountPoint::Wall { kind })
            }
            Some(WallKind::Auth) if reading.login_label.is_some() && follows => {
                Some(AccountPoint::LoginEnded)
            }
            None if self.account.is_none()
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountPoint::TurnEnd)
            }
            _ => None,
        }
    }

    /// Whether this point may take the accounts probe ([`RULE_USAGE_PROBE`]).
    #[must_use]
    pub fn probe_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> bool {
        self.probe_gate(reading, cfg, now).is_none()
    }

    /// [`Self::probe_point`] by the gate that shut.
    #[must_use]
    pub fn probe_gate(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> Option<ProbeGate> {
        let _ = now;
        let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
        let owed = self.awaiting.is_some() || self.after.is_some() || self.account_resume;
        let checks = [
            (
                !matches!(reading.account, Some(AccountAdvice::Probe { .. })),
                ProbeGate::NotDue,
            ),
            (reading.phase != Phase::Idle, ProbeGate::NotIdle),
            (!reading.authoritative, ProbeGate::NotAuthoritative),
            (reading.survey, ProbeGate::Survey),
            (
                reading.wall.is_some() || self.wall.is_some(),
                ProbeGate::Wall,
            ),
            (
                !matches!(reading.composer, Composer::Empty | Composer::Placeholder(_)),
                ProbeGate::Composer,
            ),
            (reading.pending_input, ProbeGate::PendingInput),
            (reading.interrupted, ProbeGate::Interrupted),
            (reading.asked().is_some(), ProbeGate::Question),
            (
                self.account_point(reading, cfg) != Some(AccountPoint::TurnEnd),
                ProbeGate::NotTurnEnd,
            ),
            (owed, ProbeGate::ActOwed),
            // THE LAND CHECK GOES FIRST: it is one step of the landing, typed at its defense's
            // point ([`decide_turn_end`]); a probe there would put its panel between the defense
            // and the check, and the check's answer is what ends the task.
            (
                reading.autopilot == Autopilot::Land && self.land.check_owed,
                ProbeGate::LandCheck,
            ),
            (
                reading.person.is_some_and(|ago| ago < grace),
                ProbeGate::Person,
            ),
        ];
        checks
            .into_iter()
            .find_map(|(shut, gate)| shut.then_some(gate))
    }

    /// The loop told its host what answered the move ([`Self::account_answer`]).
    pub fn account_told(&mut self, answer: AccountAnswer) {
        match answer {
            AccountAnswer::Served => self.account_over(),
            AccountAnswer::Bad(_) => self.account_told = true,
        }
    }

    fn account_over(&mut self) {
        self.account = None;
        self.account_asked = false;
        self.account_told = false;
        self.account_startup = false;
        self.account_resume = false;
    }

    fn wall_words<'a>(&'a self, reading: &'a TurnEndReading) -> &'a str {
        match &self.wall {
            Some(t) if reading.wall.is_none() => &t.said,
            _ => &reading.wall_message,
        }
    }

    /// The lost login's track, if one stands.
    #[must_use]
    pub fn login_track(&self) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth)
            .map(|t| t.attempts)
    }

    fn fold_credits(&mut self, reading: &TurnEndReading, now: Instant) {
        let grace = self.timing.grace_for(reading.program);
        let bound = self.credits_bound();
        let Some(read) = &reading.credits else {
            let stands = self.credits.as_ref().is_some_and(|c| {
                c.model.is_some()
                    || match c.reset {
                        Some(at) => now < later(at, grace),
                        None => c
                            .since
                            .is_some_and(|since| now.saturating_duration_since(since) < bound),
                    }
            });
            if reading.composer != Composer::Absent && !stands {
                self.credits = None;
            }
            return;
        };
        let episode = self.credits.get_or_insert_with(CreditsEpisode::default);
        if episode.since.is_none() {
            episode.since = Some(read.since.unwrap_or(now));
        }
        if read.model.is_some() {
            episode.model.clone_from(&read.model);
        }
        let ended = match episode.reset {
            Some(at) => now >= later(at, grace),
            None => {
                episode.model.is_none()
                    && episode
                        .since
                        .is_some_and(|since| now >= later(since, bound))
            }
        };
        if ended && !read.kept && reading.worked.is_some() {
            *episode = CreditsEpisode {
                reset: None,
                since: Some(now),
                model: read.model.clone(),
                passed: episode.reset.or(episode.passed),
                told: episode.reset.is_none() && episode.told,
            };
        }
        if let Some(at) = read.reset_at
            && episode
                .passed
                .is_none_or(|passed| at > later(passed, SAME_CREDITS_RESET))
        {
            episode.reset = Some(at);
        }
    }

    const fn credits_bound(&self) -> Duration {
        self.timing.credits_bound
    }

    /// THE CREDITS HOLD'S ESCALATION WAS DECIDED ([`CREDITS_HOLD_TOLD`], [`CREDITS_HOLD_MODEL_TOLD`]).
    pub fn credits_told(&mut self) {
        self.credits
            .get_or_insert_with(CreditsEpisode::default)
            .told = true;
    }

    /// WHETHER THE CREDITS HOLD STANDS at `now` under `cfg`, for a read that says credits carry the session (`read`, `None` for one that does not).
    #[must_use]
    pub fn credits_hold_stands(
        &self,
        read: Option<&CreditsCarry>,
        program: Program,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> bool {
        !cfg.usage_credits && self.credits_held(read, program, now).is_some()
    }

    /// WHEN THE CREDITS HOLD THAT STANDS at `now` under `cfg` LIFTS ([`Self::credits_hold_stands`]):
    /// `Some(None)` where it knows no such time (a model's bucket with no reset read), `None`
    /// where no hold stands.
    #[must_use]
    pub fn credits_hold_lifts(
        &self,
        read: Option<&CreditsCarry>,
        program: Program,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> Option<Option<Instant>> {
        if cfg.usage_credits {
            return None;
        }
        self.credits_held(read, program, now).map(|held| held.lifts)
    }

    fn credits_held(
        &self,
        read: Option<&CreditsCarry>,
        program: Program,
        now: Instant,
    ) -> Option<CreditsHeld> {
        let episode = self.credits.as_ref();
        let reset = match read {
            Some(read) => self.credits_reset(read),
            None => episode?.reset,
        };
        let model = episode
            .and_then(|c| c.model.clone())
            .or_else(|| read.and_then(|r| r.model.clone()));
        if read.is_none()
            && reset.is_none()
            && model.is_none()
            && episode.and_then(|c| c.since).is_none()
        {
            return None;
        }
        let lifts = match (reset, &model) {
            (Some(at), _) => Some(later(at, self.timing.grace_for(program))),
            (None, Some(_)) => None,
            (None, None) => {
                let since = episode
                    .and_then(|c| c.since)
                    .or_else(|| read.and_then(|r| r.since))
                    .unwrap_or(now);
                Some(later(since, self.credits_bound()))
            }
        };
        if lifts.is_some_and(|at| now >= at) {
            return None;
        }
        Some(CreditsHeld {
            reset,
            model,
            lifts,
            told: episode.is_some_and(|c| c.told),
        })
    }

    fn credits_reset(&self, read: &CreditsCarry) -> Option<Instant> {
        match &self.credits {
            Some(c) => c.reset.or(read
                .reset_at
                .filter(|&at| c.passed.is_none_or(|p| at > later(p, SAME_CREDITS_RESET)))),
            None => read.reset_at,
        }
    }

    /// Folds what a point shows into the state, before [`decide_turn_end`].
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        let arrived = self.points > 0 || reading.worked.is_some();
        let ran_turn = self.ran_turn_reading(reading, now);
        let reading = ran_turn.as_ref().unwrap_or(reading);
        self.screen_prompt = reading.answered.map(|a| a.prompt);
        // The seat this screen names, kept for a limit met later
        // ([`Self::note_seat`]).
        self.note_seat(reading);
        if reading.phase != Phase::Busy {
            if super::login::carried(reading.program) {
                self.take_up_login(
                    reading.login_at_wall.as_ref(),
                    reading.login_interrupted,
                    now,
                );
            }
            self.login_seed = None;
        }
        if !matches!(
            reading.login_screen,
            Some(aterm_phase::LoginScreen::Success { .. })
        ) {
            self.login_confirmed = false;
        }
        self.limit_ended = false;
        self.limit_ended_elsewhere = false;
        self.point_moved = false;
        self.left_judged = false;
        self.unlanded = false;
        // A program started fresh in a tab that ran another: the tab's books, and this loop's
        // points, are the other's ([`Self::opened_fresh`]). One this loop relaunched on the
        // session's conversation is that session, its banner drawn before the transcript
        // ([`Self::resumed`]), as is any foreground agent the host confirms aterm relaunched on
        // its conversation before that process draws its transcript ([`Self::resumed_agent`]).
        let resuming_by_restart = self
            .resumed
            .is_some_and(|at| now.saturating_duration_since(at) < RESUME_DRAWN);
        let resuming_by_host = self
            .resumed_agent
            .is_some_and(|cur| cur.resumed && !self.transcript_drawn);
        let fresh_by_host = self.resumed_agent.is_some_and(|cur| !cur.resumed);
        let resuming = !fresh_by_host && (resuming_by_restart || resuming_by_host);
        let drawn_before = self.transcript_drawn;
        if reading.phase != Phase::Busy {
            if reading.taskless {
                if !resuming {
                    self.opened_fresh = true;
                }
            } else {
                self.transcript_drawn = true;
            }
        }
        if !reading.wall_remembered
            && self.wall.as_ref().is_some_and(|t| {
                named_class(t.class) && met_under_another(t.program, reading.program)
            })
        {
            self.wall = None;
            self.fallback_unmade = false;
            self.limit_ended = true;
            self.limit_ended_elsewhere = true;
        }
        self.looked = Some((
            now,
            left_key(reading.since_limit.as_ref(), reading.auto_continue),
        ));
        if reading.autopilot != Autopilot::Land {
            self.leave_land_episode();
        }
        if reading.autopilot != Autopilot::Cruise {
            self.leave_cruise_episode();
        }
        self.autopilot = Some(reading.autopilot);
        // The person's switch to Land over a held tab ([`Self::note_word`]): weighed now against
        // the replies kept, and again once the next point's answer is counted.
        self.note_word(reading.autopilot, reading.autopilot_by_person);
        self.mode_takes_exchange(reading);
        let stopped = reading.interrupted && !matches!(reading.phase, Phase::Busy | Phase::Prompt);
        // The harness's own Esc stopped this turn ([`Self::woken_esc`]): no person's. A point
        // the Esc did not stop ends its claim.
        if !matches!(reading.phase, Phase::Busy | Phase::Prompt) && !reading.interrupted {
            self.woken_esc = None;
        }
        let persons_esc = stopped && self.woken_esc.is_none();
        let seeded_esc = self.point_seed.filter(|_| self.esc_seed);
        self.esc_at = match (persons_esc, reading.worked) {
            (false, _) => None,
            (true, Some(_)) => Some(now),
            (true, None) => Some(self.esc_at.or(seeded_esc).unwrap_or(now)),
        };
        if reading.login_screen.is_some() {
            self.standing_login.get_or_insert(StandingLogin {
                seen: now,
                backed_out: None,
                backs: 0,
            });
        } else if !matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            self.standing_login = None;
        }
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        self.fold_credits(reading, now);
        if reading.taskless
            && reading.wall.is_none()
            && self.wall.as_ref().is_some_and(|t| {
                matches!(t.class, WallClass::Usage | WallClass::Model) && !t.relaunch.startup
            })
        {
            self.wall = None;
            self.fallback_unmade = false;
            self.limit_ended = true;
        }
        let left_login = self.login_left(reading) && !self.ran_under_move(reading);
        let left_bucket = self.bucket_left(reading);
        self.fallback_unasked = left_bucket;
        let unwalled;
        let reading = if left_login || left_bucket {
            unwalled = TurnEndReading {
                wall: None,
                wall_message: String::new(),
                ..reading.clone()
            };
            &unwalled
        } else {
            reading
        };
        let fired;
        let went_on = self.limit_went_on(reading, Some(now));
        let reading = if went_on {
            fired = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &fired
        } else {
            reading
        };
        let walled;
        let reading = if self.task_done()
            && reading.wall == Some(WallKind::Auth)
            && reading.worked.is_none()
            && self.awaiting.is_none()
            && !self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth)
        {
            walled = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &walled
        } else {
            reading
        };
        let login_spin;
        let reading = if self.login_drew(reading) {
            login_spin = TurnEndReading {
                worked: None,
                worked_doubt: Duration::ZERO,
                ..reading.clone()
            };
            &login_spin
        } else {
            reading
        };
        let drawn_late = self.unsaid_reading(reading, now);
        let reading = drawn_late.as_ref().unwrap_or(reading);
        let unseen;
        let reading = match self
            .answered_unseen(reading, now)
            .or_else(|| self.host_answered_unseen(reading, now))
        {
            Some(work) => {
                unseen = TurnEndReading {
                    worked: Some(work),
                    ..reading.clone()
                };
                &unseen
            }
            None => reading,
        };
        let other_unseen;
        let books_turn = self.unseen_turn(reading);
        let reading = if books_turn {
            other_unseen = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &other_unseen
        } else {
            reading
        };
        // An act's answer that ran no tool, however long, is no work at a read wall or at a kept reply its tool rows are read at.
        let retried_alone;
        let reading = if !reading.ran_tools
            && (self.awaiting.is_some_and(|a| !a.command)
                || self.yield_seed.is_some_and(|y| y.typed))
            && reading.worked.is_some_and(|w| !w.is_zero())
            && (reading.wall.is_some()
                || (reading.program.reader().reads_tool_rows()
                    && self.replies.has(reply_mark(reading))))
        {
            retried_alone = TurnEndReading {
                worked: Some(Duration::ZERO),
                worked_doubt: Duration::ZERO,
                ..reading.clone()
            };
            &retried_alone
        } else {
            reading
        };
        if !reading.pending_input || reading.worked.is_some() {
            self.stale_input = false;
        }
        let awaited_command = self.awaiting.is_some_and(|a| a.command);
        let worked_to_wall =
            !awaited_command && reading.worked.is_some_and(|w| w >= self.timing.progress);
        if !self.point_judged(reading, now) {
            // No point yet: the wall it shows is tracked all the same (taking up a latch a
            // loop before this one left, as the first wall this state sees does), and an
            // act awaited is answered by it ([`Awaited::met`]).
            if let Some(kind) = reading.wall {
                let unmade_seeded = std::mem::take(&mut self.account_unmade_seed);
                let retried = self.awaiting.as_mut().is_some_and(|a| {
                    let first = !std::mem::replace(&mut a.met, true);
                    first && a.wall_retry
                });
                let at_point = false;
                self.track_wall(
                    reading,
                    kind,
                    retried,
                    worked_to_wall,
                    unmade_seeded,
                    at_point,
                    Arrival {
                        hand: false,
                        seen: arrived,
                    },
                    now,
                );
            }
            return;
        }
        self.other_turn = false;
        self.quiet_wake = false;
        let wake_repo = self.wake_repo.take();
        let seeded = self.landed_seed.take();
        let landing_words = self.landed_words.take();
        let gave_up_seed = self.gave_up_seed.take();
        let seeded_by_wake = std::mem::take(&mut self.seed_by_wake);
        let noted = self.seed_noted_ms.take();
        self.restating = noted
            .filter(|_| seeded.is_some())
            .map(|at| (TaskFace::Landed, at));
        let min = self.timing.min_work;
        if reading.program != Program::Generic {
            if self.short_program.is_some_and(|p| p != reading.program) {
                self.short = 0;
                self.cut_uncounted = false;
                self.cut_pass_spent = false;
            }
            self.short_program = Some(reading.program);
        }
        let short_before = self.short;
        let pass_before = self.cut_pass_spent;
        let point_at_before = self.point_at;
        let seeded_point = self.point_seed.take();
        let check_point = self.check_point_seed.take();
        let land_point = std::mem::take(&mut self.land_point_seed);
        self.esc_seed = false;
        let seeded_yield = self.yield_seed.take();
        let unseen_turn = std::mem::take(&mut self.gap_turn);
        let same_point =
            seeded_point.is_some() && reading.worked.is_none() && self.awaiting.is_none();
        let mut checked = std::mem::take(&mut self.checking);
        let mut landing = std::mem::take(&mut self.landing);
        let landing_act = self.landing_act.take();
        self.seed_answered = false;
        let met = self.awaiting.is_some_and(|a| a.met);
        // The door's line, where no act was awaited before it: its answer is judged as the line's
        // ([`HostTurn::Fact`]); with one, as that act's.
        let fact = self.fact.take();
        let host = self
            .awaiting
            .and_then(|a| a.host)
            .or(fact.map(|_| HostTurn::Fact));
        let unseen_after_off =
            self.awaiting.is_some_and(|a| a.off_stood) && reading.worked.is_none();
        let wall_retry = self.awaiting.is_some_and(|a| a.wall_retry);
        let own_act = self.awaiting.is_some_and(|a| a.own);
        let act_taken = self.awaiting.is_some_and(|a| a.taken) || reading.worked.is_some();
        let act_submitted = self.awaiting.is_some_and(|a| a.submitted_since(reading));
        let act_typed = self.awaiting.map(|a| a.typed);
        let awaited = self.awaiting.take().map(|a| a.rule);
        // The switch's ask is answered at its act's point ([`SwitchAsk::Typed`]): the same answer
        // holds again there with no second prompt, as any repeat does ([`Self::repeats`]).
        if awaited.is_some() && self.switch_ask == SwitchAsk::Typed {
            self.switch_ask = SwitchAsk::None;
        }
        let mut own_going_on = false;
        if awaited.is_some() || reading.worked.is_some() {
            // THE VENDOR'S OWN CONTINUATION of the limit that cut it (Claude
            // Code's `Usage limit reset · continuing automatically`, no `❯`
            // row) delivers it again, as the wall's retry does: the turn
            // that ran here, no act of this policy's awaited, is its answer
            // (read as a turn nobody asked, its done reply
            // was a first one — the check typed a second time, under Land a
            // nudge).
            if awaited.is_none()
                && reading.wall.is_none()
                && reading.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            {
                checked |= self.cut_check;
                landing |= self.cut_land;
                own_going_on = self.cut_uncounted;
            }
            let cut = reading.wall.is_some();
            self.cut_check = cut
                && reading.autopilot == Autopilot::Cruise
                && (checked || host == Some(HostTurn::Check));
            self.cut_land = cut && landing;
            self.cut_reply = cut;
            if !cut || awaited.is_some() {
                self.cut_uncounted = false;
            }
        }
        let done = says_done(reading.said_tail.as_deref());
        // WHAT THE REPLY'S WORDS LAND IS READ ONCE AT MOST, AND ONLY WHERE THE POINT'S OWN FACTS LEAVE
        // IT TO DECIDE: both reads are functions of this reading alone, and the words of a long reply
        // are the costliest thing this reads.
        let defended = std::cell::LazyCell::new(|| clean_defense(reading.said_tail.as_deref()));
        // THE LAND CHECK DECIDES A LANDING (§14.17 item 5): the awaited act was the check and
        // its answer is `LANDED` alone, over a clean repository read at that answer's yield. The
        // word reader's verdict on a defense ([`clean_defense`]) lands nothing.
        // A LAND ACT A LOOP BEFORE THIS ONE TYPED, ITS TURN NOT YET SEEN ([`Self::act_seen`]): the
        // point it was typed at, read again, is not the check's answer; the check stays owed its
        // answer, which a turn that ran since gives.
        let act_seen = Self::act_seen(landing_act, reading, now);
        let act_owed = landing && !act_seen && reading.worked.is_none() && awaited.is_none();
        let check_yield = landing && !act_owed && self.land.check_pending;
        // A CHECK'S TURN THE PERSON STOPPED (Esc) gave no answer: its last words are not the
        // agent's audit, so nothing lands on them and the defense stays owed its check. Nor did
        // one that said nothing, judged once its take window ran out at an idle composer
        // ([`Self::unsaid_reading`]) or where it is read in a question box: no words are no
        // audit either.
        let no_answer = check_yield && (reading.interrupted || reading.unsaid());
        let answer_read = std::cell::LazyCell::new(|| {
            land_check::land_check_answer(
                reading.said_tail.as_deref().unwrap_or_default(),
                reading.before(),
                reading.said_cut,
            )
        });
        let landed_answer = || *answer_read == land_check::CheckAnswer::Landed;
        let check_answer = check_yield.then(|| (*answer_read).clone());
        // A ROW OF THE AGENT'S LEFT RUNNING UNKEPT holds the landing the check's `LANDED` would make
        // ([`Self::left_holds`]). A landing made stands over it: a seeded one, one its words or its
        // wake hold, and one a finished record keeps (`by_record`, which keeps only a landing this
        // loop or its journal made, whose `LANDED` the leg already let land).
        let left_held = self.left_holds(reading);
        let clean = check_answer == Some(land_check::CheckAnswer::Landed)
            && reading.repo.is_clean()
            && !left_held;
        // A LANDING ITS JOURNAL OR ITS PARK HOLDS stands on the reply that landed it: the check's
        // `LANDED`, or an older build's clean defense.
        let seed_clean = || {
            reading.repo.is_clean()
                && ((!reading.said_cut && !reading.need_before() && *defended) || landed_answer())
        };
        let record_says = reading.finished_record == Some(true)
            && !reading.pending_input
            && reading.autopilot != Autopilot::Cruise;
        let by_record = record_says && reading.repo.is_clean();
        let off_stood = self.off_landing.is_some() || seeded == Some(false);
        let began_over = !off_stood
            && wake_repo.as_ref().is_some_and(|began| {
                crate::harness::repo_work::RepoRead::same(began, &reading.repo)
            });
        let wake_repo_kept = reading.repo.is_clean() || began_over;
        let wake_keeps = record_says && wake_repo_kept && !wake_left_open(reading);
        let repo_theirs = reading.repo.found().is_some() && began_over;
        let landed_before = self.landed();
        let tail_mark = said_mark(reading.said_tail.as_deref());
        let words_hold = || {
            !reading.need_before()
                && landing_words.is_some_and(|w| tail_mark == Some(w))
                && (seeded == Some(true) || reading.repo.is_clean())
                && reading.worked.is_none()
                && awaited.is_none()
                && (*defended || landed_answer())
        };
        let wake_hold = seeded_by_wake
            && record_says
            && landing_words.is_some_and(|w| tail_mark == Some(w))
            && !wake_words_open(reading)
            && (seeded == Some(true) || reading.repo.is_clean())
            && reading.worked.is_none()
            && awaited.is_none();
        let streak = |short: u32, d: Duration, done: bool| {
            if d < min || done {
                short.saturating_add(1)
            } else {
                0
            }
        };
        // THE STREAK COUNTS ONLY THE AGENT'S OWN SHORT ANSWERS TO ATERM (v6 §14.17.5.7). A turn a
        // wall cut (an outage, an overload, a rate limit, a dropped connection, a lost login: any
        // wall a reader names) was answered by the system message, not the agent; the vendor's
        // own continuation that delivers it again is the wall's resume, as the loop's own try is;
        // and a person's own turn (their hand in it) is no prompt of aterm's. None of them adds to
        // the streak, so it stands at the agent's short answers before and after; real work in
        // any of them still ends it (`SupervisorTurnEnd`'s `NeverBackOffAfterRealWork`). A wall
        // gets ONE such pass per rung of the streak ([`Self::cut_pass_spent`]): a cut act past it,
        // no answer of the agent's own or real work between, counts as before, so a wall that cuts
        // every act still climbs the ladder (`SupervisorTurnEnd`'s `AWallPassesOncePerRung`).
        let uncounted = |short: u32, d: Duration, done: bool| streak(short, d, done).min(short);
        // A TURN THE HARNESS'S OWN STOP ENDED gave no short answer either: the restart whose cut
        // call a resume restores ([`Self::restored_cut`]) or the Esc it pressed on a turn the Mac
        // slept through ([`Self::woken_esc`]) ended it, not the agent. Whether a read caught the
        // turn running before the stop must not decide the next prompt's back-off (2026-11-03
        // real-Claude flows f1 and f2: 2 min and 4 min after the same restart prompt's reply).
        // `SupervisorTurnEnd`'s `HarnessStopsTurn` and `HarnessStopsTurnLong` are this stop, held to
        // `NoPassedCutPersonsTurnOrHarnessStopCounts` and `NeverBackOffAfterRealWork`.
        let harness_cut = reading.interrupted && self.woken_esc.is_some();
        let wall_cut = reading.wall.is_some();
        let pass = wall_cut && !self.cut_pass_spent;
        let mut cut_counted = false;
        // A program started fresh since the last point carries nothing of the task before it
        // ([`Self::opened_fresh`]): its opening message is the person's, carried task or none.
        let carried = self.carrying && self.done == 0 && !self.opened_fresh;
        let redelivers = reading.worked.is_some()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Auth && t.login.hand);
        let hand = self.hand(reading, now);
        let keyed_in_turn = self.came_in_turn(reading, reading.key, now);
        let theirs = !carried && !reading.interrupted && (hand || redelivers);
        let answered_anew = reading
            .answered
            .is_some_and(|a| Some(a.prompt) != self.last_prompt);
        let prompted = !went_on && (hand || answered_anew);
        let seed_was_armed = seeded.is_some();
        let off_landing = self.off_landing.take();
        let ok_to_leave = reading.wall.is_none() && !reading.pending_input;
        let (points_before, done_before) = (self.points, self.done);
        let yield_stopped = seeded_yield.is_some() && reading.interrupted;
        let first_point = self.point_at.is_none();
        let mut seed_mark = None;
        let mut wake_kept = false;
        let mut land_judged = false;
        match (awaited, reading.worked) {
            (Some(RULE_HOST_TURN) | None, worked) if host == Some(HostTurn::Fact) => {
                self.point_worked = worked.unwrap_or_default();
                self.new_point(now);
            }
            (Some(RULE_HOST_TURN), worked) if host == Some(HostTurn::Check) => {
                let d = worked.unwrap_or_default();
                if d >= min {
                    self.followed = Some(RULE_HOST_TURN);
                    self.point_worked = d;
                    self.new_point(now);
                    self.short = 0;
                } else if self.point_at.is_none() {
                    self.new_point(now);
                } else {
                    self.back_off_from_restart_reply(act_typed, d, now);
                }
                self.exchange = false;
                self.carrying = true;
                if ok_to_leave {
                    self.done = u8::from(done);
                }
            }
            (Some(RULE_HOST_TURN), worked) => {
                let d = worked.unwrap_or_default();
                let carry_on = host == Some(HostTurn::CarryOn);
                if act_taken && self.left_to_person() {
                    // A PERSON'S EXCHANGE LEFT TO THEM ENDS WHERE THE HOST'S
                    // TURN IS TAKEN (an upgrade's restore notice, a carry-on):
                    // the reply answers the host, not them, so the task is
                    // open again and this point is judged as the host's.
                    self.exchange = false;
                    self.done = 0;
                }
                if d >= min && (!done || carry_on) {
                    self.followed = Some(RULE_HOST_TURN);
                    self.point_worked = d;
                    self.new_point(now);
                    self.exchange = false;
                    self.carrying = true;
                    if !done {
                        self.short = 0;
                        self.done = 0;
                    } else if ok_to_leave {
                        self.done = self.done.max(1);
                    }
                } else {
                    if carry_on && done && ok_to_leave {
                        self.done = self.done.max(1);
                    }
                    if self.point_at.is_none() {
                        self.new_point(now);
                    } else if carry_on || host == Some(HostTurn::Restore) {
                        self.back_off_from_restart_reply(act_typed, d, now);
                    }
                }
            }
            (Some(rule), worked) => {
                let d = worked.unwrap_or_default();
                if unseen_after_off && self.exchange {
                    self.exchange = false;
                    self.short = self.short.max(1);
                }
                self.exchange_work = self.exchange_work.saturating_add(d);
                self.exchange &= self.exchange_work < EXCHANGE_WORK
                    && !is_continue_rule(rule)
                    && (!awaited_command || rule == RULE_LOGIN);
                if rule != RULE_LOGIN {
                    self.carrying = !self.exchange && !reading.interrupted;
                }
                let delivers = checked || self.compact_owes.is_some_and(|o| o.check);
                if !(delivers && is_wall_retry(rule)) {
                    self.followed = Some(rule);
                } else if self.followed.is_none() {
                    // The check a loop before this one typed, a wall having
                    // cut it: its retry, typed here, delivers it again.
                    self.followed = self.seeded_check;
                }
                self.point_worked = d;
                self.new_point(now);
                if is_continue_rule(rule) {
                    self.short = if pass {
                        self.cut_pass_spent = true;
                        uncounted(self.short, d, done)
                    } else if harness_cut {
                        uncounted(self.short, d, done)
                    } else {
                        cut_counted = wall_cut;
                        streak(self.short, d, done)
                    };
                    self.cut_uncounted = wall_cut;
                } else if !awaited_command && d >= min && !done {
                    // Real work ends the streak whatever act it answered, a wall's retry included.
                    self.short = 0;
                }
                if ok_to_leave {
                    land_judged = landing && !self.exchange && act_taken && !no_answer;
                    let lands = land_judged && clean;
                    self.done = if self.exchange || lands {
                        2
                    } else {
                        u8::from(done)
                    };
                    self.landed = lands;
                    self.by_check = lands;
                }
            }
            (None, Some(_)) if wake_keeps && landed_before => wake_kept = true,
            (None, Some(_)) if wake_keeps && self.point_at.is_none() && seeded.is_some() => {
                self.new_point(now);
                self.done = 2;
                self.landed = true;
                self.by_check = false;
                self.exchange = false;
                if let Some(episode) = seeded {
                    self.land.typed |= episode;
                }
                wake_kept = true;
            }
            (None, Some(d)) => {
                self.exchange_work = d;
                // CRUISE NEVER PARKS: a person's own exchange is left to them under Land and Off
                // alone, and under Land only where it opens the session (owner ruling 2026-10-27)
                // of no managed worker ([`Self::mode_leaves_exchange`]):
                // a person's message once the session has worked (a point this loop judged, or a
                // loop before it: [`Self::seed_point`], [`Self::seed_worked`]) interrupts that
                // work, which Land carries on after the reply. A program started fresh in the
                // tab since opens a session of its own ([`Self::opened_fresh`]), as does one that
                // began in this turn, unread before ([`Self::launched_in_turn`]), or one whose
                // opening exchange the first point after Off shows ([`Self::opening_read`]); one
                // that resumes a conversation carries a session that worked before
                // ([`AgentResumed::resumed`]).
                let resumed_launch = self.resumed_agent.is_some_and(|cur| cur.resumed);
                self.exchange_opens = (first_point
                    && !resumed_launch
                    && ((seeded_point.is_none() && !self.worked_before)
                        || (self.off_stood && self.opening_drawn)))
                    || self.opened_fresh
                    || (!drawn_before && self.launched_in_turn(d, now));
                self.exchange =
                    theirs && d < EXCHANGE_WORK && self.mode_leaves_exchange(reading.autopilot);
                if self.exchange {
                    // This loop's own `LEFT` row for it, read again at the same turn (the answer
                    // to a wall's retry, a relaunch's start-up), is judged as a loop that follows
                    // judges it (`approvals::left_after_work_in`): under Land, one Off left after
                    // work is the work's.
                    self.left_after_work = !self.exchange_opens;
                }
                self.carrying &= carried;
                self.landed = false;
                let left = self.exchange && ok_to_leave;
                self.followed = if checked { self.seeded_check } else { None };
                self.point_worked = d;
                self.new_point(now);
                land_judged = landing && !left && ok_to_leave && !no_answer;
                self.land.legs = None;
                let lands = land_judged && clean;
                self.done = if left || lands {
                    2
                } else {
                    u8::from(done && reading.wall.is_none())
                };
                self.landed = lands;
                self.by_check = lands;
                match seeded_yield {
                    Some(y) => {
                        if yield_stopped {
                            self.carrying = false;
                        }
                        if y.continues {
                            // The act a loop before this one typed, its answer read here: judged
                            // as that loop would have judged it, its pass and its delivery kept.
                            self.short = if pass {
                                self.cut_pass_spent = true;
                                uncounted(self.short, d, done)
                            } else if harness_cut {
                                uncounted(self.short, d, done)
                            } else {
                                cut_counted = wall_cut;
                                streak(self.short, d, done)
                            };
                            self.cut_uncounted = wall_cut;
                        } else if y.typed && d >= min && !done {
                            self.short = 0;
                        }
                    }
                    None if reading.wall.is_none() && !left => {
                        // A short vendor continuation delivers the cut act, which it does not
                        // count again, and a person's own turn (their hand in it, as the hold
                        // reads it: a key, a scroll or a click, a draft, a driver's lease) is no
                        // prompt of aterm's: neither adds to the streak. A turn no act of aterm's
                        // asked is never aterm's to count but for a program's own going on, so
                        // the hand only narrows the count toward that rule, as the harness's own
                        // stop does. Real work in any of them must still clear it, rather than
                        // inheriting an overbroad skip.
                        self.short = if own_going_on || hand || harness_cut {
                            uncounted(self.short, d, false)
                        } else {
                            streak(self.short, d, false)
                        };
                    }
                    None => {}
                }
            }
            (None, None) if self.point_at.is_none() && !reading.taskless => {
                if yield_stopped {
                    self.carrying = false;
                }
                if let Some(at) = seeded_point.filter(|_| same_point) {
                    // The point a loop before this one judged: its streak
                    // and its time ([`Self::seed_point`]).
                    self.new_point(at);
                } else {
                    self.new_point(now);
                    // The cut a relaunch's resume restores, the first point of the loop that
                    // relaunch starts where its books hold a turn read running after their point,
                    // is the harness's own stop, as in the arms above.
                    if !harness_cut {
                        self.short = self.short.saturating_add(1);
                    }
                }
                land_judged = landing && act_seen && ok_to_leave && !no_answer;
                if landing && act_seen && reading.wall.is_some() {
                    self.cut_land = true;
                }
                if checked && reading.autopilot == Autopilot::Cruise && reading.wall.is_some() {
                    self.cut_check = true;
                }
                if let Some(rule) = check_point.filter(|_| {
                    same_point && reading.autopilot == Autopilot::Cruise && reading.wall.is_some()
                }) {
                    self.cut_check = true;
                    self.seeded_check = Some(rule);
                }
                if land_point && same_point && reading.wall.is_some() {
                    self.cut_land = true;
                }
                let lands = land_judged && clean;
                self.done = if lands {
                    2
                } else {
                    u8::from(done && reading.wall.is_none())
                };
                self.landed = lands;
                self.by_check = lands;
                self.exchange = false;
                let at_wall = by_record && reading.wall.is_some();
                if let Some(episode) = seeded
                    && reading.autopilot == Autopilot::Land
                    && ((ok_to_leave
                        && landing_words.is_none_or(|w| tail_mark == Some(w))
                        && (wake_hold || seed_clean() || words_hold()))
                        || at_wall)
                {
                    self.done = 2;
                    self.landed = true;
                    self.by_check = false;
                    self.land.typed |= episode;
                    self.by_wake = seeded_by_wake;
                    if at_wall {
                        seed_mark = landing_words;
                    }
                }
            }
            (None, None)
                if off_landing
                    .is_some_and(|mark| !(mark == tail_mark && ok_to_leave && seed_clean()))
                    && landed_before
                    && !by_record =>
            {
                self.landed = false;
                self.done = u8::from(done && reading.wall.is_none());
                self.followed = None;
                self.new_point(now);
                self.short = self.short.saturating_add(1);
            }
            (None, None) => {}
        }
        if act_owed {
            // Its turn comes: the landing and its act stand for the reading that answers it.
            self.landing = true;
            self.landing_act = landing_act;
        }
        // THE SAME REPLY AGAIN ([`Self::repeats`]): an act's no-work answer seen taken counts for the reply it settles to.
        let replies_before = self.replies;
        let mut answered_at = now;
        let mut settled_again = false;
        let mut answer_reset = self.reset_known(reading).map(|(r, _)| r);
        let answered = match (awaited, seeded_yield) {
            (Some(_), _) if awaited_command => None,
            (Some(_), _) => Some((
                act_taken || act_submitted || self.drawn_since(reading),
                own_act,
            )),
            (None, Some(y)) if y.typed => Some((act_taken || self.drawn_since(reading), y.own)),
            (None, _) if reading.worked.is_none() => self.settling.map(|s| {
                (self.replies, self.replies_at) = s.before;
                answered_at = s.at;
                answer_reset = s.reset.or(answer_reset);
                settled_again = true;
                (s.seen, s.own)
            }),
            (None, _) => None,
        };
        self.settling = None;
        // SOMEONE'S TURN (a turn on someone's prompt that no act asked, this loop's or, seeded,
        // the loop's before it) starts the done checks' angles again ([`Self::checks`]); the
        // agent's own work between its claims does not, so a worker that works between them still
        // meets every angle.
        if awaited.is_none() && seeded_yield.is_none() && reading.worked.is_some() && prompted {
            self.checks = 0;
        }
        // A turn the books say ran that no loop timed is work to it.
        let quiet = !unseen_turn && !reading.ran_tools;
        let idle = quiet && reading.worked.is_none_or(|w| w < min);
        // A long answer with no tool row where the reader reads them is kept too: said again, it is no work.
        let kept_long = quiet && reading.program.reader().reads_tool_rows();
        let reply = reply_mark(reading);
        // AN OUTAGE WALL'S ANSWER COUNTS FOR NO HOLD (v6 §14.17.5.2): the agent could not act on
        // the wall, so its answer is no reply it gave again, and a long outage's answers start no
        // run.
        let outage = outage_shown(self, reading).is_some();
        // A person typed since the last point read and the last answer counted.
        let person_at = reading.person.and_then(|ago| now.checked_sub(ago));
        let typed = person_at.is_some_and(|at| {
            self.person_seen.is_none_or(|seen| at > seen)
                && self.replies_at.is_none_or(|counted| at > counted)
        });
        self.person_seen = person_at.max(self.person_seen);
        match answered {
            // An act whose row stands unanswered got no reply: the replies stand.
            Some(_) if reading.pending_input && quiet && !settled_again => {}
            _ if outage && (idle || kept_long) => {}
            Some((seen, own)) if idle || kept_long => {
                if reading.wall.is_some() && !settled_again {
                    self.settling = Some(Settling {
                        before: (self.replies, self.replies_at),
                        at: answered_at,
                        reset: answer_reset,
                        seen,
                        own,
                    });
                }
                if seen {
                    self.replies.count(reply, reading.wall.is_some(), own, idle);
                    self.replies_at = Some(answered_at);
                    self.replies_reset = answer_reset;
                }
            }
            // A BLOCKED REPLY (v6 §14.17.5, slice 12a): an act's answer that ran tools and names
            // what only a person can give (a `NEEDS YOU:` line) is kept, so the same reply with
            // the same need again holds although tools ran, never a prompt at each free point.
            // Only the same words again, numbers kept, are the same reply (a new count beside the
            // same need is work done between), and a tools-ran answer is never a no-work answer.
            Some((seen, own)) if seen && blocked_reply(reading) => {
                let exact = exact_mark(reading);
                if self.replies.last().is_some_and(|k| k.mark == reply)
                    && self.blocked_words == Some(exact)
                {
                    self.replies.count(reply, false, own, false);
                } else {
                    self.blocked_words = Some(exact);
                    self.replies = Replies::of(&[Kept {
                        mark: reply,
                        answers: 1,
                        walled: false,
                        tries: 0,
                    }]);
                }
                self.replies_at = Some(answered_at);
                self.replies_reset = answer_reset;
            }
            Some(_) => self.replies = Replies::default(),
            // Another turn lifts the hold only on someone's prompt or keys, work or a limit going on; the program's own no-work turn while a reply is kept counts as an act's answer, a long one that ran no tool at a read wall or saying a kept reply again included.
            None if !outage
                && awaited.is_none()
                && reading.worked.is_some()
                && self.replies.last().is_some()
                && !(prompted || typed || went_on || reading.pending_input)
                && (idle || kept_long || (quiet && reading.wall.is_some())) =>
            {
                let no_work = idle
                    || (quiet && reading.wall.is_some())
                    || (kept_long && self.replies.has(reply));
                self.replies
                    .count(reply, reading.wall.is_some(), false, no_work);
                self.replies_at = Some(now);
                self.replies_reset = answer_reset;
            }
            // A person's turn only the books show, no tool run seen (its start above an unread screen included), lifts nothing: they drive, and the hold lifts at their next turn a point sees.
            None if books_turn && typed && (idle || reading.tools_unread) => {}
            None if awaited.is_none() && reading.worked.is_some() => {
                self.replies = match self.replies.last() {
                    Some(k) if idle && !reading.pending_input && k.mark == reply => {
                        Replies::of(&[Kept {
                            answers: 1,
                            tries: k.tries.min(FUTILE_AFTER_TRIES - 1),
                            ..k
                        }])
                    }
                    _ => Replies::default(),
                };
            }
            // A loop's first point showing other than the newest reply a loop before it kept, no act of that loop's awaited: the program moved while no loop watched.
            None if first_point && self.replies.last().is_some_and(|k| k.mark != reply) => {
                self.replies = Replies::default();
            }
            // The program's own turn this loop saw no busy spell of, its words other than the last point's and its reply other than the newest kept: a turn of no length, an act's answer if it ran no tool.
            None if !outage
                && awaited.is_none()
                && reading.worked.is_none()
                && self.point_words.is_some_and(|w| w != words_mark(reading))
                && self.replies.last().is_some_and(|k| k.mark != reply)
                && !(prompted || typed || went_on || reading.pending_input) =>
            {
                if quiet {
                    self.replies
                        .count(reply, reading.wall.is_some(), false, true);
                    self.replies_at = Some(now);
                    self.replies_reset = answer_reset;
                } else {
                    self.replies = Replies::default();
                }
            }
            None => {}
        }
        // A dated reset passed since the last answer: the wall's replies may be answered again, each wording's tries kept.
        if self
            .reset_known(reading)
            .is_some_and(|(r, dated)| dated && now >= r && self.replies_at.is_some_and(|at| at < r))
        {
            self.replies = match self.replies.last() {
                Some(k) if k.walled && k.mark == reply => {
                    let walls: Vec<Kept> = self
                        .replies
                        .kept()
                        .filter(|k| k.walled)
                        .map(|k| Kept { answers: 0, ..k })
                        .collect();
                    Replies::of(&walls)
                }
                _ => Replies::default(),
            };
        }
        self.weigh_switch();
        self.switch_seen = false;
        if self.switch_spent.is_some_and(|m| !self.replies.has(m)) {
            self.switch_spent = None;
        }
        // The wall's pass comes back once the agent's own short answer moved the streak, or real
        // work ended it ([`Self::cut_pass_spent`]); a cut act counted past the pass keeps it spent.
        let real_work = !awaited_command && reading.worked.is_some_and(|w| w >= min) && !done;
        if real_work || (self.short > short_before && !cut_counted) {
            self.cut_pass_spent = false;
        }
        self.point_moved = self.short != short_before
            || self.cut_pass_spent != pass_before
            || self.replies != replies_before
            || ((checked || landing) && !same_point)
            || awaited.is_some()
            || (self.point_at != point_at_before && !same_point)
            || wake_kept;
        if wake_kept {
            self.by_wake = true;
            self.wake_restated = true;
        } else if !self.landed || landing {
            self.by_wake = false;
        }
        // A loop before this one left the exchange to the person, or this one did and reads its
        // row again: theirs again, but under Cruise, which never parks
        // ([`Self::mode_takes_exchange`]), and under Land once the session worked, where only its
        // opening message is the person's: a record of one left after work is an older build's,
        // under the superseded rule, or Off's, and the work goes on. Nor is a managed worker's
        // under Land ([`Self::mode_leaves_exchange`]): a record of its opening message left is an
        // older build's too.
        let left_record = reading.left_record
            && match reading.autopilot {
                Autopilot::Cruise => false,
                Autopilot::Land => !self.left_after_work && !self.managed,
                Autopilot::Off => true,
            };
        if left_record {
            self.exchange_opens = !self.left_after_work;
        }
        if left_record && !reading.pending_input && !reading.wall_drawn() {
            self.exchange = true;
            self.done = 2;
        }
        if left_record && reading.wall.is_some() && first_point {
            self.exchange = true;
            self.exchange_work = self
                .exchange_work
                .max(reading.left_work.unwrap_or_default());
        }
        if reading.autopilot == Autopilot::Cruise && self.landed && !self.exchange {
            self.landed = false;
            self.done = 1;
            self.unlanded = true;
        }
        if awaited.is_some() || reading.worked.is_some() || first_point {
            let tail = reading.said_tail.as_deref();
            let claim = self.done == 1 && !reading.interrupted;
            self.open = if claim {
                match reading.autopilot {
                    Autopilot::Land => land_open_words(tail),
                    Autopilot::Cruise | Autopilot::Off => {
                        tail.map_or_else(Vec::new, |t| open_work(Some(&needs::without_needs(t))))
                    }
                }
            } else {
                Vec::new()
            };
            let unlanded_by_repo = record_says && landed_before && !self.landed;
            self.repo = if (claim || unlanded_by_repo || check_yield) && !repo_theirs {
                reading.repo.clone()
            } else {
                crate::harness::repo_work::RepoRead::Clean
            };
            // Cruise's done check names the rows its claim left, and holds nothing on them; Land's
            // act names those of a yield its left leg held ([`Self::set_legs`]).
            self.left_said = if claim && reading.autopilot == Autopilot::Cruise {
                self.left_unkept(reading)
            } else {
                0
            };
            if !reading.pending_input {
                let walled = reading.wall.is_some();
                // Someone else's turn on someone's prompt (`prompted`): the
                // person answered what stood. A turn nobody asked — Claude
                // Code's own wake, the vendor's continuation of a limit —
                // answered nothing, and nor did the turn on the act the loop
                // before this one typed (seeded), whose prompt reads as new
                // to a loop that never typed it: there only the person's keys
                // make the turn theirs. A turn a wall cut takes nothing down.
                if !walled
                    && awaited.is_none()
                    && reading.worked.is_some()
                    && prompted
                    && (hand || seeded_yield.is_none())
                {
                    self.needs = Needs::default();
                }
                let act = self.act_words.clone().filter(|_| reading.opens_cut);
                // A TURN A WALL CUT ONLY ADDS ([`Needs::join`]): the resume the wall gets opens
                // a turn of its own, whose point reads none of these words again, so its needs go
                // up here, and its report, which would have named every need again, never came.
                // A screen's rows are read as the lines they wrap: the words the go-ahead's park
                // reads too ([`TurnEndReading::needs_said`]).
                let (before, said) = reading.needs_said();
                self.fold_turn_needs(before, said, act.as_deref(), walled);
                // THE GO-AHEAD'S COUNT ([`GoAheads`]) stands while the replies park, on any word,
                // and no new key of the person's came.
                if !walled {
                    self.go_ahead_read(reading.go_ahead_past(act.as_deref()).is_some());
                }
            }
        }
        // A turn of real work, or one a wall cut, starts the go-ahead's span again
        // ([`Self::go_ahead_worked`]), at a wall as at a clean point.
        if reading.worked.is_some() || reading.wall.is_some() {
            self.go_ahead_worked(reading.worked.unwrap_or_default(), reading.wall.is_some());
        }
        // THE PERSON'S LAST KEY, read wherever a point is judged, at a wall as at a clean point
        // ([`Self::person_keyed`]): its hour runs from this point where the key came in the turn
        // that ended here ([`Self::came_in_turn`]: a message they typed, queued behind an act, a
        // host's fact or a carry, or typed into it), else from the key itself, so a restart,
        // timed by the window's clock, does not start their hour again.
        if let Some(key) = reading.key.and_then(|ago| now.checked_sub(ago)) {
            self.person_keyed(key, keyed_in_turn.then_some(now), now);
        }
        // A TURN RUN SINCE, OR CRUISE, WHICH NEVER RESTS, LIFTS A WORKER'S FAILED CONCLUSION: the
        // point is a report again.
        let turn_ran = awaited.is_some() || reading.worked.is_some();
        self.point_turnless = !turn_ran;
        if self.gave_up.is_some() && (turn_ran || reading.autopilot == Autopilot::Cruise) {
            self.gave_up = None;
            if self.done >= 2 && !self.landed && !self.exchange {
                self.done = u8::from(done && reading.wall.is_none());
            }
        }
        if self.done < 2 {
            self.done_at = None;
        } else if done_before < 2 || self.points != points_before {
            self.done_at = Some(now);
        }
        if self.done < 2 {
            self.landed = false;
        }
        // THE LAND CHECK'S BOOKS: real work (the streak's floor, `min`) lets a defense be checked
        // again and lifts a rest, as does someone else's turn; a short turn that ran a tool (a
        // `git status`) is no new work for the check, so the same defense and the same list are
        // not checked again. At a check's yield its answer is read, and at a defense's the check
        // is owed.
        if reading.worked.is_some_and(|d| d >= min || unseen_turn) && !kept_long {
            self.land.audited = [None; 3];
            self.land_listed = None;
            self.land.answered = false;
            self.land.rest = false;
        }
        // THE CHECK'S ANSWER STANDS ONLY AS THE REPLY IT WAS: a turn since that is no check's yield
        // (a person's exchange, someone else's turn, a wall's) said something else, so a question
        // in it is answered and a defense in it is contradicted by the list still standing.
        if reading.worked.is_some() && !check_yield {
            self.land.answered = false;
        }
        if awaited.is_none() && (reading.worked.is_some() || typed || prompted) {
            self.land.rest = false;
        }
        if awaited.is_some() || reading.worked.is_some() {
            self.land.check_owed = false;
            self.settle_left();
        }
        // THE LAND ACT'S ANSWER A LOOP BEFORE THIS ONE JUDGED, READ AGAIN ([`Self::seed_land_point`]):
        // the check that loop owed its defense is owed here too, as that loop typed it.
        let rejudged = !land_judged
            && land_point
            && same_point
            && ok_to_leave
            && check_answer.is_none()
            && reading.autopilot == Autopilot::Land;
        // A `LANDED` A LEFT ROW HELD checks no defense: said again once the row is ended, the
        // defense is owed the check again.
        let audits = |answer: &land_check::CheckAnswer| {
            !(left_held && *answer == land_check::CheckAnswer::Landed)
        };
        if land_judged {
            self.land.check_pending = false;
            match &check_answer {
                Some(answer) => {
                    if let Some(mark) = self.land.sent.take()
                        && audits(answer)
                    {
                        self.land.audit(mark);
                    }
                    self.land_answered(answer, reading);
                }
                None => {
                    self.land_relisted(reading);
                    self.land_defended(reading);
                }
            }
            self.set_legs(reading, check_answer.as_ref());
        } else if rejudged {
            // THE SEEDED POINT WAS THE LAND CHECK'S ANSWER (its act the check,
            // [`Self::seed_land_checked`]; the post-landing review, 2026-11-01): read again as the
            // loop before this one read it, so a rest on a need stays a rest, typing nothing, a
            // list stays answered and quoted, and the defense it audits is audited. A stopped
            // turn, or one that said nothing, gave no answer, there as here. Its left hold, which
            // the books that loop handed on leave out ([`Self::land_books`]), is counted where it
            // still holds, and a `LANDED` no left row holds now, over a repository read clean,
            // lands, as the check's answer lands where it is first read so ([`Self::left_holds`]).
            let left_held = self.left_holds(reading);
            let answer = (std::mem::take(&mut self.land.check_pending)
                && !reading.interrupted
                && !reading.unsaid())
            .then(|| (*answer_read).clone());
            match &answer {
                Some(answer) => {
                    if let Some(mark) = self.land.sent.take()
                        && !(left_held && *answer == land_check::CheckAnswer::Landed)
                    {
                        self.land.audit(mark);
                    }
                    self.land_answered(answer, reading);
                }
                None => self.land_defended(reading),
            }
            if answer == Some(land_check::CheckAnswer::Landed)
                && reading.repo.is_clean()
                && !left_held
                && !self.exchange
                && !self.landed()
            {
                self.done = 2;
                self.done_at = Some(now);
                self.landed = true;
                self.by_check = true;
            }
            self.set_legs(reading, answer.as_ref());
        } else if check_yield && !(self.cut_land || reading.pending_input) {
            // The check went unanswered (never taken, or its turn stopped): its defense is not
            // checked, so said again it is owed the check.
            self.land.check_pending = false;
            self.land.sent = None;
        } else if awaited.is_none()
            && reading.worked.is_none()
            && ok_to_leave
            && reading.autopilot == Autopilot::Land
            && self.land.legs.is_some_and(|legs| legs.holds(HoldLeg::Left))
            && reading.leftover != crate::harness::leftover::LeftRead::NotAsked
            && reading
                .said_tail
                .as_deref()
                .and_then(needs::defense_line)
                .is_some()
            && self.left_unkept(reading) != self.left_said
        {
            // A DEFENSE THE LEFT LEG HELD, READ AGAIN OVER OTHER ROWS before its act (a row that
            // ended by itself in the back-off), or over a view that can no longer say: judged
            // again as the first time ([`Self::uncount_left`]), so the act names the rows that run
            // now and, with none proved, the check is owed.
            self.uncount_left(reading);
            self.land_defended(reading);
            self.set_legs(reading, None);
        }
        // THE WORKER'S OWN `Could not finish: <why>` (a managed worker's, under Land alone: Cruise
        // never rests, and a person's own session has no one waiting on such a word). A turn the
        // person stopped or a wall cut gave no such answer.
        self.could_not_finish_read = (turn_ran || first_point)
            && reading.autopilot == Autopilot::Land
            && ok_to_leave
            && !reading.interrupted
            && !reading.said_cut
            && !self.landed
            && !self.exchange
            && reading
                .said_tail
                .as_deref()
                .and_then(needs::could_not_finish)
                .is_some();
        if self.managed && std::mem::take(&mut self.could_not_finish_read) {
            self.conclude(GaveUp::CouldNotFinish);
        }
        if let Some((ending, words)) = gave_up_seed
            && reading.autopilot == Autopilot::Land
            && ok_to_leave
            && words.is_none_or(|w| tail_mark == Some(w))
        {
            self.seed_gave_up(ending);
        }
        if self.landed() && !landed_before {
            self.landed_mark = seed_mark.or(tail_mark);
            // An episode that lost what its defenses kept cannot say a row it reads unkept was left.
            self.landed_left = match crate::harness::leftover::LeftWord::of(
                &reading.leftover,
                self.left_kept(reading),
            ) {
                Some(crate::harness::leftover::LeftWord::Left(_)) if self.land.kept_over => {
                    Some(crate::harness::leftover::LeftWord::Unchecked)
                }
                word => word,
            };
            self.hold_landed();
        }
        self.point_mark = tail_mark;
        self.point_mark_len = said_letters(reading.said_tail.as_deref()).len();
        self.point_words = Some(words_mark(reading));
        if let Some(opener) = turn_opener(reading) {
            self.point_opener = Some(opener);
        }
        let judged_turn = awaited.is_some() || reading.worked.is_some();
        if reading.answered_point() || judged_turn {
            let undrawn = self.submitted_since(reading) == Some(true)
                && !self.drawn_since(reading)
                && !judged_turn;
            if !undrawn {
                self.point_submits = reading.submits;
                self.point_screen = reading.screen_mark;
                self.point_tasked = !reading.taskless;
                self.point_inputs = reading.inputs_before;
            }
        }
        // A RECORD'S LANDING HOLDS at its own answer (the check's `LANDED`) or an older build's
        // clean defense: [`seed_clean`], as a journal's seed reads it.
        self.point_clean = ok_to_leave
            && reading.worked.is_none()
            && awaited.is_none()
            && !(seed_was_armed && !self.landed())
            && (seed_clean() || (seed_was_armed && self.landed() && words_hold()));
        if let Some(a) = reading.answered {
            self.last_prompt = Some(a.prompt);
        }
        if self.done == 2 {
            self.carrying = false;
        }
        self.carry_at_attach = false;
        self.left_judged = (awaited.is_some() || reading.worked.is_some()) && self.left_to_person();
        let retried = wall_retry;
        let compacted_nothing = awaited == Some(RULE_COMPACT)
            && awaited_command
            && reading.worked.is_none()
            && reading.wall.is_none()
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Context);
        let retry_taken = retried && !met && !compacted_nothing;
        let unmade_seeded = std::mem::take(&mut self.account_unmade_seed);
        if reading.login_screen.is_none()
            && reading.composer != Composer::Absent
            && let Some(t) = self
                .wall
                .as_mut()
                .filter(|t| t.class == WallClass::Auth && t.login.backed_out.is_some())
        {
            t.login.closed = true;
        }
        match reading.wall {
            Some(kind) => {
                let at_point = true;
                self.track_wall(
                    reading,
                    kind,
                    retried,
                    worked_to_wall,
                    unmade_seeded,
                    at_point,
                    Arrival {
                        hand: theirs,
                        seen: arrived,
                    },
                    now,
                );
                let seat_now = self.seat_now(reading.program, &reading.seat);
                if !reading.wall_remembered
                    && let Some(t) = self.wall.as_mut().filter(|t| {
                        named_class(t.class)
                            && t.program == Some(reading.program)
                            && screen_alone(reading.program)
                    })
                    && (t.seat == Seat::default() || reading.worked.is_some() || t.seat_moved)
                {
                    t.seat.update(&seat_now);
                    t.seat_moved = false;
                }
            }
            None => {
                let login_back = reading.login_back
                    && self
                        .wall
                        .as_ref()
                        .is_some_and(|t| t.class == WallClass::Auth);
                let owed = self.wall.as_ref().is_some_and(|t| t.owed);
                let interrupted = reading.login_interrupted
                    && self.wall.as_ref().is_some_and(|t| {
                        matches!(
                            t.class,
                            WallClass::Auth | WallClass::Usage | WallClass::Model
                        )
                    });
                let dialog = reading.login_screen.as_ref().is_some_and(|screen| {
                    !matches!(screen, aterm_phase::LoginScreen::Success { .. })
                }) && self
                    .wall
                    .as_ref()
                    .is_some_and(|t| t.class == WallClass::Auth);
                let owns_answer = self
                    .wall
                    .as_ref()
                    .is_none_or(|t| !named_class(t.class) || limit_conversation_ours(t, reading));
                let gone = (reading.worked.is_some() && !interrupted && !dialog)
                    || ((retry_taken || owed) && owns_answer)
                    || login_back;
                if gone || unmade_seeded.is_some() {
                    self.account_unmade = None;
                }
                let dropped = auto_continues(reading.program)
                    && reading.composer != Composer::Absent
                    && reading.auto_continue != Some(aterm_phase::AutoContinue::Armed);
                if gone {
                    self.limit_ended |= self.wall.as_ref().is_some_and(|t| named_class(t.class));
                    self.login_returned |= self.wall.as_ref().is_some_and(|t| {
                        t.class == WallClass::Auth || (named_class(t.class) && t.login.back_typed)
                    });
                    self.wall = None;
                    self.login_resume = false;
                    self.fallback_unmade = false;
                } else if interrupted && let Some(t) = self.wall.as_mut() {
                    t.login.cancelled = true;
                } else if reading.login_back
                    && let Some(t) = self
                        .wall
                        .as_mut()
                        .filter(|t| named_class(t.class) && !t.login.back_typed)
                {
                    t.named = None;
                    t.reset_dropped = true;
                    t.attempts = 0;
                    t.login.cancelled = false;
                    t.login.back_typed = true;
                    if self.done >= 2 && !self.landed && self.gave_up.is_none() {
                        self.done = 0;
                    }
                } else if let Some(t) = self.wall.as_mut().filter(|t| {
                    t.class == WallClass::Model
                        && limit_conversation_ours(t, reading)
                        && t.named.is_some_and(|n| n.moved_off_under(reading, &t.said))
                }) {
                    t.named = None;
                } else if let Some((t, seat_now)) = {
                    let seat_now = self.seat_now(reading.program, &reading.seat);
                    self.wall
                        .as_mut()
                        .filter(|t| {
                            named_class(t.class)
                                && !t.seat_moved
                                && t.program == Some(reading.program)
                                && screen_alone(reading.program)
                                && t.seat.moved(&seat_now, limit_per_model(reading.program))
                        })
                        .map(|t| (t, seat_now))
                } {
                    t.named = None;
                    t.seat_moved = true;
                    t.seat.update(&seat_now);
                } else if (dropped
                    || reading.auto_continue == Some(aterm_phase::AutoContinue::Unkept))
                    && let Some(n) = self.wall.as_mut().and_then(|t| t.named.as_mut())
                {
                    n.by_itself = false;
                }
                // A reset read with the notice gone is the usage tap's (`Session::tap_left_wall`):
                // it moves a named reset, and names one a cut usage limit's notice never named,
                // unless a login dropped the limit's reset since ([`WallTrack::reset_dropped`]).
                if let (Some(at), Some(t)) = (reading.reset_at, self.wall.as_mut()) {
                    let names = tapped_limit(t);
                    match (t.named.as_mut(), t.kind) {
                        (Some(n), _) => {
                            n.at = at;
                            n.jitter = reading.jitter;
                        }
                        (None, Some(kind @ (WallKind::UsageSession | WallKind::UsageWeekly)))
                            if names =>
                        {
                            t.named = Some(NamedReset {
                                kind,
                                at,
                                jitter: reading.jitter,
                                by_itself: false,
                                said: None,
                                first: now,
                                written: self.notice_unix,
                            });
                        }
                        (None, _) => {}
                    }
                }
            }
        }
        // A turn served since the move, a prompt answered since (by the new login's limit,
        // no busy spell seen), or a limit's track that ended or gave way to another wall,
        // owes the move nothing: a limit read after it is the new login's.
        if reading.worked.is_some()
            || answered_anew
            || !self.wall.as_ref().is_some_and(|t| named_class(t.class))
        {
            self.login_resume = false;
        }
        if reading.login_moved
            && let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class))
        {
            t.named = None;
            t.reset_dropped = true;
            self.login_resume = true;
        }
        let startup = std::mem::take(&mut self.account_startup);
        if std::mem::take(&mut self.account_resume_seed)
            && !left_login
            && !matches!(
                reading.wall,
                Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
            )
            && !reading.since_limit.as_ref().is_some_and(|s| !s.served)
        {
            self.account_resume = false;
        }
        if self.account.is_some() && reading.worked.is_some() && !reading.pending_input && !startup
        {
            self.account_resume = false;
            self.account_asked = true;
        }
        if self.account.is_some()
            && self.account_told
            && reading.wall.is_none()
            && reading.worked.is_some()
            && !reading.pending_input
            && !startup
        {
            self.account_over();
        }
        if let Some(owed) = self.reset_resume
            && reading
                .worked
                .and_then(|worked| now.checked_sub(worked))
                .is_some_and(|began| began >= owed.told)
        {
            self.reset_resume = None;
        }
    }

    fn limit_went_on(&self, reading: &TurnEndReading, now: Option<Instant>) -> bool {
        if self
            .wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && !limit_conversation_ours(t, reading))
        {
            return false;
        }
        let since = reading
            .since_limit
            .as_ref()
            .filter(|_| !self.limit_elsewhere(reading));
        let named = self.wall.as_ref().map(|t| t.named);
        let served =
            reading.served_in_transcript || named.flatten().is_some_and(|n| n.served_under(since));
        let continued = reading.auto_continue == Some(aterm_phase::AutoContinue::Fired)
            && named.is_some_and(|n| n.is_none_or(|n| n.continued_under(since, now)));
        reading.wall.is_none()
            && reading.worked.is_none()
            && (continued || served)
            && self.wall.as_ref().is_some_and(|t| named_class(t.class))
    }

    fn answered_unseen(&self, reading: &TurnEndReading, now: Instant) -> Option<Duration> {
        // A common continuation/check row in another or unread conversation
        // is not proof that this wall's awaited act ran. Actual observed work
        // is supplied independently by the caller and keeps its normal rule.
        if self
            .wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && !limit_conversation_ours(t, reading))
        {
            return None;
        }
        let a = self.untaken().filter(|a| !a.met && !a.off_stood)?;
        if reading.worked.is_some() || !reading.answered_point() {
            return None;
        }
        let since = now.saturating_duration_since(a.at);
        let Some(said) = a.said else {
            let by_screen = reading
                .answered
                .filter(|s| a.over.is_some_and(|o| o != s.prompt))
                .map(|s| s.work.unwrap_or(since));
            let by_opener = reading
                .opened_by
                .filter(|o| reading.said_tail.is_some() && a.opened.is_some_and(|at| at != *o))
                .map(|_| since);
            return a
                .submitted
                .then(|| by_screen.or(by_opener))
                .flatten()
                .map(|work| work.min(since));
        };
        let by_record = reading
            .record_turn
            .filter(|t| {
                t.prompt == said
                    && now
                        .checked_sub(t.age)
                        .is_some_and(|recorded| recorded + RECORD_SLACK >= a.typed)
            })
            .and_then(|t| t.ended);
        let submitted = a.submitted_since(reading);
        let by_screen = reading
            .answered
            .filter(|s| s.prompt == said && (submitted || a.over.is_some_and(|o| o != said)))
            .map(|s| s.work.unwrap_or(since));
        let by_opener = a
            .lead
            .filter(|lead| {
                reading.opened_by == Some(*lead)
                    && reading.said_tail.is_some()
                    && (submitted || a.opened.is_some_and(|o| o != *lead))
            })
            .map(|_| since);
        by_record
            .or(by_screen)
            .or(by_opener)
            .map(|work| work.min(since))
    }

    /// A TURN THE HOST TYPED, ANSWERED BEFORE ANY READ SAW IT RUN ([`Self::host_typed`]): its words
    /// are the host's, never this policy's to compare as [`Self::answered_unseen`] compares its own
    /// act's, so its answer is the turn the screen closes on a prompt the harness typed
    /// ([`ScreenAnswer::harness`]) other than the one the screen showed answered when the host
    /// typed ([`Awaited::over`]). A restart prompt Claude Code answers within a second is judged at
    /// that read, its work the screen's; [`TurnEndTiming::take_within`] stays the hang detector for
    /// a turn no screen shows answered. A host turn in the same words as that prompt (a second
    /// restart for the same cause on the same version) is one no screen tells from it: a resume
    /// redraws the screen and the host's typing moves the submit count before any answer, so
    /// neither marks the new turn as [`Awaited::submitted_since`] marks this policy's own, and it
    /// waits [`TurnEndTiming::take_within`]. `None` where a read saw work, no host turn awaits, the
    /// screen is no answered point or shows no such answer, or a limit's conversation is not the
    /// one read.
    fn host_answered_unseen(&self, reading: &TurnEndReading, now: Instant) -> Option<Duration> {
        if self
            .wall
            .as_ref()
            .is_some_and(|t| named_class(t.class) && !limit_conversation_ours(t, reading))
        {
            return None;
        }
        let a = self
            .awaiting
            .as_ref()
            .filter(|a| a.host.is_some() && !a.command && !a.taken && !a.met && !a.off_stood)?;
        if reading.worked.is_some() || !reading.answered_point() {
            return None;
        }
        let since = now.saturating_duration_since(a.at);
        reading
            .answered
            .filter(|s| s.harness && Some(s.prompt) != a.over)
            .map(|s| s.work.unwrap_or(since).min(since))
    }

    const fn submitted_since(&self, reading: &TurnEndReading) -> Option<bool> {
        match (reading.submits, self.point_submits) {
            (Some(now), Some(then)) if now >= then => Some(now > then),
            _ => None,
        }
    }

    fn drawn_since(&self, reading: &TurnEndReading) -> bool {
        reading.screen_mark.is_some() && reading.screen_mark != self.point_screen
    }

    const fn keyed_by_harness(&mut self) {
        self.point_submits = None;
        self.point_screen = None;
        self.point_tasked = false;
        self.point_inputs = None;
    }

    /// THE COUNT READ AGAIN ONCE THE HARNESS'S OWN KEYS WERE SEEN TO LAND.
    pub const fn counted_after_harness(&mut self, at: Option<(u64, u64)>) {
        if let Some((submits, screen)) = at {
            self.point_submits = Some(submits);
            self.point_screen = Some(screen);
        }
    }

    #[cfg(test)]
    pub(crate) fn counted_from(&self) -> (Option<u64>, Option<u64>) {
        (self.point_submits, self.point_screen)
    }

    /// THE HOST'S STEP AT AN IDLE POINT MOVED THE SESSION: its keys are the harness's own.
    pub const fn host_moved(&mut self) {
        self.keyed_by_harness();
    }

    /// A SCREEN READ AS NO TASK YET THAT IS THE CONVERSATION OF THE POINT JUDGED LAST, REDRAWN.
    #[must_use]
    pub const fn redrawn_fresh(&self, submits: Option<u64>, inputs: Option<u64>) -> bool {
        self.point_tasked
            && matches!((submits, self.point_submits), (Some(now), Some(then)) if now == then)
            && matches!((inputs, self.point_inputs), (Some(now), Some(then)) if now == then)
    }

    fn unseen_turn(&self, reading: &TurnEndReading) -> bool {
        let premises = self.awaiting.is_none()
            && self.wall.is_none()
            && reading.since_limit.is_none()
            && reading.auto_continue.is_none()
            && reading.worked.is_none()
            && self.point_at.is_some()
            && !reading.taskless
            && reading.answered_point();
        if !premises {
            return false;
        }
        if self.submitted_since(reading) == Some(true) {
            return self.drawn_since(reading) && reply_stands(reading);
        }
        turn_opener(reading).is_some_and(|o| self.point_opener.is_some_and(|last| last != o))
            && !words_end_with(
                reading.said_tail.as_deref(),
                self.point_mark,
                self.point_mark_len,
            )
    }

    /// THE NEEDS A REPORT NAMES AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK: a turn that ends
    /// there reaches no point ([`Self::observe`] folds needs at points), so its `NEEDS YOU:` lines
    /// are folded here from the turn's words as a point folds them: its earlier messages `before`
    /// (the act's own rows cut from their opening where it `opens_cut`), then its report `tail`.
    pub fn fold_break_needs(
        &mut self,
        before: Option<needs::Before<'_>>,
        tail: Option<&str>,
        opens_cut: bool,
    ) {
        let act = self.act_words.clone().filter(|_| opens_cut);
        self.fold_turn_needs(before, tail, act.as_deref(), false);
    }

    /// Fold the needs a turn's words name: its earlier messages `before`, the typed `act`'s own
    /// rows cut from their opening ([`without_act_rows`]), then its last `tail`. Under `add_only`
    /// (a turn a wall cut) they join the set standing ([`Needs::join`]) and take none down.
    fn fold_turn_needs(
        &mut self,
        before: Option<needs::Before<'_>>,
        tail: Option<&str>,
        act: Option<&str>,
        add_only: bool,
    ) {
        let opening = |t: &str| without_act_rows(t, act).to_string();
        // THE AGENT'S WORD ON NEEDS ([`needs::standing_before`]): its last message's own none
        // retracts every need the turn named and every need an earlier point raised; an earlier
        // message's own none retracts only the needs named before it in that message, here as
        // in the turn's words ([`needs::retracted_in`]); a none anywhere else, quoted, fenced or
        // in a message of its own, takes no need down. In a turn a wall cut the agent's word
        // retracts only what the turn itself named: the needs it took back are not added, and
        // those an earlier point raised stand.
        let last = tail.unwrap_or_default();
        let retracted = tail.is_some_and(needs::retracts);
        if let Some(before) = before.filter(|_| !retracted && !add_only) {
            self.needs.retract(&needs::retracted_in(before));
        }
        let words = match needs::standing_before(before, last) {
            Some(before) => Some(format!("{}\n\n{last}", opening(&before))),
            None => tail.map(opening),
        };
        if add_only {
            if let Some(words) = words {
                self.needs.join(needs::needs_you(&words));
            }
            return;
        }
        self.fold_needs(words.as_deref(), retracted);
    }

    fn fold_needs(&mut self, tail: Option<&str>, retracted: bool) {
        let Some(t) = tail else {
            return;
        };
        let named = needs::needs_you(t);
        if named.is_empty() && needs::defense_line(t).is_none() && !retracted {
            // A reply that goes on without the lines a go-ahead decided but did not type yet
            // answers them as its submission would (DESIGN-harness-autopilot §15.6).
            self.needs.answer(&[]);
            return;
        }
        let key = needs::needs_key(&named);
        if key == 0 {
            self.needs = Needs::default();
            return;
        }
        if key != self.needs.key {
            // A line the go-ahead answered stays answered only while a reply names it.
            self.needs.answered.retain(|line| named.contains(line));
            self.needs.answering.retain(|line| named.contains(line));
            self.needs.standing.clone_from(&named);
            self.needs.key = key;
        }
        self.needs.named = named;
        self.needs.reshow();
    }

    fn point_judged(&mut self, reading: &TurnEndReading, now: Instant) -> bool {
        if reading.pending_input {
            // THE LAND CHECK'S ANSWER BEHIND THE PERSON'S UNSENT INPUT: the turn that ran is
            // the check taken, so the point read once their input is gone (no turn run since)
            // judges its answer, never a check the agent did not take.
            if reading.worked.is_some()
                && let Some(a) = self
                    .awaiting
                    .as_mut()
                    .filter(|a| a.rule == land_check::RULE_LAND_CHECK)
            {
                a.taken = true;
            }
            let Some(a) = self.awaiting.as_mut().filter(|_| reading.worked.is_none()) else {
                return false;
            };
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return false;
            }
            self.stale_input = true;
        } else if let Some(a) = &self.awaiting
            && a.ran.is_some()
            && reading.words_may_follow()
        {
            // A LAND ACT'S ANSWER THAT SAYS NOTHING YET ([`Self::unsaid_reading`]): the act stays
            // awaited until its words are drawn or its take window, counted from that answer,
            // runs out.
            if now < a.due(&self.timing) {
                return false;
            }
        } else if let Some(a) = &mut self.awaiting
            && !a.command
            && reading.worked.is_none()
        {
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return false;
            }
        }
        true
    }

    /// A LAND ACT'S ANSWER WITH NO WORDS ANSWERS NOTHING YET (L19, the 2026-11-05 confirm run's
    /// `land-o` on agy 1.5.1 on a loaded Mac): agy can draw its composer idle under the act's
    /// message, with nothing between, before the reply it then draws (`LANDED` 0.55 s later
    /// there, the land check's answer). The race is the frame's, not the check's: Land's
    /// instruction answered so was judged a short turn and backed off, its defense drawn after
    /// it nudged instead of checked (the 2026-11-06 review's E4). Every Land act is answered by
    /// its words (a defense, `LANDED`, a list, a need), so each one's waits for them; Cruise
    /// judges an act's answer by its work where it ends, as before. A Land act's turn that ran
    /// and ended so ([`TurnEndReading::words_may_follow`]) keeps the act awaited, its work kept in
    /// [`Awaited::ran`] (the work of each later run that still says nothing added to it) and its
    /// take window counted from that answer, or from the end of the latest run that still says
    /// nothing ([`Self::point_judged`]); every reading after it is judged with that work and any
    /// run since (`Some`, the reading to judge), so the one that draws the words is that turn's
    /// end. Past the window an answer still unsaid is judged as it stands, by its shape, and a
    /// land check answered so audits nothing: its defense stays owed its check
    /// ([`Self::observe`]).
    fn unsaid_reading(&mut self, reading: &TurnEndReading, now: Instant) -> Option<TurnEndReading> {
        let a = self.awaiting.as_mut().filter(|a| !a.command)?;
        let worked = match (a.ran, reading.worked) {
            (None, Some(w))
                if (self.landing || self.land.check_pending) && reading.words_may_follow() =>
            {
                a.unseen_since = Some(now);
                a.taken = true;
                a.ran = Some(w);
                return None;
            }
            (None, _) => return None,
            (Some(ran), w) => w.map_or(ran, |w| ran.saturating_add(w)),
        };
        if reading.words_may_follow() {
            if reading.worked.is_some() {
                a.unseen_since = Some(now);
            }
            a.ran = Some(worked);
        }
        Some(TurnEndReading {
            worked: Some(worked),
            ..reading.clone()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn track_wall(
        &mut self,
        reading: &TurnEndReading,
        kind: WallKind,
        retried: bool,
        worked: bool,
        unmade_seeded: Option<Unmade>,
        at_point: bool,
        arrival: Arrival,
        now: Instant,
    ) {
        let class = class_of(kind);
        let entitlement = entitlement_ladder(reading.reset_at.is_some(), kind)
            && !goes_on_by_itself(
                self,
                self.wall.as_ref().filter(|t| t.class == class),
                reading,
            );
        if !reading.wall_remembered
            && self.wall.as_ref().is_some_and(|t| {
                named_class(t.class)
                    && matches!((&t.conversation, &reading.conversation),
                    (Conversation::Named(ours), Conversation::Named(theirs)) if ours != theirs)
            })
        {
            self.wall = None;
            self.limit_ended = true;
        }
        let down = matches!(reading.reach, Reach::Down { .. });
        match &mut self.wall {
            Some(t) if t.class == class => {
                let retried = retried || std::mem::take(&mut t.owed);
                t.pointed |= at_point;
                // An API wall's ladder ends when the agent works again, in a person's own turn as
                // after a try (v6 §14.17.5.2); every other wall's track keeps its rung through one.
                let anew = worked
                    && !work_is_its_cause(kind)
                    && (retried
                        || t.seeded
                        || (class == WallClass::Retry && reading.person_in_turn()));
                if retried || (t.seeded && reading.worked.is_some()) {
                    t.since = now;
                }
                if retried && named_class(class) {
                    t.login.back_typed = false;
                    t.login.cancelled = false;
                }
                if anew {
                    t.since = now;
                    t.attempts = 0;
                    t.ups = 0;
                    t.exhausted = false;
                    t.fill = None;
                    t.down = false;
                    t.witnessed = false;
                    self.limit_ended |= named_class(class);
                }
                t.down |= down;
                if worked {
                    self.account_unmade = None;
                }
                let prompt = reading.answered.map(|a| a.prompt);
                if class == WallClass::Auth && prompt.is_some() && prompt != t.login.prompt {
                    t.login.prompt = prompt;
                    t.login.prompt_at = Some(now);
                    t.login.hand |= arrival.hand;
                }
            }
            _ => {
                self.limit_ended |= self.wall.as_ref().is_some_and(|t| named_class(t.class));
                self.wall = Some(WallTrack {
                    relaunch: RelaunchBooks::default(),
                    seat: Seat::default(),
                    seat_moved: false,
                    reset_dropped: false,
                    class,
                    since: now,
                    attempts: 0,
                    ups: 0,
                    exhausted: false,
                    fill: None,
                    named: None,
                    pointed: at_point,
                    said: String::new(),
                    kind: Some(kind),
                    seeded: false,
                    entitlement: false,
                    owed: false,
                    down,
                    witnessed: false,
                    conversation: Conversation::Hostless,
                    program: None,
                    login: if class == WallClass::Auth {
                        LoginBooks {
                            hand: arrival.hand,
                            prompt_at: arrival.seen.then_some(now),
                            prompt: reading.answered.map(|a| a.prompt),
                            ..LoginBooks::default()
                        }
                    } else {
                        LoginBooks::default()
                    },
                });
                self.fallback_unmade = false;
                self.account_unmade = unmade_seeded;
            }
        }
        if let Some(t) = &mut self.wall {
            if named_class(class) && !reading.wall_remembered {
                t.conversation = reading.conversation.fresh_after(&t.conversation);
                t.program = Some(reading.program);
            }
            if class == WallClass::Context && compaction_exhausted_row(&reading.wall_message) {
                t.exhausted = true;
            }
            if class == WallClass::Context
                && let Some(fill) = ContextFill::of(&reading.wall_message)
            {
                t.fill = Some(fill);
            }
            t.said.clone_from(&reading.wall_message);
            t.kind = Some(kind);
            t.seeded = false;
            t.entitlement = entitlement;
            t.reset_dropped = false;
            if let Some(at) = reading.reset_at {
                let kept = t
                    .named
                    .filter(|n| n.said.is_some() && n.said == reading.reset_clock);
                let first = kept.map_or(now, |n| n.first);
                let written = kept.and_then(|n| n.written).or(self.notice_unix);
                let gone = self.countdown_gone(first) || self.written_before_start(written);
                if let Some(t) = &mut self.wall {
                    t.named = Some(NamedReset {
                        kind,
                        at,
                        jitter: reading.jitter,
                        by_itself: reading.resumes_by_itself && !gone,
                        said: reading.reset_clock,
                        first,
                        written,
                    });
                }
            }
        }
    }

    /// Records an act the loop typed (not a skipped or refused one).
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
        self.limit_try = None;
        if matches!(action, TurnEndAction::Restart { .. }) {
            return;
        }
        if matches!(action, TurnEndAction::Confirm { .. }) {
            self.login_confirmed = true;
            return;
        }
        if let TurnEndAction::Cancel { rule_id } = action {
            if *rule_id == super::login::RULE_LOGIN_OVER_LIMIT {
                self.standing_login = Some(StandingLogin {
                    seen: self.login_dialog_seen().unwrap_or(now),
                    backed_out: Some(now),
                    backs: self.login_over_limit_backs().saturating_add(1),
                });
            } else {
                self.login_backed_out_at(now);
            }
            return;
        }
        let Some(rule) = action.rule_id() else {
            return;
        };
        let kept = self.kept_words(action, reading);
        self.point_last = false;
        self.settle_left();
        self.fallback_unasked = false;
        self.account_asked |= self.account.is_some();
        self.account_startup = false;
        self.account_resume = false;
        self.login_resume = false;
        self.reset_resume = None;
        let in_limits_place = self.in_limits_place(rule);
        if (rule == RULE_LIMIT_RESUME || in_limits_place)
            && let Some(AccountAdvice::Resume { cleared_at }) = reading.advised()
        {
            self.account_resumed = Some(
                self.account_resumed
                    .map_or(*cleared_at, |typed| typed.max(*cleared_at)),
            );
        }
        self.acts.push_back(now);
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
        self.try_over = false;
        self.unread_try = None;
        self.seed_answered = false;
        self.awaiting = Some(Awaited {
            rule,
            command: matches!(action, TurnEndAction::TypeCommand { .. }),
            at: now,
            typed: now,
            unseen_since: None,
            taken: false,
            met: false,
            left: left_key(reading.since_limit.as_ref(), reading.auto_continue),
            host: None,
            off_stood: false,
            wall_retry: is_wall_retry(rule) || in_limits_place,
            own: is_wall_retry(rule) || in_limits_place || self.after.is_some(),
            said: match action {
                TurnEndAction::Type { text, .. } => {
                    Some(crate::supervise::approvals::words_hash(text))
                }
                _ => None,
            },
            over: reading.answered.map(|a| a.prompt),
            lead: match action {
                TurnEndAction::Type { text, .. } => {
                    Some(crate::supervise::approvals::words_lead(text))
                }
                _ => None,
            },
            opened: reading.opened_by,
            submitted: matches!(action, TurnEndAction::Submit { .. }),
            submits: reading.submits,
            screen: reading.screen_mark,
            ran: None,
        });
        let typed_rule = match action {
            TurnEndAction::Type { rule_id, .. } => Some(*rule_id),
            _ => None,
        };
        // An act reached the agent after the person's switch: the switch's one ask ([`switch_act`])
        // stands until its answer is read; any other act spends the switch.
        self.switch_seen = false;
        let switch_asked =
            matches!(action, TurnEndAction::Type { text, .. } if is_switch_ask(text));
        self.switch_ask = match self.switch_ask {
            SwitchAsk::Owed if switch_asked => {
                self.switch_spent = self.replies.last().map(|k| k.mark);
                SwitchAsk::Typed
            }
            SwitchAsk::Owed => SwitchAsk::None,
            kept => kept,
        };
        if reading.autopilot != Autopilot::Land {
            self.leave_land_episode();
        }
        if reading.autopilot != Autopilot::Cruise {
            self.leave_cruise_episode();
        }
        let again = typed_rule.is_some_and(is_wall_retry);
        let was_cut_check = std::mem::take(&mut self.cut_check);
        let was_cut_land = std::mem::take(&mut self.cut_land);
        let owed = self.compact_owes.take();
        let compact_cmd = matches!(
            action,
            TurnEndAction::TypeCommand {
                rule_id: RULE_COMPACT,
                then: Then::Continue,
                ..
            }
        );
        if compact_cmd {
            self.compact_owes = owed.or_else(|| {
                (was_cut_check || was_cut_land).then_some(CompactOwes {
                    check: was_cut_check,
                    land: was_cut_land,
                })
            });
        }
        let carried = owed.filter(|_| typed_rule == Some(RULE_COMPACT));
        let cut_check = (was_cut_check && again) || carried.is_some_and(|o| o.check);
        let cut_land = (was_cut_land && again) || carried.is_some_and(|o| o.land);
        if !cut_check && self.compact_owes.is_none_or(|o| !o.check) {
            self.seeded_check = None;
        }
        if !cut_land && self.compact_owes.is_none_or(|o| !o.land) {
            self.seeded_land = None;
        }
        let typed_text = match action {
            TurnEndAction::Type { text, .. } => Some(text.clone()),
            _ => None,
        };
        // The lines the go-ahead answers, whose ack it does not owe ([`Self::needs_ack_owed_past`]):
        // those its decision judged ([`Self::go_ahead_judged`]), and no others.
        let past = self.needs.answering.clone();
        if typed_rule == Some(RULE_GO_AHEAD) {
            // THE GO-AHEAD TYPED IS ONE ACT OF ITS COUNT ([`GoAheads`]) on the park its decision
            // read, and answers that park's lines alone: a point types it only there
            // ([`Self::go_ahead_owed`]), and those lines are the ones being answered. The loop
            // records the act on the point read again (`Session::turn_end_reread`), which runs no
            // whole-turn read, so its park is no judge of the turn: a park only an earlier message
            // read whole above the screen held parks nothing on `reading`, and the act counts all
            // the same, or its bound would never trip; a line that earlier message makes a
            // person's parks on `reading`, and stays on the menu bar all the same (§15.6).
            self.go_ahead_counted(&[], now);
        } else if !past.is_empty() && !matches!(action, TurnEndAction::Submit { .. }) {
            // Another act its decision typed over the park (Land's instruction first,
            // [`Self::go_ahead_first`]) answers its lines as the go-ahead's would, but is no act
            // of the go-ahead's count. A draft's Enter booked as itself answers none: the loop
            // books a line a point's act wrote and left unsent as that act
            // (`Session::turn_end_execute`), so this one sent no words its decision judged, and
            // the loop puts their lines back as for an act not submitted.
            self.go_ahead_answered(&[]);
        }
        self.act_words.clone_from(&typed_text);
        let land_text = reading.autopilot == Autopilot::Land
            && typed_text.as_deref().is_some_and(is_land_words);
        let land_given = land_text
            && typed_text
                .as_deref()
                .is_some_and(|t| past_switch_why(t).starts_with(LAND));
        let check_typed = typed_rule == Some(RULE_LAND_CHECK)
            || typed_text
                .as_deref()
                .is_some_and(|t| t.starts_with(LAND_CHECK_HEAD));
        // A RESUME PAST A CONDITION UNDER LAND ([`cleared_text`]) goes on with what Land asked, so
        // where Land's instruction was given its answer is judged as a land act's, as the land
        // words the resume typed before slice 12a were.
        let land_resumed = reading.autopilot == Autopilot::Land
            && typed_text.as_deref().is_some_and(is_cleared_line);
        let check_or_land =
            typed_rule == Some(RULE_DONE_CHECK) || typed_rule.is_some_and(is_land_rule);
        self.cut_text = if check_or_land || carried.is_some() {
            kept
        } else if cut_check || cut_land || self.compact_owes.is_some() {
            self.cut_text.take()
        } else {
            None
        };
        self.checking = typed_rule == Some(RULE_DONE_CHECK) || cut_check;
        if typed_rule == Some(RULE_DONE_CHECK) {
            // The next done claim asks the next angle ([`DONE_ANGLES`]); a wall's retry or a
            // `/compact`'s that types this one again asks nothing new.
            self.checks = next_checks(self.checks);
        }
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        if matches!(action, TurnEndAction::Type { text, .. } if text.contains(NEEDS_ACK)) {
            self.needs.acked = Some(needs::needs_key(
                &self.needs.less(&[self.needs.answered.as_slice(), &past]),
            ));
        }
        if reading.autopilot != Autopilot::Land {
            self.leave_land_episode();
        }
        if reading.autopilot != Autopilot::Cruise {
            self.checking = false;
            self.leave_cruise_episode();
        }
        // Land's instruction begins the hold whatever rule it went out under (a resume past a
        // condition, a compaction's owed continuation), as the books a later loop seeds from read
        // it, and that row stamps it.
        let gave = land_given && !self.land.typed;
        match typed_rule {
            Some(RULE_LAND) => self.land.typed = true,
            Some(RULE_LAND_NUDGE) => self.land.nudges = self.land.nudges.saturating_add(1),
            _ => {}
        }
        if check_typed {
            // A wall's or a `/compact`'s retype of the check owed nothing afresh, so it keeps the
            // defense the check audits (the post-landing review, 2026-11-01).
            if let Some(mark) = self.land.owed_mark.take() {
                self.land.sent = Some(mark);
            }
            self.land.check_pending = true;
            if let Some(tail) = self.land_owed_tail.take() {
                self.land_audited = Some(tail);
            }
            self.land_hint = None;
            self.land_listed = None;
        } else if typed_rule.is_some_and(is_land_rule) {
            self.land.check_pending = false;
        }
        if typed_rule.is_some() {
            self.land.check_owed = false;
            self.land.answered = false;
            self.land.rest = false;
        }
        if typed_rule == Some(RULE_LAND)
            || gave
            || (self.land.hold.relanded && typed_rule.is_some_and(is_land_rule))
        {
            self.land.hold = HoldStart {
                base: self.land.nudges,
                ..HoldStart::default()
            };
            self.land.unstamped = typed_rule;
        }
        self.land.typed |= land_given;
        self.landing = self.land.typed
            && (typed_rule.is_some_and(is_land_rule) || cut_land || land_text || land_resumed);
        self.after = match action {
            TurnEndAction::TypeCommand {
                then: Then::Continue,
                ..
            } => Some(rule),
            _ => None,
        };
        if is_wall_act(rule) {
            let window = self.timing.window;
            self.wall_acts
                .retain(|&(t, _)| now.saturating_duration_since(t) < window);
            self.wall_acts.push_back((now, rule));
            while self.wall_acts.len() > 4 * WALL_ACTS_PER_WINDOW {
                self.wall_acts.pop_front();
            }
        }
        if (is_wall_act(rule) || in_limits_place)
            && let Some(t) = &mut self.wall
        {
            // The resume decided on a witness latches it, whatever it typed (the post-landing
            // review, 2026-11-02): the outage's measured line, Land's instruction not given yet,
            // or the person's draft sent, but never the blind try's line. `reading` is the point
            // as decided (`Check::AsDecided`), its witness the decision's, and a blind line on a
            // witness that landed while it was typed latches nothing whatever the reading says
            // ([`types_blind_outage_line`], [`witness_clears`]).
            if rule == RULE_OUTAGE_RESUME
                && !types_blind_outage_line(action)
                && t.kind
                    .is_some_and(|k| witness_clears(Some(&*t), reading, k).is_some())
            {
                t.witnessed = true;
            }
            t.attempts += 1;
            if rule == RULE_API_BACK {
                t.ups += 1;
            }
            if rule == RULE_LOGIN {
                t.login.typed_at = Some(now);
            }
        }
        if rule == RULE_LOGIN_BACK
            && let Some(t) = self.wall.as_mut().filter(|t| t.class == WallClass::Auth)
        {
            t.login.back_typed = true;
        }
        if rule == RULE_LIMIT_RESUME || in_limits_place {
            self.limit_try = self
                .wall
                .as_ref()
                .filter(|t| named_class(t.class))
                .map(|t| LimitTry {
                    bucket: t.class == WallClass::Model,
                    in_place: in_limits_place,
                    attempts: t.attempts,
                    entitlement: t.entitlement,
                });
        }
    }

    /// THE AGENT'S PROCESS STARTED at `at` (this policy's clock).
    pub fn agent_started(&mut self, at: Instant) {
        self.agent_start = Some(at);
        self.drop_gone_countdown();
        if self
            .credits
            .as_ref()
            .is_some_and(|c| c.model.is_some() && c.since.is_some_and(|since| since < at))
        {
            self.credits = None;
        }
    }

    /// THE SAME START IN UNIX SECONDS, told by the loop beside [`Self::agent_started`].
    pub fn agent_started_unix(&mut self, unix: i64) {
        self.agent_start_unix = Some(unix);
        if self.fallback_started.is_none() && self.fallback_unix.is_some_and(|made| unix >= made) {
            self.fallback_started = Some(unix);
        }
        self.drop_gone_countdown();
        self.drop_gone_launch_latch();
    }

    /// The agent's start in unix seconds, as last told ([`Self::agent_started_unix`]).
    #[must_use]
    pub const fn agent_start_unix(&self) -> Option<i64> {
        self.agent_start_unix
    }

    /// THE HOST NAMES THE AGENT'S PROCESS (`pid`, started at `start` unix seconds) and the
    /// conversation it holds. A model switch rides every process the harness starts from the one
    /// its relaunch started, as each relaunch keeps the fallback's `--model`: one that resumes
    /// its conversation (a crash's, a stall's, an upgrade's, this loop's memory or lost-input
    /// restart), one the host still owes a continuation for (`owed`, asked only here), and one
    /// the harness started since the last process the host named ([`Self::harness_started`]: an
    /// upgrade's or a restart's that starts afresh on a new conversation). Any other process on
    /// another conversation is a program started apart from it, the person's own in the same tab
    /// on its launch's model: the switch is over, its bucket falls back as any does, and nothing
    /// restores it at `back_at`. Another process whose conversation the host does not name yet
    /// holds the switch's return ([`ModelSwitch::unproven`]) until it does; one that names a
    /// conversation where the switch's process never named its own (or a host that names
    /// conversations for neither) takes the switch on, as nothing tells the two apart. A
    /// crash's or a stall's relaunch of a conversation nobody asked anything starts afresh and
    /// reaches the loop by no step: it ends the switch as another program would.
    pub fn agent_process(
        &mut self,
        pid: u32,
        start: i64,
        conversation: Option<&str>,
        owed: impl FnOnce() -> bool,
    ) -> Option<SwitchScope> {
        let m = self.model.as_mut()?;
        let now = SwitchProc {
            pid,
            start,
            conversation: conversation.map(str::to_string),
        };
        let started = std::mem::take(&mut self.started_by_harness);
        let moved = match &m.proc {
            None => true,
            Some(was) if (was.pid, was.start) == (pid, start) => {
                now.conversation.is_some() && was.conversation != now.conversation
            }
            Some(was) => match (&was.conversation, &now.conversation) {
                (Some(_), None) => {
                    m.unproven = true;
                    self.started_by_harness = started;
                    return None;
                }
                (None, None) => true,
                (Some(held), Some(named)) if held == named => true,
                (None, Some(_)) => true,
                (Some(_), Some(_)) => {
                    if !started && !owed() {
                        self.model = None;
                        self.fallback_unasked = false;
                        return Some(SwitchScope::Closed);
                    }
                    true
                }
            },
        };
        m.unproven = false;
        moved.then(|| {
            m.proc = Some(now.clone());
            SwitchScope::Moved(now)
        })
    }

    /// THE HARNESS STARTED A NEW AGENT PROCESS: a step of the host's whose word says so
    /// ([`crate::harness::upgrade_drive::started`]), or this loop's own restart made (a memory
    /// restart's, a lost line's). While a model switch stands, the next other process the host
    /// names is that one ([`Self::agent_process`]).
    pub const fn harness_started(&mut self) {
        self.started_by_harness = self.model.is_some();
    }

    /// THE RELAUNCH'S START-UP UNDER A LIMIT: a usage limit, spend limit or
    /// model bucket ([`named_class`]) met under an agent whose screen is all
    /// its evidence ([`screen_alone`]: Muse Code, agy), whose
    /// process came back since the limit was last read — the host names a
    /// start later than that read ([`Self::agent_started`]), or, with no
    /// host's word, a point of no agent's (a shell's prompt: it crashed,
    /// exited or upgraded) was read in between.
    pub fn relaunch_startup(
        &mut self,
        reading: &mut TurnEndReading,
        now: Instant,
        ours: impl FnOnce() -> bool,
    ) {
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        let start = self.agent_start;
        let Some(t) = self.wall.as_mut().filter(|t| named_class(t.class)) else {
            return;
        };
        let Some(met) = t.program.filter(|p| screen_alone(*p)) else {
            return;
        };
        if reading.program != met {
            // No agent's screen: the agent left the tab. Another agent's
            // ends the limit ([`met_under_another`]).
            if !reads_conversation(reading.program) && !screen_alone(reading.program) {
                t.relaunch.away = true;
            }
            return;
        }
        let last = t.relaunch.read.unwrap_or(t.since);
        let relaunched = t.relaunch.away || start.is_some_and(|start| start > last);
        t.relaunch.away = false;
        t.relaunch.read = Some(now);
        if relaunched {
            t.relaunch.startup = true;
            reading.worked = None;
            if !t.seat_moved && !t.seat.same_account(&reading.seat) && !ours() {
                t.named = None;
                t.seat_moved = true;
                t.seat = reading.seat.clone();
            }
        } else if reading.worked.is_some() {
            t.relaunch.startup = false;
        }
        // The new process names its own seat: the old one's login is gone
        // with it ([`TurnEndState::seat_seen`]).
        if relaunched {
            self.seat_seen = Some((met, reading.seat.clone()));
        }
    }

    fn drop_gone_countdown(&mut self) {
        let gone = self
            .wall
            .as_ref()
            .and_then(|t| t.named)
            .is_some_and(|n| self.named_countdown_gone(&n));
        if gone && let Some(n) = self.wall.as_mut().and_then(|t| t.named.as_mut()) {
            n.by_itself = false;
        }
    }

    fn countdown_gone(&self, first: Instant) -> bool {
        self.agent_start.is_some_and(|start| start > first)
    }

    fn written_before_start(&self, written: Option<i64>) -> bool {
        written
            .zip(self.agent_start_unix)
            .is_some_and(|(written, start)| written < start)
    }

    fn named_countdown_gone(&self, n: &NamedReset) -> bool {
        self.countdown_gone(n.first) || self.written_before_start(n.written)
    }

    /// Records a turn the host typed (upgrade notice, carry-on; [`RULE_HOST_TURN`]), awaited like this policy's acts; once the agent takes it, a person's exchange left to them ([`Self::left_to_person`]) is over.
    ///
    /// Its words are the host's: a screen answers it with a prompt the harness typed
    /// ([`ScreenAnswer::harness`]) other than the one the last screen read showed answered.
    pub fn host_typed(&mut self, now: Instant, kind: HostTurn) {
        self.keyed_by_harness();
        self.land.check_pending = false;
        self.land.check_owed = false;
        self.fallback_unasked = false;
        self.checking = false;
        self.seeded_check = None;
        self.seeded_land = None;
        self.open.clear();
        self.repo = crate::harness::repo_work::RepoRead::Clean;
        self.landing = false;
        self.cut_check = false;
        self.cut_land = false;
        self.cut_uncounted = false;
        self.cut_text = None;
        self.compact_owes = None;
        self.account_asked |= self.account.is_some();
        self.account_startup = false;
        self.account_resume = false;
        self.login_resume = false;
        self.reset_resume = None;
        self.awaiting = Some(Awaited {
            rule: RULE_HOST_TURN,
            command: false,
            at: now,
            typed: now,
            unseen_since: None,
            taken: false,
            met: false,
            left: None,
            host: Some(kind),
            off_stood: false,
            wall_retry: false,
            own: false,
            said: None,
            over: self.screen_prompt,
            lead: None,
            opened: None,
            submitted: false,
            submits: None,
            screen: None,
            ran: None,
        });
    }
}

impl TurnEndState {
    /// Whether a program started fresh in the tab since the last point ([`Self::opened_fresh`]).
    #[must_use]
    pub const fn opened_fresh(&self) -> bool {
        self.opened_fresh
    }

    /// Whether the foreground process, which the host says resumes no conversation
    /// ([`AgentResumed::resumed`]), began within a turn of `worked` ending at `now`,
    /// [`PERSON_TURN_SLACK`] before the turn allowed: the person's message in it opened that
    /// program, banner unread or not. The loop folds that process's start before it observes the
    /// point (`Session::fold_point`), so [`Self::agent_started`] is its.
    fn launched_in_turn(&self, worked: Duration, now: Instant) -> bool {
        self.resumed_agent
            .is_some_and(|cur| !cur.resumed && cur.started.is_some())
            && self.agent_start.is_some_and(|at| {
                now.checked_sub(worked.saturating_add(PERSON_TURN_SLACK))
                    .is_none_or(|from| at >= from)
            })
    }

    /// Fold [`crate::supervise::IdleHost::agent_resumed`] before [`Self::observe`].
    ///
    /// A resumed process that has not yet drawn its transcript clears any [`Self::opened_fresh`]
    /// its banner tentatively set before its snapshot landed; a fresh process (`!resumed`) ends any
    /// [`Self::resumed`] grace from an earlier restart so its banner sets [`Self::opened_fresh`] at
    /// once.
    pub fn host_agent_resumed(&mut self, current: Option<AgentResumed>) {
        if let Some(cur) = current {
            let changed_process = self.transcript_pid != Some(cur.pid)
                || (self.transcript_start.is_some()
                    && cur.started.is_some()
                    && self.transcript_start != cur.started);
            if changed_process {
                self.transcript_drawn = false;
                self.transcript_pid = Some(cur.pid);
                self.transcript_start = cur.started;
            } else if self.transcript_start.is_none() {
                self.transcript_start = cur.started;
            }
            if !cur.resumed {
                self.resumed = None;
            } else if !self.transcript_drawn {
                self.opened_fresh = false;
            }
        }
        self.resumed_agent = current;
    }

    /// THE HOST'S RELAUNCH STARTED THE AGENT AFRESH (`done:fresh`: no conversation to resume), so
    /// its screen is a program started fresh, not the session's resume, and the next exchange
    /// opens its session ([`Self::opened_fresh`]) whether or not a screen of it is read first:
    /// [`Self::restarted`], called just before this, cleared any mark its banner set while the
    /// relaunch was under way (a `wait:` answer before this one). The mark is this loop's alone: a
    /// loop that takes over before the next point hears no `done:fresh`, and reads the program by
    /// its screens and by the host's word on its process ([`Self::host_agent_resumed`]).
    pub const fn relaunched_afresh(&mut self) {
        self.resumed = None;
        self.opened_fresh = true;
    }

    #[cfg(test)]
    pub(crate) const fn resumed_at(&self) -> Option<Instant> {
        self.resumed
    }

    /// Record a restart the loop asked its host for ([`TurnEndAction::Restart`]).
    pub fn restarted(&mut self, why: &Restart, made: bool, reading: &TurnEndReading, now: Instant) {
        self.keyed_by_harness();
        // `reading` is the decided point's screen, re-read for the restart: the screen as it stood
        // before the agent was ended, not one read after it. A turn the host types with the restart
        // is answered past the prompt that screen showed answered ([`Self::host_typed`]).
        self.screen_prompt = reading.answered.map(|a| a.prompt);
        if made {
            // The act the restart was decided over is not typed: the lines its go-ahead would
            // answer are back on the menu bar ([`Self::go_ahead_verdict`]).
            self.go_ahead_unwritten();
            self.replies = Replies::default();
            self.settling = None;
            // The relaunch resumes the session's conversation: a banner read while it was under
            // way (a `wait:` answer before this one), or just after, is its own. One that started
            // afresh marks the program fresh again ([`Self::relaunched_afresh`]).
            self.resumed = Some(now);
            self.opened_fresh = false;
        }
        if made && !matches!(why, Restart::Account { .. }) {
            self.account_startup = self.account.is_some();
        }
        match why {
            Restart::Memory | Restart::InputLost { .. } if made => self.harness_started(),
            Restart::Memory | Restart::InputLost { .. } => {}
            Restart::Model { to } if made => {
                let track = self.wall.as_ref().filter(|t| t.class == WallClass::Model);
                let credits = self
                    .credits
                    .take_if(|c| c.model.is_some())
                    .filter(|_| track.is_none());
                let back_at = reading
                    .reset_at
                    .or_else(|| track.and_then(|t| t.named).map(|n| n.at))
                    .or_else(|| credits.as_ref().and_then(|c| c.reset))
                    .filter(|&r| r > now);
                let from = bucket_model(&reading.wall_message)
                    .or_else(|| track.and_then(|t| bucket_model(&t.said)))
                    .or_else(|| credits.and_then(|c| c.model));
                self.model = Some(ModelSwitch {
                    from,
                    to: to.clone(),
                    back_at,
                    proc: None,
                    unproven: false,
                });
                self.fallback_unasked = true;
                if let Some(t) = &mut self.wall {
                    t.attempts += 1;
                    if t.class == WallClass::Model {
                        t.named = None;
                    }
                }
            }
            Restart::Model { .. } => self.fallback_unmade = true,
            Restart::ModelBack { .. } => {
                self.model = None;
                self.fallback_unasked = false;
            }
            Restart::Account { to, at } if made => {
                let owed = match self.account {
                    Some(_) if !self.account_asked => self.account_resume,
                    _ => true,
                };
                self.account = Some(AccountMove {
                    to: to.clone(),
                    at: now,
                    wall: *at == AccountAt::Wall,
                });
                self.account_asked = false;
                self.account_told = false;
                self.account_startup = true;
                self.account_closed = reading.answered.map(|a| a.prompt);
                self.account_resume = *at == AccountAt::Wall && owed;
                self.account_unmade = None;
                self.account_ahead_unmade = None;
                let window = self.timing.window;
                self.account_moves
                    .retain(|&t| now.saturating_duration_since(t) < window);
                self.account_moves.push_back(now);
                while self.account_moves.len() > 4 * ACCOUNT_MOVES_PER_WINDOW {
                    self.account_moves.pop_front();
                }
                self.wall = None;
                self.model = None;
                self.fallback_unmade = false;
                self.fallback_unasked = false;
                self.credits = None;
            }
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => {
                self.account_unmade = Some(Unmade::Again);
                self.account_unmade_ms = None;
            }
            Restart::Account {
                to,
                at: AccountAt::Ahead,
            } => self.account_ahead_unmade = Some((to.clone(), now)),
            Restart::Route { .. } if made => {
                self.route_unmade = None;
                // The lost login a refused token drew is the token's, left with it: the session
                // is on the row's stored login now.
                if self
                    .wall
                    .as_ref()
                    .is_some_and(|t| t.class == WallClass::Auth)
                {
                    self.wall = None;
                }
            }
            Restart::Route { to } => self.route_unmade = Some((*to, now)),
            // A click's restart is its lane's to ask, never the loop's: nothing of it is booked.
            Restart::Permission { .. } => {}
        }
    }

    /// [`Self::restarted`] for a restart the host did NOT make, with its word for why (`step`; `None`: no host answered).
    pub fn restart_unmade(
        &mut self,
        why: &Restart,
        step: Option<&str>,
        reading: &TurnEndReading,
        now: Instant,
    ) {
        self.restarted(why, false, reading, now);
        if self.account_unmade.is_some()
            && matches!(
                why,
                Restart::Account {
                    at: AccountAt::Wall,
                    ..
                }
            )
        {
            self.account_unmade = Some(Unmade::of(step));
        }
    }
}

/// The `/model` alias a bucket's notice names (`You've reached your Fable limit` → `fable`, `Opus 4.1 limit reached` → `opus`).
#[must_use]
pub fn bucket_model(message: &str) -> Option<String> {
    message
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .find(|w| MODEL_ALIASES.contains(&w.as_str()))
}

/// The work a finished turn's done row reports (`✻ Cooked for 3m 2s · done 4:24 PM` → 182 s).
#[must_use]
pub fn done_row_work(rows: &[String]) -> Option<Duration> {
    let row = status_line(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" for ")?;
    let span = rest.split(" · ").next()?.trim();
    let mut secs = 0u64;
    for word in span.split_whitespace() {
        let unit = word.chars().last()?;
        let n: u64 = crate::parse_uint(&word[..word.len() - unit.len_utf8()])?;
        let unit_secs: u64 = match unit {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        secs = secs.checked_add(n.checked_mul(unit_secs)?)?;
    }
    Some(Duration::from_secs(secs))
}

/// How far a done row may stand off the act whose turn it closes ([`act_turn_work`]).
pub const ACT_TURN_SLACK: Duration = Duration::from_secs(2 * 60);

/// THE WORK OF THE TURN AN ACT BEGAN.
#[must_use]
pub fn act_turn_work(
    act: i64,
    ended: i64,
    work: Duration,
    asleep: Duration,
    min: Duration,
) -> Option<Duration> {
    if ended <= act || work < min {
        return None;
    }
    let slack = i128::from(ACT_TURN_SLACK.as_secs());
    let span = i128::from(i64::try_from(work.as_secs()).ok()?);
    let least = i128::from(ended) - i128::from(act);
    let most = least + 59;
    if span + slack < least || span > most + slack {
        return None;
    }
    let most = u64::try_from(most).unwrap_or(u64::MAX);
    let awake = work.min(Duration::from_secs(most)).checked_sub(asleep)?;
    (awake >= min).then_some(awake)
}

/// The clock time of a turn's DONE ROW (`✻ Sautéed for 1d 2h 58m · done 12:19 PM · 1 shell still running` → `12:19 PM`).
#[must_use]
pub fn done_row_clock(rows: &[String]) -> Option<String> {
    let row = status_line(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" done ")?;
    let clock = rest.split(" · ").next()?.trim();
    (clock.starts_with(|c: char| c.is_ascii_digit())).then(|| clock.to_string())
}

/// Whether a suggestion is a continuation ([`SUGGESTION_ALLOW`]): one a build before v6's 5a accepted bare. The lab's placeholder scene drafts one, so a build that still accepted it would.
#[must_use]
pub fn suggestion_allowed(text: &str) -> bool {
    let lower = text
        .to_lowercase()
        .replace(" and ", ",")
        .replace(" then ", ",");
    let mut any = false;
    for piece in lower.split([',', ';', '.', '!']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if !SUGGESTION_ALLOW.contains(&piece) {
            return false;
        }
        any = true;
    }
    any
}

/// What the worker's last words ask of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    Plain,
    Offer,
    Question,
    Stop(String),
    Irreversible(String),
}

/// Whether the report `tail` asks a person for an irreversible act ([`Said::Irreversible`] past
/// its needs, as [`TurnEndReading::asked`] reads it): a go-ahead answers it only where
/// [`go_ahead_answers`] says.
#[must_use]
fn irreversible_ask(tail: Option<&str>) -> bool {
    asked_by(tail).is_some_and(|(_, irreversible)| irreversible)
}

/// What the report `tail` asks past its needs ([`classify_said`]): THE IRREVERSIBLE GUARD reads
/// the text the base read ([`needs::without_needs`]), whatever the go-ahead reader makes of it.
fn asked_by(tail: Option<&str>) -> Option<(String, bool)> {
    let rest = tail.map(needs::without_needs);
    match classify_said(rest.as_deref()) {
        Said::Irreversible(why) => Some((why, true)),
        Said::Stop(why) => Some((why, false)),
        Said::Question => Some(("the worker asked a question".to_string(), false)),
        Said::Offer | Said::Plain => None,
    }
}

/// Whether the owner's standing go-ahead ([`RULE_GO_AHEAD`]) may answer the report `tail`: the
/// guard reads no irreversible ask in it ([`irreversible_ask`]), or reads one only for the
/// agent's own cleanup (`remove the worktree`, `delete my merged branch`) on rows a NEEDS YOU line
/// the go-ahead reader parks on took only in that reader's view ([`needs::go_ahead_view`]: `… say
/// "rerun the checks" so` above `I can build …, push it to main and remove the worktree.`). The guard reads those rows as prose, as the base did, and the same
/// reply drawn at a width that wraps the need on a lowercase row asks nothing irreversible; read
/// with those cleanup verbs alone put aside, and every other word where the guard reads it, it
/// asks nothing irreversible either. The guard's own reading ([`TurnEndReading::asked`]) stays
/// the base's.
#[must_use]
pub(crate) fn go_ahead_answers(tail: Option<&str>) -> bool {
    let Some(tail) = tail.filter(|t| irreversible_ask(Some(t))) else {
        return true;
    };
    let mut rows: Vec<String> = tail.lines().map(str::to_string).collect();
    let mut tidied = false;
    for moved in needs::go_ahead_view(tail).moved {
        if !go_ahead::parks(&format!("{} {}", needs::NEEDS_YOU, moved.need)) {
            continue;
        }
        // The run as the need joined it, each row's words with where they start in the run.
        let mut run = String::new();
        let mut starts = Vec::new();
        for &i in &moved.rows {
            let Some(row) = rows.get(i) else { continue };
            if !run.is_empty() {
                run.push(' ');
            }
            let lead = needs::quote_lead(row);
            starts.push((i, run.len(), lead));
            run.push_str(row[lead..].trim_end());
        }
        for verb in go_ahead::own_cleanup_verbs(&run).into_iter().rev() {
            let Some(&(i, at, lead)) = starts.iter().rev().find(|(_, at, _)| *at <= verb.start)
            else {
                continue;
            };
            let from = lead + verb.start - at;
            let to = lead + verb.end - at;
            let row = &mut rows[i];
            let put = if row[from..].starts_with(char::is_uppercase) {
                "Tidy"
            } else {
                "tidy"
            };
            row.replace_range(from..to, put);
            tidied = true;
        }
    }
    tidied && !irreversible_ask(Some(&rows.join("\n")))
}

/// THE IRREVERSIBLE GUARD OVER A PARK READ OFF THE NEEDS FOLD'S WORDS: [`go_ahead_answers`] on the
/// report as the screen draws it, `rows`, as the guard always read it, and on the report as the
/// park reads it, `said` (the screen's measured lines where they hold its words:
/// [`needs_words`]), where the two differ. The go-ahead goes only where both let it, so reading
/// the park off whole lines never lets through an ask the guard reads in the rows.
#[must_use]
pub(crate) fn go_ahead_answers_on(rows: Option<&str>, said: Option<&str>) -> bool {
    go_ahead_answers(rows) && (said == rows || go_ahead_answers(said))
}

/// Judges the worker's last words ([`Said`]).
#[must_use]
pub fn classify_said(tail: Option<&str>) -> Said {
    let Some(tail) = tail else {
        return Said::Plain;
    };
    thread_local! {
        static LAST: LastTail<Said> = const { LastTail::new() };
    }
    LAST.with(|last| last.get_or(tail, || classify_said_read(tail)))
}

struct LastTail<T>(std::cell::RefCell<Option<(String, T)>>);

impl<T: Clone> LastTail<T> {
    const fn new() -> Self {
        LastTail(std::cell::RefCell::new(None))
    }

    fn get_or(&self, tail: &str, read: impl FnOnce() -> T) -> T {
        if let Some((seen, answer)) = &*self.0.borrow()
            && seen == tail
        {
            return answer.clone();
        }
        let answer = read();
        *self.0.borrow_mut() = Some((tail.to_owned(), answer.clone()));
        answer
    }
}

fn classify_said_read(tail: &str) -> Said {
    let lower = yes_no_asks(&tail.to_lowercase().replace('’', "'"));
    let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    // EVERY STOP PHRASE STOPS WHEREVER IT IS SAID (a fence, a `without`, a negation and an echo
    // included), as before the go-ahead reader: that reader only picks the answer to a stop that
    // is not irreversible ([`TurnEndState::go_ahead_owed`]), and no text the harness types holds
    // a stop phrase.
    if let Some(p) = STOP_PHRASES.iter().find(|p| flat.contains(*p)) {
        // A stop naming a destructive act anywhere asks consent to it; its
        // answer must consent to nothing irreversible.
        if let Some(w) = destructive_word(&flat) {
            return Said::Irreversible(format!("the worker said \"{p}\" of an act that would {w}"));
        }
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    let recommends = RECOMMEND_PHRASES.iter().any(|p| flat.contains(p));
    let offer_at = OFFER_PHRASES.iter().filter_map(|p| flat.find(p)).min();
    if let Some(at) = offer_at
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker offers to {w}"));
    }
    if let Some(w) = questions(&flat).into_iter().find_map(destructive_word) {
        return Said::Irreversible(format!("the worker asks whether to {w}"));
    }
    let offer = offer_at.is_some();
    let lines: Vec<&str> = lower
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let unit_start = lines.iter().rposition(|l| is_option_line(l)).unwrap_or(0);
    let unit = lines[unit_start..].join(" ");
    let unit = without_trailing_aside(&unit).trim_end_matches([')', '"', '*', '`', ' ']);
    let asks = unit.ends_with('?');
    let listed = lines.iter().filter(|l| is_option_line(l)).count();
    let ask = last_sentence(unit);
    let listed_choice = listed >= 2 && asks_to_choose(ask);
    let chooses = asks && (listed_choice || either_or(ask));
    let choice_phrase = CHOICE_PHRASES
        .iter()
        .filter_map(|p| flat.find(p).map(|at| (*p, at)))
        .min_by_key(|&(_, at)| at);
    let phrase_choice = choice_phrase.filter(|&(p, at)| phrase_asks(&flat, &lines, p, at));
    if (chooses || phrase_choice.is_some())
        && let Some(w) = destructive_word(&flat)
    {
        return Said::Irreversible(format!("the worker offers a choice that would {w}"));
    }
    if chooses && !recommends {
        return Said::Stop(if listed_choice {
            "the worker listed options with no recommendation".to_string()
        } else {
            "the worker asks to choose between options".to_string()
        });
    }
    if let Some((p, _)) = phrase_choice {
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    if let Some((_, at)) = choice_phrase
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker asks to choose whether to {w}"));
    }
    if !asks {
        return if offer { Said::Offer } else { Said::Plain };
    }
    if offer { Said::Offer } else { Said::Question }
}

const YES_NO_MARKERS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "(y/n)?", "[y/n]?"];

fn yes_no_asks(text: &str) -> String {
    let mut out = text.to_string();
    for m in YES_NO_MARKERS {
        out = out.replace(m, "?");
    }
    out
}

fn questions(flat: &str) -> Vec<&str> {
    let bytes = flat.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        let ends = matches!(bytes.get(i + 1), None | Some(b' '));
        if !ends {
            continue;
        }
        match b {
            b'?' => {
                out.push(flat[start..=i].trim());
                start = i + 1;
            }
            b'.' | b'!' | b';' | b':' => start = i + 1,
            _ => {}
        }
    }
    out
}

fn without_trailing_aside(unit: &str) -> &str {
    let mut t = unit.trim_end();
    while t.ends_with(')') {
        let Some(open) = t.rfind('(') else {
            break;
        };
        let before = t[..open].trim_end();
        if !before.ends_with(['?', '.', '!']) {
            break;
        }
        t = before;
    }
    t
}

fn last_sentence(text: &str) -> &str {
    let body = text.trim_end_matches('?');
    let mut start = 0;
    let bytes = body.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?' | b':' | b';') && bytes.get(i + 1) == Some(&b' ') {
            start = i + 1;
        }
    }
    text[start..].trim()
}

/// Leads that, before an either/or's ` or `, make it a choice the worker would act on (`should I … or …?`, `which would you prefer: … or …?`).
pub const CHOICE_LEADS: &[&str] = &[
    "should i",
    "shall i",
    "should we",
    "shall we",
    "want me to",
    "do you want",
    "would you like",
    "prefer",
    "which",
];

fn either_or(ask: &str) -> bool {
    ask.find(" or ")
        .is_some_and(|at| CHOICE_LEADS.iter().any(|l| ask[..at].contains(l)))
}

fn asks_to_choose(ask: &str) -> bool {
    let words: Vec<&str> = ask
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    either_or(ask)
        || CHOICE_LEADS.iter().any(|l| ask.contains(l))
        || words.iter().any(|w| {
            matches!(
                *w,
                "which" | "option" | "options" | "choose" | "pick" | "either" | "or"
            )
        })
}

fn phrase_asks(flat: &str, lines: &[&str], p: &str, at: usize) -> bool {
    let rest = &flat[at..];
    let bytes = rest.as_bytes();
    let end = bytes.iter().enumerate().position(|(i, b)| {
        matches!(b, b'.' | b'!' | b'?')
            && matches!(bytes.get(i + 1), None | Some(b' '))
            && !(*b == b'.' && i > 0 && bytes[i - 1].is_ascii_digit())
    });
    let asks_here = end.is_none_or(|i| bytes[i] == b'?' || i + 1 == bytes.len());
    let introduces = lines
        .iter()
        .position(|l| l.contains(p))
        .is_some_and(|k| lines.get(k + 1).is_some_and(|l| is_option_line(l)));
    asks_here || introduces
}

/// Whether `text` names a force-push git's short forms spell: a push whose clause has a short
/// flag cluster with an `f` (`push -f`, `push -uf`, `push origin main -f`), or a refspec forced
/// with `+` (`push origin +main`).
pub(crate) fn force_push(text: &str) -> bool {
    text.split([';', '|', '&', '\n', '?', '!']).any(|clause| {
        let mut push = false;
        let flagged = clause
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .filter(|word| !word.is_empty())
            .any(|word| match word {
                word if inflects(word, "push") => {
                    push = true;
                    false
                }
                _ => {
                    push && word.strip_prefix('-').is_some_and(|flags| {
                        !flags.starts_with('-')
                            && flags.bytes().take_while(|&b| b != b'o').any(|b| b == b'f')
                    })
                }
            });
        let mut push = false;
        let forced = clause.split_whitespace().any(|token| {
            let token = token.trim_matches(['`', '"', '\'', '(', ')', ',', '.', ':']);
            if token
                .split(|c: char| !(c.is_alphanumeric() || c == '-'))
                .any(|w| inflects(w, "push"))
            {
                push = true;
                return false;
            }
            push && token
                .strip_prefix('+')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))
        });
        flagged || forced
    })
}

pub(crate) fn destructive_word(text: &str) -> Option<&'static str> {
    let short_force_push = force_push(text);
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_start_matches('-'))
        .collect();
    DESTRUCTIVE_WORDS.iter().copied().find(|d| {
        if *d == "push -f" {
            return short_force_push;
        }
        let want: Vec<&str> = d.split(' ').map(|w| w.trim_start_matches('-')).collect();
        words.windows(want.len()).any(|win| {
            win.iter()
                .zip(want.iter())
                .all(|(w, base)| inflects(w, base))
        })
    })
}

fn inflects(word: &str, base: &str) -> bool {
    if word == base {
        return true;
    }
    let Some(rest) = word.strip_prefix(base) else {
        let stem = base.strip_suffix('e').unwrap_or(base);
        return (stem.len() < base.len()
            && word
                .strip_prefix(stem)
                .is_some_and(|rest| matches!(rest, "ing" | "ed" | "ten")))
            || (base.ends_with("write") && word == format!("{}ote", &base[..base.len() - 3]));
    };
    if matches!(rest, "s" | "es" | "d" | "ed" | "ing" | "ten") {
        return true;
    }
    let last = base.chars().last().unwrap_or(' ');
    rest.strip_prefix(last)
        .is_some_and(|rest| matches!(rest, "ed" | "ing" | "en"))
}

fn is_option_line(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && t[digits..].starts_with(['.', ')']) {
        return true;
    }
    let mut cs = t.chars();
    if let (Some(a), Some(b)) = (cs.next(), cs.next())
        && a.is_ascii_lowercase()
        && b == ')'
    {
        return true;
    }
    t.starts_with("(a)")
        || t.starts_with("(b)")
        || t.starts_with("- ")
        || t.starts_with("• ")
        || t.starts_with("option ")
}

/// What to type as a continuation.
#[must_use]
pub fn continuation_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.continue_text, rules)
}

/// What to type where the worker claims it is done under Cruise, as one whole act: the first
/// angle's ([`done_check_words`]). The loop's own done checks go round the angles
/// ([`TurnEndState::checks`]); this one is for a reader that types one text (the `harness policy`
/// reply's `done_check_text`, an upgrade's check after its restart, which cuts
/// [`go_ahead::NEVER_A_NEED`] within the person's hour: `harness::upgrade::checking`).
#[must_use]
pub fn done_check_text(rules: Option<&str>, irreversible: bool) -> String {
    done_check_act(0, rules, irreversible)
}

/// The done check at its `n`th claim of a Cruise run (from 0) as the loop types it where the
/// claim names no open work, the repository holds none and no need is owed: its words
/// ([`done_check_words`]), the reversible clause where the claim names an irreversible act, and
/// the standing rules fitted after them.
#[must_use]
pub fn done_check_act(n: usize, rules: Option<&str>, irreversible: bool) -> String {
    act_words(
        &done_check_words(n),
        &[reversible_clause(irreversible)],
        rules,
    )
}

/// Whether `typed` — a prompt's words as a transcript keeps them — is the
/// done check.
#[must_use]
pub fn is_done_check(typed: &str) -> bool {
    let typed = typed.trim();
    if typed
        .strip_prefix(DONE_CHECK)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    {
        return true;
    }
    RETIRED_DONE_CHECKS.iter().any(|check| {
        typed.strip_prefix(*check).is_some_and(|rest| {
            let rest = rest
                .strip_prefix(' ')
                .and_then(|tail| tail.strip_prefix(DONE_CHECK_REVERSIBLE))
                .unwrap_or(rest);
            rest.is_empty() || rest.starts_with(RULES_OPEN)
        })
    })
}

/// THE OPEN-WORK SUFFIX (the [`DONE_CHECK`] and the [`LAND_NUDGE`] where the claim or the defense named open work).
#[must_use]
pub fn open_work_suffix(found: &[&str]) -> Option<String> {
    open_named(found, OPEN_NAMED)
}

fn open_named(found: &[&str], most: usize) -> Option<String> {
    let most = most.max(1);
    let named = match found.get(..most) {
        _ if found.is_empty() => return None,
        Some(first) if found.len() > most => format!("{}, …", first.join(", ")),
        _ => found.join(", "),
    };
    Some(format!(
        "Your report still names open work ({named}): finish and land it first."
    ))
}

/// THE REPOSITORY SUFFIX: its finding in a few words, cut to [`REPO_SUMMARY_UNITS`] so the act stays a line the composer holds as typed.
#[must_use]
pub fn repo_work_suffix(repo: &crate::harness::repo_work::RepoWork) -> String {
    repo_said_to(&repo_finding(repo), RepoAsk::All)
}

/// THE REPOSITORY'S FINDING as Land's act names it ([`repo_work_suffix`]) and Cruise's
/// ([`own_repo_work_suffix`]): its summary, cut to [`REPO_SUMMARY_UNITS`] (`…/land-c: 1 untracked
/// (wip-notes.txt)`).
#[must_use]
pub fn repo_finding(repo: &crate::harness::repo_work::RepoWork) -> String {
    fit_units(&repo.summary(), REPO_SUMMARY_UNITS).into_owned()
}

/// CRUISE'S REPOSITORY SUFFIX, the done check's: the finding as [`repo_work_suffix`] names it,
/// then the agent's own work landed and the rest left as it is. The probe reads the whole
/// repository, another session's stash and worktrees included, and Cruise's ask is always the
/// agent's own work (v6 §14.5's angle 6), so it never says to land all of it.
#[must_use]
pub fn own_repo_work_suffix(repo: &crate::harness::repo_work::RepoWork) -> String {
    repo_said_to(&repo_finding(repo), RepoAsk::Own)
}

/// What the repository suffix asks of the work it names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RepoAsk {
    /// All of it on the remote main: Land's.
    All,
    /// What the agent made, the rest left as it is: Cruise's done check's.
    Own,
}

fn repo_said_to(summary: &str, ask: RepoAsk) -> String {
    let ask = match ask {
        RepoAsk::All => REPO_ALL,
        RepoAsk::Own => REPO_OWN,
    };
    if summary.is_empty() {
        return format!("{REPO_HEAD}: {ask}");
    }
    format!("{REPO_HEAD} ({summary}): {ask}")
}

const REPO_HEAD: &str = "Your repository still holds work no remote main has";
const REPO_ALL: &str = "commit, merge and push it to the remote main first.";
const REPO_OWN: &str = "land what of it you made first, and leave what you did not make exactly \
                        as it is.";

/// THE REPOSITORY SUFFIX WHERE THE PROBE COULD NOT READ THE REPOSITORY ([`crate::harness::repo_work::RepoRead::Unread`]).
pub const REPO_UNREAD: &str = "Your repository could not be read: show its git status yourself \
                               and land what it holds first.";

/// [`REPO_UNREAD`] under Cruise's done check ([`own_repo_work_suffix`]): the agent lands what of it
/// it made.
pub const REPO_UNREAD_OWN: &str = "Your repository could not be read: show its git status \
                                   yourself, land what of it you made first, and leave what you \
                                   did not make exactly as it is.";

const REPO_SUMMARY_UNITS: usize = 160;

/// RETIRED: the fixed words the last build before Autopilot Cruise opened its open-items challenge with ([`RULE_OPEN_ITEMS`]).
pub const OPEN_ITEMS_HEAD: &str = "Your report says done but still names open work";

/// RETIRED: the open-items challenge's opening words before [`OPEN_ITEMS_HEAD`].
pub const OPEN_ITEMS_HEADS_BEFORE: [&str; 2] = [
    "Your report ends with ATERM-TASK-DONE but still names open",
    "Your report still names open",
];

/// RETIRED: the fixed words the uncommitted-work challenge opened with ([`RULE_UNCOMMITTED`], [`is_challenge_text`]).
pub const UNCOMMITTED_HEAD: &str = "Your report says done but work is not committed and pushed";

/// Whether `typed` is one of the retired challenges ([`is_challenge_rule`]), by its opening words.
#[must_use]
pub fn is_challenge_text(typed: &str) -> bool {
    [OPEN_ITEMS_HEAD, UNCOMMITTED_HEAD]
        .iter()
        .chain(OPEN_ITEMS_HEADS_BEFORE.iter())
        .any(|head| {
            let mut words = typed
                .split_whitespace()
                .map(|w| w.trim_end_matches([':', ',']));
            head.split_whitespace().all(|w| words.next() == Some(w))
        })
}

const OPEN_NAMED: usize = 4;

const fn reversible_clause(irreversible: bool) -> &'static str {
    if irreversible {
        DONE_CHECK_REVERSIBLE
    } else {
        ""
    }
}

const fn ack_clause(ack: bool) -> &'static str {
    if ack { NEEDS_ACK } else { "" }
}

pub(super) fn act_words(base: &str, after: &[&str], rules: Option<&str>) -> String {
    let parts: Vec<Suffix<'_>> = after.iter().map(|a| Suffix::Fixed(a)).collect();
    fitted_act(base, &parts, rules)
}

#[derive(Clone, Copy)]
enum Suffix<'a> {
    Fixed(&'a str),
    Open(&'a [&'static str]),
    Repo(&'a crate::harness::repo_work::RepoRead, RepoAsk),
    /// The agent's own list of what is left, quoted ([`land_check::listed_suffix`]).
    Listed(Option<&'a str>),
    /// How many processes the session's agent left running ([`left_suffix`],
    /// [`own_left_suffix`]), in the room the repository's ask leaves: whole, else short
    /// ([`left_suffix_short`]), else not at all. None for `0`.
    Left(u16, RepoAsk),
}

/// THE LEFT SUFFIX'S HEAD: how many processes the session's agent started still run.
fn left_head(n: u16) -> String {
    match n {
        1 => "1 process this session started still runs".to_owned(),
        n => format!("{n} processes this session started still run"),
    }
}

/// THE CHECKS LAND MAKES BEFORE A LANDING, as `harness policy` names them (`land_checks=`), so a
/// harness that supervises itself makes the same: `git`, the session repository read for work no
/// remote main has; `left`, the processes the session's agent started that still run
/// (`aterm ctl @self sys left`), each ended or kept by a `keep <token>` line under `Nothing left:`.
pub const LAND_CHECKS: &str = "git,left";

/// Where the agent reads the rows the left suffix counts.
const LEFT_SYS: &str = "`aterm ctl @self sys left`";

/// What the left suffix asks, and its close: no process is a person's question.
const LEFT_NEVER_ASK: &str = "Do not ask about a process: it is never a NEEDS YOU line.";

/// LAND'S LEFT SUFFIX ([`HoldLeg::Left`]): `n` processes the session's agent started still run,
/// where to see them, how to keep one it was asked to run (a `keep <token>` line under `Nothing
/// left:`, which the landing then takes as kept) and how to end what it started for its own
/// checks. Numbers and fixed words only: no executable, path or argument.
#[must_use]
pub fn left_suffix(n: u16) -> String {
    format!(
        "{} ({LEFT_SYS}). Keep anything you were asked to run, or are unsure of, with a line \
         `keep <token>` under `Nothing left:`; end what you started for your own checks with \
         `aterm ctl end <token>`. {LEFT_NEVER_ASK}",
        left_head(n)
    )
}

/// CRUISE'S LEFT SUFFIX, its done check's: [`left_suffix`]'s count in its own-work words, as
/// [`own_repo_work_suffix`] asks for the repository: end what the agent started for its own
/// checks, leave running what it was asked to run. It holds nothing.
#[must_use]
pub fn own_left_suffix(n: u16) -> String {
    format!(
        "{} ({LEFT_SYS}): end what you started for your own checks with `aterm ctl end \
         <token>`, and leave running what you were asked to run. {LEFT_NEVER_ASK}",
        left_head(n)
    )
}

/// THE LEFT SUFFIX WHERE THE WHOLE ONE DOES NOT FIT: the count and where to see the rows.
#[must_use]
pub fn left_suffix_short(n: u16) -> String {
    format!("{}: {LEFT_SYS}.", left_head(n))
}

/// Whether `text` carries a left suffix of `n` rows, whole or short ([`Suffix::Left`]).
#[must_use]
pub fn carries_left_suffix(text: &str, n: u16) -> bool {
    n > 0 && text.contains(&left_head(n))
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

fn fitted_act(base: &str, parts: &[Suffix<'_>], rules: Option<&str>) -> String {
    fit_act(base, parts, rules).0
}

/// THE DONE CHECK AS THE LOOP TYPES IT ([`words_act`]): [`done_check_words`] at the `n`th claim,
/// without [`go_ahead::NEVER_A_NEED`] within the person's hour (`held`), fitted with `parts`
/// ([`fitted_act`]). The repository's ask outranks that sentence: where the check's fixed clauses
/// leave no room for the ask beside it (a destructive need's reversible clause and NEEDS YOU's
/// acknowledgment both riding) and the room the sentence leaves takes it, the sentence is left
/// out and the ask rides, which names the agent's own work to land.
fn fitted_check(n: usize, held: bool, parts: &[Suffix<'_>], rules: Option<&str>) -> String {
    let check = done_check_words(n);
    let fitted = |held| fit_act(&go_ahead::unless_held(&check, held), parts, rules);
    let (text, repo_left) = fitted(held);
    if repo_left
        && !held
        && let (cut, false) = fitted(true)
    {
        return cut;
    }
    text
}

/// [`fitted_act`]'s text, and whether the repository's ask among `parts` was left out for room.
fn fit_act(base: &str, parts: &[Suffix<'_>], rules: Option<&str>) -> (String, bool) {
    let listed = parts.iter().find_map(|p| match p {
        Suffix::Listed(l) => *l,
        _ => None,
    });
    // LAND KEEPS RESOLVING (§14.17 item 1): the ask to resolve rides while a list stands, a part
    // as fixed as the act's own, whether or not the quote fits beside the repository's ask.
    let resolve = listed.is_some_and(|l| !land_check::listed_words(l).is_empty());
    let fixed = units(base)
        + parts
            .iter()
            .map(|p| match p {
                Suffix::Fixed(f) if !f.is_empty() => 1 + units(f),
                _ => 0,
            })
            .sum::<usize>()
        + if resolve {
            1 + units(land_check::LISTED_RESOLVE)
        } else {
            0
        };
    let room = VERBATIM_UNITS.saturating_sub(fixed);
    let found: &[&'static str] = parts
        .iter()
        .find_map(|p| match p {
            Suffix::Open(f) => Some(*f),
            _ => None,
        })
        .unwrap_or(&[]);
    let (repo_read, ask) = parts
        .iter()
        .find_map(|p| match p {
            Suffix::Repo(r, ask) => Some((Some(*r), *ask)),
            _ => None,
        })
        .unwrap_or((None, RepoAsk::All));
    let repo = repo_read.and_then(|r| r.found());
    let unread = repo_read.is_some_and(|r| r.is_unread());
    let unread_said = match ask {
        RepoAsk::All => REPO_UNREAD,
        RepoAsk::Own => REPO_UNREAD_OWN,
    };
    let repo_in = |room: usize| -> Option<String> {
        if unread {
            return (units(unread_said) < room).then(|| unread_said.to_string());
        }
        let repo = repo?;
        let whole = repo_said_to(&fit_units(&repo.summary(), REPO_SUMMARY_UNITS), ask);
        if units(&whole) < room {
            return Some(whole);
        }
        let frame = units(&repo_said_to("x", ask));
        let cut = fit_units(&repo.summary(), room.saturating_sub(frame)).into_owned();
        let said = repo_said_to(&cut, ask);
        (units(&said) < room).then_some(said)
    };
    let mut chosen: (Option<String>, Option<String>) = (None, None);
    let caps = [
        land_check::LISTED_CAP,
        land_check::LISTED_CAP / 2,
        land_check::LISTED_CAP / 4,
        0,
    ];
    for cap in listed.map_or(&[][..], |_| &caps[..]) {
        let quote = listed.and_then(|l| land_check::listed_in(l, *cap));
        if quote.as_ref().is_some_and(|q| 1 + units(q) > room) {
            continue;
        }
        let left = room.saturating_sub(quote.as_ref().map_or(0, |q| 1 + units(q)));
        let repo_s = repo_in(left);
        if repo_s.is_some() || (repo.is_none() && !unread) || *cap == 0 {
            chosen = (quote, repo_s);
            break;
        }
    }
    for most in (0..=OPEN_NAMED.min(found.len()))
        .rev()
        .filter(|_| listed.is_none())
    {
        let open = (most > 0).then(|| open_named(found, most)).flatten();
        let left = room.saturating_sub(open.as_ref().map_or(0, |o| 1 + units(o)));
        if open.as_ref().is_some_and(|o| 1 + units(o) > room) {
            continue;
        }
        let repo_s = repo_in(left);
        if repo_s.is_some() || (repo.is_none() && !unread) || most == 0 {
            chosen = (open, repo_s);
            break;
        }
    }
    let (open, repo_s) = chosen;
    let repo_left = (repo.is_some() || unread) && repo_s.is_none();
    // THE LEFT SUFFIX TAKES THE ROOM THE REPOSITORY'S ASK LEFT, whole, else short, else none: it
    // never pushes that ask out.
    let spent = [&open, &repo_s]
        .into_iter()
        .flatten()
        .map(|said| 1 + units(said))
        .sum::<usize>();
    let left_s = parts
        .iter()
        .find_map(|p| match p {
            Suffix::Left(n, ask) if *n > 0 => Some((*n, *ask)),
            _ => None,
        })
        .and_then(|(n, ask)| {
            let whole = match ask {
                RepoAsk::All => left_suffix(n),
                RepoAsk::Own => own_left_suffix(n),
            };
            let room = room.saturating_sub(spent);
            [whole, left_suffix_short(n)]
                .into_iter()
                .find(|said| units(said) < room)
        });
    let mut text = base.to_string();
    for p in parts {
        let said = match p {
            Suffix::Fixed(f) => Some(*f),
            Suffix::Open(_) | Suffix::Listed(_) => open.as_deref(),
            Suffix::Repo(..) => repo_s.as_deref(),
            Suffix::Left(..) => left_s.as_deref(),
        };
        if let Some(a) = said.filter(|a| !a.is_empty()) {
            text.push(' ');
            text.push_str(a);
        }
        if resolve && matches!(p, Suffix::Listed(_)) {
            text.push(' ');
            text.push_str(land_check::LISTED_RESOLVE);
        }
    }
    (with_rules(&text, rules), repo_left)
}

/// LAND'S CONTRADICTION (v6 §14.4's slice 4c), which Land's nudge types under
/// [`RULE_LAND_NUDGE`] in place of [`LAND_NUDGE`] where a land act's reply defends the work with
/// `Nothing left:` and a leg still holds ([`land_words`]): [`LAND_CONTRADICTED_HEAD`], then the
/// measured fact or the agent's own list, then its one ask (land it, never delete work) and
/// Land's close ([`land_close!`]). Where git found work (`repo`), its finding leads
/// ([`CONTRADICTED_GIT`], as Land's act names it: [`repo_finding`]), and a
/// standing list rides after the close as Land's nudge quotes it ([`land_check::listed_suffix`]);
/// else the list leads, quoted within [`CONTRADICTED_QUOTE_UNITS`], and a repository the probe
/// could not read rides as [`REPO_UNREAD`]. `after` are the clauses that follow it (the
/// reversible clause, NEEDS YOU's acknowledgment), each whole; the standing rules take what room
/// is left ([`with_rules`]). `None` where neither leg holds: nothing to contradict.
#[must_use]
pub fn land_contradicted(
    repo: &crate::harness::repo_work::RepoRead,
    listed: Option<&str>,
    after: &[&str],
    rules: Option<&str>,
) -> Option<String> {
    land_contradicted_left(repo, listed, 0, after, rules)
}

/// [`land_contradicted`] with the left suffix of `left` rows after the repository's ask
/// ([`left_suffix`] where it fits, else [`left_suffix_short`], else none): the processes
/// the agent left running contradict no defense of their own, so they ride a contradiction git or
/// the list makes and make none.
#[must_use]
pub fn land_contradicted_left(
    repo: &crate::harness::repo_work::RepoRead,
    listed: Option<&str>,
    left: u16,
    after: &[&str],
    rules: Option<&str>,
) -> Option<String> {
    let listed = listed.filter(|l| !land_check::listed_words(l).is_empty());
    let fixed: Vec<Suffix<'_>> = after.iter().map(|a| Suffix::Fixed(a)).collect();
    if let Some(found) = repo.found() {
        let base = contradicted_git(&repo_finding(found));
        let parts: Vec<Suffix<'_>> = [Suffix::Listed(listed), Suffix::Left(left, RepoAsk::All)]
            .into_iter()
            .chain(fixed)
            .collect();
        return Some(fitted_act(&base, &parts, rules));
    }
    let list = listed?;
    let quote = quote_within(&land_check::listed_words(list), CONTRADICTED_QUOTE_UNITS);
    // The quote's own end closes its sentence where it has one (`.`, `…`), so none is added.
    let close = if quote.ends_with(['.', '!', '?', '\u{2026}']) {
        land_check::LISTED_CLOSE.trim_end_matches('.')
    } else {
        land_check::LISTED_CLOSE
    };
    let base = format!(
        "{LAND_CONTRADICTED_HEAD} {CONTRADICTED_LISTED}{quote}{close} {CONTRADICTED_LISTED_ASK} \
         {LAND_CLOSE}"
    );
    let parts: Vec<Suffix<'_>> = [
        Suffix::Repo(repo, RepoAsk::All),
        Suffix::Left(left, RepoAsk::All),
    ]
    .into_iter()
    .chain(fixed)
    .collect();
    Some(fitted_act(&base, &parts, rules))
}

/// The contradiction's lead where git found work: the head, the fact with `finding` in
/// parentheses (none where it is empty), the ask and Land's close.
fn contradicted_git(finding: &str) -> String {
    let fact = if finding.is_empty() {
        CONTRADICTED_GIT.to_string()
    } else {
        format!("{CONTRADICTED_GIT} ({finding})")
    };
    format!("{LAND_CONTRADICTED_HEAD} {fact}. {CONTRADICTED_GIT_ASK} {LAND_CLOSE}")
}

/// `s` within `room` UTF-16 units: whole, or cut at a word's edge with `…`, or, where its first
/// word alone runs past `room`, cut inside that word.
fn quote_within(s: &str, room: usize) -> String {
    let cut = fit_units(s, room);
    if !cut.is_empty() || s.is_empty() {
        return cut.into_owned();
    }
    let budget = room.saturating_sub('…'.len_utf16());
    let mut used = 0;
    let kept: String = s
        .chars()
        .take_while(|c| {
            used += c.len_utf16();
            used <= budget
        })
        .collect();
    format!("{kept}…")
}

/// What to type as an answer to a question for a person.
#[must_use]
pub fn answer_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.answer_text, rules)
}

/// The answer to a prose question under `cfg.question_priority`.
#[must_use]
pub fn prose_answer(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    act_words(prose_words(cfg), &[NEEDS_HINT], rules)
}

fn prose_words(cfg: &SupervisorConfig) -> &str {
    cfg.question_priority
        .text()
        .unwrap_or(cfg.answer_text.as_str())
}

/// The answer to a question or choice naming an irreversible act ([`Said::Irreversible`]).
pub const REVERSIBLE_ANSWER: &str = "Nobody is here to approve an irreversible step: take the \
                                     option that deletes, overwrites and force-pushes nothing, \
                                     and keep going.";

/// [`REVERSIBLE_ANSWER`], then [`NEEDS_HINT`], with the standing rules when there are any.
#[must_use]
pub fn reversible_answer(rules: Option<&str>) -> String {
    act_words(REVERSIBLE_ANSWER, &[NEEDS_HINT], rules)
}

/// THE OWNER'S STANDING GO-AHEAD for `park` ([`RULE_GO_AHEAD`]): [`go_ahead::GO_AHEAD_ANSWER`],
/// then the park's own words (the word the agent asked the person to say, or its quote, cut to
/// the room the rest leaves), the wait for room where it names memory or disk, the drop of its
/// `NEEDS YOU:` line, the other needs left standing, and [`NEEDS_ACK`] where owed.
#[must_use]
pub fn go_ahead_answer(park: &go_ahead::Park, ack: bool, rules: Option<&str>) -> String {
    let after: Vec<&str> = [
        park.waits_room.then_some(go_ahead::GO_AHEAD_ROOM),
        park.need.then_some(go_ahead::GO_AHEAD_DROP),
        park.others.then_some(go_ahead::GO_AHEAD_OTHERS),
        ack.then_some(NEEDS_ACK),
    ]
    .into_iter()
    .flatten()
    .collect();
    let fixed =
        units(go_ahead::GO_AHEAD_ANSWER) + after.iter().map(|a| 1 + units(a)).sum::<usize>();
    let said = park.said(VERBATIM_UNITS.saturating_sub(fixed + 1));
    let parts: Vec<&str> = std::iter::once(said.as_str()).chain(after).collect();
    act_words(go_ahead::GO_AHEAD_ANSWER, &parts, rules)
}

/// The most UTF-16 code units a text this policy types may hold and still be drawn as typed.
pub const VERBATIM_UNITS: usize = 800;

pub(super) fn with_rules(text: &str, rules: Option<&str>) -> String {
    let Some(r) = rules.map(str::trim).filter(|r| !r.is_empty()) else {
        return text.to_string();
    };
    let frame = text.encode_utf16().count() + RULES_OPEN.len() + ")".len();
    let r = fit_units(r, VERBATIM_UNITS.saturating_sub(frame));
    if r.is_empty() {
        return text.to_string();
    }
    format!("{text}{RULES_OPEN}{r})")
}

pub(super) fn fit_units(s: &str, room: usize) -> Cow<'_, str> {
    if s.encode_utf16().count() <= room {
        return Cow::Borrowed(s);
    }
    let budget = room.saturating_sub('…'.len_utf16());
    let mut units = 0;
    let mut end = 0;
    for (at, c) in s.char_indices() {
        units += c.len_utf16();
        if units > budget {
            break;
        }
        end = at + c.len_utf8();
    }
    let cut = if s[end..].starts_with(char::is_whitespace) {
        end
    } else {
        s[..end].rfind(char::is_whitespace).unwrap_or(0)
    };
    match s[..cut].trim_end() {
        "" => Cow::Borrowed(""),
        kept => Cow::Owned(format!("{kept}…")),
    }
}

pub(super) const RULES_OPEN: &str = " (standing rules: ";

/// THE ACTS AN `aterm:` FACT RIDES ([`crate::supervise::facts`]; system awareness §14.4): prompts
/// this policy types anyway, in its own words — the continuation, the done check, Land's
/// instruction and its nudge. Never an answer (a fact beside it could steer the choice), a
/// wall's retry or an act typed in a limit's place ([`TurnEndState::in_limits_place`]), an
/// accepted suggestion (the vendor's words), the person's draft, a command, or a line of the
/// upgrade's or a relaunch's.
pub const FACT_RIDES: [&str; 4] = [RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, RULE_LAND_NUDGE];

/// How a fact opens after the text it rides and its standing rules.
pub const FACT_OPEN: &str = " (aterm: ";

/// The words a fact adds to the act it rides: ` (aterm: {text})`.
#[must_use]
pub fn fact_suffix(fact: &crate::supervise::facts::Fact) -> String {
    format!("{FACT_OPEN}{})", fact.text)
}

/// A DECLARED WAKE'S FACT TO RIDE AN ACT (the slice-2 plan's D3 and its writer W2): the one way
/// a wake's fact reaches what this policy types at an idle point ([`TurnEndReading::wake`]).
/// [`Self::new`] alone makes one, from the owner's grant (a
/// [`PromptGrant`](crate::supervise::config::PromptGrant), which `Occupancy::grant` alone mints,
/// under `prompt`) and a closed template's fact sentence (a
/// [`FactLine`](crate::supervise::occupancy::FactLine), which `occupancy::templates` alone
/// makes), so no wake rides under `observe` or `off` and nothing its declarer wrote rides with
/// it. Its line's mark, advice and rule name stay behind: the fact sentence alone rides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RideFact {
    fact: crate::supervise::facts::Fact,
    ord: u16,
}

impl RideFact {
    /// Wake `ord` of `episode`'s fact, `line`'s sentence without its full stop (as a hung call's
    /// rides), read at `as_of_ms` (unix ms), under `grant`.
    #[must_use]
    pub fn new(
        _grant: crate::supervise::config::PromptGrant,
        line: &crate::supervise::occupancy::FactLine,
        ord: u16,
        episode: u64,
        as_of_ms: i64,
    ) -> Self {
        Self {
            fact: wake_fact(line, episode, as_of_ms),
            ord,
        }
    }

    /// Its fact, as it rides ([`fact_suffix`]).
    #[must_use]
    pub const fn fact(&self) -> &crate::supervise::facts::Fact {
        &self.fact
    }

    /// The wake's ordinal in its session's book.
    #[must_use]
    pub const fn ord(&self) -> u16 {
        self.ord
    }

    /// The wake's episode.
    #[must_use]
    pub const fn episode(&self) -> u64 {
        self.fact.key.id
    }
}

/// Wake `episode`'s fact as `line`'s sentence says it, without its full stop (as a hung call's
/// rides), read at `as_of_ms` (unix ms): what a point's act carries ([`RideFact::new`], under
/// `prompt`) or, under `observe`, would have carried ([`fact_would_ride`]). A
/// [`TurnEndReading::fact`] of the wake's kind rides nothing ([`slot_fact`]).
#[must_use]
pub fn wake_fact(
    line: &crate::supervise::occupancy::FactLine,
    episode: u64,
    as_of_ms: i64,
) -> crate::supervise::facts::Fact {
    use crate::supervise::facts::{EpisodeKey, Fact, FactKind};
    let sentence = line.fact();
    Fact {
        key: EpisodeKey {
            kind: FactKind::Wake,
            id: episode,
        },
        text: sentence.strip_suffix('.').unwrap_or(sentence).to_string(),
        as_of_ms,
    }
}

/// THE ONE FACT SLOT's fact in `reading` ([`fact_rides`]): a hung call's ([`TurnEndReading::fact`])
/// first, else a declared wake's ([`TurnEndReading::wake`]). A wake's rides by its
/// [`RideFact`] alone: one in [`TurnEndReading::fact`] rides nothing.
#[must_use]
pub fn slot_fact(reading: &TurnEndReading) -> Option<&crate::supervise::facts::Fact> {
    use crate::supervise::facts::FactKind;
    reading
        .fact
        .as_ref()
        .filter(|f| f.key.kind != FactKind::Wake)
        .or_else(|| reading.wake.as_ref().map(RideFact::fact))
}

/// `action` without `fact` where it carries it ([`carries_fact`]): the act as the policy decided
/// it before the fact rode, its own text and standing rules unchanged. Any other act as it is.
#[must_use]
pub fn without_fact(action: TurnEndAction, fact: &crate::supervise::facts::Fact) -> TurnEndAction {
    match action {
        TurnEndAction::Type { text, rule_id } if FACT_RIDES.contains(&rule_id) => {
            let text = match text.strip_suffix(&fact_suffix(fact)) {
                Some(plain) => plain.to_string(),
                None => text,
            };
            TurnEndAction::Type { text, rule_id }
        }
        other => other,
    }
}

/// Whether `action` is an act a fact may ride in `state` ([`FACT_RIDES`]).
#[must_use]
pub fn fact_may_ride(state: &TurnEndState, action: &TurnEndAction) -> bool {
    matches!(action, TurnEndAction::Type { rule_id, .. }
        if FACT_RIDES.contains(rule_id) && !state.in_limits_place(rule_id))
}

/// Whether `action` carries `fact` ([`fact_rides`]): the act whose taking tells it.
#[must_use]
pub fn carries_fact(action: &TurnEndAction, fact: &crate::supervise::facts::Fact) -> bool {
    matches!(action, TurnEndAction::Type { text, rule_id }
        if FACT_RIDES.contains(rule_id) && text.ends_with(&fact_suffix(fact)))
}

/// Whether `said`, an act's text with `fact` after it, fits what this policy types: within
/// [`VERBATIM_UNITS`], and a declared wake's fact under the facts seam's line budget too
/// ([`LINE_CAP`](crate::supervise::occupancy::templates::LINE_CAP), the slice-2 plan's F9).
fn fact_fits(fact: &crate::supervise::facts::Fact, said: &str) -> bool {
    use crate::supervise::facts::FactKind;
    use crate::supervise::occupancy::templates::LINE_CAP;
    said.encode_utf16().count() <= VERBATIM_UNITS
        && (fact.key.kind != FactKind::Wake || said.chars().count() < LINE_CAP)
}

/// Whether `fact` would ride `action`, an act decided without it ([`fact_rides`]'s test, nothing
/// typed): what `observe` books of a declared wake's fact a point's act would have carried under
/// `prompt` (the slice-2 plan's 2.9).
#[must_use]
pub fn fact_would_ride(
    state: &TurnEndState,
    action: &TurnEndAction,
    fact: &crate::supervise::facts::Fact,
) -> bool {
    fact_may_ride(state, action)
        && matches!(action, TurnEndAction::Type { text, .. }
            if fact_fits(fact, &format!("{text}{}", fact_suffix(fact))))
}

/// `action` with `fact` after its text and standing rules, where it is an act the fact rides
/// ([`fact_may_ride`]) and the whole still fits ([`fact_fits`]). ALL OR NOTHING: a fact that
/// does not fit is left off whole (the rules are never cut further for it, and the fact is never
/// cut) and stays untold for a later act.
fn fact_rides(
    state: &TurnEndState,
    action: TurnEndAction,
    fact: Option<&crate::supervise::facts::Fact>,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    let Some(fact) = fact.filter(|_| fact_may_ride(state, &action)) else {
        return action;
    };
    let TurnEndAction::Type { text, rule_id } = action else {
        return action;
    };
    // THE ACT THAT NAMES THE AGENT'S LEFT PROCESSES carries no fact that names them again.
    if fact.key.kind == crate::supervise::facts::FactKind::Leftover
        && carries_left_suffix(&text, state.left_said)
    {
        note(trace, "fact", Some(rule_id), || {
            Outcome::Unproven(format!(
                "{} {:016x}: the act names the processes left: typed without it",
                fact.key.kind.word(),
                fact.key.id
            ))
        });
        return TurnEndAction::Type { text, rule_id };
    }
    let said = format!("{text}{}", fact_suffix(fact));
    if !fact_fits(fact, &said) {
        note(trace, "fact", Some(rule_id), || {
            let limit = if said.encode_utf16().count() > VERBATIM_UNITS {
                format!("{VERBATIM_UNITS} units")
            } else {
                format!(
                    "its line's cap of {} characters",
                    crate::supervise::occupancy::templates::LINE_CAP
                )
            };
            Outcome::Unproven(format!(
                "{} {:016x} would pass {limit}: typed without it",
                fact.key.kind.word(),
                fact.key.id
            ))
        });
        return TurnEndAction::Type { text, rule_id };
    }
    note(trace, "fact", Some(rule_id), || {
        Outcome::Decided(format!(
            "{} {:016x} rides",
            fact.key.kind.word(),
            fact.key.id
        ))
    });
    TurnEndAction::Type {
        text: said,
        rule_id,
    }
}

fn later(at: Instant, d: Duration) -> Instant {
    at.checked_add(d).unwrap_or(at)
}

/// The decision at one point (module header).
#[must_use]
pub fn decide_turn_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    decide_turn_end_traced(state, reading, cfg, now, &mut NoTrace)
}

fn ended_at(trace: &mut dyn Trace, check: &'static str, action: TurnEndAction) -> TurnEndAction {
    note(trace, check, action.rule_id(), || {
        Outcome::Decided(format!("{action:?}"))
    });
    action
}

/// [`decide_turn_end`], each check it consults written to `trace` as it is consulted ([`super::trace`]).
/// The reading's fact rides the act decided last ([`fact_rides`]), once.
#[must_use]
pub fn decide_turn_end_traced(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    let action = decide_unridden(state, reading, cfg, now, trace);
    // A PARK THE PERSON'S HOUR HOLDS IS DECIDED AGAIN AT THE HOUR'S END: nothing typed here would
    // otherwise stand on an unchanged screen past the hour, as a same reply's hold does.
    let action = match (&action, state.go_ahead_hour_ends(reading, now)) {
        (TurnEndAction::Nothing, Some(until)) => ended_at(
            trace,
            GO_AHEAD_HOUR,
            TurnEndAction::WaitUntil {
                until,
                why: GO_AHEAD_HOUR_WHY.to_string(),
                wait: WaitKind::Other,
            },
        ),
        _ => action,
    };
    // `[harness] facts = false` limits wherever the policy decides, a shadow's candidate too: a
    // wake's fact as well as a hung call's.
    let fact = slot_fact(reading).filter(|_| cfg.facts);
    fact_rides(state, action, fact, trace)
}

/// [`decide_turn_end_traced`] before the reading's fact rides the act: the readings it decides
/// again on come back here, so a fact rides once.
fn decide_unridden(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    // AUTOPILOT OFF: the harness keeps its hands off the session — plain
    // Claude Code. Nothing typed, waited for, restarted or escalated,
    // whatever the screen shows: a wall, a memory banner, an owed act or
    // an open switch included.
    if reading.autopilot == Autopilot::Off {
        return ended_at(trace, "autopilot-off", TurnEndAction::Nothing);
    }
    if state.login_left(reading) && !(state.account_resume && cfg.resume_limits) {
        let plain = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            ..reading.clone()
        };
        note(trace, "login-left", None, || Outcome::Passed);
        return decide_unridden(state, &plain, cfg, now, trace);
    }
    if state.bucket_left(reading) {
        let plain = TurnEndReading {
            wall: None,
            wall_message: String::new(),
            ..reading.clone()
        };
        note(trace, "bucket-left", None, || Outcome::Passed);
        return decide_unridden(state, &plain, cfg, now, trace);
    }
    if reading.upgrading {
        match reading.wall {
            None if state.after.is_none() => {
                return ended_at(trace, "upgrading", TurnEndAction::Nothing);
            }
            None => {}
            Some(WallKind::Memory) => {}
            Some(_) if reading.restartable => {
                let unrestartable = TurnEndReading {
                    restartable: false,
                    ..reading.clone()
                };
                note(trace, "upgrading", None, || Outcome::Passed);
                return decide_unridden(state, &unrestartable, cfg, now, trace);
            }
            Some(_) => {}
        }
    }
    if reading.wall == Some(WallKind::Memory) && !reading.interrupted {
        return ended_at(trace, "memory", memory_restart(cfg, reading));
    }
    // An act the program answered with its own stop notice was taken: that notice is the act's
    // answer, read below (`stopped-turn`).
    if let Some(a) = state
        .awaiting
        .as_ref()
        .filter(|_| reading.stopped_turn().is_none())
    {
        if !reading.authoritative
            || reading.survey
            || matches!(reading.phase, Phase::Busy | Phase::Prompt)
        {
            return ended_at(trace, "awaiting", TurnEndAction::Nothing);
        }
        let due = a.due(&state.timing);
        if now < due {
            return ended_at(
                trace,
                "awaiting",
                TurnEndAction::WaitUntil {
                    until: due,
                    why: format!("the worker to take the {} act", a.rule),
                    wait: WaitKind::Other,
                },
            );
        }
        note(trace, "awaiting", None, || Outcome::Passed);
    }
    if super::login::carried(reading.program)
        && let Some(screen) = &reading.login_screen
    {
        let act = if super::login::backs_out(state, reading, cfg, screen, now) {
            TurnEndAction::Cancel {
                rule_id: super::login::RULE_LOGIN_BACK_OUT,
            }
        } else {
            match (screen, state.login_closing(now)) {
                (aterm_phase::LoginScreen::Success { .. }, _) | (_, None) => {
                    super::login::dialog_act(state, screen)
                }
                (_, Some(until)) => TurnEndAction::WaitUntil {
                    until,
                    why: "the /login dialog aterm backed out of to close".to_string(),
                    wait: WaitKind::Other,
                },
            }
        };
        // A PERSON'S OWN `/login` LEFT OVER A USAGE LIMIT is closed once the limit's act is due and
        // the dialog stood unused ([`super::login::over_limit_act`]): it covers the composer that
        // act, and Claude Code's own continue, would reach.
        if act == TurnEndAction::Nothing
            && let Some(left) = super::login::over_limit_act(state, reading, cfg, screen, now)
        {
            return ended_at(trace, "login-over-limit", left);
        }
        if act.rule_id().is_some()
            && let Some((until, why)) = carry_held(state, reading, cfg, now)
        {
            return ended_at(
                trace,
                "login-dialog",
                TurnEndAction::WaitUntil {
                    until,
                    why: why.to_string(),
                    wait: WaitKind::Other,
                },
            );
        }
        return ended_at(trace, "login-dialog", act);
    }
    let back_elsewhere = !state.landed()
        && state.login_track().is_some()
        && (login_back_elsewhere(state, reading) || login_left_renewed(state, reading));
    // CRUISE NEVER RESTS (owner ruling 2026-10-26): no done task (a landing, a person's own
    // exchange) holds a Cruise session; Land's landing and its person's exchange do.
    let rests = state.task_done() && reading.autopilot != Autopilot::Cruise && !back_elsewhere;
    if reading.at_sign_in_menu() {
        let act = sign_in_menu_waits(state, reading, cfg, now, reading.taskless || rests)
            .unwrap_or_else(|| TurnEndAction::Escalate {
                reason: format!(
                    "the login is gone: {}",
                    sign_in_how(reading.program, &reading.wall_message)
                ),
            });
        return ended_at(trace, "sign-in-menu", act);
    }
    // A PROGRAM'S OWN STOP NOTICE (Muse Code's fault rows) is no reply and no wall, so no rule
    // types at it; only a person clears what it names, so the point is handed over, once a
    // person at the keyboard, there to clear it, has had their grace.
    if let Some(row) = reading.stopped_turn() {
        let act = match person_hold(state, reading, cfg, now) {
            Some((until, why)) => TurnEndAction::WaitUntil {
                until,
                why: why.to_string(),
                wait: WaitKind::Other,
            },
            None => TurnEndAction::Escalate {
                reason: format!(
                    "{} stopped the turn: {row}; only a person fixes it",
                    reading.program.product()
                ),
            },
        };
        return ended_at(trace, "stopped-turn", act);
    }
    if !reading.at_point(state.stale_input) || reading.taskless || rests {
        return ended_at(trace, "no-task", TurnEndAction::Nothing);
    }
    note(trace, "no-task", None, || Outcome::Passed);
    // A PERSON AT THE KEYBOARD HOLDS EVERY ACT, the resume past their own `/login` at a limit
    // too (owner rule 2026-10-25: never interrupt the person): it stays owed, one line once their
    // grace lapses.
    let held = match (
        person_hold(state, reading, cfg, now),
        login_quiet(state, reading, now),
    ) {
        (Some((until, _)), Some(quiet)) => (now < quiet).then_some((
            quiet.min(until),
            "a person sent the prompt the lost login answered",
        )),
        (held, _) => held,
    };
    if let Some((until, why)) = held {
        return ended_at(
            trace,
            "person",
            TurnEndAction::WaitUntil {
                until,
                why: why.to_string(),
                wait: WaitKind::Other,
            },
        );
    }
    note(trace, "person", None, || Outcome::Passed);
    let act = act_at_point(state, reading, cfg, now, trace);
    // Recovery cannot submit a person's standing draft after their typing grace expires.
    if matches!(&act, TurnEndAction::Type { text, .. } if text.starts_with(FORWARD_RECOVERY))
        && reading.composer.drafted()
        && reading.person.is_some()
    {
        return ended_at(trace, "person-draft", TurnEndAction::Nothing);
    }

    // What stands in or over the composer asks first, whatever the act (a lost login's `/login`
    // too): its question is decided again the moment the composer reads otherwise
    // (`turn_end_loop::draft_holds`), where the login's would hold the point for good.
    let drafted = match (act.rule_id(), reading.composer.asks()) {
        (None, _) => return act,
        (Some(_), Some(why)) => TurnEndAction::Escalate {
            reason: why.to_string(),
        },
        (Some(RULE_LOGIN), None)
            if matches!(reading.composer, Composer::Typed | Composer::Command(..)) =>
        {
            TurnEndAction::Escalate {
                reason: login_gone(
                    reading.program,
                    state.wall_words(reading),
                    Some(if reading.composer == Composer::Typed {
                        "a draft stands in the composer"
                    } else {
                        "a command stands unsent in the composer"
                    }),
                    &sign_in_how(reading.program, state.wall_words(reading)),
                ),
            }
        }
        (Some(rule_id), None) if reading.composer == Composer::Typed => {
            TurnEndAction::Submit { rule_id }
        }
        // A PERSON'S COMMAND STANDS (v6 §14.17.5.8): nothing typed over it, nothing cleared, no
        // Enter; the loop journals the hold once and decides again when the draft changes or goes.
        (Some(_), None) if matches!(reading.composer, Composer::Command(..)) => {
            TurnEndAction::Nothing
        }
        (Some(_), None) => return act,
    };
    ended_at(trace, "draft", drafted)
}

fn person_hold(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<(Instant, &'static str)> {
    let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
    let held = match (reading.person, reading.interrupted) {
        (Some(ago), _) if ago < grace => Some((later(now, grace - ago), PERSON_TYPING)),
        (_, true) if state.woken_esc.is_none() => Some((
            later(state.esc_at.unwrap_or(now), grace),
            "a person stopped the turn with Esc",
        )),
        _ => None,
    };
    held.filter(|(until, _)| now < *until)
}

/// HOW LONG THE KEY THAT SENT THE PROMPT A LOST LOGIN ANSWERED HOLDS `/login` ([`login_quiet`]).
pub const LOGIN_QUIET: Duration = Duration::from_secs(10);

fn login_quiet(state: &TurnEndState, reading: &TurnEndReading, now: Instant) -> Option<Instant> {
    let track = state.wall.as_ref().filter(|t| t.class == WallClass::Auth)?;
    let seen = track.login.prompt_at?;
    let key = now.checked_sub(reading.person?)?;
    (super::login::carried(reading.program)
        && reading.wall == Some(WallKind::Auth)
        && matches!(reading.composer, Composer::Empty | Composer::Placeholder(_))
        && !reading.interrupted
        && track.attempts == 0
        && !track.login.cancelled
        && key <= seen)
        .then(|| later(key, LOGIN_QUIET))
}

fn carry_held(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<(Instant, &'static str)> {
    let held = person_hold(state, reading, cfg, now)?;
    let typed = state.login_typed_at();
    let key = reading.person.and_then(|ago| now.checked_sub(ago));
    match (typed, key) {
        (Some(typed), Some(key)) if key <= typed && !reading.interrupted => None,
        _ => Some(held),
    }
}

pub(crate) fn login_back_elsewhere(state: &TurnEndState, reading: &TurnEndReading) -> bool {
    state.login_back_from(reading.login_shared.as_ref())
}

fn login_opening_wait(state: &TurnEndState, now: Instant) -> Option<TurnEndAction> {
    state
        .login_opening(now)
        .map(|until| TurnEndAction::WaitUntil {
            until,
            why: "the /login dialog to open".to_string(),
            wait: WaitKind::Other,
        })
}

fn budget_spent(state: &TurnEndState, cfg: &SupervisorConfig, now: Instant) -> bool {
    cfg.continue_per_hour != 0 && state.acts_in_window(now) >= cfg.continue_per_hour as usize
}

const BUDGET_REACHED: &str = "continue_per_hour = ";

/// THE HOUR'S TYPING BUDGET ESCALATES AND BLOCKS NOTHING.
#[must_use]
pub fn budget_frees(
    state: &TurnEndState,
    cfg: &SupervisorConfig,
    action: &TurnEndAction,
    now: Instant,
) -> Option<Instant> {
    match action {
        TurnEndAction::Escalate { reason } if reason.starts_with(BUDGET_REACHED) => {
            Some(state.budget_frees_at(cfg.continue_per_hour, now))
        }
        _ => None,
    }
}

fn act_at_point(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
) -> TurnEndAction {
    let budgeted = |act: TurnEndAction| {
        if budget_spent(state, cfg, now) {
            TurnEndAction::Escalate {
                reason: format!(
                    "{BUDGET_REACHED}{} reached: {} typed in the last {} min",
                    cfg.continue_per_hour,
                    state.acts_in_window(now),
                    state.timing.window.as_secs() / 60
                ),
            }
        } else {
            act
        }
    };
    // A RESUME PAST A CONDITION TYPES ITS ONE LINE ([`cleared_text`]), never Cruise's or Land's
    // prompt (v6 §14.17.5): those are typed at the agent's own turn end. A compaction's owed
    // continuation is the mode's own ([`cleared_of`]), and so is a resume under Land before Land's
    // instruction was given: the instruction is the one prompt, never a line and then it. A
    // person's own exchange at the wall is theirs, which Land leaves to them
    // ([`TurnEndState::exchange_at_wall`]): its resume is the line, so their request goes on.
    let continue_as = |rule: &'static str, clearing: Clearing| {
        if rule == RULE_LIMIT_RESUME
            && let Some(act) = limit_end(state, reading, cfg, now, &budgeted)
        {
            return act;
        }
        let text = match (cleared_of(state, reading, rule), reading.autopilot) {
            (Some(cleared), Autopilot::Land) if !state.land.typed && state.exchange_at_wall() => {
                with_rules(cleared_text(cleared, clearing), reading.rules.as_deref())
            }
            (_, Autopilot::Land) if !state.land.typed => {
                land_words(state, reading, state.needs_ack_owed()).0
            }
            (Some(cleared), _) => {
                with_rules(cleared_text(cleared, clearing), reading.rules.as_deref())
            }
            (None, Autopilot::Land) => land_words(state, reading, state.needs_ack_owed()).0,
            // The person's key holds the go-ahead, and the continuation's word that it is no need;
            // so does a park answers off leave to the person. This arm is the continuation a
            // `/compact` owes (the one rule `cleared_of` reads no condition for), which goes ahead
            // of the escalation such a park gets ([`TurnEndState::owed_held_at`]).
            (None, Autopilot::Cruise | Autopilot::Off) => go_ahead::unless_held(
                &continuation_text(cfg, reading.rules.as_deref()),
                state.owed_held_at(reading, cfg, now),
            )
            .into_owned(),
        };
        budgeted(TurnEndAction::Type {
            text,
            rule_id: rule,
        })
    };
    let type_as = |rule: &'static str, text: &str| {
        budgeted(TurnEndAction::Type {
            text: with_rules(text, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    let held =
        |trace: &mut dyn Trace, act: TurnEndAction| match credits_hold(state, reading, cfg, now) {
            Some(hold) => ended_at(
                trace,
                "credits-hold",
                held_by_credits(state, reading, cfg, now, act, hold),
            ),
            None => act,
        };
    if let Some(t) = unread_track(state, reading) {
        let look = later(now, LIMIT_CONVERSATION_LOOK);
        let until = t
            .named
            .filter(|n| now < n.at)
            .map_or(look, |n| n.at.min(look));
        return ended_at(
            trace,
            "limit-conversation-unread",
            TurnEndAction::WaitUntil {
                until,
                why: "the remembered limit's conversation is unread; no act on its account"
                    .to_string(),
                wait: WaitKind::Other,
            },
        );
    }
    if let Some(kind) = reading.wall {
        // THE SAME REPLY AGAIN ([`TurnEndState::repeats`]): the wall answered its spent tries the same
        // — an old login's answers, where the login moved since ([`TurnEndState::login_resume`]).
        if !state.login_resume && state.repeats(reading) {
            return ended_at(trace, SAME_REPLY, TurnEndAction::Nothing);
        }
        let act = wall_action(
            state,
            reading,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
        let act = ended_at(trace, "wall", wall_breaker(state, now, act));
        return match class_of(kind) {
            WallClass::Retry | WallClass::Context | WallClass::Usage | WallClass::Model => {
                held(trace, act)
            }
            WallClass::Auth | WallClass::Memory => act,
        };
    }
    if let Some(t) = &state.wall
        && t.class == WallClass::Auth
        && !reading.login_back
    {
        let shown = still_shown(t, reading).or_else(|| {
            waited_login(state, reading, cfg, now).map(|waited| (WallKind::Auth, waited))
        });
        let Some((kind, shown)) = shown else {
            let held = login_opening_wait(state, now).unwrap_or(TurnEndAction::Nothing);
            if reading.login_screen.is_none() && reading.composer != Composer::Absent {
                let back = login_back_elsewhere(state, reading)
                    .then(|| type_as(RULE_LOGIN_BACK, &login_back_text()));
                let act = match (login_wall_moves(state, reading, cfg, now), back) {
                    (Some(to), back) => Some(TurnEndAction::Restart {
                        why: Restart::Account {
                            to: to.to_string(),
                            at: AccountAt::Wall,
                        },
                        rule_id: RULE_ACCOUNT,
                        otherwise: Box::new(back.unwrap_or_else(|| held.clone())),
                    }),
                    (None, back) => back,
                };
                if let Some(act) = act {
                    return ended_at(trace, "login-held", act);
                }
            }
            return ended_at(trace, "login-held", held);
        };
        let act = wall_action(
            state,
            &shown,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
        return ended_at(trace, "login-held", wall_breaker(state, now, act));
    }
    let act = act_past_walls(
        state,
        reading,
        cfg,
        now,
        trace,
        &budgeted,
        &continue_as,
        &type_as,
    );
    let act = held(trace, act);
    // A yielding move the decider would ask is made ahead of an act it types first (a command's
    // owed continuation, a limit's or a login's resume), that act its fallback: holding the act
    // would leave it owed, and every point the hold's end decides again would hold it again.
    match account_hold(reading.advised(), now, &act) {
        Some(hold) => match move_ahead(state, reading, cfg, now) {
            Some(to) => ended_at(trace, "account-ahead", ahead_to(to, act)),
            None => ended_at(trace, "account-hold", hold),
        },
        None => act,
    }
}

/// A TURN END ON AN ACCOUNT THIS MAC CANNOT EXECUTE, WITH NO ROW TO MOVE TO OR ITS MOVE
/// NOT MADE, YIELDS (including a preparation hold or a senior claim, as in
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` F19, §3.5.3): where the host advises
/// [`AccountAdvice::Hold`], or a move the session yields
/// by (`yields`) that the act does not make, an act that would type into the session or relaunch
/// it waits until the hold's instant (at least [`ACCOUNT_WAIT_FLOOR`]), when the host is asked
/// again. An act that types nothing, waits, escalates or moves the session off the account
/// stands. `advised` is the point's advice past its probe, or the host's asked again where a move
/// ahead it advised was not made (`Session::ahead_deferred`). At a point, a held act whose move
/// the decider would ask is that move's fallback instead ([`act_at_point`]), so the hold stands
/// only where the decider does not ask it (refused within the hour, or the hour's moves made), and
/// at the loop's fallback once the relaunch deferred or refused it. The wait gives one reason for
/// every hold: a Hold carries no cause, and is advised both where no row can take the session and
/// where one can but a limit reset in flight bars the move.
pub(crate) fn account_hold(
    advised: Option<&AccountAdvice>,
    now: Instant,
    act: &TurnEndAction,
) -> Option<TurnEndAction> {
    let Some(
        AccountAdvice::Hold { until }
        | AccountAdvice::Move {
            yields: Some(until),
            ..
        },
    ) = advised
    else {
        return None;
    };
    if matches!(
        act,
        TurnEndAction::Nothing
            | TurnEndAction::WaitUntil { .. }
            | TurnEndAction::Escalate { .. }
            | TurnEndAction::Restart {
                why: Restart::Account { .. },
                ..
            }
    ) {
        return None;
    }
    Some(TurnEndAction::WaitUntil {
        until: (*until).max(later(now, ACCOUNT_WAIT_FLOOR)),
        why: "this account cannot run here, and the session cannot move off it yet".to_string(),
        wait: WaitKind::Other,
    })
}

#[allow(clippy::too_many_arguments)]
fn act_past_walls(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    trace: &mut dyn Trace,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    // THE SAME REPLY AGAIN ([`TurnEndState::repeats`]), the screen's or the one a wall's track stands for: no act answers it
    // — an old login's answers, where the login moved since ([`TurnEndState::login_resume`]).
    // The screen's reply that parks on a go-ahead owed and not yet typed on the standing park
    // gets it once ([`TurnEndState::go_ahead_untried`]): the answers that formed the hold were other
    // acts', and the go-ahead may be what the agent waits for.
    // The person's switch to Land over the hold counts as their turn: Land asks once more
    // ([`TurnEndState::switch_asks`], v6 §14.17 item 3).
    if state.replies_repeat(reading, now) {
        // A repeated report is evidence to change approach, not a reason to hand the
        // session back to the person. Keep the existing pacing and leave actual provider
        // walls to their recovery policy; an ordinary Cruise reply always gets another turn.
        if state.repeat_gets_a_turn(reading, now) {
            let wait = state.short_wait().unwrap_or(Duration::from_secs(120));
            let mut until = later(state.replies_at.unwrap_or(now), wait);
            if budget_spent(state, cfg, now) {
                until = until.max(state.budget_frees_at(cfg.continue_per_hour, now));
            }
            if now < until {
                return ended_at(
                    trace,
                    SAME_REPLY,
                    TurnEndAction::WaitUntil {
                        until,
                        why: REPEAT_RECOVERY_WAIT.to_string(),
                        wait: WaitKind::Other,
                    },
                );
            }
            return ended_at(
                trace,
                SAME_REPLY,
                outage_hold(state, reading, now)
                    .unwrap_or_else(|| type_as(RULE_CONTINUE, FORWARD_RECOVERY)),
            );
        }
        // A wordless reply held is waited on and tried again ([`TurnEndState::wordless_probe`]).
        match state.wordless_probe(reading) {
            Some(until) if now < until => {
                return ended_at(
                    trace,
                    SAME_REPLY,
                    TurnEndAction::WaitUntil {
                        until,
                        why: "the agent answered with nothing; trying it again later".to_string(),
                        wait: WaitKind::Other,
                    },
                );
            }
            Some(_) => {}
            None => return ended_at(trace, SAME_REPLY, TurnEndAction::Nothing),
        }
    }
    // A command's continuation, owed once it has run: where a `/compact`
    // owes the act a full context cut ([`TurnEndState::compact_owes`]), that
    // act again — the done check, the challenge or the land act as typed.
    if let Some(rule) = state.after {
        if let Some(text) = state.compact_owed_text(rule, reading, cfg, now) {
            return ended_at(
                trace,
                "owed",
                budgeted(TurnEndAction::Type {
                    text,
                    rule_id: rule,
                }),
            );
        }
        return ended_at(trace, "owed", continue_as(rule, Clearing::Measured));
    }
    if let Some(act) = reset_continuation(state, cfg, now, continue_as) {
        return ended_at(trace, "reset-continuation", act);
    }
    // The vendor's own continuation after a usage limit, stopped by the harness's Esc where the
    // Mac slept through it in the vendor's API retry ([`TurnEndState::woken_esc`]): the
    // continuation the limit's resume types, in its place.
    if reading.interrupted && state.woken_esc.is_some_and(|e| e.resume) {
        return ended_at(
            trace,
            "woken-resume",
            continue_as(RULE_LIMIT_RESUME, Clearing::Measured),
        );
    }
    if let Some(t) = &state.wall
        && named_class(t.class)
    {
        if let Some((kind, shown)) = still_shown(t, reading) {
            if !cfg.resume_limits {
                return ended_at(trace, "limit-left", TurnEndAction::Nothing);
            }
            let act = wall_action(
                state,
                &shown,
                cfg,
                now,
                kind,
                budgeted,
                continue_as,
                type_as,
            );
            return ended_at(trace, "limit-left", wall_breaker(state, now, act));
        }
        if t.attempts == 0 && cfg.resume_limits {
            return ended_at(trace, "limit-left", resume_limit(continue_as));
        }
    }
    if let Some(t) = &state.wall
        && let Some((kind, shown)) = left_retry(t, reading)
    {
        if !cfg.retry_api_errors {
            return ended_at(trace, "retry-left", TurnEndAction::Nothing);
        }
        let act = wall_action(
            state,
            &shown,
            cfg,
            now,
            kind,
            budgeted,
            continue_as,
            type_as,
        );
        return ended_at(trace, "retry-left", wall_breaker(state, now, act));
    }
    if state.account_resume && cfg.resume_limits {
        return ended_at(trace, "account-resume", resume_limit(continue_as));
    }
    if state.login_resume && cfg.resume_limits {
        return ended_at(trace, "login-resume", resume_limit(continue_as));
    }
    // A refused standing decision changes the approach after the normal pacing interval.
    // It does not transfer routine work to the person.
    if state.go_ahead_owed(reading, now).is_some() && state.go_ahead_spent(now) {
        let until = later(
            state
                .replies_at
                .or_else(|| state.acts.back().copied())
                .or(state.go_aheads.first)
                .unwrap_or(now),
            GO_AHEAD_WAIT,
        );
        let until = if budget_spent(state, cfg, now) {
            until.max(state.budget_frees_at(cfg.continue_per_hour, now))
        } else {
            until
        };
        let recovery = if now < until {
            TurnEndAction::WaitUntil {
                until,
                why: "standing decision refused; changing approach on the next turn".to_string(),
                wait: WaitKind::Other,
            }
        } else {
            outage_hold(state, reading, now)
                .unwrap_or_else(|| type_as(RULE_CONTINUE, FORWARD_RECOVERY))
        };
        return ended_at(trace, GO_AHEAD_REFUSED, recovery);
    }
    let cont = continue_policy(state, reading, cfg, now, budgeted);
    if let Some(to) = move_ahead(state, reading, cfg, now)
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return ended_at(trace, "account-ahead", ahead_to(to, cont));
    }
    if let Some(m) = &state.model
        && let Some(at) = m.back_at
        && now >= at
        && !m.unproven
        && reading.restartable
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return ended_at(
            trace,
            "model-back",
            TurnEndAction::Restart {
                why: Restart::ModelBack { to: m.from.clone() },
                rule_id: RULE_MODEL_RESTORE,
                otherwise: Box::new(cont),
            },
        );
    }
    if let Some(to) = route_ahead(state, reading, now)
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return ended_at(trace, "route", route_to(to, cont));
    }
    ended_at(trace, "continue-policy", cont)
}

/// THE ROUTE MOVE AT A TURN END (§5.5 of `docs/DESIGN-claude-setup-token-2026-11-05.md`): the
/// host's offer ([`TurnEndReading::route`]) where the relaunch reaches the tab, no account move
/// of the session's is still unanswered, and the host did not refuse the same move within the
/// window ([`TurnEndState::route_refused`]). It is asked after every account move the claims ring
/// makes ([`move_ahead`], at a wall [`login_wall_moves`]): a session that must yield its account
/// does that instead, and its target row starts on its own route. It is asked only at a turn end
/// and at the lost-login wall ([`wall_action`]): a refused token's move back, or a stored
/// session's move onto its account's token in use here, where the turn is over too, so no session
/// is ended mid-turn for it.
fn route_ahead(state: &TurnEndState, reading: &TurnEndReading, now: Instant) -> Option<RouteTo> {
    let to = reading.route?;
    (reading.restartable
        && !reading.relaunch_off
        && !reading.upgrading
        && state.account.is_none()
        && !state.route_refused(to, now))
    .then_some(to)
}

fn route_to(to: RouteTo, otherwise: TurnEndAction) -> TurnEndAction {
    TurnEndAction::Restart {
        why: Restart::Route { to },
        rule_id: RULE_TOKEN_ROUTE,
        otherwise: Box::new(otherwise),
    }
}

fn move_ahead<'a>(
    state: &TurnEndState,
    reading: &'a TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<&'a String> {
    match reading.advised() {
        Some(AccountAdvice::Move { to, .. })
            if state.account.is_none()
                && !state.account_ahead_refused(to, now)
                && (account_rotates(state, reading, cfg)
                    || login_move(state, reading, cfg, to))
                && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW =>
        {
            Some(to)
        }
        _ => None,
    }
}

fn ahead_to(to: &str, otherwise: TurnEndAction) -> TurnEndAction {
    TurnEndAction::Restart {
        why: Restart::Account {
            to: to.to_string(),
            at: AccountAt::Ahead,
        },
        rule_id: RULE_ACCOUNT,
        otherwise: Box::new(otherwise),
    }
}

fn credits_hold(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndAction> {
    if cfg.usage_credits {
        return None;
    }
    let held = state.credits_held(reading.credits.as_ref(), reading.program, now)?;
    let hold = match super::outlook::credits_outlook(held.reset.is_some(), cfg, true) {
        super::outlook::CreditsOutlook::WaitsForReset => TurnEndAction::WaitUntil {
            until: held.lifts.unwrap_or(now),
            why: match &held.model {
                Some(model) => format!(
                    "usage credits carry {model}'s requests ({RULE_CREDITS_HOLD}): its limit resets"
                ),
                None => format!(
                    "usage credits carry the session ({RULE_CREDITS_HOLD}): the subscription's \
                     window resets"
                ),
            },
            wait: WaitKind::Other,
        },
        super::outlook::CreditsOutlook::Person if !held.told => TurnEndAction::Escalate {
            reason: if held.model.is_some() {
                CREDITS_HOLD_MODEL_TOLD
            } else {
                CREDITS_HOLD_TOLD
            }
            .to_string(),
        },
        super::outlook::CreditsOutlook::Person => match held.lifts {
            Some(until) => TurnEndAction::WaitUntil {
                until,
                why: format!(
                    "usage credits carry the session ({RULE_CREDITS_HOLD}), no reset named: \
                     the five-hour window has reset by then"
                ),
                wait: WaitKind::Other,
            },
            None => TurnEndAction::Nothing,
        },
        super::outlook::CreditsOutlook::Carried | super::outlook::CreditsOutlook::Unattended => {
            TurnEndAction::Nothing
        }
    };
    if let Some(model) = &held.model
        && let Some(to) = credits_fallback(state, reading, cfg, model)
    {
        return Some(TurnEndAction::Restart {
            why: Restart::Model { to },
            rule_id: RULE_MODEL_FALLBACK,
            otherwise: Box::new(hold),
        });
    }
    Some(hold)
}

fn credits_fallback(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    model: &str,
) -> Option<String> {
    let to = cfg.model_fallback.as_ref()?;
    let same = to
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case(model));
    (!same
        && reading.restartable
        && state.model.is_none()
        && !state.fallback_unmade
        && !state.fallback_refused())
    .then(|| to.clone())
}

fn held_by_credits(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    act: TurnEndAction,
    hold: TurnEndAction,
) -> TurnEndAction {
    let types = |act: &TurnEndAction| {
        !matches!(
            act,
            TurnEndAction::Nothing
                | TurnEndAction::WaitUntil { .. }
                | TurnEndAction::Escalate { .. }
        )
    };
    match act {
        act if !types(&act) => act,
        TurnEndAction::Restart {
            why:
                why @ Restart::Account {
                    at: AccountAt::Ahead,
                    ..
                },
            rule_id,
            otherwise,
        } => TurnEndAction::Restart {
            why,
            rule_id,
            otherwise: Box::new(if types(&otherwise) { hold } else { *otherwise }),
        },
        _ => match move_ahead(state, reading, cfg, now) {
            Some(to) => ahead_to(to, hold),
            None => hold,
        },
    }
}

fn continue_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    let land = reading.autopilot == Autopilot::Land;
    // THE LAND CHECK'S ANSWER IS READ LITERALLY (§14.17 item 5): its list, or a need beside one,
    // is the agent's own, which Land's next prompt quotes; it is never a question to answer.
    let (asked, irreversible) = match reading.asked() {
        Some((why, irreversible)) if !(land && state.land.answered) => (Some(why), irreversible),
        _ => (None, false),
    };
    let done = state.done >= 1 && !state.exchange && !reading.interrupted;
    // THE GO-AHEAD (owner, 2026-10-29): a park on a person's go-ahead for the agent's own
    // reversible work is an ask its standing go-ahead answers ([`go_ahead_answer`]).
    let park = state.go_ahead_owed(reading, now);
    let gate_ask = match park {
        Some(_) => Some(GO_AHEAD_WHY.to_string()),
        None => asked.clone(),
    };
    if let Some(act) = words_gate(cfg, gate_ask.as_deref(), done) {
        return act;
    }
    // A NEED ALONE AT THE LAND CHECK ([`land_check::CheckAnswer::Needs`]): the agent waits only
    // on the person, so nothing is typed (it would be a prompt it cannot act on). A park on the
    // person's go-ahead for the agent's own reversible work is no such need: the standing
    // go-ahead answers it (owner ruling 2026-10-29), so it is typed, after its own wait, below.
    if land && state.land.rest && park.is_none() {
        return TurnEndAction::Nothing;
    }
    // THE LAND CHECK IS ONE STEP OF THE LANDING, typed at its defense's point, never after the
    // back-off.
    let check_owed = land && state.land.check_owed;
    let waited_on =
        state.managed && (done || gate_ask.is_some()) && state.short <= MANAGED_SHORT_TURNS;
    // LAND'S INSTRUCTION WAITS NO BACK-OFF (v6 §14.10, slice 10a): where the act would be the
    // instruction itself ([`land_words`] untyped, a done claim, no question, or a park the
    // go-ahead answers once the instruction is typed), a switch to Land acts at once; its answers
    // and nudges keep the ladder, and Cruise's pacing does not change.
    let parked_first = reading.autopilot == Autopilot::Land
        && !state.land.typed
        && state.go_ahead_parked(reading, now).is_some();
    // The person's switch over a held tab asks once, at once (v6 §14.17 item 3).
    let instruction_due = state.switch_asks(reading)
        || (reading.autopilot == Autopilot::Land
            && !state.land.typed
            && (done || asked.is_none() || parked_first));
    // A park's wait runs from the last act typed, at most [`GO_AHEAD_WAIT`]: the agent's own
    // wakes, each a new point, restart no wait the answer owes.
    let (wait, from) = match park {
        Some(_) => (
            state.short_wait().map(|w| w.min(GO_AHEAD_WAIT)),
            state.acts.back().copied().or(state.point_at),
        ),
        None => (state.short_wait(), state.point_at),
    };
    if !waited_on
        && !instruction_due
        && !check_owed
        && let (Some(wait), Some(at)) = (wait, from)
    {
        let due = later(at, wait);
        // A BACK-OFF NEVER OUTLASTS THE PERSON'S HOUR (2026-10-30 review 9): within it a park is
        // owed nothing and backs off as a plain short turn, from its own point, up to the
        // ladder's top; where that runs past the hour's end, the point is decided again there,
        // where the park's own wait (at most [`GO_AHEAD_WAIT`] from the last act) decides.
        if let Some(ends) = state.go_ahead_hour_ends(reading, now)
            && due > ends
        {
            return TurnEndAction::WaitUntil {
                until: ends,
                why: GO_AHEAD_HOUR_WHY.to_string(),
                wait: WaitKind::Other,
            };
        }
        if now < due {
            let secs = wait.as_secs();
            return TurnEndAction::WaitUntil {
                until: due,
                why: if secs < 120 {
                    format!("{} short turn(s) in a row: {secs} s back-off", state.short)
                } else {
                    format!(
                        "{} short turn(s) in a row: {} min back-off",
                        state.short,
                        secs / 60
                    )
                },
                wait: WaitKind::BackOff {
                    short: u8::try_from(state.short).unwrap_or(u8::MAX),
                },
            };
        }
    }
    if let Some(hold) = outage_hold(state, reading, now) {
        return hold;
    }
    // The go-ahead outranks the done check and Land's own acts ([`words_act`]): the work it
    // names is the work they would ask for.
    if let Some(park) = &park {
        return budgeted(TurnEndAction::Type {
            text: go_ahead_answer(
                park,
                state.needs_ack_owed_past(&park.needs),
                reading.rules.as_deref(),
            ),
            rule_id: RULE_GO_AHEAD,
        });
    }
    // Land's instruction over the park the go-ahead answers once it is typed answers that park:
    // its ack owes none of the park's lines, which are off the menu bar (§15.6).
    if let Some(first) = state.go_ahead_first(reading, cfg, now) {
        let (text, rule_id) = land_words(state, reading, state.needs_ack_owed_past(&first.needs));
        return budgeted(TurnEndAction::Type { text, rule_id });
    }
    if let Some(act) = words_act(
        state,
        reading,
        cfg,
        Words {
            asked: asked.is_some() && !parked_first,
            irreversible,
            done,
            held: state.go_ahead_held_at(reading, now),
        },
        budgeted,
    ) {
        return act;
    }
    // CRUISE'S WORDS, never the vendor's: a grey suggestion in the composer (Claude Code's `keep
    // going`) is typed over with the whole continuation, which carries the hunt and NEEDS YOU's
    // rule; it is no longer accepted bare (v6 §14.5; [`RULE_SUGGESTION`] stays for older books).
    // Land never comes here ([`words_act`]), nor does Off ([`decide_unridden`]). The person's key
    // holds the go-ahead, and the continuation's word that it is no need
    // ([`go_ahead::unless_held`]).
    let ack = state.needs_ack_owed();
    budgeted(TurnEndAction::Type {
        text: act_words(
            &go_ahead::unless_held(&cfg.continue_text, state.go_ahead_held_at(reading, now)),
            &[
                reversible_clause(ack && state.needs_destroy()),
                ack_clause(ack),
            ],
            reading.rules.as_deref(),
        ),
        rule_id: RULE_CONTINUE,
    })
}

/// Why an act waits on a measured outage at a point no wall stands at ([`outage_hold`]).
pub const UNREACHABLE_HOLD: &str = "the API is unreachable; continuing when it answers";

/// A MEASURED OUTAGE HOLDS THE ACT A WALL-LESS POINT WOULD TYPE: at a point no wall stands at,
/// where the host measured the API's reach Down ([`Reach::Down`], which the loop reads at such a
/// point only when its peek says Down: `IdleHost::reach_peek`), the act [`continue_policy`] would
/// type (Cruise's continuation, its defend prompt or answer; Land's instruction, nudge or answer)
/// waits as an outage ([`WaitKind::Outage`], a connection error), after any short back-off it
/// owes: until the reach changes, whose edge the loop decides the point again at ([`Reach::Up`]
/// or Unknown types the act), or until [`TurnEndTiming::outage_cap`] has passed since the outage
/// began ([`TurnEndTiming::outage_holds_until`], which says why the cap runs once per outage).
/// Nothing network-specific is typed: no wall was read. Only these acts wait here: a wall's own
/// arm, and the arms the decision takes ahead of this policy (an owed or compact continuation, a
/// limit's, account's or login's resume, a restart), keep their own rules. `None` when the reach
/// is not Down or the cap has passed.
fn outage_hold(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
) -> Option<TurnEndAction> {
    let until = state.timing.outage_holds_until(reading.reach, now)?;
    Some(TurnEndAction::WaitUntil {
        until,
        why: UNREACHABLE_HOLD.to_string(),
        wait: WaitKind::Outage {
            wall: Outage::Connection,
            tries: true,
        },
    })
}

fn words_gate(cfg: &SupervisorConfig, asked: Option<&str>, done: bool) -> Option<TurnEndAction> {
    match asked {
        Some(why) if !cfg.answer_questions => Some(TurnEndAction::Escalate {
            reason: format!("{why}; {ANSWERS_OFF}"),
        }),
        _ if done && !cfg.continue_policy => Some(TurnEndAction::Nothing),
        None if !cfg.continue_policy => Some(TurnEndAction::Nothing),
        _ => None,
    }
}

/// What [`words_act`] answers: whether the reply asks, names an irreversible act or claims done,
/// and whether the person's hour holds the go-ahead ([`TurnEndState::go_ahead_held_at`]).
#[derive(Clone, Copy)]
struct Words {
    asked: bool,
    irreversible: bool,
    done: bool,
    held: bool,
}

fn words_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    words: Words,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> Option<TurnEndAction> {
    let Words {
        asked,
        irreversible,
        done,
        held,
    } = words;
    if reading.autopilot == Autopilot::Land {
        return Some(land_policy(state, reading, asked, done, budgeted));
    }
    let rules = reading.rules.as_deref();
    let ack = state.needs_ack_owed();
    let ack_s = ack_clause(ack);
    let need_destroys = ack && state.needs_destroy();
    // The done check says the go-ahead is no need as the continuation does, and the person's key
    // cuts it within their hour as it cuts the continuation's ([`go_ahead::unless_held`]); the
    // repository's ask outranks it for room ([`fitted_check`]).
    if done {
        return Some(budgeted(TurnEndAction::Type {
            text: state.done_check_at(irreversible, held, rules),
            rule_id: RULE_DONE_CHECK,
        }));
    }
    if !asked {
        return None;
    }
    Some(budgeted(if irreversible {
        TurnEndAction::Type {
            text: act_words(REVERSIBLE_ANSWER, &[NEEDS_HINT, ack_s], rules),
            rule_id: RULE_ANSWER,
        }
    } else {
        TurnEndAction::Type {
            text: act_words(
                prose_words(cfg),
                &[NEEDS_HINT, reversible_clause(need_destroys), ack_s],
                rules,
            ),
            rule_id: cfg.question_priority.prose_rule_id(),
        }
    }))
}

fn land_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    asked: bool,
    done: bool,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    let (text, rule_id) = if done || !asked || state.switch_asks(reading) || state.land.check_owed {
        land_words(state, reading, state.needs_ack_owed())
    } else {
        let ack_s = ack_clause(state.needs_ack_owed());
        (
            act_words(LAND_ANSWER, &[ack_s], reading.rules.as_deref()),
            RULE_LAND_ANSWER,
        )
    };
    budgeted(TurnEndAction::Type { text, rule_id })
}

/// Land's instruction, or its nudge once that was typed, owing the needs' ack where `ack`.
fn land_words(state: &TurnEndState, reading: &TurnEndReading, ack: bool) -> (String, &'static str) {
    let rules = reading.rules.as_deref();
    let ack_s = ack_clause(ack);
    let rev = reversible_clause(ack && state.needs_destroy());
    if state.switch_asks(reading) {
        let quote = held_quote(reading.said_tail.as_deref().unwrap_or_default());
        return (switch_act(&quote, &[rev, ack_s], rules), RULE_LAND);
    }
    if state.land.check_owed {
        return (
            land_check::land_check_text(state.land_hint.as_deref(), ack, rules),
            RULE_LAND_CHECK,
        );
    }
    if state.land.typed {
        if let Some(text) = contradicted(state, reading, &[rev, ack_s], rules) {
            return (text, RULE_LAND_NUDGE);
        }
        (
            fitted_act(
                LAND_NUDGE,
                &[
                    Suffix::Listed(state.land_listed.as_deref()),
                    Suffix::Repo(&state.repo, RepoAsk::All),
                    Suffix::Left(state.left_said, RepoAsk::All),
                    Suffix::Fixed(rev),
                    Suffix::Fixed(ack_s),
                ],
                rules,
            ),
            RULE_LAND_NUDGE,
        )
    } else {
        (act_words(LAND, &[rev, ack_s], rules), RULE_LAND)
    }
}

/// LAND'S CONTRADICTION AT THIS POINT ([`land_contradicted`]): where the reply defends the work
/// with a `Nothing left:` line ([`needs::defense_line`]) while git found work no remote main has,
/// or while the agent's own list stands ([`TurnEndState::land_listed`]). The list the land
/// check's answer has just given is no contradiction of that answer, whatever its words: only a
/// defense after it, the list still standing, contradicts it.
fn contradicted(
    state: &TurnEndState,
    reading: &TurnEndReading,
    after: &[&str],
    rules: Option<&str>,
) -> Option<String> {
    reading.said_tail.as_deref().and_then(needs::defense_line)?;
    let listed = state
        .land_listed
        .as_deref()
        .filter(|_| state.repo.found().is_some() || !state.land.answered);
    land_contradicted_left(&state.repo, listed, state.left_said, after, rules)
}

/// Whether `text` opens with one of Land's acts as this build types them, the switch's why before
/// Land's instruction (v6 §14.17 item 3) read past.
#[must_use]
pub fn is_land_words(text: &str) -> bool {
    let text = past_switch_why(text);
    text.starts_with(LAND)
        || text.starts_with(LAND_NUDGE)
        || text.starts_with(LAND_CONTRADICTED_HEAD)
        || text.starts_with(LAND_ANSWER)
        || text.starts_with(LAND_CHECK_HEAD)
}

fn goes_on_by_itself(
    state: &TurnEndState,
    track: Option<&WallTrack>,
    reading: &TurnEndReading,
) -> bool {
    reading.resumes_by_itself
        && !track
            .and_then(|t| t.named)
            .is_some_and(|n| state.named_countdown_gone(&n))
}

pub(crate) const fn entitlement_ladder(placed: bool, kind: WallKind) -> bool {
    !placed && matches!(kind, WallKind::Spend | WallKind::ModelBucket { .. })
}

fn resume_at_reset(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
    since: Instant,
    attempts: usize,
    kind: WallKind,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
) -> TurnEndAction {
    // A wall no reset ends and none names is an entitlement: its own, slower
    // ladder. A usage window with no readable reset still lifts by itself, so it
    // keeps the short one.
    if entitlement_ladder(reading.reset_at.is_some(), kind) {
        let due = crate::supervise::ladder::Ladder(&state.timing.entitlement_backoff)
            .at(attempts)
            .map(|w| later(since, w));
        return match due {
            Some(due) if now < due => TurnEndAction::WaitUntil {
                until: due,
                why: format!("{} (no reset named)", kind.name()),
                wait: WaitKind::Other,
            },
            Some(_) => continue_as(RULE_LIMIT_RESUME, Clearing::Blind),
            None => TurnEndAction::Nothing,
        };
    }
    let back = &state.timing.limit_backoff;
    let backoff = |i: usize| crate::supervise::ladder::Ladder(back).at(i);
    let at_reset = reading
        .reset_at
        .map(|r| later(r, state.timing.grace_for(reading.program) + reading.jitter));
    // A new reset after the meet is waited out with its own grace and jitter. An old or
    // absent reset still needs the retry floor, as does no inferred fresh reset.
    let named_past_meet = reading.reset_at.is_some_and(|r| r > since);
    let after_retry = if attempts == 0 || named_past_meet {
        None
    } else {
        backoff(attempts - 1).map(|b| later(since, b))
    };
    let due = match (at_reset, after_retry) {
        (Some(r), Some(b)) => r.max(b),
        (Some(r), None) => r,
        (None, Some(b)) => b,
        (None, None) => match back.last() {
            Some(&b) => later(since, b),
            None => return TurnEndAction::Nothing,
        },
    };
    if now < due {
        return TurnEndAction::WaitUntil {
            until: due,
            why: format!("{} resets", kind.name()),
            wait: WaitKind::Other,
        };
    }
    let clearing = if at_reset.is_some() {
        Clearing::Measured
    } else {
        Clearing::Blind
    };
    continue_as(RULE_LIMIT_RESUME, clearing)
}

fn still_shown(t: &WallTrack, reading: &TurnEndReading) -> Option<(WallKind, TurnEndReading)> {
    if t.class == WallClass::Auth {
        return left_undecided_login(t, reading).then(|| {
            (
                WallKind::Auth,
                TurnEndReading {
                    wall: Some(WallKind::Auth),
                    wall_message: t.said.clone(),
                    ..reading.clone()
                },
            )
        });
    }
    let n = left_named(t, reading)?;
    Some((
        n.kind,
        TurnEndReading {
            wall: Some(n.kind),
            wall_message: t.said.clone(),
            reset_at: Some(n.at),
            jitter: n.jitter,
            resumes_by_itself: n.by_itself,
            ..reading.clone()
        },
    ))
}

fn left_retry(t: &WallTrack, reading: &TurnEndReading) -> Option<(WallKind, TurnEndReading)> {
    let kind = left_retry_kind(t)?;
    Some((
        kind,
        TurnEndReading {
            wall: Some(kind),
            wall_message: t.said.clone(),
            ..reading.clone()
        },
    ))
}

fn left_retry_kind(t: &WallTrack) -> Option<WallKind> {
    t.kind.filter(|k| {
        t.class == WallClass::Retry
            && !matches!(
                k,
                WallKind::ApiError {
                    cause: ApiCause::Request,
                    ..
                }
            )
    })
}

fn left_named(t: &WallTrack, reading: &TurnEndReading) -> Option<NamedReset> {
    t.named
        .filter(|_| named_class(t.class) && !reading.login_back)
}

fn left_undecided_login(t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth && !t.pointed && !reading.login_back
}

fn left_waited_login(state: &TurnEndState, t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth
        && (t.attempts == 0
            || (reading.login_label.is_some()
                && reading.login_screen.is_none()
                && state.login_typed_left()))
        && (state.account.is_some() || reading.login_label.is_some())
        && !reading.login_back
}

pub(super) fn waited_login(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndReading> {
    let t = state.wall.as_ref()?;
    if reading.wall.is_some() || !left_waited_login(state, t, reading) {
        return None;
    }
    let waited = TurnEndReading {
        wall: Some(WallKind::Auth),
        wall_message: t.said.clone(),
        ..reading.clone()
    };
    login_wall_waits(state, &waited, cfg, now).map(|_| waited)
}

fn wall_breaker(state: &TurnEndState, now: Instant, act: TurnEndAction) -> TurnEndAction {
    if let Some(rule) = act.rule_id().filter(|r| is_wall_act(r))
        && let (n, Some(free)) = state.wall_acts_of(rule, now)
        && n >= WALL_ACTS_PER_WINDOW
    {
        return TurnEndAction::WaitUntil {
            until: free,
            why: format!(
                "a breaker: {n} acts of {rule} in the last {} min",
                state.timing.window.as_secs() / 60
            ),
            wait: WaitKind::Other,
        };
    }
    act
}

fn reset_continuation(
    state: &TurnEndState,
    cfg: &SupervisorConfig,
    now: Instant,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
) -> Option<TurnEndAction> {
    let owed = state.reset_resume.filter(|_| cfg.resume_limits)?;
    Some(if now < owed.due {
        TurnEndAction::WaitUntil {
            until: owed.due,
            why: "the limit reset's own continuation".to_string(),
            wait: WaitKind::Other,
        }
    } else {
        resume_limit(continue_as)
    })
}

fn limit_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> Option<TurnEndAction> {
    let said = reading.said_since_limit()?;
    let done = says_done(Some(said));
    let (asked, irreversible) = match asked_by(Some(said)) {
        Some((why, irreversible)) => (Some(why), irreversible),
        None => (None, false),
    };
    if !done && asked.is_none() && !state.limit_elsewhere(reading) {
        return None;
    }
    if let Some(act) = words_gate(cfg, asked.as_deref(), done) {
        return Some(act);
    }
    words_act(
        state,
        reading,
        cfg,
        Words {
            asked: asked.is_some(),
            irreversible,
            done,
            held: state.go_ahead_held_at(reading, now),
        },
        budgeted,
    )
}

pub(crate) const LIMIT_CONVERSATION_LOOK: Duration = Duration::from_secs(5 * 60);

fn unread_track<'a>(state: &'a TurnEndState, reading: &TurnEndReading) -> Option<&'a WallTrack> {
    state.wall.as_ref().filter(|t| {
        named_class(t.class)
            && (reading.wall.is_none() || reading.wall_remembered)
            && !limit_conversation_known(t, reading)
            && !state.relaunched_off(t)
    })
}

impl TurnEndState {
    #[must_use]
    pub(crate) fn held_unread_from(&self, reading: &TurnEndReading) -> Option<Instant> {
        unread_track(self, reading).map(|t| t.named.map_or(t.since, |n| n.at))
    }
}

fn limit_conversation_ours(t: &WallTrack, reading: &TurnEndReading) -> bool {
    limit_conversation_known(t, reading) && t.conversation == reading.conversation
}

/// A REPLY THAT SAYS ITS AGENT IS BLOCKED ON A PERSON ([`TurnEndState::observe`]): one with a
/// `NEEDS YOU:` line that stands, the turn's last word not retracting it
/// ([`TurnEndReading::names_need`]), read literally (never a word reader for "blocked"), at no
/// wall. A Land
/// defense (`Nothing left:`) is the landing decision's, never this, nor is a park on a person's
/// go-ahead for the agent's own reversible work ([`TurnEndReading::go_ahead`]): the owner's
/// standing go-ahead answers that one, its own count telling the person after its acts (§15.6).
fn blocked_reply(reading: &TurnEndReading) -> bool {
    let landing = reading.autopilot == Autopilot::Land
        && reading
            .said_tail
            .as_deref()
            .is_some_and(|tail| needs::defense_line(tail).is_some());
    reading.wall.is_none() && !landing && reading.go_ahead().is_none() && reading.names_need()
}

const fn limit_conversation_known(t: &WallTrack, reading: &TurnEndReading) -> bool {
    matches!(
        (&t.conversation, &reading.conversation),
        (Conversation::Hostless, Conversation::Hostless)
            | (Conversation::Named(_), Conversation::Named(_))
    )
}

/// The limit's resume where its clearing was measured: its reset, the accounts probe, a login or an
/// account move ([`Clearing::Measured`]).
fn resume_limit(continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction) -> TurnEndAction {
    continue_as(RULE_LIMIT_RESUME, Clearing::Measured)
}

fn retry_wait(state: &TurnEndState, attempts: usize) -> Option<Duration> {
    crate::supervise::ladder::Ladder(&state.timing.retry_backoff).at(attempts)
}

const fn moves_accounts(program: Program) -> bool {
    match program {
        Program::Claude | Program::Kimi => Some(()),
        _ => None,
    }
    .is_some()
}

fn account_rotates(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.accounts != crate::harness::accounts::Mode::Off && relaunch_askable(state, reading, cfg)
}

/// WHEN THE ACT A USAGE LIMIT OWES COMES DUE UNDER A `/login` DIALOG STANDING OVER IT
/// ([`super::login::over_limit_act`]), the tab carried and its work not resting: at once where the
/// roster advises a move onto another account that can serve and the move reaches the tab
/// ([`account_first`]), or where it measured the limit cleared and the resume was not typed since;
/// otherwise at the limit's named reset ([`TurnEndReading::reset_at`] for a usage or model limit
/// the screen draws, else the one this policy waits on, [`TurnEndState::waited_limit`]) and the
/// grace its resume waits past it ([`TurnEndTiming::grace_for`] and the reading's jitter), the
/// moment the composer is first owed. What follows the dialog's close is the wall's own act: a
/// wall that goes on by itself still has Claude Code's continue its grace to fire
/// ([`TurnEndTiming::auto_resume_grace`]). `None`: no limit's act is owed under the dialog (no
/// named limit, `resume_limits` off, Autopilot off, a fresh tab, a done task outside Cruise).
#[must_use]
pub(super) fn login_over_limit_due(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<Instant> {
    let rests = state.task_done() && reading.autopilot != Autopilot::Cruise;
    if reading.autopilot == Autopilot::Off
        || !super::login::carried(reading.program)
        || !cfg.resume_limits
        || reading.taskless
        || rests
    {
        return None;
    }
    let drawn = reading
        .wall
        .filter(|&kind| named_class(class_of(kind)))
        .and(reading.reset_at);
    let reset = drawn.or_else(|| state.waited_limit().map(|limit| limit.at));
    let rotates = reset.is_some() && account_rotates(state, reading, cfg);
    match reading.advised() {
        Some(AccountAdvice::Move { .. })
            if rotates && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW =>
        {
            return Some(now);
        }
        Some(AccountAdvice::Resume { cleared_at })
            if reset.is_some()
                && state
                    .account_resumed
                    .is_none_or(|typed| typed < *cleared_at) =>
        {
            return Some(now);
        }
        _ => {}
    }
    let grace = state
        .timing
        .grace_for(reading.program)
        .saturating_add(reading.jitter);
    reset.map(|at| later(at, grace))
}

fn login_follows(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.logins && reading.login_label.is_some() && relaunch_askable(state, reading, cfg)
}

const fn relaunch_askable(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> bool {
    cfg.resume_limits
        && relaunch_on(reading, cfg)
        && reading.restartable
        && moves_accounts(reading.program)
        && state.account_unmade.is_none()
        && !reading.composer.drafted()
}

pub(super) fn relaunch_reaches(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.resume_limits && relaunch_on(reading, cfg) && relaunch_lane(reading)
}

pub(super) const fn relaunch_lane(reading: &TurnEndReading) -> bool {
    moves_accounts(reading.program)
        && (reading.restartable || reading.relaunch_off || reading.upgrading)
}

pub(super) const fn relaunch_on(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.relaunch && !reading.relaunch_off
}

fn renewed_draft_reason(composer: &Composer) -> String {
    let what = match composer {
        Composer::Own => "aterm's own words stand unsent in the prompt: clear them",
        Composer::Diverted(Diversion::Menu) => {
            "a draft stands with Claude Code's completion menu open: close the menu, then send \
             or clear the draft"
        }
        Composer::Diverted(Diversion::Search) => {
            "the prompt-history search is open over the composer: close it"
        }
        Composer::Diverted(Diversion::Recall) => {
            "an old prompt the prompt-history search found stands in the composer: send, edit or \
             clear it"
        }
        Composer::Command(..) => {
            "a command stands unsent in the composer, which aterm never sends for you: send or \
             clear it"
        }
        _ => "a draft stands in the composer: send or clear it",
    };
    format!("the login was renewed; {what}, and the tab moves onto the new login")
}

/// WHETHER ONLY A DRAFT HOLDS A RENEWED ROSTER LOGIN'S MOVE: the session's row was signed in again
/// (`login_superseded`), the relaunch onto it reaches the tab, and the composer holds words a
/// relaunch would lose, or something stands over it ([`Composer::drafted`]). With the composer empty the relaunch follows by itself where the host
/// advises it, and advising nothing, the renewed login ended too; where no relaunch reaches the
/// tab, the tab waits on the row's own sign-in, naming what holds the move ([`login_wall_waits`]).
pub(super) fn renewed_draft_holds(reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    reading.login_label.is_some()
        && cfg.logins
        && reading.login_superseded
        && relaunch_reaches(reading, cfg)
        && reading.composer.drafted()
}

fn login_move(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    to: &str,
) -> bool {
    reading.login_label.as_deref() == Some(to) && login_follows(state, reading, cfg)
}

fn account_first(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    attempts: usize,
    today: TurnEndAction,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
) -> TurnEndAction {
    let Some(advice) = reading.advised() else {
        return today;
    };
    let today = wall_breaker(state, now, today);
    let waits = matches!(
        today,
        TurnEndAction::WaitUntil { .. } | TurnEndAction::Restart { .. }
    ) || (attempts > 0 && today.rule_id().is_some());
    if !waits || !account_rotates(state, reading, cfg) {
        return today;
    }
    match (advice, today) {
        (AccountAdvice::Move { to, .. }, today) => {
            let (n, free) = state.account_moves_of(now);
            if n < ACCOUNT_MOVES_PER_WINDOW {
                return TurnEndAction::Restart {
                    why: Restart::Account {
                        to: to.clone(),
                        at: AccountAt::Wall,
                    },
                    rule_id: RULE_ACCOUNT,
                    otherwise: Box::new(today),
                };
            }
            match (today, free) {
                (TurnEndAction::WaitUntil { until, .. }, Some(free)) if free < until => {
                    TurnEndAction::WaitUntil {
                        until: free,
                        why: format!(
                            "a breaker: {n} account moves in the last {} min",
                            state.timing.window.as_secs() / 60
                        ),
                        wait: WaitKind::Other,
                    }
                }
                (today, _) => today,
            }
        }
        (
            AccountAdvice::Wait { until } | AccountAdvice::Again { until, .. },
            TurnEndAction::WaitUntil {
                until: due,
                why,
                wait,
            },
        ) if *until > now => {
            let at = (*until).max(later(now, ACCOUNT_WAIT_FLOOR));
            let own_reset = reading.reset_at.is_some_and(|reset| at >= reset);
            if at < due && !own_reset {
                let said = match advice {
                    AccountAdvice::Again { why, .. } => why.why(),
                    _ => "another account can be used then",
                };
                TurnEndAction::WaitUntil {
                    until: at,
                    why: said.to_string(),
                    wait: WaitKind::Other,
                }
            } else {
                TurnEndAction::WaitUntil {
                    until: due,
                    why,
                    wait,
                }
            }
        }
        (
            AccountAdvice::Wait { .. } | AccountAdvice::Again { .. } | AccountAdvice::Hold { .. },
            today,
        ) => today,
        (AccountAdvice::Resume { cleared_at }, today) => {
            if state
                .account_resumed
                .is_some_and(|typed| typed >= *cleared_at)
            {
                today
            } else {
                resume_limit(continue_as)
            }
        }
        (AccountAdvice::Probe { .. }, today) => today,
    }
}

pub(super) fn login_wall_moves<'a>(
    state: &TurnEndState,
    reading: &'a TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<&'a str> {
    let Some(AccountAdvice::Move { to, .. }) = reading.advised() else {
        return None;
    };
    let moved = state.account.is_some() && account_rotates(state, reading, cfg);
    let untyped = reading.wall == Some(WallKind::Auth)
        && state.wall_attempts(WallKind::Auth).unwrap_or(0) == 0;
    let back = state.login_track().is_some() && login_back_elsewhere(state, reading);
    let left =
        reading.login_label.is_some() && reading.login_screen.is_none() && state.login_typed_left();
    ((untyped || back || left || login_left_renewed(state, reading))
        && (moved || login_move(state, reading, cfg, to))
        && !state.login_left(reading)
        && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW)
        .then_some(to.as_str())
}

fn login_left_renewed(state: &TurnEndState, reading: &TurnEndReading) -> bool {
    state.login_track().is_some()
        && reading.login_interrupted
        && reading.login_screen.is_none()
        && reading.login_superseded
}

/// THE WAIT AT A LOST LOGIN THAT ANOTHER LOGIN ENDS, in place of the `/login` the wall would
/// have typed: a move the roster made (`moved`), the renewal of the session's own roster row, and
/// that row's own sign-in to come (`signs_in`: a row with an email, which aterm's sign-in ladder,
/// `crate::harness::sign_in_ladder`, signs in again by itself, headless, after which the tab moves
/// onto the login it made). `/login` in the tab would open a browser sign-in on the desk that only
/// a person can finish, so a roster row's lost login never types it: where the relaunch reaches
/// the tab the wait is the move's, and where nothing moves the tab yet (no relaunch reaches it, a
/// restart onto the row was not made, or a draft stands before the row is renewed) the wait is
/// the row's own sign-in's, its reason naming what holds the move ([`move_held`]) — the record
/// that stands where no route can finish. The same wait stands at a sign-in menu drawn in place
/// of the composer ([`sign_in_menu_waits`]). A renewed row's draft is [`renewed_draft_holds`]'s; a
/// login back in another tab, no renewal behind it, is continued from as that tab's, and a
/// person's own `/login` left at the wall is theirs (`TurnEndState::login_cancelled`), told as
/// before. A `/login` aterm typed before the roster named the row is no such ask: once its dialog
/// is left (aterm backs out of it, `super::login::backs_out`, or a person's Esc closes it,
/// [`TurnEndState::login_typed_left`]) the tab waits on the row's sign-in as if none was typed.
pub(super) fn login_wall_waits(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndAction> {
    let moved = state.account.is_some() && cfg.accounts != crate::harness::accounts::Mode::Off;
    let signs_in = reading.login_label.is_some()
        && cfg.logins
        && (reading.login_superseded || !login_back_elsewhere(state, reading));
    let typed = state.wall_attempts(WallKind::Auth).unwrap_or(0) > 0;
    if reading.wall != Some(WallKind::Auth)
        || !(moved || signs_in)
        || state.login_left(reading)
        || if typed {
            !(signs_in && state.login_typed_left())
        } else {
            state.login_cancelled()
        }
    {
        return None;
    }
    let backoff = later(
        now,
        state
            .timing
            .restart_backoff
            .last()
            .copied()
            .unwrap_or(ACCOUNT_WAIT_FLOOR),
    );
    let wait = |until: Instant, why: String| TurnEndAction::WaitUntil {
        until: until.max(later(now, ACCOUNT_WAIT_FLOOR)),
        why: format!("the login is gone: {why}"),
        wait: WaitKind::Other,
    };
    // A row whose account's vault token is in use here is never signed in again (its renewal is
    // off): the tab moves onto the token instead ([`wall_action`]'s route move), so that is what
    // it waits on.
    let comes_back = |label: &str| {
        if reading.route == Some(RouteTo::Token) {
            format!("the tab moves onto {label}'s vault token, in use here")
        } else {
            format!("aterm signs {label} in again by itself")
        }
    };
    let held = move_held(state, reading, cfg);
    if held.is_some() || reading.composer.drafted() {
        let label = reading.login_label.as_deref().filter(|_| signs_in)?;
        return match (reading.login_superseded, held) {
            (true, None) => None,
            (true, Some(held)) => Some(wait(
                backoff,
                format!("{label} is signed in again, and the tab cannot move onto it: {held}"),
            )),
            (false, Some(held)) if reading.route == Some(RouteTo::Token) => Some(wait(
                backoff,
                format!(
                    "{label}'s vault token is in use here, and the tab cannot move onto it: {held}"
                ),
            )),
            (false, Some(held)) => Some(wait(
                backoff,
                format!(
                    "aterm signs {label} in again by itself, and the tab cannot move onto it: \
                     {held}"
                ),
            )),
            (false, None) => Some(wait(backoff, comes_back(label))),
        };
    }
    let (n, free) = state.account_moves_of(now);
    let (until, why) = match (reading.advised(), free) {
        (Some(AccountAdvice::Again { until, why }), _) => (*until, why.why().to_string()),
        (Some(AccountAdvice::Wait { until }), _) if reading.login_superseded => {
            (*until, "its renewed login can be used then".to_string())
        }
        (Some(AccountAdvice::Wait { until }), _) => {
            (*until, "another account can be used then".to_string())
        }
        (Some(AccountAdvice::Move { .. }), Some(free)) if n >= ACCOUNT_MOVES_PER_WINDOW => (
            free,
            format!(
                "a breaker: {n} account moves in the last {} min",
                state.timing.window.as_secs() / 60
            ),
        ),
        (None, _) if !signs_in && state.account_point(reading, cfg).is_some() => return None,
        _ => (
            backoff,
            match reading
                .login_label
                .as_deref()
                .filter(|_| signs_in && !moved)
            {
                Some(label) => comes_back(label),
                None => "no move can be made yet".to_string(),
            },
        ),
    };
    Some(wait(until, why))
}

/// A ROSTER ROW'S LOST LOGIN AT A SIGN-IN MENU ([`TurnEndReading::at_sign_in_menu`]): the row is
/// aterm's sign-in ladder's to sign in, as at the composer's wall, so the menu asks no person. The
/// tab waits on the row's own sign-in ([`login_wall_waits`], its reason naming what holds the
/// move) and is relaunched onto the login the renewal made ([`login_wall_moves`]), a person's
/// keys at the menu holding the relaunch ([`person_hold`]); a tab with no task, or one whose task
/// is done (`idle`), is left alone, as at the composer's wall. Either way the host is told the
/// row's login ended ([`TurnEndState::account_point`]); a menu drawn after a move is the moved-to
/// login's own, told as the move's answer ([`TurnEndState::account_answer`]), never the wall the
/// session left ([`TurnEndState::login_left`]). `None`, and the menu is escalated with how the
/// agent signs in, where the login is no roster row's, `logins` is off, the agent moves no
/// accounts (agy's and Muse Code's menus), or, for a tab with a task, [`login_wall_waits`] waits
/// on no sign-in there (a person's own `/login` left, one typed, the login back in another tab
/// with no renewal behind it).
fn sign_in_menu_waits(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    idle: bool,
) -> Option<TurnEndAction> {
    if !(cfg.logins && reading.login_label.is_some() && moves_accounts(reading.program)) {
        return None;
    }
    if idle {
        return Some(TurnEndAction::Nothing);
    }
    let wait = login_wall_waits(state, reading, cfg, now)?;
    if let Some((until, why)) = person_hold(state, reading, cfg, now) {
        return Some(TurnEndAction::WaitUntil {
            until,
            why: why.to_string(),
            wait: WaitKind::Other,
        });
    }
    Some(match login_wall_moves(state, reading, cfg, now) {
        Some(to) => TurnEndAction::Restart {
            why: Restart::Account {
                to: to.to_string(),
                at: AccountAt::Wall,
            },
            rule_id: RULE_ACCOUNT,
            otherwise: Box::new(wait),
        },
        None => wait,
    })
}

/// WHAT HOLDS A TAB'S MOVE ONTO ITS ROSTER ROW'S LOGIN, where something does: `relaunch` or
/// `resume_limits` off, no host that relaunches this agent, or a restart onto an account this
/// host asked for and did not make (asked again at the accounts' next news,
/// [`TurnEndState::account_news`]).
fn move_held(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> Option<&'static str> {
    if !relaunch_on(reading, cfg) {
        Some(MOVE_HELD_WHYS[0])
    } else if !cfg.resume_limits {
        Some(MOVE_HELD_WHYS[1])
    } else if !relaunch_lane(reading) {
        Some(MOVE_HELD_WHYS[2])
    } else if state.account_unmade.is_some() {
        Some(MOVE_HELD_WHYS[3])
    } else {
        None
    }
}

/// Every clause [`move_held`] names.
pub(crate) const MOVE_HELD_WHYS: [&str; 4] = [
    "relaunch is off in aterm.toml",
    "resume_limits is off in aterm.toml",
    "aterm cannot restart this Claude Code",
    "the restart aterm asked for here was not made",
];

#[allow(clippy::too_many_arguments)]
fn wall_action(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    let today = wall_own_act(
        state,
        reading,
        cfg,
        now,
        kind,
        budgeted,
        continue_as,
        type_as,
    );
    match kind {
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. }
            if (state.account_resume || state.login_resume) && cfg.resume_limits =>
        {
            resume_limit(continue_as)
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. } => {
            // The notice a limit reset left on the screen: the reset's own
            // continuation is this wall's ([`TurnEndState::reset_told`]) —
            // after a reset that went through the notice is stale, and after
            // one kept or failed the continuation is the wall's one try
            // before it is moved from or waited out.
            if let Some(act) = reset_continuation(state, cfg, now, continue_as) {
                return act;
            }
            let attempts = state.wall_attempts(kind).unwrap_or(0);
            account_first(state, reading, cfg, now, attempts, today, continue_as)
        }
        WallKind::Auth if state.login_left(reading) => resume_limit(continue_as),
        WallKind::Auth => {
            let today = login_wall_waits(state, reading, cfg, now).unwrap_or(today);
            match login_wall_moves(state, reading, cfg, now) {
                Some(to) => TurnEndAction::Restart {
                    why: Restart::Account {
                        to: to.to_string(),
                        at: AccountAt::Wall,
                    },
                    rule_id: RULE_ACCOUNT,
                    otherwise: Box::new(wall_breaker(state, now, today)),
                },
                // The route the host offers (§5.5, §5.6): a refused vault token's way back onto
                // the row's stored login, where it has one here, and a stored session's way onto
                // its account's token in use here, which carries it past the login's end that
                // drew this wall (no renewal signs a token row in again).
                None => match route_ahead(state, reading, now) {
                    Some(to) => route_to(to, wall_breaker(state, now, today)),
                    None => today,
                },
            }
        }
        WallKind::Spend
        | WallKind::Overloaded
        | WallKind::ApiError { .. }
        | WallKind::Context
        | WallKind::Memory => today,
    }
}

fn not_retried(program: Program, kind: WallKind) -> Option<&'static str> {
    match program {
        Program::Agy => matches!(
            kind,
            WallKind::ApiError {
                code: Some(400..=407 | 409..=428 | 430..=499),
                retryable: false,
                ..
            }
        )
        .then_some("the Gemini API refused agy's request; sent again it meets the same"),
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => None,
    }
}

fn names_usage_credits(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("usage credits") || lower.contains("extra usage")
}

fn credits_wall(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    message: &str,
    otherwise: &dyn Fn() -> TurnEndAction,
) -> Option<TurnEndAction> {
    let from = bucket_model(message)?;
    let to = credits_fallback(state, reading, cfg, &from)?;
    Some(TurnEndAction::Restart {
        why: Restart::Model { to },
        rule_id: RULE_MODEL_FALLBACK,
        otherwise: Box::new(otherwise()),
    })
}

#[allow(clippy::too_many_arguments)]
fn wall_own_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    let track = state.wall.as_ref().filter(|t| t.class == class_of(kind));
    let since = track.map_or(now, |t| t.since);
    let attempts = track.map_or(0, |t| t.attempts);
    let esc = |reason: String| TurnEndAction::Escalate { reason };
    let msg = &reading.wall_message;
    let by_itself = goes_on_by_itself(state, track, reading);
    let wait_out = || {
        if by_itself {
            if !cfg.resume_limits {
                return TurnEndAction::Nothing;
            }
            let t = &state.timing;
            let due = match reading.reset_at {
                Some(at) => later(at, t.auto_resume_grace),
                None => later(since, t.limit_backoff.last().copied().unwrap_or_default()),
            };
            if now < due {
                return TurnEndAction::WaitUntil {
                    until: due,
                    why: format!("{} goes on by itself", kind.name()),
                    wait: WaitKind::Other,
                };
            }
            let clearing = if reading.reset_at.is_some() {
                Clearing::Measured
            } else {
                Clearing::Blind
            };
            return continue_as(RULE_LIMIT_RESUME, clearing);
        }
        if cfg.resume_limits {
            resume_at_reset(state, reading, now, since, attempts, kind, continue_as)
        } else {
            esc(format!("{}: {msg} (resume_limits is off)", kind.name()))
        }
    };
    let credits_only = !cfg.usage_credits
        && reading.reset_at.is_none()
        && !by_itself
        && match kind {
            WallKind::Spend => true,
            WallKind::ModelBucket { .. } => names_usage_credits(msg),
            _ => false,
        };
    let wait_out = || {
        if credits_only {
            esc(format!(
                "{}: {msg} (only usage credits lift it, and `[harness] usage_credits` is off: \
                 nothing is typed)",
                kind.name()
            ))
        } else {
            wait_out()
        }
    };
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => {
            if !cfg.retry_api_errors {
                return esc(format!("{}: {msg} (retry_api_errors is off)", kind.name()));
            }
            if let Some(why) = not_retried(reading.program, kind) {
                return esc(format!("{} not retried ({why}): {msg}", kind.name()));
            }
            if let Some(outage) = outage_of(kind, measured_down(reading, track), reading.program) {
                return outage_wait(state, reading, now, track, kind, outage, continue_as);
            }
            let t = &state.timing;
            let act =
                |rule: &'static str| type_as(rule, &wall_retry_text(rule, kind, reading.program));
            let wait_then = |due: Instant, why: String, act: TurnEndAction| {
                if now < due {
                    TurnEndAction::WaitUntil {
                        until: due,
                        why,
                        wait: WaitKind::Retry,
                    }
                } else {
                    act
                }
            };
            let rung = |ladder: &[Duration], n: usize| {
                crate::supervise::ladder::Ladder(ladder)
                    .at(n)
                    .map_or(now, |w| later(since, w))
            };
            // The walls the agent can act on now: a refused request, an over-long reply, and a
            // one-off cut while the route is not measured down.
            match kind {
                WallKind::ApiError {
                    cause: ApiCause::Request,
                    ..
                } => wait_then(
                    rung(&t.refused_backoff, attempts),
                    format!("api-error refused: retry {}", attempts + 1),
                    act(RULE_API_RETRY),
                ),
                WallKind::ApiError {
                    cause: ApiCause::Overlong,
                    ..
                } => wait_then(
                    attempts
                        .checked_sub(1)
                        .map_or(since, |n| rung(&t.retry_backoff, n)),
                    format!("api-error overlong: retry {}", attempts + 1),
                    act(RULE_API_CUTOFF),
                ),
                _ => wait_then(
                    attempts
                        .checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error cut off: retry {}", attempts + 1),
                    act(RULE_API_CUTOFF),
                ),
            }
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => {
            if !by_itself && !cfg.resume_limits && kind != WallKind::Spend {
                // The limit episode tells the person (the loop's
                // `limit_after`).
                return TurnEndAction::Nothing;
            }
            if credits_only && let Some(act) = credits_wall(state, reading, cfg, msg, &wait_out) {
                return act;
            }
            wait_out()
        }
        WallKind::ModelBucket { consent } => {
            if consent && attempts == 0 && cfg.continue_policy && cfg.usage_credits {
                return continue_as(RULE_CONSENT, Clearing::Measured);
            }
            let Some(fallback) = &cfg.model_fallback else {
                return wait_out();
            };
            if attempts == 0
                && reading.reset_at.is_some_and(|r| {
                    now >= later(r, state.timing.grace_for(reading.program) + reading.jitter)
                })
            {
                return wait_out();
            }
            if state.model.is_some()
                || state.fallback_unmade
                || !reading.restartable
                || bucket_model(msg).as_deref() == Some(fallback.as_str())
            {
                // Switched already, a relaunch that could not be made or no
                // host to make one, or the fallback is the model at its
                // limit: the reset is waited out.
                return wait_out();
            }
            TurnEndAction::Restart {
                why: Restart::Model {
                    to: fallback.clone(),
                },
                rule_id: RULE_MODEL_FALLBACK,
                otherwise: Box::new(wait_out()),
            }
        }
        WallKind::Context => {
            if !cfg.compact_on_context_wall {
                return esc(format!(
                    "context full: {msg} (compact_on_context_wall is off)"
                ));
            }
            if let Some(wait) = attempts.checked_sub(1).and_then(|n| retry_wait(state, n)) {
                let due = later(since, wait);
                if now < due {
                    return TurnEndAction::WaitUntil {
                        until: due,
                        why: format!("context still full after /compact ({attempts})"),
                        wait: WaitKind::Other,
                    };
                }
            }
            let Some(compact) = compact_command(reading.program) else {
                return esc(format!(
                    "context full: {msg} ({} has no compaction aterm types)",
                    reading.program.product()
                ));
            };
            budgeted(TurnEndAction::TypeCommand {
                command: compact.to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            })
        }
        WallKind::Memory => memory_restart(cfg, reading),
        WallKind::Auth => {
            let back = login_back_elsewhere(state, reading);
            if (attempts > 0 || track.is_some_and(|t| t.login.cancelled)) && !back {
                return login_opening_wait(state, now).unwrap_or(TurnEndAction::Nothing);
            }
            if renewed_draft_holds(reading, cfg) {
                return esc(renewed_draft_reason(&reading.composer));
            }
            match &reading.login_shared {
                Some(LoginShared::Back { .. }) if back => {
                    return type_as(RULE_LOGIN_BACK, &login_back_text());
                }
                Some(LoginShared::Carried { by }) => {
                    return TurnEndAction::WaitUntil {
                        until: later(now, LOGIN_SHARED_LOOK),
                        why: format!("the login's sign-in is {by}'s"),
                        wait: WaitKind::Other,
                    };
                }
                Some(LoginShared::Back { .. }) | None => {}
            }
            let how = sign_in_how(reading.program, msg);
            if !cfg.retry_api_errors {
                return esc(login_gone(
                    reading.program,
                    msg,
                    Some("retry_api_errors is off"),
                    &how,
                ));
            }
            let Some(login) = login_command(reading.program) else {
                return esc(login_gone(reading.program, msg, None, &how));
            };
            // A `/login` that draws no picker (a sign-in method its settings pin) opens the
            // browser one tick after its Enter, over the person's work (v6 §14.17 rule 1; the
            // post-landing review, 2026-11-02): it is theirs to type, told quietly.
            if let Some(why) = reading.login_pin.why() {
                // Over the vault token, `/login` is no way back: a new token is.
                let how = match reading.login_pin {
                    super::login::LoginPin::Token => TOKEN_HOW.to_string(),
                    super::login::LoginPin::Picker
                    | super::login::LoginPin::Method
                    | super::login::LoginPin::Gateway
                    | super::login::LoginPin::Unread => how,
                };
                return esc(login_gone(reading.program, msg, Some(why), &how));
            }
            budgeted(TurnEndAction::TypeCommand {
                command: login.to_string(),
                rule_id: RULE_LOGIN,
                then: Then::Nothing,
            })
        }
    }
}

/// The outage wall the screen at `reading` shows ([`outage_of`]), none when its notice is off it.
fn outage_shown(state: &TurnEndState, reading: &TurnEndReading) -> Option<Outage> {
    let track = state.wall.as_ref().filter(|t| t.class == WallClass::Retry);
    outage_of(
        reading.wall?,
        measured_down(reading, track),
        reading.program,
    )
}

/// Whether the host measures the route down now, or measured it so at the wall `track` stands
/// for ([`WallTrack::down`]).
fn measured_down(reading: &TurnEndReading, track: Option<&WallTrack>) -> bool {
    matches!(reading.reach, Reach::Down { .. }) || track.is_some_and(|t| t.down)
}

/// THE OUTAGE AN API WALL OF `kind` IS (v6 §14.17.5.2), where the agent cannot act on it now and
/// it is waited out in silence ([`outage_wait`]): an overload, a server error, an unreachable
/// route or a certificate or proxy refusal, and a cut-off reply where the host measured the route
/// `down` ([`measured_down`]). A server error with no status where the host measured the route
/// `down` is the connection's, as such a cut is: nothing shows its request reached a server, and
/// nothing gets through a route measured down; its words name no lost route, so the Down holds
/// it one cap only ([`outage_wait`]). A refused request, an over-long reply, a one-off
/// cut and agy's refused request ([`not_retried`]) keep their acts: the agent can act on them now.
pub(crate) fn outage_of(kind: WallKind, down: bool, program: Program) -> Option<Outage> {
    if not_retried(program, kind).is_some() {
        return None;
    }
    match kind {
        WallKind::ApiError {
            cause: ApiCause::CutOff,
            ..
        } if !down => None,
        WallKind::ApiError {
            cause: ApiCause::Server,
            code: None,
            ..
        } if down => Some(Outage::Connection),
        _ => outage_named(kind),
    }
}

/// The outage an outage wall of `kind` names ([`outage_of`] without its reach or program).
pub(crate) const fn outage_named(kind: WallKind) -> Option<Outage> {
    match kind {
        WallKind::Overloaded => Some(Outage::Overload),
        WallKind::ApiError {
            cause: ApiCause::Server,
            ..
        } => Some(Outage::ApiError),
        WallKind::ApiError {
            cause: ApiCause::Unreachable | ApiCause::Config | ApiCause::CutOff,
            ..
        } => Some(Outage::Connection),
        _ => None,
    }
}

/// AN OUTAGE WALL'S WAIT (v6 §14.17.5.2): a wait, never a turn end, typing only its one line
/// ([`RULE_OUTAGE_RESUME`], [`cleared_text`]). A connection error the host measures down types
/// nothing while it reads so, deciding again each [`TurnEndTiming::outage_cap`] from the wall; its
/// edge up measured after the wall's last try (the track's `since`, which each re-read after a try
/// moves) resumes it once, *has cleared*. What nothing measures (an overload or a server error on
/// any route, but a code-less one measured down, which [`outage_of`] makes a connection error; a
/// connection error read unknown or up since before the last try) is tried blind,
/// *may have cleared*, at [`TurnEndTiming::outage_first`] from the wall, doubling with each try to
/// [`TurnEndTiming::outage_cap`]. A wall of `kind` that only the measure makes the connection's
/// (its own words name another outage, [`outage_named`]) is held by a Down as a wall-less point's
/// act is ([`TurnEndTiming::outage_holds_until`]): its blind try waits until the cap has passed
/// since the Down began, once per outage, so a probe wrong about the route costs it one cap.
///
/// Slice 12c (v6 §14.17.5.4) adds two clearings ahead of the blind ladder: another session's
/// turn on the same route finished after the wall's last try ([`witness_clears`]) resumes it at
/// once, *has cleared*; and a rate limit whose words name its `retry-after` ([`retry_after_of`])
/// takes its first try then, not on the ladder's first rung ([`outage_gap`]).
fn outage_wait(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
    track: Option<&WallTrack>,
    kind: WallKind,
    outage: Outage,
    continue_as: &dyn Fn(&'static str, Clearing) -> TurnEndAction,
) -> TurnEndAction {
    let t = &state.timing;
    let since = track.map_or(now, |t| t.since);
    let attempts = track.map_or(0, |t| t.attempts);
    let wait = |until: Instant, tries: bool| TurnEndAction::WaitUntil {
        until,
        why: format!("waiting: {}", outage.words()),
        wait: WaitKind::Outage {
            wall: outage,
            tries,
        },
    };
    let named = outage_named(kind) == Some(outage);
    if outage == Outage::Connection {
        match reading.reach {
            Reach::Down { .. } if named => {
                return wait(next_period(since, now, t.outage_cap), false);
            }
            Reach::Up { since: up } if up > since => {
                return continue_as(RULE_OUTAGE_RESUME, Clearing::Measured);
            }
            Reach::Down { .. } | Reach::Up { .. } | Reach::Unknown => {}
        }
    }
    if witness_clears(track, reading, kind).is_some() {
        return continue_as(RULE_OUTAGE_RESUME, Clearing::Measured);
    }
    let n = u32::try_from(attempts).unwrap_or(u32::MAX);
    let rung = later(
        since,
        outage_gap(t, n, retry_after_of(kind, &reading.wall_message)),
    );
    let rung = match t.outage_holds_until(reading.reach, now) {
        Some(held) if !named => rung.max(held),
        _ => rung,
    };
    if now < rung {
        return wait(rung, true);
    }
    continue_as(RULE_OUTAGE_RESUME, Clearing::Blind)
}

/// A RATE LIMIT IS THE ACCOUNT'S (v6 §14.17.5.4): an API error with a 429 is answered to the
/// account or the organisation that sent the request, so another session's turn on the same route
/// (another account's, maybe) witnesses nothing of it ([`witness_clears`]); its own words may name
/// when to try again ([`retry_after_of`]).
const fn rate_limited(kind: WallKind) -> bool {
    matches!(
        kind,
        WallKind::ApiError {
            code: Some(429),
            ..
        }
    )
}

/// THE WAIT A RATE LIMIT'S OWN WORDS NAME (v6 §14.17.5.4, slice 12c): the `retry-after` the wall's
/// `message` states (`Please try again in 5s.`, agy's `retryDelay:47.2s`,
/// [`aterm_phase::retry_after`]), for a rate limit only ([`rate_limited`]); `None` for any other
/// wall, and for a 429 whose words name no number.
#[must_use]
pub fn retry_after_of(kind: WallKind, message: &str) -> Option<Duration> {
    if rate_limited(kind) {
        aterm_phase::retry_after(message)
    } else {
        None
    }
}

/// THE GAP FROM AN OUTAGE WALL (or its last try) TO TRY `n` (counted from 0): the ladder's rung,
/// [`TurnEndTiming::outage_first`] doubling to [`TurnEndTiming::outage_cap`]; where the wall names
/// its `retry-after` (`after`, [`retry_after_of`]), bounded by the cap, the first try comes then,
/// not on the ladder's first rung, and no later try sooner than it (v6 §14.17.5.4).
#[must_use]
pub fn outage_gap(t: &TurnEndTiming, n: u32, after: Option<Duration>) -> Duration {
    let ladder = crate::supervise::ladder::doubling(t.outage_first, n, t.outage_cap);
    match after.map(|a| a.min(t.outage_cap)) {
        Some(after) if n == 0 => after,
        Some(after) => ladder.max(after),
        None => ladder,
    }
}

/// Whether `action` types an outage's blind try (*may have cleared*, [`Clearing::Blind`]): the one
/// resume text no witness decided, so it says no witness and latches none, whatever the reading
/// beside it ([`TurnEndState::witness_resumed`]).
fn types_blind_outage_line(action: &TurnEndAction) -> bool {
    matches!(action, TurnEndAction::Type { text, .. } if
        [Outage::Overload, Outage::ApiError, Outage::Connection]
            .into_iter()
            .any(|o| text.contains(cleared_text(Cleared::Outage(o), Clearing::Blind))))
}

/// THE WITNESS THAT CLEARS AN OUTAGE (v6 §14.17.5.4, slice 12c): another session of this aterm on
/// the same route finished a turn with a real reply ([`TurnEndReading::witness`]) after the wall's
/// last try (`track`'s `since`, which each re-read after a try moves): the API answered that route
/// since, so the wait resumes at once, *has cleared*, and no blind rung types it. None where a
/// witness says nothing of this wall: a rate limit ([`rate_limited`]: the account's), a route
/// the host measures down now (nothing typed into a Down, §14.17.5.2), or a track whose resume on
/// an earlier witness met the wall again ([`WallTrack::witnessed`]: the ladder tries it until the
/// agent works again).
fn witness_clears(
    track: Option<&WallTrack>,
    reading: &TurnEndReading,
    kind: WallKind,
) -> Option<Instant> {
    let track = track?;
    if track.witnessed || rate_limited(kind) || matches!(reading.reach, Reach::Down { .. }) {
        return None;
    }
    reading.witness.filter(|&w| w > track.since)
}

/// The first of `since`'s whole `period`s that ends after `now`: a wait that decides again on its
/// own clock, the same deadline at each decision within one period (never less than a second).
fn next_period(since: Instant, now: Instant, period: Duration) -> Instant {
    let period = period.max(Duration::from_secs(1));
    let whole = now.saturating_duration_since(since).as_nanos() / period.as_nanos();
    let n = u32::try_from(whole.saturating_add(1)).unwrap_or(u32::MAX);
    later(since, period.saturating_mul(n))
}

fn memory_restart(cfg: &SupervisorConfig, reading: &TurnEndReading) -> TurnEndAction {
    let banner = aterm_phase::memory_banner_head(&reading.wall_message);
    let reason = match reading.resume.as_deref() {
        Some(resume) => format!(
            "memory critical: restart it{}{resume}: {banner}",
            crate::harness::resume::THEN_RESUME_WITH
        ),
        None => format!("memory critical: restart it: {banner}"),
    };
    if !cfg.relaunch {
        return TurnEndAction::Escalate {
            reason: format!("{reason} (relaunch is off)"),
        };
    }
    if !reading.restartable {
        return TurnEndAction::Escalate { reason };
    }
    TurnEndAction::Restart {
        why: Restart::Memory,
        rule_id: RULE_MEMORY_RESTART,
        otherwise: Box::new(TurnEndAction::Escalate { reason }),
    }
}

#[cfg(test)]
#[path = "turn_end_tests.rs"]
mod tests;
