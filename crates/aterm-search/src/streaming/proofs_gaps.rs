// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for algorithmic gaps: content_invalidated, update_pattern.
//! Split from proofs.rs for file size compliance. Part of #2875.
//!
//! ## Scope honesty (what these harnesses do and do not prove)
//!
//! Every harness here SEEDS its terminal state via `kani_seed_results` (#6119 —
//! reaching those states through the real scan pipeline blows up CBMC), so
//! pipeline-REACHABILITY of the seeded states was never proven by kani. The
//! derived `StreamingSearch` model closes exactly that class at the scalar
//! abstraction: its Tier-0 BMC exhausts the reachable space (so e.g.
//! "HasResults with cur > stored" is proven unreachable, not assumed) and the
//! Tier-1 lockstep (`tests/conformance_streaming.rs`) drives the REAL engine
//! through cross-operation sequences with every unit-effect transition judged
//! against the model. Remaining kani-and-model-free territory, deliberately out of the
//! migration slice and NOT claimed as covered: `content_modified` (#7244),
//! `jump_to_match` (needs `Expr::InRange` — a hand-built Model extension),
//! `scan_all` wrapped-row remapping (#7471/#7572), and the regex path
//! (feature-gated off under kani, dual-path #2688). `update_pattern_*` below
//! stay ledger-UNANCHORED: pattern content is unbounded, so there is no model
//! action to join them to (the documented local-only convention).

use super::*;

fn seeded_search(
    pattern: &str,
    current_index: usize,
    results: &[(usize, usize, usize)],
) -> StreamingSearch {
    let mut search = StreamingSearch::new();
    search.kani_seed_results(pattern, current_index, results);
    search
}

/// content_invalidated preserves all 6 invariants with symbolic
/// invalidation range. Exercises the results.retain() and
/// current_index adjustment paths.
///
/// Symbolic over current_index and invalidation target: proves that
/// for any valid current_index (1 or 2) and any invalidation of
/// row 0 or row 1, all invariants hold.
///
/// Seeds a pre-validated terminal state directly under Kani so the proof
/// targets invalidation logic rather than the match-finding pipeline. This
/// keeps the proof focused and avoids the `scan_row` setup blow-up from #6119.
#[kani::proof]
#[kani::unwind(4)]
fn content_invalidated_preserves_invariants() {
    let cur_idx: usize = kani::any();
    kani::assume(cur_idx >= 1 && cur_idx <= 2);

    let invalidate_row: usize = kani::any();
    kani::assume(invalidate_row <= 1);

    let mut search = seeded_search("t", cur_idx, &[(0, 0, 1), (1, 0, 1)]);
    search.content_invalidated(invalidate_row, invalidate_row);

    // After partial invalidation, one match should survive
    kani::assert(
        search.result_count() == 1,
        "invalidating one of two matches must retain exactly one",
    );
    kani::assert(
        search.state() == SearchState::HasResults,
        "partial invalidation with one remaining match must stay in HasResults",
    );
    kani::assert(
        search.total_matches() == 1,
        "content_invalidated must decrement total_matches by the removed stored match",
    );
    kani::assert(
        search.verify_current_index_valid(),
        "partial invalidation must preserve current_index validity for any initial index",
    );
    kani::assert(
        search.verify_scan_progress_consistent(),
        "partial invalidation must preserve scan-progress consistency",
    );
    kani::assert(
        search.verify_total_matches_consistent(),
        "partial invalidation must preserve total_matches consistency",
    );
}

