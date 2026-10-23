// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resizing preserves on-screen non-BMP cells and cluster extras across rows-only and reflow paths.

use aterm_core::terminal::Terminal;

const EMOJI_LINE: &[u8] = b"Q\xf0\x9f\x98\x80Z"; // Q😀Z

fn emoji_row(t: &Terminal, max_rows: u16) -> Option<String> {
    (0..max_rows).find_map(|r| {
        let text = t.row_text(usize::from(r))?;
        let trimmed = text.trim_end().to_string();
        (trimmed.starts_with('Q') && trimmed.contains('Z')).then_some(trimmed)
    })
}

#[test]
fn every_resize_path_preserves_non_bmp_cells() {
    type Case = (
        &'static str,
        (u16, u16),
        &'static [u8],
        (u16, u16),
        &'static str,
    );
    let cases: [Case; 4] = [
        ("rows-only grow", (24, 80), EMOJI_LINE, (48, 80), "Q😀Z"),
        (
            "rows-only shrink",
            (24, 80),
            b"\r\nQ\xf0\x9f\x98\x80Z",
            (12, 80),
            "Q😀Z",
        ),
        (
            "font re-fit",
            (55, 128),
            b"\x1b]133;C\x07\r\nQ\xf0\x9f\x98\x80A\xf0\x9f\x98\x80Z",
            (61, 128),
            "Q😀A😀Z",
        ),
        ("column reflow", (24, 80), EMOJI_LINE, (50, 200), "Q😀Z"),
    ];
    for (name, (rows, cols), input, (to_rows, to_cols), want) in cases {
        let mut t = Terminal::new(rows, cols);
        t.process(input);
        assert_eq!(emoji_row(&t, rows).as_deref(), Some(want), "{name}: before");
        t.resize(to_rows, to_cols);
        assert_eq!(
            emoji_row(&t, to_rows).as_deref(),
            Some(want),
            "{name}: the non-BMP cells must survive, not become U+FFFD"
        );
    }
}

/// The parser stores the non-BMP base in a dense ring while skin-tone/ZWJ
/// suffixes live in the map; a map-only copy would keep the suffix and lose
/// the base (`�🏽‍💻`).
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

#[test]
fn a_width_change_that_brings_a_line_head_back_preserves_non_bmp_cells() {
    for (line, tail) in [
        ("abcdefghijklmnopq\u{1F600}", "pq\u{1F600}"),
        ("abcdefghijklmnop\u{1D504}z", "p\u{1D504}z"),
        (
            "abcdefghijklmnop\u{1F468}\u{200d}\u{1F469}",
            "p\u{1F468}\u{200d}\u{1F469}",
        ),
    ] {
        let mut t = Terminal::new(10, 8);
        t.process(format!("\x1b[48;2;1;2;3m{line}\x1b[0m\r\n").as_bytes());
        t.process(&b"x\r\n".repeat(8));
        assert!(
            t.grid().row(0).is_some_and(|row| row.is_wrapped()),
            "precondition: the top row continues a history line"
        );
        t.resize(10, 15);
        let row = (0..10u16).find(|&r| {
            t.row_text(usize::from(r))
                .is_some_and(|text| text.trim_end() == tail)
        });
        assert!(
            row.is_some(),
            "{line:?}: the tail {tail:?} must survive, not become U+FFFD: {:?}",
            (0..10).filter_map(|r| t.row_text(r)).collect::<Vec<_>>()
        );
        let row = row.unwrap_or(0);
        let col = u16::try_from(
            tail.chars()
                .position(|c| u32::from(c) > 0xFFFF)
                .unwrap_or(0),
        )
        .unwrap_or(0);
        assert_eq!(
            t.grid().extras().bg_rgb_for(row, col),
            Some([1, 2, 3]),
            "{line:?}: its true colour survives too"
        );
    }
}
