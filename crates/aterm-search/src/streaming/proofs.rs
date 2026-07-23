// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::types::NavigationDirection;
use super::*;

fn verify_result_positions_valid(
    results: &[StreamingMatch],
    max_rows: usize,
    max_cols: usize,
) -> bool {
    results.iter().all(|m| {
        m.row < max_rows
            && m.start_col <= m.end_col
            && m.end_col <= max_cols
            && m.match_len == m.end_col.saturating_sub(m.start_col)
    })
}

/// INV-SEARCH-1: Current index is always valid.
/// Reduced iteration counts and shortened content string for CBMC budget.
/// scan_row on "test content" (12 chars) with scan_count<=3 and nav_count<=3
/// requires internal matching loops that exceed unwind(4). Use shorter content
/// "test" (4 chars) and tighter bounds with unwind(16) to cover all paths.
#[kani::proof]
#[kani::unwind(32)] // compound: symbolic scan_count<=2 x find_overlapping_substring_positions O(n*m)
fn current_index_always_valid() {
    let mut search = StreamingSearch::new();

    let start: bool = kani::any();
    let scan_count: usize = kani::any();
    let nav_count: usize = kani::any();

    kani::assume(scan_count <= 2);
    kani::assume(nav_count <= 2);

    if start {
        let _ = search.start_search("test", FilterMode::Literal);

        for i in 0..scan_count {
            search.scan_row(i, "test", 5);
        }

        for _ in 0..nav_count {
            if kani::any() {
                search.next_match();
            } else {
                search.prev_match();
            }
        }
    }

    kani::assert(
        search.verify_current_index_valid(),
        "INV-SEARCH-1 violated: current index invalid",
    );
}

/// INV-SEARCH-2: Result positions are valid grid coordinates.
///
/// Symbolic over number of rows scanned: proves result positions are
/// valid for any scan prefix of 1-3 rows from the content set.
#[kani::proof]
#[kani::unwind(10)]
fn result_positions_always_valid() {
    let mut search = StreamingSearch::new();
    let max_rows = 3;
    let max_cols = 6;

    let scan_count: usize = kani::any();
    kani::assume(scan_count >= 1 && scan_count <= 3);

    let _ = search.start_search("ana", FilterMode::Literal);
    let rows = ["banana", "cabana", "canada"];
    for i in 0..scan_count {
        search.scan_row(i, rows[i], max_rows);
    }

    kani::assert(
        verify_result_positions_valid(search.results(), max_rows, max_cols),
        "INV-SEARCH-2 violated: result positions must be in-bounds for any scan prefix",
    );
    kani::assert(
        search.verify_result_positions_valid(),
        "INV-SEARCH-2 violated: internal result position invariant failed",
    );
}

/// INV-SEARCH-3: Memory is always bounded.
#[kani::proof]
#[kani::unwind(12)]
fn memory_always_bounded() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: 5,
        ..Default::default()
    });

    let _ = search.start_search("a", FilterMode::Literal);

    let scan_count: usize = kani::any();
    kani::assume(scan_count <= 10);

    for i in 0..scan_count {
        search.scan_row(i, "a a a a a", 20);
    }

    kani::assert(
        search.verify_memory_bounded(),
        "INV-SEARCH-3 violated: memory not bounded",
    );
}

/// INV-SEARCH-4: No duplicate results by (row, start_col).
///
/// Symbolic over number of rows scanned: proves no duplicates exist
/// for any scan prefix of 1-3 rows from the content set.
#[kani::proof]
#[kani::unwind(12)]
fn no_duplicate_results() {
    let mut search = StreamingSearch::new();
    let max_rows = 3;

    let scan_count: usize = kani::any();
    kani::assume(scan_count >= 1 && scan_count <= 3);

    let _ = search.start_search("aa", FilterMode::Literal);
    let rows = ["aaaa", "baaa", "caaa"];
    for i in 0..scan_count {
        search.scan_row(i, rows[i], max_rows);
    }

    kani::assert(
        search.verify_no_duplicates(),
        "INV-SEARCH-4 violated: duplicate positions detected for any scan prefix",
    );
}

