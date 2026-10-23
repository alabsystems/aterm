// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A/B telemetry book tracking supervised agent time and hold durations across Autopilot arms ([`AbBook`], [`AbWeek`]).

use std::collections::HashMap;

use crate::autopilot::AutopilotView;
use crate::harness_glass::Stamp;
use crate::harness_screen::{duration_words, local_midnight_ms, share_words};
use crate::native_ui::FACT_SEPARATOR;

/// Number of local days retained in the ring buffer.
pub(crate) const DAYS: usize = 8;

/// Number of local days summed on the Harness page (`today` and the six prior days).
pub(crate) const WEEK_DAYS: i64 = 7;

/// Number of doubling histogram buckets (`250 ms` base).
pub(crate) const BUCKETS: usize = 20;

/// Minimum sample count required to report a histogram median.
pub(crate) const ENOUGH: u32 = 3;

/// The first bucket's floor, and the doubling's unit.
const BUCKET_MS: u64 = 250;

/// Hold duration histogram with counts per [`bucket`].
pub(crate) type Hist = [u32; BUCKETS];

/// Hold histograms indexed by `[Cruise, Land, Off][box, question]`.
pub(crate) type Holds = [[Hist; 2]; 3];

/// Ring buffer of local days ([`AbBook`]) indexed by `day % DAYS`.
pub(crate) type AbRing = [Option<AbDay>; DAYS];

/// Autopilot A/B arm under which a stretch of agent time ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbArm {
    /// Autopilot Cruise under this host's live claim.
    Cruise,
    /// Autopilot Land under this host's live claim.
    Land,
    /// Autopilot Off under this host's live claim (baseline).
    Off,
    /// Unclaimed or unattached time.
    Unsupervised,
}

impl AbArm {
    /// Determines the [`AbArm`] from `view`, `supervised`, and `claimed`.
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

    /// Index of this arm in [`AbDay::arms`].
    #[must_use]
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Cruise => 0,
            Self::Land => 1,
            Self::Off => 2,
            Self::Unsupervised => 3,
        }
    }

    /// Index of this arm in [`Holds`], or `None` for [`Self::Unsupervised`].
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

/// Published agent phase counted by the A/B book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbPhase {
    /// `busy`: turn or background work running.
    Busy,
    /// `prompt`: permission box standing.
    AtBox,
    /// `question`: agent waiting on a user question.
    Asked,
    /// `idle`: agent sitting at its prompt.
    AtPrompt,
}

impl AbPhase {
    /// Maps a published agent word (`AgentPublication::word`) to an [`AbPhase`].
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

    /// Hold kind index in [`Holds`] (`0` box, `1` question), if this phase is a hold.
    #[must_use]
    pub(crate) const fn held(self) -> Option<usize> {
        match self {
            Self::AtBox => Some(0),
            Self::Asked => Some(1),
            Self::Busy | Self::AtPrompt => None,
        }
    }
}

/// Reason an agent under Cruise or Land sat at its prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AbCause {
    /// Work landed under Land (`face == landed`).
    Landed,
    /// Harness pausing after consecutive short turns.
    Pausing,
}

impl AbCause {
    /// Determines the [`AbCause`] for `phase` under `arm` from the session's end and pause state.
    #[must_use]
    pub(crate) fn of(
        arm: AbArm,
        phase: Option<AbPhase>,
        landed: bool,
        pausing: bool,
    ) -> Option<Self> {
        if phase != Some(AbPhase::AtPrompt) || !matches!(arm, AbArm::Cruise | AbArm::Land) {
            return None;
        }
        if landed && arm == AbArm::Land {
            Some(Self::Landed)
        } else if pausing {
            Some(Self::Pausing)
        } else {
            None
        }
    }
}

/// Cumulative milliseconds spent in each phase for one arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbArmTotals {
    /// Milliseconds spent busy.
    pub(crate) busy_ms: u64,
    /// Milliseconds spent at a permission box.
    pub(crate) at_box_ms: u64,
    /// Milliseconds spent at a question.
    pub(crate) asked_ms: u64,
    /// Milliseconds spent at the prompt.
    pub(crate) at_prompt_ms: u64,
    /// Subset of `at_prompt_ms` after work landed under Land.
    pub(crate) landed_ms: u64,
    /// Subset of `at_prompt_ms` spent in short-turn pause.
    pub(crate) pausing_ms: u64,
}

