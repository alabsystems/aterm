// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

fn stuck_pager() -> Terminal {
    let mut term = Terminal::new(5, 40);
    term.set_alt_archive_enabled(true);
    for i in 0..8 {
        term.process(format!("main line {i}\r\n").as_bytes());
    }
    term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07less file\r\n\x1b]133;C\x07");
    term.process(b"\x1b[?1049h\x1b[H\x1b[2Jpager row 1\r\npager row 2\r\npager row 3\r\n:");
    assert!(term.modes().alternate_screen);
    term
}

const PROMPT_AFTER_KILL: &[u8] = b"\r\n\x1b]133;D;-1\x07\x1b]133;A\x07PS> \x1b]133;B\x07";

fn screen(term: &Terminal) -> Vec<String> {
    (0..5)
        .map(|r| term.row_text(r).unwrap().trim_end().to_string())
        .collect()
}

#[test]
fn a_prompt_on_the_alt_screen_leaves_it_and_paints_on_the_main_grid() {
    let mut term = stuck_pager();
    term.process(PROMPT_AFTER_KILL);
    assert!(
        !term.modes().alternate_screen,
        "the prompt left the alt screen"
    );
    assert_eq!(term.grid().scrollback_lines(), 5);
    let rows = screen(&term);
    assert_eq!(rows[3], "$ less file");
    assert_eq!(rows[4], "PS>", "the prompt is on the row after the command");
    assert!(rows.iter().all(|r| !r.contains("pager")), "{rows:?}");
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (4, 4));
}

#[test]
fn the_prompt_mark_names_the_main_grid_row_it_painted_on() {
    let mut term = stuck_pager();
    term.process(PROMPT_AFTER_KILL);
    let mark = term
        .shell
        .current_mark
        .as_ref()
        .expect("the prompt opened a mark");
    assert_eq!(mark.prompt_start_row, 9);
    assert_eq!(mark.prompt_start_col, 0);
    assert_eq!(mark.command_start_col, Some(4));
    let done = term.command_marks().last().expect("the less mark closed");
    assert_eq!(done.exit_code, Some(-1));
}

#[test]
fn what_the_stuck_screen_showed_is_in_the_archive_whole() {
    let mut term = stuck_pager();
    term.process(PROMPT_AFTER_KILL);
    let rows = term.alt_archive().texts();
    for want in ["pager row 1", "pager row 2", "pager row 3", ":"] {
        assert!(
            rows.iter().any(|r| r == want),
            "{want:?} was on the stuck screen and must be readable: {rows:?}"
        );
    }
    let gaps: Vec<_> = term.alt_archive().gaps().collect();
    assert_eq!(
        gaps.len(),
        1,
        "one leave, not one from the handler and one from the epilogue"
    );
    assert_eq!(gaps[0].kind, crate::terminal::AltArchiveGapKind::Leave);
    term.process(b"echo hi\r\nhi\r\n");
    assert_eq!(term.alt_archive().texts(), rows);
}

#[test]
fn rows_painted_in_the_same_read_as_the_prompt_are_archived() {
    let mut term = stuck_pager();
    let mut read = b"\x1b[5;1Hlast paint before death".to_vec();
    read.extend_from_slice(PROMPT_AFTER_KILL);
    term.process(&read);
    assert!(!term.modes().alternate_screen);
    let rows = term.alt_archive().texts();
    assert!(
        rows.iter().any(|r| r == "last paint before death"),
        "{rows:?}"
    );
}

#[test]
fn b_c_and_d_on_the_alt_screen_do_not_leave_it() {
    let mut term = stuck_pager();
    term.process(b"\x1b]133;D;0\x07\x1b]133;B\x07\x1b]133;C\x07");
    assert!(term.modes().alternate_screen);
    assert_eq!(term.grid().scrollback_lines(), 0);
}

#[test]
fn a_vscode_633_prompt_leaves_the_alt_screen_too() {
    let mut term = stuck_pager();
    term.process(b"\r\n\x1b]633;D;-1\x07\x1b]633;A\x07PS> \x1b]633;B\x07");
    assert!(!term.modes().alternate_screen);
    assert_eq!(screen(&term)[4], "PS>");
}

