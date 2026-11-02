// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the readings series ([`super::note`], [`super::note_plan`], [`super::Tail`]).

use super::*;
use crate::harness::accounts::{State, TapRead, WindowRead};

const NOW: i64 = 1_793_068_200;
const MACHINE: &str = "3f9a1b2c4d5e6f70";

fn tmp(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-series-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn reading(five: u32, week: u32, at: i64) -> TapRead {
    TapRead {
        five_hour: Some(WindowRead {
            pct_x10: five,
            reset: NOW + 9000,
            refilled: None,
        }),
        seven_day: Some(WindowRead {
            pct_x10: week,
            reset: NOW + 311_400,
            refilled: None,
        }),
        at,
        seen_before: true,
        born: None,
        key: None,
    }
}

/// The tap's recorded-reading branch as `harness_host::tap_step` runs it: the store's
/// transaction, then the series' row from the figures it holds after.
fn tap(dir: &Path, state: &mut State, read: &TapRead, now: i64) -> Noted {
    let (events, held_back) = accounts::note_reading_held(state, "me", read, now);
    let acct = state.accounts.get("me").cloned().unwrap_or_default();
    note_recorded(
        dir,
        "me",
        &state.machine,
        (
            acct.windows.get(accounts::SESSION_LIMIT),
            acct.windows.get(accounts::WEEK_LIMIT),
        ),
        recorded(&events, held_back),
        now,
    )
    .expect("series write")
}

fn kinds(dir: &Path) -> Vec<&'static str> {
    read(dir, "me", MACHINE)
        .iter()
        .map(|row| match row.said {
            Said::Fig { .. } => "fig",
            Said::Beat => "beat",
            Said::Plan(_) => "plan",
        })
        .collect()
}

/// T1: A RECORDED READING APPENDS ONE ROW, AND ONLY A RECORDED ONE. The first reading appends a
/// `fig`; the same figures inside the store's heartbeat record nothing and append nothing; the
/// heartbeat's re-recording of equal figures appends a `beat`; a moved figure a `fig`; a reading
/// held back while the home adopts another login appends nothing. Negative control: dropping
/// the [`recorded`] gate appends a row for every one of them.
#[test]
fn series_appends_only_recorded_readings() {
    let dir = tmp("t1");
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    assert_eq!(
        tap(dir.path(), &mut state, &reading(380, 580, NOW), NOW),
        Noted::Fig
    );
    assert_eq!(kinds(dir.path()), ["fig"]);
    let later = NOW + 60;
    assert_eq!(
        tap(dir.path(), &mut state, &reading(380, 580, later), later),
        Noted::Nothing,
        "the same figures inside the heartbeat are not recorded"
    );
    assert_eq!(kinds(dir.path()), ["fig"]);
    let beat = NOW + accounts::READING_HEARTBEAT_S;
    assert_eq!(
        tap(dir.path(), &mut state, &reading(380, 580, beat), beat),
        Noted::Beat
    );
    let moved = beat + 60;
    assert_eq!(
        tap(dir.path(), &mut state, &reading(400, 580, moved), moved),
        Noted::Fig
    );
    assert_eq!(kinds(dir.path()), ["fig", "beat", "fig"]);
    let rows = read(dir.path(), "me", MACHINE);
    let last = rows.last().expect("a row");
    assert_eq!(
        last.fig(accounts::SESSION_LIMIT).map(|f| f.pct_x10),
        Some(400)
    );
    assert_eq!(last.fig(accounts::WEEK_LIMIT).map(|f| f.pct_x10), Some(580));
    assert_eq!(last.by, MACHINE);
    // Held back: another login signed in to the home a moment ago.
    if let Some(acct) = state.accounts.get_mut("me") {
        acct.keyed_at = Some(moved + 100);
    }
    let held = moved + 110;
    let (events, held_back) =
        accounts::note_reading_held(&mut state.clone(), "me", &reading(450, 600, held), held);
    assert!(held_back && !recorded(&events, held_back));
    assert_eq!(
        tap(dir.path(), &mut state, &reading(450, 600, held), held),
        Noted::Nothing
    );
    assert_eq!(kinds(dir.path()), ["fig", "beat", "fig"]);
    for line in std::fs::read_to_string(path_of(dir.path(), "me", MACHINE).unwrap())
        .unwrap()
        .lines()
    {
        assert!(line.len() <= ROW_MAX);
        assert!(!line.contains('@'), "a row names no email: {line}");
    }
}

/// THE SERIES IS PRIVATE: its directory 0700, its files 0600, its lock its own.
#[cfg(unix)]
#[test]
fn series_files_are_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tmp("mode");
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    tap(dir.path(), &mut state, &reading(380, 580, NOW), NOW);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir_in(dir.path())), 0o700);
    assert_eq!(mode(&path_of(dir.path(), "me", MACHINE).unwrap()), 0o600);
    assert_eq!(mode(&dir_in(dir.path()).join(LOCK_FILE)), 0o600);
    assert!(!dir_in(dir.path()).join("accounts.lock").exists());
}

fn fill(path: &Path, rows: usize, span: i64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = String::new();
    let plan = SeriesRow {
        t: NOW - span - 3600,
        by: MACHINE.into(),
        said: Said::Plan(PlanSaid {
            word: "max".into(),
            tier: Some("20x".into()),
            src: "profile".into(),
        }),
    };
    text.push_str(&plan.line().unwrap());
    text.push('\n');
    for i in 0..rows {
        let t = NOW - span + span * i as i64 / rows as i64;
        let row = SeriesRow {
            t,
            by: MACHINE.into(),
            said: Said::Fig {
                five_hour: None,
                seven_day: Some(Fig {
                    pct_x10: u32::try_from(i % 1000).unwrap(),
                    reset: NOW + 86_400,
                    at: t,
                    src: "tap".into(),
                    epoch: None,
                }),
            },
        };
        text.push_str(&row.line().unwrap());
        text.push('\n');
    }
    std::fs::write(path, text).unwrap();
}

