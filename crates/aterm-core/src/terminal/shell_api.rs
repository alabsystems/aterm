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
    #[must_use]
    #[inline]
    pub const fn taskbar_progress(&self) -> Option<TaskbarProgress> {
        self.taskbar_progress
    }

    /// Program's own state report (OSC 7501, [`super::program_state`]).
    #[must_use]
    #[inline]
    pub const fn program_state(&self) -> super::program_state::ProgramStateReport {
        self.program_state
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal::Terminal;

    /// `completed_command_seq` rises once per OSC 133;D, even for two completions in one
    #[test]
    fn completed_command_seq_is_strictly_per_completion() {
        let mut term = Terminal::new(6, 40);
        assert_eq!(term.completed_command_seq(), 0);
        term.process(
            b"\x1b]133;A\x07$ a\x1b]133;B\x07\r\n\x1b]133;C\x07ok\r\n\x1b]133;D;0\x07\
              \x1b]133;A\x07$ b\x1b]133;B\x07\r\n\x1b]133;C\x07no\r\n\x1b]133;D;1\x07",
        );
        assert_eq!(
            term.completed_command_seq(),
            2,
            "two same-instant completions must be two edges"
        );
        let before = term.completed_command_seq();
        term.reset();
        assert_eq!(
            term.completed_command_seq(),
            before,
            "the seq survives reset (monotonic for observers)"
        );
    }

    /// `started_command_seq` counts each C the A→B→C→D gate accepts, block or
    #[test]
    fn started_command_seq_counts_every_accepted_c_and_no_clear_takes_one() {
        let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
        let mut term = Terminal::new(6, 40);
        term.process(prompt);
        term.process(b"a\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(term.started_command_seq(), 1);
        term.process(b"\x1b]133;A\x07\x1b]133;C\x07");
        assert_eq!(term.started_command_seq(), 1, "out of order: no start");
        term.process(prompt);
        term.process(b"reset\r\n\x1b]133;C\x07\x1bc\x1b]133;D;0\x07");
        assert_eq!(term.started_command_seq(), 2);
        assert_eq!(term.completed_command_seq(), 1, "the RIS swallowed the D");
        assert!(
            term.all_blocks().next().is_none(),
            "the RIS took the blocks"
        );
        term.process(prompt);
        term.process(b"clear\r\n\x1b]133;C\x07\x1b[3J\x1b]133;D;0\x07");
        assert_eq!(term.started_command_seq(), 3);
        term.process(prompt);
        term.process(b"b\r\n\x1b]133;C\x07");
        term.clear_scrollback();
        term.process(b"\x1b]133;D;0\x07");
        assert_eq!(term.started_command_seq(), 4);
        term.reset();
        assert_eq!(term.started_command_seq(), 4, "survives reset");
        let carried = Terminal::from_checkpoint(&term.checkpoint());
        assert_eq!(carried.started_command_seq(), 4, "a checkpoint carries it");
    }

    /// The capability nonce the input-region tests' shell signs its marks with.
    const KEY: [u8; 32] = [0x6B; 32];
    /// The key a re-key hands that shell.
    const NEW_KEY: [u8; 32] = [0x7C; 32];

    /// OSC 133 `mark` signed with `key`, as aterm's integration sends it.
    fn signed_with(key: [u8; 32], mark: &str) -> Vec<u8> {
        format!(
            "\x1b]133;{mark};id={}\x07",
            crate::terminal::ShellIntegrationNonce(key).to_hex()
        )
        .into_bytes()
    }

    /// OSC 133 `mark` signed with [`KEY`].
    fn signed(mark: &str) -> Vec<u8> {
        signed_with(KEY, mark)
    }

    /// The `B` aterm's integration sends where the buffer starts: signed, and
    fn signed_b() -> Vec<u8> {
        signed("B;aterm_input=start")
    }

    /// The cell of the input region's start, if any.
    fn cell(t: &Terminal) -> Option<(u16, u16)> {
        t.input_region_start().map(|s| (s.row, s.col))
    }

    /// A 4×20 engine whose host authorized [`KEY`] and turned the nonce gate
    fn at_signed_prompt() -> Terminal {
        let mut t = Terminal::new(4, 20);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed_b());
        t
    }

    /// `input_region_start` is the visible cell of the prompt's AUTHENTICATED
    #[test]
    fn input_region_start_is_the_signed_prompt_s_b() {
        let mut t = at_signed_prompt();
        let first = t.input_region_start().expect("the signed B");
        assert_eq!((first.row, first.col), (0, 2));
        assert_eq!(t.prompt_in_front(), Some(first.prompt));
        t.process(b"echo hi");
        assert_eq!(cell(&t), Some((0, 2)), "typing moves no start");
        t.process(b"\x1b]133;A\x07\x1b]133;B;aterm_input=start\x07");
        assert_eq!(cell(&t), Some((0, 2)), "an unsigned pair moves none");
        t.process(b"\r\n");
        t.process(&signed("C"));
        assert_eq!(t.prompt_in_front(), None, "a command runs");
        t.process(b"hi\r\n");
        t.process(&signed("D;0"));
        t.process(&signed("A"));
        t.process(b"$$ ");
        t.process(&signed_b());
        let next = t.input_region_start().expect("the next prompt's own B");
        assert_eq!((next.row, next.col), (2, 3));
        assert_ne!(next.prompt, first.prompt, "another prompt, another id");

        let mut unsigned = Terminal::new(4, 20);
        unsigned.authorize_shell_integration(KEY);
        unsigned.set_require_shell_integration_nonce(true);
        unsigned.process(b"\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x07");
        assert_eq!(cell(&unsigned), None, "unsigned marks are dropped");

        let mut open = Terminal::new(4, 20);
        open.process(b"\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x07");
        assert_eq!(
            open.current_block().and_then(|b| b.command_start_col),
            Some(2),
            "control: the open gate took the mark"
        );
        assert_eq!(cell(&open), None, "an open gate vouches for nothing");
        assert_eq!(open.prompt_in_front(), None);
    }

    /// A signed `B` that does not say it stands at the buffer's start names
    #[test]
    fn a_b_that_does_not_say_it_starts_the_buffer_names_no_start() {
        let mut t = Terminal::new(4, 20);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        t.process(&signed("A"));
        t.process(b"P> echo hi");
        t.process(&signed("B"));
        assert_eq!(cell(&t), None, "a pre-filled line's B");
        assert!(t.prompt_in_front().is_some(), "its prompt is in front");

        let mut control = Terminal::new(4, 20);
        control.authorize_shell_integration(KEY);
        control.set_require_shell_integration_nonce(true);
        control.process(&signed("A"));
        control.process(b"P> ");
        control.process(&signed_b());
        assert_eq!(cell(&control), Some((0, 3)), "control: an empty line's B");
        control.process(b"\r\n");
        control.process(&signed("A"));
        control.process(b"P> pushed");
        control.process(&signed("B"));
        assert_eq!(cell(&control), None, "a word is the prompt's own");
    }

    /// In the window of a re-key the shell has not taken, the marks it signs
    #[test]
    fn input_region_start_is_none_while_the_phase_is_not_the_shell_s_last_word() {
        let mut t = at_signed_prompt();
        assert_eq!(cell(&t), Some((0, 2)), "control: before the re-key");
        t.authorize_shell_integration_on_first_mark(NEW_KEY);
        assert_eq!(cell(&t), None, "the new key is not in use yet");
        assert_eq!(t.prompt_in_front(), None);
        t.process(&signed_with(NEW_KEY, "B;aterm_input=start"));
        assert_eq!(
            t.shell_integration_nonce_in_use(),
            Some(NEW_KEY),
            "control: the key is in use"
        );
        assert_eq!(cell(&t), None, "the phase is stale");
        assert_eq!(t.prompt_in_front(), None);
        t.process(b"\r\n");
        t.process(&signed_with(NEW_KEY, "A"));
        t.process(b"$ ");
        t.process(&signed_with(NEW_KEY, "B;aterm_input=start"));
        assert_eq!(cell(&t), Some((1, 2)), "the new prompt's own B");
    }

    /// Once the prompt in front is not the one the `B` belongs to, or is not
    #[test]
    fn input_region_start_is_none_once_its_prompt_is_not_in_front() {
        let mut t = at_signed_prompt();
        t.process(b"ls\r\n");
        t.process(&signed("C"));
        assert_eq!(cell(&t), None, "a command runs");

        let mut t = at_signed_prompt();
        t.set_foreground_job(true);
        assert_eq!(cell(&t), None, "a binding's program runs");
        assert!(
            t.prompt_in_front().is_some(),
            "its prompt is still the shell's"
        );
        t.set_foreground_job(false);
        assert_eq!(cell(&t), Some((0, 2)));

        let mut t = at_signed_prompt();
        t.process(b"\x1b[?1049h");
        assert_eq!(cell(&t), None, "the alternate screen");
        t.process(b"\x1b[?1049l");
        assert_eq!(cell(&t), Some((0, 2)));

        let mut t = at_signed_prompt();
        t.process(b"\r\n\r\n\r\n");
        assert_eq!(cell(&t), Some((0, 2)), "control: still on screen");
        t.process(b"\r\n");
        assert_eq!(cell(&t), None, "the prompt's row scrolled off");

        let mut t = Terminal::new(4, 20);
        t.authorize_shell_integration(KEY);
        t.set_require_shell_integration_nonce(true);
        t.process(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        t.process(&signed("A"));
        t.process(b"$ ");
        t.process(&signed_b());
        assert_eq!(cell(&t), Some((3, 2)));
        t.scroll_display(1);
        assert_ne!(t.grid().display_offset(), 0, "control: scrolled back");
        assert_eq!(cell(&t), None, "the viewport shows history");
        t.scroll_to_bottom();
        assert_eq!(cell(&t), Some((3, 2)));
    }

    /// A predecessor's checkpoint carries no `started_seq` (the wire's
    #[test]
    fn a_restore_never_counts_fewer_starts_than_its_blocks_prove() {
        let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
        let mut term = Terminal::new(6, 40);
        term.process(prompt);
        term.process(b"a\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        term.process(prompt);
        term.process(b"b\r\n\x1b]133;C\x07");
        let mut cp = term.checkpoint();
        cp.shell.started_seq = 0;
        assert_eq!(
            Terminal::from_checkpoint(&cp).started_command_seq(),
            2,
            "one finished block and the running one each took a C"
        );
        cp.shell.started_seq = 7;
        assert_eq!(
            Terminal::from_checkpoint(&cp).started_command_seq(),
            7,
            "a carried count above the blocks stands"
        );
        let mut idle = Terminal::new(6, 40);
        idle.process(prompt);
        idle.process(prompt);
        idle.process(b"typing");
        let cp = idle.checkpoint();
        assert_eq!(
            cp.shell.output_blocks.len(),
            1,
            "CONTROL: a block is carried"
        );
        assert_eq!(
            Terminal::from_checkpoint(&cp).started_command_seq(),
            0,
            "CONTROL: no carried block reached C"
        );
    }
}
