// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The facts ring's file keeps the newest rows, and a successor writer counts what it finds.

use super::*;
use aterm_agent::supervise::occupancy::{OccupancyReading, SlotsSeen};

const MIN: i64 = 60_000;
/// 2026-10-19 18:51:41 UTC.
const T0: i64 = 1_792_435_901_000;

fn machine(at: i64, usable: u16) -> OccupancyReading {
    OccupancyReading {
        taken_ms: at,
        slots: Some(SlotsSeen { usable, n: 3 }),
        ..OccupancyReading::default()
    }
}

#[test]
fn the_file_keeps_the_newest_rows() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let path = ring::ring_path(tmp.path());
    assert!(path.ends_with("occupancy/facts.ring"));
    let cap = 5;
    let mut ring = RingFile::capped(path.clone(), cap);
    assert!(ring.read().is_empty(), "no file reads as no rows");
    for i in 0..(2 * cap as i64 - 1) {
        ring.append(&RingRow::of(&machine(T0 + i * MIN, 1), Vec::new()))
            .expect("append");
    }
    assert_eq!(
        ring.read().len(),
        2 * cap - 1,
        "under twice the cap: all kept"
    );
    ring.append(&RingRow::of(&machine(T0 + 99 * MIN, 1), Vec::new()))
        .expect("append");
    let rows = ring.read();
    assert_eq!(rows.len(), cap, "at twice the cap: the newest cap kept");
    assert_eq!(rows.last().map(|r| r.at), Some(T0 + 99 * MIN));
    assert_eq!(rows.first().map(|r| r.at), Some(T0 + 5 * MIN));
    // A successor writer counts the file it finds.
    let mut again = RingFile::capped(path, cap);
    again
        .append(&RingRow::of(&machine(T0 + 100 * MIN, 1), Vec::new()))
        .expect("append");
    assert_eq!(again.read().len(), cap + 1);
}

/// A ready session on one usable slot: a row that counts a slot-minute idle while ready.
fn ready_at(at: i64) -> RingRow {
    RingRow::of(
        &machine(at, 1),
        vec![ring::RingSession {
            sid: "a".to_string(),
            held_ms: Some(at),
            ready: true,
            guard: None,
        }],
    )
}

/// THE RING IS WRITTEN AT A MINUTE'S LOOK WHILE THE OWNER'S MODE OBSERVES (slice 1's F4, the
/// facts ring's row a minute): `observe` and `prompt` append a tick's row. NEGATIVE CONTROLS:
/// under `off` nothing is written — not even the file — and a poke between ticks has no row.
#[test]
fn the_ring_takes_a_ticks_row_while_the_mode_observes() {
    use crate::occupancy_host::{Look, append_look};
    use aterm_agent::supervise::Occupancy;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let path = ring::ring_path(tmp.path());
    let mut file = RingFile::at(path.clone());
    let row = ready_at(T0);
    for look in [Look::Tick, Look::Poke] {
        assert!(
            !append_look(&mut file, Occupancy::Off, look, Some(&row)).expect("off"),
            "{look:?}"
        );
    }
    assert!(!path.exists(), "off writes nothing");
    assert!(!append_look(&mut file, Occupancy::Observe, Look::Poke, None).expect("poke"));
    assert!(!path.exists(), "a poke has no row");
    for (i, mode) in [Occupancy::Observe, Occupancy::Prompt]
        .into_iter()
        .enumerate()
    {
        assert!(
            append_look(&mut file, mode, Look::Tick, Some(&row)).expect("tick"),
            "{mode:?}"
        );
        assert_eq!(file.read().len(), i + 1, "{mode:?}");
    }
}

/// THE HARNESS PAGE'S "IDLE WHILE READY" IS READ FROM THE RING'S FILE under the state
/// directory: the slot-minutes of the last day's rows, a minute a ready look. NEGATIVE
/// CONTROLS: no ring reads as none (the row is not drawn), and a row older than a day counts
/// nothing.
#[test]
fn idle_while_ready_reads_the_last_day_of_the_ring() {
    use crate::harness_screen::idle_ready_min_under;
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let now = T0 + 2 * aterm_agent::supervise::occupancy::rules::DAY_MS;
    assert_eq!(idle_ready_min_under(tmp.path(), now), None, "no ring");
    let mut file = RingFile::at(ring::ring_path(tmp.path()));
    file.append(&ready_at(T0)).expect("a day old");
    let looks = 5;
    for m in 0..looks {
        file.append(&ready_at(now - (looks - 1 - m) * MIN))
            .expect("append");
    }
    assert_eq!(
        idle_ready_min_under(tmp.path(), now),
        Some(u64::try_from(looks).expect("looks"))
    );
}
