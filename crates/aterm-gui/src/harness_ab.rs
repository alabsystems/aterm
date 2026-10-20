// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE A/B BOOK, BY WORD AND CAUSE (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §10.5.3 B): how a supervisable agent's time went under each Autopilot
//! state, per local day, and how long its boxes and questions stood.
//!
//! **The split.** Each published agent word is a phase ([`AbPhase`]):
//! `busy`, `prompt` (at a box), `question` (asked) and `idle` (at its
//! prompt). The words are published whether or not the harness runs
//! (`SessionTimeline::publish_agent`), so every arm is split alike. The arm
//! ([`AbArm`]) is the Autopilot state the stretch ran under. Under Cruise
//! and Land, the time at the prompt also has a cause ([`AbCause`]): the
//! agent said it was finished (the task's face `done` or `landed`), or the
//! harness was pausing after short turns. Off has no harness to say why.
//!
//! **Holds.** A run of a box (or of a question) ends at the first word that
//! is not that one. Its length goes into a log₂ histogram of 250 ms
//! buckets ([`bucket`]) for the arm in force at the run's START: an arm or
//! a cause that moves inside a run splits the time shares, never the
//! sample. The page says a median ([`median`]) as the bucket's geometric
//! centre, `about 0.7 s` ([`about_words`]). No sample list, no allocation.
//!
//! **Seven days.** The totals and the histograms sit in a ring of
//! [`DAYS`] local-day slots ([`AbDay`]). The book reads no clock: its
//! instants are on the ledger clock (`turn_ledger::now_ms`, the one a
//! published word's `changed_ms` is on), and the ledger instant of the next
//! local midnight is kept ([`AbCalendar`]) — one wall read where the book is
//! first fed and one where the wall moved under the ledger clock
//! (`Wake::WallMoved`: a sleep stops the ledger clock). A stretch that
//! crosses the midnight is split there. No "time saved" is derived (§6).
//!
//! **The ends** (§11.6.3, item 4): each day also counts the tasks that came
//! down ([`AbEnds`]) — Land's landings and quiet ends, Cruise's dones past
//! the work floor — and the time from Land to landed, one log₂ sample per
//! landing whose Land start the book saw ([`AbBook::end`]). That start is
//! kept per session apart from the open stretch ([`AbTask`]), so a word that
//! counts in no phase (a wall) drops nothing of it. Kept the way every other
//! number of the book is: a seamless update loses the part since the last
//! reading (§10.5.4).

use std::collections::HashMap;

use crate::autopilot::AutopilotView;
use crate::harness_glass::Stamp;
use crate::harness_screen::{JOINT, duration_words, local_midnight_ms, share_words};

/// The local days the ring keeps: today and the seven before, so the day
/// that just left the page's week is still there while the next one opens.
pub(crate) const DAYS: usize = 8;

/// The local days the page sums: today and the six before.
pub(crate) const WEEK_DAYS: i64 = 7;

/// A hold histogram's buckets: 250 ms doubling, the last from about 36 h.
pub(crate) const BUCKETS: usize = 20;

/// Under this many holds a side has no median: `not enough yet (2)`.
pub(crate) const ENOUGH: u32 = 3;

/// The first bucket's floor, and the doubling's unit.
const BUCKET_MS: u64 = 250;

/// One kind of hold's histogram: counts per [`bucket`].
pub(crate) type Hist = [u32; BUCKETS];

/// Every arm's holds: `[Cruise, Land, Off][box, question]`. Unsupervised
/// time is counted but its holds are compared with nothing, so none kept.
pub(crate) type Holds = [[Hist; 2]; 3];

/// The ring of local days ([`AbBook`]): a day sits at its number modulo
/// [`DAYS`].
pub(crate) type AbRing = [Option<AbDay>; DAYS];

/// Which arm of the A/B a stretch of an agent's time falls in (§4 item 5):
/// the Autopilot state it ran under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbArm {
    /// Cruise under this host's live claim: the harness kept the work moving.
    Cruise,
    /// Land under this host's live claim: the harness was wrapping the work
    /// up. A landing ends waiting, so this share is its own, never compared.
    Land,
    /// Plain Claude Code, the baseline: no harness's hands on the session —
    /// its Autopilot Off, the master switch off, or no host here — and no
    /// supervisor's claim live on it.
    Off,
    /// None of these: a mode the harness carries with no claim of this
    /// host's (the agent not yet attached, another supervisor's claim), or
    /// Off while a claim is still live. Counted, so no time is dropped; never
    /// compared.
    Unsupervised,
}

impl AbArm {
    /// The arm of a session whose effective Autopilot is `view`, whose live
    /// claim is this host's (`supervised`), and on which anyone's claim is
    /// live (`claimed`; `supervised` implies it).
    #[must_use]
    pub(crate) const fn of(view: AutopilotView, supervised: bool, claimed: bool) -> Self {
        match view {
            AutopilotView::Cruise if supervised => Self::Cruise,
            AutopilotView::Land if supervised => Self::Land,
            AutopilotView::Off
            | AutopilotView::MasterOff
            | AutopilotView::AgentOff(_)
            | AutopilotView::Unhosted
                if !claimed =>
            {
                Self::Off
            }
            _ => Self::Unsupervised,
        }
    }

    /// Its place in [`AbDay::arms`].
    #[must_use]
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Cruise => 0,
            Self::Land => 1,
            Self::Off => 2,
            Self::Unsupervised => 3,
        }
    }

    /// Its place in [`Holds`]; `None` for unsupervised time, whose holds
    /// are kept nowhere.
    #[must_use]
    pub(crate) const fn holds(self) -> Option<usize> {
        match self {
            Self::Cruise => Some(0),
            Self::Land => Some(1),
            Self::Off => Some(2),
            Self::Unsupervised => None,
        }
    }
}

/// What an agent was doing in a stretch the A/B counts, from its published
/// word. Every other word — a wall, a survey, `unknown`, `-` — counts in
/// none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbPhase {
    /// `busy`: a turn (or background work) runs.
    Busy,
    /// `prompt`: a permission box stands.
    AtBox,
    /// `question`: the agent asked the person something.
    Asked,
    /// `idle`: the agent sits at its prompt.
    AtPrompt,
}

impl AbPhase {
    /// The phase a published agent word (`AgentPublication::word`) counts as.
    #[must_use]
    pub(crate) fn of(word: &str) -> Option<Self> {
        match word {
            "busy" => Some(Self::Busy),
            "prompt" => Some(Self::AtBox),
            "question" => Some(Self::Asked),
            "idle" => Some(Self::AtPrompt),
            _ => None,
        }
    }

