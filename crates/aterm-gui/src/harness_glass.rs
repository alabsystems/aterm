// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S GLASS, per session (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §9.2, §9.4): what the Autopilot chip shows beside the session's word —
//! the act the harness just took, lit for a moment, the trail of the last
//! few, and the task's face under Land.
//!
//! The App keeps one [`SessionGlass`] per session that has any
//! (`App::harness_glass`, by local id), and the frame reads it beside the
//! session's Autopilot, once a frame, as a `Copy` [`ChipGlass`]
//! (`App::chip_glass`): no lock, no clock, no allocation. An entry goes with
//! its session (`App::forget_harness_glass`, from
//! `retire_session_registration`).
//!
//! THE ACT TICKER (§9.2.1). The session's loop tells each act as its ledger
//! row is written (`IdleHost::act_taken`, through the window's host as
//! `Wake::HarnessFact`, [`on_act`]). At receipt: one monotonic read and one
//! wall read, the act's local clock time formatted once
//! ([`SessionGlass::take`]):
//!
//! * the act joins the TRAIL — the newest [`TRAIL`], newest first, each with
//!   the clock time it arrived — unless it repeats the trail's newest act
//!   within [`DWELL`] of that act's last repeat and within [`BURST`] of its
//!   first (a question dialog's later keys, which Claude Code 2.1.301 takes
//!   about 1.8 s apart: a dialog of up to four questions is one entry). Past
//!   [`BURST`] a repeat is a new entry with its own clock time, under the
//!   rules below, so a steady run of one act is one entry per burst, never
//!   one entry forever;
//! * it LIGHTS the label (`✓ approved`, right of the chips, in the chip's
//!   ink) for [`DWELL`] from the first act of a burst. A newer act while one
//!   is up replaces it in place and does NOT extend the dwell, so a stream of
//!   approvals cannot hold it lit. After a label, nothing lights again until
//!   [`QUIET`] after that label began: at most one label every 15 s. Acts in
//!   between go into the trail alone;
//! * ONE EXCEPTION, A QUESTION DIALOG'S KEYS (design §9.2.7): the label a
//!   dialog's first key lit stays up [`DWELL`] past each later key of the
//!   same dialog (a repeat of `Answered` inside the burst). The dialog
//!   covers the composer while its keys are pressed, so no chip and no label
//!   is drawn until its last key; measured from the first key, a two-question
//!   dialog's label ended before the dialog left and was never seen. Bounded
//!   by [`BURST`], so a label is up at most `BURST + DWELL`, inside the
//!   quiet;
//! * it lights nothing where the harness carries no work (Off, the master
//!   switch — a row written as a worker stops can land after the word
//!   turned), with `[claude_footer] acts = false`, or in serious mode
//!   (`SeriousEffect::HarnessTicker`): the trail, the title and the page
//!   stay.
//!
//! ONE DEADLINE (§9.2.3, item 6): the earliest label's end, or the re-read
//! Settings ▸ Harness owes ([`App::harness_acts_deadline`],
//! `metrics::DeadlineOwner::HarnessActs`), folded where the loop waits; at it
//! the ended labels go and their footers are posted
//! ([`App::expire_harness_acts`]). No thread, no poll: with no label up and no
//! read owed, nothing is folded and no clock is read.
//!
//! THE PAGE. While Settings ▸ Harness is on screen, EVERY act — a repeat the
//! trail coalesced included, since the page counts each ledger row — marks
//! WHAT THE HARNESS DID stale and owes a re-read of the ledgers [`DWELL`]
//! after it, allowing time for a dialog's later `CHOSE` row, at most one
//! every [`REREAD`], on the same deadline (`App::note_harness_act_for_page`).
//! This debounce does not acknowledge a worker's write; a delayed row may
//! need a later act or Refresh. A page that leaves the screen owes nothing.
//!
//! The label is the window's view of an event and never itself a record: the
//! ledger is the record of every act.

use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{Act, HoldLeg, LandHeld, TaskFace};

use crate::App;
use crate::claude_lights::{ChipGlass, Clock, Weekday};

/// How long a label stays lit from the first act of its burst (§9.2.1).
pub(crate) const DWELL: Duration = Duration::from_secs(3);

/// How long after a label began nothing lights again (§9.2.1): at most one
/// label every 15 s.
pub(crate) const QUIET: Duration = Duration::from_secs(15);

