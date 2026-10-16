// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

#[cfg(feature = "disk-tier")]
use super::DiskBackedScrollback;
use super::access::ScrollbackAccess;
use super::{Line, Scrollback, ScrollbackError, WatermarkLevel};

// Only the disk-backed cold tier creates on-disk storage directories; the
// default (headless) build never touches `std::fs`.
#[cfg(feature = "disk-tier")]
pub(crate) fn create_dir_restricted(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::{DirBuilder, Permissions};
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        // A recursive create leaves an existing final directory's mode alone, and
        // a looser one would let other local users reach the files: enforce 0o700
        // unconditionally. Failing to chmod a directory we do not own fails closed.
        std::fs::set_permissions(path, Permissions::from_mode(0o700))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Iterator over scrollback storage lines (oldest to newest).
///
/// Streams whole decoded segments: each warm block or cold page is decoded once
/// and its lines moved out. A corrupt segment is skipped whole (one warning per
/// segment), matching [`super::ScrollbackIter`].
pub struct ScrollbackStorageIter<'a> {
    pub(crate) storage: &'a ScrollbackStorage,
    pub(crate) idx: usize,
    /// Decoded lines of the current segment, drained front-to-back.
    pub(crate) buf: std::collections::VecDeque<Line>,
}

impl Iterator for ScrollbackStorageIter<'_> {
    type Item = Line;

    // Skip: the segment-walk driver — routes into the per-tier bulk reads
    // (each individually classified: guarded-index / decode class).
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.buf.pop_front() {
                // saturating_add discharges the strict L0 gate's unconstrained-`idx`
                // overflow counterexample; `idx < total` on every real path.
                self.idx = self.idx.saturating_add(1);
                return Some(line);
            }
            let total = self.storage.line_count();
            if self.idx >= total {
                return None;
            }
            match self.storage.read_segment(self.idx) {
                Ok(lines) => {
                    if lines.is_empty() {
                        // In-range index with no data (stale count or short
                        // decode): end of data.
                        return None;
                    }
                    self.buf = std::collections::VecDeque::from(lines);
                }
                Err((e, skip)) => {
                    // Direct `warn!`: an error cannot go through a log shim (see
                    // log_shim.rs), which leaves one documented full-verify gap.
                    aterm_log::warn!("storage iter: skipping {skip} line(s) at {}: {e}", self.idx);
                    // Clamp so a corrupt tail segment cannot push the cursor
                    // past `total`; `max(1)` guarantees forward progress.
                    let skip = skip.min(total.saturating_sub(self.idx)).max(1);
                    self.idx = self.idx.saturating_add(skip);
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.storage.line_count().saturating_sub(self.idx);
        (0, Some(remaining))
    }
}

/// Iterator over scrollback storage lines (newest to oldest).
#[cfg(test)]
pub(crate) struct ScrollbackStorageRevIter<'a> {
    storage: &'a ScrollbackStorage,
    rev_idx: usize,
}

#[cfg(test)]
impl<'a> Iterator for ScrollbackStorageRevIter<'a> {
    type Item = Line;

    fn next(&mut self) -> Option<Self::Item> {
        let total = self.storage.line_count();
        while self.rev_idx < total {
            match self.storage.get_line_rev(self.rev_idx) {
                Ok(Some(cow_line)) => {
                    self.rev_idx += 1;
                    return Some(cow_line.into_owned());
                }
                Ok(None) => return None,
                Err(e) => {
                    aterm_log::warn!("storage rev_iter: skipping rev_index {}: {e}", self.rev_idx);
                    self.rev_idx += 1;
                }
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.storage.line_count().saturating_sub(self.rev_idx);
        (0, Some(remaining))
    }
}

/// Unified scrollback storage abstraction.
///
/// This enum provides a common interface for both memory-only [`Scrollback`]
/// and disk-backed [`DiskBackedScrollback`], allowing `Grid` to work
/// with either storage backend transparently.
///
/// # Examples
///
/// Using with memory storage (default):
///
/// ```
/// use aterm_scrollback::{ScrollbackStorage, Scrollback, Line};
///
/// // Create from Scrollback
/// let mut storage: ScrollbackStorage = Scrollback::with_defaults().into();
///
/// // Push lines (returns Result for disk-backed compatibility)
/// storage.push_line(Line::from("Hello")).unwrap();
/// storage.push_line(Line::from("World")).unwrap();
///
/// // Access lines
/// assert_eq!(storage.line_count(), 2);
/// assert_eq!(storage.get_line_rev(0).unwrap().unwrap().to_string(), "World");
/// ```
#[non_exhaustive]
#[derive(Debug)]
pub enum ScrollbackStorage {
    /// In-memory tiered scrollback.
    Memory(Scrollback),
    /// Disk-backed scrollback with mmap cold tier.
    ///
    /// Only with the opt-in `disk-tier` feature: the default build is headless
    /// (no mmap, libc or `std::fs`).
    #[cfg(feature = "disk-tier")]
    Disk(DiskBackedScrollback),
}

/// Per-tier line counts for scrollback storage.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TierLineCounts {
    pub(crate) hot: usize,
    pub(crate) warm: usize,
    pub(crate) cold: usize,
}

/// Cold tier usage metrics.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColdMetrics {
    pub(crate) memory_used: usize,
    pub(crate) disk_used: Option<usize>,
}

