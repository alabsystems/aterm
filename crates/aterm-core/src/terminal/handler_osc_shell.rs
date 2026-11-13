// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 133 (FinalTerm) and OSC 633 (VS Code) shell-integration handlers.
#![forbid(clippy::disallowed_methods)]

use super::handler::TerminalHandler;
use super::shell::{
    BlockState, COMMAND_MARKS_MAX, CommandMark, OUTPUT_BLOCKS_MAX, OutputBlock, ShellState,
};
use super::{MAX_COMMANDLINE_BYTES, MAX_CWD_PATH_BYTES};

/// OSC 133/633 `B` parameter attesting that the mark stands at the line editor buffer start.
pub(super) const INPUT_START_ATTESTATION: &[u8] = b"aterm_input=start";

/// The most startup roots one START record names.
const MAX_START_ROOTS: usize = 32;

impl TerminalHandler<'_> {
    /// Parse shell integration OSC params into (command_char, absolute_row, col).
    #[inline]
    fn parse_shell_osc(&self, params: &[&[u8]]) -> Option<(char, u64, u16)> {
        let code = params.get(1).and_then(|p| std::str::from_utf8(p).ok())?;
        let cmd = (code.len() == 1).then(|| code.chars().next()).flatten()?;
        let cursor = self.grid.cursor();
        let row = self.grid.visible_to_absolute(cursor.row);
        Some((cmd, row, cursor.col))
    }

    /// Mark A (prompt start): open a mark, close the previous block, open a new one.
    fn shell_prompt_start(&mut self, row: u64, col: u16) {
        let mut mark = CommandMark::new(row, col);
        mark.prompt_time_ms = self.transient.process_wall_ms;
        if let Some(ref cwd) = *self.current_working_directory {
            mark.working_directory = Some(cwd.as_str().into());
        }
        self.shell.current_mark = Some(mark);
        self.shell.state = ShellState::ReceivingPrompt;

        if let Some(mut prev_block) = self.shell.current_block.take() {
            prev_block.end_row = Some(row);
            if matches!(
                prev_block.state,
                BlockState::PromptOnly | BlockState::EnteringCommand
            ) {
                for named in super::handler::block_rows_mut(&mut prev_block) {
                    *named = (*named).min(row);
                }
            }
            if self.shell.output_blocks.len() >= OUTPUT_BLOCKS_MAX {
                self.shell.output_blocks.pop_front();
            }
            self.shell.output_blocks.push_back(prev_block);
            self.shell.output_blocks.make_contiguous();
        }

        let mut block = OutputBlock::new(self.shell.next_block_id, row, col);
        block.prompt_time_ms = self.transient.process_wall_ms;
        self.shell.next_block_id += 1;
        if let Some(ref cwd) = *self.current_working_directory {
            block.working_directory = Some(cwd.as_str().into());
        }
        self.shell.current_block = Some(block);
    }

    /// Shell mark B: Command input starting (prompt finished).
    const fn shell_command_input_start(&mut self, row: u64, col: u16) {
        if let Some(ref mut mark) = self.shell.current_mark {
            mark.command_start_row = Some(row);
            mark.command_start_col = Some(col);
            mark.command_input_start_time_ms = self.transient.process_wall_ms;
        }
        self.shell.state = ShellState::EnteringCommand;

        if let Some(ref mut block) = self.shell.current_block {
            block.command_start_row = Some(row);
            block.command_start_col = Some(col);
            block.state = BlockState::EnteringCommand;
            block.command_input_start_time_ms = self.transient.process_wall_ms;
        }
    }

    /// Shell mark C: Command execution starting.
    fn shell_execution_start(&mut self, row: u64) {
        self.shell.started_seq = self.shell.started_seq.saturating_add(1);
        if let Some(ref mut mark) = self.shell.current_mark {
            mark.prompt_start_row = mark.prompt_start_row.min(row);
            mark.command_start_row = mark.command_start_row.map(|c| c.min(row));
            mark.output_start_row = Some(row);
            mark.command_exec_start_time_ms = self.transient.process_wall_ms;
        }
        self.shell.state = ShellState::Executing;

        if let Some(ref mut block) = self.shell.current_block {
            block.prompt_start_row = block.prompt_start_row.min(row);
            block.command_start_row = block.command_start_row.map(|c| c.min(row));
            block.output_start_row = Some(row);
            block.state = BlockState::Executing;
            block.command_exec_start_time_ms = self.transient.process_wall_ms;
        }
    }

    /// Shell mark D: Command finished — complete mark, update block state.
    fn shell_command_finished(&mut self, row: u64, exit_code: i32) {
        if let Some(mut mark) = self.shell.current_mark.take() {
            mark.output_end_row = Some(row);
            mark.exit_code = Some(exit_code);
            mark.command_end_time_ms = self.transient.process_wall_ms;
            if self.shell.command_marks.len() >= COMMAND_MARKS_MAX {
                self.shell.command_marks.pop_front();
            }
            self.shell.command_marks.push_back(mark);
            self.shell.command_marks.make_contiguous();
        }
        self.shell.state = ShellState::Ground;
        self.shell.completed_seq += 1;

        if let Some(ref mut block) = self.shell.current_block {
            block.exit_code = Some(exit_code);
            block.state = BlockState::Complete;
            block.command_end_time_ms = self.transient.process_wall_ms;
        }
        self.shell
            .last_finished
            .clone_from(&self.shell.current_block);
    }

    /// Parse exit code from OSC params (used by both 133 D and 633 D).
    #[inline]
    fn parse_exit_code(params: &[&[u8]]) -> i32 {
        params
            .get(2)
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0)
    }

    /// Dispatch the A/B/C/D marks OSC 133 and 633 share, enforcing A→B→C→D.
    fn dispatch_shell_mark(&mut self, cmd: char, row: u64, col: u16, params: &[&[u8]]) -> bool {
        use super::grouped_state::ShellIntegrationPhase;

        match cmd {
            'A' => {
                self.shell.phase = ShellIntegrationPhase::PromptStart;
                self.shell.phase_stale = false;
                self.shell.input_start_attested = false;
                if let Some(report) = self.shell.held_cwd.take() {
                    self.apply_cwd_report(report);
                }
                self.shell_prompt_start(row, col);
                self.transient.kitty_pending = None;
            }
            'B' => {
                if self.shell.phase != ShellIntegrationPhase::PromptStart {
                    return true;
                }
                self.shell.held_cwd = None;
                self.shell.phase = ShellIntegrationPhase::CommandStart;
                self.shell.input_start_attested = params
                    .get(2..)
                    .is_some_and(|p| p.contains(&INPUT_START_ATTESTATION));
                self.shell_command_input_start(row, col);
            }
            'C' => {
                if self.shell.phase != ShellIntegrationPhase::CommandStart {
                    return true;
                }
                self.shell.held_cwd = None;
                self.shell.unmarked_run = false;
                self.shell.phase = ShellIntegrationPhase::CommandExec;
                self.shell_execution_start(row);
                self.transient.kitty_pending = None;
            }
            'D' => {
                if self.shell.phase != ShellIntegrationPhase::CommandExec {
                    // The shell says a command finished while its marks say it
                    // sat at its prompt: it ran one it did not mark with `C`
                    // (unless its marks were dropped since that prompt).
                    if !self.shell.phase_stale
                        && matches!(
                            self.shell.phase,
                            ShellIntegrationPhase::PromptStart
                                | ShellIntegrationPhase::CommandStart
                        )
                    {
                        self.shell.unmarked_run = true;
                    }
                    return true;
                }
                self.shell.phase = ShellIntegrationPhase::CommandFinished;
                self.shell_command_finished(row, Self::parse_exit_code(params));
            }
            _ => return false,
        }
        true
    }

    /// Whether the authenticated shell sits at its prompt according to its own marks.
    pub(super) const fn shell_sits_at_prompt(&self) -> bool {
        use super::grouped_state::ShellIntegrationPhase;
        self.modes.require_shell_integration_nonce
            && self.shell_integration_auth.nonce_in_use().is_some()
            && !self.shell.phase_stale
            && self.shell.started_seq > 0
            && !self.shell.unmarked_run
            && match self.shell.phase {
                ShellIntegrationPhase::PromptStart | ShellIntegrationPhase::CommandStart => true,
                ShellIntegrationPhase::CommandFinished => self.shell.signs_cwd,
                ShellIntegrationPhase::None | ShellIntegrationPhase::CommandExec => false,
            }
    }

    /// Gate OSC 133/633 on the capability nonce.
    #[inline]
    fn shell_nonce_gate_ok(&mut self, command: u32, params: &[&[u8]]) -> bool {
        if !self.modes.require_shell_integration_nonce {
            return true;
        }
        let gate = self.policy.shell_integration_gate(command, params);
        let taking_key = self.shell_integration_auth.nonce_in_use().is_none();
        let ok = self
            .shell_integration_auth
            .verify_nonce_with_engine(gate, params);
        if ok && taking_key {
            self.shell.phase_stale = true;
        }
        ok
    }

    /// Leave and archive the alternate screen when a shell prompt (`A`) arrives on it.
    fn prompt_leaves_orphaned_alt_screen(&mut self, params: &[&[u8]]) {
        if params.get(1).copied() != Some(b"A") {
            return;
        }
        self.drop_held_size_report();
        if self.modes.alternate_screen {
            self.release_dead_app_paste();
            self.release_dead_app_input();
        }
        if self.modes.alternate_screen && self.transient.host_resyncs_alt_screen {
            let screen = Some(self.transient.screen_replaced);
            self.transient.dead_alt_screen = screen;
            let at = self.grid.cursor();
            self.transient.dead_alt_prompt_at =
                Some((self.transient.screen_replaced, at.row, at.col));
            self.take_main_pen_for_dead_screen_prompt();
            if self.transient.alt_screen_resync_screen.is_none() {
                self.transient.alt_screen_resync_screen = screen;
                self.transient.alt_screen_resync_requested = true;
            }
        } else if self.modes.alternate_screen {
            self.leave_orphaned_alternate_screen();
        }
        if self.transient.app_sync_open && !self.modes.synchronized_output {
            self.disable_synchronized_output();
        }
    }

    /// Handle OSC 133 (FinalTerm) shell integration.
    pub(super) fn handle_osc_133(&mut self, params: &[&[u8]]) {
        if !self.shell_nonce_gate_ok(133, params) {
            return;
        }
        self.prompt_leaves_orphaned_alt_screen(params);
        let Some((cmd, row, col)) = self.parse_shell_osc(params) else {
            return;
        };
        if self.dispatch_shell_mark(cmd, row, col, params) {
            match cmd {
                'D' => {
                    let exit = Self::parse_exit_code(params);
                    self.queue_osc_event(133, || format!("D;exit={exit}"));
                }
                _ => self.transient.osc_events.push_mark(cmd, row, col),
            }
        }
    }

    /// Handle OSC 633 (VS Code) shell integration.
    pub(super) fn handle_osc_633(&mut self, params: &[&[u8]]) {
        if !self.shell_nonce_gate_ok(633, params) {
            return;
        }
        self.prompt_leaves_orphaned_alt_screen(params);
        let Some((cmd, row, col)) = self.parse_shell_osc(params) else {
            return;
        };

        if self.dispatch_shell_mark(cmd, row, col, params) {
            return;
        }

        match cmd {
            'E' => {
                let Some(escaped_cmd) = params.get(2).and_then(|p| std::str::from_utf8(p).ok())
                else {
                    return;
                };
                if escaped_cmd.len() > MAX_COMMANDLINE_BYTES {
                    return;
                }
                let commandline = Self::unescape_vscode_string(escaped_cmd);
                if commandline.is_empty() {
                    return;
                }
                if let Some(ref mut mark) = self.shell.current_mark {
                    mark.commandline = Some(commandline.clone().into_boxed_str());
                }
                if let Some(ref mut block) = self.shell.current_block {
                    block.commandline = Some(commandline.into_boxed_str());
                }
            }
            'P' => {
                let Some(prop) = params.get(2).and_then(|p| std::str::from_utf8(p).ok()) else {
                    return;
                };
                let Some((key, value)) = prop.split_once('=') else {
                    return;
                };
                if key == crate::shell_integration::INTEGRATION_REV_KEY {
                    self.record_integration_rev(value);
                    return;
                }
                if key == crate::shell_integration::CWD_REPORT_KEY {
                    self.report_cwd(&[value.as_bytes()], true);
                    return;
                }
                if self.record_spare_mark(key, value) {
                    return;
                }
                if key != "Cwd" || value.is_empty() {
                    return;
                }
                if !super::handler_osc::reported_cwd_is_storable(value) {
                    return;
                }
                self.shell.held_cwd = None;
                self.shell.cwd_before_job = None;
                if let Some(ref mut mark) = self.shell.current_mark {
                    mark.working_directory = Some(value.into());
                }
                if let Some(ref mut block) = self.shell.current_block {
                    block.working_directory = Some(value.into());
                }
                self.store_reported_cwd(Some(value), None);
            }
            _ => {}
        }
    }

    /// OSC 633 `P;AtermIntegration=<rev>`.
    #[inline]
    const fn record_integration_rev(&mut self, value: &str) {
        if !self.modes.require_shell_integration_nonce
            || !crate::shell_integration::is_integration_rev(value)
        {
            return;
        }
        let mut rev = [0u8; 16];
        rev.copy_from_slice(value.as_bytes());
        self.shell.integration_rev = Some(rev);
    }

    /// OSC 633 `P` keys of the warm spare.
    fn record_spare_mark(&mut self, key: &str, value: &str) -> bool {
        use crate::shell_integration::{
            SPARE_KEY, START_FLAGS_KEY, START_KEY, START_RC_KEY, START_ROOTS_KEY,
        };
        if ![
            SPARE_KEY,
            START_KEY,
            START_RC_KEY,
            START_ROOTS_KEY,
            START_FLAGS_KEY,
        ]
        .contains(&key)
        {
            return false;
        }
        if !self.modes.require_shell_integration_nonce {
            return true;
        }
        let spare = &mut self.shell.spare;
        let ok = match key {
            SPARE_KEY => {
                let ok = value == "parked";
                spare.parked |= ok;
                ok
            }
            _ if spare.recorded => true,
            START_RC_KEY => {
                let rc = (!value.is_empty() && value.len() <= MAX_CWD_PATH_BYTES)
                    .then(|| Self::unescape_vscode_string(value))
                    .filter(|rc| super::handler_osc::reported_cwd_is_storable(rc));
                let ok = rc.is_some();
                if ok {
                    spare.pending_rc = rc;
                }
                ok
            }
            START_ROOTS_KEY => {
                let roots = Self::parse_start_roots(value);
                let ok = roots.is_some();
                spare.pending_roots = roots;
                ok
            }
            START_FLAGS_KEY => {
                let flags = super::shell::StartFlags::parse(value);
                spare.pending_flags = flags;
                flags.is_some()
            }
            _ => {
                let kind = match value {
                    "cold" => Some(super::shell::StartKind::Cold),
                    "spare" => Some(super::shell::StartKind::Spare),
                    _ => None,
                };
                if let Some(kind) = kind {
                    spare.recorded = true;
                    spare.mark = Some(super::shell::StartMark {
                        kind,
                        rc_dir: spare.pending_rc.take(),
                        roots: spare.pending_roots.take(),
                        flags: spare.pending_flags.take(),
                    });
                }
                kind.is_some()
            }
        };
        if !ok {
            spare.refused = spare.refused.saturating_add(1);
        }
        true
    }

    /// A START record's other startup roots (`AtermStartRoots=`).
    fn parse_start_roots(value: &str) -> Option<Vec<String>> {
        if value.is_empty() || value.len() > MAX_START_ROOTS * MAX_CWD_PATH_BYTES {
            return None;
        }
        let roots: Vec<String> = Self::unescape_vscode_string(value)
            .split('\0')
            .map(str::to_owned)
            .collect();
        (roots.len() <= MAX_START_ROOTS
            && roots
                .iter()
                .all(|root| root.starts_with('/') && root.len() <= MAX_CWD_PATH_BYTES))
        .then_some(roots)
    }

    /// Unescape VS Code's command-line format.
    fn unescape_vscode_string(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek() {
                    Some('\\') => {
                        chars.next();
                        result.push('\\');
                    }
                    Some('x') => {
                        chars.next();
                        let hex: String = chars.by_ref().take(2).collect();
                        if hex.len() == 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                            match u8::from_str_radix(&hex, 16) {
                                Ok(byte) if byte.is_ascii() => result.push(byte as char),
                                Ok(_) => result.push(char::REPLACEMENT_CHARACTER),
                                Err(_) => {
                                    result.push_str("\\x");
                                    result.push_str(&hex);
                                }
                            }
                        } else {
                            result.push_str("\\x");
                            result.push_str(&hex);
                        }
                    }
                    _ => {
                        result.push('\\');
                    }
                }
            } else {
                result.push(c);
            }
        }

        result
    }
}

