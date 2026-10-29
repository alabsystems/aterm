// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use aterm_vitals::{
    GapKind, MemSample, MultiScaleSpec, Pressure, Resource, Sample, SavedStrip, Snapshot,
    StripEvent, StripEventKind, Ticks, TimeGap, Vitals,
};

use super::*;

/// The killed run's first reading.
const T0: i64 = 1_792_000_800_000;
/// The killed run's launch, when it made its kill marker: a little before its first reading.
const KILLED_LAUNCH_MS: i64 = T0 - 2_000;
/// The killed run's last checkpoint, an unclean one.
const LAST_CHECKPOINT_MS: i64 = T0 + 600_000;
/// The next launch.
const LAUNCH_MS: i64 = T0 + 640_000;
/// When that launch's probe thread restores the strip and takes its first reading.
const OPEN_MS: i64 = LAUNCH_MS + 150;

/// `path` with `body` in it, last written at `at_ms`.
fn written_at(path: PathBuf, body: &[u8], at_ms: i64) -> PathBuf {
    std::fs::write(&path, body).expect("scratch file");
    let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(at_ms).expect("after the epoch"));
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(at))
        .expect("its modification time");
    path
}

/// A kill's consumed marker, made empty at the killed run's launch, a little before its first
/// reading.
fn kill_evidence(dir: &Path) -> KillEvidence {
    KillEvidence {
        marker: written_at(
            dir.join("crash-marker-4242-1792000798000000000-app.log.seen"),
            b"",
            KILLED_LAUNCH_MS,
        ),
        log: dir.join("aterm.log"),
    }
}

/// A panic report its hook wrote at `at_ms`.
fn crash_evidence(dir: &Path, at_ms: i64) -> CrashEvidence {
    CrashEvidence {
        path: written_at(
            dir.join("crash-4242.log.seen"),
            b"panicked at src/lib.rs\n",
            at_ms,
        ),
        head: vec!["panicked at src/lib.rs".into()],
    }
}

fn restarted() -> StripEvent {
    StripEvent {
        at_unix_ms: LAUNCH_MS,
        kind: StripEventKind::Restart,
        label: "restarted".into(),
    }
}

#[test]
fn a_launch_notes_the_restart_and_the_stops_time_for_the_strips_restore() {
    assert_eq!(
        startup_marks(PriorStop::Killed(Some(KILLED_LAUNCH_MS)), LAUNCH_MS),
        StartupMarks {
            events: vec![restarted()],
            crashed_at: None,
            killed_since: Some(KILLED_LAUNCH_MS),
        },
        "a kill's crash is the restore's, from the last checkpoint or the killed run's launch"
    );
    assert_eq!(
        startup_marks(PriorStop::Killed(None), LAUNCH_MS),
        StartupMarks {
            events: vec![restarted()],
            crashed_at: None,
            killed_since: None,
        }
    );
    let crash_ms = LAST_CHECKPOINT_MS + 4_200;
    assert_eq!(
        startup_marks(PriorStop::Crashed(Some(crash_ms)), LAUNCH_MS),
        StartupMarks {
            events: vec![restarted()],
            crashed_at: Some(crash_ms),
            killed_since: None,
        },
        "a crash's gap and event are the restore's, dated by the crash file against the strip"
    );
    assert_eq!(
        startup_marks(PriorStop::Crashed(None), LAUNCH_MS).crashed_at,
        Some(LAUNCH_MS - 1_000),
        "a crash file whose time did not read dates the crash a second before the launch"
    );
    assert_eq!(
        startup_marks(PriorStop::Crashed(Some(LAUNCH_MS + 5_000)), LAUNCH_MS).crashed_at,
        Some(LAUNCH_MS),
        "a crash file's time past the launch is the launch"
    );
    assert_eq!(
        startup_marks(
            PriorStop::CrashedAndKilled(Some(crash_ms), Some(KILLED_LAUNCH_MS)),
            LAUNCH_MS
        ),
        StartupMarks {
            events: vec![restarted()],
            crashed_at: Some(crash_ms),
            killed_since: Some(KILLED_LAUNCH_MS),
        },
        "a crash file beside a kill's marker notes both times"
    );
}

