// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS RING'S FILE (`docs/DESIGN-harness-occupancy-2026-10-20.md` §6).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_agent::supervise::occupancy::ring::{self, RING_ROWS, RingRow};

/// THE RING'S FILE.
#[derive(Debug)]
pub(crate) struct RingFile {
    path: PathBuf,
    /// The rows the file holds, as last counted; `None` before the first append counts them.
    rows: Option<usize>,
    /// The rows kept ([`RING_ROWS`]).
    cap: usize,
}

impl RingFile {
    /// The ring at `path` ([`ring::ring_path`]).
    #[must_use]
    pub(crate) const fn at(path: PathBuf) -> Self {
        Self {
            path,
            rows: None,
            cap: RING_ROWS,
        }
    }

    /// The ring at `path` keeping `cap` rows: a test's small ring.
    #[cfg(test)]
    pub(crate) const fn capped(path: PathBuf, cap: usize) -> Self {
        Self {
            path,
            rows: None,
            cap,
        }
    }

    /// Its path.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The rows the file holds now, oldest first (none where it cannot be read).
    #[must_use]
    pub(crate) fn read(&self) -> Vec<RingRow> {
        std::fs::read_to_string(&self.path)
            .map(|body| ring::rows_in(&body))
            .unwrap_or_default()
    }

    /// Append `row`.
    /// # Errors
    /// The directory could not be made, or the append, the flush or the trim failed.
    pub(crate) fn append(&mut self, row: &RingRow) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let held = match self.rows {
            Some(n) => n,
            None => std::fs::read_to_string(&self.path).map_or(0, |body| body.lines().count()),
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut line = row.line();
        line.push('\n');
        file.write_all(line.as_bytes())?;
        aterm_durable::ordered(&file)?;
        drop(file);
        let held = held.saturating_add(1);
        if held < 2 * self.cap {
            self.rows = Some(held);
            return Ok(());
        }
        let rows = self.read();
        let keep = &rows[rows.len().saturating_sub(self.cap)..];
        let mut text = String::new();
        for row in keep {
            text.push_str(&row.line());
            text.push('\n');
        }
        aterm_agent::harness::upgrade_models::write_atomic_ordered(&self.path, &text)?;
        self.rows = Some(keep.len());
        Ok(())
    }
}

#[cfg(test)]
#[path = "occupancy_ring_tests.rs"]
mod tests;
