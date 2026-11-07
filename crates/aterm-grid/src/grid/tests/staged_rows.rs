// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The staged rows between the ring and the tiered store ([`LazyBuffer`]): each staged row reads
//! back as the eager conversion of the row it copied, whichever way it leaves, and the buffer's
//! running sums, arena and flood cut stay what a plain model of its rows says.

use std::collections::VecDeque;
use std::sync::Arc;

use aterm_scrollback::{HyperlinkSpan, ImageSpan, Line};
use aterm_spec::rng::XorShift;

use super::super::lazy_buffer::LazyBuffer;
use super::super::scroll_convert::ScrolledRowExtras;
use crate::test_counters::with_short_straddle_read;
use crate::{Cell, CellFlags, Grid, ImageData, ImageFormat, ImageScaling, PackedColor, Row};

/// Compare the text, attributes, hyperlinks, and wrapped state of two `Line`s.
fn assert_lines_equivalent(eager: &Line, lazy: &Line, ctx: &str) {
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
    for i in 0..text.chars().count() {
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

fn ascii(text: &[u8]) -> Vec<Cell> {
    text.iter().map(|&b| Cell::from_ascii_fast(b)).collect()
}

/// NEGATIVE CONTROL: non-default styles follow multi-byte complex glyphs, combining marks, and
/// wide CJK cells so `assert_lines_equivalent` catches any char-index vs byte-offset or column skew.
/// A staged row read in place (`get_line`) and drained (read first or not) matches
/// `row_to_line_with_stored_extras` across row shapes and extras.
#[test]
fn a_staged_row_reads_back_as_the_eager_conversion() {
    use aterm_alloc::SmallVec;

    let styled = |text: &[u8], fg, bg, flags| -> Vec<Cell> {
        text.iter()
            .map(|&b| Cell::with_style(char::from(b), fg, bg, flags))
            .collect()
    };
    let link =
        |start_col, end_col, url: &str| HyperlinkSpan::new(start_col, end_col, Arc::from(url));
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

    let mut cells = ascii(b"AAA");
    cells.extend(styled(
        b"AA",
        PackedColor::indexed(3),
        PackedColor::indexed(5),
        CellFlags::ITALIC,
    ));
    complex(&mut cells, 2);
    let mut extras = ScrolledRowExtras::default();
    extras.complex_chars.push((2, Arc::from("\u{1F600}")));
    cases.push(("complex char", cells, extras, false));

    let mut cells = ascii(b"ca");
    cells.extend(styled(
        b"fe",
        PackedColor::indexed(6),
        PackedColor::indexed(1),
        CellFlags::UNDERLINE,
    ));
    let mut extras = ScrolledRowExtras::default();
    extras
        .combining
        .push((1, SmallVec::from_elem('\u{0301}', 1)));
    cases.push(("combining mark", cells, extras, false));

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
    cells.extend(styled(
        b"ef",
        PackedColor::indexed(5),
        PackedColor::indexed(7),
        CellFlags::ITALIC | CellFlags::UNDERLINE,
    ));
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
    cells.extend(styled(
        b"B",
        PackedColor::indexed(9),
        PackedColor::indexed(2),
        CellFlags::BOLD,
    ));
    cases.push(("wide", cells, ScrolledRowExtras::default(), false));

    let mut rb = RowBuilder::new();
    for (name, cells, extras, wrapped) in cases {
        let row = rb.build(&cells, 80, wrapped);
        let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
        let mut buf = LazyBuffer::new();
        buf.push_row(&row, extras.clone());
        buf.push_row(&row, extras);
        let by_ref = buf.get_line(0).expect("the read row").clone();
        let drained: Vec<_> = buf.drain_all().collect();
        assert_lines_equivalent(&eager, &by_ref, &format!("{name} (get_line)"));
        assert_lines_equivalent(&eager, &drained[0], &format!("{name} (drained, read)"));
        assert_lines_equivalent(&eager, &drained[1], &format!("{name} (drained, unread)"));
        assert_eq!(drained[1].is_wrapped(), wrapped, "{name}");
        if name == "wide" {
            // "A\u{4E16}B" has 3 chars (5 UTF-8 bytes, 4 columns): char index 2 is styled 'B',
            // while byte offset 4 is out of bounds and falls back to the unstyled default of 'A'.
            assert_ne!(drained[1].get_attr(2), drained[1].get_attr(0));
            assert_eq!(drained[1].get_attr(4), drained[1].get_attr(0));
        }
    }
}

/// A row staged after a longer one left carries none of its cells: the arena reuses the space
/// the first row's cells held.
#[test]
fn a_row_staged_after_a_drained_one_carries_none_of_its_cells() {
    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();

    let long_cells: Vec<Cell> = (0..60u8)
        .map(|i| Cell::from_ascii_fast(b'a' + i % 26))
        .collect();
    let long_row = rb.build(&long_cells, 80, false);
    buf.push_row(&long_row, ScrolledRowExtras::default());
    assert_eq!(buf.drain_all().count(), 1);

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
    buf.push_row(&short_row, extras);
    buf.assert_running_sums();
    let drained: Vec<_> = buf.drain_all().collect();
    assert_lines_equivalent(&eager, &drained[0], "line staged after a drained one");
}

#[test]
fn drop_oldest_cuts_the_front_and_weighs_what_stays() {
    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();
    let row = rb.build(&ascii(b"dropped under flood"), 80, false);

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
        2 * LazyBuffer::row_bytes(b"dropped under flood".len()),
        "two staged rows remain, each weighed at its OCCUPIED cells, not the width"
    );
}

#[test]
fn front_rows_over_sheds_the_fewest_rows_that_fit_and_keeps_the_floor() {
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

/// A drained flood hands back the arena and the record spine it grew, and the buffer accounts
/// for exactly what it keeps.
#[test]
fn a_drained_flood_hands_back_its_arena_and_spine() {
    let mut rb = RowBuilder::new();
    let wide: Vec<Cell> = (0..900u16).map(|_| Cell::from_ascii_fast(b'w')).collect();
    let wide_row = rb.build(&wide, 1000, false);
    let narrow_row = rb.build(&ascii(b"n"), 1000, false);
    for drain in [true, false] {
        let mut buf = LazyBuffer::new();
        for i in 0..10_000 {
            let row = if i % 16 == 0 { &wide_row } else { &narrow_row };
            buf.push_row(row, ScrolledRowExtras::default());
        }
        assert!(buf.arena_capacity() >= 625 * wide.len());
        assert!(buf.spine_capacity() >= 10_000);
        if drain {
            assert_eq!(buf.drain_all().count(), 10_000);
            buf.release_spine_slack();
        } else {
            buf.clear();
        }
        let floor = (1_000 + 1) * 128;
        assert!(
            buf.arena_capacity() <= floor * 2,
            "the arena keeps at most twice its floor, not {} cells",
            buf.arena_capacity()
        );
        assert!(buf.spine_capacity() <= 2 * 1_000);
        assert_eq!(
            buf.memory_used(),
            buf.spine_capacity() * LazyBuffer::row_bytes(0)
                + buf.arena_capacity() * std::mem::size_of::<Cell>(),
            "empty staging still accounts for both container allocations"
        );
    }
}

#[test]
fn synchronous_drains_keep_history_whole_and_ordered() {
    use aterm_scrollback::Scrollback;

    for cols in [80u16, 120, 128] {
        let scrollback = Scrollback::new(80, 10_000, 100_000_000);
        let mut grid = Grid::with_tiered_scrollback(2, cols, 2, scrollback);
        let text = |row| {
            let mut text = format!("row{row:04} ");
            text.extend(std::iter::repeat_n('x', usize::from(cols) - 1 - text.len()));
            text
        };
        let mut drains = 0;
        let mut written = 0;
        while written < 4_000 {
            grid.carriage_return();
            for c in text(written).chars() {
                grid.write_char(c);
            }
            let before = grid.lazy_backlog_len();
            grid.line_feed();
            written += 1;
            drains += usize::from(grid.lazy_backlog_len() < before);
        }
        assert!(
            drains >= 3,
            "{cols} columns: the flood drained synchronously"
        );
        grid.drain_lazy_buffer();
        let history: Vec<String> = (0..grid.scrollback_lines())
            .rev()
            .map(|rev| grid.history_line_rev(rev).unwrap().to_string())
            .filter(|line| line.starts_with("row"))
            .collect();
        for (row, actual) in history.iter().enumerate() {
            assert_eq!(actual, &text(row), "{cols}-column history order/content");
        }
        assert_eq!(history.len(), written - 1);
    }
}

#[test]
fn a_width_change_keeps_staged_history() {
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
    grid.resize(3, 40);
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
        "staged history survives the width change"
    );
}

#[test]
fn bounded_drains_keep_flooded_history_whole_and_ordered() {
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

/// A row whose leading cells output never wrote stages only from its first written cell on, and
/// still materializes exactly as the eager conversion of the whole row (the skipped cells come
/// back blank) through `get_line` and a drain, twice so the second round stages into the space
/// the first round's cells held.
#[test]
fn a_row_staged_past_its_unwritten_lead_materializes_whole() {
    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();
    for round in 0..2 {
        let mut eager_lines = Vec::new();
        for lead in [1u16, 7, 40, 79] {
            for variant in 0..4 {
                let ctx = format!("round {round}, lead {lead}, variant {variant}");
                // SAFETY: the row is used only within this test; `rb.pages` outlives it.
                let mut row = unsafe { Row::new(80, &mut rb.pages) };
                let text: &[u8] = if lead == 79 { b"y" } else { b"tail" };
                for (col, &byte) in (lead..).zip(text) {
                    let cell = if variant == 1 {
                        Cell::with_style(
                            char::from(byte),
                            PackedColor::indexed(3),
                            PackedColor::indexed(4),
                            CellFlags::BOLD,
                        )
                    } else {
                        Cell::from_ascii_fast(byte)
                    };
                    row.set(col, cell);
                }
                let mut extras = ScrolledRowExtras::default();
                if variant == 2 {
                    extras.hyperlinks.push(HyperlinkSpan::new(
                        lead,
                        lead + 1,
                        Arc::from("https://lead.example"),
                    ));
                }
                row.set_wrapped(variant == 3);
                let eager = Grid::row_to_line_with_stored_extras(&row, &extras);
                buf.push_row(&row, extras);
                let idx = buf.staged_rows() - 1;
                assert_eq!(
                    buf.stored_cells(idx),
                    text.len(),
                    "{ctx}: only the written cells are staged"
                );
                if variant % 2 == 0 {
                    let read = buf.get_line(idx).expect("the staged row");
                    assert_lines_equivalent(&eager, read, &format!("{ctx} (get_line)"));
                }
                eager_lines.push(eager);
            }
        }
        let drained: Vec<_> = buf.drain_all().collect();
        assert_eq!(drained.len(), eager_lines.len(), "round {round}");
        for (i, (eager, line)) in eager_lines.iter().zip(&drained).enumerate() {
            assert_lines_equivalent(eager, line, &format!("round {round}, drained row {i}"));
        }
    }
}

fn test_image() -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes: vec![7; 256],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    })
}

