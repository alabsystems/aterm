// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Region and column shifts for [`CellExtras`]: scroll-region shifts and ICH/DCH
//! column shifts. Each touches a subset of rows, so each drain-rebuilds (O(E)).

use aterm_hash::FxHashMap;

use super::extra_collection::CellExtras;
use crate::extra::{CellCoord, CellExtra};

impl CellExtras {
    /// Shifts rows up within `[top, bottom]` by `n`, dropping `[top, top + n)` and compacting the row offset.
    pub(crate) fn shift_region_up_by(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rings_region_up(top, bottom, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        if offset == 0
            && !self
                .data
                .keys()
                .any(|coord| (top..=bottom).contains(&coord.row))
        {
            return;
        }
        let shift_start = top.checked_add(n);
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            // Internal row < offset: stale, drop.
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                new_data.insert(CellCoord::new(ext_row - n, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shifts rows down within `[top, bottom]` by `n`, dropping `[bottom - n + 1, bottom]` and compacting the row offset.
    pub(crate) fn shift_region_down_by(&mut self, top: u16, bottom: u16, n: u16) {
        self.shift_rings_region_down(top, bottom, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        if offset == 0
            && !self
                .data
                .keys()
                .any(|coord| (top..=bottom).contains(&coord.row))
        {
            return;
        }
        let drop_start = bottom.saturating_sub(n.saturating_sub(1));
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row >= top && ext_row <= bottom {
                if ext_row < drop_start {
                    new_data.insert(CellCoord::new(ext_row.saturating_add(n), coord.col), extra);
                }
            } else {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shifts extras up by `n` within `[top, bottom] × [left, right]` (DECLRMM scroll and DL).
    pub(crate) fn shift_rect_up_by(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        self.shift_rings_rect_up(top, bottom, left, right, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        let shift_start = top.checked_add(n);
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom || coord.col < left || coord.col > right {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if let Some(shift_start) = shift_start
                && ext_row >= shift_start
            {
                new_data.insert(CellCoord::new(ext_row - n, coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Shifts extras down by `n` within `[top, bottom] × [left, right]` (DECLRMM scroll and IL).
    pub(crate) fn shift_rect_down_by(
        &mut self,
        top: u16,
        bottom: u16,
        left: u16,
        right: u16,
        n: u16,
    ) {
        self.shift_rings_rect_down(top, bottom, left, right, n);
        if n == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let offset = self.take_row_offset();
        let drop_start = bottom.saturating_sub(n.saturating_sub(1));
        let mut new_data =
            FxHashMap::with_capacity_and_hasher(self.data.len(), aterm_hash::FxBuildHasher);
        for (coord, extra) in self.data.drain() {
            if coord.row < offset {
                continue;
            }
            let ext_row = coord.row - offset;

            if ext_row < top || ext_row > bottom || coord.col < left || coord.col > right {
                new_data.insert(CellCoord::new(ext_row, coord.col), extra);
                continue;
            }

            if ext_row < drop_start {
                new_data.insert(CellCoord::new(ext_row.saturating_add(n), coord.col), extra);
            }
        }
        self.data = new_data;
        self.enforce_hyperlink_limit();
    }

    /// Removes and returns all `(col, extra)` entries on `internal_row` in `[start_col, max_col)`.
    fn take_row_range(
        &mut self,
        internal_row: u16,
        start_col: u16,
        max_col: u16,
    ) -> aterm_alloc::SmallVec<(u16, CellExtra), 8> {
        let mut extracted: aterm_alloc::SmallVec<(u16, CellExtra), 8> =
            aterm_alloc::SmallVec::new();
        let width = usize::from(max_col.saturating_sub(start_col));
        if width.saturating_mul(2) <= self.data.len() {
            for col in start_col..max_col {
                // `internal_row` is already offset-mapped.
                if let Some(extra) = self.data.remove(&CellCoord::new(internal_row, col)) {
                    extracted.push((col, extra));
                }
            }
        } else {
            let keys: aterm_alloc::SmallVec<CellCoord, 8> = self
                .data
                .keys()
                .filter(|c| c.row == internal_row && c.col >= start_col && c.col < max_col)
                .copied()
                .collect();
            for key in keys {
                if let Some(extra) = self.data.remove(&key) {
                    extracted.push((key.col, extra));
                }
            }
        }
        extracted
    }

    /// Shifts columns in `[start_col, max_col)` right by `count` on `row` for ICH, dropping entries pushed past `max_col`.
    pub fn shift_cols_right(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        self.shift_rings_cols_right(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let internal_row = self.internal_row(row);
        let extracted = self.take_row_range(internal_row, start_col, max_col);
        for (col, extra) in extracted {
            let new_col = col.saturating_add(count);
            if new_col < max_col {
                self.data
                    .insert(CellCoord::new(internal_row, new_col), extra);
            }
        }
        self.maybe_shrink();
    }

    /// Shifts columns in `[start_col + count, max_col)` left by `count` on `row` for DCH, deleting `[start_col, start_col + count)`.
    pub(crate) fn shift_cols_left(&mut self, row: u16, start_col: u16, count: u16, max_col: u16) {
        self.shift_rings_cols_left(row, start_col, count, max_col);
        if count == 0 || self.data.is_empty() {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        crate::test_counters::count_extras_shift_ops(self.data.len());
        let internal_row = self.internal_row(row);
        let shift_start = start_col.saturating_add(count);
        let extracted = self.take_row_range(internal_row, start_col, max_col);
        for (col, extra) in extracted {
            if col >= shift_start {
                self.data
                    .insert(CellCoord::new(internal_row, col - count), extra);
            }
        }
        self.maybe_shrink();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const ROWS: u16 = 24;
    const COLS: u16 = 24;
    const RING_COLS: u16 = 80;

    #[derive(Clone, Copy, Debug)]
    enum Shift {
        RowsUp {
            start: u16,
            n: u16,
        },
        RegionUp {
            top: u16,
            bottom: u16,
            n: u16,
        },
        RegionDown {
            top: u16,
            bottom: u16,
            n: u16,
        },
        RectUp {
            top: u16,
            bottom: u16,
            left: u16,
            right: u16,
            n: u16,
        },
        RectDown {
            top: u16,
            bottom: u16,
            left: u16,
            right: u16,
            n: u16,
        },
        ColsRight {
            row: u16,
            start: u16,
            n: u16,
            max: u16,
        },
        ColsLeft {
            row: u16,
            start: u16,
            n: u16,
            max: u16,
        },
    }

    impl Shift {
        fn apply(self, extras: &mut CellExtras) {
            match self {
                Self::RowsUp { start, n } => extras.shift_rows_up_by(start, n),
                Self::RegionUp { top, bottom, n } => extras.shift_region_up_by(top, bottom, n),
                Self::RegionDown { top, bottom, n } => extras.shift_region_down_by(top, bottom, n),
                Self::RectUp {
                    top,
                    bottom,
                    left,
                    right,
                    n,
                } => {
                    extras.shift_rect_up_by(top, bottom, left, right, n);
                }
                Self::RectDown {
                    top,
                    bottom,
                    left,
                    right,
                    n,
                } => {
                    extras.shift_rect_down_by(top, bottom, left, right, n);
                }
                Self::ColsRight { row, start, n, max } => {
                    extras.shift_cols_right(row, start, n, max)
                }
                Self::ColsLeft { row, start, n, max } => extras.shift_cols_left(row, start, n, max),
            }
        }

        /// The documented contract: where the entry at `(row, col)` lands, or
        /// `None` when the shift drops it.
        fn model(self, row: u16, col: u16) -> Option<(u16, u16)> {
            let (r, c) = (u32::from(row), u32::from(col));
            let up = |top: u16, bottom: u16, left: u16, right: u16, n: u16| {
                if n == 0 || row < top || row > bottom || col < left || col > right {
                    Some((row, col))
                } else if r >= u32::from(top) + u32::from(n) {
                    Some((row - n, col))
                } else {
                    None
                }
            };
            let down = |top: u16, bottom: u16, left: u16, right: u16, n: u16| {
                if n == 0 || row < top || row > bottom || col < left || col > right {
                    Some((row, col))
                } else if r + u32::from(n) <= u32::from(bottom) {
                    Some((row + n, col))
                } else {
                    None
                }
            };
            match self {
                Self::RowsUp { start, n } => up(start, u16::MAX, 0, u16::MAX, n),
                Self::RegionUp { top, bottom, n } => up(top, bottom, 0, u16::MAX, n),
                Self::RegionDown { top, bottom, n } => down(top, bottom, 0, u16::MAX, n),
                Self::RectUp {
                    top,
                    bottom,
                    left,
                    right,
                    n,
                } => up(top, bottom, left, right, n),
                Self::RectDown {
                    top,
                    bottom,
                    left,
                    right,
                    n,
                } => down(top, bottom, left, right, n),
                Self::ColsRight {
                    row: at,
                    start,
                    n,
                    max,
                } => {
                    if row != at || n == 0 || col < start || col >= max {
                        Some((row, col))
                    } else if c + u32::from(n) < u32::from(max) {
                        Some((row, col + n))
                    } else {
                        None
                    }
                }
                Self::ColsLeft {
                    row: at,
                    start,
                    n,
                    max,
                } => {
                    if row != at || n == 0 || col < start || col >= max {
                        Some((row, col))
                    } else if c >= u32::from(start) + u32::from(n) {
                        Some((row, col - n))
                    } else {
                        None
                    }
                }
            }
        }
    }

    fn fg(row: u16, col: u16) -> [u8; 3] {
        [row as u8, col as u8, 1]
    }

    fn bg(row: u16, col: u16) -> [u8; 3] {
        [row as u8, col as u8, 2]
    }

    fn complex(row: u16, col: u16) -> char {
        char::from_u32(0x1F000 + u32::from(row) * 64 + u32::from(col)).expect("valid char")
    }

    /// Verifies that every shift moves entries in the HashMap, RGB ring, and complex-char ring according to `Shift::model`.
    #[test]
    fn every_shift_moves_every_store_by_the_model() {
        use Shift::*;
        let scenarios: &[(&str, &[Shift])] = &[
            (
                "full-screen SU",
                &[RegionUp {
                    top: 0,
                    bottom: 23,
                    n: 3,
                }],
            ),
            (
                "SU in margins",
                &[RegionUp {
                    top: 5,
                    bottom: 15,
                    n: 1,
                }],
            ),
            (
                "SU leaves rows above (bug A)",
                &[RegionUp {
                    top: 12,
                    bottom: 17,
                    n: 4,
                }],
            ),
            (
                "SU single-row region",
                &[RegionUp {
                    top: 5,
                    bottom: 5,
                    n: 1,
                }],
            ),
            (
                "SU n == region",
                &[RegionUp {
                    top: 2,
                    bottom: 5,
                    n: 4,
                }],
            ),
            (
                "SU n > region",
                &[RegionUp {
                    top: 5,
                    bottom: 6,
                    n: 9,
                }],
            ),
            (
                "SU n == 0",
                &[RegionUp {
                    top: 1,
                    bottom: 3,
                    n: 0,
                }],
            ),
            (
                "SD in margins",
                &[RegionDown {
                    top: 5,
                    bottom: 15,
                    n: 1,
                }],
            ),
            (
                "SD drops bottom",
                &[RegionDown {
                    top: 0,
                    bottom: 9,
                    n: 2,
                }],
            ),
            (
                "SD n == region",
                &[RegionDown {
                    top: 0,
                    bottom: 2,
                    n: 3,
                }],
            ),
            (
                "SD n > region",
                &[RegionDown {
                    top: 1,
                    bottom: 3,
                    n: 5,
                }],
            ),
            (
                "SD n == 0",
                &[RegionDown {
                    top: 1,
                    bottom: 3,
                    n: 0,
                }],
            ),
            (
                "SD single-row region",
                &[RegionDown {
                    top: 5,
                    bottom: 5,
                    n: 1,
                }],
            ),
            (
                "rect up",
                &[RectUp {
                    top: 2,
                    bottom: 8,
                    left: 5,
                    right: 15,
                    n: 2,
                }],
            ),
            (
                "rect up past grid",
                &[RectUp {
                    top: 2,
                    bottom: 8,
                    left: 10,
                    right: 30,
                    n: 1,
                }],
            ),
            (
                "rect up n == 0",
                &[RectUp {
                    top: 0,
                    bottom: 9,
                    left: 5,
                    right: 15,
                    n: 0,
                }],
            ),
            (
                "rect down",
                &[RectDown {
                    top: 2,
                    bottom: 8,
                    left: 5,
                    right: 15,
                    n: 2,
                }],
            ),
            (
                "rect down past grid",
                &[RectDown {
                    top: 2,
                    bottom: 8,
                    left: 10,
                    right: 30,
                    n: 1,
                }],
            ),
            (
                "ICH",
                &[ColsRight {
                    row: 5,
                    start: 10,
                    n: 3,
                    max: 24,
                }],
            ),
            (
                "ICH from col 0",
                &[ColsRight {
                    row: 0,
                    start: 0,
                    n: 5,
                    max: 80,
                }],
            ),
            (
                "ICH at the margin",
                &[ColsRight {
                    row: 3,
                    start: 23,
                    n: 1,
                    max: 24,
                }],
            ),
            (
                "ICH inside DECLRMM",
                &[ColsRight {
                    row: 0,
                    start: 5,
                    n: 3,
                    max: 12,
                }],
            ),
            (
                "ICH n == 0",
                &[ColsRight {
                    row: 0,
                    start: 3,
                    n: 0,
                    max: 24,
                }],
            ),
            (
                "DCH",
                &[ColsLeft {
                    row: 3,
                    start: 5,
                    n: 3,
                    max: 24,
                }],
            ),
            (
                "DCH whole row",
                &[ColsLeft {
                    row: 0,
                    start: 0,
                    n: 24,
                    max: 24,
                }],
            ),
            (
                "DCH inside DECLRMM",
                &[ColsLeft {
                    row: 0,
                    start: 3,
                    n: 2,
                    max: 10,
                }],
            ),
            (
                "DCH n == 0",
                &[ColsLeft {
                    row: 0,
                    start: 3,
                    n: 0,
                    max: 24,
                }],
            ),
            (
                "SU after a full-screen offset",
                &[
                    RowsUp { start: 0, n: 2 },
                    RegionUp {
                        top: 0,
                        bottom: 23,
                        n: 1,
                    },
                ],
            ),
            (
                "ICH after a full-screen offset",
                &[
                    RowsUp { start: 0, n: 3 },
                    ColsRight {
                        row: 4,
                        start: 2,
                        n: 2,
                        max: 24,
                    },
                ],
            ),
            ("partial-screen scroll", &[RowsUp { start: 1, n: 1 }]),
        ];
        for (name, shifts) in scenarios {
            // A partial-screen `shift_rows_up_by` wipes both rings instead of
            // moving them; only the map follows the model there.
            let rings_follow = !shifts
                .iter()
                .any(|s| matches!(s, RowsUp { start, .. } if *start > 0));
            for dense in [true, false] {
                let cells: Vec<(u16, u16)> = (0..ROWS)
                    .flat_map(|r| (0..COLS).map(move |c| (r, c)))
                    .filter(|&(r, c)| dense || (r * COLS + c).is_multiple_of(7))
                    .collect();
                let mut extras = CellExtras::new();
                let mut want = HashMap::new();
                for &(r, c) in &cells {
                    let mut extra = CellExtra::default();
                    extra.set_fg_rgb(Some(fg(r, c)));
                    extras.set(CellCoord::new(r, c), extra);
                    extras.set_rgb_ring_range(r, c, c + 1, None, Some(bg(r, c)), ROWS, RING_COLS);
                    extras.set_complex_char_ring(r, c, complex(r, c), ROWS, RING_COLS);
                    let dest = shifts
                        .iter()
                        .try_fold((r, c), |(row, col), s| s.model(row, col));
                    if let Some(dest) = dest {
                        want.insert(dest, (r, c));
                    }
                }
                for shift in *shifts {
                    shift.apply(&mut extras);
                }

                let got: HashMap<(u16, u16), [u8; 3]> = extras
                    .iter()
                    .map(|(coord, e)| ((coord.row, coord.col), e.fg_rgb().expect("fg")))
                    .collect();
                let want_map: HashMap<(u16, u16), [u8; 3]> =
                    want.iter().map(|(&at, &(r, c))| (at, fg(r, c))).collect();
                assert_eq!(got, want_map, "{name} (dense: {dense}): map");
                assert_eq!(extras.len(), want.len(), "{name} (dense: {dense}): len");

                if !rings_follow {
                    continue;
                }
                for row in 0..ROWS {
                    for col in 0..COLS + 8 {
                        let origin = want.get(&(row, col)).copied();
                        assert_eq!(
                            extras.bg_rgb_for(row, col),
                            origin.map(|(r, c)| bg(r, c)),
                            "{name} (dense: {dense}): rgb ring at ({row}, {col})"
                        );
                        assert_eq!(
                            extras.complex_codepoint_for(row, col),
                            origin.map(|(r, c)| complex(r, c)),
                            "{name} (dense: {dense}): complex ring at ({row}, {col})"
                        );
                    }
                }
            }
        }
    }
}
