// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use super::{App, ComposedCursorFxClock, WindowId};
use crate::{reset_visible_leaf_plan_builds, term_lock, visible_leaf_plan_builds};

const PHISH: &[u8] =
    b"Visit \x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ for details";
const LINK_COL: u16 = 6;
const TARGET: &str = "https://evil.example/steal";

fn frame_rows(app: &App, wid: WindowId) -> Vec<String> {
    app.windows[&wid]
        .input_scratch
        .cells
        .iter()
        .map(|row| row.iter().map(|cell| cell.ch).collect())
        .collect()
}

fn hover(app: &mut App, wid: WindowId, row: u16, col: u16) {
    let (cw, ch) = app.win_cell_size(wid);
    let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
    let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
    let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64
        + strip_px
        + ch as f64 * (f64::from(row) + 0.5);
    app.on_cursor_moved(wid, x, y);
}

fn compose(app: &mut App, wid: WindowId) -> Vec<String> {
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(Instant::now()),
        )
        .is_some(),
        "the headless capture must produce a frame to splice into"
    );
    app.splice_tab_strip(wid);
    app.splice_find_bar(wid);
    app.splice_link_target(wid);
    frame_rows(app, wid)
}

fn extract_only(app: &mut App, wid: WindowId) {
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(Instant::now()),
        )
        .is_some(),
        "the headless capture must produce a frame"
    );
}

fn feed(app: &App, wid: WindowId, bytes: &[u8]) {
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(bytes);
}

fn panel_rows(app: &App, wid: WindowId) -> Vec<String> {
    let hit = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("an open panel records its geometry");
    let strip = app.tab_strip_rows as usize;
    let rows = frame_rows(app, wid);
    hit.band
        .clone()
        .map(|row| rows[strip + row].clone())
        .collect()
}

fn find(app: &mut App, wid: WindowId, query: &str) {
    app.search_enter();
    app.windows
        .get_mut(&wid)
        .expect("window")
        .search
        .as_mut()
        .expect("find mode is open")
        .set_query(query.to_string());
    app.search_recompute();
}

#[test]
fn hovered_link_reuses_the_frame_plan() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, PHISH);
    extract_only(&mut app, wid);
    hover(&mut app, wid, 0, LINK_COL);

    reset_visible_leaf_plan_builds();
    let (plan, route) = app.active_visible_frame_layout(wid).expect("frame layout");
    assert_eq!(
        route,
        crate::VisibleContentRoute::Terminal { composed: false }
    );
    app.splice_link_target_from_plan(wid, &plan);
    assert_eq!(
        visible_leaf_plan_builds(),
        1,
        "hover caret projection borrows the frame plan",
    );

    reset_visible_leaf_plan_builds();
    assert!(app.active_visible_frame_layout(wid).is_some());
    app.splice_link_target(wid);
    assert_eq!(
        visible_leaf_plan_builds(),
        2,
        "an unthreaded hover consumer adds a second plan build",
    );
}

#[test]
fn a_hovered_hyperlink_puts_its_destination_in_the_composed_frame() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(PHISH);
    assert_eq!(
        term_lock(&term).hyperlink_at(0, LINK_COL),
        Some(TARGET),
        "precondition: the engine attached the link `ctl cell` reports"
    );

    hover(&mut app, wid, 0, LINK_COL);
    assert_eq!(
        app.windows[&wid].link_hover.map(|hover| hover.cell),
        Some((0, LINK_COL)),
        "the hover resolution publishes the cell to disclose, not just a cursor"
    );
    let rows = compose(&mut app, wid);
    let caption = rows.last().expect("a composed frame has rows").clone();
    assert!(caption.contains(TARGET), "{caption:?}");
    assert!(
        rows[0].contains("Visit google.com for details"),
        "the program's own line is untouched: {:?}",
        rows[0]
    );

    hover(&mut app, wid, 0, 0);
    assert!(
        app.windows[&wid].link_hover.is_none(),
        "ordinary text discloses nothing"
    );
    let rows = compose(&mut app, wid);
    assert!(
        !rows.last().expect("rows").contains("evil.example"),
        "the caption is gone with the hover: {:?}",
        rows.last()
    );
}

