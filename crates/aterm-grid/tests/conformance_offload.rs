// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scrollback-offload detach window: derived spec plus Tier-1 binding to the
//! real `Grid`.
//!
//! The `ty_model!` source is `include!`d from `offload_window_spec.rs`, the same
//! file `build.rs` checks at compile time.
//!
//! - Tier 0: `ty check` proves the invariants over every Produce/Erase/config/
//!   replacement/Reattach/Abort interleaving and requires a counterexample at
//!   `Buggy=1`.
//! - Tier 1: every bounded history interleaving, and every distinct
//!   detached-settings ordering class, is driven against a real tiered `Grid`
//!   in lockstep with the model; each real transition is judged against the
//!   spec's `Next` by `ty trace validate`.
//! - Negative controls: corrupted observations are rejected, and the
//!   resurrection scanner finds pre-erase tags before the erase.
//!
//! An absent Trust `ty` fails these tests.

use aterm_scrollback::{Line, Scrollback, ScrollbackStorage};
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
const BUDGET_UNIT: usize = 1_000_000;
const INITIAL_BUDGET: usize = 8 * BUDGET_UNIT;
const LINE_LOW: usize = 5;
const LINE_HIGH: usize = 20;
const BUDGET_LOW: usize = 2 * BUDGET_UNIT;
const BUDGET_HIGH: usize = 3 * BUDGET_UNIT;
const REPLACEMENT_BUDGET: usize = 4 * BUDGET_UNIT;

// Real-Grid driver

fn feed_line(g: &mut Grid, s: &str) {
    g.set_cursor(ROWS - 1, 0);
    for c in s.chars() {
        g.write_char(c);
    }
    g.line_feed();
    g.carriage_return();
}

/// Saturate the detach-window ring so the next scroll-off stages into the lazy
/// buffer.
///
/// The window opens with an empty ring (its rows go to the detached job), and
/// the model's `Produce` means "a line scrolls off and is staged", which holds
/// only once the ring is full. These lines are un-modelled environment output
/// (`produced`/`retained` stay 0); the precondition is asserted both ways.
fn saturate_window_ring(g: &mut Grid) {
    for i in 0..RING {
        if g.ring_buffer_scrollback() >= RING {
            break;
        }
        feed_line(g, &format!("RINGFILL{i}"));
    }
    assert_eq!(
        g.ring_buffer_scrollback(),
        RING,
        "ring must be saturated before the modelled window output"
    );
    assert_eq!(
        g.lazy_backlog_len(),
        0,
        "saturating the ring must not itself stage a row"
    );
}

/// A real tiered grid with `H` short (rewrap-count-stable) tagged history lines.
fn real_grid_with_history() -> Grid {
    let mut sb: ScrollbackStorage = Scrollback::new(64, 512, INITIAL_BUDGET).into();
    // The model uses `0` for unlimited lines; make that initial state literal.
    sb.set_line_limit(None);
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

fn store_contains(g: &Grid, needle: &str) -> bool {
    let Some(store) = g.scrollback() else {
        return false;
    };
    (0..store.line_count()).any(|i| {
        store
            .get_line(i)
            .ok()
            .flatten()
            .and_then(|line| line.as_str().map(|text| text.contains(needle)))
            .unwrap_or(false)
    })
}

fn line_code(limit: Option<usize>) -> i64 {
    limit.map_or(0, |value| {
        i64::try_from(value).expect("bounded test line limit fits i64")
    })
}

fn budget_code(budget: Option<usize>) -> i64 {
    budget.map_or(0, |bytes| {
        assert_eq!(
            bytes % BUDGET_UNIT,
            0,
            "bounded test budgets are whole-MiB codes"
        );
        i64::try_from(bytes / BUDGET_UNIT).expect("bounded test budget code fits i64")
    })
}

/// Substitute every settings value the real grid/store exposes into the model
/// state. `pending` and the dirty bits have no public projection and stay
/// model-driven.
fn project_real_settings(st: &mut BTreeMap<&'static str, i64>, g: &Grid) {
    let detached = i64::from(g.reflow_offload_in_flight());
    let backend = i64::from(g.scrollback().is_some());
    let observed_limit = g.scrollback_line_limit();
    st.insert("detached", detached);
    st.insert("backend", backend);
    st.insert("line_observed", line_code(observed_limit));
    st.insert("budget_observed", budget_code(g.scrollback_memory_budget()));

    match g.scrollback() {
        Some(store) => {
            let applied_line = match (store.line_limit(), observed_limit) {
                (None, None) => 0,
                (Some(raw), Some(total)) => {
                    let real_ring_limit = total
                        .checked_sub(raw)
                        .expect("unified total includes the raw store share");
                    st.insert(
                        "ring_limit",
                        i64::try_from(real_ring_limit).expect("bounded ring limit fits i64"),
                    );
                    line_code(Some(total))
                }
                pair => panic!("raw/effective line-limit option mismatch: {pair:?}"),
            };
            st.insert("line_applied", applied_line);
            st.insert("budget_applied", budget_code(Some(store.memory_budget())));
        }
        None => {
            st.insert("line_applied", 0);
            st.insert("budget_applied", 0);
            // Once a backend-less Abort resolves the window, the public total is
            // the surviving ring cap; during detach the getter shows the pending
            // request instead.
            if detached == 0
                && let Some(limit) = observed_limit
            {
                st.insert(
                    "ring_limit",
                    i64::try_from(limit).expect("bounded ring limit fits i64"),
                );
            }
        }
    }
}

fn real_grid_for_settings() -> Grid {
    let mut sb: ScrollbackStorage = Scrollback::new(64, 512, INITIAL_BUDGET).into();
    sb.set_line_limit(None);
    Grid::with_tiered_scrollback(ROWS, OLD_COLS, RING, sb)
}

fn replacement_store() -> ScrollbackStorage {
    let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, REPLACEMENT_BUDGET).into();
    replacement.set_line_limit(Some(LINE_HIGH - RING));
    replacement
        .push_line(Line::from("CONFIG_REPLACEMENT_SENTINEL"))
        .expect("seed replacement store");
    replacement
}

