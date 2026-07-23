// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memory-bounded scrollback with disk spill.
//!
//! Provides a budget system for the grid's scrollback storage. When the
//! in-memory scrollback exceeds the configured budget (default 256 MiB),
//! oldest cold-tier lines are serialized and evicted to a memory-mapped
//! temp file. Reads from spilled lines go through the kernel page cache
//! via mmap, so they remain fast without consuming application heap.
//!
//! The spill file is created in a temp directory and cleaned up on drop.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │  Grid  ──► drain_lazy_buffer() ──► ScrollbackStorage │
//! │                                         │            │
//! │                              ┌──────────┘            │
//! │                              ▼                       │
//! │                    ScrollbackBudget                   │
//! │                    ┌─────────────────┐               │
//! │                    │ budget: 256 MiB │               │
//! │                    │ spill_state:    │               │
//! │                    │   mmap file     │               │
//! │                    └─────────────────┘               │
//! └──────────────────────────────────────────────────────┘
//! ```

use std::io;

use aterm_scrollback::mmap::MmapMut;
use aterm_scrollback::{Line, ScrollbackError, ScrollbackStorage};

/// Default memory budget for in-memory scrollback: 256 MiB.
const DEFAULT_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Minimum useful budget: 64 KiB. Budgets below this are clamped up.
const MIN_BUDGET_BYTES: usize = 64 * 1024;

/// Number of lines to evict per spill batch. Evicting in batches
/// amortizes the serialization overhead.
const SPILL_BATCH_SIZE: usize = 512;

/// Initial mmap file capacity (1 MiB). Grows on demand.
const INITIAL_SPILL_CAPACITY: u64 = 1024 * 1024;

/// Errors from the scrollback budget system.
#[non_exhaustive]
#[derive(Debug, aterm_error::Error)]
pub enum BudgetError {
    /// I/O error creating or writing the spill file.
    #[error("disk spill I/O error: {0}")]
    Io(#[from] io::Error),
    /// Scrollback access error during eviction.
    #[error("scrollback error during budget enforcement: {0}")]
    Scrollback(#[from] ScrollbackError),
}

/// Configuration for memory-bounded scrollback.
#[derive(Debug, Clone, Copy)]
pub struct ScrollbackBudget {
    /// Maximum bytes for in-memory scrollback before disk spill.
    max_bytes: usize,
}

impl ScrollbackBudget {
    /// Create a budget with the given maximum bytes.
    ///
    /// The minimum effective budget is 1 MiB; smaller values are clamped.
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes: max_bytes.max(MIN_BUDGET_BYTES),
        }
    }

    /// Get the configured maximum bytes.
    #[must_use]
    #[inline]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

impl Default for ScrollbackBudget {
    fn default() -> Self {
        Self::new(DEFAULT_BUDGET_BYTES)
    }
}

/// Tracks memory usage and manages disk spill for a grid's scrollback.
///
/// Sits between the grid and `ScrollbackStorage`, monitoring memory after
/// each lazy-buffer drain and evicting oldest lines to an mmap file when
/// the budget is exceeded.
#[derive(Debug)]
pub(crate) struct BudgetEnforcer {
    /// The configured memory budget.
    budget: ScrollbackBudget,
    /// Disk spill state. `None` until the first spill is triggered.
    spill: Option<SpillState>,
    /// Total lines currently spilled to disk.
    spilled_line_count: usize,
    /// Approximate bytes currently on disk (serialized).
    spilled_bytes: usize,
}

/// State for the memory-mapped spill file.
#[derive(Debug)]
struct SpillState {
    /// The temporary file backing the mmap. Kept alive for cleanup on drop.
    _file: aterm_tempfile::NamedTempFile,
    /// Mutable memory map over the spill file.
    mmap: MmapMut,
    /// Current write offset into the mmap.
    write_offset: usize,
    /// File capacity (mmap length). Grown by doubling.
    capacity: usize,
    /// Index of spilled line records: (offset, length) pairs.
    index: Vec<SpillIndexEntry>,
}

/// An entry in the spill file index.
#[derive(Debug, Clone, Copy)]
struct SpillIndexEntry {
    /// Byte offset into the mmap.
    offset: usize,
    /// Byte length of the serialized line.
    length: usize,
}

impl BudgetEnforcer {
    /// Create a new enforcer with the given budget.
    pub(crate) fn new(budget: ScrollbackBudget) -> Self {
        Self {
            budget,
            spill: None,
            spilled_line_count: 0,
            spilled_bytes: 0,
        }
    }