#[test]
fn a_look_alike_domain_reaches_the_caption_as_visible_escapes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(
        "\u{1b}]8;;https://g\u{43e}\u{43e}gle.com/login\u{1b}\\sign in\u{1b}]8;;\u{1b}\\"
            .as_bytes(),
    );
    assert_eq!(
        term_lock(&term).hyperlink_at(0, 0),
        Some("https://g\u{43e}\u{43e}gle.com/login"),
        "precondition: the engine admits a look-alike host"
    );

    hover(&mut app, wid, 0, 0);
    let rows = compose(&mut app, wid);
    let caption = rows.last().expect("rows").clone();
    assert!(caption.contains("g%D0%BE%D0%BEgle.com"), "{caption:?}");
    assert!(
        !caption.contains('\u{43e}'),
        "no look-alike codepoint survives into the caption: {caption:?}"
    );
}

#[test]
fn a_link_on_the_bottom_row_moves_the_caption_off_the_text_it_discloses() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let bottom = app.windows[&wid].rows - 1;
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(format!("\x1b[{};1H", bottom + 1).as_bytes());
    term_lock(&term).process(PHISH);
    assert_eq!(
        term_lock(&term).hyperlink_at(bottom, LINK_COL),
        Some(TARGET)
    );

    hover(&mut app, wid, bottom, LINK_COL);
    let rows = compose(&mut app, wid);
    assert!(rows[0].contains(TARGET), "{:?}", rows[0]);
    assert!(
        rows[usize::from(bottom)].contains("google.com"),
        "the disclosed link stays readable: {:?}",
        rows[usize::from(bottom)]
    );
}

#[test]
fn a_top_placed_find_panel_keeps_its_rows_while_the_caption_takes_the_bottom() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, b"\x1b[5;1H");
    feed(&app, wid, PHISH);
    feed(&app, wid, b"\x1b[7;1Hneedle");
    find(&mut app, wid, "needle");

    hover(&mut app, wid, 4, LINK_COL);
    let rows = compose(&mut app, wid);
    let band = panel_rows(&app, wid);
    assert_eq!(
        band.len(),
        crate::find_bar::FIND_BAR_ROWS,
        "the panel owns its whole band"
    );
    assert!(band.concat().contains("Find: "), "{band:?}");
    assert!(
        !band.concat().contains("evil.example"),
        "the caption never lands inside the panel: {band:?}"
    );
    assert!(
        rows.last().expect("rows").contains(TARGET),
        "the caption took the free bottom row: {:?}",
        rows.last()
    );
}

#[test]
fn a_bottom_floated_find_panel_pushes_the_caption_to_the_other_end() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows_n = app.windows[&wid].rows as usize;
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"needle\r\n\r\n");
    term_lock(&term).process(b"\x1b[3;1H");
    term_lock(&term).process(PHISH);
    find(&mut app, wid, "needle");

    hover(&mut app, wid, 2, LINK_COL);
    let rows = compose(&mut app, wid);
    let band = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("panel geometry")
        .band
        .clone();
    assert_eq!(
        band,
        rows_n - crate::find_bar::FIND_BAR_ROWS..rows_n,
        "precondition: the panel floated to the bottom"
    );
    let panel = panel_rows(&app, wid);
    assert!(panel.concat().contains("Find: "), "{panel:?}");
    assert!(
        !panel.concat().contains("evil.example"),
        "the caption never lands inside the panel: {panel:?}"
    );
    assert!(rows[0].contains(TARGET), "{:?}", rows[0]);
}

#[test]
fn a_link_with_nowhere_to_put_the_caption_says_nothing_at_all() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let bottom = app.windows[&wid].rows - 1;
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"\r\n\r\n\r\n\r\nneedle\r\n");
    term_lock(&term).process(format!("\x1b[{};1H", bottom + 1).as_bytes());
    term_lock(&term).process(PHISH);
    find(&mut app, wid, "needle");

    hover(&mut app, wid, bottom, LINK_COL);
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("evil.example"),
        "no row of the frame carries the caption: {rows:?}"
    );
    let panel = panel_rows(&app, wid);
    assert!(
        panel.concat().contains("Find: "),
        "the panel is whole: {panel:?}"
    );
    assert!(
        rows[usize::from(bottom)].contains("google.com"),
        "the link is still readable: {:?}",
        rows[usize::from(bottom)]
    );
}