#[test]
fn a_kills_marker_never_dates_the_stop() {
    let dir = aterm_tempfile::tempdir().expect("scratch log dir");
    let killed = kill_evidence(dir.path());
    let crash_ms = LAST_CHECKPOINT_MS + 4_200;
    let crash = crash_evidence(dir.path(), crash_ms);
    assert_eq!(
        PriorStop::of(None, Some(&killed)),
        Some(PriorStop::Killed(Some(KILLED_LAUNCH_MS))),
        "a kill's marker dates the killed run's launch"
    );
    assert_eq!(
        PriorStop::of(Some(&crash), None),
        Some(PriorStop::Crashed(Some(crash_ms)))
    );
    assert_eq!(
        PriorStop::of(Some(&crash), Some(&killed)),
        Some(PriorStop::CrashedAndKilled(
            Some(crash_ms),
            Some(KILLED_LAUNCH_MS)
        )),
        "a crash file beside a kill's marker keeps both, for the strip to choose between"
    );
    assert_eq!(PriorStop::of(None, None), None);
    assert!(PriorStop::Killed(None).killed() && PriorStop::CrashedAndKilled(None, None).killed());
    assert!(!PriorStop::Crashed(Some(crash_ms)).killed());
    let gone = CrashEvidence {
        path: dir.path().join("crash-4243.log.seen"),
        head: Vec::new(),
    };
    assert_eq!(
        PriorStop::of(Some(&gone), Some(&killed)),
        Some(PriorStop::CrashedAndKilled(None, Some(KILLED_LAUNCH_MS)))
    );
    let unread = KillEvidence {
        marker: dir
            .path()
            .join("crash-marker-4243-1792000798000000000-app.log.seen"),
        log: dir.path().join("aterm.log"),
    };
    assert_eq!(
        PriorStop::of(None, Some(&unread)),
        Some(PriorStop::Killed(None))
    );
}

/// One reading of the killed run.
fn reading(wall_ms: i64, mono_ms: u64, busy: u64, total: u64) -> Sample {
    Sample {
        at_ns: mono_ms * 1_000_000,
        at_unix_ms: wall_ms,
        mono_ms,
        cadence_ms: 10_000,
        cores: Some(vec![Ticks { busy, total }; 2]),
        mem: Some(MemSample {
            total: 64 << 30,
            app: 24 << 30,
            wired: 8 << 30,
            free: 32 << 30,
            pressure: Pressure::Normal,
            ..MemSample::default()
        }),
        ..Sample::default()
    }
}

/// The strip a killed run left, as in `aterm-vitals`' SIGKILL restore
/// (`saved_strip_preserves_multi_scale_history_across_restarts_and_crashes`): ten minutes at 60 %
/// CPU from [`T0`], every 10 s, with the Mac asleep from +200 s to +300 s (the wall clock moved
/// 100 s and the monotonic one 10 s), and its last checkpoint at +600 s an unclean one.
fn killed_runs_strip() -> SavedStrip {
    let mut killed = Vitals::new();
    let (mut busy, mut total) = (0, 0);
    for step in (0..=60u64).filter(|step| !(21..30).contains(step)) {
        let t_ms = step * 10_000;
        let mono_ms = if step < 30 { t_ms } else { t_ms - 90_000 };
        busy += 600;
        total += 1_000;
        let _ = killed.absorb(&reading(
            T0 + i64::try_from(t_ms).expect("ten minutes"),
            mono_ms,
            busy,
            total,
        ));
    }
    let saved = killed.saved_strip(false);
    assert_eq!(saved.last_wall_ms, LAST_CHECKPOINT_MS);
    saved
}

