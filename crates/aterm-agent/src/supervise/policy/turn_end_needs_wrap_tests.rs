// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_phase::prompt::fixtures::{ink_wrap, screen};

/// A need too long for one row, its wrap opening on a capital: `System` / `Settings` at 80
/// columns, `the` / `Accessibility` at 50.
const LONG_NEED: &str = "grant aterm (dev).app the Accessibility permission in System Settings so \
                         the window lab can drive it.";

/// A row of its own under a short need: a new line, never its wrap.
const NEXT_LINE: &str = "Everything else is on main.";

/// The cells `program` gives a message's line in a pane `cols` wide: the pane less its gutter,
/// and agy's two cells short of the edge too.
fn room(program: Program, cols: usize) -> usize {
    match program {
        Program::Agy => cols - 4,
        _ => cols - 2,
    }
}

/// `head`, a word of `x`s, then `tail`: `n` cells in all.
fn row_of(head: &str, n: usize, tail: &str) -> String {
    let pad = n - head.chars().count() - tail.chars().count() - 2;
    format!("{head} {} {tail}", "x".repeat(pad))
}

/// `program`'s measured 50-column reply (`… keep the old API, or move` / `every caller now. …`)
/// with its rules drawn `cols` wide and its reply replaced by `lines`: each slice a paragraph of
/// its own, a blank row between, each line Ink-wrapped in the message's [`room`], Claude Code's
/// and Muse Code's first row behind its mark.
fn reply_screen(program: Program, cols: usize, lines: &[&[&str]]) -> Vec<String> {
    let (text, mark) = match program {
        Program::Claude => (
            aterm_phase::prompt::fixtures::REPLY_WRAPPED_50_2_1_302,
            "⏺ ",
        ),
        Program::Muse => (aterm_phase::muse::fixtures::REPLY_WRAPPED_50_1_4_10, "◆ "),
        _ => (aterm_phase::agy::fixtures::lab::REPLY_WRAPPED_50, "  "),
    };
    let room = room(program, cols);
    let rows: Vec<String> = screen(text)
        .into_iter()
        .map(|r| {
            let rule = r.chars().count();
            if rule >= 47 && r.ends_with("──") {
                format!("{r}{}", "─".repeat(cols - 50))
            } else {
                r
            }
        })
        .collect();
    let first = rows
        .iter()
        .position(|r| r.contains("I finished the parser"))
        .expect("the reply");
    let last = rows
        .iter()
        .position(|r| r.contains("or 2)?"))
        .expect("the reply's end");
    let mut reply: Vec<String> = Vec::new();
    for (p, paragraph) in lines.iter().enumerate() {
        if p > 0 {
            reply.push(String::new());
        }
        for line in *paragraph {
            for row in ink_wrap(line, room) {
                let lead = if reply.is_empty() { mark } else { "  " };
                reply.push(format!("{lead}{row}"));
            }
        }
    }
    let mut out = rows[..first].to_vec();
    out.extend(reply);
    out.extend_from_slice(&rows[last + 1..]);
    out
}

/// The needs a point of `rows` leaves standing.
fn needs_at_point(program: Program, rows: &[String]) -> Vec<String> {
    let reading = aterm_phase::read(Some(program.name()), rows, None);
    assert_eq!(reading.program, program, "{rows:#?}");
    let r = TurnEndReading::of(&reading, rows, false, Some(3 * MIN), None, None);
    let mut st = TurnEndState::default();
    st.observe(&r, t0());
    st.standing_needs().to_vec()
}

/// F2: a NEEDS YOU line the program wrapped onto a row that opens on a capital is read whole, on
/// Claude Code, agy and Muse Code alike, at 80 columns and at 50: the row continues the line when
/// the row above was too full to take its first word. NEGATIVE CONTROL: a short need's next row
/// is a new line, never its wrap, though it opens on a capital too.
#[test]
fn a_wrapped_need_is_read_whole_on_every_reader() {
    let long = format!("NEEDS YOU: {LONG_NEED}");
    for program in [Program::Claude, Program::Agy, Program::Muse] {
        for cols in [80, 50] {
            let at = format!("{program:?} at {cols}");
            let rows = reply_screen(program, cols, &[&["Merged the parser."], &[&long]]);
            let opens = if cols == 80 {
                "Settings"
            } else {
                "Accessibility"
            };
            assert!(
                rows.iter().any(|r| r.trim_start().starts_with(opens)),
                "{at}: the wrap opens on a capital: {rows:#?}"
            );
            assert_eq!(needs_at_point(program, &rows), [LONG_NEED], "{at}");
            let short = reply_screen(
                program,
                cols,
                &[
                    &["Merged the parser."],
                    &["NEEDS YOU: the signing key", NEXT_LINE],
                ],
            );
            assert_eq!(
                needs_at_point(program, &short),
                ["the signing key"],
                "CONTROL: {at}: {short:#?}"
            );
        }
    }
}