#[test]
fn a_link_hidden_under_the_find_panel_is_not_disclosed_from_on_top_of_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"\r\n");
    term_lock(&term).process(PHISH);
    term_lock(&term).process(b"\r\n\r\n\r\nneedle\r\n");
    find(&mut app, wid, "needle");
    compose(&mut app, wid);
    let band = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("panel geometry")
        .band
        .clone();
    assert!(band.contains(&1), "precondition: the panel covers row 1");

    hover(&mut app, wid, 1, LINK_COL);
    assert!(
        app.windows[&wid].link_hover.is_none(),
        "the pointer is on the panel, not on the row underneath it"
    );
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("evil.example"),
        "nothing hidden under chrome is disclosed: {rows:?}"
    );
    assert_eq!(
        app.link_under_pointer(wid),
        None,
        "a press lands on the panel, never on the grid cell it covers"
    );
}

#[test]
fn a_link_on_the_first_row_is_disclosed_under_a_tab_strip_in_either_frame_shape() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    feed(&app, wid, PHISH);

    hover(&mut app, wid, 0, LINK_COL);
    let rows = compose(&mut app, wid);
    assert_eq!(
        rows.len(),
        usize::from(app.windows[&wid].rows) + 1,
        "precondition: the strip really is prepended"
    );
    assert!(
        rows.last().expect("rows").contains(TARGET),
        "the top row of OUTPUT is not the strip's row: {:?}",
        rows.last()
    );

    extract_only(&mut app, wid);
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "the pointer is on the grid whichever shape the scratch is in"
    );
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "and the caption is composed either way"
    );
}

#[test]
fn opening_find_under_a_stationary_pointer_silences_the_caption() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, b"\r\n");
    feed(&app, wid, PHISH);
    feed(&app, wid, b"\r\n\r\n\r\nneedle\r\n");

    hover(&mut app, wid, 1, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "precondition: with no panel up the caption is disclosed"
    );

    find(&mut app, wid, "needle");
    let rows = compose(&mut app, wid);
    let band = app.windows[&wid]
        .find_bar_hit
        .as_ref()
        .expect("panel geometry")
        .band
        .clone();
    assert!(band.contains(&1), "precondition: the panel covers the link");
    assert!(
        !rows.concat().contains("evil.example"),
        "the link is under the panel and the press cannot reach it: {rows:?}"
    );
}

#[test]
fn switching_tabs_under_a_stationary_pointer_takes_the_caption_away() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, PHISH);
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "precondition: the caption is up"
    );

    let next = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(next));
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("evil.example"),
        "a brand-new tab contains no link, and the frame claims none: {rows:?}"
    );
}

#[test]
fn a_hover_resolved_over_another_session_is_never_painted() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, PHISH);
    let front = app.front_terminal(wid).expect("front terminal").session;
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.link_hover = Some(crate::link_target::LinkHover {
            cell: (0, LINK_COL),
            window_row: 0,
            session: front + 1,
        });
    }
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("evil.example"),
        "a caption speaks only for the session its hover was resolved over: {rows:?}"
    );
}

#[test]
fn output_that_swaps_the_link_under_the_pointer_is_followed_by_the_caption() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, PHISH);
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "precondition: the first destination is disclosed"
    );

    feed(
        &app,
        wid,
        b"\x1b[H\x1b[2KVisit \x1b]8;;https://second.example/other\x1b\\google.com\x1b]8;;\x1b\\",
    );
    let rows = compose(&mut app, wid);
    let caption = rows.last().expect("rows").clone();
    assert!(
        caption.contains("second.example"),
        "the band names the URI the cell carries now: {caption:?}"
    );
    assert!(
        !caption.contains("evil.example"),
        "and not the one it carried before: {caption:?}"
    );

    feed(&app, wid, b"\x1b[H\x1b[2Kplain output, no link at all");
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("example"),
        "no link under the pointer, no claim about one: {rows:?}"
    );
}

