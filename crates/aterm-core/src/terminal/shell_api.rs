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
use super::{Annotation, CommandMark, ShellEvent, ShellState, Terminal, TerminalMark};

/// Maximum number of OSC 1337 user variables retained per terminal.
///
/// When the map is full, inserting a new key evicts the oldest entry
/// (FIFO). Updates to existing keys never evict. Mirrors the cap that
/// previously lived in the now-removed OSC 1337 handler module.
const USER_VARS_MAX: usize = 256;

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

    /// Get the current (in-progress) command mark, if any.
    ///
    /// Returns `Some` if a command is currently being entered or executed,
    /// `None` if in ground state or no mark has been started.
    #[must_use]
    pub fn current_mark(&self) -> Option<&CommandMark> {
        self.shell.current_mark.as_ref()
    }

    /// Clear all command marks.
    ///
    /// This does not affect the current shell state, only clears the history
    /// of completed commands.
    pub fn clear_command_marks(&mut self) {
        self.shell.command_marks.clear();
    }

    /// Set shell integration callback.
    ///
    /// The callback is invoked when OSC 133 sequences transition the shell state.
    /// This can be used to:
    /// - Highlight prompts differently from output
    /// - Track command history with exit codes
    /// - Implement "jump to previous/next command" features
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aterm_core::terminal::{ShellEvent, Terminal};
    /// # let mut terminal = Terminal::new(24, 80);
    /// terminal.set_shell_callback(|event| {
    ///     match event {
    ///         ShellEvent::PromptStart { row, col } => {
    ///             println!("Prompt starting at row {row}, col {col}");
    ///         }
    ///         ShellEvent::CommandFinished { exit_code } => {
    ///             if exit_code != 0 {
    ///                 println!("Command failed with exit code {exit_code}");
    ///             }
    ///         }
    ///         _ => {}
    ///     }
    /// });
    /// ```
    pub fn set_shell_callback<F: FnMut(ShellEvent) + Send + 'static>(&mut self, callback: F) {
        self.shell.callback = Some(Box::new(callback));
    }

    /// Clear shell integration callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_shell_callback(&mut self) {
        self.shell.callback = None;
    }

    /// Get the most recent command mark that succeeded (exit code 0).
    #[must_use]
    pub fn last_successful_command(&self) -> Option<&CommandMark> {
        self.shell
            .command_marks
            .iter()
            .rev()
            .find(|m| m.succeeded())
    }

    /// Monotonic count of completed commands (OSC 133;D). Strictly increases
    /// once per completion — pair with [`Self::last_completed_command`] for an
    /// unambiguous "a NEW command just finished" edge (same-millisecond
    /// completions share an end timestamp but never a seq). Survives `reset`.
    #[must_use]
    pub fn completed_command_seq(&self) -> u64 {
        self.shell.completed_seq
    }

    /// Get the most recent COMPLETED command mark (exit code recorded),
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

    /// Get the most recent command mark that failed (exit code != 0).
    #[must_use]
    pub fn last_failed_command(&self) -> Option<&CommandMark> {
        self.shell
            .command_marks
            .iter()
            .rev()
            .find(|m| !m.succeeded() && m.is_complete())
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

    /// Add a terminal mark at the current cursor position.
    ///
    /// This is equivalent to receiving `OSC 1337 ; SetMark ST`.
    pub fn add_mark(&mut self) -> u64 {
        let cursor = self.grid.cursor();
        let id = self.marks_state.next_mark_id;
        self.marks_state.next_mark_id += 1;
        let row = self.grid.visible_to_absolute(cursor.row);
        let mark = TerminalMark::new(id, row, cursor.col);
        // FIFO eviction if at capacity
        if self.marks_state.marks.len() >= TERMINAL_MARKS_MAX {
            self.marks_state.marks.pop_front();
        }
        self.marks_state.marks.push_back(mark);
        self.marks_state.marks.make_contiguous();
        id
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

    /// Clear all terminal marks.
    pub fn clear_terminal_marks(&mut self) {
        self.marks_state.marks.clear();
    }

    /// Get all annotations (OSC 1337 AddAnnotation).
    ///
    /// Annotations are metadata/notes attached to specific regions of
    /// terminal output. They can be visible or hidden.
    #[must_use]
    pub fn annotations(&self) -> &[Annotation] {
        self.marks_state.annotations.as_slices().0
    }

    /// Get visible annotations only.
    pub fn visible_annotations(&self) -> impl Iterator<Item = &Annotation> {
        self.marks_state.annotations.iter().filter(|a| !a.hidden)
    }

    /// Get annotations at a specific row.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert
    ///   screen-relative row coordinates)
    pub fn annotations_at_row(&self, row: u64) -> impl Iterator<Item = &Annotation> {
        self.marks_state
            .annotations
            .iter()
            .filter(move |a| a.row == row)
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

    /// Add a hidden annotation at the current cursor position.
    pub fn add_hidden_annotation(&mut self, message: &str) -> u64 {
        let cursor = self.grid.cursor();
        let id = self.marks_state.next_annotation_id;
        self.marks_state.next_annotation_id += 1;
        let row = self.grid.visible_to_absolute(cursor.row);
        let annotation = Annotation::new_hidden(id, row, cursor.col, message.to_string());
        // FIFO eviction if at capacity
        if self.marks_state.annotations.len() >= ANNOTATIONS_MAX {
            self.marks_state.annotations.pop_front();
        }
        self.marks_state.annotations.push_back(annotation);
        self.marks_state.annotations.make_contiguous();
        id
    }

    /// Clear all annotations.
    pub fn clear_annotations(&mut self) {
        self.marks_state.annotations.clear();
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
    /// Calling this is also what makes the metric REPORTABLE: it is the only
    /// writer of the flag [`Self::host_cell_pixel_size`] reads, and the XTWINOPS
    /// pixel reports (CSI 14 t / 16 t) answer from that and stay silent until a
    /// host has spoken. A host with real font metrics should call this as soon as
    /// they exist and again whenever they change (font zoom, a DPI move).
    pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
        self.iterm2.cell_px = (width, height);
        self.iterm2.cell_px_from_host = true;
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

    /// The cell pixel size a HOST actually reported, or `None` if none has.
    ///
    /// The engine cannot measure a glyph — only the frontend holding the
    /// rasterizer can. So this is `None` for a headless engine, and for a live
    /// session whose renderer has not published metrics yet. A zero on either
    /// axis is treated as unreported too: it is not a cell box any font could
    /// have, and reporting it would tell an application to divide by zero.
    ///
    /// Callers that answer a wire QUERY must use this and stay silent on `None`
    /// rather than fall back to [`Self::cell_pixel_size`]; see
    /// `handler_window`'s XTWINOPS pixel reports.
    #[must_use]
    pub fn host_cell_pixel_size(&self) -> Option<(u16, u16)> {
        self.iterm2.host_cell_px()
    }

    /// Get all user variables (OSC 1337 SetUserVar).
    ///
    /// User variables are key-value pairs set by applications for
    /// shell integration and customization purposes.
    #[must_use]
    pub fn user_vars(&self) -> &super::UserVarsMap {
        &self.iterm2.user_vars
    }

    /// A specific user variable by key.
    #[must_use]
    pub fn user_var(&self, key: &str) -> Option<&String> {
        self.iterm2.user_vars.get(key)
    }

    /// Set a user variable.
    ///
    /// Capped at `USER_VARS_MAX` entries. Updating an existing key keeps its
    /// position and never evicts. Inserting a *new* key when the map is full
    /// evicts the oldest entry first (deterministic FIFO order, tracked in
    /// `user_vars_order`) — the backing `HashMap`'s iteration order is
    /// non-deterministic and must not be used for eviction.
    pub fn set_user_var(&mut self, key: &str, value: &str) {
        if self.iterm2.user_vars.contains_key(key) {
            // Update in place; insertion order is unchanged.
            self.iterm2
                .user_vars
                .insert(key.to_string(), value.to_string());
            return;
        }
        // New key: evict the oldest entry first if at capacity.
        if self.iterm2.user_vars.len() >= USER_VARS_MAX {
            while let Some(evict_key) = self.iterm2.user_vars_order.pop_front() {
                if self.iterm2.user_vars.remove(&evict_key).is_some() {
                    break;
                }
                // Stale order entry (key already removed) — keep popping.
            }
        }
        self.iterm2
            .user_vars
            .insert(key.to_string(), value.to_string());
        self.iterm2.user_vars_order.push_back(key.to_string());
    }

    /// Remove a user variable.
    pub fn remove_user_var(&mut self, key: &str) -> Option<String> {
        let removed = self.iterm2.user_vars.remove(key);
        if removed.is_some() {
            self.iterm2.user_vars_order.retain(|k| k != key);
        }
        removed
    }

    /// Clear all user variables.
    pub fn clear_user_vars(&mut self) {
        self.iterm2.user_vars.clear();
        self.iterm2.user_vars_order.clear();
    }

    /// Get the current taskbar progress state (ConEmu OSC 9;4).
    ///
    /// Returns the last progress state set by the application, or None
    /// if no progress has been set.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    #[test]
    fn set_user_var_evicts_oldest_first_deterministically() {
        let mut term = Terminal::new(24, 80);
        // Fill to capacity in a known insertion order: k0..k(MAX-1).
        for i in 0..USER_VARS_MAX {
            term.set_user_var(&format!("k{i}"), &format!("v{i}"));
        }
        assert_eq!(term.user_vars().len(), USER_VARS_MAX);

        // Inserting a new key at capacity must evict the OLDEST key (k0),
        // deterministically — not an arbitrary HashMap entry.
        term.set_user_var("new", "value");
        assert_eq!(term.user_vars().len(), USER_VARS_MAX);
        assert_eq!(term.user_var("k0"), None, "oldest key must be evicted");
        assert_eq!(term.user_var("k1").map(String::as_str), Some("v1"));
        assert_eq!(term.user_var("new").map(String::as_str), Some("value"));

        // Next insert evicts k1 (the new oldest).
        term.set_user_var("new2", "value2");
        assert_eq!(term.user_var("k1"), None, "second-oldest evicted next");
        assert_eq!(term.user_var("k2").map(String::as_str), Some("v2"));
    }

    #[test]
    fn set_user_var_update_existing_does_not_evict() {
        let mut term = Terminal::new(24, 80);
        for i in 0..USER_VARS_MAX {
            term.set_user_var(&format!("k{i}"), &format!("v{i}"));
        }
        // Updating an existing key at capacity must not evict anything.
        term.set_user_var("k0", "updated");
        assert_eq!(term.user_vars().len(), USER_VARS_MAX);
        assert_eq!(term.user_var("k0").map(String::as_str), Some("updated"));
        assert_eq!(
            term.user_var("k1").map(String::as_str),
            Some("v1"),
            "updating an existing key must not evict another"
        );
    }

    #[test]
    fn remove_user_var_keeps_order_queue_in_sync() {
        let mut term = Terminal::new(24, 80);
        term.set_user_var("a", "1");
        term.set_user_var("b", "2");
        term.set_user_var("c", "3");
        // Remove the oldest explicitly; eviction must then start from "b".
        assert_eq!(term.remove_user_var("a").as_deref(), Some("1"));

        // Fill exactly up to capacity (we currently hold b, c = 2 entries, so
        // add USER_VARS_MAX - 2 fillers). No eviction yet.
        for i in 0..(USER_VARS_MAX - 2) {
            term.set_user_var(&format!("fill{i}"), "x");
        }
        assert_eq!(term.user_vars().len(), USER_VARS_MAX);
        assert_eq!(term.user_var("b").map(String::as_str), Some("2"));

        // One more new key at capacity must evict "b" (the oldest remaining),
        // proving the order queue dropped the removed "a" cleanly.
        term.set_user_var("trigger", "t");
        assert_eq!(term.user_vars().len(), USER_VARS_MAX);
        assert_eq!(term.user_var("b"), None, "b evicted as new oldest");
        assert_eq!(term.user_var("c").map(String::as_str), Some("3"));
        assert_eq!(term.user_var("trigger").map(String::as_str), Some("t"));
    }

    /// `completed_command_seq` strictly increases once per OSC 133;D — two
    /// commands completing within the same MILLISECOND (indistinguishable by
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
}
