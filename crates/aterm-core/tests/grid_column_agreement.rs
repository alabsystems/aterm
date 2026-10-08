// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The grid is the authority on columns; every measurer must agree with it.
//!
//! The terminal advances the cursor per character, not per grapheme cluster,
//! with a stateful exception for emoji (VS16 widens, VS15 narrows, ZWJ and
//! skin-tone modifiers fold into the preceding cell). A Devanagari conjunct is
//! one UAX#29 cluster painted in three cells; keycap and VS16 emoji differ the
//! other way. The find bar, scrollback selection and word selection must all
//! use the grid's count.
//!
//! This file drives the real grid and is what
//! `aterm_grapheme::GridColumns` is checked against.

use aterm_core::terminal::Terminal;

/// One sample per width rule, plus ordinary cases. The marker `|` follows each
/// sample: the column the grid paints it at is the sample's cell count.
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
    // A skin-tone modifier folds only onto a WIDE Emoji_Modifier_Base; after
    // anything else the grid draws it as its own 2-cell glyph.
    ("ninja + skin tone", "\u{1f977}\u{1f3fd}"),
    ("non-base emoji + skin tone", "\u{1f680}\u{1f3fd}"),
    ("non-base heart + skin tone", "\u{1f90d}\u{1f3fd}"),
    ("text-default base + skin tone", "\u{261d}\u{1f3fd}"),
    (
        "text-default base + VS16 + skin tone",
        "\u{261d}\u{fe0f}\u{1f3fd}",
    ),
    ("regional indicator flag", "\u{1f1fa}\u{1f1f8}"),
    ("VS15 text presentation", "\u{2764}\u{fe0e}"),
    ("VS16 emoji presentation", "\u{2764}\u{fe0f}"),
    // VS16 widens every narrow emoji-capable base, not only the Dingbats and
    // pictograph blocks: copyright, play, information, ‼ and ↔ draw 2 wide.
    ("VS16 copyright", "\u{a9}\u{fe0f}"),
    ("VS16 play button", "\u{25b6}\u{fe0f}"),
    ("VS16 information", "\u{2139}\u{fe0f}"),
    ("VS16 double exclamation", "\u{203c}\u{fe0f}"),
    ("VS16 left-right arrow", "\u{2194}\u{fe0f}"),
    ("VS16 on a letter", "a\u{fe0f}"),
    // A character after a ZWJ folds into the cell before it, whatever that
    // cell's width: a minimally-qualified ZWJ emoji keeps its narrow base's
    // one cell, and an explicit Devanagari half form is one cell.
    ("ZWJ emoji on a narrow base", "\u{2764}\u{200d}\u{1f525}"),
    (
        "devanagari explicit half form",
        "\u{915}\u{94d}\u{200d}\u{937}",
    ),
    // The grid's fold state crosses UAX #29 cluster boundaries. Sinhala
    // yansaya and rakaransaya end a cluster with a ZWJ (its virama links no
    // conjunct) and the grid folds the next cluster's letter into the same
    // cell; a doubled ZWJ, or a mark between ZWJs, ends a cluster before a
    // pictograph the grid still folds; a flag's second half pairs past a mark.
    ("sinhala yansaya", "\u{d9a}\u{dca}\u{200d}\u{dba}"),
    ("sinhala sri", "\u{dc1}\u{dca}\u{200d}\u{dbb}\u{dd3}"),
    ("emoji ZWJ ZWJ emoji", "\u{1f44d}\u{200d}\u{200d}\u{1f44d}"),
    (
        "emoji ZWJ mark ZWJ emoji",
        "\u{1f44d}\u{200d}\u{308}\u{200d}\u{1f44d}",
    ),
    ("flag halves around a mark", "\u{1f1fa}\u{301}\u{1f1f8}"),
    // ASCII takes the grid's bulk path, which folds nothing after a ZWJ.
    ("ASCII after a ZWJ", "\u{1f44d}\u{200d}a"),
];

/// The column the grid paints `marker` at, having written `prefix` first — i.e.
/// the number of cells `prefix` really occupies.
fn painted_column(prefix: &str, marker: &str) -> Option<u16> {
    let mut term = Terminal::new(3, 60);
    term.process(format!("{prefix}{marker}").as_bytes());
    (0..60u16).find(|&c| term.get_line_text(0, Some((c, c))).as_deref() == Some(marker))
}

#[test]
fn grid_columns_agree_with_what_the_grid_paints() {
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
        "`str_grid_columns` disagrees with the grid it models:\n  {}",
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

/// A selection taken from a scrollback row copies the text the highlight
/// covers.
///
/// Asserted over a range, not per cell: a stored line is text, so a
/// single-cell query inside a multi-cell cluster returns the whole cluster.
#[test]
fn a_scrollback_selection_copies_the_text_the_highlight_covers() {
    let mut wrong = Vec::new();
    for (label, sample) in SAMPLES {
        let line = format!("{sample} ZEBRA");
        // Where the grid paints the needle — the columns a highlight would cover.
        let mut term = Terminal::new(3, 60);
        term.process(line.as_bytes());
        let first = (0..60u16)
            .find(|&c| term.get_line_text(0, Some((c, c))).as_deref() == Some("Z"))
            .expect("the needle is painted somewhere");
        let last = first + 4; // ZEBRA is five single-cell characters

        // The same line in scrollback, copied over those columns.
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

/// A grapheme the grid folds into the cell before it is drawn in that cell:
/// the `ය` of Sinhala `ක්‍ය`, where a ZWJ ends the first UAX #29 cluster. A find
/// match on it highlights that cell, and copying that cell from scrollback
/// copies both clusters, as copying it from the live grid does.
#[test]
fn a_grapheme_folded_across_a_cluster_boundary_lives_in_the_cell_it_joined() {
    let yansaya = "\u{d9a}\u{dca}\u{200d}\u{dba}";
    let line = format!("x {yansaya} y");
    let mut term = Terminal::new(3, 60);
    term.process(line.as_bytes());
    assert_eq!(
        term.get_line_text(0, Some((2, 2))).as_deref(),
        Some(yansaya),
        "the grid draws the conjunct in one cell"
    );
    let found = term
        .indexed_search()
        .search_results_opts("\u{dba}", false, false)
        .expect("search runs");
    let reported = found.matches.first().map(|m| (m.start_col, m.end_col));
    assert_eq!(
        reported,
        Some((2, 3)),
        "the match is in the conjunct's cell"
    );

    term.process(b"\r\n\r\n\r\n\r\n\r\n");
    let row = (1..=8i32)
        .map(|n| -n)
        .find(|&r| {
            term.get_line_text(r, None)
                .unwrap_or_default()
                .contains(yansaya)
        })
        .expect("the line reaches scrollback");
    assert_eq!(
        term.get_line_text(row, Some((2, 2))).as_deref(),
        Some(yansaya),
        "a scrollback copy of the cell copies what the cell holds"
    );
}
