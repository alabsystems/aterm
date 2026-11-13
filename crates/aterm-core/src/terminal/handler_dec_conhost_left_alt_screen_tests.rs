// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;

const ROWS: u16 = 10;
const PROMPT: &[u8] = b"\x1b]133;A\x07PS> \x1b]133;B\x07";

fn all_rows(term: &Terminal) -> Vec<String> {
    let first = term.grid().oldest_absolute_row();
    let n = term.grid().scrollback_lines() + usize::from(ROWS);
    (first..)
        .take(n)
        .map(|abs| {
            term.abs_row_text(abs)
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn count(rows: &[String], want: &str) -> usize {
    rows.iter().filter(|r| *r == want).count()
}

fn blank_repaint_then_enter() -> Vec<u8> {
    let mut read = b"\x1b[?25l\x1b[H".to_vec();
    for _ in 1..ROWS {
        read.extend_from_slice(b"\x1b[K\r\n");
    }
    read.extend_from_slice(b"\x1b[K\x1b[?25h\x1b[?1049h");
    read
}

fn stale_primary_repaint() -> Vec<u8> {
    let mut read = b"\x1b[?25l\x1b[H".to_vec();
    for row in ["history 1", "history 2", "PS> less file"] {
        read.extend_from_slice(row.as_bytes());
        read.extend_from_slice(b"\x1b[K\r\n");
    }
    read.extend_from_slice(b"\x1b[K\x1b[4;1H\x1b[?25h");
    read
}

fn recovered_with_two_commands(conhost: bool) -> Terminal {
    let mut term = Terminal::new(ROWS, 40);
    if conhost {
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h\x1b[?1004h");
    }
    term.process(b"history 1\r\nhistory 2\r\n");
    term.process(PROMPT);
    term.process(b"less file\r\n\x1b]133;C\x07");
    term.process(b"\x1b[?1049h");
    term.process(b"\x1b[H\x1b[2Jpager 1\r\npager 2\r\n:");
    term.process(b"\r");
    term.process(b"\n\x1b]133;D;-1\x07");
    term.process(PROMPT);
    assert!(
        !term.modes().alternate_screen,
        "the prompt recovered the tab"
    );
    for cmd in ["one", "two"] {
        term.process(format!("echo {cmd}\r\n\x1b]133;C\x07{cmd}\r\n\x1b]133;D;0\x07").as_bytes());
        term.process(PROMPT);
    }
    term.process(b"less file\r\n\x1b]133;C\x07");
    term
}

const TYPED_AFTER: [&str; 4] = ["PS> echo one", "one", "PS> echo two", "two"];

#[test]
fn a_second_pager_under_conhost_keeps_what_was_typed_after_the_recovery() {
    let mut term = recovered_with_two_commands(true);
    assert_eq!(
        term.grid().scrollback_lines(),
        3,
        "the recovery moved the history rows out of conhost's reach, nothing since"
    );
    term.process(&blank_repaint_then_enter());
    assert!(term.modes().alternate_screen);
    term.process(b"\x1b[Hsecond pager");
    term.process(b"\x1b[10;1H\x1b[?1049l");
    term.process(&stale_primary_repaint());
    term.process(b"\x1b]133;D;0\x07");
    term.process(PROMPT);
    let rows = all_rows(&term);
    for want in TYPED_AFTER {
        assert_eq!(
            count(&rows, want),
            1,
            "{want:?} must survive once: {rows:?}"
        );
    }
    assert_eq!(term.row_text(2).unwrap().trim_end(), "PS> less file");
    assert_eq!(term.row_text(3).unwrap().trim_end(), "PS>");
}

#[test]
fn without_conhost_the_same_bytes_move_nothing() {
    let mut term = recovered_with_two_commands(false);
    term.process(&blank_repaint_then_enter());
    assert!(term.modes().alternate_screen);
    term.process(b"\x1b[?1049l");
    term.process(&stale_primary_repaint());
    assert_eq!(term.grid().scrollback_lines(), 0);
    let rows = all_rows(&term);
    for want in TYPED_AFTER {
        assert_eq!(count(&rows, want), 0, "{rows:?}");
    }
}

#[test]
fn a_stray_1049l_under_conhost_keeps_the_rows_from_the_primary_repaint() {
    let mut term = recovered_with_two_commands(true);
    term.process(b"\x1b[?1049l");
    assert!(!term.modes().alternate_screen);
    term.process(&stale_primary_repaint());
    let rows = all_rows(&term);
    for want in TYPED_AFTER {
        assert_eq!(count(&rows, want), 1, "{rows:?}");
    }
    let kept = term.grid().scrollback_lines();
    term.process(b"\x1b[?1049l");
    assert_eq!(term.grid().scrollback_lines(), kept);
}

#[test]
fn the_host_forced_leave_under_conhost_keeps_the_rows_too() {
    let mut term = Terminal::new(ROWS, 40);
    term.set_win32_input_enabled(true);
    term.process(b"\x1b[?9001h");
    term.process(b"C:\\>less file\r\n\x1b[?1049h\x1b[H\x1b[2J");
    for i in 1..ROWS {
        term.process(format!("pager {i}\r\n").as_bytes());
    }
    term.process(b":\r\nC:\\>echo stuck\r\nstuck\r\n\r\nC:\\>");
    assert!(term.leave_alternate_screen());
    assert_eq!((term.cursor().row, term.cursor().col), (ROWS - 1, 4));
    assert_eq!(all_rows(&term)[0], "C:\\>less file");
    assert_eq!(term.grid().scrollback_lines(), 1);
    let archived = term.alt_archive().texts();
    for want in ["C:\\>echo stuck", "stuck", "C:\\>"] {
        assert!(archived.iter().any(|r| r == want), "{want:?}: {archived:?}");
    }
    term.process(b"echo after\r\nafter\r\n\r\nC:\\>");
    term.process(&blank_repaint_then_enter());
    term.process(b"\x1b[?1049l");
    let rows = all_rows(&term);
    assert_eq!(count(&rows, "C:\\>less file"), 1, "{rows:?}");
    assert_eq!(count(&rows, "after"), 1, "{rows:?}");
    assert_eq!(count(&rows, "    echo after"), 1, "{rows:?}");
}

#[test]
fn a_programs_9001_does_not_fake_a_console_host_for_the_recovery() {
    let mut term = Terminal::new(ROWS, 40);
    term.process(b"\x1b[?9001h");
    term.process(b"$ less file\r\n\x1b[?1049h\x1b[H\x1b[2J");
    for i in 1..ROWS {
        term.process(format!("pager {i}\r\n").as_bytes());
    }
    assert!(term.leave_alternate_screen());
    assert_eq!((term.cursor().row, term.cursor().col), (1, 0));
    assert_eq!(term.grid().scrollback_lines(), 0);
    assert_eq!(term.row_text(0).unwrap().trim_end(), "$ less file");
}

fn recovered_on_the_bottom_row(conhost: bool) -> Terminal {
    let mut term = Terminal::new(ROWS, 40);
    if conhost {
        term.set_win32_input_enabled(true);
        term.process(b"\x1b[?9001h\x1b[?1004h");
    }
    term.process(b"history 1\r\nhistory 2\r\n");
    term.process(PROMPT);
    term.process(b"less file\r\n\x1b]133;C\x07\x1b[?1049h\x1b[H\x1b[2J");
    for i in 1..ROWS {
        term.process(format!("pager {i}\r\n").as_bytes());
    }
    term.process(b":");
    term.process(b"\r\n\x1b]133;D;-1\x07");
    term.process(PROMPT);
    assert!(!term.modes().alternate_screen);
    term
}

const HISTORY: [&str; 3] = ["history 1", "history 2", "PS> less file"];

#[test]
fn under_conhost_the_prompt_lands_on_conhosts_row_and_absolute_moves_agree() {
    let mut term = recovered_on_the_bottom_row(true);
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (ROWS - 1, 4), "after `PS> `");
    assert_eq!(term.grid().scrollback_lines(), 3);
    assert_eq!(all_rows(&term)[..3], HISTORY);
    let head = format!("echo {}", "x".repeat(31));
    term.process(head.as_bytes());
    term.process(format!("\r\n\x1b[{};40Hx", ROWS - 1).as_bytes());
    term.process(b"TAIL");
    assert_eq!(
        term.row_text(usize::from(ROWS - 2)).unwrap().trim_end(),
        format!("PS> {head}")
    );
    assert_eq!(
        term.row_text(usize::from(ROWS - 1)).unwrap().trim_end(),
        "TAIL",
        "the tail is on the row right after its head"
    );
}

#[test]
fn a_conhost_repaint_after_the_recovery_cannot_reach_the_history() {
    let mut term = recovered_on_the_bottom_row(true);
    term.process(b"echo one\r\n\x1b]133;C\x07one\r\n\x1b]133;D;0\x07");
    term.process(PROMPT);
    let mut repaint = b"\x1b[?25l\x1b[H".to_vec();
    let conhost_buffer = (4..ROWS)
        .map(|i| format!("pager {i}"))
        .chain([":", "PS> echo one", "one", "PS> "].map(str::to_string));
    for (i, row) in conhost_buffer.enumerate() {
        if i > 0 {
            repaint.extend_from_slice(b"\r\n");
        }
        repaint.extend_from_slice(row.as_bytes());
        repaint.extend_from_slice(b"\x1b[K");
    }
    repaint.extend_from_slice(format!("\x1b[{ROWS};5H\x1b[?25h").as_bytes());
    term.process(&repaint);
    let rows = all_rows(&term);
    for want in HISTORY.into_iter().chain(["PS> echo one", "one"]) {
        assert_eq!(count(&rows, want), 1, "{want:?}: {rows:?}");
    }
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (ROWS - 1, 4));
    assert_eq!(
        term.row_text(usize::from(ROWS - 2)).unwrap().trim_end(),
        "one"
    );
}

#[test]
fn without_conhost_the_prompt_returns_to_the_row_1049_saved() {
    let term = recovered_on_the_bottom_row(false);
    assert_eq!(term.grid().scrollback_lines(), 0);
    for (row, want) in HISTORY.into_iter().enumerate() {
        assert_eq!(term.row_text(row).unwrap().trim_end(), want);
    }
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (3, 4));
}

#[test]
fn a_live_enter_or_a_reset_ends_the_state() {
    for end in [&b"\x1b[?1049h\x1b[?1049l"[..], b"\x1bc"] {
        let mut term = recovered_with_two_commands(true);
        term.process(end);
        let kept = term.grid().scrollback_lines();
        term.process(&blank_repaint_then_enter());
        term.process(b"\x1b[?1049l");
        assert_eq!(term.grid().scrollback_lines(), kept, "{end:?}");
    }
}

#[test]
fn a_partial_scroll_region_is_left_alone() {
    let mut term = recovered_with_two_commands(true);
    term.process(b"\x1b[1;5r");
    let before = all_rows(&term);
    term.process(b"\x1b[?1049l");
    assert_eq!(all_rows(&term), before);
    term.process(b"\x1b[r\x1b[?1049l");
    assert_eq!(all_rows(&term), before, "spent by the first switch");
}
