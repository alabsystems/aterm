// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified total retention limit (audit E1): derived TLA+ spec + Tier-1 binding
//! to the REAL `Grid`.
//!
//! Three layers, per `docs/RFC-ty-embed-derived-tla.md` (one Rust `ty_model!`
//! source feeds all of them, `include!`d from `retention_limit_spec.rs` — the
//! SAME file the COMPILE-TIME gate in `build.rs` checks, so no layer can check
//! a different spec):
//!
//!   Tier 0 (also enforced at COMPILE TIME by build.rs): `TotalBounded`
//!     (ring + store <= the ONE limit) proves at `Buggy=0` and is CAUGHT at
//!     `Buggy=1` — the pre-unification store-cap-equals-limit split.
//!
//!   Tier 1 (THIS file's heart): every bounded Scroll trace is driven against a
//!     real tiered `Grid` — real `line_feed` scroll-off, real lazy-buffer
//!     drain, real store truncation — in LOCKSTEP with the model, with the
//!     REAL measured (ring, store) substituted into each transition so the
//!     spec judges the machine, not the model echoing itself.
//!
//!   Negative controls: a forged observation must be REJECTED, and the
//!     RESURRECTED pre-unification configuration (store capped at the full
//!     limit, ring riding on top) must be rejected by the `Buggy=0` spec when
//!     driven on the real grid — so a green run is never vacuous and the bug
//!     the unification fixed stays expressible + caught.

use aterm_scrollback::{Scrollback, ScrollbackStorage};
use aterm_spec::derive::Model;
use aterm_spec::ty_model;
use aterm_spec::verify;
use std::collections::BTreeMap;

use aterm_grid::Grid;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/retention_limit_spec.rs"
));

// Must match the spec's consts (pinned again in the cfg overrides below).
const RING_CAP: i64 = 2;
const LIMIT: i64 = 3;
const MAX_PRODUCED: usize = 7; // the model's run bound is `produced <= 6` + 1 firing

const ROWS: u16 = 3;
const COLS: u16 = 20;

// ---------------------------------------------------------------------------
// Real-Grid driver: the LITERAL objects under test.
// ---------------------------------------------------------------------------

fn feed_line(g: &mut Grid, s: &str) {
    g.set_cursor(ROWS - 1, 0);
    for c in s.chars() {
        g.write_char(c);
    }
    g.line_feed();
    g.carriage_return();
}

/// A real tiered grid with the UNIFIED total limit applied through the
/// production entry point (`Grid::set_scrollback_line_limit`).
fn real_unified_grid() -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(4, 8, 8_000_000).into();
    let mut g = Grid::with_tiered_scrollback(ROWS, COLS, RING_CAP as usize, sb);
    g.set_scrollback_line_limit(Some(LIMIT as usize));
    assert_eq!(
        g.scrollback_line_limit(),
        Some(LIMIT as usize),
        "the ONE total round-trips"
    );
    g
}

/// Project the REAL grid onto the model's variables at a quiescent point
/// (lazy buffer drained): `ring` = ring-buffer scrollback, `store` = tiered
/// store line count. Draining is part of the projection, not a perturbation —
/// the model's Scroll is the quiescent-state transition.
fn project(g: &mut Grid) -> (i64, i64) {
    let store = g
        .scrollback_mut() // drains the lazy buffer first
        .map_or(0, |sb| sb.line_count() as i64);
    (g.ring_buffer_scrollback() as i64, store)
}

/// TIERED per-transition conformance (see [`verify::validate_transition_tiered`]):
/// the interpreter's `Next`-admission check always runs; `ty trace validate`
/// additionally validates the two-step trace wherever installed (verdicts must
/// agree). Cfg pins `Init` to `prev` with the real regime consts.
fn validate_transition(
    m: &Model,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
) -> (bool, String) {
    verify::validate_transition_tiered(
        m,
        &[("RingCap", RING_CAP), ("Limit", LIMIT), ("Buggy", 0)],
        prev,
        next,
        Some("Scroll"),
        "unified retention limit Tier-1 binding",
    )
}

