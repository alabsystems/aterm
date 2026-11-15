// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GOAL CLAUDE CODE CLEARED AFTER AN UNRECOVERABLE ERROR, PUT BACK.
//!
//! Claude Code drops a session's `/goal` when a request ends in an error it calls unrecoverable
//! (a login it reads as lost, a refused request, a full context), and asks for `/goal` to be run
//! again: 2.1.323 did it on m2 on 2026-11-13 at a Cloudflare 403 it worded as a lost login, and
//! the session then worked for hours on the same login with no goal. Its transcript tells that
//! clear from a person's `/goal clear` ([`crate::harness::goal::GoalEnd::ByError`]).
//!
//! Under Cruise and Land the harness types the goal back itself ([`act`]): at the session's first
//! point past the error (a response the API served after the clear, or the wall the clear was
//! read at gone), idle at its prompt, no wall drawn, nothing in the composer and no input
//! pending, it types `/goal <condition>` (the condition Claude Code held, exactly, as one line:
//! [`crate::harness::goal::typable`]), once per clear, in the place of a continuation the mode
//! would type there ([`replaces`]: `turn_end::restore_or_continue`, and Cruise's carry at a break
//! of the agent's own background work). An answer, a go-ahead, a credential line, Land's
//! instruction or its land check is typed as the mode decides, and the restore waits for the next
//! continuation; where the mode types nothing (Land's work landed, a back-off, an outage), the
//! restore waits with it. A goal Claude Code clears again soon after a restore waits
//! [`GOAL_RESTORE_FIRST`] after it, doubled at each restore in a row up to [`GOAL_RESTORE_CAP`];
//! the doubling starts over only for a clear first read that long after the last restore. Every
//! restore is the policy's typed act in the journal and the ledger, its row naming the clear it
//! answers and its place in the row ([`typed_reason`]), so a later loop takes both up
//! ([`GoalRestores::seed`]); what followed it is one ledger record ([`GoalRestores::outcome`]). A
//! condition that cannot be typed back as it was is never typed changed: its clear is one skipped
//! record ([`GoalRestores::untypable`]). Each point that reads the goal lost and types no restore
//! says why in the journal, once a change ([`Held`]). Nothing asks a person. Under Off nothing is
//! typed. A person's own `/goal clear`, `/goal` or a goal they set since is never undone or
//! overwritten: the transcript then reads no lost goal.

use std::time::{Duration, Instant};

use aterm_phase::Program;

use super::turn_end::{Composer, Then, TurnEndAction, TurnEndReading};
use crate::harness::goal::{GoalRead, Lost, ROW_ID_BYTES, Untypable};
use alab_policy::config::Autopilot;

/// The rule the restore is typed under.
pub const RULE_GOAL_RESTORE: &str = "goal-restore@v1";

/// The check a turn-end trace names where the restore is decided.
pub const CHECK: &str = "goal-restore";

/// How long after a restore a goal Claude Code cleared again waits for the next one: this,
/// doubled at each restore in a row, up to [`GOAL_RESTORE_CAP`].
pub const GOAL_RESTORE_FIRST: Duration = Duration::from_secs(10 * 60);

/// The longest wait between two restores. A clear first read this long or more after the last
/// restore starts the doubling over.
pub const GOAL_RESTORE_CAP: Duration = Duration::from_secs(6 * 3600);

/// Whether the harness puts back a goal Claude Code cleared after an error for `program` under
/// `autopilot`: under Cruise or Land, for Claude Code, whose transcript tells such a clear apart.
#[must_use]
pub fn restores_for(program: Program, autopilot: Autopilot) -> bool {
    let reads_goal = match program {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    };
    reads_goal && matches!(autopilot, Autopilot::Cruise | Autopilot::Land)
}

/// [`restores_for`] at `reading`'s program and mode.
#[must_use]
pub fn restores_goal(reading: &TurnEndReading) -> bool {
    restores_for(reading.program, reading.autopilot)
}

