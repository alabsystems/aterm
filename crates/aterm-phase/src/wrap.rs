// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE LINE INK WRAPPED OVER SEVERAL ROWS, read as the one line it is.
//!
//! In a pane narrower than a line of its text, Ink word-wraps the line onto
//! the rows under it, each at the column the line's text starts at. Measured
//! on Claude Code 2.1.295 and 2.1.299 in the real-Claude lab (the
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
//! And on 2.1.300 (the harness sweep of 2026-10-15, ATERM-KI-1015-1; the
//! fixtures `claude-2.1.300-*-NNcol.txt`):
//! * a box's TITLE (`What do you want to` over `do?`, the usage-limit
//!   dialog at 26 columns; [`crate::prompt`], "A WRAPPED TITLE": joined only
//!   where its row alone is no whole title, and only as far as the line is
//!   one, for a box's body is drawn under its title at the same column), and
//!   a plan approval's option label (`1. Yes, and use auto` over `mode`);
//! * the plan dialog's key-hint row under its options (`ctrl+g to edit in
//!   Vim ·` over the plan file's path, below about 70 columns;
//!   `crate::prompt`'s `last_hint`);
//! * the status row, the spinner's at its own column (`✻ Choreogr` over
//!   `aphing…` at 10 columns; `·` alone over `Precipitat` over `ing…`; `✻`
//!   over `Calculatin` over `g` over `…`, broken between its spans: [`run_end`]),
//!   a done row's at its text column two (`✻ Baked for 0s · done 7:06` over
//!   `  AM`; `phase`'s `status_line_end`: never under a row already busy
//!   alone, as a full retry row is, which has the vendor's own notice at
//!   column two under it);
//! * the session survey's question at column two (`● How is Claude doing
//!   this` over `session? (optional)`; [`crate::survey`]) — and its OPTIONS,
//!   a row of four boxes Ink lays out side by side, each box's text wrapped
//!   inside the box (`  1: Bad2:    3:    0: Dis` over `        Fine  Good
//!   miss` at 26): [`rejoined_cells`] reads each box's rows as its line.
//!
//! Read a row at a time such a line says less than it does, or nothing: the
//! banner named no reset, so the harness waited on its 30-minute backstop
//! and, after Esc on the countdown, typed `keep going` into the standing
//! limit at once; the turn's notice was no wall at all (the 2026-10-09
//! sweep, measured again 2026-10-14). So every reader of such a line reads it
//! across its rows: [`continues`] is the one rule for "this row is the rest
//! of the line above it", [`line_end`] finds a line's last row by it, and
//! [`rejoined`] joins the rows. Where those readers are: the box's footer,
//! its title, the plan dialog's hint and its options' labels
//! ([`crate::prompt`]); the wall's footer item, banner, countdown, `⎿`
//! notice, the vendor's word on its own auto-continue and the `⎿  Login
//! successful` that lifts a login wall ([`crate::phase`]'s `line_at`,
//! [`crate::wall`]); the status row ([`crate::phase`]'s `status_line_end`);
//! the rating card's question and options ([`crate::survey`]).
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
//! a notice names and no more (`phase`'s `time_after`); a title is a header
//! its grammar reads or a dialog title the anchor table names, and a row that
//! is one alone takes no row under it (`prompt`'s `title_line`: a file box's
//! long path under `Edit file` reads to this rule as the title's rest); a
//! status row already busy alone takes no row at column two. And a word longer
//! than its row is broken where the row ends (`cancelled · /rat` over
//! `e-limit-options`): joined with a space, the word reads in two halves.
//! A line drawn in spans of their own (the spinner's shimmering verb) is
//! broken between spans as well as where a row is full (`g` over `…` at 10
//! columns): the rule misses that break, and the status row is read by
//! [`run_end`] instead — its rows at its column, up to the first row that
//! opens a block of its own.

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
    wraps_with(above, 0, col, below, room, 2)
}

/// [`continues`] with no slack, for a caller that says its room to the cell
/// and counts the row above as wrap-ansi filled it: `above` (its text) and
/// the `lead` cells before it, [`lead`]'s, could not have taken `below`'s
/// first word, a space before it, in `room` cells — where Ink breaks: its
/// wrap-ansi moves a word that does not fit beside the row to the next one,
/// and keeps one that fits (`hard` only breaks a word longer than a whole
/// row). An unnumbered
/// option's label (`crate::prompt`'s `unnumbered`), in the room a cell short
/// of the pane's edge: Ink wraps one at the edge (MEASURED on 2.1.302's MCP
/// server and folder-trust dialogs), and aterm's hand-built held-message
/// frames a cell short of it (MODELLED), so a row is read as the rest where
/// its first word has no room a cell short of the edge, where Ink could have
/// wrapped it at either.
pub(crate) fn continues_exactly(
    above: &str,
    lead: usize,
    col: usize,
    below: &str,
    room: usize,
) -> bool {
    wraps_with(above, lead, col, below, room, 0)
}

