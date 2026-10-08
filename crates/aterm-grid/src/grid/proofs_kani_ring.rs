// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs that `ring_hyperlinks.len() == ring_buffer_scrollback()` across
//! scroll_up and erase_scrollback, on an abstract state machine. The concrete
//! test in `tests/scrollback.rs` checks the production code against it.

use super::KANI_MAX_ROWS;

/// ring_hyperlinks.len() == total_lines.saturating_sub(visible_rows) after any
/// sequence of scroll_up(n). Growth adds rows_to_add to both; reuse pushes and
/// pops per row (net 0). Covers visible_rows 2..=8, max_scrollback 0..=4, and
/// up to 12 scrolls of symbolic size 1..=4.
#[kani::proof]
#[kani::unwind(13)] // 12 iterations + 1
fn ring_hyperlinks_len_matches_scrollback() {
    let visible_rows: u16 = kani::any();
    let max_scrollback: usize = kani::any();
    kani::assume(visible_rows >= 2 && visible_rows <= KANI_MAX_ROWS);
    kani::assume(max_scrollback <= 4);

    let capacity = (visible_rows as usize) + max_scrollback;
    let mut total_lines = visible_rows as usize;
    let mut ring_hyperlinks_len: usize = 0;

    // Initial state: invariant holds trivially (both 0).
    let ring_sb = total_lines.saturating_sub(visible_rows as usize);
    kani::assert(ring_hyperlinks_len == ring_sb, "invariant violated at init");

    // Model a sequence of scroll_up calls with symbolic batch sizes.
    let scroll_count: u8 = kani::any();
    kani::assume(scroll_count <= 12);

    for _ in 0..scroll_count {
        let n: usize = kani::any();
        kani::assume(n >= 1 && n <= 4);

        // Phase 1: growth (not at capacity)
        let rows_to_add = n.min(capacity.saturating_sub(total_lines));

        // ring_hyperlinks grows by rows_to_add (push_back per row).
        ring_hyperlinks_len += rows_to_add;
        total_lines += rows_to_add;

        // Reuse at capacity: push_back then pop_front per row, net 0.

        // Check invariant after this scroll_up.
        let ring_sb = total_lines.saturating_sub(visible_rows as usize);
        kani::assert(
            ring_hyperlinks_len == ring_sb,
            "invariant violated: ring_hyperlinks.len() != ring_buffer_scrollback()",
        );
    }
}

/// The invariant holds after erase_scrollback (ring_hyperlinks empty,
/// total_lines = visible_rows) and subsequent scroll_up calls.
#[kani::proof]
#[kani::unwind(9)] // 8 iterations + 1
fn ring_hyperlinks_invariant_across_erase() {
    let visible_rows: u16 = kani::any();
    let max_scrollback: usize = kani::any();
    kani::assume(visible_rows >= 2 && visible_rows <= KANI_MAX_ROWS);
    kani::assume(max_scrollback <= 4);

    let capacity = (visible_rows as usize) + max_scrollback;
    let mut total_lines = visible_rows as usize;

    // Phase A: scroll some rows into ring buffer.
    let pre_scrolls: u8 = kani::any();
    kani::assume(pre_scrolls <= 8);
    for _ in 0..pre_scrolls {
        let rows_to_add = 1usize.min(capacity.saturating_sub(total_lines));
        total_lines += rows_to_add;
    }

    // Erase scrollback: ring_hyperlinks cleared, total_lines reset.
    let mut ring_hyperlinks_len: usize = 0;
    total_lines = visible_rows as usize;

    let ring_sb = total_lines.saturating_sub(visible_rows as usize);
    kani::assert(
        ring_hyperlinks_len == ring_sb,
        "invariant violated after erase_scrollback",
    );

    // Phase B: scroll again after erase.
    let post_scrolls: u8 = kani::any();
    kani::assume(post_scrolls <= 8);
    for _ in 0..post_scrolls {
        let n: usize = kani::any();
        kani::assume(n >= 1 && n <= 3);

        let rows_to_add = n.min(capacity.saturating_sub(total_lines));
        ring_hyperlinks_len += rows_to_add;
        total_lines += rows_to_add;

        let ring_sb = total_lines.saturating_sub(visible_rows as usize);
        kani::assert(
            ring_hyperlinks_len == ring_sb,
            "invariant violated after post-erase scroll_up",
        );
    }
}
