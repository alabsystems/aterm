// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::{EditorSelectionSpan, Selection, editor};

fn uncapped_line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            text.as_bytes()
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
        )
        .collect()
}

#[test]
fn projected_column_limit_is_the_clamp_it_replaced() {
    for columns in [0, 3, 4, 5, 80, 4_095, 4_096, 4_097, usize::MAX] {
        assert_eq!(projected_column_limit(columns), columns.clamp(4, 4_096));
    }
}

#[test]
fn indexed_line_oracle_matches_the_scanning_one() {
    for text in [
        "",
        "\n",
        "one",
        "one\n",
        "one\ntwo",
        "one\ntwo\n",
        "\n\n\n",
        "α\nβγ\n\nδ",
    ] {
        let starts = line_starts_capped(text).expect("short fixture fits under cap");
        let scanning = EditorLines::scanning(text);
        let indexed = EditorLines::indexed(text, &starts);
        assert_eq!(scanning.total(), indexed.total(), "total for {text:?}");
        for position in 0..=text.len() + 2 {
            assert_eq!(
                scanning.number_at(position),
                indexed.number_at(position),
                "number_at({position}) for {text:?}"
            );
        }
        for line in 0..scanning.total() + 2 {
            assert_eq!(
                scanning.byte_of(line),
                indexed.byte_of(line),
                "byte_of({line}) for {text:?}"
            );
        }
    }
}

#[test]
fn editor_cell_widths_share_tab_unicode_and_suppressed_control_semantics() {
    assert_eq!(editor_grapheme_columns("\t", 0), 4);
    assert_eq!(editor_grapheme_columns("\t", 3), 1);
    assert_eq!(editor_grapheme_columns("e\u{301}", 0), 1);
    assert_eq!(editor_grapheme_columns("中", 0), 2);
    assert_eq!(editor_grapheme_columns("👩‍💻", 0), 2);
    assert_eq!(editor_grapheme_columns("\u{0001}", 17), 0);
}

#[test]
fn viewport_projection_is_visible_only_utf8_safe_and_source_addressable() {
    let text = "zero\nβeta\nlast\n";
    let (_, _, mut view, _) = editor(text);
    view.viewport_anchor = "zero\n".len() + 1;
    view.selections = vec![
        Selection {
            anchor: "zero\n".len(),
            head: "zero\nβeta\nlast".len(),
        },
        Selection::caret("zero\nβ".len()),
    ];
    view.primary = 0;

    let projection = project_viewport_with(&EditorLines::scanning(text), &view, 2, usize::MAX);
    assert_eq!(projection.first_line, 1);
    assert_eq!(projection.total_lines, 4);
    assert_eq!(projection.lines.len(), 2, "projection stays visible-only");
    assert_eq!(projection.lines[0].number, 1);
    assert_eq!(
        projection.lines[0].source,
        "zero\n".len().."zero\nβeta".len()
    );
    assert_eq!(projection.lines[0].text, "βeta");
    assert_eq!(projection.lines[0].carets, vec![("β".len(), false)]);
    assert_eq!(
        projection.lines[0].selections,
        vec![EditorSelectionSpan {
            bytes: 0.."βeta".len(),
            continues: true,
            primary: true,
        }]
    );
    assert_eq!(projection.lines[1].text, "last");
}

#[test]
fn viewport_projection_keeps_trailing_empty_line_and_selected_newline_cell() {
    let text = "a\n";
    let (_, _, mut view, _) = editor(text);
    view.selections = vec![Selection { anchor: 0, head: 2 }];

    let projection =
        project_viewport_with(&EditorLines::scanning(text), &view, usize::MAX, usize::MAX);
    assert_eq!(projection.total_lines, 2);
    assert_eq!(projection.lines.len(), 2);
    assert_eq!(projection.lines[0].text, "a");
    assert_eq!(projection.lines[0].selections[0].bytes, 0..1);
    assert!(projection.lines[0].selections[0].continues);
    assert_eq!(projection.lines[1].source, 2..2);
    assert_eq!(projection.lines[1].text, "");
    assert_eq!(projection.lines[1].carets, vec![(0, true)]);
}

