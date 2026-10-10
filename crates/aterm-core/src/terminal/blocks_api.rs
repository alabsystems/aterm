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
//! - [`Terminal::all_blocks`] - completed blocks in order, then the current one
//! - [`Terminal::current_block`] - in-progress block
//! - [`Terminal::block_output`] - extract output text from a block

use super::{OutputBlock, Terminal};

/// Result of extracting a block's command/output text from the grid.
///
/// Blocks record monotonic absolute rows, and scrollback keeps only the newest
/// `scrollback_lines()`, so a block can outlive its text. `Evicted` keeps that
/// case distinct from empty text, so a caller never gets shifted lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockText {
    /// The text was read in full from retained history + the visible screen.
    Text(String),
    /// The block's rows are older than the oldest retained line: its content
    /// was evicted from scrollback.
    Evicted,
    /// The block has not reached the requested phase yet (no command entered, or
    /// no output produced), so there is no row range to read.
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
    fn word(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Output => "output",
        }
    }
}

impl BlockText {
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
    // Block-Based Output Model API
    // =========================================================================

    /// The block shell integration is building, in any state from
    /// `PromptOnly` to `Complete`; a `Complete` one stays current until the
    /// next prompt (OSC 133 A) archives it.
    #[must_use]
    pub fn current_block(&self) -> Option<&OutputBlock> {
        self.shell.current_block.as_ref()
    }

    /// All blocks, completed then current.
    ///
    /// Double-ended: ids strictly increase front to back, so a watermarked
    /// consumer (the subscribe `events` digest) walks backwards and stops at
    /// `id <= watermark` instead of scanning the retained history.
    pub fn all_blocks(&self) -> impl DoubleEndedIterator<Item = &OutputBlock> {
        self.shell
            .output_blocks
            .iter()
            .chain(self.shell.current_block.as_ref())
    }

    /// The block with this ID (unique per session, assigned sequentially).
    #[must_use]
    pub fn block_by_id(&self, id: u64) -> Option<&OutputBlock> {
        self.shell
            .output_blocks
            .iter()
            .find(|b| b.id == id)
            .or_else(|| self.shell.current_block.as_ref().filter(|b| b.id == id))
    }

    /// The newest archived block: the youngest whose row extent is sealed
    /// (`end_row` set when the next prompt's OSC 133 A finalized it).
    ///
    /// Never the current block, even when it is already
    /// [`Complete`](crate::terminal::BlockState::Complete): its `end_row` is
    /// `None`, so a multi-line output would read back truncated. Shells send the
    /// next A right after D, so that window is short; an archived block may lack
    /// an exit code when a prompt closed it without D. `&self`: reads
    /// `VecDeque::back`, so read-only facades can call it.
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

    /// The newest block in state [`Complete`](crate::terminal::BlockState::Complete),
    /// current block included: the O(1) form of "the last `Complete` in
    /// `all_blocks()`", which the `wait` verb re-evaluates on every output burst
    /// under the terminal lock. Checks `current_block` first, then walks the
    /// archive backwards.
    ///
    /// Unlike [`last_completed_block`](Self::last_completed_block) it includes
    /// the unsealed current block: it reads only `id`/`exit_code`/`state`, which
    /// are final at D.
    #[must_use]
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

