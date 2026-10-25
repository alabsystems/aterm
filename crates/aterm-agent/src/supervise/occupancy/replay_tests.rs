// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `occupancy check --replay` tests over a synthetic F1 night.

use super::*;
use crate::harness::disk::Level;
use crate::supervise::memory::Pressure;
use crate::supervise::occupancy::hold::{Close, RowSaid};
use crate::supervise::occupancy::ring::{RingRow, RingSession};
use crate::supervise::occupancy::{OccupancyReading, SlotsSeen};

const S: i64 = 1000;
const MIN: i64 = 60 * S;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 1_792_368_000_000;

fn at(h: i64, m: i64, s: i64) -> i64 {
    let day = if h >= 18 { DAY } else { DAY + 24 * HOUR };
    day + h * HOUR + m * MIN + s * S
}

const SID: &str = "s-1b0e";

fn instants() -> [i64; 4] {
    [at(23, 57, 2), at(0, 21, 9), at(2, 11, 2), at(2, 28, 48)]
}

fn f1_ring(free_window: bool) -> Vec<RingRow> {
    let mut rows = Vec::new();
    let mut t = at(18, 51, 41);
    let runner_end = at(10, 44, 31);
    let mut free_since: Option<i64> = None;
    while t <= at(13, 30, 0) {
        let instant = instants().iter().any(|i| (*i..*i + MIN).contains(&t));
        let usable = if t >= runner_end {
            2
        } else {
            u16::from(instant)
        };
        free_since = if usable >= 1 && t >= runner_end {
            free_since.or(Some(t))
        } else {
            None
        };
        let free_ms = free_since
            .map(|since| t - since)
            .filter(|held| *held >= 20 * MIN && free_window)
            .map(|held| held as u64);
        let machine = OccupancyReading {
            taken_ms: t,
            stale: false,
            avail_mib: Some(96 << 10),
            pressure: Some(Pressure::Normal),
            disk: Some(Level::Ok),
            slots: Some(SlotsSeen { usable, n: 3 }),
            slots_free_ms: free_ms,
            room: Some(true),
            held_slot_last_ms: None,
        };
        let session = RingSession {
            sid: SID.to_string(),
            held_ms: Some(t),
            ready: false,
            guard: None,
        };
        rows.push(RingRow::of(&machine, vec![session]));
        t += MIN;
    }
    rows
}

fn f1_holds() -> Vec<(i64, RowSaid)> {
    let since = at(18, 52, 3);
    vec![
        (
            since + 10 * MIN,
            RowSaid::Open {
                held_ms: 10 * MIN,
                declared: false,
            },
        ),
        (
            at(19, 23, 30),
            RowSaid::Declared {
                held_ms: at(19, 23, 30) - since,
            },
        ),
    ]
}

fn first_would(replayed: &[Replayed], rule: RuleId) -> Option<i64> {
    replayed
        .iter()
        .find(|r| r.rule == rule && r.verdict == SaidVerdict::Would)
        .map(|r| r.at_ms)
}

fn woulds(replayed: &[Replayed], rule: RuleId) -> Vec<i64> {
    replayed
        .iter()
        .filter(|r| r.rule == rule && r.verdict == SaidVerdict::Would)
        .map(|r| r.at_ms)
        .collect()
}

#[test]
fn the_f1_night_replays_the_slice_one_times() {
    let rows = f1_ring(true);
    let spans = spans_of(&f1_holds());
    assert_eq!(
        spans,
        vec![HoldSpan {
            since_ms: at(18, 52, 3),
            declared_ms: Some(at(19, 23, 30)),
            end_ms: None,
        }]
    );
    let replayed = replay(&rows, &spans, SID, rows[0].at);
    let nudge = woulds(&replayed, RuleId::WaitNudge);
    assert_eq!(nudge.len(), 1, "{replayed:#?}");
    assert!(
        (at(19, 22, 3)..at(19, 23, 3)).contains(&nudge[0]),
        "{replayed:#?}"
    );
    let room = woulds(&replayed, RuleId::HoldRoom);
    let first = room.first().copied().expect("hold-room fires");
    assert!(first >= at(10, 44, 31) + 20 * MIN, "{replayed:#?}");
    assert!(first <= at(11, 6, 0), "{replayed:#?}");
    assert_eq!(room.len(), 2, "{replayed:#?}");
    assert!(
        room[1] - room[0] >= rules::HOLD_ROOM_APART_MS,
        "{replayed:#?}"
    );
    let lines = replay_lines(
        &replayed,
        rows.len(),
        spans.len(),
        (rows[0].at, at(13, 30, 0)),
        0,
    );
    assert!(
        lines[0].contains(&format!("looks={}", rows.len())),
        "{lines:#?}"
    );
    let tail = &lines[lines.len() - 3..];
    assert!(
        tail[0].starts_with("wait-nudge@v1 first would 19:2"),
        "{lines:#?}"
    );
    assert!(
        tail[1].starts_with("hold-room@v1 first would 11:0"),
        "{lines:#?}"
    );
    assert!(
        tail[2].starts_with("wake@v1 ") && !tail[2].contains("would"),
        "the wake's line names no would: {lines:#?}"
    );
    assert!(
        !replayed.iter().any(|r| r.rule == RuleId::Wake),
        "{replayed:#?}"
    );
}