/// The line that sets `line` ([`Lost::line`]) as the session's goal again.
#[must_use]
pub fn command(line: &str) -> String {
    format!("/goal {line}")
}

/// The wait after the `rung`-th restore in a row before the next ([`GOAL_RESTORE_FIRST`] doubled,
/// at most [`GOAL_RESTORE_CAP`]).
#[must_use]
pub fn backoff(rung: u32) -> Duration {
    let doublings = rung.saturating_sub(1).min(16);
    GOAL_RESTORE_FIRST
        .saturating_mul(1 << doublings)
        .min(GOAL_RESTORE_CAP)
}

/// Whether the restore may go in the place of the act `rule` names: a continuation the mode types
/// again at its next point (Cruise's continuation and done check, its carry at a break, codex's
/// bounded continuation, Land's nudge). An answer to a question or a park, a credential line,
/// Land's instruction and its land check are typed where the mode decides them; the restore waits
/// for the next continuation.
#[must_use]
pub fn replaces(rule: &str) -> bool {
    use crate::harness::acts::RULE_POLICY_CONTINUE;
    use alab_policy::words::{RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND_NUDGE};
    matches!(
        rule,
        RULE_CONTINUE | RULE_POLICY_CONTINUE | RULE_DONE_CHECK | RULE_LAND_NUDGE
    ) || rule == alab_policy::cruise_carry::RULE_BACKGROUND
}

/// The note a restore's ledger row names the clear it answers and its rung in ([`typed_reason`]).
const RESTORE_OF: &str = "restore of goal clear";

/// The note beside it: the restore's place among restores in a row ([`backoff`]).
const IN_A_ROW: &str = "restores in a row";

/// A restore's ledger row's reason: `why`, the clear it answers and its `rung`, which a later
/// loop takes up ([`restore_of`], [`GoalRestores::seed`]).
#[must_use]
pub fn typed_reason(why: &str, clear: &str, rung: u32) -> String {
    format!("{why} ({RESTORE_OF}: {clear}; {IN_A_ROW}: {rung})")
}

/// The clear and rung a restore's row names ([`typed_reason`]); `None` where it names none, or a
/// clear id that is empty or longer than [`ROW_ID_BYTES`].
#[must_use]
pub fn restore_of(reason: &str) -> Option<(String, u32)> {
    let (_, rest) = reason.split_once(&format!("({RESTORE_OF}: "))?;
    let (clear, rest) = rest.split_once(&format!("; {IN_A_ROW}: "))?;
    let rung = crate::parse_uint(rest.split(')').next()?)?;
    (!clear.is_empty() && clear.len() <= ROW_ID_BYTES).then(|| (clear.to_owned(), rung))
}

/// What followed a restore ([`GoalRestores::outcome`]), each with the condition typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The transcript sets the restored condition again: a new `goal_status` set row.
    Restored(String),
    /// A later point reads the goal moved on: the restored goal set and ended since (met, given
    /// up, cleared), another goal set, a person's `/goal`, or a later clear after an error.
    Moved(String),
    /// A later point still reads the same clear: no goal row followed the restore. It is not
    /// typed again; the goal-cleared exception stands for it.
    Unconfirmed(String),
}

impl Outcome {
    /// The outcome as the ledger records it: one line naming what followed.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Restored(_) => "the goal Claude Code cleared after an error is set again".into(),
            Self::Moved(_) => {
                "the goal moved on after the restore: set and ended, replaced, or cleared again"
                    .into()
            }
            Self::Unconfirmed(_) => {
                "no goal row followed the restore: the clear still stands".into()
            }
        }
    }

    /// The condition the restore typed.
    #[must_use]
    pub fn condition(&self) -> &str {
        match self {
            Self::Restored(c) | Self::Moved(c) | Self::Unconfirmed(c) => c,
        }
    }
}

