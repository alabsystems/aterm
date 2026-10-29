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
    #[must_use]
    #[cfg(test)]
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
mod eviction_tests {
    use super::BlockText;
    use crate::terminal::{Terminal, TerminalBuilder};

    /// Drive one OSC 133 command block (prompt → command → output → done),
    /// with `cmd` typed and `output_lines` printed, then return.
    fn run_block(term: &mut Terminal, cmd: &str, output_lines: &[&str]) {
        term.process(b"\x1b]133;A\x07");
        term.process(format!("$ {cmd}").as_bytes());
        term.process(b"\x1b]133;B\x07");
        term.process(b"\r\n");
        term.process(b"\x1b]133;C\x07");
        for line in output_lines {
            term.process(line.as_bytes());
            term.process(b"\r\n");
        }
        term.process(b"\x1b]133;D;0\x07");
    }

    /// With a small scrollback cap, an old block whose rows scrolled away
    /// reports evicted, while a still-retained block returns its true output.
    #[test]
    fn evicted_block_returns_evicted_marker_retained_block_returns_true_output() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(6)
            .build();

        run_block(&mut term, "first", &["EVICT-ME-0", "EVICT-ME-1"]);
        run_block(&mut term, "second", &["KEEP-ME-0", "KEEP-ME-1"]);
        // Next prompt seals `second`'s `end_row` before subsequent output scrolls.
        term.process(b"\x1b]133;A\x07");

        let ids: Vec<u64> = term.all_blocks().map(|b| b.id).collect();
        assert!(ids.len() >= 2, "expected at least two blocks, got {ids:?}");
        let (evict_id, keep_id) = (ids[0], ids[1]);

        for i in 0..6 {
            term.process(format!("filler-{i}\r\n").as_bytes());
        }

        let evict_block = term
            .block_by_id(evict_id)
            .cloned()
            .expect("evicted block id still tracked");
        let keep_block = term
            .block_by_id(keep_id)
            .cloned()
            .expect("retained block id still tracked");

        let evicted = term.block_output_text(&evict_block);
        assert_eq!(
            evicted,
            BlockText::Evicted,
            "block whose rows scrolled past the cap must report Evicted, got {evicted:?}",
        );
        assert_eq!(term.block_output(&evict_block), None);

        assert!(
            term.block_eviction_read_count() >= 1,
            "eviction read must bump the session counter",
        );

        let bogus_id = ids.iter().copied().max().unwrap() + 1_000;
        assert!(
            term.block_by_id(bogus_id).is_none(),
            "a never-existed block id has no block (distinct from Evicted)",
        );

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
            BlockText::Evicted => panic!("retained block unexpectedly Evicted"),
            BlockText::NotAvailable => panic!("retained block unexpectedly NotAvailable"),
        }
    }

    /// The `aterm_log` records (level and message) written on this thread while
    /// `body` runs.
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

    /// A block read again and again after its eviction logs once per side, as the
    /// status observer re-reads a running command with no OSC 633;E up to 4 times a
    /// second; every read is still counted.
    #[test]
    fn a_block_read_again_and_again_after_its_eviction_is_logged_once() {
        let mut term = TerminalBuilder::new()
            .size(4, 40)
            .ring_buffer_size(6)
            .build();
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
                assert_eq!(term.block_command_text(&running), BlockText::Evicted);
                assert_eq!(term.block_command(&running), None);
            }
            assert_eq!(term.block_command_text(&older), BlockText::Evicted);
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
        assert_eq!(
            term.block_eviction_read_count(),
            151,
            "every evicted read is still counted"
        );
    }

    /// A program starting block after block (no nonce guards OSC 133) and scrolling
    /// each command out logs evictions by doubling: 64 blocks write 7 lines.
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
    }

    /// A block read before any eviction returns its output and counts no eviction.
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

    /// `last_completed_block` returns only archived (row-sealed) blocks, never a
    /// Complete current one, whose open `end_row` would truncate the read.
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

        term.process(b"\x1b]133;A\x07$ ls");
        term.process(b"\x1b]133;B\x07\r\n");
        term.process(b"\x1b]133;C\x07OUT-1\r\nOUT-2\r\n\x1b]133;D;0\x07");
        assert!(
            term.last_completed_block().is_none(),
            "a Complete-but-current block has an open end_row (truncated span)",
        );

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
        term.process(b"ABCDEFGHIJKLMNOPQRSTUVWXY");
        match term.text_range(0, 3) {
            BlockText::Text(t) => assert!(
                t.contains("ABCDEFGHIJKLMNOPQRSTUVWXY"),
                "soft-wrapped rows must be joined, not chopped at cols 10/20: {t:?}"
            ),
            other => panic!("expected text, got {other:?}"),
        }
        let mut t2 = TerminalBuilder::new().size(6, 10).build();
        t2.process(b"AAA\r\nBBB");
        match t2.text_range(0, 2) {
            BlockText::Text(t) => assert!(t.contains("AAA\nBBB"), "hard newline preserved: {t:?}"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// A wrapped output line reads as the program printed it, on screen and from
    /// history: a space the program printed in the last column of a wrapped row
    /// is text that `blocktext` keeps at the join, while a line that ends in
    /// blanks loses them, across a wrap too (a padded column wider than the
    /// window reads as its text).
    #[test]
    fn block_output_reads_a_wrapped_line_as_printed() {
        for (lines, want) in [
            (["abcdefghi jklmn", "pad   "], "abcdefghi jklmn\npad"),
            (["foo            ", "bar"], "foo\nbar"),
        ] {
            let mut term = TerminalBuilder::new()
                .size(4, 10)
                .ring_buffer_size(100)
                .build();
            run_block(&mut term, "ls", &lines);
            term.process(b"\x1b]133;A\x07");
            let block = term.last_completed_block().cloned().expect("sealed block");
            let want = BlockText::Text(want.to_string());
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
}
