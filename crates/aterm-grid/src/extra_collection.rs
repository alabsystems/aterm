// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell extras collection — HashMap-backed storage for grid-wide extras.
//!
//! See [`crate::extra`] for the per-cell `CellExtra` type and design docs.
//!
//! ## Row-Offset Amortization (#4542)
//!
//! Full-screen scroll (`shift_rows_up_by(0, n)`) is the dominant extras
//! mutation in terminal output. The naive approach drains and rebuilds the
//! entire HashMap on every scroll: O(E) per scroll where E is the number
//! of extras entries (hyperlinks, RGB colors, complex chars).
//!
//! This module uses a **row offset** to make full-screen scrolls O(1):
//!
//! - External coordinates (what callers use) are translated to internal
//!   HashMap keys by adding `row_offset`.
//! - On full-screen scroll, `row_offset` is incremented — no entries move.
//! - Stale entries (scrolled-off rows) remain in the map temporarily.
//! - Every `COMPACT_THRESHOLD` scrolls, a single O(E) compaction pass
//!   removes stale entries and resets the offset.
//!
//! Amortized cost: O(E / COMPACT_THRESHOLD) per scroll ≈ O(1) for typical
//! terminal workloads.

use std::sync::Arc;

use aterm_hash::FxHashMap;

use crate::cell::Cell;
use crate::extra::{CellCoord, CellExtra};

/// Dense ring-buffer storage for non-BMP complex chars in the visible grid.
///
/// Replaces `FxHashMap` probing (~15ns/char) with flat array indexing (~2ns/char)
/// for the common case of emoji and other non-BMP characters that need only
/// `complex_char` storage (no hyperlinks, combining marks, or RGB extras).
///
/// The ring buffer is sized to `visible_rows * cols` and uses a row offset for
/// O(1) full-screen scroll. When a row scrolls off, its entries are recycled.
/// Stale entries are harmless — readers always check `CellFlags::COMPLEX` first.
#[derive(Debug, Clone)]
pub(crate) struct ComplexCharRing {
    /// Flat array: `entries[ring_row * stride + col]`.
    /// Stores raw `char` codepoints. `'\0'` = empty slot.
    /// Eliminates `Arc<str>` atomic refcounting from the write hot path.
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

    /// Map an external row to its ring index.
    #[inline(always)]
    fn ring_row(&self, external_row: u16) -> u16 {
        let r = external_row.wrapping_add(self.ring_offset);
        if r >= self.visible_rows {
            r - self.visible_rows
        } else {
            r
        }
    }

    /// Compute flat array index from (external_row, col).
    ///
    /// # Safety invariant
    /// Caller must ensure `col < self.stride`. Use `get()` / `set()` which
    /// validate this precondition; direct `index()` calls are .
    #[inline(always)]
    fn index(&self, row: u16, col: u16) -> usize {
        debug_assert!(
            col < self.stride,
            "ComplexCharRing::index: col {col} >= stride {}",
            self.stride
        );
        (self.ring_row(row) as usize) * (self.stride as usize) + (col as usize)
    }

    /// Get the complex char codepoint at (row, col), if any.
    ///
    /// Returns `None` if `col >= stride` (stale dimensions after resize).
    #[inline]
    pub(crate) fn get(&self, row: u16, col: u16) -> Option<char> {
        if col >= self.stride {
            return None;
        }
        let idx = self.index(row, col);
        self.entries.get(idx).copied().filter(|&c| c != '\0')
    }

    /// Store a complex char codepoint at (row, col).
    ///
    /// No-op if `col >= stride` (stale dimensions after resize).
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

