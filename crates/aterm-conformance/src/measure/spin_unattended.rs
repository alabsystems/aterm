// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The decision of a [`super::SpinScene::Unattended`] take: a window at the shipped motion, rest
//! and effect defaults, focused and on glass, that nobody touched for the attention window
//! ([`ATTENTION`]), judged over its row's window by counts, never by time.
//!
//! [`decide`] is pure: it reads the readiness wait's last `trail status` reply, taken just after
//! the take's one `metrics reset`, and the `ctl metrics` line read just after it, then the `ctl
//! metrics` lines at the window's two edges, the first [`SETTLE`] after that one, and its closing
//! `trail status` reply, with the window's length as the bench's clock bounds it, and passes it,
//! fails it naming each bound it broke, leaves it undecided naming why its counts are no evidence
//! (a loop behind the readings' cadence, a reading handed over late enough to grey the deck,
//! frames only a loop late on its timers explains, journal or facts fires only a loop that ran
//! late explains), refuses it (the window took an event or lost its state) or leaves it
//! unanswered (a reply lacks a field the take reads). The CPU the window took is reported beside
//! it (`Sampled`), never judged.
//!
//! The window opens with no `trail status` of its own: that read is a wake that is activity
//! (`aterm-gui`'s `Wake::only_observes` is false for `Wake::TrailStatus`), a desk mark that owes
//! the crash journal a capture and the disk walker a read of the panes' facts after it, so a read
//! at the open would book its own work in the window. The readiness wait's last read stands for
//! the opening edge instead, and its work ends inside the settle ([`SETTLE`]) on a loop that ran
//! less late than [`spilled`] reads. Every `metrics` line the decision reads runs from the one
//! reset, so nothing between two of its reads goes uncounted.

use std::time::Duration;

use super::{BLINK_LANE, LIVE_LANE, SpinRow, fires_between};
use crate::measure::instance::{CouldNotRun, field, uint_field};
use crate::measure::rewind::ms_field_us;
use crate::measure::starvation::Disposition;

/// How long after a person's last touch a window stays attended: `aterm-gui`'s
/// `window_rest::ATTENTION`, private to that crate. The focus coming to a window touches it
/// whoever brought it (`App::note_rest_event`), so a take's window is attended until at least
/// this long after its raise.
pub const ATTENTION: Duration = Duration::from_secs(40);

/// The shared readings' period while the bars are in view, aterm has a focused window, and
/// neither it nor any other window on screen is awake and attended, with no page on screen that
/// reads them: `aterm-gui`'s `vitals_host::BACKGROUND_PERIOD`. Each reading is asked on its own
/// half-slot of a wall-clock grid of this period (`vitals_host::due_on_phase`), one at most per
/// slot.
pub const READING_PERIOD: Duration = Duration::from_secs(5);

/// How long the take waits, once it read its window unattended, before the window opens. The
/// readings change to [`READING_PERIOD`] as the attention lapses, and `vitals_host::due_on_phase`
/// keeps the due the last focused reading armed, half to one and a half `FOCUSED_PERIOD` (1 s)
/// after it: one more reading may be asked on it, under a second and a half after the lapse, and
/// the first on the background grid comes at the first half-slot at least half a
/// [`READING_PERIOD`] after the last ask. The take reads the window unattended after the lapse,
/// so from 4 s after that read a loop on its timers asks a reading at every half-slot of the
/// background grid, which [`UnattendedBounds::readings_min`] counts on.
///
/// The settle also outlasts the work the desk marks before it owe, that read's among them
/// ([`JOURNAL_FIRES_PER_MARK`], [`FACTS_FIRES_PER_MARK`]): the journal's last fire for them comes
/// at most two [`MIN_WRITE_INTERVAL`]s after the `metrics` read the window's marks count from,
/// and the facts' read at most [`FACTS_EVERY`] after it, each step later by as much as the loop
/// ran late, so none books in the window of a loop that ran less late than [`spilled`] reads. A
/// mark in the settle itself may book its work in the window; the decision counts it
/// ([`UnattendedCounts::marks`]).
pub const SETTLE: Duration = READING_PERIOD;

