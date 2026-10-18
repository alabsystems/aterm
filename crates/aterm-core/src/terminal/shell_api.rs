// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shell integration API for [`Terminal`].
//!
//! Provides access to OSC 133 shell integration state:
//! - [`Terminal::shell_state`] - current state machine state
//! - [`Terminal::command_marks`] - completed command boundaries
//! - [`Terminal::terminal_marks`] - user-set bookmarks
//! - [`Terminal::annotations`] - Terminal-style inline annotations
//!
//! OSC 133 sequences are sent by shell integrations (bash, zsh, fish)
//! to mark prompt, command, and output boundaries.

use super::TaskbarProgress;
use super::shell::{ANNOTATIONS_MAX, TERMINAL_MARKS_MAX};
use super::{Annotation, CommandMark, ShellState, Terminal, TerminalMark};

impl Terminal {
    // =========================================================================
    // Shell integration (OSC 133)
    // =========================================================================

    /// Get the current shell integration state.
    ///
    /// This reflects the state machine driven by OSC 133 sequences:
    /// - `Ground`: Waiting for prompt (initial state)
    /// - `ReceivingPrompt`: After OSC 133;A, prompt is being displayed
    /// - `EnteringCommand`: After OSC 133;B, user is typing command
    /// - `Executing`: After OSC 133;C, command is running
    #[must_use]
    pub fn shell_state(&self) -> ShellState {
        self.shell.state
    }

    /// Get all completed command marks.
    ///
    /// Command marks track the boundaries of prompts, commands, and output
    /// in the terminal. Each mark represents a completed command with its
    /// prompt range, command range, output range, and exit code.
    #[must_use]
    pub fn command_marks(&self) -> &[CommandMark] {
        self.shell.command_marks.as_slices().0
    }

    /// Monotonic count of completed commands (OSC 133;D). Strictly increases
    /// once per completion — pair with [`Self::last_completed_command`] for an
    /// unambiguous "a new command just finished" edge (same-millisecond
    /// completions share an end timestamp but never a seq). Survives `reset`.
    #[must_use]
    pub fn completed_command_seq(&self) -> u64 {
        self.shell.completed_seq
    }

    /// Monotonic count of commands the shell started (each OSC 133/633 C the
    /// A→B→C→D gate accepted, whether or not a block held it). No clear takes
    /// one back: RIS drops blocks (and the D a running command would send),
    /// ED 3 and Cmd-K ([`Self::clear_scrollback`]) the blocks wholly in the
    /// history they erase, never a counted C, so
    /// the C's since a reading, less the blocks still held that reached C
    /// since, are the commands whose blocks went unread. Survives `reset` and
    /// a checkpoint round trip.
    #[must_use]
    pub fn started_command_seq(&self) -> u64 {
        self.shell.started_seq
    }

    /// Which integration body the shell signs that it runs: the 16-hex address
    /// of its script folder, from the last signed `633;P;AtermIntegration=`
    /// mark or carried across a seamless update. `None` until one arrives
    /// (scripts without a loader never send one).
    #[must_use]
    pub fn shell_integration_rev(&self) -> Option<&str> {
        self.shell
            .integration_rev
            .as_ref()
            .and_then(|rev| std::str::from_utf8(rev).ok())
    }

    /// The shell's START record (`docs/DESIGN-warm-spare-2026-10-16.md` §4),
    /// once: the first signed `633;P;AtermStart=` with the rc folder and flags
    /// that preceded it, or `None` before it and after the take. Only the first
    /// record a shell signs is ever kept.
    pub fn take_start_mark(&mut self) -> Option<super::StartMark> {
        self.shell.spare.mark.take()
    }

    /// Whether a signed `633;P;AtermSpare=parked` arrived since the last take:
    /// a spare shell waits at its parked hold, before any startup file.
    pub fn take_spare_parked(&mut self) -> bool {
        std::mem::take(&mut self.shell.spare.parked)
    }

    /// Signed spare marks refused as malformed (`AtermSpare=`, `AtermStart*=`).
    #[must_use]
    pub fn spare_marks_refused(&self) -> u32 {
        self.shell.spare.refused
    }

    /// Whether the shell has signed a prompt start (an accepted OSC 133/633 `A`)
    /// since this engine began: a released spare has left every hold by then.
    #[must_use]
    pub fn shell_prompt_started(&self) -> bool {
        self.shell.next_block_id > 0
    }

    /// Get the most recent completed command mark (exit code recorded),
    /// regardless of status — the "what just finished" probe (PHOSPHOR's
    /// exit-status weather reads this per frame under the render lock).
    #[must_use]
    pub fn last_completed_command(&self) -> Option<&CommandMark> {
        self.shell
            .command_marks
            .iter()
            .rev()
            .find(|m| m.is_complete())
    }

    // =========================================================================
    // Terminal Extensions (OSC 1337)
    // =========================================================================