/// How long a run of one act may coalesce into one trail entry, from its
/// first act: a question dialog of four questions takes five keys about
/// 1.8 s apart (7.2 s, Claude Code 2.1.301), each a repeat of `Answered`.
pub(crate) const BURST: Duration = Duration::from_secs(9);

/// How many acts the trail keeps, newest first: the title's `last:`.
pub(crate) const TRAIL: usize = 3;

/// The page's re-read of the ledgers while acts keep coming: at most one per
/// this (§9.2.1, "The page").
pub(crate) const REREAD: Duration = Duration::from_secs(10);

/// How old a trail entry may be and still be listed in the title: older
/// ones are left out where the title is built.
const TRAIL_AGE_S: i64 = 86_400;

/// When an act arrived, read ONCE at its receipt (§9.2.3, item 4): the
/// monotonic instant its dwell is measured from, the wall second its clock
/// time and date are formatted from, and the zone's offset there (`None`
/// where the platform cannot say: the clock reads in UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
/// local day number it fell on, both read once where the fact arrived. A
/// builder that names it reads today's day once, at its event (a hover's
/// start, a publish), and prints the weekday where the two differ
/// (`landed Fri 18:11`): a weekday, never "yesterday", which would go stale
/// at midnight with nothing to repaint it — and the date (`landed 9 Oct
/// 18:11`) once the stamp is a week or more back, where a weekday would
/// name a later day ([`Stamp::day_words`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Stamp {
    /// The local clock time.
    pub(crate) clock: Clock,
    /// The local day number (days since 1970-01-01, local time).
    pub(crate) day: i64,
}

impl Stamp {
    /// The stamp of unix second `at_unix`, in the zone's offset at that
    /// instant (`presence::local_offset_at`); `None` where the platform
    /// cannot say.
    #[must_use]
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
    /// there); `None` where the platform cannot say. Read where an end's
    /// instant is no longer on the timeline: the menu bar's Done for you row
    /// ages from it (`app_tabs::end_wall_ms`).
    #[must_use]
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
    /// own, on the seventh), or after `today` (a clock set back). `long`
    /// spells them out (`Friday`, `9 October`), as a screen reader says them.
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

    /// `18:11`, `Fri 18:11` within the six days before `today`, or
    /// `9 Oct 18:11` further off ([`Self::day_words`]).
    #[must_use]
    pub(crate) fn words(self, today: Option<i64>) -> String {
        match self.day_words(today, false) {
            Some(day) => format!("{day} {}", self.clock.as_str()),
            None => self.clock.as_str().to_owned(),
        }
    }

    /// `at 18:11`, `on Friday at 18:11` within the six days before `today`,
    /// or `on 9 October at 18:11` further off: as a screen reader says it.
    #[must_use]
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
/// many short turns in a row it follows. A clock time, never a countdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PauseGlass {
    /// When the pause ends.
    pub(crate) until: Stamp,
    /// The short turns in a row it follows (the first point seen is one).
    pub(crate) short: u8,
}

impl PauseGlass {
    /// The glass of `plan`: its end's clock and day in the zone's offset at
    /// that instant, UTC's where the zone cannot be read (the trail's rule).
    #[must_use]
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
pub(crate) struct Took {
    /// The lit label moved: lit, or replaced in place.
    pub(crate) lit: bool,
    /// The trail took the act.
    pub(crate) trail: bool,
}

impl Took {
    /// Whether anything the chip, its title or the page reads moved.
    pub(crate) fn any(self) -> bool {
        self.lit || self.trail
    }
}

/// One session's harness glass, as the frame reads it ([`Self::chip`]).
/// The default is v1's chip; an entry at the default is not kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    /// WHY LAND STILL HOLDS (§12.3.3 item 3, slice 3c), as the session's
    /// loop last told it ([`on_land_held`]), with its `since` stamped once:
    /// what the SESSIONS state line, the tab's hover and the Overview, the
    /// menu bar's worker row and the chip's title (the W4 title slice) say
    /// of a landing that has not landed — each only while the session's
    /// Autopilot is Land and its chip wears `⤓ land` ([`App::shown_hold`]).
    /// Never on [`ChipGlass`], in `LightsKey` or the chip's term: a move
    /// bumps the trail's generation, which a title is built again from only
    /// while someone reads it.
    hold: Option<HeldGlass>,
}