/// The Network State band's poll period while a window draws the deck's band and aterm has no
/// network source, as in a scratch world (`aterm`'s `commons_host_enabled` is false under the
/// world's marker), once the lab feed has projected: `aterm-gui`'s `lab_deck::Feed::EVERY`. Each
/// poll is due this long after the last (`App::service_network_state`).
pub const BAND_POLL_PERIOD: Duration = Duration::from_secs(60);

/// A focused window's caret blink: one flip per `aterm-gui`'s `BLINK_INTERVAL` (`lib.rs`), each
/// flip one frame, wherever the blink clock runs. It runs in a live, focused window that does
/// not rest, attended or not (`blink_clock_may_run`), over a blinking caret the rainbow cursor
/// does not own (`blink_clock_armed`: at full motion the shipped rainbow owns a blinking block's).
pub const BLINK_PERIOD: Duration = Duration::from_millis(530);

/// How long a reading speaks for at [`READING_PERIOD`] before a still deck greys: `aterm-gui`'s
/// `stats_live::stale_after`, its `STALE_PERIODS` (3) of the slower of the cadence now and the one
/// the reading was asked at.
pub const STALE_AFTER: Duration = Duration::from_secs(15);

/// The loop wakes a fire that asks one frame takes at most: its timer's, and the surface-ready
/// wake the drawable worker posts where that frame waits for its drawable
/// ([`super::WAKES_PER_NEEDLE_FRAME`]'s count is the first alone).
pub const WAKES_PER_FRAME_FIRE: u64 = 2;

/// The deadline owner the shared readings are asked under (`App::vitals_deadline`). `aterm-gui`
/// folds the Network State band's poll (`App::network_state_deadline`) and the Lab page's reads
/// (`App::lab_state_deadline`) under the same owner, so its count holds them too.
pub const READING_LANE: &str = "system_vitals";

/// The deadline owner the crash journal's captures fire as (`App::crash_journal_wake`).
/// `aterm-gui` folds an editor document's journal retry (`App::document_preparation_wake`) under
/// the same owner; the take's window holds no editor.
pub const JOURNAL_LANE: &str = "crash_journal";

/// The deadline owner the disk walker's read of the panes' facts fires as
/// (`App::diskwalk_deadline`).
pub const FACTS_LANE: &str = "disk_facts";

/// The crash journal's least interval between captures: `aterm-gui`'s
/// `crash_journal::MIN_WRITE_INTERVAL`, private to that crate.
pub const MIN_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// How long after a read of the panes' facts the next is due while a scope is demanded:
/// `aterm-gui`'s `diskwalk_host::FACTS_EVERY`, private to that crate.
pub const FACTS_EVERY: Duration = Duration::from_secs(1);

/// The most [`JOURNAL_LANE`] fires one desk mark can owe, by `aterm-gui`'s `crash_journal::Lane`.
/// A mark (`desk_activity`, counted wherever the loop marks the journal and the walker: a wake
/// that is activity, a window event, the restore drain, a pane's front program moving) makes the
/// journal owe a capture (`note_activity`), and every capture taken after one owes a settle
/// capture [`MIN_WRITE_INTERVAL`] later (`offer`'s `settle_owed`), a fire. The capture itself runs
/// in the mark's own turn where the last was taken that interval ago or more, and otherwise waits
/// for the interval's end, one fire more. Each fire needs a mark since the capture before the one
/// it follows, so each mark accounts for at most two.
///
/// A capture owed at once (`owed_now`: an owner's words, an agent's publication; or a ticket a
/// word's verb asked, `Durable::ask`) runs in the turn that owed it on a desk the journal may
/// write (`crash_journal_writable`), so it is no fire. A ticket's turn is the journal's own wake
/// (`Wake::CrashJournalOwed`), a mark, as is the writer's wake after a failed write, which the
/// capture again follows: a writer that fails is bounded by its failures, and the row does not
/// judge the journal's writes. The decision reads the take's scratch world as owing none of the
/// lane's other fires: a capture deferred because only titles moved (`TITLE_WRITE_INTERVAL`),
/// which needs a program to retitle a tab; a capture owed at once on a desk the journal may not
/// write yet (off glass, a restore pass queued, a handoff coming), which the take's window, on
/// glass long past its launch, is not; and the settle at the end of a reopened journal's
/// probation, which a scratch world's launch has no journal to reopen for.
pub const JOURNAL_FIRES_PER_MARK: u64 = 2;

