// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS RING'S FILE (`docs/DESIGN-harness-occupancy-2026-10-20.md` §6).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use aterm_agent::supervise::occupancy::ring::{self, RING_ROWS, RingRow};

const MAX_RING_BYTES: u64 = 8 * 1024 * 1024;

/// Read the facts ring file at `path` without following symlinks or blocking on a FIFO.
pub(crate) fn read_ring_text(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "occupancy ring is a symlink",
        ));
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_RING_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "occupancy ring is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(MAX_RING_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RING_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "occupancy ring exceeds size bound",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "occupancy ring is not UTF-8",
        )
    })
}

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
        read_ring_text(&self.path)
            .map(|body| ring::rows_in(&body))
            .unwrap_or_default()
    }

    /// Append `row`, then trim to the newest [`RING_ROWS`] where the file holds twice that.
    /// # Errors
    /// The directory could not be made, or the append, the flush or the trim failed.
    pub(crate) fn append(&mut self, row: &RingRow) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
            if !std::fs::symlink_metadata(dir)?.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "occupancy ring directory is not a regular directory",
                ));
            }
        }
        let held = match self.rows {
            Some(n) => n,
            None => read_ring_text(&self.path).map_or(0, |body| body.lines().count()),
        };
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        #[cfg(not(unix))]
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "occupancy ring is a symlink",
            ));
        }
        let mut file = opts.open(&self.path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "occupancy ring is not a regular file",
            ));
        }
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
        aterm_agent::harness::atomic_write::write_atomic_ordered(&self.path, &text)?;
        self.rows = Some(keep.len());
        Ok(())
    }
}

#[cfg(test)]
#[path = "occupancy_ring_tests.rs"]
mod tests;