/// Prefix invalidation covers dropping the earliest retained matches.
///
/// Symbolic over current_index: proves that for any valid current_index
/// (1 or 2), prefix invalidation (dropping row 0) clamps correctly and
/// preserves all invariants.
#[kani::proof]
#[kani::unwind(4)]
fn content_invalidated_prefix_preserves_invariants() {
    let cur_idx: usize = kani::any();
    kani::assume(cur_idx >= 1 && cur_idx <= 2);

    let mut prefix = seeded_search("t", cur_idx, &[(0, 0, 1), (1, 0, 1)]);
    prefix.content_invalidated(0, 0);

    kani::assert(
        prefix.state() == SearchState::HasResults,
        "prefix invalidation with one remaining match must stay in HasResults",
    );
    kani::assert(
        prefix.result_count() == 1,
        "prefix invalidation must retain exactly one later match",
    );
    let remaining = &prefix.results()[0];
    kani::assert(
        remaining.row == 1
            && remaining.start_col == 0
            && remaining.end_col == 1
            && remaining.match_len == 1,
        "prefix invalidation must keep the later match span intact",
    );
    kani::assert(
        prefix.verify_current_index_valid(),
        "prefix invalidation must preserve current_index validity for any initial index",
    );
    kani::assert(
        prefix.total_matches() == 1,
        "content_invalidated must decrement total_matches by the removed stored match",
    );
    kani::assert(
        prefix.verify_scan_progress_consistent(),
        "prefix invalidation must preserve scan-progress consistency",
    );
    kani::assert(
        prefix.verify_total_matches_consistent(),
        "prefix invalidation must preserve total_matches consistency",
    );
}

/// content_invalidated transitions to NoResults when all results are
/// removed, and adjusts current_index when it exceeds remaining count.
///
/// Symbolic over current_index and invalidation order: proves that for
/// any valid current_index and either invalidation order (row 0 first
/// or row 1 first), the state transitions correctly to NoResults.
#[kani::proof]
#[kani::unwind(4)]
fn content_invalidated_state_transitions() {
    let cur_idx: usize = kani::any();
    kani::assume(cur_idx >= 1 && cur_idx <= 2);

    let first_row: usize = kani::any();
    kani::assume(first_row <= 1);
    let second_row: usize = 1 - first_row; // The other row

    let mut search = seeded_search("x", cur_idx, &[(0, 0, 1), (1, 0, 1)]);

    // First invalidation — removes one match, current_index must adjust
    search.content_invalidated(first_row, first_row);

    kani::assert(
        search.verify_current_index_valid(),
        "current_index must be valid after partial invalidation for any index and order",
    );

    // Second invalidation — removes remaining, must transition to NoResults
    search.content_invalidated(second_row, second_row);

    kani::assert(
        search.state() == SearchState::NoResults,
        "must be NoResults after all results invalidated in any order",
    );
    kani::assert(
        search.current_index() == 0,
        "current_index must be 0 in NoResults state",
    );
}

/// update_pattern preserves invariants through all branches:
/// same pattern (no-op), empty pattern (→ Idle), new pattern (→ Searching).
///
/// Seeds a concrete terminal state so the proof exercises `update_pattern`
/// itself instead of paying for `scan_row` setup. See #6119.
#[kani::proof]
#[kani::unwind(4)]
fn update_pattern_preserves_invariants() {
    let mut search = seeded_search("ab", 1, &[(0, 0, 2), (1, 0, 2)]);

    let branch: u8 = kani::any();
    kani::assume(branch <= 2);

    let result = match branch {
        0 => search.update_pattern("ab"), // Same pattern → no-op
        1 => search.update_pattern(""),   // Empty → Idle
        _ => search.update_pattern("xy"), // Different → restart
    };

    kani::assert(
        result.is_ok(),
        "update_pattern from valid state must succeed",
    );
    kani::assert(
        search.verify_all_invariants(),
        "update_pattern must preserve all streaming search invariants",
    );

    match branch {
        0 => kani::assert(
            search.state() == SearchState::HasResults || search.state() == SearchState::NoResults,
            "same pattern must preserve terminal state",
        ),
        1 => kani::assert(
            search.state() == SearchState::Idle,
            "empty pattern must transition to Idle",
        ),
        _ => kani::assert(
            search.state() == SearchState::Searching,
            "new pattern must restart search",
        ),
    }
}

/// update_pattern from invalid state (Idle) returns InvalidState error.
// INTENTIONALLY_CONCRETE: tests the specific Idle-state error path;
// StreamingSearch::new() always produces Idle, and the pattern string
// content is irrelevant since the state check rejects before parsing.
#[kani::proof]
fn update_pattern_from_idle_returns_error() {
    let mut search = StreamingSearch::new();

    let result = search.update_pattern("test");

    kani::assert(
        result.is_err(),
        "update_pattern from Idle must return error",
    );
    kani::assert(
        search.state() == SearchState::Idle,
        "state must remain Idle after failed update_pattern",
    );
}
