// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Accessibility snapshot of the visible terminal grid.

use aterm_core::terminal::RenderCell;

/// Human-facing label announced for the terminal view.
#[cfg(a11y_tree)]
pub(crate) const LABEL: &str = "aterm terminal";

/// Refillable extraction storage for the terminal documents read in one window.
/// A completed pass retains only its geometries, without image payloads.
#[cfg(a11y_tree)]
#[derive(Default)]
pub(crate) struct DocumentFrames {
    entries: std::collections::HashMap<(usize, usize), DocumentFrame>,
    epoch: u64,
}

#[cfg(a11y_tree)]
struct DocumentFrame {
    input: aterm_core::terminal::TextFrame,
    epoch: u64,
}

#[cfg(a11y_tree)]
impl DocumentFrames {
    pub(crate) fn pass(&mut self) -> DocumentFramePass<'_> {
        self.epoch = self.epoch.wrapping_add(1);
        DocumentFramePass { frames: self }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Exclusive storage for a synchronous document read; its drop retires unused frames.
#[cfg(a11y_tree)]
pub(crate) struct DocumentFramePass<'a> {
    frames: &'a mut DocumentFrames,
}

#[cfg(a11y_tree)]
impl DocumentFramePass<'_> {
    pub(crate) fn frame(
        &mut self,
        rows: usize,
        cols: usize,
    ) -> &mut aterm_core::terminal::TextFrame {
        let epoch = self.frames.epoch;
        let entry = self
            .frames
            .entries
            .entry((rows, cols))
            .or_insert_with(|| DocumentFrame {
                input: aterm_core::terminal::TextFrame::default(),
                epoch,
            });
        entry.epoch = epoch;
        &mut entry.input
    }
}

#[cfg(a11y_tree)]
impl Drop for DocumentFramePass<'_> {
    fn drop(&mut self) {
        let epoch = self.frames.epoch;
        self.frames.entries.retain(|_, entry| entry.epoch == epoch);
    }
}

/// Appends one grid row to `text` up to `cols`, trimming trailing spaces and ending with `'\n'`.
/// Each cell contributes its base character alone, so the combining marks and joined
/// characters of a multi-scalar cluster are left out.
pub(crate) fn push_visible_row<'a>(
    text: &mut String,
    cells: impl IntoIterator<Item = &'a RenderCell>,
    cols: usize,
) {
    for cell in cells.into_iter().take(cols) {
        text.push(if cell.ch == '\0' || cell.ch.is_control() {
            ' '
        } else {
            cell.ch
        });
    }
    while text.ends_with(' ') {
        text.pop();
    }
    text.push('\n');
}

/// A plain-text and cursor snapshot of the visible grid for assistive technology.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(a11y_tree, test))]
pub(crate) struct AccessibleSnapshot {
    /// Visible screen text, one newline-terminated line per row with trailing blanks trimmed.
    pub text: String,
    /// Grid height in rows.
    pub rows: usize,
    /// Grid width in columns.
    pub cols: usize,
    /// Cursor position as 0-based `(row, col)` when visible.
    pub cursor: Option<(usize, usize)>,
}

