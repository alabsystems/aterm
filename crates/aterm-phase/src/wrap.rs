// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE LINE INK WRAPPED OVER SEVERAL ROWS, read as the one line it is.
//!
//! In a pane narrower than a line of its text, Ink word-wraps the line onto
//! the rows under it, each at the column the line's text starts at. Measured
//! on Claude Code 2.1.295 and 2.1.299 in `tools/real-claude-lab` (the
//! fixtures `claude-2.1.29x-*-NNcol.txt`):
//! * a box's key-hint footer (`Enter to confirm · Esc to` over `cancel` at
//!   34 columns; [`crate::prompt`], "A WRAPPED FOOTER");
//! * the footer's usage-limit banner, at its own column (`⚠ Usage limit
//!   reached · limit` over `resets 10:25am` at 34 columns, `⚠ Usage limit
//!   reached` over `· limit resets 10:51am` at 26), and the countdown under
//!   it at its column four (`Continuing automatically at` over `10:25am · esc
//!   to cancel`; at 26, `Continuing` alone on its first row);
//! * a turn's `⎿` notice, at the gutter's text column five (`⎿  You've hit
//!   your` over `session limit ·` at 26 columns).
//!
//! Read a row at a time such a line says less than it does, or nothing: the
//! banner named no reset, so the harness waited on its 30-minute backstop
//! and, after Esc on the countdown, typed `keep going` into the standing
//! limit at once; the turn's notice was no wall at all (the 2026-10-09
//! sweep, measured again 2026-10-14). So every reader of such a line reads it
//! across its rows: [`continues`] is the one rule for "this row is the rest
//! of the line above it", [`line_end`] finds a line's last row by it, and
//! [`rejoined`] joins the rows. Where those readers are: the box's footer
//! ([`crate::prompt`]); the wall's footer item, banner, countdown, `⎿`
//! notice, the vendor's word on its own auto-continue and the `⎿  Login
//! successful` that lifts a login wall ([`crate::phase`]'s `line_at`,
//! [`crate::wall`]).
//!
//! A `⏺` message's line is its paragraph, the rows under it at column two
//! (`phase`'s `message_end`, as `error_row_notice` always read it). Claude
//! Code draws its own error row and dotted notices in a column ten columns
//! short of the pane (2.1.300's `su` and `ig`: `columns - 10`; measured on
//! 2.1.299 at 26 and 34 columns), and the worker's reply to the pane's edge
//! (`⏺ Hi! I am the local lab stub, not` fills a 34-column row). This rule
//! reads a message's rows only in the vendor's own column, to count the rows
//! one of its notices would take at 80 columns ([`rows_in`], `phase`'s
//! `notice_rows_at_80`).
//!
//! THE LIMITS. The rule cannot tell a line Ink wrapped from the NEXT line of
//! the same text drawn at the same column under a row that happens to be
//! full: at 34 columns `(America/Los_Angeles)` over `/upgrade to increase
//! your` is two lines of the notice, read as one. So a reader joins a line
//! to read what it says — its kind, its reset — and bounds what it takes by
//! the text's own grammar, never by where the row ended: a reset is the time
//! a notice names and no more (`phase`'s `time_after`). And a word longer
//! than its row is broken where the row ends (`cancelled · /rat` over
//! `e-limit-options`): joined with a space, the word reads in two halves.

use crate::phase::leading_spaces;
use crate::question::cells;

