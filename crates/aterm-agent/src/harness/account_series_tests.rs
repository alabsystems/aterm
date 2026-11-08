// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the readings series: its writers ([`super::note_committed`], [`super::note_plan`]),
//! its compaction ([`super::compact_locked`], [`super::compact_dir`]), its derived rows
//! ([`super::derive_closes`], [`super::day_summaries`], [`super::runs`]) and its readers
//! ([`super::Tail`], [`super::outlook_view`]).

use super::super::profile;
use super::*;
use crate::harness::accounts::{
    Account, Cli, Roster, Row, SESSION_LIMIT, State, TapRead, WEEK_LIMIT, WindowRead,
};

const NOW: i64 = 1_793_068_200;
const MACHINE: &str = "3f9a1b2c4d5e6f70";
const H: i64 = 3600;
const D: i64 = 86_400;
const AC: &str = "ac-0123456789abcdef";

fn utc(_: i64) -> i64 {
    0
}

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

/// The tap's branch as `harness_host::tap_step` runs it: the store's transaction, then the
/// series' rows from what it committed and the figures and keys the record holds after.
fn tap(dir: &Path, state: &mut State, read: &TapRead, now: i64) -> Noted {
    let (events, held_back) = accounts::note_reading_held(state, "me", read, now);
    let acct = state.accounts.get("me").cloned().unwrap_or_default();
    note_committed(
        dir,
        "me",
        &state.machine,
        &Committed {
            five_hour: acct.windows.get(SESSION_LIMIT),
            seven_day: acct.windows.get(WEEK_LIMIT),
            events: &events,
            held_back,
            ac: acct.pk.as_deref(),
            key: acct.key,
        },
        now,
    )
    .expect("series write")
}

fn kind(row: &SeriesRow) -> &'static str {
    match row.said {
        Said::Fig { .. } => "fig",
        Said::Beat => "beat",
        Said::Plan(_) => "plan",
        Said::Close(_) => "close",
        Said::Day(_) => "day",
        Said::Event(_) => "event",
    }
}

fn kinds(dir: &Path) -> Vec<&'static str> {
    read(dir, "me", MACHINE).iter().map(kind).collect()
}

fn fig_of(pct_x10: u32, reset: i64, at: i64) -> Fig {
    Fig {
        pct_x10,
        reset,
        at,
        src: "tap".into(),
        epoch: None,
    }
}

/// A SERIES FIGURE SAYS 100 ONLY FOR A READING OF 100: its whole-point movement uses the same
/// half-up-except-full boundary as the store and the glass. NEGATIVE CONTROL: plain half-up
/// (`(pct_x10 + 5) / 10`) makes 995 and 999 tenths 100 (run once, restored).
#[test]
fn a_series_whole_figure_says_100_only_for_a_reading_of_100() {
    for (x10, whole) in [(994, 99), (995, 99), (999, 99), (1000, 100)] {
        assert_eq!(fig_of(x10, NOW + D, NOW).whole(), whole, "{x10} tenths");
    }
}

fn fig_row(t: i64, five_hour: Option<Fig>, seven_day: Option<Fig>) -> SeriesRow {
    SeriesRow {
        t,
        by: MACHINE.into(),
        said: Said::Fig {
            five_hour,
            seven_day,
            ac: None,
            key: None,
        },
    }
}

fn row_of(t: i64, said: Said) -> SeriesRow {
    SeriesRow {
        t,
        by: MACHINE.into(),
        said,
    }
}

fn plan_row(t: i64, tier: &str) -> SeriesRow {
    row_of(
        t,
        Said::Plan(PlanSaid {
            word: "max".into(),
            tier: Some(tier.into()),
            src: "profile".into(),
        }),
    )
}

fn event_row(t: i64) -> SeriesRow {
    row_of(t, Said::Event(EventSaid::of(EventWord::AnotherAccount)))
}

/// Writes `lines` as the whole file at `path`.
fn write_lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(path, text).unwrap();
}

fn write_rows(path: &Path, rows: &[SeriesRow]) {
    let lines: Vec<String> = rows.iter().map(|r| r.line().expect("a row")).collect();
    write_lines(path, &lines);
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn rows_at(path: &Path) -> Vec<SeriesRow> {
    lines_of(path)
        .iter()
        .filter_map(|l| SeriesRow::parse(l))
        .collect()
}

fn closes(rows: &[SeriesRow], w: &str) -> Vec<(i64, Close)> {
    rows.iter()
        .filter_map(|r| match &r.said {
            Said::Close(c) if c.w == w => Some((r.t, c.clone())),
            _ => None,
        })
        .collect()
}

fn days(rows: &[SeriesRow]) -> Vec<(i64, Day)> {
    rows.iter()
        .filter_map(|r| match &r.said {
            Said::Day(d) => Some((r.t, d.clone())),
            _ => None,
        })
        .collect()
}

/// Stamps the series directory under `dir` as compacted at `at`, so an append runs no pass.
fn stamp(dir: &Path, at: i64) {
    let dir = dir_in(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(COMPACTED_FILE), at.to_string()).unwrap();
}

/// A RECORDED READING APPENDS ONE ROW, AND ONLY A RECORDED ONE. The first reading appends a
/// `fig`; the same figures inside the store's heartbeat record nothing and append nothing; the
/// heartbeat's re-recording of equal figures appends a `beat`; a moved figure a `fig`; a reading
/// held back while the home adopts another login appends nothing. Negative control: dropping
/// the [`Committed::recorded`] gate appends a `beat` for the reading inside the heartbeat.
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
    assert_eq!(last.fig(SESSION_LIMIT).map(|f| f.pct_x10), Some(400));
    assert_eq!(last.fig(WEEK_LIMIT).map(|f| f.pct_x10), Some(580));
    assert_eq!(last.by, MACHINE);
    // Held back: another login signed in to the home a moment ago.
    if let Some(acct) = state.accounts.get_mut("me") {
        acct.keyed_at = Some(moved + 100);
    }
    let held = moved + 110;
    let (events, held_back) =
        accounts::note_reading_held(&mut state.clone(), "me", &reading(450, 600, held), held);
    let committed = Committed {
        five_hour: None,
        seven_day: None,
        events: &events,
        held_back,
        ac: None,
        key: None,
    };
    assert!(held_back && !committed.recorded());
    assert_eq!(
        tap(dir.path(), &mut state, &reading(450, 600, held), held),
        Noted::Nothing
    );
    assert_eq!(kinds(dir.path()), ["fig", "beat", "fig"]);
    for line in lines_of(&path_of(dir.path(), "me", MACHINE).unwrap()) {
        assert!(line.len() <= ROW_MAX);
        assert!(!line.contains('@'), "a row names no email: {line}");
    }
}

/// ANOTHER ACCOUNT IS ONE EVENT ROW, EVEN WHEN ITS READING IS HELD BACK. A reading under another
/// key than the record holds commits an `epoch` ledger event and is held back while the home
/// adopts the new login: the series takes one `another-account` row and no figure; the next
/// reading, once adopted, is a `fig` row under the new key. Negative control: writing the event
/// only where the reading was recorded writes nothing for the held-back reading (`Nothing`, not
/// `Event`).
#[test]
fn another_account_is_one_event_row_even_when_held_back() {
    let dir = tmp("epoch");
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    let (one, two) = (
        profile::AccountKey::claude("11111111-1111-4111-8111-111111111111").unwrap(),
        profile::AccountKey::claude("22222222-2222-4222-8222-222222222222").unwrap(),
    );
    let keyed = |pct: u32, at: i64, key| TapRead {
        key: Some(key),
        ..reading(pct, 580, at)
    };
    assert_eq!(
        tap(dir.path(), &mut state, &keyed(380, NOW, one), NOW),
        Noted::Fig
    );
    let switched = NOW + 600;
    assert_eq!(
        tap(dir.path(), &mut state, &keyed(20, switched, two), switched),
        Noted::Event,
        "the epoch is committed, the reading held back"
    );
    let adopted = switched + accounts::KEY_ADOPT_S + 1;
    assert_eq!(
        tap(dir.path(), &mut state, &keyed(30, adopted, two), adopted),
        Noted::Fig
    );
    assert_eq!(kinds(dir.path()), ["fig", "event", "fig"]);
    let rows = read(dir.path(), "me", MACHINE);
    let events: Vec<&SeriesRow> = rows
        .iter()
        .filter(|r| matches!(r.said, Said::Event(_)))
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].t, switched);
    assert!(matches!(
        &events[0].said,
        Said::Event(EventSaid {
            e: EventWord::AnotherAccount,
            ..
        })
    ));
    assert!(matches!(&rows[2].said, Said::Fig { key: Some(k), .. } if *k == two));
}

/// SERIES ROWS HOLD LABELS, KEYS AND NUMBERS ONLY. A roster row naming a sentinel email, keyed
/// from it ([`super::super::journal::learn_with`]), and a profile key made from a sentinel uuid:
/// the `fig` row carries the record's lab key and account key, and no file in the series
/// directory holds the email, the uuid or an `@`, even where a host hands the email as the lab
/// key. Negative control: a lab-key check that takes any word puts the email into the file.
#[test]
fn series_rows_hold_labels_keys_and_numbers_only() {
    const EMAIL: &str = "sentinel.owner@example.invalid";
    const UUID: &str = "5c0ffee0-1234-4abc-8def-0123456789ab";
    let dir = tmp("keys");
    let roster = Roster {
        rows: vec![Row {
            label: "me".into(),
            email: Some(EMAIL.into()),
            ..Row::default()
        }],
    };
    let key = profile::AccountKey::claude(UUID).unwrap();
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    state.accounts.insert("me".into(), Account::default());
    super::super::journal::learn_with(Cli::Claude, &roster, &mut state, &|_, email| {
        (email == EMAIL).then(|| AC.to_owned())
    });
    assert_eq!(state.accounts["me"].pk.as_deref(), Some(AC), "PRECONDITION");
    let read = TapRead {
        key: Some(key),
        ..reading(380, 580, NOW)
    };
    assert_eq!(tap(dir.path(), &mut state, &read, NOW), Noted::Fig);
    let rows = read_rows(dir.path());
    assert!(
        matches!(&rows[0].said, Said::Fig { ac: Some(ac), key: Some(k), .. } if ac == AC && *k == key),
        "{rows:?}"
    );
    let fig = state.accounts["me"].windows.get(SESSION_LIMIT).cloned();
    let events = [accounts::Event::new(NOW + 60, accounts::Kind::Sample, "me")];
    note_committed(
        dir.path(),
        "me",
        MACHINE,
        &Committed {
            five_hour: fig.as_ref(),
            seven_day: None,
            events: &events,
            held_back: false,
            ac: Some(EMAIL),
            key: None,
        },
        NOW + 60,
    )
    .unwrap();
    assert_eq!(kinds(dir.path()), ["fig", "fig"]);
    for entry in std::fs::read_dir(dir_in(dir.path())).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for word in [EMAIL, UUID, "@"] {
            assert!(!text.contains(word), "{}: {word} in {text}", path.display());
        }
    }
}

fn read_rows(dir: &Path) -> Vec<SeriesRow> {
    read(dir, "me", MACHINE)
}

/// THE SERIES IS PRIVATE: its directory 0700, its files 0600 (the stamp of its last pass too),
/// its lock its own. Negative control: a stamp written 0644 fails.
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
    assert_eq!(mode(&dir_in(dir.path()).join(COMPACTED_FILE)), 0o600);
    assert!(!dir_in(dir.path()).join("accounts.lock").exists());
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

/// A file of a `plan` row `span + 1 h` ago and `rows` weekly figures over the `span` before
/// [`NOW`].
fn fill(path: &Path, rows: usize, span: i64) {
    let mut out = vec![plan_row(NOW - span - 3600, "20x")];
    for i in 0..rows {
        let t = NOW - span + span * i as i64 / rows as i64;
        out.push(fig_row(
            t,
            None,
            Some(fig_of(u32::try_from(i % 1000).unwrap(), NOW + D, t)),
        ));
    }
    write_rows(path, &out);
}

