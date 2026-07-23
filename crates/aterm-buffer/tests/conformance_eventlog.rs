// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 trace conformance: bind the REAL `EventLog` to the DERIVED ring spec.
//!
//! The committed specs in `aterm-spec-models/specs/` are model-checked in the
//! abstract (`ty check`) — they prove the *design* sound, but nothing ties them
//! to the *code that actually runs*. A model can be correct while the
//! implementation drifts. This test closes that gap for the bounded event-log
//! ring: it drives the genuine shipping `Surface`/`EventLog` (the same
//! `apply(&WriteCap, Edit::AppendLine(..))` path production uses), projects each
//! reachable state onto the spec variables `<<seq, lo>>`, and asks the real `ty`
//! binary to confirm every observed transition is one the spec's `Next` admits.
//! This is the "Tier 1" layer of `docs/RFC-ty-embed-derived-tla.md`: it binds
//! model <-> executable.
//!
//! SINGLE SOURCE — the spec here is NOT hand-written: it is generated from
//! `aterm_spec::derive::ring_model()`, the very same model that Tier-0
//! exhaustively `ty check`s (`aterm-spec/tests/derived_ring_ty.rs`). One Rust
//! source feeds both the exhaustive check and this conformance binding, so the
//! spec cannot drift from the model — and the model is what the projection targets.
//!
//! METHOD — strict per-transition validation. `ty trace validate --spec` strictly
//! checks a trace's INITIAL state (against `Init`) and its FIRST transition
//! (against `Next`), but past the first step it observation-matches leniently and
//! will not reliably reject a deep transition corruption. So instead of one long
//! trace, we validate EACH real transition `(prev -> next)` as its own strict
//! first transition: the derived spec's `Init` is parameterized (`Model::
//! transition_spec`, CONSTANTS `seq_init, lo_init`) and pinned to `prev`, and a
//! two-step trace `[prev, next]` is checked. A corrupted `next` (wrong seq or
//! wrong lo) is then reliably rejected — which the negative control asserts, so a
//! pass is never vacuous.
//!
//! SCOPE — two regimes are covered: the no-eviction append discipline
//! (`< Cap` appends, `lo == 1`; monotone seq, ring head tracked) AND the eviction
//! regime (`> MAX_LOG_EVENTS` appends, where `lo` advances as oldest events are
//! popped) — see `real_eventlog_eviction_conforms_to_ring_spec`. Only full
//! all-inputs refinement (any starting state, not the traced executions) remains
//! Tier-2 (MIR refinement via trust-mc), per the RFC.
//!
//! VERIFICATION GATE (two-tier, see [`aterm_spec::verify`]): every transition is
//! validated by the IN-PROCESS interpreter (`Next` admission via
//! `verify::validate_transition_tiered` — a real check, toolchain-free), and
//! ADDITIONALLY by the external `ty trace validate` wherever the Trust
//! toolchain is installed. The tiers must agree; disagreement panics.

use aterm_buffer::{Edit, Surface, SurfaceId, WriteCap};
use aterm_spec::derive::ring_model;
use aterm_spec::verify;
use std::collections::BTreeMap;
use std::num::NonZeroU64;

/// The bounded ring's real cap (mirrors `aterm_buffer::MAX_LOG_EVENTS = 1<<16`).
/// The conformance spec MUST use the SAME cap the code uses, or eviction (which
/// only triggers past the cap) would diverge spuriously.
const CAP: u64 = 1 << 16;

/// The scalar projection of the ring the spec reasons about: `seq` (total events
/// ever appended) and `lo` (the oldest still-live seq; 1 when empty). This is the
/// `Refines`-style projection from concrete `EventLog` state to abstract vars.
fn project(s: &Surface) -> (u64, u64) {
    let seq = s.seq().0;
    let lo = s.log().live().next().map(|e| e.seq.0).unwrap_or(1);
    (seq, lo)
}

