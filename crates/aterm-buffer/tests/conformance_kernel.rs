// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests binding the real event-log spine to `aterm_models_session::kernel_model()`.

use crate::support::{read_cap, write_cap};
use aterm_buffer::{Edit, EventLog, LineId, MAX_LOG_EVENTS, Op, Seq, Surface, Ticks, TxnOutcome};
use aterm_models_session::kernel_model;
use aterm_spec::derive::Model;
use aterm_spec::{interp, verify};
use std::collections::BTreeMap;

type State = BTreeMap<&'static str, i64>;

const OVERRIDES: &[(&str, i64)] = &[("MaxSeq", 1_000_000_000)];

fn state(seq: u64, count: u64) -> State {
    [
        ("seq", i64::try_from(seq).expect("small seq")),
        ("count", i64::try_from(count).expect("small count")),
    ]
    .into_iter()
    .collect()
}

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

fn newest_stored(log: &EventLog, n: usize) -> Vec<u64> {
    if n == 1 {
        return vec![log.newest_live().expect("an event was stored").seq.0];
    }
    let live: Vec<u64> = log.live().map(|e| e.seq.0).collect();
    assert!(live.len() >= n, "the ring holds the {n} events just stored");
    live[live.len() - n..].to_vec()
}

fn assert_live_contiguous(log: &EventLog) {
    let oldest = log.oldest_live().expect("non-empty").seq.0;
    let newest = log.newest_live().expect("non-empty").seq.0;
    assert_eq!(
        newest - oldest + 1,
        log.live().count() as u64,
        "the live window [{oldest}, {newest}] has a gap"
    );
}

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

    let edits = [
        Edit::AppendLine("a".into()),
        Edit::AppendLine("b".into()),
        Edit::SetLine(LineId(0), "a2".into()),
        Edit::ClearLine(LineId(1)),
        Edit::SetLine(LineId(99), "nowhere".into()),
        Edit::ClearLine(LineId(99)),
    ];
    for edit in edits {
        let assigned = s.apply(&write_cap(), edit);
        spine.emitted(s.log(), 1, assigned, true);
    }

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

    let _ = s.snapshot(&read_cap());
    let _ = s.poll(s.subscribe(&read_cap()));
    assert_eq!(s.seq(), head, "reads are not events");

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
