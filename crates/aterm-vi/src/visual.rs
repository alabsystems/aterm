// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Visual selection mode for vi mode (v/V/Ctrl+V).

use super::{ViMode, ViPoint, ViVisualType};

impl ViMode {
    /// Toggle visual selection mode.
    ///
    /// If visual mode is inactive, start it with the given type and anchor
    /// at the current cursor position. If already in the same visual type,
    /// cancel visual mode. If in a different visual type, switch to the
    /// new type (anchor preserved).
    pub fn toggle_visual(&mut self, vtype: ViVisualType) {
        if !self.active {
            return;
        }
        match self.visual_type {
            Some(current) if current == vtype => {
                // Same type — cancel visual mode.
                self.visual_anchor = None;
                self.visual_type = None;
            }
            Some(_) => {
                // Different type — switch (anchor stays).
                self.visual_type = Some(vtype);
            }
            None => {
                // Not in visual mode — start.
                self.visual_anchor = Some(self.cursor.point);
                self.visual_type = Some(vtype);
            }
        }
    }

    /// Whether visual selection is currently active.
    #[must_use]
    pub fn visual_is_active(&self) -> bool {
        self.visual_type.is_some()
    }

    /// Get the visual selection type, if active.
    #[must_use]
    pub fn visual_type(&self) -> Option<ViVisualType> {
        self.visual_type
    }

    /// Get the visual selection range as `(start, end)` in document order.
    ///
    /// Returns `None` if visual selection is not active. For line-wise
    /// selection, `start.col` is 0 and `end.col` is `cols - 1`.
    #[must_use]
    pub fn visual_range(&self, cols: u16) -> Option<(ViPoint, ViPoint)> {
        let anchor = self.visual_anchor?;
        let vtype = self.visual_type?;
        let cursor = self.cursor.point;

        let (mut start, mut end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };

        match vtype {
            ViVisualType::Char => {}
            ViVisualType::Line => {
                start.col = 0;
                end.col = cols.saturating_sub(1);
            }
            ViVisualType::Block => {
                let min_col = start.col.min(end.col);
                let max_col = start.col.max(end.col);
                start.col = min_col;
                end.col = max_col;
            }
        }

        Some((start, end))
    }

    /// Cancel visual selection mode without exiting vi mode.
    pub fn cancel_visual(&mut self) {
        self.visual_anchor = None;
        self.visual_type = None;
    }
}
