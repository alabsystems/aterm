// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Replayable ANSI serialization of a [`Terminal`], shared by both web bindings'
//! `serialize` / `serialize_scrollback` (aterm-wasm and aterm-gpu-web).

use std::fmt::Write as _;

use super::Terminal;

impl Terminal {
    /// Serialize the terminal to a replayable ANSI string (the aterm-native
    /// `@xterm/addon-serialize` `serialize({scrollback})`), so a renderer needs no shadow
    /// xterm.js buffer to snapshot, restore or fork a pane. Layout: SGR reset, the capped
    /// recent history (text + CRLF), `CSI H`, each visible row placed with absolute CUP +
    /// erase-line (so a full-width row cannot autowrap on replay) via the grid's
    /// `row_ansi_text_screen`, then the cursor (its row printed again when its wrap is
    /// deferred, which a CUP would clear). `scrollback_rows`: `None` prepends all
    /// history, `Some(n)` the last `n`, `Some(0)` none.
    #[must_use]
    pub fn serialize_ansi(&self, rows: usize, scrollback_rows: Option<u32>) -> String {
        let grid = self.grid();
        let cap = scrollback_rows.map(|n| n as usize);
        let active_history = grid.scrollback_lines();
        let take = cap.map_or(active_history, |n| n.min(active_history));
        // `take` reaches 100k lines by default; a doubling `String` would re-copy the
        // multi-megabyte output ~two dozen times on the way.
        let mut out = String::with_capacity(
            take.saturating_mul(8)
                .saturating_add(rows.saturating_mul(16)),
        );
        out.push_str("\x1b[0m");
        for i in (active_history - take)..active_history {
            // The `Cow` is bound so `as_str` borrows instead of copying. A missing or
            // non-UTF-8 line contributes only its CRLF.
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        if take > 0 {
            // Scroll the printed history off the screen into the replay target's
            // scrollback, or the viewport paint below would erase the trailing lines still
            // on screen: one LF per resident line (at most rows-1; the final CRLF left the
            // bottom row blank) from the bottom row.
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
        let cols = grid.cols();
        // A cursor on the last column with its wrap deferred (the program's output ended
        // at the right margin) is re-armed by printing its row again, last: a CUP clears
        // the deferral, so the replay's next glyph would overwrite that column instead of
        // wrapping. Only a row printed out to that column re-arms it, and without DECAWM
        // the deferral is never acted on.
        if grid.pending_wrap()
            && self.modes().auto_wrap
            && c.col.saturating_add(1) == cols
            && grid.row_at_screen(c.row).is_some_and(|r| r.len() == cols)
            && let Some(row_ansi) = grid.row_ansi_text_screen(c.row)
        {
            let _ = write!(out, "\x1b[{};1H\x1b[K", c.row as usize + 1);
            out.push_str(&row_ansi);
            out.push_str("\x1b[0m");
        } else {
            let _ = write!(out, "\x1b[{};{}H", c.row as usize + 1, c.col as usize + 1);
        }
        out
    }

    /// Scrollback history only (the lines above the viewport) as text + CRLF, no cursor or
    /// grid framing. Reads the main buffer's scrollback, kept in the inactive grid while the
    /// alt screen is up, so an in-alt (vim/htop/less) snapshot still recovers the pre-TUI
    /// history. `max_rows` keeps the last `n` lines (`None` = all).
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
            let line = grid.get_history_line(i);
            if let Some(s) = line.as_ref().and_then(|l| l.as_str()) {
                out.push_str(s.trim_end());
            }
            out.push_str("\r\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::Terminal;

    /// The rows after `next` lands on `t`, and on a fresh terminal fed `t`'s serialization.
    fn after_replay(rows: u16, cols: u16, input: &str, next: &str) -> (Vec<String>, Vec<String>) {
        let mut a = Terminal::new(rows, cols);
        a.process(input.as_bytes());
        let mut b = Terminal::new(rows, cols);
        b.process(a.serialize_ansi(usize::from(rows), Some(0)).as_bytes());
        let text = |t: &mut Terminal| {
            t.process(next.as_bytes());
            (0..usize::from(rows))
                .map(|r| t.row_text(r).unwrap_or_default())
                .collect()
        };
        (text(&mut a), text(&mut b))
    }

    #[test]
    fn a_replay_wraps_where_the_source_would() {
        // Output that ended at the right margin defers its wrap: the next glyph goes
        // on the next row, in the replay as in the source.
        for (rows, input) in [
            (3, "abcde"),
            (2, "12345\r\nabcde"),
            (3, "abc\u{4E2D}"),
            (3, "\x1b[31mabcd\x1b[44me"),
        ] {
            let (source, replay) = after_replay(rows, 5, input, "Z");
            assert_eq!(source, replay, "{input:?}");
        }
        // Controls: no deferred wrap, and DECAWM off (the next glyph overwrites).
        for input in ["abcd", "\x1b[?7labcde"] {
            let (source, replay) = after_replay(3, 5, input, "Z");
            assert_eq!(source, replay, "{input:?}");
        }
    }
}
