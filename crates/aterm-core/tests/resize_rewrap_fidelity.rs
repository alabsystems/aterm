// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that width changes preserve screen and scrollback text across sync and async resizes.

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

fn logical_lines(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    let mut out: Vec<String> = Vec::new();
    for i in 0..grid.scrollback_lines() {
        let line = grid.get_history_line(i).expect("history line");
        let text = line.to_string();
        if line.is_wrapped() && !out.is_empty() {
            out.last_mut().expect("non-empty").push_str(&text);
        } else {
            out.push(text);
        }
    }
    for r in 0..grid.rows() {
        let text = grid.row_text(r).expect("visible row");
        let wrapped = grid.row(r).is_some_and(|row| row.is_wrapped());
        if wrapped && !out.is_empty() {
            out.last_mut().expect("non-empty").push_str(&text);
        } else {
            out.push(text);
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

fn screen(term: &Terminal) -> Vec<String> {
    let grid = term.grid();
    (0..grid.rows())
        .map(|r| grid.row_text(r).expect("visible row"))
        .collect()
}

fn offloaded_resize(term: &mut Terminal, rows: u16, cols: u16) {
    let mut job = term.resize_offloading_scrollback(rows, cols);
    while let Some(pending) = job {
        job = term.finish_resize_offload(pending.reflow());
    }
}

fn tiered(rows: u16, cols: u16, ring: usize) -> Terminal {
    Terminal::with_scrollback(rows, cols, ring, Scrollback::with_defaults())
}

#[test]
fn a_real_space_before_an_early_wrapped_glyph_survives_a_screen_round_trip() {
    for (text, narrow) in [
        ("abc 中fg", 4u16),
        ("done ✅ ok", 5),
        ("가나다 라마", 7),
        ("abcd 中文", 5),
        ("👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 zwj", 9),
    ] {
        let mut term = Terminal::new(6, 20);
        term.process(text.as_bytes());
        let before = screen(&term);
        term.resize(6, narrow);
        term.resize(6, 20);
        assert_eq!(screen(&term), before, "{text:?} through {narrow} columns");
    }
}

#[test]
fn a_real_early_wrap_hole_is_dropped_on_rejoin() {
    let hole_bg = |frame: &aterm_core::render::RenderInput| {
        frame.cells[0].get(9).map_or_else(
            || {
                let [_, r, g, b] = frame.default_bg.to_be_bytes();
                [r, g, b]
            },
            |cell| cell.bg,
        )
    };
    for text in [
        "abcdefghi中xyz",
        "\x1b[41mabcdefghi中\x1b[0mxyz",
        "\x1b[48;2;17;34;51mabcdefghi中\x1b[0mxyz",
    ] {
        let mut term = Terminal::new(6, 10);
        term.process(text.as_bytes());
        let before = term.cell_frame(6, 10);
        let before_bg = hole_bg(&before);
        assert!(
            term.grid().row(1).is_some_and(|r| r.is_wrapped()),
            "fixture: the wide glyph early-wrapped ({text:?})"
        );
        term.resize(6, 20);
        assert_eq!(
            logical_lines(&term),
            vec!["abcdefghi中xyz".to_string()],
            "{text:?}"
        );
        term.resize(6, 10);
        let after = screen(&term);
        assert_eq!(
            after[0].trim_end(),
            "abcdefghi",
            "{text:?}: the hole is blank"
        );
        assert_eq!(after[1], "中xyz", "{text:?}: the early wrap comes back");
        let after = term.cell_frame(6, 10);
        assert_eq!(
            hole_bg(&after),
            before_bg,
            "{text:?}: the regenerated hole uses the wrapping glyph's background"
        );
    }
}

#[test]
fn margined_vertical_moves_keep_printed_blanks_and_wrap_holes_distinct() {
    for (name, op, source, destination) in [
        ("IL", "\x1b[L", 0, 1),
        ("DL", "\x1b[M", 1, 0),
        ("SU", "\x1b[S", 1, 0),
        ("SD", "\x1b[T", 0, 1),
    ] {
        let mut term = Terminal::new(6, 4);
        term.process(format!("\x1b[{};1Habc中", destination + 1).as_bytes());
        assert!(term.grid().row(destination).unwrap().has_early_wrap_hole());
        term.process(format!("\x1b[{};1Hdef ", source + 1).as_bytes());
        term.process(b"\x1b[3;1Hgh");
        term.process(format!("\x1b[?69h\x1b[2;4s\x1b[1;2H{op}").as_bytes());
        assert!(
            !term.grid().row(destination).unwrap().has_early_wrap_hole(),
            "{name}: the incoming printed blank is content"
        );
        term.resize(6, 8);
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|line| line.contains("aef ")),
            "{name}: reflow deleted the copied printed space: {lines:?}"
        );

        for (right, expected_row) in [(4, destination), (3, source)] {
            let mut term = Terminal::new(6, 4);
            term.process(format!("\x1b[{};1Habc中", source + 1).as_bytes());
            term.process(format!("\x1b[?69h\x1b[2;{right}s\x1b[1;2H{op}").as_bytes());
            assert!(
                term.grid().row(expected_row).unwrap().has_early_wrap_hole(),
                "{name}: right={right}, hole belongs to row {expected_row}"
            );
        }

        let vacated = if matches!(name, "IL" | "SD") { 0 } else { 4 };
        let mut term = Terminal::new(6, 4);
        term.process(format!("\x1b[{};1Habc中", vacated + 1).as_bytes());
        assert!(term.grid().row(vacated).unwrap().has_early_wrap_hole());
        term.process(format!("\x1b[?69h\x1b[1;5r\x1b[2;4s\x1b[1;2H{op}").as_bytes());
        assert!(
            !term.grid().row(vacated).unwrap().has_early_wrap_hole(),
            "{name}: the vacated row {vacated}"
        );
    }
}

fn in_history(mut term: Terminal, text: &str) -> Terminal {
    term.process(text.as_bytes());
    term.process(b"\r\n\r\n\r\n\r\n\r\n");
    assert!(term.grid().scrollback_lines() >= 2, "fixture: in history");
    term
}

#[test]
fn a_full_rows_last_cell_survives_widening_out_of_history() {
    let cases: [(&str, u16, &str); 4] = [
        ("abc 中fg", 4, "abc 中fg"),
        ("hello你好", 5, "hello你好"),
        ("abcd中fg", 4, "abcd中fg"),
        ("abcdefghij中xyz", 10, "abcdefghij中xyz"),
    ];
    for (text, cols, want) in cases {
        type Make = fn(u16) -> Terminal;
        let makes: [(&str, Make); 2] = [
            ("ring", |cols| Terminal::new(3, cols)),
            ("tiered", |cols| tiered(3, cols, 1)),
        ];
        for (tier, make) in makes {
            for offload in [false, true] {
                let mut term = in_history(make(cols), text);
                if offload {
                    offloaded_resize(&mut term, 3, 20);
                } else {
                    term.resize(3, 20);
                }
                let lines = logical_lines(&term);
                assert!(
                    lines.iter().any(|l| l == want),
                    "{text:?} at {cols} in {tier} history, offload={offload}: {lines:?}"
                );
                if offload {
                    offloaded_resize(&mut term, 3, cols);
                } else {
                    term.resize(3, cols);
                }
                let lines = logical_lines(&term);
                assert!(
                    lines.iter().any(|l| l == want),
                    "{text:?} back at {cols} in {tier} history, offload={offload}: {lines:?}"
                );
            }
        }
    }
}

#[test]
fn a_hole_a_rewrap_made_is_dropped_by_the_next_rewrap() {
    const LINE: &str = "abcdefghi中xyz";
    let mut term = Terminal::new(6, 20);
    term.process(LINE.as_bytes());
    term.resize(6, 10);
    term.resize(6, 20);
    assert_eq!(logical_lines(&term), vec![LINE.to_string()], "screen");
    for offload in [false, true] {
        let mut term = in_history(tiered(3, 20, 100), LINE);
        for cols in [10, 20] {
            if offload {
                offloaded_resize(&mut term, 3, cols);
            } else {
                term.resize(3, cols);
            }
        }
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|l| l == LINE),
            "ring history, offload={offload}: {lines:?}"
        );
    }
}