/// Project the replacement-backed history accounting: the replacement starts
/// with one sentinel line, every further store line was drained from this
/// window, and `lazy_backlog_len` is the still-staged remainder.
fn project_replacement_history(st: &mut BTreeMap<&'static str, i64>, g: &Grid) {
    let relocated = g.scrollback().map_or(0, |store| {
        assert!(
            store_contains(g, "CONFIG_REPLACEMENT_SENTINEL"),
            "targeted projection requires the authoritative replacement"
        );
        store.line_count().saturating_sub(1)
    });
    st.insert(
        "relocated",
        i64::try_from(relocated).expect("bounded relocated count fits i64"),
    );
    st.insert(
        "retained",
        i64::try_from(g.lazy_backlog_len()).expect("bounded staged count fits i64"),
    );
}

// ---------------------------------------------------------------------------
// ty-as-judge: strictly validate one projected transition against the spec's Next
// (the `conformance_eventlog.rs` method: open Init, the trace's step 0 is `prev`).
// ---------------------------------------------------------------------------

/// TIERED per-transition conformance (see [`verify::validate_transition_tiered`]):
/// the interpreter's `Next`-admission check always runs; `ty trace validate`
/// additionally validates the two-step trace wherever installed (verdicts must
/// agree). The trace's step 0 is `prev`; the cfg carries the real regime overrides.
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

// The bounded trace alphabet, enumerated exhaustively.

#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    Produce,
    Erase,
    Reattach,
    Abort,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum ConfigStep {
    SetLineLow,
    SetLineHigh,
    SetLineUnlimited,
    SetBudgetLow,
    SetBudgetHigh,
    AttachReplacement,
    Erase,
    Reattach,
    Abort,
}

/// All interleavings: k Produces (0..=W), an optional Erase at every position,
/// ending in Reattach or Abort (the space Tier 0 explores).
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