    /// Store consecutive wide-char codepoints on the same row.
    ///
    /// Computes `ring_row` once and writes at col, col+2, col+4...
    #[inline]
    pub(crate) fn set_wide_run(&mut self, row: u16, start_col: u16, chars: &[char]) {
        if chars.is_empty() || start_col >= self.stride {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let last_idx = base + (start_col as usize) + (chars.len() - 1) * 2;
        if last_idx < self.entries.len() {
            // SAFETY: bounds verified above — last index is in range,
            // so all prior indices are too (stride increases monotonically).
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

    /// Scroll up by `n` rows: recycle the top `n` rows by clearing them.
    pub(crate) fn scroll_up(&mut self, n: u16) {
        // Over-scroll: when the entire visible area (or more) scrolls off, the
        // ring is fully recycled and the offset must reset to 0. The single-
        // subtraction `ring_row` only normalizes inputs < 2*visible_rows, so a
        // huge `n` (e.g. a full-screen `CSI 65535 S`) would otherwise leave
        // `ring_offset >= visible_rows`, corrupting every subsequent index.
        if n >= self.visible_rows {
            self.clear();
            return;
        }
        let stride = self.stride as usize;
        for i in 0..n {
            let old_row = self.ring_row(i);
            let start = (old_row as usize) * stride;
            let end = start + stride;
            if end <= self.entries.len() {
                self.entries[start..end].fill('\0');
            }
        }
        self.ring_offset = self.ring_row(n);
    }

    /// Clear all entries.
    pub(crate) fn clear(&mut self) {
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

    /// Clear a `[left, right]` column span on one external row.
    ///
    /// Every column of one row shares a single `ring_row`, so the touched flat
    /// indices are contiguous and a single `fill('\0')` (a memset — the value is
    /// all-zero bytes) replaces the per-column `get_mut`. The all-or-nothing
    /// `<= entries.len()` guard is exactly equivalent to the old per-element
    /// bounds checks: `entries` is `vec!['\0'; visible_rows * cols]` in `new`
    /// and `stride`/`visible_rows` are never reassigned, so `entries.len() ==
    /// visible_rows * stride`; with `end <= stride`, `ring_row < visible_rows`
    /// makes `base + end <= len` (guard always passes, same indices), and
    /// `ring_row >= visible_rows` makes `base >= len` (old loop stored nothing
    /// either). A PARTIAL clear is therefore unreachable — a future
    /// resize-in-place of the ring would have to revisit this.
    #[inline]
    fn clear_range(&mut self, row: u16, left: u16, right_excl: u16) {
        // Clamp exactly like the old `right_excl.min(self.stride)` range bound.
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

    /// Clear a rectangular `[rows] x [cols]` region, one row at a time.
    ///
    /// Mirrors `RgbColorRing::clear_rect`. Used by `CellExtras::clear_rect`
    /// so the dense complex-char ring is wiped alongside the RGB ring (#7977):
    /// a complex cell's codepoint lives in this ring, so without clearing it a
    /// rect overwrite (e.g. DECCRA) could read the prior dst codepoint.
    pub(crate) fn clear_rect(&mut self, rows: core::ops::Range<u16>, cols: core::ops::Range<u16>) {
        for row in rows {
            self.clear_range(row, cols.start, cols.end);
        }
    }

    /// Copy a `[left, right]` column span from `src_row` to `dst_row`.
    #[inline]
    fn copy_row_span(&mut self, src_row: u16, dst_row: u16, left: u16, right: u16) {
        let stride = self.stride;
        let hi = right.min(stride.saturating_sub(1));
        let src_base = (self.ring_row(src_row) as usize) * (stride as usize);
        let dst_base = (self.ring_row(dst_row) as usize) * (stride as usize);
        if left > hi {
            return;
        }
        // Contiguous per-row span: replace the per-cell loop with a single
        // memmove. Keep a runtime all-or-nothing bounds guard (matching
        // clear_row/clear_range) so degenerate dims silently skip rather than
        // panic.
        let span = (hi - left) as usize + 1;
        let s0 = src_base + left as usize;
        let d0 = dst_base + left as usize;
        if s0 + span <= self.entries.len() && d0 + span <= self.entries.len() {
            self.entries.copy_within(s0..s0 + span, d0);
        }
    }

    /// Region shift up by `n` within rows `[top, bottom]` (full-width).
    /// Mirrors `RgbColorRing::shift_region_up`.
    pub(crate) fn shift_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        if n == 0 || top > bottom {
            return;
        }
        let n = n.min(bottom - top + 1);
        if let Some(shift_start) = top.checked_add(n) {
            let mut dst = top;
            let mut src = shift_start;
            while src <= bottom {
                self.copy_row_span(src, dst, 0, self.stride.saturating_sub(1));
                dst += 1;
                src += 1;
            }
        }
        for row in (bottom + 1 - n)..=bottom {
            self.clear_row(row);
        }
    }

    /// Region shift down by `n` within rows `[top, bottom]` (full-width).
    /// Mirrors `RgbColorRing::shift_region_down`.
    pub(crate) fn shift_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        if n == 0 || top > bottom {
            return;
        }
        let n = n.min(bottom - top + 1);
        let mut src = bottom + 1 - n;
        while src > top {
            src -= 1;
            let dst = src + n;
            if dst <= bottom {
                self.copy_row_span(src, dst, 0, self.stride.saturating_sub(1));
            }
        }
        let clear_to = (top + n).min(bottom + 1);
        for row in top..clear_to {
            self.clear_row(row);
        }
    }

    /// Rect shift up within `[top, bottom]` × `[left, right]`.
    pub(crate) fn shift_rect_up(&mut self, top: u16, bottom: u16, left: u16, right: u16, n: u16) {
        if n == 0 || top > bottom || left > right {
            return;
        }
        let n = n.min(bottom - top + 1);
        if let Some(shift_start) = top.checked_add(n) {
            let mut dst = top;
            let mut src = shift_start;
            while src <= bottom {
                self.copy_row_span(src, dst, left, right);
                dst += 1;
                src += 1;
            }
        }
        for row in (bottom + 1 - n)..=bottom {
            self.clear_range(row, left, right.saturating_add(1));
        }
    }

    /// Rect shift down within `[top, bottom]` × `[left, right]`.
    pub(crate) fn shift_rect_down(&mut self, top: u16, bottom: u16, left: u16, right: u16, n: u16) {
        if n == 0 || top > bottom || left > right {
            return;
        }
        let n = n.min(bottom - top + 1);
        let mut src = bottom + 1 - n;
        while src > top {
            src -= 1;
            let dst = src + n;
            if dst <= bottom {
                self.copy_row_span(src, dst, left, right);
            }
        }
        let clear_to = (top + n).min(bottom + 1);
        for row in top..clear_to {
            self.clear_range(row, left, right.saturating_add(1));
        }
    }

    /// Column shift right (ICH) on one row. Mirrors `RgbColorRing::shift_cols_right`.
    pub(crate) fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let hi = max_col.min(self.stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let mut col = hi;
        while col > start_col {
            col -= 1;
            let idx = base + col as usize;
            if col >= start_col + count {
                let s = base + (col - count) as usize;
                if s < self.entries.len() && idx < self.entries.len() {
                    self.entries[idx] = self.entries[s];
                }
            } else if let Some(e) = self.entries.get_mut(idx) {
                *e = '\0';
            }
        }
    }

    /// Column shift left (DCH) on one row. Mirrors `RgbColorRing::shift_cols_left`.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let hi = max_col.min(self.stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let mut col = start_col;
        while col < hi {
            let src_col = col.saturating_add(count);
            let idx = base + col as usize;
            if src_col < hi {
                let s = base + src_col as usize;
                if s < self.entries.len() && idx < self.entries.len() {
                    self.entries[idx] = self.entries[s];
                }
            } else if let Some(e) = self.entries.get_mut(idx) {
                *e = '\0';
            }
            col += 1;
        }
    }
}

/// Dense ring-buffer storage for RGB overflow colors in the visible grid.
///
/// Replaces `FxHashMap` probing (~15ns/cell) with flat array indexing (~2ns/cell)
/// for cells whose only extra is fg/bg RGB color (no hyperlinks, underline
/// color, or extended flags). Follows the same pattern as `ComplexCharRing`.
///
/// Encoding: `0` = no color, `0x01_RRGGBB` = has RGB (matches `PackedColor::rgb` format).
/// Black is `0x01_000000`, not zero, so `0` is an unambiguous sentinel.
#[derive(Debug, Clone)]
pub(crate) struct RgbColorRing {
    /// FG entries: `fg[ring_row * stride + col]`.
    ///
    /// Lazy: absent (`None`) until a true-colour fg is actually stored. An
    /// absent plane reads exactly like a dense all-zero plane (every cell
    /// returns the default/indexed path), so behaviour is identical — it just
    /// skips the `capacity * 4 B` allocation for screens that never use a
    /// true-colour foreground (e.g. indexed-only or bg-only colouring).
    fg: Option<Box<[u32]>>,
    /// BG entries: `bg[ring_row * stride + col]`.
    ///
    /// Lazy, mirroring `fg`: syntax-highlighted output that only overrides the
    /// foreground never allocates this plane.
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
        let fg = self.fg.as_deref().map_or(0, |p| p.len() * 4);
        let bg = self.bg.as_deref().map_or(0, |p| p.len() * 4);
        fg + bg
    }

    #[inline(always)]
    fn ring_row(&self, external_row: u16) -> u16 {
        let r = external_row.wrapping_add(self.ring_offset);
        if r >= self.visible_rows {
            r - self.visible_rows
        } else {
            r
        }
    }

    /// Compute flat array index from (external_row, col).
    ///
    /// # Safety invariant
    /// Caller must ensure `col < self.stride`. Use the public accessors which
    /// validate this precondition; direct `index()` calls are .
    #[inline(always)]
    fn index(&self, row: u16, col: u16) -> usize {
        debug_assert!(
            col < self.stride,
            "RgbColorRing::index: col {col} >= stride {}",
            self.stride
        );
        (self.ring_row(row) as usize) * (self.stride as usize) + (col as usize)
    }

    /// Get fg RGB at (row, col), if any.
    ///
    /// Returns `None` if `col >= stride` (stale dimensions after resize).
    #[inline]
    pub(crate) fn fg(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        if col >= self.stride {
            return None;
        }
        // Absent plane == all-zero plane: no override stored.
        let plane = self.fg.as_deref()?;
        let idx = self.index(row, col);
        let packed = *plane.get(idx)?;
        if packed != 0 {
            Some([
                ((packed >> 16) & 0xFF) as u8,
                ((packed >> 8) & 0xFF) as u8,
                (packed & 0xFF) as u8,
            ])
        } else {
            None
        }
    }

    /// Get bg RGB at (row, col), if any.
    ///
    /// Returns `None` if `col >= stride` (stale dimensions after resize).
    #[inline]
    pub(crate) fn bg(&self, row: u16, col: u16) -> Option<[u8; 3]> {
        if col >= self.stride {
            return None;
        }
        // Absent plane == all-zero plane: no override stored.
        let plane = self.bg.as_deref()?;
        let idx = self.index(row, col);
        let packed = *plane.get(idx)?;
        if packed != 0 {
            Some([
                ((packed >> 16) & 0xFF) as u8,
                ((packed >> 8) & 0xFF) as u8,
                (packed & 0xFF) as u8,
            ])
        } else {
            None
        }
    }

    /// Store fg RGB at (row, col). No-op if `col >= stride` (stale dimensions after
    /// resize).
    ///
    /// TEST-ONLY per-cell reference oracle: production writes go through the bulk
    /// [`Self::fill_fg_run`] (one slice `fill` per run), and the tests pin that the run
    /// path is byte-identical to looping this. Kept as an INDEPENDENT implementation
    /// (its own inline packing, not `pack_rgb`) so the parity test cross-checks two
    /// distinct code paths.
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

    /// Store bg RGB at (row, col). No-op if `col >= stride` (stale dimensions after
    /// resize). TEST-ONLY per-cell reference oracle — see [`Self::set_fg`]; production
    /// writes go through [`Self::fill_bg_run`].
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

    /// Pack `rgb` the way `set_fg`/`set_bg` do: `0x01RRGGBB`, the `0x01` high byte
    /// marking "true-colour override present" (so a stored pure black `0x00000000`
    /// still reads back as set, distinct from the absent-entry 0).
    #[inline(always)]
    fn pack_rgb(rgb: [u8; 3]) -> u32 {
        0x01_000000 | ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | (rgb[2] as u32)
    }

    /// Store fg RGB across the column run `[col_start, col_end)` of `row` in ONE
    /// `fill`, byte-identical to calling `set_fg` on each column: same `0x01RRGGBB`
    /// packing, the same `col >= stride` silent-skip (the run is clamped to `stride`),
    /// and the same lazy plane allocation — skipped entirely when the whole run is out
    /// of range, exactly as the per-cell path never allocates on a fully out-of-range
    /// write. Every column of one row shares a single `ring_row`, so the flat indices
    /// are contiguous and a slice fill is exact. (Parity with the per-cell path is
    /// pinned by `fill_run_matches_per_cell_set` in the tests below.)
    #[inline]
    fn fill_fg_run(&mut self, row: u16, col_start: u16, col_end: u16, rgb: [u8; 3]) {
        let end = col_end.min(self.stride);
        if col_start >= end {
            return; // empty / wholly out-of-range run — no allocation (like set_fg)
        }
        let packed = Self::pack_rgb(rgb);
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let (lo, hi) = (base + col_start as usize, base + end as usize);
        let plane = self.fg_mut();
        if hi <= plane.len() {
            plane[lo..hi].fill(packed);
        }
    }

    /// [`Self::fill_fg_run`] for the bg plane — same contract, byte-identical to
    /// per-column `set_bg`.
    #[inline]
    fn fill_bg_run(&mut self, row: u16, col_start: u16, col_end: u16, rgb: [u8; 3]) {
        let end = col_end.min(self.stride);
        if col_start >= end {
            return;
        }
        let packed = Self::pack_rgb(rgb);
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let (lo, hi) = (base + col_start as usize, base + end as usize);
        let plane = self.bg_mut();
        if hi <= plane.len() {
            plane[lo..hi].fill(packed);
        }
    }

    /// Scroll up by `n` rows: recycle the top `n` rows by clearing them.
    pub(crate) fn scroll_up(&mut self, n: u16) {
        // Over-scroll: when the entire visible area (or more) scrolls off, the
        // ring is fully recycled and the offset must reset to 0. The single-
        // subtraction `ring_row` only normalizes inputs < 2*visible_rows, so a
        // huge `n` (e.g. a full-screen `CSI 65535 S`) would otherwise leave
        // `ring_offset >= visible_rows`, corrupting every subsequent index.
        if n >= self.visible_rows {
            self.clear();
            return;
        }
        let stride = self.stride as usize;
        for i in 0..n {
            let old_row = self.ring_row(i);
            let start = (old_row as usize) * stride;
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
        self.ring_offset = self.ring_row(n);
    }

    /// Clear all entries.
    pub(crate) fn clear(&mut self) {
        if let Some(fg) = self.fg.as_deref_mut() {
            fg.fill(0);
        }
        if let Some(bg) = self.bg.as_deref_mut() {
            bg.fill(0);
        }
        self.ring_offset = 0;
    }

    /// Clear RGB entries for a column range on a single row.
    ///
    /// Used by erase operations to remove stale truecolor data when cells
    /// are cleared. Without this, erased cells retain ring-buffer RGB values
    /// that shadow the cell's actual (default) colors (#7697).
    ///
    /// One `fill(0)` per plane, byte-identical to the old per-column loop and
    /// mirroring `clear_row`/`fill_fg_run`: every column of one row shares a
    /// single `ring_row`, so the touched flat indices are contiguous and the
    /// per-column `index()` re-derivation (wrapping add, compare, conditional
    /// subtract, imul) is hoisted to one `ring_row` call. EL-0/EL-1/ECH run this
    /// once per erase over up to `cols` columns, so the scalar loop was a real
    /// term next to the memset-class row clear it accompanies.
    ///
    /// The all-or-nothing `hi <= plane.len()` guard is exactly equivalent to the
    /// old per-element `get_mut` checks: `stride`/`visible_rows` are written once
    /// in `new` and never reassigned (resize drops the whole ring via
    /// `invalidate_rings`), and each plane is allocated once as
    /// `vec![0u32; capacity()].into_boxed_slice()`, with
    /// `capacity() == visible_rows * stride`. So with `end <= stride`:
    /// `ring_row < visible_rows` ⇒ `base + end <= len` (guard passes, identical
    /// indices), and `ring_row >= visible_rows` ⇒ `base >= len` (the old loop
    /// stored nothing either). A PARTIAL clear is unreachable — a future
    /// resize-in-place of the ring would have to revisit this.
    #[inline]
    pub(crate) fn clear_range(&mut self, row: u16, start_col: u16, end_col: u16) {
        // Clamp exactly like the old per-column `col >= self.stride` break.
        let end = end_col.min(self.stride);
        if start_col >= end {
            return;
        }
        // Both computed BEFORE the plane borrow (they read `self`).
        let base = (self.ring_row(row) as usize) * (self.stride as usize);
        let (lo, hi) = (base + start_col as usize, base + end as usize);
        // Clearing an absent plane is a no-op (already all-zero) — `as_deref_mut`,
        // never `fg_mut()`/`bg_mut()`, which would allocate a `capacity * 4 B`
        // plane on every EL of an uncoloured screen.
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

    /// Clear RGB entries for an entire row.
    ///
    /// Faster than `clear_range(row, 0, stride)` — uses a single `fill(0)`
    /// on the contiguous slice (#7697).
    #[inline]
    pub(crate) fn clear_row(&mut self, row: u16) {
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

    /// Clear RGB entries for a range of rows.
    ///
    /// Delegates to `clear_row` per row (#7697).
    #[inline]
    pub(crate) fn clear_rows(&mut self, rows: core::ops::Range<u16>) {
        for row in rows {
            self.clear_row(row);
        }
    }

    /// Clear RGB entries inside a rectangular area.
    ///
    /// Delegates to `clear_range` per row (#7697).
    pub(crate) fn clear_rect(&mut self, rows: core::ops::Range<u16>, cols: core::ops::Range<u16>) {
        for row in rows {
            self.clear_range(row, cols.start, cols.end);
        }
    }

    /// Copy one external row's `[left, right]` column span from `src_row` into
    /// `dst_row`, preserving the packed fg/bg values exactly. Source cells in
    /// the span are NOT cleared (the caller handles vacated cells separately,
    /// matching the HashMap drain-rebuild which only inserts moved entries).
    #[inline]
    fn copy_row_span(&mut self, src_row: u16, dst_row: u16, left: u16, right: u16) {
        let stride = self.stride;
        let hi = right.min(stride.saturating_sub(1));
        let src_base = (self.ring_row(src_row) as usize) * (stride as usize);
        let dst_base = (self.ring_row(dst_row) as usize) * (stride as usize);
        // Operate per-plane: an absent plane is all-zero, so copying it would be
        // a no-op (dst stays absent/all-zero) — we simply skip it.
        Self::copy_span_in_plane(self.fg.as_deref_mut(), src_base, dst_base, left, hi);
        Self::copy_span_in_plane(self.bg.as_deref_mut(), src_base, dst_base, left, hi);
    }

    /// Copy `[left, hi]` columns from `src_base` to `dst_base` within one plane.
    #[inline]
    fn copy_span_in_plane(
        plane: Option<&mut [u32]>,
        src_base: usize,
        dst_base: usize,
        left: u16,
        hi: u16,
    ) {
        let Some(plane) = plane else { return };
        if left > hi {
            return;
        }
        // The span `[left, hi]` is contiguous within each row for both src and
        // dst, so a single memmove (`copy_within`) replaces the per-cell loop.
        // Keep a runtime all-or-nothing bounds guard (matching clear_row/
        // clear_range) so degenerate dims silently skip rather than panic.
        let span = (hi - left) as usize + 1;
        let s0 = src_base + left as usize;
        let d0 = dst_base + left as usize;
        if s0 + span <= plane.len() && d0 + span <= plane.len() {
            plane.copy_within(s0..s0 + span, d0);
        }
    }

    /// Region shift up by `n` within rows `[top, bottom]` (full-width).
    ///
    /// Mirrors `CellExtras::shift_region_up_by`: rows `[top, top+n)` are
    /// cleared, rows `[top+n, bottom]` move up by `n`, vacated bottom rows
    /// `[bottom-n+1, bottom]` are cleared. Rows outside `[top, bottom]` are
    /// untouched — preserving their truecolor (#7458 follow-up: do not wipe
    /// the whole ring).
    pub(crate) fn shift_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        if n == 0 || top > bottom {
            return;
        }
        let region = bottom - top + 1;
        let n = n.min(region);
        // Move surviving rows up: dst in [top, bottom-n], src = dst+n.
        if let Some(shift_start) = top.checked_add(n) {
            let mut dst = top;
            let mut src = shift_start;
            while src <= bottom {
                self.copy_row_span(src, dst, 0, self.stride.saturating_sub(1));
                dst += 1;
                src += 1;
            }
        }
        // Clear the n vacated rows at the bottom of the region.
        let clear_from = bottom + 1 - n;
        for row in clear_from..=bottom {
            self.clear_row(row);
        }
    }

    /// Region shift down by `n` within rows `[top, bottom]` (full-width).
    ///
    /// Mirrors `CellExtras::shift_region_down_by`: rows `[top, bottom-n]` move
    /// down by `n`, rows `[bottom-n+1, bottom]` are dropped, vacated top rows
    /// `[top, top+n)` are cleared. Rows outside `[top, bottom]` are untouched.
    pub(crate) fn shift_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        if n == 0 || top > bottom {
            return;
        }
        let region = bottom - top + 1;
        let n = n.min(region);
        let drop_start = bottom + 1 - n; // first dst row at/below this is out of region top
        // Move surviving rows down, iterating from the bottom to avoid clobber.
        // src in [top, drop_start-1] (== bottom-n), dst = src+n.
        let mut src = drop_start; // == bottom - n + 1; survivors are src-1 downwards
        while src > top {
            src -= 1;
            let dst = src + n;
            if dst <= bottom {
                self.copy_row_span(src, dst, 0, self.stride.saturating_sub(1));
            }
        }
        // Clear the n vacated rows at the top of the region.
        let clear_to = (top + n).min(bottom + 1);
        for row in top..clear_to {
            self.clear_row(row);
        }
    }

    /// Rectangular shift up by `n` within rows `[top, bottom]` × cols
    /// `[left, right]` (DECLRMM). Mirrors `CellExtras::shift_rect_up_by`.
    pub(crate) fn shift_rect_up(&mut self, top: u16, bottom: u16, left: u16, right: u16, n: u16) {
        if n == 0 || top > bottom || left > right {
            return;
        }
        let region = bottom - top + 1;
        let n = n.min(region);
        if let Some(shift_start) = top.checked_add(n) {
            let mut dst = top;
            let mut src = shift_start;
            while src <= bottom {
                self.copy_row_span(src, dst, left, right);
                dst += 1;
                src += 1;
            }
        }
        let clear_from = bottom + 1 - n;
        for row in clear_from..=bottom {
            self.clear_range(row, left, right.saturating_add(1));
        }
    }

    /// Rectangular shift down by `n` within rows `[top, bottom]` × cols
    /// `[left, right]` (DECLRMM). Mirrors `CellExtras::shift_rect_down_by`.
    pub(crate) fn shift_rect_down(&mut self, top: u16, bottom: u16, left: u16, right: u16, n: u16) {
        if n == 0 || top > bottom || left > right {
            return;
        }
        let region = bottom - top + 1;
        let n = n.min(region);
        let drop_start = bottom + 1 - n;
        let mut src = drop_start;
        while src > top {
            src -= 1;
            let dst = src + n;
            if dst <= bottom {
                self.copy_row_span(src, dst, left, right);
            }
        }
        let clear_to = (top + n).min(bottom + 1);
        for row in top..clear_to {
            self.clear_range(row, left, right.saturating_add(1));
        }
    }

    /// Shift columns right within a single row for ICH (Insert Character).
    ///
    /// Mirrors `CellExtras::shift_cols_right`: columns in
    /// `[start_col, max_col-count)` move right by `count`; columns in
    /// `[start_col, start_col+count)` are cleared; columns shifted past
    /// `max_col` are dropped.
    pub(crate) fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let stride = self.stride;
        let hi = max_col.min(stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (stride as usize);
        // Per-plane: an absent plane is all-zero, so both the move and the
        // blank-fill are no-ops on it — skip absent planes entirely.
        Self::shift_cols_right_in_plane(self.fg.as_deref_mut(), base, start_col, count, hi);
        Self::shift_cols_right_in_plane(self.bg.as_deref_mut(), base, start_col, count, hi);
    }

    #[inline]
    fn shift_cols_right_in_plane(
        plane: Option<&mut [u32]>,
        base: usize,
        start_col: u16,
        count: u16,
        hi: u16,
    ) {
        let Some(plane) = plane else { return };
        // Iterate destinations from the right edge down so a moved value never
        // clobbers a not-yet-read source.
        let mut col = hi; // exclusive upper bound for destinations
        while col > start_col {
            col -= 1;
            // This destination col is filled from col - count (if in range).
            if col >= start_col + count {
                let s = base + (col - count) as usize;
                let d = base + col as usize;
                if s < plane.len() && d < plane.len() {
                    plane[d] = plane[s];
                }
            } else {
                // Newly inserted blank column.
                let d = base + col as usize;
                if d < plane.len() {
                    plane[d] = 0;
                }
            }
        }
    }

    /// Shift columns left within a single row for DCH (Delete Character).
    ///
    /// Mirrors `CellExtras::shift_cols_left`: columns in
    /// `[start_col, start_col+count)` are deleted; columns in
    /// `[start_col+count, max_col)` move left by `count`. Columns at/after
    /// `max_col` are preserved in place.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        if count == 0 {
            return;
        }
        let stride = self.stride;
        let hi = max_col.min(stride);
        if start_col >= hi {
            return;
        }
        let base = (self.ring_row(row) as usize) * (stride as usize);
        // Per-plane: an absent plane is all-zero, so both the move and the
        // blank-fill are no-ops on it — skip absent planes entirely.
        Self::shift_cols_left_in_plane(self.fg.as_deref_mut(), base, start_col, count, hi);
        Self::shift_cols_left_in_plane(self.bg.as_deref_mut(), base, start_col, count, hi);
    }