    /// A hold's kind, where this phase holds the agent for the person: `0`
    /// a box, `1` a question ([`Holds`]' inner index).
    #[must_use]
    pub(crate) const fn held(self) -> Option<usize> {
        match self {
            Self::AtBox => Some(0),
            Self::Asked => Some(1),
            Self::Busy | Self::AtPrompt => None,
        }
    }
}

/// Why an agent under Cruise or Land sat at its prompt, where the harness
/// can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbCause {
    /// The agent said it was finished: the task's face is `done` or
    /// `landed` (§9.4.2).
    Finished,
    /// The harness's own pause after short turns (§10.5.1).
    Pausing,
}

impl AbCause {
    /// The cause of a stretch in `phase` under `arm`, from the session's
    /// glass: whether its face says finished, and whether a pause stands.
    /// Cruise and Land at the prompt only; finished first (an agent done
    /// with its work is not waiting on the pause).
    #[must_use]
    pub(crate) fn of(
        arm: AbArm,
        phase: Option<AbPhase>,
        finished: bool,
        pausing: bool,
    ) -> Option<Self> {
        if phase != Some(AbPhase::AtPrompt) || !matches!(arm, AbArm::Cruise | AbArm::Land) {
            return None;
        }
        if finished {
            Some(Self::Finished)
        } else if pausing {
            Some(Self::Pausing)
        } else {
            None
        }
    }
}

/// One arm's time, by phase, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbArmTotals {
    /// Busy.
    pub(crate) busy_ms: u64,
    /// At a box.
    pub(crate) at_box_ms: u64,
    /// At a question.
    pub(crate) asked_ms: u64,
    /// At its prompt.
    pub(crate) at_prompt_ms: u64,
    /// Of `at_prompt_ms`, after the agent said it was finished.
    pub(crate) finished_ms: u64,
    /// Of `at_prompt_ms`, the harness pausing after short turns.
    pub(crate) pausing_ms: u64,
}

impl AbArmTotals {
    /// All the time this arm measured.
    #[must_use]
    pub(crate) const fn measured_ms(self) -> u64 {
        self.busy_ms
            .saturating_add(self.at_box_ms)
            .saturating_add(self.asked_ms)
            .saturating_add(self.at_prompt_ms)
    }

    /// The time held for the person: at a box or a question.
    #[must_use]
    pub(crate) const fn held_ms(self) -> u64 {
        self.at_box_ms.saturating_add(self.asked_ms)
    }

    /// WORKING time: busy, or held for the person. An agent's idle after its
    /// work is in no comparison of holds.
    #[must_use]
    pub(crate) const fn working_ms(self) -> u64 {
        self.busy_ms.saturating_add(self.held_ms())
    }

    fn add(&mut self, phase: AbPhase, cause: Option<AbCause>, ms: u64) {
        let bucket = match phase {
            AbPhase::Busy => &mut self.busy_ms,
            AbPhase::AtBox => &mut self.at_box_ms,
            AbPhase::Asked => &mut self.asked_ms,
            AbPhase::AtPrompt => &mut self.at_prompt_ms,
        };
        *bucket = bucket.saturating_add(ms);
        let of_prompt = match cause {
            Some(AbCause::Finished) => &mut self.finished_ms,
            Some(AbCause::Pausing) => &mut self.pausing_ms,
            None => return,
        };
        *of_prompt = of_prompt.saturating_add(ms);
    }

    fn plus(&mut self, other: Self) {
        self.busy_ms = self.busy_ms.saturating_add(other.busy_ms);
        self.at_box_ms = self.at_box_ms.saturating_add(other.at_box_ms);
        self.asked_ms = self.asked_ms.saturating_add(other.asked_ms);
        self.at_prompt_ms = self.at_prompt_ms.saturating_add(other.at_prompt_ms);
        self.finished_ms = self.finished_ms.saturating_add(other.finished_ms);
        self.pausing_ms = self.pausing_ms.saturating_add(other.pausing_ms);
    }
}

/// One kind of task end the book counts ([`AbBook::end`], [`AbEnds`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbEnd {
    /// Cruise's done past the work floor (`harness_task::END_WORK_MS`): the
    /// one end (§11.2.3, item 1), which slice 2a's `App::note_harness_end`
    /// counts. Nothing counts it before that slice lands.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "slice 2a's `App::note_harness_end` counts it")
    )]
    Done,
    /// A landing that landed under Land (`harness_task::is_an_end`).
    Landed,
    /// A landing spent with no done reply under Land: quiet.
    Quiet,
}

/// THE ENDS ONE DAY COUNTED (§11.6.3, item 4): each kind of end
/// ([`AbEnd`]), and the time from Land to landed as a hold's is kept, one
/// sample per landing whose Land start the book saw, in [`bucket`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbEnds {
    /// Cruise's dones past the work floor ([`AbEnd::Done`]).
    pub(crate) done: u32,
    /// Land's landings ([`AbEnd::Landed`]).
    pub(crate) landed: u32,
    /// Land's quiet ends ([`AbEnd::Quiet`]).
    pub(crate) quiet: u32,
    /// The time from Land to landed.
    pub(crate) landing: Hist,
}

impl AbEnds {
    /// Another day's ends added to these.
    pub(crate) fn plus(&mut self, other: &Self) {
        self.done = self.done.saturating_add(other.done);
        self.landed = self.landed.saturating_add(other.landed);
        self.quiet = self.quiet.saturating_add(other.quiet);
        for (count, add) in self.landing.iter_mut().zip(other.landing) {
            *count = count.saturating_add(add);
        }
    }
}

/// One local day of the book: every arm's time, every arm's holds and the
/// ends counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AbDay {
    /// The local day number (days since 1970-01-01, local time).
    pub(crate) day: i64,
    /// The unix ms the day's first counted stretch began.
    pub(crate) first_unix_ms: i64,
    /// Each arm's time, at its [`AbArm::index`].
    pub(crate) arms: [AbArmTotals; 4],
    /// Each compared arm's holds ([`Holds`]).
    pub(crate) holds: Holds,
    /// The tasks that came down this day ([`AbEnds`]).
    pub(crate) ends: AbEnds,
}

impl AbDay {
    /// An empty day.
    #[must_use]
    pub(crate) fn new(day: i64, first_unix_ms: i64) -> Self {
        Self {
            day,
            first_unix_ms,
            arms: [AbArmTotals::default(); 4],
            holds: [[[0; BUCKETS]; 2]; 3],
            ends: AbEnds::default(),
        }
    }