impl AbArmTotals {
    /// Total milliseconds measured across all phases.
    #[must_use]
    pub(crate) const fn measured_ms(self) -> u64 {
        self.busy_ms
            .saturating_add(self.at_box_ms)
            .saturating_add(self.asked_ms)
            .saturating_add(self.at_prompt_ms)
    }

    /// Milliseconds held waiting for the person (`at_box_ms + asked_ms`).
    #[must_use]
    pub(crate) const fn held_ms(self) -> u64 {
        self.at_box_ms.saturating_add(self.asked_ms)
    }

    /// Working milliseconds (`busy_ms + held_ms`).
    #[must_use]
    pub(crate) const fn working_ms(self) -> u64 {
        self.busy_ms.saturating_add(self.held_ms())
    }

    const fn add(&mut self, phase: AbPhase, cause: Option<AbCause>, ms: u64) {
        let bucket = match phase {
            AbPhase::Busy => &mut self.busy_ms,
            AbPhase::AtBox => &mut self.at_box_ms,
            AbPhase::Asked => &mut self.asked_ms,
            AbPhase::AtPrompt => &mut self.at_prompt_ms,
        };
        *bucket = bucket.saturating_add(ms);
        let of_prompt = match cause {
            Some(AbCause::Landed) => &mut self.landed_ms,
            Some(AbCause::Pausing) => &mut self.pausing_ms,
            None => return,
        };
        *of_prompt = of_prompt.saturating_add(ms);
    }

    const fn plus(&mut self, other: Self) {
        self.busy_ms = self.busy_ms.saturating_add(other.busy_ms);
        self.at_box_ms = self.at_box_ms.saturating_add(other.at_box_ms);
        self.asked_ms = self.asked_ms.saturating_add(other.asked_ms);
        self.at_prompt_ms = self.at_prompt_ms.saturating_add(other.at_prompt_ms);
        self.landed_ms = self.landed_ms.saturating_add(other.landed_ms);
        self.pausing_ms = self.pausing_ms.saturating_add(other.pausing_ms);
    }
}

/// Per-day task landing counts and Land-to-landed duration histogram.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbEnds {
    /// Number of tasks landed under Land.
    pub(crate) landed: u32,
    /// Histogram of elapsed times from entering Land to landing.
    pub(crate) landing: Hist,
}

impl AbEnds {
    /// Adds `other`'s landing counts and histogram into `self`.
    pub(crate) fn plus(&mut self, other: &Self) {
        self.landed = self.landed.saturating_add(other.landed);
        for (count, add) in self.landing.iter_mut().zip(other.landing) {
            *count = count.saturating_add(add);
        }
    }
}

/// Single local day of A/B totals, hold histograms, and task ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AbDay {
    /// Local day number (days since `1970-01-01`).
    pub(crate) day: i64,
    /// Wall-clock timestamp (unix ms) when the day's first stretch began.
    pub(crate) first_unix_ms: i64,
    /// Per-arm phase totals indexed by [`AbArm::index`].
    pub(crate) arms: [AbArmTotals; 4],
    /// Per-arm hold histograms ([`Holds`]).
    pub(crate) holds: Holds,
    /// Landed task counts and durations ([`AbEnds`]).
    pub(crate) ends: AbEnds,
}

impl AbDay {
    /// Creates an empty [`AbDay`] for `day` starting at `first_unix_ms`.
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

    /// Merges `other` into `self` for the same local day.
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

/// Histogram bucket index (`0..BUCKETS`) for duration `ms`.
#[must_use]
pub(crate) fn bucket(ms: u64) -> usize {
    let log = (ms / BUCKET_MS).max(1).ilog2();
    usize::try_from(log).map_or(BUCKETS - 1, |b| b.min(BUCKETS - 1))
}

/// A bucket's geometric centre: `250 ms × 2ᵇ × √2`.
fn centre_ms(bucket: usize) -> u64 {
    (BUCKET_MS << bucket.min(BUCKETS - 1)) * 1_414 / 1_000
}

/// Geometric-center median (ms) and total sample count of `hist`.
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

/// Formats a median duration `ms` as `about …`.
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

/// Mapping between monotonic ledger timestamps and local calendar days.
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
    /// Computes the [`AbCalendar`] at `ledger_ms` and wall time `unix_ms`.
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