    /// Get all terminal marks (OSC 1337 SetMark).
    ///
    /// Terminal marks are user/application-created navigation points,
    /// allowing users to jump back to important locations in output.
    /// Unlike command marks (OSC 133), these are explicitly set.
    #[must_use]
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
        // FIFO eviction if at capacity
        if self.marks_state.marks.len() >= TERMINAL_MARKS_MAX {
            self.marks_state.marks.pop_front();
        }
        self.marks_state.marks.push_back(mark);
        self.marks_state.marks.make_contiguous();
        id
    }

    /// Get all annotations (OSC 1337 AddAnnotation).
    ///
    /// Annotations are metadata/notes attached to specific regions of
    /// terminal output. They can be visible or hidden.
    #[must_use]
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
        // FIFO eviction if at capacity
        if self.marks_state.annotations.len() >= ANNOTATIONS_MAX {
            self.marks_state.annotations.pop_front();
        }
        self.marks_state.annotations.push_back(annotation);
        self.marks_state.annotations.make_contiguous();
        id
    }

    /// Set the cell pixel size `(width, height)` used to convert pixel/auto
    /// inline-image dimensions (OSC 1337 `File=`) into a CELL footprint.
    ///
    /// The frontend reports its real font metrics here so an image requested in
    /// pixels (`width=200px`) lands on the right number of cells. Cell-unit
    /// requests (`width=10`) never consult this. A zero on either axis is clamped
    /// to 1 at use to avoid a divide-by-zero. Defaults to a sane 8×16 so headless
    /// tests are self-contained.
    ///
    /// Calling this is also what makes the metric reportable: it is the only
    /// writer of the flag [`Self::host_cell_pixel_size`] reads, and the XTWINOPS
    /// pixel reports (CSI 14 t / 16 t) answer from that and stay silent until a
    /// host has spoken; a DEC 2048 in-band report says 0 pixels until then, and
    /// a change of the measured box while 2048 is on queues a new one. A host
    /// with real font metrics should call this as soon as they exist and again
    /// whenever they change (font zoom, a DPI move).
    pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
        let before = self.iterm2.host_cell_px();
        self.iterm2.cell_px = (width, height);
        self.iterm2.cell_px_from_host = true;
        // DEC 2048: a new cell box at the same rows/cols (the first measurement, a
        // font zoom, a move to a display of another scale) changes the text area's
        // pixels, which `resize` alone would never report. Only a change: hosts set
        // the box on every layout pass.
        let after = self.iterm2.host_cell_px();
        if self.modes.in_band_size_reports && after != before {
            super::state_accessors::push_in_band_size_report(
                &mut self.transient.response_buffer,
                self.grid.rows(),
                self.grid.cols(),
                after,
            );
        }
    }

    /// The cell pixel size used for inline-image footprint math (OSC 1337).
    ///
    /// Always a usable pair — the [`DEFAULT_CELL_PX`](super::grouped_state) 8×16
    /// placeholder until a host reports. Use [`Self::host_cell_pixel_size`] when
    /// the caller must not pass the placeholder off as a measurement.
    #[must_use]
    pub fn cell_pixel_size(&self) -> (u16, u16) {
        self.iterm2.cell_px
    }

    /// The cell pixel size a host actually reported, or `None` if none has.
    ///
    /// The engine cannot measure a glyph — only the frontend holding the
    /// rasterizer can. So this is `None` for a headless engine, and for a live
    /// session whose renderer has not published metrics yet. A zero on either
    /// axis is treated as unreported too: it is not a cell box any font could
    /// have, and reporting it would tell an application to divide by zero.
    ///
    /// Callers that answer a wire query must use this and stay silent on `None`
    /// rather than fall back to [`Self::cell_pixel_size`]; see
    /// `handler_window`'s XTWINOPS pixel reports.
    #[must_use]
    pub fn host_cell_pixel_size(&self) -> Option<(u16, u16)> {
        self.iterm2.host_cell_px()
    }

    /// The last taskbar progress state the program set (ConEmu OSC 9;4).
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
    pub fn taskbar_progress(&self) -> Option<TaskbarProgress> {
        self.taskbar_progress
    }

    /// The program's own state report (OSC 7501): the last report it made and
    /// the sequence that moves with each one, a `clear` and a full reset
    /// included ([`super::program_state`]). A claim for display, never
    /// evidence a decision rests on.
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
    pub fn program_state(&self) -> super::program_state::ProgramStateReport {
        self.program_state
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal::Terminal;

    /// `completed_command_seq` strictly increases once per OSC 133;D — two
    /// commands completing within the same millisecond (indistinguishable by
    /// `command_end_time_ms`) still read as two edges — and it survives a
    /// terminal reset (a reset must never replay an old value to observers).
    #[test]
    fn completed_command_seq_is_strictly_per_completion() {
        let mut term = Terminal::new(6, 40);
        assert_eq!(term.completed_command_seq(), 0);
        // Two full command cycles back-to-back in one `process` burst — the
        // same wall-clock ms, the exact collapse case for timestamp keying.
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
    /// no block, and nothing takes one back: RIS drops the blocks (and the
    /// running command's D, which `completed_command_seq` then never counts),
    /// ED 3 and Cmd-K the ones wholly in the history they erase (a running
    /// `clear` keeps its own), and a checkpoint round trip carries it.
    /// NEGATIVE CONTROL: a C out of order (no B before it) is not a start.
    #[test]
    fn started_command_seq_counts_every_accepted_c_and_no_clear_takes_one() {
        let prompt = b"\x1b]133;A\x07$ \x1b]133;B\x07";
        let mut term = Terminal::new(6, 40);
        term.process(prompt);
        term.process(b"a\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        assert_eq!(term.started_command_seq(), 1);
        // A C with no B before it is refused by the gate.
        term.process(b"\x1b]133;A\x07\x1b]133;C\x07");
        assert_eq!(term.started_command_seq(), 1, "out of order: no start");
        // `reset` at a prompt: its C counts, the RIS takes its block and the
        // phase, so the shell's D is refused and no D is counted.
        term.process(prompt);
        term.process(b"reset\r\n\x1b]133;C\x07\x1bc\x1b]133;D;0\x07");
        assert_eq!(term.started_command_seq(), 2);
        assert_eq!(term.completed_command_seq(), 1, "the RIS swallowed the D");
        assert!(
            term.all_blocks().next().is_none(),
            "the RIS took the blocks"
        );
        // `clear` (ED 3 from the running command) and Cmd-K.
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
}
