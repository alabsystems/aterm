// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Conformance: the real `snapshot()` obeys the derived isolation discipline.

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Edit, LineId, Range, Surface};
use aterm_models_session::snapshot_model;
use std::collections::BTreeMap;

fn full_range() -> Range {
    Range {
        start: LineId(0),
        end: LineId(u64::MAX),
    }
}

#[test]
fn real_snapshot_isolated_from_later_writes() {
    let m = snapshot_model();
    let mut s = Surface::new();
    s.apply(&write_cap(), Edit::AppendLine("a".into()));
    s.apply(&write_cap(), Edit::AppendLine("b".into()));

    let snap = s.snapshot(&read_cap()); // capture the frozen world at seq = 2
    let snap_at = snap.at.0;
    let captured = snap.read_text(&read_cap(), full_range()).text;

    s.apply(&write_cap(), Edit::AppendLine("c".into()));

    let snap_after = snap.read_text(&read_cap(), full_range()).text;
    let surface_now = s.read_text(&read_cap(), full_range()).text;

    assert_eq!(
        snap_after, captured,
        "snapshot view must be isolated from later writes (no leak)"
    );
    assert_ne!(
        snap_after, surface_now,
        "the later write is visible on the surface but NOT in the isolated snapshot"
    );
    assert_eq!(
        snap_at, 2,
        "snapshot captured the head at snapshot time (seq = 2)"
    );
    assert_eq!(s.seq().0, 3, "the surface advanced past the snapshot");

    let observed: BTreeMap<&'static str, i64> =
        [("seq", s.seq().0 as i64), ("snapped", 1), ("leaked", 0)]
            .into_iter()
            .collect();
    assert!(
        m.check_invariant("SnapshotIsolated", &observed),
        "the observed no-leak state satisfies the model's SnapshotIsolated invariant"
    );
}
