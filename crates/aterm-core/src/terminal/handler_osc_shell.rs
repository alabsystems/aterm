// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC shell integration handlers for the terminal.
//!
//! This module contains handlers for shell-integration OSC sequences:
//! - OSC 133: FinalTerm/Terminal shell integration
//! - OSC 633: VS Code shell integration

use super::handler::TerminalHandler;
use super::shell::{
    BlockState, COMMAND_MARKS_MAX, CommandMark, OUTPUT_BLOCKS_MAX, OutputBlock, ShellEvent,
    ShellState,
};
use super::{MAX_COMMANDLINE_BYTES, MAX_CWD_PATH_BYTES};

impl TerminalHandler<'_> {
    /// Parse shell integration OSC params into (command_char, absolute_row, col).
    fn parse_shell_osc(&self, params: &[&[u8]]) -> Option<(char, u64, u16)> {
        let code = params.get(1).and_then(|p| std::str::from_utf8(p).ok())?;
        let cmd = code.chars().next()?;
        let cursor = self.grid.cursor();
        let row = self.grid.visible_to_absolute(cursor.row);
        Some((cmd, row, cursor.col))
    }

    /// Send a shell callback event if one is registered.
    fn emit_shell_event(&mut self, event: ShellEvent) {
        if let Some(ref mut cb) = self.shell.callback {
            cb(event);
        }
    }

    /// Notify shell callback consumers that the working directory changed.
    pub(super) fn shell_directory_changed(&mut self, path: Option<&str>) {
        self.emit_shell_event(ShellEvent::DirectoryChanged {
            path: path.map(Into::into),
        });
    }

    /// Shell mark A: Prompt starting — create mark, finalize previous block, start new block.
    fn shell_prompt_start(&mut self, row: u64, col: u16) {
        let mut mark = CommandMark::new(row, col);
        if let Some(ref cwd) = *self.current_working_directory {
            mark.working_directory = Some(cwd.as_str().into());
        }
        self.shell.current_mark = Some(mark);
        self.shell.state = ShellState::ReceivingPrompt;

        // Finalize any in-progress block
        if let Some(ref mut prev_block) = self.shell.current_block.take() {
            prev_block.end_row = Some(row);
            if self.shell.output_blocks.len() >= OUTPUT_BLOCKS_MAX {
                self.shell.output_blocks.pop_front();
            }
            self.shell.output_blocks.push_back(prev_block.clone());
            self.shell.output_blocks.make_contiguous();
        }

        // Start new block
        let mut block = OutputBlock::new(self.shell.next_block_id, row, col);
        self.shell.next_block_id += 1;
        if let Some(ref cwd) = *self.current_working_directory {
            block.working_directory = Some(cwd.as_str().into());
        }
        self.shell.current_block = Some(block);

        self.emit_shell_event(ShellEvent::PromptStart { row, col });
    }

    /// Shell mark B: Command input starting (prompt finished).
    fn shell_command_input_start(&mut self, row: u64, col: u16) {
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

        self.emit_shell_event(ShellEvent::CommandStart { row, col });
    }

    /// Shell mark C: Command execution starting.
    fn shell_execution_start(&mut self, row: u64) {
        if let Some(ref mut mark) = self.shell.current_mark {
            mark.output_start_row = Some(row);
            mark.command_exec_start_time_ms = self.transient.process_wall_ms;
        }
        self.shell.state = ShellState::Executing;

        if let Some(ref mut block) = self.shell.current_block {
            block.output_start_row = Some(row);
            block.state = BlockState::Executing;
            block.command_exec_start_time_ms = self.transient.process_wall_ms;
        }

        self.emit_shell_event(ShellEvent::OutputStart { row });
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

        self.emit_shell_event(ShellEvent::CommandFinished { exit_code });
    }

    /// Parse exit code from OSC params (used by both 133 D and 633 D).
    fn parse_exit_code(params: &[&[u8]]) -> i32 {
        params
            .get(2)
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0)
    }

    /// Dispatch shared A/B/C/D shell integration marks (common to OSC 133 and 633).
    ///
    /// Enforces the valid A→B→C→D state machine. Out-of-order markers are
    /// silently ignored, matching Terminal behavior (#7668). Valid transitions:
    /// - `None`/`CommandFinished` → A (prompt start)
    /// - `PromptStart` → B (command input start)
    /// - `CommandStart` → C (command execution start)
    /// - `CommandExec` → D (command finished)
    ///
    /// Returns `true` if the command character was recognized (A/B/C/D),
    /// regardless of whether the transition was accepted.
    fn dispatch_shell_mark(&mut self, cmd: char, row: u64, col: u16, params: &[&[u8]]) -> bool {
        use super::grouped_state::ShellIntegrationPhase;

        match cmd {
            'A' => {
                // Accept A from any phase — Terminal treats A as a hard reset
                // to prompt-start. Common when user presses Enter on an empty
                // line or Ctrl-C during typing (shell emits A→B→A) (#7684).
                self.shell.phase = ShellIntegrationPhase::PromptStart;
                self.shell_prompt_start(row, col);
            }
            'B' => {
                if self.shell.phase != ShellIntegrationPhase::PromptStart {
                    return true;
                }
                self.shell.phase = ShellIntegrationPhase::CommandStart;
                self.shell_command_input_start(row, col);
            }
            'C' => {
                if self.shell.phase != ShellIntegrationPhase::CommandStart {
                    return true;
                }
                self.shell.phase = ShellIntegrationPhase::CommandExec;
                self.shell_execution_start(row);
            }
            'D' => {
                if self.shell.phase != ShellIntegrationPhase::CommandExec {
                    return true;
                }
                self.shell.phase = ShellIntegrationPhase::CommandFinished;
                self.shell_command_finished(row, Self::parse_exit_code(params));
            }
            _ => return false,
        }
        true
    }

    /// Gate OSC 133/633 on the capability nonce (#7937 F01-2, #7960).
    ///
    /// When [`super::types::TerminalModes::require_shell_integration_nonce`]
    /// is set, every OSC 133 A/B/C/D and OSC 633 A/B/C/D/E/F/G/H/P must
    /// carry an `id=<64-hex>` parameter matching the host-authorized nonce.
    /// Sequences without a matching nonce are silently dropped (no state
    /// transition, no callback, no response) and counted in
    /// `ShellIntegrationAuth::dropped_count`. When the bit is clear
    /// (default), the handler preserves the pre-nonce dispatch behavior
    /// for backward compatibility with unnonced shell integrations.
    ///
    /// Returns `true` if dispatch should proceed, `false` if the handler
    /// should silently drop.
    ///
    /// The engine-consulting variant (#7994) is used when a policy engine is
    /// attached: the engine's decision wins before the nonce check runs (per
    /// design §6.3), with Deny dropping the sequence, Allow/Fallback deferring
    /// to the existing nonce check. When no policy engine is attached, the
    /// legacy nonce-only gate is preserved.
    fn shell_nonce_gate_ok(&mut self, command: u32, params: &[&[u8]]) -> bool {
        if !self.modes.require_shell_integration_nonce {
            return true;
        }
        self.shell_integration_auth.verify_nonce_with_engine(
            self.policy_engine.as_ref(),
            aterm_policy::OriginTag::Pty,
            command,
            params,
        )
    }

    /// Handle OSC 133 - Shell integration (FinalTerm/Terminal protocol).
    ///
    /// Marks: A (prompt start), B (command input), C (execution start), D (finished).
    ///
    /// When `modes.require_shell_integration_nonce` is set, requires every
    /// sequence to carry a valid `id=<64-hex>` nonce (#7937 F01-2, #7960).
    pub(super) fn handle_osc_133(&mut self, params: &[&[u8]]) {
        if !self.shell_nonce_gate_ok(133, params) {
            return;
        }
        let Some((cmd, row, col)) = self.parse_shell_osc(params) else {
            return;
        };
        // Queue a compact REAL mark for poll-based hosts, but only for a
        // recognized A/B/C/D char (dispatch returns true) so out-of-band chars
        // don't surface as bogus events. row/col come from the live cursor.
        if self.dispatch_shell_mark(cmd, row, col, params) {
            let payload = match cmd {
                'D' => format!("D;exit={}", Self::parse_exit_code(params)),
                _ => format!("{cmd};row={row};col={col}"),
            };
            self.queue_osc_event(133, payload);
        }
    }

    /// Handle OSC 633 - VS Code shell integration protocol.
    ///
    /// Extends OSC 133 with E/F/G/H (payload/progress) and P (property settings).
    /// See: <https://code.visualstudio.com/docs/terminal/shell-integration>
    ///
    /// When `modes.require_shell_integration_nonce` is set, requires every
    /// sequence to carry a valid `id=<64-hex>` nonce (#7937 F01-2, #7960).
    pub(super) fn handle_osc_633(&mut self, params: &[&[u8]]) {
        if !self.shell_nonce_gate_ok(633, params) {
            return;
        }
        let Some((cmd, row, col)) = self.parse_shell_osc(params) else {
            return;
        };

        if self.dispatch_shell_mark(cmd, row, col, params) {
            return;
        }

        match cmd {
            'E' => {
                // Explicit command text (VS Code extension)
                let Some(escaped_cmd) = params.get(2).and_then(|p| std::str::from_utf8(p).ok())
                else {
                    return;
                };
                // Reject an absurd command line before unescaping/retaining it,
                // mirroring the Cwd cap: it lands in the count-capped (but not
                // byte-capped) shell mark/block `commandline` fields and the parser
                // admits up to MAX_OSC_DATA (8 MiB). unescape only shrinks, so
                // bounding the escaped input bounds both the stored string and the
                // unescape work. (#7172)
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
                let semantic_text = commandline.clone().into_boxed_str();
                if let Some(ref mut block) = self.shell.current_block {
                    block.commandline = Some(commandline.into_boxed_str());
                }
                self.emit_shell_event(ShellEvent::SemanticText {
                    text: semantic_text,
                });
            }
            'F' | 'G' | 'H' => {
                let payload = params
                    .get(2)
                    .and_then(|p| std::str::from_utf8(p).ok())
                    // Bound the payload before unescaping (same cap as `E`); these
                    // are transient (emitted, not retained) but still shouldn't
                    // unescape a multi-MiB blob on every progress update.
                    .filter(|p| p.len() <= MAX_COMMANDLINE_BYTES)
                    .map(Self::unescape_vscode_string)
                    .filter(|payload| !payload.is_empty())
                    .map(String::into_boxed_str);
                let event = match cmd {
                    'F' => ShellEvent::ProgressStart { payload },
                    'G' => ShellEvent::ProgressUpdate { payload },
                    'H' => ShellEvent::ProgressEnd { payload },
                    _ => return,
                };
                self.emit_shell_event(event);
            }
            'P' => {
                // Property setting (VS Code extension)
                let Some(prop) = params.get(2).and_then(|p| std::str::from_utf8(p).ok()) else {
                    return;
                };
                let Some(pos) = prop.find('=') else {
                    return;
                };
                let key = &prop[..pos];
                let value = &prop[pos + 1..];
                if key != "Cwd" || value.is_empty() {
                    return;
                }
                // Reject absurd cwd paths before they reach the count-capped
                // (but not byte-capped) current_working_directory / mark / block
                // working-directory fields, mirroring the OSC 7 cap. The parser
                // admits up to MAX_OSC_DATA (8 MiB); 4 KiB (PATH_MAX) is generous
                // for any real directory. (#7172)
                if value.len() > MAX_CWD_PATH_BYTES {
                    return;
                }
                if let Some(ref mut mark) = self.shell.current_mark {
                    mark.working_directory = Some(value.into());
                }
                if let Some(ref mut block) = self.shell.current_block {
                    block.working_directory = Some(value.into());
                }
                // Store + notify last (`DirectoryChanged` keeps firing after the
                // mark/block fields are current, as before), bumping the shared
                // tab-label epoch on a real change like the OSC 7 path.
                self.store_reported_cwd(Some(value));
            }
            _ => {}
        }
    }

    /// Unescape VS Code's command line escape format.
    ///
    /// VS Code shell integration uses backslash escapes:
    /// - `\\` → `\` (literal backslash)
    /// - `\xAB` → byte with hex value AB
    ///
    /// Shells must escape:
    /// - `;` as `\x3b`
    /// - All bytes <= 0x20 (control chars, space)
    fn unescape_vscode_string(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek() {
                    Some('\\') => {
                        // \\ → literal backslash
                        chars.next();
                        result.push('\\');
                    }
                    Some('x') => {
                        // \xAB → hex-encoded byte
                        chars.next(); // consume 'x'
                        let hex: String = chars.by_ref().take(2).collect();
                        if hex.len() == 2 {
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
                        // Unknown escape, output literally
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
