// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SCR-1 absolute-content pinning over random volumes.
//!
//! Generalizes `scrolled_back_viewport_pins_during_live_output`
//! (src/terminal/processing.rs) over random prefill / scroll-back / live
//! volumes: while the user is scrolled into history, incremental live output
//! must keep the SAME ABSOLUTE row at the viewport top, and `display_offset`
//! must advance by exactly the number of lines that entered scrollback. This is
//! the engine invariant the renderer's scroll-restore relies on.

use crate::terminal::TerminalBuilder;
use aterm_spec::rng::XorShift;

#[test]
fn scrollback_pin_preserves_absolute_top_during_live_output() {
    let mut rng = XorShift::new(0x5C2_0011);
    for case in 0..128 {
        let prefill = rng.range(30, 120);
        let scroll_back = rng.range(1, 25);
        let live = rng.range(1, 60);
        // 6-row screen, large ring so the pinned row is never evicted in-bounds.
        let mut term = TerminalBuilder::new()
            .size(6, 40)
            .ring_buffer_size(4000)
            .build();
        // Each prefill line is unique ("L#####") so the top-row content
        // identifies an ABSOLUTE buffer line, not a relative position.
        for i in 0..prefill {
            term.process(format!("L{i:05}\r\n").as_bytes());
        }

        term.scroll_display(scroll_back as i32);
        let offset_before = term.grid().display_offset();
        assert!(offset_before > 0, "case {case}: the prefill scrolls back");
        let top_before = term.display_row_text(0).unwrap_or_default();
        assert!(top_before.contains('L'), "case {case}: {top_before:?}");

        // Feed live output (tail -f): distinct content so a drift would show.
        for i in 0..live {
            term.process(format!("X{i:05}\r\n").as_bytes());
        }

        // 1) absolute preservation: the same content stays at the viewport top.
        let top_after = term.display_row_text(0).unwrap_or_default();
        assert_eq!(
            top_after, top_before,
            "case {case}: pinned content moved under live output"
        );
        // 2) display_offset advanced by EXACTLY the lines that entered scrollback.
        assert_eq!(
            term.grid().display_offset(),
            offset_before + live,
            "case {case}: display_offset must advance by the {live} lines entering scrollback",
        );
        // 3) live output must not be visible while scrolled back.
        for r in 0..6 {
            let row = term.display_row_text(r).unwrap_or_default();
            assert!(
                !row.contains('X'),
                "case {case}: live output leaked into the pinned viewport at row {r}: {row:?}",
            );
        }
    }
}
