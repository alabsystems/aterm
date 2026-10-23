// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the built-in occupancy rules decider.

use super::super::reading::SlotsSeen;
use super::*;
use crate::supervise::memory::Pressure;

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

const T0: i64 = 1_760_000_000_000;

type Unset = fn(&mut OccupancyReading);

type SetGuard = fn(&mut Guards);

fn hold() -> Option<HoldSeen> {
    Some(HoldSeen {
        since_ms: 0,
        declared: false,
    })
}

fn room() -> OccupancyReading {
    OccupancyReading {
        taken_ms: T0,
        stale: false,
        avail_mib: Some(96 * 1024),
        pressure: Some(Pressure::Normal),
        disk: Some(Level::Ok),
        slots: Some(SlotsSeen { usable: 1, n: 3 }),
        slots_free_ms: Some(20 * 60_000),
        room: Some(true),
        held_slot_last_ms: Some(T0 - MIN),
    }
}

fn verdict(
    rule: RuleId,
    r: &OccupancyReading,
    hold: Option<HoldSeen>,
    books: &Books,
    g: &Guards,
    now_ms: i64,
) -> Verdict {
    let mut books = books.clone();
    if let Some(h) = hold {
        books.hold(h.since_ms);
    }
    decide(r, hold, &books, g, WAIT_NUDGE_AFTER_MS, now_ms)[rule.index()]
        .1
        .clone()
}

fn cruise() -> Guards {
    Guards::clear(Autopilot::Cruise)
}

fn is_due(v: &Verdict, rule: RuleId) -> bool {
    matches!(v, Verdict::Due(line) if line.rule() == rule)
}

fn nudged_at(at_ms: i64) -> Books {
    let mut b = Books::default();
    b.hold(0);
    b.fired(RuleId::WaitNudge, at_ms, true);
    b
}

#[test]
fn the_nudge_is_due_at_thirty_minutes_not_before() {
    let b = Books::default();
    let r = OccupancyReading::default();
    let at = |ms| verdict(RuleId::WaitNudge, &r, hold(), &b, &cruise(), ms);
    assert_eq!(at(WAIT_NUDGE_AFTER_MS - 1), Verdict::Quiet);
    assert!(is_due(&at(WAIT_NUDGE_AFTER_MS), RuleId::WaitNudge));
    assert_eq!(
        verdict(RuleId::WaitNudge, &r, None, &b, &cruise(), 10 * HOUR),
        Verdict::Quiet,
        "no hold, no nudge"
    );
}

#[test]
fn a_declared_hold_gets_no_nudge_but_hold_room_still_goes() {
    let declared = Some(HoldSeen {
        since_ms: 0,
        declared: true,
    });
    let b = Books::default();
    let v = decide(
        &room(),
        declared,
        &b,
        &cruise(),
        WAIT_NUDGE_AFTER_MS,
        HOLD_ROOM_AFTER_MS,
    );
    assert_eq!(v[RuleId::WaitNudge.index()].1, Verdict::Quiet);
    assert!(is_due(&v[RuleId::HoldRoom.index()].1, RuleId::HoldRoom));
    let v = decide(
        &room(),
        hold(),
        &b,
        &cruise(),
        WAIT_NUDGE_AFTER_MS,
        WAIT_NUDGE_AFTER_MS,
    );
    assert!(is_due(&v[RuleId::WaitNudge.index()].1, RuleId::WaitNudge));
}

#[test]
fn the_nudge_goes_once_a_hold() {
    let r = OccupancyReading::default();
    let b = nudged_at(WAIT_NUDGE_AFTER_MS);
    assert_eq!(
        verdict(RuleId::WaitNudge, &r, hold(), &b, &cruise(), 5 * HOUR),
        Verdict::Quiet
    );
    let next = Some(HoldSeen {
        since_ms: 6 * HOUR,
        declared: false,
    });
    assert!(is_due(
        &verdict(RuleId::WaitNudge, &r, next, &b, &cruise(), 7 * HOUR),
        RuleId::WaitNudge
    ));
}