/// THE KIND OF A TASK'S END (§11.2.3, item 1): under Autopilot Cruise /
/// Autopilot Land only Land's landing, the face moved to `landed`. Cruise never
/// ends, so it has no end to tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum EndFace {
    /// `landed`.
    Landed,
}

impl EndFace {
    /// The end `face` would be: `landed`; `None` for every face that ends no
    /// task (open, theirs).
    #[must_use]
    pub(crate) const fn of(face: TaskFace) -> Option<Self> {
        match face {
            TaskFace::Landed => Some(Self::Landed),
            TaskFace::Open | TaskFace::Theirs => None,
        }
    }

    /// The task face this end is ([`Self::of`]'s inverse): the face whose
    /// time the timeline holds for it (`SessionTimeline::task_face_at`).
    #[must_use]
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
/// Unseen while `seen != serial`: the tab's `•` (`App::done_unseen`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EndGlass {
    /// Landed.
    pub(crate) face: EndFace,
    /// When the face moved there, as the chip's clock reads it.
    pub(crate) at: Option<Stamp>,
    /// This end's serial (1 for the session's first): `EndGlass::gen` in the
    /// design, renamed since `gen` is a reserved word in edition 2024.
    pub(crate) serial: u32,
    /// The serial last looked at.
    pub(crate) seen: u32,
    /// The serial last cheered (§11.3.3, items 1 to 3): the first frame that
    /// shows this pane to someone hands the end back
    /// (`aterm_effects::companion::PetTick::landed_seen`) and the pet cheers
    /// it there. The latch is the glass's, not the pet's: a switch onto the
    /// landed tab IS the sighting, and of two windows the first pet to see it
    /// takes it. Kept apart from `seen`, which the strip's looking rule moves
    /// (an end seen as it lands in the front tab is still cheered on its first
    /// frame); a cheer a fresh key wiped is spent, never replayed.
    pub(crate) cheered: u32,
}

impl EndGlass {
    /// Whether nobody has looked at it yet.
    #[must_use]
    pub(crate) const fn unseen(self) -> bool {
        self.seen != self.serial
    }

    /// Whether no pet has cheered it yet.
    #[must_use]
    pub(crate) const fn uncheered(self) -> bool {
        self.cheered != self.serial
    }
}

impl SessionGlass {
    /// The task's end standing on the glass, if one does.
    #[must_use]
    pub(crate) const fn end(&self) -> Option<EndGlass> {
        self.end
    }

    /// Land's hold as the session's loop last told it, whatever the
    /// session's word now ([`App::shown_hold`] reads it under Land alone).
    #[must_use]
    pub(crate) const fn hold(&self) -> Option<HeldGlass> {
        self.hold
    }

    /// AN END IS TOLD (`App::note_harness_end`): `face`, at the task face's
    /// own stamp, with the next serial, unseen. Replaces an end that
    /// stood. What now stands.
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
    /// room's `landed`; `None` when no end stands or it was cheered. Read on
    /// a frame already being drawn, one map lookup.
    #[must_use]
    pub(crate) fn landed(&self) -> Option<u32> {
        self.end.filter(|end| end.uncheered()).map(|end| end.serial)
    }

    /// THE END WAS ON SCREEN (`PetTick::landed_seen`): `serial` cheered,
    /// where it is the end that stands. Whether that moved anything; a serial
    /// handed again, or one an end told since replaced, moves nothing.
    pub(crate) fn cheer_end(&mut self, serial: u32) -> bool {
        match self.end.as_mut() {
            Some(end) if end.serial == serial && end.uncheered() => {
                end.cheered = serial;
                true
            }
            _ => false,
        }
    }

    /// THE END IS LOOKED AT (`App::note_done_seen`, the menu bar's Go): its
    /// serial seen. Whether that moved anything (an end stood unseen).
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

    /// Take `act`, arrived at `at` (§9.2.1): into the trail unless it
    /// repeats the trail's newest act within [`DWELL`] of its last repeat
    /// and [`BURST`] of its first (its last repeat then moves to `at`: a
    /// burst of one act, a dialog's keys, is one entry, at its first act's
    /// time — a dialog's later key keeping the label it lit up [`DWELL`]
    /// past itself, §9.2.7); and, where it may `light`, the label —
    /// replaced in place while one is up (its dwell not extended), lit anew
    /// once [`QUIET`] has passed since the last one began, else nothing.
    /// What moved on the glass (a dwell kept up moves nothing drawn).
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
    pub(crate) fn lit_until(&self) -> Option<Instant> {
        self.lit.map(|(_, end)| end)
    }

