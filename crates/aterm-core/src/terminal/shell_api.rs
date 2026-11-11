// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shell integration API for [`Terminal`]: OSC 133/1337/9;4/7501 state, marks, annotations, and cell metrics.

use super::TaskbarProgress;
use super::shell::{ANNOTATIONS_MAX, TERMINAL_MARKS_MAX};
use super::{Annotation, BlockState, CommandMark, ShellState, Terminal, TerminalMark};

/// Where the foreground shell's line editor starts its buffer ([`Terminal::input_region_start`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputRegionStart {
    /// The visible row of the prompt's `B`, from `0`.
    pub row: u16,
    /// Its column: the cursor's when the shell sent it.
    pub col: u16,
    /// Block id opened by the prompt's `A` mark ([`Terminal::prompt_in_front`]).
    pub prompt: u64,
}

impl Terminal {
    /// Current OSC 133 shell-integration state.
    #[must_use]
    #[inline]
    pub const fn shell_state(&self) -> ShellState {
        self.shell.state
    }

    /// All completed command marks (prompt, command, and output ranges with exit code).
    #[must_use]
    #[inline]
    pub fn command_marks(&self) -> &[CommandMark] {
        self.shell.command_marks.as_slices().0
    }

    /// Monotonic count of completed commands (OSC 133;D), surviving `reset`.
    #[must_use]
    #[inline]
    pub const fn completed_command_seq(&self) -> u64 {
        self.shell.completed_seq
    }

    /// Monotonic count of commands started by the shell (accepted OSC 133/633 `C` marks).
    #[must_use]
    #[inline]
    pub const fn started_command_seq(&self) -> u64 {
        self.shell.started_seq
    }

    /// 16-hex address of the integration script folder signed by the shell, if any.
    #[must_use]
    #[inline]
    pub fn shell_integration_rev(&self) -> Option<&str> {
        self.shell
            .integration_rev
            .as_ref()
            .and_then(|rev| std::str::from_utf8(rev).ok())
    }

    /// Take the shell's first signed `633;P;AtermStart=` record, if not yet taken.
    #[inline]
    pub const fn take_start_mark(&mut self) -> Option<super::StartMark> {
        self.shell.spare.mark.take()
    }

    /// Whether a signed `633;P;AtermSpare=parked` arrived since the last take.
    #[inline]
    pub const fn take_spare_parked(&mut self) -> bool {
        std::mem::replace(&mut self.shell.spare.parked, false)
    }

    /// Signed spare marks refused as malformed (`AtermSpare=`, `AtermStart*=`).
    #[must_use]
    #[inline]
    pub const fn spare_marks_refused(&self) -> u32 {
        self.shell.spare.refused
    }

    /// Whether the shell has signed a prompt start (accepted OSC 133/633 `A`).
    #[must_use]
    #[inline]
    pub const fn shell_prompt_started(&self) -> bool {
        self.shell.next_block_id > 0
    }

    /// Visible cell and prompt id where the foreground shell's authenticated `B` mark starts its input buffer.
    #[must_use]
    #[inline]
    pub fn input_region_start(&self) -> Option<InputRegionStart> {
        let prompt = self.prompt_in_front()?;
        if !self.shell.input_start_attested
            || self.shell.foreground_job
            || self.is_alternate_screen()
            || self.grid.display_offset() != 0
        {
            return None;
        }
        let block = self.shell.current_block.as_ref()?;
        let (absolute, col) = (block.command_start_row?, block.command_start_col?);
        let row = absolute.checked_sub(self.grid.visible_to_absolute(0))?;
        let row = u16::try_from(row).ok().filter(|&r| r < self.rows())?;
        (col < self.cols()).then_some(InputRegionStart { row, col, prompt })
    }

    /// Block id of the authenticated prompt whose line editor currently reads keys, or `None`.
    #[must_use]
    #[inline]
    pub fn prompt_in_front(&self) -> Option<u64> {
        if !self.modes.require_shell_integration_nonce
            || self.shell_integration_auth.nonce_in_use().is_none()
            || self.shell.phase_stale
            || self.shell.state != ShellState::EnteringCommand
        {
            return None;
        }
        let block = self.shell.current_block.as_ref()?;
        (block.state == BlockState::EnteringCommand).then_some(block.id)
    }