/// The deck's first reading after the launch, composed as the app composes it: the probe thread
/// opens the strip as it starts, dating a kill before the killed run's first checkpoint from that
/// run's launch and a crash the strip holds no checkpoint since from its crash file, the two in the
/// order they came (`vitals_host::open_strip`, for a strip file last written at its last reading,
/// with no handoff and no live peer), and restores it (`vitals_host::run`), and the first reading's
/// request carries the events the launch noted (`App::ask_reading`) into the sample the model
/// absorbs.
fn first_reading_after(mut saved: SavedStrip, marks: StartupMarks) -> Snapshot {
    let crash_ms = marks
        .crashed_at
        .filter(|&ms| saved.last_wall_ms < ms && ms < OPEN_MS);
    if let Some(crash_ms) = crash_ms.filter(|&ms| {
        marks
            .killed_since
            .is_some_and(|launched_ms| ms < launched_ms)
    }) {
        date_crash(&mut saved, crash_ms, OPEN_MS);
    }
    if let Some(launched_ms) = marks.killed_since {
        date_unseen_kill(&mut saved, launched_ms, OPEN_MS);
    }
    if let Some(crash_ms) = crash_ms {
        date_crash(&mut saved, crash_ms, OPEN_MS);
    }
    let mut after = Vitals::new();
    after.restore_strip_history(saved, OPEN_MS);
    let mut first = reading(OPEN_MS, 1_000, 600, 1_000);
    first.cadence_ms = 1_000;
    first.note_events = marks.events;
    after.absorb(&first)
}

/// Each CPU strip box: whether it draws a reading, and the gap it draws.
fn cpu_boxes(snap: &Snapshot) -> Vec<(bool, Option<GapKind>)> {
    snap.resources[Resource::Cpu.index()]
        .heat_multi_scale(
            MultiScaleSpec::FULL_6H,
            snap.at_unix_ms,
            2_500,
            &snap.time_gaps,
        )
        .iter()
        .map(|slot| (slot.cell.heat.is_some(), slot.cell.gap))
        .collect()
}

fn of_kind(snap: &Snapshot, kind: GapKind) -> Vec<TimeGap> {
    snap.time_gaps
        .iter()
        .copied()
        .filter(|gap| gap.kind == kind)
        .collect()
}

fn events_of(snap: &Snapshot, kind: StripEventKind) -> Vec<i64> {
    snap.strip_events
        .iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.at_unix_ms)
        .collect()
}

#[test]
fn after_a_kill_the_strip_keeps_the_killed_runs_sleep_and_readings_and_dates_its_crash_at_its_last_checkpoint()
 {
    let dir = aterm_tempfile::tempdir().expect("scratch log dir");
    let prior = PriorStop::of(None, Some(&kill_evidence(dir.path()))).expect("a kill");
    let restored = first_reading_after(killed_runs_strip(), StartupMarks::default());
    let snap = first_reading_after(killed_runs_strip(), startup_marks(prior, LAUNCH_MS));
    assert_eq!(
        of_kind(&snap, GapKind::Sleep),
        vec![TimeGap {
            start_ms: T0 + 200_000,
            end_ms: T0 + 300_000,
            kind: GapKind::Sleep,
        }],
        "the killed run's sleep survives: {:?}",
        snap.time_gaps
    );
    assert_eq!(
        of_kind(&snap, GapKind::Crash),
        vec![TimeGap {
            start_ms: LAST_CHECKPOINT_MS,
            end_ms: OPEN_MS,
            kind: GapKind::Crash,
        }],
        "the crash runs from the last checkpoint to the restore"
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![LAST_CHECKPOINT_MS]
    );
    assert_eq!(events_of(&snap, StripEventKind::Restart).len(), 1);
    let boxes = cpu_boxes(&snap);
    assert!(
        boxes.iter().filter(|(read, _)| *read).count() > 1,
        "the readings before the kill draw: {boxes:?}"
    );
    assert_eq!(
        boxes,
        cpu_boxes(&restored),
        "the launch's marks for a kill take nothing the restore kept"
    );
}

#[test]
fn after_a_crash_the_strip_dates_it_by_the_crash_file_and_keeps_the_runs_history() {
    let dir = aterm_tempfile::tempdir().expect("scratch log dir");
    let crash_ms = LAST_CHECKPOINT_MS + 4_200;
    let prior = PriorStop::of(Some(&crash_evidence(dir.path(), crash_ms)), None).expect("a crash");
    let restored = first_reading_after(killed_runs_strip(), StartupMarks::default());
    let snap = first_reading_after(killed_runs_strip(), startup_marks(prior, LAUNCH_MS));
    assert_eq!(
        of_kind(&snap, GapKind::Sleep).len(),
        1,
        "{:?}",
        snap.time_gaps
    );
    assert_eq!(
        of_kind(&snap, GapKind::Crash),
        vec![TimeGap {
            start_ms: LAST_CHECKPOINT_MS,
            end_ms: OPEN_MS,
            kind: GapKind::Crash,
        }],
        "the crash runs from the last checkpoint, as the restore alone draws it"
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![crash_ms],
        "the crash event moves to the crash file's time"
    );
    assert_eq!(events_of(&snap, StripEventKind::Restart).len(), 1);
    assert_eq!(cpu_boxes(&snap), cpu_boxes(&restored));
}

