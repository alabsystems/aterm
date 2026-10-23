// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session harness glass state for the Autopilot chip, act trail, and Land task face ([`SessionGlass`]).

use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, HoldLeg, LandHeld, TaskFace};

use crate::App;
use crate::claude_lights::{ChipGlass, Clock, Weekday};

/// How long a label stays lit from the first act of its burst.
pub(crate) const DWELL: Duration = Duration::from_secs(3);

/// Minimum interval after a label begins before another lights.
pub(crate) const QUIET: Duration = Duration::from_secs(15);

/// Window within which repeated acts coalesce into one trail entry.
pub(crate) const BURST: Duration = Duration::from_secs(9);

/// Maximum number of acts retained in the trail, newest first.
pub(crate) const TRAIL: usize = 3;

/// Minimum interval between ledger re-reads while acts arrive.
pub(crate) const REREAD: Duration = Duration::from_secs(10);

/// How old a trail entry may be and still be listed in the title: older
/// ones are left out where the title is built.
const TRAIL_AGE_S: i64 = 86_400;

/// When an act arrived, read ONCE at its receipt (§9.2.3, item 4): the
/// monotonic instant its dwell is measured from, the wall second its clock
/// time and date are formatted from, and the zone's offset there (`None`
/// where the platform cannot say: the clock reads in UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Monotonic and wall-clock arrival timestamps for a received act.
pub(crate) struct Arrival {
    pub(crate) now: Instant,
    pub(crate) unix: i64,
    pub(crate) offset_s: Option<i64>,
}

impl Arrival {
    /// The clocks now: one monotonic read and one wall read
    /// (`work_counts::harness_clock_read` counts the pair as one).
    fn now() -> Self {
        #[cfg(test)]
        crate::work_counts::harness_clock_read();
        let now = Instant::now();
        let unix = wall_unix_s();
        Self {
            now,
            unix,
            offset_s: crate::presence::local_offset_at(unix),
        }
    }

    /// The local day number (days since 1970-01-01, local time).
    fn day(self) -> i64 {
        (self.unix + self.offset_s.unwrap_or(0)).div_euclid(86_400)
    }

    /// The local clock time.
    fn clock(self) -> Clock {
        let minute = (self.unix + self.offset_s.unwrap_or(0)).rem_euclid(86_400) / 60;
        Clock::of_minute(u16::try_from(minute).unwrap_or(0))
    }
}

/// A FACE'S CLOCK WITH ITS DAY (design §10.2.3, item 3): the local `HH:MM`
/// a fact arrived at — a task's face moving, an act of the trail — and the
/// local day number it fell on, both read once where the fact arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Local clock time and day number when a fact arrived.
pub(crate) struct Stamp {
    /// Local clock time.
    pub(crate) clock: Clock,
    /// Local day number (days since 1970-01-01, local time).
    pub(crate) day: i64,
}

impl Stamp {
    /// The stamp of unix second `at_unix`, in the zone's offset at that
    /// instant (`presence::local_offset_at`); `None` where the platform
    /// cannot say.
    #[must_use]
    /// Stamp for unix second `at_unix` in its local time-zone offset, or `None` when not positive.
    pub(crate) fn at(at_unix: i64) -> Option<Self> {
        let offset = crate::presence::local_offset_at(at_unix)?;
        let local = at_unix + offset;
        let minute = local.rem_euclid(86_400) / 60;
        Some(Self {
            clock: Clock::of_minute(u16::try_from(minute).unwrap_or(0)),
            day: local.div_euclid(86_400),
        })
    }

    /// THE WALL TIME (unix ms) ITS MINUTE STARTS AT, [`Self::at`]'s inverse
    /// to the minute: the local minute and day placed by the zone's offset
    /// at that instant, read twice, so a minute within the zone's offset of
    /// a change of offset is placed by the offset it fell under (one read,
    /// at the local reading taken as an instant, places it an hour off
    /// there); `None` where the platform cannot say.
    #[must_use]
    /// Wall time in unix milliseconds when this stamp's minute starts ([`Self::at`]'s inverse to the minute).
    pub(crate) fn unix_ms(self) -> Option<i64> {
        let local = self
            .day
            .checked_mul(86_400)?
            .checked_add(i64::from(self.clock.minute()) * 60)?;
        let guess = local.checked_sub(crate::presence::local_offset_at(local)?)?;
        local
            .checked_sub(crate::presence::local_offset_at(guess)?)?
            .checked_mul(1000)
    }

    /// The day it fell on as a builder names it against `today` (§10.2.3,
    /// item 3): nothing on `today` itself, or with no day read for the build
    /// (`None`: the clock stands alone); its weekday within the six days
    /// before (`Fri`), the one stretch where a weekday cannot be read as
    /// another; else its date (`9 Oct`, `9 Oct 2025` from another year) —
    /// a week or more back, where a weekday would name a later day (today's
    /// own, on the seventh), or after `today` (a clock set back).
    #[must_use]
    /// Relative or calendar day label against `today`, or `None` on `today` itself.
    pub(crate) fn day_words(self, today: Option<i64>, long: bool) -> Option<String> {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let today = today.filter(|today| *today != self.day)?;
        if (1..=6).contains(&today.saturating_sub(self.day)) {
            let day = Weekday::of_days(self.day);
            return Some(if long { day.long() } else { day.short() }.to_owned());
        }
        let (date, now) = (
            crate::autopilot_timers::Date::of_days(self.day),
            crate::autopilot_timers::Date::of_days(today),
        );
        let month = MONTHS[usize::from(date.month.saturating_sub(1)) % 12];
        let month = if long { month } else { &month[..3] };
        Some(if date.year == now.year {
            format!("{} {month}", date.day)
        } else {
            format!("{} {month} {}", date.day, date.year)
        })
    }