#[test]
fn the_caption_reads_its_cached_destination_until_the_frame_moves() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    feed(&app, wid, PHISH);
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "precondition: the destination is disclosed"
    );
    let acq = crate::term_lock_acquisitions_on_this_thread;
    let fresh = |app: &App| {
        let ws = &app.windows[&wid];
        let session = app.front_terminal(wid).expect("terminal").session;
        ws.hover_memo
            .as_ref()
            .is_some_and(|m| m.stands_for(ws, session))
    };
    let redraw = |app: &mut App| {
        let frame_before = acq();
        extract_only(app, wid);
        app.splice_tab_strip(wid);
        let splice_before = acq();
        app.splice_link_target(wid);
        let after = acq();
        (after - frame_before, after - splice_before)
    };

    let (frame_cost, _) = redraw(&mut app);
    assert!(
        frame_cost >= 1,
        "precondition: the frame's own extraction locks"
    );
    for pass in 0..2 {
        let (frame, splice) = redraw(&mut app);
        assert_eq!(
            (frame, splice),
            (frame_cost, 0),
            "redraw {pass} with an unchanged engine fill re-paints the caption lock-free"
        );
        assert!(
            frame_rows(&app, wid).last().expect("rows").contains(TARGET),
            "…and it still names the destination"
        );
    }

    feed(&app, wid, b"\x1b[3;1Hmore output on another row");
    assert_eq!(
        redraw(&mut app),
        (frame_cost, 0),
        "a changed engine fill re-reads under the frame's own hold: no acquisition of its own"
    );
    assert!(fresh(&app), "…and the memo stands on the new fill");
    assert_eq!(redraw(&mut app), (frame_cost, 0), "and is cached again");
    let rows = frame_rows(&app, wid);
    assert!(
        rows.last().expect("rows").contains(TARGET),
        "the re-read names what the cell still carries: {rows:?}"
    );
}

fn captioned(app: &App, wid: WindowId) -> bool {
    frame_rows(app, wid)
        .last()
        .is_some_and(|row| row.contains(TARGET))
}

fn present_compose(app: &mut App, wid: WindowId, now: Instant) -> bool {
    let presented = app
        .redraw_compose(wid, 24, 80, false, false, None, 0, now)
        .is_some();
    if presented {
        app.splice_tab_strip(wid);
        app.splice_link_target(wid);
    }
    presented
}

fn settle_compose(app: &mut App, wid: WindowId, now: &mut Instant) {
    for _ in 0..40 {
        *now += std::time::Duration::from_millis(16);
        if !present_compose(app, wid, *now) {
            return;
        }
    }
    panic!("the fixture must reach a settled (skipping) state");
}

const TWO_LINKS: &[u8] = b"Visit \x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ \
    or \x1b]8;;https://other.example/\x1b\\bing.com\x1b]8;;\x1b\\";
const OTHER: &str = "https://other.example/";
const OTHER_COL: u16 = 20;

#[test]
fn a_probe_deferred_by_a_busy_mutex_is_answered_by_the_next_frame_under_its_own_hold() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut now = Instant::now();
    feed(&app, wid, TWO_LINKS);
    hover(&mut app, wid, 0, LINK_COL);
    now += std::time::Duration::from_millis(16);
    assert!(
        present_compose(&mut app, wid, now),
        "precondition: the first frame presents"
    );
    assert!(
        captioned(&app, wid),
        "precondition: the first link is disclosed"
    );
    settle_compose(&mut app, wid, &mut now);

    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let guard = term.lock().unwrap_or_else(|p| p.into_inner());
        held_tx.send(()).expect("test thread listening");
        release_rx.recv().expect("release");
        drop(guard);
    });
    held_rx.recv().expect("the holder took the mutex");
    let acq = crate::term_lock_acquisitions_on_this_thread;
    let before = acq();
    hover(&mut app, wid, 0, OTHER_COL);
    assert_eq!(
        acq() - before,
        0,
        "the motion path never waits on the reader"
    );
    assert_eq!(
        app.windows[&wid].link_hover.map(|h| h.cell),
        Some((0, LINK_COL)),
        "the probe deferred: the parked caption (the first link) stands"
    );
    release_tx.send(()).expect("holder waiting");
    holder.join().expect("holder thread");

    now += std::time::Duration::from_millis(16);
    assert!(
        present_compose(&mut app, wid, now),
        "the frame that answers the deferred probe presents"
    );
    assert_eq!(
        app.windows[&wid].link_hover.map(|h| h.cell),
        Some((0, OTHER_COL)),
        "the hover names the cell the pointer rests on"
    );
    let rows = frame_rows(&app, wid);
    assert!(
        rows.last().is_some_and(|row| row.contains(OTHER)),
        "the caption names the second link's destination: {rows:?}"
    );
    now += std::time::Duration::from_millis(16);
    assert!(
        !present_compose(&mut app, wid, now),
        "no retry frame is needed once the answer has presented"
    );
}

