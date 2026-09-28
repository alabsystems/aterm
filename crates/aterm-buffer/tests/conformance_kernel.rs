// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 conformance: bind the REAL event-log spine to the DERIVED `Kernel`
//! model (`aterm_spec::derive::kernel_model()`).
//!
//! The model is the spine's gap-free law: every `Emit` bumps `count` by one and
//! assigns the next contiguous `seq`, so `SeqIsCount` (`seq = count`) holds in
//! every state. Its `Buggy = 1` member opens a gap (an append that advances
//! `seq` by two).
//!
//! The two projected variables come from DIFFERENT sides of the real code, so
//! the law is checked rather than restated:
//!
//! * `count` is the WRITER'S side — how many edits this test handed the
//!   shipping `Surface::apply` / `Surface::transact` / `EventLog::append_at`;
//! * `seq` is the SPINE'S side — the seq stamped on each event the real log
//!   STORED, the one every reader is handed, read back off the ring (never the
//!   log's own counter). Each stored event is one `Emit`, so a call that stored
//!   events with a gap between their seqs is a step the model rejects even when
//!   the counter itself kept counting. The seq a call returns and the log's
//!   reported head must both be the newest stored one.
//!
//! Every real edit is validated as an `Emit` (in-process interpreter on every
//! step, `ty trace validate` on a spread wherever installed), across every edit
//! kind, a committed multi-edit transaction (its events read back one by one),
//! a refused one (no events, so no step), the temporal `append_at` seam, and the
//! eviction regime past `MAX_LOG_EVENTS` — the spine keeps counting after the
//! ring forgets, and the live window stays contiguous at the eviction edge
//! (what `append_at` spills is exactly the event that fell off).
//!
//! NEGATIVE CONTROL — the modelled gap, projected from a real state, is rejected
//! by the committed model and admitted by `Buggy = 1`.

use aterm_buffer::{Edit, EventLog, LineId, MAX_LOG_EVENTS, Op, Seq, Surface, Ticks, TxnOutcome};
mod support;
use aterm_spec::derive::{Model, kernel_model};
use aterm_spec::{interp, verify};
use std::collections::BTreeMap;
use support::{read_cap, write_cap};

type State = BTreeMap<&'static str, i64>;

/// `MaxSeq` lifted past every real run so the model's bound never refuses one.
const OVERRIDES: &[(&str, i64)] = &[("MaxSeq", 1_000_000_000)];

fn state(seq: u64, count: u64) -> State {
    [
        ("seq", i64::try_from(seq).expect("small seq")),
        ("count", i64::try_from(count).expect("small count")),
    ]
    .into_iter()
    .collect()
}

/// Is `prev -> next` the model's `Emit`? The interpreter answers every step;
/// `tiered` additionally asks `ty` (spawned per call, so used on a spread).
fn emits(m: &Model, prev: &State, next: &State, tiered: bool) -> (bool, String) {
    if tiered {
        return verify::validate_transition_tiered(
            m,
            OVERRIDES,
            prev,
            next,
            Some("Emit"),
            "event-log spine conformance",
        );
    }
    let m = interp::with_consts(m, OVERRIDES);
    (
        m.successors("Emit", prev).contains(next),
        "interpreter: Emit".to_string(),
    )
}

/// The seqs of the `n` NEWEST events the log stored, oldest first — read back
/// off the ring, which is what every reader sees.
fn newest_stored(log: &EventLog, n: usize) -> Vec<u64> {
    if n == 1 {
        return vec![log.newest_live().expect("an event was stored").seq.0];
    }
    let live: Vec<u64> = log.live().map(|e| e.seq.0).collect();
    assert!(live.len() >= n, "the ring holds the {n} events just stored");
    live[live.len() - n..].to_vec()
}

/// The live window is contiguous: from the oldest live seq to the newest, one
/// event per seq. O(ring), so only asserted at the eviction edge.
fn assert_live_contiguous(log: &EventLog) {
    let oldest = log.oldest_live().expect("non-empty").seq.0;
    let newest = log.newest_live().expect("non-empty").seq.0;
    assert_eq!(
        newest - oldest + 1,
        log.live().count() as u64,
        "the live window [{oldest}, {newest}] has a gap"
    );
}

/// Drive writer-side emits and validate the spine's answer.
struct Spine {
    model: Model,
    count: u64,
    state: State,
    steps: usize,
}

impl Spine {
    fn new() -> Self {
        let model = kernel_model();
        let state = model.init_state();
        Self {
            model,
            count: 0,
            state,
            steps: 0,
        }
    }

    /// Record that ONE real call handed the spine `n` events: each of the `n`
    /// events it stored is one `Emit`, projected from the seq stored on it. The
    /// seq the call returned and the log's head must be the newest of them.
    fn emitted(&mut self, log: &EventLog, n: usize, returned: Seq, tiered: bool) {
        let stored = newest_stored(log, n);
        let newest = *stored.last().expect("n > 0");
        for seq in stored {
            self.count += 1;
            let next = state(seq, self.count);
            let (ok, why) = emits(&self.model, &self.state, &next, tiered);
            assert!(
                ok,
                "real spine step {:?} -> {next:?} is not Emit\n{why}",
                self.state
            );
            assert!(
                self.model.check_invariant("SeqIsCount", &next),
                "real spine opened a gap: {next:?}"
            );
            self.state = next;
            self.steps += 1;
        }
        assert_eq!(returned.0, newest, "the call returns the seq it stored");
        assert_eq!(log.head().0, newest, "the head is the newest stored seq");
    }
}

