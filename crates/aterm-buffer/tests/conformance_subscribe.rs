// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance: `poll()` obeys the derived no-silent-loss discipline.

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Edit, SubUpdate, Surface};
use aterm_models_core::subscribe_model;
use std::collections::BTreeMap;

const CAP: u64 = 1 << 16;

fn oldest_live(s: &Surface) -> u64 {
    s.log().live().next().map(|e| e.seq.0).unwrap_or(1)
}

fn state(s: &Surface, cursor_at: u64) -> BTreeMap<&'static str, i64> {
    [
        ("seq", s.seq().0 as i64),
        ("lo", oldest_live(s) as i64),
        ("cursor", cursor_at as i64),
        ("lost", 0),
    ]
    .into_iter()
    .collect()
}

#[test]
fn real_poll_not_behind_delivers_matching_model() {
    let m = subscribe_model();
    let mut s = Surface::new();
    let cur = s.subscribe(&read_cap()); // at = current head = 0
    let cursor_at = s.seq().0;
    for i in 0..3 {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}"))); // no eviction (< CAP)
    }
    let (upd, _next) = s.poll(cur);
    let gapped = matches!(upd, SubUpdate::Gap { .. });
    let st = state(&s, cursor_at);

    assert!(
        !gapped,
        "a reader that is not behind must be delivered, not gapped"
    );
    assert_eq!(
        gapped,
        m.action_enabled("PollGap", &st),
        "real gap-decision must equal the model's PollGap guard"
    );
    assert!(
        m.action_enabled("PollDeliver", &st),
        "model permits delivery here"
    );
}

#[test]
fn real_poll_behind_gaps_no_silent_loss() {
    let m = subscribe_model();
    let mut s = Surface::new();
    let cur = s.subscribe(&read_cap()); // at = 0
    let cursor_at = s.seq().0; // 0
    for i in 0..(CAP + 4) {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}")));
    }
    let (upd, _next) = s.poll(cur);
    let gapped = matches!(upd, SubUpdate::Gap { .. });
    let st = state(&s, cursor_at);

    assert!(
        m.action_enabled("PollGap", &st),
        "model: a behind reader must gap"
    );
    assert!(
        !m.action_enabled("PollDeliver", &st),
        "model: a behind reader must not deliver"
    );
    assert!(
        gapped,
        "real poll() MUST gap a behind reader, never silently deliver"
    );
    assert_eq!(
        gapped,
        m.action_enabled("PollGap", &st),
        "real gap-decision must equal the model's PollGap guard"
    );
}