    /// `18:11`, `Fri 18:11` within the six days before `today`, or
    /// `9 Oct 18:11` further off ([`Self::day_words`]).
    #[must_use]
    /// Compact clock and day label against `today` ([`Self::day_words`]).
    pub(crate) fn words(self, today: Option<i64>) -> String {
        match self.day_words(today, false) {
            Some(day) => format!("{day} {}", self.clock.as_str()),
            None => self.clock.as_str().to_owned(),
        }
    }

    /// `at 18:11`, `on Friday at 18:11` within the six days before `today`,
    /// or `on 9 October at 18:11` further off: as a screen reader says it.
    #[must_use]
    /// Screen-reader phrasing of this stamp against `today`.
    pub(crate) fn spoken(self, today: Option<i64>) -> String {
        match self.day_words(today, true) {
            Some(day) => format!("on {day} at {}", self.clock.as_str()),
            None => format!("at {}", self.clock.as_str()),
        }
    }
}

/// THE LOOP'S PAUSE ON THE GLASS (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §10.5.3, item 4; [`on_pause`]): when it ends, as the local clock and day
/// its unix second falls on — read once, where the fact arrived — and how
/// many short turns in a row it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Loop pause state displayed on the glass ([`on_pause`]).
pub(crate) struct PauseGlass {
    /// When the pause ends.
    pub(crate) until: Stamp,
    /// Consecutive short turns preceding the pause.
    pub(crate) short: u8,
}

impl PauseGlass {
    /// The glass of `plan`: its end's clock and day in the zone's offset at
    /// that instant, UTC's where the zone cannot be read (the trail's rule).
    #[must_use]
    /// Builds pause glass state from `plan`.
    pub(crate) fn of(plan: crate::PausePlan) -> Self {
        let until = Stamp::at(plan.at_unix).unwrap_or_else(|| {
            let minute = plan.at_unix.rem_euclid(86_400) / 60;
            Stamp {
                clock: Clock::of_minute(u16::try_from(minute).unwrap_or(0)),
                day: plan.at_unix.div_euclid(86_400),
            }
        });
        Self {
            until,
            short: plan.short,
        }
    }

    /// When it ends as a title says it: `18:41`, and on another local day
    /// than `today` (a pause past midnight) its weekday, `Sat 00:12`, as an
    /// armed timer names a later day; the clock alone with no day read for
    /// the build (`None`).
    #[must_use]
    /// Title phrasing of when the pause ends against `today`.
    pub(crate) fn until_words(self, today: Option<i64>) -> String {
        match today.filter(|today| *today != self.until.day) {
            Some(_) => format!(
                "{} {}",
                Weekday::of_days(self.until.day).short(),
                self.until.clock.as_str()
            ),
            None => self.until.clock.as_str().to_owned(),
        }
    }

    /// The chip's title's part (§10.5.1): `pausing until 18:41 after 3
    /// short turns`, `… after a short turn` for one.
    #[must_use]
    /// Chip title fragment describing the pause against `today`.
    pub(crate) fn title_words(self, today: Option<i64>) -> String {
        let until = self.until_words(today);
        match self.short {
            0 | 1 => format!("pausing until {until} after a short turn"),
            n => format!("pausing until {until} after {n} short turns"),
        }
    }

    /// Settings ▸ Harness's state line (§10.5.1): `pausing until 18:41 (3
    /// short turns in a row)`, `(a short turn)` for one.
    #[must_use]
    /// Settings state line describing the pause against `today`.
    pub(crate) fn state_words(self, today: Option<i64>) -> String {
        let until = self.until_words(today);
        match self.short {
            0 | 1 => format!("pausing until {until} (a short turn)"),
            n => format!("pausing until {until} ({n} short turns in a row)"),
        }
    }

    /// The Autopilot node's description's lead, as a screen reader says it
    /// (§10.5.1): `Pausing until 18:41.`, on another local day `Pausing
    /// until Saturday at 00:12.`
    #[cfg_attr(
        not(any(test, a11y_tree)),
        allow(
            dead_code,
            reason = "the chips' accessibility nodes describe it; the tree is built only under \
                      the `a11y_tree` cfg (Linux, macOS and Windows)"
        )
    )]
    #[must_use]
    /// Screen-reader lead describing the pause against `today`.
    pub(crate) fn spoken(self, today: Option<i64>) -> String {
        match today.filter(|today| *today != self.until.day) {
            Some(_) => format!(
                "Pausing until {} at {}.",
                Weekday::of_days(self.until.day).long(),
                self.until.clock.as_str()
            ),
            None => format!("Pausing until {}.", self.until.clock.as_str()),
        }
    }
}

/// The local day number at wall second `unix` (the trail's rule: UTC's day
/// where the zone cannot be read).
#[must_use]
/// Local day number at wall second `unix` (falling back to UTC when the zone cannot be read).
pub(crate) fn local_day_at(unix: i64) -> i64 {
    (unix + crate::presence::local_offset_at(unix).unwrap_or(0)).div_euclid(86_400)
}

