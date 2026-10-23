// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The hold's clock, by tables of what the loop sees.

use super::*;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

fn settled(since_ms: i64, now_ms: i64) -> HoldEvent {
    HoldEvent::Background {
        since_ms,
        now_ms,
        settled: true,
    }
}

fn point(acts: bool, background: bool) -> HoldEvent {
    HoldEvent::DecidedPoint { acts, background }
}

fn live(ours: bool) -> HoldEvent {
    HoldEvent::LiveTurn { ours }
}

/// A clock with a hold open since 0.
fn open() -> HoldClock {
    let mut c = HoldClock::default();
    assert_eq!(
        c.step(settled(0, 30_000)),
        Some(HoldChange::Opened {
            since_ms: 0,
            seeded: false,
            declared: false,
        })
    );
    c
}

#[test]
fn short_idle_reads_never_restart_the_hold() {
    let windows = |c: &mut HoldClock, between: Option<HoldEvent>| -> i64 {
        let mut longest = 0;
        for i in 0..24_i64 {
            let start = i * 25 * MIN;
            c.step(HoldEvent::Background {
                since_ms: start,
                now_ms: start,
                settled: false,
            });
            c.step(settled(start, start + 20_000));
            c.step(settled(start, start + 20 * MIN));
            longest = longest.max(c.held_ms(start + 20 * MIN).unwrap_or(0));
            if let Some(ev) = between {
                c.step(ev);
            }
        }
        longest
    };
    let mut c = HoldClock::default();
    assert!(windows(&mut c, None) >= 9 * HOUR);
    assert_eq!(c.since_ms(), Some(0), "no idle read moved it");
    let mut c = HoldClock::default();
    assert!(windows(&mut c, Some(point(false, true))) >= 9 * HOUR);
    assert_eq!(c.since_ms(), Some(0), "nothing decided over the work: kept");
    let mut c = HoldClock::default();
    assert!(
        windows(&mut c, Some(point(false, false))) < 2 * HOUR,
        "a clock restarted at each window never reaches 2 h"
    );
}

#[test]
fn a_point_closes_the_hold_unless_it_decides_nothing_over_the_work() {
    for (acts, background, closes) in [
        (false, true, false),
        (false, false, true),
        (true, true, true),
        (true, false, true),
    ] {
        let mut c = open();
        c.step(settled(0, 40 * MIN));
        let change = c.step(point(acts, background));
        let want = closes.then_some(HoldChange::Closed {
            why: Close::Point,
            held_ms: 40 * MIN,
        });
        assert_eq!(change, want, "acts={acts} background={background}");
        assert_eq!(c.since_ms().is_some(), !closes);
    }
}

#[test]
fn only_a_foreign_live_edge_or_a_later_point_ends_the_hold() {
    let mut c = open();
    assert_eq!(
        c.step(live(false)),
        Some(HoldChange::Closed {
            why: Close::Foreign,
            held_ms: 30_000,
        })
    );
    let mut c = open();
    for ev in [HoldEvent::HarnessLine, live(true), point(true, false)] {
        assert_eq!(c.step(ev), None, "{ev:?}");
    }
    assert_eq!(c.since_ms(), Some(0), "the answer's point keeps it");
    assert!(matches!(
        c.step(point(true, false)),
        Some(HoldChange::Closed {
            why: Close::Point,
            ..
        })
    ));
    let mut c = open();
    for ev in [
        HoldEvent::HarnessLine,
        live(true),
        settled(5 * MIN, 6 * MIN),
        HoldEvent::AnswerTail {
            waiting_line: false,
        },
    ] {
        assert_eq!(c.step(ev), None, "{ev:?}");
    }
    assert_eq!(c.since_ms(), Some(0), "the answer's break restarts nothing");
    let mut c = open();
    assert_eq!(c.step(live(true)), None, "an act's live turn");
    assert_eq!(
        c.step(live(false)),
        None,
        "the same turn read again: no edge"
    );
    assert_eq!(c.since_ms(), Some(0));
    c.step(settled(9 * MIN, 10 * MIN));
    assert!(matches!(
        c.step(live(false)),
        Some(HoldChange::Closed {
            why: Close::Foreign,
            ..
        })
    ));
    assert_eq!(
        c.step(settled(11 * MIN, 12 * MIN)),
        Some(HoldChange::Opened {
            since_ms: 11 * MIN,
            seeded: false,
            declared: false,
        })
    );
}

