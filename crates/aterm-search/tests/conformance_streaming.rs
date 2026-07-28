// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Streaming search: derived TLA+ spec + Tier-1 binding to the REAL engine.
//!
//! Three layers, per `docs/RFC-ty-embed-derived-tla.md` (ONE registered Rust
//! model — `aterm_spec::derive::streaming_search_model()` — feeds all of them,
//! the SAME `Model` the compile-time gate in `build.rs`, the Tier-0 tests in
//! aterm-spec, the global strict-vacuity audit, and trust-ir spec-link check,
//! so no layer can check a different spec):
//!
//!   Tier 0 (also enforced at COMPILE TIME by build.rs): the invariants are
//!     proven exhaustively over every Start/Scan/Nav/Add/Invalidate/Reflow/
//!     Cancel interleaving, and a counterexample is required at `Buggy=1`
//!     (the dropped invalidation clamp — non-vacuity).
//!
//!   Tier 1 (THIS file's heart): the model is bound to the LITERAL shipping
//!     `StreamingSearch`. Every bounded interleaving of the unit-effect trace alphabet is
//!     driven against a real engine — real `start_search`, real `scan_row`
//!     (whose last-row auto-complete the model FOLDS into ScanHit/ScanMiss),
//!     real `next_match`/`prev_match`, real `content_added`/
//!     `content_invalidated`/`content_reflowed`, real `cancel` — in LOCKSTEP
//!     with the model. After every step the engine's public accessors are
//!     projected onto the model variables and must MATCH exactly, and every
//!     unique projected transition is judged by the tiered per-transition
//!     validator (interpreter `Next`-admission always; `ty trace validate`
//!     additionally wherever installed, verdicts must agree). If the code's
//!     unit-effect behavior drifts from the model, this goes RED, forcing the model (and
//!     hence the auto-derived TLA+) to change with the code.
//!     General calls that add or remove multiple matches atomically are outside
//!     this exact scalar abstraction; a boundary control below proves those
//!     transitions are rejected rather than silently overclaimed. Their broader
//!     invariants remain covered by local tests and Kani harnesses.
//!
//!   Negative controls: a forged observation MUST be rejected; a disabled model
//!     action MUST correspond to a real engine no-op; multi-effect shipping calls
//!     MUST be rejected by the unit-effect abstraction; the at-capacity
//!     counts-not-stores discipline MUST be exactly what the spec admits — so a
//!     green run is never vacuous.
//!
//! Driver discipline (the design-arounds that make the binding honest):
//!   * rows are fed IN ORDER at the engine's own `scan_progress` (`scan_row`
//!     silently no-ops on any other row);
//!   * `Add` uses a FRESH unique row each time so dedup never fires and exactly
//!     one match stores-or-counts;
//!   * `Invalidate` targets the row of a currently STORED result (invalidating
//!     a counted-not-stored row would remove 0 and diverge — the model's
//!     `stored-1 / total-1` matches the engine's removed-stored-only
//!     accounting);
//!   * `Rows`/`MaxTotal` are trace-window bounds the driver stays inside, NOT
//!     engine claims.

use std::collections::{BTreeMap, BTreeSet};

use aterm_search::streaming::{FilterMode, SearchState, StreamingSearch, StreamingSearchConfig};
use aterm_spec::derive::{Model, streaming_search_model};
use aterm_spec::verify;

// Must match the model's consts (asserted in `model_consts_match_driver`).
const ROWS: usize = 3;
const MAX_RESULTS: usize = 2;
const MAX_TOTAL: i64 = 4;

/// Bounded trace depth: Start + a full 3-row scan + 3 post-scan operations
/// (deep enough for capacity, wrap, invalidate-to-empty, revive, and
/// restart-after-Cancel/Reflow shapes to all appear).
const MAX_DEPTH: usize = 7;