#[test]
fn compact_horizontal_window_follows_caret_and_is_shared_across_rows() {
    let line = "0123456789".repeat(24);
    let text = format!("{line}\n{line}\n{line}");
    let (_, _, mut view, _) = editor(&text);
    let caret = 173;
    view.selections = vec![Selection::caret(caret)];

    let projection = project_viewport_with(&EditorLines::scanning(&text), &view, 3, 40);
    assert_eq!(projection.lines.len(), 3);
    let expected_start = caret - 30;
    for (row, projected) in projection.lines.iter().enumerate() {
        let line_start = row * (line.len() + 1);
        assert_eq!(projected.source.start, line_start + expected_start);
        assert_eq!(projected.column_start, expected_start);
        assert!(projected.text.chars().count() <= 40);
        assert!(text.is_char_boundary(projected.source.start));
        assert!(text.is_char_boundary(projected.source.end));
    }
    assert_eq!(projection.lines[0].carets, vec![(30, true)]);
    assert!(
        projection.lines[0].source.start > 0,
        "negative control: the old projection always began ordinary lines at byte zero"
    );
}

/// THE WINDOW SCROLLS NO FURTHER THAN THE CARET'S LINE NEEDS: the window put the caret three
/// quarters of the way across whatever the line held, so a caret at the end of
/// `agents_auto_prime = false` (column 25) in a 29-column window started every row at column
/// 4 ("ts_auto_prime", "ate]", "led = false") although the line fitted whole. Where the
/// caret's line ends (the caret after it) inside the window it stops there: a line that fits
/// shows from its first column, and a long line's end lands at the right edge.
#[test]
fn the_horizontal_window_scrolls_no_further_than_the_caret_line_needs() {
    let text = "agents_auto_prime = false\n[update]\nenabled = false\n";
    let (_, _, mut view, _) = editor(text);
    view.selections = vec![Selection::caret(25)];
    let projection = project_viewport_with(&EditorLines::scanning(text), &view, 3, 29);
    let texts: Vec<&str> = projection
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(
        texts,
        ["agents_auto_prime = false", "[update]", "enabled = false"]
    );
    assert!(projection.lines.iter().all(|line| line.column_start == 0));
    assert_eq!(projection.lines[0].carets, vec![(25, true)]);

    // A line one column too wide for the caret after it scrolls one column, not to 3/4.
    let view_at = |caret: usize| {
        let (_, _, mut view, _) = editor(text);
        view.selections = vec![Selection::caret(caret)];
        view
    };
    let projection = project_viewport_with(&EditorLines::scanning(text), &view_at(25), 3, 25);
    assert_eq!(projection.lines[0].column_start, 1);
    assert_eq!(projection.lines[0].text, "gents_auto_prime = false");

    // In a long line the caret still keeps a quarter of the window ahead of it, until the
    // line's end comes into view.
    let long = format!("{}\n", "0123456789".repeat(10));
    let (_, _, mut view, _) = editor(&long);
    for (caret, start) in [(50, 20), (80, 50), (85, 55), (91, 61), (100, 61)] {
        view.selections = vec![Selection::caret(caret)];
        let projection = project_viewport_with(&EditorLines::scanning(&long), &view, 1, 40);
        assert_eq!(
            projection.lines[0].column_start, start,
            "caret at column {caret}"
        );
        let carets = &projection.lines[0].carets;
        assert_eq!(
            carets,
            &vec![(caret - start, true)],
            "caret at column {caret}"
        );
    }
}

#[test]
fn compact_horizontal_window_is_utf8_and_tab_safe() {
    let line = format!("{}\t{}", "α".repeat(80), "終".repeat(80));
    let text = format!("{line}\n{line}");
    let (_, _, mut view, _) = editor(&text);
    let caret = "α".len() * 70;
    view.selections = vec![Selection::caret(caret)];

    let projection = project_viewport_with(&EditorLines::scanning(&text), &view, 2, 32);
    assert_eq!(projection.lines.len(), 2);
    assert!(projection.lines[0].source.start > 0);
    assert_eq!(projection.lines[0].column_start, 46);
    assert_eq!(projection.lines[1].column_start, 46);
    assert!(
        projection.lines[0]
            .carets
            .iter()
            .any(|(byte, primary)| { *primary && *byte <= projection.lines[0].text.len() })
    );
    for line in &projection.lines {
        assert!(text.is_char_boundary(line.source.start));
        assert!(text.is_char_boundary(line.source.end));
    }
}

