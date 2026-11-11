// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Block-based output model API for [`Terminal`]: OSC 133 command+output blocks for navigation, per-block copy and agent context ([`Terminal::all_blocks`], [`Terminal::current_block`], [`Terminal::block_output`]).

use super::{OutputBlock, Terminal};

/// Result of reading a block's command or output text from the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockText {
    /// The text was read from retained history + the visible screen: in full, but for a block whose command was still running when a full clear or a scrollback erase took some of its rows.
    Text(String),
    /// The block's rows are older than the oldest retained line, or a full clear blanked them after its command finished (its command rows: a full clear or an erase while it was being built): its content is gone.
    Evicted,
    /// The block has not reached the requested phase yet (no command entered, or no output produced), so there is no row range to read.
    NotAvailable,
}

/// Which rows of a block a read asked for: the eviction log keeps one
/// newest-seen block per side (`ShellIntegrationState::eviction_seen`,
/// indexed in this order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockSide {
    /// The command rows (prompt end to output start).
    Command = 0,
    /// The output rows.
    Output = 1,
}

impl BlockSide {
    /// The word the log line names the side by.
    #[inline]
    const fn word(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Output => "output",
        }
    }
}

impl BlockText {
    /// True iff the block's content was evicted from scrollback.
    #[must_use]
    #[inline]
    pub const fn is_evicted(&self) -> bool {
        matches!(self, BlockText::Evicted)
    }

    /// Consume into the owned `String` if fully readable.
    #[must_use]
    #[inline]
    pub fn into_text(self) -> Option<String> {
        match self {
            BlockText::Text(s) => Some(s),
            BlockText::Evicted | BlockText::NotAvailable => None,
        }
    }
}

impl Terminal {
    /// The block shell integration is building, in any state from `PromptOnly` to `Complete`; a `Complete` one stays current until the next prompt (OSC 133 A) archives it.
    #[must_use]
    #[inline]
    pub const fn current_block(&self) -> Option<&OutputBlock> {
        self.shell.current_block.as_ref()
    }

    /// All blocks, completed then current.
    #[inline]
    pub fn all_blocks(&self) -> impl DoubleEndedIterator<Item = &OutputBlock> {
        self.shell
            .output_blocks
            .iter()
            .chain(self.shell.current_block.as_ref())
    }

    /// The id the next block will take: one past every block id this engine has issued.
    #[must_use]
    #[inline]
    pub const fn next_block_id(&self) -> u64 {
        self.shell.next_block_id
    }

    /// The block with this ID (unique per session, assigned sequentially).
    #[must_use]
    #[inline]
    pub fn block_by_id(&self, id: u64) -> Option<&OutputBlock> {
        self.shell
            .output_blocks
            .iter()
            .find(|b| b.id == id)
            .or_else(|| self.shell.current_block.as_ref().filter(|b| b.id == id))
    }

