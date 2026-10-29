// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance: `poll()` obeys the derived no-silent-loss discipline.

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Cursor, Edit, SubUpdate, Surface};
use aterm_models_core::subscribe_model;
use aterm_spec::{interp, verify};
use std::collections::BTreeMap;

type State = BTreeMap<&'static str, i64>;

const CAP: u64 = aterm_buffer::MAX_LOG_EVENTS as u64;
const OVERRIDES: &[(&str, i64)] = &[("MaxSeq", (CAP + 16) as i64), ("Cap", CAP as i64)];

fn oldest_live(s: &Surface) -> u64 {
    s.log().live().next().map_or(1, |e| e.seq.0)
}

fn cursor_position(s: &Surface, cur: Cursor) -> u64 {
    let mut probe = s.clone();
    probe.apply(&write_cap(), Edit::AppendLine("probe".into()));
    match probe.poll(cur).0 {
        SubUpdate::Events(events) => events
            .first()
            .map_or(probe.seq().0, |first| first.seq.0 - 1),
        SubUpdate::Gap { .. } => panic!("cursor_position requires a caught-up or in-window cursor"),
    }
}

fn state(s: &Surface, cursor_at: u64, lost: i64) -> State {
    [
        ("seq", i64::try_from(s.seq().0).expect("small seq")),
        ("lo", i64::try_from(oldest_live(s)).expect("small lo")),
        ("cursor", i64::try_from(cursor_at).expect("small cursor")),
        ("lost", lost),
    ]
    .into_iter()
    .collect()
}

fn conforms(prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        &subscribe_model(),
        OVERRIDES,
        prev,
        next,
        Some(action),
        "Surface subscribe conformance",
    )
}

#[test]
fn real_poll_not_behind_delivers_matching_model() {
    let m = subscribe_model();
    let mut s = Surface::new();
    let cur = s.subscribe(&read_cap());
    let before_cursor = cursor_position(&s, cur);
    for i in 0..3 {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}")));
    }
    let pre = state(&s, before_cursor, 0);
    let (upd, next) = s.poll(cur);
    let after_cursor = cursor_position(&s, next);
    let (gapped, lost) = match &upd {
        SubUpdate::Events(events) => {
            let expected: Vec<u64> = ((before_cursor + 1)..=s.seq().0).collect();
            let got: Vec<u64> = events.iter().map(|e| e.seq.0).collect();
            (false, i64::from(got != expected))
        }
        SubUpdate::Gap { .. } => (true, 0),
    };
    let post = state(&s, after_cursor, lost);

    assert!(!gapped, "an in-window reader must be delivered, not gapped");
    assert_eq!(gapped, m.action_enabled("PollGap", &pre));
    assert!(m.action_enabled("PollDeliver", &pre));
    let (ok, why) = conforms(&pre, &post, "PollDeliver");
    assert!(ok, "real PollDeliver {pre:?} -> {post:?}\n{why}");
    assert!(m.check_invariant("NoSilentLoss", &post));
}

#[test]
fn real_poll_behind_gaps_no_silent_loss() {
    let m = subscribe_model();
    let mut s = Surface::new();
    let cur = s.subscribe(&read_cap());
    let before_cursor = cursor_position(&s, cur);
    for i in 0..(CAP + 4) {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}")));
    }
    let pre = state(&s, before_cursor, 0);
    let (upd, next) = s.poll(cur);
    let after_cursor = cursor_position(&s, next);
    let (gapped, lost) = match upd {
        SubUpdate::Gap { resync_to } => (true, i64::from(resync_to != s.seq())),
        SubUpdate::Events(_) => (false, 1),
    };
    let post = state(&s, after_cursor, lost);

    assert!(m.action_enabled("PollGap", &pre));
    assert!(!m.action_enabled("PollDeliver", &pre));
    assert!(gapped, "real poll() must gap a behind reader");
    let (ok, why) = conforms(&pre, &post, "PollGap");
    assert!(ok, "real PollGap {pre:?} -> {post:?}\n{why}");
    assert!(m.check_invariant("NoSilentLoss", &post));

    let silent_loss = state(&s, after_cursor, 1);
    assert!(!conforms(&pre, &silent_loss, "PollDeliver").0);
    assert!(
        interp::with_consts(&interp::with_buggy(&m, 1), OVERRIDES)
            .successors("PollDeliver", &pre)
            .contains(&silent_loss),
        "Buggy = 1 admits silent-loss delivery on a behind reader"
    );
    assert!(!m.check_invariant("NoSilentLoss", &silent_loss));
}
