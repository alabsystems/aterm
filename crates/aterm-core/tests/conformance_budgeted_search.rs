// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the budgeted full-buffer search lifecycle.
//!
//! The trace projects the real public `BudgetedSearchStep`: `rows_fed`,
//! `total_rows`, `complete`, `cursor`, `search_id`, and `reset`. It drives fresh
//! start, valid resume, every modeled restart class, cancellation, ordinary and
//! one-turn completion, post-completion token retirement, and a dense result
//! stream whose scan finishes before two drain-only turns deliver its backlog.
//!
//! Process-global `u64` identities are normalized by first-seen rank. A valid
//! resume must therefore preserve its rank, while every `reset` step must expose
//! the next never-before-seen rank even when it completes without a cursor.

use std::collections::BTreeMap;

use aterm_core::search::SearchResults;
use aterm_core::terminal::{BudgetedSearchStep, Terminal};
use aterm_spec::derive::{Model, budgeted_search_resume_model};
use aterm_spec::verify;

const ROWS: u16 = 3;
const MAX_MATCHES_PER_STEP: usize = 4_096;

type State = BTreeMap<&'static str, i64>;

/// Stable first-seen ranks for opaque process-global search identities.
#[derive(Default)]
struct TokenRanks {
    tokens: Vec<u64>,
}

impl TokenRanks {
    fn rank(&mut self, token: u64) -> i64 {
        if let Some(index) = self.tokens.iter().position(|known| *known == token) {
            return i64::try_from(index + 1).expect("bounded token rank fits i64");
        }
        self.tokens.push(token);
        i64::try_from(self.tokens.len()).expect("bounded token rank fits i64")
    }

    fn issued(&self) -> i64 {
        i64::try_from(self.tokens.len()).expect("bounded token count fits i64")
    }
}

/// Project one returned shipping step. `prior_id` is the sole trace ghost: it
/// records the previously observed logical stream when the real `reset` bit says
/// this call replaced it. `delivery` is derived from public scan/completion state
/// plus non-empty result deltas; reset starts a new delivery stream at zero/one.
fn project_step(step: &BudgetedSearchStep, ranks: &mut TokenRanks, prev: &State) -> State {
    assert!(
        step.results.matches.len() <= MAX_MATCHES_PER_STEP,
        "one real step exceeded the documented result-delta bound"
    );
    assert!(
        step.rows_fed <= step.total_rows,
        "real row progress exceeded its fixed denominator"
    );

    // Rank search_id first so a live cursor that equals it reuses the same rank.
    // A mismatched cursor creates/dereferences a different rank and fails the
    // model's CursorMatchesSearchId invariant/projection equality.
    let search_id = ranks.rank(step.search_id);
    let cursor = step.cursor.map_or(0, |token| ranks.rank(token));
    let scan_done = i64::from(step.rows_fed == step.total_rows);
    let delivery = if step.reset {
        i64::from(scan_done == 1 && !step.complete)
    } else if scan_done == 1 && (!step.complete || prev["delivery"] > 0) {
        prev["delivery"] + 1
    } else {
        0
    };
    if delivery > 0 {
        assert!(
            !step.results.matches.is_empty(),
            "a modeled delivery turn must return a non-empty stable delta"
        );
    }

    [
        ("live", i64::from(step.cursor.is_some())),
        (
            "progress",
            i64::try_from(step.rows_fed).expect("bounded row progress fits i64"),
        ),
        (
            "total",
            i64::try_from(step.total_rows).expect("bounded total rows fit i64"),
        ),
        ("scan_done", scan_done),
        ("complete", i64::from(step.complete)),
        ("cursor", cursor),
        ("search_id", search_id),
        ("issued", ranks.issued()),
        ("reset", i64::from(step.reset)),
        ("prior_id", if step.reset { prev["search_id"] } else { 0 }),
        ("delivery", delivery),
    ]
    .into_iter()
    .collect()
}

fn project_cancel(ranks: &TokenRanks) -> State {
    [
        ("live", 0),
        ("progress", 0),
        ("total", 0),
        ("scan_done", 0),
        ("complete", 0),
        ("cursor", 0),
        ("search_id", 0),
        ("issued", ranks.issued()),
        ("reset", 0),
        ("prior_id", 0),
        ("delivery", 0),
    ]
    .into_iter()
    .collect()
}