/// Whether row `below` is the rest of a line Ink wrapped, the line's text on
/// the row above it being `above` (trimmed), drawn from column `col` in a box
/// `room` cells wide ([`room`]): `below` starts at `col`, both are words —
/// no rule, frame or block glyph (U+2500–U+259F), so a composer's rule under
/// a footer is never its wrap — and `above` is too full to take `below`'s
/// first word, with two cells of slack, as the question dialog's measured
/// labels wrap ([`crate::question`]).
pub(crate) fn continues(above: &str, col: usize, below: &str, room: usize) -> bool {
    let rest = crate::text::trim(below);
    let drawn = |t: &str| t.chars().any(|c| ('\u{2500}'..='\u{259F}').contains(&c));
    if above.is_empty()
        || rest.is_empty()
        || leading_spaces(below) != col
        || drawn(above)
        || drawn(rest)
    {
        return false;
    }
    let word = rest.split(' ').next().map_or(0, cells);
    cells(above) + 1 + word + 2 > room
}

/// The room a line drawn from column `col` of a pane `width` cells wide has
/// in a box as wide as the pane less `col` on each side: a footer's, a
/// banner's, a `⎿` notice's (whose box ends at the pane's edge: the rule
/// reads such a row full a little early).
pub(crate) fn room(col: usize, width: usize) -> usize {
    width.saturating_sub(2 * col)
}

/// The last row of the line row `first` opens, its text there `head`
/// (trimmed) from column `col`, in a pane `width` cells wide ([`room`]): each
/// row after it, short of row `until`, that [`continues`] the row above it.
/// `first` itself when none does.
pub(crate) fn line_end(
    rows: &[String],
    first: usize,
    head: &str,
    col: usize,
    width: usize,
    until: usize,
) -> usize {
    let until = until.min(rows.len());
    let room = room(col, width);
    let (mut end, mut above) = (first, head);
    while end + 1 < until && continues(above, col, &rows[end + 1], room) {
        end += 1;
        above = crate::text::trim(&rows[end]);
    }
    end
}

/// How many rows Ink's word wrap gives `line` (its words one space apart)
/// in a box `room` cells wide, as wrap-ansi draws Ink's text (`hard`, no
/// `trim`): greedily, each word on the row it fits on, the space before it
/// counted even at a row's start once the row above is exactly full (at 26
/// columns `automatically at` fills its 16 cells, and `10:51am · esc` breaks
/// before `to`); a word wider than the box broken where the rows end, from
/// the row it opens on unless the next row breaks it fewer times
/// (`cancelled · /rat` over `e-limit-options`). Measured on 2.1.299 at 26
/// and 34 columns (`claude-2.1.299-*-NNcol.txt`).
pub(crate) fn rows_in(line: &str, room: usize) -> usize {
    let room = room.max(1);
    let (mut rows, mut used) = (1, 0);
    for (k, word) in line.split(' ').filter(|w| !w.is_empty()).enumerate() {
        let n = cells(word);
        if k > 0 {
            if used >= room {
                rows += 1;
                used = 0;
            }
            used += 1;
        }
        if n > room {
            let left = room.saturating_sub(used);
            let here = 1 + n.saturating_sub(left + 1) / room;
            if left == 0 || n.saturating_sub(1) / room < here {
                rows += 1;
                used = 0;
            }
            let last = (used + n).saturating_sub(1);
            rows += last / room;
            used = last % room + 1;
            continue;
        }
        if used + n > room && used > 0 {
            rows += 1;
            used = 0;
        }
        used += n;
    }
    rows
}