    /// Current local day number.
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
    /// that day ends.
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
/// day; `first_unix_ms` is kept where it is the day's earliest.
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

impl AbClock {
    /// Whether this stretch is THE TASK'S WORK (§11.2.3, item 2): the
    /// agent busy under any arm but [`AbArm::Off`].
    const fn is_task_work(&self) -> bool {
        matches!(self.phase, AbPhase::Busy) && !matches!(self.arm, AbArm::Off)
    }
}

/// Per-session task work and Land entry tracking for [`AbBook`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AbTask {
    /// Whether the session's mode was Land when the book last read it;
    /// `None` until the book first reads it ([`AbBook::follow_mode`]).
    lands: Option<bool>,
    /// The ledger instant the session's mode moved into Land, as the book
    /// read it move: where the time from Land to landed starts.
    land_from: Option<u64>,
    /// The agent's busy time charged since [`Self::from_ms`], ledger ms.
    worked_ms: u64,
    /// The ledger instant the task opened ([`AbBook::task_opened`]); 0 for
    /// a session no move from a known face has opened a task in yet (its
    /// first task, a seamless update's successor, a cold restore): every
    /// busy piece the book saw counts.
    from_ms: u64,
}

impl AbTask {
    /// The task of a session the book has just begun to follow: no mode
    /// read, no work, open since the book first saw it.
    const fn new() -> Self {
        Self {
            lands: None,
            land_from: None,
            worked_ms: 0,
            from_ms: 0,
        }
    }

    /// The session's mode is Land (`lands`) or not, read at `at_ms`. A mode
    /// first read as Land has no start.
    const fn mode(&mut self, lands: bool, at_ms: u64) {
        self.land_from = match (self.lands, lands) {
            (Some(true), true) => self.land_from,
            (Some(false), true) => Some(at_ms),
            (None, true) | (_, false) => None,
        };
        self.lands = Some(lands);
    }

    /// The session's mode is Land (`lands`) or not as CARRIED from before
    /// this process ([`AbBook::carry_mode`]): read as a first read is, with
    /// no start; the work is kept.
    const fn carried(&mut self, lands: bool) {
        self.lands = Some(lands);
        self.land_from = None;
    }

    /// A busy piece `from..to` was charged: its part after the task opened
    /// is the task's work.
    fn worked(&mut self, from: u64, to: u64) {
        let ms = to.saturating_sub(from.max(self.from_ms));
        self.worked_ms = self.worked_ms.saturating_add(ms);
    }
}