#[test]
fn a_prompt_the_nonce_gate_drops_leaves_nothing() {
    let mut term = stuck_pager();
    term.authorize_shell_integration([0x11; 32]);
    term.modes_mut().require_shell_integration_nonce = true;
    term.process(b"\r\n\x1b]133;A\x07forged> ");
    assert!(
        term.modes().alternate_screen,
        "a forged prompt cannot yank the app"
    );
    let id = "11".repeat(32);
    term.process(format!("\x1b]133;A;id={id}\x07PS> ").as_bytes());
    assert!(!term.modes().alternate_screen);
    assert_eq!(screen(&term)[4], "PS>");
}

#[test]
fn with_the_resync_policy_off_the_prompt_leaves_locally_and_requests_nothing() {
    let mut term = stuck_pager();
    term.process(PROMPT_AFTER_KILL);
    assert!(!term.modes().alternate_screen, "the local leave ran");
    assert_eq!(screen(&term)[4], "PS>");
    assert!(!term.take_alt_screen_resync_request());

    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.set_host_resyncs_alt_screen(false);
    term.process(PROMPT_AFTER_KILL);
    assert!(!term.modes().alternate_screen, "off again: the local leave");
    assert!(!term.take_alt_screen_resync_request());

    let requesting = || {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.modes().alternate_screen, "the host must leave it");
        term
    };
    let mut control = requesting();
    assert!(
        control.take_alt_screen_resync_request(),
        "control: a request is pending"
    );

    let mut term = requesting();
    term.set_host_resyncs_alt_screen(false);
    assert!(
        !term.take_alt_screen_resync_request(),
        "ALT_RESYNC_REQUEST_SURVIVED_OPT_OFF"
    );
    term.set_host_resyncs_alt_screen(true);
    term.process(PROMPT_AFTER_KILL);
    assert!(
        !term.take_alt_screen_resync_request(),
        "opting off keeps the once-per-screen memo"
    );
}

#[test]
fn under_a_resyncing_host_the_prompt_requests_the_host_leave_once() {
    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(PROMPT_AFTER_KILL);
    assert!(term.modes().alternate_screen, "no local leave");
    assert!(term.take_alt_screen_resync_request(), "one request");
    assert!(!term.take_alt_screen_resync_request(), "taken");
    term.process(PROMPT_AFTER_KILL);
    assert!(!term.take_alt_screen_resync_request());
    assert_eq!(
        term.grid().scrollback_lines(),
        0,
        "nothing scrolled locally"
    );

    term.process(b"\x1b[?1049l");
    assert!(!term.modes().alternate_screen);
    let rows = screen(&term);
    assert_eq!(rows[3], "$ less file");
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (4, 0));
}

#[test]
fn the_hosts_switch_out_of_a_dead_screen_releases_the_dead_apps_modes() {
    use crate::terminal::{MouseEncoding, MouseMode};
    const DEAD_APP_MODES: &[u8] = b"\x1b[2;4r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=";
    let released = |term: &Terminal, why: &str| {
        assert!(!term.modes().alternate_screen, "{why}");
        let modes = term.modes();
        assert_eq!(modes.mouse_mode, MouseMode::None, "{why}");
        assert_eq!(modes.mouse_encoding, MouseEncoding::X10, "{why}");
        assert!(!modes.application_cursor_keys, "{why}");
        assert!(!modes.application_keypad, "{why}");
        let region = term.grid().scroll_region();
        assert_eq!((region.top, region.bottom), (0, 4), "{why}");
    };

    let mut local = stuck_pager();
    local.process(DEAD_APP_MODES);
    local.process(PROMPT_AFTER_KILL);
    released(&local, "control: the local leave");

    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(DEAD_APP_MODES);
    term.process(PROMPT_AFTER_KILL);
    assert!(term.take_alt_screen_resync_request());
    assert!(term.modes().alternate_screen, "the host is asked instead");
    term.process(b"\x1b[?1049l");
    released(&term, "the host's forwarded switch");

    for host_resyncs in [true, false] {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(host_resyncs);
        term.process(b"\x1b[?1000h\x1b[?1h\x1b[?1049l");
        assert!(!term.modes().alternate_screen);
        assert_eq!(term.modes().mouse_mode, MouseMode::Normal, "{host_resyncs}");
        assert!(term.modes().application_cursor_keys, "{host_resyncs}");
    }

    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(PROMPT_AFTER_KILL);
    term.process(b"\x1b[?1049h\x1b[?1000hnext app");
    term.process(b"\x1b[?1049l");
    assert_eq!(term.modes().mouse_mode, MouseMode::Normal);
}