#[test]
fn a_crash_file_the_strip_was_written_after_dates_nothing() {
    let survived_ms = T0 + 100_000;
    let restored = first_reading_after(killed_runs_strip(), StartupMarks::default());
    let snap = first_reading_after(
        killed_runs_strip(),
        startup_marks(PriorStop::Crashed(Some(survived_ms)), LAUNCH_MS),
    );
    assert_eq!(
        of_kind(&snap, GapKind::Crash),
        vec![TimeGap {
            start_ms: LAST_CHECKPOINT_MS,
            end_ms: OPEN_MS,
            kind: GapKind::Crash,
        }],
        "the run went on reading past the panic it survived, so its unclean stop dates the crash \
         from its last checkpoint: {:?}",
        snap.time_gaps
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![LAST_CHECKPOINT_MS],
        "the crash event is the restore's, not the crash file's"
    );
    assert_eq!(cpu_boxes(&snap), cpu_boxes(&restored));
    let quit = first_reading_after(earlier_clean_strip(), StartupMarks::default());
    let snap = first_reading_after(
        earlier_clean_strip(),
        startup_marks(
            PriorStop::Crashed(Some(EARLIER_LAST_MS - 300_000)),
            LAUNCH_MS,
        ),
    );
    assert_eq!(
        snap.time_gaps,
        vec![TimeGap {
            start_ms: EARLIER_LAST_MS,
            end_ms: OPEN_MS,
            kind: GapKind::Paused,
        }],
        "a panic the run survived before its clean quit draws the quit's pause and no crash"
    );
    assert!(events_of(&snap, StripEventKind::Crash).is_empty());
    assert_eq!(events_of(&snap, StripEventKind::Restart).len(), 1);
    assert_eq!(cpu_boxes(&snap), cpu_boxes(&quit));
    for crash_ms in [EARLIER_LAST_MS - 300_000, EARLIER_LAST_MS] {
        let mut kept = earlier_clean_strip();
        date_crash(&mut kept, crash_ms, OPEN_MS);
        assert_eq!(
            kept,
            earlier_clean_strip(),
            "a checkpoint at or after the crash file at {crash_ms} is left as it is"
        );
    }
    for crash_ms in [OPEN_MS, OPEN_MS + 5_000] {
        let mut kept = SavedStrip::default();
        date_crash(&mut kept, crash_ms, OPEN_MS);
        assert_eq!(
            kept,
            SavedStrip::default(),
            "a crash at or after the open (the clock moved back) dates nothing"
        );
    }
}

#[test]
fn a_kill_beside_a_crash_file_dates_each_stop_in_the_order_they_came() {
    let stale =
        PriorStop::CrashedAndKilled(Some(EARLIER_LAST_MS - 300_000), Some(KILLED_LAUNCH_MS));
    let snap = first_reading_after(earlier_clean_strip(), startup_marks(stale, LAUNCH_MS));
    assert_eq!(
        snap.time_gaps,
        vec![
            TimeGap {
                start_ms: EARLIER_LAST_MS,
                end_ms: KILLED_LAUNCH_MS,
                kind: GapKind::Paused,
            },
            TimeGap {
                start_ms: KILLED_LAUNCH_MS,
                end_ms: OPEN_MS,
                kind: GapKind::Crash,
            },
        ],
        "the crash file is older than the strip, so the kill before the killed run's first \
         checkpoint dates the crash from that run's launch"
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![KILLED_LAUNCH_MS]
    );
    let crash_ms = T0 + 3_000;
    let newer = PriorStop::CrashedAndKilled(Some(crash_ms), Some(KILLED_LAUNCH_MS));
    let snap = first_reading_after(earlier_clean_strip(), startup_marks(newer, LAUNCH_MS));
    assert_eq!(
        snap.time_gaps,
        vec![
            TimeGap {
                start_ms: EARLIER_LAST_MS,
                end_ms: KILLED_LAUNCH_MS,
                kind: GapKind::Paused,
            },
            TimeGap {
                start_ms: KILLED_LAUNCH_MS,
                end_ms: OPEN_MS,
                kind: GapKind::Crash,
            },
        ],
        "the killed run ran from its launch, so the crash file after it moves only the event"
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![crash_ms],
        "a crash file newer than the strip and the launch dates the crash's event"
    );
    let crash_ms = EARLIER_LAST_MS + 60_000;
    let between = PriorStop::CrashedAndKilled(Some(crash_ms), Some(KILLED_LAUNCH_MS));
    let snap = first_reading_after(earlier_clean_strip(), startup_marks(between, LAUNCH_MS));
    assert_eq!(
        snap.time_gaps,
        vec![
            TimeGap {
                start_ms: EARLIER_LAST_MS,
                end_ms: crash_ms,
                kind: GapKind::Paused,
            },
            TimeGap {
                start_ms: crash_ms,
                end_ms: OPEN_MS,
                kind: GapKind::Crash,
            },
        ],
        "a crash file older than the killed run's launch is an earlier run's crash, dated before \
         the kill: aterm was down from it to the launch, and the killed run never checkpointed"
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![KILLED_LAUNCH_MS],
        "one crash gap keeps its later event"
    );
}

