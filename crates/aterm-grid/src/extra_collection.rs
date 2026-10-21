// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell extras collection: HashMap-backed storage for grid-wide extras (the
//! per-cell type is [`crate::extra::CellExtra`]).
//!
//! Full-screen scroll (`shift_rows_up_by(0, n)`) is the dominant mutation, so
//! it is O(1): external rows map to internal keys by adding `row_offset`, a
//! scroll only bumps the offset and leaves scrolled-off entries in the map, and
//! once the offset reaches `COMPACT_THRESHOLD` rows one O(E) compaction drops
//! them and resets it.

use std::sync::Arc;

use aterm_hash::FxHashMap;

use crate::cell::Cell;
use crate::extra::{CellCoord, CellExtra};

/// Dense ring-buffer storage for non-BMP complex chars in the visible grid:
/// flat indexing instead of a HashMap probe for cells (emoji and the like) whose
/// only extra is `complex_char`.
///
/// Sized `visible_rows * cols`, with a row offset for O(1) full-screen scroll.
/// Stale entries are harmless: readers check `CellFlags::COMPLEX` first.
#[derive(Debug, Clone)]
pub(crate) struct ComplexCharRing {
    /// Flat array: `entries[ring_row * stride + col]`, raw `char` codepoints,
    /// `'\0'` = empty. No `Arc<str>` refcounting on the write path.
    entries: Vec<char>,
    /// Columns per row (for index calculation).
    stride: u16,
    /// Number of visible rows (ring capacity).
    visible_rows: u16,
    /// Ring offset: external row 0 maps to `ring_row = ring_offset`.
    /// Incremented on full-screen scroll to recycle the top row.
    ring_offset: u16,
}

impl ComplexCharRing {
    /// Create a new ring buffer for the visible grid.
    fn new(visible_rows: u16, cols: u16) -> Self {
        let capacity = (visible_rows as usize) * (cols as usize);
        Self {
            entries: vec!['\0'; capacity],
            stride: cols,
            visible_rows,
            ring_offset: 0,
        }
    }

    /// Flat index of (external row, col); callers ensure `col < stride`.
    #[inline(always)]
    fn index(&self, row: u16, col: u16) -> usize {
        debug_assert!(
            col < self.stride,
            "ComplexCharRing::index: col {col} >= stride {}",
            self.stride
        );
        (self.ring_row(row) as usize) * (self.stride as usize) + (col as usize)
    }

    /// The complex char codepoint at (row, col); `None` if `col >= stride`.
    #[inline]
    pub(crate) fn get(&self, row: u16, col: u16) -> Option<char> {
        if col >= self.stride {
            return None;
        }
        let idx = self.index(row, col);
        self.entries.get(idx).copied().filter(|&c| c != '\0')
    }

    /// Store a complex char codepoint at (row, col); no-op if `col >= stride`
    /// (stale dimensions after resize).
    #[inline]
    pub(crate) fn set(&mut self, row: u16, col: u16, value: char) {
        if col >= self.stride {
            return;
        }
        let idx = self.index(row, col);
        if let Some(entry) = self.entries.get_mut(idx) {
            *entry = value;
        }
    }

    /// Store consecutive wide-char codepoints on one row, at col, col+2, ...
    #[inline]
    #[cfg(test)]
    pub(crate) fn set_wide_run(&mut self, row: u16, start_col: u16, chars: &[char]) {
        if chars.is_empty() || start_col >= self.stride {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let last_idx = base + (start_col as usize) + (chars.len() - 1) * 2;
        if last_idx < self.entries.len() {
            // SAFETY: the last index is in bounds and indices grow with `j`,
            // so every earlier one is too.
            for (j, &c) in chars.iter().enumerate() {
                let idx = base + (start_col as usize) + j * 2;
                unsafe {
                    *self.entries.get_unchecked_mut(idx) = c;
                }
            }
        } else {
            let entries_len = self.entries.len();
            for (j, &c) in chars.iter().enumerate() {
                let idx = base + (start_col as usize) + j * 2;
                if idx < entries_len {
                    self.entries[idx] = c;
                }
            }
        }
    }

    /// Clear a rectangular `[rows] x [cols]` region, so a rect overwrite (e.g.
    /// DECCRA) cannot read the prior destination codepoint from this ring.
    pub(crate) fn clear_rect(&mut self, rows: core::ops::Range<u16>, cols: core::ops::Range<u16>) {
        for row in rows {
            self.clear_range(row, cols.start, cols.end);
        }
    }

    /// Region shift up by `n` within rows `[top, bottom]` (full-width).
    /// Mirrors `CellExtras::shift_region_up_by`.
    pub(crate) fn shift_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rect_up(top, bottom, 0, self.stride.saturating_sub(1), n);
    }

    /// Region shift down by `n` within rows `[top, bottom]` (full-width).
    /// Mirrors `CellExtras::shift_region_down_by`.
    pub(crate) fn shift_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rect_down(top, bottom, 0, self.stride.saturating_sub(1), n);
    }

    /// Column shift right (ICH) on one row. Mirrors `CellExtras::shift_cols_right`.
    pub(crate) fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let hi = max_col.min(self.stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        shift_right_in(&mut self.entries, base, start_col, count, hi, '\0');
    }

    /// Column shift left (DCH) on one row. Mirrors `CellExtras::shift_cols_left`.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let hi = max_col.min(self.stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        shift_left_in(&mut self.entries, base, start_col, count, hi, '\0');
    }
}

impl RingSpans for ComplexCharRing {
    #[inline(always)]
    fn visible_rows(&self) -> u16 {
        self.visible_rows
    }

    #[inline(always)]
    fn ring_offset(&self) -> u16 {
        self.ring_offset
    }

    #[inline(always)]
    fn set_ring_offset(&mut self, offset: u16) {
        self.ring_offset = offset;
    }

    /// Clear all entries.
    fn clear(&mut self) {
        self.entries.fill('\0');
        self.ring_offset = 0;
    }

    /// Clear a single external row's entries.
    #[inline]
    fn clear_row(&mut self, row: u16) {
        let stride = self.stride as usize;
        let start = (self.ring_row(row) as usize) * stride;
        let end = start + stride;
        if end <= self.entries.len() {
            self.entries[start..end].fill('\0');
        }
    }

    /// Copy a `[left, right]` column span from `src_row` to `dst_row`.
    #[inline]
    fn copy_row_span(&mut self, src_row: u16, dst_row: u16, left: u16, right: u16) {
        let hi = right.min(self.stride.saturating_sub(1));
        let src_base = (self.ring_row(src_row) as usize) * (self.stride as usize);
        let dst_base = (self.ring_row(dst_row) as usize) * (self.stride as usize);
        copy_span(&mut self.entries, src_base, dst_base, left, hi);
    }

    /// Clear the `[left, right_excl)` column span of one external row with one
    /// `fill`. `entries.len()` is `visible_rows * stride` for the ring's life, so
    /// the guard clears an in-range row whole and skips an out-of-range one.
    #[inline]
    fn clear_range(&mut self, row: u16, left: u16, right_excl: u16) {
        let end = right_excl.min(self.stride);
        if left >= end {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let (lo, hi) = (base + left as usize, base + end as usize);
        if hi <= self.entries.len() {
            self.entries[lo..hi].fill('\0');
        }
    }
}

/// Dense ring-buffer storage for cells whose only extra is fg/bg RGB (no
/// hyperlink, underline colour or extended flags), laid out like
/// `ComplexCharRing`.
///
/// Encoding: `0` = no colour, `0x01_RRGGBB` = RGB (as `PackedColor::rgb`), so
/// black is nonzero and `0` is an unambiguous sentinel.
#[derive(Debug, Clone)]
pub(crate) struct RgbColorRing {
    /// FG entries, `fg[ring_row * stride + col]`. `None` until a true-colour fg
    /// is stored: an absent plane reads as all-zero, so screens without
    /// true-colour foregrounds skip the `capacity * 4 B` allocation.
    fg: Option<Box<[u32]>>,
    /// BG entries, laid out and allocated like `fg`.
    bg: Option<Box<[u32]>>,
    /// Columns per row.
    stride: u16,
    /// Number of visible rows (ring capacity).
    visible_rows: u16,
    /// Ring offset: external row 0 maps to `ring_row = ring_offset`.
    ring_offset: u16,
}

impl RgbColorRing {
    fn new(visible_rows: u16, cols: u16) -> Self {
        Self {
            fg: None,
            bg: None,
            stride: cols,
            visible_rows,
            ring_offset: 0,
        }
    }

    /// Flat backing-array capacity (`visible_rows * stride`).
    #[inline(always)]
    fn capacity(&self) -> usize {
        (self.visible_rows as usize) * (self.stride as usize)
    }

    /// Borrow the fg plane mutably, allocating it zero-filled on first write.
    #[inline]
    fn fg_mut(&mut self) -> &mut [u32] {
        let cap = self.capacity();
        self.fg
            .get_or_insert_with(|| vec![0u32; cap].into_boxed_slice())
    }

    /// Borrow the bg plane mutably, allocating it zero-filled on first write.
    #[inline]
    fn bg_mut(&mut self) -> &mut [u32] {
        let cap = self.capacity();
        self.bg
            .get_or_insert_with(|| vec![0u32; cap].into_boxed_slice())
    }

    /// Heap bytes held by the (lazily allocated) fg/bg planes.
    #[inline]
    fn heap_bytes(&self) -> usize {
        let fg = self.fg.as_deref().map_or(0, |p| p.len().saturating_mul(4));
        let bg = self.bg.as_deref().map_or(0, |p| p.len().saturating_mul(4));
        fg.saturating_add(bg)
    }

    /// Flat index of (external row, col); callers ensure `col < stride`.
    #[inline(always)]
    fn index(&self, row: u16, col: u16) -> usize {
        debug_assert!(
            col < self.stride,
            "RgbColorRing::index: col {col} >= stride {}",
            self.stride
        );
        (self.ring_row(row) as usize) * (self.stride as usize) + (col as usize)
    }

    /// Read `plane`'s RGB at (row, col), if any.
    ///
    /// Returns `None` if `col >= stride` (stale dimensions after resize).
    #[inline]
    fn read(&self, plane: Option<&[u32]>, row: u16, col: u16) -> Option<[u8; 3]> {
        if col >= self.stride {
            return None;
        }
        // Absent plane == all-zero plane: no override stored.
        let packed = *plane?.get(self.index(row, col))?;
        (packed != 0).then_some([(packed >> 16) as u8, (packed >> 8) as u8, packed as u8])
    }

