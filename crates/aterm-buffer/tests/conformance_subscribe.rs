// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `poll()` obeys the derived no-silent-loss discipline.
//!
//! `aterm_spec::derive::subscribe_model()`: a reader behind the live ring window
//! must get a `Gap`, never events delivered as if nothing was lost (the
//! `PollGap` guard `lo > cursor + 1`). The test drives the real
//! `Surface::subscribe`/`poll`, projects onto `<<seq, lo, cursor, lost>>`, and
//! asserts the real gap-vs-deliver decision equals the model's own
//! `Model::action_enabled("PollGap", state)`. Pure Rust, so it always runs.

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Edit, SubUpdate, Surface};
use aterm_spec::derive::subscribe_model;
use std::collections::BTreeMap;

/// The real ring cap (`aterm_buffer::MAX_LOG_EVENTS`); a cursor falls behind
/// only after more than `CAP` appends.
const CAP: u64 = 1 << 16;

/// The oldest live seq (the ring head `lo`); 1 when empty.
fn oldest_live(s: &Surface) -> u64 {
    s.log().live().next().map(|e| e.seq.0).unwrap_or(1)
}

/// Project the subscriber state. `cursor_at` is tracked by the caller: the real
/// `Cursor.at` is private but is the head at subscribe/poll time.
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
    // Drive past the cap so eviction passes the cursor (oldest live > cursor + 1).
    for i in 0..(CAP + 4) {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}")));
    }
    let (upd, _next) = s.poll(cur);
    let gapped = matches!(upd, SubUpdate::Gap { .. });
    let st = state(&s, cursor_at);

    // The model forbids delivery here (PollGap enabled), so the real subscriber
    // must gap; a delivery would be a silent loss.
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