#[test]
fn a_crash_before_the_crashed_runs_first_checkpoint_draws_the_earlier_runs_pause_to_it_and_the_crash_from_there()
 {
    let crash_ms = T0 + 3_000;
    let restored = first_reading_after(earlier_clean_strip(), StartupMarks::default());
    let snap = first_reading_after(
        earlier_clean_strip(),
        startup_marks(PriorStop::Crashed(Some(crash_ms)), LAUNCH_MS),
    );
    assert_eq!(
        snap.time_gaps,
        vec![
            TimeGap {
                start_ms: EARLIER_LAST_MS,
                end_ms: crash_ms,
                kind: GapKind::Paused,
            },
            TimeGap {
                start_ms: crash_ms,
                end_ms: OPEN_MS,
                kind: GapKind::Crash,
            },
        ],
        "the earlier run's clean stop stays a pause up to the crash, where the crash starts"
    );
    assert_eq!(events_of(&snap, StripEventKind::Crash), vec![crash_ms]);
    assert_eq!(events_of(&snap, StripEventKind::Restart).len(), 1);
    let read = |boxes: &[(bool, Option<GapKind>)]| -> Vec<bool> {
        boxes.iter().map(|(read, _)| *read).collect()
    };
    assert_eq!(
        read(&cpu_boxes(&snap)),
        read(&cpu_boxes(&restored)),
        "the crash takes none of the earlier run's readings"
    );
    let mut none = SavedStrip::default();
    date_crash(&mut none, crash_ms, OPEN_MS);
    assert_eq!(
        (none.last_wall_ms, none.clean_stop, none.time_gaps),
        (crash_ms, false, Vec::new()),
        "with no strip the crash is the whole stop"
    );
}

/// The last reading of an earlier run that quit cleanly before the killed run launched.
const EARLIER_LAST_MS: i64 = T0 - 1_200_000;

/// The strip an earlier run left at a clean quit: ten minutes at 60 % CPU, every 10 s, ending at
/// [`EARLIER_LAST_MS`], twenty minutes before the killed run launched and never checkpointed.
fn earlier_clean_strip() -> SavedStrip {
    let mut earlier = Vitals::new();
    let (mut busy, mut total) = (0, 0);
    for step in 0..=60u64 {
        busy += 600;
        total += 1_000;
        let _ = earlier.absorb(&reading(
            EARLIER_LAST_MS - 600_000 + i64::try_from(step * 10_000).expect("ten minutes"),
            step * 10_000,
            busy,
            total,
        ));
    }
    let saved = earlier.saved_strip(true);
    assert_eq!(saved.last_wall_ms, EARLIER_LAST_MS);
    saved
}

