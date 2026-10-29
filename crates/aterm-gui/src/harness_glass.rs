// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session harness glass state for the Autopilot chip, act trail, and Land task face ([`SessionGlass`]).

use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, HeldBy, HoldLeg, LandHeld, TaskFace};

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

const TRAIL_AGE_S: i64 = 86_400;

/// When an act arrived, read ONCE at its receipt (§9.2.3, item 4): the monotonic instant its dwell is measured from, the wall second its clock time and date are formatted from, and the zone's offset there (`None` where the platform cannot say: the clock reads in UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Monotonic and wall-clock arrival timestamps for a received act.
pub(crate) struct Arrival {
    pub(crate) now: Instant,
    pub(crate) unix: i64,
    pub(crate) offset_s: Option<i64>,
}

impl Arrival {
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

    fn day(self) -> i64 {
        (self.unix + self.offset_s.unwrap_or(0)).div_euclid(86_400)
    }

    fn clock(self) -> Clock {
        let minute = (self.unix + self.offset_s.unwrap_or(0)).rem_euclid(86_400) / 60;
        Clock::of_minute(u16::try_from(minute).unwrap_or(0))
    }
}

/// A FACE'S CLOCK WITH ITS DAY (design §10.2.3, item 3): the local `HH:MM` a fact arrived at — a task's face moving, an act of the trail — and the local day number it fell on, both read once where the fact arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Local clock time and day number when a fact arrived.
pub(crate) struct Stamp {
    /// Local clock time.
    pub(crate) clock: Clock,
    /// Local day number (days since 1970-01-01, local time).
    pub(crate) day: i64,
}

impl Stamp {
    /// The stamp of unix second `at_unix`, in the zone's offset at that instant (`presence::local_offset_at`); `None` where the platform cannot say.
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

    /// [`Self::at`], in UTC where the zone cannot be read (the trail's rule).
    #[must_use]
    pub(crate) fn at_or_utc(at_unix: i64) -> Self {
        Self::at(at_unix).unwrap_or_else(|| {
            let minute = at_unix.rem_euclid(86_400) / 60;
            Self {
                clock: Clock::of_minute(u16::try_from(minute).unwrap_or(0)),
                day: at_unix.div_euclid(86_400),
            }
        })
    }

    /// A time ahead as a title says it: `18:41`, and on another local day than `today` (past
    /// midnight) its weekday, `Sat 00:12`, as an armed timer names a later day; the clock alone
    /// with no day read for the build (`None`).
    #[must_use]
    pub(crate) fn ahead_words(self, today: Option<i64>) -> String {
        match today.filter(|today| *today != self.day) {
            Some(_) => format!(
                "{} {}",
                Weekday::of_days(self.day).short(),
                self.clock.as_str()
            ),
            None => self.clock.as_str().to_owned(),
        }
    }

    /// THE WALL TIME (unix ms) ITS MINUTE STARTS AT, [`Self::at`]'s inverse to the minute: the local minute and day placed by the zone's offset at that instant, read twice, so a minute within the zone's offset of a change of offset is placed by the offset it fell under (one read, at the local reading taken as an instant, places it an hour off there); `None` where the platform cannot say.
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

    /// The day it fell on as a builder names it against `today` (§10.2.3, item 3): nothing on `today` itself, or with no day read for the build (`None`: the clock stands alone); its weekday within the six days before (`Fri`), the one stretch where a weekday cannot be read as another; else its date (`9 Oct`, `9 Oct 2025` from another year) — a week or more back, where a weekday would name a later day (today's own, on the seventh), or after `today` (a clock set back).
    #[must_use]
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

    /// `18:11`, `Fri 18:11` within the six days before `today`, or `9 Oct 18:11` further off ([`Self::day_words`]).
    #[must_use]
    /// Compact clock and day label against `today` ([`Self::day_words`]).
    pub(crate) fn words(self, today: Option<i64>) -> String {
        match self.day_words(today, false) {
            Some(day) => format!("{day} {}", self.clock.as_str()),
            None => self.clock.as_str().to_owned(),
        }
    }

    /// `at 18:11`, `on Friday at 18:11` within the six days before `today`, or `on 9 October at 18:11` further off: as a screen reader says it.
    #[must_use]
    /// Screen-reader phrasing of this stamp against `today`.
    pub(crate) fn spoken(self, today: Option<i64>) -> String {
        match self.day_words(today, true) {
            Some(day) => format!("on {day} at {}", self.clock.as_str()),
            None => format!("at {}", self.clock.as_str()),
        }
    }
}