    /// Another day's counts added to this one's (two processes' books of
    /// the same local day), the earlier first stretch kept.
    pub(crate) fn plus(&mut self, other: &Self) {
        self.first_unix_ms = self.first_unix_ms.min(other.first_unix_ms);
        for (mine, theirs) in self.arms.iter_mut().zip(other.arms) {
            mine.plus(theirs);
        }
        for (mine, theirs) in self
            .holds
            .iter_mut()
            .flatten()
            .zip(other.holds.iter().flatten())
        {
            for (count, add) in mine.iter_mut().zip(theirs) {
                *count = count.saturating_add(*add);
            }
        }
        self.ends.plus(&other.ends);
    }
}

/// The bucket a hold of `ms` falls in: `⌊log₂(ms / 250)⌋`, the first
/// taking everything under 500 ms and the last everything from 2¹⁹ × 250 ms
/// (about 36 h) on.
#[must_use]
pub(crate) fn bucket(ms: u64) -> usize {
    let log = (ms / BUCKET_MS).max(1).ilog2();
    usize::try_from(log).map_or(BUCKETS - 1, |b| b.min(BUCKETS - 1))
}

/// A bucket's geometric centre: `250 ms × 2ᵇ × √2`.
fn centre_ms(bucket: usize) -> u64 {
    (BUCKET_MS << bucket.min(BUCKETS - 1)) * 1_414 / 1_000
}

/// A histogram's median and its count: the bucket holding the `⌈n/2⌉`-th
/// sample, at its geometric centre; `None` with no sample.
#[must_use]
pub(crate) fn median(hist: &Hist) -> (Option<u64>, u32) {
    let n = hist
        .iter()
        .fold(0_u32, |sum, count| sum.saturating_add(*count));
    let want = n.div_ceil(2);
    let mut seen = 0_u32;
    for (at, count) in hist.iter().enumerate() {
        seen = seen.saturating_add(*count);
        if n > 0 && seen >= want {
            return (Some(centre_ms(at)), n);
        }
    }
    (None, n)
}

/// A median as the page says it: `about 0.4 s`, `about 23 s`, `about 2m`,
/// `about 1.6h`, `about 25h` — a bucket's centre, so never more digits.
#[must_use]
pub(crate) fn about_words(ms: u64) -> String {
    if ms < 9_950 {
        let tenths = (ms + 50) / 100;
        format!("about {}.{} s", tenths / 10, tenths % 10)
    } else if ms < 59_500 {
        format!("about {} s", (ms + 500) / 1_000)
    } else if ms < 3_570_000 {
        format!("about {}m", (ms + 30_000) / 60_000)
    } else if ms < 35_820_000 {
        let tenths = (ms + 180_000) / 360_000;
        format!("about {}.{}h", tenths / 10, tenths % 10)
    } else {
        format!("about {}h", (ms + 1_800_000) / 3_600_000)
    }
}

/// A day is 23 to 25 hours: 26 after a day's midnight is the next day.
const PAST_TOMORROWS_START_MS: i64 = 26 * 3_600_000;

/// The local day number of unix ms `unix_ms` under `offset_at`.
fn day_of(unix_ms: i64, offset_at: fn(i64) -> Option<i64>) -> i64 {
    let unix_s = unix_ms.div_euclid(1000);
    unix_s
        .saturating_add(offset_at(unix_s).unwrap_or(0))
        .div_euclid(86_400)
}

/// THE BOOK'S CALENDAR: one wall read and the ledger instant it was taken
/// at, today's local day, its start on the ledger clock, and the next local
/// midnight on both clocks. Read where the book is first fed and again where
/// the wall moved under the ledger clock ([`AbBook::set_calendar`]);
/// between, a midnight passed is found by comparing ledger instants, so the
/// book reads no clock. One calendar serves every session: an instant before
/// today's start is placed in its own day ([`Self::day_at`]), never today.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AbCalendar {
    /// The ledger instant of the read.
    ledger_ms: u64,
    /// The wall at that instant, unix ms.
    unix_ms: i64,
    /// The local day stretches are charged to.
    today: i64,
    /// Today's local midnight on the ledger clock (0 where it fell before
    /// the ledger's epoch: every instant is then today's or later).
    today_start_ms: u64,
    /// The next local midnight, unix ms.
    next_midnight_unix_ms: i64,
    /// The same midnight on the ledger clock.
    next_midnight_ms: u64,
    /// The zone's offset east of UTC at a unix second
    /// (`presence::local_offset_at`; `None`: UTC's days).
    offset_at: fn(i64) -> Option<i64>,
}

impl AbCalendar {
    /// The calendar at ledger instant `ledger_ms`, whose wall is `unix_ms`.
    #[must_use]
    pub(crate) fn read(ledger_ms: u64, unix_ms: i64, offset_at: fn(i64) -> Option<i64>) -> Self {
        let today_start = local_midnight_ms(unix_ms, offset_at);
        let next = local_midnight_ms(
            today_start.saturating_add(PAST_TOMORROWS_START_MS),
            offset_at,
        );
        let ahead = u64::try_from(next.saturating_sub(unix_ms)).unwrap_or(0);
        let behind = u64::try_from(unix_ms.saturating_sub(today_start)).unwrap_or(0);
        Self {
            ledger_ms,
            unix_ms,
            today: day_of(unix_ms, offset_at),
            today_start_ms: ledger_ms.saturating_sub(behind),
            next_midnight_unix_ms: next,
            next_midnight_ms: ledger_ms.saturating_add(ahead),
            offset_at,
        }
    }

    /// Today's local day number, as the calendar last rolled.
    #[must_use]
    pub(crate) const fn today(&self) -> i64 {
        self.today
    }

    /// The wall at ledger instant `at`, unix ms.
    fn unix_at(&self, at: u64) -> i64 {
        let signed = |ms: u64| i64::try_from(ms).unwrap_or(i64::MAX);
        self.unix_ms
            .saturating_add(signed(at).saturating_sub(signed(self.ledger_ms)))
    }

    /// The calendar moves on past each midnight at or before `at`.
    fn advance_to(&mut self, at: u64) {
        while at >= self.next_midnight_ms {
            let start = self.next_midnight_unix_ms;
            let next = local_midnight_ms(
                start.saturating_add(PAST_TOMORROWS_START_MS),
                self.offset_at,
            );
            let Ok(length) = u64::try_from(next.saturating_sub(start)) else {
                return;
            };
            if length == 0 {
                return;
            }
            self.today = day_of(start, self.offset_at).max(self.today.saturating_add(1));
            self.today_start_ms = self.next_midnight_ms;
            self.next_midnight_unix_ms = next;
            self.next_midnight_ms = self.next_midnight_ms.saturating_add(length);
        }
    }