    /// Get fg RGB at (row, col), if any.
    #[inline]
    pub(crate) fn fg(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        self.read(self.fg.as_deref(), row, col)
    }

    /// Get bg RGB at (row, col), if any.
    #[inline]
    pub(crate) fn bg(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        self.read(self.bg.as_deref(), row, col)
    }

    /// Store fg RGB at (row, col); no-op if `col >= stride`.
    ///
    /// Test-only per-cell reference for [`Self::fill_runs`]. It packs inline
    /// rather than via `pack_rgb`, so the comparison checks independent code.
    #[cfg(test)]
    #[inline]
    pub(crate) fn set_fg(&mut self, row: u16, col: u16, rgb: [u8; 3]) {
        if col >= self.stride {
            return;
        }
        let idx = self.index(row, col);
        if let Some(entry) = self.fg_mut().get_mut(idx) {
            *entry =
                0x01_000000 | ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | (rgb[2] as u32);
        }
    }

    /// Store bg RGB at (row, col); test-only, see [`Self::set_fg`].
    #[cfg(test)]
    #[inline]
    pub(crate) fn set_bg(&mut self, row: u16, col: u16, rgb: [u8; 3]) {
        if col >= self.stride {
            return;
        }
        let idx = self.index(row, col);
        if let Some(entry) = self.bg_mut().get_mut(idx) {
            *entry =
                0x01_000000 | ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | (rgb[2] as u32);
        }
    }

    /// Pack `rgb` as `0x01RRGGBB` (the struct's encoding).
    #[inline(always)]
    fn pack_rgb(rgb: [u8; 3]) -> u32 {
        0x01_000000 | ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | (rgb[2] as u32)
    }

    /// Store fg and/or bg RGB across `[col_start, col_end)` of `row` with one
    /// `fill` per present plane, equivalent to `set_fg`/`set_bg` per column
    /// (`fill_run_matches_per_cell_set`). A plane is allocated only for a `Some`
    /// axis with an in-range run.
    #[inline]
    fn fill_runs(
        &mut self,
        row: u16,
        col_start: u16,
        col_end: u16,
        fg: Option<[u8; 3]>,
        bg: Option<[u8; 3]>,
    ) {
        let end = col_end.min(self.stride);
        if col_start >= end {
            return; // empty / wholly out-of-range run — no allocation (like set_fg)
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let (lo, hi) = (base + col_start as usize, base + end as usize);
        if let Some(rgb) = fg {
            let plane = self.fg_mut();
            if hi <= plane.len() {
                plane[lo..hi].fill(Self::pack_rgb(rgb));
            }
        }
        if let Some(rgb) = bg {
            let plane = self.bg_mut();
            if hi <= plane.len() {
                plane[lo..hi].fill(Self::pack_rgb(rgb));
            }
        }
    }

    /// Clear RGB entries for a range of rows via `clear_row`.
    #[inline]
    pub(crate) fn clear_rows(&mut self, rows: core::ops::Range<u16>) {
        for row in rows {
            self.clear_row(row);
        }
    }

    /// Clear RGB entries inside a rectangular area via `clear_range` per row.
    pub(crate) fn clear_rect(&mut self, rows: core::ops::Range<u16>, cols: core::ops::Range<u16>) {
        for row in rows {
            self.clear_range(row, cols.start, cols.end);
        }
    }

    /// Region shift up by `n` within rows `[top, bottom]` (full-width).
    /// Mirrors `CellExtras::shift_region_up_by`.
    pub(crate) fn shift_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rect_up(top, bottom, 0, self.stride.saturating_sub(1), n);
    }

    /// Region shift down by `n` within rows `[top, bottom]` (full-width).
    /// Mirrors `CellExtras::shift_region_down_by`.
    pub(crate) fn shift_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rect_down(top, bottom, 0, self.stride.saturating_sub(1), n);
    }

    /// Column shift right (ICH) on one row. Mirrors `CellExtras::shift_cols_right`.
    pub(crate) fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let hi = max_col.min(self.stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        // An absent plane is all-zero, so skip it.
        if let Some(plane) = self.fg.as_deref_mut() {
            shift_right_in(plane, base, start_col, count, hi, 0);
        }
        if let Some(plane) = self.bg.as_deref_mut() {
            shift_right_in(plane, base, start_col, count, hi, 0);
        }
    }

    /// Column shift left (DCH) on one row. Mirrors `CellExtras::shift_cols_left`.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let hi = max_col.min(self.stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        if let Some(plane) = self.fg.as_deref_mut() {
            shift_left_in(plane, base, start_col, count, hi, 0);
        }
        if let Some(plane) = self.bg.as_deref_mut() {
            shift_left_in(plane, base, start_col, count, hi, 0);
        }
    }
}

impl RingSpans for RgbColorRing {
    #[inline(always)]
    fn visible_rows(&self) -> u16 {
        self.visible_rows
    }

    #[inline(always)]
    fn ring_offset(&self) -> u16 {
        self.ring_offset
    }

    #[inline(always)]
    fn set_ring_offset(&mut self, offset: u16) {
        self.ring_offset = offset;
    }

    /// Clear all entries.
    fn clear(&mut self) {
        if let Some(fg) = self.fg.as_deref_mut() {
            fg.fill(0);
        }
        if let Some(bg) = self.bg.as_deref_mut() {
            bg.fill(0);
        }
        self.ring_offset = 0;
    }

    /// Clear one external row's entries.
    #[inline]
    fn clear_row(&mut self, row: u16) {
        let ring_row = self.ring_row(row);
        let stride = self.stride as usize;
        let start = (ring_row as usize) * stride;
        let end = start + stride;
        // Clearing an absent plane is a no-op (already all-zero).
        if let Some(fg) = self.fg.as_deref_mut()
            && end <= fg.len()
        {
            fg[start..end].fill(0);
        }
        if let Some(bg) = self.bg.as_deref_mut()
            && end <= bg.len()
        {
            bg[start..end].fill(0);
        }
    }

    /// Copy a `[left, right]` column span from `src_row` to `dst_row`; the
    /// caller clears vacated rows.
    #[inline]
    fn copy_row_span(&mut self, src_row: u16, dst_row: u16, left: u16, right: u16) {
        let hi = right.min(self.stride.saturating_sub(1));
        let src_base = (self.ring_row(src_row) as usize) * (self.stride as usize);
        let dst_base = (self.ring_row(dst_row) as usize) * (self.stride as usize);
        if let Some(plane) = self.fg.as_deref_mut() {
            copy_span(plane, src_base, dst_base, left, hi);
        }
        if let Some(plane) = self.bg.as_deref_mut() {
            copy_span(plane, src_base, dst_base, left, hi);
        }
    }

    /// Clear RGB entries for a column range on a single row, so erased cells do
    /// not keep ring RGB that would shadow their default colors. Each plane holds
    /// `visible_rows * stride` entries for the ring's life (resize drops it via
    /// `invalidate_rings`), so the `hi <= plane.len()` guard admits an in-range
    /// row whole and rejects an out-of-range one.
    #[inline]
    fn clear_range(&mut self, row: u16, start_col: u16, end_col: u16) {
        let end = end_col.min(self.stride);
        if start_col >= end {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let (lo, hi) = (base + start_col as usize, base + end as usize);
        // Clearing an absent plane is a no-op: use `as_deref_mut`, never
        // `fg_mut()`/`bg_mut()`, which would allocate the plane on every EL of
        // an uncoloured screen.
        if let Some(fg) = self.fg.as_deref_mut()
            && hi <= fg.len()
        {
            fg[lo..hi].fill(0);
        }
        if let Some(bg) = self.bg.as_deref_mut()
            && hi <= bg.len()
        {
            bg[lo..hi].fill(0);
        }
    }
}

/// Row-span primitives both dense rings provide, so the rectangular shifts built
/// on them — and the full-width region shifts, which are the rect shift over
/// `[0, stride - 1]` — are written once.
trait RingSpans {
    /// Number of visible rows (ring capacity).
    fn visible_rows(&self) -> u16;

    /// Ring offset: external row 0 maps to `ring_row = ring_offset`.
    fn ring_offset(&self) -> u16;

    /// Move the ring offset (full-screen scroll recycles the top rows).
    fn set_ring_offset(&mut self, offset: u16);

    /// Clear all entries and reset the offset.
    fn clear(&mut self);

    /// Clear a single external row's entries.
    fn clear_row(&mut self, row: u16);

    /// Map an external row to its ring index.
    #[inline(always)]
    fn ring_row(&self, external_row: u16) -> u16 {
        let r = external_row.wrapping_add(self.ring_offset());
        if r >= self.visible_rows() {
            r - self.visible_rows()
        } else {
            r
        }
    }

    /// Scroll up by `n` rows: recycle the top `n` rows by clearing them.
    fn scroll_up(&mut self, n: u16) {
        // Over-scroll: when the whole visible area (or more) scrolls off, the
        // ring is fully recycled and the offset resets to 0. `ring_row` only
        // normalizes inputs < 2*visible_rows, so a huge `n` (e.g. `CSI 65535 S`)
        // would otherwise leave `ring_offset >= visible_rows`.
        if n >= self.visible_rows() {
            self.clear();
            return;
        }
        for i in 0..n {
            self.clear_row(i);
        }
        let offset = self.ring_row(n);
        self.set_ring_offset(offset);
    }

    /// Copy a `[left, right]` column span from `src_row` to `dst_row`.
    fn copy_row_span(&mut self, src_row: u16, dst_row: u16, left: u16, right: u16);

    /// Clear the `[left, right_excl)` column span of one external row.
    fn clear_range(&mut self, row: u16, left: u16, right_excl: u16);