/// The cells wrap-ansi spent on a row before its text: 1 where the row above
/// it, `above` cells (its own lead counted), is at or past `room`, else 0.
/// At it: the row exactly full in a box `room` wide, as Ink's wrap fills
/// one. Past it: a row read in a room a cell short of where Ink wrapped it,
/// as `crate::prompt`'s `unnumbered` reads a label (a cell short of the
/// pane's edge), that is full to that edge — at 31 columns the MCP server
/// dialog's `Use this and all future MCP`, 27 cells against 26 (MEASURED on
/// 2.1.302) — under which Ink's row starts with the space too. A label row
/// exactly full a cell short of the edge is counted so as well, though Ink,
/// wrapping at the edge, starts the row under it with none: the room is read
/// at the nearer of the two edges Ink could have wrapped at.
/// Ink wraps its text with wrap-ansi, `trim` off (READ in
/// 2.1.302's bundle: `Ku` calls `Bun.wrapAnsi(text, room, {trim: false, hard:
/// true})`), which starts the row under a full row with the space before its
/// next word; Ink's `Vb` then slices that one space off the row as it draws
/// it, at the line's column. So the space is drawn nowhere and still takes a
/// cell of that row: MEASURED on 2.1.302 (the MCP server dialog's title at 27
/// columns, `New MCP server found in` filling its 23 cells over `this
/// project:` over `lab-probe`, which fits beside `this project:` only
/// without the space; its body at 21, `require approval.` over `Learn more
/// in` over `the MCP`; the model picker's `0 per Mtok` over `($0.50/$2` over
/// `.50 for`) and on 2.1.299 ([`rows_in`]).
pub(crate) fn lead(above: usize, room: usize) -> usize {
    usize::from(above >= room)
}