#[test]
fn a_coloured_early_wrap_hole_stays_out_of_history() {
    type Make = fn(u16) -> Terminal;
    let makes: [(&str, Make); 2] = [
        ("ring", |cols| Terminal::new(3, cols)),
        ("tiered", |cols| tiered(3, cols, 1)),
    ];
    for (tier, make) in makes {
        for offload in [false, true] {
            let mut term = in_history(make(10), "\x1b[41mabcdefghi中\x1b[0mxyz");
            if offload {
                offloaded_resize(&mut term, 3, 20);
            } else {
                term.resize(3, 20);
            }
            let lines = logical_lines(&term);
            assert!(
                lines.iter().any(|l| l == "abcdefghi中xyz"),
                "{tier} history, offload={offload}: {lines:?}"
            );
        }
    }
}

#[test]
fn a_coloured_early_wrap_hole_is_no_text_before_any_rewrap() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let text = "\x1b[44mabcdefghi中\x1b[0mxyz";
    let mut term = Terminal::new(4, 10);
    term.process(text.as_bytes());
    assert!(
        logical_lines(&term).iter().any(|l| l == "abcdefghi中xyz"),
        "on screen: {:?}",
        logical_lines(&term)
    );
    let sel = term.text_selection_mut();
    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(1, 9, SelectionSide::Right);
    sel.complete_selection();
    assert_eq!(
        term.selection_to_string().as_deref(),
        Some("abcdefghi中xyz"),
        "a copy on screen"
    );
    type Make = fn(u16) -> Terminal;
    let makes: [(&str, Make); 2] = [
        ("ring", |cols| Terminal::new(3, cols)),
        ("tiered", |cols| tiered(3, cols, 1)),
    ];
    for (tier, make) in makes {
        let term = in_history(make(10), text);
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|l| l == "abcdefghi中xyz"),
            "{tier} history: {lines:?}"
        );
    }
}