/// One representative of every semantically distinct settings ordering:
/// latest-writer order, low/high/unlimited fallback-ring policy, replacement
/// before/between/after setters, untouched-baseline adoption, and both terminal
/// paths.
fn settings_traces() -> Vec<Vec<ConfigStep>> {
    use ConfigStep::{
        Abort, AttachReplacement, Erase, Reattach, SetBudgetHigh, SetBudgetLow, SetLineHigh,
        SetLineLow, SetLineUnlimited,
    };
    vec![
        // Backend-less Abort: only the newest LOW finite value may tighten ring.
        vec![SetLineLow, Abort],
        vec![SetLineHigh, Abort],
        vec![SetLineLow, SetLineHigh, Abort],
        vec![SetLineHigh, SetLineLow, Abort],
        vec![SetLineLow, SetLineUnlimited, Abort],
        vec![SetLineUnlimited, SetLineLow, Abort],
        vec![SetBudgetLow, Abort],
        // Clean re-attach: latest finite/unlimited line values and budget values.
        vec![SetLineLow, SetLineHigh, Reattach],
        vec![SetLineHigh, SetLineLow, Reattach],
        vec![SetLineLow, SetLineUnlimited, Reattach],
        vec![SetBudgetLow, SetBudgetHigh, Reattach],
        vec![SetBudgetHigh, SetBudgetLow, Reattach],
        // Clean replacement adopts its baseline; Abort and Reattach both consume.
        vec![AttachReplacement, Reattach],
        vec![AttachReplacement, Abort],
        // A dirty field wins while the untouched sibling adopts replacement.
        vec![SetLineLow, AttachReplacement, Reattach],
        vec![SetBudgetLow, AttachReplacement, Reattach],
        // Clear-generation replacement of the stale worker store preserves config.
        vec![
            SetLineLow,
            SetBudgetLow,
            Erase,
            SetLineHigh,
            SetBudgetHigh,
            Reattach,
        ],
        // Setters after replacement apply immediately; the final setter wins.
        vec![
            AttachReplacement,
            SetLineLow,
            SetLineHigh,
            SetBudgetHigh,
            SetBudgetLow,
            Reattach,
        ],
        vec![
            SetLineLow,
            SetBudgetLow,
            AttachReplacement,
            SetLineHigh,
            SetBudgetHigh,
            Abort,
        ],
        // Explicit unlimited remains distinguishable from "not dirty".
        vec![
            SetLineHigh,
            SetBudgetHigh,
            AttachReplacement,
            SetLineUnlimited,
            SetBudgetLow,
            Reattach,
        ],
    ]
}

/// Drive one trace against the real grid in lockstep with the model; returns
/// the final model state and per-step `(prev, next, action)`, with the final
/// step's `retained` replaced by the real measurement.
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

    // Open the window on the real grid (the literal production entry point).
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

        // `detached` comes from the real flag accessor, not store presence, so a
        // flag that outlives Reattach/Abort diverges here.
        assert_eq!(
            i64::from(g.reflow_offload_in_flight()),
            st["detached"],
            "detached-flag projection diverged at step {i} of {trace:?}"
        );
        let store_expected_none = st["backend"] == 0;
        assert_eq!(
            g.scrollback().is_none(),
            store_expected_none,
            "store presence diverged at step {i} of {trace:?}"
        );

        let mut next = st.clone();
        // Final step: substitute the real measured `retained`, so `ty` judges the
        // grid rather than the model echoing itself.
        if i == trace.len() - 1 && *s == Step::Reattach {
            let baseline = if cleared { 0 } else { H as i64 };
            let real_retained = g.scrollback_lines() as i64 - baseline;
            next.insert("retained", real_retained);
        }
        project_real_settings(&mut next, &g);
        st = next.clone(); // carry every real projection into the next predecessor
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

