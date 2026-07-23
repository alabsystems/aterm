// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::fmt::Write;

use super::Row;

impl std::fmt::Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for cell in &self.as_slice()[..self.len() as usize] {
            if !cell.is_wide_continuation() {
                // NUL (empty cell) → space, matching push_cell_text() in
                // content.rs. Without this, scrollback text extraction via
                // Line::to_string() can contain literal NUL bytes while
                // visible-row extraction converts them to spaces (#7465).
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