fn bg_in_last_column(term: &mut Terminal, row: usize) -> [u8; 3] {
    let (rows, cols) = (term.grid().rows(), term.grid().cols());
    let frame = term.cell_frame(usize::from(rows), usize::from(cols));
    frame.cells[row].get(usize::from(cols) - 1).map_or_else(
        || {
            let [_, r, g, b] = frame.default_bg.to_be_bytes();
            [r, g, b]
        },
        |cell| cell.bg,
    )
}

#[test]
fn a_coloured_early_wrap_hole_keeps_its_paint_in_history() {
    for text in [
        "\x1b[41mabcdefghi中\x1b[0mxyz",
        "\x1b[48;2;17;34;51mabcdefghi中\x1b[0mxyz",
    ] {
        for (tier, ring) in [("ring", 8), ("store", 0)] {
            let mut term = tiered(3, 10, ring);
            term.process(text.as_bytes());
            let painted = bg_in_last_column(&mut term, 0);
            term.process(b"\r\n\r\n");
            assert_eq!(term.grid().scrollback_lines(), 1, "fixture");
            term.scroll_to_top();
            assert_eq!(
                bg_in_last_column(&mut term, 0),
                painted,
                "{text:?}, {tier}: the newest history line"
            );
            assert_eq!(term.grid().row_text(0).as_deref(), Some("abcdefghi"));
            term.scroll_to_bottom();
            term.process(b"1\r\n2\r\n3");
            term.scroll_to_top();
            assert_eq!(
                term.grid().ring_buffer_scrollback() > 0,
                tier == "ring",
                "fixture: {tier}"
            );
            assert_eq!(
                bg_in_last_column(&mut term, 0),
                painted,
                "{text:?}, {tier}: scrolled back"
            );
            assert_eq!(term.grid().row_text(0).as_deref(), Some("abcdefghi"));
        }
    }
}

#[test]
fn a_coloured_early_wrap_hole_keeps_its_paint_back_on_the_screen() {
    for colour in ["41", "48;2;17;34;51"] {
        let text = format!("\x1b[{colour}mabcdefghijklmnopqrs中\x1b[0mxyz");
        for (tier, ring) in [("ring", 8), ("store", 0)] {
            for way in ["rows grow", "unscroll", "widening"] {
                let mut term = tiered(4, 10, ring);
                term.process(text.as_bytes());
                let painted = bg_in_last_column(&mut term, 1);
                term.process(b"\r\n\r\n\r\n\r\n");
                assert_eq!(term.grid().scrollback_lines(), 3, "fixture: in history");
                let want = match way {
                    "rows grow" => {
                        term.resize(8, 10);
                        "klmnopqrs"
                    }
                    "unscroll" => {
                        term.process(b"\x1b[3+T");
                        "klmnopqrs"
                    }
                    _ => {
                        term.process(b"\x1b[H0123456789ABCDEFGHI\r\n0123456789ABCDEFGHI");
                        term.resize(4, 20);
                        "abcdefghijklmnopqrs"
                    }
                };
                let rows = screen(&term);
                let row = rows
                    .iter()
                    .position(|r| r == want)
                    .unwrap_or_else(|| panic!("{way}, {tier}: the row is back: {rows:?}"));
                assert_eq!(
                    bg_in_last_column(&mut term, row),
                    painted,
                    "{colour}, {tier}: back on the screen by {way}"
                );
            }
        }
    }
}