#[test]
fn a_strip_without_the_killed_runs_checkpoint_gains_an_unclean_one_at_its_launch() {
    let mut none = SavedStrip::default();
    date_unseen_kill(&mut none, KILLED_LAUNCH_MS, OPEN_MS);
    assert_eq!(
        (none.last_wall_ms, none.clean_stop, none.time_gaps),
        (KILLED_LAUNCH_MS, false, Vec::new())
    );
    let mut clean = earlier_clean_strip();
    date_unseen_kill(&mut clean, KILLED_LAUNCH_MS, OPEN_MS);
    assert_eq!(
        (clean.last_wall_ms, clean.clean_stop),
        (KILLED_LAUNCH_MS, false)
    );
    assert_eq!(
        clean.time_gaps,
        vec![TimeGap {
            start_ms: EARLIER_LAST_MS,
            end_ms: KILLED_LAUNCH_MS,
            kind: GapKind::Paused,
        }],
        "an earlier run's clean quit is a pause up to the killed run's launch"
    );
    let mut unclean = SavedStrip {
        clean_stop: false,
        ..earlier_clean_strip()
    };
    date_unseen_kill(&mut unclean, KILLED_LAUNCH_MS, OPEN_MS);
    assert_eq!(
        unclean.time_gaps,
        vec![TimeGap {
            start_ms: EARLIER_LAST_MS,
            end_ms: KILLED_LAUNCH_MS,
            kind: GapKind::Crash,
        }],
        "an earlier run's unclean stop is its crash, up to the killed run's launch"
    );
    let mut close = earlier_clean_strip();
    date_unseen_kill(&mut close, EARLIER_LAST_MS + 9_999, OPEN_MS);
    assert_eq!(
        (close.last_wall_ms, close.time_gaps),
        (EARLIER_LAST_MS + 9_999, Vec::new()),
        "a clean quit under a 10 s bucket before the launch is no pause, as in the restore"
    );
}

/// The last reading of an earlier run that stopped a millisecond further back than the strips'
/// six hours before the next launch's open.
const AGED_LAST_MS: i64 = OPEN_MS - STRIP_HORIZON_MS - 1;

#[test]
fn a_kill_after_a_stop_older_than_the_six_hours_draws_its_pause_to_the_killed_runs_launch_and_the_crash_from_there()
 {
    let paused_then_crash = vec![
        TimeGap {
            start_ms: AGED_LAST_MS,
            end_ms: KILLED_LAUNCH_MS,
            kind: GapKind::Paused,
        },
        TimeGap {
            start_ms: KILLED_LAUNCH_MS,
            end_ms: OPEN_MS,
            kind: GapKind::Crash,
        },
    ];
    for clean_stop in [false, true] {
        let old = SavedStrip {
            last_wall_ms: AGED_LAST_MS,
            clean_stop,
            ..SavedStrip::default()
        };
        let relaunched = first_reading_after(old.clone(), StartupMarks::default());
        assert_eq!(
            relaunched.time_gaps,
            vec![TimeGap {
                start_ms: AGED_LAST_MS,
                end_ms: OPEN_MS,
                kind: GapKind::Paused,
            }],
            "a launch with no kill draws the time since that stop as the first reading's pause"
        );
        let snap = first_reading_after(
            old.clone(),
            startup_marks(PriorStop::Killed(Some(KILLED_LAUNCH_MS)), LAUNCH_MS),
        );
        assert_eq!(
            snap.time_gaps, paused_then_crash,
            "the restore keeps the pause, its start outside the six hours and unclipped, up to the \
             killed run's launch, where the crash starts (clean_stop {clean_stop})"
        );
        assert_eq!(
            events_of(&snap, StripEventKind::Crash),
            vec![KILLED_LAUNCH_MS]
        );
        assert_eq!(events_of(&snap, StripEventKind::Restart).len(), 1);
        for drawn in [&snap, &relaunched] {
            let boxes = cpu_boxes(drawn);
            assert!(
                boxes.iter().all(|(read, gap)| *read || gap.is_some()),
                "no box draws as not read: {boxes:?}"
            );
        }
        let boxes = cpu_boxes(&snap);
        assert!(
            boxes.iter().any(|(_, gap)| *gap == Some(GapKind::Paused))
                && boxes.iter().any(|(_, gap)| *gap == Some(GapKind::Crash)),
            "the strip draws the pause and the crash: {boxes:?}"
        );
        let mut aged = old;
        date_unseen_kill(&mut aged, KILLED_LAUNCH_MS, OPEN_MS);
        assert_eq!(
            (aged.last_wall_ms, aged.clean_stop, aged.time_gaps),
            (KILLED_LAUNCH_MS, false, paused_then_crash[..1].to_vec()),
            "an earlier stop that old, clean_stop {clean_stop}, is a pause up to the killed run's launch"
        );
    }
}