#[test]
fn compact_horizontal_window_never_splits_zwj_or_combining_graphemes() {
    let text = format!("{}👩‍💻x", "a".repeat(31));
    let (_, _, mut view, _) = editor(&text);
    view.selections = vec![Selection::caret(text.len())];

    let projection = project_viewport_with(&EditorLines::scanning(&text), &view, 1, 4);
    let line = &projection.lines[0];
    assert_eq!(line.text, "👩‍💻x");
    assert_eq!(line.source.start, 31);
    assert!(
        line.source.start == text.len()
            || text
                .grapheme_indices()
                .any(|(boundary, _)| boundary == line.source.start)
    );
    assert!(
        line.source.end == text.len()
            || text
                .grapheme_indices()
                .any(|(boundary, _)| boundary == line.source.end)
    );

    let text = "abce\u{301}z";
    let (_, _, view, _) = editor(text);
    let projection = project_viewport_with(&EditorLines::scanning(text), &view, 1, 4);
    let line = &projection.lines[0];
    assert_eq!(line.text, "abce\u{301}");
    assert_eq!(line.source, 0.."abce\u{301}".len());
    assert_eq!(
        line.text.graphemes().collect::<Vec<_>>(),
        ["a", "b", "c", "e\u{301}"]
    );
}

#[test]
fn viewport_projection_bounds_pathological_long_line_and_keeps_caret_visible() {
    let text = "α".repeat(600_000);
    let (_, _, mut view, _) = editor(&text);
    let caret = text.len() - "α".len() * 9;
    view.selections = vec![Selection::caret(caret)];

    let projection =
        project_viewport_with(&EditorLines::scanning(&text), &view, usize::MAX, usize::MAX);
    assert_eq!(projection.lines.len(), 1);
    let line = &projection.lines[0];
    assert!(line.text.len() <= 32 * 1024);
    assert!(line.source.start <= caret && caret <= line.source.end);
    assert_eq!(line.carets, vec![(caret - line.source.start, true)]);
    assert!(text.is_char_boundary(line.source.start));
    assert!(text.is_char_boundary(line.source.end));
}

#[test]
fn viewport_projection_drops_an_over_budget_grapheme_instead_of_splitting_it() {
    let text = format!("e{}", "\u{301}".repeat(20_000));
    assert!(text.len() > 32 * 1024);
    assert_eq!(text.graphemes().count(), 1);
    let (_, _, view, _) = editor(&text);

    let projection = project_viewport_with(&EditorLines::scanning(&text), &view, 1, usize::MAX);
    let line = &projection.lines[0];
    assert_eq!(line.text, "");
    assert_eq!(line.source, 0..0);
    assert!(line.text.len() <= 32 * 1024);
}

#[test]
fn viewport_projection_has_a_total_byte_budget_across_many_large_lines() {
    let line = format!("{}\n", "\u{0001}".repeat(40 * 1024));
    let text = line.repeat(40);
    let (_, _, view, _) = editor(&text);
    let projection =
        project_viewport_with(&EditorLines::scanning(&text), &view, usize::MAX, usize::MAX);
    let total = projection
        .lines
        .iter()
        .map(|line| line.text.len())
        .sum::<usize>();
    assert!(total <= 512 * 1024);
    assert!(
        projection.lines.len() < 40,
        "the byte budget truncates rows"
    );
}

#[test]
fn line_index_is_capped_and_the_fallback_answers_identically() {
    let dense = "\n".repeat(MAX_LINE_INDEX_ENTRIES + 8);
    assert!(
        line_starts_capped(&dense).is_none(),
        "a document past the cap must decline to be indexed"
    );

    let ok = "\n".repeat(64);
    let starts = line_starts_capped(&ok).expect("under the cap must index");
    assert_eq!(
        starts,
        uncapped_line_starts(&ok),
        "capped index must equal the plain one"
    );

    let scanning = EditorLines::scanning(&dense);
    let reference = uncapped_line_starts(&dense);
    let indexed = EditorLines::indexed(&dense, &reference);
    assert_eq!(scanning.total(), indexed.total());
    for position in [0, 1, 7, dense.len() / 2, dense.len(), dense.len() + 3] {
        assert_eq!(
            scanning.number_at(position),
            indexed.number_at(position),
            "number_at({position}) must not depend on whether an index exists"
        );
    }
}