#[test]
fn three_nudges_a_day_then_spent() {
    let r = OccupancyReading::default();
    let mut b = Books::default();
    for k in 0..3 {
        let since = k * 2 * HOUR;
        b.hold(since);
        b.fired(RuleId::WaitNudge, since + WAIT_NUDGE_AFTER_MS, true);
    }
    let fourth = |since: i64| {
        Some(HoldSeen {
            since_ms: since,
            declared: false,
        })
    };
    assert_eq!(
        verdict(
            RuleId::WaitNudge,
            &r,
            fourth(6 * HOUR),
            &b,
            &cruise(),
            7 * HOUR
        ),
        Verdict::Spent
    );
    let late = DAY_MS + WAIT_NUDGE_AFTER_MS;
    assert!(is_due(
        &verdict(
            RuleId::WaitNudge,
            &r,
            fourth(late - WAIT_NUDGE_AFTER_MS),
            &b,
            &cruise(),
            late
        ),
        RuleId::WaitNudge
    ));
}

#[test]
fn hold_room_is_due_at_two_hours_on_room() {
    let b = nudged_at(WAIT_NUDGE_AFTER_MS);
    let at = |ms| verdict(RuleId::HoldRoom, &room(), hold(), &b, &cruise(), ms);
    assert_eq!(at(HOLD_ROOM_AFTER_MS - 1), Verdict::Quiet);
    assert!(is_due(&at(HOLD_ROOM_AFTER_MS), RuleId::HoldRoom));
}

#[test]
fn hold_room_goes_two_hours_apart_four_a_hold() {
    let mut b = nudged_at(WAIT_NUDGE_AFTER_MS);
    b.fired(RuleId::HoldRoom, 2 * HOUR, true);
    let at = |b: &Books, ms| verdict(RuleId::HoldRoom, &room(), hold(), b, &cruise(), ms);
    assert_eq!(at(&b, 4 * HOUR - 1), Verdict::Quiet);
    assert!(is_due(&at(&b, 4 * HOUR), RuleId::HoldRoom));
    for k in 2..=4 {
        b.fired(RuleId::HoldRoom, 2 * k * HOUR, true);
    }
    assert_eq!(b.this_hold(RuleId::HoldRoom), 4);
    assert_eq!(at(&b, 20 * HOUR), Verdict::Spent);
    let next = Some(HoldSeen {
        since_ms: 21 * HOUR,
        declared: true,
    });
    assert!(is_due(
        &verdict(RuleId::HoldRoom, &room(), next, &b, &cruise(), 23 * HOUR),
        RuleId::HoldRoom
    ));
}

#[test]
fn every_term_of_hold_room_is_needed() {
    let b = nudged_at(WAIT_NUDGE_AFTER_MS);
    let at = |r: &OccupancyReading| {
        verdict(
            RuleId::HoldRoom,
            r,
            hold(),
            &b,
            &cruise(),
            HOLD_ROOM_AFTER_MS,
        )
    };
    assert!(is_due(&at(&room()), RuleId::HoldRoom), "the control");
    let unmet: [(&str, Unset); 12] = [
        ("empty", |r| *r = OccupancyReading::default()),
        ("stale", |r| r.stale = true),
        ("no slot latch", |r| r.slots_free_ms = None),
        ("machine unmet", |r| r.room = Some(false)),
        ("machine unjudged", |r| r.room = None),
        ("disk low", |r| r.disk = Some(Level::Low)),
        ("disk unjudged", |r| r.disk = None),
        ("never held a slot", |r| r.held_slot_last_ms = None),
        ("held before the hold", |r| {
            r.held_slot_last_ms = Some(T0 - HOLD_ROOM_AFTER_MS - 1);
        }),
        ("no slots read", |r| r.slots = None),
        ("no slot usable", |r| {
            r.slots = Some(SlotsSeen { usable: 0, n: 3 })
        }),
        ("no memory read", |r| r.avail_mib = None),
    ];
    for (why, unset) in unmet {
        let mut r = room();
        unset(&mut r);
        assert_eq!(at(&r), Verdict::Quiet, "{why}");
    }
}

