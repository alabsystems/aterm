// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// The walk back from the line last said keeps [`room`] and its slack outside a `⎿` block, not
/// the transcript's box ([`transcript_room`]): the row it starts from may be a reply's, a vendor
/// `⏺` notice's or a dialog's, and the rows alone do not tell them apart. MEASURED: the 2.1.283
/// unreachable API's notice at 80 columns and the 2.1.304 hook's notice (each its text wrapped
/// in the pane less ten, `NOTICE_MARGIN`), the 2.1.302 MCP server dialog at 40 and the
/// fullscreen 2.1.310 auto mode offer's footer at 34 (each in the pane less twice its column)
/// are each read whole.
/// NEGATIVE CONTROL: in the transcript's box the line's first row opens a line that ends short of
/// its last row ([`transcript_line_end`]), so a walk in that box would lose the line's first
/// words.
#[test]
fn a_notice_or_a_dialogs_line_last_said_is_walked_back_in_the_boxed_room() {
    use crate::prompt::fixtures::screen;
    for (name, capture, line) in [
        (
            "2.1.283 unreachable API notice at 80",
            include_str!("fixtures/claude-2.1.283-api-error-enotfound-80col-measured.txt"),
            "⏺ API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)",
        ),
        (
            "2.1.304 hook notice",
            include_str!(
                "fixtures/claude-2.1.304-user-prompt-submit-hook-continue-false-long-reason-wrapped-limit-phrase.txt"
            ),
            "⏺ Operation stopped by hook: the nightly pipeline finished its integration suite \
             and the deploy gate reported back to the build bot that Usage limit reached · \
             resets 3pm",
        ),
        (
            "2.1.302 MCP server dialog at 40",
            include_str!("fixtures/claude-2.1.302-mcp-server-two-40col.txt"),
            "MCP servers may execute code or access system resources. All tool calls require \
             approval. Learn more in the MCP documentation.",
        ),
        (
            "2.1.310 auto mode offer's footer at 34",
            include_str!("fixtures/claude-2.1.310-auto-mode-setup-fullscreen-34col.txt"),
            "Enter to confirm · Esc to cancel",
        ),
    ] {
        let rows = screen(capture);
        let width = crate::question::screen_width(&rows);
        assert_eq!(
            crate::phase::last_said_line(&rows, Some(width)).as_deref(),
            Some(line),
            "{name}"
        );
        let last = crate::phase::last_said_index(&rows).expect("the last row said");
        let (from, col) = (line_start(&rows, last, width), text_column(&rows[last]));
        let head = rows[from]
            .char_indices()
            .nth(col)
            .map_or("", |(at, _)| &rows[from][at..]);
        assert!(
            from < last
                && transcript_line_end(
                    &rows,
                    from,
                    crate::text::trim(head),
                    col,
                    width,
                    rows.len()
                ) < last,
            "NEGATIVE CONTROL: {name}"
        );
    }
}