#[test]
fn a_recoloured_hole_takes_the_print_paths_paint_off_the_screen() {
    let red = {
        let mut term = Terminal::new(3, 10);
        term.process("\x1b[41mabcdefghi中".as_bytes());
        bg_in_last_column(&mut term, 0)
    };
    let recoloured = |term: &mut Terminal| {
        term.process("\x1b[41mabcdefghi中\x1b[0mxyz".as_bytes());
        term.process(b"\x1b[44m\x1b[1;10;1;10$z\x1b[0m");
        assert!(term.grid().row(0).unwrap().has_early_wrap_hole(), "fixture");
        let blue = bg_in_last_column(term, 0);
        assert_ne!(blue, red, "fixture: recoloured");
    };
    let mut term = Terminal::new(3, 10);
    recoloured(&mut term);
    term.resize(3, 20);
    assert_eq!(logical_lines(&term), vec!["abcdefghi中xyz".to_string()]);
    term.resize(3, 10);
    assert_eq!(bg_in_last_column(&mut term, 0), red, "after a rewrap");
    let mut term = tiered(3, 10, 8);
    recoloured(&mut term);
    term.process(b"\r\n\r\n");
    term.scroll_to_top();
    assert_eq!(bg_in_last_column(&mut term, 0), red, "in history");
    assert_eq!(term.grid().row_text(0).as_deref(), Some("abcdefghi"));
}

#[test]
fn a_space_written_into_a_hole_later_is_kept() {
    let mut term = Terminal::new(6, 10);
    term.process("abcdefghi中xyz".as_bytes());
    term.process(b"\x1b[1;10H \x1b[2;4H");
    term.resize(6, 20);
    assert_eq!(logical_lines(&term), vec!["abcdefghi 中xyz".to_string()]);
}

#[test]
fn an_edit_over_the_holes_column_ends_the_hole() {
    for (text, edit, expected) in [
        ("ab 中xy", &b"\x1b[1;1H\x1b[@"[..], " ab 中xy"),
        ("abc中xy", b"\x1b[1;1H\x1b[P", "bc  中xy"),
        ("abc中xy", "\x1b[1;3H文\x1b[1;3Hx".as_bytes(), "abx 中xy"),
        ("abc中xy", "\x1b[1;1H文文\x1b[1;3Hx".as_bytes(), "文x 中xy"),
        ("abc中xy", b"\x1b[120;1;4;1;4$x\x1b[1;4;1;4$z", "abc 中xy"),
    ] {
        let mut term = Terminal::new(6, 4);
        term.process(text.as_bytes());
        assert!(
            term.grid().row(1).is_some_and(|r| r.is_wrapped()),
            "fixture: the wide glyph early-wrapped ({text:?})"
        );
        term.process(edit);
        term.resize(6, 20);
        assert_eq!(
            logical_lines(&term),
            vec![expected.to_string()],
            "{text:?} edited by {edit:?}"
        );
    }
}

#[test]
fn a_line_straddling_the_history_boundary_keeps_its_full_rows_last_cells() {
    const LINE: &str = "0123456789abcdefghij中xyz";
    for offload in [false, true] {
        let mut term = tiered(4, 10, 4);
        term.process(format!("{LINE}\r\n$ \r\n").as_bytes());
        assert!(
            term.grid().row(1).is_some_and(|r| r.is_wrapped()),
            "fixture: the tail leads the screen"
        );
        let lines = logical_lines(&term);
        assert!(lines.iter().any(|l| l == LINE), "fixture {lines:?}");
        if offload {
            offloaded_resize(&mut term, 4, 30);
        } else {
            term.resize(4, 30);
        }
        let lines = logical_lines(&term);
        assert!(
            lines.iter().any(|l| l == LINE),
            "offload={offload}: {lines:?}"
        );
    }
}