/// The programs and pane widths each wrap test reads.
const PANES: [(Program, usize); 6] = [
    (Program::Claude, 80),
    (Program::Claude, 50),
    (Program::Agy, 80),
    (Program::Agy, 50),
    (Program::Muse, 80),
    (Program::Muse, 50),
];

/// A row the width would join to a need is still a line of its own where the needs reader sets
/// it apart (`needs::apart`): a need whose last row ends within one word of the edge keeps the
/// next `NEEDS YOU:` line its own need, after a one-row need and a two-row one alike, and a need
/// that ends its sentence there keeps the next line out. Before, the two needs read as one, and a
/// long first need's cap cut the second away.
#[test]
fn a_need_ending_near_the_edge_keeps_the_next_line_apart() {
    const NEXT: &str = "NEEDS YOU: the signing key";
    for (program, cols) in PANES {
        let room = room(program, cols);
        let at = format!("{program:?} at {cols}");
        let near = row_of("NEEDS YOU: the password", room - 3, "please");
        let rows = reply_screen(program, cols, &[&["Merged the parser."], &[&near, NEXT]]);
        let need = near.trim_start_matches("NEEDS YOU: ");
        assert_eq!(
            needs_at_point(program, &rows),
            [need, "the signing key"],
            "{at}: {rows:#?}"
        );
        let first = row_of("NEEDS YOU: the Developer ID", room - 2, "cert,");
        let second = row_of("which lives in", room - 3, "at all");
        let long = format!("{first} {second}");
        assert_eq!(ink_wrap(&long, room), [first.clone(), second], "{at}");
        let rows = reply_screen(program, cols, &[&["Merged the parser."], &[&long, NEXT]]);
        let need = long.trim_start_matches("NEEDS YOU: ");
        assert_eq!(
            needs_at_point(program, &rows),
            [need, "the signing key"],
            "{at}: {rows:#?}"
        );
        let ended = row_of("NEEDS YOU: the signing key for", room - 1, "0.145.0.");
        let rows = reply_screen(program, cols, &[&["Merged."], &[&ended, NEXT_LINE]]);
        assert_eq!(
            needs_at_point(program, &rows),
            [ended.trim_start_matches("NEEDS YOU: ")],
            "{at}: {rows:#?}"
        );
    }
}

/// A row continues the line above it only where that row had no room left for its first word,
/// to the cell: `… in System` / `Settings so …` is one need when `Settings` would have overrun the
/// room by one cell. NEGATIVE CONTROLS: one cell short, and two, the row above had room for it,
/// so the author broke the line there and the need ends at `System`.
#[test]
fn a_wrap_is_read_by_the_room_to_the_cell() {
    const REST: &str = "Settings so the window lab can drive it.";
    for (program, cols) in PANES {
        let room = room(program, cols);
        for (short, joined) in [(8, true), (9, false), (10, false)] {
            let at = format!("{program:?} at {cols}, {short} cells short");
            let need = row_of("NEEDS YOU: grant", room - short, "in System");
            let rows = reply_screen(program, cols, &[&["Merged the parser."], &[&need, REST]]);
            let need = need.trim_start_matches("NEEDS YOU: ");
            let whole = format!("{need} {REST}");
            let want = if joined { whole.as_str() } else { need };
            assert_eq!(needs_at_point(program, &rows), [want], "{at}: {rows:#?}");
        }
    }
}

/// The row under an exactly full row starts a cell in (Ink's lead), so the row after it is judged
/// with that cell spent: a need whose first row fills the room and whose third row opens on
/// `Settings` is read whole. NEGATIVE CONTROL: a 7-cell word there fit even past the lead, so
/// `Display is next.` is a line of its own.
#[test]
fn a_need_past_an_exactly_full_row_is_read_whole() {
    const REST: &str = "Settings so the window lab can drive it.";
    for (program, cols) in PANES {
        let room = room(program, cols);
        let at = format!("{program:?} at {cols}");
        let first = row_of("NEEDS YOU: grant", room, "the app");
        let second = row_of("and", room - 9, "rights under");
        let long = format!("{first} {second} {REST}");
        assert_eq!(
            ink_wrap(&long, room),
            [first.clone(), second.clone(), REST.to_string()],
            "{at}"
        );
        let rows = reply_screen(program, cols, &[&["Merged the parser."], &[&long]]);
        let need = needs::sanitized(long.trim_start_matches("NEEDS YOU: "));
        assert_eq!(needs_at_point(program, &rows), [need], "{at}: {rows:#?}");
        let two = format!("{first} {second}");
        let rows = reply_screen(
            program,
            cols,
            &[&["Merged the parser."], &[&two, "Display is next."]],
        );
        assert_eq!(
            needs_at_point(program, &rows),
            [two.trim_start_matches("NEEDS YOU: ")],
            "CONTROL: {at}: {rows:#?}"
        );
    }
}