/// THE LOOP'S PAUSE ON THE GLASS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.5.3, item 4; [`on_pause`]): when it ends, as the local clock and day its unix second falls on — read once, where the fact arrived — and how many short turns in a row it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Loop pause state displayed on the glass ([`on_pause`]).
pub(crate) struct PauseGlass {
    /// When the pause ends.
    pub(crate) until: Stamp,
    /// Consecutive short turns preceding the pause.
    pub(crate) short: u8,
}

impl PauseGlass {
    /// The glass of `plan`: its end's clock and day in the zone's offset at that instant, UTC's where the zone cannot be read (the trail's rule).
    #[must_use]
    /// Builds pause glass state from `plan`.
    pub(crate) fn of(plan: crate::PausePlan) -> Self {
        Self {
            until: Stamp::at_or_utc(plan.at_unix),
            short: plan.short,
        }
    }

    /// When it ends as a title says it: `18:41`, and on another local day than `today` (a pause past midnight) its weekday, `Sat 00:12`, as an armed timer names a later day; the clock alone with no day read for the build (`None`).
    #[must_use]
    /// Title phrasing of when the pause ends against `today`.
    pub(crate) fn until_words(self, today: Option<i64>) -> String {
        self.until.ahead_words(today)
    }

    /// The chip's title's part (§10.5.1): `pausing until 18:41 after 3 short turns`, `… after a short turn` for one.
    #[must_use]
    /// Chip title fragment describing the pause against `today`.
    pub(crate) fn title_words(self, today: Option<i64>) -> String {
        let until = self.until_words(today);
        match self.short {
            0 | 1 => format!("pausing until {until} after a short turn"),
            n => format!("pausing until {until} after {n} short turns"),
        }
    }

    /// Settings ▸ Harness's state line (§10.5.1): `pausing until 18:41 (3 short turns in a row)`, `(a short turn)` for one.
    #[must_use]
    /// Settings state line describing the pause against `today`.
    pub(crate) fn state_words(self, today: Option<i64>) -> String {
        let until = self.until_words(today);
        match self.short {
            0 | 1 => format!("pausing until {until} (a short turn)"),
            n => format!("pausing until {until} ({n} short turns in a row)"),
        }
    }

    /// The Autopilot node's description's lead, as a screen reader says it (§10.5.1).
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

/// THE LOOP'S OUTAGE WAIT ON THE GLASS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.17.5.2; [`on_waiting`]): which outage the loop waits on, and when its next blind try comes as the local clock and day its unix second falls on — read once, where the fact arrived — or none while the route is measured down. Only Settings ▸ Harness's state line and `status waiting=` say it: no chip title, no need, no alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WaitGlass {
    /// The outage waited on.
    pub(crate) wall: aterm_agent::supervise::Outage,
    /// When the next try comes; `None` while only the route's edge up resumes.
    pub(crate) next: Option<Stamp>,
}

impl WaitGlass {
    /// The glass of `wait`: its next try's clock and day in the zone's offset at that instant, UTC's where the zone cannot be read (the trail's rule).
    #[must_use]
    pub(crate) fn of(wait: crate::OutageWait) -> Self {
        Self {
            wall: wait.wall,
            next: wait.next_unix().map(Stamp::at_or_utc),
        }
    }

    /// Settings ▸ Harness's state line's head: `waiting: API overload · next try 18:41` (`next try Sat 00:12` on another day than `today`), `waiting: connection error` while the route is measured down.
    #[must_use]
    pub(crate) fn state_words(self, today: Option<i64>) -> String {
        let waiting = format!("waiting: {}", self.wall.words());
        match self.next {
            Some(next) => format!(
                "{waiting}{}next try {}",
                crate::native_ui::FACT_SEPARATOR,
                next.ahead_words(today)
            ),
            None => waiting,
        }
    }
}

/// The local day number at wall second `unix` (the trail's rule: UTC's day where the zone cannot be read).
#[must_use]
/// Local day number at wall second `unix` (falling back to UTC when the zone cannot be read).
pub(crate) fn local_day_at(unix: i64) -> i64 {
    (unix + crate::presence::local_offset_at(unix).unwrap_or(0)).div_euclid(86_400)
}

fn wall_unix_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// The longest name of a self-claim's holder the glass keeps ([`Claimant`]).
const CLAIMANT_BYTES: usize = 24;