impl super::Terminal {
    /// Bytes the console host writes after `?1049l` to resync the prompt onto the primary screen.
    #[must_use]
    pub fn dead_alt_screen_carry(&self) -> Option<String> {
        use std::fmt::Write as _;
        let screen = self.transient.screen_replaced;
        if !self.modes.alternate_screen || self.transient.dead_alt_screen != Some(screen) {
            return None;
        }
        let (at_screen, a_row, a_col) = self.transient.dead_alt_prompt_at?;
        if at_screen != screen {
            return None;
        }
        let alt = &self.grid;
        let cursor = alt.cursor();
        let wrap = alt.pending_wrap();
        let top = a_row.min(cursor.row);
        let left = if top == a_row { a_col } else { 0 };
        let left = if top == cursor.row {
            left.min(cursor.col)
        } else {
            left
        };
        if (cursor.row, cursor.col) == (top, left) && !wrap {
            return None;
        }
        let main = self.main_grid();
        let (saved_row, saved_col, saved_wrap) = self.cursor_save.main.map_or((0, 0, false), |s| {
            (s.cursor.row, s.cursor.col, s.pending_wrap)
        });
        let used = (0..main.rows())
            .rev()
            .find(|&r| main.row(r).is_some_and(|row| !row.is_empty()))
            .map_or(0, |r| r + 1);
        let free = saved_row
            .saturating_add(u16::from(saved_col > 0 || saved_wrap))
            .max(used);
        let rows = alt.rows();
        let scroll = free.saturating_sub(top).min(rows);
        let mut out = String::from("\x1b[0m\x1b[r");
        if scroll > 0 {
            let _ = write!(out, "\x1b[{rows};1H");
            for _ in 0..scroll {
                out.push('\n');
            }
        }
        let _ = write!(out, "\x1b[{};1H\x1b[J", top + 1);
        for r in top..=cursor.row {
            let _ = write!(out, "\x1b[{};1H", r + 1);
            let _ = alt.push_row_ansi_text_screen(r, &mut out);
            out.push_str("\x1b[0m");
        }
        if left > 0 {
            let _ = write!(out, "\x1b[{};1H\x1b[{left}X", top + 1);
        }
        let _ = write!(out, "\x1b[{};{}H", cursor.row + 1, cursor.col + 1);
        if !wrap {
            out.push_str("\x1b[K");
        }
        Some(out)
    }
}

#[cfg(test)]
#[path = "handler_osc_shell_prompt_on_alt_screen_tests.rs"]
mod prompt_on_alt_screen_tests;

#[cfg(test)]
#[path = "handler_osc_shell_spare_mark_tests.rs"]
mod spare_mark_tests;

#[cfg(test)]
#[path = "handler_osc_shell_reported_cwd_tests.rs"]
mod reported_cwd_tests;

#[cfg(test)]
#[path = "handler_osc_shell_dead_screen_carry_tests.rs"]
mod dead_screen_carry_tests;