    /// The label goes if its dwell has ended by `now`: whether it went.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        if self.lit.is_some_and(|(_, end)| end <= now) {
            self.lit = None;
            return true;
        }
        false
    }

    /// The trail's generation, whole (the chip carries its low byte).
    #[must_use]
    pub(crate) fn trail_gen(&self) -> u64 {
        self.trail_gen
    }

    /// THE TRAIL AS THE TITLE LISTS IT (§9.2.1): newest first, each act's
    /// glyph and word and the clock time it arrived — `✓ approved 14:05,
    /// ↻ kept going 14:02` — an entry from another local date than `today`
    /// with its weekday (`Thu 23:58`), and one older than a day (at wall
    /// second `now_unix`) left out. Clock times, never ages: nothing
    /// repaints a title as time passes. Empty with no act to list. Built
    /// where a hover or a selection asks, and again where an act arrives
    /// while one is up — never per frame.
    #[must_use]
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

/// The wall clock as the trail is read against it: unix seconds and the
/// local day — one wall read, an event's (a hover's start, an act's arrival
/// under one, an accessibility tree's build), never a frame's.
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

/// The trail of session `session`'s glass in `glass` as the title lists it
/// now ([`SessionGlass::trail_text`]), and the local day the title's face
/// clock is read against (§10.2.3, item 3): ONE wall read, for the date and
/// the day's age — an event's (a hover's start, an act's arrival under one,
/// a face's move under one), never a frame's — and none at all where the
/// session has no trail and the title names no face clock or pause
/// (`stamped`). Empty, and no day, where neither.
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
pub(crate) fn spoken_last_now(
    glass: &std::collections::HashMap<u64, SessionGlass>,
    session: u64,
) -> Option<String> {
    let g = glass.get(&session).filter(|g| g.trail[0].is_some())?;
    let (unix, today) = wall_today();
    g.spoken_last(unix, today)
}

/// An ACT the harness took on session `session` (`HarnessFact::Act`, §9.2.3
/// item 4): taken into the session's glass at its receipt's clocks
/// ([`on_act_at`]).
pub(crate) fn on_act(app: &mut App, session: u64, act: Act) {
    on_act_at(app, session, act, Arrival::now());
}

/// [`on_act`] at `at`: the act lights only where the harness carries the
/// session's work (Cruise or Land) and the ticker is shown
/// ([`App::harness_acts_shown`]); either way the trail takes it. Where the
/// glass moved, the session's footer is posted. EVERY act — one the trail
/// coalesced too, since the page counts each ledger row — is told to
/// Settings ▸ Harness, which, while on screen, marks WHAT THE HARNESS DID
/// stale and owes a re-read ([`App::note_harness_act_for_page`]). Never a
/// record.
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

/// The TASK'S FACE of session `session` moved to `face` at `at_ms` (unix
/// ms; `None`: unknown), as the timeline now holds it: kept for the frame
/// with its local clock time and day ([`Stamp`]), read here once. An
/// unknown face clears only the task face; any act trail remains. A face
/// that keeps its kind and moves its clock moves only the title's words
/// (`landed 17:42` to `landed 18:05`), which the footer's term keys only
/// through the trail's generation while someone reads the title: the
/// generation moves with it, so that title is built again. The task's END
/// (§11.2.3, item 4) is kept through unknown and through a landed face
/// (whose end, if it is one, `App::note_harness_end` tells next), and
/// cleared at open or theirs. Whether the frame's glass moved.
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

/// THE LOOP'S PAUSE of session `session` moved to `plan` (`None`: none
/// stands; `HarnessFact::Pause`, §10.5.3 item 4): kept for the chip's title
/// and the page with its end's local clock and day ([`PauseGlass::of`], read
/// here once). The pause moves no face and nothing the chip draws at rest,
/// so the trail's generation moves instead: a title someone is reading is
/// built again ([`ChipGlass::trail`], folded into the footer's term only
/// while it is read), and a frame at rest keys nothing new. Whether the
/// glass moved.
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