/// Drive one detached-settings schedule against the public Grid API, projecting
/// getters, raw store settings, backend presence and the inferred ring share
/// into each successor before `ty` judges it.
#[allow(clippy::type_complexity)]
fn run_settings_lockstep(
    trace: &[ConfigStep],
) -> (
    Grid,
    Vec<(
        BTreeMap<&'static str, i64>,
        BTreeMap<&'static str, i64>,
        &'static str,
    )>,
) {
    let m = offload_window_model();
    let mut g = real_grid_for_settings();
    let mut pending = Some(
        g.resize_offloading_scrollback(ROWS, NEW_COLS)
            .expect("width change with tiered store opens settings detach window"),
    );
    assert_eq!(g.scrollback_line_limit(), None);
    assert_eq!(g.scrollback_memory_budget(), Some(INITIAL_BUDGET));

    let mut st = m.init_state();
    let mut steps = Vec::new();
    for (i, step) in trace.iter().enumerate() {
        let prev = st.clone();
        let (name, ok): (&'static str, bool) = match step {
            ConfigStep::SetLineLow => {
                g.set_scrollback_line_limit(Some(LINE_LOW));
                ("SetLineLow", m.fire("SetLineLow", &mut st))
            }
            ConfigStep::SetLineHigh => {
                g.set_scrollback_line_limit(Some(LINE_HIGH));
                ("SetLineHigh", m.fire("SetLineHigh", &mut st))
            }
            ConfigStep::SetLineUnlimited => {
                g.set_scrollback_line_limit(None);
                ("SetLineUnlimited", m.fire("SetLineUnlimited", &mut st))
            }
            ConfigStep::SetBudgetLow => {
                g.set_scrollback_memory_budget(BUDGET_LOW)
                    .expect("bounded low budget applies");
                ("SetBudgetLow", m.fire("SetBudgetLow", &mut st))
            }
            ConfigStep::SetBudgetHigh => {
                g.set_scrollback_memory_budget(BUDGET_HIGH)
                    .expect("bounded high budget applies");
                ("SetBudgetHigh", m.fire("SetBudgetHigh", &mut st))
            }
            ConfigStep::AttachReplacement => {
                g.attach_scrollback(replacement_store());
                ("AttachReplacement", m.fire("AttachReplacement", &mut st))
            }
            ConfigStep::Erase => {
                g.erase_scrollback();
                ("Erase", m.fire("Erase", &mut st))
            }
            ConfigStep::Reattach => {
                let job = pending.take().expect("terminal step consumes worker job");
                g.reattach_reflowed_scrollback(job.reflow());
                ("Reattach", m.fire("Reattach", &mut st))
            }
            ConfigStep::Abort => {
                drop(pending.take().expect("terminal step consumes worker job"));
                g.abort_reflow_offload();
                ("Abort", m.fire("Abort", &mut st))
            }
        };
        assert!(
            ok,
            "model guard rejects {name} at step {i} of settings trace {trace:?}"
        );

        let mut next = st.clone();
        project_real_settings(&mut next, &g);
        st = next.clone();
        steps.push((prev, next, name));
    }

    assert!(
        pending.is_none(),
        "settings traces must resolve by Reattach or Abort"
    );
    g.assert_structural_invariants();
    (g, steps)
}

// Tests

/// Tier 0 backstop (the build.rs gate runs the same check at compile time; this
/// duplicates it in the test suite so `cargo test` alone also proves it).
#[test]
fn offload_window_spec_model_checked_in_trust() {
    // Interpreter prove-and-catch always; ty too where installed.
    verify::prove_and_catch_scalar(
        &offload_window_model(),
        "scrollback-offload detach-window spec",
    );
}

/// Tier 1: every bounded interleaving on the real Grid in lockstep with the
/// model, judged by `ty trace validate`.
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

    // Negative control 1: claiming one more retained line than measured is
    // rejected.
    let (_st, steps) = run_lockstep(&[Step::Produce, Step::Reattach]);
    let (prev, next, action) = steps.last().unwrap().clone();
    let mut forged = next.clone();
    forged.insert("retained", next["retained"] + 1);
    let (ok, _) = validate_transition(&m, &prev, &forged, action);
    assert!(
        !ok,
        "negative control: ty must reject a forged final observation (binding is vacuous otherwise)"
    );

    // Negative control 2: the resurrection scanner sees pre-erase tags.
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

/// Tier 1 settings binding: exercise every semantically distinct ordering class
/// against public getters and raw backend values, including replacement and abort.
#[test]
fn real_grid_detached_settings_conform_to_spec() {
    use ConfigStep::{Abort, AttachReplacement, Reattach, SetLineHigh, SetLineLow};

    let m = offload_window_model();
    let traces = settings_traces();
    assert!(
        traces.len() >= 18,
        "settings matrix must retain every ordering class"
    );

    for trace in &traces {
        let (g, steps) = run_settings_lockstep(trace);
        for (prev, next, action) in &steps {
            let (ok, out) = validate_transition(&m, prev, next, action);
            assert!(
                ok,
                "ty REJECTED real settings transition {action} in {trace:?}\n\
                 prev={prev:?}\nnext={next:?}\n{out}"
            );
        }

        if trace == &[AttachReplacement, Reattach] || trace == &[AttachReplacement, Abort] {
            assert!(
                store_contains(&g, "CONFIG_REPLACEMENT_SENTINEL"),
                "the stale worker must not clobber an authoritative replacement: {trace:?}"
            );
        }
    }

    // Settings negative control 1: a forged stale/high getter after SetLineLow
    // satisfies the scalar domain but not the action body.
    let (_g, steps) = run_settings_lockstep(&[SetLineLow, Reattach]);
    let (prev, next, action) = steps.first().expect("setter transition").clone();
    let mut forged = next.clone();
    forged.insert(
        "line_observed",
        i64::try_from(LINE_HIGH).expect("bounded line fits i64"),
    );
    let (ok, _) = validate_transition(&m, &prev, &forged, action);
    assert!(
        !ok,
        "negative control: ty must reject a forged deferred line getter"
    );

    // Settings negative control 2: a high request cannot expand the backend-less
    // ring on Abort.
    let (_g, steps) = run_settings_lockstep(&[SetLineHigh, Abort]);
    let (prev, next, action) = steps.last().expect("abort transition").clone();
    let mut forged = next.clone();
    let high = i64::try_from(LINE_HIGH).expect("bounded line fits i64");
    forged.insert("ring_limit", high);
    forged.insert("line_observed", high);
    let (ok, _) = validate_transition(&m, &prev, &forged, action);
    assert!(
        !ok,
        "negative control: ty must reject abort expanding the emergency ring"
    );

    eprintln!(
        "TRUST Tier-1: {} detached-settings schedules driven on the REAL Grid.",
        traces.len()
    );
}