fn bind_transition(model: &Model, state: &mut State, action: &str, observed: State) {
    let prev = state.clone();
    let mut expected = prev.clone();
    assert!(
        model.fire(action, &mut expected),
        "model action {action} disabled at {prev:?}"
    );
    assert_eq!(
        observed, expected,
        "real budgeted-search projection diverged after {action}"
    );
    let (conforms, diagnostics) = verify::validate_transition_tiered(
        model,
        &[],
        &prev,
        &observed,
        Some(action),
        "BudgetedSearchResume Tier-1",
    );
    assert!(
        conforms,
        "real transition {action} is not admitted by the derived model: {diagnostics}"
    );
    *state = observed;
}

fn bind_step(
    model: &Model,
    state: &mut State,
    ranks: &mut TokenRanks,
    action: &str,
    step: &BudgetedSearchStep,
) {
    let observed = project_step(step, ranks, state);
    bind_transition(model, state, action, observed);
}

#[test]
fn real_terminal_reset_identity_resume_and_retirement_refine_the_model() {
    let model = budgeted_search_resume_model();
    let mut state = model.init_state();
    let mut ranks = TokenRanks::default();
    let mut terminal = Terminal::new(ROWS, 20);

    // Zero is clamped to one scan row. A later valid resume changes the numeric
    // budget to one, proving row_budget is not part of cursor identity.
    let start = terminal
        .search_budgeted("x", true, false, None, 0)
        .expect("fresh search");
    bind_step(&model, &mut state, &mut ranks, "Start", &start);
    assert!(start.reset);
    assert_eq!(start.cursor, Some(start.search_id));

    let resumed = terminal
        .search_budgeted("x", true, false, start.cursor, 1)
        .expect("valid resume");
    bind_step(&model, &mut state, &mut ranks, "Resume", &resumed);
    assert!(!resumed.reset);
    assert_eq!(resumed.search_id, start.search_id);
    assert_eq!(resumed.cursor, start.cursor);

    // Content invalidation restarts at row one with reset + fresh identity.
    let content_before = terminal.content_seq();
    terminal.process(b"x");
    assert!(terminal.content_seq() > content_before);
    let content_restart = terminal
        .search_budgeted("x", true, false, resumed.cursor, 0)
        .expect("content-stale restart");
    bind_step(
        &model,
        &mut state,
        &mut ranks,
        "ContentRestart",
        &content_restart,
    );
    assert!(content_restart.reset);
    assert_ne!(content_restart.search_id, resumed.search_id);

    let resumed_again = terminal
        .search_budgeted("x", true, false, content_restart.cursor, 1)
        .expect("valid resume after content restart");
    bind_step(&model, &mut state, &mut ranks, "Resume", &resumed_again);

    // Query/options mismatches share this reset class; this trace changes query.
    let query_restart = terminal
        .search_budgeted("y", true, false, resumed_again.cursor, 0)
        .expect("query restart");
    bind_step(
        &model,
        &mut state,
        &mut ranks,
        "QueryRestart",
        &query_restart,
    );

    // A token different from the sole live search cannot resume its stream.
    let forged = Some(query_restart.search_id.wrapping_add(10_000));
    assert_ne!(forged, query_restart.cursor);
    let forged_restart = terminal
        .search_budgeted("y", true, false, forged, 0)
        .expect("forged-cursor restart");
    bind_step(
        &model,
        &mut state,
        &mut ranks,
        "ForgedRestart",
        &forged_restart,
    );

    // None explicitly supersedes an in-flight stream, even for the same query.
    let superseded = terminal
        .search_budgeted("y", true, false, None, 0)
        .expect("explicit supersession");
    bind_step(&model, &mut state, &mut ranks, "Supersede", &superseded);

    // Cancellation has no returned step. The immediately following stale-token
    // call demonstrates that the real owner state was retired, not just the model.
    let cancelled_cursor = superseded.cursor;
    terminal.cancel_budgeted_search();
    let cancel = project_cancel(&ranks);
    bind_transition(&model, &mut state, "Cancel", cancel);

    let after_cancel = terminal
        .search_budgeted("y", true, false, cancelled_cursor, 0)
        .expect("cancelled token starts fresh");
    bind_step(&model, &mut state, &mut ranks, "Start", &after_cancel);
    assert!(after_cancel.reset);
    assert_ne!(after_cancel.search_id, superseded.search_id);

    let penultimate = terminal
        .search_budgeted("y", true, false, after_cancel.cursor, 1)
        .expect("penultimate scan resume");
    bind_step(&model, &mut state, &mut ranks, "Resume", &penultimate);
    let completed = terminal
        .search_budgeted("y", true, false, penultimate.cursor, 1)
        .expect("ordinary completion");
    bind_step(&model, &mut state, &mut ranks, "FinishScan", &completed);
    assert!(completed.complete);
    assert!(!completed.reset);
    assert_eq!(completed.cursor, None);
    assert_eq!(completed.search_id, after_cancel.search_id);
    assert_eq!(completed.rows_fed, usize::from(ROWS));

    // Completion RETIRES the owner cursor (the completed index is retained for
    // search_summary to read, fed E-1, but is no longer resumable). Presenting
    // the completed search_id cannot resurrect it: this is a new reset-marked
    // logical stream.
    let after_completion = terminal
        .search_budgeted("y", true, false, Some(completed.search_id), 0)
        .expect("completed identity is retired");
    bind_step(&model, &mut state, &mut ranks, "Start", &after_completion);
    assert!(after_completion.reset);
    assert_ne!(after_completion.search_id, completed.search_id);

    terminal.cancel_budgeted_search();
    let cancel = project_cancel(&ranks);
    bind_transition(&model, &mut state, "Cancel", cancel);
}