    /// The local day ledger instant `at` falls on, and the ledger instant
    /// that day ends. From today's start on, the calendar moves on to `at`
    /// ([`Self::advance_to`]). An instant before today's start — a stretch
    /// one session opened before the midnight another session's change
    /// already moved the calendar past, or a word told late — is placed by
    /// the zone at its own wall instant (zone math, no clock read), and its
    /// day ends by today's start at the latest, so no piece of it is ever
    /// charged to today.
    fn day_at(&mut self, at: u64) -> (i64, u64) {
        if at >= self.today_start_ms {
            self.advance_to(at);
            return (self.today, self.next_midnight_ms);
        }
        let unix = self.unix_at(at);
        let day = day_of(unix, self.offset_at).min(self.today.saturating_sub(1));
        let start = local_midnight_ms(unix, self.offset_at);
        let next = local_midnight_ms(
            start.saturating_add(PAST_TOMORROWS_START_MS),
            self.offset_at,
        );
        let ahead = u64::try_from(next.saturating_sub(unix)).unwrap_or(0).max(1);
        (day, at.saturating_add(ahead).min(self.today_start_ms))
    }
}

/// The ring's slot for local day `day`, emptied where it held an earlier
/// day; `first_unix_ms` is kept where it is the day's earliest. `None` for
/// a day the ring no longer keeps: [`DAYS`] or more before `today`, or older
/// than the later day its slot holds (a stretch open since before the ring's
/// oldest day never pushes a kept day out).
fn slot(ring: &mut AbRing, day: i64, first_unix_ms: i64, today: i64) -> Option<&mut AbDay> {
    if day <= today.saturating_sub(DAYS as i64) {
        return None;
    }
    let at = usize::try_from(day.rem_euclid(DAYS as i64)).unwrap_or(0);
    let held = &mut ring[at];
    match held {
        Some(kept) if kept.day > day => return None,
        Some(kept) if kept.day < day => *held = None,
        _ => {}
    }
    let kept = held.get_or_insert_with(|| AbDay::new(day, first_unix_ms));
    kept.first_unix_ms = kept.first_unix_ms.min(first_unix_ms);
    Some(kept)
}

/// Charge `from..to` (ledger ms) to `arm`, `phase` and `cause`, split at
/// each local midnight it crosses: each piece in the day its own instant
/// falls on ([`AbCalendar::day_at`]), however far another session's change
/// has moved the calendar.
fn charge(
    ring: &mut AbRing,
    calendar: &mut AbCalendar,
    stretch: (AbArm, AbPhase, Option<AbCause>),
    from: u64,
    to: u64,
) {
    let (arm, phase, cause) = stretch;
    let mut from = from;
    while from < to {
        let (day, ends) = calendar.day_at(from);
        // A zone whose next midnight would not move on: the rest is the day's.
        let end = if ends > from { to.min(ends) } else { to };
        let first = calendar.unix_at(from);
        if let Some(kept) = slot(ring, day, first, calendar.today) {
            kept.arms[arm.index()].add(phase, cause, end - from);
        }
        from = end;
    }
}

/// A hold of kind `kind` that began at `start` under `arm` and ended at
/// `end`: one sample, in the day it ended.
fn sample(
    ring: &mut AbRing,
    calendar: &mut AbCalendar,
    arm: AbArm,
    kind: usize,
    start: u64,
    end: u64,
) {
    let Some(arm) = arm.holds() else {
        return;
    };
    let (day, _) = calendar.day_at(end);
    let first = calendar.unix_at(end);
    if let Some(kept) = slot(ring, day, first, calendar.today) {
        let count = &mut kept.holds[arm][kind][bucket(end.saturating_sub(start))];
        *count = count.saturating_add(1);
    }
}

/// One session's open stretch: the phase, cause and arm in force since
/// `since_ms`, and the hold it is part of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AbClock {
    phase: AbPhase,
    cause: Option<AbCause>,
    arm: AbArm,
    since_ms: u64,
    /// The box's or question's run this stretch is part of: when it began,
    /// and the arm in force then (its sample's arm).
    hold: Option<(u64, AbArm)>,
}

/// ONE SESSION'S TASK as the book follows it (§11.2.3, item 2; §11.6.3,
/// item 4), kept APART from its [`AbClock`]: `observe(…, None, …)` drops a
/// session's clock at any word that counts in no phase — a wall's among
/// them — and a landing's start must outlive that. Made where the book
/// first reads the session's mode ([`AbBook::follow_mode`]); only
/// [`AbBook::retire`] drops it.
///
/// It follows the session's MODE — its Autopilot word, else the table's
/// default — never its [`AbArm`]: the arm is Land only under this host's
/// live claim, so a claim that lapses and is taken again (a supervisor's
/// restart), or one first taken after a relaunch or a seamless update,
/// would move the arm into Land with no word moved, and start the time from
/// Land to landed where nobody said Land.
///
/// Slice 2a adds the work since the task opened (`worked_ms`, `from_ms`) and
/// `task_opened`, which sets `land_from` again where a task opens under
/// Land; until then a second landing in one Land stretch is counted with no
/// span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AbTask {
    /// Whether the session's mode was Land when the book last read it.
    lands: bool,
    /// The ledger instant the session's mode moved into Land, as the book
    /// read it move: where the time from Land to landed starts. Carried
    /// while the mode stays Land, whatever the claim does; cleared where it
    /// leaves Land; spent by the landing it measures. `None` for a session
    /// whose mode the book first read as Land (a landing begun before this
    /// process, or before the book read it): its landing counts, with no
    /// span.
    land_from: Option<u64>,
    /// The day this book counted the session's last quiet end on, while
    /// that landing may still land after all ([`AbBook::quiet_landed`]):
    /// cleared by a landing and where the mode leaves Land.
    quiet_day: Option<i64>,
}

impl AbTask {
    /// The task of a session whose mode the book first reads (`lands`: Land).
    const fn first_read(lands: bool) -> Self {
        Self {
            lands,
            land_from: None,
            quiet_day: None,
        }
    }

    /// The session's mode is Land (`lands`) or not, read at `at_ms`.
    fn mode(&mut self, lands: bool, at_ms: u64) {
        self.land_from = match (self.lands, lands) {
            (true, true) => self.land_from,
            (false, true) => Some(at_ms),
            (_, false) => None,
        };
        if !lands {
            self.quiet_day = None;
        }
        self.lands = lands;
    }
}