    #[inline]
    fn shift_cols_left_in_plane(
        plane: Option<&mut [u32]>,
        base: usize,
        start_col: u16,
        count: u16,
        hi: u16,
    ) {
        let Some(plane) = plane else { return };
        // Iterate destinations left-to-right so a moved value never clobbers a
        // not-yet-read source.
        let mut col = start_col;
        while col < hi {
            let src_col = col.saturating_add(count);
            let d = base + col as usize;
            if src_col < hi {
                let s = base + src_col as usize;
                if s < plane.len() && d < plane.len() {
                    plane[d] = plane[s];
                }
            } else {
                // No source: this column becomes blank.
                if d < plane.len() {
                    plane[d] = 0;
                }
            }
            col += 1;
        }
    }
}

/// Compact after this many accumulated scroll rows.
///
/// Chosen to be large enough that compaction is rare (amortized O(1) per
/// scroll) but small enough that stale entry memory waste is bounded and
/// offset + max_screen_height never approaches u16 overflow.
const COMPACT_THRESHOLD: u16 = 256;

/// Maximum number of entries with hyperlinks allowed in `CellExtras`.
///
/// When this limit is exceeded, the oldest hyperlink-bearing entries are
/// evicted to prevent unbounded memory growth from malicious OSC 8 spam.
/// Cells that lose their hyperlink degrade gracefully: text remains visible,
/// only the clickable link is removed.
///
/// 10,000 entries covers a 200-column x 50-row terminal fully linked, with
/// headroom for stale entries awaiting compaction.
///
/// Part of #7172 (P1: unbounded hyperlink storage).
pub(crate) const MAX_HYPERLINK_ENTRIES: usize = 10_000;