/// THE TAIL READS ONLY WHAT WAS APPENDED, AND THE WHOLE FILE AFTER A COMPACTION'S RENAME, AND
/// HOLDS ONLY THE OUTLOOK'S VIEW: a `beat` row but no `event` row, and no `fig` row older than
/// [`VIEW_S`] before now, and as now moves on it lets go of what left the view though the file
/// did not change; what it holds is what [`outlook_view`] reads of the whole file. Negative
/// control: holding every row it reads takes the event in.
#[test]
fn the_tail_reads_appended_bytes_and_rereads_after_a_rename() {
    let dir = tmp("tail");
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let mut tail = Tail::new(path.clone());
    assert!(!tail.refresh(NOW));
    tap(dir.path(), &mut state, &reading(380, 580, NOW), NOW);
    assert!(tail.refresh(NOW));
    assert_eq!(tail.rows().len(), 1);
    assert!(!tail.refresh(NOW), "an unchanged file reads nothing");
    let later = NOW + 600;
    tap(dir.path(), &mut state, &reading(420, 590, later), later);
    assert!(tail.refresh(NOW));
    assert_eq!(tail.rows().len(), 2);
    let beat = later + accounts::READING_HEARTBEAT_S;
    assert_eq!(
        tap(dir.path(), &mut state, &reading(420, 590, beat), beat),
        Noted::Beat
    );
    assert!(
        tail.refresh(NOW),
        "a beat is a reading the week chart repeats"
    );
    assert_eq!(tail.rows().len(), 3, "the beat is held");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("{}\n", event_row(beat + 60).line().unwrap()).as_bytes())
        .unwrap();
    assert!(!tail.refresh(NOW), "an event changes nothing it holds");
    assert_eq!(tail.rows().len(), 3, "no event is held");
    let rename = |path: &Path| {
        let renamed = path.with_extension("new");
        std::fs::copy(path, &renamed).unwrap();
        std::fs::rename(&renamed, path).unwrap();
    };
    fill(&path, 3, 3600);
    rename(&path);
    assert!(tail.refresh(NOW));
    assert_eq!(
        tail.rows().len(),
        4,
        "the plan row and three figures, read afresh"
    );
    fill(&path, 40, 10 * D);
    rename(&path);
    assert!(tail.refresh(NOW));
    assert!(
        tail.rows()
            .iter()
            .all(|r| matches!(r.said, Said::Plan(_)) || NOW - r.t <= VIEW_S),
        "no figure older than the view"
    );
    assert!(tail.rows().len() < 41, "older figures are not held");
    assert_eq!(
        tail.rows(),
        outlook_view(read(dir.path(), "me", MACHINE), NOW).as_slice(),
        "the tail holds what the whole read's view holds"
    );
    let ahead = fig_row(
        NOW + 2 * D,
        None,
        Some(fig_of(10, NOW + 3 * D, NOW + 2 * D)),
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("{}\n", ahead.line().unwrap()).as_bytes())
        .unwrap();
    assert!(tail.refresh(NOW));
    assert_eq!(
        tail.rows(),
        outlook_view(read(dir.path(), "me", MACHINE), NOW).as_slice(),
        "a row from the future drops nothing"
    );
    let on = NOW + 2 * D;
    assert!(tail.refresh(on), "two days on, older figures left the view");
    assert_eq!(
        tail.rows(),
        outlook_view(read(dir.path(), "me", MACHINE), on).as_slice(),
        "what is left is the view two days on"
    );
}

/// RUNS SPLIT AT A NEW ANCHOR, AN EPOCH AND A REFILL, by when each figure was served whatever
/// order they come in. Negative control: a splitter that ignores `epoch` joins the epoch's
/// figures to the refill's run (`(Refill, [15, 35, 50, 70])`).
#[test]
fn runs_split_at_anchor_epoch_and_refill() {
    let (old, new) = (NOW - 2 * H, NOW + 5 * D);
    let epoch = NOW - 55 * 60;
    let mut figs = vec![
        fig_of(900, old, NOW - 3 * H),
        fig_of(950, old, NOW - 150 * 60),
        fig_of(20, new, NOW - 90 * 60),
        fig_of(60, new, NOW - 80 * 60),
        fig_of(15, new, NOW - 70 * 60),
        fig_of(35, new, NOW - 60 * 60),
        Fig {
            epoch: Some(epoch),
            ..fig_of(50, new, NOW - 50 * 60)
        },
        Fig {
            epoch: Some(epoch),
            ..fig_of(70, new, NOW - 40 * 60)
        },
    ];
    figs.reverse();
    let shape: Vec<(Began, Vec<u32>)> = runs(&figs)
        .iter()
        .map(|run| (run.began, run.figs.iter().map(|f| f.pct_x10).collect()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (Began::Window, vec![900, 950]),
            (Began::Window, vec![20, 60]),
            (Began::Refill, vec![15, 35]),
            (Began::Epoch(epoch), vec![50, 70]),
        ]
    );
}