/// THE A/B BOOK: each session's open stretch and its task, the ring of days
/// and the calendar. Fed at each change ([`Self::observe`]), at a task's end
/// ([`Self::end`]) and at a session's end ([`Self::retire`]); read with the
/// open stretches counted up to a given instant ([`Self::ring_at`],
/// [`Self::week_at`]). Every instant is on the ledger clock.
#[derive(Debug, Default)]
pub(crate) struct AbBook {
    /// Session → its open stretch. A session counted in no phase has none.
    clocks: HashMap<u64, AbClock>,
    /// Session → its task ([`AbTask`]), apart from its clock.
    tasks: HashMap<u64, AbTask>,
    /// The days kept.
    ring: Box<AbRing>,
    /// `None` until the book is first fed ([`Self::set_calendar`]).
    calendar: Option<AbCalendar>,
}

impl AbBook {
    /// Whether the calendar was read.
    #[must_use]
    pub(crate) const fn has_calendar(&self) -> bool {
        self.calendar.is_some()
    }

    /// THE CALENDAR READ: where the book is first fed, and again where the
    /// wall moved under the ledger clock. Every open stretch is charged up
    /// to the read's instant under the calendar it ran under, and goes on
    /// from there under the new one — so a sleep that crossed a midnight
    /// leaves the time before it in the day it was spent, and a clock set
    /// back charges no negative (ledger instants only ever grow).
    pub(crate) fn set_calendar(&mut self, calendar: AbCalendar) {
        let at = calendar.ledger_ms;
        if let Some(old) = self.calendar.as_mut() {
            for open in self.clocks.values_mut() {
                if at > open.since_ms {
                    charge(
                        &mut self.ring,
                        old,
                        (open.arm, open.phase, open.cause),
                        open.since_ms,
                        at,
                    );
                    open.since_ms = at;
                }
            }
        }
        self.calendar = Some(calendar);
    }

    /// Session `session` is in `phase` under `arm`, with `cause` (kept
    /// only at the prompt under Cruise or Land: [`AbCause::of`]), from
    /// `at_ms` on: the time since its last change goes to the arm, phase
    /// and cause that were in force (never now's), split at each midnight,
    /// and a new stretch opens. A box's or question's run that ends here is
    /// one sample for the arm it began under; one that goes on (the same
    /// phase, an arm or cause moved) keeps its start. An instant before the
    /// open stretch's start counts nothing (never a negative). Nothing is
    /// charged before the calendar is read.
    pub(crate) fn observe(
        &mut self,
        session: u64,
        phase: Option<AbPhase>,
        cause: Option<AbCause>,
        arm: AbArm,
        at_ms: u64,
    ) {
        let cause = cause.filter(|_| {
            phase == Some(AbPhase::AtPrompt) && matches!(arm, AbArm::Cruise | AbArm::Land)
        });
        let open = self.clocks.remove(&session);
        let at_ms = open.map_or(at_ms, |open| at_ms.max(open.since_ms));
        let mut hold = None;
        if let Some(open) = open {
            let goes_on = phase == Some(open.phase);
            if let Some(calendar) = self.calendar.as_mut() {
                charge(
                    &mut self.ring,
                    calendar,
                    (open.arm, open.phase, open.cause),
                    open.since_ms,
                    at_ms,
                );
                if let (Some((start, start_arm)), Some(kind), false) =
                    (open.hold, open.phase.held(), goes_on)
                {
                    sample(&mut self.ring, calendar, start_arm, kind, start, at_ms);
                }
            }
            if goes_on {
                hold = open.hold;
            }
        }
        if let Some(phase) = phase {
            let hold = hold.or_else(|| phase.held().map(|_| (at_ms, arm)));
            self.clocks.insert(
                session,
                AbClock {
                    phase,
                    cause,
                    arm,
                    since_ms: at_ms,
                    hold,
                },
            );
        }
    }

    /// Session `session` ended at `at_ms`: its open stretch is settled (a
    /// box standing at its end is one sample) and the book forgets it, its
    /// task too.
    pub(crate) fn retire(&mut self, session: u64, at_ms: u64) {
        if let Some(open) = self.clocks.get(&session).copied() {
            self.observe(session, None, None, open.arm, at_ms);
        }
        self.tasks.remove(&session);
    }

    /// SESSION `session`'S MODE IS LAND (`lands`: its Autopilot word, else
    /// the table's default, is `land`) or not, read at `at_ms` (§11.6.3,
    /// item 4; [`AbTask`]). Read where the session's arm may have moved
    /// (`App::note_harness_arm`: every write of the word — a press, a `meta
    /// set`, the timer's fire — the policy's default moved, a claim taken or
    /// given back). Where the mode MOVED into Land since the book last read
    /// it, the time from Land to landed starts here; read Land again, it is
    /// carried, whatever the claim, the master switch or the host did
    /// meanwhile; read anything else, it is dropped. A session whose mode
    /// the book first reads as Land has no start.
    pub(crate) fn follow_mode(&mut self, session: u64, lands: bool, at_ms: u64) {
        self.tasks
            .entry(session)
            .and_modify(|task| task.mode(lands, at_ms))
            .or_insert_with(|| AbTask::first_read(lands));
    }

    /// SESSION `session`'S MODE WAS CARRIED from before this process (a
    /// relaunch's restored word, a seamless update's owed one, grafted onto a
    /// shell the book may have read under the default first): it is read as
    /// Land (`lands`) or not with NO move, as a first read is — a landing it
    /// carries began before this process (§11.6.4 "Before this process").
    pub(crate) fn carry_mode(&mut self, session: u64, lands: bool) {
        self.tasks.insert(session, AbTask::first_read(lands));
    }

    /// SESSION `session`'S TASK CAME DOWN at `at_ms` (§11.6.3, item 4): one
    /// count of `end` in the day `at_ms` falls on, as a hold's sample is
    /// placed; and where it LANDED with a Land start the book read
    /// ([`AbTask`]), one sample of the time from Land to landed, the start
    /// spent. The caller decides that it is an end, once
    /// (`harness_task::is_an_end`, the records' own rule). Nothing is
    /// counted before the calendar is read.
    pub(crate) fn end(&mut self, session: u64, end: AbEnd, at_ms: u64) {
        let task = self.tasks.get_mut(&session);
        let span = match (end, task) {
            (AbEnd::Landed, Some(task)) => {
                task.quiet_day = None;
                task.land_from.take()
            }
            _ => None,
        };
        let Some(calendar) = self.calendar.as_mut() else {
            return;
        };
        let (day, _) = calendar.day_at(at_ms);
        let first = calendar.unix_at(at_ms);
        let Some(kept) = slot(&mut self.ring, day, first, calendar.today) else {
            return;
        };
        if let (AbEnd::Quiet, Some(task)) = (end, self.tasks.get_mut(&session)) {
            task.quiet_day = Some(day);
        }
        let ends = &mut kept.ends;
        let count = match end {
            AbEnd::Done => &mut ends.done,
            AbEnd::Landed => &mut ends.landed,
            AbEnd::Quiet => &mut ends.quiet,
        };
        *count = count.saturating_add(1);
        if let Some(from) = span {
            let sample = &mut ends.landing[bucket(at_ms.saturating_sub(from))];
            *sample = sample.saturating_add(1);
        }
    }