/// WHO DID AN ACT WHERE IT WAS NOT ATERM'S OWN HARNESS: the name a proven self-claim's holder acts under ([`aterm_agent::harness::acts::claimant_name`]: `codex-alab` for `codex-alab@4242`), kept inline so the glass stays a copy.
pub(crate) type Claimant = crate::claude_footer::Inline<CLAIMANT_BYTES>;

/// The [`Claimant`] of a proven self-claim's `holder`.
#[must_use]
pub(crate) fn claimant(holder: &str) -> Claimant {
    Claimant::new(aterm_agent::harness::acts::claimant_name(holder))
}

/// AN ACT IN WORDS, NAMING WHO DID IT WHERE IT WAS NOT ATERM'S OWN HARNESS: `✓ approved` for aterm's own, `codex-alab: ✓ approved` for a self-claim's ([`TrailEntry::by`]) — the words the title's trail and a SESSIONS row's `last:` say, so no act of another agent's reads as aterm's.
#[must_use]
pub(crate) fn act_words(act: Act, by: Option<Claimant>) -> String {
    match by {
        Some(by) => format!("{}: {} {}", by.as_str(), act.glyph(), act.word()),
        None => format!("{} {}", act.glyph(), act.word()),
    }
}

/// A STOP AS ITS SURFACES SAY IT ([`App::shown_stop`]): the arm of ruling F's hold that stopped the typing, and who stopped where it was not aterm's own harness (a proven self-claim's holder, `codex-alab`), as [`TrailEntry::by`] names who did an act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShownStop {
    pub(crate) arm: HeldBy,
    /// A proven self-claim's holder, for its own stop; `None` for aterm's.
    pub(crate) by: Option<Claimant>,
}

impl ShownStop {
    /// ITS WORDS IN `form`'s PLACE, NAMING WHO STOPPED WHERE IT WAS NOT ATERM'S OWN HARNESS: [`stop_words`]' own for aterm's, led by the holder's name for a self-claim's (`codex-alab stopped typing: the same reply twice, no work`), so no stop of another agent's reads as aterm's, as [`act_words`] names who did an act. For the surfaces [`App::shown_stop`] feeds: the state line, the hover and the Overview, and a worker's tip (a chip's title and description say only aterm's own, from its glass).
    #[must_use]
    pub(crate) fn words(self, form: HoldForm) -> String {
        let words = stop_words(self.arm, form);
        match self.by {
            Some(by) => format!("{} {words}", by.as_str()),
            None => words,
        }
    }
}

/// LAND'S HOLD AS ITS SURFACES SHOW IT ([`App::shown_hold`]): the hold on the glass, and whose Land it is where that is not aterm's own harness (a proven self-claim's holder, `codex-alab`), as [`ShownStop::by`] names who stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShownHold {
    pub(crate) hold: HeldGlass,
    /// A proven self-claim's holder, for its own hold; `None` for aterm's.
    pub(crate) by: Option<Claimant>,
}

impl ShownHold {
    /// ITS WORDS IN `form`'s PLACE ([`hold_words`]), NAMING WHOSE LAND HOLDS WHERE IT IS NOT ATERM'S OWN HARNESS'S: aterm's in its own words, a self-claim's led by its holder's name (`codex-alab: not landed yet: 2 unpushed commits · 3 nudges`, as [`act_words`] names who did an act), so no hold of another agent's reads as aterm's — unless `after`, the stop said before it on the same line, already named that holder (`codex-alab stopped typing: … · not landed yet: …`, [`ShownStop::words`]), so a line names its holder once. For the surfaces [`App::shown_hold`] feeds that say words: the state line, the hover, the Overview's held line and a worker's tip (a chip's title and description say only aterm's own, from its glass).
    #[must_use]
    pub(crate) fn words(
        self,
        need: Option<&crate::needs::HarnessAsk>,
        form: HoldForm,
        today: Option<i64>,
        after: Option<ShownStop>,
    ) -> Option<String> {
        let words = hold_words(&self.hold, need, form, today)?;
        Some(
            match self
                .by
                .filter(|by| after.is_none_or(|stop| stop.by != Some(*by)))
            {
                Some(by) => format!("{}: {words}", by.as_str()),
                None => words,
            },
        )
    }
}

/// One act of the trail: the act, who did it where it was not aterm's own harness, and when it arrived — its monotonic instant, and that of its last repeat (a repeat within the dwell of it is no new entry), its local clock time and day, formatted at receipt, and its wall second (an entry older than a day is left out of the title).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Single act in the trail with its arrival and repeat timestamps.
pub(crate) struct TrailEntry {
    pub(crate) act: Act,
    /// A proven self-claim's holder, for its act; `None` for aterm's own.
    pub(crate) by: Option<Claimant>,
    pub(crate) at: Instant,
    pub(crate) repeated: Instant,
    pub(crate) clock: Clock,
    pub(crate) day: i64,
    pub(crate) unix: i64,
}

