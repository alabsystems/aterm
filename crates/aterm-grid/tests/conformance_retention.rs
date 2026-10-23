// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified total retention limit: the `ty_model!` spec (`include!`d from
//! `retention_limit_spec.rs`) bound to the real `Grid`.

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

const RING_CAP: i64 = 2;
const LIMIT: i64 = 3;
const MAX_PRODUCED: usize = 7; // the model's run bound is `produced <= 6` + 1 firing

const ROWS: u16 = 3;
const COLS: u16 = 20;

fn feed_line(g: &mut Grid, s: &str) {
    g.set_cursor(ROWS - 1, 0);
    for c in s.chars() {
        g.write_char(c);
    }
    g.line_feed();
    g.carriage_return();
}

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

fn project(g: &mut Grid) -> (i64, i64) {
    let store = g
        .scrollback_mut() // drains the lazy buffer first
        .map_or(0, |sb| sb.line_count() as i64);
    (g.ring_buffer_scrollback() as i64, store)
}

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

        let (real_ring, real_store) = project(&mut g);
        let mut next = st.clone();
        next.insert("ring", real_ring);
        next.insert("store", real_store);
        st = next.clone(); // carry the real measurement forward

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

#[test]
fn retention_limit_spec_model_checked_in_trust() {
    verify::prove_and_catch_scalar(&retention_limit_model(), "unified retention limit spec");
}

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

#[test]
fn resurrected_pre_unification_split_is_rejected() {
    let m = retention_limit_model();
    let sb: ScrollbackStorage = Scrollback::new(4, 8, 8_000_000).into();
    let mut g = Grid::with_tiered_scrollback(ROWS, COLS, RING_CAP as usize, sb);
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