#[test]
fn each_guard_holds_a_due_line() {
    let r = OccupancyReading::default();
    let b = Books::default();
    let now = WAIT_NUDGE_AFTER_MS;
    let at = |g: Guards| verdict(RuleId::WaitNudge, &r, hold(), &b, &g, now);
    assert!(is_due(&at(cruise()), RuleId::WaitNudge));
    let cases: [(Guard, SetGuard); 17] = [
        (Guard::Stall, |g| g.brake = Some(Guard::Stall)),
        (Guard::LimitEpisode, |g| g.brake = Some(Guard::LimitEpisode)),
        (Guard::ActUntaken, |g| g.brake = Some(Guard::ActUntaken)),
        (Guard::WakeSettle, |g| g.brake = Some(Guard::WakeSettle)),
        (Guard::Credits, |g| g.brake = Some(Guard::Credits)),
        (Guard::Person, |g| g.person = true),
        (Guard::Halted, |g| g.halted = true),
        (Guard::Draft, |g| g.draft = true),
        (Guard::Taskless, |g| g.taskless = true),
        (Guard::Exchange, |g| g.exchange = true),
        (Guard::Managed, |g| g.managed = true),
        (Guard::Memory, |g| g.memory = true),
        (Guard::Reach, |g| g.reach_down = true),
        (Guard::Autopilot, |g| g.mode = Autopilot::Land),
        (Guard::Autopilot, |g| g.mode = Autopilot::Off),
        (Guard::Stall, |g| {
            g.person = true;
            g.brake = Some(Guard::Stall);
        }),
        (Guard::Draft, |g| {
            g.taskless = true;
            g.draft = true;
        }),
    ];
    for (want, set) in cases {
        let mut g = cruise();
        set(&mut g);
        assert_eq!(at(g), Verdict::Held(want), "{g:?}");
    }
}

#[test]
fn a_spent_cap_is_said_before_a_guard() {
    let mut b = nudged_at(WAIT_NUDGE_AFTER_MS);
    for k in 1..=i64::try_from(HOLD_ROOM_PER_HOLD).expect("a small cap") {
        b.fired(RuleId::HoldRoom, k * 2 * HOUR, true);
    }
    let mut g = cruise();
    g.person = true;
    assert_eq!(
        verdict(RuleId::HoldRoom, &room(), hold(), &b, &g, 20 * HOUR),
        Verdict::Spent
    );
    assert_eq!(
        verdict(
            RuleId::HoldRoom,
            &OccupancyReading::default(),
            hold(),
            &b,
            &cruise(),
            20 * HOUR
        ),
        Verdict::Spent
    );
}

#[test]
fn the_session_pace_holds_a_second_line() {
    let b = nudged_at(HOUR + 50 * MIN);
    let at = |ms| verdict(RuleId::HoldRoom, &room(), hold(), &b, &cruise(), ms);
    assert_eq!(
        at(2 * HOUR),
        Verdict::Paced {
            next_ms: 2 * HOUR + 10 * MIN
        }
    );
    assert!(is_due(&at(2 * HOUR + 10 * MIN), RuleId::HoldRoom));
    let v = decide(
        &room(),
        hold(),
        &Books::default(),
        &cruise(),
        WAIT_NUDGE_AFTER_MS,
        2 * HOUR,
    );
    assert!(is_due(&v[RuleId::WaitNudge.index()].1, RuleId::WaitNudge));
    assert_eq!(
        v[RuleId::HoldRoom.index()].1,
        Verdict::Paced {
            next_ms: 2 * HOUR + SESSION_PACE_MS
        }
    );
}

#[test]
fn the_reading_is_wanted_only_where_hold_room_could_be_due() {
    let mut b = Books::default();
    b.hold(0);
    assert!(!wants_reading(hold(), &b, HOLD_ROOM_AFTER_MS - 1));
    assert!(wants_reading(hold(), &b, HOLD_ROOM_AFTER_MS));
    assert!(!wants_reading(None, &b, 10 * HOUR));
    b.fired(RuleId::HoldRoom, 2 * HOUR, true);
    assert!(!wants_reading(hold(), &b, 3 * HOUR));
    assert!(wants_reading(hold(), &b, 4 * HOUR));
    for k in 2..=4 {
        b.fired(RuleId::HoldRoom, 2 * k * HOUR, true);
    }
    assert!(
        !wants_reading(hold(), &b, 20 * HOUR),
        "past the cap: nothing the board says can let it go"
    );
}