/// The staged image-row count, which lets `any_image` skip a text-only backlog, stays exact
/// whichever history mutation adds or removes an image; text and surviving spans stay intact.
#[test]
fn lazy_buffer_image_row_count_tracks_images_across_history_mutations() {
    let image = test_image();
    let span = || ImageSpan::new(0, 1, 0, 0, Arc::clone(&image));
    let mut rb = RowBuilder::new();
    let row = rb.build(&[Cell::from_ascii_fast(b'x')], 80, false);
    let mut buf = LazyBuffer::new();
    for _ in 0..4 {
        buf.push_row(&row, ScrolledRowExtras::default());
    }
    let check = |buf: &LazyBuffer, text_only: bool| {
        buf.assert_running_sums();
        assert_eq!(buf.any_image(&|_| true), !text_only);
    };
    check(&buf, true);
    assert!(buf.set_line_images(1, vec![span(), span()]));
    assert!(buf.set_line_images(3, vec![span()]));
    check(&buf, false);
    assert!(!buf.any_image(&|_| false));
    assert!(buf.set_line_images(1, vec![]));
    check(&buf, false);
    assert_eq!(buf.drain_back(1).count(), 1);
    check(&buf, true);

    assert!(buf.set_line_images(0, vec![span()]));
    assert!(buf.set_line_images(2, vec![span()]));
    let mut moved = buf.take_staged();
    check(&buf, true);
    check(&moved, false);
    let first = moved.drain_front(1).next().expect("oldest row");
    assert_eq!(first.to_string(), "x");
    check(&moved, false);
    assert!(moved.retain_images(&mut |_| false));
    check(&moved, true);
    assert!(!moved.retain_images(&mut |_| false));
    assert!(moved.set_line_images(0, vec![span()]));
    assert_eq!(moved.drain_all().count(), 2);
    check(&moved, true);
    moved.push_row(&row, ScrolledRowExtras::default());
    assert!(moved.set_line_images(0, vec![span()]));
    moved.clear();
    check(&moved, true);

    let mut extras = ScrolledRowExtras::default();
    extras.images.push(span());
    extras.images.push(span());
    moved.push_row(&row, extras);
    moved.push_row(&row, ScrolledRowExtras::default());
    check(&moved, false);
    let mut visits = 0;
    assert!(moved.retain_images(&mut |_| {
        visits += 1;
        visits == 1
    }));
    check(&moved, false);
    assert_eq!(moved.drop_oldest(1), 1);
    moved.assert_running_sums();
    assert!(!moved.any_image(&|_| true));
    assert!(!moved.set_line_images(0, vec![span()]));
    assert!(moved.set_line_images(1, vec![span()]));
    assert_eq!(moved.drain_front(1).count(), 1); // Only the marker leaves.
    check(&moved, false);
    assert_eq!(moved.shed_oldest(1, 0), 0);
    check(&moved, true);
}