/// A CLOSE IS DERIVED ONCE PER ANCHOR. Three five-hour anchors ended: one with a limit reset
/// used inside it, one that read full, one a newer anchor's first figure ended before its reset;
/// the newest is still open. Each ended anchor gets one `close` row with what its figures said,
/// and a second pass leaves the file byte-identical and unwritten. Negative control: a deriver
/// blind to the closes the file holds closes each anchor again and rewrites the file on the
/// second pass.
#[test]
fn a_close_is_derived_once_per_anchor() {
    let dir = tmp("close");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let (r1, r2, r3) = (NOW - 20 * H, NOW - 15 * H, NOW - 10 * H);
    let r4 = NOW + H;
    let epoch = r1 - 2 * H - 600;
    let five = |pct, reset, at| fig_row(at, Some(fig_of(pct, reset, at)), None);
    let mut rows = vec![
        five(100, r1, r1 - 4 * H),
        five(300, r1, r1 - 3 * H),
        fig_row(
            r1 - 2 * H,
            Some(Fig {
                epoch: Some(epoch),
                ..fig_of(50, r1, r1 - 2 * H)
            }),
            None,
        ),
        five(200, r1, r1 - H),
        five(500, r2, r2 - 4 * H),
        five(1000, r2, r2 - 2 * H),
        five(1000, r2, r2 - H),
        five(100, r3, r3 - 4 * H),
        five(200, r3, r3 - 3 * H),
        five(40, r4, r3 - 2 * H),
        five(90, r4, NOW - 600),
    ];
    if let Said::Fig { ac, .. } = &mut rows[3].said {
        *ac = Some(AC.into());
    }
    write_rows(&path, &rows);
    let first = compact_locked(&path, NOW, utc).unwrap();
    assert!(first.rewrote);
    let after = rows_at(&path);
    let got = closes(&after, SESSION_LIMIT);
    assert_eq!(
        got.iter().map(|(_, c)| c.reset).collect::<Vec<_>>(),
        [r1, r2, r3],
        "one close per ended anchor, none for the open one"
    );
    let (t1, c1) = &got[0];
    assert_eq!(*t1, r1);
    assert_eq!(
        (c1.peak, c1.end, c1.n, c1.first, c1.last),
        (300, 200, 4, r1 - 4 * H, r1 - H)
    );
    assert_eq!(c1.from, r1 - accounts::SESSION_WINDOW_S);
    assert_eq!(c1.epochs, [epoch]);
    assert!(!c1.walled && !c1.early);
    assert_eq!(c1.ac.as_deref(), Some(AC), "the anchor's keys");
    assert_eq!(c1.src, SRC_SERIES);
    let (_, c2) = &got[1];
    assert!(c2.walled && c2.peak == 1000 && c2.epochs.is_empty());
    let (t3, c3) = &got[2];
    assert!(c3.early, "a newer anchor came before its reset");
    assert_eq!(
        *t3,
        r3 - 2 * H,
        "it ended at the newer anchor's first figure"
    );
    let bytes = std::fs::read(&path).unwrap();
    let second = compact_locked(&path, NOW + 60, utc).unwrap();
    assert!(!second.rewrote, "nothing to derive again");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

// The zone `day_rows_follow_the_local_day` reads its days by: an hour east of UTC, two hours
// from a spring change on the third day to a fall change on the sixth.
const M0: i64 = 1_790_467_200;
const SPRING: i64 = M0 + 2 * D + 2 * H - 3600;
const FALL: i64 = M0 + 5 * D + 3 * H - 7200;

fn zone(t: i64) -> i64 {
    if (SPRING..FALL).contains(&t) {
        7200
    } else {
        3600
    }
}

/// DAY ROWS FOLLOW THE LOCAL DAY. Under a zone an hour east of UTC with a daylight-saving change
/// on the third and the sixth days, one figure an hour gives one `day` row per local day at its
/// local midnight: 23 hours and 23 readings on the spring day, 25 on the fall day, and each day's
/// offset. Negative control: UTC days put each day row at a UTC midnight (the instants differ).
#[test]
fn day_rows_follow_the_local_day() {
    let dir = tmp("days");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let start = M0 - 3600;
    let end = M0 + 7 * D - 3600;
    let rows: Vec<SeriesRow> = (0..)
        .map(|i| start + i * H)
        .take_while(|t| *t < end)
        .map(|t| {
            fig_row(
                t,
                Some(fig_of(100, t + H / 2, t)),
                Some(fig_of(
                    u32::try_from((t - start) / H).unwrap(),
                    M0 + 9 * D,
                    t,
                )),
            )
        })
        .collect();
    write_rows(&path, &rows);
    let now = M0 + 8 * D;
    compact_locked(&path, now, zone).unwrap();
    let got = days(&rows_at(&path));
    let midnights = [
        M0 - 3600,
        M0 + D - 3600,
        M0 + 2 * D - 3600,
        M0 + 3 * D - 7200,
        M0 + 4 * D - 7200,
        M0 + 5 * D - 7200,
        M0 + 6 * D - 3600,
    ];
    assert_eq!(
        got.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        midnights,
        "local midnights"
    );
    assert_eq!(
        got.iter().map(|(_, d)| d.span_s).collect::<Vec<_>>(),
        [D, D, D - H, D, D, D + H, D]
    );
    assert_eq!(
        got.iter().map(|(_, d)| d.n).collect::<Vec<_>>(),
        [24, 24, 23, 24, 24, 25, 24]
    );
    assert_eq!(
        got.iter().map(|(_, d)| d.tz_s).collect::<Vec<_>>(),
        [3600, 3600, 3600, 7200, 7200, 7200, 3600]
    );
    let week = got[1].1.seven_day.expect("a week");
    assert_eq!((week.first, week.last, week.closed), (24, 47, false));
    let five = got[1].1.five_hour.as_ref().expect("five-hour windows");
    assert_eq!(five.closed, 24, "one closed each hour");
}

/// A DAY ROW NAMES THE FIVE-HOUR WINDOWS IT COUNTS BY ANCHOR. Three windows end on one UTC day,
/// the middle one read full: compaction's `day` row names each by its reset, oldest first, the
/// full one marked, and its line reads back as the row. A day of a window an hour still counts
/// all 24 and names its oldest [`DAY_ANCHORS_MAX`]. A row of the longest machine id, both keys,
/// every count and figure at its widest (figures at `10 *` [`accounts::PCT_MAX`], which a row
/// may hold) and every anchor full names all six of its windows at ten-digit instants, and of
/// seven the oldest six; at the widest instants a row may hold it is still written, under
/// [`ROW_MAX`], naming the oldest that fit, with its counts. Negative controls: `day_summaries`
/// naming no anchor (`anchors: Vec::new()`) fails the first assertion, and a row written whole
/// or not at all (no fitting in `SeriesRow::line`) writes neither the row of seven nor the
/// widest.
#[test]
fn a_day_row_names_its_windows_by_anchor() {
    let dir = tmp("anchors");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let day = NOW.div_euclid(D) * D - 3 * D;
    let resets = [day + 6 * H, day + 12 * H, day + 18 * H];
    let rows: Vec<SeriesRow> = resets
        .iter()
        .zip([400, 1000, 500])
        .flat_map(|(reset, peak)| {
            [
                fig_row(
                    reset - 3 * H,
                    Some(fig_of(200, *reset, reset - 3 * H)),
                    None,
                ),
                fig_row(reset - H, Some(fig_of(peak, *reset, reset - H)), None),
            ]
        })
        .collect();
    write_rows(&path, &rows);
    compact_locked(&path, NOW, utc).unwrap();
    let got = days(&rows_at(&path));
    let [(t, d)] = got.as_slice() else {
        panic!("one day: {got:?}");
    };
    let five = d.five_hour.as_ref().expect("five-hour windows");
    assert_eq!(
        five.anchors,
        [(resets[0], false), (resets[1], true), (resets[2], false)],
        "{five:?}"
    );
    assert_eq!((*t, five.closed, five.walled), (day, 3, 1));
    let line = lines_of(&path)
        .into_iter()
        .find(|l| l.contains(r#""k":"day""#))
        .expect("the day's line");
    let row = SeriesRow::parse(&line).expect("the day reads");
    assert_eq!(
        row.line().as_deref(),
        Some(line.as_str()),
        "it reads back whole"
    );

    let most = i64::try_from(DAY_ANCHORS_MAX).unwrap();
    let busy = path_of(dir.path(), "busy", MACHINE).unwrap();
    let hourly: Vec<SeriesRow> = (0..24)
        .map(|i| day + i * H)
        .map(|t| fig_row(t, Some(fig_of(100, t + H / 2, t)), None))
        .chain([fig_row(
            day + D,
            Some(fig_of(100, day + D + H / 2, day + D)),
            None,
        )])
        .collect();
    write_rows(&busy, &hourly);
    compact_locked(&busy, NOW, utc).unwrap();
    let got = days(&rows_at(&busy));
    let five = got
        .iter()
        .find(|(t, _)| *t == day)
        .and_then(|(_, d)| d.five_hour.clone())
        .expect("the busy day");
    assert_eq!(five.closed, 24, "every window counts");
    assert_eq!(
        five.anchors,
        (0..most)
            .map(|i| (day + i * H + H / 2, false))
            .collect::<Vec<_>>(),
        "the oldest {DAY_ANCHORS_MAX} are named"
    );

    let widest = |at: i64, anchors: &[i64]| SeriesRow {
        t: at,
        by: "f".repeat(accounts::WORD_MAX),
        said: Said::Day(Day {
            span_s: 25 * H,
            tz_s: -14 * H,
            five_hour: Some(DayFive {
                closed: u32::MAX,
                peak: 10 * accounts::PCT_MAX,
                walled: u32::MAX,
                anchors: anchors.iter().map(|a| (*a, true)).collect(),
            }),
            seven_day: Some(DayWeek {
                first: 10 * accounts::PCT_MAX,
                last: 10 * accounts::PCT_MAX,
                reset: at,
                closed: false,
            }),
            n: u32::MAX,
            ac: Some("ac-ffffffffffffffff".into()),
            key: profile::AccountKey::claude("ffffffff-ffff-4fff-bfff-ffffffffffff"),
        }),
    };
    let ten_digits = 9_999_999_999;
    let seven: Vec<i64> = (0..=most).map(|i| ten_digits - most + i).collect();
    let six = &seven[..DAY_ANCHORS_MAX];
    let line = widest(ten_digits, six).line().expect("the row is written");
    assert!(line.len() <= ROW_MAX, "{} bytes", line.len());
    assert_eq!(
        SeriesRow::parse(&line),
        Some(widest(ten_digits, six)),
        "six windows are named at ten-digit instants"
    );
    assert_eq!(
        widest(ten_digits, &seven)
            .line()
            .as_deref()
            .and_then(SeriesRow::parse),
        Some(widest(ten_digits, six)),
        "of seven the oldest six"
    );
    let eleven: Vec<i64> = (0..most).map(|i| -ten_digits + i).collect();
    let line = widest(-ten_digits, &eleven)
        .line()
        .expect("the widest row is written");
    assert!(line.len() <= ROW_MAX, "{} bytes", line.len());
    let back = SeriesRow::parse(&line).expect("it reads");
    let Said::Day(Day {
        five_hour: Some(five),
        ..
    }) = &back.said
    else {
        panic!("a day row: {back:?}");
    };
    assert_eq!((five.closed, five.walled), (u32::MAX, u32::MAX), "{back:?}");
    assert!(
        !five.anchors.is_empty()
            && five.anchors.len() < DAY_ANCHORS_MAX
            && eleven.starts_with(
                &five
                    .anchors
                    .iter()
                    .map(|(reset, _)| *reset)
                    .collect::<Vec<_>>()
            ),
        "the oldest that fit: {back:?}"
    );
}

/// A 420-day series of a figure every half hour over its last 91 days and every two hours
/// before (so the file stays inside [`READ_MAX`]), a beat every hour, an event every 30 days, and
/// `plan` rows at 415 and 410 days.
fn long_series() -> Vec<SeriesRow> {
    let start = (NOW - 420 * D).div_euclid(H) * H;
    let mut rows = Vec::new();
    let mut t = start + 600;
    while t <= NOW {
        let five_reset =
            (t.div_euclid(accounts::SESSION_WINDOW_S) + 1) * accounts::SESSION_WINDOW_S;
        let week_reset = (t.div_euclid(accounts::WEEK_WINDOW_S) + 1) * accounts::WEEK_WINDOW_S;
        let pct = |reset: i64, len: i64| u32::try_from((len - (reset - t)) * 999 / len).unwrap();
        rows.push(fig_row(
            t,
            Some(fig_of(
                pct(five_reset, accounts::SESSION_WINDOW_S),
                five_reset,
                t,
            )),
            Some(fig_of(
                pct(week_reset, accounts::WEEK_WINDOW_S),
                week_reset,
                t,
            )),
        ));
        if (t - start - 600) % H == 0 {
            rows.push(row_of(t + 300, Said::Beat));
        }
        t += if t < NOW - HOURLY_S - D { 2 * H } else { 1800 };
    }
    for days_ago in (0..420).step_by(30) {
        rows.push(event_row(NOW - days_ago * D - 7 * H));
    }
    rows.push(plan_row(NOW - 415 * D, "5x"));
    rows.push(plan_row(NOW - 410 * D, "20x"));
    rows.sort_by_key(|r| r.t);
    rows
}

/// Where each weekly run of `rows` begins and the figure before it, by when each was served:
/// what the hourly tier keeps beside each hour's last figure.
fn weekly_turns(rows: &[SeriesRow]) -> BTreeSet<i64> {
    let split = runs(rows.iter().filter_map(|r| r.fig(WEEK_LIMIT)));
    let mut out = BTreeSet::new();
    for (i, run) in split.iter().enumerate() {
        out.insert(run.figs[0].at);
        if let Some(prev) = i.checked_sub(1).and_then(|j| split[j].figs.last()) {
            out.insert(prev.at);
        }
    }
    out
}

/// An unknown row of kind `glimpse` at `t`, with fields no reader knows.
fn unknown_line(t: i64) -> String {
    format!(r#"{{"v":1,"t":{t},"k":"glimpse","by":"{MACHINE}","zz":[1,2,{{"q":true}}]}}"#)
}

/// COMPACTION KEEPS EACH TIER. A 420-day series: inside 7 days every row stays; from 7 to 90
/// days the last figure of each UTC hour stays, and where each weekly run begins with the figure
/// before it, and every beat goes, with every close, event, `day` and unknown row; from 90 to
/// 400 days only `day` rows, weekly closes, events, plans and unknown rows; past 400 days only
/// the newest `plan` row. A second pass changes nothing. Negative control: keeping every figure
/// of the hourly tier leaves two in an hour no weekly run begins in.
#[test]
fn compaction_keeps_each_tier() {
    let dir = tmp("tiers");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let input = long_series();
    let mut lines: Vec<String> = input.iter().map(|r| r.line().unwrap()).collect();
    for days_ago in [30, 200, 410] {
        lines.push(unknown_line(NOW - days_ago * D));
    }
    write_lines(&path, &lines);
    assert!(
        std::fs::metadata(&path).unwrap().len() <= READ_MAX,
        "PRECONDITION: a file a reader reads whole"
    );
    let left = compact_locked(&path, NOW, utc).unwrap();
    assert!(left.rewrote && left.bytes <= BOUND_BYTES, "{left:?}");
    let out_lines = lines_of(&path);
    let out = rows_at(&path);
    let age = |r: &SeriesRow| NOW - r.t;
    let tier = |r: &SeriesRow| match age(r) {
        a if a < RAW_S => 0,
        a if a < HOURLY_S => 1,
        a if a < DAILY_S => 2,
        _ => 3,
    };
    let count = |rows: &[SeriesRow], t: u8, k: &str| {
        rows.iter().filter(|r| tier(r) == t && kind(r) == k).count()
    };
    for k in ["fig", "beat"] {
        assert_eq!(
            count(&out, 0, k),
            count(&input, 0, k),
            "raw {k} rows all stay"
        );
    }
    assert_eq!(count(&out, 1, "beat"), 0, "no beat past the raw tier");
    let mut hours: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for r in input.iter().filter(|r| kind(r) == "fig") {
        hours.entry(r.t.div_euclid(H)).or_default().push(r.t);
    }
    let mut kept: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for r in out.iter().filter(|r| kind(r) == "fig" && tier(r) == 1) {
        kept.entry(r.t.div_euclid(H)).or_default().push(r.t);
    }
    let turns = weekly_turns(&input);
    let mut checked = 0;
    for (hour, ts) in &hours {
        if ts.iter().all(|t| (RAW_S..HOURLY_S).contains(&(NOW - t))) {
            let got: BTreeSet<i64> = kept
                .get(hour)
                .map(|ts| ts.iter().copied().collect())
                .unwrap_or_default();
            let mut want: BTreeSet<i64> =
                ts.iter().copied().filter(|t| turns.contains(t)).collect();
            want.insert(*ts.iter().max().unwrap());
            assert_eq!(
                got, want,
                "hour {hour}: its last figure, and where a weekly run begins"
            );
            checked += 1;
        }
    }
    assert!(checked > 1900, "{checked} hours checked");
    for k in ["fig", "beat"] {
        assert_eq!(count(&out, 2, k), 0, "no {k} in the daily tier");
    }
    let five = closes(&out, SESSION_LIMIT);
    assert!(
        five.iter().all(|(t, _)| NOW - t < HOURLY_S),
        "five-hour closes end at 90 days"
    );
    let resets: BTreeSet<i64> = five.iter().map(|(_, c)| c.reset).collect();
    let mut want = (NOW - HOURLY_S + accounts::SESSION_WINDOW_S)
        .div_euclid(accounts::SESSION_WINDOW_S)
        * accounts::SESSION_WINDOW_S;
    while want <= NOW - CLOSE_SLACK_S {
        assert!(
            resets.contains(&want),
            "a close for the five-hour reset {want}"
        );
        want += accounts::SESSION_WINDOW_S;
    }
    let week: BTreeSet<i64> = closes(&out, WEEK_LIMIT)
        .iter()
        .map(|(_, c)| c.reset)
        .collect();
    let mut want = (NOW - DAILY_S + accounts::WEEK_WINDOW_S).div_euclid(accounts::WEEK_WINDOW_S)
        * accounts::WEEK_WINDOW_S;
    while want <= NOW - CLOSE_SLACK_S {
        assert!(week.contains(&want), "a close for the weekly reset {want}");
        want += accounts::WEEK_WINDOW_S;
    }
    let day_rows = days(&out);
    let day_ts: BTreeSet<i64> = day_rows.iter().map(|(t, _)| *t).collect();
    assert_eq!(day_ts.len(), day_rows.len(), "one row per day");
    let mut want = (NOW - DAILY_S).div_euclid(D) * D + D;
    while want < NOW.div_euclid(D) * D {
        assert!(day_ts.contains(&want), "a day row for {want}");
        want += D;
    }
    assert!(day_ts.iter().all(|t| NOW - t < DAILY_S));
    let full = day_rows
        .iter()
        .find(|(t, _)| NOW - t > 200 * D)
        .map(|(_, d)| d.clone())
        .unwrap();
    assert_eq!(full.n, 24, "12 figures and 12 beats");
    assert!(full.five_hour.is_some_and(|f| (4..=5).contains(&f.closed)));
    for days_ago in [30, 200] {
        assert!(out_lines.contains(&unknown_line(NOW - days_ago * D)));
    }
    assert!(!out_lines.contains(&unknown_line(NOW - 410 * D)));
    let events: Vec<i64> = out
        .iter()
        .filter(|r| kind(r) == "event")
        .map(|r| r.t)
        .collect();
    assert_eq!(events.len(), 14, "every event inside 400 days: {events:?}");
    let plans: Vec<i64> = out
        .iter()
        .filter(|r| kind(r) == "plan")
        .map(|r| r.t)
        .collect();
    assert_eq!(
        plans,
        [NOW - 410 * D],
        "past 400 days only the newest plan stays"
    );
    assert!(
        out.iter().all(|r| tier(r) < 3 || kind(r) == "plan"),
        "nothing else past 400 days"
    );
    assert!(out.windows(2).all(|p| p[0].t <= p[1].t), "sorted by t");
    let bytes = std::fs::read(&path).unwrap();
    assert!(!compact_locked(&path, NOW, utc).unwrap().rewrote);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// `rows` with a `plan` row at `t` put first, older than every other row, so a bound that gives
/// up the oldest rows meets it first.
fn with_old_plan(mut rows: Vec<SeriesRow>, t: i64) -> Vec<SeriesRow> {
    rows.insert(0, plan_row(t, "20x"));
    rows
}

/// COMPACTION KEEPS EVERY EVENT AND WEEKLY CLOSE UNDER THE BOUND. A file whose raw week alone is
/// past [`BOUND_BYTES`] ends under it, [`BOUND_ROOM_BYTES`] under: the raw tier shrinks to three
/// days (whose rows all stay), the oldest `day` rows and hourly figures go while some still
/// remain to give, and every event, weekly close and the newest plan stay. Negative control:
/// with compaction off the file stays past the bound.
#[test]
fn compaction_keeps_every_event_and_weekly_close_under_the_bound() {
    let dir = tmp("bound");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let mut rows = Vec::new();
    let both = |t: i64| {
        let five = (t.div_euclid(accounts::SESSION_WINDOW_S) + 1) * accounts::SESSION_WINDOW_S;
        let week = (t.div_euclid(accounts::WEEK_WINDOW_S) + 1) * accounts::WEEK_WINDOW_S;
        let into = accounts::WEEK_WINDOW_S - (week - t);
        fig_row(
            t,
            Some(fig_of(u32::try_from(t % 900).unwrap(), five, t)),
            Some(fig_of(
                u32::try_from(into * 999 / accounts::WEEK_WINDOW_S).unwrap(),
                week,
                t,
            )),
        )
    };
    // The shrunk raw tier 400 KiB short of what the bound leaves, so the bound is met by giving
    // up some of the older figures and days; the rest of the raw week takes it 128 KiB past.
    let one = i64::try_from(both(NOW).line().unwrap().len()).unwrap() + 1;
    let room = i64::try_from(BOUND_BYTES - BOUND_ROOM_BYTES).unwrap() - 400 * 1024;
    let older = i64::try_from(BOUND_BYTES).unwrap() - room + 128 * 1024;
    let mut t = NOW - RAW_SHRUNK_S + 1;
    while t <= NOW {
        rows.push(both(t));
        t += RAW_SHRUNK_S * one / room;
    }
    let mut t = NOW - RAW_S;
    while t < NOW - RAW_SHRUNK_S {
        rows.push(both(t));
        t += (RAW_S - RAW_SHRUNK_S) * one / older;
    }
    let mut t = NOW - HOURLY_S + H;
    while t < NOW - RAW_S {
        rows.push(both(t));
        t += 1800;
    }
    for i in 200..380 {
        let at = NOW - i * D;
        rows.push(row_of(
            at,
            Said::Day(Day {
                span_s: D,
                tz_s: 0,
                five_hour: None,
                seven_day: Some(DayWeek {
                    first: 10,
                    last: 20,
                    reset: at + 3 * D,
                    closed: false,
                }),
                n: 1,
                ac: None,
                key: None,
            }),
        ));
        if i % 7 == 0 {
            rows.push(row_of(
                at,
                Said::Close(Close {
                    w: WEEK_LIMIT,
                    reset: at,
                    from: at - accounts::WEEK_WINDOW_S,
                    peak: 700,
                    end: 700,
                    first: at - 6 * D,
                    last: at - H,
                    n: 300,
                    walled: false,
                    epochs: Vec::new(),
                    early: false,
                    src: SRC_SERIES.into(),
                    ac: None,
                    key: None,
                }),
            ));
        }
    }
    for i in (0..380).step_by(2) {
        rows.push(event_row(NOW - i * D - 5 * H));
    }
    rows.sort_by_key(|r| r.t);
    let rows = with_old_plan(rows, NOW - 395 * D);
    write_rows(&path, &rows);
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(
        len > BOUND_BYTES && len <= READ_MAX,
        "PRECONDITION: past the bound, inside what a reader reads: {len}"
    );
    let raw_week: usize = rows
        .iter()
        .filter(|r| NOW - r.t < RAW_S)
        .map(|r| r.line().unwrap().len() + 1)
        .sum();
    assert!(
        raw_week as u64 > BOUND_BYTES,
        "PRECONDITION: the raw week alone: {raw_week}"
    );
    let left = compact_locked(&path, NOW, utc).unwrap();
    assert!(left.bytes <= BOUND_BYTES - BOUND_ROOM_BYTES, "{left:?}");
    assert!(std::fs::metadata(&path).unwrap().len() <= BOUND_BYTES);
    let out = rows_at(&path);
    let shrunk = |r: &SeriesRow| NOW - r.t < RAW_SHRUNK_S;
    assert_eq!(
        out.iter().filter(|r| shrunk(r) && kind(r) == "fig").count(),
        rows.iter()
            .filter(|r| shrunk(r) && kind(r) == "fig")
            .count(),
        "the shrunk raw tier keeps every row"
    );
    let turns = weekly_turns(&rows);
    let mut hours: BTreeMap<i64, usize> = BTreeMap::new();
    for r in out
        .iter()
        .filter(|r| !shrunk(r) && kind(r) == "fig" && !turns.contains(&r.t))
    {
        *hours.entry(r.t.div_euclid(H)).or_default() += 1;
    }
    assert!(
        hours.values().all(|n| *n == 1),
        "from three days the hourly tier keeps one figure an hour, beside where a weekly run \
         begins"
    );
    let events = |rows: &[SeriesRow]| rows.iter().filter(|r| kind(r) == "event").count();
    assert_eq!(events(&out), events(&rows), "every event stays");
    let weekly_in: BTreeSet<i64> = closes(&rows, WEEK_LIMIT)
        .iter()
        .map(|(_, c)| c.reset)
        .collect();
    let weekly_out: BTreeSet<i64> = closes(&out, WEEK_LIMIT)
        .iter()
        .map(|(_, c)| c.reset)
        .collect();
    assert!(weekly_in.is_subset(&weekly_out), "every weekly close stays");
    assert!(
        weekly_out.len() > weekly_in.len(),
        "the weekly closes derived from the figures stay too"
    );
    assert!(
        !days(&out).iter().any(|(t, _)| *t == NOW - 379 * D),
        "the oldest day row went first"
    );
    assert!(
        out.iter()
            .any(|r| !shrunk(r) && matches!(kind(r), "fig" | "day")),
        "the bound was met with figures and days still left to give"
    );
    assert_eq!(
        newest_plan(&out).and_then(|p| p.tier.as_deref()),
        Some("20x"),
        "the newest plan stays"
    );
}

/// COMPACTION KEEPS ROWS IT CANNOT READ, exactly as written: a kind this build does not know to
/// the daily horizon, a known kind with a field it does not know by its tiers, and a known kind
/// whose fields it cannot read (no newer build writes one) to its kind's horizon: a `fig` the
/// raw tier's, a `close` the hourly's, an `event` the daily's. A line that is not a schema-1
/// envelope goes, and so does one whose kind is not a word. Negative controls: keeping only the
/// rows `SeriesRow::parse` reads drops the unknown kind; keeping an unreadable known kind as an
/// unknown one keeps the unreadable `fig` 30 days.
#[test]
fn compaction_keeps_rows_it_cannot_read() {
    let dir = tmp("unknown");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let hourly = NOW - 30 * D;
    let daily = NOW - 200 * D;
    let extra_fig = format!(
        r#"{{"v":1,"t":{hourly},"k":"fig","w":{{"seven_day":{{"pct_x10":10,"reset":{},"at":{hourly},"src":"tap","model":"m"}}}},"by":"{MACHINE}","later":"word"}}"#,
        hourly + D
    );
    let extra_close = format!(
        r#"{{"v":1,"t":{daily},"k":"close","w":"seven_day","reset":{daily},"from":{},"peak":5,"end":5,"first":{},"last":{},"n":2,"walled":false,"epochs":[],"early":false,"src":"series","by":"{MACHINE}","cost":12}}"#,
        daily - accounts::WEEK_WINDOW_S,
        daily - D,
        daily - H
    );
    let odd_fig = |t: i64| format!(r#"{{"v":1,"t":{t},"k":"fig","w":"oops","by":"{MACHINE}"}}"#);
    let odd_close =
        format!(r#"{{"v":1,"t":{daily},"k":"close","w":"seven_day","by":"{MACHINE}"}}"#);
    let odd_event =
        format!(r#"{{"v":1,"t":{daily},"k":"event","e":"Not A Word","by":"{MACHINE}"}}"#);
    let kept = [
        unknown_line(hourly - 60),
        unknown_line(daily),
        extra_fig.clone(),
        extra_close.clone(),
        odd_fig(NOW - D),
        odd_event,
    ];
    let dropped = [
        odd_fig(hourly + 60),
        odd_close,
        format!(r#"{{"v":2,"t":{hourly},"k":"fig"}}"#),
        format!(r#"{{"t":{hourly},"k":"fig"}}"#),
        format!(r#"{{"v":1,"t":{hourly},"k":"free text, not a kind","by":"{MACHINE}"}}"#),
        "not a row".to_owned(),
        unknown_line(NOW - 410 * D),
    ];
    let mut lines: Vec<String> = kept.iter().chain(&dropped).cloned().collect();
    lines.push(
        fig_row(NOW, None, Some(fig_of(10, NOW + D, NOW)))
            .line()
            .unwrap(),
    );
    write_lines(&path, &lines);
    compact_locked(&path, NOW, utc).unwrap();
    let out = lines_of(&path);
    for line in &kept {
        assert!(out.contains(line), "kept as written: {line}\n{out:#?}");
    }
    for line in &dropped {
        assert!(!out.contains(line), "dropped: {line}");
    }
    assert!(
        SeriesRow::parse(&extra_fig).is_some() && SeriesRow::parse(&unknown_line(daily)).is_none(),
        "PRECONDITION: one known with an unknown field, one unknown kind"
    );
    assert_eq!(
        Envelope::parse(&unknown_line(daily)),
        Some(Envelope {
            t: daily,
            k: "glimpse".into()
        })
    );
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).unwrap().ino()
}

#[cfg(unix)]
fn plant_beat(path: &Path, t: i64) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(format!("{}\n", row_of(t, Said::Beat).line().unwrap()).as_bytes())
        .unwrap();
}

#[cfg(unix)]
fn has_beat(path: &Path, t: i64) -> bool {
    rows_at(path)
        .iter()
        .any(|r| r.t == t && matches!(r.said, Said::Beat))
}

/// AN APPEND COMPACTS ONCE A DAY: with no stamp it runs a pass (the file is rewritten, a new
/// inode, and a beat past the raw tier goes); with a fresh stamp it runs none; with a stamp a
/// day old, or more than a day ahead of the clock, it runs one again; each pass stamps its own
/// second. Negative control: ignoring the stamp rewrites the file on the fresh-stamp append (the
/// inodes differ).
#[cfg(unix)]
#[test]
fn an_append_compacts_once_a_day() {
    let dir = tmp("daily");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    write_rows(
        &path,
        &[fig_row(
            NOW - 8 * D,
            None,
            Some(fig_of(5, NOW + D, NOW - 8 * D)),
        )],
    );
    let append = |at: i64, pct: u32| {
        note_within(
            dir.path(),
            "me",
            MACHINE,
            (None, Some(fig_of(pct, NOW + D, at))),
            (None, None),
            at,
            LOCK_WAIT,
        )
        .unwrap()
    };
    let stamped = || compacted_at(&dir_in(dir.path()));
    let mut cases = 0;
    for (stamp_at, at, runs) in [
        (None, NOW, true),
        (Some(NOW), NOW + 60, false),
        (Some(NOW + 3 * D), NOW + 120, true),
        (Some(NOW + 180 - COMPACT_EVERY_S), NOW + 180, true),
    ] {
        if let Some(stamp_at) = stamp_at {
            stamp(dir.path(), stamp_at);
        }
        let planted = NOW - 8 * D + 10 + at - NOW;
        plant_beat(&path, planted);
        let before = inode(&path);
        assert_eq!(
            append(at, 10 + u32::try_from(at - NOW).unwrap()),
            Noted::Fig
        );
        let rewrote = inode(&path) != before;
        assert_eq!(rewrote, runs, "stamp {stamp_at:?}, append at {at}");
        assert_eq!(!has_beat(&path, planted), runs);
        assert_eq!(stamped(), Some(if runs { at } else { NOW }));
        cases += 1;
    }
    assert_eq!(cases, 4);
}

/// AN APPEND PAST THE BOUND COMPACTS that file at once, whatever the stamp says: the file ends
/// under [`BOUND_BYTES`], the appended row and the newest plan in it. Negative control: an
/// append that never compacts leaves the file past the bound.
#[test]
fn an_append_past_the_bound_compacts() {
    let dir = tmp("past");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    stamp(dir.path(), NOW);
    let mut lines = vec![plan_row(NOW - 30 * D, "20x").line().unwrap()];
    let mut len = lines[0].len() as u64 + 1;
    let mut t = NOW - 10 * D;
    while len < BOUND_BYTES - 64 {
        let line = row_of(t, Said::Beat).line().unwrap();
        len += line.len() as u64 + 1;
        lines.push(line);
        t += 1;
    }
    write_lines(&path, &lines);
    assert_eq!(
        note_within(
            dir.path(),
            "me",
            MACHINE,
            (None, Some(fig_of(999, NOW + D, NOW))),
            (None, None),
            NOW,
            LOCK_WAIT,
        )
        .unwrap(),
        Noted::Fig
    );
    assert!(std::fs::metadata(&path).unwrap().len() <= BOUND_BYTES);
    let rows = read_rows(dir.path());
    assert_eq!(kind(rows.last().unwrap()), "fig");
    assert!(newest_plan(&rows).is_some());
}

/// AN APPEND WAITS OUT A COMPACTION: an append takes the lock, so one that may not wait while
/// it is held writes nothing, and one that waits while a compaction holds it lands in the
/// compacted file. Negative control: an append that skips the lock writes under a held one
/// (`Fig`, not `Dropped`).
#[test]
fn an_append_waits_out_a_compaction() {
    let dir = tmp("wait");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    fill(&path, 2000, 10 * D);
    stamp(dir.path(), NOW);
    let held = lock_within(&dir_in(dir.path()), LOCK_WAIT)
        .unwrap()
        .expect("the lock");
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        note_within(
            dir.path(),
            "me",
            MACHINE,
            (None, Some(fig_of(555, NOW + D, NOW))),
            (None, None),
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
            // A one-minute hang detector, not a latency budget.
            note_within(
                &state,
                "me",
                MACHINE,
                (None, Some(fig_of(777, NOW + D, NOW + 1))),
                (None, None),
                NOW + 1,
                Duration::from_secs(60),
            )
        })
    };
    let left = compact_locked(&path, NOW, utc).unwrap();
    assert!(left.rewrote, "PRECONDITION: the compaction gave rows up");
    drop(held);
    assert_eq!(waiting.join().unwrap().unwrap(), Noted::Fig);
    let rows = read_rows(dir.path());
    assert_eq!(
        rows.last()
            .and_then(|r| r.fig(WEEK_LIMIT))
            .map(|f| f.pct_x10),
        Some(777),
        "the append that waited on the compaction's lock survives it"
    );
    assert!(
        rows.len() < 2001,
        "the append landed in the compacted file: {}",
        rows.len()
    );
}

/// A SERIES WITH NOTHING INSIDE 400 DAYS IS REMOVED at a pass over the directory, whatever its
/// newest plan says, and no other: 33 live series and one whose newest row is a `day` row 399
/// days old stay. Negative control: a cap of 32 series removes a live one.
#[test]
fn a_series_with_nothing_inside_400_days_is_removed() {
    let dir = tmp("gone");
    let live: Vec<String> = (0..33).map(|i| format!("a{i:02}")).collect();
    for label in &live {
        let t = NOW - D;
        write_rows(
            &path_of(dir.path(), label, MACHINE).unwrap(),
            &[fig_row(t, None, Some(fig_of(10, NOW + D, t)))],
        );
    }
    let old = path_of(dir.path(), "old", MACHINE).unwrap();
    write_rows(
        &old,
        &[row_of(
            NOW - 399 * D,
            Said::Day(Day {
                span_s: D,
                tz_s: 0,
                five_hour: None,
                seven_day: None,
                n: 3,
                ac: None,
                key: None,
            }),
        )],
    );
    let gone = path_of(dir.path(), "gone", MACHINE).unwrap();
    write_rows(
        &gone,
        &[
            plan_row(NOW - 500 * D, "20x"),
            fig_row(
                NOW - 401 * D,
                None,
                Some(fig_of(10, NOW - 400 * D, NOW - 401 * D)),
            ),
        ],
    );
    compact_dir(&dir_in(dir.path()), NOW, utc).unwrap();
    assert!(!gone.exists(), "nothing inside 400 days");
    assert!(old.exists());
    for label in &live {
        assert!(
            path_of(dir.path(), label, MACHINE).unwrap().exists(),
            "{label} stays"
        );
    }
    assert_eq!(compacted_at(&dir_in(dir.path())), Some(NOW));
    assert!(!compact_due(&dir_in(dir.path()), NOW + 60));
}

/// THE OUTLOOK READS A WEEK AND ITS LOOKBACK BEFORE NOW, and no row's clock hides another's: a
/// figure seven and a half days old, one an hour old and the beat after it are in the view (the
/// week chart repeats a figure at each beat), a beat nine days old is not, and a figure a clock 30
/// days ahead wrote is in it and hides neither (the outlook's own admission refuses its figure);
/// the tail holds the same. Negative control: a view of seven days of the newest row drops the
/// old figure, and with the row from the future every real one; a view of `fig` rows alone drops
/// the beat.
#[test]
fn the_outlook_view_reads_a_week_and_its_lookback_before_now() {
    let dir = tmp("view");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let old = NOW - 7 * D - 12 * H;
    let mut rows = vec![
        plan_row(NOW - 30 * D, "20x"),
        row_of(NOW - 9 * D, Said::Beat),
        fig_row(old, None, Some(fig_of(900, NOW - 12 * H, old))),
        fig_row(NOW - H, None, Some(fig_of(60, NOW + 6 * D, NOW - H))),
        row_of(NOW - H / 2, Said::Beat),
    ];
    let ts = |rows: &[SeriesRow]| rows.iter().map(|r| r.t).collect::<Vec<_>>();
    assert_eq!(
        ts(&outlook_view(rows.clone(), NOW)),
        [NOW - 30 * D, old, NOW - H, NOW - H / 2],
        "the week and its lookback"
    );
    let ahead = NOW + 30 * D;
    rows.push(fig_row(ahead, None, Some(fig_of(10, ahead + 6 * D, ahead))));
    assert_eq!(
        ts(&outlook_view(rows.clone(), NOW)),
        [NOW - 30 * D, old, NOW - H, NOW - H / 2, ahead],
        "a row from the future hides nothing"
    );
    write_rows(&path, &rows);
    let mut tail = Tail::new(path);
    assert!(tail.refresh(NOW));
    assert_eq!(tail.rows(), outlook_view(rows, NOW).as_slice());
}

/// AN ANCHOR ENDS AT THE NEXT ANCHOR READ AFTER IT, by when the figures came, not by which reset
/// is later: an account whose five-hour window resets at 14:00 read from 09:00, then another
/// signed in whose window resets at 13:30 read from 10:10. A pass at 11:00 closes the first
/// early at 10:10 and leaves the second open; once the second's reset passed, its close holds
/// every figure of it, its wall included. A figure from a clock 30 days ahead names no newer
/// anchor and closes nothing. Negative control: ordering anchors by their resets closes the
/// 13:30 window at 11:00 from its first figures and never again.
#[test]
fn an_anchor_ends_at_the_next_anchor_read_after_it() {
    let dir = tmp("next");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let (x, y) = (NOW + 3 * H, NOW + 150 * 60);
    let five = |pct: u32, reset: i64, at: i64| fig_row(at, Some(fig_of(pct, reset, at)), None);
    let mut rows: Vec<SeriesRow> = (0..6)
        .map(|i| five(100 + 20 * i, x, NOW - 2 * H + i64::from(i) * 600))
        .collect();
    rows.extend((0..5).map(|i| five(10 + 10 * i, y, NOW - 50 * 60 + i64::from(i) * 600)));
    write_rows(&path, &rows);
    compact_locked(&path, NOW, utc).unwrap();
    let got = closes(&rows_at(&path), SESSION_LIMIT);
    assert_eq!(
        got.iter()
            .map(|(t, c)| (*t, c.reset, c.early, c.n))
            .collect::<Vec<_>>(),
        [(NOW - 50 * 60, x, true, 6)],
        "the 14:00 window ended at the 13:30 window's first figure; the 13:30 window is open"
    );
    let mut later: Vec<SeriesRow> = (1..=12)
        .map(|i| five(60 + 50 * i, y, NOW + i64::from(i) * 600))
        .collect();
    later.push(five(1000, y, NOW + 2 * H + 60));
    let mut all = rows_at(&path);
    all.extend(later);
    write_rows(&path, &all);
    compact_locked(&path, y + CLOSE_SLACK_S + 60, utc).unwrap();
    let got = closes(&rows_at(&path), SESSION_LIMIT);
    let (_, c) = got
        .iter()
        .find(|(_, c)| c.reset == y)
        .expect("the 13:30 window's close");
    assert_eq!(
        (c.n, c.peak, c.walled, c.early),
        (18, 1000, true, false),
        "every figure of it, its wall included"
    );
    let open = NOW + 2 * H;
    let ahead = NOW + 30 * D;
    let rows = vec![
        five(100, open, NOW - H),
        five(200, open, NOW - 600),
        five(50, ahead + 3 * H, ahead),
    ];
    assert!(
        derive_closes(&rows, NOW).is_empty(),
        "a figure from the future closes nothing"
    );
}

/// AN EVENT OR A PLAN THE LOCK KEPT OUT IS WRITTEN BY THE NEXT APPEND: another account's sign-in
/// and a tier written while a compaction holds the series lock past the wait are dropped from
/// that write, held by the process, and appended at their own instants by the next writer to
/// the file that takes the lock. Negative control: dropping them leaves only the two `fig`
/// rows.
#[test]
fn an_event_or_a_plan_the_lock_kept_out_is_written_by_the_next_append() {
    let dir = tmp("outbox");
    stamp(dir.path(), NOW);
    let mut state = State {
        machine: MACHINE.into(),
        ..State::default()
    };
    let (one, two) = (
        profile::AccountKey::claude("11111111-1111-4111-8111-111111111111").unwrap(),
        profile::AccountKey::claude("22222222-2222-4222-8222-222222222222").unwrap(),
    );
    let keyed = |pct: u32, at: i64, key| TapRead {
        key: Some(key),
        ..reading(pct, 580, at)
    };
    assert_eq!(
        tap(dir.path(), &mut state, &keyed(380, NOW, one), NOW),
        Noted::Fig
    );
    let held = lock_within(&dir_in(dir.path()), LOCK_WAIT)
        .unwrap()
        .expect("the lock");
    let switched = NOW + 600;
    assert_eq!(
        tap(dir.path(), &mut state, &keyed(20, switched, two), switched),
        Noted::Dropped
    );
    let tier = PlanSaid {
        word: "max".into(),
        tier: Some("5x".into()),
        src: "profile".into(),
    };
    assert_eq!(
        note_plan(dir.path(), "me", MACHINE, &tier, switched + 1).unwrap(),
        Noted::Dropped
    );
    drop(held);
    let adopted = switched + accounts::KEY_ADOPT_S + 1;
    assert_eq!(
        tap(dir.path(), &mut state, &keyed(30, adopted, two), adopted),
        Noted::Fig
    );
    let rows = read_rows(dir.path());
    assert_eq!(
        rows.iter().map(|r| (kind(r), r.t)).collect::<Vec<_>>(),
        [
            ("fig", NOW),
            ("event", switched),
            ("plan", switched + 1),
            ("fig", adopted)
        ]
    );
    assert_eq!(
        note_plan(dir.path(), "me", MACHINE, &tier, adopted + 1).unwrap(),
        Noted::SamePlan,
        "the held plan was written once"
    );
}

/// A DAY TAKES ITS BEATS AND ITS ROWS IN TIME ORDER. A beat repeats the last `fig` row's
/// figures and keys, so a day of beats alone says its week and its keys; and a day's rows give
/// one summary whatever order they come in, a week's close taking the day's figures only where
/// it read none. Negative controls: a beat counted with no figures leaves the day's week and
/// keys empty; a close that fills the day's first figure when it comes first gives a day an 80%
/// first figure in one order and 5% in the other.
#[test]
fn a_day_takes_its_beats_and_its_rows_in_time_order() {
    let day = (NOW.div_euclid(D) - 3) * D;
    let mut rows = vec![SeriesRow {
        t: day - H,
        by: MACHINE.into(),
        said: Said::Fig {
            five_hour: None,
            seven_day: Some(fig_of(300, NOW + 3 * D, day - H)),
            ac: Some(AC.into()),
            key: None,
        },
    }];
    rows.extend((0..96).map(|i| row_of(day + i * 15 * 60, Said::Beat)));
    let got = days(&day_summaries(&rows, day, day + D, utc));
    let (t, d) = got.first().expect("a day");
    assert_eq!(*t, day);
    assert_eq!(d.n, 96);
    assert_eq!(
        d.seven_day.map(|w| (w.first, w.last)),
        Some((300, 300)),
        "the week its beats repeat"
    );
    assert_eq!(d.ac.as_deref(), Some(AC), "the keys its beats repeat");
    let next = day + D;
    let reset = next + 2 * H;
    let close = row_of(
        reset,
        Said::Close(Close {
            w: WEEK_LIMIT,
            reset,
            from: reset - accounts::WEEK_WINDOW_S,
            peak: 800,
            end: 800,
            first: reset - 6 * D,
            last: reset - H,
            n: 400,
            walled: false,
            epochs: Vec::new(),
            early: false,
            src: SRC_SERIES.into(),
            ac: None,
            key: None,
        }),
    );
    let new_week = fig_row(
        next + 10 * H,
        None,
        Some(fig_of(50, reset + 7 * D, next + 10 * H)),
    );
    let in_order = day_summaries(&[close.clone(), new_week.clone()], next, next + D, utc);
    let reversed = day_summaries(&[new_week, close], next, next + D, utc);
    assert_eq!(in_order, reversed, "one summary whatever the order");
    let week = days(&in_order)[0].1.seven_day.expect("a week");
    assert_eq!(
        (week.first, week.last, week.reset, week.closed),
        (50, 50, reset + 7 * D, true)
    );
}

/// THE BOUND KEEPS WHAT DERIVATION READS, SO A SECOND PASS WRITES NOTHING: a file past the bound
/// whose only older rows are a day ten days back and that day's figures gives up the figures
/// and keeps the day's row, the newest, which says which days were summed, so the next pass
/// sums none again and leaves the file unwritten. Negative control: giving up the day row first
/// makes the next pass sum the day again from its remaining figures and rewrite the file.
#[test]
fn the_bound_keeps_what_derivation_reads_so_a_second_pass_writes_nothing() {
    let dir = tmp("marks");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let today = NOW.div_euclid(D) * D;
    let now = today + 20 * H;
    let reset = now + D;
    let old = today - 10 * D;
    let mut lines = vec![
        row_of(
            old,
            Said::Day(Day {
                span_s: D,
                tz_s: 0,
                five_hour: None,
                seven_day: Some(DayWeek {
                    first: 10,
                    last: 30,
                    reset,
                    closed: false,
                }),
                n: 24,
                ac: None,
                key: None,
            }),
        )
        .line()
        .unwrap(),
    ];
    for i in 0..24 {
        let at = old + i * H + 1800;
        lines.push(
            fig_row(
                at,
                None,
                Some(Fig {
                    epoch: Some(old - D),
                    ..fig_of(10 + u32::try_from(i).unwrap(), reset, at)
                }),
            )
            .line()
            .unwrap(),
        );
    }
    let size = |l: &String| l.len() as u64 + 1;
    let (day_bytes, fig_bytes) = (size(&lines[0]), size(&lines[1]));
    let mut total: u64 = lines.iter().map(size).sum();
    let mut t = today + 60;
    while total <= BOUND_BYTES + day_bytes + fig_bytes {
        let line = fig_row(t, None, Some(fig_of(40, reset, t))).line().unwrap();
        total += size(&line);
        lines.push(line);
        t += 3;
    }
    assert!(t < now, "PRECONDITION: every raw row before now");
    write_lines(&path, &lines);
    let first = compact_locked(&path, now, utc).unwrap();
    assert!(first.rewrote && first.bytes <= BOUND_BYTES, "{first:?}");
    assert!(
        days(&rows_at(&path)).iter().any(|(t, _)| *t == old),
        "the newest day row stays"
    );
    let bytes = std::fs::read(&path).unwrap();
    let second = compact_locked(&path, now + 60, utc).unwrap();
    assert!(!second.rewrote, "nothing to sum again: {second:?}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// A COMPACTION PAST THE BOUND LEAVES ROOM FOR THE APPENDS AFTER IT: a file whose rows are all
/// raw and past the bound is compacted by the append that took it past, and the appends that
/// follow write into the room it left without compacting again (the file keeps its inode).
/// Negative control: giving rows up only to the bound makes the next append cross it and
/// rewrite the file again.
#[cfg(unix)]
#[test]
fn a_compaction_past_the_bound_leaves_room_for_the_appends_after_it() {
    let dir = tmp("room");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    stamp(dir.path(), NOW);
    let reset = NOW + 3 * D;
    let mut lines = Vec::new();
    let mut len = 0u64;
    let mut t = NOW - 2 * D;
    while len < BOUND_BYTES - 64 {
        let line = fig_row(t, None, Some(fig_of(40, reset, t))).line().unwrap();
        len += line.len() as u64 + 1;
        lines.push(line);
        t += 1;
    }
    write_lines(&path, &lines);
    let append = |at: i64, pct: u32| {
        note_within(
            dir.path(),
            "me",
            MACHINE,
            (
                Some(fig_of(pct, NOW + 4 * H, at)),
                Some(fig_of(pct, reset, at)),
            ),
            (None, None),
            at,
            LOCK_WAIT,
        )
        .unwrap()
    };
    let before = inode(&path);
    assert_eq!(append(NOW, 100), Noted::Fig);
    assert_ne!(inode(&path), before, "PRECONDITION: the append compacted");
    assert!(std::fs::metadata(&path).unwrap().len() <= BOUND_BYTES);
    let compacted = inode(&path);
    for i in 1..=8 {
        assert_eq!(append(NOW + i, 100 + u32::try_from(i).unwrap()), Noted::Fig);
        assert_eq!(inode(&path), compacted, "append {i} compacted again");
    }
}

/// A PASS IS CLAIMED BEFORE IT RUNS: the stamp is written under the series lock before any
/// series is compacted, so an appender that comes while a pass runs finds it claimed, and
/// where the stamp cannot be written no pass runs (it would run on every append). Negative
/// control: stamping after the pass compacts the other series though the stamp cannot be
/// written.
#[cfg(unix)]
#[test]
fn a_pass_is_claimed_before_it_runs() {
    let dir = tmp("claim");
    let other = path_of(dir.path(), "other", MACHINE).unwrap();
    write_rows(
        &other,
        &[fig_row(
            NOW - 8 * D,
            None,
            Some(fig_of(5, NOW + D, NOW - 8 * D)),
        )],
    );
    let planted = NOW - 8 * D + 10;
    plant_beat(&other, planted);
    let stamp_at = dir_in(dir.path()).join(COMPACTED_FILE);
    std::fs::create_dir_all(stamp_at.join("held")).unwrap();
    let append = |at: i64, pct: u32| {
        note_within(
            dir.path(),
            "me",
            MACHINE,
            (None, Some(fig_of(pct, NOW + D, at))),
            (None, None),
            at,
            LOCK_WAIT,
        )
        .unwrap()
    };
    assert_eq!(append(NOW, 10), Noted::Fig);
    assert!(has_beat(&other, planted), "no pass ran without its stamp");
    std::fs::remove_dir_all(&stamp_at).unwrap();
    assert_eq!(append(NOW + 60, 20), Noted::Fig);
    assert!(!has_beat(&other, planted), "the claimed pass ran");
    assert_eq!(compacted_at(&dir_in(dir.path())), Some(NOW + 60));
}

/// COMPACTION READS AT MOST WHAT A READER READS: a file past [`READ_MAX`] (a foreign writer's,
/// or one whose rewrites kept failing) is compacted from its newest [`READ_MAX`] bytes, so it
/// ends under the bound and readable again; a row before those bytes is not read, the newest
/// plan included, as no reader reads such a file at all. Negative control: reading the whole
/// file keeps that plan.
#[test]
fn compaction_reads_at_most_what_a_reader_reads() {
    let dir = tmp("cap");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let mut lines = vec![plan_row(NOW - 2 * D, "20x").line().unwrap()];
    let mut len = lines[0].len() as u64 + 1;
    let mut t = NOW - 2 * D + 1;
    while len < READ_MAX + 64 * 1024 {
        let line = fig_row(t, None, Some(fig_of(40, NOW + D, t)))
            .line()
            .unwrap();
        len += line.len() as u64 + 1;
        lines.push(line);
        t += 1;
    }
    write_lines(&path, &lines);
    assert!(
        read_rows(dir.path()).is_empty(),
        "PRECONDITION: no reader reads it"
    );
    let left = compact_locked(&path, NOW, utc).unwrap();
    assert!(left.rewrote && left.bytes <= BOUND_BYTES, "{left:?}");
    let rows = read_rows(dir.path());
    assert!(!rows.is_empty(), "readable again");
    assert!(
        newest_plan(&rows).is_none(),
        "the plan before the newest bytes is not read"
    );
    assert_eq!(
        rows.last().map(|r| r.t),
        Some(t - 1),
        "the newest row stays"
    );
}

/// THE HOURLY TIER KEEPS WHERE EACH WEEKLY RUN BEGINS: under the bound the raw tier shrinks to
/// three days, and a week still open has its older figures thinned to one an hour before it
/// closes; each weekly run's first figure and the one before it stay, so the close made after
/// its reset still names the refill at its instant. Negative control: keeping only each hour's
/// last figure moves the refill to that figure.
#[test]
fn the_hourly_tier_keeps_where_each_weekly_run_begins() {
    let dir = tmp("turns");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let began = NOW - 5 * D;
    let reset = began + accounts::WEEK_WINDOW_S;
    let refill = (NOW - 4 * D).div_euclid(H) * H + 20 * 60;
    let pct = |t: i64| {
        let from = if t >= refill { refill } else { began };
        u32::try_from((t - from) * 999 / accounts::WEEK_WINDOW_S).unwrap()
    };
    let mut rows = Vec::new();
    let mut t = began + 10;
    while t <= NOW {
        rows.push(fig_row(t, None, Some(fig_of(pct(t), reset, t))));
        t += 20;
    }
    write_rows(&path, &rows);
    assert!(
        std::fs::metadata(&path).unwrap().len() > BOUND_BYTES,
        "PRECONDITION: past the bound"
    );
    let first_after = rows.iter().map(|r| r.t).find(|t| *t >= refill).unwrap();
    compact_locked(&path, NOW, utc).unwrap();
    let kept = rows_at(&path);
    let thinned = kept
        .iter()
        .filter(|r| kind(r) == "fig" && NOW - r.t >= RAW_SHRUNK_S)
        .count();
    assert!(
        thinned <= 2 * 24 + 4,
        "PRECONDITION: the older figures were thinned to about one an hour: {thinned}"
    );
    let shape: Vec<(Began, i64)> = runs(kept.iter().filter_map(|r| r.fig(WEEK_LIMIT)))
        .iter()
        .map(|run| (run.began, run.figs[0].at))
        .collect();
    assert_eq!(
        shape,
        [(Began::Window, began + 10), (Began::Refill, first_after)]
    );
    let close = derive_closes(&kept, reset + CLOSE_SLACK_S);
    let (_, c) = &closes(&close, WEEK_LIMIT)[0];
    assert_eq!(c.epochs, [first_after], "the refill at its instant");
    assert_eq!(c.first, began + 10);
}

/// A TAIL THAT COULD NOT READ ITS FILE READS IT AGAIN at the next refresh, though the file did
/// not change. Negative control: taking the file's stamp before reading it leaves the tail
/// empty until the next append.
#[cfg(unix)]
#[test]
fn a_tail_that_could_not_read_its_file_reads_it_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tmp("unread");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    write_rows(
        &path,
        &[
            fig_row(NOW - H, None, Some(fig_of(10, NOW + D, NOW - H))),
            fig_row(NOW, None, Some(fig_of(20, NOW + D, NOW))),
        ],
    );
    let mode =
        |m: u32| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o000);
    let mut tail = Tail::new(path.clone());
    let _ = tail.refresh(NOW);
    assert!(tail.rows().is_empty(), "PRECONDITION: unreadable");
    mode(0o600);
    assert!(tail.refresh(NOW), "read again");
    assert_eq!(tail.rows().len(), 2);
}

/// THE BOUND GIVES ROWS UP IN ITS ORDER, to [`BOUND_ROOM_BYTES`] under it: a file past the bound
/// whose shrunk tiers hold older figures, `day` rows, five-hour closes and events loses every
/// older figure and `day` row first, then every five-hour close but the newest, then the oldest
/// events, and stops there with the newer events, every raw row and the newest plan kept; a
/// second pass writes nothing. Negative controls: giving up events before five-hour closes
/// leaves older closes; giving up events before figures leaves older figures.
#[test]
fn the_bound_gives_rows_up_in_its_order() {
    let dir = tmp("order");
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let reset = NOW + 4 * D;
    let older = NOW - RAW_SHRUNK_S;
    let mut rows = vec![plan_row(NOW - 395 * D, "20x")];
    for j in 1..=1500 {
        let t = older - j * H - 1800;
        rows.push(fig_row(t, None, Some(fig_of(5, reset, t))));
    }
    let today = NOW.div_euclid(D) * D;
    let day = |t: i64| {
        row_of(
            t,
            Said::Day(Day {
                span_s: D,
                tz_s: 0,
                five_hour: None,
                seven_day: None,
                n: 24,
                ac: None,
                key: None,
            }),
        )
    };
    rows.push(day(today - D));
    rows.extend((4..34).map(|i| day(today - i * D)));
    for i in 1..=1000 {
        let t = older - i * 6000;
        rows.push(row_of(
            t,
            Said::Close(Close {
                w: SESSION_LIMIT,
                reset: t,
                from: t - accounts::SESSION_WINDOW_S,
                peak: 500,
                end: 500,
                first: t - 4 * H,
                last: t - 600,
                n: 20,
                walled: false,
                epochs: Vec::new(),
                early: false,
                src: SRC_SERIES.into(),
                ac: None,
                key: None,
            }),
        ));
    }
    let events: Vec<SeriesRow> = (1..=2000)
        .map(|i| event_row(older - 7 - i * 3000))
        .collect();
    let size = |r: &SeriesRow| r.line().unwrap().len() as u64 + 1;
    let half_events: u64 = events.iter().map(size).sum::<u64>() / 2;
    rows.extend(events);
    let target = BOUND_BYTES - BOUND_ROOM_BYTES;
    let mut raw = size(&rows[0]) + size(&rows[1501]);
    let mut t = NOW - 2 * D;
    while raw + half_events < target {
        let row = fig_row(t, None, Some(fig_of(40, reset, t)));
        raw += size(&row);
        rows.push(row);
        t += 5;
    }
    assert!(t < NOW, "PRECONDITION: every raw row before now");
    rows.sort_by_key(|r| r.t);
    write_rows(&path, &rows);
    assert!(
        std::fs::metadata(&path).unwrap().len() > BOUND_BYTES,
        "PRECONDITION: past the bound"
    );
    let left = compact_locked(&path, NOW, utc).unwrap();
    assert!(left.bytes <= target, "{left:?}");
    let out = rows_at(&path);
    let is_older = |r: &SeriesRow| NOW - r.t >= RAW_SHRUNK_S;
    let count = |rows: &[SeriesRow], k: &str, old: bool| {
        rows.iter()
            .filter(|r| kind(r) == k && is_older(r) == old)
            .count()
    };
    assert_eq!(
        count(&out, "fig", false),
        count(&rows, "fig", false),
        "raw rows stay"
    );
    assert_eq!(count(&out, "fig", true), 0, "older figures go first");
    assert_eq!(count(&out, "day", true), 0, "older days with them");
    assert!(
        days(&out).iter().any(|(t, _)| *t == today - D),
        "the newest day stays"
    );
    let five = closes(&out, SESSION_LIMIT);
    assert_eq!(
        five.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [older - 6000],
        "then every five-hour close but the newest"
    );
    let kept: Vec<i64> = out
        .iter()
        .filter(|r| kind(r) == "event")
        .map(|r| r.t)
        .collect();
    assert!(
        !kept.is_empty() && kept.len() < 2000,
        "then the oldest events, until the room is made: {} kept",
        kept.len()
    );
    assert_eq!(
        kept.first().copied(),
        Some(older - 7 - i64::try_from(kept.len()).unwrap() * 3000),
        "the newest events stay"
    );
    assert_eq!(
        newest_plan(&out).and_then(|p| p.tier.as_deref()),
        Some("20x"),
        "the newest plan stays"
    );
    let bytes = std::fs::read(&path).unwrap();
    assert!(!compact_locked(&path, NOW + 60, utc).unwrap().rewrote);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// A `wall` and a `grant-seen` row read back as written, closed words and numbers only (`w`, a
/// bucket id; `kind`, a reset kind's word; `n`, `at`, `reset`, `use_by`), and the reader knows
/// both words. NEGATIVE CONTROL: no events append nothing and make no file.
#[test]
fn wall_and_grant_seen_rows_read_back_whole() {
    let dir = tmp("event-rows");
    assert_eq!(
        note_events(dir.path(), "me", MACHINE, &[], 100).unwrap(),
        Noted::Nothing
    );
    assert!(read(dir.path(), "me", MACHINE).is_empty());
    assert!(!dir_in(dir.path()).exists(), "no file made");
    let wall = EventSaid::wall(&accounts::WeekWalled {
        bucket: "seven_day_overage_included",
        reset: Some(5_000),
        at: 90,
        by: MACHINE.to_string(),
    });
    let grant = EventSaid::grant_seen(&accounts::Offer {
        kind: Some(aterm_phase::ResetKind::Weekly),
        left: 2,
        use_by: Some(7_000),
        at: 95,
        by: MACHINE.to_string(),
    });
    assert_eq!(
        note_events(
            dir.path(),
            "me",
            MACHINE,
            &[wall.clone(), grant.clone()],
            100
        )
        .unwrap(),
        Noted::Event
    );
    let rows = read(dir.path(), "me", MACHINE);
    assert_eq!(
        rows.iter().map(|r| r.said.clone()).collect::<Vec<_>>(),
        [Said::Event(wall.clone()), Said::Event(grant.clone())]
    );
    assert_eq!((wall.e.as_str(), grant.e.as_str()), ("wall", "grant-seen"));
    assert_eq!(EventWord::parse("wall"), Some(EventWord::Wall));
    assert_eq!(EventWord::parse("grant-seen"), Some(EventWord::GrantSeen));
    let path = path_of(dir.path(), "me", MACHINE).unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    let keys: Vec<Vec<String>> = text
        .lines()
        .map(|line| {
            let v: Value = aterm_json::from_str(line).unwrap();
            let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        })
        .collect();
    assert_eq!(
        keys,
        [
            vec!["at", "by", "e", "k", "reset", "t", "v", "w"],
            vec!["at", "by", "e", "k", "kind", "n", "t", "use_by", "v"],
        ]
    );
}

/// SINCE NEVER CROSSES ANOTHER ACCOUNT. The home's week read 40% a day and more ago; then
/// another account signed in to it (its `event` row and, in the same transaction, the first
/// reading of the new account, 20%), which read 25% and now 30%. The baseline is the new
/// account's first reading, the span shortened to its age, so the week rose 10 points; a series
/// whose last row is that event has no baseline at all, and no current figure has no `since`.
/// Negative control: taking the baseline from every row (the cut after the event left at the
/// series' start) compares the new account with the old one's 40%, reads the fall as a reset
/// used, and the first assertion fails.
#[test]
fn since_never_crosses_another_account() {
    let reset = NOW + 3 * D;
    let current = accounts::WindowFig {
        pct_x10: 300,
        reset,
        at: NOW - 60,
        by: MACHINE.into(),
        source: accounts::SOURCE_TAP.into(),
        epoch_at: None,
    };
    let rows = [
        fig_row(NOW - 30 * H, None, Some(fig_of(400, reset, NOW - 30 * H))),
        event_row(NOW - 5 * H),
        fig_row(
            NOW - 5 * H,
            None,
            Some(fig_of(200, reset, NOW - 5 * H - 30)),
        ),
        fig_row(NOW - 2 * H, None, Some(fig_of(250, reset, NOW - 2 * H))),
    ];
    let since = |rows: &[SeriesRow], current: Option<&accounts::WindowFig>| {
        since_of(
            rows,
            Keys::default(),
            (WEEK_LIMIT, accounts::WEEK_WINDOW_S),
            current,
            D,
            NOW,
        )
    };
    assert_eq!(
        since(&rows, Some(&current)),
        Some(Since {
            span_s: 5 * H + 30,
            moved: Moved::Rose(10)
        })
    );
    let mut ended = rows.to_vec();
    ended.push(event_row(NOW - H));
    assert_eq!(
        since(&ended, Some(&current)),
        None,
        "nothing after the event"
    );
    assert_eq!(since(&rows, None), None, "no current figure");
}

// A zone that springs forward at local midnight (as America/Santiago does): four hours west of
// UTC until the change, three after it, the change at 00:00 local of day `SPRING_DAY`, which so
// begins at 01:00.
const SPRING_DAY: i64 = M0 / D + 10;
const AT_MIDNIGHT: i64 = SPRING_DAY * D + 4 * H;

fn springs_at_midnight(t: i64) -> i64 {
    if t < AT_MIDNIGHT { -4 * H } else { -3 * H }
}

// A zone that falls back at local midnight (as America/Asuncion does): three hours west until
// the change, four after it, so 00:00 of `SPRING_DAY` becomes 23:00 of the day before, and the
// day begins at the second 00:00.
const FALLS_AT_MIDNIGHT: i64 = SPRING_DAY * D + 3 * H;

fn falls_at_midnight(t: i64) -> i64 {
    if t < FALLS_AT_MIDNIGHT {
        -3 * H
    } else {
        -4 * H
    }
}

/// A DAY BEGINS WHERE A ZONE SPRINGS FORWARD ACROSS ITS MIDNIGHT. Where the clocks move from
/// 00:00 to 01:00, the day's first instant is the change: `midnight` names it, the instant
/// before is still the day before, and the day's row from a reading at 01:30 is 23 hours long,
/// on its own date at the new offset. Where the clocks fall back at midnight, the day begins at
/// the second 00:00, so the day before, which holds the repeated hour, is 25 hours long.
/// Negative control: the midnight of the offset read at the
/// change (`local - tz_at(guess)` returned as it was) is 23:00 of the day before, and the first
/// assertion fails (left `AT_MIDNIGHT - 3600`).
#[test]
fn a_day_begins_where_a_zone_springs_forward_across_its_midnight() {
    assert_eq!(midnight(SPRING_DAY, springs_at_midnight), AT_MIDNIGHT);
    assert_eq!(
        local_day(AT_MIDNIGHT - 1, springs_at_midnight),
        SPRING_DAY - 1
    );
    assert_eq!(local_day(AT_MIDNIGHT, springs_at_midnight), SPRING_DAY);
    let reading = fig_row(
        AT_MIDNIGHT + 30 * 60,
        None,
        Some(fig_of(100, AT_MIDNIGHT + 3 * D, AT_MIDNIGHT + 30 * 60)),
    );
    let got = days(&day_summaries(
        &[reading],
        AT_MIDNIGHT - D,
        AT_MIDNIGHT + D,
        springs_at_midnight,
    ));
    assert_eq!(got.len(), 1, "{got:?}");
    let (t, day) = &got[0];
    assert_eq!((*t, day.span_s, day.tz_s), (AT_MIDNIGHT, D - H, -3 * H));
    assert_eq!(
        (t + day.tz_s).div_euclid(D),
        SPRING_DAY,
        "the row's own date"
    );

    let second = FALLS_AT_MIDNIGHT + H;
    assert_eq!(midnight(SPRING_DAY, falls_at_midnight), second);
    assert_eq!(local_day(second - 1, falls_at_midnight), SPRING_DAY - 1);
    assert_eq!(
        second - midnight(SPRING_DAY - 1, falls_at_midnight),
        D + H,
        "the day before holds the repeated hour"
    );
}

/// SINCE NEVER CROSSES A READING OF ANOTHER ACCOUNT. The label's record was given to another
/// account with no `another-account` event (a fleet gave the label away, or no login keyed the
/// reading): the rows of the account before carry its lab key, the new account's carry its own.
/// Its week, now 30%, is read against its own first reading (20%, five hours ago), never the
/// other account's 70% a day ago, by lab key or by account key; a reader that knows no key reads
/// all of them as before. Negative control: the parting rows left to the events alone (the
/// `Said::Fig` arm of `after_parting` false) compares 30% with 70%, reads the fall as a reset
/// used, and the first assertion fails.
#[test]
fn since_never_crosses_a_reading_of_another_account() {
    const OTHER: &str = "ac-fedcba9876543210";
    let before = profile::AccountKey::claude("11111111-1111-4111-8111-111111111111").unwrap();
    let after = profile::AccountKey::claude("22222222-2222-4222-8222-222222222222").unwrap();
    let reset = NOW + 3 * D;
    let current = accounts::WindowFig {
        pct_x10: 300,
        reset,
        at: NOW - 60,
        by: MACHINE.into(),
        source: accounts::SOURCE_TAP.into(),
        epoch_at: None,
    };
    let keyed = |t: i64, pct: u32, ac: &str, key| SeriesRow {
        t,
        by: MACHINE.into(),
        said: Said::Fig {
            five_hour: None,
            seven_day: Some(fig_of(pct, reset, t)),
            ac: Some(ac.to_owned()),
            key: Some(key),
        },
    };
    let rows = [
        keyed(NOW - 30 * H, 650, OTHER, before),
        keyed(NOW - 24 * H, 700, OTHER, before),
        keyed(NOW - 5 * H, 200, AC, after),
        keyed(NOW - 2 * H, 250, AC, after),
    ];
    let since = |keys: Keys<'_>| {
        since_of(
            &rows,
            keys,
            (WEEK_LIMIT, accounts::WEEK_WINDOW_S),
            Some(&current),
            D,
            NOW,
        )
    };
    let own = Some(Since {
        span_s: 5 * H,
        moved: Moved::Rose(10),
    });
    assert_eq!(
        since(Keys {
            ac: Some(AC),
            key: None
        }),
        own,
        "by lab key"
    );
    assert_eq!(
        since(Keys {
            ac: None,
            key: Some(after)
        }),
        own,
        "by account key"
    );
    assert!(
        matches!(
            since(Keys::default()).map(|s| s.moved),
            Some(Moved::ResetUsed(_))
        ),
        "a reader that knows no key cannot tell them"
    );
}

/// SINCE SAYS THE SPAN ITS BASELINE COVERS. A week read 30% on Tuesday at 09:00 and nothing
/// after on this Mac until 60% now, Thursday at 10:05: the rise is 30 points over the 49 hours
/// it took, never `+30 since 24h`; a baseline read within a reading heartbeat before the span's
/// start stands for the span asked. Negative control: the span asked kept for every baseline at
/// or before the start (the `span_s` it was) says 24 hours, and the first assertion fails.
#[test]
fn since_says_the_span_its_baseline_covers() {
    let reset = NOW + 3 * D;
    let current = accounts::WindowFig {
        pct_x10: 600,
        reset,
        at: NOW - 60,
        by: MACHINE.into(),
        source: accounts::SOURCE_TAP.into(),
        epoch_at: None,
    };
    let since = |rows: &[SeriesRow]| {
        since_of(
            rows,
            Keys::default(),
            (WEEK_LIMIT, accounts::WEEK_WINDOW_S),
            Some(&current),
            D,
            NOW,
        )
    };
    let gap = [fig_row(
        NOW - 49 * H,
        None,
        Some(fig_of(300, reset, NOW - 49 * H)),
    )];
    assert_eq!(
        since(&gap),
        Some(Since {
            span_s: 49 * H,
            moved: Moved::Rose(30)
        })
    );
    let at_start = NOW - D - accounts::READING_HEARTBEAT_S;
    let near = [fig_row(at_start, None, Some(fig_of(300, reset, at_start)))];
    assert_eq!(
        since(&near),
        Some(Since {
            span_s: D,
            moved: Moved::Rose(30)
        })
    );
}

/// What `since_of` says of both windows of `rows` for `keys` over `span_s` at `now`.
fn both_since(
    rows: &[SeriesRow],
    keys: Keys<'_>,
    (span_s, now): (i64, i64),
    current: (&accounts::WindowFig, &accounts::WindowFig),
) -> [Option<Since>; 2] {
    [
        since_of(
            rows,
            keys,
            (SESSION_LIMIT, accounts::SESSION_WINDOW_S),
            Some(current.0),
            span_s,
            now,
        ),
        since_of(
            rows,
            keys,
            (WEEK_LIMIT, accounts::WEEK_WINDOW_S),
            Some(current.1),
            span_s,
            now,
        ),
    ]
}

/// SINCE ROWS HOLD WHAT SINCE READS. Over a series of another account's readings, a reading no
/// key names, an event, the account's own readings (two served at one instant, one served
/// before it was written, a refill, rows of one window only), beats and plans, `since_of` over
/// the rows `since_rows` keeps says what it says over every row, for each window, each key and
/// each span and instant asked, while keeping fewer rows; so does it over a report's view, which
/// adds the outlook's rows (the readings before the event among them, but not the event).
/// Negative control: `since_rows` without the parting row (`parting.into_iter()` left out of
/// `out`) lets the report's view part at the older reading of another account, reads the
/// reading no key names as a baseline over 52 hours, and the view's assertion fails.
#[test]
fn since_rows_hold_what_since_reads() {
    const OTHER: &str = "ac-fedcba9876543210";
    let five = NOW + 2 * H;
    let week = NOW + 3 * D;
    let keyed = |t: i64, f: Option<(u32, i64)>, w: Option<(u32, i64)>, ac: &str| SeriesRow {
        t,
        by: MACHINE.into(),
        said: Said::Fig {
            five_hour: f.map(|(p, at)| fig_of(p, five, at)),
            seven_day: w.map(|(p, at)| fig_of(p, week, at)),
            ac: Some(ac.to_owned()),
            key: None,
        },
    };
    let mut rows = vec![
        plan_row(NOW - 40 * D, "5x"),
        keyed(NOW - 60 * H, None, Some((900, NOW - 60 * H)), OTHER),
        fig_row(NOW - 55 * H, None, Some(fig_of(950, week, NOW - 55 * H))),
        event_row(NOW - 50 * H),
        keyed(NOW - 50 * H, None, Some((100, NOW - 50 * H)), AC),
        keyed(
            NOW - 30 * H,
            Some((50, NOW - 30 * H)),
            Some((150, NOW - 30 * H)),
            AC,
        ),
        keyed(NOW - 26 * H, None, Some((200, NOW - 26 * H)), AC),
        keyed(NOW - 25 * H, None, Some((210, NOW - 26 * H)), AC),
        row_of(NOW - 25 * H + 60, Said::Beat),
        keyed(
            NOW - 10 * H,
            Some((400, NOW - 11 * H)),
            Some((300, NOW - 10 * H)),
            AC,
        ),
        keyed(
            NOW - 6 * H,
            Some((100, NOW - 6 * H)),
            Some((320, NOW - 6 * H)),
            AC,
        ),
        plan_row(NOW - 5 * H, "20x"),
        keyed(NOW - H, Some((150, NOW - H)), None, AC),
    ];
    rows.push(row_of(NOW - 30, Said::Beat));
    let now_fig = |pct_x10, reset| accounts::WindowFig {
        pct_x10,
        reset,
        at: NOW - 60,
        by: MACHINE.into(),
        source: accounts::SOURCE_TAP.into(),
        epoch_at: None,
    };
    let current = (now_fig(160, five), now_fig(330, week));
    let keys = [
        Keys {
            ac: Some(AC),
            key: None,
        },
        Keys::default(),
    ];
    for keys in keys {
        for span_s in [H, 5 * H, D, 2 * D, 52 * H, 10 * D] {
            for now in [NOW, NOW - 8 * H, NOW - 27 * H] {
                let all = both_since(&rows, keys, (span_s, now), (&current.0, &current.1));
                let kept = since_rows(&rows, keys, span_s, now);
                let reduced: Vec<SeriesRow> = rows
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| kept.contains(i))
                    .map(|(_, r)| r.clone())
                    .collect();
                assert_eq!(
                    both_since(&reduced, keys, (span_s, now), (&current.0, &current.1)),
                    all,
                    "{keys:?} over {span_s} at {now}: {kept:?}"
                );
                assert!(reduced.len() < rows.len(), "{kept:?}");
                let view = report_view(rows.clone(), keys, span_s, now);
                assert!(
                    outlook_view(rows.clone(), now)
                        .iter()
                        .all(|row| view.contains(row)),
                    "the view holds the outlook's"
                );
                assert_eq!(
                    both_since(&view, keys, (span_s, now), (&current.0, &current.1)),
                    all,
                    "the view: {keys:?} over {span_s} at {now}"
                );
            }
        }
    }
}