    /// Get the configured budget.
    #[must_use]
    #[allow(dead_code, reason = "API for callers querying budget; used in tests")]
    pub(crate) fn budget(&self) -> &ScrollbackBudget {
        &self.budget
    }

    /// Update the budget. Does not immediately enforce; enforcement
    /// happens on the next drain cycle.
    #[allow(dead_code, reason = "API for runtime budget adjustment; used in tests")]
    pub(crate) fn set_budget(&mut self, budget: ScrollbackBudget) {
        self.budget = budget;
    }

    /// Total lines currently spilled to disk.
    #[must_use]
    #[allow(dead_code, reason = "API for querying spill state; used in tests")]
    pub(crate) fn spilled_line_count(&self) -> usize {
        self.spilled_line_count
    }

    /// Approximate bytes currently stored on disk.
    #[must_use]
    #[allow(dead_code, reason = "API for querying spill state; used in tests")]
    pub(crate) fn spilled_bytes(&self) -> usize {
        self.spilled_bytes
    }

    /// Check whether the scrollback's memory usage exceeds the budget.
    #[must_use]
    fn is_over_budget(&self, scrollback: &ScrollbackStorage) -> bool {
        scrollback.total_memory_used() > self.budget.max_bytes
    }

    /// Enforce the memory budget by evicting oldest lines from the
    /// scrollback to the disk spill file.
    ///
    /// Called after `drain_lazy_buffer` pushes lines into the scrollback.
    /// Returns the number of lines evicted, or an error if spill failed.
    pub(crate) fn enforce(
        &mut self,
        scrollback: &mut ScrollbackStorage,
    ) -> Result<usize, BudgetError> {
        if !self.is_over_budget(scrollback) {
            return Ok(0);
        }

        let mut total_evicted = 0;

        while self.is_over_budget(scrollback) {
            let cold_count = scrollback.cold_line_count();
            if cold_count == 0 {
                // Nothing left to evict from cold tier. The hot/warm tiers
                // are needed for recent access so we stop here.
                break;
            }

            let batch = cold_count.min(SPILL_BATCH_SIZE);
            if batch == 0 {
                break;
            }

            // Read oldest lines from scrollback before evicting them.
            let mut lines = Vec::with_capacity(batch);
            for i in 0..batch {
                match scrollback.get_line(i) {
                    Ok(Some(cow)) => lines.push(cow.into_owned()),
                    Ok(None) => break,
                    Err(e) => {
                        aterm_log::warn!("budget enforce: failed to read line {i}: {e}");
                        // Push a placeholder so indices stay consistent.
                        lines.push(Line::from(""));
                    }
                }
            }

            if lines.is_empty() {
                break;
            }

            // Serialize and write lines to spill file.
            let count = lines.len();
            self.spill_lines(&lines)?;

            // Set a line limit to remove the evicted lines from the scrollback.
            // We reduce the total count by the number of evicted lines.
            let current_count = scrollback.line_count();
            let new_limit = current_count.saturating_sub(count);

            // Use the scrollback's own line-limit mechanism to discard the oldest.
            let old_limit = scrollback.line_limit();
            scrollback.set_line_limit(Some(new_limit));
            // Restore original limit (or None for unlimited).
            scrollback.set_line_limit(old_limit);

            total_evicted += count;
        }

        Ok(total_evicted)
    }