#[test]
fn a_waiting_line_declares_the_hold_and_no_words_are_kept() {
    for (tail, declares) in [
        ("WAITING: the mint, about 6 h", true),
        ("  WAITING: the mint", true),
        ("Started it.\nWAITING: build 2 of 3", true),
        ("WAITING on the mint", false),
        ("I am WAITING: on the mint", false),
        ("Waiting: the mint", false),
        ("", false),
    ] {
        assert_eq!(waiting_declared(tail), declares, "{tail:?}");
    }
    let mut c = open();
    assert!(!c.wants_tail());
    c.step(HoldEvent::HarnessLine);
    c.step(live(true));
    c.step(settled(5 * MIN, 7 * MIN));
    assert!(c.wants_tail());
    assert_eq!(
        c.step(HoldEvent::AnswerTail { waiting_line: true }),
        Some(HoldChange::Declared { held_ms: 7 * MIN })
    );
    assert!(c.declared() && !c.wants_tail());
    assert!(!format!("{c:?}").contains("mint"), "{c:?}");
    let mut c = open();
    c.step(HoldEvent::HarnessLine);
    c.step(HoldEvent::AnswerTail {
        waiting_line: false,
    });
    assert!(c.wants_tail(), "still owed");
    let mut c = open();
    assert_eq!(c.step(HoldEvent::AnswerTail { waiting_line: true }), None);
    assert!(!c.declared());
    let mut c = open();
    c.step(HoldEvent::HarnessLine);
    c.step(HoldEvent::AnswerTail { waiting_line: true });
    c.step(live(false));
    c.step(settled(20 * MIN, 21 * MIN));
    assert!(!c.declared());
}

#[test]
fn a_seed_opens_only_a_clock_with_no_hold() {
    let mut c = HoldClock::default();
    assert_eq!(
        c.seed(-HOUR, true, 0),
        Some(HoldChange::Opened {
            since_ms: -HOUR,
            seeded: true,
            declared: true,
        })
    );
    assert_eq!(c.held_ms(0), Some(HOUR));
    assert!(c.declared());
    assert_eq!(c.seed(0, false, 0), None, "one open already");
    assert_eq!(c.since_ms(), Some(-HOUR));
}

fn row(ts: i64, sid: &str, rule: &str, reason: &str) -> String {
    format!(
        "{{\"ts\":{ts},\"sid\":\"{sid}\",\"rule_id\":\"{rule}\",\"decision\":\"waiting\",\
         \"cmd_sha256\":\"\",\"command\":\"\",\"reason\":\"{reason}\",\"box_seq\":7}}"
    )
}

#[test]
fn the_ledgers_open_hold_is_its_last_hold_row() {
    let rule = crate::supervise::facts::FactKind::Hold.rule();
    for said in [
        RowSaid::Open {
            held_ms: 600_000,
            declared: false,
        },
        RowSaid::Open {
            held_ms: 600_000,
            declared: true,
        },
        RowSaid::Declared { held_ms: 700_000 },
        RowSaid::Closed {
            why: Close::Foreign,
            held_ms: 1,
        },
        RowSaid::Closed {
            why: Close::Stale,
            held_ms: 0,
        },
    ] {
        assert_eq!(row_said(&row_reason(&said)), Some(said), "{said:?}");
    }
    let open = row(
        1_000_000,
        "s-1",
        rule,
        &row_reason(&RowSaid::Open {
            held_ms: 600_000,
            declared: false,
        }),
    );
    let declared = row(
        1_100_000,
        "s-1",
        rule,
        &row_reason(&RowSaid::Declared { held_ms: 700_000 }),
    );
    let closed = row(
        1_200_000,
        "s-1",
        rule,
        &row_reason(&RowSaid::Closed {
            why: Close::Point,
            held_ms: 800_000,
        }),
    );
    let seed = |rows: &[&str]| seed_row_in(&rows.join("\n"), Some("@s-1"));
    assert_eq!(
        seed(&[&open]),
        Some(SeedRow {
            row_ms: 1_000_000,
            held_ms: 600_000,
            declared: false,
            followed: false,
        })
    );
    assert_eq!(seed(&[&open]).map(|s| s.since_wall_ms()), Some(400_000));
    assert_eq!(
        seed(&[&open, &declared]),
        Some(SeedRow {
            row_ms: 1_100_000,
            held_ms: 700_000,
            declared: true,
            followed: false,
        })
    );
    assert_eq!(seed(&[&open, &declared, &closed]), None);
    let other_sid = row(1_300_000, "s-2", rule, "hold=closed why=point held_ms=1");
    let other_rule = row(
        1_300_000,
        "s-1",
        "fact-wake@v1",
        "hold=closed why=point held_ms=1",
    );
    let unread = row(1_300_000, "s-1", rule, "hold=closed why=sometime held_ms=1");
    assert_eq!(
        seed(&[&open, &other_sid, &other_rule, &unread]),
        seed(&[&open]),
        "passed over"
    );
    let typed = row(1_150_000, "s-1", "continue@v1", "-");
    let followed = |rows: &[&str]| seed(rows).map(|s| s.followed);
    assert_eq!(followed(&[&open, &typed]), Some(true));
    assert_eq!(followed(&[&open, &declared, &typed]), Some(true));
    assert_eq!(
        followed(&[&open, &typed, &declared]),
        Some(false),
        "booked again after it"
    );
    assert_eq!(followed(&[&typed, &open]), Some(false));
    let elsewhere = row(1_150_000, "s-2", "continue@v1", "-");
    assert_eq!(followed(&[&open, &elsewhere]), Some(false));
    assert_eq!(seed(&[&open, &typed, &closed]), None);
    assert_eq!(seed(&[]), None);
}
