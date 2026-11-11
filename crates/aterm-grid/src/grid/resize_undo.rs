// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Resize undo for rows-only shrink-and-grow round trips on history-free grids.

use super::scroll_convert::ScrolledRowExtras;
use super::{Cursor, Grid, HorizontalMargins, ScrollRegion};
use crate::grid::reflow::ResizePolicy;
use crate::row::RowSnapshot;

/// One row a shrink took off the screen, in unit-shrink order.
#[derive(Debug)]
pub(crate) enum UndoRow {
    /// A trailing blank row below the cursor, dropped off the bottom.
    Trimmed,
    /// A top row demoted into (and evicted from) the absent history.
    Demoted(RowSnapshot, Option<Box<ScrolledRowExtras>>),
    /// A bottom row pushed off the screen (the cursor sat too high to demote).
    Pushed(RowSnapshot, Option<Box<ScrolledRowExtras>>),
}

/// One row a grow puts back at the bottom.
pub(crate) type BottomRow = Option<(RowSnapshot, Option<Box<ScrolledRowExtras>>)>;

/// The cursor-owned state a resize resets or moves, compared whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorPicture {
    cursor: Cursor,
    pending_wrap: bool,
    follows_glyph: bool,
    saved_cursor: Cursor,
    saved_valid: bool,
    saved_pending_wrap: bool,
    scroll_region: ScrollRegion,
    margins: HorizontalMargins,
    has_margins: bool,
}

/// The stash: the rows a rows-only shrink took off the screen, popped LIFO by a grow.
#[derive(Debug)]
pub(crate) struct ResizeUndo {
    /// LIFO: the newest shrink's rows are at the end.
    pub(crate) entries: Vec<UndoRow>,
    cols: u16,
    rows_before: u16,
    pre: CursorPicture,
    rows_after: u16,
    seal_gen: u64,
    after: CursorPicture,
    serial: u64,
}