    /// Write serialized lines to the spill file.
    fn spill_lines(&mut self, lines: &[Line]) -> Result<(), BudgetError> {
        self.ensure_spill_state()?;

        let mut added_bytes: usize = 0;
        let added_lines = lines.len();

        // The borrow checker requires us to re-borrow spill inside the loop
        // because grow() takes &mut self on SpillState.
        for line in lines {
            let serialized = serialize_line(line);
            let len = serialized.len();

            let spill = self
                .spill
                .as_mut()
                .expect("invariant: spill initialized by ensure_spill_state");

            // Grow mmap if needed.
            if spill.write_offset + len > spill.capacity {
                spill.grow(spill.write_offset + len)?;
            }

            // Write into the mmap.
            let start = spill.write_offset;
            let end = start + len;
            spill.mmap[start..end].copy_from_slice(&serialized);
            spill.index.push(SpillIndexEntry {
                offset: start,
                length: len,
            });
            spill.write_offset = end;
            added_bytes += len;
        }

        self.spilled_bytes += added_bytes;
        self.spilled_line_count += added_lines;

        // Flush changes to disk.
        if let Some(spill) = self.spill.as_mut() {
            spill.mmap.flush()?;
        }

        Ok(())
    }

    /// Ensure the spill state is initialized, creating the temp file if needed.
    fn ensure_spill_state(&mut self) -> Result<&mut SpillState, BudgetError> {
        if self.spill.is_none() {
            let file = aterm_tempfile::NamedTempFile::new()?;
            file.as_file().set_len(INITIAL_SPILL_CAPACITY)?;

            let capacity = INITIAL_SPILL_CAPACITY as usize;
            // SAFETY: The file was just created and set to INITIAL_SPILL_CAPACITY
            // bytes. The mmap is valid for the file's current length. We hold
            // exclusive ownership of the file via NamedTempFile, so no other
            // process can modify it concurrently.
            let mmap = unsafe { MmapMut::map_mut(file.as_file())? };

            self.spill = Some(SpillState {
                _file: file,
                mmap,
                write_offset: 0,
                capacity,
                index: Vec::new(),
            });
        }

        // The borrow checker needs this pattern (we just ensured it's Some).
        Ok(self
            .spill
            .as_mut()
            .expect("invariant: spill was just initialized"))
    }

    /// Read a spilled line by its index (0 = oldest spilled line).
    ///
    /// Returns `None` if the index is out of bounds.
    pub(crate) fn get_spilled_line(&self, idx: usize) -> Option<Line> {
        let spill = self.spill.as_ref()?;
        let entry = spill.index.get(idx)?;

        if entry.offset + entry.length > spill.mmap.len() {
            aterm_log::warn!(
                "spilled line {idx} extends beyond mmap: offset={}, length={}, mmap_len={}",
                entry.offset,
                entry.length,
                spill.mmap.len()
            );
            return None;
        }

        let data = &spill.mmap[entry.offset..entry.offset + entry.length];
        deserialize_line(data)
    }