#[test]
fn a_line_split_at_the_screen_keeps_its_text_when_the_ring_rejoins_it() {
    let line: String = (0..170)
        .map(|i| char::from(b'0' + (i % 10) as u8))
        .chain("abcdefghijKLMNO".chars())
        .collect();
    for (ring_only, offload) in [(true, false), (false, false), (false, true)] {
        let mut term = if ring_only {
            Terminal::new(4, 10)
        } else {
            tiered(4, 10, 100)
        };
        let resize = |term: &mut Terminal, cols: u16| {
            if offload {
                offloaded_resize(term, 4, cols);
            } else {
                term.resize(4, cols);
            }
        };
        term.process(format!("{line}\r\nx\r\ny").as_bytes());
        term.process(b"\x1b[H");
        resize(&mut term, 7);
        let case = format!("ring_only={ring_only} offload={offload}");
        assert!(
            logical_lines(&term).contains(&line),
            "{case}: fixture {:?}",
            logical_lines(&term)
        );
        term.process(b"\x1b[4;1H");
        resize(&mut term, 10);
        let lines = logical_lines(&term);
        assert!(lines.contains(&line), "{case}: {lines:?}");
    }
}

#[test]
fn tiered_history_keeps_spaces_at_reflow_chunk_ends() {
    let lines = [
        "the quick brown fox jumps over the lazy dog",
        "pack my box with five dozen liquor jugs",
        "sphinx of black quartz judge my vow",
        "how vexingly quick daft zebras jump",
    ];
    let mut term = tiered(6, 20, 1000);
    for line in lines {
        term.process(format!("{line}\r\n").as_bytes());
    }
    term.resize(6, 10);
    term.resize(6, 20);
    assert_eq!(logical_lines(&term), lines.to_vec());
}

#[test]
fn offloaded_tiered_history_keeps_spaces_at_reflow_chunk_ends() {
    let lines = [
        "the quick brown fox jumps over the lazy dog",
        "sphinx of black quartz judge my vow",
    ];
    let mut term = tiered(6, 20, 64);
    for line in lines {
        term.process(format!("{line}\r\n").as_bytes());
    }
    offloaded_resize(&mut term, 6, 10);
    for i in 0..74 {
        term.process(format!("n{i}\r\n").as_bytes());
    }
    offloaded_resize(&mut term, 6, 20);
    let got = logical_lines(&term);
    for line in lines {
        assert!(got.iter().any(|l| l == line), "{line:?} in {got:?}");
    }
}

#[test]
fn a_long_run_of_printed_spaces_survives_offloaded_width_changes() {
    let line = format!("ab{}cd", " ".repeat(60));
    let mut term = Terminal::with_scrollback(10, 40, 8, Scrollback::with_defaults());
    term.process(format!("{line}\r\n").as_bytes());
    offloaded_resize(&mut term, 10, 25);
    for i in 0..30 {
        term.process(format!("l{i}\r\n").as_bytes());
    }
    offloaded_resize(&mut term, 10, 33);
    offloaded_resize(&mut term, 10, 80);
    let got = logical_lines(&term);
    assert!(got.contains(&line), "{got:?}");
}

#[test]
fn a_cursor_after_a_printed_space_keeps_its_column() {
    for widths in [[10u16, 20], [30, 40]] {
        let mut term = Terminal::new(4, 20);
        term.process(b"Name: ");
        for cols in widths {
            term.resize(4, cols);
        }
        term.process(b"Bob");
        assert_eq!(
            term.grid().row_text(0).as_deref(),
            Some("Name: Bob"),
            "through {widths:?}"
        );
    }
}