    /// SESSION `session`'S QUIET LANDING LANDED AFTER ALL at `at_ms` (its
    /// face moved spent → landed: a question's land answer, whose done reply
    /// ends the quiet): ONE task's end, not two. The quiet end this book
    /// counted for it ([`AbTask::quiet_day`]) is taken back from its day,
    /// where the ring still keeps that day, and the landing is counted
    /// ([`Self::end`]). A quiet end another process counted (a relaunch
    /// between the two) stays in that process's book.
    pub(crate) fn quiet_landed(&mut self, session: u64, at_ms: u64) {
        let quiet_day = self
            .tasks
            .get_mut(&session)
            .and_then(|task| task.quiet_day.take());
        if let Some(day) = quiet_day {
            let at = usize::try_from(day.rem_euclid(DAYS as i64)).unwrap_or(0);
            if let Some(kept) = self.ring[at].as_mut().filter(|kept| kept.day == day) {
                kept.ends.quiet = kept.ends.quiet.saturating_sub(1);
            }
        }
        self.end(session, AbEnd::Landed, at_ms);
    }

    /// Where session `session`'s time from Land to landed starts, if the
    /// book read it ([`AbTask::land_from`]).
    #[cfg(test)]
    pub(crate) fn land_from(&self, session: u64) -> Option<u64> {
        self.tasks.get(&session).and_then(|task| task.land_from)
    }

    /// How many sessions' tasks the book follows.
    #[cfg(test)]
    pub(crate) fn tasks(&self) -> usize {
        self.tasks.len()
    }

    /// Session `session`'s open stretch: its phase, arm and cause.
    #[cfg(test)]
    pub(crate) fn stretch(&self, session: u64) -> Option<(AbPhase, AbArm, Option<AbCause>)> {
        self.clocks
            .get(&session)
            .map(|open| (open.phase, open.arm, open.cause))
    }

    /// How many sessions have a stretch open.
    #[cfg(test)]
    pub(crate) fn open_stretches(&self) -> usize {
        self.clocks.len()
    }

    /// The ring at `now_ms` — the settled days and every open stretch
    /// charged up to `now_ms`, in a copy — and today's day there. `None`
    /// before the calendar is read: nothing was counted.
    #[must_use]
    pub(crate) fn ring_at(&self, now_ms: u64) -> Option<(AbRing, i64)> {
        let mut calendar = self.calendar?;
        let mut ring = *self.ring;
        for open in self.clocks.values() {
            charge(
                &mut ring,
                &mut calendar,
                (open.arm, open.phase, open.cause),
                open.since_ms,
                now_ms,
            );
        }
        calendar.advance_to(now_ms);
        Some((ring, calendar.today()))
    }

    /// The page's week at `now_ms`: today and the six days before, of this
    /// book and of `others` (other processes' books, summed by day:
    /// `harness_ab_store::read_others`), summed ([`AbWeek::of_days`]). With
    /// no calendar read, the others' alone, against the newest of their
    /// days.
    #[must_use]
    pub(crate) fn week_at(&self, now_ms: u64, others: &[AbDay]) -> AbWeek {
        match self.ring_at(now_ms) {
            Some((ring, today)) => AbWeek::of_days(ring.iter().flatten().chain(others), today),
            None => others
                .iter()
                .map(|day| day.day)
                .max()
                .map(|today| AbWeek::of_days(others, today))
                .unwrap_or_default(),
        }
    }
}

/// THE PAGE'S WEEK: the days of the last [`WEEK_DAYS`] summed — every
/// arm's time and holds, and the ends — when the earliest began, and the
/// instant it was counted to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbWeek {
    /// Each arm's time, at its [`AbArm::index`].
    pub(crate) arms: [AbArmTotals; 4],
    /// Each compared arm's holds.
    pub(crate) holds: Holds,
    /// The tasks that came down ([`AbEnds`]).
    pub(crate) ends: AbEnds,
    /// The earliest day summed and the unix ms its first stretch began;
    /// `None`: nothing measured.
    pub(crate) first: Option<(i64, i64)>,
    /// That instant's local clock and day, read where the page is built
    /// (`App::harness_ab`); `None` where the zone cannot be read.
    pub(crate) since: Option<Stamp>,
    /// THE END CLOCK (§11.6.3, item 3): the publish's own wall, as the
    /// local clock and day the numbers were counted to — stamped where the
    /// page is built (`App::harness_ab`), never where the book was; `None`
    /// where the zone cannot be read.
    pub(crate) until: Option<Stamp>,
}

impl AbWeek {
    /// The days among `days` within the week ending `today` (today and the
    /// six before), summed. A day after `today` (a clock set back) is left
    /// out: it is no part of this week yet.
    #[must_use]
    pub(crate) fn of_days<'a>(days: impl IntoIterator<Item = &'a AbDay>, today: i64) -> Self {
        let oldest = today.saturating_sub(WEEK_DAYS - 1);
        let mut sum: Option<AbDay> = None;
        let mut first: Option<(i64, i64)> = None;
        for day in days
            .into_iter()
            .filter(|d| (oldest..=today).contains(&d.day))
        {
            match sum.as_mut() {
                Some(sum) => sum.plus(day),
                None => sum = Some(*day),
            }
            first = Some(first.map_or((day.day, day.first_unix_ms), |kept| {
                kept.min((day.day, day.first_unix_ms))
            }));
        }
        sum.map_or_else(Self::default, |sum| Self {
            arms: sum.arms,
            holds: sum.holds,
            ends: sum.ends,
            first,
            since: None,
            until: None,
        })
    }

    /// An arm's time.
    #[must_use]
    pub(crate) const fn arm(&self, arm: AbArm) -> AbArmTotals {
        self.arms[arm.index()]
    }

    /// An arm's holds of `kind` (0 boxes, 1 questions); none for
    /// unsupervised time.
    #[must_use]
    pub(crate) fn hist(&self, arm: AbArm, kind: usize) -> Hist {
        arm.holds()
            .and_then(|a| self.holds.get(a))
            .and_then(|kinds| kinds.get(kind))
            .copied()
            .unwrap_or([0; BUCKETS])
    }
}