/// The most [`FACTS_LANE`] fires one desk mark can owe: a mark makes a read of the panes' facts
/// owed (`DiskwalkHost::note_activity`), the deadline is armed only while one is owed
/// (`facts_deadline`), and a read pays what was owed, so each fire needs a mark since the read
/// before it.
pub const FACTS_FIRES_PER_MARK: u64 = 1;

/// How many fires of a lane the desk marks before the settle may book in the window of a loop
/// that ran up to `late` past a due: of the `steps` fires those marks can owe after the `metrics`
/// read the window's marks count from ([`JOURNAL_FIRES_PER_MARK`] of [`MIN_WRITE_INTERVAL`],
/// [`FACTS_FIRES_PER_MARK`] of [`FACTS_EVERY`]), the `k`th comes at most `k` steps of `step` and
/// `late` after that read, and the window opens at least [`SETTLE`] after it, so each `k` with
/// `k·(step + late) ≥ SETTLE`. Each fire needs a mark since the capture or read before the one it
/// follows, so the marks before that read owe the window no more than `steps` together, however
/// many they were.
#[must_use]
pub fn spilled(steps: u64, step: Duration, late: Duration) -> u64 {
    let step = step.saturating_add(late).as_nanos();
    if step == 0 {
        return 0;
    }
    // The least such `k`; every `k` from it to `steps` is one.
    let first = SETTLE.as_nanos().div_ceil(step).max(1);
    u64::try_from(u128::from(steps).saturating_add(1).saturating_sub(first)).unwrap_or(u64::MAX)
}

/// The most a step of owed work can have run after its due, by a `ctl metrics` line: the loop's
/// timer wake furthest past its deadline (`max_wake_late_ms`) and its longest turn
/// (`max_turn_ms`), each since `metrics reset`, read as whole microseconds rounded up from the
/// hundredths of a millisecond the line prints. The decision reads a step as waiting at most for
/// its wake and one turn. `None` where the line lacks either.
#[must_use]
pub fn late(line: &str) -> Option<Duration> {
    let wake = ms_field_us(line, "max_wake_late_ms")?;
    let turn = ms_field_us(line, "max_turn_ms")?;
    Some(Duration::from_micros(wake.saturating_add(turn)))
}

/// The `trail status` fields an unattended take records at each edge: what its readiness reads,
/// and the motion and shed its effects saw, which load or the Mac's Reduce Motion can move and
/// which the take records without requiring.
pub const FIELDS: [&str; 10] = [
    "focused",
    "attended",
    "sight",
    "resting",
    "motion",
    "motion_stage",
    "shed",
    "pet_active",
    "pet_deep",
    "pet_action",
];

/// What a `trail status` reply must read for the take to measure an unattended window: focused
/// (a window without the focus blinks no caret, so its counts would bound less), unattended, and
/// on glass (`sight=visible`: a covered window draws on its own period, a minimised one draws
/// nothing). Beside these its pet must sleep deep or not be drawn ([`unready`]).
const READY: [(&str, &str); 3] = [
    ("focused", "true"),
    ("attended", "false"),
    ("sight", "visible"),
];

/// Why a build cannot answer the scene, from a `trail status` reply: one from before the fields
/// reports no `attended=` or no `sight=`, so its window could never read ready, and the take is
/// unanswered rather than refused.
#[must_use]
pub fn unanswered(line: &str) -> Option<&'static str> {
    if field(line, "attended").is_none() {
        Some("ctl trail status does not report attended")
    } else if field(line, "sight").is_none() {
        Some("ctl trail status does not report sight")
    } else {
        None
    }
}

/// The first field a `trail status` reply does not read as an unattended take needs, as
/// `key=value` (`?` for one it lacks), or its pet's two fields where the pet is drawn and not in
/// deep sleep; `None` when the take can decide.
#[must_use]
pub fn unready(line: &str) -> Option<String> {
    if let Some((key, _)) = READY
        .iter()
        .find(|(key, want)| field(line, key) != Some(*want))
    {
        return Some(format!("{key}={}", field(line, key).unwrap_or("?")));
    }
    let still =
        field(line, "pet_deep") == Some("true") || field(line, "pet_active") == Some("false");
    (!still).then(|| {
        format!(
            "pet_active={} pet_deep={}",
            field(line, "pet_active").unwrap_or("?"),
            field(line, "pet_deep").unwrap_or("?")
        )
    })
}

