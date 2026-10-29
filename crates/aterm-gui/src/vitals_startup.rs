// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The marks a launch notes on the stats deck's strips for how the run before it stopped
//! (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §6). A stop is dated by the stopped run's last
//! checkpoint, which the strip's restore reads ([`aterm_vitals::Vitals::restore_strip_history`]),
//! or, where the strip is older than the stop's evidence, by that evidence: a crash by the
//! file its handler wrote at the crash ([`date_crash`]), and a kill by the killed run's launch
//! ([`date_unseen_kill`]).

use std::path::Path;

use aterm_vitals::model::STRIP_HORIZON_MS;
use aterm_vitals::ring::BUCKET_MS;
use aterm_vitals::{GapKind, SavedStrip, StripEvent, StripEventKind, TimeGap};

use crate::logging::{CrashEvidence, KillEvidence};
use crate::vitals_host::VitalsHost;

/// How the startup scan found the run before this one stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PriorStop {
    /// A crash report or a fatal signal's written marker ([`CrashEvidence`]), with that file's
    /// modification time where it read: its writer wrote it at the crash. A panic the run caught
    /// and survived writes a report too, so the time dates a crash only where the strip is older
    /// than the file ([`date_crash`]).
    Crashed(Option<i64>),
    /// A kill's marker alone ([`KillEvidence`]), with that file's modification time where it
    /// read: the killed run made it empty at its launch and nothing wrote it after, so the time
    /// is when that run started, not when it stopped. It dates a crash only where the strip holds
    /// no checkpoint of the killed run ([`date_unseen_kill`]).
    Killed(Option<i64>),
    /// Both, each with its file's time as above, dated in the order they came ([`date_crash`],
    /// [`date_unseen_kill`]), the crash only where its file is newer than the strip. One older than
    /// the killed run's launch is an earlier run's crash, which the killed run never took (it died
    /// before its startup scan), and is dated before the kill; one newer than the launch dates the
    /// killed run's crash, after it.
    CrashedAndKilled(Option<i64>, Option<i64>),
}

impl PriorStop {
    /// The stop the startup scan's evidence names; `None` when it found neither.
    pub(crate) fn of(crash: Option<&CrashEvidence>, killed: Option<&KillEvidence>) -> Option<Self> {
        let crashed = crash.map(|crash| written_ms(&crash.path));
        let killed = killed.map(|killed| written_ms(&killed.marker));
        match (crashed, killed) {
            (Some(crashed), Some(killed)) => Some(Self::CrashedAndKilled(crashed, killed)),
            (Some(crashed), None) => Some(Self::Crashed(crashed)),
            (None, Some(killed)) => Some(Self::Killed(killed)),
            (None, None) => None,
        }
    }

    /// Whether the scan found a kill's marker, which says the run it names died with no exit
    /// path, so no handoff ended it.
    pub(crate) const fn killed(self) -> bool {
        matches!(self, Self::Killed(_) | Self::CrashedAndKilled(..))
    }
}

/// When `path` was last written, in milliseconds since the epoch.
pub(crate) fn written_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// The events a launch notes on the strip for how the run before it stopped, and the times the
/// probe thread's strip restore dates that stop by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartupMarks {
    /// The markers: the restart's.
    pub(crate) events: Vec<StripEvent>,
    /// When the run before this one crashed, by its crash file, for the probe thread's strip
    /// restore ([`VitalsHost::note_crash_at`], [`date_crash`]).
    pub(crate) crashed_at: Option<i64>,
    /// When a killed run before this one launched, for the probe thread's strip restore
    /// ([`VitalsHost::note_killed_run_launch`], [`date_unseen_kill`]).
    pub(crate) killed_since: Option<i64>,
}

impl StartupMarks {
    /// Queues the events for the probe thread's next reading, and a crash's time or a killed
    /// run's launch for its start.
    pub(crate) fn note(self, host: &mut VitalsHost) {
        for event in self.events {
            host.note_strip_event(event);
        }
        if let Some(crash_ms) = self.crashed_at {
            host.note_crash_at(crash_ms);
        }
        if let Some(launched_ms) = self.killed_since {
            host.note_killed_run_launch(launched_ms);
        }
    }
}

/// The marks a launch at `launch_ms` notes for `prior`: a "restarted" event at the launch, and the
/// stop's times for the probe thread, which dates the stop against the kept strip
/// (`vitals_host::open_strip`). A kill notes the killed run's launch: the restore marks the killed
/// run's unclean checkpoint as a crash from its last saved reading, and a gap from the launch would
/// cover the whole killed run, its sleeps and readings with it. The probe thread checkpoints on a
/// reading at least 10 s after its last write, so while readings come the crash starts a bucket or
/// two before the kill; readings are asked by the main loop or a sys pull, and a hung main loop
/// (the usual Force Quit) asks for none, so with no pull meanwhile the crash starts at the last
/// checkpoint before the hang. Where the strip holds no checkpoint of the killed run (no strip
/// file, or only an earlier run's), the kill came before that run's first checkpoint, and the probe
/// thread dates the crash from that run's launch ([`date_unseen_kill`]). A crash's time is its
/// crash file's (a second before the launch where it did not read), and the probe thread then dates
/// the crash from it where the strip is older than it, in time order with a killed run's launch
/// ([`date_crash`]): a crash after the crashed run's last checkpoint draws from that checkpoint,
/// with its "aterm crash" event at the crash, and one before that run's first checkpoint from the
/// crash, after the earlier run's stop. A strip no older than the crash file says its writer ran
/// past it (a panic it caught and survived, or another aterm's crash), and the restore draws the
/// stop as it reads. A last checkpoint, a crash or a launch older than the strips' six hours draws
/// no crash, as the restore dates no crash from a stop that old.
pub(crate) fn startup_marks(prior: PriorStop, launch_ms: i64) -> StartupMarks {
    let events = vec![StripEvent {
        at_unix_ms: launch_ms,
        kind: StripEventKind::Restart,
        label: "restarted".into(),
    }];
    let at = |written_ms: Option<i64>| {
        written_ms.map_or(launch_ms.saturating_sub(1_000), |ms| ms.min(launch_ms))
    };
    let (crashed_at, killed_since) = match prior {
        PriorStop::Crashed(crash) => (Some(at(crash)), None),
        PriorStop::Killed(launched_ms) => (None, launched_ms),
        PriorStop::CrashedAndKilled(crash, launched_ms) => (Some(at(crash)), launched_ms),
    };
    StartupMarks {
        events,
        crashed_at,
        killed_since,
    }
}