// Static dispatch over the two concrete backends: through `&dyn
// ScrollbackAccess` the strict L0 gate cannot resolve the callee in the lowered
// bundle and charges every delegation a "may panic" obligation. Matching on the
// enum calls the same `ScrollbackAccess` impl the vtable would.
macro_rules! dispatch {
    ($self:expr, $method:ident($($arg:expr),*)) => {
        match $self {
            ScrollbackStorage::Memory(sb) => ScrollbackAccess::$method(sb $(, $arg)*),
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(sb) => ScrollbackAccess::$method(sb $(, $arg)*),
        }
    };
}

impl ScrollbackStorage {
    // --- Delegated via ScrollbackAccess ---

    /// Get the total number of lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        dispatch!(self, line_count())
    }

    /// Get a line by index (0 = oldest).
    ///
    /// Returns `Cow::Borrowed` for hot-tier lines (zero-copy) and
    /// `Cow::Owned` for warm/cold-tier lines (decompressed on access).
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for I/O or decompression failures.
    #[must_use = "line data is discarded if not consumed"]
    pub fn get_line(&self, idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        dispatch!(self, get_line(idx))
    }

    /// Get a line by reverse index (0 = newest).
    ///
    /// Returns `Ok(None)` for out-of-bounds, `Err` for I/O or decompression failures.
    #[must_use = "line data is discarded if not consumed"]
    pub fn get_line_rev(&self, rev_idx: usize) -> Result<Option<Cow<'_, Line>>, ScrollbackError> {
        dispatch!(self, get_line_rev(rev_idx))
    }

    /// Get the hot tier limit.
    #[must_use]
    pub fn hot_limit(&self) -> usize {
        dispatch!(self, hot_limit())
    }

    /// Get the warm tier limit.
    #[must_use]
    pub fn warm_limit(&self) -> usize {
        dispatch!(self, warm_limit())
    }

    /// Get the line limit.
    #[must_use]
    pub fn line_limit(&self) -> Option<usize> {
        dispatch!(self, line_limit())
    }

    /// Get the memory budget.
    #[must_use]
    pub fn memory_budget(&self) -> usize {
        dispatch!(self, memory_budget())
    }

    /// Get reclaimable storage bytes used for budget enforcement.
    #[must_use]
    pub fn budgeted_memory_used(&self) -> usize {
        dispatch!(self, budgeted_memory_used())
    }

    /// Get total memory usage across all tiers (bytes).
    ///
    /// For disk-backed storage, this excludes disk usage.
    #[must_use]
    pub fn total_memory_used(&self) -> usize {
        dispatch!(self, total_memory_used())
    }

    /// Get the number of lines in cold tier.
    #[must_use]
    pub fn cold_line_count(&self) -> usize {
        dispatch!(self, cold_line_count())
    }

    /// Get cold tier memory usage (bytes).
    #[must_use]
    #[cfg(test)]
    pub fn cold_memory_used(&self) -> usize {
        dispatch!(self, cold_memory_used())
    }

    /// Get the current memory pressure watermark level.
    ///
    /// Lightweight O(1) read — the level is precomputed on every accounting sync.
    #[must_use]
    pub fn watermark_level(&self) -> WatermarkLevel {
        dispatch!(self, watermark_level())
    }

    /// Lines dropped by memory-pressure eviction: real retention loss, reported
    /// out of band. The disk-backed store spills warm blocks to its disk cold
    /// tier instead (retained), so it reports 0. The in-memory store also counts
    /// the lines of a quarantined warm block, whose rows stay in
    /// [`line_count`](Self::line_count) as blank rows when older history sits
    /// before them, so the two can overlap.
    #[must_use]
    pub fn pressure_evicted_lines(&self) -> u64 {
        match self {
            ScrollbackStorage::Memory(sb) => sb.pressure_evicted_lines(),
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(_) => 0,
        }
    }

    /// History lines whose inline image was discarded because the line was
    /// compressed, reported by both backends (spilling to disk retains a line,
    /// but compression has already dropped its picture). See
    /// [`Scrollback::image_rows_dropped_by_compression`].
    #[must_use]
    pub fn image_rows_dropped_by_compression(&self) -> u64 {
        dispatch!(self, image_rows_dropped_by_compression())
    }

    /// Push a new line to the scrollback.
    // Skip: the tier-promotion driver — the blanket-drain/alloc class of the
    // tier operations it dispatches into (each individually classified).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn push_line(&mut self, line: Line) -> std::io::Result<()> {
        dispatch!(self, push_line(line))
    }

    /// Clear all lines from scrollback.
    // Skip: dispatches the per-tier clears (drop-glue lane).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn clear(&mut self) -> std::io::Result<()> {
        dispatch!(self, clear())
    }

    /// Remove the `n` most recent lines from scrollback.
    ///
    /// Per the Kitty unscroll spec: "The lines that have been scrolled into
    /// the scrollback buffer are removed from the scrollback buffer."
    ///
    /// Returns an error if decompression or I/O fails during line extraction;
    /// the scrollback is then unchanged.
    pub fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, remove_newest(n))
    }

    /// Drop the `n` oldest lines, keeping the newest `line_count() - n`;
    /// `n >= line_count()` clears the store.
    ///
    /// Both backends advance a `front_offset` through cold → warm → hot, dropping
    /// whole blocks/pages as they empty, with no decompression (O(1) amortized
    /// per line). The off-thread reflow reads its input from the front of the
    /// store it refills at the back; this releases consumed input as it goes,
    /// keeping its transient memory O(step budget) rather than O(history).
    // Skip: dispatches into the tier truncations (blanket-drain class), like
    // `set_line_limit` below.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn truncate_oldest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        if n == 0 {
            return Ok(());
        }
        let keep = self.line_count().saturating_sub(n);
        match self {
            ScrollbackStorage::Memory(sb) => sb.truncate(keep),
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(sb) => sb.truncate(keep),
        }
    }

    /// Set the line limit.
    // Skip: dispatches into the tier truncations (blanket-drain class).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn set_line_limit(&mut self, limit: Option<usize>) {
        dispatch!(self, set_line_limit(limit));
    }

    /// Set the memory budget (bytes).
    ///
    /// Returns `Err` if enforcement failed (e.g. corrupted data prevented
    /// eviction and usage still exceeds the new budget).
    // Skip: dispatches into the tier evictions (blanket-drain/alloc class).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn set_memory_budget(&mut self, budget: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, set_memory_budget(budget))
    }

    /// Create a snapshot of only the hot and warm tier lines, skipping cold-tier
    /// decompression: at most `hot_limit + warm_limit` lines (~11K by default).
    /// [`checkpoint_snapshot`](Self::checkpoint_snapshot) is the full snapshot.
    #[must_use]
    #[cfg(test)]
    pub fn checkpoint_snapshot_fast(&self) -> Scrollback {
        dispatch!(self, checkpoint_snapshot_fast())
    }

    // --- Non-trait methods (variant-specific or self-referential) ---

    /// Create an in-memory snapshot of this storage for serialization.
    ///
    /// Preserves line order and runtime limits for both backends. Skips lines
    /// with decompression/I/O errors and logs how many.
    #[must_use]
    // Skip: the full snapshot walk — routes into the tier `get_line`s (each
    // individually classified) plus Vec/Line clones (idiomatic-alloc class).
    // Round-trip tested by the checkpoint gate.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn checkpoint_snapshot(&self) -> Scrollback {
        let mut snapshot =
            Scrollback::new(self.hot_limit(), self.warm_limit(), self.memory_budget());
        snapshot.set_line_limit(self.line_limit());
        let expected = match self.line_limit() {
            Some(limit) => self.line_count().min(limit),
            None => self.line_count(),
        };
        for line in self.iter() {
            snapshot.push_line(line);
        }
        let actual = snapshot.line_count();
        if actual < expected {
            aterm_log::warn!(
                "checkpoint_snapshot: {actual}/{expected} lines saved ({} skipped due to errors)",
                expected - actual,
            );
        }
        snapshot
    }

    /// Get per-tier line counts.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tier_line_counts(&self) -> TierLineCounts {
        match self {
            ScrollbackStorage::Memory(sb) => TierLineCounts {
                hot: sb.hot_line_count(),
                warm: sb.warm_line_count(),
                cold: sb.cold_line_count(),
            },
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(sb) => TierLineCounts {
                hot: sb.hot_line_count(),
                warm: sb.warm_line_count(),
                cold: sb.cold_line_count(),
            },
        }
    }

    /// Check if this storage is disk-backed.
    ///
    /// Always `false` without the `disk-tier` feature.
    #[must_use]
    #[cfg(test)]
    pub fn is_disk_backed(&self) -> bool {
        #[cfg(feature = "disk-tier")]
        {
            matches!(self, ScrollbackStorage::Disk(_))
        }
        #[cfg(not(feature = "disk-tier"))]
        {
            false
        }
    }

    /// Get cold tier disk usage (bytes, compressed).
    ///
    /// Returns `None` for memory-only scrollback (always `None` without the
    /// `disk-tier` feature).
    #[must_use]
    #[cfg(test)]
    pub fn cold_disk_used(&self) -> Option<usize> {
        match self {
            ScrollbackStorage::Memory(_) => None,
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(sb) => Some(sb.cold_disk_used()),
        }
    }

    /// Get cold tier usage metrics (memory + optional disk).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cold_metrics(&self) -> ColdMetrics {
        ColdMetrics {
            memory_used: self.cold_memory_used(),
            disk_used: self.cold_disk_used(),
        }
    }

    /// Iterate over all lines (oldest to newest).
    #[must_use]
    pub fn iter(&self) -> ScrollbackStorageIter<'_> {
        ScrollbackStorageIter {
            storage: self,
            idx: 0,
            buf: std::collections::VecDeque::new(),
        }
    }

    /// Bulk segment read backing [`ScrollbackStorageIter`] — devirtualized
    /// dispatch to the concrete backend's `read_segment` (see the `dispatch!`
    /// rationale above).
    pub(crate) fn read_segment(&self, idx: usize) -> crate::iter::SegmentResult {
        match self {
            ScrollbackStorage::Memory(sb) => sb.read_segment(idx),
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(sb) => sb.read_segment(idx),
        }
    }

    /// Iterate over lines in reverse (newest to oldest).
    #[cfg(test)]
    pub(crate) fn iter_rev(&self) -> ScrollbackStorageRevIter<'_> {
        ScrollbackStorageRevIter {
            storage: self,
            rev_idx: 0,
        }
    }
}