#[test]
fn one_turn_restart_retains_reset_and_search_id_without_a_cursor() {
    let model = budgeted_search_resume_model();
    let mut state = model.init_state();
    let mut ranks = TokenRanks::default();
    let mut terminal = Terminal::new(ROWS, 20);

    terminal.process(b"x");
    let first = terminal
        .search_budgeted("x", true, false, None, 1)
        .expect("initial partial stream");
    bind_step(&model, &mut state, &mut ranks, "Start", &first);
    assert!(
        !first.results.matches.is_empty(),
        "the superseded stream must have a delivered delta"
    );

    terminal.process(b"x");
    let restarted = terminal
        .search_budgeted("x", true, false, first.cursor, usize::MAX)
        .expect("one-turn content restart");
    bind_step(
        &model,
        &mut state,
        &mut ranks,
        "RestartComplete",
        &restarted,
    );
    assert!(restarted.complete);
    assert!(
        restarted.reset,
        "host must clear the superseded delta stream"
    );
    assert_eq!(restarted.cursor, None);
    assert_ne!(restarted.search_id, first.search_id);

    let oracle = terminal
        .indexed_search()
        .search_results_opts("x", true, false)
        .expect("one-shot oracle")
        .clone();
    assert_eq!(restarted.results, oracle);
    let mut host_matches = first.results.matches.clone();
    if restarted.reset {
        host_matches.clear();
    }
    host_matches.extend(restarted.results.matches.iter().cloned());
    assert_eq!(
        host_matches, oracle.matches,
        "reset prevents mixed snapshots"
    );

    // A fresh sparse search may likewise finish in its first reset-marked turn.
    let fresh_complete = terminal
        .search_budgeted("not-present", true, false, None, usize::MAX)
        .expect("one-turn fresh search");
    bind_step(
        &model,
        &mut state,
        &mut ranks,
        "StartComplete",
        &fresh_complete,
    );
    assert!(fresh_complete.complete);
    assert!(fresh_complete.reset);
    assert_eq!(fresh_complete.cursor, None);
    assert_ne!(fresh_complete.search_id, restarted.search_id);
}