#[cfg(any(a11y_tree, test))]
impl AccessibleSnapshot {
    /// Builds a snapshot from rendered rows of cells.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_cells(
        rows_cells: &[Vec<RenderCell>],
        cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        Self::from_cells_in(rows_cells, 0..rows_cells.len(), 0..cols, cursor)
    }

    /// Builds a snapshot of the rectangle `rows` × `cols` within `rows_cells`.
    #[must_use]
    pub(crate) fn from_cells_in(
        rows_cells: &[Vec<RenderCell>],
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
        cursor: Option<(usize, usize)>,
    ) -> Self {
        let width = cols.len();
        let mut text = String::with_capacity(rows.len() * (width + 1));
        for r in rows.clone() {
            let row = rows_cells.get(r).map_or(&[][..], Vec::as_slice);
            let end = cols.end.max(cols.start).min(row.len());
            push_visible_row(&mut text, &row[cols.start.min(row.len())..end], width);
        }
        Self {
            text,
            rows: rows.len(),
            cols: width,
            cursor,
        }
    }

    /// Returns the visible screen text as an accessibility value string.
    #[must_use]
    pub(crate) fn value(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    #[cfg(a11y_tree)]
    #[test]
    fn same_geometry_sessions_retain_one_extraction_buffer() {
        fn retained_bytes(terms: &mut [Terminal], rows: usize, cols: usize) -> usize {
            let mut frames = DocumentFrames::default();
            for _ in 0..3 {
                let mut pass = frames.pass();
                for term in terms.iter_mut() {
                    term.text_frame_into(pass.frame(rows, cols), rows, cols);
                }
            }
            let (_, freed) = aterm_heap_count::bytes_freed_in(|| frames.clear());
            freed
        }
        for (rows, cols) in [(24usize, 80usize), (120, 240)] {
            let mut terms: Vec<_> = (0..8)
                .map(|session| {
                    let mut term = Terminal::new(rows as u16, cols as u16);
                    for row in 0..rows {
                        term.process(
                            format!("\x1b[{};1H{session} {}", row + 1, "x".repeat(cols - 3))
                                .as_bytes(),
                        );
                    }
                    term
                })
                .collect();
            let one = retained_bytes(&mut terms[..1], rows, cols);
            let eight = retained_bytes(&mut terms, rows, cols);
            println!("rows={rows} cols={cols} one_session_bytes={one} eight_session_bytes={eight}");
            assert!(
                one > 0,
                "CONTROL: filled extraction storage owns row buffers"
            );
            assert_eq!(eight, one, "one geometry needs only one refillable buffer");
        }
    }

    #[cfg(a11y_tree)]
    #[test]
    fn cross_terminal_refill_matches_fresh_frames() {
        use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
        use aterm_core::render::RenderInput;
        use aterm_core::selection::{SelectionSide, SelectionType};
        use std::sync::Arc;

        let (rows, cols) = (6usize, 24usize);
        let mut rich = Terminal::new(rows as u16, cols as u16);
        rich.process("\x1b[31;47mשלום e\u{301}".as_bytes());
        rich.text_selection_mut()
            .start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        rich.text_selection_mut()
            .update_selection(0, 3, SelectionSide::Right);
        rich.text_selection_mut().complete_selection();
        rich.grid_mut()
            .cell_extra_mut(2, 1)
            .set_image(Some(ImageRef {
                image: Arc::new(ImageData {
                    bytes: vec![7; 4],
                    format: ImageFormat::Png,
                    cols: 1,
                    rows: 1,
                    z_index: 0,
                    band_lift_px: 0,
                    cell_offset_px: (0, 0),
                    scaling: ImageScaling::Fit,
                    source_rect: None,
                }),
                cell_row: 0,
                cell_col: 0,
                kitty: None,
            }));
        let mut plain = Terminal::new(rows as u16, cols as u16);
        plain.process(b"\x1b[?25lplain terminal");
        let mut alternate = Terminal::new(rows as u16, cols as u16);
        alternate.process(b"\x1b[?1049h\x1b[2;1H\x1b#6alternate");
        let mut scratch = RenderInput::empty();
        for _ in 0..3 {
            for (index, term) in [&mut rich, &mut plain, &mut alternate]
                .into_iter()
                .enumerate()
            {
                let mut fresh = term.cell_frame(rows, cols);
                term.cell_frame_into(&mut scratch, rows, cols);
                if index == 0 {
                    assert!(
                        fresh.visual_to_logical.iter().any(|row| !row.is_empty()),
                        "CONTROL: RTL row was reordered"
                    );
                    assert!(
                        fresh.images.iter().any(|row| !row.is_empty()),
                        "CONTROL: the first terminal carries an image"
                    );
                    fresh.overlay_ime_preedit("日e\u{301}", None, false);
                    scratch.overlay_ime_preedit("日e\u{301}", None, false);
                }
                assert_eq!(
                    scratch, fresh,
                    "all projected data must match a fresh frame"
                );
                assert_eq!(scratch.terminal_id, fresh.terminal_id);
                assert_eq!(scratch.engine_row_order, fresh.engine_row_order);
                assert_eq!(scratch.apron_row, fresh.apron_row);
            }
        }

        let mut history: Vec<_> = ['A', 'B']
            .into_iter()
            .map(|label| {
                let mut term = Terminal::new(rows as u16, cols as u16);
                for row in 0..12 {
                    term.process(format!("{label} row {row}\r\n").as_bytes());
                }
                term.process(format!("{label} incoming").as_bytes());
                term.scroll_display(1);
                term
            })
            .collect();
        let a = history[0].cell_frame(rows, cols);
        let b = history[1].cell_frame(rows, cols);
        assert_ne!(
            a.terminal_id, b.terminal_id,
            "CONTROL: distinct extraction identities"
        );
        assert!(a.apron_row.present && b.apron_row.present);
        assert_ne!(
            a.apron_row, b.apron_row,
            "CONTROL: the incoming rows carry different text"
        );
        for _ in 0..3 {
            for term in &mut history {
                let fresh = term.cell_frame(rows, cols);
                term.cell_frame_into(&mut scratch, rows, cols);
                assert_eq!(scratch, fresh);
                assert_eq!(scratch.terminal_id, fresh.terminal_id);
                assert_eq!(
                    scratch.apron_row, fresh.apron_row,
                    "the apron must belong to the current terminal"
                );
            }
        }
    }

    #[cfg(a11y_tree)]
    #[test]
    #[ignore = "measures text versus paint extraction; run alone"]
    fn text_frame_refill_cost() {
        use aterm_core::render::RenderInput;
        use std::hint::black_box;
        use std::time::Instant;

        fn round(
            terms: &mut [Terminal],
            rows: usize,
            cols: usize,
            isolated: &mut RenderInput,
            pool: &mut DocumentFrames,
            shared: bool,
        ) {
            if shared {
                let mut pass = pool.pass();
                for term in terms {
                    let frame = pass.frame(rows, cols);
                    term.text_frame_into(frame, rows, cols);
                    black_box(&*frame);
                }
            } else {
                for term in terms.iter_mut() {
                    term.cell_frame_into(isolated, rows, cols);
                    black_box(&*isolated);
                }
                for row in &mut isolated.images {
                    row.clear();
                }
            }
        }
        for scrolled in [false, true] {
            for images in [false, true] {
                for (rows, cols) in [(24usize, 80usize), (120, 240)] {
                    let mut terms: Vec<_> = (0..8)
                        .map(|index| {
                            let mut term = Terminal::new(rows as u16, cols as u16);
                            for _ in 0..rows + 4 {
                                term.process(
                                    format!("{index} {}\r\n", "x".repeat(cols - 3)).as_bytes(),
                                );
                            }
                            term.process(format!("{index} {}", "x".repeat(cols - 3)).as_bytes());
                            if images {
                                use aterm_core::grid::extra::{
                                    ImageData, ImageFormat, ImageRef, ImageScaling,
                                };
                                use std::sync::Arc;
                                let image = Arc::new(ImageData {
                                    bytes: vec![7; 4],
                                    format: ImageFormat::Png,
                                    cols: 1,
                                    rows: 1,
                                    z_index: 0,
                                    band_lift_px: 0,
                                    cell_offset_px: (0, 0),
                                    scaling: ImageScaling::Fit,
                                    source_rect: None,
                                });
                                for row in 0..rows {
                                    for col in (0..cols).step_by(4) {
                                        term.grid_mut()
                                            .cell_extra_mut(row as u16, col as u16)
                                            .set_image(Some(ImageRef {
                                                image: Arc::clone(&image),
                                                cell_row: 0,
                                                cell_col: 0,
                                                kitty: None,
                                            }));
                                    }
                                }
                            }
                            if scrolled {
                                term.scroll_display(1);
                            }
                            term
                        })
                        .collect();
                    let mut isolated = RenderInput::empty();
                    let mut pool = DocumentFrames::default();
                    for _ in 0..3 {
                        for shared in [false, true] {
                            round(&mut terms, rows, cols, &mut isolated, &mut pool, shared);
                        }
                    }
                    for sample in 0..5 {
                        for shared in if sample % 2 == 0 {
                            [false, true]
                        } else {
                            [true, false]
                        } {
                            let start = Instant::now();
                            for _ in 0..100 {
                                round(&mut terms, rows, cols, &mut isolated, &mut pool, shared);
                            }
                            println!(
                                "scrolled={scrolled} images={images} rows={rows} cols={cols} documents=800 text={shared} sample={sample} elapsed_ns={}",
                                start.elapsed().as_nanos()
                            );
                        }
                    }
                }
            }
        }
    }

    #[cfg(a11y_tree)]
    #[test]
    fn document_pass_reuses_each_geometry_and_frees_retired_storage() {
        let mut frames = DocumentFrames::default();
        let mut large = Terminal::new(24, 256);
        let mut small = Terminal::new(12, 40);
        for row in 0..24 {
            large.process(format!("\x1b[{};1H{}", row + 1, "x".repeat(256)).as_bytes());
        }
        for row in 0..12 {
            small.process(format!("\x1b[{};1H{}", row + 1, "x".repeat(40)).as_bytes());
        }
        {
            let mut pass = frames.pass();
            large.text_frame_into(pass.frame(24, 256), 24, 256);
            small.text_frame_into(pass.frame(12, 40), 12, 40);
        }
        let (_, allocations) = aterm_heap_count::allocations_in(|| {
            let mut pass = frames.pass();
            large.text_frame_into(pass.frame(24, 256), 24, 256);
            small.text_frame_into(pass.frame(12, 40), 12, 40);
        });
        assert_eq!(
            allocations, 0,
            "both pane geometries keep their reusable storage"
        );
        let (_, freed) = aterm_heap_count::bytes_freed_in(|| {
            let mut pass = frames.pass();
            let _ = pass.frame(12, 40);
        });
        assert!(
            freed >= 24 * 256 * std::mem::size_of::<RenderCell>(),
            "retired rows must be released"
        );
    }

    #[cfg(a11y_tree)]
    #[test]
    fn document_pass_never_captures_image_payloads() {
        use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
        use std::sync::Arc;

        let image = || {
            Arc::new(ImageData {
                bytes: vec![7; 4],
                format: ImageFormat::Png,
                cols: 1,
                rows: 1,
                z_index: 0,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            })
        };
        let mut frames = DocumentFrames::default();
        let image = image();
        let mut term = Terminal::new(24, 80);
        term.grid_mut()
            .cell_extra_mut(0, 0)
            .set_image(Some(ImageRef {
                image: Arc::clone(&image),
                cell_row: 0,
                cell_col: 0,
                kitty: None,
            }));
        let owners = Arc::strong_count(&image);
        {
            let mut pass = frames.pass();
            let frame = pass.frame(24, 80);
            term.text_frame_into(frame, 24, 80);
            assert_eq!(Arc::strong_count(&image), owners);
        }
        assert_eq!(Arc::strong_count(&image), owners);
        let painted = term.cell_frame(24, 80);
        assert!(
            !painted.images[0].is_empty(),
            "CONTROL: full paint captures the image"
        );
        assert!(Arc::strong_count(&image) > owners);
    }

    #[test]
    fn a_pane_rectangle_reads_only_its_own_columns() {
        let mut term = Terminal::new(2, 21);
        term.process(b"left\x1b[11G\xe2\x96\x90right");
        let cells: Vec<Vec<RenderCell>> = (0..2).map(|r| term.render_row(r)).collect();

        let whole = AccessibleSnapshot::from_cells(&cells, 21, None);
        let composed = whole.value().lines().next().unwrap();
        assert!(
            composed.contains("left") && composed.contains("right"),
            "the composed row carries both panes: {composed:?}"
        );

        let left = AccessibleSnapshot::from_cells_in(&cells[..], 0..2, 0..10, None);
        let right = AccessibleSnapshot::from_cells_in(&cells[..], 0..2, 11..21, Some((0, 5)));
        assert_eq!(left.value(), "left\n\n", "no divider, no neighbour");
        assert_eq!(right.value(), "right\n\n");
        assert_eq!((left.rows, left.cols), (2, 10));
        assert_eq!(right.cursor, Some((0, 5)), "the pane-local caret");
    }
}
