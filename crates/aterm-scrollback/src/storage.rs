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
        // Defense-in-depth: a recursive create is a no-op on the mode of an
        // ALREADY-EXISTING final directory, so a pre-existing looser dir (e.g.
        // 0o755) would leave scrollback files traversable by other local users
        // regardless of the files' own bits. Enforce owner-only (0o700) on the
        // final component unconditionally — a no-op when we just created it
        // 0o700, and fail-closed if we cannot chmod a pre-existing dir we do not
        // own (we then refuse to use a directory we cannot secure).
        std::fs::set_permissions(path, Permissions::from_mode(0o700))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Iterator over scrollback storage lines (oldest to newest).
pub struct ScrollbackStorageIter<'a> {
    pub(crate) storage: &'a ScrollbackStorage,
    pub(crate) idx: usize,
}

impl Iterator for ScrollbackStorageIter<'_> {
    type Item = Line;

    // Skip: the tier-walk driver — routes into the per-tier `get_line`s (each
    // individually classified: guarded-index / decode class).
    #[cfg_attr(trust_verify, trust::skip)]
    fn next(&mut self) -> Option<Self::Item> {
        let total = self.storage.line_count();
        // `saturating_add(1)`: the loop guard keeps `idx < total <= usize::MAX`,
        // so the increment can never overflow on a real path — the saturation
        // just discharges the strict L0 gate's unconstrained-`idx` overflow
        // counterexample (same cursor idiom as aterm-buffer's id counters).
        while self.idx < total {
            match self.storage.get_line(self.idx) {
                Ok(Some(cow_line)) => {
                    self.idx = self.idx.saturating_add(1);
                    return Some(cow_line.into_owned());
                }
                Ok(None) => return None,
                Err(e) => {
                    // Direct `warn!`: the error interpolation cannot be
                    // pre-rendered without the fmt machinery (the `Io` variant
                    // wraps `std::io::Error`), and routing it through a shim
                    // whose local-callee closure includes `ScrollbackError`'s
                    // Display sends the strict gate's CHC engine into a
                    // non-terminating solve. The macro-expanded `fmt::Arguments`
                    // here leaves ONE documented full-verify gap on this
                    // function (a toolchain limitation, not a refutation).
                    aterm_log::warn!("storage iter: skipping line {}: {e}", self.idx);
                    self.idx = self.idx.saturating_add(1);
                }
            }
        }
        None
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

// ============================================================================
// ScrollbackStorage - Unified storage abstraction for Grid
// ============================================================================

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
    /// Only present with the opt-in `disk-tier` feature (§2.7: the default
    /// engine build is headless, no mmap/libc/std::fs).
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

// Devirtualized dispatch over the two concrete backends. The former
// `inner()`/`inner_mut()` helpers erased the backend to `&dyn ScrollbackAccess`,
// so every delegated call was a virtual call whose callee body the strict L0
// gate could not resolve in the lowered bundle — each of the 19 delegations
// carried a [trust-absent-callee-assumption] "may panic" obligation. Matching
// on the enum and invoking the trait method on the CONCRETE backend performs
// the exact dispatch decision the vtable would have made (the `#[non_exhaustive]`
// enum has exactly these two variants, and both arms call the same
// `ScrollbackAccess` impl the vtable slot pointed at — including the memory
// backend's `Ok(())`-wrapping `push_line`/`clear` shims), so the observable
// behavior is byte-identical while the callee becomes a statically-known local
// function the verifier can see. The public `ScrollbackAccess` trait and its
// impls are unchanged; only this internal dispatch devirtualizes.
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
    // --- Delegated via ScrollbackAccess (19 methods) ---

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

    /// Get the hot+warm memory usage (bytes).
    #[must_use]
    pub fn memory_used(&self) -> usize {
        dispatch!(self, memory_used())
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
    /// Returns an error if decompression or I/O fails during line extraction.
    /// On error, scrollback state is unchanged (no lines lost). (#4638)
    pub fn remove_newest(&mut self, n: usize) -> Result<(), ScrollbackError> {
        dispatch!(self, remove_newest(n))
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

    /// Create a snapshot containing only hot + warm tier lines (skip cold).
    ///
    /// Fast path for checkpointing under a lock: cold tier decompression
    /// (Zstd or disk I/O) is skipped, bounding the snapshot to at most
    /// `hot_limit + warm_limit` lines (~11K with defaults). This prevents
    /// multi-second UI freezes during auto-save. (#5946)
    ///
    /// Use `checkpoint_snapshot()` when a full-fidelity snapshot is needed
    /// (e.g., offline migration or explicit user-triggered save).
    #[must_use]
    pub fn checkpoint_snapshot_fast(&self) -> Scrollback {
        dispatch!(self, checkpoint_snapshot_fast())
    }

    // --- Non-trait methods (variant-specific or self-referential) ---

    /// Create an in-memory snapshot of this storage for serialization.
    ///
    /// Preserves line order and runtime limits across both memory-only and
    /// disk-backed variants. Skips lines with decompression/I/O errors and
    /// logs the total count of skipped lines. (#4641)
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

    /// Corrupt the oldest warm block for cross-crate behavioral tests.
    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub fn corrupt_oldest_warm_block_for_testing(&mut self) -> bool {
        match self {
            ScrollbackStorage::Memory(sb) => {
                if sb.warm.block_count() == 0 {
                    return false;
                }
                sb.warm.corrupt_oldest_block();
                true
            }
            #[cfg(feature = "disk-tier")]
            ScrollbackStorage::Disk(_) => false,
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

    /// Defense-in-depth regression: a PRE-EXISTING world-traversable (0o755) dir
    /// must be TIGHTENED to 0o700, since a recursive create is a no-op on an
    /// existing dir's mode — without this, scrollback files would be readable by
    /// other local users despite their own restrictive bits.
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
