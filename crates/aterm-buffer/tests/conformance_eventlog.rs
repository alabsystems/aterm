// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 conformance: the real `EventLog` against the derived ring model
//! (`aterm_models_core::ring_model()`, the same model Tier-0 checks in
//! `aterm-models/tests/derived_ring_ty.rs`). Each run drives `Surface::apply`,
//! projects every state onto `<<seq, lo>>`, and validates each real transition
//! as its own two-step trace: `ty trace validate` checks only the first
//! transition strictly, so one long trace would miss a deep corruption.
//! Negative controls show the check rejects corrupted steps.
//!
//! Both regimes are covered: below the cap (`lo == 1`) and past it (`lo`
//! advances on eviction). All-inputs refinement remains Tier-2.
//!
//! Tiered via [`verify::validate_transition_tiered`]: the in-process
//! interpreter always runs, `ty` re-checks where installed, and they must agree.

use crate::support::write_cap;
use aterm_buffer::{Edit, Surface};
use aterm_models_core::ring_model;
use aterm_spec::verify;
use std::collections::BTreeMap;

/// The ring's real cap (`aterm_buffer::MAX_LOG_EVENTS`); the model must use the
/// same one, or eviction would diverge.
const CAP: u64 = 1 << 16;

/// Projects the ring onto the model's `seq` (total appended) and `lo` (oldest
/// live seq; 1 when empty).
fn project(s: &Surface) -> (u64, u64) {
    let seq = s.seq().0;
    let lo = s.log().live().next().map(|e| e.seq.0).unwrap_or(1);
    (seq, lo)
}

/// Validate one real `Push` transition against the derived ring spec, with
/// `Cap` set to the real ring and `MaxSeq` high enough never to block a step.
fn validate_transition(prev: (u64, u64), next: (u64, u64)) -> (bool, String) {
    let m = ring_model();
    let state = |(seq, lo): (u64, u64)| -> BTreeMap<&'static str, i64> {
        [("seq", seq as i64), ("lo", lo as i64)]
            .into_iter()
            .collect()
    };
    verify::validate_transition_tiered(
        &m,
        &[("MaxSeq", 1_000_000_000), ("Cap", CAP as i64)],
        &state(prev),
        &state(next),
        Some("Push"),
        "EventLog conformance",
    )
}

/// Drive the real `Surface`/`EventLog` `n` appends and capture projected states
/// `[(seq, lo); n+1]` (index 0 is the initial empty state).
fn drive_real_eventlog(n: u64) -> Vec<(u64, u64)> {
    let mut surface = Surface::new();
    let mut states = vec![project(&surface)];
    for i in 0..n {
        surface.apply(&write_cap(), Edit::AppendLine(format!("line {i}")));
        states.push(project(&surface));
    }
    states
}

#[test]
fn real_eventlog_conforms_to_ring_spec() {
    // Drive the genuine shipping EventLog (no-eviction regime: 200 < Cap).
    let states = drive_real_eventlog(200);
    assert_eq!(
        states.first(),
        Some(&(0u64, 1u64)),
        "initial projected state must be Init (seq=0, lo=1)"
    );
    assert_eq!(
        states.last(),
        Some(&(200u64, 1u64)),
        "after 200 appends (< Cap): seq=200, lo=1 (no eviction)"
    );

    // Every sampled real transition must conform; in this regime all steps share
    // one shape, so a spread including the first and a deep one is representative.
    let sample = [0usize, 1, 2, 3, 5, 10, 25, 50, 75, 100, 150, 199];
    for &i in &sample {
        let (ok, out) = validate_transition(states[i], states[i + 1]);
        assert!(
            ok,
            "real transition #{i} {:?} -> {:?} must conform to Ring spec\n--- ty ---\n{out}",
            states[i],
            states[i + 1]
        );
    }

    // Negative control: two corruptions of a real transition must be rejected.
    let prev = states[100];
    let (seq, lo) = states[101];
    let (bad_seq_ok, o1) = validate_transition(prev, (seq + 7, lo)); // seq skip
    assert!(
        !bad_seq_ok,
        "corrupted transition (skipped seq) MUST fail conformance\n{o1}"
    );
    let (bad_lo_ok, o2) = validate_transition(prev, (seq, lo + 9)); // wrong ring head
    assert!(
        !bad_lo_ok,
        "corrupted transition (wrong lo) MUST fail conformance\n{o2}"
    );
}

/// Eviction regime: past `MAX_LOG_EVENTS` the ring head `lo` advances, and those
/// transitions must conform to the model.
#[test]
fn real_eventlog_eviction_conforms_to_ring_spec() {
    // Drive just past the cap so eviction begins (CAP+4 appends).
    let n = CAP + 4;
    let states = drive_real_eventlog(n);

    // `lo` stays 1 through seq == Cap (ring exactly full), then advances one per
    // append: lo == seq - Cap + 1 once over the cap.
    assert_eq!(
        states[CAP as usize],
        (CAP, 1),
        "at seq=Cap the ring is exactly full, lo=1"
    );
    assert_eq!(
        states[(CAP + 1) as usize],
        (CAP + 1, 2),
        "first eviction: appending past Cap pops seq 1, so lo advances to 2"
    );
    assert_eq!(
        states[n as usize],
        (n, n - CAP + 1),
        "in the eviction regime lo tracks the ring head (seq - Cap + 1)"
    );

    // Every transition across the onset of eviction must conform.
    let first = (CAP - 2) as usize;
    for i in first..(n as usize) {
        let (ok, out) = validate_transition(states[i], states[i + 1]);
        assert!(
            ok,
            "eviction transition #{i} {:?} -> {:?} must conform\n--- ty ---\n{out}",
            states[i],
            states[i + 1]
        );
    }

    // Negative control: a transition that fails to evict (lo held) is rejected,
    // so the model enforces eviction, not just monotone seq.
    let i = (CAP + 1) as usize;
    let prev = states[i]; // (Cap+1, 2) — next real step evicts, lo -> 3
    let next_seq = states[i + 1].0;
    let (held_ok, o) = validate_transition(prev, (next_seq, prev.1));
    assert!(
        !held_ok,
        "a non-evicting transition in the eviction regime MUST fail conformance\n{o}"
    );
}
