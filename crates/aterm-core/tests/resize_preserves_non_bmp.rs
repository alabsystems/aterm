// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression: resizing must not strand on-screen non-BMP (emoji / CJK-SMP)
//! cells as U+FFFD, including bases stored separately from cluster tails.
//!
//! The hot write path stores a non-BMP codepoint in a per-viewport ComplexCharRing
//! (O(1), no alloc); the column-reflow resize path migrates those into the
//! persistent HashMap extras (#7447), but the rows-only / no-reflow paths used to
//! drop BOTH rings via `invalidate_rings` WITHOUT migrating — so every visible
//! emoji became U+FFFD. Lazy mid-session font injection made this fire in the wild:
//! installing a fallback/emoji face changes cell metrics, the host re-fits the grid
//! (a rows-only resize), and every emoji already on screen was corrupted.

use aterm_core::terminal::Terminal;

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

/// The parser stores the non-BMP base in a dense ring while skin-tone/ZWJ
/// suffixes live in the map. A map-only copy keeps the suffix and loses the
/// base, producing the observed `�🏽‍💻` after even a non-wrapping resize.
#[test]
fn column_reflow_preserves_parser_clusters_and_render_payloads() {
    const TEXT: &str = "Q👩🏽‍💻👍🏽🇯🇵🐈‍⬛e\u{0301}Z";
    for style in ["", "\x1b[1;36m", "\x1b[3m"] {
        let mut terminal = Terminal::new(28, 80);
        terminal.process(format!("{style}{TEXT}\x1b[0m").as_bytes());
        let extra = terminal.grid().cell_extra(0, 1).unwrap();
        assert!(extra.complex_char().is_none(), "base is ring-backed");
        assert_eq!(extra.combining(), &['🏽', '\u{200d}', '💻']);
        assert_eq!(
            terminal.grid().extras().complex_codepoint_for(0, 1),
            Some('👩')
        );
        let before = terminal.cell_frame(28, 80);
        assert_eq!(before.clusters[0].len(), 4);
        assert_eq!(before.combining[0].len(), 1);

        // No-wrap shrink, actual split, merge, and restoration all use the
        // real parser/grid path; no manufactured full-string extras mask it.
        for cols in [52, 7, 128, 80] {
            terminal.resize(28, cols);
            let text: String = (0..28)
                .filter_map(|r| terminal.row_text(r))
                .map(|row| row.trim_end().to_owned())
                .collect();
            assert_eq!(text, TEXT, "style={style:?}, cols={cols}");
            let after = terminal.cell_frame(28, usize::from(cols));
            assert_eq!(
                after
                    .clusters
                    .iter()
                    .flatten()
                    .map(|(_, s)| s.as_ref())
                    .collect::<Vec<_>>(),
                before
                    .clusters
                    .iter()
                    .flatten()
                    .map(|(_, s)| s.as_ref())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                after
                    .combining
                    .iter()
                    .flatten()
                    .map(|(_, m)| m.as_ref())
                    .collect::<Vec<_>>(),
                before
                    .combining
                    .iter()
                    .flatten()
                    .map(|(_, m)| m.as_ref())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                after
                    .cells
                    .iter()
                    .flatten()
                    .filter(|c| c.ch != ' ')
                    .collect::<Vec<_>>(),
                before
                    .cells
                    .iter()
                    .flatten()
                    .filter(|c| c.ch != ' ')
                    .collect::<Vec<_>>(),
                "glyphs, width, style and color must survive reflow"
            );
        }
    }
}

#[test]
fn column_reflow_preserves_ring_rgb_with_structured_extras() {
    let mut terminal = Terminal::new(8, 80);
    terminal.process(
        concat!(
            "\x1b[38;2;17;23;39;48;2;41;47;53me\u{0301}",
            "\x1b]8;id=reflow;https://example.com/reflow\x1b\\👩🏽‍💻",
            "\x1b]8;;\x1b\\\x1b[0m"
        )
        .as_bytes(),
    );
    let extra = terminal.grid().cell_extra(0, 0).unwrap();
    assert_eq!(extra.combining(), &['\u{0301}']);
    assert!(
        extra.fg_rgb().is_none() && extra.bg_rgb().is_none(),
        "colors are ring-backed"
    );
    assert_eq!(
        terminal.grid().extras().fg_rgb_for(0, 0),
        Some([17, 23, 39])
    );
    assert_eq!(
        terminal.grid().extras().bg_rgb_for(0, 0),
        Some([41, 47, 53])
    );
    let before = terminal.cell_frame(8, 80);
    for cols in [52, 128, 80] {
        terminal.resize(8, cols);
        assert_eq!(terminal.row_text(0).unwrap().trim_end(), "e\u{0301}👩🏽‍💻");
        assert_eq!(
            terminal.hyperlink_at(0, 1),
            Some("https://example.com/reflow")
        );
        let after = terminal.cell_frame(8, usize::from(cols));
        assert_eq!(&after.cells[0][..3], &before.cells[0][..3]);
        assert_eq!(after.clusters[0], before.clusters[0]);
        assert_eq!(after.combining[0], before.combining[0]);
    }
}
