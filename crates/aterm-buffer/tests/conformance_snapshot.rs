// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `snapshot()` obeys the DERIVED isolation discipline.
//!
//! `aterm_spec::derive::snapshot_model()` is the drift-free model of snapshot
//! isolation: once a snapshot is taken, a later write must NOT leak into it
//! (`SnapshotIsolated`: `leaked = 0`; `ty` catches a leak at `Buggy=1` in
//! `aterm-spec/tests/derived_ring_ty.rs`). This binds that property to the real
//! `Surface::snapshot` / `Snapshot::read_text`: a snapshot's view is captured at
//! snapshot time and is unaffected by subsequent writes to the surface. The
//! observed no-leak state is checked against the model's own `SnapshotIsolated`
//! invariant. Pure Rust + real code, so it always runs.

use aterm_buffer::{Edit, LineId, Range, ReadCap, Surface, SurfaceId, WriteCap};
use aterm_spec::derive::snapshot_model;
use std::collections::BTreeMap;
use std::num::NonZeroU64;

fn full_range() -> Range {
    Range {
        start: LineId(0),
        end: LineId(u64::MAX),
    }
}

#[test]
fn real_snapshot_isolated_from_later_writes() {
    let m = snapshot_model();
    let mut s = Surface::new(SurfaceId(NonZeroU64::new(1).unwrap()));
    s.apply(&WriteCap, Edit::AppendLine("a".into()));
    s.apply(&WriteCap, Edit::AppendLine("b".into()));

    let snap = s.snapshot(&ReadCap); // capture the frozen world at seq = 2
    let snap_at = snap.at.0;
    let captured = snap.read_text(&ReadCap, full_range()).text;

    // A later write advances the surface and changes its content.
    s.apply(&WriteCap, Edit::AppendLine("c".into()));

    let snap_after = snap.read_text(&ReadCap, full_range()).text;
    let surface_now = s.read_text(&ReadCap, full_range()).text;

    // Isolation: the snapshot's view is unchanged by the later write, and differs
    // from the surface's new view. In the derived model this is exactly `leaked = 0`.
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

    // Tie the observed no-leak outcome to the model's actual invariant.
    let observed: BTreeMap<&'static str, i64> =
        [("seq", s.seq().0 as i64), ("snapped", 1), ("leaked", 0)]
            .into_iter()
            .collect();
    assert!(
        m.check_invariant("SnapshotIsolated", &observed),
        "the observed no-leak state satisfies the model's SnapshotIsolated invariant"
    );
}