/// Tier 1 replacement-loss binding: attaching a replacement drains rows already
/// staged in the detach window, and replacement-backed Abort drains rows staged
/// afterward. Unlike backend-less worker death, this close has no loss waiver.
#[test]
fn replacement_backed_abort_preserves_window_output_in_spec() {
    let m = offload_window_model();
    let mut g = real_grid_with_history();
    let pending = g
        .resize_offloading_scrollback(ROWS, NEW_COLS)
        .expect("open detach window");
    // RFL-1: the window opens with the ring history riding the job, so refill
    // the ring first — the modelled Produce steps below assume a full ring.
    saturate_window_ring(&mut g);

    let mut st = m.init_state();
    let mut transitions = Vec::new();

    // One full-ring eviction is staged before the replacement arrives.
    let prev = st.clone();
    feed_line(&mut g, "REPLACEMENT-PRE");
    assert!(m.fire("Produce", &mut st));
    let mut next = st.clone();
    project_real_settings(&mut next, &g);
    project_replacement_history(&mut next, &g);
    st = next.clone();
    transitions.push((prev, next, "Produce"));
    assert_eq!(g.lazy_backlog_len(), 1, "one pre-attach row staged");

    // Replacement attachment must immediately relocate that staged row.
    let prev = st.clone();
    g.attach_scrollback(replacement_store());
    assert!(m.fire("AttachReplacement", &mut st));
    let mut next = st.clone();
    project_real_settings(&mut next, &g);
    project_replacement_history(&mut next, &g);
    st = next.clone();
    transitions.push((prev, next, "AttachReplacement"));
    assert_eq!(g.lazy_backlog_len(), 0);
    assert_eq!(
        g.scrollback().expect("replacement attached").line_count(),
        2,
        "sentinel + pre-attach staged row"
    );

    // Keep the next eviction staged until Abort so that Abort's replacement
    // branch—not attachment—must perform the second conserving transfer.
    g.set_compress_offload_active(true);
    let prev = st.clone();
    feed_line(&mut g, "REPLACEMENT-POST");
    assert!(m.fire("Produce", &mut st));
    let mut next = st.clone();
    project_real_settings(&mut next, &g);
    project_replacement_history(&mut next, &g);
    st = next.clone();
    transitions.push((prev, next, "Produce"));
    assert_eq!(g.lazy_backlog_len(), 1, "one post-attach row staged");

    let prev = st.clone();
    drop(pending);
    g.abort_reflow_offload();
    assert!(m.fire("Abort", &mut st));
    let mut next = st.clone();
    project_real_settings(&mut next, &g);
    project_replacement_history(&mut next, &g);
    transitions.push((prev.clone(), next.clone(), "Abort"));
    assert_eq!(g.lazy_backlog_len(), 0);
    assert_eq!(
        g.scrollback()
            .expect("replacement survives Abort")
            .line_count(),
        3,
        "sentinel + both window rows"
    );

    for (prev, next, action) in &transitions {
        let (ok, out) = validate_transition(&m, prev, next, action);
        assert!(
            ok,
            "ty REJECTED replacement-loss transition {action}\n\
             prev={prev:?}\nnext={next:?}\n{out}"
        );
    }

    // Negative control: forge the old lossy Abort (second staged row vanished).
    let mut forged = next;
    forged.insert("relocated", forged["relocated"] - 1);
    let (ok, _) = validate_transition(&m, &prev, &forged, "Abort");
    assert!(
        !ok,
        "negative control: ty must reject replacement-backed Abort losing a staged row"
    );
    g.assert_structural_invariants();
}