#[test]
fn a_ring_with_no_free_window_gives_no_hold_room() {
    let rows = f1_ring(false);
    let spans = spans_of(&f1_holds());
    let replayed = replay(&rows, &spans, SID, rows[0].at);
    assert_eq!(
        first_would(&replayed, RuleId::HoldRoom),
        None,
        "{replayed:#?}"
    );
    assert!(first_would(&replayed, RuleId::WaitNudge).is_some());
    let lines = replay_lines(
        &replayed,
        rows.len(),
        spans.len(),
        (rows[0].at, at(13, 30, 0)),
        0,
    );
    assert_eq!(
        lines.get(lines.len() - 2).map(String::as_str),
        Some("hold-room@v1 first would never")
    );
}

#[test]
fn a_told_guard_holds_the_replayed_line() {
    let mut rows = f1_ring(true);
    let person_until = at(19, 40, 0);
    for row in rows.iter_mut().filter(|r| r.at < person_until) {
        row.sessions[0].guard = Some(Guard::Person.word().to_string());
    }
    let undeclared = &f1_holds()[..1];
    let spans = spans_of(undeclared);
    let replayed = replay(&rows, &spans, SID, rows[0].at);
    let held = replayed
        .iter()
        .find(|r| r.rule == RuleId::WaitNudge)
        .expect("the nudge said");
    assert_eq!(
        held.verdict,
        SaidVerdict::Held(Guard::Person),
        "{replayed:#?}"
    );
    assert!(held.at_ms < person_until, "{replayed:#?}");
    let nudge = first_would(&replayed, RuleId::WaitNudge).expect("then it goes");
    assert!(
        (person_until..person_until + MIN).contains(&nudge),
        "{replayed:#?}"
    );
}

#[test]
fn the_ledger_rows_make_the_holds() {
    let t = at(20, 0, 0);
    let rows = vec![
        (
            t - 2 * HOUR,
            RowSaid::Closed {
                why: Close::Point,
                held_ms: HOUR,
            },
        ),
        (
            t,
            RowSaid::Open {
                held_ms: 10 * MIN,
                declared: false,
            },
        ),
        (
            t + 20 * MIN,
            RowSaid::Open {
                held_ms: 30 * MIN,
                declared: true,
            },
        ),
        (
            t + 2 * HOUR,
            RowSaid::Open {
                held_ms: 10 * MIN,
                declared: false,
            },
        ),
        (
            t + 3 * HOUR,
            RowSaid::Closed {
                why: Close::Foreign,
                held_ms: HOUR + 10 * MIN,
            },
        ),
    ];
    assert_eq!(
        spans_of(&rows),
        vec![
            HoldSpan {
                since_ms: t - 3 * HOUR,
                declared_ms: None,
                end_ms: Some(t - 2 * HOUR),
            },
            HoldSpan {
                since_ms: t - 10 * MIN,
                declared_ms: Some(t + 20 * MIN),
                end_ms: Some(t + 2 * HOUR - 10 * MIN),
            },
            HoldSpan {
                since_ms: t + 2 * HOUR - 10 * MIN,
                declared_ms: None,
                end_ms: Some(t + 3 * HOUR),
            },
        ]
    );
}

#[test]
fn the_window_takes_a_span() {
    for (word, ms) in [
        ("24h", Some(24 * HOUR)),
        ("90m", Some(90 * MIN)),
        ("30s", Some(30 * S)),
        ("1d", Some(24 * HOUR)),
        ("0h", None),
        ("3d", None),
        ("24", None),
        ("h", None),
        ("-1h", None),
        ("2w", None),
    ] {
        assert_eq!(window_ms(word), ms, "{word}");
    }
}
