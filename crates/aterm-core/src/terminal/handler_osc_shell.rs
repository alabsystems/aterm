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
    BlockState, COMMAND_MARKS_MAX, CommandMark, OUTPUT_BLOCKS_MAX, OutputBlock, ShellState,
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

    /// Shell mark A: Prompt starting — create mark, finalize previous block, start new block.
    fn shell_prompt_start(&mut self, row: u64, col: u16) {
        // Every mark time comes from the batch's pipeline clock (`process_at`),
        // like B/C/D below — not from the constructors' own wall-clock read,
        // which made a replayed or re-chunked byte log stamp a different
        // prompt time (a checkpoint that was not a function of its input).
        let mut mark = CommandMark::new(row, col);
        mark.prompt_time_ms = self.transient.process_wall_ms;
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
        block.prompt_time_ms = self.transient.process_wall_ms;
        self.shell.next_block_id += 1;
        if let Some(ref cwd) = *self.current_working_directory {
            block.working_directory = Some(cwd.as_str().into());
        }
        self.shell.current_block = Some(block);
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
        let gate = self.policy.shell_integration_gate(command, params);
        self.shell_integration_auth
            .verify_nonce_with_engine(gate, params)
    }

    /// A shell PROMPT (OSC 133/633 `A`) on the ALTERNATE screen: the full-screen
    /// app the shell ran is dead and its `?1049l` never came, so leave the screen
    /// for it before the prompt is placed — a prompt on the alt screen is never
    /// intended; the shell believes it is on the main screen.
    ///
    /// MEASURED (2026-09-22, Windows 11, the `cast` tap): `less` killed from
    /// another tab; the bytes after the kill were pwsh's `133;D;-1`, `133;A`, the
    /// prompt and `133;B`, and nothing else — conhost never sends the `l` for a
    /// client it outlived (`handler_dec.rs`, `leave_orphaned_alternate_screen`,
    /// has the ConPTY probe). The same bytes follow a SIGKILLed pager on unix.
    ///
    /// Only `A` is a trigger, and only past the nonce gate: `A` is the one mark
    /// that says the shell is drawing its prompt, while `B`/`C`/`D` only open or
    /// close a block; and a forged `133;A` an unauthorized writer prints must not
    /// yank a running app off its screen when the host demands the nonce. (A
    /// multiplexer on the alt screen passes no pane's marks outward: screen and
    /// tmux forward no unknown OSC, and aterm's scripts install none in a pane.)
    /// The phase before the `A` is deliberately not consulted: pwsh's prompt
    /// sends `D` first (CommandFinished), a shell whose prompt sends no `D`
    /// arrives from CommandExec, and an app a key binding launched from the
    /// prompt (`fzf` on Ctrl-R) arrives from PromptStart/CommandStart — all the
    /// same fact. Runs BEFORE `parse_shell_osc` reads the cursor, so the mark's
    /// row is the main-grid row the prompt actually paints on. What the alt
    /// screen showed, including anything typed onto it while stuck, is flushed to
    /// the archive first (`offscreen`).
    fn prompt_leaves_orphaned_alt_screen(&mut self, params: &[&[u8]]) {
        if !self.modes.alternate_screen {
            return;
        }
        if params.get(1).and_then(|p| p.first()) != Some(&b'A') {
            return;
        }
        self.leave_orphaned_alternate_screen();
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
        self.prompt_leaves_orphaned_alt_screen(params);
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
        self.prompt_leaves_orphaned_alt_screen(params);
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
                if let Some(ref mut block) = self.shell.current_block {
                    block.commandline = Some(commandline.into_boxed_str());
                }
            }
            // 'F'/'G'/'H' (progress) retain nothing and no host consumes them:
            // they fall through to the ignored arm below.
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
                if key == crate::shell_integration::INTEGRATION_REV_KEY {
                    self.record_integration_rev(value);
                    return;
                }
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
                // Store last, bumping the shared tab-label epoch on a real
                // change like the OSC 7 path.
                self.store_reported_cwd(Some(value));
            }
            _ => {}
        }
    }

    /// OSC 633 `P;AtermIntegration=<rev>` — which integration BODY the shell
    /// runs (the LOADER / BODY split, 2026-09-26; `status integration_rev=`).
    ///
    /// Recorded ONLY when the nonce gate is on: the mark reached here signed
    /// with the shell's authorized key, so it is the shell's word and not a
    /// program's output (with the gate off every mark passes, and a revision
    /// anyone can print is worth nothing). The value must be a script folder's
    /// address — exactly 16 lowercase hex digits — or it is ignored.
    fn record_integration_rev(&mut self, value: &str) {
        if !self.modes.require_shell_integration_nonce
            || !crate::shell_integration::is_integration_rev(value)
        {
            return;
        }
        let mut rev = [0u8; 16];
        rev.copy_from_slice(value.as_bytes());
        self.shell.integration_rev = Some(rev);
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

#[cfg(test)]
mod prompt_on_alt_screen_tests {
    //! The measured stream of a killed pager (2026-09-22, ConPTY `cast` tap):
    //! `?1049h`, the pager's frames, then — no `?1049l` anywhere — the shell's
    //! `133;D;-1`, `133;A`, its prompt and `133;B`. The host's own recovery is
    //! tested beside the exit paths in `handler_dec.rs`; the archive's share of
    //! both is in `alt_archive_tests.rs`.
    use crate::terminal::Terminal;

    /// Eight lines of history, a marked `less` command, and the pager on the alt
    /// screen — the screen the audit's tab A was left with.
    fn stuck_pager() -> Terminal {
        let mut term = Terminal::new(5, 40);
        term.set_alt_archive_enabled(true);
        for i in 0..8 {
            term.process(format!("main line {i}\r\n").as_bytes());
        }
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07less file\r\n\x1b]133;C\x07");
        term.process(b"\x1b[?1049h\x1b[H\x1b[2Jpager row 1\r\npager row 2\r\npager row 3\r\n:");
        assert!(term.modes().alternate_screen);
        term
    }

    /// What pwsh sent after `Stop-Process`, byte for byte in shape.
    const PROMPT_AFTER_KILL: &[u8] = b"\r\n\x1b]133;D;-1\x07\x1b]133;A\x07PS> \x1b]133;B\x07";

    /// The visible rows, trailing blanks trimmed (a prompt's `PS> ` reads `PS>`).
    fn screen(term: &Terminal) -> Vec<String> {
        (0..5)
            .map(|r| term.row_text(r).unwrap().trim_end().to_string())
            .collect()
    }

    #[test]
    fn a_prompt_on_the_alt_screen_leaves_it_and_paints_on_the_main_grid() {
        let mut term = stuck_pager();
        term.process(PROMPT_AFTER_KILL);
        assert!(
            !term.modes().alternate_screen,
            "the prompt left the alt screen"
        );
        // 8 lines, the command line and the empty row its `\r\n` opened = 10 rows
        // on a 5-row grid: 5 in scrollback, reachable again (the audit measured
        // `lines` = 0 while stuck).
        assert_eq!(term.grid().scrollback_lines(), 5);
        let rows = screen(&term);
        assert_eq!(rows[3], "$ less file");
        assert_eq!(rows[4], "PS>", "the prompt is on the row after the command");
        assert!(rows.iter().all(|r| !r.contains("pager")), "{rows:?}");
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (4, 4));
    }

    #[test]
    fn the_prompt_mark_names_the_main_grid_row_it_painted_on() {
        let mut term = stuck_pager();
        term.process(PROMPT_AFTER_KILL);
        let mark = term
            .shell
            .current_mark
            .as_ref()
            .expect("the prompt opened a mark");
        // Absolute row 9 = 5 scrolled off + visible row 4: the row `PS> ` is on,
        // not the alt-grid row the cursor was on when the `A` arrived.
        assert_eq!(mark.prompt_start_row, 9);
        assert_eq!(mark.prompt_start_col, 0);
        assert_eq!(mark.command_start_col, Some(4));
        // The killed command's mark closed with the exit code pwsh reported.
        let done = term.command_marks().last().expect("the less mark closed");
        assert_eq!(done.exit_code, Some(-1));
    }

    #[test]
    fn what_the_stuck_screen_showed_is_in_the_archive_whole() {
        let mut term = stuck_pager();
        term.process(PROMPT_AFTER_KILL);
        let rows = term.alt_archive().texts();
        for want in ["pager row 1", "pager row 2", "pager row 3", ":"] {
            assert!(
                rows.iter().any(|r| r == want),
                "{want:?} was on the stuck screen and must be readable: {rows:?}"
            );
        }
        let gaps: Vec<_> = term.alt_archive().gaps().collect();
        assert_eq!(
            gaps.len(),
            1,
            "one leave, not one from the handler and one from the epilogue"
        );
        assert_eq!(gaps[0].kind, crate::terminal::AltArchiveGapKind::Leave);
        // A later main-screen batch archives nothing more.
        term.process(b"echo hi\r\nhi\r\n");
        assert_eq!(term.alt_archive().texts(), rows);
    }

    #[test]
    fn rows_painted_in_the_same_read_as_the_prompt_are_archived() {
        // The kill's read carries the app's last paint AND the prompt: the commit
        // must happen at the `A`, not at an epilogue that runs after the swap.
        let mut term = stuck_pager();
        let mut read = b"\x1b[5;1Hlast paint before death".to_vec();
        read.extend_from_slice(PROMPT_AFTER_KILL);
        term.process(&read);
        assert!(!term.modes().alternate_screen);
        let rows = term.alt_archive().texts();
        assert!(
            rows.iter().any(|r| r == "last paint before death"),
            "{rows:?}"
        );
    }

    #[test]
    fn b_c_and_d_on_the_alt_screen_do_not_leave_it() {
        // `D` for the very app entering the alt screen can share its read with
        // the `?1049h`; none of the three names a prompt.
        let mut term = stuck_pager();
        term.process(b"\x1b]133;D;0\x07\x1b]133;B\x07\x1b]133;C\x07");
        assert!(term.modes().alternate_screen);
        assert_eq!(term.grid().scrollback_lines(), 0);
    }

    #[test]
    fn a_vscode_633_prompt_leaves_the_alt_screen_too() {
        let mut term = stuck_pager();
        term.process(b"\r\n\x1b]633;D;-1\x07\x1b]633;A\x07PS> \x1b]633;B\x07");
        assert!(!term.modes().alternate_screen);
        assert_eq!(screen(&term)[4], "PS>");
    }

    #[test]
    fn a_prompt_the_nonce_gate_drops_leaves_nothing() {
        // With the nonce demanded, an unauthenticated `133;A` — what any program
        // on the alt screen could print — is dropped whole: the app keeps its
        // screen. The authenticated one recovers as before.
        let mut term = stuck_pager();
        term.authorize_shell_integration([0x11; 32]);
        term.modes_mut().require_shell_integration_nonce = true;
        term.process(b"\r\n\x1b]133;A\x07forged> ");
        assert!(
            term.modes().alternate_screen,
            "a forged prompt cannot yank the app"
        );
        let id = "11".repeat(32);
        term.process(format!("\x1b]133;A;id={id}\x07PS> ").as_bytes());
        assert!(!term.modes().alternate_screen);
        assert_eq!(screen(&term)[4], "PS>");
    }

    #[test]
    fn a_prompt_on_the_main_screen_changes_no_screen_state() {
        let mut term = Terminal::new(5, 40);
        term.process(b"one\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(!term.modes().alternate_screen);
        assert_eq!(screen(&term)[1], "$");
        assert_eq!(term.alt_archive().gaps().count(), 0);
    }
}