    /// Most recent completed command mark (exit code recorded), regardless of status.
    #[must_use]
    #[inline]
    pub fn last_completed_command(&self) -> Option<&CommandMark> {
        self.shell
            .command_marks
            .iter()
            .rev()
            .find(|m| m.is_complete())
    }

    /// All explicitly set terminal marks (OSC 1337 SetMark).
    #[must_use]
    #[inline]
    pub fn terminal_marks(&self) -> &[TerminalMark] {
        self.marks_state.marks.as_slices().0
    }

    /// Add a named terminal mark at the current cursor position.
    pub fn add_named_mark(&mut self, name: &str) -> u64 {
        let cursor = self.grid.cursor();
        let id = self.marks_state.next_mark_id;
        self.marks_state.next_mark_id += 1;
        let row = self.grid.visible_to_absolute(cursor.row);
        let mut mark = TerminalMark::new(id, row, cursor.col);
        mark.name = Some(name.to_string());
        if self.marks_state.marks.len() >= TERMINAL_MARKS_MAX {
            self.marks_state.marks.pop_front();
        }
        self.marks_state.marks.push_back(mark);
        self.marks_state.marks.make_contiguous();
        id
    }

    /// All output region annotations (OSC 1337 AddAnnotation).
    #[must_use]
    #[inline]
    pub fn annotations(&self) -> &[Annotation] {
        self.marks_state.annotations.as_slices().0
    }

    /// Add a visible annotation at the current cursor position.
    pub fn add_annotation(&mut self, message: &str) -> u64 {
        let cursor = self.grid.cursor();
        let id = self.marks_state.next_annotation_id;
        self.marks_state.next_annotation_id += 1;
        let row = self.grid.visible_to_absolute(cursor.row);
        let annotation = Annotation::new(id, row, cursor.col, message.to_string());
        if self.marks_state.annotations.len() >= ANNOTATIONS_MAX {
            self.marks_state.annotations.pop_front();
        }
        self.marks_state.annotations.push_back(annotation);
        self.marks_state.annotations.make_contiguous();
        id
    }

    /// Set the cell pixel size `(width, height)` used for inline-image footprint math and DEC 2048 reports.
    pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
        let before = self.iterm2.host_cell_px();
        self.iterm2.cell_px = (width, height);
        self.iterm2.cell_px_from_host = true;
        let after = self.iterm2.host_cell_px();
        if self.modes.in_band_size_reports && after != before {
            super::state_accessors::push_in_band_size_report(
                &mut self.transient,
                self.grid.rows(),
                self.grid.cols(),
                after,
            );
        }
    }

    /// Cell pixel size for inline-image footprint math (falling back to the 8×16 placeholder).
    #[must_use]
    #[inline]
    pub const fn cell_pixel_size(&self) -> (u16, u16) {
        self.iterm2.cell_px
    }

    /// Cell pixel size reported by the host, or `None` if unreported.
    #[must_use]
    #[inline]
    pub const fn host_cell_pixel_size(&self) -> Option<(u16, u16)> {
        self.iterm2.host_cell_px()
    }

    /// Last taskbar progress state set by the program (ConEmu OSC 9;4).
    ///
    /// # Example
    /// ```
    /// use aterm_core::terminal::Terminal;
    /// use aterm_types::TaskbarProgress;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// term.process(b"\x1b]9;4;1;50\x07");  // Set 50% progress
    /// assert_eq!(term.taskbar_progress(), Some(TaskbarProgress::Normal(50)));
    /// ```
    #[must_use]
    #[inline]
    pub const fn taskbar_progress(&self) -> Option<TaskbarProgress> {
        self.taskbar_progress
    }

    /// Program's own state report (OSC 7501, [`super::program_state`]).
    ///
    /// # Example
    /// ```
    /// use aterm_core::terminal::Terminal;
    /// use aterm_core::terminal::program_state::ProgramStateWord;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
    /// let report = term.program_state();
    /// assert_eq!(report.state.map(|s| s.state), Some(ProgramStateWord::Working));
    /// ```
    #[must_use]
    #[inline]
    pub const fn program_state(&self) -> super::program_state::ProgramStateReport {
        self.program_state
    }
}

#[cfg(test)]
#[path = "shell_api_tests.rs"]
mod tests;