static NEXT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Grid {
    #[inline]
    const fn cursor_picture(&self) -> CursorPicture {
        let s = &self.storage;
        CursorPicture {
            cursor: s.cursor_state.cursor,
            pending_wrap: s.cursor_state.pending_wrap,
            follows_glyph: s.cursor_state.follows_glyph,
            saved_cursor: s.cursor_state.saved_cursor.cursor,
            saved_valid: s.cursor_state.saved_cursor.valid,
            saved_pending_wrap: s.cursor_state.saved_cursor.pending_wrap,
            scroll_region: s.cursor_state.scroll_region,
            margins: s.cursor_state.horizontal_margins,
            has_margins: s.has_horizontal_margins,
        }
    }

    #[inline]
    const fn set_cursor_picture(&mut self, p: CursorPicture) {
        let s = &mut self.storage;
        s.cursor_state.cursor = p.cursor;
        s.cursor_state.pending_wrap = p.pending_wrap;
        s.cursor_state.follows_glyph = p.follows_glyph;
        s.cursor_state.saved_cursor.cursor = p.saved_cursor;
        s.cursor_state.saved_cursor.valid = p.saved_valid;
        s.cursor_state.saved_cursor.pending_wrap = p.saved_pending_wrap;
        s.cursor_state.scroll_region = p.scroll_region;
        s.cursor_state.horizontal_margins = p.margins;
        s.has_horizontal_margins = p.has_margins;
    }

    #[inline]
    fn keeps_resize_undo(&self, policy: ResizePolicy) -> bool {
        policy == ResizePolicy::Native
            && self.storage.max_scrollback == 0
            && !self.storage.stages_evicted_rows()
            && self.storage.total_lines == usize::from(self.storage.visible_rows)
    }

    /// Run first by every resize, before anything moves.
    pub(super) fn admit_resize_undo(&mut self, new_rows: u16, new_cols: u16, policy: ResizePolicy) {
        let shape = self.keeps_resize_undo(policy) && new_cols == self.storage.cols;
        let picture = self.cursor_picture();
        let valid = shape
            && self.storage.resize_undo.as_ref().is_some_and(|u| {
                u.cols == new_cols
                    && u.rows_after == self.storage.visible_rows
                    && u.seal_gen == self.storage.content_gen
                    && u.after == picture
            });
        if !valid {
            self.storage.resize_undo = None;
        }
        if shape && self.storage.resize_undo.is_none() && new_rows < self.storage.visible_rows {
            self.storage.resize_undo = Some(Box::new(ResizeUndo {
                entries: Vec::new(),
                cols: new_cols,
                rows_before: self.storage.visible_rows,
                pre: picture,
                rows_after: self.storage.visible_rows,
                seal_gen: self.storage.content_gen,
                after: picture,
                serial: NEXT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            }));
        }
    }

    /// Run last by every resize: seal the stash at what this resize left, or, once a grow has
    /// handed every entry back on the first shrink's height, put that shrink's cursor picture back
    /// and drop it.
    pub(super) fn seal_resize_undo(&mut self) {
        let Some(undo) = self.storage.resize_undo.as_ref() else {
            return;
        };
        if undo.entries.is_empty() {
            let back = (undo.rows_before == self.storage.visible_rows).then_some(undo.pre);
            self.storage.resize_undo = None;
            if let Some(pre) = back {
                self.set_cursor_picture(pre);
                self.storage.last_resize_shape.undone = true;
            }
            return;
        }
        let picture = self.cursor_picture();
        let (rows, generation) = (self.storage.visible_rows, self.storage.content_gen);
        if let Some(undo) = self.storage.resize_undo.as_mut() {
            undo.rows_after = rows;
            undo.seal_gen = generation;
            undo.after = picture;
        }
    }

    /// The Native rows-grow's half: pop up to `grow` entries (newest first).
    pub(super) fn take_undo_rows(&mut self, grow: usize, cols: u16) -> Vec<BottomRow> {
        let Some(undo) = self.storage.resize_undo.as_mut() else {
            return Vec::new();
        };
        let mut demoted = Vec::new();
        let mut bottom = Vec::new();
        for _ in 0..grow {
            match undo.entries.pop() {
                Some(UndoRow::Demoted(row, extras)) => demoted.push((row, extras)),
                Some(UndoRow::Pushed(row, extras)) => bottom.push(Some((row, extras))),
                Some(UndoRow::Trimmed) => bottom.push(None),
                None => break,
            }
        }
        if !demoted.is_empty() {
            debug_assert_eq!(
                self.storage.total_lines,
                usize::from(self.storage.visible_rows),
                "a resize undo is only kept on a grid with no history"
            );
            let storage = &mut self.storage;
            storage.linearize_ring();
            demoted.reverse();
            let pages = &mut storage.pages;
            let rows: Vec<_> = demoted
                .iter()
                // SAFETY: the rows are stored in the same `GridStorage` that owns `pages`, and rows
                // drop before the backing pages.
                .map(|(row, _)| unsafe { crate::Row::from_snapshot(row, cols, pages) })
                .collect();
            let n = rows.len();
            drop(storage.rows.splice(0..0, rows));
            storage.total_lines += n;
            storage.ring_extras.clear();
            storage
                .ring_extras
                .extend(demoted.into_iter().map(|(_, extras)| extras));
            storage.last_resize_shape.restored_top = crate::row_u16(n);
        }
        bottom
    }

    /// Forget the stash: the next grow appends blanks.
    #[inline]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ResizeRowReuse",
            action = "Output",
            project = "grid::tests::resize_row_reuse::resize_row_reuse_conforms"
        )
    )]
    pub fn drop_resize_undo(&mut self) {
        if self.storage.resize_undo.is_some() {
            self.storage.resize_undo = None;
        }
    }

    /// The open resize undo's serial (`None`: no undo).
    #[must_use]
    #[inline]
    pub fn resize_undo_serial(&self) -> Option<u64> {
        self.storage.resize_undo.as_ref().map(|u| u.serial)
    }

    /// How many rows with content a grow could hand back right now.
    #[must_use]
    pub fn resize_undo_rows(&self) -> usize {
        self.storage.resize_undo.as_ref().map_or(0, |u| {
            u.entries
                .iter()
                .filter(|e| !matches!(e, UndoRow::Trimmed))
                .count()
        })
    }
}

#[cfg(test)]
#[path = "resize_undo_tests.rs"]
mod tests;