#[test]
fn the_prompt_that_proves_an_app_dead_turns_off_its_paste_and_reports() {
    const VIM_MODES: &[u8] = b"\x1b[?2004h\x1b[?2048h\x1b[?2031h";
    for host_resyncs in [false, true] {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(host_resyncs);
        term.process(VIM_MODES);
        let _ = term.take_response();
        term.process(PROMPT_AFTER_KILL);
        let m = term.modes();
        assert!(!m.bracketed_paste, "host_resyncs={host_resyncs}");
        assert!(!m.in_band_size_reports, "host_resyncs={host_resyncs}");
        assert!(!m.report_color_scheme, "host_resyncs={host_resyncs}");
        term.resize(5, 30);
        assert_eq!(
            term.take_response(),
            None,
            "no size report typed at the prompt (host_resyncs={host_resyncs})"
        );
    }

    let mut term = stuck_pager();
    term.process(b"\x1b[?1049l");
    term.process(VIM_MODES);
    term.process(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07");
    assert!(term.modes().bracketed_paste);
    assert!(term.modes().in_band_size_reports);
}

#[test]
fn the_prompt_that_proves_an_app_dead_releases_its_input_at_once() {
    use crate::terminal::{MouseEncoding, MouseMode};
    const DEAD_APP_MODES: &[u8] = b"\x1b[2;4r\x1b[?1002h\x1b[?1006h\x1b[?1h\x1b=\x1b[?2048h";
    for host_resyncs in [true, false] {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(host_resyncs);
        term.process(DEAD_APP_MODES);
        let _ = term.take_response();
        term.set_cell_pixel_size(9, 23);
        term.process(PROMPT_AFTER_KILL);
        let m = term.modes();
        assert_eq!(m.mouse_mode, MouseMode::None, "{host_resyncs}");
        assert_eq!(m.mouse_encoding, MouseEncoding::X10, "{host_resyncs}");
        assert!(!m.application_cursor_keys, "{host_resyncs}");
        assert!(!m.application_keypad, "{host_resyncs}");
        assert_eq!(
            term.take_reports(),
            None,
            "no report for the dead app reaches the shell (host_resyncs={host_resyncs})"
        );
    }

    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(DEAD_APP_MODES);
    term.process(PROMPT_AFTER_KILL);
    assert!(term.modes().alternate_screen, "the host is asked instead");
    let region = term.grid().scroll_region();
    assert_eq!((region.top, region.bottom), (1, 3), "the dead screen's own");
    term.process(b"\x1b[?1049l");
    let region = term.grid().scroll_region();
    assert_eq!(
        (region.top, region.bottom),
        (0, 4),
        "released with the screen"
    );
}

#[test]
fn a_shells_own_bracketed_paste_for_the_prompt_survives_the_dead_apps_release() {
    let mut term = stuck_pager();
    term.process(b"\x1b[?2004h\x1b[?2048h");
    term.process(b"\r\n\x1b]133;D;-1\x07\x1b[?2004h\x1b]133;A\x07> \x1b]133;B\x07");
    assert!(term.modes().bracketed_paste, "fish's own, set after its D");
    assert!(!term.modes().in_band_size_reports, "no shell asks for 2048");

    let mut term = stuck_pager();
    term.process(b"\x1b[?2004h");
    term.process(b"\r\n\x1b]133;D;-1\x07\x1b]133;A\x07");
    assert!(!term.modes().bracketed_paste, "vim's, released at the A");
    term.process(b"\x1b[?2004h$ \x1b]133;B\x07");
    assert!(term.modes().bracketed_paste, "readline's own, after the A");
}

#[test]
fn expecting_the_hosts_leave_marks_the_screen_dead() {
    use crate::terminal::MouseMode;
    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(b"\x1b[?1000h\x1b[?1h");
    assert!(term.expect_host_alt_screen_leave());
    term.process(PROMPT_AFTER_KILL);
    assert!(
        !term.take_alt_screen_resync_request(),
        "the verb's client is the one request for this screen"
    );
    term.process(b"\x1b[?1049l");
    assert!(!term.modes().alternate_screen);
    assert_eq!(term.modes().mouse_mode, MouseMode::None);
    assert!(!term.modes().application_cursor_keys);

    assert!(!term.expect_host_alt_screen_leave(), "the primary screen");
    term.process(b"\x1b[?1049h\x1b[?1000h\x1b[?1049l");
    assert_eq!(term.modes().mouse_mode, MouseMode::Normal);

    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(b"\x1b[?1000h");
    assert!(term.expect_host_alt_screen_leave());
    term.set_host_resyncs_alt_screen(false);
    term.process(b"\x1b[?1049l");
    assert_eq!(term.modes().mouse_mode, MouseMode::Normal);
}

#[test]
fn a_new_stuck_screen_asks_again() {
    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(PROMPT_AFTER_KILL);
    assert!(term.take_alt_screen_resync_request());
    term.process(b"\x1b[?1049l");
    term.process(b"\x1b[?1049hsecond pager");
    term.process(PROMPT_AFTER_KILL);
    assert!(term.take_alt_screen_resync_request(), "the new screen asks");
}

/// A program re-entering the alternate screen and marking a prompt on each one asks the host
/// once per visit: no screen it enters asks again until the alternate screen is left.
#[test]
fn a_leave_in_flight_holds_the_next_request() {
    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(PROMPT_AFTER_KILL);
    assert!(term.take_alt_screen_resync_request(), "one request");
    for _ in 0..100 {
        term.process(b"\x1b[?1049h\x1b]133;A\x07");
    }
    assert!(term.modes().alternate_screen);
    assert!(
        !term.take_alt_screen_resync_request(),
        "screens entered while the leave is in flight ask nothing"
    );
    term.process(b"\x1b[?1049l\x1b[?1049hpager again");
    term.process(PROMPT_AFTER_KILL);
    assert!(
        term.take_alt_screen_resync_request(),
        "asked again once left"
    );
}

/// Each `?1049l` ends a visit, the program's own included, so a program that leaves between
/// screens asks once per visit; the embedder, not the engine, bounds the host clients.
#[test]
fn a_program_that_leaves_between_screens_asks_once_per_visit() {
    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    for _ in 0..50 {
        term.process(b"\x1b[?1049l\x1b[?1049h\x1b]133;A\x07");
        assert!(term.take_alt_screen_resync_request(), "the visit asks");
        term.process(b"\x1b[?1049h\x1b]133;A\x07");
        assert!(
            !term.take_alt_screen_resync_request(),
            "the re-entry within it does not"
        );
    }
}

/// A request the embedder gives back (its leave client did not start) is asked again by the
/// next prompt on the same screen; one it keeps is not.
#[test]
fn an_abandoned_request_is_asked_again() {
    for abandon in [true, false] {
        let mut term = stuck_pager();
        term.set_host_resyncs_alt_screen(true);
        term.process(PROMPT_AFTER_KILL);
        assert!(term.take_alt_screen_resync_request(), "abandon={abandon}");
        if abandon {
            term.abandon_alt_screen_resync();
        }
        term.process(PROMPT_AFTER_KILL);
        assert!(term.modes().alternate_screen, "abandon={abandon}");
        assert_eq!(
            term.take_alt_screen_resync_request(),
            abandon,
            "the next prompt asks again only when the request was given back"
        );
    }
}

#[test]
fn ris_keeps_the_resync_policy() {
    let mut term = stuck_pager();
    term.set_host_resyncs_alt_screen(true);
    term.process(PROMPT_AFTER_KILL);
    term.process(b"\x1bc");
    assert!(
        !term.take_alt_screen_resync_request(),
        "RIS drops the request"
    );
    term.process(b"\x1b[?1049hpager again");
    term.process(PROMPT_AFTER_KILL);
    assert!(
        term.modes().alternate_screen,
        "still no local leave after RIS"
    );
    assert!(term.take_alt_screen_resync_request());
}