/// WHY A POINT THAT READS THE GOAL LOST TYPES NO RESTORE ([`GoalRestores::held`]), as the journal
/// says it once a change ([`GoalRestores::say`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// The condition cannot be typed back as it was.
    Untypable(Untypable),
    /// The restore of this clear was typed already: once per clear.
    Typed,
    /// A wall is drawn.
    Walled,
    /// The composer holds something.
    Composer,
    /// Input the agent has not read is pending.
    Pending,
    /// The error has not passed: nothing served since the clear, and no wall seen gone.
    Unpassed,
    /// The wait after the last restore runs to this time.
    Waits(Instant),
    /// The mode's act here is no continuation (an answer, a go-ahead, a credential line, Land's
    /// instruction or land check), or it types nothing.
    Act,
}

impl Held {
    /// The hold as the journal says it, `stamp` naming a time.
    #[must_use]
    pub fn why(self, stamp: &dyn Fn(Instant) -> String) -> String {
        match self {
            Self::Untypable(u) => format!("left cleared: {}", u.why()),
            Self::Typed => "restored once already: no goal row followed".to_owned(),
            Self::Walled => "held:wall".to_owned(),
            Self::Composer => "held:composer".to_owned(),
            Self::Pending => "held:input-pending".to_owned(),
            Self::Unpassed => "held:error-not-passed".to_owned(),
            Self::Waits(until) => format!("waits until {}", stamp(until)),
            Self::Act => "held:the mode's act is no continuation".to_owned(),
        }
    }
}

/// What a point's screen shows the restore must wait for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seat {
    /// A wall is drawn.
    pub walled: bool,
    /// The composer is empty or holds only its placeholder.
    pub clear: bool,
    /// Input the agent has not read is pending.
    pub pending: bool,
}

impl Seat {
    /// The seat `reading` shows.
    #[must_use]
    pub fn of(reading: &TurnEndReading) -> Self {
        Self {
            walled: reading.wall.is_some(),
            clear: matches!(reading.composer, Composer::Empty | Composer::Placeholder(_)),
            pending: reading.pending_input,
        }
    }

    /// A settled break of the agent's own background work whose write guards passed: no wall, no
    /// draft and no unread input.
    pub const AT_BREAK: Self = Self {
        walled: false,
        clear: true,
        pending: false,
    };
}

/// A clear a point read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    /// The clear ([`Lost::clear`]).
    clear: String,
    /// A wall was drawn at a point that read it.
    walled: bool,
    /// When a point first read it.
    at: Instant,
}

/// The restore the policy typed last.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Typed {
    /// The clear it answered ([`Lost::clear`]).
    clear: String,
    /// The condition it typed (empty for one a loop before this one typed, [`GoalRestores::seed`]).
    condition: String,
    /// When.
    at: Instant,
    /// Its place among restores in a row ([`backoff`]).
    rung: u32,
    /// The point number it was typed at.
    point: u64,
    /// What followed it is recorded ([`GoalRestores::outcome`]).
    told: bool,
    /// A later point still read the clear it answered ([`Outcome::Unconfirmed`]).
    unconfirmed: bool,
}

/// THE POLICY'S MEMORY OF ITS RESTORES, kept in the turn-end state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalRestores {
    /// The clear a point last read.
    seen: Option<Seen>,
    /// The restore typed last.
    typed: Option<Typed>,
    /// The clear whose condition was booked untypable ([`Self::untypable`]).
    booked: Option<String>,
    /// What the journal last said of a clear left unrestored ([`Self::say`]).
    said: Option<(String, Held)>,
}

impl GoalRestores {
    /// A point read `goal` at `now`, a wall drawn there or not.
    pub fn observe(&mut self, goal: &GoalRead, walled: bool, now: Instant) {
        let GoalRead::Lost(lost) = goal else {
            return;
        };
        match self.seen.as_mut() {
            Some(seen) if seen.clear == lost.clear => seen.walled |= walled,
            _ => {
                self.seen = Some(Seen {
                    clear: lost.clear.clone(),
                    walled,
                    at: now,
                });
            }
        }
    }