/// The wall clock, unix seconds.
fn wall_unix_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// One act of the trail: the act, and when it arrived — its monotonic
/// instant, and that of its last repeat (a repeat within the dwell of it is
/// no new entry), its local clock time and day, formatted at receipt, and
/// its wall second (an entry older than a day is left out of the title).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Single act in the trail with its arrival and repeat timestamps.
pub(crate) struct TrailEntry {
    pub(crate) act: Act,
    pub(crate) at: Instant,
    pub(crate) repeated: Instant,
    pub(crate) clock: Clock,
    pub(crate) day: i64,
    pub(crate) unix: i64,
}

/// What one act moved on the glass ([`SessionGlass::take`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Glass state changes caused by [`SessionGlass::take`].
pub(crate) struct Took {
    /// Whether the lit label lit or was replaced in place.
    pub(crate) lit: bool,
    /// Whether the trail recorded the act.
    pub(crate) trail: bool,
}

impl Took {
    /// Whether any surface read by the chip, title, or page changed.
    pub(crate) fn any(self) -> bool {
        self.lit || self.trail
    }
}

/// One session's harness glass, as the frame reads it ([`Self::chip`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Per-session harness glass state read by the frame ([`Self::chip`]).
pub(crate) struct SessionGlass {
    /// The task's face as the session's loop last told it (§9.4.2), and the
    /// local clock time and day it moved there (§10.2.3, item 3).
    task: Option<(TaskFace, Option<Stamp>)>,
    /// The last [`TRAIL`] acts, newest first.
    trail: [Option<TrailEntry>; TRAIL],
    /// Moves with every act the trail takes, and with a task face's clock
    /// that moves under the same face ([`on_task`]): a title someone reads
    /// is built again ([`ChipGlass::trail`], its low byte).
    trail_gen: u64,
    /// The loop's pause after short turns (§10.5.3, item 4), while one
    /// stands.
    pause: Option<PauseGlass>,
    /// The label lit now, and the instant its dwell ends.
    lit: Option<(Act, Instant)>,
    /// Nothing lights again before this: [`QUIET`] after the last label
    /// began.
    quiet_until: Option<Instant>,
    /// THE TASK'S END (§11.2.3, item 4), while it stands: told once by
    /// `App::note_harness_end` (`harness_task::is_a_task_end`), KEPT across
    /// the host's "unknown" (`on_task(None)`, so it never flickers between
    /// loop runs) and cleared at a move to a known face that is no end —
    /// open, theirs ([`on_task`]): an end never outlives the work it
    /// names.
    end: Option<EndGlass>,
    /// How many ends this session told: the next end's [`EndGlass::serial`].
    ends: u32,
    /// WHY LAND STILL HOLDS (§12.3.3 item 3), as the session's
    /// loop last told it ([`on_land_held`]), with its `since` stamped once:
    /// what the SESSIONS state line, the tab's hover and the Overview, the
    /// menu bar's worker row and the chip's title (the W4 title slice) say
    /// of a landing that has not landed — each only while the session's
    /// Autopilot is Land and its chip wears `⤓ land` ([`App::shown_hold`]).
    hold: Option<HeldGlass>,
}

/// THE KIND OF A TASK'S END (§11.2.3, item 1): under Autopilot Cruise /
/// Autopilot Land only Land's landing, the face moved to `landed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Kind of task end shown on the glass.
pub(crate) enum EndFace {
    /// `landed`.
    Landed,
}

impl EndFace {
    /// The end `face` would be: `landed`; `None` for every face that ends no
    /// task (open, theirs).
    #[must_use]
    /// Maps `face` to its task end, or `None` if `face` does not end a task.
    pub(crate) const fn of(face: TaskFace) -> Option<Self> {
        match face {
            TaskFace::Landed => Some(Self::Landed),
            TaskFace::Open | TaskFace::Theirs => None,
        }
    }

    /// The task face this end is ([`Self::of`]'s inverse): the face whose
    /// time the timeline holds for it (`SessionTimeline::task_face_at`).
    #[must_use]
    /// Returns the [`TaskFace`] corresponding to this end ([`Self::of`]'s inverse).
    pub(crate) const fn task_face(self) -> TaskFace {
        match self {
            Self::Landed => TaskFace::Landed,
        }
    }
}

/// ONE END ON THE GLASS (§11.2.3, items 4 to 6): its face, the local clock
/// time and day the face moved there (the task's own stamp), its serial —
/// the session's count of ends told, so a second end is news again — and the
/// serial last looked at (`App::note_done_seen`, the one looking rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Task end state on the glass with its timestamp, serial, and seen/cheered tracking.
pub(crate) struct EndGlass {
    /// Landed.
    pub(crate) face: EndFace,
    /// When the face moved there, as read by the chip's clock.
    pub(crate) at: Option<Stamp>,
    /// Serial number of this end (1 for the session's first).
    pub(crate) serial: u32,
    /// Serial number last looked at.
    pub(crate) seen: u32,
    /// Serial number last cheered by a pet.
    pub(crate) cheered: u32,
}

impl EndGlass {
    /// Whether nobody has looked at it yet.
    #[must_use]
    /// Whether nobody has looked at this end yet.
    pub(crate) const fn unseen(self) -> bool {
        self.seen != self.serial
    }