#[test]
fn dense_result_backlog_drains_without_rescanning_or_replaying_deltas() {
    let model = budgeted_search_resume_model();
    let mut state = model.init_state();
    let mut ranks = TokenRanks::default();
    let mut terminal = Terminal::new(ROWS, 10_000);

    // Exactly three grid rows (no line feed / scrollback), but 9,000 matches on
    // one wide row: enough for the scan turn plus two drain turns at the
    // shipping 4,096-record delta bound.
    terminal.process("a".repeat(9_000).as_bytes());
    let oracle = terminal
        .indexed_search()
        .search_results_opts("a", true, false)
        .expect("dense one-shot oracle")
        .clone();
    assert_eq!(oracle.matches.len(), 9_000, "dense fixture drifted");

    let first = terminal
        .search_budgeted("a", true, false, None, usize::MAX)
        .expect("scan + first dense delta");
    bind_step(&model, &mut state, &mut ranks, "StartBacklog", &first);
    assert_eq!(first.rows_fed, first.total_rows, "scan finished");
    assert!(!first.complete, "delivery backlog keeps the cursor live");
    assert_eq!(first.cursor, Some(first.search_id));
    assert_eq!(first.results.matches.len(), MAX_MATCHES_PER_STEP);

    let second = terminal
        .search_budgeted("a", true, false, first.cursor, 1)
        .expect("first drain-only resume");
    bind_step(&model, &mut state, &mut ranks, "Drain", &second);
    assert_eq!(second.rows_fed, first.rows_fed, "drain scans no rows");
    assert!(!second.complete, "one more delta remains");
    assert!(!second.reset);
    assert_eq!(second.search_id, first.search_id);
    assert_eq!(second.cursor, first.cursor);
    assert_eq!(second.results.matches.len(), MAX_MATCHES_PER_STEP);

    let third = terminal
        .search_budgeted("a", true, false, second.cursor, usize::MAX)
        .expect("final drain-only resume");
    bind_step(&model, &mut state, &mut ranks, "DrainComplete", &third);
    assert_eq!(third.rows_fed, first.rows_fed, "final drain scans no rows");
    assert!(third.complete);
    assert!(!third.reset);
    assert_eq!(third.search_id, first.search_id);
    assert_eq!(third.cursor, None);
    assert!(!third.results.matches.is_empty());

    let got = SearchResults::new(
        [
            first.results.matches,
            second.results.matches,
            third.results.matches,
        ]
        .concat(),
        third.results.incomplete,
        third.results.lowest_retained_line,
    );
    assert_eq!(got, oracle, "ordered deltas concatenate exactly once");
}

#[test]
fn stale_identity_and_early_or_stuttering_drain_negative_controls_are_rejected() {
    let model = budgeted_search_resume_model();

    let mut scanning = model.init_state();
    assert!(model.fire("Start", &mut scanning));
    assert!(model.fire("Resume", &mut scanning));
    let mut stale = scanning.clone();
    stale.insert("reset", 1);
    stale.insert("prior_id", scanning["search_id"]);
    let (conforms, diagnostics) = verify::validate_transition_tiered(
        &model,
        &[],
        &scanning,
        &stale,
        Some("ContentRestart"),
        "BudgetedSearchResume stale identity negative control",
    );
    assert!(
        !conforms,
        "stale reset continuation passed unexpectedly: {diagnostics}"
    );

    let mut backlog = model.init_state();
    assert!(model.fire("StartBacklog", &mut backlog));
    let mut early_complete = backlog.clone();
    early_complete.insert("live", 0);
    early_complete.insert("complete", 1);
    early_complete.insert("cursor", 0);
    early_complete.insert("reset", 0);
    early_complete.insert("prior_id", 0);
    let (conforms, diagnostics) = verify::validate_transition_tiered(
        &model,
        &[],
        &backlog,
        &early_complete,
        Some("DrainComplete"),
        "BudgetedSearchResume early completion negative control",
    );
    assert!(
        !conforms,
        "completion with backlog passed unexpectedly: {diagnostics}"
    );

    let mut stutter = backlog.clone();
    stutter.insert("reset", 0);
    stutter.insert("prior_id", 0);
    let (conforms, diagnostics) = verify::validate_transition_tiered(
        &model,
        &[],
        &backlog,
        &stutter,
        Some("Drain"),
        "BudgetedSearchResume duplicate-delta stutter negative control",
    );
    assert!(
        !conforms,
        "drain with no delivery progress passed unexpectedly: {diagnostics}"
    );
}