#[test]
fn a_hover_settling_on_a_link_presents_with_nothing_else_moving() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut now = Instant::now();
    feed(&app, wid, PHISH);
    hover(&mut app, wid, 3, LINK_COL);
    settle_compose(&mut app, wid, &mut now);
    assert!(!captioned(&app, wid), "precondition: nothing captioned");

    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "the motion path resolved the link"
    );
    now += std::time::Duration::from_millis(16);
    assert!(
        present_compose(&mut app, wid, now),
        "the frame that discloses the destination presents"
    );
    assert!(
        captioned(&app, wid),
        "…naming it: {:?}",
        frame_rows(&app, wid)
    );
    settle_compose(&mut app, wid, &mut now);

    hover(&mut app, wid, 3, LINK_COL);
    now += std::time::Duration::from_millis(16);
    assert!(
        present_compose(&mut app, wid, now),
        "the frame that retires the caption presents"
    );
    assert!(!captioned(&app, wid), "…and the band is gone");
}

#[test]
fn a_link_scrolled_under_a_still_pointer_is_disclosed_by_the_frame() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let rows = usize::from(app.windows[&wid].rows);
    term_lock(&term).process(PHISH);
    term_lock(&term).process(b"\r\n");
    for _ in 0..rows {
        term_lock(&term).process(b"filler\r\n");
    }
    let scroll = i32::try_from(rows).expect("a small viewport");
    term_lock(&term).scroll_display(scroll);
    let phish_row = u16::try_from(
        (0..rows)
            .find(|row| {
                term_lock(&term)
                    .row_text(*row)
                    .unwrap_or_default()
                    .contains("Visit google.com for details")
            })
            .expect("the link line is on screen when scrolled back"),
    )
    .expect("a small viewport");
    term_lock(&term).scroll_display(-scroll);
    assert_eq!(term_lock(&term).grid().display_offset(), 0);

    hover(&mut app, wid, phish_row, LINK_COL);
    assert!(
        !compose(&mut app, wid).concat().contains("example"),
        "precondition: the live row under the pointer carries no link"
    );
    assert!(app.windows[&wid].link_hover.is_none());

    term_lock(&term).scroll_display(scroll);
    let out = compose(&mut app, wid);
    assert!(
        out.last().expect("rows").contains(TARGET),
        "the frame that scrolled the link under the pointer discloses it: {out:?}"
    );
    assert_eq!(
        app.windows[&wid].link_hover.map(|h| h.cell),
        Some((phish_row, LINK_COL)),
        "the hover names the cell the frame put the link on"
    );

    let px = app.windows[&wid].last_cursor_px;
    let acq = crate::term_lock_acquisitions_on_this_thread;
    let before = acq();
    app.on_cursor_moved(wid, px.0, px.1);
    assert_eq!(
        acq() - before,
        0,
        "a stationary pointer event takes no terminal lock"
    );
    assert_eq!(
        app.windows[&wid].link_hover.map(|h| h.cell),
        Some((phish_row, LINK_COL)),
        "and hands the parked caption back untouched"
    );
}

#[cfg(unix)]
#[test]
fn a_stationary_pointer_event_under_mouse_tracking_takes_no_lock_and_reports_nothing() {
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let drain = || {
        let mut bytes = [0u8; 256];
        let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
        if read <= 0 {
            Vec::new()
        } else {
            bytes[..read as usize].to_vec()
        }
    };
    let mut app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1])));
    let wid = WindowId(0);
    feed(&app, wid, PHISH);
    let acq = crate::term_lock_acquisitions_on_this_thread;
    for mode in [
        &b"\x1b[?1000h\x1b[?1006h"[..],
        &b"\x1b[?1002h"[..],
        &b"\x1b[?1003h"[..],
    ] {
        feed(&app, wid, mode);
        hover(&mut app, wid, 0, LINK_COL);
        let _ = drain();
        assert_eq!(
            app.windows[&wid].link_hover.map(|h| h.cell),
            Some((0, LINK_COL)),
            "precondition ({mode:?}): the link is disclosed under tracking"
        );

        let px = app.windows[&wid].last_cursor_px;
        let before = acq();
        app.on_cursor_moved(wid, px.0, px.1);
        assert_eq!(
            acq() - before,
            0,
            "({mode:?}) a stationary pointer event takes no terminal lock with tracking on"
        );
        assert!(
            drain().is_empty(),
            "({mode:?}) …and reports no motion for a pointer that did not move"
        );
        assert_eq!(
            app.windows[&wid].link_hover.map(|h| h.cell),
            Some((0, LINK_COL)),
            "({mode:?}) and hands the parked caption back untouched"
        );

        app.on_mouse_wheel(
            wid,
            aterm_window::event::MouseScrollDelta::LineDelta(0.0, -1.0),
        );
        let reported = drain();
        assert!(
            reported.starts_with(b"\x1b[<64;") || reported.starts_with(b"\x1b[<65;"),
            "({mode:?}) the wheel notch is reported to the tracking app: {reported:?}"
        );
    }
    unsafe {
        libc::close(pipe[0]);
    }
}

