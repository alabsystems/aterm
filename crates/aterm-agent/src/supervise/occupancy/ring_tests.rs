// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The facts ring: a row carries the reading back, and the slot-time idle while ready counts
//! each look once.

use super::*;

const MIN: i64 = 60_000;
/// 2026-10-19 18:51:41 UTC.
const T0: i64 = 1_792_435_901_000;

fn machine(at: i64, usable: u16) -> OccupancyReading {
    OccupancyReading {
        taken_ms: at,
        stale: false,
        avail_mib: Some(96 << 10),
        pressure: Some(Pressure::Warn),
        disk: Some(Level::Short),
        slots: Some(SlotsSeen { usable, n: 3 }),
        slots_free_ms: Some(25 * MIN as u64),
        room: Some(false),
        held_slot_last_ms: None,
    }
}

fn session(sid: &str, ready: bool) -> RingSession {
    RingSession {
        sid: sid.to_string(),
        held_ms: Some(T0 - MIN),
        ready,
        guard: None,
    }
}

#[test]
fn a_row_carries_the_reading_back() {
    let reading = machine(T0, 2);
    let row = RingRow::of(&reading, vec![session("s-1", true)]);
    let back = rows_in(&format!("{}\nnot a row\n{{\"at\":\"x\"}}\n", row.line()));
    assert_eq!(back, vec![row.clone()], "an unreadable line is passed over");
    let mut want = reading;
    want.held_slot_last_ms = Some(T0 - MIN);
    assert_eq!(back[0].reading("@s-1"), want);
    assert_eq!(back[0].reading("s-2").held_slot_last_ms, None);
    let empty = RingRow::of(&OccupancyReading::default(), Vec::new());
    assert_eq!(
        rows_in(&empty.line())[0].reading("s-1"),
        OccupancyReading::default()
    );
}

/// Each look counts the fewer of the usable slots and the ready sessions, for the time since
/// the look before, at most a look: a window asleep counts nothing for its sleep, and a stale
/// board none.
#[test]
fn idle_while_ready_counts_each_look_once() {
    let row = |at: i64, usable: u16, ready: usize, stale: bool| {
        let mut m = machine(at, usable);
        m.stale = stale;
        let sessions = (0..3)
            .map(|i| session(&format!("s-{i}"), i < ready))
            .collect();
        RingRow::of(&m, sessions)
    };
    let rows = [
        row(T0, 2, 3, false),
        row(T0 + MIN, 2, 3, false),
        row(T0 + 2 * MIN, 2, 3, false),
    ];
    assert_eq!(idle_ready_ms(&rows, 0), 3 * 2 * MIN as u64);
    assert_eq!(idle_ready_ms(&rows, T0 + MIN), 2 * 2 * MIN as u64);
    let one = [row(T0, 2, 1, false), row(T0 + MIN, 2, 1, false)];
    assert_eq!(idle_ready_ms(&one, 0), 2 * MIN as u64);
    let none = [row(T0, 2, 0, false), row(T0 + MIN, 0, 3, false)];
    assert_eq!(idle_ready_ms(&none, 0), 0);
    let gap = [
        row(T0, 2, 3, false),
        row(T0 + MIN, 2, 3, true),
        row(T0 + 121 * MIN, 1, 3, false),
    ];
    assert_eq!(idle_ready_ms(&gap, 0), (2 * MIN + MIN) as u64);
}

/// One random draw (xorshift), for the property test.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// THE RING'S FILE MAY LEAVE OUT A LOOK THAT NAMED NO SESSION (the window writes none,
/// `occupancy_host::append_look`): over rows whose consecutive looks lie at least
/// [`LOOK_EVERY_MS`] apart, the slot-time idle while ready is the same with those rows left out.
/// Such a row counts nothing itself, and the span of the row after it is a whole look either way.
/// The precondition is this test's, not the ring's: a row's time is its held snapshot's, which
/// two ticks can share, and a row whose snapshot an empty row's shares spans nothing after it and
/// a whole look without it, so a successor seeded from the file can count up to one look more
/// for each session that starts ready on such a snapshot. NEGATIVE CONTROL: an `idle_ready_ms`
/// that stops clamping each span to a look counts the left-out minute into the next row's span,
/// and the two differ.
#[test]
fn rows_a_look_apart_with_no_session_change_no_idle_while_ready() {
    let mut draw = Draw(0x9e37_79b9_7f4a_7c15);
    for case in 0..500 {
        let mut at = T0;
        let rows: Vec<RingRow> = (0..2 + draw.below(30))
            .map(|_| {
                at += LOOK_EVERY_MS + i64::try_from(draw.below(3 * 60)).expect("small") * 1000;
                let mut m = machine(at, u16::try_from(draw.below(4)).expect("small"));
                m.stale = draw.below(8) == 0;
                let sessions = if draw.below(3) == 0 {
                    Vec::new()
                } else {
                    (0..1 + draw.below(3))
                        .map(|i| session(&format!("s-{i}"), draw.below(2) == 0))
                        .collect()
                };
                RingRow::of(&m, sessions)
            })
            .collect();
        let kept: Vec<RingRow> = rows
            .iter()
            .filter(|r| !r.sessions.is_empty())
            .cloned()
            .collect();
        let since = T0 + i64::try_from(draw.below(20)).expect("small") * MIN;
        assert_eq!(
            idle_ready_ms(&rows, since),
            idle_ready_ms(&kept, since),
            "case {case}: {rows:?}"
        );
    }
}