/// What one act moved on the glass ([`SessionGlass::take_by`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Glass state changes caused by [`SessionGlass::take_by`].
pub(crate) struct Took {
    /// Whether the lit label lit or was replaced in place.
    pub(crate) lit: bool,
    /// Whether the trail recorded the act.
    pub(crate) trail: bool,
}

impl Took {
    /// Whether any surface read by the chip, title, or page changed.
    pub(crate) const fn any(self) -> bool {
        self.lit || self.trail
    }
}

/// One session's harness glass, as the frame reads it ([`Self::chip`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Per-session harness glass state read by the frame ([`Self::chip`]).
pub(crate) struct SessionGlass {
    /// The task's face as the session's loop last told it (§9.4.2), and the local clock time and day it moved there (§10.2.3, item 3).
    task: Option<(TaskFace, Option<Stamp>)>,
    /// The last [`TRAIL`] acts, newest first.
    trail: [Option<TrailEntry>; TRAIL],
    /// Moves with every act the trail takes, and with a task face's clock that moves under the same face ([`on_task`]): a title someone reads is built again ([`ChipGlass::trail`], its low byte).
    trail_gen: u64,
    /// The loop's pause after short turns (§10.5.3, item 4), while one stands.
    pause: Option<PauseGlass>,
    /// The label lit now, and the instant its dwell ends.
    lit: Option<(Act, Instant)>,
    /// Nothing lights again before this: [`QUIET`] after the last label began.
    quiet_until: Option<Instant>,
    /// THE TASK'S END (§11.2.3, item 4), while it stands: told once by `App::note_harness_end` (`harness_task::is_a_task_end`), KEPT across the host's "unknown" (`on_task(None)`, so it never flickers between loop runs) and cleared at a move to a known face that is no end — open, theirs ([`on_task`]): an end never outlives the work it names.
    end: Option<EndGlass>,
    /// How many ends this session told: the next end's [`EndGlass::serial`].
    ends: u32,
    /// WHY LAND STILL HOLDS (§12.3.3 item 3), as the session's loop last told it ([`on_land_held`]), with its `since` stamped once: what the SESSIONS state line, the tab's hover and the Overview, the menu bar's worker row and the chip's title (the W4 title slice) and description say of a landing that has not landed — each only while the session's Autopilot is Land and its chip wears `⤓ land`, and all but the chip's title and description only while aterm holds its claim ([`App::shown_hold`], which shows a codex-alab's own hold in its place under that agent's proven claim, §14.3.3).
    hold: Option<HeldGlass>,
    /// ATERM STOPPED TYPING INTO IT (§14.3), by which arm of ruling F's hold, as the session's loop last told its standing exception (`HostNote::ResumeException`'s `held`, [`on_stopped`]): set while the hold stands, whatever its urgency, and cleared as the loop lifts it or ends. What the chip's title and description, the state line, the hover and the Overview, a worker's tip and `status held=` say ([`stop_words`]), each only while the chip carries a mode ([`App::shown_stop`]).
    stopped: Option<HeldBy>,
    /// THE LOOP'S OUTAGE WAIT (§14.17.5.2), while one stands ([`on_waiting`]): told by the loop's wait at an outage wall, and cleared by the wait that replaces it, the wait's end or the loop's.
    waiting: Option<WaitGlass>,
}

/// THE KIND OF A TASK'S END (§11.2.3, item 1): under Autopilot Cruise / Autopilot Land only Land's landing, the face moved to `landed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Kind of task end shown on the glass.
pub(crate) enum EndFace {
    /// `landed`.
    Landed,
}

impl EndFace {
    /// The end `face` would be: `landed`; `None` for every face that ends no task (open, theirs).
    #[must_use]
    /// Maps `face` to its task end, or `None` if `face` does not end a task.
    pub(crate) const fn of(face: TaskFace) -> Option<Self> {
        match face {
            TaskFace::Landed => Some(Self::Landed),
            TaskFace::Open | TaskFace::Theirs => None,
        }
    }

    /// The task face this end is ([`Self::of`]'s inverse): the face whose time the timeline holds for it (`SessionTimeline::task_face_at`).
    #[must_use]
    /// Returns the [`TaskFace`] corresponding to this end ([`Self::of`]'s inverse).
    pub(crate) const fn task_face(self) -> TaskFace {
        match self {
            Self::Landed => TaskFace::Landed,
        }
    }
}