/// [`continues`] with `slack` cells of it, the row above `lead` cells into
/// the room before its text.
fn wraps_with(
    above: &str,
    lead: usize,
    col: usize,
    below: &str,
    room: usize,
    slack: usize,
) -> bool {
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
    lead + cells(above) + 1 + word + slack > room
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

/// THE FIRST ROW OF THE LINE ROW `last` ENDS, in a pane `width` cells wide,
/// read back from `last` over the rows a word wrap broke the line onto: each
/// row above that the row under it [`continues`], at the column `last`'s text
/// starts at, up to the row that OPENS the line — its text at that column
/// past a mark of its own ([`text_column`]: a reply's `•`, `✦`, `◆` or `⏺`, a
/// notice's `⎿`, a list's `-` or `1.`) — which is the first. `last` itself
/// where it opens its line, or no row above continues it. MEASURED at 50
/// columns on the agents' replies (the `*-lab-reply-wrapped-50col` captures
/// of 2026-10-16): Claude Code, Gemini CLI and Muse Code wrap a paragraph at the pane's edge, agy a cell or two short of it, and
/// each draws every row of it at the column its first row's text starts at
/// (two in). The rule's limit (module header, "THE LIMITS") holds here too:
/// a line that ended under a row full enough to have been wrapped reads as
/// one with the line under it.
#[must_use]
pub fn line_start(rows: &[String], last: usize, width: usize) -> usize {
    let Some(row) = rows.get(last) else {
        return last;
    };
    let col = leading_spaces(row);
    if text_column(row) != col {
        return last;
    }
    let room = room(col, width);
    let mut first = last;
    while first > 0 {
        let above = rows[first - 1].as_str();
        let lead = leading_spaces(above);
        let opens = lead < col;
        if lead > col || (opens && text_column(above) != col) {
            break;
        }
        let text: String = if opens {
            above.chars().skip(col).collect()
        } else {
            above.to_string()
        };
        if !continues(crate::text::trim(&text), col, &rows[first], room) {
            break;
        }
        first -= 1;
        if opens {
            break;
        }
    }
    first
}

/// The column a row's TEXT starts at: past its indent and, where the first
/// thing on it is a MARK and a space — one glyph that is no letter or digit
/// (`•`, `✦`, `◆`, `⏺`, `⎿`, `-`), or a list's number and its `.` or `)` —
/// past the mark and the spaces after it.
fn text_column(row: &str) -> usize {
    let lead = leading_spaces(row);
    let rest = crate::text::trim_start(row);
    let Some((mark, after)) = rest.split_once(' ') else {
        return lead;
    };
    let mut glyphs = mark.chars();
    let one_glyph = glyphs
        .next()
        .is_some_and(|c| !c.is_alphanumeric() && glyphs.next().is_none());
    let numbered = mark
        .strip_suffix(['.', ')'])
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if !(one_glyph || numbered) {
        return lead;
    }
    lead + mark.chars().count() + 1 + leading_spaces(after)
}

/// The last row of a line Ink broke at the edges of its SPANS rather than
/// where a row was full (module header, "THE LIMITS"): row `first` and each
/// row after it, short of row `until`, that is not blank, starts at column
/// `col` and draws no rule, frame or block glyph. The spinner's verb is drawn
/// in spans whose colours move across it (its shimmer), and in a pane
/// narrower than the line Ink breaks it between them as often as where a row
/// is full (MEASURED on 2.1.300 at 10 columns: `✻` over `Calculatin` over
/// `g` over `…`, the ellipsis alone on its row; `phase`'s
/// `status_line_end`). [`continues`] cannot see such a break, so the caller
/// bounds the run: `until` is the first row that opens a block of its own.
pub(crate) fn run_end(rows: &[String], first: usize, col: usize, until: usize) -> usize {
    let until = until.min(rows.len());
    let drawn = |t: &str| t.chars().any(|c| ('\u{2500}'..='\u{259F}').contains(&c));
    let mut end = first;
    while end + 1 < until && {
        let r = &rows[end + 1];
        let t = crate::text::trim(r);
        !t.is_empty() && leading_spaces(r) == col && !drawn(t)
    } {
        end += 1;
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

/// Rows `run`, ONE ROW OF BOXES Ink lays out side by side (a flex row: the
/// session survey's options), each box's text wrapped inside its own box,
/// read as each box's line: box `k` starts at cell `starts[k]` on every row
/// and runs to the next box's start, the last one to `width` (never before
/// its own start, so a degenerate screen cannot underflow the box-local
/// width). A box's rows
/// are joined as [`rejoined`] joins a line, with one space — save where the
/// row above FILLS its box: Ink broke a word longer than the box there, and
/// the halves join with nothing between them (`0: Dis` over `miss` in a box
/// six cells wide). `None` when a row under the first is none of the boxes'
/// rows: text left of the first box, or a box's text that starts anywhere
/// but at its box's column.
pub(crate) fn rejoined_cells(
    run: &[String],
    starts: &[usize],
    width: usize,
) -> Option<Vec<String>> {
    let (&first, rest) = starts.split_first()?;
    // Starts are strictly increasing. Clamp the final end to its own start
    // when the pane is narrower, keeping every box width nonnegative.
    let last_start = *rest.last().unwrap_or(&first);
    let ends: Vec<usize> = rest
        .iter()
        .copied()
        .chain([width.max(last_start)])
        .collect();
    let mut texts = vec![String::new(); starts.len()];
    let mut full = vec![false; starts.len()];
    for (k, row) in run.iter().enumerate() {
        let (left, mut row) = crate::question::split_at_cell(row, first);
        if !crate::text::trim(left).is_empty() {
            return None;
        }
        for (b, &end) in ends.iter().enumerate() {
            let (cell, after) = crate::question::split_at_cell(row, end - starts[b]);
            row = after;
            let text = crate::text::trim_end(cell);
            if text.is_empty() {
                continue;
            }
            // A box's own row starts at its column; the first row may hold a
            // box's text that ran into the next box (`1: Bad2:`), never one
            // under it.
            if k > 0 && leading_spaces(text) > 0 {
                return None;
            }
            let text = crate::text::trim(text);
            if !texts[b].is_empty() && !full[b] {
                texts[b].push(' ');
            }
            texts[b].push_str(text);
            full[b] = cells(cell.trim_end()) >= end - starts[b];
        }
    }
    Some(texts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
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

    /// A row of boxes as Claude Code 2.1.300 draws the session survey's
    /// options in a narrow pane (the fixtures
    /// `claude-2.1.300-survey-session-NNcol.txt`): each box's rows joined —
    /// with a space where Ink broke at one, with none where the row filled
    /// its box (`0: Dis` over `miss` in a box six cells wide). NEGATIVE
    /// CONTROLS: a row whose text starts inside a box, text left of the first
    /// box.
    #[test]
    fn rejoined_cells_reads_each_box_of_a_wrapped_row() {
        let at_26 = rows(&["  1: Bad2:    3:    0: Dis", "        Fine  Good  miss"]);
        assert_eq!(
            rejoined_cells(&at_26, &[2, 8, 14, 20], 26),
            Some(
                ["1: Bad", "2: Fine", "3: Good", "0: Dismiss"]
                    .map(String::from)
                    .to_vec()
            )
        );
        let at_34 = rows(&[
            "  1: Bad  2: Fine 3: Good 0:",
            "                          Dismiss",
        ]);
        assert_eq!(
            rejoined_cells(&at_34, &[2, 10, 18, 26], 34),
            Some(
                ["1: Bad", "2: Fine", "3: Good", "0: Dismiss"]
                    .map(String::from)
                    .to_vec()
            )
        );
        // One row is its boxes as drawn.
        assert_eq!(
            rejoined_cells(&at_34[..1], &[2, 10, 18, 26], 34),
            Some(
                ["1: Bad", "2: Fine", "3: Good", "0:"]
                    .map(String::from)
                    .to_vec()
            )
        );
        // A degenerate screen whose last box key sits PAST a short pane rule
        // (an adversarial child: keys at 2,8,14,20 under a width-10 rule) must
        // not underflow the box-local width `end - starts[b]`; the last box
        // reads empty and nothing panics.
        assert_eq!(
            rejoined_cells(&at_26, &[2, 8, 14, 20], 10),
            Some(
                ["1: Bad", "2: Fine", "3: Good", ""]
                    .map(String::from)
                    .to_vec()
            )
        );
        // NEGATIVE CONTROLS.
        let inside = rows(&[
            "  1: Bad  2: Fine 3: Good 0:",
            "                           Dismiss",
        ]);
        assert_eq!(rejoined_cells(&inside, &[2, 10, 18, 26], 34), None);
        let left = rows(&[
            "  1: Bad  2: Fine 3: Good 0:",
            " x                        Dismiss",
        ]);
        assert_eq!(rejoined_cells(&left, &[2, 10, 18, 26], 34), None);
    }

    /// A REPLY'S LAST LINE IS READ BACK TO THE FIRST ROW OF ITS PARAGRAPH, on
    /// each agent's reply measured at 50 columns: Claude Code's, Gemini CLI's and Muse Code's
    /// second paragraph, two rows, the blank row above it ending the walk;
    /// agy's, three rows a cell or two short of the pane's edge. NEGATIVE
    /// CONTROLS: the same rows read in a pane they fit in whole (144 columns)
    /// are each their own line; a paragraph's head row (`⏺`, `✦`, `◆` past
    /// its mark) opens its line, and so does a list item's row.
    #[test]
    fn a_replys_last_line_starts_at_its_paragraphs_first_row() {
        use crate::prompt::fixtures::screen;
        let last = "every caller now. Which do you prefer (1 or 2)?";
        let second = "Two ways on from here: keep the old API, or move";
        let head = "I finished the parser and the suite is green.";
        for (name, text, first, last, rows_in_line) in [
            (
                "claude",
                crate::prompt::fixtures::REPLY_WRAPPED_50_2_1_302,
                second,
                last,
                2,
            ),
            (
                "gemini",
                crate::gemini::fixtures::REPLY_WRAPPED_50,
                second,
                last,
                2,
            ),
            (
                "muse",
                crate::muse::fixtures::REPLY_WRAPPED_50_1_4_10,
                second,
                last,
                2,
            ),
            (
                "agy",
                crate::agy::fixtures::lab::REPLY_WRAPPED_50,
                "Two ways on from here: keep the old API, or",
                "or 2)?",
                3,
            ),
        ] {
            let rows = screen(text);
            let at = rows
                .iter()
                .rposition(|r| crate::text::trim(r) == last)
                .expect("the last row");
            let from = line_start(&rows, at, 50);
            assert_eq!(at + 1 - from, rows_in_line, "{name}");
            assert!(rows[from].ends_with(first), "{name}: {:?}", rows[from]);
            assert_eq!(line_start(&rows, at, 144), at, "the control: {name}");
            let opens = rows
                .iter()
                .position(|r| r.ends_with(head))
                .expect("the head row");
            assert_eq!(line_start(&rows, opens, 50), opens, "the control: {name}");
        }
        let list = rows(&[
            "  - keep the old API and wrap every new call in",
            "    an adapter",
            "  - move every caller now",
        ]);
        assert_eq!(line_start(&list, 1, 50), 0, "an item's wrapped row");
        assert_eq!(line_start(&list, 2, 50), 2, "the control: an item opens");
    }
}