#[cfg(unix)]
#[test]
fn a_keyboard_focus_move_under_a_still_pointer_re_derives_the_cell_before_the_wheel_report() {
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let drain = || {
        let mut bytes = [0u8; 256];
        let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
        if read <= 0 {
            Vec::new()
        } else {
            bytes[..read as usize].to_vec()
        }
    };
    let mut app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new(pipe[1])));
    let wid = WindowId(0);
    feed(&app, wid, b"\x1b[?1000h\x1b[?1006h");
    let a = app.split_active_stub_tab(wid);
    assert_eq!(
        app.front_terminal(wid).map(|t| t.session),
        Some(a),
        "precondition: the split leaves the new (right) pane focused"
    );
    let (a_row_off, a_col_off, _, _) = app.focused_pane_rect(wid);
    const ROW: u16 = 3;
    const COL: u16 = 5;
    assert!(
        COL < a_col_off,
        "precondition: window column {COL} lies in pane B, left of A's origin {a_col_off}"
    );

    hover(&mut app, wid, ROW, COL);
    let _ = drain();
    assert_eq!(app.windows[&wid].last_mouse_window_cell, (ROW, COL));
    assert_eq!(
        app.windows[&wid].last_mouse_cell,
        (ROW - a_row_off, 0),
        "precondition: over the unfocused pane the parked cell is A's clamp"
    );
    assert!(
        app.windows[&wid].hover_grid_owned,
        "precondition: the grid arm ran"
    );

    assert!(app.focus_pane_in(wid, crate::pane::FocusDir::Left));
    assert_eq!(
        app.front_terminal(wid).map(|t| t.session),
        Some(0),
        "precondition: B (session 0) now holds the keyboard"
    );
    assert_eq!(
        app.focused_pane_rect(wid).1,
        0,
        "precondition: B's origin column is 0"
    );

    let (b_row_off, b_col_off, _, _) = app.focused_pane_rect(wid);
    let b_cell = (ROW - b_row_off, COL - b_col_off);

    let px = app.windows[&wid].last_cursor_px;
    app.on_cursor_moved(wid, px.0, px.1);
    assert_eq!(
        app.windows[&wid].last_mouse_cell, b_cell,
        "the first stationary event after a keyboard focus move re-derives the pane-local cell"
    );
    assert!(
        drain().is_empty(),
        "…and still reports no motion under 1000 for a pointer that did not move"
    );
    app.on_mouse_wheel(
        wid,
        aterm_window::event::MouseScrollDelta::LineDelta(0.0, -1.0),
    );
    let reported = drain();
    let expected_tail = format!(";{};{}M", b_cell.1 + 1, b_cell.0 + 1);
    assert!(
        (reported.starts_with(b"\x1b[<64") || reported.starts_with(b"\x1b[<65"))
            && reported.ends_with(expected_tail.as_bytes()),
        "the wheel report names the pointer's window cell minus B's origin: {reported:?}"
    );

    let acq = crate::term_lock_acquisitions_on_this_thread;
    let before = acq();
    app.on_cursor_moved(wid, px.0, px.1);
    assert_eq!(
        acq() - before,
        0,
        "a stationary pointer event under an unchanged mapping takes no terminal lock"
    );
    assert!(drain().is_empty());
    assert_eq!(app.windows[&wid].last_mouse_cell, b_cell);
    app.on_mouse_wheel(
        wid,
        aterm_window::event::MouseScrollDelta::LineDelta(0.0, -1.0),
    );
    let reported = drain();
    assert!(
        reported.ends_with(expected_tail.as_bytes()),
        "the notch after the skipped event still names B's cell: {reported:?}"
    );
    unsafe {
        libc::close(pipe[0]);
    }
}