/// THE NEED THE CHIP'S TITLE SAYS on session `session` moved (§12.2.3 item 5;
/// the W4 title slice): its published need's words, the first need and how
/// many more (`App::publish_need`). The need is no part of the glass — the
/// title reads it from the published verdict where it is built
/// (`claude_lights::TitleWords`) — and moves nothing the chip draws at
/// rest, so only the trail's generation moves: a title someone is reading
/// is built again ([`ChipGlass::trail`], folded into the footer's term only
/// while it is read), and a frame at rest keys nothing new.
pub(crate) fn on_need_words(app: &mut App, session: u64) {
    let glass = app.harness_glass.entry(session).or_default();
    glass.trail_gen = glass.trail_gen.wrapping_add(1);
}

/// LAND'S HOLD of session `session` moved to `held` (`None`: none stands;
/// `HarnessFact::LandHeld`, §12.3.3 item 3, slice 3c): kept on the
/// session's glass with its `since` stamped ONCE — its local clock and day,
/// read here where the hold's `since` is new and kept as it was where only
/// the legs or the nudges moved ([`HeldGlass::of`]). The hold moves nothing
/// the chip draws at rest, so the trail's generation moves instead: a title
/// someone is reading is built again, and a frame at rest keys nothing new.
/// Whether the glass moved.
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
/// where the hold arrived — never a countdown: nothing repaints it. `Copy`,
/// no text: its words are built where a reader asks ([`hold_words`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HeldGlass {
    /// The hold as told.
    pub(crate) held: LandHeld,
    /// When the land act that began it was ledgered, as a clock and day;
    /// `None` where the loop could not say (an older ledger's episode).
    pub(crate) since: Option<Stamp>,
}

impl HeldGlass {
    /// The glass of `held`, its `since` stamped where it is new: kept from
    /// `was` where that held the same `since_ms` (a nudge, a yield's legs),
    /// else read in the zone's offset at that instant — UTC's where the zone
    /// cannot be read, the trail's rule.
    #[must_use]
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
    pub(crate) const fn read(self) -> bool {
        self.held.read && self.held.legs != 0
    }
}

/// THE FIVE FORMS OF A HOLD'S WORDS ([`hold_words`], §12.3.1): where they
/// are read, and how much room and which clock that place has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldForm {
    /// The chip's title, after `autopilot: land — `
    /// (`claude_lights::ChipFace::explanation`): its legs title-safe, its
    /// `since` and nudges after ` · `.
    Title,
    /// Settings ▸ Harness's SESSIONS state line: wrapped, never cut, its
    /// clock with its weekday on another day.
    Line,
    /// The tab's hover, after `driven by aterm (land: `: no clock (it is
    /// composed at a push and read long after), the need named by `needs
    /// you` alone (its own lines stand above it).
    Hover,
    /// The menu bar's landing worker row's tip, and the Overview's line
    /// under a held chip: the hover's legs and the nudges, no clock (the
    /// menu keeps ages, composed at paint; the Overview is composed at a
    /// glance's rebuild and read long after).
    Tip,
    /// The chip's accessibility description's lead
    /// (`claude_lights::autopilot_description`): one sentence a leg,
    /// glyph-free.
    Spoken,
}

/// WHY LAND STILL HOLDS, IN WORDS (§12.3.1): ONE function, five forms
/// ([`HoldForm`]). Where a land act's yield was read ([`HeldGlass::read`]):
/// `not landed yet:` and its legs in the order the owner can act on them —
/// a need (`needs you: <the need>[ (+N more)]`, the published need's own
/// words `need` where one stands, else `a need named earlier in its
/// reply`), the repository's counts (each only where not zero: `1 changed
/// file`, `1 untracked file`, `1 stash`, `2 unpushed commits`, `4 commits
/// not on the remote main`), the open work Land's strict reading named
/// (`open work in its report: follow-up, unchecked box`, [`open_leg`]), no
/// defense (`no “Nothing left:” defense yet`), a reply too tall (`a reply
/// too tall to read whole`) — joined `; `, and inside them nothing else separates them, so
/// a title's cut marks hold. Then, where the form has a clock, `landing
/// since 14:02` (`Fri 14:02` on another day than `today`) and the nudges.
/// Before a yield is read only that clock part stands, the hover says
/// nothing (`None`: main's `land`) and the tip only the nudges where there
/// are any. Composed where a reader asks, never per frame.
#[must_use]
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
        // The line's words, each said (`#n` `number n`, `%` `percent`).
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
/// unchecked box, a part`. Never in quotation marks: the strict reading
/// names a line by a spelling it matched (`follow-up`), by its own word for
/// a kind of line (`unchecked box`, `%`, `a question`) or by its family's
/// word for a row (`local`, `a part`), and one spelling can be either
/// (`not done` is a status row and a family's word), so a quote would say
/// the report holds words it may not. The owner sees the word the nudge
/// named, the misread in one look. `open work in its report` where the
/// hold kept no word.
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
pub(crate) fn need_title(ask: &crate::needs::HarnessAsk) -> String {
    title_safe(&crate::harness_screen::need_head(ask))
}