/// ONE END ON THE GLASS (§11.2.3, items 4 to 6): its face, the local clock time and day the face moved there (the task's own stamp), its serial — the session's count of ends told, so a second end is news again — and the serial last looked at (`App::note_done_seen`, the one looking rule).
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

    /// Land's hold as the session's loop last told it, whatever the session's word now ([`App::shown_hold`] reads it under Land alone).
    #[must_use]
    /// Land hold last reported by the session's loop ([`App::shown_hold`]).
    pub(crate) const fn hold(&self) -> Option<HeldGlass> {
        self.hold
    }

    /// Which arm of ruling F's hold stopped the typing, as the session's loop last told it ([`App::shown_stop`] reads it under a mode that carries).
    #[must_use]
    pub(crate) const fn stopped(&self) -> Option<HeldBy> {
        self.stopped
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

    /// THE END THE PET HAS YET TO CHEER (§11.3.3, item 1): its serial, the room's `landed`; `None` when no end stands or it was cheered.
    #[must_use]
    /// Serial of the standing end that a pet has yet to cheer, if any.
    pub(crate) fn landed(&self) -> Option<u32> {
        self.end.filter(|end| end.uncheered()).map(|end| end.serial)
    }

    /// Marks `serial` as cheered if it matches the standing end, returning whether it changed.
    pub(crate) const fn cheer_end(&mut self, serial: u32) -> bool {
        match self.end.as_mut() {
            Some(end) if end.serial == serial && end.uncheered() => {
                end.cheered = serial;
                true
            }
            _ => false,
        }
    }

    /// Marks the standing end as seen, returning whether it changed.
    pub(crate) const fn see_end(&mut self) -> bool {
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
            last_by: self.trail[0].and_then(|e| e.by),
            trail: self.trail_gen.to_le_bytes()[0],
            pause: self.pause,
            ..ChipGlass::default()
        }
    }

    /// [`Self::take_by`] of aterm's own act, as the tests take one.
    #[cfg(test)]
    pub(crate) fn take(&mut self, act: Act, at: Arrival, light: bool) -> Took {
        self.take_by(act, None, at, light)
    }

    /// Records `act` at `at`, which `by` did (a proven self-claim's holder, `Some`, or aterm's own harness), into the trail and lit label within [`DWELL`], [`BURST`], and [`QUIET`]. A repeat is the same act by the same hand: one of aterm's never joins a self-claim's entry, nor theirs aterm's.
    pub(crate) fn take_by(
        &mut self,
        act: Act,
        by: Option<Claimant>,
        at: Arrival,
        light: bool,
    ) -> Took {
        if let Some(newest) = self.trail[0].as_mut()
            && newest.act == act
            && newest.by == by
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
            by,
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
    pub(crate) const fn trail_gen(&self) -> u64 {
        self.trail_gen
    }

    /// THE TRAIL AS THE TITLE LISTS IT (§9.2.1): newest first, each act's glyph and word and the clock time it arrived — `✓ approved 14:05, ↻ kept going 14:02`, a self-claim's led by its holder's name (`codex-alab: ✓ approved 14:05`, [`act_words`]) — an entry from another local date than `today` with its weekday (`Thu 23:58`), and one older than a day (at wall second `now_unix`) left out.
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
            out.push_str(&act_words(entry.act, entry.by));
            out.push(' ');
            if entry.day != today {
                out.push_str(Weekday::of_days(entry.day).short());
                out.push(' ');
            }
            out.push_str(entry.clock.as_str());
        }
        out
    }

    /// THE NEWEST ACT AS A SCREEN READER HEARS IT (§9.2.1), by the title's own rule ([`Self::trail_text`]): `Last: approved at 14:05.`, from another local date than `today` with its weekday (`Last: approved on Thursday at 23:58.`), and nothing where it is older than a day at wall second `now_unix` — never an old act read as today's.
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
            entry.by.as_ref().map(Claimant::as_str),
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

/// The newest act of session `session`'s glass in `glass` as a screen reader hears it now ([`SessionGlass::spoken_last`]): one wall read where the session has a trail, none where it has not.
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
    on_act_by_at(app, session, act, None, at);
}