    /// Whether no pet has cheered it yet.
    #[must_use]
    /// Whether no pet has cheered this end yet.
    pub(crate) const fn uncheered(self) -> bool {
        self.cheered != self.serial
    }
}

impl SessionGlass {
    /// The task's end standing on the glass, if one does.
    #[must_use]
    /// Task end currently standing on the glass, if any.
    pub(crate) const fn end(&self) -> Option<EndGlass> {
        self.end
    }

    /// Land's hold as the session's loop last told it, whatever the
    /// session's word now ([`App::shown_hold`] reads it under Land alone).
    #[must_use]
    /// Land hold last reported by the session's loop ([`App::shown_hold`]).
    pub(crate) const fn hold(&self) -> Option<HeldGlass> {
        self.hold
    }

    /// Records a new task end `face` with the next serial, unseen and uncheered.
    pub(crate) fn tell_end(&mut self, face: EndFace) -> EndGlass {
        self.ends = self.ends.saturating_add(1);
        let end = EndGlass {
            face,
            at: self.task.and_then(|(_, at)| at),
            serial: self.ends,
            seen: self.end.map_or(0, |was| was.seen),
            cheered: self.end.map_or(0, |was| was.cheered),
        };
        self.end = Some(end);
        end
    }

    /// THE END THE PET HAS YET TO CHEER (§11.3.3, item 1): its serial, the
    /// room's `landed`; `None` when no end stands or it was cheered.
    #[must_use]
    /// Serial of the standing end that a pet has yet to cheer, if any.
    pub(crate) fn landed(&self) -> Option<u32> {
        self.end.filter(|end| end.uncheered()).map(|end| end.serial)
    }

    /// Marks `serial` as cheered if it matches the standing end, returning whether it changed.
    pub(crate) fn cheer_end(&mut self, serial: u32) -> bool {
        match self.end.as_mut() {
            Some(end) if end.serial == serial && end.uncheered() => {
                end.cheered = serial;
                true
            }
            _ => false,
        }
    }

    /// Marks the standing end as seen, returning whether it changed.
    pub(crate) fn see_end(&mut self) -> bool {
        match self.end.as_mut() {
            Some(end) if end.unseen() => {
                end.seen = end.serial;
                true
            }
            _ => false,
        }
    }

    /// What the Autopilot chip reads of it this frame: a copy.
    #[must_use]
    /// Snapshot of this glass state for the Autopilot chip.
    pub(crate) fn chip(&self) -> ChipGlass {
        ChipGlass {
            lit: self.lit.map(|(act, _)| act),
            task: self.task.map(|(face, _)| face),
            task_at: self.task.and_then(|(_, at)| at),
            last: self.trail[0].map(|e| {
                (
                    e.act,
                    Stamp {
                        clock: e.clock,
                        day: e.day,
                    },
                )
            }),
            trail: self.trail_gen.to_le_bytes()[0],
            pause: self.pause,
            ..ChipGlass::default()
        }
    }

    /// Records `act` at `at` into the trail and lit label within [`DWELL`], [`BURST`], and [`QUIET`].
    pub(crate) fn take(&mut self, act: Act, at: Arrival, light: bool) -> Took {
        if let Some(newest) = self.trail[0].as_mut()
            && newest.act == act
            && at.now < newest.repeated + DWELL
            && at.now < newest.at + BURST
        {
            newest.repeated = at.now;
            if act == Act::Answered
                && light
                && let Some((Act::Answered, end)) = self.lit.as_mut()
                && at.now < *end
            {
                *end = (*end).max(at.now + DWELL);
            }
            return Took::default();
        }
        self.trail.rotate_right(1);
        self.trail[0] = Some(TrailEntry {
            act,
            at: at.now,
            repeated: at.now,
            clock: at.clock(),
            day: at.day(),
            unix: at.unix,
        });
        self.trail_gen = self.trail_gen.wrapping_add(1);
        let mut took = Took {
            lit: false,
            trail: true,
        };
        if !light {
            return took;
        }
        match self.lit {
            Some((was, end)) if at.now < end => {
                took.lit = was != act;
                self.lit = Some((act, end));
            }
            _ if self.quiet_until.is_none_or(|quiet| at.now >= quiet) => {
                self.lit = Some((act, at.now + DWELL));
                self.quiet_until = Some(at.now + QUIET);
                took.lit = true;
            }
            _ => {}
        }
        took
    }

    /// When the lit label's dwell ends, if one is lit.
    #[must_use]
    /// When the lit label's dwell ends, if one is lit.
    pub(crate) fn lit_until(&self) -> Option<Instant> {
        self.lit.map(|(_, end)| end)
    }

