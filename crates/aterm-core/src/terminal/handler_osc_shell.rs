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
            // A prompt (no C came) has no row past its end. A full clear
            // restarted it where the screen restarts, and the shell can write
            // more above its rows before this A (a hook between its A and its
            // prompt that Ctrl-L's redraw leaves out), or send this A from the
            // redraw itself (fish). A command's rows stay: a clear already
            // left none of them past where the shell writes next, and one
            // that moved the cursor above its own command line without a
            // clear (`tput cup`, a TUI that exits with the cursor high) still
            // reads its command and output there, as its command mark does.
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
    ///
    /// The prompt and command rows are at most this one, so the command range
    /// never runs backwards. A full clear at the prompt can leave them past
    /// it: Ctrl-L's redraw at home leaves out what the shell wrote between its
    /// A and its prompt, and a clear the line editor never saw (a background
    /// job's `ESC[2J ESC[H`) moves the cursor up under it.
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
                // The shell reported its own cwd immediately before this mark:
                // a report held at its prompt lands now, so the prompt's mark
                // and block record it.
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
                // A report since the A was the prompt's own text, not the
                // shell's (which comes before the A).
                self.shell.held_cwd = None;
                self.shell.phase = ShellIntegrationPhase::CommandStart;
                self.shell_command_input_start(row, col);
            }
            'C' => {
                if self.shell.phase != ShellIntegrationPhase::CommandStart {
                    return true;
                }
                // A report held at the prompt was not the shell's: the shell
                // starts a command instead of redrawing its prompt.
                self.shell.held_cwd = None;
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

    /// Whether an OSC 7 waits for the shell's next prompt start instead of
    /// landing now: only while an authenticated shell sits at its prompt, after
    /// its `A` and before its `C`, and only where that phase is the shell's own
    /// word that it runs nothing:
    ///
    /// - the nonce gate is on and its nonce is in use (`status
    ///   integration=on`), so the phase follows the shell's marks alone (a
    ///   remote shell's or a program's are dropped) and the shell's marks
    ///   arrive. A successor whose handoff lost the nonce, or a re-key the
    ///   shell has not taken, drops them all, and the phase it shows is stale.
    /// - the shell has marked a command it ran with `C` (`started_seq`), so
    ///   it is one that marks what it runs. PowerShell without PSReadLine never
    ///   sends `C`, and a program it starts speaks while the phase still reads
    ///   its prompt.
    ///
    /// Then a report is a background job's output or the prompt's own text,
    /// never a program the shell started (a remote shell over ssh, a nested
    /// shell, a script's `cd`), which all speak after the `C` and land at once,
    /// as does the shell's own report after a `D`. Every integration reports
    /// its own cwd immediately before its next `A`, which applies the latest
    /// held report; a `B` or a `C` first drops it. A program a key binding
    /// starts runs with no `C`, so what it reports is held too.
    pub(super) fn reported_cwd_waits_for_prompt(&self) -> bool {
        use super::grouped_state::ShellIntegrationPhase;
        self.modes.require_shell_integration_nonce
            && self.shell_integration_auth.nonce_in_use().is_some()
            && self.shell.started_seq > 0
            && matches!(
                self.shell.phase,
                ShellIntegrationPhase::PromptStart | ShellIntegrationPhase::CommandStart
            )
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
    ///
    /// On either screen it also ends a program's synchronized frame the
    /// terminal's timeout already cleared the mode under: a live program's
    /// bracket, the shell's own prompt redraw included, closes within its
    /// timeout, so one still open past it at a prompt belongs to a program
    /// that died mid-frame. Left set, `Terminal::sync_frame_unfinished` would
    /// withhold every later hidden caret from the motion engines.
    ///
    /// Under a console host that forwards screen switches verbatim
    /// (`TransientState::host_resyncs_alt_screen`, OpenConsole; off unless the
    /// embedder sets it) the alt screen is NOT left here: the host is asked, once
    /// per stuck screen, to leave its own dead buffer, and the `?1049l` it
    /// forwards leaves ours from the same place in the stream.
    fn prompt_leaves_orphaned_alt_screen(&mut self, params: &[&[u8]]) {
        if params.get(1).and_then(|p| p.first()) != Some(&b'A') {
            return;
        }
        if self.modes.alternate_screen && self.transient.host_resyncs_alt_screen {
            let screen = Some(self.transient.screen_replaced);
            if self.transient.alt_screen_resync_screen != screen {
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
                if self.record_spare_mark(key, value) {
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
                // The shell's own word: an OSC 7 held at its prompt does not
                // override it at the next `A`.
                self.shell.held_cwd = None;
                if let Some(ref mut mark) = self.shell.current_mark {
                    mark.working_directory = Some(value.into());
                }
                if let Some(ref mut block) = self.shell.current_block {
                    block.working_directory = Some(value.into());
                }
                // Stored last; bumps the tab-label epoch on a real change, as
                // OSC 7 does.
                // The shell's own word names a path on its own machine: no host.
                self.store_reported_cwd(Some(value), None);
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

    /// OSC 633 `P` keys of the warm spare (`docs/DESIGN-warm-spare-2026-10-16.md`
    /// §4): `AtermSpare=parked`, and the START record's `AtermStartRc=`,
    /// `AtermStartFlags=` and `AtermStart=cold|spare`, which completes it.
    /// Recorded only under the nonce gate, like [`Self::record_integration_rev`],
    /// so a program's output cannot forge a spare's readiness. Parsed strictly;
    /// a malformed value is ignored and counted. Only the first record is kept.
    /// Returns whether `key` is one of these.
    fn record_spare_mark(&mut self, key: &str, value: &str) -> bool {
        use crate::shell_integration::{SPARE_KEY, START_FLAGS_KEY, START_KEY, START_RC_KEY};
        if ![SPARE_KEY, START_KEY, START_RC_KEY, START_FLAGS_KEY].contains(&key) {
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
                let ok = !value.is_empty() && value.len() <= MAX_CWD_PATH_BYTES;
                if ok {
                    spare.pending_rc = Some(Self::unescape_vscode_string(value));
                }
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

    // The host policy is off unless the embedder sets it, and off it changes
    // nothing: the prompt leaves the stuck screen locally, exactly as above, and
    // no resync is ever requested. Turned on and off again, the local leave is
    // back.
    #[test]
    fn with_the_resync_policy_off_the_prompt_leaves_locally_and_requests_nothing() {
        let mut term = stuck_pager();
        term.process(PROMPT_AFTER_KILL);
        assert!(!term.modes().alternate_screen, "the local leave ran");
        assert_eq!(screen(&term)[4], "PS>");
        assert!(!term.take_alt_screen_resync_request());

        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.set_host_resyncs_alt_screen(false);
        term.process(PROMPT_AFTER_KILL);
        assert!(!term.modes().alternate_screen, "off again: the local leave");
        assert!(!term.take_alt_screen_resync_request());

        let requesting = || {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(true);
            term.process(PROMPT_AFTER_KILL);
            assert!(term.modes().alternate_screen, "the host must leave it");
            term
        };
        let mut control = requesting();
        assert!(
            control.take_alt_screen_resync_request(),
            "control: a request is pending"
        );

        let mut term = requesting();
        term.set_host_resyncs_alt_screen(false);
        assert!(
            !term.take_alt_screen_resync_request(),
            "ALT_RESYNC_REQUEST_SURVIVED_OPT_OFF"
        );
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        assert!(
            !term.take_alt_screen_resync_request(),
            "opting off keeps the once-per-screen memo"
        );
    }

    // Under a host that forwards screen switches (OpenConsole) the prompt does
    // not leave the screen: it asks the host ONCE for this stuck screen, and the
    // `?1049l` the host then forwards leaves it the ordinary way: the main grid
    // and the cursor `?1049h` saved come back, as they do on the host's side.
    #[test]
    fn under_a_resyncing_host_the_prompt_requests_the_host_leave_once() {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.modes().alternate_screen, "no local leave");
        assert!(term.take_alt_screen_resync_request(), "one request");
        assert!(!term.take_alt_screen_resync_request(), "taken");
        // A second prompt on the SAME stuck screen does not ask again: a resync
        // that never arrives leaves the screen up, as Windows Terminal does.
        term.process(PROMPT_AFTER_KILL);
        assert!(!term.take_alt_screen_resync_request());
        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "nothing scrolled locally"
        );

        // The host's forwarded switch: the screen, and the cursor the `?1049h`
        // saved (row 4, col 0 after `less file\r\n`), come back.
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
        let rows = screen(&term);
        assert_eq!(rows[3], "$ less file");
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (4, 0));
    }

    // Each stuck screen asks once: a later app that dies on a NEW alternate
    // screen raises a new request.
    #[test]
    fn a_new_stuck_screen_asks_again() {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.take_alt_screen_resync_request());
        term.process(b"\x1b[?1049l");
        term.process(b"\x1b[?1049hsecond pager");
        term.process(PROMPT_AFTER_KILL);
        assert!(term.take_alt_screen_resync_request(), "the new screen asks");
    }

    // The policy is the host's: a RIS from the byte stream keeps it and drops
    // only a pending request (the reset left the screen already).
    #[test]
    fn ris_keeps_the_resync_policy() {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        term.process(b"\x1bc");
        assert!(
            !term.take_alt_screen_resync_request(),
            "RIS drops the request"
        );
        term.process(b"\x1b[?1049hpager again");
        term.process(PROMPT_AFTER_KILL);
        assert!(
            term.modes().alternate_screen,
            "still no local leave after RIS"
        );
        assert!(term.take_alt_screen_resync_request());
    }
}

#[cfg(test)]
mod spare_mark_tests {
    //! The warm spare's marks (`docs/DESIGN-warm-spare-2026-10-16.md` §4, test
    //! E1): only a signed mark counts, a malformed one is refused and counted,
    //! only the first START record is kept, and each take happens once.
    use crate::terminal::{ShellIntegrationNonce, StartFlags, StartKind, StartMark, Terminal};

    const KEY: [u8; 32] = [0x3Cu8; 32];

    fn id() -> String {
        format!(";id={}", ShellIntegrationNonce(KEY).to_hex())
    }

    fn p(prop: &str, id: &str) -> Vec<u8> {
        format!("\x1b]633;P;{prop}{id}\x07").into_bytes()
    }

    fn gated() -> Terminal {
        let mut t = Terminal::new(4, 40);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        t
    }

    fn record(t: &mut Terminal, rc: &str, flags: &str, kind: &str) {
        let id = id();
        t.process(&p(&format!("AtermStartRc={rc}"), &id));
        t.process(&p(&format!("AtermStartFlags={flags}"), &id));
        t.process(&p(&format!("AtermStart={kind}"), &id));
    }

    #[test]
    fn spare_marks_count_only_signed_and_are_taken_once() {
        // The gate off (a shell nobody authorized): a program's output claims nothing.
        let mut open = Terminal::new(4, 40);
        open.process(&p("AtermSpare=parked", ""));
        record(&mut open, "/h", "pre:0,s:0,j:0,r:0", "spare");
        assert!(!open.take_spare_parked());
        assert_eq!(open.take_start_mark(), None);

        let mut t = gated();
        // Unsigned and wrong-id marks are dropped by the nonce gate.
        t.process(&p("AtermSpare=parked", ""));
        t.process(&p("AtermSpare=parked", ";id=00"));
        t.process(&p("AtermStart=spare", ";id=00"));
        assert!(
            !t.take_spare_parked(),
            "an unsigned parked mark is no spare"
        );
        assert_eq!(t.take_start_mark(), None);

        // Malformed signed values are ignored and counted.
        let id = id();
        for bad in [
            "AtermSpare=warm",
            "AtermStart=hot",
            "AtermStartFlags=pre:0,s:0",
            "AtermStartFlags=pre:0,s:0,j:0,r:2",
            "AtermStartFlags=pre:x,s:0,j:0,r:0",
            "AtermStartFlags=pre:0,s:0,j:0,r:0,z:1",
            "AtermStartRc=",
        ] {
            t.process(&p(bad, &id));
        }
        assert_eq!(t.spare_marks_refused(), 7);
        assert!(!t.take_spare_parked());
        assert_eq!(t.take_start_mark(), None);

        // The parked one-shot.
        t.process(&p("AtermSpare=parked", &id));
        assert!(t.take_spare_parked());
        assert!(!t.take_spare_parked(), "taken once");

        // A record: the rc unescaped, the flags parsed, then taken once.
        record(&mut t, "/Users//a\\x20b\\x3bc", "pre:2,s:1,j:-,r:1", "spare");
        let want = StartMark {
            kind: StartKind::Spare,
            rc_dir: Some("/Users//a b;c".into()),
            flags: Some(StartFlags {
                pre: 2,
                sched: 1,
                jobs: None,
                periodic: true,
            }),
        };
        assert_eq!(t.take_start_mark(), Some(want));
        assert_eq!(t.take_start_mark(), None, "taken once");

        // Only the first record is kept: a later one changes nothing.
        record(&mut t, "/other", "pre:0,s:0,j:0,r:0", "cold");
        assert_eq!(t.take_start_mark(), None);
        assert_eq!(t.spare_marks_refused(), 7, "a later record is not refused");
    }

    #[test]
    fn a_prompt_start_is_seen_only_once_signed() {
        let mut t = gated();
        assert!(!t.shell_prompt_started());
        t.process(b"\x1b]133;A\x07");
        assert!(!t.shell_prompt_started(), "an unsigned A is dropped");
        t.process(format!("\x1b]133;A{}\x07", id()).as_bytes());
        assert!(t.shell_prompt_started());
    }
}

#[cfg(test)]
mod reported_cwd_tests {
    //! Which OSC 7 sets the folder (New Tab, Split, New Window, `aterm ctl cwd`)
    //! while an authenticated shell holds its prompt. OSC 7 carries no nonce (a
    //! remote shell over ssh must be able to report its cwd), so the shell's own
    //! signed marks say who can be speaking: between its `A` and its `C` the
    //! shell sits at its prompt and runs nothing, and every integration reports
    //! its own cwd immediately before its next `A`. That holds only while its
    //! marks arrive and once it has marked a command it ran with `C`.
    use crate::terminal::{ShellIntegrationNonce, Terminal};

    const KEY: [u8; 32] = [0x5Du8; 32];

    /// The shell's signed OSC 133 mark `mark` (`A`, `B`, `C`, `D;0`).
    fn signed(mark: &str) -> Vec<u8> {
        format!(
            "\x1b]133;{mark};id={}\x07",
            ShellIntegrationNonce(KEY).to_hex()
        )
        .into_bytes()
    }

    fn osc7(path: &str) -> Vec<u8> {
        format!("\x1b]7;file://host{path}\x07").into_bytes()
    }

    /// A gated terminal whose shell reported `/home/me` and drew its prompt,
    /// signing every mark with `KEY`.
    fn gated_at_first_prompt() -> Terminal {
        let mut t = Terminal::new(6, 60);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        t.process(&osc7("/home/me"));
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed("B"));
        t
    }

    /// [`gated_at_first_prompt`] after the shell ran one command it marked
    /// with `C` (`true`), and drew its prompt again.
    fn at_prompt() -> Terminal {
        let mut t = gated_at_first_prompt();
        t.process(b"true\r\n");
        t.process(&signed("C"));
        t.process(&signed("D;0"));
        t.process(&osc7("/home/me"));
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed("B"));
        while t.take_osc_event().is_some() {}
        t
    }

    fn cwd_events(t: &mut Terminal) -> Vec<String> {
        std::iter::from_fn(|| t.take_osc_event())
            .filter(|(code, _)| *code == 7)
            .map(|(_, payload)| payload)
            .collect()
    }

    /// A stray report while the prompt is idle (a background job's output, or
    /// the user's own prompt text) sets nothing: the folder stays the shell's
    /// own. The shell's next prompt re-asserts its cwd before its `A`, which
    /// replaces the stray; a command the shell starts first (`C`) drops it.
    #[test]
    fn a_stray_report_at_the_idle_prompt_sets_no_folder() {
        let mut t = at_prompt();
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert_eq!(cwd_events(&mut t), Vec::<String>::new());
        // The prompt is redrawn (Ctrl-C, an empty Enter): the re-assert lands.
        t.process(&osc7("/home/me"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert_eq!(cwd_events(&mut t), vec!["//host/home/me".to_owned()]);
        let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
        assert_eq!(mark.working_directory.as_deref(), Some("/home/me"));

        // A stray, then the user runs a command: dropped, never applied.
        let mut t = at_prompt();
        t.process(&osc7("/elsewhere"));
        t.process(b"ls\r\n");
        t.process(&signed("C"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        t.process(&signed("D;0"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert_eq!(cwd_events(&mut t), Vec::<String>::new());
    }

    /// A shell that changes folder at its prompt (fish's Alt-Left, a widget that
    /// runs `cd` and redraws) reports it immediately before the redrawn
    /// prompt's `A`, which commits it, and the prompt's mark records it.
    #[test]
    fn a_folder_change_at_the_prompt_lands_with_the_redrawn_prompt() {
        let mut t = at_prompt();
        t.process(&osc7("/home/me/src"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me/src"));
        let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
        assert_eq!(mark.working_directory.as_deref(), Some("/home/me/src"));
        assert_eq!(cwd_events(&mut t), vec!["//host/home/me/src".to_owned()]);
    }

    /// A program the shell runs reports at once: a remote shell over ssh (its
    /// own marks are unsigned and dropped), a nested shell with its own
    /// integration, `cd` inside a script. And the shell's own report after a
    /// command ends (`D`, then OSC 7, then `A`) lands at once too.
    #[test]
    fn a_report_while_a_command_runs_or_after_it_ends_lands_at_once() {
        let mut t = at_prompt();
        t.process(b"ssh build\r\n");
        t.process(&signed("C"));
        // The remote shell's own integration: unsigned marks, its cwd.
        t.process(b"\x1b]133;D;0\x07");
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        t.process(b"\x1b]133;A\x07remote$ \x1b]133;B\x07");
        t.process(&osc7("/srv/app/logs"));
        assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
        assert_eq!(
            cwd_events(&mut t),
            vec![
                "//host/srv/app".to_owned(),
                "//host/srv/app/logs".to_owned()
            ]
        );
        // ssh exits; the local shell finishes the command and re-asserts.
        t.process(&signed("D;0"));
        t.process(&osc7("/home/me"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
    }

    /// With no authenticated shell (the nonce gate off), marks are anyone's, so
    /// they say nothing about who speaks: every report lands at once.
    #[test]
    fn without_the_nonce_gate_every_report_lands_at_once() {
        let mut t = Terminal::new(6, 60);
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/elsewhere"));
    }

    /// The shell's signed `633;P;Cwd=` is its own word: it lands at once at the
    /// idle prompt, and a stray report held before it does not override it at
    /// the next `A`.
    #[test]
    fn a_signed_cwd_property_lands_at_once_over_a_stray_report() {
        let mut t = at_prompt();
        t.process(&osc7("/elsewhere"));
        t.process(
            format!(
                "\x1b]633;P;Cwd=/home/me/docs;id={}\x07",
                ShellIntegrationNonce(KEY).to_hex()
            )
            .as_bytes(),
        );
        assert_eq!(t.current_working_directory(), Some("/home/me/docs"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me/docs"));
    }

    /// A shell that has marked no command with `C` gives no phase in which it
    /// runs nothing: PowerShell without PSReadLine skips `C` by design
    /// (`aterm_shell_integration.ps1`), so a program it starts (ssh, a nested
    /// shell) speaks while the phase still reads its prompt. Every report lands
    /// at once, as with no gate.
    #[test]
    fn a_shell_that_marks_no_command_holds_no_report() {
        let mut t = gated_at_first_prompt();
        // `ssh build` runs with no 133;C; the remote shell reports its cwd.
        t.process(b"ssh build\r\n");
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        t.process(&osc7("/srv/app/logs"));
        assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
        // ssh exits: the prompt function's D (out of order with no C, ignored),
        // its own cwd, its A.
        t.process(&signed("D;0"));
        t.process(&osc7("/home/me"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
    }

    /// A successor whose handoff carried no nonce keeps the gate but accepts
    /// none of the shell's marks (posture `Degraded`), so the phase it carried
    /// is the predecessor's last word, not the shell's now: every report lands
    /// at once, the shell's own and a remote shell's alike. Once the shell
    /// signs with a key the host re-authorized, its marks are followed again,
    /// and a stray at its idle prompt is held once more.
    #[test]
    fn a_degraded_successor_holds_no_report() {
        use crate::terminal::ShellIntegrationPosture;
        const REKEY: [u8; 32] = [0xA7u8; 32];
        let signed_rekeyed = |mark: &str| {
            format!(
                "\x1b]133;{mark};id={}\x07",
                ShellIntegrationNonce(REKEY).to_hex()
            )
            .into_bytes()
        };

        let t = at_prompt();
        let mut cp = t.checkpoint_carry(0).expect("an idle prompt carries");
        // A handoff that lost the nonce (a keeper META carries none).
        cp.shell_integration_nonce = None;
        let mut s = Terminal::from_checkpoint(&cp);
        assert!(s.is_require_shell_integration_nonce(), "the gate stands");
        assert_eq!(
            s.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
        // The shell, still signing with the key the successor lost, runs `cd`.
        s.process(b"cd /srv\r\n");
        s.process(&signed("C"));
        s.process(&signed("D;0"));
        s.process(&osc7("/srv"));
        assert_eq!(s.current_working_directory(), Some("/srv"));
        s.process(&signed("A"));
        s.process(b"$ ");
        s.process(&signed("B"));
        s.process(b"ssh build\r\n");
        s.process(&signed("C"));
        s.process(&osc7("/srv/app"));
        assert_eq!(s.current_working_directory(), Some("/srv/app"));

        // The host re-keys; the shell takes the key at its next prompt.
        s.authorize_shell_integration_on_first_mark(REKEY);
        s.process(&osc7("/srv/app/logs"));
        assert_eq!(s.current_working_directory(), Some("/srv/app/logs"));
        s.process(&signed_rekeyed("D;0"));
        s.process(&osc7("/srv"));
        s.process(&signed_rekeyed("A"));
        assert_eq!(s.shell_integration_posture(), ShellIntegrationPosture::On);
        assert_eq!(s.current_working_directory(), Some("/srv"));
        s.process(b"$ ");
        s.process(&signed_rekeyed("B"));
        s.process(&osc7("/elsewhere"));
        assert_eq!(s.current_working_directory(), Some("/srv"));
    }

    /// A pending OSC 7 belongs to this engine's old prompt, not the shell state
    /// a public in-place restore installs. The ordinary report and fresh-adopt
    /// controls precede the old-lineage report's final refusal.
    #[test]
    fn an_in_place_restore_drops_the_previous_prompts_held_cwd() {
        // Without a restore, a held report still lands on this prompt's A.
        // A refused non-file URI does not erase that ordinary pending report.
        let mut ordinary = at_prompt();
        ordinary.process(&osc7("/same-prompt"));
        ordinary.process(b"\x1b]7;https://not-a-folder\x07");
        assert_eq!(ordinary.current_working_directory(), Some("/home/me"));
        assert!(cwd_events(&mut ordinary).is_empty());
        ordinary.process(&signed("A"));
        assert_eq!(ordinary.current_working_directory(), Some("/same-prompt"));
        assert_eq!(cwd_events(&mut ordinary), vec!["//host/same-prompt"]);

        // A different shell projection with an ordinary, completed cwd change.
        let mut source = at_prompt();
        source.process(&signed("C"));
        source.process(&signed("D;0"));
        source.process(&osc7("/adopted"));
        source.process(&signed("A"));
        source.process(&signed("B"));
        assert_eq!(source.current_working_directory(), Some("/adopted"));
        let checkpoint = source.checkpoint_carry(0).expect("a prompt carries");

        // A fresh adopt has no old pending report. Authorization is the host's
        // own binding, deliberately installed separately from buffer restore.
        let mut fresh = Terminal::from_checkpoint(&checkpoint);
        fresh.authorize_shell_integration(KEY);
        fresh.process(&signed("A"));
        assert_eq!(fresh.current_working_directory(), Some("/adopted"));
        assert!(cwd_events(&mut fresh).is_empty());

        let mut target = at_prompt();
        target.process(&osc7("/previous-prompt"));
        assert_eq!(target.current_working_directory(), Some("/home/me"));
        assert!(cwd_events(&mut target).is_empty(), "the old report is held");
        target.restore_checkpoint(&checkpoint);
        assert_eq!(target.current_working_directory(), Some("/adopted"));
        assert_eq!(target.shell_integration_nonce_in_use(), Some(KEY));
        target.process(&signed("A"));
        assert_eq!(
            target.current_working_directory(),
            Some("/adopted"),
            "an adopted shell must not commit the previous prompt's held cwd"
        );
        assert!(cwd_events(&mut target).is_empty());
        assert_eq!(
            target
                .shell
                .current_mark
                .as_ref()
                .expect("adopted prompt")
                .working_directory
                .as_deref(),
            Some("/adopted")
        );
    }
}
