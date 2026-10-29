// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::fmt::Write;

use super::{Row, u16_from_usize};

impl std::fmt::Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (col, cell) in self.as_slice()[..usize::from(self.len())]
            .iter()
            .enumerate()
        {
            if !self.is_cell_wide_continuation(u16_from_usize(col)) {
                // NUL (empty cell) → space, as push_cell_text() does, so
                // scrollback text matches visible-row text.
                let ch = cell.char();
                f.write_char(if ch == '\0' { ' ' } else { ch })?;
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Row")
            .field("cols", &self.cols())
            .field("len", &self.len())
            .field("flags", &self.flags())
            .field("content", &self.to_string())
            .finish()
    }
}