    /// Expires the lit label if its dwell has ended by `now`, returning whether it expired.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        if self.lit.is_some_and(|(_, end)| end <= now) {
            self.lit = None;
            return true;
        }
        false
    }

    /// The trail's generation, whole (the chip carries its low byte).
    #[must_use]
    /// Full generation counter of the trail.
    pub(crate) fn trail_gen(&self) -> u64 {
        self.trail_gen
    }

    /// THE TRAIL AS THE TITLE LISTS IT (§9.2.1): newest first, each act's
    /// glyph and word and the clock time it arrived — `✓ approved 14:05,
    /// ↻ kept going 14:02` — an entry from another local date than `today`
    /// with its weekday (`Thu 23:58`), and one older than a day (at wall
    /// second `now_unix`) left out.
    #[must_use]
    /// Formats the trail for the chip title, newest first, against `now_unix` and `today`.
    pub(crate) fn trail_text(&self, now_unix: i64, today: i64) -> String {
        let mut out = String::new();
        for entry in self.trail.iter().flatten() {
            if now_unix.saturating_sub(entry.unix) > TRAIL_AGE_S {
                continue;
            }
            if !out.is_empty() {
                out.push_str(", ");
            }
            out.push_str(entry.act.glyph());
            out.push(' ');
            out.push_str(entry.act.word());
            out.push(' ');
            if entry.day != today {
                out.push_str(Weekday::of_days(entry.day).short());
                out.push(' ');
            }
            out.push_str(entry.clock.as_str());
        }
        out
    }

    /// THE NEWEST ACT AS A SCREEN READER HEARS IT (§9.2.1), by the title's
    /// own rule ([`Self::trail_text`]): `Last: approved at 14:05.`, from
    /// another local date than `today` with its weekday (`Last: approved on
    /// Thursday at 23:58.`), and nothing where it is older than a day at
    /// wall second `now_unix` — never an old act read as today's.
    #[cfg_attr(
        not(any(test, a11y_tree)),
        allow(
            dead_code,
            reason = "the chips' accessibility nodes describe it; the tree is built only under \
                      the `a11y_tree` cfg (Linux, macOS and Windows)"
        )
    )]
    #[must_use]
    /// Formats the newest act for a screen reader ([`Self::trail_text`]).
    pub(crate) fn spoken_last(&self, now_unix: i64, today: i64) -> Option<String> {
        let entry = self.trail[0].filter(|e| now_unix.saturating_sub(e.unix) <= TRAIL_AGE_S)?;
        let day = (entry.day != today).then(|| Weekday::of_days(entry.day));
        Some(crate::claude_lights::spoken_last(
            entry.act,
            entry.clock,
            day,
        ))
    }
}

/// Current wall-clock unix seconds and local day number.
pub(crate) fn wall_today() -> (i64, i64) {
    #[cfg(test)]
    crate::work_counts::harness_clock_read();
    let unix = wall_unix_s();
    let at = Arrival {
        now: Instant::now(),
        unix,
        offset_s: crate::presence::local_offset_at(unix),
    };
    (unix, at.day())
}

/// Trail title string for `session` in `glass` ([`SessionGlass::trail_text`]).
pub(crate) fn trail_title(
    glass: &std::collections::HashMap<u64, SessionGlass>,
    session: u64,
    stamped: bool,
) -> (String, Option<i64>) {
    let trail = glass.get(&session).filter(|g| g.trail[0].is_some());
    if trail.is_none() && !stamped {
        return (String::new(), None);
    }
    let (unix, today) = wall_today();
    (
        trail.map_or_else(String::new, |g| g.trail_text(unix, today)),
        Some(today),
    )
}

/// The newest act of session `session`'s glass in `glass` as a screen reader
/// hears it now ([`SessionGlass::spoken_last`]): one wall read where the
/// session has a trail, none where it has not.
#[cfg_attr(
    not(a11y_tree),
    allow(
        dead_code,
        reason = "the chips' accessibility nodes describe it; the tree is built only under the \
                  `a11y_tree` cfg (Linux, macOS and Windows)"
    )
)]
/// Screen-reader string for the newest act of `session` in `glass` ([`SessionGlass::spoken_last`]).
pub(crate) fn spoken_last_now(
    glass: &std::collections::HashMap<u64, SessionGlass>,
    session: u64,
) -> Option<String> {
    let g = glass.get(&session).filter(|g| g.trail[0].is_some())?;
    let (unix, today) = wall_today();
    g.spoken_last(unix, today)
}

/// Records `act` on `session` into the session's glass and posts its footer.
pub(crate) fn on_act(app: &mut App, session: u64, act: Act) {
    on_act_at(app, session, act, Arrival::now());
}

/// [`on_act`] at `at`, lighting only under Cruise or Land when acts are shown.
pub(crate) fn on_act_at(app: &mut App, session: u64, act: Act, at: Arrival) {
    let (_, view, _) = app.session_autopilot(session);
    let light = view.carries() && app.harness_acts_shown();
    let took = app
        .harness_glass
        .entry(session)
        .or_default()
        .take(act, at, light);
    if took.any() {
        crate::claude_footer::post_changed(session);
    }
    app.note_harness_act_for_page(at.now, took.any());
}

/// Updates the task face of `session` to `face` at `at_ms`, returning whether it changed.
pub(crate) fn on_task(app: &mut App, session: u64, face: Option<TaskFace>, at_ms: i64) -> bool {
    let task = face.map(|face| (face, Stamp::at(at_ms.div_euclid(1000))));
    let was = app.harness_glass.get(&session).copied().unwrap_or_default();
    let mut now = was;
    now.task = task;
    if face.is_some_and(|face| EndFace::of(face).is_none()) {
        now.end = None;
    }
    if now == was {
        return false;
    }
    if now.task.map(|(face, _)| face) == was.task.map(|(face, _)| face) {
        now.trail_gen = now.trail_gen.wrapping_add(1);
    }
    if now == SessionGlass::default() {
        app.harness_glass.remove(&session);
    } else {
        app.harness_glass.insert(session, now);
    }
    true
}

