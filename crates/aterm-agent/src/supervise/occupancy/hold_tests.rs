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

/// THE 10-19 DAY: 24 short idle windows inside 10 h, each a new break (the loop starts a
/// break's settle over after every read that is none), and the hold's `since` never moves, so it
/// reaches 2 h; a point that decides nothing while the work still shows keeps it too (F5).
/// NEGATIVE CONTROL: the same windows each ended by a decided point that finds no background work
/// close and reopen the hold, and it never reaches 2 h — the assertion sees a restarted clock.
#[test]
fn short_idle_reads_never_restart_the_hold() {
    // Window `i` starts 25 min apart; each is a break read unsettled, then settled.
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
            // The short idle window that ends it.
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
    // The control.
    let mut c = HoldClock::default();
    assert!(
        windows(&mut c, Some(point(false, false))) < 2 * HOUR,
        "a clock restarted at each window never reaches 2 h"
    );
}

/// A DECIDED POINT closes the hold where it acts or finds no background work, and keeps it where
/// it decides nothing while the work still shows (F5).
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

/// A LIVE TURN THE HARNESS DID NOT TYPE ends the hold, at its edge; the harness's own line, its
/// live turn and the point that ends its answer neither end it nor restart it. Only the edge is
/// classified: a later read of the same turn is no new turn. NEGATIVE CONTROL: with a break read
/// between, the same foreign read is an edge and closes it.
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
    // The harness's line, answered with an idle point.
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
    // The harness's line, answered with a break.
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
    // Edge only.
    let mut c = open();
    assert_eq!(c.step(live(true)), None, "an act's live turn");
    assert_eq!(
        c.step(live(false)),
        None,
        "the same turn read again: no edge"
    );
    assert_eq!(c.since_ms(), Some(0));
    // The control: a break between, so the foreign read is an edge.
    c.step(settled(9 * MIN, 10 * MIN));
    assert!(matches!(
        c.step(live(false)),
        Some(HoldChange::Closed {
            why: Close::Foreign,
            ..
        })
    ));
    // A closed hold opens afresh at the next settled break, at that break's start.
    assert_eq!(
        c.step(settled(11 * MIN, 12 * MIN)),
        Some(HoldChange::Opened {
            since_ms: 11 * MIN,
            seeded: false,
            declared: false,
        })
    );
}

/// `WAITING:` AT A LINE'S START declares the hold, read at the first break after the harness's
/// line, and nothing of the words is kept. NEGATIVE CONTROLS: `WAITING` without its colon, a
/// mid-line `WAITING:`, another case, and a tail read with no harness line typed declare nothing.
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
    // Read before the answer's turn ran: the line may still be answered.
    let mut c = open();
    c.step(HoldEvent::HarnessLine);
    c.step(HoldEvent::AnswerTail {
        waiting_line: false,
    });
    assert!(c.wants_tail(), "still owed");
    // The control: no line typed, no tail asked, and a tail given declares nothing.
    let mut c = open();
    assert_eq!(c.step(HoldEvent::AnswerTail { waiting_line: true }), None);
    assert!(!c.declared());
    // A new hold starts undeclared.
    let mut c = open();
    c.step(HoldEvent::HarnessLine);
    c.step(HoldEvent::AnswerTail { waiting_line: true });
    c.step(live(false));
    c.step(settled(20 * MIN, 21 * MIN));
    assert!(!c.declared());
}

/// A SEED carries a hold only into a clock with none open, declared as its row said.
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

/// THE LEDGER'S OPEN HOLD is the session's last `fact-hold@v1` row where it says open or
/// declared, with its stamp and how long it had held; a close after it leaves none. Rows of
/// another session or of another fact's rule, and a reason this build never wrote, are passed
/// over; a row of the session's under any other rule after it marks it FOLLOWED (no last word
/// of the loop that watched it, the red-team's F3), and a hold row after that one reads afresh.
/// NEGATIVE CONTROL: every reason [`row_reason`] writes reads back.
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
    // A row of the session's under another rule (a continuation typed, a point recorded)
    // follows the open hold: followed. One before it, or another session's, does not.
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
