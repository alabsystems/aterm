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
    width.saturating_sub(col.saturating_mul(2))
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
        assert_eq!(rejoined_cells(&at_34, &[10, 2], 34), None);
        assert_eq!(rejoined(&[]), "");
        assert_eq!(room(usize::MAX, 80), 0);
        assert_eq!(
            line_end(&at_34, usize::MAX, "x", 0, 80, usize::MAX),
            usize::MAX
        );
        assert_eq!(run_end(&at_34, usize::MAX, 0, usize::MAX), usize::MAX);
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

    /// A reply drawn at column2, with a text box `room` cells wide and the reader's right margin.
    fn said_at(parts: &[String], room: usize, margin: usize) -> String {
        let mut screen: Vec<String> = parts
            .iter()
            .enumerate()
            .map(|(i, part)| format!("{}{part}", if i == 0 { "⏺ " } else { "  " }))
            .collect();
        screen.push("─".repeat(2 + room + margin));
        let texts: Vec<(usize, &str)> = parts
            .iter()
            .enumerate()
            .map(|(i, part)| (i, crate::text::trim(part)))
            .collect();
        said_lines(&screen, &texts, margin, |_, _| false)
    }

    #[test]
    fn said_lines_preserve_a_word_boundary_after_a_full_row() {
        let prefix = "NEEDS YOU: open the ";
        let (first, second) = ("A".repeat(60), "B".repeat(40));
        let line = format!("{prefix}{first} {second}");
        let parts = crate::prompt::fixtures::ink_wrap(&line, 80);
        assert_eq!(parts, [format!("{prefix}{first}"), second.clone()]);
        // A hard-broken token can draw exactly the same rows. Their combined width is no proof
        // that the author omitted a space, so the ordinary word-wrap reading must keep it.
        let token = format!("{prefix}{first}{second}");
        assert_eq!(crate::prompt::fixtures::ink_wrap(&token, 80), parts);
        for margin in [0, 2] {
            assert_eq!(
                said_at(&parts, 80, margin),
                line,
                "separate words individually fit the box; margin={margin}"
            );
        }
    }

    /// Each reader's `said_lines` reads its measured 50-column reply's wrapped paragraph as one
    /// line, its first paragraph a line of its own, where `said_tail` reads a line a row; the turn's
    /// earlier message is read so too. NEGATIVE CONTROL: the wrapped paragraph's first row cut
    /// short, its next row is a line of its own.
    #[test]
    fn said_lines_rejoin_each_readers_measured_wrap() {
        use crate::prompt::fixtures::screen;
        use crate::reader::{ScreenReader, identify};
        let first = "I finished the parser and the suite is green.";
        let second = "Two ways on from here: keep the old API, or move every caller now. Which do \
                      you prefer (1 or 2)?";
        for (name, text) in [
            ("claude", crate::prompt::fixtures::REPLY_WRAPPED_50_2_1_302),
            ("agy", crate::agy::fixtures::lab::REPLY_WRAPPED_50),
            ("muse", crate::muse::fixtures::REPLY_WRAPPED_50_1_4_10),
        ] {
            let rows = screen(text);
            let reader: &dyn ScreenReader = identify(Some(name), &rows);
            let tail = reader.said_tail(&rows).expect("the reply");
            assert_eq!(
                tail.lines().count(),
                if name == "agy" { 4 } else { 3 },
                "{name}"
            );
            assert_eq!(
                reader.said_lines(&rows, |_, _| false).as_deref(),
                Some(format!("{first}\n{second}").as_str()),
                "{name}"
            );
            assert_eq!(
                reader.said_lines(&rows, |_, _| true).as_deref(),
                Some(tail.as_str()),
                "CONTROL: {name}: every row set apart is a line of its own"
            );
            let at = rows
                .iter()
                .position(|r| r.contains("Two ways on"))
                .expect("the wrapped paragraph");
            let mut cut = rows.clone();
            cut[at] = "  Two ways on from here.".to_string();
            let lines = reader.said_lines(&cut, |_, _| false).expect("the reply");
            assert_eq!(lines.lines().count(), 3, "CONTROL: {name}: {lines:?}");
            assert!(
                lines.lines().nth(1) == Some("Two ways on from here."),
                "CONTROL: {name}: {lines:?}"
            );
            if name == "claude" {
                let mut earlier = rows.clone();
                let head = earlier
                    .iter()
                    .position(|r| r.starts_with("⏺ "))
                    .expect("the reply's head");
                let moved: Vec<String> = earlier.drain(head + 2..=at + 1).collect();
                let mut turn = vec![format!("⏺ {}", crate::text::trim(&moved[0]))];
                turn.extend(moved[1..].iter().cloned());
                turn.extend([
                    "".to_string(),
                    "⏺ Bash(targo --unverified test)".to_string(),
                ]);
                turn.extend(["  ⎿  ok".to_string(), String::new()]);
                earlier.splice(head..head, turn);
                assert_eq!(
                    reader.turn_lines_before(&earlier, |_, _| false).as_deref(),
                    Some(second),
                    "{earlier:#?}"
                );
                assert_eq!(
                    reader.turn_said_before(&earlier).map(|t| t.lines().count()),
                    Some(2),
                    "{earlier:#?}"
                );
            }
        }
    }

    /// `said_lines` carries the lead Ink leaves under an exactly full row ([`lead`]): a need whose
    /// first row fills the room (78 of 78 cells) and whose third row opens on `Settings` is read
    /// as one line, as Ink wrapped it (1 + 69 + 1 + 8 > 78). NEGATIVE CONTROL: a 7-cell word there
    /// fits even past the lead, so that row is a line of its own.
    #[test]
    fn said_lines_carry_the_lead_an_exactly_full_row_leaves() {
        use crate::prompt::fixtures::ink_wrap;
        let need = "NEEDS YOU: grant aterm (dev).app the Accessibility and the Screen Recording \
                    on rights under Privacy & Security in the pane for the dev app in System \
                    Settings so the window lab can drive it.";
        let wrapped = ink_wrap(need, 78);
        let widths: Vec<usize> = wrapped.iter().map(|r| cells(r)).collect();
        assert_eq!(widths, [78, 69, 40], "{wrapped:#?}");
        assert!(wrapped[2].starts_with("Settings "), "{wrapped:#?}");
        let screen = |third: &str| -> Vec<String> {
            vec![
                format!("⏺ {}", wrapped[0]),
                format!("  {}", wrapped[1]),
                format!("  {third}"),
            ]
        };
        let lines = |rows: &[String]| {
            let said: Vec<(usize, &str)> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (i, crate::text::trim(r.trim_start_matches('⏺'))))
                .collect();
            said_lines(rows, &said, 0, |_, _| false)
        };
        assert_eq!(lines(&screen(&wrapped[2])), need);
        let control = lines(&screen("Display is next."));
        assert_eq!(
            control.lines().nth(1),
            Some("Display is next."),
            "CONTROL: {control:?}"
        );
    }
}