impl Default for ScrollbackStorage {
    fn default() -> Self {
        ScrollbackStorage::Memory(Scrollback::with_defaults())
    }
}

impl From<Scrollback> for ScrollbackStorage {
    fn from(sb: Scrollback) -> Self {
        ScrollbackStorage::Memory(sb)
    }
}

#[cfg(feature = "disk-tier")]
impl From<DiskBackedScrollback> for ScrollbackStorage {
    fn from(sb: DiskBackedScrollback) -> Self {
        ScrollbackStorage::Disk(sb)
    }
}

#[cfg(all(test, unix, feature = "disk-tier"))]
mod restricted_dir_tests {
    use super::create_dir_restricted;
    use std::os::unix::fs::PermissionsExt;

    /// A freshly-created scrollback dir must be owner-only (0o700).
    #[test]
    fn create_dir_restricted_makes_fresh_dir_0700() {
        let base = std::env::temp_dir().join(format!("aterm_restr_fresh_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        create_dir_restricted(&base).expect("create");
        let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "fresh dir should be 0o700, got 0o{mode:03o}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A pre-existing 0o755 dir is tightened to 0o700: a recursive create leaves
    /// an existing dir's mode alone.
    #[test]
    fn create_dir_restricted_tightens_preexisting_loose_dir() {
        let base = std::env::temp_dir().join(format!("aterm_restr_loose_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            std::fs::metadata(&base).unwrap().permissions().mode() & 0o777,
            0o755
        );

        create_dir_restricted(&base).expect("re-restrict existing dir");

        let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "pre-existing 0o755 dir must be tightened to 0o700, got 0o{mode:03o}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
