// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Replayable ANSI serialization of a [`Terminal`]: the one body behind both
//! web bindings' `serialize` / `serialize_scrollback` (aterm-wasm's CPU
//! `AtermTerminal` and aterm-gpu-web's `AtermGpuTerminal`).

use std::fmt::Write as _;

use super::Terminal;

impl Terminal {
    /// Serialize the terminal to a REPLAYABLE ANSI string — the aterm-native
    /// replacement for `@xterm/addon-serialize`'s `serialize({scrollback})`, so a
    /// renderer needs no shadow xterm.js buffer to snapshot/restore/fork a pane.
    /// Layout: SGR reset, then the capped recent history (text + CRLF), then
    /// `CSI H`, then each of the `rows` visible rows placed with absolute CUP +
    /// erase-line (so a full-width row can't autowrap on replay) emitted via the
    /// grid's `row_ansi_text_screen` (minimal change-based SGR, wide-char aware),
    /// then the cursor restored. `scrollback_rows` = `None` prepends ALL history,
    /// `Some(n)` the last `n`, `Some(0)` viewport-only. Byte-compatible with
    /// the daemon's `serialize_ansi` replay pipeline.
    #[must_use]
    pub fn serialize_ansi(&self, rows: usize, scrollback_rows: Option<u32>) -> String {
        let grid = self.grid();
        let cap = scrollback_rows.map(|n| n as usize);
        let active_history = grid.scrollback_lines();
        let take = cap.map_or(active_history, |n| n.min(active_history));
        // Pre-size: `take` reaches the construction default of 100k lines, and a
        // doubling `String` would re-copy the whole (multi-megabyte) output ~two
        // dozen times on the way there.
        let mut out = String::with_capacity(
            take.saturating_mul(8)
                .saturating_add(rows.saturating_mul(16)),
        );
        out.push_str("\x1b[0m");
        for i in (active_history - take)..active_history {
            // Bind the `Cow<'_, Line>` so `as_str` borrows instead of copying
            // each line. A missing or non-UTF-8 line contributes only its CRLF.
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        if take > 0 {
            // Scroll the printed history OFF the screen so it lands in the replay
            // target's scrollback: the trailing printed lines are still on the
            // visible grid here, and the absolute-CUP viewport paint below would
            // ERASE them — losing a viewport-sized chunk of history on every
            // replay (all of it when take < rows). One LF per resident text line
            // (at most rows-1: the final CRLF left the bottom row blank) from the
            // bottom row scrolls each top line into history and leaves a clean
            // screen for the viewport paint.
            let _ = write!(out, "\x1b[{rows};1H");
            for _ in 0..take.min(rows.saturating_sub(1)) {
                out.push('\n');
            }
        }
        out.push_str("\x1b[H");
        for r in 0..u16::try_from(rows).unwrap_or(u16::MAX) {
            let _ = write!(out, "\x1b[{};1H\x1b[K", r + 1);
            if let Some(row_ansi) = grid.row_ansi_text_screen(r) {
                out.push_str(&row_ansi);
            }
            out.push_str("\x1b[0m");
        }
        let c = self.cursor();
        let _ = write!(out, "\x1b[{};{}H", c.row as usize + 1, c.col as usize + 1);
        out
    }

    /// Scrollback HISTORY ONLY (the off-screen lines above the viewport) as
    /// flowing text + CRLF, no cursor/grid framing. Reads the MAIN buffer's
    /// scrollback (kept in the inactive grid while the alt screen is active) so
    /// an in-alt (vim/htop/less) snapshot still recovers the pre-TUI history —
    /// the only recoverable history on cold-restore of an alt-screen session.
    /// `max_rows` caps to the last `n` lines (`None` = all). Mirrors the daemon's
    /// `serialize_scrollback_ansi`.
    #[must_use]
    pub fn serialize_scrollback_ansi(&self, max_rows: Option<u32>) -> String {
        let grid = self.main_grid();
        let history = grid.scrollback_lines();
        if history == 0 {
            return String::new();
        }
        let take = max_rows.map_or(history, |n| (n as usize).min(history));
        let mut out = String::with_capacity(take.saturating_mul(8));
        for i in (history - take)..history {
            // Same borrow-don't-copy discipline as `serialize_ansi`.
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        out
    }
}
