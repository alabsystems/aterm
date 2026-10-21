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
use super::{Annotation, BlockState, CommandMark, ShellState, Terminal, TerminalMark};

/// Where the line editor of the shell in front starts its buffer, as its
/// authenticated prompt vouches for it ([`Terminal::input_region_start`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputRegionStart {
    /// The visible row of the prompt's `B`, from `0`.
    pub row: u16,
    /// Its column: the cursor's when the shell sent it.
    pub col: u16,
    /// Which prompt: the id of the block its `A` opened
    /// ([`Terminal::prompt_in_front`]). Every prompt has its own, so a `B` the
    /// engine keeps across a redraw is told from the next prompt's at the
    /// same cell.
    pub prompt: u64,
}

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
    /// a checkpoint round trip; a restore never sets it below the carried
    /// blocks that reached C (a predecessor that did not count carries none).
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

    /// Where the line editor of the shell in front starts the buffer it is
    /// editing: the visible cell of the AUTHENTICATED OSC 133/633 `B` of its
    /// prompt, and which prompt that is, or `None` whenever that cannot be
    /// vouched for.
    ///
    /// A `B` mark is the shell saying "the command line starts here": the
    /// prompt is drawn, the cursor stands where typed text goes. It is the one
    /// far-end acknowledgement of the buffer's start a terminal gets, and the
    /// predictor places Home, End, Ctrl-A, Ctrl-E and Delete only on a line it
    /// started there, with the first key typed at that prompt
    /// (`aterm_predict::KeySample::input_start`). So it is handed over only
    /// when every one of these holds:
    ///
    /// - the capability-nonce gate is on and its nonce is in use, and the phase
    ///   is the shell's last word (`phase_stale` clear): a mark then passed
    ///   only with the shell's own `id=`, so no program's output (a `cat` of a
    ///   log holding `133;B`, a remote shell over ssh) can name a buffer start;
    ///   and in the window of a re-key the shell has not taken, or of a
    ///   successor's first mark, the marks the shell signed meanwhile were
    ///   dropped, so the `B` the engine still holds may be a prompt that is
    ///   over;
    /// - the `B` said it stands where the buffer STARTS (`aterm_input=start`,
    ///   `ShellIntegrationState::input_start_attested`): zsh sends its `B`
    ///   from zle-line-init, after zle drew the buffer, and a line pre-filled
    ///   from its buffer stack (push-line, accept-and-hold, `print -z`) is
    ///   drawn first, so a `B` there stands after that text (measured on zsh
    ///   5.9: `P> echo hi` with the mark at column 10); aterm's zsh
    ///   integration says so only over an empty buffer;
    /// - the shell is between that `B` and its `C` (`EnteringCommand`), and no
    ///   program a key binding started holds the foreground
    ///   ([`Self::set_foreground_job`]): once a command runs, the prompt the
    ///   mark belonged to is over, whatever the screen still shows;
    /// - the main screen is up and the viewport is at the live bottom
    ///   (`display_offset == 0`), so the visible row is the grid row the
    ///   cursor and the predictor's coordinates name;
    /// - the mark's absolute row (`OutputBlock::command_start_row`, kept in
    ///   step with every row splice) is on screen, and its column inside it.
    ///
    /// The row is visible, from `0`; the column is the cursor's when the shell
    /// sent the mark. Nothing moves the cell after: a prompt the shell redraws
    /// in place (zsh's `reset-prompt` of an async segment, a resize) sends no
    /// new `B`, which is why the predictor trusts it only for the FIRST key of
    /// a prompt, over the buffer the mark found empty.
    #[must_use]
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

    /// Which prompt's line editor reads the keys now, by the authenticated
    /// shell's own marks: the id of the block its `A` opened, once its `B`
    /// came and before its `C`, or `None`. The capability-nonce gate on, its
    /// nonce in use and the phase the shell's last word, as for
    /// [`Self::input_region_start`]; nothing about the screen.
    ///
    /// The predictor is told it with a line terminator
    /// (`aterm_predict::Predictor::note_line_submit_at`): the next prompt's
    /// first key may start a witnessed line only at ANOTHER prompt, so an
    /// Enter the line editor kept for itself (accepting a completion menu's
    /// pick) opens nothing at the prompt it was pressed at.
    #[must_use]
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
                &mut self.transient,
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
    /// saying so (`aterm_input=start`).
    fn signed_b() -> Vec<u8> {
        signed("B;aterm_input=start")
    }

    /// The cell of the input region's start, if any.
    fn cell(t: &Terminal) -> Option<(u16, u16)> {
        t.input_region_start().map(|s| (s.row, s.col))
    }

    /// A 4×20 engine whose host authorized [`KEY`] and turned the nonce gate
    /// on, at a signed prompt `$ ` on row 0 with nothing typed.
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
    /// `B`, and that prompt's id: where the shell said its buffer starts,
    /// whatever is typed after it, until the next prompt's own `B`, whose id
    /// is another. NEGATIVE CONTROLS: the same marks unsigned under the gate
    /// are dropped (no start), the gate off vouches for nothing however the
    /// marks read, and a forged `A` and `B` printed mid-line (a program's
    /// output, a `cat` of a log) move nothing.
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
    /// none: zsh sends its `B` from zle-line-init after zle drew a line
    /// pre-filled from its buffer stack (accept-and-hold: `P> echo hi` with
    /// the mark at column 10, measured on zsh 5.9), and aterm's integration
    /// says `aterm_input=start` only over an empty buffer. The prompt is still
    /// the one in front (`prompt_in_front`). Control: the same prompt's `B`
    /// saying so names its cell.
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
        // The next prompt's `A` takes the word back until its own `B` says so.
        control.process(b"\r\n");
        control.process(&signed("A"));
        control.process(b"P> pushed");
        control.process(&signed("B"));
        assert_eq!(cell(&control), None, "a word is the prompt's own");
    }

    /// In the window of a re-key the shell has not taken, the marks it signs
    /// with the new key before then were dropped: the phase, and the `B` the
    /// engine holds, may belong to a prompt that is over. No start then: not
    /// while the new key is not in use, and not while the phase is stale
    /// after the first mark signed with it that is not an `A`. Each is a
    /// negative control of its own conjunct; the new prompt's `A` and `B`
    /// name a start again.
    #[test]
    fn input_region_start_is_none_while_the_phase_is_not_the_shell_s_last_word() {
        let mut t = at_signed_prompt();
        assert_eq!(cell(&t), Some((0, 2)), "control: before the re-key");
        t.authorize_shell_integration_on_first_mark(NEW_KEY);
        assert_eq!(cell(&t), None, "the new key is not in use yet");
        assert_eq!(t.prompt_in_front(), None);
        // The shell's first mark with the new key is no `A` (a property it
        // signs before its prompt): the key is in use, the phase stale.
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
    /// where the grid's coordinates say, there is no input region: a command
    /// started (`C`), a program a key binding started holds the foreground, the
    /// alternate screen is up, the prompt's row scrolled off the top, or the
    /// viewport is scrolled back into history. Each returns when it is undone,
    /// where it can be.
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
    /// default reads 0): the restore starts the count at the carried blocks
    /// that reached C, finished or running, and a carried count above them
    /// (a clear took some) stands. NEGATIVE CONTROL: blocks at a prompt or
    /// mid-input prove no C.
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
