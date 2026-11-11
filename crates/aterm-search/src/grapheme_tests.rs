// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#[test]
fn one_shot_stored_projection_matches_maps_for_every_endpoint_pair() {
    let mut texts: Vec<String> = [
        "",
        "abcde",
        "éx日a",
        "🇦🇧XYZ",
        "a\u{301}b",
        "\t\ra\n",
        "👩🏽‍💻→aaaaa",
        "\u{301}a→İ",
        "\u{600}a\u{301}bc",
        "\u{600}ab\u{301}cd",
        "🇦🇧a\u{600}bc🇨🇩",
        "न्दीab👩🏽‍💻cd\u{301}",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for prefix in ["a", "abcdefgh"] {
        for shape in SHAPES {
            texts.push(format!("{prefix}{shape}tail"));
        }
    }
    for text in &texts {
        let text = text.as_str();
        for shape in 0..9 {
            let mut cells = LineCells::new();
            if shape == 6 {
                cells.push_repeated(0..1, 0, 1, text.len());
            } else if shape == 7 {
                cells.mark_truncated();
            } else if shape == 8 {
                cells.push(text.len()..text.len() + 2, 99, 2);
            } else if shape == 1 {
                cells.push(0..text.len(), 7, 3);
            } else {
                let mut col = if shape == 5 { u32::MAX as usize - 2 } else { 4 };
                for (i, (byte, ch)) in text.char_indices().enumerate() {
                    if shape == 4 && i % 2 == 1 {
                        continue;
                    }
                    let width = (i + shape) % 3;
                    cells.push(byte..byte + ch.len_utf8(), col, width);
                    col = col.saturating_add(width + usize::from(shape == 3));
                }
            }
            let map = ColumnMap::with_cells(text, &cells);
            for start in 0..=text.len() + 2 {
                for end in 0..=text.len() + 2 {
                    assert_eq!(
                        cells.columns_of(text, start, end),
                        map.span(start, end),
                        "{text:?} shape={shape} {start}..{end}"
                    );
                }
            }
        }
    }
}

#[test]
fn local_ascii_boundaries_and_resets_match_full_unicode_segmentation() {
    use aterm_grapheme::GraphemeClusters;

    for context in ["", "\u{600}", "\u{301}", "\r\n", "🇦", "🇦🇧", "👩🏽‍💻", "न्दी"]
    {
        for left in b' '..=b'~' {
            for right in b' '..=b'~' {
                let text = format!(
                    "{context}{}{}\u{301}🇦🇧न्दी",
                    char::from(left),
                    char::from(right)
                );
                let at = context.len() + 1;
                assert!(super::local_ascii_boundary(text.as_bytes(), at));
                let full: Vec<_> = text
                    .grapheme_indices()
                    .filter(|(byte, _)| *byte >= at)
                    .collect();
                let suffix: Vec<_> = text[at..]
                    .grapheme_indices()
                    .map(|(byte, g)| (at + byte, g))
                    .collect();
                assert_eq!(full, suffix, "{text:?}");
            }
        }
    }
    let text = "\u{600}a";
    assert!(!super::local_ascii_boundary(
        text.as_bytes(),
        "\u{600}".len()
    ));
    assert_eq!(text.grapheme_indices().count(), 1);
}

#[test]
fn word_printable_classification_matches_every_byte_and_lane_boundary() {
    let reference = |bytes: &[u8]| bytes.iter().all(|byte| matches!(byte, b' '..=b'~'));
    for length in 0..=40 {
        let mut text = vec![b'a'; length];
        assert!(super::printable_ascii(&text));
        for at in 0..length {
            for byte in 0..=u8::MAX {
                text[at] = byte;
                assert_eq!(
                    super::printable_ascii(&text),
                    reference(&text),
                    "{length} {at} {byte}"
                );
            }
            text[at] = b'a';
        }
    }
    for at in [0, 6, 7, 8, 14, 15] {
        let mut text = [b'a'; 24];
        for first in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                text[at] = first;
                text[at + 1] = second;
                assert_eq!(super::printable_ascii(&text), reference(&text));
            }
        }
    }
}