#[test]
fn would_rows_read_back_for_this_session() {
    let row =
        |sid: &str, rule: &str, ts: i64, reason: &str| row_said(sid, rule, ts, "waiting", reason);
    let nudge = would_reason(RuleId::WaitNudge, 7, 30 * MIN, false);
    let room_said = would_reason(RuleId::HoldRoom, 9, 2 * HOUR, true);
    let body = [
        row("s-1", WOULD_RULE, T0, &nudge),
        row("s-2", WOULD_RULE, T0 + 1, &nudge),
        row("s-1", "fact-hold@v1", T0 + 2, "hold=open held_ms=600000"),
        row("s-1", WOULD_RULE, T0 + 3, "rule=later-rule@v9 held_ms=1"),
        row("s-1", WOULD_RULE, T0 + 4, &room_said),
        row_said("s-1", "fact-hold@v1", T0 + 5, "skipped", &room_said),
        row_said("s-1", "fact-hold@v1", T0 + 6, "typed", &room_said),
    ]
    .join("\n");
    assert_eq!(
        would_rows_in(&body, Some("@s-1")),
        [
            WouldRow {
                rule: RuleId::WaitNudge,
                row_ms: T0,
                held_ms: 30 * MIN,
            },
            WouldRow {
                rule: RuleId::HoldRoom,
                row_ms: T0 + 4,
                held_ms: 2 * HOUR,
            },
            WouldRow {
                rule: RuleId::HoldRoom,
                row_ms: T0 + 6,
                held_ms: 2 * HOUR,
            },
        ]
    );
    assert_eq!(would_rows_in(&body, Some("s-3")), []);
}

fn row_said(sid: &str, rule: &str, ts: i64, decision: &str, reason: &str) -> String {
    format!(
        r#"{{"ts":{ts},"sid":"{sid}","rule_id":"{rule}","decision":"{decision}","reason":"{reason}"}}"#
    )
}

#[test]
fn an_episode_is_its_rule_hold_and_ordinal() {
    let id = episode(RuleId::WaitNudge, T0, 0);
    assert_eq!(id, episode(RuleId::WaitNudge, T0, 0));
    for other in [
        episode(RuleId::HoldRoom, T0, 0),
        episode(RuleId::WaitNudge, T0 + 1, 0),
        episode(RuleId::WaitNudge, T0, 1),
    ] {
        assert_ne!(id, other);
    }
}

#[test]
fn rule_names_read_back() {
    for rule in RuleId::ALL {
        assert_eq!(RuleId::of_name(rule.name()), Some(rule));
        assert_eq!(RuleId::ALL[rule.index()], rule);
    }
    assert_eq!(RuleId::of_name("wait-nudge@v2"), None);
}

/// The lab's seam (`ATERM_OCCUPANCY_NUDGE_S`) shortens the nudge's wait to its whole seconds and
/// never lengthens it. NEGATIVE CONTROL: no value, a word, a unit, zero, a negative count and one
/// past the shipped wait all leave the shipped 30 min.
#[test]
fn the_nudge_seam_only_shortens_the_wait() {
    let of = |v: &str| nudge_after_ms_of(Some(std::ffi::OsStr::new(v)));
    assert_eq!(of("60"), MIN);
    assert_eq!(of(" 5 "), 5_000);
    assert_eq!(of("1800"), WAIT_NUDGE_AFTER_MS);
    assert_eq!(nudge_after_ms_of(None), WAIT_NUDGE_AFTER_MS);
    for shipped in ["", "soon", "60s", "0", "-60", "1801", "86400"] {
        assert_eq!(of(shipped), WAIT_NUDGE_AFTER_MS, "{shipped:?}");
    }
}

/// THE WAIT IS THE CALLER'S: a session set up with a 1 min wait (the seam's, carried in its
/// setup) has the nudge due at 1 min and quiet a millisecond before, and `hold-room` keeps its
/// own 2 h. NEGATIVE CONTROL: the shipped wait at the same 1 min is quiet.
#[test]
fn the_setups_wait_is_the_nudges() {
    let r = room();
    let at = |after_ms, now_ms| {
        let mut b = Books::default();
        b.hold(0);
        decide(&r, hold(), &b, &cruise(), after_ms, now_ms)
    };
    assert_eq!(
        at(MIN, MIN - 1)[RuleId::WaitNudge.index()].1,
        Verdict::Quiet
    );
    let due = at(MIN, MIN);
    assert!(is_due(&due[RuleId::WaitNudge.index()].1, RuleId::WaitNudge));
    assert_eq!(due[RuleId::HoldRoom.index()].1, Verdict::Quiet);
    assert_eq!(
        at(WAIT_NUDGE_AFTER_MS, MIN)[RuleId::WaitNudge.index()].1,
        Verdict::Quiet,
        "the shipped wait at 1 min"
    );
}