/// A PROVEN SELF-CLAIM'S ACT ON THE GLASS: `act`, recorded by `holder` on `session`'s proven claim (`HarnessFact::SelfAct`), lights the chip and joins the trail as aterm's own act does ([`on_act`]), worded as the holder's ([`act_words`]), and owes Settings ▸ Harness its reading.
pub(crate) fn on_self_act(app: &mut App, session: u64, act: Act, holder: &str) {
    on_act_by_at(app, session, act, Some(claimant(holder)), Arrival::now());
}

/// [`on_act_at`] of an act `by` did ([`SessionGlass::take_by`]).
pub(crate) fn on_act_by_at(
    app: &mut App,
    session: u64,
    act: Act,
    by: Option<Claimant>,
    at: Arrival,
) {
    let (_, view, _) = app.session_autopilot(session);
    let light = view.carries() && app.harness_acts_shown();
    let took = app
        .harness_glass
        .entry(session)
        .or_default()
        .take_by(act, by, at, light);
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

/// Updates the stop of `session` to `stopped` (its standing exception's arm, [`SessionGlass::stopped`]), returning whether it changed: a title someone reads is built again ([`SessionGlass::trail_gen`]).
pub(crate) fn on_stopped(app: &mut App, session: u64, stopped: Option<HeldBy>) -> bool {
    let was = app.harness_glass.get(&session).copied().unwrap_or_default();
    if was.stopped == stopped {
        return false;
    }
    let mut now = was;
    now.stopped = stopped;
    now.trail_gen = now.trail_gen.wrapping_add(1);
    if now == SessionGlass::default() {
        app.harness_glass.remove(&session);
    } else {
        app.harness_glass.insert(session, now);
    }
    true
}

/// Updates the outage wait of `session` to `wait` ([`SessionGlass::waiting`]), returning whether it changed. No title reads it, so the trail's generation stands.
pub(crate) fn on_waiting(app: &mut App, session: u64, wait: Option<crate::OutageWait>) -> bool {
    let waiting = wait.map(WaitGlass::of);
    let was = app.harness_glass.get(&session).copied().unwrap_or_default();
    if was.waiting == waiting {
        return false;
    }
    let mut now = was;
    now.waiting = waiting;
    if now == SessionGlass::default() {
        app.harness_glass.remove(&session);
    } else {
        app.harness_glass.insert(session, now);
    }
    true
}

/// LAND'S HOLD ON THE GLASS (§12.3.1, §12.3.3 item 3): the hold the loop told ([`LandHeld`]: its legs, open words and counts, its nudges) and its `since` as the local clock and day its unix second falls on, read once where the hold arrived — never a countdown: nothing repaints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// Land hold state on the glass ([`LandHeld`]) with its start stamp.
pub(crate) struct HeldGlass {
    /// The hold as told.
    pub(crate) held: LandHeld,
    /// Clock and day stamp when the land act began, if known.
    pub(crate) since: Option<Stamp>,
}

impl HeldGlass {
    /// The glass of `held`, its `since` stamped where it is new: kept from `was` where that held the same `since_ms` (a nudge, a yield's legs), else read in the zone's offset at that instant — UTC's where the zone cannot be read, the trail's rule.
    #[must_use]
    /// Builds hold glass state for `held`, preserving `since` from `was` when unchanged.
    pub(crate) fn of(held: LandHeld, was: Option<HeldGlass>) -> Self {
        let since = match was {
            Some(was) if was.held.since_ms == held.since_ms => was.since,
            _ => held
                .since_ms
                .map(|ms| Stamp::at_or_utc(ms.div_euclid(1000))),
        };
        Self { held, since }
    }

    /// Whether a land act's yield was read and its legs stand (`not landed yet: …`); else the hold says only `landing since …`.
    #[must_use]
    /// Whether a land act's yield was read and its legs stand.
    pub(crate) const fn read(self) -> bool {
        self.held.read && self.held.legs != 0
    }

    /// Whether its session's need is all that holds the landing (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7): the yield was read ([`Self::read`]) and the hold's one leg is [`HoldLeg::Needs`](aterm_agent::harness::acts::HoldLeg::Needs) — read off the legs [`LandHeld`] carries, never off the hold's words.
    #[must_use]
    pub(crate) const fn need_alone(self) -> bool {
        self.read() && self.held.legs == aterm_agent::harness::acts::HoldLeg::Needs.bit()
    }
}

/// THE FIVE FORMS OF A HOLD'S WORDS ([`hold_words`], §12.3.1): where they are read, and how much room and which clock that place has.
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

/// WHY LAND STILL HOLDS, IN WORDS (§12.3.1): ONE function, five forms ([`HoldForm`]).
#[must_use]
/// Formats why Land still holds in `form` ([`HoldForm`]) against `today`.
pub(crate) fn hold_words(
    hold: &HeldGlass,
    need: Option<&crate::needs::HarnessAsk>,
    form: HoldForm,
    today: Option<i64>,
) -> Option<String> {
    use aterm_agent::harness::acts::{NOT_LANDED_YET, held_tip, nudge_words};
    let sep = crate::native_ui::FACT_SEPARATOR;
    let held = hold.held;
    let nudges = nudge_words(held.nudges);
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
            Some(format!("{NOT_LANDED_YET}: {}{sep}{}", legs(), since(", ")))
        }
        HoldForm::Title => Some(since(", ")),
        HoldForm::Line if hold.read() => {
            Some(format!("{NOT_LANDED_YET}: {}{sep}{}", legs(), since(sep)))
        }
        HoldForm::Line => Some(since(sep)),
        HoldForm::Hover => hold.read().then(|| format!("{NOT_LANDED_YET}, {}", legs())),
        HoldForm::Tip if hold.read() => Some(held_tip(&legs(), held.nudges)),
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

/// A STOPPED AUTOPILOT, IN WORDS (§14.3), beside [`hold_words`]: ONE function, in `form`'s place (`aterm_agent::harness::acts::stop_words`, which the lab reads too) — the title's `stopped typing: the same reply twice, no work; types again at your turn or once the agent works`, the state line's with its facts ` · ` apart, the hover's `stopped typing`, a worker's tip without what restarts it, and the description's sentences.
#[must_use]
pub(crate) fn stop_words(held: HeldBy, form: HoldForm) -> String {
    use aterm_agent::harness::acts::StopForm;
    aterm_agent::harness::acts::stop_words(
        held,
        match form {
            HoldForm::Title => StopForm::Title,
            HoldForm::Line => StopForm::Line,
            HoldForm::Hover => StopForm::Hover,
            HoldForm::Tip => StopForm::Tip,
            HoldForm::Spoken => StopForm::Spoken,
        },
    )
}

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
        HoldLeg::Unread | HoldLeg::Defense | HoldLeg::Cut => {
            leg.fixed_words().unwrap_or_default().to_owned()
        }
    }
}

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
        HoldLeg::Unread => "A repository that could not be read.".to_owned(),
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

