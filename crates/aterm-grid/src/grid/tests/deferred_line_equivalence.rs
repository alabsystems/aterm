// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Equivalence tests for `DeferredLine` lazy scrollback promotion.

use std::sync::Arc;

use super::super::scroll_convert::{DeferredLine, ScrolledRowExtras};
use crate::{Cell, CellFlags, Grid, PackedColor, Row};

/// Compare the text, attributes, hyperlinks, and wrapped state of two `Line`s.
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

/// `DeferredLine::to_line` and `into_line` match `row_to_line_with_stored_extras` across row shapes and extras.
#[test]
fn deferred_line_matches_the_eager_conversion() {
    use aterm_alloc::SmallVec;
    use aterm_scrollback::HyperlinkSpan;

    let ascii =
        |text: &[u8]| -> Vec<Cell> { text.iter().map(|&b| Cell::from_ascii_fast(b)).collect() };
    let styled = |text: &[u8], fg, bg, flags| -> Vec<Cell> {
        text.iter()
            .map(|&b| Cell::with_style(char::from(b), fg, bg, flags))
            .collect()
    };
    let link = |start_col, end_col, url: &str| HyperlinkSpan {
        start_col,
        end_col,
        url: Arc::from(url),
        id: None,
    };
    let complex = |cells: &mut Vec<Cell>, col: usize| cells[col].set_overflow_index(0);
    let mut cases: Vec<(&str, Vec<Cell>, ScrolledRowExtras, bool)> = vec![
        ("empty", Vec::new(), ScrolledRowExtras::default(), false),
        (
            "empty wrapped",
            Vec::new(),
            ScrolledRowExtras::default(),
            true,
        ),
        (
            "ascii",
            ascii(b"Hello, World!"),
            ScrolledRowExtras::default(),
            false,
        ),
        (
            "ascii wrapped",
            ascii(b"wrapped content"),
            ScrolledRowExtras::default(),
            true,
        ),
        (
            "styled",
            styled(
                b"Styled",
                PackedColor::indexed(1),
                PackedColor::indexed(4),
                CellFlags::BOLD | CellFlags::UNDERLINE,
            ),
            ScrolledRowExtras::default(),
            false,
        ),
    ];

    let mut cells = ascii(b"AAAAA");
    complex(&mut cells, 2);
    let mut extras = ScrolledRowExtras::default();
    extras.complex_chars.push((2, Arc::from("\u{1F600}")));
    cases.push(("complex char", cells, extras, false));

    let mut extras = ScrolledRowExtras::default();
    extras
        .combining
        .push((3, SmallVec::from_elem('\u{0301}', 1)));
    cases.push(("combining mark", ascii(b"cafe"), extras, false));

    let mut extras = ScrolledRowExtras::default();
    extras.hyperlinks.push(link(0, 2, "https://foo.com"));
    extras.hyperlinks.push(link(4, 6, "https://bar.com"));
    cases.push(("hyperlinks", ascii(b"foo bar baz"), extras, false));

    let mut extras = ScrolledRowExtras::default();
    for col in 0..4u16 {
        extras.rgb_fg.push((col, [255, 128, 0]));
        extras.rgb_bg.push((col, [0, 64, 128]));
    }
    let rgb = styled(
        b"RGB!",
        PackedColor::rgb(255, 128, 0),
        PackedColor::rgb(0, 64, 128),
        CellFlags::empty(),
    );
    cases.push(("truecolor", rgb, extras, false));

    let mut cells = ascii(b"abc");
    cells.extend(styled(
        b"d",
        PackedColor::indexed(2),
        PackedColor::DEFAULT_BG,
        CellFlags::BOLD,
    ));
    cells.push(Cell::new('\u{FFFD}'));
    complex(&mut cells, 4);
    cells.extend(ascii(b"ef"));
    let mut extras = ScrolledRowExtras::default();
    extras.complex_chars.push((4, Arc::from("\u{1F680}")));
    extras
        .combining
        .push((1, SmallVec::from_elem('\u{0308}', 1)));
    extras.hyperlinks.push(link(5, 6, "https://test.com"));
    cases.push(("mixed", cells, extras, true));

    let mut cells = ascii(b"A");
    cells.extend(styled(
        b" ",
        PackedColor::DEFAULT_FG,
        PackedColor::DEFAULT_BG,
        CellFlags::WIDE_CONTINUATION,
    ));
    cells.insert(
        1,
        Cell::with_style(
            '\u{4E16}',
            PackedColor::DEFAULT_FG,
            PackedColor::DEFAULT_BG,
            CellFlags::WIDE,
        ),
    );
    cells.extend(ascii(b"B"));
    cases.push(("wide", cells, ScrolledRowExtras::default(), false));

    let mut rb = RowBuilder::new();
    for (name, cells, extras, wrapped) in cases {
        let row = rb.build(&cells, 80, wrapped);
        let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
        let by_ref = DeferredLine::new(&row, extras.clone(), Vec::new())
            .to_line()
            .clone();
        let owned = DeferredLine::new(&row, extras, Vec::new()).into_line();
        assert_lines_equivalent(&eager, &by_ref, &format!("{name} (to_line)"));
        assert_lines_equivalent(&eager, &owned, &format!("{name} (into_line)"));
        assert_eq!(owned.is_wrapped(), wrapped, "{name}");
    }
}