/// Per-process A/B telemetry book across sessions and local days.
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
    /// Whether a calendar has been initialized on this book.
    #[must_use]
    pub(crate) const fn has_calendar(&self) -> bool {
        self.calendar.is_some()
    }

    /// Updates the book's calendar, charging open stretches up to the new reading.
    pub(crate) fn set_calendar(&mut self, calendar: AbCalendar) {
        let at = calendar.ledger_ms;
        if let Some(old) = self.calendar.as_mut() {
            for (session, open) in &mut self.clocks {
                if at > open.since_ms {
                    charge(
                        &mut self.ring,
                        old,
                        (open.arm, open.phase, open.cause),
                        open.since_ms,
                        at,
                    );
                    if open.is_task_work() {
                        self.tasks
                            .entry(*session)
                            .or_insert_with(AbTask::new)
                            .worked(open.since_ms, at);
                    }
                    open.since_ms = at;
                }
            }
        }
        self.calendar = Some(calendar);
    }

    /// Records that `session` is in `phase` under `arm` (with `cause`) at `at_ms`.
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
            // The task's work: the busy piece's part after the task opened,
            // kept apart from the clock, so the word that drops the clock (a
            // wall's) keeps it; never under Off (`AbClock::is_task_work`).
            if open.is_task_work() {
                self.tasks
                    .entry(session)
                    .or_insert_with(AbTask::new)
                    .worked(open.since_ms, at_ms);
            }
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

    /// Settles and removes `session`'s open stretch and task state at `at_ms`.
    pub(crate) fn retire(&mut self, session: u64, at_ms: u64) {
        if let Some(open) = self.clocks.get(&session).copied() {
            self.observe(session, None, None, open.arm, at_ms);
        }
        self.tasks.remove(&session);
    }

    /// Updates whether `session`'s effective mode is Land at `at_ms`.
    pub(crate) fn follow_mode(&mut self, session: u64, lands: bool, at_ms: u64) {
        self.tasks
            .entry(session)
            .or_insert_with(AbTask::new)
            .mode(lands, at_ms);
    }

    /// Busy milliseconds accumulated on `session`'s current task as of `now_ms`.
    #[must_use]
    pub(crate) fn worked(&self, session: u64, now_ms: u64) -> u64 {
        let Some(task) = self.tasks.get(&session) else {
            return 0;
        };
        let open = self
            .clocks
            .get(&session)
            .filter(|open| open.is_task_work())
            .map_or(0, |open| {
                now_ms.saturating_sub(open.since_ms.max(task.from_ms))
            });
        task.worked_ms.saturating_add(open)
    }

    /// Resets `session`'s task work counter when its task reopens at `at_ms`.
    pub(crate) fn task_opened(&mut self, session: u64, at_ms: u64) {
        let task = self.tasks.entry(session).or_insert_with(AbTask::new);
        task.worked_ms = 0;
        task.from_ms = at_ms;
        if task.lands == Some(true) {
            task.land_from = Some(at_ms);
        }
    }

    /// Seeds `session`'s carried task work (`worked_ms`) across a seamless update.
    pub(crate) fn carry_work(&mut self, session: u64, worked_ms: u64) {
        let task = self.tasks.entry(session).or_insert_with(AbTask::new);
        if task.from_ms == 0 {
            task.worked_ms = task.worked_ms.saturating_add(worked_ms);
        }
    }

    /// Seeds whether `session`'s mode was carried as Land (`lands`).
    pub(crate) fn carry_mode(&mut self, session: u64, lands: bool) {
        self.tasks
            .entry(session)
            .or_insert_with(AbTask::new)
            .carried(lands);
    }

    /// Records that `session`'s task landed at `at_ms`.
    pub(crate) fn landed(&mut self, session: u64, at_ms: u64) {
        let span = self
            .tasks
            .get_mut(&session)
            .and_then(|task| task.land_from.take());
        let Some(calendar) = self.calendar.as_mut() else {
            return;
        };
        let (day, _) = calendar.day_at(at_ms);
        let first = calendar.unix_at(at_ms);
        let Some(kept) = slot(&mut self.ring, day, first, calendar.today) else {
            return;
        };
        let ends = &mut kept.ends;
        ends.landed = ends.landed.saturating_add(1);
        if let Some(from) = span {
            let sample = &mut ends.landing[bucket(at_ms.saturating_sub(from))];
            *sample = sample.saturating_add(1);
        }
    }

    /// Ledger timestamp when `session` entered Land for its current task, if known.
    #[cfg(test)]
    pub(crate) fn land_from(&self, session: u64) -> Option<u64> {
        self.tasks.get(&session).and_then(|task| task.land_from)
    }

    /// Number of sessions whose tasks the book currently tracks.
    #[cfg(test)]
    pub(crate) fn tasks(&self) -> usize {
        self.tasks.len()
    }

    /// Open stretch `(phase, arm, cause)` for `session`, if any.
    #[cfg(test)]
    pub(crate) fn stretch(&self, session: u64) -> Option<(AbPhase, AbArm, Option<AbCause>)> {
        self.clocks
            .get(&session)
            .map(|open| (open.phase, open.arm, open.cause))
    }

    /// Number of sessions with an open stretch.
    #[cfg(test)]
    pub(crate) fn open_stretches(&self) -> usize {
        self.clocks.len()
    }

    /// Returns a snapshot of the day ring with open stretches charged up to `now_ms`.
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

    /// Sums the 7-day [`AbWeek`] ending at `now_ms` across this book and `others`.
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

/// Seven-day summed A/B totals, hold histograms, and task ends for the Harness page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct AbWeek {
    /// Per-arm phase totals indexed by [`AbArm::index`].
    pub(crate) arms: [AbArmTotals; 4],
    /// Per-arm hold histograms.
    pub(crate) holds: Holds,
    /// Summed task landings ([`AbEnds`]).
    pub(crate) ends: AbEnds,
    /// Earliest included `(day, first_unix_ms)`, or `None` when nothing was measured.
    pub(crate) first: Option<(i64, i64)>,
    /// Local [`Stamp`] of the earliest included measurement.
    pub(crate) since: Option<Stamp>,
    /// Local [`Stamp`] when the week summary was published.
    pub(crate) until: Option<Stamp>,
}

