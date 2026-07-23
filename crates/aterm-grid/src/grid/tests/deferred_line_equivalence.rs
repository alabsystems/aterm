// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Equivalence tests for `DeferredLine` lazy scrollback promotion.
//!
//! Verifies that `DeferredLine::to_line()` and `DeferredLine::into_line()`
//! produce identical `Line` output as the eager `row_to_line_with_stored_extras()`
//! for all cell/extras combinations: empty rows, ASCII rows, complex chars,
//! combining marks, hyperlinks, RGB colors, wrapped rows, and mixed content.

use std::sync::Arc;

use super::super::scroll_convert::{DeferredLine, ScrolledRowExtras};
use crate::{Cell, CellFlags, Grid, PackedColor, Row};

/// Compare the text, attribute count, and wrapped state of two Lines.
/// Uses the public accessors of `aterm_scrollback::Line`.
fn assert_lines_equivalent(
    eager: &aterm_scrollback::Line,
    lazy: &aterm_scrollback::Line,
    ctx: &str,
) {
    assert_eq!(eager.to_string(), lazy.to_string(), "{ctx}: text mismatch");
    assert_eq!(
        eager.is_wrapped(),
        lazy.is_wrapped(),
        "{ctx}: wrapped flag mismatch"
    );
    assert_eq!(
        eager.has_hyperlinks(),
        lazy.has_hyperlinks(),
        "{ctx}: has_hyperlinks mismatch"
    );
    assert_eq!(
        eager.hyperlink_count(),
        lazy.hyperlink_count(),
        "{ctx}: hyperlink_count mismatch"
    );
    // Compare per-character attributes
    let text = eager.to_string();
    for (i, _) in text.char_indices() {
        let ea = eager.get_attr(i);
        let la = lazy.get_attr(i);
        assert_eq!(ea.fg, la.fg, "{ctx}: fg mismatch at char index {i}");
        assert_eq!(ea.bg, la.bg, "{ctx}: bg mismatch at char index {i}");
        assert_eq!(
            ea.flags, la.flags,
            "{ctx}: flags mismatch at char index {i}"
        );
    }
}

/// Helper: build a Row with the given cells using a temporary PageStore.
struct RowBuilder {
    pages: crate::PageStore,
}

impl RowBuilder {
    fn new() -> Self {
        Self {
            pages: crate::PageStore::new(),
        }
    }

    /// Create a row from cells. The row length equals the number of cells.
    fn build(&mut self, cells: &[Cell], cols: u16, wrapped: bool) -> Row {
        // SAFETY: Row is used only within this test; pages outlive the row.
        let mut row = unsafe { Row::new(cols, &mut self.pages) };
        for (i, cell) in cells.iter().enumerate() {
            row.set(i as u16, *cell);
        }
        if wrapped {
            row.set_wrapped(true);
        }
        row
    }
}

// =============================================================================
// Equivalence: Empty rows
// =============================================================================

#[test]
fn deferred_line_empty_row_equivalence() {
    let mut rb = RowBuilder::new();
    let row = rb.build(&[], 80, false);
    let extras = ScrolledRowExtras::default();

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras.clone());
    let lazy_ref = deferred.to_line().clone();

    let deferred2 = DeferredLine::new(&row, extras);
    let lazy_owned = deferred2.into_line();

    assert_lines_equivalent(&eager, &lazy_ref, "empty row (to_line)");
    assert_lines_equivalent(&eager, &lazy_owned, "empty row (into_line)");
}

#[test]
fn deferred_line_empty_row_wrapped_equivalence() {
    let mut rb = RowBuilder::new();
    let row = rb.build(&[], 80, true);
    let extras = ScrolledRowExtras::default();

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "empty wrapped row");
}

// =============================================================================
// Equivalence: ASCII rows
// =============================================================================

#[test]
fn deferred_line_ascii_row_equivalence() {
    let cells: Vec<Cell> = b"Hello, World!"
        .iter()
        .map(|&b| Cell::from_ascii_fast(b))
        .collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);
    let extras = ScrolledRowExtras::default();

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);

    let deferred = DeferredLine::new(&row, extras.clone());
    let lazy_ref = deferred.to_line().clone();

    let deferred2 = DeferredLine::new(&row, extras);
    let lazy_owned = deferred2.into_line();

    assert_lines_equivalent(&eager, &lazy_ref, "ASCII row (to_line)");
    assert_lines_equivalent(&eager, &lazy_owned, "ASCII row (into_line)");
    assert_eq!(eager.to_string(), "Hello, World!");
}