fn open_leg<'a>(words: impl Iterator<Item = &'a str>) -> String {
    let words: Vec<&str> = words.collect();
    if words.is_empty() {
        "open work in its report".to_owned()
    } else {
        format!("open work in its report: {}", words.join(", "))
    }
}

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

/// A HARNESS'S NEED IN THE CHIP'S TITLE (§12.2.1, §12.2.3 item 5; the W4 title slice): the state line's head (`needs you: <the need> (+1 more); carrying on`, `harness_screen::need_head`, the one function that words it), title-safe, so the worker's own ` · ` and ` — ` never move the title's cuts ([`crate::claude_lights::lean_titles`]).
#[must_use]
/// Formats `ask` for the chip's title.
pub(crate) fn need_title(ask: &crate::needs::HarnessAsk) -> String {
    title_safe(&crate::harness_screen::need_head(ask))
}

/// A HARNESS'S NEED AS A SCREEN READER HEARS IT, in the chip's accessibility description (§12.2.1; the W4 title slice): the hold's own need sentence ([`spoken_leg`]: `Needs you: <the need>. And 1 more.`, glyph-free), then `Carrying on.`, as the entry says the work goes on.
#[must_use]
/// Formats `ask` for the chip's screen-reader accessibility description.
pub(crate) fn need_spoken(ask: &crate::needs::HarnessAsk) -> String {
    format!(
        "{} Carrying on.",
        spoken_leg(LandHeld::default(), HoldLeg::Needs, Some(ask))
    )
}

