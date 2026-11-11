// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rejoining single logical lines that Ink (`wrap-ansi`) wrapped across multiple terminal rows in narrow panes.

use crate::phase::leading_spaces;
use crate::question::cells;

/// Whether row `below` is the rest of a line Ink wrapped, the line's text on the row above it being `above` (trimmed), drawn from column `col` in a box `room` cells wide ([`room`]): `below` starts at `col`, both are words — no rule, frame or block glyph (U+2500–U+259F), so a composer's rule under a footer is never its wrap — and `above` is too full to take `below`'s first word, with two cells of slack, as the question dialog's measured labels wrap ([`crate::question`]).
pub(crate) fn continues(above: &str, col: usize, below: &str, room: usize) -> bool {
    wraps_with(above, 0, col, below, room, 2)
}

/// [`continues`] with zero slack: tests whether `above` (plus `lead` leading space cells from [`lead`]) had no room in `room` cells for `below`'s first word and its preceding space.
pub(crate) fn continues_exactly(
    above: &str,
    lead: usize,
    col: usize,
    below: &str,
    room: usize,
) -> bool {
    wraps_with(above, lead, col, below, room, 0)
}

/// Leading space cells `wrap-ansi` (`trim: false, hard: true`) consumed on a row before its drawn text: `1` when the row above (`above` cells including its own lead) filled or exceeded `room`, else `0`.
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

/// The room a line drawn from column `col` of a pane `width` cells wide has in a box as wide as the pane less `col` on each side: a footer's, a dialog's, the chrome intro's. A row of Claude Code's transcript is drawn in a box that ends at the pane's edge, its room [`transcript_room`].
pub(crate) const fn room(col: usize, width: usize) -> usize {
    width.saturating_sub(col.saturating_mul(2))
}

/// The room the text of a row of Claude Code's transcript, drawn from column `col` of a pane `width` cells wide (beside the side panel, as wide as the rule's column), has: its box ends at the pane's edge. Measured across the captured Claude Code screens, for each layout the transcript draws:
/// - a `⎿` row's text (the gutter under a call, a prompt or a message), from column 5 (4 under `⎿ `): drawn to the pane's edge (the usage limit notice at 26 and 34 columns, a tool's output at 40, a question's answer at 120), and a next word that would end one cell past the edge opens the next row (the limit notice's `session limit ·` over `resets` at 26, a tool's output at 40 and 60, the auto-continue countdown at 144);
/// - a `⏺` reply's text, from column 2: drawn to the pane's edge in 27 reply paragraphs (the lab stub's replies at 12 to 50 columns, its tick rows at 34, 80 and 144, replies at 80, 146 and 150 and beside the 2.1.316 rule at 81; the sweep's other 33 `⏺` paragraphs at the edge are one-row call heads and notices), and a next word that would end one or two cells past it opens the next row (in replies at 12 to 146 columns);
/// - a `✻` status line, from column 2: 15 drawn to the pane's edge (14 done rows at 12 to 34 columns and a spinner's word split at 10), the 13 done rows whose next word would end one cell past it wrapped there (at 12 to 34 columns), and under the 2.1.310 no-response retry at 60 the vendor's hint opens a row of its own though its first word fits on the retry row.
///
/// Ink wraps there with no slack, the lead an exactly full row leaves counted ([`transcript_line_end`], which reads a `⎿` block's lines and a done row's status line in it, `phase::line_end_in`). The readers of a column-2 row that may be a vendor `⏺` notice's (its text wrapped in the pane less ten, `NOTICE_MARGIN`) or a dialog's (in [`room`]) keep [`room`]: [`line_start`] outside a `⎿` block and `turn::wraps_onto`. So does the status line of a wait or of a spinner whose parenthesis wraps, whose wrap no capture measures. One captured `⎿` paragraph wraps narrower: 2.1.316's `Message queued for delivery …` beside the side panel, at no more than 71 cells of the rule's 81, so in this room its rows read as lines of their own.
pub(crate) const fn transcript_room(col: usize, width: usize) -> usize {
    width.saturating_sub(col)
}