#[test]
fn lazy_buffer_drain_produces_equivalent_lines() {
    use crate::grid::scroll_convert::LazyBuffer;

    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();

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
    buf.push(DeferredLine::new(&row1, extras1, Vec::new()));

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
    buf.push(DeferredLine::new(&row2, extras2, Vec::new()));

    assert_eq!(buf.len(), 2);

    let drained: Vec<_> = buf.drain_all().collect();
    assert_eq!(drained.len(), 2);
    assert!(buf.is_empty());

    assert_lines_equivalent(&eager1, &drained[0], "lazy buffer drain line 1");
    assert_lines_equivalent(&eager2, &drained[1], "lazy buffer drain line 2");
}

#[test]
fn recycled_cell_body_carries_no_state_from_the_previous_line() {
    use crate::grid::scroll_convert::LazyBuffer;

    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();

    let long_cells: Vec<Cell> = (0..60u8)
        .map(|i| Cell::from_ascii_fast(b'a' + i % 26))
        .collect();
    let long_row = rb.build(&long_cells, 80, false);
    buf.push_row(&long_row, ScrolledRowExtras::default());
    assert_eq!(buf.drain_all().count(), 1);
    assert_eq!(
        buf.pooled_bodies(),
        1,
        "a materialized line must hand its cell body back"
    );

    let fg = PackedColor::rgb(255, 128, 0);
    let bg = PackedColor::rgb(0, 64, 128);
    let short_cells: Vec<Cell> = b"hi"
        .iter()
        .map(|&b| Cell::with_style(b as char, fg, bg, CellFlags::BOLD))
        .collect();
    let short_row = rb.build(&short_cells, 80, true);
    let mut extras = ScrolledRowExtras::default();
    for col in 0..2u16 {
        extras.rgb_fg.push((col, [255, 128, 0]));
        extras.rgb_bg.push((col, [0, 64, 128]));
    }

    let eager = Grid::row_to_line_with_stored_extras(&short_row, &extras);
    let fresh_body = DeferredLine::new(&short_row, extras.clone(), Vec::new()).into_line();

    buf.push_row(&short_row, extras);
    assert_eq!(
        buf.pooled_bodies(),
        0,
        "the pooled body must actually be reused, not left idle"
    );
    let drained: Vec<_> = buf.drain_all().collect();

    assert_lines_equivalent(&eager, &drained[0], "line built on a recycled body");
    assert_eq!(
        fresh_body.to_string(),
        drained[0].to_string(),
        "recycled body must produce the same text as a fresh Vec"
    );
}

#[test]
fn flood_backpressure_drop_recycles_bodies() {
    use crate::grid::scroll_convert::LazyBuffer;

    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();
    let cells: Vec<Cell> = b"dropped under flood"
        .iter()
        .map(|&b| Cell::from_ascii_fast(b))
        .collect();
    let row = rb.build(&cells, 80, false);

    for _ in 0..5 {
        buf.push_row(&row, ScrolledRowExtras::default());
    }
    assert_eq!(
        buf.drop_oldest(3),
        3,
        "every dropped line is counted into the cut"
    );
    assert_eq!(buf.len(), 3, "drop_oldest still drops the staged lines");
    assert_eq!(buf.cut_dropped(), 3, "the cut remembers what it lost");
    assert_eq!(
        buf.staged_bytes(),
        2 * (std::mem::size_of::<crate::grid::scroll_convert::DeferredLine>()
            + b"dropped under flood".len() * std::mem::size_of::<Cell>()),
        "two staged rows remain, each weighed at its OCCUPIED cells, not the width"
    );
    assert_eq!(
        buf.pooled_bodies(),
        3,
        "dropped lines' bodies must be recycled, not freed"
    );
}