#[test]
fn a_restored_engine_rewraps_as_the_live_one() {
    for input in [
        "abc\t\t中xy",
        "abc\x1b[10G中xy",
        "abc\x1b[9C中xy",
        "abcdefghijklm\x1b[1;5H\x1b[P",
    ] {
        for offload in [false, true] {
            let build = || {
                if offload {
                    tiered(4, 10, 100)
                } else {
                    Terminal::new(4, 10)
                }
            };
            let mut live = build();
            live.process(input.as_bytes());
            let cp = live.checkpoint_carry(0).expect("Ground");
            let mut fresh = Terminal::from_checkpoint(&cp);
            let mut adopted = build();
            adopted.restore_checkpoint(&cp);
            let mut widened = Vec::new();
            for term in [&mut live, &mut fresh, &mut adopted] {
                if offload {
                    offloaded_resize(term, 4, 20);
                } else {
                    term.resize(4, 20);
                }
                widened.push(screen(term));
            }
            assert_eq!(
                widened[0], widened[1],
                "from_checkpoint {input:?} offload={offload}"
            );
            assert_eq!(
                widened[0], widened[2],
                "restore_checkpoint {input:?} offload={offload}"
            );
        }
    }
}

#[test]
fn a_wide_glyph_one_column_cannot_hold_leaves_no_link_in_history() {
    let link_on = "\x1b]8;;https://example.com/\x1b\\";
    let link_off = "\x1b]8;;\x1b\\";
    for glyph in ["\u{1F680}", "\u{4E2D}"] {
        let line = format!("\x1b[4:3m\x1b[58;5;1m{link_on}{glyph}{link_off}\x1b[m\r\n");
        for offload in [false, true] {
            let mut term = if offload {
                tiered(3, 4, 2)
            } else {
                let mut term = Terminal::new(3, 4);
                term.set_scrollback_line_limit(Some(100));
                term
            };
            for _ in 0..6 {
                term.process(line.as_bytes());
            }
            let what = format!("{glyph:?} offload={offload}");
            assert!(term.hyperlink_at(0, 0).is_some(), "{what}: linked before");
            let history = term.grid().scrollback_lines();
            for (rows, cols) in [(3, 1), (12, 1)] {
                if offload {
                    offloaded_resize(&mut term, rows, cols);
                } else {
                    term.resize(rows, cols);
                }
            }
            assert!(
                term.grid().scrollback_lines() < history,
                "{what}: the rows grow reveals history"
            );
            for row in 0..term.rows() {
                assert_eq!(
                    term.hyperlink_at(row, 0),
                    None,
                    "{what}: row {row} holds no glyph, so it links nothing"
                );
            }
            let (rows, cols) = (term.rows(), term.cols());
            for (at, extra) in term.grid().extras().iter() {
                assert!(
                    at.row >= rows
                        || at.col >= cols
                        || (extra.hyperlink().is_none() && extra.underline_color_index().is_none()),
                    "{what}: ({}, {}) keeps the dropped glyph's {extra:?}",
                    at.row,
                    at.col
                );
            }
        }
    }
}

fn view_style_strays(
    term: &mut Terminal,
    styled: impl Fn(char) -> bool,
    what: &str,
) -> Vec<String> {
    let (rows, cols) = (term.rows(), term.cols());
    let history = term.grid().scrollback_lines();
    let mut strays = Vec::new();
    term.scroll_to_bottom();
    for offset in 0..=history {
        if offset > 0 {
            term.scroll_display(1);
        }
        for r in 0..rows {
            let view = term.grid().visible_row_view(r);
            for c in 0..cols {
                let ch = view.col_char(c);
                let lead = if ch == '\0' && c > 0 {
                    view.col_char(c - 1)
                } else {
                    ch
                };
                let want = styled(lead);
                let link = term.hyperlink_at_visible(r, c).is_some();
                let underline = view.cell(c).is_some_and(|cell| {
                    view.cell_data(c, cell)
                        .cell_extra()
                        .is_some_and(|extra| extra.underline_color_u32().is_some())
                });
                if link != want || underline != want {
                    strays.push(format!(
                        "{what}: offset {offset} ({r}, {c}) {lead:?} link={link} underline={underline}"
                    ));
                }
            }
        }
    }
    term.scroll_to_bottom();
    strays
}