/// One staged row as the model holds it: the row it copied and the extras it keeps.
struct ModelRow {
    row: Row,
    extras: ScrolledRowExtras,
}

impl ModelRow {
    /// What a reader must get for this row: the eager conversion of the row and its extras.
    fn line(&self) -> String {
        format!(
            "{:?}",
            Grid::row_to_line_with_stored_extras(&self.row, &self.extras)
        )
    }

    /// What staging this row costs: its record, its occupied cells, its extras.
    fn weight(&self) -> usize {
        let extras = if self.extras.is_empty() {
            0
        } else {
            self.extras.retained_memory_used()
        };
        LazyBuffer::row_bytes(usize::from(self.row.written_len())) + extras
    }
}

/// What the buffer must hold: its rows, oldest first, and the flood cut's count (`None` when no
/// cut is open).
#[derive(Default)]
struct Model {
    rows: VecDeque<ModelRow>,
    cut: Option<u64>,
}

impl Model {
    fn len(&self) -> usize {
        self.rows.len() + usize::from(self.cut.is_some())
    }

    /// Remove the `n` oldest rows (never the marker); returns how many went.
    fn discard_front(&mut self, n: usize) -> usize {
        let n = n.min(self.rows.len());
        self.rows.drain(..n);
        n
    }
}