/// Updates the loop pause of `session` to `plan`, returning whether it changed.
pub(crate) fn on_pause(app: &mut App, session: u64, plan: Option<crate::PausePlan>) -> bool {
    let pause = plan.map(PauseGlass::of);
    let was = app.harness_glass.get(&session).copied().unwrap_or_default();
    if was.pause == pause {
        return false;
    }
    let mut now = was;
    now.pause = pause;
    now.trail_gen = now.trail_gen.wrapping_add(1);
    app.harness_glass.insert(session, now);
    true
}

/// Refreshes the published chip title and accessibility description when `session`'s need changes.
pub(crate) fn on_need_words(app: &mut App, session: u64) {
    let glass = app.harness_glass.entry(session).or_default();
    glass.trail_gen = glass.trail_gen.wrapping_add(1);
}

/// Updates Land's hold on `session` to `held`, returning whether it changed.
pub(crate) fn on_land_held(app: &mut App, session: u64, held: Option<LandHeld>) -> bool {
    let was = app.harness_glass.get(&session).copied().unwrap_or_default();
    let hold = held.map(|held| HeldGlass::of(held, was.hold));
    if was.hold == hold {
        return false;
    }
    let mut now = was;
    now.hold = hold;
    now.trail_gen = now.trail_gen.wrapping_add(1);
    app.harness_glass.insert(session, now);
    true
}

/// LAND'S HOLD ON THE GLASS (§12.3.1, §12.3.3 item 3): the hold the loop
/// told ([`LandHeld`]: its legs, open words and counts, its nudges) and its
/// `since` as the local clock and day its unix second falls on, read once
/// where the hold arrived — never a countdown: nothing repaints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Land hold state on the glass ([`LandHeld`]) with its start stamp.
pub(crate) struct HeldGlass {
    /// The hold as told.
    pub(crate) held: LandHeld,
    /// Clock and day stamp when the land act began, if known.
    pub(crate) since: Option<Stamp>,
}

impl HeldGlass {
    /// The glass of `held`, its `since` stamped where it is new: kept from
    /// `was` where that held the same `since_ms` (a nudge, a yield's legs),
    /// else read in the zone's offset at that instant — UTC's where the zone
    /// cannot be read, the trail's rule.
    #[must_use]
    /// Builds hold glass state for `held`, preserving `since` from `was` when unchanged.
    pub(crate) fn of(held: LandHeld, was: Option<HeldGlass>) -> Self {
        let since = match was {
            Some(was) if was.held.since_ms == held.since_ms => was.since,
            _ => held.since_ms.map(|ms| {
                let at_unix = ms.div_euclid(1000);
                Stamp::at(at_unix).unwrap_or_else(|| {
                    let minute = at_unix.rem_euclid(86_400) / 60;
                    Stamp {
                        clock: Clock::of_minute(u16::try_from(minute).unwrap_or(0)),
                        day: at_unix.div_euclid(86_400),
                    }
                })
            }),
        };
        Self { held, since }
    }

    /// Whether a land act's yield was read and its legs stand (`not landed
    /// yet: …`); else the hold says only `landing since …`.
    #[must_use]
    /// Whether a land act's yield was read and its legs stand.
    pub(crate) const fn read(self) -> bool {
        self.held.read && self.held.legs != 0
    }
}

/// THE FIVE FORMS OF A HOLD'S WORDS ([`hold_words`], §12.3.1): where they
/// are read, and how much room and which clock that place has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Output form for [`hold_words`].
pub(crate) enum HoldForm {
    /// Chip title form after `autopilot: land — `.
    Title,
    /// Settings ▸ Harness SESSIONS state line form.
    Line,
    /// Tab hover form without a clock.
    Hover,
    /// Menu bar landing worker tip and Overview held-chip line form.
    Tip,
    /// Chip accessibility description lead form.
    Spoken,
}

/// WHY LAND STILL HOLDS, IN WORDS (§12.3.1): ONE function, five forms
/// ([`HoldForm`]).
#[must_use]
/// Formats why Land still holds in `form` ([`HoldForm`]) against `today`.
pub(crate) fn hold_words(
    hold: &HeldGlass,
    need: Option<&crate::needs::HarnessAsk>,
    form: HoldForm,
    today: Option<i64>,
) -> Option<String> {
    let sep = crate::native_ui::FACT_SEPARATOR;
    let held = hold.held;
    let nudges = match held.nudges {
        0 => None,
        1 => Some("1 nudge".to_owned()),
        n => Some(format!("{n} nudges")),
    };
    let since = |between: &str| {
        let head = match hold.since {
            Some(at) => format!("landing since {}", at.words(today)),
            None => "landing".to_owned(),
        };
        match &nudges {
            Some(n) => format!("{head}{between}{n}"),
            None => head,
        }
    };
    let legs = || {
        held.legs_held()
            .map(|leg| leg_words(held, leg, need, form))
            .collect::<Vec<_>>()
            .join("; ")
    };
    match form {
        HoldForm::Title if hold.read() => {
            Some(format!("not landed yet: {}{sep}{}", legs(), since(", ")))
        }
        HoldForm::Title => Some(since(", ")),
        HoldForm::Line if hold.read() => {
            Some(format!("not landed yet: {}{sep}{}", legs(), since(sep)))
        }
        HoldForm::Line => Some(since(sep)),
        HoldForm::Hover => hold.read().then(|| format!("not landed yet, {}", legs())),
        HoldForm::Tip if hold.read() => Some(match nudges {
            Some(n) => format!("not landed yet: {}{sep}{n}", legs()),
            None => format!("not landed yet: {}", legs()),
        }),
        HoldForm::Tip => nudges,
        HoldForm::Spoken => {
            let head = match hold.since {
                Some(at) => match at.day_words(today, true) {
                    Some(day) => format!("Landing since {day} at {}", at.clock.as_str()),
                    None => format!("Landing since {}", at.clock.as_str()),
                },
                None => "Landing".to_owned(),
            };
            let since = match &nudges {
                Some(n) => format!("{head}, {n}."),
                None => format!("{head}."),
            };
            if !hold.read() {
                return Some(since);
            }
            let mut out = String::from("Not landed yet.");
            for leg in held.legs_held() {
                out.push(' ');
                out.push_str(&spoken_leg(held, leg, need));
            }
            out.push(' ');
            out.push_str(&since);
            Some(out)
        }
    }
}

