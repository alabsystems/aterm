// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Search-content adapters for scrollback types.

use std::borrow::Cow;

#[cfg(feature = "disk-tier")]
use crate::DiskBackedScrollback;
use crate::{Scrollback, ScrollbackStorage};
use aterm_types::SearchContent;

// Skip: the row reader routes into the tier `get_line`s (guarded-index /
// decode class, each individually classified) plus Cow/String handling.
#[cfg_attr(trust_verify, trust::skip)]
fn read_search_row_text(
    source: &str,
    row: usize,
    line: Result<Option<Cow<'_, crate::Line>>, crate::ScrollbackError>,
) -> Option<String> {
    match line {
        // What `Line`'s Display writes, without the `format_args!` machinery
        // whose macro-expanded unsafe the strict Trust gate fails closed on.
        Ok(Some(line)) => Some(String::from_utf8_lossy(line.as_bytes()).into_owned()),
        Ok(None) => None,
        Err(error) => {
            // Direct `warn!`: an error cannot go through a log shim (see
            // log_shim.rs), which leaves one documented full-verify gap.
            aterm_log::warn!("{source}::get_row_text({row}) failed: {error}");
            None
        }
    }
}

// Skip: same tier-lookup class as `read_search_row_text`.
#[cfg_attr(trust_verify, trust::skip)]
fn read_is_row_wrapped(line: Result<Option<Cow<'_, crate::Line>>, crate::ScrollbackError>) -> bool {
    matches!(line, Ok(Some(l)) if l.is_wrapped())
}

impl SearchContent for Scrollback {
    fn row_count(&self) -> usize {
        self.line_count()
    }

    fn get_row_text(&mut self, row: usize) -> Option<String> {
        read_search_row_text("Scrollback", row, self.get_line(row))
    }

    fn is_row_wrapped(&self, row: usize) -> bool {
        read_is_row_wrapped(self.get_line(row))
    }
}

impl SearchContent for ScrollbackStorage {
    fn row_count(&self) -> usize {
        self.line_count()
    }

    fn get_row_text(&mut self, row: usize) -> Option<String> {
        read_search_row_text("ScrollbackStorage", row, self.get_line(row))
    }

    fn is_row_wrapped(&self, row: usize) -> bool {
        read_is_row_wrapped(self.get_line(row))
    }
}

#[cfg(feature = "disk-tier")]
impl SearchContent for DiskBackedScrollback {
    fn row_count(&self) -> usize {
        self.line_count()
    }

    fn get_row_text(&mut self, row: usize) -> Option<String> {
        read_search_row_text("DiskBackedScrollback", row, self.get_line(row))
    }

    fn is_row_wrapped(&self, row: usize) -> bool {
        read_is_row_wrapped(self.get_line(row))
    }
}