    /// Text of the absolute rows `[start_row, end_row)`, joined with newlines.
    ///
    /// Rows are in the space `OutputBlock` records and
    /// `Grid::visible_to_absolute()` produces; history rows map through
    /// `absolute_row - Grid::oldest_absolute_row()`, and rows at or below the
    /// top visible row map onto the screen. Returns `BlockText::Evicted` when
    /// `start_row` is older than `oldest_absolute_row()`.
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
        // Where the logical line now being read starts in `result`.
        let mut line_start = 0usize;
        for row in start_row..end_row {
            // Absolute row → history index relative to the oldest retained line.
            let rel = row.saturating_sub(oldest);
            // Read each row once: its soft-wrap flag and its text. `is_wrapped()` means
            // "this row continues the previous row's content" (soft wrap, not a hard
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
                // Visible line, read independently of the display offset: this
                // reconstruction must not follow the user's scroll position.
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
            // A soft-wrap continuation is not a new logical line: `selection`/`copy`
            // join these, so `blocktext` must too — else a long command line wrapping
            // across rows is chopped at the terminal width (an AI parsing the output
            // gets re-split tokens). Emit `\n` before a row only when it is not a
            // continuation. Both readers return the row as written, trailing blanks
            // included, and a logical line is trimmed whole where it ends, as
            // `copy` does: a row the next continues keeps its blanks (a space
            // printed in its last column separates two words), and a line that
            // ends in blanks across a wrap loses all of them.
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

    /// Number of block command/output reads that found the block's rows already
    /// evicted from scrollback, over this session's lifetime.
    #[must_use]
    #[cfg(test)]
    pub fn block_eviction_read_count(&self) -> u64 {
        self.shell.eviction_reads.get()
    }

    /// Record that a block read hit eviction: every read bumps the session
    /// counter, the first read to find a newer block's `side` evicted is news,
    /// and the 1st, 2nd, 4th … piece of news is logged.
    ///
    /// Not a line per read: an evicted block never comes back, and the status
    /// observer re-reads the running block's command up to 4 times a second
    /// where no OSC 633;E names it, so a long command's own output made a WARN
    /// line per look, for as long as the command ran. Rows and block ids only
    /// grow, so a block no newer than the newest one seen for `side` is evicted
    /// on that side too: its read is counted, not news. Not a line per block
    /// either: where no nonce guards OSC 133, a program can start a block a
    /// look and scroll each one out, so news is thinned by doubling. Each line
    /// carries both counts, so nothing the thinning skips is lost.
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

    /// The command text from a block: OSC 633;E's explicit commandline when
    /// present, else the text of the command rows.
    ///
    /// Returns [`BlockText::NotAvailable`] if no command was entered and
    /// [`BlockText::Evicted`] if its rows left the scrollback.
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
                self.record_block_eviction(block.id, BlockSide::Command, start, end);
                BlockText::Evicted
            }
            BlockText::NotAvailable => BlockText::NotAvailable,
        }
    }

    /// The output text from a block.
    ///
    /// Returns [`BlockText::NotAvailable`] if the command has not started
    /// output and [`BlockText::Evicted`] if its rows left the scrollback.
    #[must_use]
    pub fn block_output_text(&self, block: &OutputBlock) -> BlockText {
        let Some((start, end)) = block.output_rows() else {
            return BlockText::NotAvailable;
        };
        let result = self.text_range(start, end);
        if result.is_evicted() {
            self.record_block_eviction(block.id, BlockSide::Output, start, end);
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

    /// With a small scrollback cap, an old block whose rows scrolled away
    /// reports evicted, while a still-retained block returns its true output.
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
        // Block 1: an older block that should still be retained after the churn.
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

        // The old block's output rows are now older than the oldest retained row:
        // it must return the evicted marker, not shifted/empty text.
        let evicted = term.block_output_text(&evict_block);
        assert_eq!(
            evicted,
            BlockText::Evicted,
            "block whose rows scrolled past the cap must report Evicted, got {evicted:?}",
        );
        // Backwards-compatible Option wrapper returns None for an evicted block.
        assert_eq!(term.block_output(&evict_block), None);

        // The eviction read was counted (we did two: enum + Option wrapper).
        assert!(
            term.block_eviction_read_count() >= 1,
            "eviction read must bump the session counter",
        );

        // The id is known (monotonic), so this is genuinely "evicted", not
        // "never existed": querying a never-assigned id returns no block at all.
        let bogus_id = ids.iter().copied().max().unwrap() + 1_000;
        assert!(
            term.block_by_id(bogus_id).is_none(),
            "a never-existed block id has no block (distinct from Evicted)",
        );

        // The second block is also old but should still be within the retained
        // window: its true output must come back, never shifted text.
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

    /// The `aterm_log` records written on THIS thread while `body` runs (level and
    /// message). The one logger this test binary installs keeps only the records
    /// of a thread that is capturing, so tests running beside it add nothing.
    fn logged_on_this_thread(body: impl FnOnce()) -> Vec<String> {
        use std::cell::RefCell;
        thread_local! {
            static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
        }
        struct Capture;
        impl aterm_log::Log for Capture {
            fn enabled(&self, _: &aterm_log::Metadata) -> bool {
                true
            }
            fn log(&self, record: &aterm_log::Record<'_>) {
                LINES.with(|lines| {
                    if let Some(lines) = lines.borrow_mut().as_mut() {
                        lines.push(format!("{} {}", record.level(), record.args()));
                    }
                });
            }
            fn flush(&self) {}
        }
        static CAPTURE: Capture = Capture;
        let _ = aterm_log::set_logger(&CAPTURE);
        aterm_log::set_max_level(aterm_log::LevelFilter::Trace);
        LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
        body();
        LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default())
    }

    /// THE LOG FLOOD: a read that finds a block evicted is a fact about the block,
    /// and the block stays evicted, so it is logged once, not once per read. The
    /// status observer reads the RUNNING block's command up to 4 times a second
    /// for a shell that sends no OSC 633;E (any shell aterm does not integrate, or
    /// a program printing its own OSC 133), and a long command's output scrolls
    /// that command out of the history: measured on a headless instance, 79 WARN
    /// lines in 23 s, churning `aterm.log`'s two 4 MiB files in hours and the
    /// audit lines with them. Every read is still counted.
    #[test]
    fn a_block_read_again_and_again_after_its_eviction_is_logged_once() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(6)
            .build();
        // A finished block, then a running one with no 633;E whose output
        // scrolls its own command line out of the history.
        run_block(&mut term, "first", &["out-a", "out-b"]);
        term.process(b"\x1b]133;A\x07$ long-job\x1b]133;B\x07\r\n\x1b]133;C\x07");
        for i in 0..40 {
            term.process(format!("job-{i}\r\n").as_bytes());
        }
        let older = term.all_blocks().next().cloned().expect("the first block");
        let running = term.current_block().cloned().expect("the running block");
        assert!(running.id > older.id);

        let lines = logged_on_this_thread(|| {
            for _ in 0..50 {
                // The observer's read of the running command.
                assert_eq!(term.block_command_text(&running), BlockText::Evicted);
                assert_eq!(term.block_command(&running), None);
            }
            // An older block's eviction is implied by the newer one's.
            assert_eq!(term.block_command_text(&older), BlockText::Evicted);
            // The other side of a block is its own fact: once, too.
            for _ in 0..50 {
                assert_eq!(term.block_output_text(&older), BlockText::Evicted);
            }
        });
        let evicted: Vec<&String> = lines.iter().filter(|l| l.contains("EVICTED")).collect();
        assert_eq!(
            evicted.len(),
            2,
            "one line for the running block's command, one for the older block's \
             output: {evicted:#?}"
        );
        assert!(evicted[0].starts_with("WARN block ") && evicted[0].contains(" command "));
        assert!(evicted[1].contains(" output "));
        assert_eq!(
            term.block_eviction_read_count(),
            151,
            "every evicted read is still counted"
        );
    }

    /// THE LOG FLOOD, minted: where no nonce guards OSC 133 (no 633;E), a
    /// program can start a new block every look and scroll each one's command
    /// out of the history. A line per block was then a line per look again
    /// (measured on a headless instance: 73 lines in 20 s, a block every
    /// 250 ms), so the news is thinned by doubling: 64 blocks write 7 lines,
    /// and each line says how many reads and blocks it stands for.
    #[test]
    fn a_program_starting_block_after_block_logs_their_evictions_by_doubling() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(6)
            .build();
        let lines = logged_on_this_thread(|| {
            for k in 0..64 {
                term.process(
                    format!("\x1b]133;A\x07$ job-{k}\x1b]133;B\x07\r\n\x1b]133;C\x07").as_bytes(),
                );
                for i in 0..20 {
                    term.process(format!("out-{k}-{i}\r\n").as_bytes());
                }
                let running = term.current_block().cloned().expect("the running block");
                for _ in 0..4 {
                    // The status observer's looks at the running command.
                    assert_eq!(term.block_command_text(&running), BlockText::Evicted);
                }
                term.process(b"\x1b]133;D;0\x07");
            }
        });
        assert_eq!(
            term.block_eviction_read_count(),
            256,
            "every read is counted"
        );
        let evicted: Vec<&String> = lines.iter().filter(|l| l.contains("EVICTED")).collect();
        assert_eq!(
            evicted.len(),
            7,
            "the 1st, 2nd, 4th … 64th evicted block, not one line per block or per read: \
             {evicted:#?}"
        );
        assert!(
            evicted
                .iter()
                .all(|l| l.starts_with("WARN block ") && l.contains(" command "))
        );
        for (line, news) in evicted.iter().zip([1u64, 2, 4, 8, 16, 32, 64]) {
            let reads = 4 * (news - 1) + 1;
            assert!(
                line.ends_with(&format!(
                    "session eviction reads = {reads}, evicted blocks seen = {news}"
                )),
                "each line carries the counts it stands for: {line}"
            );
        }
    }

    /// A block read before any eviction returns its true output and does not bump
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

    /// `last_completed_block` is a `&self` read of the newest archived
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
        // its extent is unsealed, so it must not be returned.
        term.process(b"\x1b]133;A\x07$ ls");
        term.process(b"\x1b]133;B\x07\r\n");
        term.process(b"\x1b]133;C\x07OUT-1\r\nOUT-2\r\n\x1b]133;D;0\x07");
        assert!(
            term.last_completed_block().is_none(),
            "a Complete-but-current block has an open end_row (truncated span)",
        );

        // The next prompt's A seals + archives it; now both output lines read.
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

    /// `text_range` joins soft-wrapped rows (like `selection`/`copy`): a 25-char
    /// line wrapped across 3 rows of a 10-col terminal reads back whole.
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
        // A hard newline still separates logical lines (the join is wrap-only).
        let mut t2 = TerminalBuilder::new().size(6, 10).build();
        t2.process(b"AAA\r\nBBB");
        match t2.text_range(0, 2) {
            BlockText::Text(t) => assert!(t.contains("AAA\nBBB"), "hard newline preserved: {t:?}"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A space the program printed in the last column of a wrapped row is text:
    /// `blocktext` keeps it at the join, on screen and from history, while a
    /// row that ends its line still loses its trailing blanks.
    #[test]
    fn block_output_keeps_a_space_printed_in_a_wrapped_rows_last_column() {
        let mut term = TerminalBuilder::new()
            .size(4, 10)
            .ring_buffer_size(100)
            .build();
        // `abcdefghi ` fills row 0 (the space lands in the last column) and
        // `jklmn` soft-wraps onto the next row; `pad   ` ends a line in blanks.
        run_block(&mut term, "ls", &["abcdefghi jklmn", "pad   "]);
        term.process(b"\x1b]133;A\x07");
        let block = term.last_completed_block().cloned().expect("sealed block");
        let want = BlockText::Text("abcdefghi jklmn\npad".to_string());
        assert_eq!(term.block_output_text(&block), want, "on screen");

        for i in 0..6 {
            term.process(format!("more {i}\r\n").as_bytes());
        }
        assert_eq!(
            term.block_output_text(&block),
            want,
            "the same block read from history"
        );
    }

    /// An output line that ends in blanks across a wrap is trimmed whole, as it
    /// was before wrapped rows kept their blanks: a padded column wider than
    /// the window reads as its text.
    #[test]
    fn block_output_trims_a_line_that_ends_in_blanks_across_a_wrap() {
        let mut term = TerminalBuilder::new()
            .size(4, 10)
            .ring_buffer_size(100)
            .build();
        run_block(&mut term, "ls", &["foo            ", "bar"]);
        term.process(b"\x1b]133;A\x07");
        let block = term.last_completed_block().cloned().expect("sealed block");
        let want = BlockText::Text("foo\nbar".to_string());
        assert_eq!(term.block_output_text(&block), want, "on screen");

        for i in 0..6 {
            term.process(format!("more {i}\r\n").as_bytes());
        }
        assert_eq!(
            term.block_output_text(&block),
            want,
            "the same block read from history"
        );
    }
}