#[test]
fn a_one_column_history_row_scrolled_back_styles_no_blank() {
    let link_on = "\x1b]8;;https://example.com/\x1b\\";
    let link_off = "\x1b]8;;\x1b\\";
    for glyph in ["\u{1F680}", "\u{4E2D}"] {
        let line = format!("\x1b[4:3m\x1b[58;5;1m{link_on}{glyph}{link_off}\x1b[m\r\n");
        for offload in [false, true] {
            let mut term = if offload {
                tiered(3, 4, 2)
            } else {
                let mut term = Terminal::new(3, 4);
                term.set_scrollback_line_limit(Some(100));
                term
            };
            for _ in 0..6 {
                term.process(line.as_bytes());
            }
            if offload {
                offloaded_resize(&mut term, 3, 1);
            } else {
                term.resize(3, 1);
            }
            let what = format!("{glyph:?} offload={offload}");
            assert!(
                term.grid().scrollback_lines() >= 3,
                "{what}: history to scroll back through"
            );
            let strays = view_style_strays(&mut term, |_| false, &what);
            assert!(strays.is_empty(), "{strays:#?}");
        }
    }
}

#[test]
fn a_vs16_glyph_kept_narrow_in_mid_row_keeps_the_styles_after_it_in_place() {
    let text = "abc\u{2764}\u{FE0F} \x1b[4:3m\x1b[58;5;1m\x1b]8;;https://e.x/\x1b\\\u{1F680}\x1b]8;;\x1b\\\x1b[m";
    let rocket = |ch: char| ch == '\u{1F680}';
    for offload in [false, true] {
        let mut term = if offload {
            tiered(3, 4, 2)
        } else {
            let mut term = Terminal::new(3, 4);
            term.set_scrollback_line_limit(Some(100));
            term
        };
        let resize = |term: &mut Terminal, rows: u16, cols: u16| {
            if offload {
                offloaded_resize(term, rows, cols);
            } else {
                term.resize(rows, cols);
            }
        };
        term.process(text.as_bytes());
        resize(&mut term, 3, 8);
        let what = format!("offload={offload}");
        let row = term.grid().visible_row_view(0);
        let cells: String = (0..8).map(|c| row.col_char(c)).collect();
        assert_eq!(
            cells, "abc\u{2764} \u{1F680}\0 ",
            "{what}: the screen keeps the heart narrow in mid row"
        );
        let strays = view_style_strays(&mut term, rocket, &format!("{what} on screen"));
        assert!(strays.is_empty(), "{strays:#?}");
        term.process(b"\r\n\r\n\r\n\r\n");
        let strays = view_style_strays(&mut term, rocket, &format!("{what} in history"));
        assert!(strays.is_empty(), "{strays:#?}");
        for _ in 0..8 {
            term.process(b"x\r\n");
        }
        let strays = view_style_strays(&mut term, rocket, &format!("{what} deeper"));
        assert!(strays.is_empty(), "{strays:#?}");
        resize(&mut term, 12, 9);
        let strays = view_style_strays(&mut term, rocket, &format!("{what} rewrapped"));
        assert!(strays.is_empty(), "{strays:#?}");
    }
}

#[test]
fn a_vs16_glyph_kept_narrow_at_the_edge_and_its_wrapped_skin_tone_stay_apart() {
    let text = "abcdefghi\u{261D}\u{FE0F}\u{1F3FD}z";
    let top = |term: &Terminal| -> Vec<(String, bool)> {
        let mut cells = term.display_row_grapheme_cells(0).expect("the top row");
        cells.truncate(14);
        cells
    };
    let mut want: Vec<(String, bool)> = "abcdefghi"
        .chars()
        .map(|c| (c.to_string(), false))
        .collect();
    want.extend([
        ("\u{261D}\u{FE0F}".to_owned(), false),
        ("\u{1F3FD}".to_owned(), true),
        (String::new(), false),
        ("z".to_owned(), false),
        (" ".to_owned(), false),
    ]);

    let mut term = Terminal::new(3, 10);
    term.process(text.as_bytes());
    term.resize(3, 20);
    assert_eq!(top(&term), want, "on screen");

    type Make = fn() -> Terminal;
    let makes: [(&str, Make); 2] = [
        ("ring", || Terminal::new(3, 10)),
        ("tiered", || tiered(3, 10, 1)),
    ];
    for (tier, make) in makes {
        for offload in [false, true] {
            let mut term = in_history(make(), text);
            if offload {
                offloaded_resize(&mut term, 3, 20);
            } else {
                term.resize(3, 20);
            }
            term.scroll_to_top();
            assert_eq!(top(&term), want, "{tier} history, offload={offload}");
        }
    }
}