/// Everything a reader sees of `buf`, against the model: the line count, the cut, every line
/// (read in place, which caches it), the weights, the link payload and the image rows.
fn compare(buf: &LazyBuffer, model: &Model) -> Result<(), String> {
    let weight: usize = model.rows.iter().map(ModelRow::weight).sum();
    let links: usize = model.rows.iter().map(|row| row.extras.link_bytes()).sum();
    let images = model.rows.iter().any(|row| !row.extras.images.is_empty());
    let seen = (
        buf.len(),
        buf.cut_open().then(|| buf.cut_dropped()),
        buf.staged_bytes(),
        buf.link_bytes(),
        buf.any_image(&|_| true),
    );
    let want = (model.len(), model.cut, weight, links, images);
    if seen != want {
        return Err(format!(
            "(lines, cut, staged bytes, link bytes, images): {seen:?}, the model {want:?}"
        ));
    }
    let offset = usize::from(model.cut.is_some());
    for (i, row) in model.rows.iter().enumerate() {
        let staged = buf
            .get_line(i + offset)
            .map(|line| format!("{line:?}"))
            .unwrap_or_default();
        if staged != row.line() {
            return Err(format!(
                "staged row {i}: {staged}, the model {}",
                row.line()
            ));
        }
    }
    Ok(())
}