    /// Rect shift up by `n` within rows `[top, bottom]` × cols `[left, right]`
    /// (DECLRMM). Mirrors `CellExtras::shift_rect_up_by`.
    fn shift_rect_up(&mut self, top: u16, bottom: u16, left: u16, right: u16, n: u16) {
        if n == 0 || top > bottom || left > right {
            return;
        }
        let n = n.min(bottom - top + 1);
        // Move surviving rows up: dst in [top, bottom-n], src = dst+n.
        if let Some(shift_start) = top.checked_add(n) {
            let mut dst = top;
            let mut src = shift_start;
            while src <= bottom {
                self.copy_row_span(src, dst, left, right);
                dst += 1;
                src += 1;
            }
        }
        // Clear the n vacated rows at the bottom of the region.
        for row in (bottom + 1 - n)..=bottom {
            self.clear_range(row, left, right.saturating_add(1));
        }
    }

    /// Rect shift down by `n` within rows `[top, bottom]` × cols `[left, right]`
    /// (DECLRMM). Mirrors `CellExtras::shift_rect_down_by`.
    fn shift_rect_down(&mut self, top: u16, bottom: u16, left: u16, right: u16, n: u16) {
        if n == 0 || top > bottom || left > right {
            return;
        }
        let n = n.min(bottom - top + 1);
        // Move surviving rows down, iterating from the bottom to avoid clobber:
        // src in [top, bottom-n], dst = src+n.
        let mut src = bottom + 1 - n;
        while src > top {
            src -= 1;
            let dst = src + n;
            if dst <= bottom {
                self.copy_row_span(src, dst, left, right);
            }
        }
        // Clear the n vacated rows at the top of the region.
        let clear_to = (top + n).min(bottom + 1);
        for row in top..clear_to {
            self.clear_range(row, left, right.saturating_add(1));
        }
    }
}

/// Copy columns `[left, hi]` of the row at `src_base` onto the row at `dst_base`
/// with one `copy_within`; out-of-range dimensions skip the copy, never panic.
#[inline]
fn copy_span<T: Copy>(plane: &mut [T], src_base: usize, dst_base: usize, left: u16, hi: u16) {
    if left > hi {
        return;
    }
    let span = (hi - left) as usize + 1;
    let s0 = src_base + left as usize;
    let d0 = dst_base + left as usize;
    if s0 + span <= plane.len() && d0 + span <= plane.len() {
        plane.copy_within(s0..s0 + span, d0);
    }
}

/// ICH on one ring row, `start < hi`: columns `[start, hi - count)` move right by
/// `count`, `[start, start + count)` become `blank`, and what passes `hi` drops.
///
/// One memmove plus memset. `len == visible_rows * stride`, `base` is a whole
/// row times `stride` and `hi <= stride`, so the `base + hi <= len` guard makes
/// a row wholly in range or wholly skipped.
#[inline]
fn shift_right_in<T: Copy>(
    plane: &mut [T],
    base: usize,
    start: u16,
    count: u16,
    hi: u16,
    blank: T,
) {
    let (s, h) = (base + start as usize, base + hi as usize);
    if h > plane.len() {
        return;
    }
    let split = (s + count as usize).min(h);
    plane.copy_within(s..h - (split - s), split);
    plane[s..split].fill(blank);
}

/// DCH on one ring row, `start < hi`: columns `[start + count, hi)` move left by
/// `count` and the last `count` columns before `hi` become `blank`. Same
/// memmove + memset and bounds argument as [`shift_right_in`].
#[inline]
fn shift_left_in<T: Copy>(plane: &mut [T], base: usize, start: u16, count: u16, hi: u16, blank: T) {
    let (s, h) = (base + start as usize, base + hi as usize);
    if h > plane.len() {
        return;
    }
    let keep_end = h - (count as usize).min(h - s);
    plane.copy_within(s + (h - keep_end)..h, s);
    plane[keep_end..h].fill(blank);
}

/// Compact after this many accumulated scroll rows: rare enough to amortize to
/// O(1) per scroll, small enough to bound stale-entry memory and keep
/// `row_offset` plus the screen height clear of u16 overflow.
const COMPACT_THRESHOLD: u16 = 256;

/// Maximum number of hyperlink-bearing entries allowed in `CellExtras`.
///
/// Past this limit the oldest hyperlink-bearing entries are evicted, bounding
/// memory under OSC 8 spam. Affected cells keep their text; only the link is
/// removed. 10,000 covers a fully linked 200x50 terminal with headroom for
/// stale entries awaiting compaction.
pub(crate) const MAX_HYPERLINK_ENTRIES: usize = 10_000;

/// Per-cell extras snapshot for render/FFI hot paths: one accessor over the
/// rings and the map, so callers do not re-probe each layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct CellRenderData<'a> {
    extra: Option<&'a CellExtra>,
    complex_char: Option<char>,
    fg_rgb: Option<[u8; 3]>,
    bg_rgb: Option<[u8; 3]>,
}

impl<'a> CellRenderData<'a> {
    /// Get the cell's `CellExtra` entry when the HAS_EXTRAS flag requested it.
    #[must_use]
    #[inline]
    pub fn cell_extra(&self) -> Option<&'a CellExtra> {
        self.extra
    }

    /// Get the base codepoint for a complex cell.
    #[must_use]
    #[inline]
    pub fn complex_char(&self) -> Option<char> {
        self.complex_char
    }

    /// Get the foreground RGB overflow color for the cell.
    #[must_use]
    #[inline]
    pub fn fg_rgb(&self) -> Option<[u8; 3]> {
        self.fg_rgb
    }

    /// Get the background RGB overflow color for the cell.
    #[must_use]
    #[inline]
    pub fn bg_rgb(&self) -> Option<[u8; 3]> {
        self.bg_rgb
    }
}

/// Storage for cell extras across the grid.
///
/// FxHashMap keyed by (row, col); most cells have no extras, so this beats
/// storing extras inline. Row coordinates are offset by `row_offset` for O(1)
/// scroll amortization (see module docs).
#[derive(Debug, Clone, Default)]
pub struct CellExtras {
    /// Map from cell coordinate to extra data. Keys are internal coordinates
    /// (external row `r` is `r.wrapping_add(row_offset)`): go through the
    /// accessors, or call `compact()` first so the offset is 0.
    pub(crate) data: FxHashMap<CellCoord, CellExtra>,
    /// Accumulated row offset for O(1) full-screen scroll.
    row_offset: u16,
    /// Dense ring for non-BMP complex chars, allocated on first non-BMP write.
    /// Cells that also have other extras use the map.
    complex_ring: Option<Box<ComplexCharRing>>,
    /// Dense ring for cells whose only extra is fg/bg RGB, allocated on first
    /// such write.
    rgb_ring: Option<Box<RgbColorRing>>,
    /// Scratch for [`Self::enforce_hyperlink_limit_cold`]'s candidate walk:
    /// empty between calls, it keeps its capacity so the cold path does not
    /// allocate. Never shrunk: it grows large only where the map dwarfs it.
    hyperlink_scratch: Vec<CellCoord>,
    /// Upper bound on the number of hyperlink-bearing entries in `data`, which
    /// guards [`Self::enforce_hyperlink_limit`]: `data.len()` also counts marks,
    /// colours, flags and images, so guarding on it would walk the whole map on
    /// every linked write once anything else filled it.
    ///
    /// Invariant: `hyperlink_upper_bound >= (entries whose hyperlink is Some)`.
    /// Every path that can raise the population charges it (`get_or_create`,
    /// `fresh`, `set`, `set_range_uniform`); a missed charge loses the memory
    /// bound. Paths that lower it need not report: the cold walk resets it to the
    /// exact count.
    hyperlink_upper_bound: usize,
    /// Sticky: an inline image has been placed here at some point; reset only by
    /// [`Self::clear`]. Scroll-off must scan the full row width for image refs
    /// (an image-only row has `Row::len() == 0`), and this gate spares sessions
    /// that never place an image that scan.
    any_image: bool,
}

/// The empty entry a writer's new cell takes at internal key `key`: the one
/// rule [`CellExtras::fresh`] and [`CellExtras::set_range_uniform`]'s map path
/// write a cell's map extras by, so such a write never adopts an orphan's mark
/// or link (the ring path writes no entry; see `set_range_uniform`).
///
/// Returns the entry and whether the one it replaced carried a link, which the
/// caller refunds. Only an occupied slot is reset; a vacant slot is a plain
/// insert (resetting every slot cost +8% instructions on densely linked text).
#[inline]
fn fresh_slot(
    data: &mut FxHashMap<CellCoord, CellExtra>,
    key: CellCoord,
) -> (&mut CellExtra, bool) {
    use std::collections::hash_map::Entry;
    match data.entry(key) {
        Entry::Vacant(slot) => (slot.insert(CellExtra::default()), false),
        Entry::Occupied(slot) => {
            let extra = slot.into_mut();
            let had_link = extra.hyperlink().is_some();
            *extra = CellExtra::default();
            (extra, had_link)
        }
    }
}

