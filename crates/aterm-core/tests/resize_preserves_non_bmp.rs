// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression: a rows-only (cols-unchanged) resize must not strand on-screen
//! non-BMP (emoji / CJK-SMP) cells as U+FFFD.
//!
//! The hot write path stores a non-BMP codepoint in a per-viewport ComplexCharRing
//! (O(1), no alloc); the column-reflow resize path migrates those into the
//! persistent HashMap extras (#7447), but the rows-only / no-reflow paths used to
//! drop BOTH rings via `invalidate_rings` WITHOUT migrating — so every visible
//! emoji became U+FFFD. Lazy mid-session font injection made this fire in the wild:
//! installing a fallback/emoji face changes cell metrics, the host re-fits the grid
//! (a rows-only resize), and every emoji already on screen was corrupted.

use aterm_core::prelude::Terminal;

/// Q😀Z written through the real hot path (process) lands 😀 in the complex ring.
const EMOJI_LINE: &[u8] = b"Q\xf0\x9f\x98\x80Z"; // Q😀Z

fn emoji_row(t: &Terminal, max_rows: u16) -> Option<String> {
    (0..max_rows).find_map(|r| {
        let text = t.row_text(usize::from(r))?;
        let trimmed = text.trim_end().to_string();
        (trimmed.starts_with('Q') && trimmed.contains('Z')).then_some(trimmed)
    })
}

#[test]
fn rows_only_grow_preserves_non_bmp_cell() {
    let mut t = Terminal::new(24, 80);
    t.process(EMOJI_LINE);
    assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("Q😀Z"));

    // Rows GROW, cols UNCHANGED (the path that used to skip complex-ring migration).
    t.resize(48, 80);
    assert_eq!(
        emoji_row(&t, 48).as_deref(),
        Some("Q😀Z"),
        "non-BMP emoji must survive a rows-only grow, not become U+FFFD"
    );
}

#[test]
fn rows_only_shrink_preserves_non_bmp_cell() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\r\n");
    t.process(EMOJI_LINE); // emoji on row 1 (not the top row)
    t.resize(12, 80); // rows shrink, cols unchanged
    assert_eq!(
        emoji_row(&t, 12).as_deref(),
        Some("Q😀Z"),
        "non-BMP emoji must survive a rows-only shrink"
    );
}

#[test]
fn font_refit_style_resize_preserves_non_bmp_cell() {
    // The exact shape of the wild bug: a small rows delta with the emoji already
    // on a non-top row (a font-metrics re-fit right after the emoji rendered).
    let mut t = Terminal::new(55, 128);
    t.process(b"\x1b]133;C\x07"); // OSC 133;C command mark, like the shell
    t.process(b"\r\n");
    t.process(b"Q\xf0\x9f\x98\x80A\xf0\x9f\x98\x80Z"); // two emoji: Q😀A😀Z
    assert_eq!(emoji_row(&t, 55).as_deref(), Some("Q😀A😀Z"));

    t.resize(61, 128); // font-refit re-fit: rows 55 -> 61, cols unchanged
    assert_eq!(
        emoji_row(&t, 61).as_deref(),
        Some("Q😀A😀Z"),
        "both on-screen emoji must survive the font-refit rows-only resize"
    );
}

#[test]
fn column_reflow_still_preserves_non_bmp_cell() {
    // Guard the pre-existing column-reflow migration (#7447) still works.
    let mut t = Terminal::new(24, 80);
    t.process(EMOJI_LINE);
    t.resize(50, 200); // cols change -> column reflow path
    assert_eq!(emoji_row(&t, 50).as_deref(), Some("Q😀Z"));
}