/// A `⎿` row's text is read in its box, which ends at the pane's edge ([`transcript_room`]), as
/// Ink wraps it: no slack, the lead an exactly full row leaves counted. On measured captures
/// each row whose first word had no room on the row above is the rest of that row's line: the
/// 2.1.299 limit notice at 26 columns (`session limit ·` over `resets`, one cell past the
/// edge; under its full `(America/Los_Angeles)` row the `/upgrade …` line, which a wrap would
/// draw the same, joins too), the 2.1.309 tool output at 40 (`command printed was lost and
/// its` over `own`, one cell past), the 2.1.302 auto-continue countdown at 144 (`… Press esc`
/// over `to`, one cell past, its row past twice the column's room) and the 2.1.283 question
/// answer at 120, whose third row had room for the next word but for the lead its full second
/// row leaves. NEGATIVE CONTROL: rows that each had room for the next word are lines of their
/// own: the 2.1.309 command's own copy at 179 columns, which the command wrapped at 169 (a room
/// of the pane less twice the column, or two cells of slack, joined it), and the 2.1.302 limit
/// notice over its `/upgrade …` line at 144.
#[test]
fn a_gutter_rows_text_is_read_in_a_box_that_ends_at_the_panes_edge() {
    use crate::prompt::fixtures::{
        TOOL_DISK_FULL_40_2_1_309, TOOL_PRESTART_PRINTED_EXIT0_179_2_1_309,
        WALL_SONNET_BUCKET_AUTO_CONTINUE_CANCELLED_2_1_302, screen,
    };
    let line_end = |rows: &[String], at: usize, width: usize| {
        let row = rows[at].as_str();
        let (head, col) = match crate::text::trim_start(row).strip_prefix('⎿') {
            Some(after) => (
                crate::text::trim(after),
                leading_spaces(row) + 1 + leading_spaces(after),
            ),
            None => (crate::text::trim(row), leading_spaces(row)),
        };
        transcript_line_end(rows, at, head, col, width, rows.len())
    };
    let at = |rows: &[String], text: &str| {
        let last = |ends: bool| {
            rows.iter().rposition(|r| {
                let t = crate::text::trim(r);
                t == text || (ends && t.ends_with(text))
            })
        };
        last(false)
            .or_else(|| last(true))
            .unwrap_or_else(|| panic!("the row {text:?}"))
    };
    for (name, capture, width, from, to) in [
        (
            "2.1.299 limit notice at 26",
            include_str!("fixtures/claude-2.1.299-wall-session-auto-continue-cancelled-26col.txt"),
            26,
            "You've hit your",
            "your usage limit.",
        ),
        (
            "2.1.309 tool output at 40",
            TOOL_DISK_FULL_40_2_1_309,
            40,
            "Disk full: this computer's volume",
            "anything to make room.",
        ),
        (
            "2.1.302 countdown at 144",
            include_str!(
                "fixtures/claude-2.1.302-wall-sonnet-bucket-auto-continue-cancelled-over-24h.txt"
            ),
            144,
            "may still pause for permission prompts. Press esc",
            "to cancel the wait.",
        ),
        (
            "2.1.283 question answer at 120",
            include_str!("fixtures/claude-2.1.283-question-typed-short-term.txt"),
            120,
            "→ Decide yourself: choose the option that",
            "not asked.)",
        ),
        (
            "NEGATIVE CONTROL: the 2.1.309 command's own copy at 179",
            TOOL_PRESTART_PRINTED_EXIT0_179_2_1_309,
            179,
            "Code's temporary directory), so the file",
            "Code's temporary directory), so the file",
        ),
        (
            "NEGATIVE CONTROL: the 2.1.302 limit notice at 144",
            WALL_SONNET_BUCKET_AUTO_CONTINUE_CANCELLED_2_1_302,
            144,
            "resets 10:06am (America/Los_Angeles)",
            "resets 10:06am (America/Los_Angeles)",
        ),
    ] {
        let rows = screen(capture);
        assert_eq!(
            crate::question::screen_width(&rows),
            width,
            "PRECONDITION: {name}: drawn to the pane's edge"
        );
        assert_eq!(
            line_end(&rows, at(&rows, from), width),
            at(&rows, to),
            "{name}"
        );
    }
    let answer = screen(include_str!(
        "fixtures/claude-2.1.283-question-typed-short-term.txt"
    ));
    let third = at(&answer, "the user was");
    assert_eq!(cells(crate::text::trim(&answer[third - 1])), 115, "full");
    assert!(
        !continues_exactly(
            crate::text::trim(&answer[third]),
            0,
            5,
            &answer[third + 1],
            115
        ),
        "PRECONDITION: the lead decides the question answer's last row"
    );
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

/// The lead Ink leaves under an exactly full row ([`lead`]) counts on the next row at the
/// reader's right margin too (`margin` 2, which
/// `said_lines_carry_the_lead_an_exactly_full_row_leaves` does not draw): `NEEDS YOU: x…`
/// fills the 48-cell box, so `A…` and `B…` (23 + 1 + 24 cells) overran the 47 left and are one
/// line with it. CONTROL: a short need's next row is a line of its own at either margin. (Lost
/// in the merge ff6b099179; restored as aa1c37ea3d wrote it.)
#[test]
fn said_lines_keep_exact_fill_leads() {
    let line = format!(
        "NEEDS YOU: {} {} {}",
        "x".repeat(37),
        "A".repeat(23),
        "B".repeat(24)
    );
    let parts = crate::prompt::fixtures::ink_wrap(&line, 48);
    assert_eq!(
        parts,
        [
            format!("NEEDS YOU: {}", "x".repeat(37)),
            "A".repeat(23),
            "B".repeat(24)
        ]
    );
    for margin in [0, 2] {
        assert_eq!(
            said_at(&parts, 48, margin),
            line,
            "the consumed blank counts on the next row at the same gutter; margin={margin}"
        );
        assert_eq!(
            said_at(
                &["NEEDS YOU: the key".into(), "Settings are ready.".into()],
                48,
                margin
            ),
            "NEEDS YOU: the key\nSettings are ready.",
            "control: a short new line is not a wrap; margin={margin}"
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