fn strip_px(app: &App, wid: WindowId, col: u16) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    (
        app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5),
        (app.win_pad_top(wid) + app.win_head(wid)) as f64 + ch as f64 * 0.5,
    )
}

#[test]
fn a_tab_menu_open_over_a_link_row_earns_no_caption_and_no_ibeam() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.splice_tab_strip(wid);
    feed(&app, wid, PHISH);
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "precondition: the destination is disclosed"
    );

    let (sx, sy) = strip_px(&app, wid, 2);
    app.on_cursor_moved(wid, sx, sy);
    assert!(
        app.open_tab_context_menu_at_chip(wid, 0),
        "the menu opens at the chip"
    );
    hover(&mut app, wid, 0, LINK_COL);
    assert!(app.windows[&wid].tab_menu.is_some(), "the card is up");
    for frame in 0..2 {
        compose(&mut app, wid);
        app.splice_tab_menu(wid);
        let rows = frame_rows(&app, wid);
        assert!(
            !rows.concat().contains("example"),
            "frame {frame}: no caption for a link under the card: {rows:?}"
        );
        assert!(
            app.windows[&wid].link_hover.is_none(),
            "frame {frame}: no hover published under the card"
        );
        assert!(
            !app.windows[&wid].native_text_cursor,
            "frame {frame}: no I-beam over the menu"
        );
    }

    assert!(app.close_tab_menu(wid));
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "the caption is back once the card is gone"
    );
    assert!(app.open_tab_context_menu_at_chip(wid, 0));
    compose(&mut app, wid);
    app.splice_tab_menu(wid);
    let rows = frame_rows(&app, wid);
    assert!(
        !rows.concat().contains("example"),
        "a keyboard-opened card silences the caption under it: {rows:?}"
    );
    assert!(app.close_tab_menu(wid));
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "…and it returns when the card closes"
    );
}

#[test]
fn the_caption_never_takes_the_row_the_caret_is_on() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let bottom = app.windows[&wid].rows - 1;
    feed(&app, wid, b"\x1b[3;1H");
    feed(&app, wid, PHISH);
    feed(
        &app,
        wid,
        format!(
            "\x1b[{};1H$ echo my important half-typed command",
            bottom + 1
        )
        .as_bytes(),
    );

    let rows = compose(&mut app, wid);
    let caret_row = app.windows[&wid].input_scratch.cursor_row;
    assert_eq!(caret_row, usize::from(bottom), "the caret is on the prompt");
    assert!(
        app.windows[&wid].input_scratch.cursor_visible,
        "control: the caret is visible before any hover"
    );
    assert!(
        rows[caret_row].contains("half-typed command"),
        "control: the command line is on screen: {:?}",
        rows[caret_row]
    );

    hover(&mut app, wid, 2, LINK_COL);
    let rows = compose(&mut app, wid);
    assert!(
        rows[caret_row].contains("half-typed command"),
        "a hover must not delete the line being typed: {:?}",
        rows[caret_row]
    );
    assert!(
        app.windows[&wid].input_scratch.cursor_visible,
        "nor the caret the person is typing at"
    );
    assert!(
        rows[0].contains(TARGET),
        "the caption took the free end instead: {:?}",
        rows[0]
    );
}

#[test]
fn a_pointer_over_a_sibling_pane_discloses_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    let term = app.pool.get(right).expect("the focused pane").term.clone();
    term_lock(&term)
        .process(b"\x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ for details");
    let (ro, co, _, _) = app.focused_pane_rect(wid);
    assert!(co > 0, "precondition: the focused pane starts mid-window");
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.mods = if cfg!(target_os = "macos") {
            aterm_window::keyboard::ModifiersState::SUPER
        } else {
            aterm_window::keyboard::ModifiersState::CONTROL
        };
    }

    hover(&mut app, wid, ro, co);
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "precondition: the focused pane's link is resolvable"
    );
    assert!(
        app.windows[&wid].hover_pointer,
        "precondition: and it offers the click"
    );

    hover(&mut app, wid, ro, 0);
    assert!(
        app.windows[&wid].link_hover.is_none(),
        "the pointer is over another pane's ordinary text"
    );
    assert!(
        !app.windows[&wid].hover_pointer,
        "and the cursor promises no click there either"
    );
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("evil.example"),
        "no caption over a pane the pointer is not in: {rows:?}"
    );
}