/// The [`FIELDS`] of a `trail status` reply, space-separated, `?` for one it lacks.
#[must_use]
pub fn fields(line: &str) -> String {
    FIELDS
        .iter()
        .map(|key| format!("{key}={}", field(line, key).unwrap_or("?")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What an unattended take read, every `ctl metrics` line since its one `metrics reset`, made
/// just before the readiness wait's last `trail status` read: that reply, the `metrics` line read
/// just after it, the `metrics` lines as its window opened ([`SETTLE`] after that one) and as it
/// ended, its `trail status` reply as it ended, and how long the window between the opening and
/// closing `metrics` reads can have run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnattendedReadings<'a> {
    /// The least the window can have run: the row's window, which the take waits out between
    /// its two `metrics` reads.
    pub at_least: Duration,
    /// The most: from just before the opening `metrics` read to just after the closing one, by
    /// the bench's clock.
    pub at_most: Duration,
    /// The `trail status` reply that ended the readiness wait, the window's opening edge.
    pub ready_trail: &'a str,
    /// The `metrics` line read just after [`Self::ready_trail`]: the desk marks the settle and
    /// the window count from.
    pub ready: &'a str,
    pub opened: &'a str,
    pub ended: &'a str,
    pub ended_trail: &'a str,
}

/// The window's counts the decision read, each between the two `metrics` reads but the marks
/// and the lateness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnattendedCounts {
    /// The fires of each owner the row bounds ([`SpinRow::max_owner_fires`]), in its order.
    pub owner_fires: Vec<(&'static str, u64)>,
    /// The deck's live edge's fires ([`LIVE_LANE`]).
    pub live: u64,
    /// The blink lane's fires ([`BLINK_LANE`]): a take whose blink clock ran fires it.
    pub blink: u64,
    /// The fires of [`READING_LANE`].
    pub readings: u64,
    /// Every owner's fires: the loop's timer wakes (`timer_wakes`).
    pub timer_wakes: u64,
    /// The frames presented.
    pub frames: u64,
    /// Every loop wake (`event_wakes`).
    pub wakes: u64,
    /// The arms past their own deadline.
    pub arms: u64,
    /// The fires of [`JOURNAL_LANE`].
    pub journal: u64,
    /// The fires of [`FACTS_LANE`].
    pub facts: u64,
    /// The desk marks in the settle and the window (`desk_activity`), from the line read just
    /// after the readiness wait's last read to the closing one: each owes work that may book in
    /// the window.
    pub marks: u64,
    /// The most a step of the work the marks before the settle owe can have run past its due:
    /// [`late`] of the closing line, whose maxima run from the reset, which came before every
    /// such step.
    pub late: Duration,
}

/// What the code lets an unattended window do in a window of `at_least` to `at_most`, with the
/// blink, live-edge and timer fires it counted and the desk marks it and its settle counted.
/// Where a bound is the decision's reading rather than the code's, its doc says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnattendedBounds {
    /// The fewest [`READING_LANE`] fires a window whose loop kept its cadence books. The window
    /// opens [`SETTLE`] after the take read it unattended, so every half-slot of the readings'
    /// grid in it gets its ask, and its two `metrics` reads lie at least `at_least` apart: a loop
    /// on its timers books a reading for each whole [`READING_PERIOD`] of `at_least`, and a poll
    /// of the band's for each whole [`BAND_POLL_PERIOD`] (each due a period after the last), all
    /// but one, which another owner's earlier deadline may have woken and booked (a timer wake
    /// counts one owner's fire, the earliest's). Fewer is a loop that fell behind.
    pub readings_min: u64,
    /// The most [`READING_LANE`] fires the window's asks take: a reading in each slot of
    /// [`READING_PERIOD`] `at_most` can touch, and the band's poll in each of
    /// [`BAND_POLL_PERIOD`]'s. A loop that falls behind fires fewer, never more.
    pub readings_max: u64,
    /// The most [`LIVE_LANE`] fires the decision reads as a still deck's greys. An unattended
    /// deck holds still (`window_rest::Rest::deck_moves` needs attention), and a still deck's
    /// live edge fires only at its grey (`stats_live`'s `next_change` gates the grid on motion):
    /// [`STALE_AFTER`] after the sample of the newest reading it adopted, once, till it adopts a
    /// newer one. The decision admits one a reading slot `at_most` can touch, and one for the
    /// reading adopted before the window opened; more it reads as a live edge that moved.
    pub greys_max: u64,
    /// The flips the blink's own fires drew: its fires, at most one a slot of [`BLINK_PERIOD`]
    /// `at_most` can touch.
    pub flips: u64,
    /// The most flips a timer wake booked to another owner may have drawn, where the blink lane
    /// fired: the blink's deadline had passed when that owner's earlier one woke the loop, as on a
    /// loop late on its timers. At most one a timer wake of another owner, and no more than the
    /// blink's slots its own fires left.
    pub carried_max: u64,
    /// The most frames a window presents whose loop kept its timers: one a [`Self::flips`], one a
    /// reading slot (each reading that lands asks a frame of a window that draws the bars,
    /// `App::show_vitals_reading`), and one a band poll slot (a poll that moved what the band
    /// draws). By a reading of `aterm-gui`'s frame sources nothing else plans one in such a window:
    /// a still deck's needle holds still with it, and a pet in deep sleep or not drawn asks none.
    pub frames_max: u64,
    /// The most frames the decision does not fail: [`Self::frames_max`], one a flip in
    /// [`Self::carried_max`], and one a grey ([`LIVE_LANE`] fires up to [`Self::greys_max`]), which
    /// asks a frame as the deck's freshness changes.
    pub frames_late_max: u64,
    /// The most loop wakes the decision does not fail: the row's [`SpinRow::max_wakes`] for the
    /// window's readings, polls and timers, [`WAKES_PER_FRAME_FIRE`] for each of its
    /// [`Self::flips`] and greys, and one for each flip in [`Self::carried_max`], whose timer wake
    /// is another owner's.
    pub wakes_max: u64,
    /// The most [`JOURNAL_LANE`] fires the window's and its settle's desk marks owe:
    /// [`JOURNAL_FIRES_PER_MARK`] each. A window that woke for nothing but its readings, which
    /// only observe, owes none.
    pub journal_max: u64,
    /// The most [`FACTS_LANE`] fires the same marks owe: [`FACTS_FIRES_PER_MARK`] each.
    pub facts_max: u64,
    /// The most [`JOURNAL_LANE`] fires the marks before the settle may book in the window, by how
    /// late the loop ran ([`spilled`]): fires past [`Self::journal_max`] by no more are no
    /// evidence, and past it by more are work no mark asked.
    pub journal_spilled: u64,
    /// The same of [`FACTS_LANE`]'s, past [`Self::facts_max`].
    pub facts_spilled: u64,
}

impl UnattendedBounds {
    /// The bounds of `row`'s window that ran at least `at_least` and at most `at_most`, from its
    /// `counts` of the blink's, the live edge's and every owner's fires, of the desk marks in it
    /// and in its settle, and of how late its loop ran.
    #[must_use]
    pub fn of(
        row: &SpinRow,
        at_least: Duration,
        at_most: Duration,
        counts: &UnattendedCounts,
    ) -> Self {
        let reading_slots = slots_at_most(at_most, READING_PERIOD);
        let poll_slots = slots_at_most(at_most, BAND_POLL_PERIOD);
        let blink_slots = slots_at_most(at_most, BLINK_PERIOD);
        let flips = counts.blink.min(blink_slots);
        let carried_max = if counts.blink > 0 {
            blink_slots
                .saturating_sub(flips)
                .min(counts.timer_wakes.saturating_sub(counts.blink))
        } else {
            0
        };
        let greys_max = reading_slots.saturating_add(1);
        let greys = counts.live.min(greys_max);
        let frames_max = flips
            .saturating_add(reading_slots)
            .saturating_add(poll_slots);
        Self {
            readings_min: slots_at_least(at_least, READING_PERIOD)
                .saturating_add(slots_at_least(at_least, BAND_POLL_PERIOD))
                .saturating_sub(1),
            readings_max: reading_slots.saturating_add(poll_slots),
            greys_max,
            flips,
            carried_max,
            frames_max,
            frames_late_max: frames_max.saturating_add(carried_max).saturating_add(greys),
            wakes_max: row
                .max_wakes
                .saturating_add(WAKES_PER_FRAME_FIRE.saturating_mul(flips.saturating_add(greys)))
                .saturating_add(carried_max),
            journal_max: JOURNAL_FIRES_PER_MARK.saturating_mul(counts.marks),
            facts_max: FACTS_FIRES_PER_MARK.saturating_mul(counts.marks),
            journal_spilled: spilled(JOURNAL_FIRES_PER_MARK, MIN_WRITE_INTERVAL, counts.late),
            facts_spilled: spilled(FACTS_FIRES_PER_MARK, FACTS_EVERY, counts.late),
        }
    }
}

/// The fewest slots of a grid of `period` a span of at least `span` holds: its whole periods.
fn slots_at_least(span: Duration, period: Duration) -> u64 {
    span.as_nanos()
        .checked_div(period.as_nanos())
        .map_or(0, |n| u64::try_from(n).unwrap_or(u64::MAX))
}

/// The most slots of a grid of `period` a span of at most `span` can touch: its whole periods,
/// and one more where it opens just before a slot.
fn slots_at_most(span: Duration, period: Duration) -> u64 {
    span.as_nanos()
        .checked_div(period.as_nanos())
        .map_or(u64::MAX, |n| {
            u64::try_from(n).unwrap_or(u64::MAX).saturating_add(1)
        })
}

/// An unattended take's decision ([`decide`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnattendedDecision {
    pub verdict: Disposition,
    /// Each bound the counts broke, by name, for a fail; each reason the counts are no evidence,
    /// for an undecided take; empty for a pass.
    pub why: String,
    pub counts: UnattendedCounts,
    pub bounds: UnattendedBounds,
    /// The take's [`FIELDS`] as the readiness wait read it ready, the window's opening edge, and
    /// as its window ended.
    pub ready: String,
    pub ended: String,
}