type State = BTreeMap<&'static str, i64>;
/// A projected state flattened into an Ord-able key for the unique-transition set.
type StateKey = Vec<(&'static str, i64)>;

/// The trace alphabet — model action names; the driver binds each to one
/// concrete engine call.
const ALPHABET: [&str; 9] = [
    "Start",
    "ScanHit",
    "ScanMiss",
    "NextMatch",
    "PrevMatch",
    "Add",
    "Invalidate",
    "Reflow",
    "Cancel",
];

/// PROJECTION (`conformance_streaming::project`, named by the `#[refines]`
/// anchors in engine/operations.rs): the real engine's public accessors onto
/// the model variables. `scanp` encodes `scan_progress + 1` (0 = the engine's
/// idle `-1`); `state` is the 0..3 enum order.
pub fn project(e: &StreamingSearch) -> State {
    let state = match e.state() {
        SearchState::Idle => 0,
        SearchState::Searching => 1,
        SearchState::HasResults => 2,
        SearchState::NoResults => 3,
        // `SearchState` is #[non_exhaustive]; a new variant must extend the
        // model too, so failing loudly here is the correct drift alarm.
        other => panic!("unmodeled SearchState variant {other:?}"),
    };
    let mut st = State::new();
    st.insert("state", state);
    st.insert("scanp", e.scan_progress() as i64 + 1);
    st.insert("stored", e.result_count() as i64);
    st.insert("total", e.total_matches() as i64);
    st.insert("cur", e.current_index() as i64);
    st
}

/// Enumerate EVERY model-enabled action sequence of length <= `MAX_DEPTH` from
/// Init (DFS over the model's own guards — the same bounded space Tier-0
/// exhausts). Enabledness-by-construction, the offload `all_traces()` idiom;
/// the engine-side agreement is the per-step projection assert in
/// `run_lockstep`, plus the explicit disabled-action negative control.
fn all_traces(m: &Model) -> Vec<Vec<&'static str>> {
    let mut out = Vec::new();
    let mut trace: Vec<&'static str> = Vec::new();
    fn dfs(m: &Model, st: &State, trace: &mut Vec<&'static str>, out: &mut Vec<Vec<&'static str>>) {
        if trace.len() >= MAX_DEPTH {
            return;
        }
        for a in ALPHABET {
            let mut next = st.clone();
            if m.fire(a, &mut next) {
                trace.push(a);
                out.push(trace.clone());
                dfs(m, &next, trace, out);
                trace.pop();
            }
        }
    }
    dfs(m, &m.init_state(), &mut trace, &mut out);
    out
}

/// Drive ONE trace against a fresh REAL engine in lockstep with the model;
/// assert projection equality after every step and return the per-step
/// `(prev, next, action)` list.
fn run_lockstep(
    m: &Model,
    wrap: bool,
    trace: &[&'static str],
) -> Vec<(State, State, &'static str)> {
    let mut e = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: MAX_RESULTS,
        wrap_enabled: wrap,
        ..StreamingSearchConfig::default()
    });
    let mut st = m.init_state();
    let mut steps = Vec::new();
    let mut fresh_row = 100usize; // unique Add rows: dedup can never fire

    for (i, &a) in trace.iter().enumerate() {
        let prev = st.clone();
        match a {
            "Start" => {
                e.start_search("t", FilterMode::Literal)
                    .expect("start_search with a valid pattern");
            }
            // The scan rows are fed IN ORDER at the engine's own frontier;
            // "t" hits (one match at col 0), "x" misses. The last row's
            // auto-complete happens INSIDE this one call (folded).
            "ScanHit" => {
                let row = usize::try_from(e.scan_progress()).expect("scanning row");
                assert_eq!(e.scan_row(row, "t", ROWS), 1, "hit row yields one match");
            }
            "ScanMiss" => {
                let row = usize::try_from(e.scan_progress()).expect("scanning row");
                assert_eq!(e.scan_row(row, "x", ROWS), 0, "miss row yields no match");
            }
            "NextMatch" => e.next_match(),
            "PrevMatch" => e.prev_match(),
            "Add" => {
                e.content_added(fresh_row, "t");
                fresh_row += 1;
            }
            // Invalidate the row of a currently STORED result (see file doc).
            "Invalidate" => {
                let row = e
                    .results()
                    .last()
                    .expect("Invalidate needs a stored result")
                    .row;
                e.content_invalidated(row, row);
            }
            "Reflow" => e.content_reflowed(),
            "Cancel" => e.cancel(),
            other => unreachable!("unknown trace action {other}"),
        }
        // ENABLEDNESS AGREEMENT: the model must admit the step the real engine
        // just took (the trace is enabled by construction; a false here means
        // the enumerator and the executor disagree — a harness bug).
        assert!(
            m.fire(a, &mut st),
            "model guard rejects {a} at step {i} of {trace:?} — model/real enabledness diverged"
        );
        // PROJECTION LOCKSTEP: the engine's public accessors, projected onto
        // the model variables, must equal the model state EXACTLY — this is the
        // real measurement (not the model echoing itself): a drifted engine
        // update (e.g. a dropped clamp) diverges here.
        assert_eq!(
            project(&e),
            st,
            "projection diverged after {a} at step {i} of {trace:?}"
        );
        steps.push((prev, st.clone(), a));
    }
    steps
}