/// Unified per-cell extras snapshot for render/FFI hot paths.
///
/// Collapses ring-buffer and HashMap reads into a single accessor so callers
/// do not re-probe the same storage layers for complex chars, RGB overflow,
/// and `CellExtra`.
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
/// Uses FxHashMap for O(1) lookup with fast non-cryptographic hashing.
/// FxHashMap is 2-3x faster than std HashMap for small keys like (u16, u16).
/// Most cells have no extras, so this is more memory-efficient than storing extras inline.
///
/// Internally, row coordinates are offset by `row_offset` for O(1) scroll
/// amortization. See module docs for details.
#[derive(Debug, Clone, Default)]
pub struct CellExtras {
    /// Map from cell coordinate to extra data.
    ///
    /// **Warning:** Keys use internal coordinates (shifted by `row_offset`).
    /// Use the accessor methods (`get`, `set`, `iter`, etc.) for correct
    /// external-to-internal translation. Direct `data` access requires
    /// calling `compact()` first to reset `row_offset` to 0.
    pub(crate) data: FxHashMap<CellCoord, CellExtra>,
    /// Accumulated row offset for O(1) full-screen scroll.
    ///
    /// External row `r` maps to internal key `r.wrapping_add(row_offset)`.
    row_offset: u16,
    /// Dense ring buffer for non-BMP complex chars.
    ///
    /// Allocated on first non-BMP write. Uses O(1) flat-array indexing
    /// instead of HashMap probing for the emoji write hot path.
    /// Falls back to HashMap for cells that also have style extras.
    complex_ring: Option<Box<ComplexCharRing>>,
    /// Dense ring buffer for RGB overflow colors.
    ///
    /// Allocated on first RGB-only extras write. Bypasses HashMap for cells
    /// whose only extra is fg/bg RGB color (~15ns → ~2ns per cell).
    rgb_ring: Option<Box<RgbColorRing>>,
    /// Resident scratch for [`Self::enforce_hyperlink_limit_cold`]'s candidate
    /// walk. Always empty between calls — it exists only to carry CAPACITY.
    ///
    /// That cold path runs once per written OSC 8 run and after every row
    /// shift, and it used to `collect()` a fresh `Vec<CellCoord>` on every
    /// call. Measured on `hyperlink_screen/mixed_extras_under_limit` (a map of
    /// 14_600 entries holding only 600 hyperlinks, i.e. over the map guard but
    /// far under the hyperlink budget): 1_835 such allocations per MiB, every
    /// one of them freed without a single entry being evicted.
    ///
    /// Capacity is retained rather than shrunk, deliberately: the grids that
    /// grow it past a few hundred coords are exactly the ones holding >10_000
    /// hyperlink entries in `data`, where the map itself is an order of
    /// magnitude larger than this buffer's worst case (~40 KiB). Shrinking it
    /// would just reintroduce the churn on the eviction path, which is the one
    /// path that refills it every time.
    hyperlink_scratch: Vec<CellCoord>,
    /// UPPER BOUND on the number of HYPERLINK-BEARING entries in `data`.
    ///
    /// The quantity [`Self::enforce_hyperlink_limit`] bounds is the hyperlink
    /// population, but the only O(1) number the map itself offers is
    /// `data.len()` — which also counts combining marks, underline colours,
    /// extended flags, kitty placeholders and inline-image refs (one
    /// `place_image` inserts ~3 * rows * cols of them). Guarding on `data.len()`
    /// therefore collapses to a full O(E) walk per hyperlinked write-run as soon
    /// as anything ELSE fills the map, and that walk evicts nothing: measured on
    /// `hyperlink_screen/mixed_extras_under_limit` at 1_835 futile walks per MiB
    /// over 14_600 entries, zero evictions.
    ///
    /// This field tracks the right quantity instead. The invariant is
    /// `hyperlink_upper_bound >= (entries in data whose hyperlink is Some)`:
    ///
    /// * every path that can RAISE the hyperlink population charges it here
    ///   (`get_or_create`, `set`, `set_range_uniform`) — a missed charge would
    ///   lose the memory bound, so those are mandatory;
    /// * paths that LOWER it (row shifts dropping entries, the retain-based
    ///   clears, compaction) need not report, because leaving the bound high is
    ///   merely conservative — it buys one cold walk, which then repairs the
    ///   bound EXACTLY from the walk it was already doing.
    ///
    /// So the structure is self-healing: drift is always in the safe direction
    /// and is erased the next time the cold path runs. `clear()` resets it to 0
    /// because it replaces the whole map.
    hyperlink_upper_bound: usize,
    /// STICKY: an inline image has been placed into this collection at some
    /// point. Never cleared except by [`Self::clear`], which replaces the map.
    ///
    /// The scroll-off path has to look for image refs over the FULL physical row
    /// width, because `place_image` writes no glyph — an image-only row has
    /// `Row::len() == 0`, so the text-driven extraction walk cannot reach the
    /// cells that carry the picture. Without a gate that scan would run for
    /// every scrolled row of every session whose map is merely non-empty (one
    /// hyperlink or one combining mark anywhere on screen is enough), inside the
    /// PTY reader's `term_lock` hold. This is the same shape as the
    /// `complex_ring`/`rgb_ring` gates: sticky and conservative, so a session
    /// that never draws a picture pays exactly one bool test per scrolled row,
    /// and one that draws a single picture pays a bit-test scan it would have
    /// paid anyway while the image was live.
    any_image: bool,
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
            // Unallocated until the hyperlink cold path first runs; a grid that
            // never sees an OSC 8 link never pays a byte for it.
            hyperlink_scratch: Vec::new(),
            hyperlink_upper_bound: 0,
            any_image: false,
        }
    }

    /// Store a complex char codepoint in the dense ring buffer.
    ///
    /// Allocates the ring on first use. O(1) flat-array write.
    /// Stores the raw `char` codepoint — no `Arc<str>` allocation or
    /// atomic refcounting, making this ~5ns faster per emoji write.
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
    ///
    /// Hoists the ring_row computation to avoid per-char overhead.
    #[inline]
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

    /// Store non-BMP chars from a mixed BMP/non-BMP wide-char run.
    ///
    /// Hoists ring_row once. Only writes entries for chars > U+FFFF.
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
        let entries_len = ring.entries.len();
        for (j, &c) in chars.iter().enumerate() {
            if (c as u32) > 0xFFFF {
                let idx = base + (start_col as usize) + j * 2;
                if idx < entries_len {
                    // SAFETY: `idx < entries_len` is verified on the preceding
                    // line, so `get_unchecked_mut(idx)` is within the bounds of
                    // `ring.entries`. The mutable borrow is exclusive because
                    // we hold `&mut self` on `ExtraCollection`.
                    unsafe {
                        *ring.entries.get_unchecked_mut(idx) = c;
                    }
                }
            }
        }
    }

    /// Look up a complex char codepoint: ring buffer first (O(1)), then HashMap.
    ///
    /// Returns the codepoint from the ring if present, otherwise falls back
    /// to the HashMap for cells with combined extras (hyperlinks + complex char).
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

    /// Look up a complex char as `Arc<str>`: HashMap only (ring stores codepoints).
    ///
    /// Used by scrollback materialization which needs the full `Arc<str>`.
    /// For most callers that just need the codepoint, use `complex_codepoint_for`.
    #[inline]
    pub fn complex_char_arc_for(&self, row: u16, col: u16) -> Option<&Arc<str>> {
        self.data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.complex_char())
    }

    /// Look up a complex char as a full `String`: ring buffer or HashMap.
    ///
    /// Used by text extraction (row_text, content export) which needs the
    /// complete string representation. More expensive than `complex_codepoint_for`.
    pub fn complex_char_str_for(&self, row: u16, col: u16) -> Option<String> {
        if let Some(ring) = &self.complex_ring
            && let Some(c) = ring.get(row, col)
        {
            return Some(c.to_string());
        }
        self.data
            .get(&self.internal_coord(CellCoord::new(row, col)))
            .and_then(|e| e.complex_char())
            .map(|s| s.to_string())
    }

    /// Store RGB fg/bg in the dense ring buffer for a column range.
    ///
    /// Allocates the ring on first use. O(1) per cell. Used by
    /// `set_range_uniform` for RGB-only extras (no hyperlinks, no underline).
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
        // One contiguous slice fill per plane instead of a per-cell store — the whole
        // run shares a `ring_row`, so this is byte-identical to the old cell-by-cell
        // loops (see `fill_fg_run`/`fill_bg_run`) while cutting the per-cell overhead on
        // wide truecolor spans (24-bit-coloured `cat`/scroll: bat, delta, vim tgc).
        if let Some(rgb) = fg_rgb {
            ring.fill_fg_run(row, col_start, col_end, rgb);
        }
        if let Some(rgb) = bg_rgb {
            ring.fill_bg_run(row, col_start, col_end, rgb);
        }
    }

    /// Clear the dense RGB-ring fg/bg entries for a `width`-cell span starting
    /// at `(row, col)`, if a ring exists.
    ///
    /// Used by the per-character ("slow") write-split path to drop stale
    /// truecolor overflow when overwriting an already-occupied cell. The fast
    /// bulk paths and the BCE/erase path populate the ring directly
    /// (`set_rgb_ring_range`), but the slow path applies the *new* cell's RGB
    /// via the HashMap (`apply_cell_extras_preflagged`). Since reads consult
    /// the ring FIRST (`bg_rgb_for`/`render_data_for_cell`), a leftover ring
    /// entry would shadow the freshly-written HashMap color. Clearing the ring
    /// slot here mirrors the stale-HashMap-entry drop already done on overwrite
    /// (#7456), keeping the two truecolor stores consistent.
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

    /// Read all render-relevant extras for a cell in one coordinated pass.
    ///
    /// Uses the cell's flags to decide which storage layers are relevant:
    /// - `HAS_EXTRAS` requests the `CellExtra` reference.
    /// - `COMPLEX` requests the complex-char ring/HashMap fallback.
    /// - RGB overflow flags request fg/bg ring/HashMap fallback.
    ///
    /// This keeps ring-only cells on the flat-array fast path while ensuring
    /// callers pay for at most one HashMap probe when fallback data is needed.
    #[must_use]
    #[inline]
    pub fn render_data_for_cell(&self, row: u16, col: u16, cell: Cell) -> CellRenderData<'_> {
        let wants_extra = cell.has_extras();
        let wants_complex = cell.is_complex();
        let wants_fg_rgb = !cell.uses_style_id() && cell.fg_needs_overflow();
        let wants_bg_rgb = !cell.uses_style_id() && cell.bg_needs_overflow();

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

    /// Returns `true` when no cell has extras data.
    ///
    /// Used by the FFI render path to skip per-cell hash probes when the
    /// map is empty — the common case for plain-text terminal output.
    ///
    /// Note: may return `false` when only stale (scrolled-off) entries remain.
    /// This is conservative — callers that skip work on `true` are correct.
    ///
    /// **Important:** This only checks the HashMap, not the ring buffers.
    /// Use [`has_any_data`] when you need to know if *any* storage (HashMap
    /// or ring buffers) contains entries.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns `true` when any storage layer (HashMap, complex ring, or RGB
    /// ring) contains data.
    ///
    /// Unlike [`is_empty`], this accounts for ring-buffer-only entries that
    /// bypass the HashMap on the write hot path.
    #[must_use]
    #[inline]
    pub fn has_any_data(&self) -> bool {
        !self.data.is_empty() || self.complex_ring.is_some() || self.rgb_ring.is_some()
    }

    /// Iterate over all valid (non-stale) (coord, extra) pairs.
    ///
    /// Coordinates are translated back to external (caller-visible) values.
    /// Used by checkpoint serialization to persist extras (#4423).
    pub fn iter(&self) -> impl Iterator<Item = (CellCoord, &CellExtra)> + '_ {
        let offset = self.row_offset;
        self.data.iter().filter_map(move |(coord, extra)| {
            // Stale entries have internal_row < offset (they scrolled off).
            if offset > 0 && coord.row < offset {
                return None;
            }
            let external = CellCoord::new(coord.row.wrapping_sub(offset), coord.col);
            Some((external, extra))
        })
    }

    /// Get extras for a cell, if any.
    #[must_use]
    #[inline]
    pub fn get(&self, coord: CellCoord) -> Option<&CellExtra> {
        self.data.get(&self.internal_coord(coord))
    }

    /// Get mutable extras for a cell, creating if needed.
    ///
    /// CHARGES THE HYPERLINK BUDGET when the entry does not already carry a
    /// link. A `&mut CellExtra` handed out here can have a hyperlink written
    /// into it — `handler_write`'s per-cell OSC 8 path and the checkpoint
    /// restore in `scroll_fill` both do exactly that — and `CellExtra` has no
    /// way to report that back to the collection that owns it. Charging on
    /// handout is the only sound accounting: an entry that ALREADY has a link
    /// can only keep it (no change) or drop it (which just leaves the bound
    /// conservative), so the `is_none()` case is precisely the set of handouts
    /// that can raise the population.
    ///
    /// Over-charging costs one cold walk per `MAX_HYPERLINK_ENTRIES` handouts,
    /// and that walk repairs the bound exactly; under-charging would lose the
    /// memory bound entirely. The asymmetry is deliberate.
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

    /// Whether an inline image has EVER been placed here (see
    /// [`any_image`](Self::any_image) field docs).
    ///
    /// The scroll-off image extraction consults this before scanning a row, so
    /// a session with no picture in it never pays for the scan. `false` proves
    /// no cell carries an image ref; `true` merely permits the scan.
    #[must_use]
    #[inline]
    pub fn any_image(&self) -> bool {
        self.any_image
    }

    /// Attach an inline-image ref to a cell, arming the scroll-off scan gate.
    ///
    /// The one entry point for placing a picture, so the gate cannot be missed:
    /// `CellExtra::set_image` on a `&mut` handed out by `get_or_create` would
    /// store the ref with no way to tell the collection about it, and the ref
    /// would then be invisible to the scroll-off extraction that must carry it
    /// into history.
    #[inline]
    pub fn set_image(&mut self, coord: CellCoord, image: crate::extra::ImageRef) {
        self.any_image = true;
        self.get_or_create(coord).set_image(Some(image));
    }

    /// Set extras for a cell.
    ///
    /// If the extra has no data, removes the entry to save memory.
    #[inline]
    pub fn set(&mut self, coord: CellCoord, extra: CellExtra) {
        let internal = self.internal_coord(coord);
        if extra.has_data() {
            // Charge only for a value that actually carries a link: DECCRA's
            // rect copy and the RGB-only seeding in `extras_shift` store
            // link-free extras through here and must not inflate the bound.
            if extra.hyperlink().is_some() {
                self.hyperlink_upper_bound += 1;
            }
            // Arm the scroll-off image scan for the bulk path too: checkpoint
            // restore and DECCRA's rect copy both re-enter whole `CellExtra`
            // values here, and one of them may carry a picture.
            if extra.image().is_some() {
                self.any_image = true;
            }
            self.data.insert(internal, extra);
        } else {
            self.data.remove(&internal);
        }
    }

    /// Remove extras for a single cell.
    ///
    /// Returns `true` if an entry was present and removed. Used by the grid
    /// write paths to drop stale entries when overwriting extras-bearing
    /// cells (#7456).
    #[inline]
    pub(crate) fn remove(&mut self, coord: CellCoord) -> bool {
        let key = self.internal_coord(coord);
        let Some(extra) = self.data.remove(&key) else {
            return false;
        };
        // Give the charge back. This is the OSC 8 overwrite path — `write_split`
        // drops the stale entry of a cell it is about to rewrite — so without
        // the refund a screen that repaints links in place would inflate the
        // bound by one per rewritten cell and buy a futile cold walk it does not
        // need. The value is already in cache (it is being dropped here), so the
        // test is a load and a branch. `saturating_sub` because the bound is a
        // bound, not a ledger: it must never wrap, and a refund that would take
        // it below zero can only mean it was already conservative.
        if extra.hyperlink().is_some() {
            self.hyperlink_upper_bound = self.hyperlink_upper_bound.saturating_sub(1);
        }
        true
    }

    /// Strip character-bearing fields (combining marks + complex-char string)
    /// from a cell's extra, preserving SGR colors and every other overflow
    /// field.
    ///
    /// Returns `true` when, after scrubbing, the entry holds no data and the
    /// caller should drop it (and clear the cell's HAS_EXTRAS flag to keep the
    /// flag ⇔ entry-exists invariant). Returns `false` when there is no entry
    /// or color-bearing data remains. Used by DECSERA's selective wipe to keep
    /// truecolor/underline overflow while removing stale combining marks.
    pub(crate) fn scrub_char_marks(&mut self, row: u16, col: u16) -> bool {
        let coord = self.internal_coord(CellCoord::new(row, col));
        let Some(extra) = self.data.get_mut(&coord) else {
            return false;
        };
        extra.clear_char_marks();
        !extra.has_data()
    }

    /// Clear extras for a single row.
    ///
    /// Called when a row is cleared or scrolls off.
    /// Also clears the RGB ring buffer to prevent stale truecolor data (#7697).
    pub(crate) fn clear_row(&mut self, row: u16) {
        // Clear RGB ring unconditionally — it may have data even when the
        // HashMap is empty (RGB-only writes bypass the HashMap).
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_row(row);
        }
        if self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let internal_row = self.internal_row(row);
        self.data.retain(|coord, _| coord.row != internal_row);
        self.maybe_shrink();
    }

    /// Clear extras for a range of columns in a row.
    ///
    /// Also clears the RGB ring buffer for the range (#7697).
    pub fn clear_range(&mut self, row: u16, start_col: u16, end_col: u16) {
        // Clear RGB ring unconditionally — it may have data even when the
        // HashMap is empty (RGB-only writes bypass the HashMap).
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_range(row, start_col, end_col);
        }
        if self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let internal_row = self.internal_row(row);
        // The retain predicate is exactly membership in
        // `{(internal_row, c) : start_col <= c < end_col}`, and the map is KEYED
        // by that coordinate — so delete by coordinate (O(range) probes) rather
        // than visiting every entry in the grid-wide map. EL/ECH run per erase,
        // and the map spans the whole scrollback-adjacent extras population, so
        // the scan was O(all extras) to remove at most one row's worth.
        //
        // The scan is kept for the case where it is genuinely cheaper: a map
        // smaller than the range walks faster than the range probes it. Both
        // arms remove exactly the same keys, so this is a cost choice only.
        // `clear_row` above deliberately keeps its retain — it has no column
        // bound and must still reach stale entries past the current width.
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

    /// Clear extras for a range of rows in a single pass.
    ///
    /// Also clears the RGB ring buffer for the rows (#7697).
    pub(crate) fn clear_rows(&mut self, rows: core::ops::Range<u16>) {
        // Clear RGB ring unconditionally — it may have data even when the
        // HashMap is empty (RGB-only writes bypass the HashMap).
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_rows(rows.clone());
        }
        if rows.is_empty() || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let offset = self.row_offset;
        self.data.retain(|coord, _| {
            let external = coord.row.wrapping_sub(offset);
            !rows.contains(&external)
        });
        self.maybe_shrink();
    }

    /// Clear extras inside a rectangular area in a single pass.
    ///
    /// Also clears the RGB ring buffer (#7697) and the complex-char ring
    /// (#7977) for the area — both may hold data even when the HashMap is
    /// empty (the bulk/fast write paths populate the rings directly), so a
    /// rect overwrite (e.g. DECCRA) must wipe them or a complex/truecolor dst
    /// cell would resolve to the stale prior dst value.
    pub(crate) fn clear_rect(&mut self, rows: core::ops::Range<u16>, cols: core::ops::Range<u16>) {
        // Clear RGB ring unconditionally — it may have data even when the
        // HashMap is empty (RGB-only writes bypass the HashMap).
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear_rect(rows.clone(), cols.clone());
        }
        // Clear the complex-char ring for the same reason: a complex cell's
        // codepoint lives in the dense ring (#7977), not the HashMap.
        if let Some(ring) = &mut self.complex_ring {
            ring.clear_rect(rows.clone(), cols.clone());
        }
        if rows.is_empty() || cols.is_empty() || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_clear_ops(self.data.len());
        let offset = self.row_offset;
        self.data.retain(|coord, _| {
            let external_row = coord.row.wrapping_sub(offset);
            !(rows.contains(&external_row) && cols.contains(&coord.col))
        });
        self.maybe_shrink();
    }

    /// Batch shift rows up by `n` (for multi-line scroll).
    ///
    /// Rows in `[start_row, start_row + n)` are deleted.
    /// Rows >= `start_row + n` are shifted up by `n`.
    /// Rows < `start_row` are preserved unchanged.
    ///
    /// When `start_row == 0` (full-screen scroll), uses O(1) offset
    /// amortization: just bumps `row_offset` without touching entries.
    /// Compacts every `COMPACT_THRESHOLD` accumulated scroll rows.
    ///
    /// When `start_row > 0`, compacts first then uses drain-rebuild.
    pub fn shift_rows_up_by(&mut self, start_row: u16, n: u16) {
        // Scroll the complex char ring buffer.
        if let Some(ring) = &mut self.complex_ring {
            if start_row == 0 {
                ring.scroll_up(n);
            } else {
                ring.clear();
            }
        }
        // Scroll the RGB color ring buffer.
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
                #[cfg(any(test, feature = "testing"))]
                crate::test_counters::count_extras_shift_ops(0);
                self.row_offset = new_offset;
                return;
            }
            // Threshold exceeded or u16 overflow: compact + shift in one pass.
            self.apply_offset_and_shift(n);
            return;
        }
        // Non-zero start_row: compact first (sets offset=0), then drain-rebuild.
        self.compact();
        #[cfg(any(test, feature = "testing"))]
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
            // Rows in [start_row, start_row + n) are deleted
        }
        self.data = new_data;
    }

    // Region shifts (shift_region_up_by, shift_region_down_by) and column
    // shifts (shift_cols_right, shift_cols_left) are in extra_collection_shifts.rs.

    /// Batch-set uniform extras for a contiguous column range on the same row.
    ///
    /// Faster than per-cell `get_or_create` when all cells in the range share
    /// the same extras (RGB-styled ASCII runs).
    ///
    /// For RGB-only extras (no hyperlinks, underline color, or extended flags),
    /// writes to the dense ring buffer instead of the HashMap (~2ns vs ~15ns per cell).
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

        // Fast path: RGB-only extras use the dense ring buffer (O(1) per cell).
        let is_rgb_only =
            vals.hyperlink.is_none() && vals.underline_color.is_none() && vals.extended_flags == 0;
        if is_rgb_only && (vals.fg_rgb.is_some() || vals.bg_rgb.is_some()) {
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

        // Build the shared (url, id) payload ONCE for the whole run. Previously
        // every covered cell ran `set_hyperlink` on a freshly-defaulted entry,
        // which did its own `HyperlinkData` heap allocation plus a second atomic
        // refcount bump for the id — N mallocs and 2N atomic RMWs to store one
        // immutable pair. The value written per cell is byte-identical: the old
        // pair was `set_hyperlink(url)` (which clears any stale id) followed by
        // `set_hyperlink_id(id)`, i.e. exactly `{url, id}`. Cells that later edit
        // their link individually copy-on-write inside `CellExtra`.
        let hyperlink = vals.hyperlink.map(|url| {
            Arc::new(crate::extra::HyperlinkData {
                url: Arc::clone(url),
                id: vals.hyperlink_id.cloned(),
            })
        });

        let internal_row = self.internal_row(row);
        // EXACT charge for the run: only a cell that did not already carry a
        // link raises the hyperlink population, so repainting the same OSC 8 row
        // (what every TUI redraw does) costs the budget nothing.
        let mut new_links = 0usize;
        for col in col_start..col_end {
            let coord = CellCoord::new(internal_row, col);
            let extra = self.data.entry(coord).or_default();
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
                if extra.hyperlink().is_none() {
                    new_links += 1;
                }
                extra.set_hyperlink_data(Some(Arc::clone(data)));
            }
        }

        // Enforce hyperlink limit after batch insertion (#7172).
        if vals.hyperlink.is_some() {
            self.hyperlink_upper_bound += new_links;
            self.enforce_hyperlink_limit();
        }
    }

    /// Clear all extras, releasing excess capacity.
    ///
    /// Unlike `HashMap::clear()` which retains capacity, this replaces the
    /// map entirely so memory from peak usage is reclaimed. This matters
    /// after hyperlink-heavy sessions where the map may have grown large.
    #[inline]
    pub fn clear(&mut self) {
        self.data = FxHashMap::default();
        self.row_offset = 0;
        // Exact, not conservative: the map is gone, so the hyperlink population
        // is zero. ESC[2J therefore hands the guard a clean slate.
        self.hyperlink_upper_bound = 0;
        // Same reasoning for the image gate: every image ref lived in the map
        // that was just replaced, so there is provably none left to scan for.
        self.any_image = false;
        if let Some(ring) = &mut self.complex_ring {
            ring.clear();
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.clear();
        }
    }

    /// Region shift up by `n` within rows `[top, bottom]` on BOTH rings.
    ///
    /// Replaces the old whole-ring wipe: cells OUTSIDE `[top, bottom]` keep
    /// their truecolor and complex-char ring data, and cells merely shifted
    /// within the region move with the row instead of being dropped (#7458).
    #[inline]
    pub(crate) fn shift_rings_region_up(&mut self, top: u16, bottom: u16, n: u16) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_region_up(top, bottom, n);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_region_up(top, bottom, n);
        }
    }

    /// Region shift down by `n` within rows `[top, bottom]` on BOTH rings.
    #[inline]
    pub(crate) fn shift_rings_region_down(&mut self, top: u16, bottom: u16, n: u16) {
        if let Some(ring) = &mut self.complex_ring {
            ring.shift_region_down(top, bottom, n);
        }
        if let Some(ring) = &mut self.rgb_ring {
            ring.shift_region_down(top, bottom, n);
        }
    }

    /// Rect shift up within `[top, bottom]` × `[left, right]` on BOTH rings.
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

    /// Rect shift down within `[top, bottom]` × `[left, right]` on BOTH rings.
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

    /// Column shift right (ICH) on one row for BOTH rings.
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

    /// Column shift left (DCH) on one row for BOTH rings.
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

    /// Apply accumulated row offset, resetting internal keys to external coords.
    ///
    /// Must be called before any operation that directly accesses `self.data`
    /// with external coordinates (reflow, direct `data.retain`, etc.).
    ///
    /// O(E) drain-rebuild — but only when `row_offset > 0`.
    pub(crate) fn compact(&mut self) {
        self.apply_offset_and_shift(0);
    }

    /// Take the accumulated row offset, resetting it to 0.
    ///
    /// Returns the previous offset. Used by region shift operations to
    /// combine offset compaction and key shifting into a single drain pass.
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

    /// Remap extras coordinates during reflow (#3977).
    ///
    /// Instead of clearing all extras on column resize, preserves extras by
    /// translating coordinates through the reflow mapping. The callback maps
    /// `(old_row, old_col)` to `Some(new_coord)` or `None` (drop the extra).
    /// Extras mapping to rows >= `max_row` are also dropped (truncated rows).
    #[cfg(test)]
    pub(crate) fn remap_reflow(
        &mut self,
        remap_fn: impl Fn(u16, u16) -> Option<CellCoord>,
        max_row: u16,
    ) {
        // Compact first so remap_fn receives external coordinates.
        self.compact();
        if self.data.is_empty() {
            return;
        }
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if let Some(new_coord) = remap_fn(coord.row, coord.col)
                && new_coord.row < max_row
            {
                new_data.insert(new_coord, extra);
            }
        }
        self.data = new_data;
    }

    /// Apply accumulated `row_offset` plus an additional `shift` in a single pass.
    ///
    /// Discards stale entries (internal row < current offset) and entries that
    /// are being scrolled off (external row < shift). Re-keys survivors to
    /// offset 0.
    ///
    /// After re-keying, enforces the hyperlink entry limit to clean up any
    /// hyperlink-bearing entries that accumulated between compactions (#7208).
    /// # The `#[inline(never)]` here is LOAD-BEARING — do not remove it
    ///
    /// This sits on the scroll path, which the mixed-VT workload hammers through
    /// its erase/clear/CUP traffic. Under `lto = true` + `codegen-units = 1` its
    /// body size decides an inlining cascade several frames deep, and the mixed
    /// lane is acutely sensitive to where that cascade lands: before this pin,
    /// **every** edit to [`Self::enforce_hyperlink_limit`] cost ~10-11 % on that
    /// lane — including one that changed no behaviour whatsoever (moving the body
    /// out of line and nothing else). Two controls held flat across the same
    /// runs, so it was neither build noise nor generic layout roulette: a
    /// rebuild-with-no-change, and adding a dead `#[inline(never)]` fn to this
    /// same file. `#[inline]`, `#[inline(never)]` and `#[cold]` on
    /// `enforce_hyperlink_limit` itself all failed to pin it; pinning the CALLER
    /// is what works.
    ///
    /// With the pin, the hyperlink-limit rewrite below is free on the mixed lane
    /// (interleaved A/B: 324.7 -> 327.1 MB/s) while `long_escapes` goes
    /// **130 -> 360 MB/s (2.8x)**. Without it, that same rewrite cost 11 %.
    #[inline(never)]
    fn apply_offset_and_shift(&mut self, shift: u16) {
        self.apply_offset_and_shift_only(shift);
        self.enforce_hyperlink_limit();
    }

    /// The compaction core, WITHOUT the trailing hyperlink-limit enforcement, so
    /// [`Self::enforce_hyperlink_limit_cold`] can compact before it decides
    /// anything without recursing back into itself. `#[inline(never)]` for the
    /// same cascade reason as its caller above.
    #[inline(never)]
    fn apply_offset_and_shift_only(&mut self, shift: u16) {
        let total = match self.row_offset.checked_add(shift) {
            Some(0) => return,
            Some(t) => t,
            None => {
                // Overflow: offset + shift > u16::MAX — all entries scrolled off.
                self.data.clear();
                self.row_offset = 0;
                return;
            }
        };
        #[cfg(any(test, feature = "testing"))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row >= total {
                new_data.insert(CellCoord::new(coord.row - total, coord.col), extra);
            }
            // Entries with internal row < total are stale or scrolled off
        }
        self.data = new_data;
        self.row_offset = 0;
    }

    /// Shrink internal storage if capacity significantly exceeds usage.
    ///
    /// Called after retain-based operations that may have removed many entries.
    /// Threshold: shrink when capacity > 4x len and capacity > 64 (avoid thrashing
    /// on small maps).
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
        let base =
            std::mem::size_of::<Self>() + self.data.capacity() * std::mem::size_of::<CellCoord>();
        let extras_mem: usize = self.data.values().map(CellExtra::memory_used).sum();
        let rgb_mem = self.rgb_ring.as_ref().map(|r| r.heap_bytes()).unwrap_or(0);
        base + extras_mem + rgb_mem
    }

    /// Number of valid (non-stale) extras entries.
    ///
    /// Accounts for row offset: entries with internal row below the offset
    /// are stale (scrolled off) and excluded from the count.
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

    /// Drop ring buffers so they are re-created with current dimensions
    /// on the next write.
    ///
    /// Called when the grid dimensions change (resize) to prevent the ring
    /// buffers from using stale `stride`/`visible_rows` values that would
    /// produce incorrect flat-array indexing.
    pub(crate) fn invalidate_rings(&mut self) {
        self.complex_ring = None;
        self.rgb_ring = None;
    }

    /// Remove all extras in rows `>= max_row` (external coordinates).
    ///
    /// Compacts first so the retain operates on external coordinates,
    /// then shrinks if the map became oversized. Used by the reflow path
    /// to discard extras beyond the visible area.
    pub(crate) fn retain_rows_below(&mut self, max_row: u16) {
        if self.data.is_empty() {
            return;
        }
        self.compact();
        self.data.retain(|coord, _| coord.row < max_row);
        self.maybe_shrink();
    }

    /// Remove all extras in columns `>= max_col` (external coordinates).
    ///
    /// Compacts first so the retain operates on external coordinates,
    /// then shrinks if the map became oversized. Used by the no-reflow
    /// resize path to discard extras beyond the new column count (#7280).
    pub(crate) fn retain_cols_below(&mut self, max_col: u16) {
        if self.data.is_empty() {
            return;
        }
        self.compact();
        self.data.retain(|coord, _| coord.col < max_col);
        self.maybe_shrink();
    }

    /// Check if any cell in a row has a hyperlink.
    ///
    /// O(E) linear scan over all extras entries.
    #[must_use]
    pub fn row_has_hyperlinks(&self, row: u16) -> bool {
        let internal_row = self.internal_row(row);
        self.data
            .iter()
            .any(|(coord, extra)| coord.row == internal_row && extra.hyperlink().is_some())
    }

    /// Enforce the hyperlink entry limit to prevent unbounded memory growth.
    ///
    /// When the number of HYPERLINK-BEARING entries may exceed
    /// [`MAX_HYPERLINK_ENTRIES`] (see the `hyperlink_upper_bound` field),
    /// evicts hyperlink data from the oldest entries (lowest internal row
    /// coordinates, which correspond to the earliest-written rows) until the
    /// hyperlink count is within budget.
    ///
    /// Entries that lose their hyperlink retain any other extras (RGB colors,
    /// combining marks, underline color). If the entry has no remaining data
    /// after hyperlink removal, it is deleted entirely.
    ///
    /// Callers should invoke this after setting hyperlinks on entries to bound
    /// memory usage from OSC 8 spam (#7172).
    #[inline]
    pub fn enforce_hyperlink_limit(&mut self) {
        // GUARD ON THE QUANTITY BEING BOUNDED. This used to read
        // `self.data.len()`, i.e. the TOTAL extras population — combining marks,
        // underline colours, extended flags, kitty placeholders and inline-image
        // refs included. One inline image (~3 * rows * cols entries) or a
        // colour-heavy screen therefore pushed every hyperlinked write-run into
        // the cold path, which walked the whole map and evicted nothing.
        //
        // The cold path is a no-op unless the hyperlink population is over
        // budget, so skipping it whenever that population is provably within
        // budget is observably identical — and it is the ONLY case the old guard
        // got wrong. See `hyperlink_screen/mixed_extras_under_limit`.
        //
        // TWO UPPER BOUNDS, AND THE CHEAPER VERDICT WINS. `data.len()` is ALSO a
        // sound upper bound on the hyperlink population — every hyperlink lives
        // in an entry, so links <= entries — so it is KEPT as a second early-out
        // rather than replaced. Either bound proving "under budget" is proof
        // enough, and `||` short-circuits, so the common case still costs one
        // load and one branch.
        //
        // Keeping it is not cosmetic. `hyperlink_upper_bound` OVER-charges by
        // design: `get_or_create` charges on every handout to a link-free entry
        // because it cannot see what the caller will write, and the per-cell
        // write path hands out an entry for EVERY extras cell — SGR 58
        // underlines, combining marks, RGB overflow — not just hyperlinked ones.
        // A hyperlink-free workload that repaints extras therefore drives the
        // counter past the limit on entries that will never hold a link, and
        // without this clause it would buy a full cold walk every
        // MAX_HYPERLINK_ENTRIES handouts — walks the ORIGINAL `data.len()` guard
        // never took, because a small map takes its early-out. Measured on
        // `extras_erase/status_line_redraw` (1_600 underline entries, zero
        // hyperlinks, 200 redraws per screen cycle): +1.45% without this clause,
        // back to parity with it. See
        // `small_map_never_walks_however_inflated_the_counter`.
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
        // ONE walk, and NO allocation on either outcome.
        //
        // Reached only when `hyperlink_upper_bound` says the HYPERLINK-BEARING
        // population might be over budget. That bound is conservative by design
        // — a generic `get_or_create` handout charges for a link it may never
        // create, and the bulk drop paths never refund — so this walk still has
        // to decide, and its second job is to REPAIR the bound with the exact
        // count it computes on the way, which is what keeps the conservatism
        // from compounding.
        //
        // Until that bound existed the guard tested `data.len()`, the TOTAL
        // extras population — combining marks, underline colours, extended
        // flags, kitty placeholders, inline-image refs (one `place_image` is
        // ~3 * rows * cols entries) — so a map over 10k entries holding far
        // fewer hyperlinks reached here on EVERY call and evicted nothing:
        // 1_835 futile walks per MiB on
        // `hyperlink_screen/mixed_extras_under_limit`. That outcome was the
        // common one, and it used to cost a fresh `Vec<CellCoord>` every time.
        //
        // Two shapes were measured against `hyperlink_screen` before this one
        // was kept. Counting first and collecting only when genuinely over is
        // 2.8% faster on the common outcome but 8.1% SLOWER when it does evict,
        // because deciding and collecting then need two separate walks of the
        // map. Filling a RESIDENT buffer instead needs neither the second walk
        // nor the allocation, so it wins on both.
        let mut coords = std::mem::take(&mut self.hyperlink_scratch);
        loop {
            coords.clear();
            #[cfg(any(test, feature = "testing"))]
            crate::test_counters::count_hyperlink_walk_ops(self.data.len());
            coords.extend(
                self.data
                    .iter()
                    .filter(|(_, extra)| extra.hyperlink().is_some())
                    .map(|(coord, _)| *coord),
            );
            // The bound must never sit BELOW the truth, or the limit stops being
            // enforced. This walk already knows the truth, so the check is free
            // exactly where it can be made — and it fires in debug/test builds on
            // any future insertion path that forgets to charge the budget.
            debug_assert!(
                self.hyperlink_upper_bound >= coords.len(),
                "hyperlink_upper_bound {} under-counts the {} hyperlink entries \
                 present — an insertion path is not charging the budget",
                self.hyperlink_upper_bound,
                coords.len()
            );

            // The map may hold non-hyperlink extras (RGB, combining, underline)
            // that do not count toward the limit.
            if coords.len() <= MAX_HYPERLINK_ENTRIES {
                // REPAIR: the walk just established the exact population, so the
                // conservative drift accumulated since the last walk (dropped
                // entries never refund, generic `get_or_create` handouts charge
                // for links they may not create) is erased here rather than
                // buying another walk on the next call.
                self.hyperlink_upper_bound = coords.len();
                break;
            }

            // Genuinely over. Entries the O(1) scroll-offset amortization has
            // already scrolled off are unreachable but still counted, so drop
            // those first and retry — at most once, since compaction zeroes
            // `row_offset`. The retry re-walks, exactly as before: compaction
            // can bring the population back under the limit.
            if !compacted && self.row_offset != 0 {
                compacted = true;
                self.apply_offset_and_shift_only(0);
                continue;
            }

            // `coords.len() > MAX_HYPERLINK_ENTRIES > HYPERLINK_LOW_WATER`, so
            // the `to_evict - 1` pivot below is always in range.
            // EVICT TO A LOW-WATER MARK, not to the ceiling. The single walk and
            // the `select_nth_unstable_by` below already made ONE pass cheap; what
            // remains is HOW OFTEN a pass runs. Trimming to exactly
            // `MAX_HYPERLINK_ENTRIES` leaves the map one insertion over the line,
            // so the very next insert re-enters this cold path to evict a single
            // entry — and the hot caller is CHECKPOINT RESTORE:
            // `Grid::fill_row_from_line` (`grid/scroll_fill.rs`) calls this once
            // PER ROW, so a tall carried grid pays a full collect-and-partition on
            // nearly every row of the adoption.
            //
            // Trimming to 75% amortizes that: one pass buys
            // `MAX_HYPERLINK_ENTRIES / 4` inserts of headroom, which on a tall
            // restore turns tens of thousands of passes into single digits. The
            // bound callers rely on is UNCHANGED and in fact strictly stronger —
            // this evicts more, never less, so the map still never exceeds
            // `MAX_HYPERLINK_ENTRIES`.
            const HYPERLINK_LOW_WATER: usize = MAX_HYPERLINK_ENTRIES * 3 / 4;
            let to_evict = coords.len() - HYPERLINK_LOW_WATER;

            // Only the `to_evict` SMALLEST coords are needed, so partition
            // instead of sorting: O(n) rather than O(n log n), and the evicted
            // SET is identical (keys are unique, and eviction clears each
            // independently, so order within the prefix is unobservable).
            coords.select_nth_unstable_by(to_evict - 1, |a, b| {
                a.row.cmp(&b.row).then(a.col.cmp(&b.col))
            });

            // `coords` is a LOCAL (moved out of `self` above), so iterating it
            // by shared reference while `self.data` is mutated below is not a
            // borrow conflict — which is what lets this stay a slice walk.
            for &coord in &coords[..to_evict] {
                if let Some(extra) = self.data.get_mut(&coord) {
                    extra.set_hyperlink(None);
                    if !extra.has_data() {
                        self.data.remove(&coord);
                    }
                }
            }

            // Every coord came from the walk above and nothing between removed
            // one, so exactly `to_evict` links were dropped: the population is
            // now the low-water mark, exactly.
            self.hyperlink_upper_bound = HYPERLINK_LOW_WATER;

            self.maybe_shrink();
            break;
        }
        // Hand the buffer back EMPTIED but with its capacity, on every exit —
        // that capacity is the whole point of the field.
        coords.clear();
        self.hyperlink_scratch = coords;
    }

    /// The maximum number of hyperlink entries allowed before eviction.
    ///
    /// Exposed for tests and diagnostics.
    #[must_use]
    pub fn max_hyperlink_entries() -> usize {
        MAX_HYPERLINK_ENTRIES
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

    // =========================================================================
    // ComplexCharRing stride bounds-check tests (#7354)
    // =========================================================================

    /// Reading a column at or beyond the ring's stride must return None,
    /// not bleed into the next ring row's data.
    #[test]
    fn test_complex_ring_get_col_at_stride_returns_none() {
        let mut ring = ComplexCharRing::new(4, 10);
        // Place an emoji at row 0, col 0 and row 1, col 0.
        ring.set(0, 0, '\u{1F389}');
        ring.set(1, 0, '\u{1F680}');
        assert_eq!(ring.get(0, 0), Some('\u{1F389}'));
        assert_eq!(ring.get(1, 0), Some('\u{1F680}'));

        // col == stride (10): without the bounds check this would alias to
        // row 1, col 0 and return the rocket emoji.
        assert_eq!(
            ring.get(0, 10),
            None,
            "col == stride should return None, not cross-row data"
        );
        // col well beyond stride.
        assert_eq!(ring.get(0, 100), None, "col >> stride should return None");
    }

    /// Writing to a column at or beyond stride must be a no-op — it must not
    /// corrupt another row's slot.
    #[test]
    fn test_complex_ring_set_col_at_stride_is_noop() {
        let mut ring = ComplexCharRing::new(4, 10);
        // Write beyond stride: would alias to row 1, col 0 without the guard.
        ring.set(0, 10, '\u{1F680}');
        // Write beyond stride: would alias to row 1, col 5 without the guard.
        ring.set(0, 15, '\u{2764}');

        // The aliased slots must remain empty.
        assert_eq!(
            ring.get(1, 0),
            None,
            "set at col == stride must not write to next row"
        );
        assert_eq!(
            ring.get(1, 5),
            None,
            "set at col > stride must not write to aliased slot"
        );
    }

    /// Over-scroll regression: scrolling the complex ring by `n >= visible_rows`
    /// (e.g. a full-screen `CSI 65535 S`) must fully recycle the ring and reset
    /// `ring_offset` to 0, not corrupt it to a value >= visible_rows. After the
    /// over-scroll, a freshly written non-BMP char must read back correctly.
    #[test]
    fn test_complex_ring_overscroll_resets_offset() {
        let mut ring = ComplexCharRing::new(24, 10);
        ring.set(0, 0, '\u{1F389}');
        // Scroll the whole visible area (and more) off in one operation.
        ring.scroll_up(65535);
        // Offset is back to a clean state and the old char is gone.
        assert_eq!(ring.ring_offset, 0, "over-scroll must reset ring_offset");
        assert_eq!(ring.get(0, 0), None, "scrolled-off char must be cleared");
        // A subsequent write must land in a readable slot (no index corruption).
        ring.set(5, 3, '\u{1F680}');
        assert_eq!(
            ring.get(5, 3),
            Some('\u{1F680}'),
            "post-overscroll write must read back (offset not corrupted)"
        );
    }

    /// Over-scroll regression for the RGB ring: `scroll_up(n >= visible_rows)`
    /// must reset `ring_offset` and leave subsequent truecolor writes readable.
    #[test]
    fn test_rgb_ring_overscroll_resets_offset() {
        let mut ring = RgbColorRing::new(24, 10);
        ring.set_fg(0, 0, [255, 0, 0]);
        ring.set_bg(0, 0, [0, 255, 0]);
        ring.scroll_up(65535);
        assert_eq!(ring.ring_offset, 0, "over-scroll must reset ring_offset");
        assert_eq!(ring.fg(0, 0), None, "scrolled-off fg must be cleared");
        assert_eq!(ring.bg(0, 0), None, "scrolled-off bg must be cleared");
        ring.set_fg(7, 2, [10, 20, 30]);
        ring.set_bg(7, 2, [40, 50, 60]);
        assert_eq!(
            ring.fg(7, 2),
            Some([10, 20, 30]),
            "post-overscroll fg write must read back"
        );
        assert_eq!(
            ring.bg(7, 2),
            Some([40, 50, 60]),
            "post-overscroll bg write must read back"
        );
    }

    /// PERF/parity: the bulk `fill_fg_run`/`fill_bg_run` (one slice `fill` per run —
    /// the production write path for uniform truecolor spans) produce a ring state
    /// BYTE-IDENTICAL to looping the per-cell `set_fg`/`set_bg` oracle, across a lattice
    /// of geometries and ranges — including empty runs, runs that overhang `stride`
    /// (partial clamp), and fully out-of-range runs (which must NOT allocate the plane).
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
            bulk.fill_fg_run(row, cs, ce, rgb_f);
            bulk.fill_bg_run(row, cs, ce, rgb_b);
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

    /// PERF/parity: the hoisted double slice `fill` in `RgbColorRing::clear_range`
    /// leaves the planes BYTE-IDENTICAL to the old per-column scalar loop (a fresh
    /// `index()` derivation plus `get_mut` per column), across a lattice of
    /// geometries and ranges — empty, inverted, overhanging `stride`, wholly
    /// out-of-range, a stale `row >= visible_rows`, and non-zero ring offsets.
    /// Also pins allocation parity: clearing must never materialize an absent
    /// plane (`as_deref_mut`, not `fg_mut`).
    #[test]
    fn clear_range_matches_per_cell_clear() {
        // The pre-optimization body, kept verbatim as the reference oracle.
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

    /// Exactly `n == visible_rows` is the boundary of the over-scroll guard and
    /// must also fully clear and reset (the entire visible area scrolled off).
    #[test]
    fn test_complex_ring_scroll_exactly_visible_rows() {
        let mut ring = ComplexCharRing::new(4, 10);
        ring.set(3, 1, '\u{1F389}');
        ring.scroll_up(4);
        assert_eq!(ring.ring_offset, 0);
        assert_eq!(ring.get(3, 1), None);
        ring.set(0, 0, '\u{1F600}');
        assert_eq!(ring.get(0, 0), Some('\u{1F600}'));
    }

    /// Same parity argument for `ComplexCharRing::clear_range` (DECERA/DECCRA
    /// path): one `fill('\0')` must equal the old per-column `get_mut` store.
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

    // =========================================================================
    // RgbColorRing stride bounds-check tests (#7354)
    // =========================================================================

    /// Reading fg/bg at a column at or beyond stride must return None.
    #[test]
    fn test_rgb_ring_get_col_at_stride_returns_none() {
        let mut ring = RgbColorRing::new(4, 10);
        ring.set_fg(0, 0, [255, 0, 0]);
        ring.set_bg(0, 0, [0, 255, 0]);
        // Place data at row 1, col 0 to verify no cross-row bleed.
        ring.set_fg(1, 0, [0, 0, 255]);
        ring.set_bg(1, 0, [128, 128, 0]);

        assert_eq!(ring.fg(0, 0), Some([255, 0, 0]));
        assert_eq!(ring.bg(0, 0), Some([0, 255, 0]));

        // col == stride: must not bleed into row 1.
        assert_eq!(
            ring.fg(0, 10),
            None,
            "fg at col == stride should return None"
        );
        assert_eq!(
            ring.bg(0, 10),
            None,
            "bg at col == stride should return None"
        );
        // col well beyond stride.
        assert_eq!(
            ring.fg(0, 200),
            None,
            "fg at col >> stride should return None"
        );
        assert_eq!(
            ring.bg(0, 200),
            None,
            "bg at col >> stride should return None"
        );
    }

    /// Writing fg/bg at a column at or beyond stride must be a no-op.
    #[test]
    fn test_rgb_ring_set_col_at_stride_is_noop() {
        let mut ring = RgbColorRing::new(4, 10);
        // Attempt writes beyond stride.
        ring.set_fg(0, 10, [255, 0, 0]);
        ring.set_bg(0, 10, [0, 255, 0]);

        // The aliased slot (row 1, col 0) must remain empty.
        assert_eq!(
            ring.fg(1, 0),
            None,
            "set_fg at col == stride must not corrupt next row"
        );
        assert_eq!(
            ring.bg(1, 0),
            None,
            "set_bg at col == stride must not corrupt next row"
        );
    }

    /// End-to-end: after writing data and then querying with an out-of-range
    /// column, the ring buffer must not return stale cross-row entries.
    #[test]
    fn test_complex_ring_scroll_then_oob_col_returns_none() {
        let mut ring = ComplexCharRing::new(4, 10);
        // Fill row 0 fully.
        for col in 0..10 {
            ring.set(
                0,
                col,
                char::from_u32(0x1F600 + u32::from(col)).expect("valid char"),
            );
        }
        // Scroll up by 1: row 0 is recycled, old row 1 becomes new row 0.
        ring.scroll_up(1);

        // Query with col == stride on the new row 0 — must return None.
        assert_eq!(
            ring.get(0, 10),
            None,
            "after scroll, col == stride must still return None"
        );
    }

    // =========================================================================
    // RgbColorRing erase clearing tests (#7697)
    // =========================================================================

    /// `RgbColorRing::clear_range` zeroes fg/bg for the specified column range.
    #[test]
    fn test_rgb_ring_clear_range_zeroes_entries() {
        let mut ring = RgbColorRing::new(4, 10);
        // Populate cells 2..7 on row 1.
        for col in 2..7 {
            ring.set_fg(1, col, [255, 0, col as u8]);
            ring.set_bg(1, col, [0, 255, col as u8]);
        }
        // Verify data is present.
        assert!(ring.fg(1, 3).is_some());
        assert!(ring.bg(1, 3).is_some());

        // Clear range 3..6 on row 1.
        ring.clear_range(1, 3, 6);

        // Cleared entries should be None.
        for col in 3..6 {
            assert_eq!(ring.fg(1, col), None, "fg at col {col} should be cleared");
            assert_eq!(ring.bg(1, col), None, "bg at col {col} should be cleared");
        }
        // Entries outside the range should be preserved.
        assert!(ring.fg(1, 2).is_some(), "col 2 should be preserved");
        assert!(ring.fg(1, 6).is_some(), "col 6 should be preserved");
    }

    /// `RgbColorRing::clear_row` zeroes the entire row.
    #[test]
    fn test_rgb_ring_clear_row_zeroes_entire_row() {
        let mut ring = RgbColorRing::new(4, 10);
        for col in 0..10 {
            ring.set_fg(2, col, [col as u8, 0, 0]);
            ring.set_bg(2, col, [0, col as u8, 0]);
        }
        // Also populate row 1 to verify it's not affected.
        ring.set_fg(1, 0, [42, 42, 42]);

        ring.clear_row(2);

        for col in 0..10 {
            assert_eq!(ring.fg(2, col), None, "fg at col {col} should be cleared");
            assert_eq!(ring.bg(2, col), None, "bg at col {col} should be cleared");
        }
        // Row 1 should be untouched.
        assert_eq!(
            ring.fg(1, 0),
            Some([42, 42, 42]),
            "row 1 should be preserved"
        );
    }

    /// `CellExtras::clear_range` zeroes the RGB ring for the erased range (#7697).
    #[test]
    fn test_cell_extras_clear_range_clears_rgb_ring() {
        let mut extras = CellExtras::new();
        // Write RGB-only data through the ring buffer path.
        extras.set_rgb_ring_range(0, 0, 5, Some([255, 0, 0]), Some([0, 0, 255]), 4, 10);
        // Verify it's readable.
        assert_eq!(extras.fg_rgb_for(0, 2), Some([255, 0, 0]));
        assert_eq!(extras.bg_rgb_for(0, 2), Some([0, 0, 255]));

        // Simulate erase: clear_range should remove ring data.
        extras.clear_range(0, 1, 4);

        // Cleared cells should return None (no stale data).
        for col in 1..4 {
            assert_eq!(
                extras.fg_rgb_for(0, col),
                None,
                "fg_rgb at col {col} should be None after clear_range (#7697)"
            );
            assert_eq!(
                extras.bg_rgb_for(0, col),
                None,
                "bg_rgb at col {col} should be None after clear_range (#7697)"
            );
        }
        // Cells outside the range should be preserved.
        assert_eq!(
            extras.fg_rgb_for(0, 0),
            Some([255, 0, 0]),
            "col 0 preserved"
        );
        assert_eq!(
            extras.fg_rgb_for(0, 4),
            Some([255, 0, 0]),
            "col 4 preserved"
        );
    }

    /// `CellExtras::clear_row` zeroes the RGB ring for the erased row (#7697).
    #[test]
    fn test_cell_extras_clear_row_clears_rgb_ring() {
        let mut extras = CellExtras::new();
        extras.set_rgb_ring_range(1, 0, 8, Some([0, 128, 0]), None, 4, 10);
        assert_eq!(extras.fg_rgb_for(1, 3), Some([0, 128, 0]));

        extras.clear_row(1);

        for col in 0..8 {
            assert_eq!(
                extras.fg_rgb_for(1, col),
                None,
                "fg_rgb at (1, {col}) should be None after clear_row (#7697)"
            );
        }
    }

    /// `RgbColorRing::shift_region_up` moves surviving rows up via the
    /// `copy_within` memmove in `copy_span_in_plane`, preserving fg/bg exactly.
    #[test]
    fn test_rgb_ring_shift_region_up_preserves_via_memmove() {
        let mut ring = RgbColorRing::new(4, 10);
        // Distinct fg/bg per (row, col) so a wrong copy is detectable.
        for row in 0..4 {
            for col in 0..10 {
                ring.set_fg(row, col, [row as u8, col as u8, 1]);
                ring.set_bg(row, col, [row as u8, col as u8, 2]);
            }
        }
        // Shift region [0, 3] up by 1: row r+1 moves to row r; row 3 cleared.
        ring.shift_region_up(0, 3, 1);
        for row in 0..3 {
            for col in 0..10 {
                assert_eq!(
                    ring.fg(row, col),
                    Some([(row + 1) as u8, col as u8, 1]),
                    "fg at ({row}, {col}) should equal old row {}",
                    row + 1
                );
                assert_eq!(
                    ring.bg(row, col),
                    Some([(row + 1) as u8, col as u8, 2]),
                    "bg at ({row}, {col}) should equal old row {}",
                    row + 1
                );
            }
        }
        for col in 0..10 {
            assert_eq!(ring.fg(3, col), None, "vacated row 3 fg cleared");
            assert_eq!(ring.bg(3, col), None, "vacated row 3 bg cleared");
        }
    }

    /// `ComplexCharRing::shift_region_up` moves surviving rows up via the
    /// `copy_within` memmove in `copy_row_span`, preserving chars exactly.
    #[test]
    fn test_complex_ring_shift_region_up_preserves_via_memmove() {
        let mut ring = ComplexCharRing::new(4, 10);
        for row in 0..4 {
            for col in 0..10 {
                let c = char::from_u32(0x1F600 + u32::from(row) * 16 + u32::from(col))
                    .expect("valid char");
                ring.set(row, col, c);
            }
        }
        ring.shift_region_up(0, 3, 1);
        for row in 0..3 {
            for col in 0..10 {
                let expected = char::from_u32(0x1F600 + u32::from(row + 1) * 16 + u32::from(col))
                    .expect("valid char");
                assert_eq!(
                    ring.get(row, col),
                    Some(expected),
                    "char at ({row}, {col}) should equal old row {}",
                    row + 1
                );
            }
        }
        for col in 0..10 {
            assert_eq!(ring.get(3, col), None, "vacated row 3 cleared");
        }
    }

    /// `CellExtras::clear_rows` zeroes the RGB ring for the erased rows (#7697).
    #[test]
    fn test_cell_extras_clear_rows_clears_rgb_ring() {
        let mut extras = CellExtras::new();
        for row in 0..4 {
            extras.set_rgb_ring_range(row, 0, 5, Some([row as u8, 0, 0]), None, 4, 10);
        }
        assert_eq!(extras.fg_rgb_for(2, 0), Some([2, 0, 0]));

        // Clear rows 1..3.
        extras.clear_rows(1..3);

        // Cleared rows should return None.
        for row in 1..3 {
            assert_eq!(
                extras.fg_rgb_for(row, 0),
                None,
                "row {row} should be cleared (#7697)"
            );
        }
        // Rows 0 and 3 should be preserved.
        assert_eq!(extras.fg_rgb_for(0, 0), Some([0, 0, 0]));
        assert_eq!(extras.fg_rgb_for(3, 0), Some([3, 0, 0]));
    }

    /// `CellExtras::clear_rect` zeroes the RGB ring for the erased rect (#7697).
    #[test]
    fn test_cell_extras_clear_rect_clears_rgb_ring() {
        let mut extras = CellExtras::new();
        for row in 0..4 {
            extras.set_rgb_ring_range(row, 0, 10, Some([255, row as u8, 0]), None, 4, 10);
        }
        assert_eq!(extras.fg_rgb_for(1, 5), Some([255, 1, 0]));

        // Clear rect rows 1..3, cols 3..7.
        extras.clear_rect(1..3, 3..7);

        // Inside rect: should be cleared.
        for row in 1..3 {
            for col in 3..7 {
                assert_eq!(
                    extras.fg_rgb_for(row, col),
                    None,
                    "({row}, {col}) inside rect should be cleared (#7697)"
                );
            }
        }
        // Outside rect: should be preserved.
        assert_eq!(
            extras.fg_rgb_for(1, 2),
            Some([255, 1, 0]),
            "left of rect preserved"
        );
        assert_eq!(
            extras.fg_rgb_for(1, 7),
            Some([255, 1, 0]),
            "right of rect preserved"
        );
        assert_eq!(
            extras.fg_rgb_for(0, 5),
            Some([255, 0, 0]),
            "above rect preserved"
        );
        assert_eq!(
            extras.fg_rgb_for(3, 5),
            Some([255, 3, 0]),
            "below rect preserved"
        );
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

    /// Sharing must be UNOBSERVABLE. `set_range_uniform` hands every cell of an
    /// OSC 8 run the SAME `Arc<HyperlinkData>`, so a later single-cell edit has
    /// to copy-on-write and leave its neighbours' link alone — the one thing an
    /// `Arc` payload could break that a per-cell `Box` could not. Written so it
    /// passes under BOTH representations: it pins the behaviour, not the shape.
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

        // Edit ONE cell's URL (which also clears that cell's stale id) and a
        // DIFFERENT cell's id — the two single-cell mutators, one each.
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

        // The untouched cells of the run must still read the ORIGINAL pair.
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