/// Rows `run`, one line Ink wrapped, as that line: the first row's indent,
/// then each row's text, joined with one space (Ink's word wrap breaks at a
/// space and draws none at either end of a row).
pub(crate) fn rejoined(run: &[String]) -> String {
    let mut out = " ".repeat(leading_spaces(&run[0]));
    for (k, r) in run.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.push_str(crate::text::trim(r));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    /// The rule on the rows Claude Code 2.1.299 drew at 34 columns: the
    /// banner's rest at its column, the countdown at its own, the gutter's
    /// text column under a `⎿` notice. NEGATIVE CONTROLS: a row at another
    /// column (the countdown under the banner), a row the row above had room
    /// for (the same rows at 144 columns), a rule, a blank row.
    #[test]
    fn a_row_continues_a_line_only_where_ink_could_have_wrapped_it() {
        let footer = rows(&[
            "  ⚠ Usage limit reached · limit",
            "  resets 10:25am",
            "    Continuing automatically at",
            "    10:25am · esc to cancel",
            "  ⏵⏵ auto mode on (shift+tab to",
        ]);
        let end = |i: usize, w: usize| {
            line_end(
                &footer,
                i,
                crate::text::trim(&footer[i]),
                leading_spaces(&footer[i]),
                w,
                footer.len(),
            )
        };
        assert_eq!(end(0, 34), 1);
        assert_eq!(end(2, 34), 3);
        assert_eq!(
            rejoined(&footer[0..=1]),
            "  ⚠ Usage limit reached · limit resets 10:25am"
        );
        assert_eq!(
            rejoined(&footer[2..=3]),
            "    Continuing automatically at 10:25am · esc to cancel"
        );
        // A `⎿` notice: the line's text from the gutter's column five.
        let notice = rows(&[
            "  ⎿  You've hit your session limit",
            "     · resets 10:25am",
            "",
            "     not this",
        ]);
        assert_eq!(
            line_end(&notice, 0, "You've hit your session limit", 5, 34, 4),
            1
        );
        // NEGATIVE CONTROLS. The same rows where the row above had room.
        assert_eq!(end(0, 144), 0);
        assert_eq!(end(2, 144), 2);
        assert!(!continues(
            "You've hit your session limit",
            5,
            "     · resets 10:25am",
            room(5, 144)
        ));
        // Another column, a rule, a blank row, `until`.
        assert!(!continues(
            "resets 10:25am",
            2,
            "    Continuing automatically at",
            20
        ));
        assert!(!continues(
            "Enter to confirm · Esc to cancel",
            2,
            "  ────────────────",
            20
        ));
        assert!(!continues("· resets 10:25am", 5, "", 20));
        assert_eq!(
            line_end(&notice, 0, "You've hit your session limit", 5, 34, 1),
            0
        );
    }

    /// Ink's wrap counted as Claude Code 2.1.299 drew it in its notice column
    /// (`width - 10` cells, `phase`'s `NOTICE_MARGIN`): the rows of each
    /// measured `⏺` notice at 26 and 34 columns (the fixtures
    /// `claude-2.1.299-*-NNcol.txt`), the exactly full row's next row
    /// (`automatically at` over `10:51am · esc`, which breaks before `to`)
    /// and the word wider than its row (`cancelled · /rat`) included; and the
    /// counts at 80 columns, where the notice takes two rows.
    #[test]
    fn rows_in_counts_inks_wrap_as_measured() {
        let at_26 = "Usage limit reached · continuing automatically at 10:51am · esc to cancel";
        let at_34 = "Usage limit reached · continuing automatically at 10:25am · esc to cancel";
        let cancel = "Automatic continue cancelled · /rate-limit-options to re-arm";
        let overloaded = "API Error: 529 Overloaded. This is a server-side issue, usually \
                          temporary — try again in a moment. If it persists, check your \
                          inference gateway (127.0.0.1:58931).";
        assert_eq!(rows_in(at_26, 16), 6);
        assert_eq!(rows_in(at_34, 24), 4);
        assert_eq!(rows_in(cancel, 16), 5);
        assert_eq!(rows_in(overloaded, 24), 8);
        assert_eq!(rows_in(at_34, 70), 2);
        assert_eq!(rows_in(overloaded, 70), 3);
        // The exactly full row: `to` would fit in the 16 cells after
        // `10:51am · esc` but for the space wrap-ansi keeps at the row's start.
        assert_eq!(rows_in("automatically at 10:51am · esc to", 16), 3);
        assert_eq!(rows_in("automatically 10:51am · esc to", 16), 2);
        // One word, and nothing.
        assert_eq!(rows_in("e-limit-options", 16), 1);
        assert_eq!(rows_in("", 16), 1);
    }
}