/// Generate a row for the differential: a blank lead, then plain or styled text, wrapped or not,
/// with no extras, a link, true colour or an image.
fn generated_row(
    rng: &mut XorShift,
    rb: &mut RowBuilder,
    image: &Arc<ImageData>,
) -> (Row, ScrolledRowExtras) {
    let cols = 40usize;
    let lead = if rng.below(3) == 0 { rng.index(20) } else { 0 };
    let text = [0, 1, 1, 2, 3, 7, 18, 39][rng.index(8)].min(cols - lead);
    // SAFETY: the row is used only within this test; `rb.pages` outlives it.
    let mut row = unsafe { Row::new(40, &mut rb.pages) };
    let styled = rng.below(4) == 0;
    for col in lead..lead + text {
        let byte = b'a' + u8::try_from(col % 26).unwrap_or(0);
        let cell = if styled {
            Cell::with_style(
                char::from(byte),
                PackedColor::indexed(2),
                PackedColor::indexed(5),
                CellFlags::BOLD,
            )
        } else {
            Cell::from_ascii_fast(byte)
        };
        row.set(u16::try_from(col).unwrap_or(0), cell);
    }
    row.set_wrapped(rng.below(5) == 0);
    let mut extras = ScrolledRowExtras::default();
    let at = u16::try_from(lead).unwrap_or(0);
    match rng.below(8) {
        0 if text > 0 => extras.hyperlinks.push(HyperlinkSpan::new(
            at,
            at + 1,
            Arc::from("https://staged.example/a/long/enough/path"),
        )),
        1 if text > 0 => extras.rgb_fg.push((at, [9, 8, 7])),
        2 => extras
            .images
            .push(ImageSpan::new(0, 1, 0, 0, Arc::clone(image))),
        _ => {}
    }
    (row, extras)
}

