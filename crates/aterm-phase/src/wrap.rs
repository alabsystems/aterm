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

/// The room a line drawn from column `col` of a pane `width` cells wide has in a box as wide as the pane less `col` on each side: a footer's, a banner's, a `⎿` notice's (whose box ends at the pane's edge: the rule reads such a row full a little early).
pub(crate) const fn room(col: usize, width: usize) -> usize {
    width.saturating_sub(2 * col)
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
    while end + 1 < until && continues(above, col, &rows[end + 1], room) {
        end += 1;
        above = crate::text::trim(&rows[end]);
    }
    end
}

/// Walk backward from `last` across rows that [`continues`] joins at `last`'s [`text_column`], stopping at the row that opens the paragraph or list item (a bullet/marker prefix before `text_column`) or where [`continues`] ceases to hold.
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

/// Last row of a multi-span line (such as a shimmering status verb) that Ink may break at span boundaries before a row is full: walks non-blank rows after `first` (up to `until`) that start at `col` and contain no box/rule glyphs.
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
    let mut out = " ".repeat(leading_spaces(&run[0]));
    for (k, r) in run.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.push_str(crate::text::trim(r));
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

    /// Verify `rows_in` against Claude Code `⏺` notice fixtures at 26, 34,
    /// and 80 columns, including exact-fill leading-space rows and hard-broken words.
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
        assert_eq!(rows_in("automatically at 10:51am · esc to", 16), 3);
        assert_eq!(rows_in("automatically 10:51am · esc to", 16), 2);
        assert_eq!(rows_in("e-limit-options", 16), 1);
        assert_eq!(rows_in("", 16), 1);
    }

    /// Verify `rejoined_cells` on narrow session-survey option boxes (both
    /// space-broken and hard-broken words) and negative controls.
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
        assert_eq!(
            rejoined_cells(&at_34[..1], &[2, 10, 18, 26], 34),
            Some(
                ["1: Bad", "2: Fine", "3: Good", "0:"]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert_eq!(
            rejoined_cells(&at_26, &[2, 8, 14, 20], 10),
            Some(
                ["1: Bad", "2: Fine", "3: Good", ""]
                    .map(String::from)
                    .to_vec()
            )
        );
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

    /// Verify `line_start` walks back to the opening row of wrapped agent
    /// reply paragraphs at 50 columns and stops at blank lines, markers, or
    /// unwrapped widths.
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