/// The working time each of Cruise and Off needs before the card leads
/// with their shares ([`ab_lead`]): 10 minutes.
pub(crate) const LEAD_FLOOR_MS: u64 = 10 * 60_000;

/// What the lead's shares measure, said above them in plain type: the
/// share of WORKING time (busy plus held) — two rows above At its prompt's
/// share of ALL the arm's time, whose total is larger — held at a box or a
/// question.
pub(crate) const LEAD_CAPTION: &str = "Working time held at a box or a question";

/// The lead where Cruise has working time and Off none.
pub(crate) const LEAD_NOTE: &str = "Set a session Off to measure plain Claude Code beside Cruise";

/// One side of the lead: its share of its working time held at a box or a
/// question (`<1%`, drawn in Hero type) and that working time (`of 6h 10m
/// on Cruise`, plain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeadShare {
    /// The share ([`share_words`]).
    pub(crate) share: String,
    /// Of how much working time, and on which side ([`duration_words`]).
    pub(crate) of: String,
}

impl LeadShare {
    /// The side as one phrase: `<1% of 6h 10m on Cruise`.
    #[must_use]
    pub(crate) fn words(&self) -> String {
        format!("{} {}", self.share, self.of)
    }
}

/// THE VERDICT UP FRONT (§11.6.1): what EFFECT OF EACH AUTOPILOT opens with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AbLead {
    /// Cruise and Off each have [`LEAD_FLOOR_MS`] of working time: each
    /// side's share held at a box or a question, beside the other.
    Both {
        /// Cruise's side.
        cruise: LeadShare,
        /// Off's side.
        off: LeadShare,
    },
    /// Cruise has working time and Off none: [`LEAD_NOTE`].
    NoOff,
}

impl AbLead {
    /// The whole lead as one line — how a screen reader says it and how a
    /// page too narrow for the Hero shares draws it: `Working time held at a
    /// box or a question: <1% of 6h 10m on Cruise · 14% of 25m off`, or the
    /// note.
    #[must_use]
    pub(crate) fn words(&self) -> String {
        match self {
            Self::Both { cruise, off } => {
                format!("{LEAD_CAPTION}: {}{JOINT}{}", cruise.words(), off.words())
            }
            Self::NoOff => LEAD_NOTE.to_string(),
        }
    }
}

/// THE LEAD'S RULE (§11.6.3, item 2): [`AbLead::Both`] where Cruise's and
/// Off's working time (busy plus held, [`AbArmTotals::working_ms`]) are
/// each at least [`LEAD_FLOOR_MS`]; [`AbLead::NoOff`] where Cruise has
/// working time and Off none; else no lead. Land is never part of it. Pure.
#[must_use]
pub(crate) fn ab_lead(week: &AbWeek) -> Option<AbLead> {
    let (cruise, off) = (week.arm(AbArm::Cruise), week.arm(AbArm::Off));
    let side = |t: AbArmTotals, on: &str| LeadShare {
        share: share_words(t.held_ms(), t.working_ms()),
        of: format!("of {} {on}", duration_words(t.working_ms())),
    };
    if cruise.working_ms() >= LEAD_FLOOR_MS && off.working_ms() >= LEAD_FLOOR_MS {
        Some(AbLead::Both {
            cruise: side(cruise, "on Cruise"),
            off: side(off, "off"),
        })
    } else if cruise.working_ms() > 0 && off.working_ms() == 0 {
        Some(AbLead::NoOff)
    } else {
        None
    }
}

/// THE EFFECT OF EACH AUTOPILOT card (§10.5.1, §11.6.1): the boxes and
/// questions (medians with n on each side), the time held as a share of
/// working time — left out where the lead says it ([`ab_lead`]'s
/// [`AbLead::Both`]) — the time at the prompt with its causes, Land on its
/// own row, the ends ([`ends_words`]), the unsupervised time, and how long
/// the book has measured, to when. Cruise is set beside Off only; Land
/// never is, since a landing ends waiting by design. Never a "time saved":
/// nothing measures one (§6). `today` is the publish's local day. Pure.
///
/// THE ROWS STAND WHERE NOTHING IS COUNTED YET (§11.6.7): the card sits
/// above SUPERVISION's switches, so a row that appeared at a background
/// event — the week's first landing, its first unsupervised minute — would
/// move the switches under a still pointer. The Ends row reads `none yet`
/// and the Unsupervised row `none` until they count something, as every
/// other row says what it has not measured.
#[must_use]
pub(crate) fn ab_rows(week: &AbWeek, today: i64) -> Vec<(String, String)> {
    let row = |label: &str, words: String| (label.to_string(), words);
    let mut rows = vec![
        row("Boxes", holds_words(week, 0, "boxes")),
        row("Questions", holds_words(week, 1, "questions")),
    ];
    if !matches!(ab_lead(week), Some(AbLead::Both { .. })) {
        rows.push(row("Held", held_words(week)));
    }
    rows.push(row("At its prompt", prompt_words(week)));
    rows.push(row("Land", land_words(week)));
    rows.push(row(
        "Ends",
        ends_words(&week.ends).unwrap_or_else(|| "none yet".to_string()),
    ));
    let unsupervised = week.arm(AbArm::Unsupervised);
    rows.push(row(
        "Unsupervised",
        if unsupervised.measured_ms() > 0 {
            format!(
                "at its prompt {} of {}, in no comparison",
                share_words(unsupervised.at_prompt_ms, unsupervised.measured_ms()),
                duration_words(unsupervised.measured_ms())
            )
        } else {
            "none".to_string()
        },
    ));
    rows.push(row("Measured", measured_words(week, today)));
    rows
}

/// One side's median: `about 2m off`, or `not enough yet (2) off` under
/// [`ENOUGH`].
fn side_words(hist: &Hist, on: &str) -> String {
    match median(hist) {
        (Some(ms), n) if n >= ENOUGH => format!("{} {on}", about_words(ms)),
        (_, n) => format!("not enough yet ({n}) {on}"),
    }
}