#[test]
fn front_rows_over_sheds_the_fewest_rows_that_fit_and_keeps_the_floor() {
    use crate::grid::scroll_convert::LazyBuffer;

    let mut rb = RowBuilder::new();
    let fill = |buf: &mut LazyBuffer, rb: &mut RowBuilder| {
        for i in 0..30 {
            let cells: Vec<Cell> = std::iter::repeat_n(b'x', [2, 40, 200][i % 3])
                .map(Cell::from_ascii_fast)
                .collect();
            buf.push_row(&rb.build(&cells, 200, false), ScrolledRowExtras::default());
        }
    };
    let mut buf = LazyBuffer::new();
    fill(&mut buf, &mut rb);
    let total = buf.staged_bytes();
    assert_eq!(buf.front_rows_over(total, 0), 0, "it fits: nothing to shed");

    let budget = total * 2 / 3;
    let n = buf.front_rows_over(budget, 0);
    assert!(n > 0);
    let mut probe = LazyBuffer::new();
    fill(&mut probe, &mut rb);
    probe.shed_oldest(n - 1, 0);
    assert!(probe.staged_bytes() > budget, "one row fewer would not fit");
    probe.shed_oldest(1, 0);
    assert!(probe.staged_bytes() <= budget, "`n` rows do");

    assert_eq!(buf.front_rows_over(0, 25), 5);
    assert_eq!(buf.front_rows_over(0, 40), 0);
    assert_eq!(buf.drop_oldest(2), 2);
    assert_eq!(
        buf.front_rows_over(0, 25),
        4,
        "28 rows + the marker, keep 25"
    );

    assert_eq!(buf.drain_front(7).count(), 7);
    assert!(buf.staged_bytes() > 0);
    assert_eq!(buf.drain_all().count(), 22);
    assert_eq!(buf.staged_bytes(), 0);
}

#[test]
fn cell_pool_is_bounded_by_depth_and_by_total_cells() {
    use crate::grid::scroll_convert::LazyBuffer;

    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();
    let narrow: Vec<Cell> = b"narrow"
        .iter()
        .map(|&b| Cell::from_ascii_fast(b))
        .collect();
    let narrow_row = rb.build(&narrow, 80, false);
    for _ in 0..700 {
        buf.push_row(&narrow_row, ScrolledRowExtras::default());
    }
    assert_eq!(buf.drain_all().count(), 700);
    let depth = buf.pooled_bodies();
    assert!(
        depth <= 512,
        "free-list depth must be capped (POOL_MAX_BUFFERS), got {depth}"
    );

    let mut buf = LazyBuffer::new();
    let wide: Vec<Cell> = (0..900u16).map(|_| Cell::from_ascii_fast(b'w')).collect();
    let wide_row = rb.build(&wide, 1000, false);
    for _ in 0..200 {
        buf.push_row(&wide_row, ScrolledRowExtras::default());
    }
    assert_eq!(buf.drain_all().count(), 200);
    let wide_depth = buf.pooled_bodies();
    assert!(
        wide_depth < 200,
        "the cell budget must bite long before the depth cap, got {wide_depth}"
    );
    let used = buf.memory_used();
    assert!(
        used < 640 * 1024,
        "pooled + staged bytes must stay under the budget, got {used}"
    );
}

#[test]
fn width_change_drops_recycled_bodies_and_history_survives() {
    use aterm_scrollback::Scrollback;

    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    for i in 0..40usize {
        grid.carriage_return();
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
    grid.drain_lazy_buffer();
    assert!(
        grid.storage.lazy_buffer.pooled_bodies() > 0,
        "the drain should have parked recycled bodies"
    );

    grid.resize(3, 40);
    assert_eq!(
        grid.storage.lazy_buffer.pooled_bodies(),
        0,
        "a width change must drop the old-width free-list"
    );

    grid.resize(5, 40);
    let history: Vec<String> = (0..grid.scrollback_lines())
        .rev()
        .map(|rev| {
            grid.history_line_rev(rev)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    let expected: Vec<String> = (0..history.len()).map(|i| format!("L{i}")).collect();
    assert_eq!(
        history, expected,
        "recycled bodies must not perturb scrollback content"
    );
}

#[test]
fn recycling_preserves_flooded_scrollback_content() {
    use aterm_scrollback::Scrollback;

    let scrollback = Scrollback::new(400, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);
    for i in 0..300usize {
        grid.carriage_return();
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        if i % 7 == 0 {
            grid.drain_lazy_bounded(4);
        }
    }
    grid.drain_lazy_buffer();

    let history: Vec<String> = (0..grid.scrollback_lines())
        .rev()
        .map(|rev| {
            grid.history_line_rev(rev)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect();
    let expected: Vec<String> = (0..history.len()).map(|i| format!("L{i}")).collect();
    assert_eq!(
        history, expected,
        "flooded history must be intact and ordered"
    );
}
