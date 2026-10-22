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

/// The OSC 133/633 `B` parameter by which aterm's integration says the mark
/// stands where the line editor's buffer starts (`ShellIntegrationState::
/// input_start_attested`).
pub(super) const INPUT_START_ATTESTATION: &[u8] = b"aterm_input=start";

/// The most startup roots one START record names: a fish's sysconf folder and
/// its vendor conf.d folders (one per `$XDG_DATA_DIRS` entry, its user data
/// folder's and its build's). A record naming more names none.
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
    #[inline]
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
                self.shell.phase_stale = false;
                // A new prompt: its `B` has yet to say where its buffer starts.
                self.shell.input_start_attested = false;
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
                // Where the buffer starts, as the shell vouches for it: only a
                // `B` that says so (`aterm_input=start`) is the buffer's start
                // (`Terminal::input_region_start`). zsh sends its `B` from
                // zle-line-init, after zle drew the buffer, so a line pre-filled
                // from its buffer stack puts the mark after the text.
                self.shell.input_start_attested = params
                    .get(2..)
                    .is_some_and(|p| p.contains(&INPUT_START_ATTESTATION));
                self.shell_command_input_start(row, col);
            }
            'C' => {
                if self.shell.phase != ShellIntegrationPhase::CommandStart {
                    return true;
                }
                // A report held at the prompt was not the shell's: the shell
                // starts a command instead of redrawing its prompt. And it
                // marks what it runs.
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

    /// Whether the authenticated shell sits at its prompt by its own marks, so
    /// an OSC 7 it did not sign is not a program it started with a `C`
    /// ([`Self::handle_osc_7`] says what becomes of one). Only where the phase
    /// is the shell's own word that it runs nothing:
    ///
    /// - the nonce gate is on and its nonce is in use (`status
    ///   integration=on`), so the phase follows the shell's marks alone (a
    ///   remote shell's or a program's are dropped) and the shell's marks
    ///   arrive. A successor whose handoff lost the nonce, or a re-key the
    ///   shell has not taken, drops them all, and the phase it shows is stale
    ///   until the shell's first `A` with the key it takes (`phase_stale`).
    /// - the shell has marked a command it ran with `C` (`started_seq`) and has
    ///   not since finished one it did not mark (`unmarked_run`), so it is one
    ///   that marks what it runs. PowerShell without PSReadLine never sends
    ///   `C`, and a program it starts speaks while the phase still reads its
    ///   prompt; one whose readline shim was replaced says so with its next
    ///   `D`.
    /// - the phase is its prompt: after its `A` and before its `C`, and for a
    ///   shell that signs its own report also from its `D` on (the report it
    ///   signs before its `A` names its cwd; anything else there is not its).
    ///
    /// A program the shell started with `C` speaks after it and lands at once,
    /// a remote shell over ssh, a nested shell or a script's `cd` alike. A
    /// program a key binding starts runs with no `C`; the host says it holds
    /// the foreground (`ShellIntegrationState::foreground_job`).
    pub(super) fn shell_sits_at_prompt(&self) -> bool {
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
    /// [`super::types::TerminalModes::require_shell_integration_nonce`] set, a
    /// mark passes only with an `id=<64-hex>` matching the host-authorized nonce
    /// (others are dropped and counted); an attached policy engine's Deny drops it
    /// first. With the bit clear (default) every mark passes. Returns whether to
    /// dispatch.
    #[inline]
    fn shell_nonce_gate_ok(&mut self, command: u32, params: &[&[u8]]) -> bool {
        if !self.modes.require_shell_integration_nonce {
            return true;
        }
        let gate = self.policy.shell_integration_gate(command, params);
        // A key not in use yet (a re-key, a successor's after a handoff that
        // lost the nonce): the marks signed with the key before it were
        // dropped, so the phase this mark meets is not the shell's last word.
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
    /// forwards leaves ours from the same place in the stream. The prompt marks
    /// the screen dead, so that switch also releases the modes the dead app left
    /// on, as the local leave does (`TerminalHandler::take_dead_alt_screen`). Where
    /// this `A` put the prompt on the dead screen is kept, so the host's leave can
    /// carry the prompt along (`Terminal::dead_alt_screen_carry`).
    ///
    /// What turns the person's own input into typed junk goes at the prompt
    /// itself on either path: the app's input modes, the reports no shell asks
    /// for and the ones still queued for it (`release_dead_app_input`), and its
    /// bracketed paste (`release_dead_app_paste`). The host's switch comes a
    /// client's start later (about 12 ms), and can arrive after a line editor's
    /// own `?2004h`, too late to tell the two apart. On either screen a
    /// DEC 2048 size report still held for the program that asked
    /// (`TerminalHandler::hold_size_report`) is dropped: the shell reads now.
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
    /// `AtermIntegration=`, `AtermCwd=`). Nonce-gated like OSC 133.
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
                // The OSC 7 cap, applied before the count-capped (not byte-capped)
                // mark and block fields.
                if value.len() > MAX_CWD_PATH_BYTES {
                    return;
                }
                // The shell's own word: an OSC 7 held at its prompt does not
                // override it at the next `A`, nor does a job's end restore
                // the cwd before it.
                self.shell.held_cwd = None;
                self.shell.cwd_before_job = None;
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
    #[inline]
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
    /// `AtermStartRoots=` (fish's), `AtermStartFlags=` and
    /// `AtermStart=cold|spare`, which completes it.
    /// Recorded only under the nonce gate, like [`Self::record_integration_rev`],
    /// so a program's output cannot forge a spare's readiness. Parsed strictly;
    /// a malformed value is ignored and counted. Only the first record is kept.
    /// Returns whether `key` is one of these.
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
    /// paths, escaped as the rc folder is (a raw space too) and joined by the
    /// escape `\x00` (no path holds a NUL), at most [`MAX_START_ROOTS`] of
    /// them, each within [`MAX_CWD_PATH_BYTES`]; anything else names none. An
    /// empty or relative one is refused rather than dropped: the host stamps
    /// every folder the shell read, and a folder relative to the shell's own is
    /// no fixed one.
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

impl super::Terminal {
    /// THE CARRY: what the console host should write right after its `?1049l` when it
    /// leaves the alternate screen a dead app left up (`set_host_resyncs_alt_screen`), so
    /// the prompt the shell already drew on that screen comes along, at the same rows and
    /// columns, with the cursor where it was. `None` when there is nothing to carry: no
    /// host leave is pending, no prompt was marked on this screen, or the shell has drawn
    /// nothing since its `A` (its prompt is still to come and lands on the primary screen
    /// by itself, as Git Bash's does).
    ///
    /// WHY. The `A` that proves the app dead arrives with the prompt it starts, so the host
    /// is asked only after the shell drew that prompt on the dead screen, and a line editor
    /// that anchors at the cursor it read when its read began keeps that anchor across the
    /// switch. PSReadLine does: after a vim killed in a pwsh tab its first redraw went to
    /// `CSI 1 ; 55 H`, the dead screen's prompt end, while the host's switch had put both
    /// cursors at row 4 of the primary screen, so every line pwsh drew after it overwrote
    /// the rows above and left stale tails of the old command line (measured 2026-10-18,
    /// the bundled OpenConsole 1.25.2609.30003). The carry makes the primary screen agree
    /// with that anchor: the rows the primary screen used scroll up (into the scrollback)
    /// until the prompt's first row is free, the rows from there down are cleared, the
    /// prompt's rows are drawn as the dead screen showed them (the dead app's cells left
    /// of the prompt's start, and right of the cursor, cleared), and the cursor goes back
    /// where the shell left it. The host draws the same bytes and forwards them, so both
    /// screens stay in step.
    ///
    /// The host's client writes it only where the shell keeps reading across the switch
    /// (a console in line mode is conhost's own line editor, whose read the switch ends:
    /// cmd prompts again on the primary screen at once), and only while the host's cursor
    /// is still the one it was composed for (`aterm_pty::resync_alt_screen_carrying`).
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
        // The prompt's first row: no lower than the cursor's (a screen that scrolled under
        // a prompt started on its last row moved that row up), and its start column only on
        // the row it started on.
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
        // Where the host's `?1049l` puts the primary cursor (the main slot `?1049h` saved;
        // home when none was), and the first row below everything the primary screen holds.
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
        // A plain pen, and the margins the dead app may have left on the host's screen.
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
        // The cursor on a full row's last cell (its wrap deferred) is the prompt's own end.
        if !wrap {
            out.push_str("\x1b[K");
        }
        Some(out)
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

    // The host's forwarded switch out of a screen a prompt proved dead releases
    // what the dead app left on, as the local leave does: a killed editor's mouse
    // tracking, cursor-key and keypad modes and scroll region would otherwise
    // route the wheel and clicks to the shell and keep its output out of the
    // scrollback. The local leave is the positive control; an app's own
    // `?1049l` (no prompt before it) keeps its modes, which are its own to reset.
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

        // A live app's own exit: no prompt proved it dead, so nothing is taken
        // from it, under the host policy or not.
        for host_resyncs in [true, false] {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(host_resyncs);
            term.process(b"\x1b[?1000h\x1b[?1h\x1b[?1049l");
            assert!(!term.modes().alternate_screen);
            assert_eq!(term.modes().mouse_mode, MouseMode::Normal, "{host_resyncs}");
            assert!(term.modes().application_cursor_keys, "{host_resyncs}");
        }

        // The mark is the dead screen's alone: an app started on a NEW screen
        // after the prompt (its own `?1049h`) keeps its modes through its exit.
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        term.process(b"\x1b[?1049h\x1b[?1000hnext app");
        term.process(b"\x1b[?1049l");
        assert_eq!(term.modes().mouse_mode, MouseMode::Normal);
    }

    // A killed vim leaves bracketed paste (2004), in-band size reports (2048) and
    // colour-scheme reports (2031) on, and OpenConsole forwards all three; pwsh
    // and cmd never turn them off, so the next line got `CSI 48 ; … t` and every
    // paste its brackets (measured 2026-10-18). The prompt that proves the app
    // dead turns them off on both paths: the local leave, and before the host's
    // forwarded `?1049l`, which can follow a line editor's own `?2004h`.
    #[test]
    fn the_prompt_that_proves_an_app_dead_turns_off_its_paste_and_reports() {
        const VIM_MODES: &[u8] = b"\x1b[?2004h\x1b[?2048h\x1b[?2031h";
        for host_resyncs in [false, true] {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(host_resyncs);
            term.process(VIM_MODES);
            let _ = term.take_response(); // 2048's report on enabling
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

        // Control: the same modes from a live program on the primary screen stay;
        // no prompt proved anything dead.
        let mut term = stuck_pager();
        term.process(b"\x1b[?1049l");
        term.process(VIM_MODES);
        term.process(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07");
        assert!(term.modes().bracketed_paste);
        assert!(term.modes().in_band_size_reports);
    }

    // The prompt that proves an app dead releases at once, on both paths, what
    // decides what the person's input becomes: mouse tracking and its encoding,
    // cursor-key and keypad modes, and a report still queued for the app (the
    // DEC 2048 report a new cell box queued, which waited for the session
    // reader's next take). Under a host that is asked to leave, the dead screen
    // keeps its scroll region until the host's switch, which releases it.
    #[test]
    fn the_prompt_that_proves_an_app_dead_releases_its_input_at_once() {
        use crate::terminal::{MouseEncoding, MouseMode};
        const DEAD_APP_MODES: &[u8] = b"\x1b[2;4r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=\x1b[?2048h";
        for host_resyncs in [true, false] {
            let mut term = stuck_pager();
            term.set_host_resyncs_alt_screen(host_resyncs);
            term.process(DEAD_APP_MODES);
            let _ = term.take_response(); // the enable's own report, the reader's
            // The window's first cell box, while the app sits idle.
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

    // The shell's own bracketed paste is the shell's. fish 3 sets it from its
    // `fish_prompt` event, after its `D` and before its `A`, and a readline
    // whose `A` sits in `PS1` sets it before drawing that prompt: kept. bash and
    // zsh under aterm's integration set it after the `A`, which comes first.
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

    // `mainscreen` under the host policy marks the screen as a dead app's before
    // the host is asked, so the forwarded switch releases its modes too, and a
    // prompt on the same screen does not ask the host a second time. On the
    // primary screen nothing is marked.
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

        // Opting off forgets the mark with the request.
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(b"\x1b[?1000h");
        assert!(term.expect_host_alt_screen_leave());
        term.set_host_resyncs_alt_screen(false);
        term.process(b"\x1b[?1049l");
        assert_eq!(term.modes().mouse_mode, MouseMode::Normal);
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

        // Only the first record is kept: a later one changes nothing.
        record(&mut t, "/other", "pre:0,s:0,j:0,r:0", "cold");
        assert_eq!(t.take_start_mark(), None);
        assert_eq!(t.spare_marks_refused(), 7, "a later record is not refused");
    }

    /// fish's startup roots (`AtermStartRoots=`): escaped absolute folders
    /// joined by `\x00`, kept with the record that follows. An empty list, an
    /// empty or relative folder, or more than `MAX_START_ROOTS` names none and
    /// is counted, and the record then carries none, as zsh's and bash's do.
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
    //! Which OSC 7 sets the folder (New Tab, Split, New Window, `aterm ctl cwd`)
    //! while an authenticated shell holds its prompt
    //! (docs/DESIGN-cwd-reports-2026-10-19.md). OSC 7 cannot require the nonce
    //! (a remote shell over ssh must be able to report its cwd), so the shell
    //! signs its own report, its signed marks say when it sits at its prompt and
    //! runs nothing (between its `A` and its `C`, once it has marked a command
    //! with `C`), and the host says when a program holds the foreground anyway.
    //! The unsigned `osc7` reports below are a shell's from before the signature.
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

    /// The shell's own report as every integration sends it: the `AtermCwd`
    /// property it signs with `KEY`, then the same URI as its OSC 7, which
    /// carries no nonce.
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

    /// The shell's own report (the `AtermCwd` it signs, then its OSC 7) is its
    /// word at any phase, and one report queues one event, none when the folder
    /// is unchanged. A property signed
    /// with another key is dropped as the marks it would forge are. The OSC 7
    /// carries no nonce, and one whose last parameter is `id=` is URI through
    /// and through, as an engine from before the property reads it: the nonce
    /// never reaches a stored path there.
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
        // The same folder again: no event.
        t.process(&own_report("/home/me/src"));
        assert!(cwd_events(&mut t).is_empty(), "unchanged");
        // Where the shell does not sit at its prompt (no `C` yet) its OSC 7
        // lands too, naming the folder its signed word just stored: one event.
        let mut t = gated_at_first_prompt();
        while t.take_osc_event().is_some() {}
        t.process(&own_report("/srv"));
        assert_eq!(t.current_working_directory(), Some("/srv"));
        assert_eq!(cwd_events(&mut t), vec!["//host/srv".to_owned()]);
        // A last parameter that looks like a signature is part of the URI.
        let mut t = Terminal::new(6, 60);
        t.process(b"\x1b]7;file://host/a;id=b\x07");
        assert_eq!(t.current_working_directory(), Some("/a;id=b"));
    }

    /// A stray between the shell's report and its `A`: the
    /// shell reports its cwd and then prints its prompt start, and a background
    /// job's report lands between the two. From a shell that signs its own it
    /// is dropped, after a command (`D` on) and after an empty Enter alike, so
    /// the folder and the prompt's mark keep the shell's. Control: from a shell
    /// that does not sign (an integration from before the signature) the
    /// latest held report still wins, as it always did.
    #[test]
    fn a_stray_between_the_shells_signed_report_and_its_prompt_sets_nothing() {
        // After a command.
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

        // After an empty Enter (no C, no D: the phase still reads its prompt).
        let mut t = signing_at_prompt();
        t.process(b"\r\n");
        t.process(&own_report("/home/me"));
        t.process(&osc7("/elsewhere"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        assert!(cwd_events(&mut t).is_empty(), "nothing changed");

        // Control: unsigned reports, the latest held one wins at the A.
        let mut t = at_prompt();
        t.process(b"\r\n");
        t.process(&osc7("/home/me"));
        t.process(&osc7("/elsewhere"));
        t.process(&signed("A"));
        assert_eq!(t.current_working_directory(), Some("/elsewhere"));
    }

    /// A program a key binding starts: it runs with no `C`
    /// (a zsh widget, a bash `bind -x`, a fish binding), but it holds the
    /// terminal's foreground, which the host says. Its reports land while it
    /// does, and once it leaves the foreground the folder is the prompt's
    /// again: the shell reports its own only at its next prompt. A stray while
    /// the shell holds the foreground is still dropped.
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
        // The program exits; the shell holds the foreground and redraws its line.
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
        // Nothing more to restore: a second word changes nothing.
        t.set_foreground_job(false);
        assert!(cwd_events(&mut t).is_empty());
        // A stray at the prompt again.
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));

        // A program the shell started with `C` reports as it always did, and
        // its end restores nothing: the shell reports its own at its prompt.
        let mut t = signing_at_prompt();
        t.process(b"ssh build\r\n");
        t.process(&signed("C"));
        t.set_foreground_job(true);
        t.process(&osc7("/srv/app"));
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
    }

    /// A program the shell keeps in its own process group (PowerShell runs
    /// every program there, so one a PSReadLine key handler starts holds no
    /// foreground of its own; ConPTY has no foreground group at all): the
    /// host's program probe says whether one the shell started since its prompt
    /// began runs. Its report lands while one does, and the cwd it replaced
    /// comes back at the first foreground word once none does. A program the
    /// shell started before its prompt (a job an earlier command left running)
    /// is no such program: its report at the prompt is a stray, dropped. The
    /// probe is asked only for a report at the prompt and while one it let land
    /// shows.
    #[test]
    fn a_program_the_probe_sees_since_the_prompt_reports_its_cwd() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
        // When the shell's one child started (0: none runs).
        let started = Arc::new(AtomicU64::new(0));
        let asked = Arc::new(AtomicU64::new(0));
        let mut t = signing_at_prompt();
        let (child, count) = (started.clone(), asked.clone());
        t.set_program_probe(Some(Box::new(move |since| {
            count.fetch_add(1, SeqCst);
            let at = child.load(SeqCst);
            at != 0 && at >= since
        })));
        // A job an earlier command started.
        started.store(1, SeqCst);
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        // A key handler's program, started since the prompt.
        started.store(u64::MAX, SeqCst);
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        // It runs on in the shell's group: the foreground word keeps it.
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
        // It exits.
        started.store(0, SeqCst);
        t.set_foreground_job(false);
        assert_eq!(t.current_working_directory(), Some("/home/me"));
        // Nothing shows now, and the shell's own report needs no word: the
        // probe is not asked.
        let before = asked.load(SeqCst);
        t.set_foreground_job(false);
        t.process(&own_report("/home/me/src"));
        assert_eq!(asked.load(SeqCst), before);
        // After the shell's `D` the prompt began there: a program started
        // after it is seen too.
        t.process(b"true\r\n");
        t.process(&signed("C"));
        t.process(&signed("D;0"));
        started.store(u64::MAX, SeqCst);
        t.process(&osc7("/srv/logs"));
        assert_eq!(t.current_working_directory(), Some("/srv/logs"));
    }

    /// A shell that stops sending `C` (PowerShell after `Import-Module
    /// PSReadLine -Force` replaced aterm's readline shim): its next `D` comes
    /// while its marks
    /// say it sat at its prompt, which says it ran a command it did not mark,
    /// and from there on its reports land at once, until a `C` says it marks
    /// what it runs again. The first command it runs so is read as its prompt
    /// where the host says nothing of the foreground (a stray, dropped); a host
    /// that does says the program holds it, and it lands.
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
        // PSReadLine is back: its C, then the prompt rule holds again.
        t.process(b"true\r\n");
        t.process(&signed("C"));
        assert!(!t.shell.unmarked_run);
        t.process(&signed("D;0"));
        t.process(&own_report("/home/me"));
        t.process(&signed("A"));
        t.process(&osc7("/elsewhere"));
        assert_eq!(t.current_working_directory(), Some("/home/me"));

        // The first unmarked command, where the host says the program holds
        // the foreground.
        let mut t = signing_at_prompt();
        t.process(b"ssh build\r\n");
        t.set_foreground_job(true);
        t.process(&osc7("/srv/app"));
        assert_eq!(t.current_working_directory(), Some("/srv/app"));
    }

    /// What the prompt left pending rides a checkpoint: a
    /// report held at the prompt of a shell that does not sign its own lands at
    /// the successor's next `A`; a shell's signing goes on being read (a stray
    /// before its `A` is dropped there too); a shell that stopped marking
    /// commands is still read so; and the cwd a key-bound program's report
    /// replaced comes back when the successor's host says it left the
    /// foreground. Through the meta wire as well.
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

        // Carried between the shell's D and its report: a stray there is
        // dropped, not landed as from a shell that does not sign.
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

        // A re-key's first property takes the key before its next A makes the
        // old prompt phase current. Carrying at that boundary must not turn
        // the stale phase into a reason to drop a report the live engine accepts.
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
            // Once A arrives, both the live and restored engine refuse strays.
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
    //! The carry a console host's leave writes after its switch
    //! (`Terminal::dead_alt_screen_carry`), played on two engines: OURS, under the
    //! host policy, and a HOST model that draws what the shell, the app and the
    //! host's client write (never the marks), as OpenConsole does. The leave's
    //! bytes reach both, the host drawing them and ours from what it forwards.
    use crate::terminal::Terminal;

    const ROWS: u16 = 6;

    struct Tab {
        ours: Terminal,
        host: Terminal,
    }

    impl Tab {
        /// A tab whose primary screen used `used` rows (`out 0` ...), then an app
        /// on the alternate screen that drew `app` and died there, and the shell's
        /// next prompt `prompt` drawn on the dead screen.
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
        /// ours composed; the host draws it and forwards it to ours. Whether
        /// there was a carry.
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
        /// alternate screen. Returns our rows.
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

    // The killed vim of the live run, small: the app's cursor on its top row, so
    // pwsh's prompt is drawn at row 0 of the dead screen and PSReadLine anchors
    // there. The carry scrolls the three used rows into the scrollback, draws the
    // prompt at row 0 with nothing of the app's row after it, and leaves the
    // cursor where the prompt ended, on both sides.
    #[test]
    fn the_prompt_comes_back_at_the_row_and_column_it_was_drawn_at() {
        let mut tab = Tab::killed(3, b"line 1 of the file\r\nline 2\x1b[H", b"PS> ");
        assert!(tab.leave(true), "a prompt to carry");
        let rows = tab.in_step();
        assert_eq!(rows, ["PS>", "", "", "", "", ""]);
        assert_eq!(tab.cursor(), (0, 4), "where the line editor anchored");
        assert_eq!(Tab::history(&tab.ours), ["out 0", "out 1", "out 2"]);
        // The line editor's redraw from its anchor lands after the prompt.
        tab.both(b"\x1b[1;5Hecho after");
        assert_eq!(Tab::rows(&tab.ours)[0], "PS> echo after");
    }

    // The control, the plain leave: the cursor goes back under the used rows
    // while the line editor's anchor stays on row 0, so its redraw overwrites
    // `out 0` and no prompt is left on screen.
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

    // A prompt below the used rows keeps them on screen: nothing scrolls, the rows
    // between are cleared, and the prompt stays on its own row.
    #[test]
    fn a_prompt_below_the_used_rows_scrolls_nothing() {
        let mut tab = Tab::killed(2, b"a\r\nb\r\nc\r\nd\x1b[5;1H", b"PS> ");
        assert!(tab.leave(true));
        let rows = tab.in_step();
        assert_eq!(rows, ["out 0", "out 1", "", "", "PS>", ""]);
        assert_eq!(tab.cursor(), (4, 4));
        assert!(Tab::history(&tab.ours).is_empty());
    }

    // A prompt that started mid-row (the app's cursor was not in column 0) comes
    // back without the app's cells left of it or right of the cursor; a prompt of
    // two rows comes back whole; the used rows scroll only as far as needed.
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

    // The prompt's colours come along: what the host's client writes is our own
    // row with its SGR.
    #[test]
    fn the_prompt_keeps_its_colours() {
        let mut tab = Tab::killed(1, b"\x1b[H", b"\x1b[32mPS\x1b[0m> ");
        assert!(tab.leave(true));
        tab.in_step();
        let text = tab.ours.grid().row_ansi_text_screen(0).unwrap_or_default();
        assert!(text.contains("\x1b[0;32mPS"), "{text:?}");
    }

    // The dead app's pen stays with it: a killed vim left bold reverse video on
    // from its ruler, and pwsh, which sets no colour for its prompt, drew the
    // prompt in it. The carried prompt comes back in the pen the leave restores,
    // on both sides, as the local leave draws it.
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

    // Nothing to carry: the shell has drawn nothing since its `A` (Git Bash's
    // prompt comes in a later write and lands on the primary screen itself), no
    // prompt marked the screen, the host's leave already came, or the policy is
    // off.
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