#[test]
fn a_kill_after_an_earlier_runs_clean_quit_draws_its_pause_to_the_killed_runs_launch_and_the_crash_from_there()
 {
    let dir = aterm_tempfile::tempdir().expect("scratch log dir");
    let prior = PriorStop::of(None, Some(&kill_evidence(dir.path()))).expect("a kill");
    let restored = first_reading_after(earlier_clean_strip(), StartupMarks::default());
    assert_eq!(
        restored.time_gaps,
        vec![TimeGap {
            start_ms: EARLIER_LAST_MS,
            end_ms: OPEN_MS,
            kind: GapKind::Paused,
        }],
        "the restore alone draws the killed run's time as the earlier run's pause"
    );
    let snap = first_reading_after(earlier_clean_strip(), startup_marks(prior, LAUNCH_MS));
    assert_eq!(
        snap.time_gaps,
        vec![
            TimeGap {
                start_ms: EARLIER_LAST_MS,
                end_ms: KILLED_LAUNCH_MS,
                kind: GapKind::Paused,
            },
            TimeGap {
                start_ms: KILLED_LAUNCH_MS,
                end_ms: OPEN_MS,
                kind: GapKind::Crash,
            },
        ],
        "the earlier run's pause ends at the killed run's launch, where the crash starts"
    );
    assert_eq!(
        events_of(&snap, StripEventKind::Crash),
        vec![KILLED_LAUNCH_MS]
    );
    assert_eq!(events_of(&snap, StripEventKind::Restart).len(), 1);
    let boxes = cpu_boxes(&snap);
    assert!(
        boxes.iter().any(|(_, gap)| *gap == Some(GapKind::Paused))
            && boxes.iter().any(|(_, gap)| *gap == Some(GapKind::Crash)),
        "the strip draws the pause and the crash: {boxes:?}"
    );
    let read = |boxes: &[(bool, Option<GapKind>)]| boxes.iter().map(|(read, _)| *read).collect();
    let drawn: Vec<bool> = read(&boxes);
    assert!(
        drawn.iter().filter(|read| **read).count() > 1,
        "the earlier run's readings draw: {boxes:?}"
    );
    assert_eq!(
        drawn,
        read(&cpu_boxes(&restored)),
        "the crash takes none of the earlier run's readings"
    );
}

#[test]
fn the_killed_runs_launch_never_dates_a_crash_where_a_checkpoint_of_it_is_kept() {
    let checkpointed = killed_runs_strip();
    for launched_ms in [KILLED_LAUNCH_MS, T0 + 300_000, LAST_CHECKPOINT_MS] {
        let mut kept = checkpointed.clone();
        date_unseen_kill(&mut kept, launched_ms, OPEN_MS);
        assert_eq!(
            kept, checkpointed,
            "a checkpoint at or after the launch at {launched_ms} is the killed run's"
        );
        let snap = first_reading_after(
            killed_runs_strip(),
            startup_marks(PriorStop::Killed(Some(launched_ms)), LAUNCH_MS),
        );
        assert_eq!(
            of_kind(&snap, GapKind::Crash),
            vec![TimeGap {
                start_ms: LAST_CHECKPOINT_MS,
                end_ms: OPEN_MS,
                kind: GapKind::Crash,
            }],
            "the last checkpoint dates the crash, not the launch at {launched_ms}"
        );
        assert_eq!(
            events_of(&snap, StripEventKind::Crash),
            vec![LAST_CHECKPOINT_MS]
        );
        assert_eq!(of_kind(&snap, GapKind::Sleep).len(), 1);
    }
    for launched_ms in [OPEN_MS, OPEN_MS + 5_000] {
        let mut kept = SavedStrip::default();
        date_unseen_kill(&mut kept, launched_ms, OPEN_MS);
        assert_eq!(
            kept,
            SavedStrip::default(),
            "a launch at or after the open (the clock moved back) dates nothing"
        );
    }
}
