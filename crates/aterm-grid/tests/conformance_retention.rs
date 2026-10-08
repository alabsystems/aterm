// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified total retention limit: derived spec plus Tier-1 binding to the real
//! `Grid`.
//!
//! The `ty_model!` source is `include!`d from `retention_limit_spec.rs`, the
//! same file `build.rs` checks at compile time.
//!
//! - Tier 0: `TotalBounded` (ring + store <= the one limit) proves at `Buggy=0`
//!   and is caught at `Buggy=1` (store capped at the limit, ring on top).
//! - Tier 1: every bounded Scroll trace runs on a real tiered `Grid` (line_feed
//!   scroll-off, lazy-buffer drain, store truncation) in lockstep with the
//!   model, with the measured (ring, store) substituted into each transition.
//! - Negative controls: a forged observation is rejected, and the split
//!   configuration driven on the real grid is rejected by the `Buggy=0` spec.

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

// Real-Grid driver

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

/// Project the real grid onto the model at a quiescent point (lazy buffer
/// drained): `ring` = ring scrollback, `store` = tiered store line count. The
/// model's Scroll is the quiescent-state transition.
fn project(g: &mut Grid) -> (i64, i64) {
    let store = g
        .scrollback_mut() // drains the lazy buffer first
        .map_or(0, |sb| sb.line_count() as i64);
    (g.ring_buffer_scrollback() as i64, store)
}

/// TIERED per-transition conformance (see [`verify::validate_transition_tiered`]):
/// the interpreter's `Next`-admission check always runs; `ty trace validate`
/// additionally validates the two-step trace wherever installed (verdicts must
/// agree). The trace's step 0 is `prev`; the cfg carries the real regime consts.
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

/// Drive `n` Scrolls on the real grid in lockstep with the model; returns the
/// per-step (prev, next) pairs with the measured (ring, store) in every `next`.
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

        // Substitute the real measured outcome so ty judges the grid.
        let (real_ring, real_store) = project(&mut g);
        let mut next = st.clone();
        next.insert("ring", real_ring);
        next.insert("store", real_store);
        st = next.clone(); // carry the real measurement forward

        // The invariant-bearing real observable: total retention never
        // exceeds the one limit at a quiescent point.
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

// Tests

/// Tier 0 backstop (the build.rs gate runs the same check at compile time; this
/// duplicates it in the test suite so `cargo test` alone also proves it).
#[test]
fn retention_limit_spec_model_checked_in_trust() {
    verify::prove_and_catch_scalar(&retention_limit_model(), "unified retention limit spec");
}

/// Tier 1: every bounded Scroll run (0..=MAX_PRODUCED steps) on the real Grid,
/// each transition judged against the spec's `Next`.
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

    // Negative control 1: claiming one more store line than measured is
    // rejected.
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

/// Negative control 2: capping the store at the full limit via the raw store
/// API, with the ring on top, retains `limit + ring` lines on the real grid;
/// the `Buggy=0` spec rejects it and the unified invariant flags it.
#[test]
fn resurrected_pre_unification_split_is_rejected() {
    let m = retention_limit_model();
    let sb: ScrollbackStorage = Scrollback::new(4, 8, 8_000_000).into();
    let mut g = Grid::with_tiered_scrollback(ROWS, COLS, RING_CAP as usize, sb);
    // The old semantics: the store alone is capped at the user's limit; the
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
