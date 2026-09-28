// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 conformance: bind the REAL subscription cursor to the DERIVED
//! `Cursor` model (`aterm_spec::derive::cursor_model()`).
//!
//! The model is the writer/reader pair in miniature: `Grow` advances the
//! writer's `seq` and leaves the reader alone; `Deliver` (enabled only while the
//! reader is behind) catches the reader's `cursor` up to `seq`;
//! `CursorBounded` says the reader never passes the writer. Its `Buggy = 1`
//! member is the off-by-one class a cursor API can ship with (not a defect this
//! one is known to have had) — a delivery that parks the cursor one PAST the
//! head, so the next event written is silently skipped.
//!
//! This drives aterm-buffer's real `Surface::apply` (Grow) and
//! `Surface::subscribe`/`poll` (Deliver), projects each state onto
//! `<<seq, cursor>>`, and validates every real transition against the model
//! (in-process interpreter, plus `ty trace validate` wherever installed).
//! `subscribe`/`poll` are the library's public read face and no product crate
//! calls them today — aterm-gui, the only dependent, never opens a subscription
//! — so this binds the library API, not a product path.
//!
//! It is not a second copy of `conformance_subscribe`, which binds the same API
//! to `subscribe_model`'s no-silent-loss discipline but tracks the cursor itself
//! ("the head at poll time"): a `poll` that parks the cursor one past the head
//! passes that bind and fails this one.
//!
//! PROJECTION — `cursor` is READ BACK THROUGH THE REAL `poll`, never tracked by
//! the test: [`cursor_position`] clones the surface, writes a few probe events
//! past the head, and polls the cursor under test; the first event the real
//! `poll` delivers is `cursor + 1`. So a cursor the shipping code parks in the
//! wrong place projects to the wrong place — the property a caller-tracked
//! cursor (the "it is the head at poll time" shortcut) would assume away.
//!
//! NEGATIVE CONTROL — the modelled overshoot, projected from a real state, is
//! rejected by the committed model and admitted by `Buggy = 1`, so a pass is
//! never vacuous.

use aterm_buffer::{Cursor, Edit, SubUpdate, Surface};
mod support;
use aterm_spec::derive::cursor_model;
use aterm_spec::{interp, verify};
use std::collections::BTreeMap;
use support::{read_cap, write_cap};

type State = BTreeMap<&'static str, i64>;

/// Probe events written past the head on a CLONE, enough to see an overshoot of
/// up to two positions (the model's defect is one).
const PROBE: u64 = 3;

/// Where the real `cursor` stands, observed through the real `poll`: on a clone
/// of `s`, write [`PROBE`] events past the head and poll `cur`. The first
/// delivered event is `cursor + 1`; nothing delivered means the cursor is at or
/// past `head + PROBE`, which is reported as that bound (a projection the model
/// then rejects).
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

/// Validate one real transition as the named action, with `MaxSeq` lifted so the
/// model's bound never refuses a real write.
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
    // Bursts of writes separated by polls, including a poll with nothing new
    // (Deliver disabled: the real poll must deliver nothing and stay put).
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

    // NEGATIVE CONTROL — the modelled defect from this real state: one more
    // write, then a delivery that parks the cursor one past the head.
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