#[test]
fn refreshed_cell_maps_equal_fresh_maps_across_shape_changes() {
    let mut map = ColumnMap::new("initial", AmbiguousWidth::Single);
    for text in ["", "abc", "日a\u{301}👩🏽‍💻", "plain ASCII", "日日日", ""] {
        for shape in 0..4 {
            let mut cells = crate::LineCells::new();
            let mut col = 0;
            for (i, (byte, ch)) in text.char_indices().enumerate() {
                let width = if shape == 0 { 1 } else { (i + shape) % 3 };
                cells.push(byte..byte + ch.len_utf8(), col, width);
                col += width + usize::from(shape == 3);
            }
            map.refresh_cells(text, &cells);
            assert_eq!(
                map,
                super::ColumnMap::with_cells(text, &cells),
                "text={text:?} shape={shape}"
            );
        }
    }
}

use aterm_types::text_shaping::AmbiguousWidth;

use aterm_grapheme::{GridColumns, GridStep, grid_graphemes_with};

use super::{
    ColumnMap, LowerByteCursor, LowerByteCursors, byte_to_column, display_columns,
    map_lower_byte_to_original,
};
use crate::cells::LineCells;

/// The per-grapheme map the runs encode (one `(offset, start, before)` entry per
/// grapheme plus a sentinel, binary-searched): the oracle the encoding must match.
struct ReferenceMap(Vec<(usize, usize, usize)>);

impl ReferenceMap {
    fn new(text: &str, ambiguous: AmbiguousWidth) -> Self {
        let mut entries = Vec::new();
        let mut before = 0;
        for g in grid_graphemes_with(text, ambiguous) {
            entries.push((g.byte_offset, g.start_col, before));
            before = g.end_col;
        }
        entries.push((text.len(), before, before));
        Self(entries)
    }

    fn look(&self, offset: usize, pick: fn(&(usize, usize, usize)) -> usize) -> usize {
        let total = self.0.last().map_or(0, |e| e.2);
        match self.0.binary_search_by_key(&offset, |e| e.0) {
            Ok(i) => pick(&self.0[i]),
            Err(i) if i < self.0.len() => pick(&self.0[i]),
            Err(_) => total,
        }
    }

    fn span(&self, start: usize, end: usize) -> (usize, usize) {
        (self.look(start, |e| e.1), self.look(end, |e| e.2))
    }
}

/// Every piece of a line that changes how its graphemes advance: ASCII,
/// controls, wide, combining, folding, presentation selectors, flags,
/// conjuncts, ambiguous-width symbols.
const SHAPES: &[&str] = &[
    "",
    "ab",
    "\t",
    "日本",
    "e\u{301}",
    "\u{301}",
    "न्दी",
    "\u{d9a}\u{dca}\u{200d}\u{dba}",
    "1\u{fe0f}\u{20e3}",
    "👩🏽‍💻",
    "\u{1f1e6}\u{1f1e7}",
    "\u{1f1e6}",
    "⌚\u{fe0e}",
    "♥\u{fe0f}",
    "→─°",
    "\u{23bf}",
    "\u{200b}",
];