/// TIERED per-transition conformance (see `verify::validate_transition_tiered`):
/// the interpreter's `Next`-admission check always runs; `ty trace validate`
/// additionally validates the two-step trace wherever installed (verdicts must
/// agree). `Wrap` is pinned to the run's engine config.
fn validate_transition(
    m: &Model,
    wrap: bool,
    prev: &State,
    next: &State,
    action: &str,
) -> (bool, String) {
    verify::validate_transition_tiered(
        m,
        &[("Wrap", i64::from(wrap)), ("Buggy", 0)],
        prev,
        next,
        Some(action),
        "streaming-search Tier-1 binding",
    )
}

/// The driver constants and the registered model's consts must be the SAME
/// numbers, or the lockstep silently checks a different regime.
#[test]
fn model_consts_match_driver() {
    let m = streaming_search_model();
    let c = |n: &str| {
        m.consts
            .iter()
            .find(|(k, _)| *k == n)
            .unwrap_or_else(|| panic!("const {n} missing"))
            .1
    };
    assert_eq!(c("Rows"), ROWS as i64);
    assert_eq!(c("MaxResults"), MAX_RESULTS as i64);
    assert_eq!(c("MaxTotal"), MAX_TOTAL);
    assert_eq!(c("Buggy"), 0, "committed config is the fixed engine");
}

/// Tier 0 backstop (the build.rs gate runs the same check at compile time; this
/// keeps `cargo test -p aterm-search` alone proving it too).
#[test]
fn streaming_search_spec_model_checked_in_trust() {
    verify::prove_and_catch_scalar(
        &streaming_search_model(),
        "streaming-search derived spec (Tier-0 backstop)",
    );
}

/// Tier 1: EVERY bounded interleaving, driven on the REAL engine in lockstep
/// with the model; every UNIQUE projected transition judged by the tiered
/// validator (identical `(prev, next, action)` steps recur across traces and
/// get the same verdict, so each is judged once — the ty fan-out stays bounded
/// while the per-step lockstep asserts still run on every trace).
#[test]
fn real_engine_conforms_to_spec_exhaustively() {
    for wrap in [true, false] {
        let m = if wrap {
            streaming_search_model()
        } else {
            aterm_spec::interp::with_consts(&streaming_search_model(), &[("Wrap", 0)])
        };

        let traces = all_traces(&m);
        assert!(
            traces.len() > 1_000,
            "exhaustive bounded alphabet (wrap={wrap}: {} traces)",
            traces.len()
        );

        let mut unique: BTreeSet<(StateKey, StateKey, &str)> = BTreeSet::new();
        let key = |s: &State| -> StateKey { s.iter().map(|(k, v)| (*k, *v)).collect() };
        for trace in &traces {
            for (prev, next, action) in run_lockstep(&m, wrap, trace) {
                unique.insert((key(&prev), key(&next), action));
            }
        }
        assert!(
            unique.len() >= 40,
            "transition coverage is non-trivial (wrap={wrap}: {})",
            unique.len()
        );

        for (prev_k, next_k, action) in &unique {
            let prev: State = prev_k.iter().map(|&(k, v)| (k, v)).collect();
            let next: State = next_k.iter().map(|&(k, v)| (k, v)).collect();
            let (ok, out) = validate_transition(&m, wrap, &prev, &next, action);
            assert!(
                ok,
                "ty REJECTED real transition {action} (wrap={wrap})\nprev={prev:?}\nnext={next:?}\n{out}"
            );
        }
        eprintln!(
            "TRUST Tier-1 (wrap={wrap}): {} interleavings driven on the REAL engine, {} unique \
             transitions judged by the tiered validator.",
            traces.len(),
            unique.len()
        );
    }
}

/// NEGATIVE CONTROL 1: a forged observation must be REJECTED — take a real
/// mid-scan ScanHit step and claim one MORE stored result than measured.
#[test]
fn forged_observation_is_rejected() {
    let m = streaming_search_model();
    let steps = run_lockstep(&m, true, &["Start", "ScanHit"]);
    let (prev, next, action) = steps.last().expect("two steps ran").clone();
    let mut forged = next.clone();
    forged.insert("stored", next["stored"] + 1);
    let (ok, _) = validate_transition(&m, true, &prev, &forged, action);
    assert!(
        !ok,
        "negative control: the validator must reject a forged stored-count \
         (the binding is vacuous otherwise)"
    );
}

