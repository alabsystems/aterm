// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{Cell, PackedColor};

impl Cell {
    /// Get foreground color when it is encoded inline in the cell; `None` for
    /// RGB (`CellExtra` overflow) and `StyleId` (style table) cells.
    #[must_use]
    #[inline]
    pub const fn fg_color(&self) -> Option<PackedColor> {
        if self.uses_style_id() {
            return None;
        }

        let colors = self.colors();
        if colors.fg_is_default() {
            Some(PackedColor::DEFAULT_FG)
        } else if colors.fg_is_indexed() {
            Some(PackedColor::indexed(colors.fg_index()))
        } else {
            None
        }
    }

    /// Get background color when it is encoded inline in the cell; `None` for
    /// RGB (`CellExtra` overflow) and `StyleId` (style table) cells.
    #[must_use]
    #[inline]
    pub const fn bg_color(&self) -> Option<PackedColor> {
        if self.uses_style_id() {
            return None;
        }

        let colors = self.colors();
        if colors.bg_is_default() {
            Some(PackedColor::DEFAULT_BG)
        } else if colors.bg_is_indexed() {
            Some(PackedColor::indexed(colors.bg_index()))
        } else {
            None
        }
    }

    /// Check if foreground needs overflow lookup for RGB.
    #[must_use]
    #[inline]
    pub const fn fg_needs_overflow(&self) -> bool {
        let colors = self.colors();
        colors.fg_is_rgb()
    }

    /// Check if background needs overflow lookup for RGB.
    #[must_use]
    #[inline]
    pub const fn bg_needs_overflow(&self) -> bool {
        let colors = self.colors();
        colors.bg_is_rgb()
    }
}
