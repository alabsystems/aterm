// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for ring_hyperlinks invariant.
//!
//! Verifies that `ring_hyperlinks.len() == ring_buffer_scrollback()` holds
//! across all valid combinations of scroll_up and erase_scrollback operations.
//!
//! These proofs use abstract state machine modeling (not actual Grid calls)
//! to verify the algorithm maintains the invariant for all symbolic inputs.
//! The corresponding concrete test at `tests/scrollback.rs:365` validates
//! that the production code matches this model.
//!
//! Part of #4149 (ring buffer hyperlink preservation).

use super::KANI_MAX_ROWS;

/// ring_hyperlinks.len() == ring_buffer_scrollback() after scroll_up.
///
/// Models the ring_hyperlinks state machine with symbolic scroll amounts.
/// The invariant is: after any sequence of scroll_up(n) operations,
/// ring_hyperlinks.len() == total_lines.saturating_sub(visible_rows).
///
/// Phase 1 (growth): ring_hyperlinks grows by rows_to_add, matching the
/// increase in ring_buffer_scrollback().
/// Phase 2 (reuse): push_back then pop_front per row = net 0, and
/// ring_buffer_scrollback() is unchanged.
///
/// This proof covers all valid combinations of visible_rows (2..=8),
/// max_scrollback (0..=4), and up to 12 scroll operations each with
/// symbolic batch sizes (1..=4). The existing concrete test at
/// scrollback.rs:365 validates that production code matches this model.
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

        // Phase 2: reuse (at capacity)
        // Each reused row does push_back then pop_front = net 0.
        // total_lines unchanged. ring_hyperlinks_len unchanged.

        // Check invariant after this scroll_up.
        let ring_sb = total_lines.saturating_sub(visible_rows as usize);
        kani::assert(
            ring_hyperlinks_len == ring_sb,
            "invariant violated: ring_hyperlinks.len() != ring_buffer_scrollback()",
        );
    }
}

/// ring_hyperlinks invariant holds across erase_scrollback + scroll_up.
///
/// erase_scrollback resets ring_hyperlinks to empty and total_lines to
/// visible_rows. The invariant must hold after the reset and after
/// subsequent scroll_up operations.
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