    /// The restore `command` was typed at `now`, at point number `point`, for the clear a point
    /// read last. It is one more in the row unless that clear was first read
    /// [`GOAL_RESTORE_CAP`] or more after the last restore.
    pub fn typed(&mut self, command: &str, now: Instant, point: u64) {
        let Some(seen) = &self.seen else {
            return;
        };
        let rung = match &self.typed {
            Some(t) if seen.at.saturating_duration_since(t.at) < GOAL_RESTORE_CAP => {
                t.rung.saturating_add(1)
            }
            _ => 1,
        };
        self.typed = Some(Typed {
            clear: seen.clear.clone(),
            condition: command.strip_prefix("/goal ").unwrap_or(command).to_owned(),
            at: now,
            rung,
            point,
            told: false,
            unconfirmed: false,
        });
    }

    /// A LOOP BEFORE THIS ONE TYPED A RESTORE of `clear` at `at`, the `rung`-th in a row (its
    /// ledger row, [`restore_of`]): this loop types none for that clear and waits its back-off.
    /// What followed it was that loop's to tell.
    pub fn seed(&mut self, clear: String, rung: u32, at: Instant) {
        self.typed = Some(Typed {
            clear,
            condition: String::new(),
            at,
            rung: rung.max(1),
            point: 0,
            told: true,
            unconfirmed: false,
        });
    }

    /// The clear the restore typed last answered and its rung, for its ledger row
    /// ([`typed_reason`]).
    #[must_use]
    pub fn last(&self) -> Option<(&str, u32)> {
        self.typed.as_ref().map(|t| (t.clear.as_str(), t.rung))
    }

    /// Whether the error `lost` was cleared after has passed: a response the API served after
    /// the clear, or a wall drawn at a point that read it, gone now (`walled` says whether one is
    /// drawn now).
    #[must_use]
    pub fn passed(&self, lost: &Lost, walled: bool) -> bool {
        lost.served
            || (!walled
                && self
                    .seen
                    .as_ref()
                    .is_some_and(|seen| seen.clear == lost.clear && seen.walled))
    }

    /// Whether a restore of `lost` may be typed at `now`: none was typed for this clear, and the
    /// wait after the last restore ([`backoff`]) is over.
    #[must_use]
    pub fn due(&self, lost: &Lost, now: Instant) -> bool {
        self.waits(lost, now).is_ok()
    }

    /// [`Self::due`], or why not: [`Held::Typed`], or [`Held::Waits`] until the wait is over.
    fn waits(&self, lost: &Lost, now: Instant) -> Result<(), Held> {
        match &self.typed {
            None => Ok(()),
            Some(t) if t.clear == lost.clear => Err(Held::Typed),
            Some(t) => {
                let until = t.at + backoff(t.rung);
                if now >= until {
                    Ok(())
                } else {
                    Err(Held::Waits(until))
                }
            }
        }
    }

    /// Whether the harness restores the goal `lost` names: its condition types back as it was,
    /// and no restore of this clear was booked [`Outcome::Unconfirmed`]. Where it does not, the
    /// goal-cleared exception stands ([`super::exception::exception_for`]).
    #[must_use]
    pub fn restoring(&self, lost: &Lost) -> bool {
        lost.line.is_ok()
            && self
                .typed
                .as_ref()
                .is_none_or(|t| t.clear != lost.clear || !t.unconfirmed)
    }

    /// THE LINE A RESTORE OF `lost` TYPES at a point that shows `seat` at `now`, or why none: its
    /// condition untypable, its restore typed already, a wall drawn, the composer not clear, input
    /// pending, its error not passed, or the wait after the last restore not over.
    ///
    /// # Errors
    /// [`Held`]: why no restore is typed here.
    pub fn held(&self, lost: &Lost, seat: Seat, now: Instant) -> Result<String, Held> {
        let line = lost.line.as_ref().map_err(|u| Held::Untypable(*u))?;
        if let Err(Held::Typed) = self.waits(lost, now) {
            return Err(Held::Typed);
        }
        if seat.walled {
            return Err(Held::Walled);
        }
        if !seat.clear {
            return Err(Held::Composer);
        }
        if seat.pending {
            return Err(Held::Pending);
        }
        if !self.passed(lost, seat.walled) {
            return Err(Held::Unpassed);
        }
        self.waits(lost, now)?;
        Ok(line.clone())
    }