/// The last row of the line row `first` opens in the transcript's box ([`transcript_room`]), its text there `head` (trimmed) from column `col`, in a pane `width` cells wide: each row after it, short of row `until`, that Ink wrapped onto the row above it there, as Ink wraps it — the row above had no room for the row's first word and its space ([`continues_exactly`], no slack), its [`lead`] after an exactly full row counted. A row that would have fit on the row above is a line of its own.
pub(crate) fn transcript_line_end(
    rows: &[String],
    first: usize,
    head: &str,
    col: usize,
    width: usize,
    until: usize,
) -> usize {
    let until = until.min(rows.len());
    let room = transcript_room(col, width);
    let (mut end, mut above, mut lead) = (first, head, 0);
    while let Some(next) = end.checked_add(1)
        && next < until
        && continues_exactly(above, lead, col, &rows[next], room)
    {
        lead = self::lead(lead + cells(above), room);
        end = next;
        above = crate::text::trim(&rows[end]);
    }
    end
}

/// The `⎿` row whose block row `i` stands in ([`crate::phase::gutter_open`]) and the column that row's text is drawn from, where row `i` is the `⎿` row itself or a row of the block's text under it at that column: every line of a `⎿` block is drawn in one box, the transcript's, which ends at the pane's edge (the sweep [`transcript_room`] cites measured the rows under a `⎿` row at its text column with that row's). A row of the block drawn further in (a tool's own indent) is neither.
pub(crate) fn gutter_of(rows: &[String], i: usize) -> Option<(usize, usize)> {
    let open = crate::phase::gutter_open(rows, i)?;
    let row = rows[open].as_str();
    let after = crate::text::trim_start(row).strip_prefix('⎿')?;
    let col = leading_spaces(row) + 1 + leading_spaces(after);
    (open == i || leading_spaces(&rows[i]) == col).then_some((open, col))
}

/// The last row of the line row `i` opens, short of row `until`, in a pane `width` cells wide: a `⎿` block's text ([`gutter_of`]) in the transcript's box, from its text (past the `⎿` on the block's own row) at the block's column ([`transcript_line_end`]); any other row's from its indent in [`room`] ([`line_end`]).
pub(crate) fn row_line_end(rows: &[String], i: usize, width: usize, until: usize) -> usize {
    let row = rows[i].as_str();
    let text = crate::text::trim_start(row);
    match gutter_of(rows, i) {
        Some((_, col)) => {
            let head = crate::text::trim(text.strip_prefix('⎿').unwrap_or(text));
            transcript_line_end(rows, i, head, col, width, until)
        }
        None => line_end(
            rows,
            i,
            crate::text::trim(row),
            leading_spaces(row),
            width,
            until,
        ),
    }
}

/// The last row of the line row `first` opens, its text there `head` (trimmed) from column `col`, in a pane `width` cells wide ([`room`]): each row after it, short of row `until`, that [`continues`] the row above it.
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
    while let Some(next) = end.checked_add(1)
        && next < until
        && continues(above, col, &rows[next], room)
    {
        end = next;
        above = crate::text::trim(&rows[end]);
    }
    end
}