/// NEGATIVE CONTROL 2: a model-DISABLED action corresponds to a real engine
/// no-op — `next_match` in NoResults changes nothing, and the model agrees the
/// action is not enabled (enabledness agreement on the disabled side).
#[test]
fn disabled_navigation_is_a_real_noop() {
    let m = streaming_search_model();
    let mut e = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: MAX_RESULTS,
        ..StreamingSearchConfig::default()
    });
    e.start_search("t", FilterMode::Literal).expect("start");
    for row in 0..ROWS {
        e.scan_row(row, "x", ROWS); // all-miss scan -> NoResults
    }
    assert_eq!(e.state(), SearchState::NoResults);
    let before = project(&e);
    assert!(
        !m.action_enabled("NextMatch", &before) && !m.action_enabled("PrevMatch", &before),
        "model: navigation disabled in NoResults"
    );
    e.next_match();
    e.prev_match();
    assert_eq!(
        project(&e),
        before,
        "engine: navigation is a no-op in NoResults"
    );
}

/// NEGATIVE CONTROL 3: the at-capacity discipline — the third hit COUNTS but
/// does not STORE (stored stays flat, total increments) and the spec admits
/// exactly that transition (and rejects the store-past-capacity variant).
#[test]
fn at_capacity_hit_counts_but_does_not_store() {
    let m = streaming_search_model();
    let steps = run_lockstep(&m, true, &["Start", "ScanHit", "ScanHit", "ScanHit"]);
    let (prev, next, action) = steps.last().expect("four steps ran").clone();
    assert_eq!(action, "ScanHit");
    assert_eq!(
        prev["stored"], MAX_RESULTS as i64,
        "at capacity before the hit"
    );
    assert_eq!(next["stored"], prev["stored"], "engine stored nothing");
    assert_eq!(next["total"], prev["total"] + 1, "engine still counted it");
    let (ok, out) = validate_transition(&m, true, &prev, &next, action);
    assert!(ok, "spec admits counts-not-stores at capacity\n{out}");
    let mut overstored = next.clone();
    overstored.insert("stored", prev["stored"] + 1);
    let (ok, _) = validate_transition(&m, true, &prev, &overstored, action);
    assert!(
        !ok,
        "spec must reject storing past MaxResults (INV-SEARCH-3 discipline)"
    );
}

/// ABSTRACTION-BOUNDARY CONTROL: the derived machine deliberately models one
/// added or removed match per ScanHit/Add/Invalidate action. The shipping
/// methods accept broader text/ranges and may affect several matches in one
/// atomic call. Prove those transitions are NOT accidentally admitted as unit
/// actions; their invariants belong to the local/Kani suites instead.
#[test]
fn multi_effect_calls_are_rejected_by_the_unit_effect_model() {
    let m = streaming_search_model();
    let config = || StreamingSearchConfig {
        max_results: MAX_RESULTS,
        ..StreamingSearchConfig::default()
    };

    let mut scan = StreamingSearch::with_config(config());
    scan.start_search("t", FilterMode::Literal)
        .expect("start scan boundary case");
    let before_scan = project(&scan);
    assert_eq!(scan.scan_row(0, "tt", ROWS), 2);
    let after_scan = project(&scan);
    assert_eq!(
        aterm_spec::interp::admits(&m, &before_scan, &after_scan),
        None,
        "two matches from one scan_row call are outside unit ScanHit"
    );

    let mut add = StreamingSearch::with_config(config());
    add.start_search("t", FilterMode::Literal)
        .expect("start add boundary case");
    for row in 0..ROWS {
        assert_eq!(add.scan_row(row, "x", ROWS), 0);
    }
    assert_eq!(add.state(), SearchState::NoResults);
    let before_add = project(&add);
    add.content_added(100, "tt");
    let after_add = project(&add);
    assert_eq!(
        aterm_spec::interp::admits(&m, &before_add, &after_add),
        None,
        "two matches from one content_added call are outside unit Add"
    );

    let mut invalidate = StreamingSearch::with_config(config());
    invalidate
        .start_search("t", FilterMode::Literal)
        .expect("start invalidate boundary case");
    assert_eq!(invalidate.scan_row(0, "t", ROWS), 1);
    assert_eq!(invalidate.scan_row(1, "t", ROWS), 1);
    assert_eq!(invalidate.scan_row(2, "x", ROWS), 0);
    assert_eq!(invalidate.state(), SearchState::HasResults);
    let before_invalidate = project(&invalidate);
    invalidate.content_invalidated(0, 1);
    let after_invalidate = project(&invalidate);
    assert_eq!(
        aterm_spec::interp::admits(&m, &before_invalidate, &after_invalidate),
        None,
        "two removals from one content_invalidated call are outside unit Invalidate"
    );
}