/// One leg of a hold in `form`'s words ([`hold_words`]): the title's need
/// made title-safe, the hover's and tip's need `needs you` alone.
fn leg_words(
    held: LandHeld,
    leg: HoldLeg,
    need: Option<&crate::needs::HarnessAsk>,
    form: HoldForm,
) -> String {
    match leg {
        HoldLeg::Needs => match (form, need) {
            (HoldForm::Hover | HoldForm::Tip, _) => "needs you".to_owned(),
            (_, Some(ask)) => {
                let words = format!("needs you: {}{}", ask.first, ask.more_words());
                if form == HoldForm::Title {
                    title_safe(&words)
                } else {
                    words
                }
            }
            (_, None) => "a need named earlier in its reply".to_owned(),
        },
        HoldLeg::Repo => repo_words(held),
        HoldLeg::Open => open_leg(held.open_words()),
        HoldLeg::Defense => "no \u{201c}Nothing left:\u{201d} defense yet".to_owned(),
        HoldLeg::Cut => "a reply too tall to read whole".to_owned(),
    }
}

/// One leg as a screen reader hears it: a sentence, glyph-free.
fn spoken_leg(held: LandHeld, leg: HoldLeg, need: Option<&crate::needs::HarnessAsk>) -> String {
    match leg {
        HoldLeg::Needs => match need {
            Some(ask) => {
                let more = match ask.more {
                    0 => String::new(),
                    n => format!(" And {n} more."),
                };
                format!("Needs you: {}.{more}", speakable(&ask.first))
            }
            None => "A need named earlier in its reply.".to_owned(),
        },
        HoldLeg::Repo => format!("{}.", repo_words(held)),
        HoldLeg::Open => {
            let words: Vec<String> = held
                .open_words()
                .map(speakable)
                .filter(|w| !w.is_empty())
                .collect();
            if words.is_empty() {
                "Open work in its report.".to_owned()
            } else {
                format!("Open work in its report: {}.", words.join(", "))
            }
        }
        HoldLeg::Defense => "No Nothing left defense yet.".to_owned(),
        HoldLeg::Cut => "A reply too tall to read whole.".to_owned(),
    }
}

/// THE OPEN LEG (§12.3.1, §12.3.5): the open work Land's strict reading
/// named, in the words its nudge's suffix gave the agent (`Your report still
/// names open work (unchecked box, a part)`): `open work in its report:
/// unchecked box, a part`.
fn open_leg<'a>(words: impl Iterator<Item = &'a str>) -> String {
    let words: Vec<&str> = words.collect();
    if words.is_empty() {
        "open work in its report".to_owned()
    } else {
        format!("open work in its report: {}", words.join(", "))
    }
}

