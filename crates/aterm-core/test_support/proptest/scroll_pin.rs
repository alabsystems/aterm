// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SCR-1 absolute-content pinning — property test.
//!
//! Generalizes the single hand-picked `scrolled_back_viewport_pins_during_live_output`
//! unit test (src/terminal/processing.rs) over random prefill / scroll-back / live
//! volumes: while the user is scrolled into history, incremental live output must keep
//! the SAME ABSOLUTE row at the viewport top, and `display_offset` must advance by
//! exactly the number of lines that entered scrollback. This is the engine invariant
//! the renderer's scroll-restore relies on; proving it general isolates any
//! scroll-restore regression to the TS pin lifecycle, not the engine.

use crate::terminal::TerminalBuilder;
use proptest::prelude::*;

proptest! {
    /// SCR-1: live output while scrolled back preserves the absolute top row and
    /// advances display_offset by the lines that entered scrollback (big ring => no
    /// eviction within these bounds, so the advance is exact).
    #[test]
    fn scrollback_pin_preserves_absolute_top_during_live_output(
        prefill in 30usize..120,
        scroll_back in 1usize..25,
        live in 1usize..60,
    ) {
        // 6-row screen, large ring so the pinned row is never evicted in-bounds.
        let mut term = TerminalBuilder::new().size(6, 40).ring_buffer_size(4000).build();
        // Each prefill line is unique ("L#####") so the top-row content identifies an
        // ABSOLUTE buffer line, not a relative position.
        for i in 0..prefill {
            term.process(format!("L{i:05}\r\n").as_bytes());
        }

        term.scroll_display(scroll_back as i32);
        let offset_before = term.grid().display_offset();
        // Only meaningful when the user is actually scrolled back (not clamped to 0).
        prop_assume!(offset_before > 0);
        let top_before = term.display_row_text(0).unwrap_or_default();
        prop_assume!(top_before.contains("L"));

        // Feed live output (tail -f): distinct content so a drift would show.
        for i in 0..live {
            term.process(format!("X{i:05}\r\n").as_bytes());
        }

        // 1) ABSOLUTE preservation: the SAME content stays at the viewport top.
        let top_after = term.display_row_text(0).unwrap_or_default();
        prop_assert_eq!(
            &top_after, &top_before,
            "SCR-1: pinned content moved under live output (top was {:?}, now {:?})",
            top_before, top_after
        );
        // 2) display_offset advanced by EXACTLY the lines that entered scrollback.
        let offset_after = term.grid().display_offset();
        prop_assert_eq!(
            offset_after, offset_before + live,
            "display_offset must advance by the {} lines entering scrollback", live,
        );
        // 3) live output must not be visible while scrolled back.
        for r in 0..6 {
            let row = term.display_row_text(r).unwrap_or_default();
            prop_assert!(
                !row.contains('X'),
                "live output leaked into the pinned viewport at row {}: {:?}", r, row,
            );
        }
    }
}