fn title_safe(words: &str) -> String {
    words
        .replace(crate::native_ui::FACT_SEPARATOR, ", ")
        .replace(" \u{2014} ", ", ")
}

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
    /// LAND'S HOLD OF SESSION `session` AS ITS SURFACES SHOW IT (§12.3.3 item 3, 3b's contract in §12.3.10; §14.3.3), told by whoever drives it, as [`Self::shown_stop`] reads a stop — while aterm holds its claim, the hold on its glass ([`SessionGlass::hold`]); under a proven self-claim, the hold that claim told of its own landing (`meta set harness_land`, [`crate::session_timeline::SessionMeta::self_land`]) while the claim stands, named for its holder — and either only while its chip's face is `⤓ land` ([`crate::claude_lights::ChipFace::of`] over its view and glass: its own word, else the default, is Land, a harness runs it, and its task is open or unknown), so a codex-alab's own landing and a person's own exchange (`landed@v1`, `left-to-person@v1`) take its hold down as they take aterm's. Nobody's with neither claim: a hold left on aterm's glass is never shown under another's claim, nor after aterm's ends.
    #[must_use]
    pub(crate) fn shown_hold(&self, session: u64) -> Option<ShownHold> {
        let own = self
            .harness_glass
            .get(&session)
            .and_then(SessionGlass::hold);
        let told = self.pool.get(session).and_then(|entry| {
            let meta = entry
                .ctx
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = crate::metrics::now_us();
            meta.self_land(now).map(|hold| ShownHold {
                hold,
                by: meta.live_supervisor(now).map(claimant),
            })
        });
        if own.is_none() && told.is_none() {
            return None;
        }
        let (_, view, supervised, _) = self.session_autopilot_for(
            session,
            self.session_agent(session),
            &crate::autopilot::ClaimClock::default(),
        );
        let hold = if supervised {
            own.map(|hold| ShownHold { hold, by: None })
        } else {
            told
        }?;
        let glass = self.chip_glass(session);
        (crate::claude_lights::ChipFace::of(view, &glass) == crate::claude_lights::ChipFace::Land)
            .then_some(hold)
    }

    /// THE STOP OF SESSION `session` AS ITS SURFACES SAY IT (§14.3, §14.3.2), told by whoever drives it ([`crate::autopilot::carried`]'s rule) — while aterm holds its claim, the arm on its glass ([`SessionGlass::stopped`]); under a proven self-claim, the arm that claim told of its own typing (`meta set harness_held`, [`crate::session_timeline::SessionMeta::self_held`]) while the claim stands, named for its holder — and either only while its chip carries a mode (`∞`, `∞ ⤓18:00`, `⤓ land`: [`crate::claude_lights::ChipFace::of`] over its view and glass), never on an Off, landed or quiet chip, so `status held=` and the worker row's tip say a stop exactly where the hover, the Overview and the state line, which read the face, do: a codex-alab tells its own landing and a person's own exchange (`landed@v1`, `left-to-person@v1`) onto the same face, and its stop gives way to them as aterm's does. Nobody's with neither claim.
    #[must_use]
    pub(crate) fn shown_stop(&self, session: u64) -> Option<ShownStop> {
        let own = self
            .harness_glass
            .get(&session)
            .and_then(SessionGlass::stopped);
        let told = {
            let meta = self
                .pool
                .get(session)?
                .ctx
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = crate::metrics::now_us();
            meta.self_held(now).map(|arm| ShownStop {
                arm,
                by: meta.live_supervisor(now).map(claimant),
            })
        };
        if own.is_none() && told.is_none() {
            return None;
        }
        let (_, view, supervised, _) = self.session_autopilot_for(
            session,
            self.session_agent(session),
            &crate::autopilot::ClaimClock::default(),
        );
        let stop = if supervised {
            own.map(|arm| ShownStop { arm, by: None })
        } else {
            told
        }?;
        let glass = self.chip_glass(session);
        crate::claude_lights::ChipFace::of(view, &glass)
            .carries()
            .then_some(stop)
    }

    /// THE OUTAGE WAIT OF SESSION `session` (§14.17.5.2): the wait on its glass ([`SessionGlass::waiting`]), whatever its chip's face: only a running loop tells one, and the wait's end or the loop's takes it down.
    #[must_use]
    pub(crate) fn shown_wait(&self, session: u64) -> Option<WaitGlass> {
        self.harness_glass.get(&session)?.waiting
    }

    /// Whether the act ticker's label is shown at all: `[claude_footer] acts` on (the default) and serious mode off (`SeriousEffect::HarnessTicker`).
    #[must_use]
    /// Whether the act ticker's label is enabled and serious mode is off.
    pub(crate) fn harness_acts_shown(&self) -> bool {
        self.config.claude_footer_acts()
            && self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::HarnessTicker)
    }

    /// When the harness's glass next needs the loop (`metrics::DeadlineOwner::HarnessActs`): the end of a label on show, or a re-read Settings ▸ Harness owes while it is on screen (a page that left owes none, [`Self::expire_harness_acts`]).
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
    pub(crate) const fn carry_end(&mut self, face: EndFace, at: Option<Stamp>, seen: bool) -> bool {
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