/// The run-length encoded map answers every `(start, end)` byte pair as the
/// per-grapheme reference does, under both `ambiguous_width` policies: on every
/// pair and triple of shapes, and on long lines where a shape repeats then changes.
#[test]
fn the_runs_answer_as_the_per_grapheme_map_did() {
    let mut texts: Vec<String> = Vec::new();
    for a in SHAPES {
        for b in SHAPES {
            texts.push(format!("{a}{b}"));
            for c in SHAPES {
                texts.push(format!("{a}{b}{c}"));
            }
        }
    }
    for shape in SHAPES {
        texts.push(format!("{}x{}", shape.repeat(9), shape.repeat(3)));
    }
    for ambiguous in [AmbiguousWidth::Single, AmbiguousWidth::Double] {
        for text in &texts {
            let map = ColumnMap::new(text, ambiguous);
            let reference = ReferenceMap::new(text, ambiguous);
            for start in 0..=text.len() + 1 {
                for end in start..=text.len() + 1 {
                    assert_eq!(
                        map.span(start, end),
                        reference.span(start, end),
                        "{text:?} {ambiguous:?} bytes {start}..{end}"
                    );
                }
            }
        }
    }
}

/// The cells a grid writing `text` left to right in one go stores it in,
/// as [`GridColumns`] lays them out: a printing character opens a cell,
/// a folded or zero-width one joins the cell before it (VS16 widening
/// it), and one with no cell before it is stored nowhere.
fn written_cells(text: &str, ambiguous: AmbiguousWidth) -> LineCells {
    let mut grid = GridColumns::with_ambiguous_width(ambiguous);
    let mut cells = LineCells::new();
    let mut open: Option<(usize, usize, usize, usize)> = None;
    for (byte, c) in text.char_indices() {
        let end = byte + c.len_utf8();
        match grid.step(c) {
            GridStep::Opens { col, width } => {
                if let Some((start, stop, col, width)) = open.take() {
                    cells.push(start..stop, col, width);
                }
                open = Some((byte, end, col, width));
            }
            GridStep::Folds { .. } | GridStep::Joins { .. } => {
                if let Some(cell) = open.as_mut() {
                    cell.1 = end;
                    cell.3 = grid.column() - cell.2;
                }
            }
        }
    }
    if let Some((start, stop, col, width)) = open {
        cells.push(start..stop, col, width);
    }
    cells
}

