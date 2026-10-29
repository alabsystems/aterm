// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Conformance: the real `snapshot()` obeys the derived isolation discipline.

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Edit, LineId, Range, Snapshot, Surface};
use aterm_models_session::snapshot_model;
use aterm_spec::{interp, verify};
use std::collections::BTreeMap;

type State = BTreeMap<&'static str, i64>;

fn full_range() -> Range {
    Range {
        start: LineId(0),
        end: LineId(u64::MAX),
    }
}

fn project(s: &Surface, active: Option<(&Snapshot, &str)>) -> State {
    let (snapped, leaked) = match active {
        None => (0, 0),
        Some((snap, baseline)) => {
            let now = snap.read_text(&read_cap(), full_range()).text;
            let leaked = i64::from(now != baseline || snap.at.0 > s.seq().0);
            (1, leaked)
        }
    };
    [
        ("seq", i64::try_from(s.seq().0).expect("small seq")),
        ("snapped", snapped),
        ("leaked", leaked),
    ]
    .into_iter()
    .collect()
}

fn conforms(prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        &snapshot_model(),
        &[("MaxSeq", 16)],
        prev,
        next,
        Some(action),
        "Surface snapshot conformance",
    )
}

#[test]
fn real_snapshot_isolated_from_later_writes() {
    let m = snapshot_model();
    let mut s = Surface::new();
    let mut state = project(&s, None);
    assert_eq!(state, m.init_state(), "empty surface is Init");

    for line in ["a", "b"] {
        s.apply(&write_cap(), Edit::AppendLine(line.into()));
        let next = project(&s, None);
        let (ok, why) = conforms(&state, &next, "Write");
        assert!(ok, "pre-snap Write {state:?} -> {next:?}\n{why}");
        state = next;
    }

    let snap = s.snapshot(&read_cap());
    assert_eq!(snap.at.0, 2, "snapshot captured head at seq = 2");
    let captured = snap.read_text(&read_cap(), full_range()).text;
    let snapped_state = project(&s, Some((&snap, &captured)));
    let (ok, why) = conforms(&state, &snapped_state, "Snap");
    assert!(ok, "Snap {state:?} -> {snapped_state:?}\n{why}");
    state = snapped_state;

    s.apply(&write_cap(), Edit::AppendLine("c".into()));
    let after = project(&s, Some((&snap, &captured)));
    let (ok, why) = conforms(&state, &after, "Write");
    assert!(ok, "post-snap Write {state:?} -> {after:?}\n{why}");
    assert_ne!(
        snap.read_text(&read_cap(), full_range()).text,
        s.read_text(&read_cap(), full_range()).text,
        "the later write is visible on the surface but not in the isolated snapshot"
    );
    assert!(
        m.check_invariant("SnapshotIsolated", &after),
        "projected post-write snapshot state satisfies SnapshotIsolated: {after:?}"
    );

    let mut leaked = after.clone();
    leaked.insert("leaked", 1);
    assert!(!conforms(&state, &leaked, "Write").0);
    assert!(
        interp::with_consts(&interp::with_buggy(&m, 1), &[("MaxSeq", 16)])
            .successors("Write", &state)
            .contains(&leaked),
        "Buggy = 1 admits the leaked post-snapshot write"
    );
    assert!(!m.check_invariant("SnapshotIsolated", &leaked));
}
