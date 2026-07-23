// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Adapter implementations for `aterm_types::terminal_host` traits.

use aterm_types::TerminalSize;
use aterm_types::terminal_host::{
    TerminalBlockAccess, TerminalBlockSnapshot, TerminalBlockState, TerminalHost,
};

use super::{BlockState, Terminal};

impl TerminalHost for Terminal {
    fn process(&mut self, input: &[u8]) {
        Terminal::process(self, input);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        Terminal::resize(self, rows, cols);
    }

    fn size(&self) -> TerminalSize {
        TerminalSize::new(Terminal::grid(self).rows(), Terminal::grid(self).cols())
    }

    fn visible_content(&self) -> String {
        Terminal::visible_content(self)
    }

    fn current_working_directory(&self) -> Option<String> {
        Terminal::current_working_directory(self).map(ToOwned::to_owned)
    }

    fn scrollback_line_count(&self) -> usize {
        Terminal::grid(self).scrollback_lines()
    }

    fn scrollback_line(&self, index: usize) -> Option<String> {
        Terminal::grid(self)
            .get_history_line(index)
            .map(|line| line.to_string())
    }

    fn scrollback_line_from_end(&self, reverse_index: usize) -> Option<String> {
        Terminal::grid(self)
            .history_line_rev(reverse_index)
            .map(|line| line.to_string())
    }
}

impl TerminalBlockAccess for Terminal {
    fn blocks(&self) -> Vec<TerminalBlockSnapshot> {
        self.all_blocks().map(block_snapshot_from_output).collect()
    }

    fn block_command(&self, block_id: u64) -> Option<String> {
        let block = self.block_by_id(block_id)?;
        self.block_command(block)
    }

    fn block_output(&self, block_id: u64) -> Option<String> {
        let block = self.block_by_id(block_id)?;
        self.block_output(block)
    }
}

fn block_snapshot_from_output(block: &crate::terminal::OutputBlock) -> TerminalBlockSnapshot {
    TerminalBlockSnapshot {
        id: block.id,
        state: block_state_from_core(block.state),
        prompt_start_row: block.prompt_start_row,
        command_start_row: block.command_start_row,
        output_start_row: block.output_start_row,
        end_row: block.end_row,
        exit_code: block.exit_code,
        working_directory: block.working_directory.as_deref().map(ToOwned::to_owned),
        commandline: block.commandline.as_deref().map(ToOwned::to_owned),
        collapsed: block.collapsed,
    }
}

fn block_state_from_core(state: BlockState) -> TerminalBlockState {
    match state {
        BlockState::PromptOnly => TerminalBlockState::PromptOnly,
        BlockState::EnteringCommand => TerminalBlockState::EnteringCommand,
        BlockState::Executing => TerminalBlockState::Executing,
        BlockState::Complete => TerminalBlockState::Complete,
        _ => TerminalBlockState::PromptOnly, // future variants default to PromptOnly
    }
}

#[cfg(test)]
mod tests {
    use aterm_types::TerminalSize;
    use aterm_types::terminal_host::{TerminalBlockAccess, TerminalHost};

    use crate::terminal::Terminal;

    #[test]
    fn terminal_host_exposes_size_and_scrollback() {
        let mut terminal = Terminal::new(2, 12);
        <Terminal as TerminalHost>::process(&mut terminal, b"line1\r\nline2\r\nline3");

        let size = <Terminal as TerminalHost>::size(&terminal);
        assert_eq!(size, TerminalSize::new(2, 12));
        assert_eq!(
            <Terminal as TerminalHost>::scrollback_line_count(&terminal),
            1
        );
        assert_eq!(
            <Terminal as TerminalHost>::scrollback_line(&terminal, 0).as_deref(),
            Some("line1")
        );
        assert_eq!(
            <Terminal as TerminalHost>::scrollback_line_from_end(&terminal, 0).as_deref(),
            Some("line1")
        );
    }

    #[test]
    fn terminal_block_access_snapshots_are_available() {
        let mut terminal = Terminal::new(4, 20);
        terminal.process(b"\x1b]133;A\x07");
        terminal.process(b"echo hi\r\n");
        terminal.process(b"\x1b]133;C\x07");
        terminal.process(b"hi\r\n");
        terminal.process(b"\x1b]133;D;0\x07");

        let blocks = <Terminal as TerminalBlockAccess>::blocks(&terminal);
        assert!(!blocks.is_empty());
    }
}
