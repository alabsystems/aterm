// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `poll()` obeys the DERIVED no-silent-loss discipline.
//!
//! `aterm_spec::derive::subscribe_model()` is the drift-free model of the
//! subscriber: a reader that has fallen behind the live ring window MUST receive
//! a `Gap` (resync), never be silently delivered events as if nothing was lost —
//! encoded as the model's `PollGap` guard `lo > cursor + 1` (and proven by `ty`,
//! with a `Buggy=1` counterexample, in `aterm-spec/tests/derived_ring_ty.rs`).
//!
//! This binds that model to the CODE THAT ACTUALLY RUNS: it drives the genuine
//! shipping `Surface::subscribe`/`poll`, projects each state onto the model
//! variables `<<seq, lo, cursor, lost>>`, and asserts the real gap-vs-deliver
//! decision equals `Model::action_enabled("PollGap", state)` — the model's own
//! guard, not a re-stated predicate. So if `poll()` ever silently delivered to a
//! behind reader (a silent loss), this test catches it. Unlike the `ty` tests,
//! this is pure Rust (the model interpreter) + real code, so it always runs.

use aterm_buffer::{Edit, ReadCap, SubUpdate, Surface, SurfaceId, WriteCap};
use aterm_spec::derive::subscribe_model;
use std::collections::BTreeMap;
use std::num::NonZeroU64;

/// The real ring cap (mirrors `aterm_buffer::MAX_LOG_EVENTS = 1<<16`); a cursor
/// only falls behind once eviction passes it, i.e. after > CAP appends.
const CAP: u64 = 1 << 16;

fn surface(id: u64) -> Surface {
    Surface::new(SurfaceId(NonZeroU64::new(id).unwrap()))
}

/// The oldest still-live seq (the ring head `lo`); 1 when empty — the same
/// projection the ring conformance uses.
fn oldest_live(s: &Surface) -> u64 {
    s.log().live().next().map(|e| e.seq.0).unwrap_or(1)
}

/// Project the subscriber state the model reasons about. `cursor_at` is tracked
/// by the caller (the real `Cursor.at` is private but deterministic: it is the
/// head at subscribe/poll time).
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
    let mut s = surface(1);
    let cur = s.subscribe(&ReadCap); // at = current head = 0
    let cursor_at = s.seq().0;
    for i in 0..3 {
        s.apply(&WriteCap, Edit::AppendLine(format!("e{i}"))); // no eviction (< CAP)
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
    let mut s = surface(2);
    let cur = s.subscribe(&ReadCap); // at = 0
    let cursor_at = s.seq().0; // 0
    // Drive past the cap so eviction passes the cursor (oldest live > cursor + 1).
    for i in 0..(CAP + 4) {
        s.apply(&WriteCap, Edit::AppendLine(format!("e{i}")));
    }
    let (upd, _next) = s.poll(cur);
    let gapped = matches!(upd, SubUpdate::Gap { .. });
    let st = state(&s, cursor_at);

    // The model forbids delivery (PollGap enabled, PollDeliver disabled): the real
    // subscriber MUST gap. A silent delivery here would be exactly the bug the
    // derived NoSilentLoss invariant catches.
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
