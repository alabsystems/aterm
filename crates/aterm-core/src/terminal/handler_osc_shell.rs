// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC shell-integration handlers:
//! - OSC 133: FinalTerm/Terminal shell integration
//! - OSC 633: VS Code shell integration
// Zero tolerance: the shell marks are stamped with the injected instant (a
// clock read here was an incident), so no `#[expect]` may excuse one.
#![forbid(clippy::disallowed_methods)]

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

    /// Mark A (prompt start): open a mark, close the previous block, open a new
    /// one.
    fn shell_prompt_start(&mut self, row: u64, col: u16) {
        // Mark times (here and in B/C/D) come from the batch's pipeline clock
        // (`process_at`), never a wall-clock read, so a replayed or re-chunked
        // byte log stamps the same times.
        let mut mark = CommandMark::new(row, col);
        mark.prompt_time_ms = self.transient.process_wall_ms;
        if let Some(ref cwd) = *self.current_working_directory {
            mark.working_directory = Some(cwd.as_str().into());
        }
        self.shell.current_mark = Some(mark);
        self.shell.state = ShellState::ReceivingPrompt;

        if let Some(mut prev_block) = self.shell.current_block.take() {
            prev_block.end_row = Some(row);
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

    /// Shell mark C: Command execution starting. Counted
    /// (`started_seq`) whether or not a block holds it.
    fn shell_execution_start(&mut self, row: u64) {
        self.shell.started_seq = self.shell.started_seq.saturating_add(1);
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

    /// Dispatch the A/B/C/D marks OSC 133 and 633 share, enforcing A→B→C→D: A
    /// from any phase, B only after A, C after B, D after C; an out-of-order mark
    /// is ignored. Returns whether `cmd` is one of A/B/C/D, accepted or not.
    ///
    /// A and an accepted C end any kitty chunked load in flight: its program has
    /// ended or is not the one about to run, and a kept load would swallow the
    /// next program's first kitty command as its continuation.
    fn dispatch_shell_mark(&mut self, cmd: char, row: u64, col: u16, params: &[&[u8]]) -> bool {
        use super::grouped_state::ShellIntegrationPhase;

        match cmd {
            'A' => {
                // A is a hard reset to prompt start from any phase (Enter on an
                // empty line or Ctrl-C while typing emits A→B→A).
                self.shell.phase = ShellIntegrationPhase::PromptStart;
                self.shell_prompt_start(row, col);
                self.transient.kitty_pending = None;
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
                self.transient.kitty_pending = None;
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

    /// Gate OSC 133/633 on the capability nonce. With
    /// [`super::types::TerminalModes::require_shell_integration_nonce`] set, a
    /// mark passes only with an `id=<64-hex>` matching the host-authorized nonce
    /// (others are dropped and counted); an attached policy engine's Deny drops it
    /// first. With the bit clear (default) every mark passes. Returns whether to
    /// dispatch.
    fn shell_nonce_gate_ok(&mut self, command: u32, params: &[&[u8]]) -> bool {
        if !self.modes.require_shell_integration_nonce {
            return true;
        }
        let gate = self.policy.shell_integration_gate(command, params);
        self.shell_integration_auth
            .verify_nonce_with_engine(gate, params)
    }

    /// A shell prompt (OSC 133/633 `A`) on the alternate screen means the
    /// full-screen app the shell ran died without its `?1049l` (conhost never
    /// sends it for a client it outlived; neither does a SIGKILLed pager on
    /// unix), so leave the alt screen, archiving its content, before the prompt
    /// is placed.
    ///
    /// Only `A` triggers, and only past the nonce gate, so a forged `133;A`
    /// cannot yank a running app off its screen when the host demands the nonce.
    /// The prior phase is not consulted: the `A` can follow any phase
    /// (PromptStart or CommandStart when a key binding launched the app). Runs
    /// before `parse_shell_osc` reads the cursor, so the mark's row is the
    /// main-grid row the prompt paints on.
    fn prompt_leaves_orphaned_alt_screen(&mut self, params: &[&[u8]]) {
        if !self.modes.alternate_screen {
            return;
        }
        if params.get(1).and_then(|p| p.first()) != Some(&b'A') {
            return;
        }
        self.leave_orphaned_alternate_screen();
    }

    /// Handle OSC 133 (FinalTerm) shell integration: A prompt start, B command
    /// input, C execution start, D finished. Gated by `shell_nonce_gate_ok`.
    pub(super) fn handle_osc_133(&mut self, params: &[&[u8]]) {
        if !self.shell_nonce_gate_ok(133, params) {
            return;
        }
        self.prompt_leaves_orphaned_alt_screen(params);
        let Some((cmd, row, col)) = self.parse_shell_osc(params) else {
            return;
        };
        // Queue an event for poll-based hosts, only for a recognized A/B/C/D.
        if self.dispatch_shell_mark(cmd, row, col, params) {
            let payload = match cmd {
                'D' => format!("D;exit={}", Self::parse_exit_code(params)),
                _ => format!("{cmd};row={row};col={col}"),
            };
            self.queue_osc_event(133, payload);
        }
    }

    /// Handle OSC 633, VS Code shell integration
    /// (<https://code.visualstudio.com/docs/terminal/shell-integration>): the
    /// OSC 133 marks plus E (explicit command line) and P (`Cwd=`,
    /// `AtermIntegration=`). Nonce-gated like OSC 133.
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
                // Capped before unescaping: the mark and block `commandline`
                // fields are count-capped, not byte-capped, and the parser admits
                // up to MAX_OSC_DATA (8 MiB). Unescaping only shrinks, so this
                // bounds both the stored string and the work.
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
                if key != "Cwd" || value.is_empty() {
                    return;
                }
                // The OSC 7 cap, applied before the count-capped (not byte-capped)
                // mark and block fields.
                if value.len() > MAX_CWD_PATH_BYTES {
                    return;
                }
                if let Some(ref mut mark) = self.shell.current_mark {
                    mark.working_directory = Some(value.into());
                }
                if let Some(ref mut block) = self.shell.current_block {
                    block.working_directory = Some(value.into());
                }
                // Stored last; bumps the tab-label epoch on a real change, as
                // OSC 7 does.
                self.store_reported_cwd(Some(value));
            }
            _ => {}
        }
    }

    /// OSC 633 `P;AtermIntegration=<rev>`: which integration body the shell runs
    /// (`status integration_rev=`). Recorded only under the nonce gate, so the
    /// value is the shell's word, not a program's output; anything but 16
    /// lowercase hex digits (a script folder's address) is ignored.
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

    /// Unescape VS Code's command-line format: `\\` is a backslash and `\xAB`
    /// the byte 0xAB (shells escape `;` and every byte <= 0x20 this way). A
    /// non-ASCII byte becomes U+FFFD; an unknown or malformed escape stays
    /// literal.
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
    //! A killed pager's stream: `?1049h`, the pager's frames, then no `?1049l`,
    //! only the shell's `133;D;-1`, `133;A`, prompt and `133;B`. The host's own
    //! recovery is tested in `handler_dec.rs`; the archive side in
    //! `alt_archive_tests.rs`.
    use crate::terminal::Terminal;

    /// Eight lines of history, a marked `less` command, and the pager stuck on
    /// the alt screen.
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

    /// The shell's bytes after its pager is killed (pwsh after `Stop-Process`).
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
        // on a 5-row grid: 5 in scrollback, reachable again.
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
        // The kill's read carries the app's last paint and the prompt: the commit
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
        // screen. The authenticated one recovers.
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