/// One generated stream of `steps` operations on a staged buffer and its model ([`compare`]d
/// after each); returns how many rows whose cells straddle the arena's ring it read, or the
/// first divergence.
fn run(seed: u64, steps: usize) -> Result<usize, String> {
    let image = test_image();
    let mut rng = XorShift::new(seed);
    let mut rb = RowBuilder::new();
    let mut buf = LazyBuffer::new();
    let mut model = Model::default();
    let mut straddled = 0;
    let mut ops = Vec::new();
    for step in 0..steps {
        let op = rng.below(20);
        ops.push(op);
        let mut drained: Option<(Vec<String>, Vec<String>)> = None;
        match op {
            0..=8 => {
                let (row, extras) = generated_row(&mut rng, &mut rb, &image);
                // Both sides take a clone, so their vectors' capacities (which the weight
                // counts) agree.
                if rng.below(2) == 0 {
                    buf.push_row(&row, extras.clone());
                } else {
                    buf.push_row_boxed(&row, Some(Box::new(extras.clone())));
                }
                model.rows.push_back(ModelRow {
                    row,
                    extras: extras.clone(),
                });
            }
            9 | 10 => {
                let n = rng.index(4);
                let dropped = model.discard_front(n);
                if dropped > 0 {
                    model.cut = Some(model.cut.unwrap_or(0) + dropped as u64);
                }
                buf.drop_oldest(n);
            }
            11 | 12 => {
                let n = rng.index(4);
                let hole = if rng.below(2) == 0 { 0 } else { rng.below(9) };
                model.discard_front(n);
                model.cut = (hole > 0).then_some(hole);
                buf.shed_oldest(n, hole);
            }
            13 | 14 => {
                let n = rng.index(6);
                let take = rng.index(n + 1);
                let n = n.min(model.len());
                let mut want = Vec::new();
                if n > 0
                    && let Some(cut) = model.cut.take()
                {
                    want.push(format!(
                        "{:?}",
                        super::super::lazy_buffer::flood_marker_line(cut)
                    ));
                }
                let rows = n - want.len();
                want.extend(model.rows.drain(..rows).map(|row| row.line()));
                want.truncate(take);
                let seen = buf
                    .drain_front(n)
                    .take(take)
                    .map(|line| format!("{line:?}"))
                    .collect();
                drained = Some((seen, want));
            }
            15 => {
                let n = rng.index(4);
                let keep = model.rows.len() - n.min(model.rows.len());
                let want = model.rows.drain(keep..).map(|row| row.line()).collect();
                let seen = buf.drain_back(n).map(|line| format!("{line:?}")).collect();
                drained = Some((seen, want));
            }
            16 if model.len() > 0 => {
                let idx = rng.index(model.len());
                let spans = if rng.below(2) == 0 {
                    Vec::new()
                } else {
                    vec![ImageSpan::new(0, 1, 0, 0, Arc::clone(&image))]
                };
                let offset = usize::from(model.cut.is_some());
                if idx >= offset {
                    let images = &mut model.rows[idx - offset].extras.images;
                    if spans.is_empty() {
                        images.clear();
                    } else {
                        *images = spans.clone();
                    }
                }
                if buf.set_line_images(idx, spans) != (idx >= offset) {
                    return Err(format!(
                        "step {step}: set_line_images({idx}) answered wrong"
                    ));
                }
            }
            17 => {
                // The buffer strips the oldest rows that hold links until it has freed `want`
                // bytes: its cursor only skips rows that hold none.
                let want = rng.index(200);
                let mut left = want;
                for row in &mut model.rows {
                    if left == 0 {
                        break;
                    }
                    let links = row.extras.link_bytes();
                    if links > 0 {
                        row.extras.strip_hyperlinks();
                        left = left.saturating_sub(links);
                    }
                }
                buf.strip_oldest_links(want);
            }
            18 => {
                let moved = buf.take_staged();
                if !buf.is_empty() {
                    return Err(format!("step {step}: the move left lines behind"));
                }
                buf = moved;
            }
            _ if rng.below(10) == 0 => {
                buf.clear();
                model = Model::default();
            }
            _ => buf.release_spine_slack(),
        }
        if let Some((seen, want)) = drained
            && seen != want
        {
            return Err(format!(
                "step {step} after ops {ops:?}: drained {seen:#?}, the model {want:#?}"
            ));
        }
        straddled += buf.straddling_rows();
        compare(&buf, &model)
            .map_err(|divergence| format!("step {step} after ops {ops:?}: {divergence}"))?;
        buf.assert_running_sums();
    }
    Ok(straddled)
}

/// The staged rows against a plain model of them, over generated streams of every operation the
/// grid applies to them: staging rows (cells in the arena past a blank lead, extras beside),
/// reading them in place, the flood's drops and sheds, drains from the front (taken whole or in
/// part) and the back, image and link edits, a move to a reflow job, a clear and a slack release.
/// After every step the buffer reads, weighs and counts as the model does, and its arena holds
/// the rows' cells back to back. The streams wrap the arena's ring under rows, so rows whose
/// cells straddle it are read too.
#[test]
fn staged_rows_match_a_model_of_them() {
    let mut straddled = 0;
    for seed in 1..=6 {
        straddled += run(seed, 3_000).unwrap_or_else(|divergence| panic!("{divergence}"));
    }
    assert!(
        straddled > 100,
        "the streams must read rows whose cells straddle the arena's ring: {straddled}"
    );
}

#[test]
fn the_staged_row_model_catches_a_straddling_row_read_one_cell_short() {
    assert!(
        with_short_straddle_read(|| run(1, 3_000)).is_err(),
        "a straddling row read one cell short read back as the row it copied"
    );
}
