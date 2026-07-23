// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scrollback-offload detach-window: derived TLA+ spec + Tier-1 binding to the REAL `Grid`.
//!
//! Three layers, per `docs/RFC-ty-embed-derived-tla.md` (one Rust `ty_model!` source
//! feeds all of them, `include!`d from `offload_window_spec.rs` — the SAME file the
//! COMPILE-TIME gate in `build.rs` checks, so no layer can check a different spec):
//!
//!   Tier 0 (also enforced at COMPILE TIME by build.rs): `ty check` proves the
//!     invariants exhaustively over every Produce/Erase/Reattach/Abort interleaving,
//!     and requires a counterexample at `Buggy=1` (non-vacuity).
//!
//!   Tier 1 (THIS file's heart): the model is bound to the LITERAL shipping objects.
//!     Every bounded interleaving is driven against a real tiered `Grid` — real
//!     `resize_offloading_scrollback`, real `line_feed` scroll-off, real
//!     `erase_scrollback`, real `reflow`/`reattach_reflowed_scrollback`, real
//!     `abort_reflow_offload` — in LOCKSTEP with the model, and the REAL measured
//!     outcome is judged by `ty trace validate` against the spec's `Next`. If the
//!     code's behavior drifts from the model, this goes RED, forcing the model (and
//!     hence the auto-derived TLA+) to change with the code.
//!
//!   Negative controls: a corrupted final observation MUST be rejected by `ty`, and
//!     the resurrection scanner MUST find pre-erase tags before the erase — so a
//!     green run is never vacuous.
//!
//! VERIFICATION GATE (batteries-on): an absent Trust `ty` FAILS these tests.

use aterm_scrollback::{Scrollback, ScrollbackStorage};
use aterm_spec::derive::Model;
use aterm_spec::ty_model;
use aterm_spec::verify;
use std::collections::BTreeMap;

use aterm_grid::Grid;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/offload_window_spec.rs"
));

const W: i64 = 3; // must match the spec's `const W`
const RING: usize = 8; // small ring so history lives in the tiered store
const H: usize = 1200; // pre-window history lines (all tagged "H…")
const ROWS: u16 = 10;
const OLD_COLS: u16 = 80;
const NEW_COLS: u16 = 40; // width change triggers the offload window

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

/// A real tiered grid with `H` short (rewrap-count-stable) tagged history lines.
fn real_grid_with_history() -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut g = Grid::with_tiered_scrollback(ROWS, OLD_COLS, RING, sb);
    for i in 0..H {
        feed_line(&mut g, &format!("H{i}"));
    }
    assert!(g.scrollback_lines() >= H, "history built");
    g
}

/// Count store lines whose text starts with the pre-erase tag `H` (resurrection scan).
fn store_h_tags(g: &Grid) -> usize {
    let Some(store) = g.scrollback() else {
        return 0;
    };
    (0..store.line_count())
        .filter(|&i| {
            store
                .get_line(i)
                .ok()
                .flatten()
                .and_then(|l| l.as_str().map(|s| s.starts_with('H')))
                .unwrap_or(false)
        })
        .count()
}

// ---------------------------------------------------------------------------
// ty-as-judge: strictly validate one projected transition against the spec's Next
// (the `conformance_eventlog.rs` method: parameterized Init pinned to `prev`).
// ---------------------------------------------------------------------------

/// TIERED per-transition conformance (see [`verify::validate_transition_tiered`]):
/// the interpreter's `Next`-admission check always runs; `ty trace validate`
/// additionally validates the two-step trace wherever installed (verdicts must
/// agree). Cfg pins `Init` to `prev` with the real regime overrides.
fn validate_transition(
    m: &Model,
    prev: &BTreeMap<&'static str, i64>,
    next: &BTreeMap<&'static str, i64>,
    action: &str,
) -> (bool, String) {
    verify::validate_transition_tiered(
        m,
        &[("W", W), ("Buggy", 0)],
        prev,
        next,
        Some(action),
        "offload detach-window Tier-1 binding",
    )
}

// ---------------------------------------------------------------------------
// The bounded trace alphabet, enumerated exhaustively.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    Produce,
    Erase,
    Reattach,
    Abort,
}

/// All interleavings: k Produces (0..=W), an optional Erase at every position,
/// terminated by Reattach or Abort. Exhaustive over the same bounded space the
/// Tier-0 `ty check` explores.
fn all_traces() -> Vec<Vec<Step>> {
    let mut out = Vec::new();
    for k in 0..=(W as usize) {
        for terminal in [Step::Reattach, Step::Abort] {
            // No erase.
            let mut t: Vec<Step> = vec![Step::Produce; k];
            t.push(terminal);
            out.push(t.clone());
            // Erase at each possible position among the produces.
            for pos in 0..=k {
                let mut t: Vec<Step> = vec![Step::Produce; k];
                t.insert(pos, Step::Erase);
                t.push(terminal);
                out.push(t);
            }
        }
    }
    out
}