/// The repository's work in words: each count only where it is not zero,
/// joined `, ` (a leg's own words hold no `; `); the probe's own words where
/// it named work and counted none.
fn repo_words(held: LandHeld) -> String {
    let count = |n: u16, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let parts: Vec<String> = [
        count(held.changed, "changed file", "changed files"),
        count(held.untracked, "untracked file", "untracked files"),
        count(held.stashes, "stash", "stashes"),
        count(held.unpushed, "unpushed commit", "unpushed commits"),
        count(
            held.unlanded,
            "commit not on the remote main",
            "commits not on the remote main",
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "work no remote main has".to_owned()
    } else {
        parts.join(", ")
    }
}

/// A HARNESS'S NEED IN THE CHIP'S TITLE (§12.2.1, §12.2.3 item 5; the W4
/// title slice): the state line's head (`needs you: <the need> (+1 more);
/// carrying on`, `harness_screen::need_head`, the one function that words
/// it), title-safe, so the worker's own ` · ` and ` — ` never move the
/// title's cuts ([`crate::claude_lights::lean_titles`]).
#[must_use]
/// Formats `ask` for the chip's title.
pub(crate) fn need_title(ask: &crate::needs::HarnessAsk) -> String {
    title_safe(&crate::harness_screen::need_head(ask))
}

/// A HARNESS'S NEED AS A SCREEN READER HEARS IT, in the chip's accessibility
/// description (§12.2.1; the W4 title slice): the hold's own need sentence
/// ([`spoken_leg`]: `Needs you: <the need>. And 1 more.`, glyph-free), then
/// `Carrying on.`, as the entry says the work goes on.
#[must_use]
/// Formats `ask` for the chip's screen-reader accessibility description.
pub(crate) fn need_spoken(ask: &crate::needs::HarnessAsk) -> String {
    format!(
        "{} Carrying on.",
        spoken_leg(LandHeld::default(), HoldLeg::Needs, Some(ask))
    )
}

/// A worker's words made safe for the chip's title (§12.2.3 item 6): its
/// ` · ` and ` — `, which the title reads as its cut marks, become `, `.
fn title_safe(words: &str) -> String {
    words
        .replace(crate::native_ui::FACT_SEPARATOR, ", ")
        .replace(" \u{2014} ", ", ")
}

/// Words as a screen reader says them (§12.2.3 item 6, the face table's
/// rule): letters, digits, spaces and `.:,` alone — `&` said `and`, `%`
/// `percent` and `#` `number` (`#45`, `90%`: words, where dropped they said
/// nothing), `▸` a comma against the word before it (`System Settings,
/// Privacy and Security`), a hyphen or a dash a space, anything else
/// dropped — and the spaces folded.
fn speakable(words: &str) -> String {
    let mut out = String::with_capacity(words.len());
    for c in words.chars() {
        match c {
            '&' => out.push_str(" and "),
            '%' => out.push_str(" percent "),
            '#' => out.push_str(" number "),
            '\u{25b8}' => {
                out.truncate(out.trim_end().len());
                out.push(',');
            }
            '-' | '\u{2010}'..='\u{2014}' => out.push(' '),
            c if c.is_alphanumeric() || matches!(c, '.' | ':' | ',') => out.push(c),
            c if c.is_whitespace() => out.push(' '),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl App {
    /// LAND'S HOLD OF SESSION `session` AS ITS SURFACES SHOW IT (§12.3.3
    /// item 3, 3b's contract in §12.3.10): the hold on its glass, only while
    /// its chip's face is `⤓ land` ([`crate::claude_lights::ChipFace::of`]
    /// over its view and glass: its own word, else the default, is Land, a
    /// harness runs it, and its task is open or unknown).
    #[must_use]
    /// Land hold of `session` when its mode is Land, if any.
    pub(crate) fn shown_hold(&self, session: u64) -> Option<HeldGlass> {
        let hold = self.harness_glass.get(&session)?.hold?;
        let glass = self.chip_glass(session);
        (crate::claude_lights::ChipFace::of(self.session_autopilot(session).1, &glass)
            == crate::claude_lights::ChipFace::Land)
            .then_some(hold)
    }

    /// Whether the act ticker's label is shown at all: `[claude_footer]
    /// acts` on (the default) and serious mode off
    /// (`SeriousEffect::HarnessTicker`).
    #[must_use]
    /// Whether the act ticker's label is enabled and serious mode is off.
    pub(crate) fn harness_acts_shown(&self) -> bool {
        self.config.claude_footer_acts()
            && self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::HarnessTicker)
    }

    /// When the harness's glass next needs the loop
    /// (`metrics::DeadlineOwner::HarnessActs`): the end of a label on show,
    /// or a re-read Settings ▸ Harness owes while it is on screen (a page
    /// that left owes none, [`Self::expire_harness_acts`]).
    #[must_use]
    /// Earliest deadline when the harness glass next needs the event loop.
    pub(crate) fn harness_acts_deadline(&self) -> Option<Instant> {
        let lit = self
            .harness_glass
            .values()
            .filter_map(SessionGlass::lit_until)
            .min();
        let reread = self
            .harness_screen
            .reread_at()
            .filter(|_| self.harness_page_shown());
        match (lit, reread) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Advances expired harness acts and returns the next deadline for the event loop.
    pub(crate) fn harness_acts_wait(&mut self) -> Option<Instant> {
        self.expire_harness_acts();
        self.harness_acts_deadline()
    }

    /// Expires finished label dwells, posts their footers, and triggers due ledger re-reads.
    pub(crate) fn expire_harness_acts(&mut self) {
        if self.harness_screen.reread_at().is_some() && !self.harness_page_shown() {
            self.harness_screen.forget_acts();
        }
        if self.harness_acts_deadline().is_none() {
            return;
        }
        #[cfg(test)]
        crate::work_counts::harness_clock_read();
        self.expire_harness_acts_at(Instant::now());
    }

    /// [`Self::expire_harness_acts`] at `now`.
    pub(crate) fn expire_harness_acts_at(&mut self, now: Instant) {
        for (session, glass) in &mut self.harness_glass {
            if glass.expire(now) {
                crate::claude_footer::post_changed(*session);
            }
        }
        if self.harness_screen.reread_at().is_some_and(|at| at <= now) {
            self.reread_harness_stats(now);
        }
    }

    /// Removes harness glass state for retired session `id`.
    pub(crate) fn forget_harness_glass(&mut self, id: u64) {
        self.harness_glass.remove(&id);
    }
}

impl SessionGlass {
    /// Adopts a carried task end from a seamless update, returning whether it took effect.
    pub(crate) fn carry_end(&mut self, face: EndFace, at: Option<Stamp>, seen: bool) -> bool {
        if self.end.is_some() {
            return false;
        }
        self.ends = self.ends.saturating_add(1);
        self.end = Some(EndGlass {
            face,
            at,
            serial: self.ends,
            seen: if seen { self.ends } else { 0 },
            cheered: self.ends,
        });
        true
    }
}

#[cfg(test)]
#[path = "harness_glass_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "harness_cheer_tests.rs"]
mod cheer_tests;

#[cfg(test)]
#[path = "harness_hold_tests.rs"]
pub(crate) mod hold_tests;