impl std::fmt::Display for UnattendedDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let c = &self.counts;
        let b = &self.bounds;
        write!(
            f,
            "unattended_verdict={:?} owner_fires={:?} live={} greys_max={} blink={} \
             readings={} readings_cadence={}..={} timer_wakes={} frames={} frames_max={} \
             frames_late_max={} wakes={} wakes_max={} arms={} marks={} late={:?} \
             journal={} journal_max={} journal_spilled={} facts={} facts_max={} \
             facts_spilled={} ready=[{}] ended=[{}]",
            self.verdict,
            c.owner_fires,
            c.live,
            b.greys_max,
            c.blink,
            c.readings,
            b.readings_min,
            b.readings_max,
            c.timer_wakes,
            c.frames,
            b.frames_max,
            b.frames_late_max,
            c.wakes,
            b.wakes_max,
            c.arms,
            c.marks,
            c.late,
            c.journal,
            b.journal_max,
            b.journal_spilled,
            c.facts,
            b.facts_max,
            b.facts_spilled,
            self.ready,
            self.ended
        )?;
        if !self.why.is_empty() {
            write!(f, " why=\"{}\"", self.why)?;
        }
        Ok(())
    }
}

/// Decide an unattended take from what it read ([`UnattendedReadings`]) against its `row`.
///
/// The take is refused where the readiness wait's last `trail status` reply (the window's
/// opening edge) or its closing one does not read the scene's state ([`unready`]), or the window
/// took any event other than a redraw (`os_window_events`) since `metrics reset`, which came just
/// before that read: a touch attends the window again and a focus or occlusion change moves what
/// it draws, so the counts would be another window's. Otherwise, with its [`UnattendedBounds`],
/// it fails, naming each, where an owner the row bounds fired past its bound, the live edge past
/// [`UnattendedBounds::greys_max`], [`READING_LANE`] past [`UnattendedBounds::readings_max`]
/// (work under the readings' owner that their asks and the band's polls do not account for: a
/// loop that falls behind fires fewer), the frames past [`UnattendedBounds::frames_late_max`],
/// the wakes past [`UnattendedBounds::wakes_max`], the past-deadline arms past the row's, or
/// [`JOURNAL_LANE`] or [`FACTS_LANE`] past what the window's and its settle's desk marks owe
/// ([`UnattendedBounds::journal_max`], [`UnattendedBounds::facts_max`]) and the fires the marks
/// before the settle may book in the window of a loop as late as it ran
/// ([`UnattendedBounds::journal_spilled`], [`UnattendedBounds::facts_spilled`]): work no mark
/// asked. Within every bound it is undecided, naming each reason, where [`READING_LANE`] fired
/// under [`UnattendedBounds::readings_min`] (a loop that fell behind the readings' cadence, as on
/// a machine that starved it), where the live edge fired at all (a still deck's grey: a reading
/// handed over [`STALE_AFTER`] or more after its sample, as by a probe thread a machine starved of
/// its Background lane), where the frames passed [`UnattendedBounds::frames_max`] (frames only
/// flips another owner's wake carried or greys explain), or where [`JOURNAL_LANE`] or
/// [`FACTS_LANE`] fired past what the marks in the settle and the window owe, by no more than
/// the marks before the settle may book there. Any of these leaves its quiet counts no evidence.
/// Else it passes.
///
/// # Errors
/// [`CouldNotRun::Refused`] for a take refused as above; [`CouldNotRun::Unanswered`] where a
/// reply lacks a field the take reads ([`unanswered`], `os_window_events`,
/// `deadline_fires_by_owner`, `timer_wakes`, `frames`, `past_deadline_arms`, `event_wakes`,
/// `desk_activity`, `max_wake_late_ms`, `max_turn_ms`).
pub fn decide(
    row: &SpinRow,
    r: &UnattendedReadings<'_>,
) -> Result<UnattendedDecision, CouldNotRun> {
    for (edge, trail) in [
        (
            "as the readiness wait read it, before its settle",
            r.ready_trail,
        ),
        ("as its window ended", r.ended_trail),
    ] {
        if let Some(why) = unanswered(trail) {
            return Err(CouldNotRun::Unanswered(why.into()));
        }
        if let Some(off) = unready(trail) {
            return Err(CouldNotRun::Refused(format!(
                "the unattended window read {off} {edge}, not a focused, unattended window on \
                 glass whose pet sleeps deep or is not drawn ({})",
                fields(trail)
            )));
        }
    }
    let events = uint_field(r.ended, "os_window_events").ok_or_else(|| {
        CouldNotRun::Unanswered("ctl metrics does not report os_window_events".into())
    })?;
    if events > 0 {
        return Err(CouldNotRun::Refused(format!(
            "the window took {events} {} other than a redraw ({}) from just before the readiness \
             wait's last read to its window's end: any can attend it or move what it draws",
            if events == 1 { "event" } else { "events" },
            field(r.ended, "os_window_events_by_kind").unwrap_or("kinds unreported")
        )));
    }
    let fired = |owner: &'static str| {
        fires_between(r.opened, r.ended, owner).ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics does not report deadline_fires_by_owner".into())
        })
    };
    let between = |from: &str, key: &str| {
        uint_field(r.ended, key)
            .zip(uint_field(from, key))
            .map(|(end, start)| end.saturating_sub(start))
            .ok_or_else(|| CouldNotRun::Unanswered(format!("ctl metrics does not report {key}")))
    };
    let counts = UnattendedCounts {
        owner_fires: row
            .max_owner_fires
            .iter()
            .map(|&(owner, _)| fired(owner).map(|n| (owner, n)))
            .collect::<Result<_, _>>()?,
        live: fired(LIVE_LANE)?,
        blink: fired(BLINK_LANE)?,
        readings: fired(READING_LANE)?,
        timer_wakes: between(r.opened, "timer_wakes")?,
        frames: between(r.opened, "frames")?,
        wakes: between(r.opened, "event_wakes")?,
        arms: between(r.opened, "past_deadline_arms")?,
        journal: fired(JOURNAL_LANE)?,
        facts: fired(FACTS_LANE)?,
        marks: between(r.ready, "desk_activity")?,
        late: late(r.ended).ok_or_else(|| {
            CouldNotRun::Unanswered(
                "ctl metrics does not report max_wake_late_ms and max_turn_ms".into(),
            )
        })?,
    };
    let bounds = UnattendedBounds::of(row, r.at_least, r.at_most, &counts);
    let mut breaches = Vec::new();
    let mut late_work = Vec::new();
    for (&(owner, max), &(_, n)) in row.max_owner_fires.iter().zip(&counts.owner_fires) {
        if n > max {
            breaches.push(format!("{owner} fired {n} times, at most {max}"));
        }
    }
    if counts.live > bounds.greys_max {
        breaches.push(format!(
            "{LIVE_LANE} fired {} times, past the {} greys a still deck's readings make in the \
             window: its live edge moved",
            counts.live, bounds.greys_max
        ));
    }
    if counts.readings > bounds.readings_max {
        breaches.push(format!(
            "{READING_LANE} fired {} times, past the {} the readings every {READING_PERIOD:?} and \
             the band's poll every {BAND_POLL_PERIOD:?} ask in a window of at most {:?}: work \
             under the readings' owner they do not account for",
            counts.readings, bounds.readings_max, r.at_most
        ));
    }
    if counts.frames > bounds.frames_late_max {
        breaches.push(format!(
            "frames={}, past the {} its blink, readings, band polls and greys ask, with the \
             flips other owners' wakes may carry",
            counts.frames, bounds.frames_late_max
        ));
    }
    if counts.wakes > bounds.wakes_max {
        breaches.push(format!(
            "event_wakes={}, past the {}: the row's {} and what its flips and greys take",
            counts.wakes, bounds.wakes_max, row.max_wakes
        ));
    }
    if counts.arms > row.max_arms {
        breaches.push(format!(
            "past_deadline_arms={}, past the row's {}",
            counts.arms, row.max_arms
        ));
    }
    for (lane, fires, max, spilled, each, what) in [
        (
            JOURNAL_LANE,
            counts.journal,
            bounds.journal_max,
            bounds.journal_spilled,
            JOURNAL_FIRES_PER_MARK,
            "a capture and its settle",
        ),
        (
            FACTS_LANE,
            counts.facts,
            bounds.facts_max,
            bounds.facts_spilled,
            FACTS_FIRES_PER_MARK,
            "a read of the panes' facts",
        ),
    ] {
        if fires <= max {
            continue;
        }
        let owed = format!(
            "{lane} fired {fires} times, past the {max} its {} desk marks in the settle and the \
             window owe ({each} a mark: {what})",
            counts.marks
        );
        if fires > max.saturating_add(spilled) {
            breaches.push(if spilled == 0 {
                format!("{owed}: work no mark asked")
            } else {
                format!(
                    "{owed} and the {spilled} the marks before the settle may book there on a \
                     loop that ran up to {:?} late: work no mark asked",
                    counts.late
                )
            });
        } else {
            late_work.push(format!(
                "{owed}, by no more than the {spilled} the marks before the settle may book there \
                 on a loop that ran up to {:?} late",
                counts.late
            ));
        }
    }
    let mut unproved = Vec::new();
    if counts.readings < bounds.readings_min {
        unproved.push(format!(
            "{READING_LANE} fired {} times, under the {} the readings every {READING_PERIOD:?} \
             and the band's poll every {BAND_POLL_PERIOD:?} book in a window of at least {:?}: \
             the loop fell behind their cadence",
            counts.readings, bounds.readings_min, r.at_least
        ));
    }
    if counts.live > 0 {
        unproved.push(format!(
            "{LIVE_LANE} fired {} times: a still deck greys {STALE_AFTER:?} after its newest \
             reading's sample, so a reading was handed over that late",
            counts.live
        ));
    }
    if counts.frames > bounds.frames_max {
        unproved.push(format!(
            "frames={}, past the {} a loop on its timers asks: what is past it only flips \
             another owner's late wake carried, or greys, explain",
            counts.frames, bounds.frames_max
        ));
    }
    unproved.extend(late_work);
    let (verdict, why) = if !breaches.is_empty() {
        (Disposition::Fail, breaches.join("; "))
    } else if !unproved.is_empty() {
        (
            Disposition::Unproved,
            format!("{}: its quiet counts are no evidence", unproved.join("; ")),
        )
    } else {
        (Disposition::Pass, String::new())
    };
    Ok(UnattendedDecision {
        verdict,
        why,
        counts,
        bounds,
        ready: fields(r.ready_trail),
        ended: fields(r.ended_trail),
    })
}

#[cfg(test)]
#[path = "spin_unattended_tests.rs"]
mod tests;