    /// Whether the journal should say `held` of `lost` now: once a change for a clear.
    pub fn say(&mut self, lost: &Lost, held: Held) -> bool {
        let now = Some((lost.clear.clone(), held));
        if self.said == now {
            return false;
        }
        self.said = now;
        true
    }

    /// What followed the last restore, once, read at a point numbered `point` that read `goal`:
    /// the restored condition set again, or at a later point the goal moved on or the same clear
    /// still standing. An unread goal tells nothing.
    pub fn outcome(&mut self, goal: &GoalRead, point: u64) -> Option<Outcome> {
        let t = self.typed.as_mut().filter(|t| !t.told)?;
        let later = point > t.point;
        let outcome = match goal {
            GoalRead::Active(active) if *active == t.condition => {
                Outcome::Restored(t.condition.clone())
            }
            GoalRead::Lost(lost) if lost.clear == t.clear => {
                if !later {
                    return None;
                }
                t.unconfirmed = true;
                Outcome::Unconfirmed(t.condition.clone())
            }
            GoalRead::Off | GoalRead::Active(_) | GoalRead::Lost(_) if later => {
                Outcome::Moved(t.condition.clone())
            }
            GoalRead::Unread | GoalRead::Off | GoalRead::Active(_) | GoalRead::Lost(_) => {
                return None;
            }
        };
        t.told = true;
        Some(outcome)
    }

    /// Why the goal `goal` reads lost is left cleared, once per clear: its condition cannot be
    /// typed back as it was ([`crate::harness::goal::typable`]), so the clear is one skipped
    /// record and never a changed goal.
    pub fn untypable(&mut self, goal: &GoalRead) -> Option<Untypable> {
        let GoalRead::Lost(lost) = goal else {
            return None;
        };
        let why = *lost.line.as_ref().err()?;
        if self.booked.as_deref() == Some(lost.clear.as_str()) {
            return None;
        }
        self.booked = Some(lost.clear.clone());
        Some(why)
    }
}

/// THE RESTORE AT `reading`, where one is owed ([`restores_goal`]): the goal read lost and its
/// line typed as [`GoalRestores::held`] allows. The act is typed as a command, which starts the
/// goal's own turn.
#[must_use]
pub fn act(
    restores: &GoalRestores,
    reading: &TurnEndReading,
    now: Instant,
) -> Option<TurnEndAction> {
    let GoalRead::Lost(lost) = &reading.goal else {
        return None;
    };
    if !restores_goal(reading) {
        return None;
    }
    let line = restores.held(lost, Seat::of(reading), now).ok()?;
    Some(restore(&line))
}

/// The restore of `line` as the policy's act.
#[must_use]
pub fn restore(line: &str) -> TurnEndAction {
    TurnEndAction::TypeCommand {
        command: command(line),
        rule_id: RULE_GOAL_RESTORE,
        then: Then::Nothing,
    }
}

/// WHY `reading`'s point, whose decided act is `action`, types no restore of the goal it reads
/// lost: its hold ([`GoalRestores::held`]), or [`Held::Act`] where the restore was free but the
/// mode's act is no continuation it may take the place of. `None` where it reads no lost goal,
/// the harness restores none here ([`restores_goal`]) or `action` is the restore.
#[must_use]
pub fn hold(
    restores: &GoalRestores,
    reading: &TurnEndReading,
    action: &TurnEndAction,
    now: Instant,
) -> Option<Held> {
    let GoalRead::Lost(lost) = &reading.goal else {
        return None;
    };
    if !restores_goal(reading) || action.rule_id() == Some(RULE_GOAL_RESTORE) {
        return None;
    }
    Some(
        restores
            .held(lost, Seat::of(reading), now)
            .err()
            .unwrap_or(Held::Act),
    )
}

#[cfg(test)]
#[path = "goal_restore_tests.rs"]
mod tests;
