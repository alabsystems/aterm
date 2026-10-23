// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Conformance tests for the subscription [`Cursor`] ([`cursor_model`]).

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Cursor, Edit, SubUpdate, Surface};
use aterm_models_core::cursor_model;
use aterm_spec::{interp, verify};
use std::collections::BTreeMap;

type State = BTreeMap<&'static str, i64>;

const PROBE: u64 = 3;

fn cursor_position(s: &Surface, cur: Cursor) -> u64 {
    let mut probe = s.clone();
    for i in 0..PROBE {
        probe.apply(&write_cap(), Edit::AppendLine(format!("probe {i}")));
    }
    match probe.poll(cur).0 {
        SubUpdate::Events(events) => events
            .first()
            .map_or(probe.seq().0, |first| first.seq.0 - 1),
        SubUpdate::Gap { .. } => panic!("the cursor bind stays inside the live ring"),
    }
}

fn project(s: &Surface, cur: Cursor) -> State {
    [
        ("seq", i64::try_from(s.seq().0).expect("small seq")),
        (
            "cursor",
            i64::try_from(cursor_position(s, cur)).expect("small cursor"),
        ),
    ]
    .into_iter()
    .collect()
}

fn conforms(prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        &cursor_model(),
        &[("MaxSeq", 1_000_000)],
        prev,
        next,
        Some(action),
        "Surface cursor conformance",
    )
}

#[test]
fn real_subscription_cursor_conforms_to_cursor_model() {
    let m = cursor_model();
    let mut s = Surface::new();
    let mut cur = s.subscribe(&read_cap());
    let mut state = project(&s, cur);
    assert_eq!(
        state,
        m.init_state(),
        "a fresh subscription is the model's Init"
    );

    let mut grows = 0;
    let mut delivers = 0;
    for burst in [1u64, 3, 0, 2, 5, 0, 1] {
        for i in 0..burst {
            let edit = match i % 3 {
                0 => Edit::AppendLine(format!("line {burst}.{i}")),
                1 => Edit::SetLine(aterm_buffer::LineId(0), format!("set {burst}.{i}")),
                _ => Edit::ClearLine(aterm_buffer::LineId(0)),
            };
            s.apply(&write_cap(), edit);
            let next = project(&s, cur);
            let (ok, why) = conforms(&state, &next, "Grow");
            assert!(ok, "real write {state:?} -> {next:?} is not Grow\n{why}");
            grows += 1;
            state = next;
        }

        let enabled = m.action_enabled("Deliver", &state);
        let (update, advanced) = s.poll(cur);
        cur = advanced;
        let next = project(&s, cur);
        let delivered = match update {
            SubUpdate::Events(events) => events.len(),
            SubUpdate::Gap { .. } => panic!("no eviction in this regime"),
        };
        if enabled {
            let (ok, why) = conforms(&state, &next, "Deliver");
            assert!(ok, "real poll {state:?} -> {next:?} is not Deliver\n{why}");
            assert_eq!(
                i64::try_from(delivered).expect("small"),
                state["seq"] - state["cursor"],
                "Deliver hands over exactly the events between cursor and head"
            );
            delivers += 1;
        } else {
            assert_eq!(delivered, 0, "a caught-up poll delivers nothing");
            assert_eq!(next, state, "a caught-up poll moves nothing");
        }
        assert!(
            m.check_invariant("CursorBounded", &next),
            "real cursor passed the writer: {next:?}"
        );
        state = next;
    }
    assert!(grows > 0 && delivers > 0, "both actions were driven");

    s.apply(&write_cap(), Edit::AppendLine("tail".into()));
    let behind = project(&s, cur);
    let mut overshoot = behind.clone();
    overshoot.insert("cursor", behind["seq"] + 1);
    let (ok, _) = conforms(&behind, &overshoot, "Deliver");
    assert!(
        !ok,
        "the committed model must reject an overshooting delivery"
    );
    assert!(
        interp::with_buggy(&m, 1)
            .successors("Deliver", &behind)
            .contains(&overshoot),
        "Buggy = 1 admits exactly this overshoot"
    );
    assert!(!m.check_invariant("CursorBounded", &overshoot));
}