/// Drive `n` Scrolls on the REAL grid in lockstep with the model; return the
/// per-step (prev, next) pairs with the REAL measured (ring, store)
/// substituted into every `next`.
#[allow(clippy::type_complexity)]
fn run_lockstep(n: usize) -> Vec<(BTreeMap<&'static str, i64>, BTreeMap<&'static str, i64>)> {
    let m = retention_limit_model();
    let mut g = real_unified_grid();
    let mut st = m.init_state();
    let mut steps = Vec::new();

    for i in 0..n {
        let prev = st.clone();
        let ok = m.fire("Scroll", &mut st);
        assert!(ok, "model guard rejects Scroll at step {i}");
        feed_line(&mut g, &format!("L{i}"));

        // Substitute the REAL measured outcome so ty judges the real grid.
        let (real_ring, real_store) = project(&mut g);
        let mut next = st.clone();
        next.insert("ring", real_ring);
        next.insert("store", real_store);
        st = next.clone(); // carry the real measurement forward

        // The invariant-bearing REAL observable: total retention never
        // exceeds the ONE limit at a quiescent point.
        assert!(
            g.scrollback_lines() as i64 <= LIMIT,
            "unified limit violated on the REAL grid at step {i}: {} > {LIMIT}",
            g.scrollback_lines()
        );
        steps.push((prev, next));
    }
    g.assert_structural_invariants();
    steps
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// Tier 0 backstop (the build.rs gate runs the same check at compile time; this
/// duplicates it in the test suite so `cargo test` alone also proves it).
#[test]
fn retention_limit_spec_model_checked_in_trust() {
    verify::prove_and_catch_scalar(&retention_limit_model(), "unified retention limit spec");
}

/// Tier 1: every bounded Scroll run (0..=MAX_PRODUCED steps), driven on the
/// REAL Grid in lockstep with the model, every transition's real measurement
/// judged against the spec's Next.
#[test]
fn real_grid_retention_conforms_to_spec() {
    let m = retention_limit_model();
    for n in 0..=MAX_PRODUCED {
        let steps = run_lockstep(n);
        for (i, (prev, next)) in steps.iter().enumerate() {
            let (ok, out) = validate_transition(&m, prev, next);
            assert!(
                ok,
                "ty REJECTED real Scroll transition {i} of a {n}-step run\nprev={prev:?}\nnext={next:?}\n{out}"
            );
        }
    }

    // NEGATIVE CONTROL 1: a forged observation must be REJECTED — claim one
    // MORE retained store line than the real grid measured.
    let steps = run_lockstep(MAX_PRODUCED);
    let (prev, next) = steps.last().expect("non-empty run").clone();
    let mut forged = next.clone();
    forged.insert("store", next["store"] + 1);
    let (ok, _) = validate_transition(&m, &prev, &forged);
    assert!(
        !ok,
        "negative control: ty must reject a forged store count (binding is vacuous otherwise)"
    );
}

/// NEGATIVE CONTROL 2 — the resurrected PRE-UNIFICATION configuration is
/// caught on the REAL grid: cap the store at the FULL limit (the old
/// store-cap-equals-limit split) via the raw store API, ride the ring on top,
/// and the real machine retains `limit + ring` lines — which the `Buggy=0`
/// spec must reject and the unified invariant assertion must flag.
#[test]
fn resurrected_pre_unification_split_is_rejected() {
    let m = retention_limit_model();
    let sb: ScrollbackStorage = Scrollback::new(4, 8, 8_000_000).into();
    let mut g = Grid::with_tiered_scrollback(ROWS, COLS, RING_CAP as usize, sb);
    // The OLD semantics: the store alone is capped at the user's limit; the
    // ring keeps retaining its full cap on top (double retention).
    g.scrollback_mut()
        .expect("store attached")
        .set_line_limit(Some(LIMIT as usize));

    let mut st = m.init_state();
    let mut rejected = false;
    for i in 0..MAX_PRODUCED {
        let prev = st.clone();
        assert!(m.fire("Scroll", &mut st));
        feed_line(&mut g, &format!("B{i}"));
        let store = g.scrollback_mut().map_or(0, |sb| sb.line_count() as i64);
        let ring = g.ring_buffer_scrollback() as i64;
        let mut next = st.clone();
        next.insert("ring", ring);
        next.insert("store", store);
        st = next.clone();
        let (ok, _) = validate_transition(&m, &prev, &next);
        if !ok {
            rejected = true;
            break;
        }
    }
    assert!(
        rejected,
        "the pre-unification split must produce a transition the unified spec rejects"
    );
    assert!(
        g.scrollback_lines() as i64 > LIMIT,
        "and the real double-retention overshoot is observable ({} > {LIMIT})",
        g.scrollback_lines()
    );
}