/// Walk backward from `last` across rows that [`continues`] joins at `last`'s [`text_column`], stopping at the row that opens the paragraph or list item (a bullet/marker prefix before `text_column`) or where [`continues`] ceases to hold. A row of a `⎿` block's text ([`gutter_of`]) is read as [`crate::phase::line_at`] reads the block, line by line from its `⎿` row ([`row_line_end`]): the row that opens the line holding `last`. Outside a `⎿` block the walk keeps [`room`] and its slack, not the transcript's box ([`transcript_room`]): the row it starts from may be a reply's, a vendor `⏺` notice's (its text wrapped in the pane less ten, `NOTICE_MARGIN`) or a dialog's (in [`room`]), which its rows alone do not tell apart.
#[must_use]
pub fn line_start(rows: &[String], last: usize, width: usize) -> usize {
    let Some(row) = rows.get(last) else {
        return last;
    };
    let col = leading_spaces(row);
    if text_column(row) != col {
        return last;
    }
    if let Some((mut first, _)) = gutter_of(rows, last) {
        loop {
            let end = row_line_end(rows, first, width, last + 1);
            if end >= last {
                return first;
            }
            first = end + 1;
        }
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

/// Last row of a multi-span line (such as a shimmering status verb) that Ink may break at span boundaries before a row is full: walks non-blank rows after `first` (up to `until`) that start at `col` and contain no box/rule glyphs.
pub(crate) fn run_end(rows: &[String], first: usize, col: usize, until: usize) -> usize {
    let until = until.min(rows.len());
    let drawn = |t: &str| t.chars().any(|c| ('\u{2500}'..='\u{259F}').contains(&c));
    let mut end = first;
    while let Some(next) = end.checked_add(1)
        && next < until
        && {
            let r = &rows[next];
            let t = crate::text::trim(r);
            !t.is_empty() && leading_spaces(r) == col && !drawn(t)
        }
    {
        end = next;
    }
    end
}

/// Count the rows `wrap-ansi` (`hard: true, trim: false`) uses to wrap `line` into `room` cells, accounting for the leading space cell consumed after an exactly-full row and hard breaks on words wider than `room`.
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

/// Rows `run`, one line Ink wrapped, as that line: the first row's indent, then each row's text, joined with one space (Ink's word wrap breaks at a space and draws none at either end of a row).
pub(crate) fn rejoined(run: &[String]) -> String {
    let Some(first) = run.first() else {
        return String::new();
    };
    let mut out = " ".repeat(leading_spaces(first));
    for (k, r) in run.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.push_str(crate::text::trim(r));
    }
    out
}

/// A message's rows `said` (each a row of `rows` and its text there, trimmed, in order), a line a row.
pub(crate) fn said_rows(said: &[(usize, &str)]) -> String {
    let texts: Vec<&str> = said.iter().map(|&(_, text)| text).collect();
    texts.join("\n")
}

/// Rejoin word-wrapped message rows `said` (as [`said_rows`] takes them), whose box ends `margin`
/// cells short of a pane as wide as `rows`' widest row. The leading cell consumed but not drawn
/// after an exactly-full row ([`lead`]) counts against its next row's room. A row that
/// [`continues_exactly`] the one above, from its [`text_column`], joins it with one separating
/// space unless `apart(line, row)` opens a line of its own. A first word that would have fit above
/// opens a new line. These row strings alone cannot distinguish a hard-broken token from separate
/// words that drew the same rows, so the original spelling cannot always be recovered.
pub(crate) fn said_lines(
    rows: &[String],
    said: &[(usize, &str)],
    margin: usize,
    apart: crate::reader::Apart,
) -> String {
    let width = crate::question::screen_width(rows);
    let mut out = String::new();
    let (mut col, mut lead, mut from) = (0, 0, 0);
    for (k, &(i, text)) in said.iter().enumerate() {
        let room = width.saturating_sub(col + margin);
        let wraps = k > 0 && {
            let (above, line) = said[k - 1];
            above + 1 == i
                && continues_exactly(line, lead, col, &rows[i], room)
                && !apart(&out[from..], text)
        };
        if wraps {
            lead = self::lead(lead + cells(said[k - 1].1), room);
            out.push(' ');
        } else {
            if k > 0 {
                out.push('\n');
            }
            (col, lead, from) = (text_column(&rows[i]), 0, out.len());
        }
        out.push_str(text);
    }
    out
}

/// Rejoin a side-by-side flex row of boxes (`starts[k]..starts[k+1]`, last to `width`) whose text wrapped within each box.
pub(crate) fn rejoined_cells(
    run: &[String],
    starts: &[usize],
    width: usize,
) -> Option<Vec<String>> {
    let (&first, rest) = starts.split_first()?;
    let last_start = *rest.last().unwrap_or(&first);
    let widths: Vec<usize> = starts
        .iter()
        .zip(rest.iter().copied().chain([width.max(last_start)]))
        .map(|(&start, end)| end.checked_sub(start))
        .collect::<Option<_>>()?;
    let mut texts = vec![String::new(); starts.len()];
    let mut full = vec![false; starts.len()];
    for (k, row) in run.iter().enumerate() {
        let (left, mut row) = crate::question::split_at_cell(row, first);
        if !crate::text::trim(left).is_empty() {
            return None;
        }
        for (b, &box_w) in widths.iter().enumerate() {
            let (cell, after) = crate::question::split_at_cell(row, box_w);
            row = after;
            let text = crate::text::trim_end(cell);
            if text.is_empty() {
                continue;
            }
            if k > 0 && leading_spaces(text) > 0 {
                return None;
            }
            let text = crate::text::trim(text);
            if !texts[b].is_empty() && !full[b] {
                texts[b].push(' ');
            }
            texts[b].push_str(text);
            full[b] = cells(cell.trim_end()) >= box_w;
        }
    }
    Some(texts)
}

#[cfg(test)]
#[path = "wrap_tests.rs"]
mod tests;