/// The Boxes or Questions row: Cruise's median beside Off's, and how many
/// each is of.
fn holds_words(week: &AbWeek, kind: usize, noun: &str) -> String {
    let (cruise, off) = (week.hist(AbArm::Cruise, kind), week.hist(AbArm::Off, kind));
    let (cruise_n, off_n) = (median(&cruise).1, median(&off).1);
    if cruise_n == 0 && off_n == 0 {
        return "none seen yet".to_string();
    }
    let sides = format!(
        "{} \u{b7} {}",
        side_words(&cruise, "on Cruise"),
        side_words(&off, "off")
    );
    match (cruise_n >= ENOUGH, off_n >= ENOUGH) {
        (true, true) => format!("{sides} \u{b7} median of {cruise_n} and {off_n} {noun} seen"),
        (true, false) => format!("{sides} \u{b7} median of {cruise_n} {noun} seen"),
        (false, true) => format!("{sides} \u{b7} median of {off_n} {noun} seen"),
        (false, false) => sides,
    }
}

/// The Held row: the time at a box or a question as a share of WORKING
/// time (busy plus held), Cruise beside Off.
fn held_words(week: &AbWeek) -> String {
    let (cruise, off) = (week.arm(AbArm::Cruise), week.arm(AbArm::Off));
    if cruise.working_ms() == 0 && off.working_ms() == 0 {
        return "not measured yet".to_string();
    }
    let share = |t: AbArmTotals| share_words(t.held_ms(), t.working_ms());
    let cruise_words = if cruise.working_ms() == 0 {
        "not measured yet on Cruise".to_string()
    } else {
        format!("{} of working time on Cruise", share(cruise))
    };
    let off_words = match (off.working_ms(), cruise.working_ms()) {
        (0, _) => "not measured yet off".to_string(),
        (_, 0) => format!("{} of working time off", share(off)),
        _ => format!("{} off", share(off)),
    };
    format!("{cruise_words} \u{b7} {off_words}")
}

/// The At its prompt row: how much of Cruise's and Off's time the agent
/// sat at its prompt, and for Cruise how much of its time came after the
/// agent said it was finished and how much was the harness's pause — each
/// a share of the arm's whole time.
fn prompt_words(week: &AbWeek) -> String {
    let mut parts = Vec::new();
    for (arm, label) in [(AbArm::Cruise, "Cruise"), (AbArm::Off, "Off")] {
        let t = week.arm(arm);
        let measured = t.measured_ms();
        if measured == 0 {
            continue;
        }
        let mut words = format!(
            "{label} {} of {}",
            share_words(t.at_prompt_ms, measured),
            duration_words(measured)
        );
        if arm == AbArm::Cruise {
            words.push_str(&format!(
                ": finished {}, pausing {}",
                share_words(t.finished_ms, measured),
                share_words(t.pausing_ms, measured)
            ));
        }
        parts.push(words);
    }
    if parts.is_empty() {
        "not measured yet".to_string()
    } else {
        parts.join(" \u{b7} ")
    }
}

/// Land's own row: held, at its prompt and finished as shares of its time
/// (pausing where it paused), and its boxes' median. Never set beside
/// Cruise.
fn land_words(week: &AbWeek) -> String {
    let t = week.arm(AbArm::Land);
    let measured = t.measured_ms();
    if measured == 0 {
        return "not measured yet".to_string();
    }
    let mut words = format!(
        "held {} \u{b7} at its prompt {}, finished {}",
        share_words(t.held_ms(), measured),
        share_words(t.at_prompt_ms, measured),
        share_words(t.finished_ms, measured)
    );
    if t.pausing_ms > 0 {
        words.push_str(&format!(
            ", pausing {}",
            share_words(t.pausing_ms, measured)
        ));
    }
    match median(&week.hist(AbArm::Land, 0)) {
        (Some(ms), n) if n >= ENOUGH => {
            words.push_str(&format!(" \u{b7} boxes {} ({n})", about_words(ms)))
        }
        (_, 0) => {}
        (_, n) => words.push_str(&format!(" \u{b7} boxes not enough yet ({n})")),
    }
    words
}

/// A count of times as words: `once`, else `6 times`.
fn times_words(n: u32) -> String {
    if n == 1 {
        "once".to_string()
    } else {
        format!("{n} times")
    }
}

/// THE ENDS ROW (§11.6.1): its three parts, each only where it counted
/// anything — Cruise's dones with the work floor's words (only ends past
/// it count), Land's landings and quiet ends, and the median time from
/// Land to landed with its n (`Land to landed: about 6m (5)`; `Land to
/// landed: not enough yet (2)` under [`ENOUGH`]) — `None` with nothing to
/// count (the row then reads `none yet`, [`ab_rows`]). No Off side: Off has
/// no done reader.
#[must_use]
pub(crate) fn ends_words(ends: &AbEnds) -> Option<String> {
    let mut parts = Vec::new();
    if ends.done > 0 {
        parts.push(format!(
            "Cruise said done {} ({} s of work or more)",
            times_words(ends.done),
            crate::harness_task::END_WORK_MS / 1_000
        ));
    }
    match (ends.landed, ends.quiet) {
        (0, 0) => {}
        (landed, 0) => parts.push(format!("Land landed {landed}")),
        (0, quiet) => parts.push(format!("Land quiet {quiet}")),
        (landed, quiet) => parts.push(format!("Land landed {landed}, quiet {quiet}")),
    }
    match median(&ends.landing) {
        (_, 0) => {}
        (Some(ms), n) if n >= ENOUGH => {
            parts.push(format!("Land to landed: {} ({n})", about_words(ms)));
        }
        (_, n) => parts.push(format!("Land to landed: not enough yet ({n})")),
    }
    (!parts.is_empty()).then(|| parts.join(JOINT))
}

/// The Measured row: how many local days the week holds, since when, and
/// to when — the publish's clock ([`AbWeek::until`]): `7 days, since Mon
/// 09:12, to 14:05`; `today, since 09:12, to 14:05`.
fn measured_words(week: &AbWeek, today: i64) -> String {
    let Some((first_day, _)) = week.first else {
        return "nothing yet".to_string();
    };
    let days = today.saturating_sub(first_day).saturating_add(1).max(1);
    let since = week.since.map(|stamp| stamp.words(Some(today)));
    let mut words = match (days, since) {
        (1, Some(since)) => format!("today, since {since}"),
        (1, None) => "today".to_string(),
        (days, Some(since)) => format!("{days} days, since {since}"),
        (days, None) => format!("{days} days"),
    };
    if let Some(until) = week.until {
        words.push_str(&format!(", to {}", until.words(Some(today))));
    }
    words
}

#[cfg(test)]
#[path = "harness_ab_tests.rs"]
mod tests;