    /// Query current scrollback memory usage including spill state.
    #[must_use]
    pub(crate) fn memory_stats(
        &self,
        scrollback: Option<&ScrollbackStorage>,
    ) -> ScrollbackMemoryStats {
        let in_memory_bytes = scrollback.map_or(0, ScrollbackStorage::total_memory_used);
        let budget_max = self.budget.max_bytes;
        let spill_index_bytes = self.spill.as_ref().map_or(0, |s| {
            s.index.len() * std::mem::size_of::<SpillIndexEntry>()
        });

        ScrollbackMemoryStats {
            in_memory_bytes,
            budget_max_bytes: budget_max,
            spilled_line_count: self.spilled_line_count,
            spilled_disk_bytes: self.spilled_bytes,
            spill_index_bytes,
        }
    }
}

impl SpillState {
    /// Grow the mmap backing file to at least `min_capacity` bytes.
    fn grow(&mut self, min_capacity: usize) -> Result<(), BudgetError> {
        // Double until we fit, starting from current capacity.
        let mut new_capacity = self.capacity;
        while new_capacity < min_capacity {
            new_capacity = new_capacity.saturating_mul(2).max(min_capacity);
        }

        // Resize the underlying file.
        let file = self._file.as_file();
        file.set_len(new_capacity as u64)?;

        // Remap. We need to drop the old mmap first.
        // SAFETY: The file was resized to new_capacity bytes. We hold
        // exclusive ownership of the file, and the old mmap is being
        // replaced. No other references to the old mmap exist because
        // we have &mut self.
        self.mmap = unsafe { MmapMut::map_mut(file)? };
        self.capacity = new_capacity;

        Ok(())
    }
}

/// Statistics about scrollback memory usage.
#[derive(Debug, Clone, Copy)]
pub struct ScrollbackMemoryStats {
    /// Bytes currently used by in-memory scrollback (all tiers).
    pub in_memory_bytes: usize,
    /// Configured budget maximum.
    pub budget_max_bytes: usize,
    /// Number of lines that have been spilled to disk.
    pub spilled_line_count: usize,
    /// Approximate bytes of serialized line data on disk.
    pub spilled_disk_bytes: usize,
    /// Bytes used by the in-memory spill index.
    pub spill_index_bytes: usize,
}

impl ScrollbackMemoryStats {
    /// Whether the in-memory usage is within budget.
    #[must_use]
    pub fn within_budget(&self) -> bool {
        self.in_memory_bytes <= self.budget_max_bytes
    }