#[test]
fn a_scrolled_back_link_is_disclosed_by_the_line_the_pointer_is_on() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let rows = usize::from(app.windows[&wid].rows);
    term_lock(&term).process(PHISH);
    term_lock(&term).process(b"\r\n");
    for _ in 0..rows {
        term_lock(&term).process(b"filler\r\n");
    }
    term_lock(&term).scroll_display(i32::try_from(rows).expect("a small viewport"));
    assert!(
        term_lock(&term).grid().display_offset() > 0,
        "precondition: the viewport really is scrolled"
    );
    let phish_row = u16::try_from(
        (0..rows)
            .find(|row| {
                term_lock(&term)
                    .row_text(*row)
                    .unwrap_or_default()
                    .contains("Visit google.com for details")
            })
            .expect("the phishing line is back on screen"),
    )
    .expect("a small viewport");
    assert_eq!(
        term_lock(&term).hyperlink_at(phish_row, LINK_COL),
        None,
        "precondition: the live row sharing that index carries no link"
    );
    hover(&mut app, wid, phish_row, LINK_COL);
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "a plainly underlined scrolled-back link is a link"
    );
    let rows_out = compose(&mut app, wid);
    let caption = rows_out.last().expect("a composed frame has rows").clone();
    assert!(
        caption.contains(TARGET),
        "the band names the line the pointer is on: {caption:?}"
    );

    term_lock(&term).process(
        format!(
            "\x1b[{};1HVisit \x1b]8;;https://second.example/other\x1b\\google.com\
             \x1b]8;;\x1b\\ elsewhere",
            phish_row + 1
        )
        .as_bytes(),
    );
    assert_eq!(
        term_lock(&term)
            .hyperlink_at(phish_row, LINK_COL)
            .map(str::to_owned),
        Some("https://second.example/other".to_string()),
        "precondition: the live row now carries a DIFFERENT link"
    );
    hover(&mut app, wid, phish_row, LINK_COL);
    let rows_out = compose(&mut app, wid);
    let caption = rows_out.last().expect("a composed frame has rows").clone();
    assert!(
        caption.contains(TARGET) && !caption.contains("second.example"),
        "the band names the line under the pointer, never the live row that \
         happens to share its index: {caption:?}"
    );
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some(TARGET),
        "the press opens exactly what the band named"
    );
}

#[test]
fn the_caption_never_takes_a_sibling_panes_command_line() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let left = app
        .active_tree(wid)
        .map(crate::pane::PaneTree::focus)
        .expect("the tab's original pane");
    let right = app.split_active_stub_tab(wid);
    assert_ne!(left, right, "two panes");
    let (_, _, left_rows, _) = {
        let plan = app
            .active_visible_leaf_plan(wid)
            .expect("a split has a plan");
        let leaf = plan.leaves.first().expect("the left leaf");
        (
            leaf.rect.origin.y.round() as u16,
            leaf.rect.origin.x.round() as u16,
            leaf.rect.size.height.round() as u16,
            leaf.rect.size.width.round() as u16,
        )
    };
    let left_term = app.pool.get(left).expect("the left pane").term.clone();
    for _ in 0..left_rows.saturating_sub(1) {
        term_lock(&left_term).process(b"\r\n");
    }
    term_lock(&left_term).process(b"$ echo sibling half-typed");
    let right_term = app.pool.get(right).expect("the focused pane").term.clone();
    term_lock(&right_term).process(b"\r\n\r\n");
    term_lock(&right_term).process(PHISH);

    let rows = compose(&mut app, wid);
    let bottom = rows.len() - 1;
    assert!(
        rows[bottom].contains("$ echo sibling half-typed"),
        "control: the sibling's command line is on the shared bottom row: {:?}",
        rows[bottom]
    );

    let (ro, co, _, _) = app.focused_pane_rect(wid);
    hover(&mut app, wid, ro + 2, co + LINK_COL);
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "precondition: the focused pane's link is resolvable"
    );
    let rows = compose(&mut app, wid);
    assert!(
        rows[bottom].contains("$ echo sibling half-typed"),
        "a hover must not delete the command line in the pane next door: {:?}",
        rows[bottom]
    );
    assert!(
        rows[0].contains(TARGET),
        "the caption took the free end instead: {:?}",
        rows[0]
    );
}
