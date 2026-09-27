// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Visual selection mode for vi mode (v/V/Ctrl+V).

use super::{ViMode, ViVisualType};

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

    /// Cancel visual selection mode without exiting vi mode.
    pub fn cancel_visual(&mut self) {
        self.visual_anchor = None;
        self.visual_type = None;
    }
}
