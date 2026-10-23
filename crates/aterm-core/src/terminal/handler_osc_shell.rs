// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC shell-integration handlers: - OSC 133: FinalTerm/Terminal shell integration - OSC 633: VS Code shell integration
#![forbid(clippy::disallowed_methods)]

use super::handler::TerminalHandler;
use super::shell::{
    BlockState, COMMAND_MARKS_MAX, CommandMark, OUTPUT_BLOCKS_MAX, OutputBlock, ShellState,
};
use super::{MAX_COMMANDLINE_BYTES, MAX_CWD_PATH_BYTES};

/// The OSC 133/633 `B` parameter by which aterm's integration says the mark stands where the line editor's buffer starts (`ShellIntegrationState:: input_start_attested`).
pub(super) const INPUT_START_ATTESTATION: &[u8] = b"aterm_input=start";

/// The most startup roots one START record names: a fish's sysconf folder and
const MAX_START_ROOTS: usize = 32;

impl TerminalHandler<'_> {
    /// Parse shell integration OSC params into (command_char, absolute_row, col).
    #[inline]
    fn parse_shell_osc(&self, params: &[&[u8]]) -> Option<(char, u64, u16)> {
        let code = params.get(1).and_then(|p| std::str::from_utf8(p).ok())?;
        let cmd = code.chars().next()?;
        let cursor = self.grid.cursor();
        let row = self.grid.visible_to_absolute(cursor.row);
        Some((cmd, row, cursor.col))
    }

    /// Mark A (prompt start): open a mark, close the previous block, open a new
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

    /// Shell mark C: Command execution starting. Counted
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

    /// Dispatch the A/B/C/D marks OSC 133 and 633 share, enforcing A→B→C→D: A
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

    /// Whether the authenticated shell sits at its prompt by its own marks, so an OSC 7 it did not sign is not a program it started with a `C` ([`Self::handle_osc_7`] says what becomes of one).
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

    /// Gate OSC 133/633 on the capability nonce. With
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

    /// A shell prompt (OSC 133/633 `A`) on the alternate screen means the
    fn prompt_leaves_orphaned_alt_screen(&mut self, params: &[&[u8]]) {
        if params.get(1).and_then(|p| p.first()) != Some(&b'A') {
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

    /// Handle OSC 133 (FinalTerm) shell integration: A prompt start, B command input, C execution start, D finished.
    pub(super) fn handle_osc_133(&mut self, params: &[&[u8]]) {
        if !self.shell_nonce_gate_ok(133, params) {
            return;
        }
        self.prompt_leaves_orphaned_alt_screen(params);
        let Some((cmd, row, col)) = self.parse_shell_osc(params) else {
            return;
        };
        if self.dispatch_shell_mark(cmd, row, col, params) {
            let payload = match cmd {
                'D' => format!("D;exit={}", Self::parse_exit_code(params)),
                _ => format!("{cmd};row={row};col={col}"),
            };
            self.queue_osc_event(133, payload);
        }
    }

    /// Handle OSC 633, VS Code shell integration (<https://code.visualstudio.com/docs/terminal/shell-integration>).
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
                if value.len() > MAX_CWD_PATH_BYTES {
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

    /// OSC 633 `P;AtermIntegration=<rev>`: which integration body the shell runs
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

    /// OSC 633 `P` keys of the warm spare (`docs/DESIGN-warm-spare-2026-10-16.md`
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
                let ok = !value.is_empty() && value.len() <= MAX_CWD_PATH_BYTES;
                if ok {
                    spare.pending_rc = Some(Self::unescape_vscode_string(value));
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

    /// A START record's other startup roots (`AtermStartRoots=`): absolute
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

    /// Unescape VS Code's command-line format: `\\` is a backslash and `\xAB`
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

impl super::Terminal {
    /// Bytes the console host writes after `?1049l` (`set_host_resyncs_alt_screen`) to carry the shell prompt and cursor onto the primary screen.
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
            if let Some(text) = alt.row_ansi_text_screen(r) {
                out.push_str(&text);
            }
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
mod prompt_on_alt_screen_tests {
    use crate::terminal::Terminal;

    /// Eight lines of history, a marked `less` command, and the pager stuck on
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
        assert_eq!(mark.prompt_start_row, 9);
        assert_eq!(mark.prompt_start_col, 0);
        assert_eq!(mark.command_start_col, Some(4));
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
        term.process(b"echo hi\r\nhi\r\n");
        assert_eq!(term.alt_archive().texts(), rows);
    }

    #[test]
    fn rows_painted_in_the_same_read_as_the_prompt_are_archived() {
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

    #[test]
    fn under_a_resyncing_host_the_prompt_requests_the_host_leave_once() {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.modes().alternate_screen, "no local leave");
        assert!(term.take_alt_screen_resync_request(), "one request");
        assert!(!term.take_alt_screen_resync_request(), "taken");
        term.process(PROMPT_AFTER_KILL);
        assert!(!term.take_alt_screen_resync_request());
        assert_eq!(
            term.grid().scrollback_lines(),
            0,
            "nothing scrolled locally"
        );

        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
        let rows = screen(&term);
        assert_eq!(rows[3], "$ less file");
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (4, 0));
    }

    #[test]
    fn the_hosts_switch_out_of_a_dead_screen_releases_the_dead_apps_modes() {
        use crate::terminal::{MouseEncoding, MouseMode};
        const DEAD_APP_MODES: &[u8] = b"\x1b[2;4r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=";
        let released = |term: &Terminal, why: &str| {
            assert!(!term.modes().alternate_screen, "{why}");
            let modes = term.modes();
            assert_eq!(modes.mouse_mode, MouseMode::None, "{why}");
            assert_eq!(modes.mouse_encoding, MouseEncoding::X10, "{why}");
            assert!(!modes.application_cursor_keys, "{why}");
            assert!(!modes.application_keypad, "{why}");
            let region = term.grid().scroll_region();
            assert_eq!((region.top, region.bottom), (0, 4), "{why}");
        };

        let mut local = stuck_pager();
        local.process(DEAD_APP_MODES);
        local.process(PROMPT_AFTER_KILL);
        released(&local, "control: the local leave");

        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(DEAD_APP_MODES);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.take_alt_screen_resync_request());
        assert!(term.modes().alternate_screen, "the host is asked instead");
        term.process(b"\x1b[?1049l");
        released(&term, "the host's forwarded switch");

        for host_resyncs in [true, false] {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(host_resyncs);
            term.process(b"\x1b[?1000h\x1b[?1h\x1b[?1049l");
            assert!(!term.modes().alternate_screen);
            assert_eq!(term.modes().mouse_mode, MouseMode::Normal, "{host_resyncs}");
            assert!(term.modes().application_cursor_keys, "{host_resyncs}");
        }

        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        term.process(b"\x1b[?1049h\x1b[?1000hnext app");
        term.process(b"\x1b[?1049l");
        assert_eq!(term.modes().mouse_mode, MouseMode::Normal);
    }

    #[test]
    fn the_prompt_that_proves_an_app_dead_turns_off_its_paste_and_reports() {
        const VIM_MODES: &[u8] = b"\x1b[?2004h\x1b[?2048h\x1b[?2031h";
        for host_resyncs in [false, true] {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(host_resyncs);
            term.process(VIM_MODES);
            let _ = term.take_response();
            term.process(PROMPT_AFTER_KILL);
            let m = term.modes();
            assert!(!m.bracketed_paste, "host_resyncs={host_resyncs}");
            assert!(!m.in_band_size_reports, "host_resyncs={host_resyncs}");
            assert!(!m.report_color_scheme, "host_resyncs={host_resyncs}");
            term.resize(5, 30);
            assert_eq!(
                term.take_response(),
                None,
                "no size report typed at the prompt (host_resyncs={host_resyncs})"
            );
        }

        let mut term = stuck_pager();
        term.process(b"\x1b[?1049l");
        term.process(VIM_MODES);
        term.process(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07");
        assert!(term.modes().bracketed_paste);
        assert!(term.modes().in_band_size_reports);
    }

    #[test]
    fn the_prompt_that_proves_an_app_dead_releases_its_input_at_once() {
        use crate::terminal::{MouseEncoding, MouseMode};
        const DEAD_APP_MODES: &[u8] = b"\x1b[2;4r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=\x1b[?2048h";
        for host_resyncs in [true, false] {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(host_resyncs);
            term.process(DEAD_APP_MODES);
            let _ = term.take_response();
            term.set_cell_pixel_size(9, 23);
            term.process(PROMPT_AFTER_KILL);
            let m = term.modes();
            assert_eq!(m.mouse_mode, MouseMode::None, "{host_resyncs}");
            assert_eq!(m.mouse_encoding, MouseEncoding::X10, "{host_resyncs}");
            assert!(!m.application_cursor_keys, "{host_resyncs}");
            assert!(!m.application_keypad, "{host_resyncs}");
            assert_eq!(
                term.take_reports(),
                None,
                "no report for the dead app reaches the shell (host_resyncs={host_resyncs})"
            );
        }

        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(DEAD_APP_MODES);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.modes().alternate_screen, "the host is asked instead");
        let region = term.grid().scroll_region();
        assert_eq!((region.top, region.bottom), (1, 3), "the dead screen's own");
        term.process(b"\x1b[?1049l");
        let region = term.grid().scroll_region();
        assert_eq!(
            (region.top, region.bottom),
            (0, 4),
            "released with the screen"
        );
    }

    #[test]
    fn a_shells_own_bracketed_paste_for_the_prompt_survives_the_dead_apps_release() {
        let mut term = stuck_pager();
        term.process(b"\x1b[?2004h\x1b[?2048h");
        term.process(b"\r\n\x1b]133;D;-1\x07\x1b[?2004h\x1b]133;A\x07> \x1b]133;B\x07");
        assert!(term.modes().bracketed_paste, "fish's own, set after its D");
        assert!(!term.modes().in_band_size_reports, "no shell asks for 2048");

        let mut term = stuck_pager();
        term.process(b"\x1b[?2004h");
        term.process(b"\r\n\x1b]133;D;-1\x07\x1b]133;A\x07");
        assert!(!term.modes().bracketed_paste, "vim's, released at the A");
        term.process(b"\x1b[?2004h$ \x1b]133;B\x07");
        assert!(term.modes().bracketed_paste, "readline's own, after the A");
    }

    #[test]
    fn expecting_the_hosts_leave_marks_the_screen_dead() {
        use crate::terminal::MouseMode;
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(b"\x1b[?1000h\x1b[?1h");
        assert!(term.expect_host_alt_screen_leave());
        term.process(PROMPT_AFTER_KILL);
        assert!(
            !term.take_alt_screen_resync_request(),
            "the verb's client is the one request for this screen"
        );
        term.process(b"\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
        assert_eq!(term.modes().mouse_mode, MouseMode::None);
        assert!(!term.modes().application_cursor_keys);

        assert!(!term.expect_host_alt_screen_leave(), "the primary screen");
        term.process(b"\x1b[?1049h\x1b[?1000h\x1b[?1049l");
        assert_eq!(term.modes().mouse_mode, MouseMode::Normal);

        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(b"\x1b[?1000h");
        assert!(term.expect_host_alt_screen_leave());
        term.set_host_resyncs_alt_screen(false);
        term.process(b"\x1b[?1049l");
        assert_eq!(term.modes().mouse_mode, MouseMode::Normal);
    }

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
    use super::MAX_START_ROOTS;
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
        let mut open = Terminal::new(4, 40);
        open.process(&p("AtermSpare=parked", ""));
        record(&mut open, "/h", "pre:0,s:0,j:0,r:0", "spare");
        assert!(!open.take_spare_parked());
        assert_eq!(open.take_start_mark(), None);

        let mut t = gated();
        t.process(&p("AtermSpare=parked", ""));
        t.process(&p("AtermSpare=parked", ";id=00"));
        t.process(&p("AtermStart=spare", ";id=00"));
        assert!(
            !t.take_spare_parked(),
            "an unsigned parked mark is no spare"
        );
        assert_eq!(t.take_start_mark(), None);

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

        t.process(&p("AtermSpare=parked", &id));
        assert!(t.take_spare_parked());
        assert!(!t.take_spare_parked(), "taken once");

        record(&mut t, "/Users//a\\x20b\\x3bc", "pre:2,s:1,j:-,r:1", "spare");
        let want = StartMark {
            kind: StartKind::Spare,
            rc_dir: Some("/Users//a b;c".into()),
            roots: None,
            flags: Some(StartFlags {
                pre: 2,
                sched: 1,
                jobs: None,
                periodic: true,
            }),
        };
        assert_eq!(t.take_start_mark(), Some(want));
        assert_eq!(t.take_start_mark(), None, "taken once");

        record(&mut t, "/other", "pre:0,s:0,j:0,r:0", "cold");
        assert_eq!(t.take_start_mark(), None);
        assert_eq!(t.spare_marks_refused(), 7, "a later record is not refused");
    }

    /// fish's startup roots (`AtermStartRoots=`): escaped absolute folders
    #[test]
    fn a_start_record_carries_the_startup_roots_it_names() {
        let id = id();
        let mut t = gated();
        let too_many = format!(
            "AtermStartRoots={}",
            ["/v"; MAX_START_ROOTS + 1].join("\\x00")
        );
        for bad in [
            "AtermStartRoots=",
            "AtermStartRoots=etc/fish",
            "AtermStartRoots=/etc/fish\\x00",
            "AtermStartRoots=/etc/fish\\x00\\x00/v",
            too_many.as_str(),
        ] {
            t.process(&p(bad, &id));
        }
        assert_eq!(t.spare_marks_refused(), 5);
        record(&mut t, "/c", "pre:0,s:0,j:0,r:0", "cold");
        assert_eq!(t.take_start_mark().map(|m| m.roots), Some(None));

        let mut t = gated();
        t.process(&p(
            "AtermStartRoots=/opt/homebrew/etc/fish\\x00/Users//a\\x20b/.local/share/fish/vendor_conf.d\\x00/x\\x3by\\\\z/fish/vendor_conf.d",
            &id,
        ));
        record(&mut t, "/c", "pre:0,s:0,j:0,r:0", "cold");
        assert_eq!(
            t.take_start_mark().and_then(|m| m.roots),
            Some(vec![
                "/opt/homebrew/etc/fish".to_owned(),
                "/Users//a b/.local/share/fish/vendor_conf.d".to_owned(),
                "/x;y\\z/fish/vendor_conf.d".to_owned(),
            ])
        );
        assert_eq!(t.spare_marks_refused(), 0);
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
        // host{path}\x07").into_bytes()
        format!("\x1b]7;file://host{path}\x07").into_bytes()
    }

    /// A gated terminal whose shell reported `/home/me` and drew its prompt,
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

    /// The shell's own report as every integration sends it: the `AtermCwd`
    fn own_report(path: &str) -> Vec<u8> {
        format!(
            "\x1b]633;P;AtermCwd=file://host{path};id={}\x07\x1b]7;file://host{path}\x07",
            ShellIntegrationNonce(KEY).to_hex()
        )
        .into_bytes()
    }

    /// [`at_prompt`] for a shell that signs its own reports.
    fn signing_at_prompt() -> Terminal {
        let mut t = Terminal::new(6, 60);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        for line in ["", "true\r\n"] {
            if !line.is_empty() {
                t.process(line.as_bytes());
                t.process(&signed("C"));
                t.process(&signed("D;0"));
            }
            t.process(&own_report("/home/me"));
            t.process(&signed("A"));
            t.process(b"$ ");
            t.process(&signed("B"));
        }
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
    #[test]
    fn a_stray_report_at_the_idle_prompt_sets_no_folder() {
        let mut t = at_prompt();
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert_eq!(cwd_events(&mut t), Vec::<String>::new());
        t.process(&osc7("/home/me"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert_eq!(cwd_events(&mut t), vec!["//host/home/me".to_owned()]);
        let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
        assert_eq!(mark.working_directory.as_deref(), Some("/home/me"));

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
    #[test]
    fn a_report_while_a_command_runs_or_after_it_ends_lands_at_once() {
        let mut t = at_prompt();
        t.process(b"ssh build\r\n");
        t.process(&signed("C"));
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
        t.process(&signed("D;0"));
        t.process(&osc7("/home/me"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
    }

    /// With no authenticated shell (the nonce gate off), marks are anyone's, so
    #[test]
    fn without_the_nonce_gate_every_report_lands_at_once() {
        let mut t = Terminal::new(6, 60);
        t.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/elsewhere"));
    }

    /// The shell's signed `633;P;Cwd=` is its own word: it lands at once at the
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
    #[test]
    fn a_shell_that_marks_no_command_holds_no_report() {
        let mut t = gated_at_first_prompt();
        t.process(b"ssh build\r\n");
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        t.process(&osc7("/srv/app/logs"));
        assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
        t.process(&signed("D;0"));
        t.process(&osc7("/home/me"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
    }

    /// A successor whose handoff carried no nonce keeps the gate but accepts
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
        cp.shell_integration_nonce = None;
        let mut s = Terminal::from_checkpoint(&cp);
        assert!(s.is_require_shell_integration_nonce(), "the gate stands");
        assert_eq!(
            s.shell_integration_posture(),
            ShellIntegrationPosture::Degraded
        );
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
    #[test]
    fn an_in_place_restore_drops_the_previous_prompts_held_cwd() {
        let mut ordinary = at_prompt();
        ordinary.process(&osc7("/same-prompt"));
        ordinary.process(b"\x1b]7;https://not-a-folder\x07");
        assert_eq!(ordinary.current_working_directory(), Some("/home/me"));
        assert!(cwd_events(&mut ordinary).is_empty());
        ordinary.process(&signed("A"));
        assert_eq!(ordinary.current_working_directory(), Some("/same-prompt"));
        assert_eq!(cwd_events(&mut ordinary), vec!["//host/same-prompt"]);

        let mut source = at_prompt();
        source.process(&signed("C"));
        source.process(&signed("D;0"));
        source.process(&osc7("/adopted"));
        source.process(&signed("A"));
        source.process(&signed("B"));
        assert_eq!(source.current_working_directory(), Some("/adopted"));
        let checkpoint = source.checkpoint_carry(0).expect("a prompt carries");

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

    /// The shell's own report (the `AtermCwd` it signs, then its OSC 7) is its
    #[test]
    fn the_shells_own_report_lands_at_once_with_no_nonce_in_its_uri() {
        let mut t = signing_at_prompt();
        assert!(t.shell.signs_cwd);
        t.process(&own_report("/home/me/src"));
        assert_eq!(t.current_working_directory(), Some("/home/me/src"));
        assert_eq!(cwd_events(&mut t), vec!["//host/home/me/src".to_owned()]);
        t.process(
            format!(
                "\x1b]633;P;AtermCwd=file://host/forged;id={}\x07",
                ShellIntegrationNonce([0x11; 32]).to_hex()
            )
            .as_bytes(),
        );
        assert_eq!(t.current_working_directory(), Some("/home/me/src"));
        t.process(&own_report("/home/me/src"));
        assert!(cwd_events(&mut t).is_empty(), "unchanged");
        let mut t = gated_at_first_prompt();
        while t.take_osc_event().is_some() {}
        t.process(&own_report("/srv"));
        assert_eq!(t.current_working_directory(), Some("/srv"));
        assert_eq!(cwd_events(&mut t), vec!["//host/srv".to_owned()]);
        let mut t = Terminal::new(6, 60);
        t.process(b"\x1b]7;file://host/a;id=b\x07");
        assert_eq!(t.current_working_directory(), Some("/a;id=b"));
    }

    /// A stray between the shell's report and its `A`: the
    #[test]
    fn a_stray_between_the_shells_signed_report_and_its_prompt_sets_nothing() {
        let mut t = signing_at_prompt();
        t.process(b"cd src\r\n");
        t.process(&signed("C"));
        t.process(&signed("D;0"));
        t.process(&own_report("/home/me/src"));
        t.process(&osc7("/elsewhere"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me/src"));
        let mark = t.shell.current_mark.as_ref().expect("the prompt's mark");
        assert_eq!(mark.working_directory.as_deref(), Some("/home/me/src"));
        assert_eq!(cwd_events(&mut t), vec!["//host/home/me/src".to_owned()]);

        let mut t = signing_at_prompt();
        t.process(b"\r\n");
        t.process(&own_report("/home/me"));
        t.process(&osc7("/elsewhere"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert!(cwd_events(&mut t).is_empty(), "nothing changed");

        let mut t = at_prompt();
        t.process(b"\r\n");
        t.process(&osc7("/home/me"));
        t.process(&osc7("/elsewhere"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/elsewhere"));
    }

    /// A program a key binding starts: it runs with no `C`
    #[test]
    fn a_key_bound_programs_report_lands_while_it_holds_the_foreground() {
        let mut t = signing_at_prompt();
        t.set_foreground_job(true);
        t.process(b"\x1b]133;A\x07remote$ ");
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        assert_eq!(t.reported_cwd_host(), Some("host"));
        t.process(&osc7("/srv/app/logs"));
        assert_eq!(t.current_working_directory(), Some("/srv/app/logs"));
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert_eq!(
            cwd_events(&mut t),
            vec![
                "//host/srv/app".to_owned(),
                "//host/srv/app/logs".to_owned(),
                "//host/home/me".to_owned()
            ]
        );
        t.set_foreground_job(false);
        assert!(cwd_events(&mut t).is_empty());
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));

        let mut t = signing_at_prompt();
        t.process(b"ssh build\r\n");
        t.process(&signed("C"));
        t.set_foreground_job(true);
        t.process(&osc7("/srv/app"));
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
    }

    /// A program the shell keeps in its own process group (PowerShell runs
    #[test]
    fn a_program_the_probe_sees_since_the_prompt_reports_its_cwd() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
        let started = Arc::new(AtomicU64::new(0));
        let asked = Arc::new(AtomicU64::new(0));
        let mut t = signing_at_prompt();
        let (child, count) = (started.clone(), asked.clone());
        t.set_program_probe(Some(Box::new(move |since| {
            count.fetch_add(1, SeqCst);
            let at = child.load(SeqCst);
            at != 0 && at >= since
        })));
        started.store(1, SeqCst);
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        started.store(u64::MAX, SeqCst);
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        started.store(0, SeqCst);
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        let before = asked.load(SeqCst);
        t.set_foreground_job(false);
        t.process(&own_report("/home/me/src"));
        assert_eq!(asked.load(SeqCst), before);
        t.process(b"true\r\n");
        t.process(&signed("C"));
        t.process(&signed("D;0"));
        started.store(u64::MAX, SeqCst);
        t.process(&osc7("/srv/logs"));
        assert_eq!(t.current_working_directory(), Some("/srv/logs"));
    }

    /// A shell that stops sending `C` (PowerShell after `Import-Module
    #[test]
    fn a_shell_that_stops_marking_commands_holds_no_report_once_it_says_so() {
        let mut t = signing_at_prompt();
        t.process(b"ssh build\r\n");
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/home/me"), "the first");
        t.process(&signed("D;0"));
        assert!(t.shell.unmarked_run);
        t.process(&own_report("/home/me"));
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed("B"));
        t.process(b"ssh build\r\n");
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        t.process(&signed("D;0"));
        t.process(&own_report("/home/me"));
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed("B"));
        t.process(b"true\r\n");
        t.process(&signed("C"));
        assert!(!t.shell.unmarked_run);
        t.process(&signed("D;0"));
        t.process(&own_report("/home/me"));
        t.process(&signed("A"));
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));

        let mut t = signing_at_prompt();
        t.process(b"ssh build\r\n");
        t.set_foreground_job(true);
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
    }

    /// What the prompt left pending rides a checkpoint: a
    #[test]
    fn what_the_prompt_left_pending_survives_a_checkpoint() {
        fn carried(t: &Terminal) -> Vec<Terminal> {
            let cp = t.checkpoint_carry(0).expect("a prompt carries");
            let key = cp
                .shell_integration_nonce
                .expect("an authenticated prompt carries its key")
                .0;
            #[allow(unused_mut)]
            let mut out = vec![Terminal::from_checkpoint(&cp)];
            #[cfg(feature = "serde")]
            {
                let meta = crate::terminal::CheckpointMeta::from_checkpoint(&cp);
                out.push(Terminal::from_checkpoint(
                    &meta.into_checkpoint(cp.grid.clone(), cp.alt_grid.clone()),
                ));
            }
            for s in &mut out {
                s.authorize_shell_integration(key);
            }
            out
        }

        let mut held = at_prompt();
        held.process(b"\r\n");
        held.process(&osc7("/home/me/src"));
        for mut s in carried(&held) {
            s.process(&signed("A"));
            assert_eq!(s.current_working_directory(), Some("/home/me/src"));
        }

        let mut signing = signing_at_prompt();
        signing.process(b"cd src\r\n");
        signing.process(&signed("C"));
        signing.process(&signed("D;0"));
        for mut s in carried(&signing) {
            s.process(&osc7("/elsewhere"));
            assert_eq!(s.current_working_directory(), Some("/home/me"));
            assert!(cwd_events(&mut s).is_empty());
            s.process(&own_report("/home/me/src"));
            s.process(&signed("A"));
            assert_eq!(s.current_working_directory(), Some("/home/me/src"));
        }

        const REKEY: [u8; 32] = [0xA7; 32];
        let hex = ShellIntegrationNonce(REKEY).to_hex();
        let mut rekeying = signing_at_prompt();
        rekeying.authorize_shell_integration_on_first_mark(REKEY);
        rekeying
            .process(format!("\x1b]633;P;AtermCwd=file://host/home/me;id={hex}\x07").as_bytes());
        let mut continuations = carried(&rekeying);
        continuations.push(rekeying);
        for mut s in continuations {
            s.process(&osc7("/srv/app"));
            assert_eq!(s.current_working_directory(), Some("/srv/app"));
            s.process(format!("\x1b]133;A;id={hex}\x07").as_bytes());
            s.process(&osc7("/elsewhere"));
            assert_eq!(s.current_working_directory(), Some("/srv/app"));
        }

        let mut unmarked = signing_at_prompt();
        unmarked.process(&signed("D;0"));
        unmarked.process(&own_report("/home/me"));
        unmarked.process(&signed("A"));
        unmarked.process(&signed("B"));
        for mut s in carried(&unmarked) {
            s.process(&osc7("/srv/app"));
            assert_eq!(s.current_working_directory(), Some("/srv/app"));
        }

        let mut job = signing_at_prompt();
        job.set_foreground_job(true);
        job.process(&osc7("/srv/app"));
        for mut s in carried(&job) {
            assert_eq!(s.current_working_directory(), Some("/srv/app"));
            s.set_foreground_job(true);
            s.set_foreground_job(false);
            assert_eq!(s.current_working_directory(), Some("/home/me"));
        }
    }
}

#[cfg(test)]
mod dead_screen_carry_tests {
    use crate::terminal::Terminal;

    const ROWS: u16 = 6;

    struct Tab {
        ours: Terminal,
        host: Terminal,
    }

    impl Tab {
        /// A tab whose primary screen used `used` rows (`out 0` ...), then an app
        fn killed(used: u16, app: &[u8], prompt: &[u8]) -> Self {
            let mut ours = Terminal::new(ROWS, 40);
            ours.set_host_resyncs_alt_screen(true);
            let mut tab = Self {
                ours,
                host: Terminal::new(ROWS, 40),
            };
            for i in 0..used {
                tab.both(format!("out {i}\r\n").as_bytes());
            }
            tab.both(b"\x1b[?1049h\x1b[H\x1b[2J");
            tab.both(app);
            tab.both(b"\x1b]133;D;-1\x07\x1b]133;A\x07");
            tab.both(prompt);
            tab.both(b"\x1b]133;B\x07");
            tab
        }

        /// Bytes both sides draw: ours with the marks, the host without.
        fn both(&mut self, bytes: &[u8]) {
            self.ours.process(bytes);
            self.host.process(&without_marks(bytes));
        }

        /// The host's leave: its client writes `?1049l` and, with `carry`, what
        fn leave(&mut self, carry: bool) -> bool {
            let text = self.ours.dead_alt_screen_carry();
            let mut bytes = b"\x1b[?1049l".to_vec();
            if carry && let Some(text) = &text {
                bytes.extend_from_slice(text.as_bytes());
            }
            self.both(&bytes);
            text.is_some()
        }

        fn rows(t: &Terminal) -> Vec<String> {
            (0..usize::from(ROWS))
                .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
                .collect()
        }

        fn history(t: &Terminal) -> Vec<String> {
            let grid = t.grid();
            (0..grid.scrollback_lines())
                .map(|i| {
                    grid.get_history_line(i)
                        .as_ref()
                        .and_then(|l| l.as_str())
                        .map(|s| s.trim_end().to_string())
                        .unwrap_or_default()
                })
                .collect()
        }

        /// Both screens show the same rows, history and cursor, off the
        fn in_step(&self) -> Vec<String> {
            let (a, b) = (self.ours.cursor(), self.host.cursor());
            assert_eq!(Self::rows(&self.ours), Self::rows(&self.host));
            assert_eq!(Self::history(&self.ours), Self::history(&self.host));
            assert_eq!((a.row, a.col), (b.row, b.col), "cursors");
            assert!(!self.ours.is_alternate_screen() && !self.host.is_alternate_screen());
            Self::rows(&self.ours)
        }

        fn cursor(&self) -> (u16, u16) {
            let c = self.ours.cursor();
            (c.row, c.col)
        }
    }

    /// The bytes without their OSC 133 marks (`ESC ] 133 ; ... BEL`).
    fn without_marks(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"\x1b]133;") {
                i += bytes[i..]
                    .iter()
                    .position(|&b| b == 0x07)
                    .map_or(bytes.len(), |p| p + 1);
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    #[test]
    fn the_prompt_comes_back_at_the_row_and_column_it_was_drawn_at() {
        let mut tab = Tab::killed(3, b"line 1 of the file\r\nline 2\x1b[H", b"PS> ");
        assert!(tab.leave(true), "a prompt to carry");
        let rows = tab.in_step();
        assert_eq!(rows, ["PS>", "", "", "", "", ""]);
        assert_eq!(tab.cursor(), (0, 4), "where the line editor anchored");
        assert_eq!(Tab::history(&tab.ours), ["out 0", "out 1", "out 2"]);
        tab.both(b"\x1b[1;5Hecho after");
        assert_eq!(Tab::rows(&tab.ours)[0], "PS> echo after");
    }

    #[test]
    fn without_the_carry_the_anchored_redraw_overwrites_an_earlier_row() {
        let mut tab = Tab::killed(3, b"line 1 of the file\r\nline 2\x1b[H", b"PS> ");
        tab.leave(false);
        tab.in_step();
        assert_eq!(tab.cursor(), (3, 0));
        tab.both(b"\x1b[1;5Hecho after");
        assert_eq!(
            Tab::rows(&tab.ours)[..3],
            ["out echo after", "out 1", "out 2"]
        );
    }

    #[test]
    fn a_prompt_below_the_used_rows_scrolls_nothing() {
        let mut tab = Tab::killed(2, b"a\r\nb\r\nc\r\nd\x1b[5;1H", b"PS> ");
        assert!(tab.leave(true));
        let rows = tab.in_step();
        assert_eq!(rows, ["out 0", "out 1", "", "", "PS>", ""]);
        assert_eq!(tab.cursor(), (4, 4));
        assert!(Tab::history(&tab.ours).is_empty());
    }

    #[test]
    fn the_carry_is_the_prompt_alone_and_all_of_it() {
        let mut tab = Tab::killed(1, b"\x1b[2;1Hdead app text that runs on\x1b[2;10H", b"PS> ");
        assert!(tab.leave(true));
        assert_eq!(tab.in_step(), ["out 0", "         PS>", "", "", "", ""]);
        assert_eq!(tab.cursor(), (1, 13));

        let mut tab = Tab::killed(4, b"x\r\ny\r\nz\x1b[2;1H", b"~\\dir\r\nPS> ");
        assert!(tab.leave(true));
        assert_eq!(tab.in_step(), ["out 3", "~\\dir", "PS>", "", "", ""]);
        assert_eq!(tab.cursor(), (2, 4));
        assert_eq!(Tab::history(&tab.ours), ["out 0", "out 1", "out 2"]);
    }

    #[test]
    fn the_prompt_keeps_its_colours() {
        let mut tab = Tab::killed(1, b"\x1b[H", b"\x1b[32mPS\x1b[0m> ");
        assert!(tab.leave(true));
        tab.in_step();
        let text = tab.ours.grid().row_ansi_text_screen(0).unwrap_or_default();
        assert!(text.contains("\x1b[0;32mPS"), "{text:?}");
    }

    #[test]
    fn the_prompt_comes_back_in_the_pen_the_leave_restores() {
        let mut tab = Tab::killed(1, b"\x1b[0;1;7mruler\x1b[H", b"PS> ");
        assert!(tab.leave(true));
        assert_eq!(tab.in_step()[0], "PS>");
        for t in [&tab.ours, &tab.host] {
            let text = t.grid().row_ansi_text_screen(0).unwrap_or_default();
            assert!(!text.contains('\x1b'), "{text:?}");
        }
    }

    #[test]
    fn nothing_drawn_after_the_mark_carries_nothing() {
        let tab = Tab::killed(2, b"pager\x1b[H", b"");
        assert_eq!(tab.ours.dead_alt_screen_carry(), None);

        let mut term = Terminal::new(ROWS, 40);
        term.set_host_resyncs_alt_screen(true);
        term.process(b"\x1b[?1049hpager\x1b[H");
        assert_eq!(term.dead_alt_screen_carry(), None, "no prompt on it");
        term.process(b"\x1b]133;A\x07PS> ");
        assert!(term.dead_alt_screen_carry().is_some());
        term.process(b"\x1b[?1049l");
        assert_eq!(term.dead_alt_screen_carry(), None, "left");

        let mut local = Terminal::new(ROWS, 40);
        local.process(b"\x1b[?1049hpager\x1b[H\x1b]133;A\x07PS> ");
        assert!(!local.is_alternate_screen(), "the local leave");
        assert_eq!(local.dead_alt_screen_carry(), None);
    }
}