#[test]
fn deferred_line_styled_ascii_row_equivalence() {
    let fg = PackedColor::indexed(1); // Red
    let bg = PackedColor::indexed(4); // Blue
    let flags = CellFlags::BOLD | CellFlags::UNDERLINE;
    let cells: Vec<Cell> = b"Styled"
        .iter()
        .map(|&b| Cell::with_style(b as char, fg, bg, flags))
        .collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);
    let extras = ScrolledRowExtras::default();

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "styled ASCII row");
    assert_eq!(eager.to_string(), "Styled");
}

// =============================================================================
// Equivalence: Wrapped rows
// =============================================================================

#[test]
fn deferred_line_wrapped_row_equivalence() {
    let cells: Vec<Cell> = b"wrapped content"
        .iter()
        .map(|&b| Cell::from_ascii_fast(b))
        .collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, true);
    let extras = ScrolledRowExtras::default();

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "wrapped row");
    assert!(lazy.is_wrapped(), "lazy line should be wrapped");
}

// =============================================================================
// Equivalence: Complex characters (non-BMP / overflow)
// =============================================================================

#[test]
fn deferred_line_complex_chars_equivalence() {
    // Build cells where some have complex chars (stored in extras).
    let mut cells = vec![Cell::from_ascii_fast(b'A'); 5];
    // Cell at col 2 has a complex character (emoji stored via overflow)
    cells[2].set_overflow_index(0);

    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);

    let mut extras = ScrolledRowExtras::default();
    // The complex char at col 2 is a multi-codepoint emoji
    extras.complex_chars.push((2, Arc::from("\u{1F600}"))); // grinning face

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "complex chars row");
}

// =============================================================================
// Equivalence: Combining marks
// =============================================================================

#[test]
fn deferred_line_combining_marks_equivalence() {
    let cells: Vec<Cell> = b"cafe".iter().map(|&b| Cell::from_ascii_fast(b)).collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);

    let mut extras = ScrolledRowExtras::default();
    // Add combining acute accent on 'e' (col 3): e + U+0301 = e-acute
    use aterm_alloc::SmallVec;
    extras
        .combining
        .push((3, SmallVec::from_elem('\u{0301}', 1)));

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "combining marks row");
    // Verify the combining mark is present in the text
    assert!(
        eager.to_string().contains('\u{0301}'),
        "eager line should contain combining mark"
    );
}

// =============================================================================
// Equivalence: Hyperlinks
// =============================================================================

#[test]
fn deferred_line_hyperlinks_equivalence() {
    let cells: Vec<Cell> = b"click here"
        .iter()
        .map(|&b| Cell::from_ascii_fast(b))
        .collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);

    let url: Arc<str> = Arc::from("https://example.com");
    let mut extras = ScrolledRowExtras::default();
    use aterm_scrollback::HyperlinkSpan;
    extras.hyperlinks.push(HyperlinkSpan {
        start_col: 0,
        end_col: 4, // "click"
        url: url.clone(),
        id: None,
    });

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "hyperlinks row");
    assert!(lazy.has_hyperlinks(), "lazy line should have hyperlinks");
    assert_eq!(
        lazy.hyperlink_count(),
        1,
        "lazy line should have 1 hyperlink span"
    );
}

#[test]
fn deferred_line_multiple_hyperlinks_equivalence() {
    let cells: Vec<Cell> = b"foo bar baz"
        .iter()
        .map(|&b| Cell::from_ascii_fast(b))
        .collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);

    let url1: Arc<str> = Arc::from("https://foo.com");
    let url2: Arc<str> = Arc::from("https://bar.com");
    let mut extras = ScrolledRowExtras::default();
    use aterm_scrollback::HyperlinkSpan;
    extras.hyperlinks.push(HyperlinkSpan {
        start_col: 0,
        end_col: 2,
        url: url1,
        id: None,
    });
    extras.hyperlinks.push(HyperlinkSpan {
        start_col: 4,
        end_col: 6,
        url: url2,
        id: None,
    });

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "multiple hyperlinks row");
    assert_eq!(lazy.hyperlink_count(), 2);
}

// =============================================================================
// Equivalence: RGB colors
// =============================================================================

#[test]
fn deferred_line_rgb_colors_equivalence() {
    // Create cells with RGB overflow marker
    let fg = PackedColor::rgb(255, 128, 0); // Orange
    let bg = PackedColor::rgb(0, 64, 128); // Dark teal
    let cells: Vec<Cell> = b"RGB!"
        .iter()
        .map(|&b| Cell::with_style(b as char, fg, bg, CellFlags::empty()))
        .collect();
    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);

    let mut extras = ScrolledRowExtras::default();
    // Store resolved RGB for each cell with overflow
    for col in 0..4u16 {
        extras.rgb_fg.push((col, [255, 128, 0]));
        extras.rgb_bg.push((col, [0, 64, 128]));
    }

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "RGB colors row");
}