/// `kept`, the strip a probe thread restores at `open_ms`, after a crash at `crash_ms`, its crash
/// file's time ([`PriorStop::Crashed`]). A strip whose last saved reading is at or after the crash
/// holds a checkpoint written since, so its writer ran past the crash file (a panic it caught and
/// survived, or another aterm's crash), and is left as it is (`vitals_host::open_strip` also leaves
/// a clean one whose file was written at or after the crash file: only a quit writes a strip clean,
/// never a panic's unwind, and that write takes no reading of its own): the restore draws its stop
/// as it reads, a clean one as a pause where it spans a 10 s bucket and an unclean one as a crash
/// from that reading. A strip whose last reading is before the crash holds no checkpoint since, and
/// gains one at the crash, unclean, so the restore draws the crash gap from the crash with its
/// "aterm crash" event there, where that is inside the strips' six hours. The stop before it draws
/// up to the crash as [`date_unseen_kill`] draws one up to a killed run's launch: where it is the
/// crashed run's own unclean checkpoint, the crash gap runs on back to its last reading, and after
/// an earlier run's clean stop (the crash came before the crashed run's first checkpoint) the time
/// to the crash is a paused gap where it spans a 10 s bucket. The test is by wall clock, as that
/// function's is. A crash at or after `open_ms` (the clock moved back) changes nothing.
pub(crate) fn date_crash(kept: &mut SavedStrip, crash_ms: i64, open_ms: i64) {
    date_unseen_stop(kept, crash_ms, open_ms);
}

/// `kept`, the strip a probe thread restores at `open_ms`, after a kill of a run that launched at
/// `launched_ms` ([`PriorStop::Killed`]). A strip whose last saved reading is at or after that
/// launch holds a checkpoint written since the killed run launched, and is left as it is: the
/// killed run's own, or, where that run was a peer of a lock holder, the holder's. The restore
/// draws its stop as it reads: an unclean one is a crash from that reading, and a clean one (the
/// holder's quit, or the killed run's clean final write, where the kill came in its exit path) is a
/// pause where it spans a 10 s bucket; a stop older than the strips' six hours is neither, and the
/// first reading draws the time since it as a pause. A strip whose last reading is before the
/// launch holds none (it is empty or an earlier run's, however old), so the kill came before the
/// killed run's first checkpoint: `kept` gains the checkpoint that run never wrote, an unclean one
/// at its launch, and the restore draws the crash gap and its "aterm crash" event from the launch
/// where that is inside the strips' six hours. The earlier run's stop up to the launch draws as the
/// restore draws a stop: after an unclean one a crash gap, after a clean one a paused gap where it
/// spans a 10 s bucket. Where that run's last reading is older than the six hours, clean or not, it
/// draws as a paused gap where it spans a bucket, as a first reading draws one after a stop that
/// old; the restore keeps that gap, its start unclipped, while its end is inside the six hours. The
/// test is by wall clock, so a clock set back during the killed run can make that run's own
/// checkpoint read as an earlier run's, and one set back before its launch an earlier run's as one
/// written since; a writer's stamp in the strip would tell them apart, and none is kept. A launch
/// at or after `open_ms` (the clock moved back) changes nothing.
pub(crate) fn date_unseen_kill(kept: &mut SavedStrip, launched_ms: i64, open_ms: i64) {
    date_unseen_stop(kept, launched_ms, open_ms);
}

/// `kept` after a stop at or after `since_ms` that it holds no checkpoint of: where its last saved
/// reading is before `since_ms` (and `since_ms` before `open_ms`), the stop before it draws up to
/// `since_ms` as the restore draws a stop, and `kept` gains an unclean checkpoint there
/// ([`date_unseen_kill`], [`date_crash`]).
fn date_unseen_stop(kept: &mut SavedStrip, since_ms: i64, open_ms: i64) {
    if kept.last_wall_ms >= since_ms || since_ms >= open_ms {
        return;
    }
    if kept.last_wall_ms > 0 {
        let aged = kept.last_wall_ms < open_ms.saturating_sub(STRIP_HORIZON_MS);
        let stop = if kept.clean_stop || aged {
            (since_ms - kept.last_wall_ms >= BUCKET_MS).then_some(GapKind::Paused)
        } else {
            Some(GapKind::Crash)
        };
        kept.time_gaps.extend(stop.map(|kind| TimeGap {
            start_ms: kept.last_wall_ms,
            end_ms: since_ms,
            kind,
        }));
    }
    kept.last_wall_ms = since_ms;
    kept.clean_stop = false;
}

#[cfg(test)]
#[path = "vitals_startup_tests.rs"]
mod tests;