/// Drive ONE trace against the REAL grid in lockstep with the model; return
/// `(final model state, per-step (prev, next, action) list with the FINAL step's
/// `retained` substituted by the REAL measurement)`.
#[allow(clippy::type_complexity)]
fn run_lockstep(
    trace: &[Step],
) -> (
    BTreeMap<&'static str, i64>,
    Vec<(
        BTreeMap<&'static str, i64>,
        BTreeMap<&'static str, i64>,
        &'static str,
    )>,
) {
    let m = offload_window_model();
    let mut g = real_grid_with_history();

    // Open the window on the REAL grid (the literal production entry point).
    let mut pending = Some(
        g.resize_offloading_scrollback(ROWS, NEW_COLS)
            .expect("width change with tiered store opens the offload window"),
    );
    assert!(
        g.scrollback().is_none(),
        "window open: tiered store detached on the real grid"
    );

    let mut st = m.init_state();
    let mut steps = Vec::new();
    let mut w_i = 0usize; // window-line counter
    let mut cleared = false;

    for (i, s) in trace.iter().enumerate() {
        let prev = st.clone();
        let (name, ok): (&'static str, bool) = match s {
            Step::Produce => {
                feed_line(&mut g, &format!("W{w_i}"));
                w_i += 1;
                ("Produce", m.fire("Produce", &mut st))
            }
            Step::Erase => {
                g.erase_scrollback();
                cleared = true;
                ("Erase", m.fire("Erase", &mut st))
            }
            Step::Reattach => {
                let p = pending.take().expect("terminal step consumes the job");
                let reflowed = p.reflow();
                g.reattach_reflowed_scrollback(reflowed);
                ("Reattach", m.fire("Reattach", &mut st))
            }
            Step::Abort => {
                drop(pending.take().expect("terminal step consumes the job"));
                g.abort_reflow_offload();
                ("Abort", m.fire("Abort", &mut st))
            }
        };
        assert!(
            ok,
            "model guard rejects {name} at step {i} of {trace:?} — model/real enabledness diverged"
        );

        // Per-step observables. `detached` is projected from the REAL flag accessor
        // — not inferred from store presence — so a wedged window (a flag that
        // outlives Reattach/Abort, the un-drainable-lazy leak) diverges here. This
        // closed the abort-wedge blind spot the mutation matrix found: with the
        // flag-clear removed from `abort_reflow_offload`, this assertion goes RED.
        assert_eq!(
            i64::from(g.reflow_offload_in_flight()),
            st["detached"],
            "detached-flag projection diverged at step {i} of {trace:?}"
        );
        let store_expected_none = st["done"] == 0 || st["aborted"] == 1;
        assert_eq!(
            g.scrollback().is_none(),
            store_expected_none,
            "store presence diverged at step {i} of {trace:?}"
        );

        let mut next = st.clone();
        // TERMINAL step: substitute the REAL measured outcome for `retained`, so `ty`
        // judges the real grid's behavior, not the model echoing itself.
        if i == trace.len() - 1 && *s == Step::Reattach {
            let baseline = if cleared { 0 } else { H as i64 };
            let real_retained = g.scrollback_lines() as i64 - baseline;
            next.insert("retained", real_retained);
            st = next.clone(); // the model state carries the real measurement forward
        }
        steps.push((prev, next, name));
    }

    // Post-trace real-world assertions (the invariant-bearing observables).
    match trace.last().unwrap() {
        Step::Reattach => {
            if cleared {
                assert_eq!(
                    store_h_tags(&g),
                    0,
                    "ErasedStaysErased on the REAL grid: no pre-erase tag survives {trace:?}"
                );
            }
        }
        Step::Abort => {
            assert!(
                g.scrollback_lines() <= RING,
                "abort leaves a bounded ring-only grid (no wedge) {trace:?}"
            );
        }
        _ => unreachable!("traces end in a terminal step"),
    }
    g.assert_structural_invariants();
    (st, steps)
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// Tier 0 backstop (the build.rs gate runs the same check at compile time; this
/// duplicates it in the test suite so `cargo test` alone also proves it).
#[test]
fn offload_window_spec_model_checked_in_trust() {
    // TIERED (VERIFY-1): interpreter prove-and-catch always; ty additionally
    // wherever installed (the build.rs gate runs the same ty check at compile
    // time on Trust machines; this keeps `cargo test` alone proving it too).
    verify::prove_and_catch_tiered(
        &offload_window_model(),
        "scrollback-offload detach-window spec",
    );
}

/// Tier 1: EVERY bounded interleaving, driven on the REAL Grid in lockstep with the
/// model, with the real measured outcome judged by `ty trace validate`.
#[test]
fn real_grid_offload_window_conforms_to_spec_exhaustively() {
    let m = offload_window_model();
    let dir = std::env::temp_dir().join(format!("aterm-offload-t1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    let traces = all_traces();
    assert!(traces.len() >= 20, "exhaustive alphabet ({})", traces.len());

    for trace in &traces {
        let (_final_st, steps) = run_lockstep(trace);
        for (prev, next, action) in &steps {
            let (ok, out) = validate_transition(&m, prev, next, action);
            assert!(
                ok,
                "ty REJECTED real transition {action} in {trace:?}\nprev={prev:?}\nnext={next:?}\n{out}"
            );
        }
    }

    // NEGATIVE CONTROL 1: a corrupted real measurement must be REJECTED — take a
    // clean Produce->Reattach trace and claim one MORE retained line than measured.
    let (_st, steps) = run_lockstep(&[Step::Produce, Step::Reattach]);
    let (prev, next, action) = steps.last().unwrap().clone();
    let mut forged = next.clone();
    forged.insert("retained", next["retained"] + 1);
    let (ok, _) = validate_transition(&m, &prev, &forged, action);
    assert!(
        !ok,
        "negative control: ty must reject a forged final observation (binding is vacuous otherwise)"
    );

    // NEGATIVE CONTROL 2: the resurrection scanner actually sees pre-erase tags.
    let g = real_grid_with_history();
    assert!(
        store_h_tags(&g) > 0,
        "negative control: the tag scanner must find pre-erase history"
    );

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "TRUST Tier-1: {} interleavings driven on the REAL Grid, every transition judged by ty.",
        traces.len()
    );
}
