// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GRID IS THE AUTHORITY ON COLUMNS, AND EVERY MEASURER MUST AGREE WITH IT.
//!
//! The terminal does not allocate cells per grapheme CLUSTER. It advances the
//! cursor per CHARACTER as it writes, with a stateful exception for emoji (VS16
//! widens, VS15 narrows, ZWJ and skin-tone modifiers fold into the preceding
//! cell). Three shipped surfaces re-derived columns from the extracted text with
//! their own rules, and all three disagreed with the paint on the same content:
//!
//! * the find bar (`aterm_search`'s `ColumnMap`) charged `str_width(g).min(2)`;
//! * a scrollback selection (`column_range_to_byte_offsets`) and
//! * double-click word selection (`aterm_selection`'s `column_to_byte_pos`)
//!   charged the cluster's display width.
//!
//! A Devanagari conjunct is ONE cluster under UAX#29 GB9c and the grid paints it
//! in THREE cells, so every column after it named a cell to the LEFT of the
//! text: the find bar tinted a blank cell and dropped the match's last
//! character, and a copied selection extracted a different run of cells than the
//! one highlighted. Keycap and VS16 emoji drifted the other way.
//!
//! This file drives the REAL grid and is the authority
//! `aterm_grapheme::grapheme_grid_columns` is checked against — a second model
//! is exactly what caused the bug, so the function is pinned to measurement
//! rather than to a derivation.

use aterm_core::terminal::Terminal;

/// Samples chosen so each disagreeing rule is represented, plus the ordinary
/// cases that must not move. The marker `|` follows each sample: the column the
/// grid paints it at IS the sample's cell count.
const SAMPLES: &[(&str, &str)] = &[
    ("ascii", "ab"),
    ("cjk", "\u{4e2d}\u{6587}"),
    ("latin + combining", "e\u{301}"),
    ("devanagari ka + vowel II", "\u{915}\u{940}"),
    (
        "devanagari conjunct na+virama+da+II",
        "\u{928}\u{94d}\u{926}\u{940}",
    ),
    ("bengali conjunct", "\u{995}\u{9cd}\u{9b7}\u{9be}"),
    ("keycap 1 + VS16 + U+20E3", "1\u{fe0f}\u{20e3}"),
    (
        "emoji ZWJ family",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
    ),
    ("emoji + skin tone", "\u{1f44d}\u{1f3fd}"),
    ("regional indicator flag", "\u{1f1fa}\u{1f1f8}"),
    ("VS15 text presentation", "\u{2764}\u{fe0e}"),
    ("VS16 emoji presentation", "\u{2764}\u{fe0f}"),
];

/// The column the grid paints `marker` at, having written `prefix` first — i.e.
/// the number of cells `prefix` really occupies.
fn painted_column(prefix: &str, marker: &str) -> Option<u16> {
    let mut term = Terminal::new(3, 60);
    term.process(format!("{prefix}{marker}").as_bytes());
    (0..60u16).find(|&c| term.get_line_text(0, Some((c, c))).as_deref() == Some(marker))
}

#[test]
fn grapheme_grid_columns_agrees_with_what_the_grid_paints() {
    let mut wrong = Vec::new();
    for (label, sample) in SAMPLES {
        let painted = painted_column(sample, "|").expect("the marker is painted somewhere");
        let modelled = aterm_grapheme::str_grid_columns(sample);
        if usize::from(painted) != modelled {
            wrong.push(format!("{label}: grid {painted} vs model {modelled}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "`grapheme_grid_columns` disagrees with the grid it models:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_find_bar_reports_the_column_the_grid_paints() {
    let mut wrong = Vec::new();
    for (label, sample) in SAMPLES {
        let mut term = Terminal::new(3, 60);
        term.process(format!("{sample} ZEBRA").as_bytes());
        let painted = (0..60u16)
            .find(|&c| term.get_line_text(0, Some((c, c))).as_deref() == Some("Z"))
            .expect("the needle is painted somewhere");
        let found = term
            .indexed_search()
            .search_results_opts("ZEBRA", false, false)
            .expect("search runs");
        let reported = found.matches.first().map(|m| m.start_col);
        if reported != Some(usize::from(painted)) {
            wrong.push(format!(
                "{label}: painted {painted} vs reported {reported:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "a find-bar match names a cell the text is not in — the highlight lands \
         on the wrong cells:\n  {}",
        wrong.join("\n  ")
    );
}

/// The reported defect, at the granularity it is actually felt: a selection
/// taken from a SCROLLBACK row must copy the text the highlight covers.
///
/// Asserted over a RANGE, not cell by cell, because a stored line is text and a
/// single-cell query inside a multi-cell cluster can only return the whole
/// cluster — you cannot extract half a grapheme from a string. That is a
/// representation difference, not the bug. The bug was the RUN: the columns the
/// highlight covers and the bytes the copy extracts were derived by two
/// different rules, so they named different text.
#[test]
fn a_scrollback_selection_copies_the_text_the_highlight_covers() {
    let mut wrong = Vec::new();
    for (label, sample) in SAMPLES {
        let line = format!("{sample} ZEBRA");
        // Where the grid PAINTS the needle — the columns a highlight would cover.
        let mut term = Terminal::new(3, 60);
        term.process(line.as_bytes());
        let first = (0..60u16)
            .find(|&c| term.get_line_text(0, Some((c, c))).as_deref() == Some("Z"))
            .expect("the needle is painted somewhere");
        let last = first + 4; // ZEBRA is five single-cell characters

        // The same line in SCROLLBACK, copied over those columns.
        let mut term = Terminal::new(3, 60);
        term.process(format!("{line}\r\n\r\n\r\n\r\n\r\n").as_bytes());
        let Some(row) = (1..=8i32).map(|n| -n).find(|&r| {
            term.get_line_text(r, None)
                .unwrap_or_default()
                .contains("ZEBRA")
        }) else {
            wrong.push(format!("{label}: the line never reached scrollback"));
            continue;
        };
        let copied = term
            .get_line_text(row, Some((first, last)))
            .unwrap_or_default();
        if copied != "ZEBRA" {
            wrong.push(format!(
                "{label}: the highlight covers cols {first}..={last} (painted \
                 \"ZEBRA\") but the copy extracts {copied:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "a scrollback selection copies different text than the highlight \
         covers:\n  {}",
        wrong.join("\n  ")
    );
}