    /// Percentage of budget used (0.0 to 1.0+).
    #[must_use]
    pub fn budget_utilization(&self) -> f64 {
        if self.budget_max_bytes == 0 {
            return 0.0;
        }
        self.in_memory_bytes as f64 / self.budget_max_bytes as f64
    }
}

// ---------------------------------------------------------------------------
// Line serialization (simple format for spill file)
// ---------------------------------------------------------------------------

/// Serialize a Line to bytes for spill storage.
///
/// Format: [text_len: u32][text: bytes][wrapped: u8][attrs_len: u32][attrs: bytes]
///
/// This is a compact format for temporary spill storage (not a stable format).
fn serialize_line(line: &Line) -> Vec<u8> {
    let text = line.to_string();
    let text_bytes = text.as_bytes();
    let wrapped: u8 = u8::from(line.is_wrapped());

    // Pre-compute total size: text_len(4) + text + wrapped(1) + attrs placeholder
    // For simplicity, we store text + wrapped flag. Attributes are serialized
    // via the Line's existing RLE encoding.
    let attrs_data = serialize_attrs(line);

    let total = 4 + text_bytes.len() + 1 + 4 + attrs_data.len();
    let mut buf = Vec::with_capacity(total);

    // text_len (u32 LE)
    let text_len = u32::try_from(text_bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&text_len.to_le_bytes());
    // text
    buf.extend_from_slice(text_bytes);
    // wrapped flag
    buf.push(wrapped);
    // attrs_len (u32 LE)
    let attrs_len = u32::try_from(attrs_data.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&attrs_len.to_le_bytes());
    // attrs data
    buf.extend_from_slice(&attrs_data);

    buf
}

/// Deserialize a Line from spill bytes.
fn deserialize_line(data: &[u8]) -> Option<Line> {
    if data.len() < 9 {
        // Minimum: text_len(4) + wrapped(1) + attrs_len(4)
        return None;
    }

    let text_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let text_end = 4 + text_len;
    if data.len() < text_end + 1 + 4 {
        return None;
    }

    let text = std::str::from_utf8(&data[4..text_end]).ok()?;
    let wrapped = data[text_end] != 0;

    let attrs_len_offset = text_end + 1;
    let attrs_len = u32::from_le_bytes([
        data[attrs_len_offset],
        data[attrs_len_offset + 1],
        data[attrs_len_offset + 2],
        data[attrs_len_offset + 3],
    ]) as usize;

    let attrs_start = attrs_len_offset + 4;
    let attrs_end = attrs_start + attrs_len;
    if data.len() < attrs_end {
        return None;
    }

    let attrs = deserialize_attrs(&data[attrs_start..attrs_end]);

    let mut line = if let Some(attrs_rle) = attrs {
        Line::with_hyperlinks(text, attrs_rle, Vec::new())
    } else {
        Line::from(text)
    };
    if wrapped {
        line.set_wrapped(true);
    }
    Some(line)
}

/// Serialize Line attributes (CellAttrs RLE) to bytes.
///
/// Format: [length: u32][fg: u32][bg: u32][flags: u16][pad: u16] repeated per RLE run.
/// Each run is 14 bytes. The 2-byte pad aligns runs to even boundaries.
fn serialize_attrs(line: &Line) -> Vec<u8> {
    let Some(rle) = line.attrs() else {
        return Vec::new();
    };
    let runs = rle.runs();
    let mut buf = Vec::with_capacity(runs.len() * 14);
    for run in runs {
        buf.extend_from_slice(&run.length.to_le_bytes());
        buf.extend_from_slice(&run.value.fg.to_le_bytes());
        buf.extend_from_slice(&run.value.bg.to_le_bytes());
        buf.extend_from_slice(&run.value.flags.to_le_bytes());
    }
    buf
}

/// Deserialize attributes from bytes.
///
/// Each run is 12 bytes: [length: u32][fg: u32][bg: u32][flags: u16].
fn deserialize_attrs(data: &[u8]) -> Option<aterm_rle::Rle<aterm_scrollback::CellAttrs>> {
    use aterm_scrollback::CellAttrs;

    // Each run: 4 (length) + 4 (fg) + 4 (bg) + 2 (flags) = 14 bytes
    const RUN_SIZE: usize = 14;

    if data.is_empty() {
        return None;
    }
    if !data.len().is_multiple_of(RUN_SIZE) {
        return None;
    }

    let mut rle = aterm_rle::Rle::new();
    let mut offset = 0;
    while offset + RUN_SIZE <= data.len() {
        let length = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        let fg = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]);
        let bg = u32::from_le_bytes([
            data[offset + 8],
            data[offset + 9],
            data[offset + 10],
            data[offset + 11],
        ]);
        let flags = u16::from_le_bytes([data[offset + 12], data[offset + 13]]);
        let attrs = CellAttrs::new(fg, bg, flags);
        rle.extend_with(attrs, length);
        offset += RUN_SIZE;
    }
    Some(rle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_scrollback::{CellAttrs, Scrollback};

    // -----------------------------------------------------------------------
    // Serialization round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn line_roundtrip_plain_text() {
        let line = Line::from("Hello, scrollback world!");
        let serialized = serialize_line(&line);
        let deserialized = deserialize_line(&serialized).expect("deserialize");
        assert_eq!(deserialized.to_string(), "Hello, scrollback world!");
        assert!(!deserialized.is_wrapped());
    }

    #[test]
    fn line_roundtrip_wrapped() {
        let mut line = Line::from("wrapped line");
        line.set_wrapped(true);
        let serialized = serialize_line(&line);
        let deserialized = deserialize_line(&serialized).expect("deserialize");
        assert_eq!(deserialized.to_string(), "wrapped line");
        assert!(deserialized.is_wrapped());
    }

    #[test]
    fn line_roundtrip_with_attrs() {
        let mut rle = aterm_rle::Rle::new();
        // Bold red on default bg
        let attrs = CellAttrs::new(0xFF_00_00_01, 0x00_00_00_00, 0x0001);
        for _ in 0..5 {
            rle.push(attrs);
        }
        let line = Line::with_hyperlinks("Hello", rle, Vec::new());
        let serialized = serialize_line(&line);
        let deserialized = deserialize_line(&serialized).expect("deserialize");
        assert_eq!(deserialized.to_string(), "Hello");

        // Verify attrs survived the round-trip.
        let orig_rle = line.attrs().expect("original has attrs");
        let deser_rle = deserialized.attrs().expect("deserialized has attrs");
        let orig_runs = orig_rle.runs();
        let deser_runs = deser_rle.runs();
        assert_eq!(orig_runs.len(), deser_runs.len());
        for (orig, deser) in orig_runs.iter().zip(deser_runs.iter()) {
            assert_eq!(orig.length, deser.length, "run lengths differ");
            assert_eq!(orig.value.fg, deser.value.fg, "fg differs");
            assert_eq!(orig.value.bg, deser.value.bg, "bg differs");
            assert_eq!(orig.value.flags, deser.value.flags, "flags differ");
        }
    }

    #[test]
    fn deserialize_truncated_data_returns_none() {
        assert!(deserialize_line(&[]).is_none());
        assert!(deserialize_line(&[0; 5]).is_none());
    }

    // -----------------------------------------------------------------------
    // Budget enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn budget_enforcement_triggers_spill() {
        // Use a tiny budget (1 MiB) for the enforcer. The scrollback
        // itself gets a large internal budget (1 GiB) so it does NOT
        // self-evict — all memory pressure is handled by our enforcer.
        let budget = ScrollbackBudget::new(MIN_BUDGET_BYTES);
        let mut enforcer = BudgetEnforcer::new(budget);

        // Small hot/warm limits force data into the cold tier quickly.
        // Large internal budget (1 GiB) ensures the scrollback's own
        // eviction never triggers.
        let sb = Scrollback::new(10, 50, 1_000_000_000);
        let mut storage: ScrollbackStorage = sb.into();

        // Push lines with varied content so compression ratio is poor.
        // Each line has a unique number, making it harder to compress.
        for i in 0..20_000 {
            let text = format!(
                "Line-{i:08}-{:x}-{}",
                (i as u64).wrapping_mul(0x9E37_79B9),
                "data".repeat(40)
            );
            storage
                .push_line(Line::from(text.as_str()))
                .expect("push_line");
        }

        let mem_before = storage.total_memory_used();
        assert!(
            mem_before > MIN_BUDGET_BYTES,
            "expected > {MIN_BUDGET_BYTES} bytes, got {mem_before}"
        );

        let evicted = enforcer.enforce(&mut storage).expect("enforce");
        assert!(evicted > 0, "expected some lines to be evicted");
        assert!(
            enforcer.spilled_line_count() > 0,
            "expected spilled lines > 0"
        );
        assert!(enforcer.spilled_bytes() > 0, "expected spilled bytes > 0");

        // Memory should have decreased.
        let mem_after = storage.total_memory_used();
        assert!(
            mem_after < mem_before,
            "expected memory to decrease: before={mem_before}, after={mem_after}"
        );
    }

    #[test]
    fn read_back_spilled_lines() {
        // Use a small enforcer budget, large scrollback internal budget.
        let budget = ScrollbackBudget::new(MIN_BUDGET_BYTES);
        let mut enforcer = BudgetEnforcer::new(budget);

        let sb = Scrollback::new(10, 50, 1_000_000_000);
        let mut storage: ScrollbackStorage = sb.into();

        // Push numbered lines with varied content for poor compression.
        for i in 0..20_000 {
            let text = format!(
                "Line {i:06} {:x}-{}",
                (i as u64).wrapping_mul(0x9E37_79B9),
                "data".repeat(40)
            );
            storage
                .push_line(Line::from(text.as_str()))
                .expect("push_line");
        }

        let evicted = enforcer.enforce(&mut storage).expect("enforce");
        assert!(evicted > 0, "expected evictions, got 0");

        // Read back the first spilled line (oldest).
        let first = enforcer.get_spilled_line(0).expect("get_spilled_line(0)");
        assert!(
            first.to_string().starts_with("Line 000"),
            "expected oldest spilled line to start with 'Line 000', got: {}",
            &first.to_string()[..20.min(first.to_string().len())]
        );

        // Read back the last spilled line.
        let last_idx = enforcer.spilled_line_count() - 1;
        let last = enforcer
            .get_spilled_line(last_idx)
            .expect("get_spilled_line(last)");
        assert!(
            !last.to_string().is_empty(),
            "last spilled line should not be empty"
        );
    }

    #[test]
    fn no_spill_when_within_budget() {
        // Use a huge budget that won't be exceeded.
        let budget = ScrollbackBudget::new(512 * 1024 * 1024);
        let mut enforcer = BudgetEnforcer::new(budget);

        let sb = Scrollback::new(100, 500, 100_000_000);
        let mut storage: ScrollbackStorage = sb.into();

        for i in 0..100 {
            storage
                .push_line(Line::from(format!("Line {i}").as_str()))
                .expect("push_line");
        }

        let evicted = enforcer.enforce(&mut storage).expect("enforce");
        assert_eq!(evicted, 0);
        assert_eq!(enforcer.spilled_line_count(), 0);
        assert!(enforcer.spill.is_none(), "no spill file should be created");
    }

    #[test]
    fn memory_stats_reports_correctly() {
        let budget = ScrollbackBudget::new(MIN_BUDGET_BYTES);
        let enforcer = BudgetEnforcer::new(budget);

        let sb = Scrollback::new(100, 500, MIN_BUDGET_BYTES);
        let storage: ScrollbackStorage = sb.into();

        let stats = enforcer.memory_stats(Some(&storage));
        assert_eq!(stats.budget_max_bytes, MIN_BUDGET_BYTES);
        assert_eq!(stats.spilled_line_count, 0);
        assert_eq!(stats.spilled_disk_bytes, 0);
        assert!(stats.within_budget());
    }

    #[test]
    fn budget_clamps_to_minimum() {
        let budget = ScrollbackBudget::new(100); // below MIN_BUDGET_BYTES
        assert_eq!(budget.max_bytes(), MIN_BUDGET_BYTES);
    }

    #[test]
    fn spill_file_cleaned_up_on_drop() {
        let budget = ScrollbackBudget::new(MIN_BUDGET_BYTES);
        let mut enforcer = BudgetEnforcer::new(budget);

        let sb = Scrollback::new(10, 50, 1_000_000_000);
        let mut storage: ScrollbackStorage = sb.into();

        for i in 0..20_000 {
            let text = format!(
                "drop-test-{i:08}-{:x}-{}",
                (i as u64).wrapping_mul(0x9E37_79B9),
                "data".repeat(40)
            );
            storage
                .push_line(Line::from(text.as_str()))
                .expect("push_line");
        }

        let _ = enforcer.enforce(&mut storage);

        // Capture the path before drop.
        let spill_path = enforcer
            .spill
            .as_ref()
            .map(|s| s._file.path().to_path_buf());

        // Verify file exists.
        if let Some(path) = &spill_path {
            assert!(path.exists(), "spill file should exist before drop");
        }

        // Drop the enforcer.
        drop(enforcer);

        // NamedTempFile deletes on drop.
        if let Some(path) = &spill_path {
            assert!(!path.exists(), "spill file should be cleaned up after drop");
        }
    }

    #[test]
    fn budget_utilization_calculation() {
        let stats = ScrollbackMemoryStats {
            in_memory_bytes: 128 * 1024 * 1024,
            budget_max_bytes: 256 * 1024 * 1024,
            spilled_line_count: 0,
            spilled_disk_bytes: 0,
            spill_index_bytes: 0,
        };
        let utilization = stats.budget_utilization();
        assert!(
            (utilization - 0.5).abs() < f64::EPSILON,
            "expected 50% utilization, got {utilization}"
        );
        assert!(stats.within_budget());
    }

    #[test]
    fn mmap_grow_works_correctly() {
        let budget = ScrollbackBudget::new(MIN_BUDGET_BYTES);
        let mut enforcer = BudgetEnforcer::new(budget);

        // Force creation of spill state.
        let spill = enforcer.ensure_spill_state().expect("ensure_spill_state");
        let initial_capacity = spill.capacity;
        assert_eq!(initial_capacity, INITIAL_SPILL_CAPACITY as usize);

        // Grow beyond initial capacity.
        let target = initial_capacity * 3;
        spill.grow(target).expect("grow");
        assert!(
            spill.capacity >= target,
            "capacity should be >= target: {} >= {}",
            spill.capacity,
            target
        );
        assert!(spill.mmap.len() >= target, "mmap len should be >= target");
    }
}