// =============================================================================
// Equivalence: Mixed content (the stress test)
// =============================================================================

#[test]
fn deferred_line_mixed_content_equivalence() {
    // Build a row with plain ASCII, styled, complex char, and RGB overflow cells.
    let mut cells = Vec::new();

    // Cols 0-2: plain ASCII "abc"
    for &b in b"abc" {
        cells.push(Cell::from_ascii_fast(b));
    }
    // Col 3: bold cell
    cells.push(Cell::with_style(
        'd',
        PackedColor::indexed(2),
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
    ));
    // Col 4: complex char (overflow index 0)
    let mut complex_cell = Cell::new('\u{FFFD}');
    complex_cell.set_overflow_index(0);
    cells.push(complex_cell);
    // Col 5-6: more ASCII
    for &b in b"ef" {
        cells.push(Cell::from_ascii_fast(b));
    }

    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, true); // wrapped

    let mut extras = ScrolledRowExtras::default();
    extras.complex_chars.push((4, Arc::from("\u{1F680}"))); // rocket emoji
    // Adding combining mark on col 1 ('b')
    use aterm_alloc::SmallVec;
    extras
        .combining
        .push((1, SmallVec::from_elem('\u{0308}', 1))); // diaeresis
    // Hyperlink spanning cols 5-6
    use aterm_scrollback::HyperlinkSpan;
    extras.hyperlinks.push(HyperlinkSpan {
        start_col: 5,
        end_col: 6,
        url: Arc::from("https://test.com"),
        id: None,
    });

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);

    let deferred1 = DeferredLine::new(&row, extras.clone());
    let lazy_ref = deferred1.to_line().clone();

    let deferred2 = DeferredLine::new(&row, extras);
    let lazy_owned = deferred2.into_line();

    assert_lines_equivalent(&eager, &lazy_ref, "mixed content (to_line)");
    assert_lines_equivalent(&eager, &lazy_owned, "mixed content (into_line)");
    assert!(lazy_owned.is_wrapped(), "mixed content should be wrapped");
    assert!(
        lazy_owned.has_hyperlinks(),
        "mixed content should have hyperlinks"
    );
}

// =============================================================================
// Equivalence: Full-width / wide character rows
// =============================================================================

#[test]
fn deferred_line_wide_chars_equivalence() {
    // Wide chars occupy 2 cells: primary + continuation
    let mut cells = Vec::new();
    // Col 0: ASCII 'A'
    cells.push(Cell::from_ascii_fast(b'A'));
    // Col 1-2: Wide CJK char (U+4E16 = world)
    let wide_cell = Cell::with_style(
        '\u{4E16}',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::WIDE,
    );
    cells.push(wide_cell);
    let cont_cell = Cell::with_style(
        ' ',
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::WIDE_CONTINUATION,
    );
    cells.push(cont_cell);
    // Col 3: ASCII 'B'
    cells.push(Cell::from_ascii_fast(b'B'));

    let mut rb = RowBuilder::new();
    let row = rb.build(&cells, 80, false);
    let extras = ScrolledRowExtras::default();

    let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
    let deferred = DeferredLine::new(&row, extras);
    let lazy = deferred.into_line();

    assert_lines_equivalent(&eager, &lazy, "wide chars row");
}

// =============================================================================
// Lazy buffer integration: drain produces equivalent lines
// =============================================================================

#[test]
fn lazy_buffer_drain_produces_equivalent_lines() {
    use crate::grid::scroll_convert::LazyBuffer;

    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();

    // Push several rows with different content
    let row1 = rb.build(
        &b"Line one"
            .iter()
            .map(|&b| Cell::from_ascii_fast(b))
            .collect::<Vec<_>>(),
        80,
        false,
    );
    let extras1 = ScrolledRowExtras::default();
    let eager1 = Grid::row_to_line_with_stored_extras(&row1, &extras1);
    buf.push(DeferredLine::new(&row1, extras1));

    let row2 = rb.build(
        &b"Line two"
            .iter()
            .map(|&b| Cell::from_ascii_fast(b))
            .collect::<Vec<_>>(),
        80,
        true,
    );
    let extras2 = ScrolledRowExtras::default();
    let eager2 = Grid::row_to_line_with_stored_extras(&row2, &extras2);
    buf.push(DeferredLine::new(&row2, extras2));

    assert_eq!(buf.len(), 2);

    let drained: Vec<_> = buf.drain_all().collect();
    assert_eq!(drained.len(), 2);
    assert!(buf.is_empty());

    assert_lines_equivalent(&eager1, &drained[0], "lazy buffer drain line 1");
    assert_lines_equivalent(&eager2, &drained[1], "lazy buffer drain line 2");
}