    /// The newest archived block, whose row extent the next prompt's OSC 133 A sealed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            reason = "stateless read over the block ledger (no transition); block \
                      lifecycle state itself is exercised by the blocks_api eviction \
                      conformance tests, not a ty model"
        )
    )]
    #[must_use]
    #[inline]
    pub fn last_completed_block(&self) -> Option<&OutputBlock> {
        self.shell.output_blocks.back()
    }

    /// The newest block in state [`Complete`](crate::terminal::BlockState::Complete), current block included: an O(1) "last `Complete` in `all_blocks()`" for the `wait` verb, which asks on every output burst under the terminal lock.
    #[must_use]
    #[inline]
    pub fn newest_completed_block(&self) -> Option<&OutputBlock> {
        if let Some(block) = &self.shell.current_block
            && block.is_complete()
        {
            return Some(block);
        }
        self.shell
            .output_blocks
            .iter()
            .rev()
            .find(|b| b.is_complete())
    }

    /// The block the latest accepted OSC 133/633 D completed, as it stood at that D: its id, exit code and stamps (its rows are not kept up to date).
    #[must_use]
    #[inline]
    pub const fn last_finished_block(&self) -> Option<&OutputBlock> {
        self.shell.last_finished.as_ref()
    }

    /// Text of the absolute rows `start_row, end_row)`, joined with newlines.
    pub(crate) fn text_range(&self, start_row: u64, end_row: u64) -> BlockText {
        let oldest = self.grid.oldest_absolute_row();
        let scrollback_lines = self.grid.scrollback_lines() as u64;
        let visible_rows = u64::from(self.grid.rows());

        if end_row > start_row && start_row < oldest {
            return BlockText::Evicted;
        }

        let mut result = String::new();
        let mut line_start = 0usize;
        for row in start_row..end_row {
            let rel = row.saturating_sub(oldest);
            let (is_continuation, text) = if rel < scrollback_lines {
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
            if row > start_row && !is_continuation {
                result.truncate(line_start + result[line_start..].trim_end().len());
                result.push('\n');
                line_start = result.len();
            }
            result.push_str(&text);
        }
        result.truncate(line_start + result[line_start..].trim_end().len());
        BlockText::Text(result)
    }

    /// Number of block command/output reads that found the block's rows already evicted from scrollback, over this session's lifetime.
    #[inline]
    #[must_use]
    pub fn block_eviction_read_count(&self) -> u64 {
        self.shell.eviction_reads.get()
    }

    /// Record that a block read hit eviction: every read bumps the session counter;
    /// the first read to find a newer block's `side` evicted is news, and the 1st,
    /// 2nd, 4th … piece of news is logged.
    fn record_block_eviction(&self, block_id: u64, side: BlockSide, start: u64, end: u64) {
        let count = self.shell.eviction_reads.get().saturating_add(1);
        self.shell.eviction_reads.set(count);
        let seen = &self.shell.eviction_seen[side as usize];
        if seen.get().is_some_and(|newest| block_id <= newest) {
            return;
        }
        seen.set(Some(block_id));
        let news = self.shell.eviction_news.get().saturating_add(1);
        self.shell.eviction_news.set(news);
        if !news.is_power_of_two() {
            return;
        }
        aterm_log::warn!(
            "block {block_id} {} EVICTED: rows [{start}, {end}) are older than oldest \
             retained row {}; session eviction reads = {count}, evicted blocks seen = {news}",
            side.word(),
            self.grid.oldest_absolute_row(),
        );
    }

    /// The command text from a block: OSC 633;E's explicit commandline when present, else the text of the command rows.
    #[must_use]
    pub fn block_command_text(&self, block: &OutputBlock) -> BlockText {
        // OSC 633;E's commandline is clean of prompt decorations and, stored on the
        // block, survives eviction.
        if let Some(ref cmd) = block.commandline {
            let trimmed = cmd.trim();
            if !trimmed.is_empty() {
                return BlockText::Text(trimmed.to_string());
            }
        }
        let Some((start, end)) = block.command_rows() else {
            return BlockText::NotAvailable;
        };
        if block.text_cleared || block.command_text_cleared {
            return BlockText::Evicted;
        }
        match self.text_range(start, end) {
            BlockText::Text(mut text) => {
                let trimmed_len = text.trim_end().len();
                text.truncate(trimmed_len);
                BlockText::Text(text)
            }
            BlockText::Evicted => {
                self.record_block_eviction(block.id, BlockSide::Command, start, end);
                BlockText::Evicted
            }
            BlockText::NotAvailable => BlockText::NotAvailable,
        }
    }

    /// The output text from a block.
    #[must_use]
    pub fn block_output_text(&self, block: &OutputBlock) -> BlockText {
        let Some((start, end)) = block.output_rows() else {
            return BlockText::NotAvailable;
        };
        if block.text_cleared {
            return BlockText::Evicted;
        }
        let result = self.text_range(start, end);
        if result.is_evicted() {
            self.record_block_eviction(block.id, BlockSide::Output, start, end);
        }
        result
    }

    /// [`block_command_text`](Self::block_command_text) as an `Option`: `None` when not available or evicted (an eviction is still logged and counted).
    #[must_use]
    #[inline]
    pub fn block_command(&self, block: &OutputBlock) -> Option<String> {
        self.block_command_text(block).into_text()
    }

    /// [`block_output_text`](Self::block_output_text) as an `Option`: `None` when not available or evicted (an eviction is still logged and counted).
    #[must_use]
    #[inline]
    pub fn block_output(&self, block: &OutputBlock) -> Option<String> {
        self.block_output_text(block).into_text()
    }
}

#[cfg(test)]
#[path = "blocks_api_eviction_tests.rs"]
mod eviction_tests;
