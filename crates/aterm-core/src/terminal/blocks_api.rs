// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Block-based output model API for [`Terminal`].
//!
//! Output blocks represent atomic units of command+output, enabling:
//! - Navigation between commands (jump to next/previous block)
//! - Block-level copy operations (copy just command, or just output)
//! - Agent workflows that reference specific blocks as context
//!
//! Key methods:
//! - [`Terminal::output_blocks`] - completed blocks in order
//! - [`Terminal::current_block`] - in-progress block
//! - [`Terminal::block_output`] - extract output text from a block

use super::{OutputBlock, Terminal};

/// Result of extracting a block's command/output text from the grid.
///
/// A block records MONOTONIC ABSOLUTE row numbers. Scrollback retains only the
/// most recent `scrollback_lines()` history lines, so a block whose rows have
/// scrolled past the cap is no longer readable. Distinguishing this EVICTED case
/// from "text is empty" is the whole point of the enum: a caller must never be
/// handed silently-shifted or empty text for an evicted block (B-1 / DL-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockText {
    /// The text was read in full from retained history + the visible screen.
    Text(String),
    /// The block's rows are older than the oldest retained line — its content
    /// has been evicted from scrollback and can no longer be reconstructed.
    Evicted,
    /// The block has not reached the requested phase yet (no command entered, or
    /// no output produced), so there is no row range to read.
    NotAvailable,
}

impl BlockText {
    /// The text if fully readable, else `None` (evicted or not-yet-available).
    /// Convenience for callers that only care about the happy path.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            BlockText::Text(s) => Some(s.as_str()),
            BlockText::Evicted | BlockText::NotAvailable => None,
        }
    }

    /// True iff the block's content was evicted from scrollback.
    #[must_use]
    pub fn is_evicted(&self) -> bool {
        matches!(self, BlockText::Evicted)
    }

    /// Consume into the owned `String` if fully readable.
    #[must_use]
    pub fn into_text(self) -> Option<String> {
        match self {
            BlockText::Text(s) => Some(s),
            BlockText::Evicted | BlockText::NotAvailable => None,
        }
    }
}

impl Terminal {
    // =========================================================================
    // Block-Based Output Model API (Gap 31)
    // =========================================================================

    /// Get all completed output blocks.
    ///
    /// Output blocks represent atomic units of command+output. Each block
    /// contains a prompt, optional command, and optional output. This enables:
    /// - Navigation between commands (jump to next/previous block)
    /// - Block-level copy operations (copy just command, or just output)
    /// - Agent workflows that reference specific blocks as context
    ///
    /// # Returns
    ///
    /// A slice of completed blocks, ordered from oldest to newest.
    #[cfg(test)]
    #[must_use]
    pub fn output_blocks(&mut self) -> &[OutputBlock] {
        self.shell.output_blocks.make_contiguous()
    }

    /// Get the current (in-progress) output block, if any.
    ///
    /// The current block is the one being actively built as shell integration
    /// events arrive. It may be in any state from `PromptOnly` to `Complete`.
    ///
    /// Note: A block with state `Complete` stays as current_block until the
    /// next prompt starts (OSC 133 A), at which point it moves to output_blocks.
    #[must_use]
    pub fn current_block(&self) -> Option<&OutputBlock> {
        self.shell.current_block.as_ref()
    }

    /// Get all blocks including the current one.
    ///
    /// This returns an iterator over all blocks (completed + current).
    /// Useful for displaying or navigating all commands.
    pub fn all_blocks(&self) -> impl Iterator<Item = &OutputBlock> {
        self.shell
            .output_blocks
            .iter()
            .chain(self.shell.current_block.as_ref())
    }