/// T2: A SERIES PAST ITS BOUND IS TRIMMED TO ITS NEWEST ROWS WITHIN EIGHT DAYS, its newest `plan`
/// row kept however old (the one row the bound does not hold), and an append that waited on
/// the lock while the trim ran lands in the trimmed file: an append takes the lock, so one that
/// may not wait while it is held writes nothing. Negative controls: with the trim off the file
/// keeps every row past the bound; an append that skips the lock writes under a held one.
#[test]
fn series_trim_keeps_eight_days() {
    let dir = tmp("t2");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    fill(&path, TRIM_ROWS + 200, 10 * 86_400);
    let held = lock_within(&dir_in(dir.path()), LOCK_WAIT)
        .unwrap()
        .expect("the lock");
    let before = std::fs::read(&path).unwrap();
    let barred = Fig {
        pct_x10: 555,
        reset: NOW + 86_400,
        at: NOW,
        src: "tap".into(),
        epoch: None,
    };
    assert_eq!(
        note_within(
            dir.path(),
            "me",
            MACHINE,
            (None, Some(barred)),
            NOW,
            Duration::ZERO
        )
        .unwrap(),
        Noted::Dropped,
        "an append under a held lock waits for it"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let waiting = {
        let state = dir.path().to_path_buf();
        std::thread::spawn(move || {
            let fig = Fig {
                pct_x10: 777,
                reset: NOW + 86_400,
                at: NOW + 1,
                src: "tap".into(),
                epoch: None,
            };
            // A one-minute hang detector, not a latency budget.
            note_within(
                &state,
                "me",
                MACHINE,
                (None, Some(fig)),
                NOW + 1,
                Duration::from_secs(60),
            )
        })
    };
    trim_locked(&path, NOW, false).unwrap();
    drop(held);
    assert_eq!(waiting.join().unwrap().unwrap(), Noted::Fig);
    let rows = read(dir.path(), "me", MACHINE);
    assert!(
        rows.len() <= KEEP_ROWS + 2,
        "trimmed to the newest {KEEP_ROWS}: {}",
        rows.len()
    );
    assert!(
        matches!(rows[0].said, Said::Plan(_)),
        "the newest plan row stays"
    );
    assert!(
        rows[1..].iter().all(|row| NOW + 1 - row.t <= KEEP_S),
        "every kept figure is within eight days"
    );
    let last = rows.last().unwrap();
    assert_eq!(
        last.fig(accounts::WEEK_LIMIT).map(|f| f.pct_x10),
        Some(777),
        "the append that waited on the trim's lock survives it"
    );
    assert!(std::fs::metadata(&path).unwrap().len() <= TRIM_BYTES);
}

/// AN APPEND PAST THE BOUND TRIMS BY ITSELF.
#[test]
fn an_append_past_the_bound_trims() {
    let dir = tmp("t2b");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    fill(&path, TRIM_ROWS + 10, 3 * 86_400);
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    tap(dir.path(), &mut state, &reading(380, 999, NOW), NOW);
    assert!(read(dir.path(), "me", MACHINE).len() <= KEEP_ROWS + 2);
}

/// A PLAN IS WRITTEN ONCE PER CHANGE.
#[test]
fn a_plan_row_is_written_once_per_change() {
    let dir = tmp("plan");
    let plan = |tier: &str| PlanSaid {
        word: "max".into(),
        tier: Some(tier.into()),
        src: "profile".into(),
    };
    let note = |p: &PlanSaid| note_plan(dir.path(), "alt2", MACHINE, p, NOW).unwrap();
    assert_eq!(note(&plan("5x")), Noted::Plan);
    assert_eq!(note(&plan("5x")), Noted::SamePlan);
    assert_eq!(note(&plan("20x")), Noted::Plan);
    let rows = read(dir.path(), "alt2", MACHINE);
    assert_eq!(
        newest_plan(&rows).and_then(|p| p.tier.as_deref()),
        Some("20x")
    );
    assert_eq!(
        note_plan(dir.path(), "../x", MACHINE, &plan("5x"), NOW).unwrap(),
        Noted::Nothing,
        "no file is named after an inadmissible label"
    );
}

/// THE TAIL READS ONLY WHAT WAS APPENDED, AND THE WHOLE FILE AFTER A TRIM'S RENAME.
#[test]
fn the_tail_reads_appended_bytes_and_rereads_after_a_rename() {
    let dir = tmp("tail");
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let mut tail = Tail::new(path.clone());
    assert!(!tail.refresh());
    tap(dir.path(), &mut state, &reading(380, 580, NOW), NOW);
    assert!(tail.refresh());
    assert_eq!(tail.rows().len(), 1);
    assert!(!tail.refresh(), "an unchanged file reads nothing");
    let later = NOW + 600;
    tap(dir.path(), &mut state, &reading(420, 590, later), later);
    assert!(tail.refresh());
    assert_eq!(tail.rows().len(), 2);
    fill(&path, 3, 3600);
    let renamed = path.with_extension("new");
    std::fs::copy(&path, &renamed).unwrap();
    std::fs::rename(&renamed, &path).unwrap();
    assert!(tail.refresh());
    assert_eq!(
        tail.rows().len(),
        4,
        "the plan row and three figures, read afresh"
    );
}