/// A HARNESS'S NEED AS A SCREEN READER HEARS IT, in the chip's accessibility
/// description (§12.2.1; the W4 title slice): the hold's own need sentence
/// ([`spoken_leg`]: `Needs you: <the need>. And 1 more.`, glyph-free), then
/// `Carrying on.`, as the entry says the work goes on.
#[must_use]
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
    /// harness runs it, and its task is open or unknown). The loop reads the
    /// word only where it decides, so a switch from Land to Cruise at a held
    /// point is told `None` only at the loop's next read; the window owns
    /// the word and sees it change at once, so its surfaces never say
    /// `landing since …` beside a `∞` chip. One map lookup, and the
    /// session's Autopilot (leaf locks) only where a hold stands.
    #[must_use]
    pub(crate) fn shown_hold(&self, session: u64) -> Option<HeldGlass> {
        let hold = self.harness_glass.get(&session)?.hold?;
        let glass = self.chip_glass(session);
        (crate::claude_lights::ChipFace::of(self.session_autopilot(session).1, &glass)
            == crate::claude_lights::ChipFace::Land)
            .then_some(hold)
    }

    /// Whether the act ticker's label is shown at all: `[claude_footer]
    /// acts` on (the default) and serious mode off
    /// (`SeriousEffect::HarnessTicker`). Off either way, acts still go into
    /// the trail, and the title and the page keep them.
    #[must_use]
    pub(crate) fn harness_acts_shown(&self) -> bool {
        self.config.claude_footer_acts()
            && self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::HarnessTicker)
    }

    /// When the harness's glass next needs the loop
    /// (`metrics::DeadlineOwner::HarnessActs`): the end of a label on show,
    /// or a re-read Settings ▸ Harness owes while it is on screen (a page
    /// that left owes none, [`Self::expire_harness_acts`]). `None`: no label
    /// is lit and no read is owed, so nothing is folded and no clock is read.
    #[must_use]
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

    /// What the loop folds for the harness's glass on its way to a wait
    /// (`metrics::DeadlineOwner::HarnessActs`): what is due is done first
    /// ([`Self::expire_harness_acts`]: an ended label goes, a re-read that
    /// came due starts), so the instant handed to the loop is the NEXT one,
    /// never one already past. Folded before the expiry, the deadline that
    /// woke the loop was armed again, already past: a second wake for every
    /// label's end, booked as a `past_deadline_arms`.
    pub(crate) fn harness_acts_wait(&mut self) -> Option<Instant> {
        self.expire_harness_acts();
        self.harness_acts_deadline()
    }

    /// The labels whose dwell ended go, and their footers are posted; a
    /// re-read the page owes is started once due, while the page is on
    /// screen, and forgotten — with its `Newer` mark — once the page has
    /// left it. With no label lit and no read owed, nothing is read — not
    /// even the clock.
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

    /// Session `id` is retired: its glass goes with it.
    pub(crate) fn forget_harness_glass(&mut self, id: u64) {
        self.harness_glass.remove(&id);
    }
}

impl SessionGlass {
    /// A SEAMLESS UPDATE'S CARRIED END (§11.2.3, item 8; slice 2b,
    /// `App::adopt_end_carry`): the end the outgoing process held, `face` at
    /// its own clock `at`, put back as it stood — the session's first end in
    /// this process, seen where it was seen — where none stands (an end told
    /// here since is newer, and wins). It comes in CHEERED (§11.3.4): the
    /// kitty's cheer is the outgoing process's, and one it had not played
    /// by the update is not played after it (§11.3.6: no cheer for a carried
    /// end), so [`SessionGlass::landed`] hands the successor's pet nothing
    /// for it. Whether it was put.
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