impl CellExtras {
    /// Create empty extras storage.
    #[must_use]
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            data: FxHashMap::default(),
            row_offset: 0,
            complex_ring: None,
            rgb_ring: None,
            hyperlink_scratch: Vec::new(),
            hyperlink_upper_bound: 0,
            any_image: false,
        }
    }

    /// Store a complex char codepoint in the dense ring buffer, allocating the
    /// ring on first use; no `Arc<str>` allocation or refcounting.
    #[inline]
    pub fn set_complex_char_ring(
        &mut self,
        row: u16,
        col: u16,
        value: char,
        visible_rows: u16,
        cols: u16,
    ) {
        let ring = self
            .complex_ring
            .get_or_insert_with(|| Box::new(ComplexCharRing::new(visible_rows, cols)));
        ring.set(row, col, value);
    }

    /// Store a run of wide-char codepoints on the same row.
    #[inline]
    #[cfg(test)]
    pub fn set_complex_char_wide_run(
        &mut self,
        row: u16,
        start_col: u16,
        chars: &[char],
        visible_rows: u16,
        cols: u16,
    ) {
        let ring = self
            .complex_ring
            .get_or_insert_with(|| Box::new(ComplexCharRing::new(visible_rows, cols)));
        ring.set_wide_run(row, start_col, chars);
    }

    /// Store the non-BMP chars (> U+FFFF) of a mixed BMP/non-BMP wide-char run.
    #[inline]
    pub fn set_mixed_wide_ring(
        &mut self,
        row: u16,
        start_col: u16,
        chars: &[char],
        visible_rows: u16,
        cols: u16,
    ) {
        let ring = self
            .complex_ring
            .get_or_insert_with(|| Box::new(ComplexCharRing::new(visible_rows, cols)));
        if start_col >= ring.stride {
            return;
        }
        let base = (ring.ring_row(row) as usize) * (ring.stride as usize);
        for (j, &c) in chars.iter().enumerate() {
            if (c as u32) > 0xFFFF
                && let Some(e) = ring.entries.get_mut(base + (start_col as usize) + j * 2)
            {
                *e = c;
            }
        }
    }

    /// Look up a complex char codepoint: ring first, then the map (cells whose
    /// complex char shares an entry with other extras).
    #[inline]
    pub fn complex_codepoint_for(&self, row: u16, col: u16) -> Option<char> {
        if let Some(ring) = &self.complex_ring
            && let Some(c) = ring.get(row, col)
        {
            return Some(c);
        }
        self.data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.complex_char())
            .and_then(|s| s.chars().next())
    }

    /// Look up a complex char as `Arc<str>`: map only (the ring stores
    /// codepoints). Callers that need only the codepoint use
    /// `complex_codepoint_for`.
    #[inline]
    pub fn complex_char_arc_for(&self, row: u16, col: u16) -> Option<&Arc<str>> {
        self.data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.complex_char())
    }

    /// Append a complex char's full text to `out`: the ring's codepoint, else
    /// the HashMap's string (a multi-scalar cluster). Returns whether either
    /// held one; `false` appends nothing.
    ///
    /// Used by text extraction (row_text, content export, the styled frame),
    /// which needs the complete string. It borrows the stored text, so a
    /// complex cell costs no allocation of its own.
    pub fn push_complex_char(&self, row: u16, col: u16, out: &mut String) -> bool {
        if let Some(ring) = &self.complex_ring
            && let Some(c) = ring.get(row, col)
        {
            out.push(c);
            return true;
        }
        match self
            .data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.complex_char())
        {
            Some(s) => {
                out.push_str(s);
                true
            }
            None => false,
        }
    }

    /// Store RGB fg/bg in the dense ring buffer for a column range, allocating
    /// the ring on first use. For RGB-only extras (no hyperlink, no underline).
    #[allow(
        clippy::too_many_arguments,
        reason = "all params are semantically distinct"
    )]
    pub(crate) fn set_rgb_ring_range(
        &mut self,
        row: u16,
        col_start: u16,
        col_end: u16,
        fg_rgb: Option<[u8; 3]>,
        bg_rgb: Option<[u8; 3]>,
        visible_rows: u16,
        cols: u16,
    ) {
        let ring = self
            .rgb_ring
            .get_or_insert_with(|| Box::new(RgbColorRing::new(visible_rows, cols)));
        ring.fill_runs(row, col_start, col_end, fg_rgb, bg_rgb);
    }

    /// Clear the dense RGB-ring fg/bg entries for a `width`-cell span starting
    /// at `(row, col)`, if a ring exists.
    ///
    /// The per-character write-split path stores the new cell's RGB in the map,
    /// but reads consult the ring first, so a leftover ring entry would shadow
    /// the new color.
    #[inline]
    pub(crate) fn clear_rgb_ring_cell(&mut self, row: u16, col: u16, width: u16) {
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_range(row, col, col.saturating_add(width));
        }
    }

    /// Look up fg RGB: ring buffer first (O(1)), then HashMap.
    #[inline]
    pub fn fg_rgb_for(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        if let Some(ring) = &self.rgb_ring
            && let Some(rgb) = ring.fg(row, col)
        {
            return Some(rgb);
        }
        self.data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.fg_rgb())
    }

    /// Look up bg RGB: ring buffer first (O(1)), then HashMap.
    #[inline]
    pub fn bg_rgb_for(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        if let Some(ring) = &self.rgb_ring
            && let Some(rgb) = ring.bg(row, col)
        {
            return Some(rgb);
        }
        self.data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.bg_rgb())
    }

    /// Read all render-relevant extras for a cell in one pass.
    ///
    /// The cell's flags pick the layers: `HAS_EXTRAS` the `CellExtra`, `COMPLEX`
    /// the complex char, the RGB overflow flags fg/bg. Ring hits stay on the
    /// flat-array path; a fallback costs at most one map probe.
    #[must_use]
    #[inline]
    pub fn render_data_for_cell(&self, row: u16, col: u16, cell: Cell) -> CellRenderData<'_> {
        let wants_extra = cell.has_extras();
        let wants_complex = cell.is_complex();
        let wants_fg_rgb = cell.fg_needs_overflow();
        let wants_bg_rgb = cell.bg_needs_overflow();

        let mut data = CellRenderData::default();
        if !(wants_extra || wants_complex || wants_fg_rgb || wants_bg_rgb) {
            return data;
        }

        if wants_complex && let Some(ring) = &self.complex_ring {
            data.complex_char = ring.get(row, col);
        }

        if let Some(ring) = &self.rgb_ring {
            if wants_fg_rgb {
                data.fg_rgb = ring.fg(row, col);
            }
            if wants_bg_rgb {
                data.bg_rgb = ring.bg(row, col);
            }
        }

        let needs_map = wants_extra
            || (wants_complex && data.complex_char.is_none())
            || (wants_fg_rgb && data.fg_rgb.is_none())
            || (wants_bg_rgb && data.bg_rgb.is_none());
        if !needs_map || self.data.is_empty() {
            return data;
        }

        let coord = self.internal_coord(CellCoord::new(row, col));
        let extra = self.data.get(&coord);

        if wants_extra {
            data.extra = extra;
        }

        if let Some(extra) = extra {
            if wants_complex && data.complex_char.is_none() {
                data.complex_char = extra.complex_char().and_then(|s| s.chars().next());
            }
            if wants_fg_rgb && data.fg_rgb.is_none() {
                data.fg_rgb = extra.fg_rgb();
            }
            if wants_bg_rgb && data.bg_rgb.is_none() {
                data.bg_rgb = extra.bg_rgb();
            }
        }

        data
    }

    /// Translate an external row to the internal HashMap key row.
    #[inline(always)]
    pub(crate) fn internal_row(&self, row: u16) -> u16 {
        row.wrapping_add(self.row_offset)
    }

    /// Translate an external coordinate to the internal HashMap key.
    #[inline(always)]
    fn internal_coord(&self, coord: CellCoord) -> CellCoord {
        CellCoord::new(self.internal_row(coord.row), coord.col)
    }

    /// Returns `true` when the map holds no entry, so the FFI render path can
    /// skip per-cell probes. Conservative: stale (scrolled-off) entries make it
    /// `false`. The rings are not checked; see [`Self::has_any_data`].
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns `true` when any storage layer (map, complex ring or RGB ring)
    /// holds data, unlike [`Self::is_empty`], which sees only the map.
    #[must_use]
    #[inline]
    pub fn has_any_data(&self) -> bool {
        !self.data.is_empty() || self.complex_ring.is_some() || self.rgb_ring.is_some()
    }

    /// Iterate over all valid (non-stale) (coord, extra) pairs, in external
    /// coordinates.
    pub fn iter(&self) -> impl Iterator<Item = (CellCoord, &CellExtra)> + '_ {
        let offset = self.row_offset;
        self.data.iter().filter_map(move |(coord, extra)| {
            // Stale entries (scrolled off) have internal row < offset.
            if offset > 0 && coord.row < offset {
                return None;
            }
            let external = CellCoord::new(coord.row.wrapping_sub(offset), coord.col);
            Some((external, extra))
        })
    }

    /// Whether the map may hold an entry on external row `row`. `false` is
    /// exact (every [`Self::get`] on that row returns `None`); `true` means
    /// it does, or that the map holds more than `scan_budget` entries and was
    /// not scanned. A caller about to probe `scan_budget` cells of one row
    /// passes that count, so the scan never costs more than the probes it can
    /// save.
    #[must_use]
    pub(crate) fn map_may_have_row(&self, row: u16, scan_budget: usize) -> bool {
        if self.data.len() > scan_budget {
            return true;
        }
        let key = self.internal_row(row);
        self.data.keys().any(|coord| coord.row == key)
    }

    /// Get extras for a cell, if any.
    #[must_use]
    #[inline]
    pub fn get(&self, coord: CellCoord) -> Option<&CellExtra> {
        self.data.get(&self.internal_coord(coord))
    }

    /// Get mutable extras for a cell, creating if needed.
    ///
    /// Charges the hyperlink budget unless the entry already carries a link: a
    /// caller (checkpoint restore in `scroll_fill`, for one) may write a link
    /// through the `&mut`, which cannot report back. Over-charging costs at most
    /// one cold walk (which repairs the bound); under-charging loses the memory
    /// bound.
    #[inline]
    pub fn get_or_create(&mut self, coord: CellCoord) -> &mut CellExtra {
        let key = self.internal_coord(coord);
        // Disjoint field borrows: the counter and the map are separate fields,
        // so the charge lands without a second lookup.
        let bound = &mut self.hyperlink_upper_bound;
        let extra = self.data.entry(key).or_default();
        if extra.hyperlink().is_none() {
            *bound += 1;
        }
        extra
    }

    /// A fresh entry for a cell a writer has just written, replacing whatever
    /// entry is keyed to the coordinate.
    ///
    /// Writers drop the entry of every HAS_EXTRAS cell they overwrite
    /// (`grid::write::StaleExtras`), so an entry still keyed here is an orphan;
    /// merging into it, as [`Self::get_or_create`] would, revives an old
    /// combining mark, skin tone or link on the new glyph. Every write that
    /// creates an entry (this and [`Self::set_range_uniform`]'s map path) starts
    /// fresh.
    ///
    /// Charges the hyperlink budget as `get_or_create` does and refunds the
    /// replaced entry's link, as [`Self::remove`] does.
    #[inline]
    pub fn fresh(&mut self, coord: CellCoord) -> &mut CellExtra {
        let key = self.internal_coord(coord);
        let bound = &mut self.hyperlink_upper_bound;
        let (extra, had_link) = fresh_slot(&mut self.data, key);
        if had_link {
            *bound = bound.saturating_sub(1);
        }
        *bound += 1;
        extra
    }

    /// Whether an inline image has ever been placed here (see the
    /// [`any_image`](Self::any_image) field). `false` proves no cell carries an
    /// image ref; `true` merely permits the scroll-off scan.
    #[must_use]
    #[inline]
    pub fn any_image(&self) -> bool {
        self.any_image
    }

    /// Attach an inline-image ref to a cell, arming the scroll-off scan gate.
    ///
    /// The one entry point for placing a picture: `CellExtra::set_image` on a
    /// `&mut` from `get_or_create` could not arm the gate, leaving the ref
    /// invisible to the scroll-off extraction that carries it into history.
    #[inline]
    pub fn set_image(&mut self, coord: CellCoord, image: crate::extra::ImageRef) {
        self.any_image = true;
        self.get_or_create(coord).set_image(Some(image));
    }

    /// [`set_image`](Self::set_image)'s layering twin: add `image` to the
    /// cell's layers ([`CellExtra::stack_image`]) instead of replacing them.
    #[inline]
    pub fn stack_image(&mut self, coord: CellCoord, image: crate::extra::ImageRef) {
        self.any_image = true;
        self.get_or_create(coord).stack_image(image);
    }

    /// Set extras for a cell; an extra with no data removes the entry.
    #[inline]
    pub fn set(&mut self, coord: CellCoord, extra: CellExtra) {
        let internal = self.internal_coord(coord);
        if extra.has_data() {
            // Charge only for a value that carries a link: reflow and DECCRA's
            // rect copy also store link-free extras here.
            if extra.hyperlink().is_some() {
                self.hyperlink_upper_bound += 1;
            }
            // Arm the scroll-off image scan here too: reflow and DECCRA's rect
            // copy re-enter whole `CellExtra` values, which may carry a picture.
            if extra.image().is_some() {
                self.any_image = true;
            }
            self.data.insert(internal, extra);
        } else {
            self.data.remove(&internal);
        }
    }

    /// Remove extras for a single cell; `true` if an entry was removed.
    #[inline]
    pub(crate) fn remove(&mut self, coord: CellCoord) -> bool {
        let key = self.internal_coord(coord);
        let Some(extra) = self.data.remove(&key) else {
            return false;
        };
        // Refund the charge: on the OSC 8 overwrite path (`write_split` drops a
        // cell's stale entry before rewriting it), a screen repainting links in
        // place must not inflate the bound.
        if extra.hyperlink().is_some() {
            self.hyperlink_upper_bound = self.hyperlink_upper_bound.saturating_sub(1);
        }
        true
    }

    /// Strip what a cell without HAS_EXTRAS cannot own from its entry
    /// ([`CellExtra::retain_overflow_colors`]): everything but truecolor fg/bg
    /// bytes, which a BCE blank owns through its RGB marker. The entry goes
    /// when nothing is left. Refunds a dropped link, as [`Self::remove`] does.
    pub(crate) fn strip_unclaimed(&mut self, coord: CellCoord) {
        let key = self.internal_coord(coord);
        let Some(extra) = self.data.get_mut(&key) else {
            return;
        };
        if extra.has_claimable_data() {
            if extra.hyperlink().is_some() {
                self.hyperlink_upper_bound = self.hyperlink_upper_bound.saturating_sub(1);
            }
            if extra.retain_overflow_colors() {
                return;
            }
        } else if extra.has_data() {
            // Truecolor bytes alone: a BCE blank's, kept.
            return;
        }
        // Nothing left, or nothing ever (the extras run hands every written
        // cell an entry, empty when its style's extras carry no value).
        self.data.remove(&key);
    }

    /// Strip a cell's combining marks and complex-char string, keeping SGR colors
    /// and every other overflow field (DECSERA's selective wipe).
    ///
    /// Returns `true` when the entry is left empty and the caller should drop it
    /// (and clear the cell's HAS_EXTRAS flag, keeping flag ⇔ entry); `false` when
    /// there is no entry or data remains.
    pub(crate) fn scrub_char_marks(&mut self, row: u16, col: u16) -> bool {
        let coord = self.internal_coord(CellCoord::new(row, col));
        let Some(extra) = self.data.get_mut(&coord) else {
            return false;
        };
        extra.clear_char_marks();
        !extra.has_data()
    }

    /// Clear extras for a single row (cleared or scrolled off), including the RGB
    /// ring.
    pub(crate) fn clear_row(&mut self, row: u16) {
        // The RGB ring first: RGB-only writes bypass the map, so it may hold
        // data even when the map is empty.
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_row(row);
        }
        if self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let internal_row = self.internal_row(row);
        self.data.retain(|coord, _| coord.row != internal_row);
        self.maybe_shrink();
    }

    /// Clear extras for a range of columns in a row, including the RGB ring.
    pub fn clear_range(&mut self, row: u16, start_col: u16, end_col: u16) {
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_range(row, start_col, end_col);
        }
        if self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let internal_row = self.internal_row(row);
        // Delete the range's keys directly (O(range) probes) unless the map is
        // smaller than the range, where the scan is cheaper. `clear_row` keeps
        // its retain: it has no column bound and must reach stale entries past
        // the current width.
        let width = usize::from(end_col.saturating_sub(start_col));
        if width.saturating_mul(2) <= self.data.len() {
            for col in start_col..end_col {
                self.data.remove(&CellCoord::new(internal_row, col));
            }
        } else {
            self.data.retain(|coord, _| {
                !(coord.row == internal_row && coord.col >= start_col && coord.col < end_col)
            });
        }
        self.maybe_shrink();
    }

    /// Clear extras for a range of rows in a single pass, including the RGB
    /// ring.
    pub(crate) fn clear_rows(&mut self, rows: core::ops::Range<u16>) {
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_rows(rows.clone());
        }
        if rows.is_empty() || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let offset = self.row_offset;
        self.data.retain(|coord, _| {
            let external = coord.row.wrapping_sub(offset);
            !rows.contains(&external)
        });
        self.maybe_shrink();
    }

    /// Clear extras inside a rectangular area in one pass, including both rings,
    /// which the fast write paths fill directly (a DECCRA overwrite must wipe
    /// them).
    pub(crate) fn clear_rect(&mut self, rows: core::ops::Range<u16>, cols: core::ops::Range<u16>) {
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_rect(rows.clone(), cols.clone());
        }
        if let Some(ring) = &mut self.complex_ring {
            ring.clear_rect(rows.clone(), cols.clone());
        }
        if rows.is_empty() || cols.is_empty() || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let offset = self.row_offset;
        self.data.retain(|coord, _| {
            let external_row = coord.row.wrapping_sub(offset);
            !(rows.contains(&external_row) && cols.contains(&coord.col))
        });
        self.maybe_shrink();
    }

    /// Shift rows up by `n` (multi-line scroll): rows `[start_row, start_row + n)`
    /// drop, rows below move up by `n`, rows above stay. A full-screen scroll
    /// (`start_row == 0`) only bumps `row_offset` until it reaches
    /// `COMPACT_THRESHOLD`; otherwise the map is compacted and rebuilt.
    pub fn shift_rows_up_by(&mut self, start_row: u16, n: u16) {
        if let Some(ring) = &mut self.complex_ring {
            if start_row == 0 {
                ring.scroll_up(n);
            } else {
                ring.clear();
            }
        }
        if let Some(ring) = &mut self.rgb_ring {
            if start_row == 0 {
                ring.scroll_up(n);
            } else {
                ring.clear();
            }
        }
        if n == 0 || self.data.is_empty() {
            return;
        }
        if start_row == 0 {
            // O(1) fast path for full-screen scroll.
            if let Some(new_offset) = self.row_offset.checked_add(n)
                && new_offset < COMPACT_THRESHOLD
            {
                #[cfg(any(test, debug_assertions))]
                crate::test_counters::count_extras_shift_ops(0);
                self.row_offset = new_offset;
                return;
            }
            // Threshold exceeded or u16 overflow: compact + shift in one pass.
            self.apply_offset_and_shift(n);
            return;
        }
        // Partial scroll: compact (offset to 0), then drain-rebuild.
        self.compact();
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let shift_start = start_row.checked_add(n);
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < start_row {
                new_data.insert(coord, extra);
                continue;
            }
            if let Some(shift_start) = shift_start
                && coord.row >= shift_start
            {
                new_data.insert(
                    CellCoord::new(coord.row.saturating_sub(n), coord.col),
                    extra,
                );
            }
        }
        self.data = new_data;
    }

    // Region shifts (shift_region_up_by, shift_region_down_by) and column
    // shifts (shift_cols_right, shift_cols_left) are in extra_collection_shifts.rs.

    /// Set the same extras on a contiguous column range of one row (an RGB-styled
    /// ASCII run) in one pass; RGB-only extras go to the dense ring.
    #[inline]
    pub(crate) fn set_range_uniform(
        &mut self,
        row: u16,
        col_start: u16,
        col_end: u16,
        vals: &crate::extra::UniformExtras<'_>,
        visible_rows: u16,
        cols: u16,
    ) {
        if col_start >= col_end {
            return;
        }

        // RGB-only extras go to the dense ring.
        let is_rgb_only =
            vals.hyperlink.is_none() && vals.underline_color.is_none() && vals.extended_flags == 0;
        if is_rgb_only && (vals.fg_rgb.is_some() || vals.bg_rgb.is_some()) {
            // The ring path writes no map entry, so it cannot start its cells
            // fresh: an orphan keyed under the run would be claimed by the
            // HAS_EXTRAS bit the writer set. No sweep here (a probe per cell cost
            // +10% instructions on truecolor text with periodic OSC 8 links):
            // orphans are closed where they are made (`Grid::drop_unclaimed_extras`),
            // which `Grid::assert_extras_claimed` checks.
            self.set_rgb_ring_range(
                row,
                col_start,
                col_end,
                vals.fg_rgb,
                vals.bg_rgb,
                visible_rows,
                cols,
            );
            return;
        }

        // This path keeps the run's RGB in the map, but reads consult the ring first, so
        // drop the bytes an earlier ring writer (an RGB-only run, a truecolour erase) left
        // here, as the per-character writer does (`Grid::write_char_at_cursor_packed`).
        self.clear_rgb_ring_cell(row, col_start, col_end - col_start);

        // Build the shared (url, id) payload once, so every cell holds the same
        // `Arc`; a cell that later edits its link copies on write.
        let hyperlink = vals.hyperlink.map(|url| {
            Arc::new(crate::extra::HyperlinkData {
                url: Arc::clone(url),
                id: vals.hyperlink_id.cloned(),
            })
        });

        let internal_row = self.internal_row(row);
        // Exact charge for the run: every cell ends up holding the link, and a
        // replaced entry's link is refunded, so repainting the same OSC 8 row
        // costs the budget nothing.
        let mut replaced_links = 0usize;
        for col in col_start..col_end {
            let coord = CellCoord::new(internal_row, col);
            // An entry still keyed here is an orphan: start fresh (`Self::fresh`).
            let (extra, had_link) = fresh_slot(&mut self.data, coord);
            replaced_links += usize::from(had_link);
            if let Some(rgb) = vals.fg_rgb {
                extra.set_fg_rgb(Some(rgb));
            }
            if let Some(rgb) = vals.bg_rgb {
                extra.set_bg_rgb(Some(rgb));
            }
            if let Some(color) = vals.underline_color {
                extra.set_underline_color_u32(Some(color));
            }
            if vals.extended_flags != 0 {
                extra.set_extended_flags(vals.extended_flags);
            }
            if let Some(data) = &hyperlink {
                extra.set_hyperlink_data(Some(Arc::clone(data)));
            }
        }

        self.hyperlink_upper_bound = self.hyperlink_upper_bound.saturating_sub(replaced_links);
        if vals.hyperlink.is_some() {
            self.hyperlink_upper_bound += usize::from(col_end - col_start);
            self.enforce_hyperlink_limit();
        }
    }

    /// Clear all extras, releasing capacity: a new map (unlike `HashMap::clear`)
    /// reclaims the memory of a hyperlink-heavy peak.
    #[inline]
    pub fn clear(&mut self) {
        self.data = FxHashMap::default();
        self.row_offset = 0;
        // Exact: every link and image ref lived in the replaced map.
        self.hyperlink_upper_bound = 0;
        self.any_image = false;
        if let Some(ring) = &mut self.complex_ring {
            ring.clear();
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear();
        }
    }

    /// Region shift up by `n` within rows `[top, bottom]` on both rings.
    #[inline]
    pub(crate) fn shift_rings_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_region_up(top, bottom, n);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_region_up(top, bottom, n);
        }
    }

    /// Region shift down by `n` within rows `[top, bottom]` on both rings.
    #[inline]
    pub(crate) fn shift_rings_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_region_down(top, bottom, n);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_region_down(top, bottom, n);
        }
    }

    /// Rect shift up within `[top, bottom]` × `[left, right]` on both rings.
    #[inline]
    pub(crate) fn shift_rings_rect_up(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_rect_up(top, bottom, left, right, n);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_rect_up(top, bottom, left, right, n);
        }
    }

    /// Rect shift down within `[top, bottom]` × `[left, right]` on both rings.
    #[inline]
    pub(crate) fn shift_rings_rect_down(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_rect_down(top, bottom, left, right, n);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_rect_down(top, bottom, left, right, n);
        }
    }

    /// Column shift right (ICH) on one row on both rings.
    #[inline]
    pub(crate) fn shift_rings_cols_right(
        &mut self,
        row: u16,
        start_col: u16,
        count: u16,
        max_col: u16,
    ) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_cols_right(row, start_col, count, max_col);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_cols_right(row, start_col, count, max_col);
        }
    }

    /// Column shift left (DCH) on one row on both rings.
    #[inline]
    pub(crate) fn shift_rings_cols_left(
        &mut self,
        row: u16,
        start_col: u16,
        count: u16,
        max_col: u16,
    ) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_cols_left(row, start_col, count, max_col);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_cols_left(row, start_col, count, max_col);
        }
    }

    /// Apply the accumulated row offset, re-keying entries to external coords.
    /// Call before touching `self.data` with external coordinates (reflow, a
    /// direct `data.retain`). O(E), and only when `row_offset > 0`.
    pub(crate) fn compact(&mut self) {
        self.apply_offset_and_shift(0);
    }

    /// Take the accumulated row offset, resetting it to 0, so a region shift can
    /// compact and shift keys in one drain pass.
    #[inline]
    pub(crate) fn take_row_offset(&mut self) -> u16 {
        let offset = self.row_offset;
        self.row_offset = 0;
        offset
    }

    /// Current row offset (for diagnostics / test assertions).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn row_offset(&self) -> u16 {
        self.row_offset
    }

    /// Apply the accumulated `row_offset` plus an additional `shift` in one pass:
    /// drop stale entries and those scrolled off (external row < shift), re-key
    /// survivors to offset 0, then enforce the hyperlink limit.
    ///
    /// `#[inline(never)]` (here and on `apply_offset_and_shift_only`): on the
    /// scroll path under `lto = true` + `codegen-units = 1`, its body size decides
    /// an inlining cascade the mixed-VT workload is sensitive to. Pinning these,
    /// not `enforce_hyperlink_limit`, keeps hyperlink-limit edits from shifting it.
    #[inline(never)]
    fn apply_offset_and_shift(&mut self, shift: u16) {
        self.apply_offset_and_shift_only(shift);
        self.enforce_hyperlink_limit();
    }

    /// The compaction core without the hyperlink-limit enforcement, so
    /// [`Self::enforce_hyperlink_limit_cold`] can compact without recursing.
    #[inline(never)]
    fn apply_offset_and_shift_only(&mut self, shift: u16) {
        let total = match self.row_offset.checked_add(shift) {
            Some(0) => return,
            Some(t) => t,
            None => {
                // offset + shift > u16::MAX: every entry scrolled off.
                self.data.clear();
                self.row_offset = 0;
                return;
            }
        };
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row >= total {
                new_data.insert(CellCoord::new(coord.row - total, coord.col), extra);
            }
        }
        self.data = new_data;
        self.row_offset = 0;
    }

    /// Shrink the map after retain-style removals when capacity exceeds 4x len
    /// (and 64, to avoid thrashing small maps).
    #[inline]
    pub(crate) fn maybe_shrink(&mut self) {
        let cap = self.data.capacity();
        let len = self.data.len();
        if cap > 64 && cap > len.saturating_mul(4) {
            self.data.shrink_to_fit();
        }
    }

    /// Calculate total memory used.
    #[must_use]
    pub(crate) fn memory_used(&self) -> usize {
        // Saturating throughout, as in `Grid::memory_used`.
        let base = std::mem::size_of::<Self>().saturating_add(
            self.data
                .capacity()
                .saturating_mul(std::mem::size_of::<CellCoord>()),
        );
        let extras_mem = self
            .data
            .values()
            .map(CellExtra::memory_used)
            .fold(0, usize::saturating_add);
        let rgb_mem = self.rgb_ring.as_ref().map(|r| r.heap_bytes()).unwrap_or(0);
        base.saturating_add(extras_mem).saturating_add(rgb_mem)
    }

    /// Number of valid extras entries (excluding those scrolled off under the
    /// row offset).
    #[must_use]
    pub fn len(&self) -> usize {
        if self.row_offset == 0 {
            self.data.len()
        } else {
            self.data
                .keys()
                .filter(|c| c.row >= self.row_offset)
                .count()
        }
    }

    /// Backing map capacity (for memory/regression tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Drop the ring buffers on resize, so the next write re-creates them with
    /// the current `stride`/`visible_rows` instead of indexing with stale ones.
    pub(crate) fn invalidate_rings(&mut self) {
        self.complex_ring = None;
        self.rgb_ring = None;
    }

    /// Whether the dense RGB ring exists. It is created by the first ring-stored
    /// truecolor write after each [`Self::invalidate_rings`], so `false` means
    /// the resize's harvest of ring colours into the map has nothing to read.
    #[inline]
    pub(crate) fn has_rgb_ring(&self) -> bool {
        self.rgb_ring.is_some()
    }

    /// Remove all extras in rows `>= max_row` (external coordinates), for the
    /// reflow path.
    pub(crate) fn retain_rows_below(&mut self, max_row: u16) {
        if self.data.is_empty() {
            return;
        }
        self.compact();
        self.data.retain(|coord, _| coord.row < max_row);
        self.maybe_shrink();
    }

    /// Remove all extras in columns `>= max_col` (external coordinates), for the
    /// no-reflow resize path.
    pub(crate) fn retain_cols_below(&mut self, max_col: u16) {
        if self.data.is_empty() {
            return;
        }
        self.compact();
        self.data.retain(|coord, _| coord.col < max_col);
        self.maybe_shrink();
    }

    /// Check if any cell in a row has a hyperlink (O(E) scan).
    #[must_use]
    #[cfg(test)]
    pub fn row_has_hyperlinks(&self, row: u16) -> bool {
        let internal_row = self.internal_row(row);
        self.data
            .iter()
            .any(|(coord, extra)| coord.row == internal_row && extra.hyperlink().is_some())
    }

    /// Enforce the hyperlink entry limit (bounding memory under OSC 8 spam):
    /// when the population may exceed [`MAX_HYPERLINK_ENTRIES`], drop the links
    /// of the oldest entries (lowest internal rows). Entries keep their other
    /// extras; one left empty is deleted.
    #[inline]
    pub fn enforce_hyperlink_limit(&mut self) {
        // Two sound upper bounds on the hyperlink population; either proving
        // "under budget" skips the cold walk. `data.len()` caps the counter's
        // over-charge (`get_or_create` charges every link-free handout).
        if self.hyperlink_upper_bound <= MAX_HYPERLINK_ENTRIES
            || self.data.len() <= MAX_HYPERLINK_ENTRIES
        {
            return;
        }
        self.enforce_hyperlink_limit_cold();
    }

    #[cold]
    #[inline(never)]
    fn enforce_hyperlink_limit_cold(&mut self) {
        let mut compacted = false;
        // One walk into the resident scratch buffer; it decides with the exact
        // count and repairs the bound.
        let mut coords = std::mem::take(&mut self.hyperlink_scratch);
        loop {
            coords.clear();
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_hyperlink_walk_ops(self.data.len());
            coords.extend(
                self.data
                    .iter()
                    .filter(|(_, extra)| extra.hyperlink().is_some())
                    .map(|(coord, _)| *coord),
            );
            // Catches an insertion path that forgets to charge the bound.
            debug_assert!(
                self.hyperlink_upper_bound >= coords.len(),
                "hyperlink_upper_bound {} under-counts the {} hyperlink entries \
                 present — an insertion path is not charging the budget",
                self.hyperlink_upper_bound,
                coords.len()
            );

            if coords.len() <= MAX_HYPERLINK_ENTRIES {
                // The walk counted exactly: repair the bound.
                self.hyperlink_upper_bound = coords.len();
                break;
            }

            // Over. Entries scrolled off under the row offset are unreachable but
            // still counted, so compact and retry, at most once.
            if !compacted && self.row_offset != 0 {
                compacted = true;
                self.apply_offset_and_shift_only(0);
                continue;
            }

            // Evict to a low-water mark, not the ceiling: trimming to exactly
            // `MAX_HYPERLINK_ENTRIES` would make the next insert re-enter this
            // path, and checkpoint restore (`Grid::fill_row_from_line`) calls it
            // once per row. `to_evict >= 1` since `coords.len() > MAX > LOW_WATER`.
            const HYPERLINK_LOW_WATER: usize = MAX_HYPERLINK_ENTRIES * 3 / 4;
            let to_evict = coords.len() - HYPERLINK_LOW_WATER;

            // Only the `to_evict` smallest coords are needed: partition, not sort.
            coords.select_nth_unstable_by(to_evict - 1, |a, b| {
                a.row.cmp(&b.row).then(a.col.cmp(&b.col))
            });

            for &coord in &coords[..to_evict] {
                if let Some(extra) = self.data.get_mut(&coord) {
                    extra.set_hyperlink(None);
                    if !extra.has_data() {
                        self.data.remove(&coord);
                    }
                }
            }

            self.hyperlink_upper_bound = HYPERLINK_LOW_WATER;

            self.maybe_shrink();
            break;
        }
        // Return the buffer empty but with its capacity, on every exit.
        coords.clear();
        self.hyperlink_scratch = coords;
    }

    /// The current hyperlink upper bound (for the invariant tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn hyperlink_upper_bound(&self) -> usize {
        self.hyperlink_upper_bound
    }

    /// The true number of hyperlink-bearing entries — O(E), tests only.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn hyperlink_entries_slow(&self) -> usize {
        self.data
            .values()
            .filter(|extra| extra.hyperlink().is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both rings ignore a column at or past their stride: a read is `None`
    /// and a write a no-op, never an alias into the next ring row's slots
    /// (row 0, col 10 of a 10-wide ring is row 1, col 0's slot).
    #[test]
    fn ring_columns_past_the_stride_never_alias_the_next_row() {
        let mut complex = ComplexCharRing::new(4, 10);
        complex.set(1, 0, '\u{1F680}');
        complex.set(0, 15, '\u{2764}');
        assert_eq!(complex.get(1, 5), None, "a write past the stride aliased");
        for col in [10, 100] {
            assert_eq!(complex.get(0, col), None, "col {col}");
        }
        complex.set(0, 10, '\u{1F389}');
        assert_eq!(complex.get(1, 0), Some('\u{1F680}'));

        let mut rgb = RgbColorRing::new(4, 10);
        rgb.set_fg(1, 0, [0, 0, 255]);
        rgb.set_bg(1, 0, [128, 128, 0]);
        rgb.set_fg(0, 15, [255, 0, 0]);
        rgb.set_bg(0, 15, [0, 255, 0]);
        assert_eq!((rgb.fg(1, 5), rgb.bg(1, 5)), (None, None));
        for col in [10, 200] {
            assert_eq!((rgb.fg(0, col), rgb.bg(0, col)), (None, None), "col {col}");
        }
        rgb.set_fg(0, 10, [1, 1, 1]);
        assert_eq!(rgb.fg(1, 0), Some([0, 0, 255]));
    }

    /// Scrolling a ring by at least its visible rows (e.g. `CSI 65535 S`)
    /// recycles it whole and resets `ring_offset`, so later writes stay
    /// readable.
    #[test]
    fn ring_overscroll_recycles_and_resets_the_offset() {
        for n in [4, 5, 65535] {
            let mut complex = ComplexCharRing::new(4, 10);
            complex.set(3, 1, '\u{1F389}');
            complex.scroll_up(1);
            complex.scroll_up(n);
            assert_eq!(complex.ring_offset, 0, "scroll {n}");
            assert_eq!(complex.get(2, 1), None, "scroll {n}");
            complex.set(2, 3, '\u{1F680}');
            assert_eq!(complex.get(2, 3), Some('\u{1F680}'), "scroll {n}");

            let mut rgb = RgbColorRing::new(4, 10);
            rgb.set_fg(3, 0, [255, 0, 0]);
            rgb.set_bg(3, 0, [0, 255, 0]);
            rgb.scroll_up(1);
            rgb.scroll_up(n);
            assert_eq!(rgb.ring_offset, 0, "scroll {n}");
            assert_eq!((rgb.fg(2, 0), rgb.bg(2, 0)), (None, None), "scroll {n}");
            rgb.set_fg(2, 2, [10, 20, 30]);
            rgb.set_bg(2, 2, [40, 50, 60]);
            assert_eq!(rgb.fg(2, 2), Some([10, 20, 30]), "scroll {n}");
            assert_eq!(rgb.bg(2, 2), Some([40, 50, 60]), "scroll {n}");
        }
    }

    /// EVERY CLEAR REACHES BOTH STORES (#7697): `clear_row`, `clear_range`,
    /// `clear_rows` and `clear_rect` remove exactly their cells from the
    /// HashMap and from the RGB ring (where RGB-only writes live), keep every
    /// other cell, and an empty range clears nothing. Each runs on a dense and
    /// a sparse map so `clear_range` takes both its probe and its scan.
    #[test]
    fn every_clear_removes_exactly_its_cells_from_map_and_rgb_ring() {
        type Clear = fn(&mut CellExtras);
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let cases: &[(&str, Clear, fn(u16, u16) -> bool)] = &[
            ("clear_row(1)", |e| e.clear_row(1), |r, _| r == 1),
            (
                "clear_range(3, 1..4)",
                |e| e.clear_range(3, 1, 4),
                |r, c| r == 3 && (1..4).contains(&c),
            ),
            (
                "clear_range(3, 4..4)",
                |e| e.clear_range(3, 4, 4),
                |_, _| false,
            ),
            (
                "clear_rows(1..3)",
                |e| e.clear_rows(1..3),
                |r, _| (1..3).contains(&r),
            ),
            ("clear_rows(3..3)", |e| e.clear_rows(3..3), |_, _| false),
            (
                "clear_rect(1..3, 3..7)",
                |e| e.clear_rect(1..3, 3..7),
                |r, c| (1..3).contains(&r) && (3..7).contains(&c),
            ),
            (
                "clear_rect(0..4, 3..3)",
                |e| e.clear_rect(0..4, 3..3),
                |_, _| false,
            ),
        ];
        for &(name, clear, cleared) in cases {
            for dense in [true, false] {
                let cells: Vec<(u16, u16)> = (0..4u16)
                    .flat_map(|r| (0..10u16).map(move |c| (r, c)))
                    .filter(|&(r, c)| dense || (r + c) % 9 == 0)
                    .collect();
                let mut extras = CellExtras::new();
                for &(r, c) in &cells {
                    let mut extra = CellExtra::default();
                    extra.set_fg_rgb(Some([r as u8, c as u8, 1]));
                    extras.set(CellCoord::new(r, c), extra);
                    extras.set_rgb_ring_range(
                        r,
                        c,
                        c + 1,
                        None,
                        Some([r as u8, c as u8, 2]),
                        4,
                        10,
                    );
                }
                clear(&mut extras);
                for &(r, c) in &cells {
                    let kept = !cleared(r, c);
                    let map = extras.get(CellCoord::new(r, c)).and_then(CellExtra::fg_rgb);
                    assert_eq!(
                        map.is_some(),
                        kept,
                        "{name} (dense {dense}): map ({r}, {c})"
                    );
                    let ring = extras.bg_rgb_for(r, c);
                    assert_eq!(
                        ring.is_some(),
                        kept,
                        "{name} (dense {dense}): ring ({r}, {c})"
                    );
                }
            }
        }
    }

    /// `fill_runs` leaves the ring exactly as per-cell `set_fg`/`set_bg` does,
    /// including empty, overhanging and fully out-of-range runs (which must not
    /// allocate the plane).
    #[test]
    fn fill_run_matches_per_cell_set() {
        let rgb_f = [17u8, 34, 51];
        let rgb_b = [68u8, 85, 102];
        // (visible_rows, cols, row, col_start, col_end)
        let cases: &[(u16, u16, u16, u16, u16)] = &[
            (4, 10, 0, 0, 10),  // full row
            (4, 10, 2, 3, 7),   // interior run
            (4, 10, 1, 0, 1),   // single cell
            (4, 10, 3, 5, 5),   // empty run (start == end)
            (4, 10, 1, 7, 3),   // inverted (start > end) — empty
            (4, 10, 2, 6, 20),  // overhang past stride → clamp to 10
            (4, 10, 0, 10, 12), // wholly out of range → no write, no alloc
            (4, 10, 0, 12, 15), // fully past stride → no write, no alloc
            (24, 8, 23, 0, 8),  // last ring row, full width
            (24, 8, 5, 2, 6),   // mid ring row
        ];
        for &(vis, cols, row, cs, ce) in cases {
            let mut per_cell = RgbColorRing::new(vis, cols);
            let mut bulk = RgbColorRing::new(vis, cols);
            for col in cs..ce {
                per_cell.set_fg(row, col, rgb_f);
                per_cell.set_bg(row, col, rgb_b);
            }
            bulk.fill_runs(row, cs, ce, Some(rgb_f), Some(rgb_b));
            // The whole grid must read back identically through the public getters.
            for r in 0..vis {
                for c in 0..cols {
                    assert_eq!(
                        per_cell.fg(r, c),
                        bulk.fg(r, c),
                        "fg mismatch at ({r},{c}) for case vis={vis} cols={cols} row={row} {cs}..{ce}"
                    );
                    assert_eq!(
                        per_cell.bg(r, c),
                        bulk.bg(r, c),
                        "bg mismatch at ({r},{c}) for case vis={vis} cols={cols} row={row} {cs}..{ce}"
                    );
                }
            }
            // Allocation parity: a fully out-of-range run must leave the plane absent,
            // exactly as the per-cell path (which returns before `fg_mut`) does.
            assert_eq!(
                per_cell.heap_bytes(),
                bulk.heap_bytes(),
                "plane-allocation mismatch for case vis={vis} cols={cols} row={row} {cs}..{ce}"
            );
        }
    }

    /// `RgbColorRing::clear_range` leaves the planes exactly as a per-column loop
    /// does, including a stale `row >= visible_rows` and non-zero ring offsets,
    /// and never allocates an absent plane.
    #[test]
    fn clear_range_matches_per_cell_clear() {
        // Reference oracle: the per-column loop.
        fn clear_range_per_cell(ring: &mut RgbColorRing, row: u16, start_col: u16, end_col: u16) {
            for col in start_col..end_col {
                if col >= ring.stride {
                    break;
                }
                let idx = ring.index(row, col);
                if let Some(fg) = ring.fg.as_deref_mut()
                    && let Some(entry) = fg.get_mut(idx)
                {
                    *entry = 0;
                }
                if let Some(bg) = ring.bg.as_deref_mut()
                    && let Some(entry) = bg.get_mut(idx)
                {
                    *entry = 0;
                }
            }
        }

        // (visible_rows, cols, row, start_col, end_col, scroll_before)
        let cases: &[(u16, u16, u16, u16, u16, u16)] = &[
            (4, 10, 1, 3, 6, 0),   // interior run
            (4, 10, 0, 0, 10, 0),  // full row
            (4, 10, 2, 5, 5, 0),   // empty run (start == end)
            (4, 10, 2, 7, 3, 0),   // inverted (start > end)
            (4, 10, 3, 6, 20, 0),  // overhang past stride → clamp to 10
            (4, 10, 0, 10, 12, 0), // wholly out of range
            (4, 10, 0, 12, 15, 0), // fully past stride
            (4, 10, 9, 2, 5, 0),   // stale row >= visible_rows → no writes
            (4, 10, 1, 0, 10, 2),  // non-zero ring offset
            (4, 10, 3, 2, 8, 3),   // non-zero ring offset, wrapped row
            (24, 8, 23, 0, 8, 0),  // last ring row, full width
            (24, 8, 5, 2, 6, 7),   // mid ring row, offset
        ];
        for &(vis, cols, row, cs, ce, scroll) in cases {
            let mut seed = RgbColorRing::new(vis, cols);
            for r in 0..vis {
                for c in 0..cols {
                    seed.set_fg(r, c, [r as u8, c as u8, 1]);
                    seed.set_bg(r, c, [r as u8, c as u8, 2]);
                }
            }
            if scroll > 0 {
                seed.scroll_up(scroll);
            }
            let mut per_cell = seed.clone();
            let mut bulk = seed;
            clear_range_per_cell(&mut per_cell, row, cs, ce);
            bulk.clear_range(row, cs, ce);
            assert_eq!(
                per_cell.fg, bulk.fg,
                "fg plane mismatch for vis={vis} cols={cols} row={row} {cs}..{ce} scroll={scroll}"
            );
            assert_eq!(
                per_cell.bg, bulk.bg,
                "bg plane mismatch for vis={vis} cols={cols} row={row} {cs}..{ce} scroll={scroll}"
            );
            assert_eq!(
                per_cell.heap_bytes(),
                bulk.heap_bytes(),
                "plane-allocation mismatch for vis={vis} cols={cols} row={row} {cs}..{ce}"
            );
        }
    }

    /// `ComplexCharRing::clear_range`'s one `fill('\0')` equals a per-column
    /// store, across the same kinds of ranges.
    #[test]
    fn complex_clear_range_matches_per_cell_clear() {
        fn clear_range_per_cell(ring: &mut ComplexCharRing, row: u16, left: u16, right_excl: u16) {
            let base = (ring.ring_row(row) as usize) * (ring.stride as usize);
            for col in left..right_excl.min(ring.stride) {
                let idx = base + col as usize;
                if let Some(e) = ring.entries.get_mut(idx) {
                    *e = '\0';
                }
            }
        }

        // (visible_rows, cols, row, left, right_excl, scroll_before)
        let cases: &[(u16, u16, u16, u16, u16, u16)] = &[
            (4, 10, 1, 3, 6, 0),
            (4, 10, 0, 0, 10, 0),
            (4, 10, 2, 5, 5, 0),
            (4, 10, 2, 7, 3, 0),
            (4, 10, 3, 6, 20, 0),
            (4, 10, 0, 10, 12, 0),
            (4, 10, 9, 2, 5, 0),
            (4, 10, 1, 0, 10, 2),
            (24, 8, 23, 0, 8, 0),
        ];
        for &(vis, cols, row, left, right, scroll) in cases {
            let mut seed = ComplexCharRing::new(vis, cols);
            for r in 0..vis {
                for c in 0..cols {
                    let ch = char::from_u32(0x1F600 + u32::from(r) * 32 + u32::from(c))
                        .expect("valid char");
                    seed.set(r, c, ch);
                }
            }
            if scroll > 0 {
                seed.scroll_up(scroll);
            }
            let mut per_cell = seed.clone();
            let mut bulk = seed;
            clear_range_per_cell(&mut per_cell, row, left, right);
            bulk.clear_range(row, left, right);
            assert_eq!(
                per_cell.entries, bulk.entries,
                "entries mismatch for vis={vis} cols={cols} row={row} {left}..{right} scroll={scroll}"
            );
        }
    }

    #[test]
    fn test_render_data_for_cell_reads_hashmap_fallbacks() {
        let mut extras = CellExtras::new();
        let coord = CellCoord::new(1, 2);
        let entry = extras.get_or_create(coord);
        entry.set_complex_char(Some(std::sync::Arc::<str>::from("x\u{0301}")));
        entry.set_fg_rgb(Some([10, 20, 30]));
        entry.set_bg_rgb(Some([40, 50, 60]));
        entry.add_combining('\u{0301}');

        let cell = Cell::from_raw_parts(
            0,
            crate::PackedColors::new()
                .with_rgb_fg()
                .with_rgb_bg()
                .with_extras_flag(),
            crate::CellFlags::COMPLEX,
        );

        let data = extras.render_data_for_cell(1, 2, cell);
        let extra = data
            .cell_extra()
            .expect("HAS_EXTRAS should expose CellExtra");
        assert_eq!(data.complex_char(), Some('x'));
        assert_eq!(data.fg_rgb(), Some([10, 20, 30]));
        assert_eq!(data.bg_rgb(), Some([40, 50, 60]));
        assert_eq!(extra.combining(), &['\u{0301}']);
    }

    #[test]
    fn test_render_data_for_cell_prefers_ring_values() {
        let mut extras = CellExtras::new();
        let coord = CellCoord::new(0, 1);
        let entry = extras.get_or_create(coord);
        entry.set_complex_char(Some(std::sync::Arc::<str>::from("A")));
        entry.set_fg_rgb(Some([90, 91, 92]));
        entry.set_bg_rgb(Some([93, 94, 95]));
        entry.add_combining('\u{0308}');

        extras.set_complex_char_ring(0, 1, '\u{1F680}', 2, 4);
        extras.set_rgb_ring_range(0, 1, 2, Some([1, 2, 3]), Some([4, 5, 6]), 2, 4);

        let cell = Cell::from_raw_parts(
            0,
            crate::PackedColors::new()
                .with_rgb_fg()
                .with_rgb_bg()
                .with_extras_flag(),
            crate::CellFlags::COMPLEX,
        );

        let data = extras.render_data_for_cell(0, 1, cell);
        let extra = data
            .cell_extra()
            .expect("HAS_EXTRAS should expose CellExtra");
        assert_eq!(data.complex_char(), Some('\u{1F680}'));
        assert_eq!(data.fg_rgb(), Some([1, 2, 3]));
        assert_eq!(data.bg_rgb(), Some([4, 5, 6]));
        assert_eq!(extra.combining(), &['\u{0308}']);
    }

    /// Sharing is unobservable: `set_range_uniform` gives every cell of an OSC 8
    /// run the same `Arc<HyperlinkData>`, so a later single-cell edit must
    /// copy-on-write and leave its neighbours' link alone.
    #[test]
    fn per_cell_hyperlink_edit_does_not_leak_across_the_run() {
        const RUN_URL: &str = "https://example.com/run";
        let url: Arc<str> = Arc::from(RUN_URL);
        let id: Arc<str> = Arc::from("run-1");
        let mut extras = CellExtras::new();
        extras.set_range_uniform(
            0,
            0,
            4,
            &crate::extra::UniformExtras {
                fg_rgb: None,
                bg_rgb: None,
                underline_color: None,
                extended_flags: 0,
                hyperlink: Some(&url),
                hyperlink_id: Some(&id),
            },
            24,
            80,
        );
        for col in 0..4 {
            let extra = extras
                .get(CellCoord::new(0, col))
                .expect("the bulk run must create an entry per covered cell");
            assert_eq!(extra.hyperlink().map(|u| &**u), Some(RUN_URL));
            assert_eq!(extra.hyperlink_id().map(|i| &**i), Some("run-1"));
        }

        // Edit one cell's URL (clearing its id) and another cell's id.
        let other: Arc<str> = Arc::from("https://example.com/other");
        extras
            .get_or_create(CellCoord::new(0, 1))
            .set_hyperlink(Some(other));
        let other_id: Arc<str> = Arc::from("run-2");
        extras
            .get_or_create(CellCoord::new(0, 2))
            .set_hyperlink_id(Some(other_id));

        let c1 = extras.get(CellCoord::new(0, 1)).expect("entry 1");
        assert_eq!(
            c1.hyperlink().map(|u| &**u),
            Some("https://example.com/other")
        );
        assert_eq!(c1.hyperlink_id(), None, "set_hyperlink clears the stale id");

        let c2 = extras.get(CellCoord::new(0, 2)).expect("entry 2");
        assert_eq!(c2.hyperlink().map(|u| &**u), Some(RUN_URL));
        assert_eq!(c2.hyperlink_id().map(|i| &**i), Some("run-2"));

        // The untouched cells keep the original pair.
        for col in [0u16, 3] {
            let extra = extras.get(CellCoord::new(0, col)).expect("untouched entry");
            assert_eq!(
                extra.hyperlink().map(|u| &**u),
                Some(RUN_URL),
                "col {col} lost its url to a neighbour's edit"
            );
            assert_eq!(
                extra.hyperlink_id().map(|i| &**i),
                Some("run-1"),
                "col {col} lost its id to a neighbour's edit"
            );
        }
    }
}