#[test]
fn real_surface_spine_conforms_to_kernel_model() {
    let mut s = Surface::new();
    let mut spine = Spine::new();
    assert_eq!(
        state(s.seq().0, 0),
        spine.model.init_state(),
        "an empty surface is the model's Init"
    );

    // Every edit kind, each one event on the spine. `apply` returns the seq it
    // assigned: it must be the head the spine then reports.
    let edits = [
        Edit::AppendLine("a".into()),
        Edit::AppendLine("b".into()),
        Edit::SetLine(LineId(0), "a2".into()),
        Edit::ClearLine(LineId(1)),
        // An absent line still rides the spine (the edit is recorded, not lost).
        Edit::SetLine(LineId(99), "nowhere".into()),
        Edit::ClearLine(LineId(99)),
    ];
    for edit in edits {
        let assigned = s.apply(&write_cap(), edit);
        spine.emitted(s.log(), 1, assigned, true);
    }

    // A committed transaction: its body lands atomically, one event per edit, so
    // ONE real call stores that many events, each read back as one `Emit`.
    let base = s.seq();
    let body = vec![
        Edit::AppendLine("t1".into()),
        Edit::AppendLine("t2".into()),
        Edit::SetLine(LineId(2), "t1'".into()),
    ];
    let n = body.len();
    let TxnOutcome::Committed(committed) = s.transact(&write_cap(), base, body) else {
        panic!("a transaction on the current head commits");
    };
    spine.emitted(s.log(), n, committed, true);

    // A refused transaction appends nothing: no event, so no step — the head
    // must not move.
    let stale = Seq(base.0);
    let head = s.seq();
    assert_eq!(
        s.transact(&write_cap(), stale, vec![Edit::AppendLine("lost?".into())]),
        TxnOutcome::Conflict
    );
    assert_eq!(
        s.seq(),
        head,
        "a refused transaction leaves the spine alone"
    );
    assert_eq!(state(s.seq().0, spine.count), spine.state);

    // A reader never moves the spine either.
    let _ = s.snapshot(&read_cap());
    let _ = s.poll(s.subscribe(&read_cap()));
    assert_eq!(s.seq(), head, "reads are not events");

    // EVICTION REGIME: past MAX_LOG_EVENTS the ring forgets its oldest entries
    // but the spine keeps counting — `seq = count` must survive eviction.
    let cap = MAX_LOG_EVENTS as u64;
    let target = cap + 16;
    while spine.count < target {
        let assigned = s.apply(&write_cap(), Edit::AppendLine("flood".into()));
        let near_edge = spine.count + 4 > cap && spine.count < cap + 4;
        spine.emitted(
            s.log(),
            1,
            assigned,
            near_edge || spine.count.is_multiple_of(8192),
        );
        if near_edge {
            assert_live_contiguous(s.log());
        }
    }
    let oldest = s.log().oldest_live().expect("ring is non-empty").seq.0;
    assert!(
        oldest > 1,
        "eviction actually happened (oldest live = {oldest})"
    );
    assert_eq!(s.seq().0, spine.count, "the spine counted every event");
    assert!(spine.steps as u64 >= target, "every real event was a step");
}

/// The temporal seam (`EventLog::append_at`) rides the same spine — including
/// across its own spill point, where it hands back the evicted event.
#[test]
fn real_append_at_spine_conforms_to_kernel_model() {
    let mut log = EventLog::default();
    let mut spine = Spine::new();
    let cap = MAX_LOG_EVENTS as u64;
    let mut spilled = 0u64;
    while spine.count < cap + 3 {
        let (seq, evicted) = log.append_at(Op::Resize { rows: 24, cols: 80 }, Ticks(spine.count));
        let near_edge = spine.count + 3 > cap;
        spine.emitted(&log, 1, seq, near_edge || spine.count == 0);
        if let Some(evicted) = evicted {
            spilled += 1;
            // The spill is exactly the event that fell off the live window.
            assert_eq!(
                evicted.seq.0 + 1,
                log.oldest_live().expect("non-empty").seq.0,
                "append_at spilled an event that was not the oldest"
            );
        }
        if near_edge {
            assert_live_contiguous(&log);
        }
    }
    assert_eq!(
        spilled, 3,
        "the spill seam fired once per event past the cap"
    );
    assert_eq!(log.total(), spine.count);
}

/// The modelled defect from a REAL state: an append that jumps the head by two.
#[test]
fn a_gapped_append_is_the_buggy_step_the_bind_rejects() {
    let m = kernel_model();
    let mut s = Surface::new();
    s.apply(&write_cap(), Edit::AppendLine("x".into()));
    let stored = newest_stored(s.log(), 1)[0];
    let prev = state(stored, 1);
    let gapped = state(stored + 2, 2);
    let (ok, _) = emits(&m, &prev, &gapped, true);
    assert!(!ok, "the committed model must reject a gapped append");
    assert!(
        interp::with_consts(&interp::with_buggy(&m, 1), OVERRIDES)
            .successors("Emit", &prev)
            .contains(&gapped),
        "Buggy = 1 admits exactly this gap"
    );
    assert!(!m.check_invariant("SeqIsCount", &gapped));
}