impl AbWeek {
    /// Sums days within the [`WEEK_DAYS`] window ending on `today`.
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

    /// Phase totals for `arm`.
    #[must_use]
    pub(crate) const fn arm(&self, arm: AbArm) -> AbArmTotals {
        self.arms[arm.index()]
    }

    /// Hold histogram for `arm` and `kind` (`0` boxes, `1` questions).
    #[must_use]
    pub(crate) fn hist(&self, arm: AbArm, kind: usize) -> Hist {
        arm.holds()
            .and_then(|a| self.holds.get(a))
            .and_then(|kinds| kinds.get(kind))
            .copied()
            .unwrap_or([0; BUCKETS])
    }
}

/// Minimum working time (`10m`) required on both Cruise and Off before showing [`AbLead::Both`].
pub(crate) const LEAD_FLOOR_MS: u64 = 10 * 60_000;

/// Caption displayed above the Hero lead shares.
pub(crate) const LEAD_CAPTION: &str = "Working time held at a box or a question";

/// Note shown when Cruise has working time but Off has none.
pub(crate) const LEAD_NOTE: &str = "Set a session Off to measure plain Claude Code beside Cruise";

/// Formatted hold share (`<1%`) and working-time denominator for one arm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeadShare {
    /// Formatted percentage share ([`share_words`]).
    pub(crate) share: String,
    /// Formatted working-time denominator (`of 6h 10m on Cruise`).
    pub(crate) of: String,
}

impl LeadShare {
    /// Formats `<share> <of>` as a single phrase.
    #[must_use]
    pub(crate) fn words(&self) -> String {
        format!("{} {}", self.share, self.of)
    }
}

/// Lead comparison headline for the EFFECT OF EACH AUTOPILOT card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AbLead {
    /// Both Cruise and Off have at least [`LEAD_FLOOR_MS`] of working time.
    Both {
        /// Cruise lead share.
        cruise: LeadShare,
        /// Off lead share.
        off: LeadShare,
    },
    /// Cruise has working time and Off has none ([`LEAD_NOTE`]).
    NoOff,
}

impl AbLead {
    /// Formats the lead comparison as a single line.
    #[must_use]
    pub(crate) fn words(&self) -> String {
        match self {
            Self::Both { cruise, off } => {
                format!(
                    "{LEAD_CAPTION}: {}{FACT_SEPARATOR}{}",
                    cruise.words(),
                    off.words()
                )
            }
            Self::NoOff => LEAD_NOTE.to_string(),
        }
    }
}

/// Computes the [`AbLead`] headline for `week`, if any.
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

/// Formats the key-value rows of the EFFECT OF EACH AUTOPILOT card for `week`.
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
/// sat at its prompt, and for Cruise how much was the harness's pause — a
/// share of the arm's whole time (Cruise never lands, so it has no landed
/// share).
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
                ": pausing {}",
                share_words(t.pausing_ms, measured)
            ));
        }
        parts.push(words);
    }
    if parts.is_empty() {
        "not measured yet".to_string()
    } else {
        parts.join(FACT_SEPARATOR)
    }
}

/// Land's own row: held, at its prompt and landed as shares of its time
/// (pausing where it paused), and its boxes' median.
fn land_words(week: &AbWeek) -> String {
    let t = week.arm(AbArm::Land);
    let measured = t.measured_ms();
    if measured == 0 {
        return "not measured yet".to_string();
    }
    let mut words = format!(
        "held {} \u{b7} at its prompt {}, landed {}",
        share_words(t.held_ms(), measured),
        share_words(t.at_prompt_ms, measured),
        share_words(t.landed_ms, measured)
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

/// Formats the `Tasks landed` summary row for `ends`, if non-empty.
#[must_use]
pub(crate) fn ends_words(ends: &AbEnds) -> Option<String> {
    let mut parts = Vec::new();
    if ends.landed > 0 {
        parts.push(format!("Land landed {}", ends.landed));
    }
    match median(&ends.landing) {
        (_, 0) => {}
        (Some(ms), n) if n >= ENOUGH => {
            parts.push(format!("Land to landed: {} ({n})", about_words(ms)));
        }
        (_, n) => parts.push(format!("Land to landed: not enough yet ({n})")),
    }
    (!parts.is_empty()).then(|| parts.join(FACT_SEPARATOR))
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

#[cfg(test)]
#[path = "harness_ab_work_tests.rs"]
mod work_tests;