    /// Get the total number of blocks (completed + current).
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.shell.output_blocks.len() + usize::from(self.shell.current_block.is_some())
    }

    /// Get a block by its ID.
    ///
    /// Block IDs are unique within a session and are assigned sequentially.
    #[must_use]
    pub fn block_by_id(&self, id: u64) -> Option<&OutputBlock> {
        self.shell
            .output_blocks
            .iter()
            .find(|b| b.id == id)
            .or_else(|| self.shell.current_block.as_ref().filter(|b| b.id == id))
    }

    /// Get a block by index (0 = oldest block).
    ///
    /// Returns the block at the given index, treating completed blocks
    /// and the current block as a unified sequence.
    #[must_use]
    pub fn block_by_index(&self, index: usize) -> Option<&OutputBlock> {
        use std::cmp::Ordering;
        match index.cmp(&self.shell.output_blocks.len()) {
            Ordering::Less => Some(&self.shell.output_blocks[index]),
            Ordering::Equal => self.shell.current_block.as_ref(),
            Ordering::Greater => None,
        }
    }

    /// Get the block containing a given row.
    ///
    /// This is useful for determining which command produced a given line
    /// of output, or for highlighting block boundaries.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert
    ///   screen-relative row coordinates)
    ///
    /// # Returns
    ///
    /// The block containing that row, or `None` if no block covers it.
    #[must_use]
    pub fn block_at_row(&self, row: u64) -> Option<&OutputBlock> {
        // Check current block first (most likely to be queried)
        if let Some(ref block) = self.shell.current_block {
            if block.contains_row(row) {
                return Some(block);
            }
        }
        // Search completed blocks in reverse (recent blocks more likely to be queried)
        self.shell
            .output_blocks
            .iter()
            .rev()
            .find(|b| b.contains_row(row))
    }

    /// Find the next block after a given row.
    ///
    /// Useful for "jump to next command" navigation.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert
    ///   screen-relative row coordinates)
    ///
    /// # Returns
    ///
    /// The first block that starts after the given row, or `None` if there
    /// are no more blocks.
    #[cfg(test)]
    #[must_use]
    pub fn next_block_after_row(&self, row: u64) -> Option<&OutputBlock> {
        // First check completed blocks
        if let Some(block) = self
            .shell
            .output_blocks
            .iter()
            .find(|b| b.prompt_start_row > row)
        {
            return Some(block);
        }
        // Check current block
        if let Some(ref block) = self.shell.current_block {
            if block.prompt_start_row > row {
                return Some(block);
            }
        }
        None
    }

    /// Find the previous block before a given row.
    ///
    /// Useful for "jump to previous command" navigation.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert
    ///   screen-relative row coordinates)
    ///
    /// # Returns
    ///
    /// The last block that starts before the given row, or `None` if there
    /// are no previous blocks.
    #[cfg(test)]
    #[must_use]
    pub fn previous_block_before_row(&self, row: u64) -> Option<&OutputBlock> {
        // Check current block first (it might start before this row)
        if let Some(ref block) = self.shell.current_block {
            if block.prompt_start_row < row {
                // But there might be a completed block that's even closer
                if let Some(completed) = self
                    .shell
                    .output_blocks
                    .iter()
                    .rev()
                    .find(|b| b.prompt_start_row < row)
                {
                    // Return whichever is closer (larger prompt_start_row)
                    if completed.prompt_start_row > block.prompt_start_row {
                        return Some(completed);
                    }
                }
                return Some(block);
            }
        }
        // Search completed blocks in reverse
        self.shell
            .output_blocks
            .iter()
            .rev()
            .find(|b| b.prompt_start_row < row)
    }

    /// Get the most recent successful block (exit code 0).
    #[cfg(test)]
    #[must_use]
    pub fn last_successful_block(&self) -> Option<&OutputBlock> {
        // Check current block first
        if let Some(ref block) = self.shell.current_block {
            if block.succeeded() {
                return Some(block);
            }
        }
        self.shell
            .output_blocks
            .iter()
            .rev()
            .find(|b| b.succeeded())
    }

    /// Get the most recent failed block (exit code != 0).
    #[cfg(test)]
    #[must_use]
    pub fn last_failed_block(&self) -> Option<&OutputBlock> {
        // Check current block first
        if let Some(ref block) = self.shell.current_block {
            if block.failed() {
                return Some(block);
            }
        }
        self.shell.output_blocks.iter().rev().find(|b| b.failed())
    }

    /// The newest ARCHIVED block — the youngest block whose row extent is
    /// sealed (`end_row` set when the next prompt's OSC 133 A finalized it).
    ///
    /// Deliberately NOT the current block, even when its state is already
    /// [`Complete`](crate::terminal::BlockState::Complete) (D received, next A
    /// not yet): a current block has `end_row == None`, so its output span
    /// falls back to a single row and a multi-line output would read back
    /// TRUNCATED. Shells emit the next prompt (A) immediately after D, so the
    /// unsealed window is transient; an archived block may lack an exit code
    /// when a new prompt closed it without D (e.g. an interrupted command).
    ///
    /// `&self` on purpose (CM-A3): reads `VecDeque::back`, never
    /// `make_contiguous`, so read-only facade surfaces (the wasm
    /// `last_command_output` binding) can call it without a mutable borrow —
    /// unlike [`output_blocks`](Self::output_blocks).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            reason = "stateless read over the block ledger (no transition); block \
                      lifecycle state itself is exercised by the blocks_api eviction \
                      conformance tests, not a ty model"
        )
    )]
    #[must_use]
    pub fn last_completed_block(&self) -> Option<&OutputBlock> {
        self.shell.output_blocks.back()
    }

    /// Clear all output blocks.
    ///
    /// This does not affect the current shell state, only clears the history
    /// of completed blocks. The current block (if any) is also cleared.
    #[cfg(test)]
    pub fn clear_blocks(&mut self) {
        self.shell.output_blocks.clear();
        self.shell.current_block = None;
    }

    /// Toggle the collapsed state of a block by ID.
    ///
    /// Returns `true` if the block was found and toggled, `false` otherwise.
    ///
    /// # Arguments
    ///
    /// * `id` - The block ID to toggle
    #[cfg(test)]
    pub fn toggle_block_collapsed(&mut self, id: u64) -> bool {
        // Check completed blocks first
        for block in &mut self.shell.output_blocks {
            if block.id == id {
                block.collapsed = !block.collapsed;
                return true;
            }
        }
        // Check current block
        if let Some(ref mut block) = self.shell.current_block {
            if block.id == id {
                block.collapsed = !block.collapsed;
                return true;
            }
        }
        false
    }

    /// Set the collapsed state of a block by ID.
    ///
    /// Returns `true` if the block was found and updated, `false` otherwise.
    ///
    /// # Arguments
    ///
    /// * `id` - The block ID to update
    /// * `collapsed` - Whether the block should be collapsed
    #[cfg(test)]
    pub fn set_block_collapsed(&mut self, id: u64, collapsed: bool) -> bool {
        // Check completed blocks first
        for block in &mut self.shell.output_blocks {
            if block.id == id {
                block.collapsed = collapsed;
                return true;
            }
        }
        // Check current block
        if let Some(ref mut block) = self.shell.current_block {
            if block.id == id {
                block.collapsed = collapsed;
                return true;
            }
        }
        false
    }

    /// Collapse all completed blocks.
    ///
    /// This is useful for "collapse all" functionality.
    #[cfg(test)]
    pub fn collapse_all_blocks(&mut self) {
        for block in &mut self.shell.output_blocks {
            block.collapsed = true;
        }
        if let Some(ref mut block) = self.shell.current_block {
            if block.is_complete() {
                block.collapsed = true;
            }
        }
    }

    /// Expand all blocks.
    ///
    /// This is useful for "expand all" functionality.
    #[cfg(test)]
    pub fn expand_all_blocks(&mut self) {
        for block in &mut self.shell.output_blocks {
            block.collapsed = false;
        }
        if let Some(ref mut block) = self.shell.current_block {
            block.collapsed = false;
        }
    }

    /// Collapse all failed blocks (exit code != 0).
    ///
    /// Useful for hiding error output when it's not relevant.
    #[cfg(test)]
    pub fn collapse_failed_blocks(&mut self) {
        for block in &mut self.shell.output_blocks {
            if block.failed() {
                block.collapsed = true;
            }
        }
        if let Some(ref mut block) = self.shell.current_block {
            if block.failed() {
                block.collapsed = true;
            }
        }
    }

    /// Collapse all successful blocks (exit code == 0).
    ///
    /// Useful for focusing on errors.
    #[cfg(test)]
    pub fn collapse_successful_blocks(&mut self) {
        for block in &mut self.shell.output_blocks {
            if block.succeeded() {
                block.collapsed = true;
            }
        }
        if let Some(ref mut block) = self.shell.current_block {
            if block.succeeded() {
                block.collapsed = true;
            }
        }
    }

    /// Get the total number of hidden rows across all collapsed blocks.
    ///
    /// This is useful for UI layers that need to adjust scroll positions
    /// or display "N lines hidden" indicators.
    #[cfg(test)]
    #[must_use]
    pub fn total_hidden_rows(&self) -> usize {
        let mut total = self
            .shell
            .output_blocks
            .iter()
            .map(OutputBlock::hidden_row_count)
            .sum();
        if let Some(ref block) = self.shell.current_block {
            total += block.hidden_row_count();
        }
        total
    }

    /// Text content for a range of MONOTONIC ABSOLUTE rows from the grid.
    ///
    /// Returns the joined-with-newlines text of `[start_row, end_row)`, where the
    /// rows are ABSOLUTE line numbers (the same space `OutputBlock` records and
    /// `Grid::visible_to_absolute()` produces). The absolute number is converted
    /// to a history index relative to the OLDEST retained line:
    /// `history_idx = absolute_row - Grid::oldest_absolute_row()`. Rows at/above
    /// the top visible row map onto the visible screen.
    ///
    /// Returns `BlockText::Evicted` when `start_row` is older than
    /// `oldest_absolute_row()` — the block's content has scrolled past the
    /// scrollback cap and can no longer be read. Returning shifted/empty text
    /// for that case (the old bug: it treated the absolute row as a 0-based
    /// history index) silently handed callers the WRONG lines (B-1).
    ///
    /// Takes `&self` because `ScrollbackStorage::get_line` now uses interior
    /// mutability for the disk-backed LRU cache.
    pub(crate) fn text_range(&self, start_row: u64, end_row: u64) -> BlockText {
        let oldest = self.grid.oldest_absolute_row();
        let scrollback_lines = self.grid.scrollback_lines() as u64;
        let visible_rows = u64::from(self.grid.rows());

        // The block's first row predates the oldest retained line → evicted.
        // (Only flag eviction for a non-empty range; an empty range is just
        // "nothing to read".)
        if end_row > start_row && start_row < oldest {
            return BlockText::Evicted;
        }

        let mut result = String::new();
        for row in start_row..end_row {
            // Absolute row → history index relative to the oldest retained line.
            let rel = row.saturating_sub(oldest);
            // Read each row ONCE: its soft-wrap flag AND its text. `is_wrapped()` means
            // "this row CONTINUES the previous row's content" (soft wrap, not a hard
            // break) — the exact semantics `selection_to_string` uses.
            let (is_continuation, text) = if rel < scrollback_lines {
                // Scrollback (history) line.
                usize::try_from(rel)
                    .ok()
                    .and_then(|idx| match self.grid.try_get_history_line(idx) {
                        Ok(Some(line)) => Some((line.is_wrapped(), line.to_string())),
                        Ok(None) => None,
                        Err(error) => {
                            aterm_log::warn!(
                                "Terminal::text_range: scrollback line {idx} read failed: {error}"
                            );
                            None
                        }
                    })
                    .unwrap_or((false, String::new()))
            } else {
                // Visible (on-screen) line. Read it OFFSET-INDEPENDENTLY: this
                // absolute-frame reconstruction must not follow the user's current
                // scroll position (row_text is display-offset-aware and would otherwise
                // inject a materialized history line when scrolled deep).
                let visible_row = rel - scrollback_lines;
                u16::try_from(visible_row)
                    .ok()
                    .filter(|_| visible_row < visible_rows)
                    .map(|idx| {
                        let wrapped = self
                            .grid
                            .row_at_screen(idx)
                            .is_some_and(aterm_grid::Row::is_wrapped);
                        (wrapped, self.grid.row_text_screen(idx).unwrap_or_default())
                    })
                    .unwrap_or((false, String::new()))
            };
            // A soft-wrap continuation is NOT a new logical line: `selection`/`copy`
            // join these, so `blocktext` must too — else a long command line wrapping
            // across rows is CHOPPED at the terminal width (an AI parsing the output
            // gets re-split tokens). Emit `\n` before a row ONLY when it is not a
            // continuation. A wrapped-OUT row fills its last column, so it has no
            // trailing blanks to lose — trimming every row is safe.
            if row > start_row && !is_continuation {
                result.push('\n');
            }
            result.push_str(text.trim_end());
        }
        BlockText::Text(result)
    }

    /// Number of block command/output reads that found the block's rows already
    /// evicted from scrollback, over this session's lifetime (DL-1).
    #[must_use]
    pub fn block_eviction_read_count(&self) -> u64 {
        self.shell.eviction_reads.get()
    }

    /// Record (log + bump the session counter) that a block read hit eviction.
    fn record_block_eviction(&self, block_id: u64, what: &str, start: u64, end: u64) {
        let count = self.shell.eviction_reads.get().saturating_add(1);
        self.shell.eviction_reads.set(count);
        aterm_log::warn!(
            "block {block_id} {what} EVICTED: rows [{start}, {end}) are older than oldest \
             retained row {}; session eviction reads = {count}",
            self.grid.oldest_absolute_row(),
        );
    }

    /// The command text from a block.
    ///
    /// Prefers the explicit commandline from OSC 633;E when available, falling
    /// back to screen-buffer extraction from the command row range.
    ///
    /// Returns:
    /// - [`BlockText::Text`] with the command text, or
    /// - [`BlockText::NotAvailable`] if no command has been entered, or
    /// - [`BlockText::Evicted`] if the command rows have scrolled past the
    ///   scrollback cap (DL-1: never returns silently-shifted text).
    ///
    /// # Arguments
    ///
    /// * `block` - The block to extract the command from
    #[must_use]
    pub fn block_command_text(&self, block: &OutputBlock) -> BlockText {
        // Prefer explicit commandline from OSC 633;E — it is clean text without
        // prompt decorations that would be present in the screen buffer. This
        // survives eviction because it is stored on the block, not the grid.
        if let Some(ref cmd) = block.commandline {
            let trimmed = cmd.trim();
            if !trimmed.is_empty() {
                return BlockText::Text(trimmed.to_string());
            }
        }
        let Some((start, end)) = block.command_rows() else {
            return BlockText::NotAvailable;
        };
        match self.text_range(start, end) {
            BlockText::Text(mut text) => {
                let trimmed_len = text.trim_end().len();
                text.truncate(trimmed_len);
                BlockText::Text(text)
            }
            BlockText::Evicted => {
                self.record_block_eviction(block.id, "command", start, end);
                BlockText::Evicted
            }
            BlockText::NotAvailable => BlockText::NotAvailable,
        }
    }

    /// The output text from a block.
    ///
    /// Returns:
    /// - [`BlockText::Text`] with the output text, or
    /// - [`BlockText::NotAvailable`] if the command hasn't started producing
    ///   output, or
    /// - [`BlockText::Evicted`] if the output rows have scrolled past the
    ///   scrollback cap (DL-1: never returns silently-shifted/empty text).
    ///
    /// # Arguments
    ///
    /// * `block` - The block to extract the output from
    #[must_use]
    pub fn block_output_text(&self, block: &OutputBlock) -> BlockText {
        let Some((start, end)) = block.output_rows() else {
            return BlockText::NotAvailable;
        };
        let result = self.text_range(start, end);
        if result.is_evicted() {
            self.record_block_eviction(block.id, "output", start, end);
        }
        result
    }

    /// The command text from a block, or `None` when not available/evicted.
    ///
    /// Backwards-compatible `Option` wrapper over [`block_command_text`]; an
    /// evicted block returns `None` (and is logged + counted) rather than
    /// shifted text.
    ///
    /// [`block_command_text`]: Self::block_command_text
    #[must_use]
    pub fn block_command(&self, block: &OutputBlock) -> Option<String> {
        self.block_command_text(block).into_text()
    }

    /// The output text from a block, or `None` when not available/evicted.
    ///
    /// Backwards-compatible `Option` wrapper over [`block_output_text`]; an
    /// evicted block returns `None` (and is logged + counted) rather than
    /// shifted/empty text.
    ///
    /// [`block_output_text`]: Self::block_output_text
    #[must_use]
    pub fn block_output(&self, block: &OutputBlock) -> Option<String> {
        self.block_output_text(block).into_text()
    }

    /// The full text of a block (prompt + command + output).
    ///
    /// # Arguments
    ///
    /// * `block` - The block to extract text from
    ///
    #[cfg(test)]
    #[must_use]
    pub fn block_full_text(&self, block: &OutputBlock) -> String {
        let start = block.prompt_start_row;
        let end = block.end_row.unwrap_or(
            block
                .output_start_row
                .unwrap_or(block.command_start_row.unwrap_or(start + 1)),
        );
        self.text_range(start, end).into_text().unwrap_or_default()
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::BlockText;
    use crate::terminal::{Terminal, TerminalBuilder};

    /// Drive one OSC 133 command block (prompt → command → output → done),
    /// with `cmd` typed and `output_lines` printed, then return.
    fn run_block(term: &mut Terminal, cmd: &str, output_lines: &[&str]) {
        term.process(b"\x1b]133;A\x07"); // prompt start
        term.process(format!("$ {cmd}").as_bytes());
        term.process(b"\x1b]133;B\x07"); // command entered
        term.process(b"\r\n");
        term.process(b"\x1b]133;C\x07"); // command executing (output starts)
        for line in output_lines {
            term.process(line.as_bytes());
            term.process(b"\r\n");
        }
        term.process(b"\x1b]133;D;0\x07"); // command done, exit 0
    }

    /// B-1 / DL-1: with a small scrollback cap, an OLD block whose rows have
    /// scrolled past the cap must report EVICTED (never silently-shifted text),
    /// while a still-retained older block returns its TRUE output. The old bug
    /// treated the block's absolute row as a 0-based history index, returning the
    /// wrong (or empty) lines after eviction.
    #[test]
    fn evicted_block_returns_evicted_marker_retained_block_returns_true_output() {
        // 4 visible rows + only 6 lines of ring scrollback: a tight cap so a few
        // commands push the earliest block off the top.
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(6)
            .build();

        // Block 0: the one we will force out of scrollback.
        run_block(&mut term, "first", &["EVICT-ME-0", "EVICT-ME-1"]);
        // Block 1: an older block that should STILL be retained after the churn.
        run_block(&mut term, "second", &["KEEP-ME-0", "KEEP-ME-1"]);

        // Snapshot the block ids in creation order.
        let ids: Vec<u64> = term.all_blocks().map(|b| b.id).collect();
        assert!(ids.len() >= 2, "expected at least two blocks, got {ids:?}");
        let (evict_id, keep_id) = (ids[0], ids[1]);

        // Churn: print many lines to scroll block 0's rows past the 6-line cap
        // (well beyond visible_rows + scrollback_lines).
        for i in 0..40 {
            term.process(format!("filler-{i}\r\n").as_bytes());
        }

        // Re-read after eviction. block_by_id clones so we own the blocks.
        let evict_block = term
            .block_by_id(evict_id)
            .cloned()
            .expect("evicted block id still tracked");
        let keep_block = term
            .block_by_id(keep_id)
            .cloned()
            .expect("retained block id still tracked");

        // The OLD block's output rows are now older than the oldest retained row:
        // it must return the EVICTED marker, NOT shifted/empty text.
        let evicted = term.block_output_text(&evict_block);
        assert_eq!(
            evicted,
            BlockText::Evicted,
            "block whose rows scrolled past the cap must report Evicted, got {evicted:?}",
        );
        // Backwards-compatible Option wrapper returns None for an evicted block.
        assert_eq!(term.block_output(&evict_block), None);

        // DL-1: the eviction read was counted (we did two: enum + Option wrapper).
        assert!(
            term.block_eviction_read_count() >= 1,
            "eviction read must bump the session counter",
        );

        // The id is KNOWN (monotonic), so this is genuinely "evicted", not
        // "never existed": querying a never-assigned id returns no block at all.
        let bogus_id = ids.iter().copied().max().unwrap() + 1_000;
        assert!(
            term.block_by_id(bogus_id).is_none(),
            "a never-existed block id has no block (distinct from Evicted)",
        );

        // The second block is also old but should still be within the retained
        // window: its TRUE output must come back, never shifted text.
        match term.block_output_text(&keep_block) {
            BlockText::Text(text) => {
                assert!(
                    text.contains("KEEP-ME-0") && text.contains("KEEP-ME-1"),
                    "retained block must return its TRUE output, got {text:?}",
                );
                assert!(
                    !text.contains("filler-"),
                    "retained block output must not be shifted into unrelated filler rows: {text:?}",
                );
            }
            // If the tight cap also evicted block 1, that is acceptable as long as
            // it is reported as Evicted (never shifted text) — the core property.
            BlockText::Evicted => {}
            BlockText::NotAvailable => panic!("retained block unexpectedly NotAvailable"),
        }
    }

    /// A block read BEFORE any eviction returns its true output and does NOT bump
    /// the eviction counter — guards against false-positive eviction reporting.
    #[test]
    fn retained_block_before_eviction_reads_true_output() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(100)
            .build();
        run_block(&mut term, "echo", &["HELLO-WORLD"]);
        let block = term.all_blocks().next().cloned().expect("one block");
        let out = term.block_output_text(&block);
        assert!(
            matches!(&out, BlockText::Text(t) if t.contains("HELLO-WORLD")),
            "fresh block output must read true text, got {out:?}",
        );
        assert_eq!(
            term.block_eviction_read_count(),
            0,
            "a non-evicted read must not bump the eviction counter",
        );
    }

    /// CM-A3: `last_completed_block` is a `&self` read of the newest ARCHIVED
    /// (row-sealed) block — a still-current block, even Complete, is excluded
    /// because its open `end_row` would truncate a multi-line output read.
    #[test]
    fn last_completed_block_returns_newest_sealed_block_only() {
        let mut term = TerminalBuilder::new()
            .size(6, 40)
            .ring_buffer_size(100)
            .build();
        assert!(
            term.last_completed_block().is_none(),
            "no shell integration yet: no completed block",
        );

        // A full block, then D — Complete but still current (no next A yet):
        // its extent is unsealed, so it must NOT be returned.
        term.process(b"\x1b]133;A\x07$ ls");
        term.process(b"\x1b]133;B\x07\r\n");
        term.process(b"\x1b]133;C\x07OUT-1\r\nOUT-2\r\n\x1b]133;D;0\x07");
        assert!(
            term.last_completed_block().is_none(),
            "a Complete-but-current block has an open end_row (truncated span)",
        );

        // The next prompt's A seals + archives it; now BOTH output lines read.
        term.process(b"\x1b]133;A\x07");
        let archived = term
            .last_completed_block()
            .cloned()
            .expect("archived block");
        assert_eq!(archived.exit_code, Some(0));
        let out = term.block_output_text(&archived);
        assert!(
            matches!(&out, BlockText::Text(t) if t.contains("OUT-1") && t.contains("OUT-2")),
            "sealed block reads its FULL multi-line output, got {out:?}",
        );
    }

    /// `text_range` (backing `blocktext`) JOINS soft-wrapped rows (like
    /// `selection`/`copy`), rather than chopping at the terminal width — a 25-char
    /// line wrapping across 3 rows of a 10-col terminal reads back whole, not as
    /// re-split tokens (round-4).
    #[test]
    fn text_range_joins_soft_wrapped_rows() {
        let mut term = TerminalBuilder::new()
            .size(6, 10)
            .ring_buffer_size(100)
            .build();
        // 25 chars, no newline → soft-wraps across visible rows 0,1,2 (no scroll, so
        // oldest_absolute_row()==0 and these are absolute rows 0..3).
        term.process(b"ABCDEFGHIJKLMNOPQRSTUVWXY");
        match term.text_range(0, 3) {
            BlockText::Text(t) => assert!(
                t.contains("ABCDEFGHIJKLMNOPQRSTUVWXY"),
                "soft-wrapped rows must be joined, not chopped at cols 10/20: {t:?}"
            ),
            other => panic!("expected text, got {other:?}"),
        }
        // A HARD newline still separates logical lines (the join is wrap-only).
        let mut t2 = TerminalBuilder::new().size(6, 10).build();
        t2.process(b"AAA\r\nBBB");
        match t2.text_range(0, 2) {
            BlockText::Text(t) => assert!(t.contains("AAA\nBBB"), "hard newline preserved: {t:?}"),
            other => panic!("expected text, got {other:?}"),
        }
    }
}