/// INV-SEARCH-5: Scan progress stays consistent with state transitions.
#[kani::proof]
#[kani::unwind(12)]
fn scan_progress_consistent() {
    let mut search = StreamingSearch::new();
    let max_rows = 3;
    let pre_complete_scans: usize = kani::any();

    kani::assume(pre_complete_scans <= 2);
    kani::assert(
        search.verify_scan_progress_consistent(),
        "INV-SEARCH-5 violated in idle state",
    );

    let _ = search.start_search("a", FilterMode::Literal);
    kani::assert(
        search.verify_scan_progress_consistent(),
        "INV-SEARCH-5 violated immediately after start_search",
    );

    for row in 0..pre_complete_scans {
        search.scan_row(row, "a", max_rows);
        kani::assert(
            search.verify_scan_progress_consistent(),
            "INV-SEARCH-5 violated while actively scanning",
        );
    }

    for row in pre_complete_scans..max_rows {
        search.scan_row(row, "a", max_rows);
    }

    kani::assert(
        search.verify_scan_progress_consistent(),
        "INV-SEARCH-5 violated after scan completion",
    );

    search.cancel();
    kani::assert(
        search.verify_scan_progress_consistent(),
        "INV-SEARCH-5 violated after cancel transition",
    );
}

/// INV-SEARCH-6: Total matches >= stored results.
#[kani::proof]
#[kani::unwind(12)]
fn total_matches_consistent() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: 3,
        ..Default::default()
    });

    let _ = search.start_search("x", FilterMode::Literal);

    let scan_count: usize = kani::any();
    kani::assume(scan_count <= 10);

    for i in 0..scan_count {
        search.scan_row(i, "x x x", 20);
    }

    kani::assert(
        search.verify_total_matches_consistent(),
        "INV-SEARCH-6 violated: total matches < stored results",
    );
}

/// Cancel always returns to idle with cleared state.
///
/// Symbolic over scan count: proves cancel clears state regardless
/// of how many rows (0-3) were scanned before cancellation.
#[kani::proof]
#[kani::unwind(32)]
fn cancel_clears_state() {
    let mut search = StreamingSearch::new();

    let _ = search.start_search("test", FilterMode::Literal);

    let scan_count: usize = kani::any();
    kani::assume(scan_count <= 3);

    for i in 0..scan_count {
        search.scan_row(i, "test", 5);
    }

    search.cancel();

    kani::assert(
        search.state() == SearchState::Idle,
        "state must be idle after cancel regardless of scan count",
    );
    kani::assert(
        search.pattern().is_empty(),
        "pattern must be empty after cancel regardless of scan count",
    );
    kani::assert(
        search.results().is_empty(),
        "results must be empty after cancel regardless of scan count",
    );
    kani::assert(
        search.current_index() == 0,
        "current_index must be 0 after cancel regardless of scan count",
    );
}

/// Navigation never produces invalid index.
#[kani::proof]
#[kani::unwind(20)]
fn navigation_index_valid() {
    let len: usize = kani::any();
    let start_idx: usize = kani::any();
    let wrap: bool = kani::any();

    kani::assume(len > 0 && len <= 10);
    kani::assume(start_idx <= len);

    let fwd = StreamingSearch::next_index(start_idx, len, NavigationDirection::Forward, wrap);
    let bwd = StreamingSearch::next_index(start_idx, len, NavigationDirection::Backward, wrap);

    kani::assert(fwd <= len, "forward navigation produced invalid index");
    kani::assert(bwd <= len, "backward navigation produced invalid index");
}

