// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The decision of a [`super::SpinScene::Unattended`] take: a window at the shipped motion, rest
//! and effect defaults, focused and on glass, that nobody touched for the attention window
//! ([`ATTENTION`]), judged over its row's window by counts, never by time.
//!
//! [`decide`] is pure: it reads the take's `ctl metrics` lines and `trail status` replies at the
//! window's two edges, with the window's length as the bench's clock bounds it, and passes it,
//! fails it naming each bound it broke, leaves it undecided naming why its counts are no
//! evidence (a loop behind the readings' cadence, a reading handed over late enough to grey the
//! deck, frames only a loop late on its timers explains), refuses it (the window took an event or
//! lost its state) or leaves it unanswered (a reply lacks a field the take reads). The CPU the
//! window took is reported beside it (`Sampled`), never judged.

use std::time::Duration;

use super::{BLINK_LANE, LIVE_LANE, SpinRow, fires_between};
use crate::measure::instance::{CouldNotRun, field, uint_field};
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

/// What an unattended take read: the `ctl metrics` lines as its window opened (just after
/// `metrics reset`) and as it ended, its `trail status` replies at the same two edges, and how
/// long the window between the two `metrics` reads can have run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnattendedReadings<'a> {
    /// The least the window can have run: the row's window, which the take waits out between
    /// its two `metrics` reads.
    pub at_least: Duration,
    /// The most: from just before the opening `metrics` read to just after the closing one, by
    /// the bench's clock.
    pub at_most: Duration,
    pub opened: &'a str,
    pub ended: &'a str,
    pub opened_trail: &'a str,
    pub ended_trail: &'a str,
}

/// The window's counts the decision read, each between the two `metrics` reads but the wakes.
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
    /// Every loop wake since `metrics reset` (`event_wakes`).
    pub wakes: u64,
    /// The arms past their own deadline.
    pub arms: u64,
}

/// What the code lets an unattended window do in a window of `at_least` to `at_most`, with the
/// blink, live-edge and timer fires it counted. Where a bound is the decision's reading rather
/// than the code's, its doc says so.
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
}

impl UnattendedBounds {
    /// The bounds of `row`'s window that ran at least `at_least` and at most `at_most`, from its
    /// `counts` of the blink's, the live edge's and every owner's fires.
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
    /// The take's [`FIELDS`] as its window opened, and as it ended.
    pub opened: String,
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
             frames_late_max={} wakes={} wakes_max={} arms={} opened=[{}] ended=[{}]",
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
            self.opened,
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
/// The take is refused where either `trail status` reply does not read the scene's state
/// ([`unready`]), or the window took any event other than a redraw since `metrics reset`
/// (`os_window_events`): a touch attends the window again and a focus or occlusion change moves
/// what it draws, so the counts would be another window's. Otherwise, with its
/// [`UnattendedBounds`], it fails, naming each, where an owner the row bounds fired past its
/// bound, the live edge past [`UnattendedBounds::greys_max`], [`READING_LANE`] past
/// [`UnattendedBounds::readings_max`] (work under the readings' owner that their asks and the
/// band's polls do not account for: a loop that falls behind fires fewer), the frames past
/// [`UnattendedBounds::frames_late_max`], the wakes past [`UnattendedBounds::wakes_max`] or the
/// past-deadline arms past the row's. Within every bound it is undecided, naming each reason,
/// where [`READING_LANE`] fired under [`UnattendedBounds::readings_min`] (a loop that fell behind
/// the readings' cadence, as on a machine that starved it), where the live edge fired at all (a
/// still deck's grey: a reading handed over [`STALE_AFTER`] or more after its sample, as by a
/// probe thread a machine starved of its Background lane), or where the frames passed
/// [`UnattendedBounds::frames_max`] (frames only flips another owner's wake carried or greys
/// explain). Any of these leaves its quiet counts no evidence. Else it passes.
///
/// # Errors
/// [`CouldNotRun::Refused`] for a take refused as above; [`CouldNotRun::Unanswered`] where a
/// reply lacks a field the take reads ([`unanswered`], `os_window_events`,
/// `deadline_fires_by_owner`, `timer_wakes`, `frames`, `past_deadline_arms`, `event_wakes`).
pub fn decide(
    row: &SpinRow,
    r: &UnattendedReadings<'_>,
) -> Result<UnattendedDecision, CouldNotRun> {
    for (edge, trail) in [("opened", r.opened_trail), ("ended", r.ended_trail)] {
        if let Some(why) = unanswered(trail) {
            return Err(CouldNotRun::Unanswered(why.into()));
        }
        if let Some(off) = unready(trail) {
            return Err(CouldNotRun::Refused(format!(
                "the unattended window read {off} as its window {edge}, not a focused, \
                 unattended window on glass whose pet sleeps deep or is not drawn ({})",
                fields(trail)
            )));
        }
    }
    let events = uint_field(r.ended, "os_window_events").ok_or_else(|| {
        CouldNotRun::Unanswered("ctl metrics does not report os_window_events".into())
    })?;
    if events > 0 {
        return Err(CouldNotRun::Refused(format!(
            "the window took {events} {} other than a redraw ({}) in its window: any can attend \
             it or move what it draws",
            if events == 1 { "event" } else { "events" },
            field(r.ended, "os_window_events_by_kind").unwrap_or("kinds unreported")
        )));
    }
    let fired = |owner: &'static str| {
        fires_between(r.opened, r.ended, owner).ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics does not report deadline_fires_by_owner".into())
        })
    };
    let between = |key: &str| {
        uint_field(r.ended, key)
            .zip(uint_field(r.opened, key))
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
        timer_wakes: between("timer_wakes")?,
        frames: between("frames")?,
        wakes: uint_field(r.ended, "event_wakes").ok_or_else(|| {
            CouldNotRun::Unanswered("ctl metrics does not report event_wakes".into())
        })?,
        arms: between("past_deadline_arms")?,
    };
    let bounds = UnattendedBounds::of(row, r.at_least, r.at_most, &counts);
    let mut breaches = Vec::new();
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
        opened: fields(r.opened_trail),
        ended: fields(r.ended_trail),
    })
}

#[cfg(test)]
#[path = "spin_unattended_tests.rs"]
mod tests;
