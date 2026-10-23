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
