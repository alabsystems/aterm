// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;

fn on_alt_with_history() -> Terminal {
    let mut term = Terminal::new(5, 40);
    for i in 0..8 {
        term.process(format!("main line {i}\r\n").as_bytes());
    }
    term.process(b"$ less file\r\n\x1b[?1049h\x1b[H\x1b[2Jpager row 1\r\npager row 2");
    assert!(term.modes().alternate_screen);
    assert_eq!(
        term.grid().scrollback_lines(),
        0,
        "the alt grid has no scrollback"
    );
    term
}

#[test]
fn host_leave_restores_the_main_grid_and_its_scrollback() {
    let mut term = on_alt_with_history();
    assert!(term.leave_alternate_screen());
    assert!(!term.modes().alternate_screen);
    assert_eq!(term.grid().scrollback_lines(), 5);
    let screen: Vec<String> = (0..5).map(|r| term.row_text(r).unwrap()).collect();
    assert_eq!(screen[3].trim_end(), "$ less file");
    assert!(
        screen.iter().all(|r| !r.contains("pager")),
        "the pager rows stay on the alt grid, not the restored screen: {screen:?}"
    );
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (4, 0));
}

#[test]
fn host_leave_on_the_main_screen_is_a_refused_no_op() {
    let mut term = Terminal::new(5, 40);
    term.process(b"hello\r\n");
    let before = term.row_text(0).unwrap();
    assert!(!term.leave_alternate_screen());
    assert!(!term.modes().alternate_screen);
    assert_eq!(term.row_text(0).unwrap(), before);
    assert_eq!(term.grid().scrollback_lines(), 0);
}

#[test]
fn host_leave_is_one_shot_and_a_later_app_can_enter_again() {
    let mut term = on_alt_with_history();
    assert!(term.leave_alternate_screen());
    term.process(b"\x1b[?1049h\x1b[Hsecond app");
    assert!(term.modes().alternate_screen);
    assert_eq!(term.row_text(0).unwrap().trim_end(), "second app");
    term.process(b"\x1b[?1049l");
    assert!(!term.modes().alternate_screen);
}

#[test]
fn host_leave_of_a_mode_47_screen_keeps_the_cursor_where_the_app_had_it() {
    let mut term = Terminal::new(5, 40);
    term.process(b"one\r\n\x1b[?1049h\x1b[5;9Hpager\x1b[?1049l");
    assert_eq!(
        (term.cursor().row, term.cursor().col),
        (1, 0),
        "the clean 1049 exit"
    );
    term.process(b"two\r\n\x1b[?47h\x1b[3;5Hdead app");
    assert!(term.modes().alternate_screen);
    assert!(term.leave_alternate_screen());
    assert!(!term.modes().alternate_screen);
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (2, 12));
    assert_eq!(term.row_text(0).unwrap().trim_end(), "one");
    assert_eq!(term.row_text(1).unwrap().trim_end(), "two");
    let archived = term.alt_archive().texts();
    assert_eq!(archived.last().map(String::as_str), Some("    dead app"));
    term.process(b"\x1b[?47h");
    assert!(
        (0..5).all(|r| !term.row_text(r).unwrap().contains("dead app")),
        "a later ?47h re-showed the dead screen"
    );
}

#[test]
fn host_leave_of_a_1049_screen_entered_after_a_47_one_restores_its_save() {
    let mut term = Terminal::new(5, 40);
    term.process(b"one\r\n\x1b[?47hx\x1b[?47l\r\ntwo\r\n");
    term.process(b"\x1b[?1049h\x1b[4;7Hpager");
    assert!(term.leave_alternate_screen());
    let cursor = term.cursor();
    assert_eq!(
        (cursor.row, cursor.col),
        (3, 0),
        "the cursor `?1049h` saved"
    );
}

#[test]
fn a_dead_apps_scroll_region_and_input_modes_do_not_outlive_it() {
    use crate::terminal::{MouseEncoding, MouseMode};
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[?1004hhistory\r\n$ vim file\r\n");
    term.process(b"\x1b[?1049h\x1b[1;5r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=\x1b[?2026h");
    term.process(b"\x1b[Hbuffer row\r\n");
    term.process(b"\r\n\x1b]133;D;-1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    assert!(!term.modes().alternate_screen);
    let modes = term.modes();
    assert_eq!(modes.mouse_mode, MouseMode::None);
    assert_eq!(modes.mouse_encoding, MouseEncoding::X10);
    assert!(!modes.application_cursor_keys);
    assert!(!modes.application_keypad);
    assert!(!modes.synchronized_output);
    assert!(modes.focus_reporting);
    let region = term.grid().scroll_region();
    assert_eq!((region.top, region.bottom), (0, 9));
    term.process(b"seq 20\r\n");
    for i in 0..20 {
        term.process(format!("out {i}\r\n").as_bytes());
    }
    let first = term.grid().oldest_absolute_row();
    let n = term.grid().scrollback_lines() + 10;
    let rows: Vec<String> = (first..)
        .take(n)
        .map(|abs| {
            term.abs_row_text(abs)
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
        .collect();
    for want in ["history", "$ vim file", "$ seq 20"]
        .map(str::to_string)
        .into_iter()
        .chain((0..20).map(|i| format!("out {i}")))
    {
        assert_eq!(
            rows.iter().filter(|r| **r == want).count(),
            1,
            "{want:?}: {rows:?}"
        );
    }
}

#[test]
fn the_host_leave_releases_the_dead_apps_modes_too() {
    let mut term = on_alt_with_history();
    term.process(b"\x1b[2;4r\x1b[?1000h\x1b[?1h\x1b[?2048h\x1b[?2031h\x1b[?2004h");
    assert!(term.leave_alternate_screen());
    let region = term.grid().scroll_region();
    assert_eq!((region.top, region.bottom), (0, 4));
    assert_eq!(term.modes().mouse_mode, crate::terminal::MouseMode::None);
    assert!(!term.modes().application_cursor_keys);
    assert!(!term.modes().in_band_size_reports);
    assert!(!term.modes().report_color_scheme);
    assert!(term.modes().bracketed_paste);
}