/// A two-step `ty` trace: `prev` (must match `Init`) then `next` (must match the
/// `Push` action). `ty` strictly enforces both, so this is a strict check that
/// the spec's `Next` admits the real `prev -> next` transition. Module name is
/// `Ring` — it must match the derived spec's `---- MODULE Ring ----`.
///
/// Validate one real transition against the derived ring spec, TIERED: the
/// in-process interpreter's `Next`-admission check always runs, and `ty trace
/// validate` additionally validates the two-step trace wherever installed (the
/// verdicts must agree — see [`verify::validate_transition_tiered`]). The spec
/// is DERIVED from the SAME `ring_model()` Tier-0 exhaustively checks — one
/// Rust source feeds both. The cfg pins `Init` to `prev` and overrides the
/// bounds to the real ring (`Cap` = `MAX_LOG_EVENTS`, `MaxSeq` large enough
/// that the action's guard never blocks a real transition).
fn validate_transition(prev: (u64, u64), next: (u64, u64)) -> (bool, String) {
    let m = ring_model();
    let state = |(seq, lo): (u64, u64)| -> BTreeMap<&'static str, i64> {
        [("seq", seq as i64), ("lo", lo as i64)].into_iter().collect()
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
    let mut surface = Surface::new(SurfaceId(NonZeroU64::new(1).unwrap()));
    let mut states = vec![project(&surface)];
    for i in 0..n {
        surface.apply(&WriteCap, Edit::AppendLine(format!("line {i}")));
        states.push(project(&surface));
    }
    states
}

#[test]
fn real_eventlog_conforms_to_ring_spec() {

    let dir = std::env::temp_dir().join(format!("aterm-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

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

    // POSITIVE: a spread of real transitions must each strictly conform to Next.
    // (In the no-eviction regime every transition has the same shape, so a spread
    // is representative; we include the first and a deep one explicitly.)
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

    // NEGATIVE CONTROL — corrupt a real transition's target two ways; ty MUST
    // reject each, proving the conformance check is not vacuous.
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

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "EventLog Tier-1 conformance: {} real transitions strictly validated against Ring spec; \
         negative controls (seq skip, wrong lo) both rejected.",
        sample.len()
    );
}

/// EVICTION regime: drive the real `EventLog` PAST its cap (`MAX_LOG_EVENTS`) so
/// eviction actually fires, and conformance-validate the transitions where the
/// ring head `lo` advances — the genuinely interesting ring behaviour the
/// no-eviction test never reaches. This closes the scope caveat: the real
/// eviction discipline (pop-oldest-when-over-cap) is bound to the derived spec.
#[test]
fn real_eventlog_eviction_conforms_to_ring_spec() {
    // Resolve `ty` BEFORE the heavy drive so an absent-toolchain run fails cheaply,
    // without first driving CAP+4 appends.

    let dir = std::env::temp_dir().join(format!("aterm-evict-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    // Drive just past the cap so eviction begins (CAP+4 appends).
    let n = CAP + 4;
    let states = drive_real_eventlog(n);

    // The real eviction discipline, observed: lo stays 1 up to and including
    // seq == Cap (ring exactly full), then advances 1-per-append as the oldest
    // events are popped (lo == seq - Cap + 1 once over the cap).
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

    // POSITIVE: every transition spanning the onset of eviction must conform to
    // the DERIVED spec's `Next` (lo advances exactly when the window exceeds Cap).
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

    // NEGATIVE CONTROL — in the eviction regime a transition that FAILS to evict
    // (lo held instead of advancing) must be rejected: it proves the spec enforces
    // eviction, not just monotone seq.
    let i = (CAP + 1) as usize;
    let prev = states[i]; // (Cap+1, 2) — next real step evicts, lo -> 3
    let next_seq = states[i + 1].0;
    let (held_ok, o) = validate_transition(prev, (next_seq, prev.1));
    assert!(
        !held_ok,
        "a non-evicting transition in the eviction regime MUST fail conformance\n{o}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "EventLog eviction conformance: drove {n} real appends (past Cap={CAP}); eviction \
         transitions strictly validated; non-evicting negative control rejected."
    );
}