/// Read from cells laid out as a line written left to right in one go,
/// the map answers every byte pair as the walk of the text does: the
/// cells only change an answer where the grid did something else.
#[test]
fn cells_of_a_line_written_in_one_go_answer_as_the_walk() {
    let shapes: Vec<&str> = SHAPES
        .iter()
        .copied()
        .filter(|shape| !shape.contains('\u{fe0e}'))
        .collect();
    for ambiguous in [AmbiguousWidth::Single, AmbiguousWidth::Double] {
        for a in &shapes {
            for b in &shapes {
                for c in &shapes {
                    let text = format!("{a}{b}{c}");
                    let walked = ColumnMap::new(&text, ambiguous);
                    let celled = ColumnMap::with_cells(&text, &written_cells(&text, ambiguous));
                    assert_eq!(walked.is_ascii(), text.is_ascii());
                    assert_eq!(celled.is_ascii(), text.is_ascii());
                    for start in 0..=text.len() + 1 {
                        for end in start..=text.len() + 1 {
                            assert_eq!(
                                celled.span(start, end),
                                walked.span(start, end),
                                "{text:?} {ambiguous:?} bytes {start}..{end}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Where the grid kept apart what a walk of the text folds, the cells
/// decide: two flag halves in cells 0-1 and 2-3 put `XYZ` in cells 4-6
/// (the walk pairs them into one flag and says 2-4); a skin tone in a
/// cell of its own after its base does the same; a base in a row's last
/// column left narrow by its VS16 keeps the next cell where the grid has
/// it.
#[test]
fn cells_the_grid_kept_apart_are_where_matches_are_drawn() {
    let flag_halves = "\u{1f1e6}\u{1f1e7}XYZ";
    let mut cells = LineCells::new();
    cells.push(0..4, 0, 2);
    cells.push(4..8, 2, 2);
    for i in 0..3 {
        cells.push(8 + i..9 + i, 4 + i, 1);
    }
    let map = ColumnMap::with_cells(flag_halves, &cells);
    assert_eq!(map.span(8, 11), (4, 7));
    assert_eq!(
        map.span(0, 4),
        (0, 4),
        "the pair is one grapheme, over both cells"
    );
    assert_eq!(
        ColumnMap::new(flag_halves, AmbiguousWidth::Single).span(8, 11),
        (2, 5),
        "precondition: the walk pairs the halves"
    );

    let base_last = "a\u{2714}\u{fe0f}b";
    let mut cells = LineCells::new();
    cells.push(0..1, 0, 1);
    cells.push(1..7, 1, 1);
    cells.push(7..8, 2, 1);
    let map = ColumnMap::with_cells(base_last, &cells);
    assert_eq!(map.span(7, 8), (2, 3));
    assert_eq!(map.span(0, 8), (0, 3), "the line is three cells");
}

/// A grapheme inside a cell is drawn in it when it prints (the grid
/// folded it there) and in no column when it is zero-width, and bytes
/// no cell holds are drawn in none: the rules that keep a lone mark, a
/// ZWSP or a prepended digit out of the results.
#[test]
fn graphemes_inside_a_cell_and_bytes_outside_every_cell() {
    let yansaya = "x \u{d9a}\u{dca}\u{200d}\u{dba} y";
    let mut cells = LineCells::new();
    cells.push(0..1, 0, 1);
    cells.push(1..2, 1, 1);
    cells.push(2..14, 2, 1);
    cells.push(14..15, 3, 1);
    cells.push(15..16, 4, 1);
    let map = ColumnMap::with_cells(yansaya, &cells);
    assert_eq!(map.span(11, 14), (2, 3), "the folded letter is in its cell");

    let zwsp = "a\u{200b}b";
    let mut cells = LineCells::new();
    cells.push(0..4, 0, 1);
    cells.push(4..5, 1, 1);
    let map = ColumnMap::with_cells(zwsp, &cells);
    assert_eq!(map.span(1, 4), (1, 1), "a ZWSP is drawn in no column");

    let orphans = "\u{301}ab\u{301}";
    let mut cells = LineCells::new();
    cells.push(2..3, 0, 1);
    cells.push(3..4, 1, 1);
    let map = ColumnMap::with_cells(orphans, &cells);
    assert_eq!(map.span(0, 2), (0, 0));
    assert_eq!(map.span(2, 4), (0, 2));
    assert_eq!(map.span(4, 6), (2, 2));
}

/// A line of one shape is one run however long it is, and runs are sized exactly:
/// a 100-ideograph row, 100 decomposed accents, or an ASCII row with one
/// box-drawing glyph holds a few dozen bytes.
#[test]
fn a_line_of_one_shape_is_one_run() {
    let one = |text: &str| ColumnMap::new(text, AmbiguousWidth::Single).heap_bytes();
    let run = one("日日");
    assert!(run > 0);
    assert_eq!(one(&"日".repeat(100)), run, "CJK");
    assert_eq!(one(&"e\u{301}".repeat(100)), run, "decomposed accents");
    assert_eq!(
        one(&format!("  \u{23bf}  {}", "x".repeat(110))),
        2 * run,
        "ASCII ending in the glyph, then ASCII"
    );
    assert_eq!(
        one(&"plain ascii ".repeat(10)),
        0,
        "the identity holds nothing"
    );
}

#[test]
fn width_only_matches_coordinate_map_for_unicode_and_controls() {
    let pieces = [
        "",
        "plain ASCII ~",
        "\0\t\r\n\u{7f}",
        "日本語",
        "e\u{0301}",
        "\u{0301}\u{0308}",
        "\u{fe0f}",
        "न्दी",
        "1\u{fe0f}\u{20e3}",
        "👩🏽‍💻",
        "🇦",
        "🇧",
        "♥\u{fe0e}",
        "♥\u{fe0f}",
    ];
    for left in pieces {
        for right in pieces {
            let text = format!("{left}{right}");
            assert_eq!(
                display_columns(&text),
                ColumnMap::new(&text, AmbiguousWidth::Single).end_column(text.len()),
                "text={text:?}"
            );
        }
    }
    assert_eq!(display_columns("\0\t\r\n\u{7f}"), 0);
    assert_eq!(display_columns("日"), 2);
    assert_eq!(display_columns("न्दी"), 3);
    assert_eq!(display_columns("1\u{fe0f}\u{20e3}"), 2);
    assert_eq!(display_columns("👩🏽‍💻"), 2);
}

/// `ColumnMap::end_column` answers every byte offset as the O(G) scan
/// `byte_to_column` does: ASCII, wide CJK, combining marks, emoji,
/// controls, and a long CJK line (the O(log G) lookup path).
#[test]
fn column_map_matches_byte_to_column() {
    let long: String = "日本語テスト漢字表示".chars().cycle().take(80).collect();
    let texts = [
        "",
        "hello world, this is a test line!",
        "ab日本語cd",
        "abc\u{0301}def",
        "\u{1F600}hello\u{1F600}",
        "日a本b語c",
        "a\u{0308}\u{0301}b",
        "\t\n\r abc",
        long.as_str(),
    ];
    for text in texts {
        let map = ColumnMap::new(text, AmbiguousWidth::Single);
        for offset in 0..=text.len() {
            assert_eq!(
                map.end_column(offset),
                byte_to_column(text, offset),
                "{text:?} at offset {offset}"
            );
        }
    }
}

/// The cursors map every lowered byte offset back as the independent O(C) scan
/// `map_lower_byte_to_original` does, including the Kelvin sign (3 bytes
/// lowered to 1), Greek, CJK, the empty line and a long mixed line.
#[test]
fn lower_byte_cursors_match_the_scalar_oracle_in_both_orders() {
    let long: String = "Hello日本語World".chars().cycle().take(100).collect();
    let texts = [
        "Hello World Test",
        "a\u{212A}b",
        "ABCdef",
        "Straße",
        "ΔΕΛΤΑ",
        "日本語ABC",
        "",
        "İx\u{301}ΣςK👩🏽‍💻",
        long.as_str(),
    ];
    for text in texts {
        let len = super::lower_fold(text).len();
        let mut forward = LowerByteCursor::<false>::new(text, len);
        let mut backward = LowerByteCursor::<true>::new(text, len);
        for offset in 0..=len + 2 {
            assert_eq!(
                forward.map_to_original(offset),
                map_lower_byte_to_original(text, offset),
                "{text:?} at lower offset {offset}"
            );
        }
        for offset in (0..=len + 2).rev() {
            assert_eq!(
                backward.map_to_original(offset),
                map_lower_byte_to_original(text, offset)
            );
        }
    }
}

#[test]
fn paired_cursors_keep_overlaps_gaps_and_expansions_in_both_orders() {
    for text in ["", "abc", "İaKΣςe\u{301}日本👩🏽‍💻", "long gap before a match"] {
        let len = super::lower_fold(text).len();
        for width in 0..=len + 2 {
            for step in [1, 3, 17] {
                let mut forward = LowerByteCursors::<false>::new(text, len);
                let mut backward = LowerByteCursors::<true>::new(text, len);
                let starts: Vec<_> = (0..=len + 2).step_by(step).collect();
                for &start in &starts {
                    let end = start + width;
                    assert_eq!(
                        forward.span(start, end),
                        (
                            map_lower_byte_to_original(text, start),
                            map_lower_byte_to_original(text, end)
                        ),
                        "forward {text:?} {start}..{end}"
                    );
                }
                for &start in starts.iter().rev() {
                    let end = start + width;
                    assert_eq!(
                        backward.span(start, end),
                        (
                            map_lower_byte_to_original(text, start),
                            map_lower_byte_to_original(text, end)
                        ),
                        "backward {text:?} {start}..{end}"
                    );
                }
            }
        }
    }
}