/// INV-SEARCH-2: Result positions are valid grid coordinates (symbolic scan count variant).
///
/// After scanning rows, all stored match results have:
/// - row < max_rows (valid row index)
/// - start_col < end_col (non-empty match span)
/// - match_len > 0 (positive match length)
///
/// Shortened content from "ab cab abc" (10 chars) to "ab cab" (6 chars) and
/// reduced scan_count from 4 to 3, with unwind increased to 16 to cover
/// the internal matching engine char-iteration loops.
#[kani::proof]
#[kani::unwind(40)] // compound: scan_count<=3 x "ab cab" (6 bytes) x find_overlapping O(n*m)
fn result_positions_valid_symbolic_scan() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: 10,
        ..Default::default()
    });

    let _ = search.start_search("ab", FilterMode::Literal);

    let max_rows: usize = 3;
    let scan_count: usize = kani::any();
    kani::assume(scan_count <= 3);

    for i in 0..scan_count {
        search.scan_row(i, "ab cab", max_rows);
    }

    for m in search.results() {
        kani::assert(m.row < max_rows, "INV-SEARCH-2a: row >= max_rows");
        kani::assert(
            m.start_col < m.end_col,
            "INV-SEARCH-2b: start_col >= end_col",
        );
        kani::assert(m.match_len > 0, "INV-SEARCH-2c: zero-length match");
    }
}

/// INV-SEARCH-4: No duplicate results (content_added dedup path).
///
/// After scanning rows and adding content that could create duplicates,
/// verify_no_duplicates() holds (dedup via seen_positions is enforced).
///
/// Symbolic over which rows get content_added: proves dedup holds
/// regardless of which combination of already-scanned rows receive
/// content_added calls.
#[kani::proof]
#[kani::unwind(40)] // compound: 5 invocations x "no match" (8 bytes) x find_overlapping O(n*m)
fn no_duplicate_results_content_added() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: 10,
        ..Default::default()
    });

    let _ = search.start_search("a", FilterMode::Literal);

    // Scan rows sequentially — content with multiple matches per row
    search.scan_row(0, "a ba", 3);
    search.scan_row(1, "aa", 3);
    search.scan_row(2, "no match", 3);

    // After search completes, symbolically choose which rows get content_added.
    // This exercises the dedup path in content_added() for all combinations.
    if search.state() == SearchState::HasResults {
        let add_row0: bool = kani::any();
        let add_row1: bool = kani::any();
        if add_row0 {
            search.content_added(0, "a ba");
        }
        if add_row1 {
            search.content_added(1, "aa");
        }
    }

    kani::assert(
        search.verify_no_duplicates(),
        "INV-SEARCH-4 violated: duplicate results found for any content_added combination",
    );
}

/// INV-SEARCH-5: Scan progress consistent with state (symbolic branching variant).
///
/// Through any sequence of start/scan/cancel operations,
/// the scan_progress field is consistent with the search state:
/// - Idle → scan_progress == -1
/// - Searching → scan_progress >= 0
/// - HasResults/NoResults → scan_progress == -1
///
/// Reduced scan_count to 2, content to "a" (1 char), unwind 24 to cover
/// internal scan_row iteration plus outer symbolic loop without violating
/// CBMC unwinding assertions.
#[kani::proof]
#[kani::unwind(40)] // compound: symbolic branches + scan_row + grapheme iteration + find_overlapping
fn scan_progress_consistent_symbolic() {
    let mut search = StreamingSearch::with_config(StreamingSearchConfig {
        max_results: 5,
        ..Default::default()
    });

    // Construction: Idle state
    kani::assert(
        search.verify_scan_progress_consistent(),
        "INV-SEARCH-5: inconsistent at construction",
    );

    let do_start: bool = kani::any();
    if do_start {
        let _ = search.start_search("a", FilterMode::Literal);
        kani::assert(
            search.verify_scan_progress_consistent(),
            "INV-SEARCH-5: inconsistent after start_search",
        );

        // Scan a symbolic number of rows
        let scan_count: usize = kani::any();
        kani::assume(scan_count <= 2);
        let max_rows = 2usize;

        for i in 0..scan_count {
            search.scan_row(i, "a", max_rows);
            kani::assert(
                search.verify_scan_progress_consistent(),
                "INV-SEARCH-5: inconsistent after scan_row",
            );
        }

        // Optionally cancel
        let do_cancel: bool = kani::any();
        if do_cancel {
            search.cancel();
            kani::assert(
                search.verify_scan_progress_consistent(),
                "INV-SEARCH-5: inconsistent after cancel",
            );
        }
    }

    kani::assert(
        search.verify_scan_progress_consistent(),
        "INV-SEARCH-5: inconsistent at proof end",
    );
}
