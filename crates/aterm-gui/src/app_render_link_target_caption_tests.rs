// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use super::{App, ComposedCursorFxClock, WindowId};
use crate::{reset_visible_leaf_plan_builds, term_lock, visible_leaf_plan_builds};

/// OSC 8 phishing shape: the visible run reads `google.com`, the URI
/// addresses `evil.example`.
const PHISH: &[u8] =
    b"Visit \x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ for details";
/// Column of the `g` in `google.com` — the first linked cell.
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

/// Move the pointer to the centre of window cell `(row, col)` through the
/// real motion path (so a split clamps the cell as `CursorMoved` does); the
/// inverse of [`super::pixel_to_term_cell`].
fn hover(app: &mut App, wid: WindowId, row: u16, col: u16) {
    let (cw, ch) = app.win_cell_size(wid);
    let strip_px = f64::from(app.tab_strip_rows) * ch as f64;
    let x = app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5);
    let y = (app.win_pad_top(wid) + app.win_head(wid)) as f64
        + strip_px
        + ch as f64 * (f64::from(row) + 0.5);
    app.on_cursor_moved(wid, x, y);
}

/// One frame, spliced in production order: strip, find panel, then the
/// caption. The strip and find splices are no-ops when neither is open.
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

/// Re-extract the grid into `input_scratch` and stop, leaving the terminal
/// band alone with no chrome rows prepended (a shape pointer events can
/// observe, since motion is asynchronous to the frame loop).
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

/// Feed bytes to the window's front terminal without touching the pointer —
/// the "a program wrote something while the hand was still" gesture.
fn feed(app: &App, wid: WindowId, bytes: &[u8]) {
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(bytes);
}

/// The rows the find panel owns, as text — read through the geometry the
/// panel itself recorded, so these tests never hard-code its placement.
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

/// Open find on `query` through the real entry point and run the real match
/// over live content, the way the panel's own tests do.
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

    // Negative control: the unthreaded splice rebuilds the plan itself.
    reset_visible_leaf_plan_builds();
    assert!(app.active_visible_frame_layout(wid).is_some());
    app.splice_link_target(wid);
    assert_eq!(
        visible_leaf_plan_builds(),
        2,
        "an unthreaded hover consumer adds a second plan build",
    );
}

/// Hovering the `google.com` run puts `https://evil.example/steal` into the
/// composed frame without touching the program's line; moving off removes it.
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

    // Off the link — column 0 is the `V` of `Visit`, which carries none.
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

/// A look-alike domain (Cyrillic `о` in `gооgle.com`) is a valid URI, so the
/// caption spells non-ASCII characters out as visible escapes.
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

/// The caption must never cover the very text whose destination it is
/// disclosing: a link on the bottom row moves it to the top.
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

/// The caption is spliced after the find panel, so it never takes a panel
/// row: with the panel at the top it takes the bottom row.
#[test]
fn a_top_placed_find_panel_keeps_its_rows_while_the_caption_takes_the_bottom() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Both the link (row 4) and the match (row 6) sit clear of the band a
    // top panel owns, so the panel stays put and the pointer is on the grid.
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

/// A panel floated to the bottom owns the caption's preferred row, so the
/// caption goes to the other end.
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
    // The only match is on the top row, which floats the panel to the bottom.
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

/// With the panel on the top rows and the link on the bottom one, the
/// caption has no row to take and says nothing.
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

/// The find panel covers the rows it owns; a press there hits the panel, so
/// a link beneath it is not disclosed.
#[test]
fn a_link_hidden_under_the_find_panel_is_not_disclosed_from_on_top_of_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    // The link is on terminal row 1 — inside the top band the panel owns —
    // and the match is far below it, so the panel stays at the top.
    term_lock(&term).process(b"\r\n");
    term_lock(&term).process(PHISH);
    term_lock(&term).process(b"\r\n\r\n\r\nneedle\r\n");
    find(&mut app, wid, "needle");
    // One frame, so the panel has recorded the band it owns.
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
    // The pointer shape agrees with the caption: no hand over a hidden link.
    assert_eq!(
        app.link_under_pointer(wid),
        None,
        "a press lands on the panel, never on the grid cell it covers"
    );
}

/// The row register is keyed on terminal rows, so a first-row link is
/// disclosed under a tab strip whether `input_scratch` holds the composed
/// frame or the bare grid.
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

    // The same pointer, arriving while the frame is mid-compose.
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

/// Find can open under a still pointer; the panel declares the rows it took
/// and the caption reads that register on the same frame.
#[test]
fn opening_find_under_a_stationary_pointer_silences_the_caption() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // The link is on row 1 — inside the band a top panel owns — and the
    // match is far below, so the panel does not float away from it.
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

    // One keystroke, no pointer motion at all.
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

/// The destination is read from the front terminal, and only while it is
/// the one the hover was resolved over.
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

/// A hover carrying a session that is not the front one is never painted.
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

/// The caption is re-read from the live grid each frame, so it follows a
/// program rewriting the cell under a motionless pointer.
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

    // The pointer does not move for the rest of this test.
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

    // And when the substitution leaves no link there, the band says nothing
    // rather than naming a destination no press could reach.
    feed(&app, wid, b"\x1b[H\x1b[2Kplain output, no link at all");
    let rows = compose(&mut app, wid);
    assert!(
        !rows.concat().contains("example"),
        "no link under the pointer, no claim about one: {rows:?}"
    );
}

/// The destination is resolved once per (hover cell, engine fill): an unchanged
/// fill repaints the caption with no terminal lock; a changed fill re-reads
/// under the fill's own hold (`App::refresh_hover_for_frame_locked`), so the
/// frame's acquisition count is the same either way. The strip splice bumps
/// `snapshot_seq` without a new fill, so the memo is not keyed on it.
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
    // A redraw: refill, strip, caption — the order every production route
    // uses. Returns (the frame's whole acquisition count, the splice's own).
    let redraw = |app: &mut App| {
        let frame_before = acq();
        extract_only(app, wid);
        app.splice_tab_strip(wid);
        let splice_before = acq();
        app.splice_link_target(wid);
        let after = acq();
        (after - frame_before, after - splice_before)
    };

    // Two redraws with no output: the engine fill is the same, the caption
    // is re-painted from the memo, and the hover takes no lock at all.
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

    // A new engine fill under the still pointer: the re-read rides that
    // fill's hold, so the frame costs the same and the splice acquires nothing.
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

/// Whether the composed present of window `wid` currently names `TARGET`
/// on its caption row.
fn captioned(app: &App, wid: WindowId) -> bool {
    frame_rows(app, wid)
        .last()
        .is_some_and(|row| row.contains(TARGET))
}

/// One frame through the production composed route: `redraw_compose`, then
/// (on a presenting frame) the strip splice and the caption, in the caller's
/// order. Returns whether the frame presented; `input_scratch` is only the
/// glass after a presenting frame.
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

/// Drive the composed route until it takes the RepaintKey early-out (the
/// fixture's effects are idle, so one skipped frame is settled).
fn settle_compose(app: &mut App, wid: WindowId, now: &mut Instant) {
    for _ in 0..40 {
        *now += std::time::Duration::from_millis(16);
        if !present_compose(app, wid, *now) {
            return;
        }
    }
    panic!("the fixture must reach a settled (skipping) state");
}

/// Two OSC 8 links on one row: `google.com` → `TARGET` at `LINK_COL`, and
/// `bing.com` → [`OTHER`] at [`OTHER_COL`].
const TWO_LINKS: &[u8] = b"Visit \x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ \
    or \x1b]8;;https://other.example/\x1b\\bing.com\x1b]8;;\x1b\\";
const OTHER: &str = "https://other.example/";
/// Column of the `b` in `bing.com`: `Visit ` (6) + `google.com` (10) + ` or ` (4).
const OTHER_COL: u16 = 20;

/// A hover probe deferred by a held mutex is answered by the next frame under
/// its own hold. That needs the fill to re-probe on the memo's cell, not only
/// its fill epoch (`HoverMemo::stands_for`), and the caption to be a
/// `RepaintKey` term so the frame does not take the early-out.
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
    // Read the caption right after a presenting frame, never after the settle.
    settle_compose(&mut app, wid, &mut now);

    // The reader holds the mutex while the pointer crosses to the second link.
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

    // The next frame, with no output and no other change: it presents,
    // and it names the link the pointer is actually on.
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
    // Nothing further is owed: the frame after it takes the early-out.
    now += std::time::Duration::from_millis(16);
    assert!(
        !present_compose(&mut app, wid, now),
        "no retry frame is needed once the answer has presented"
    );
}

/// The caption is a RepaintKey term: settling onto a link and leaving it
/// both present, though neither dirties a cell.
#[test]
fn a_hover_settling_on_a_link_presents_with_nothing_else_moving() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut now = Instant::now();
    feed(&app, wid, PHISH);
    // The pointer rests off the link (row 3 is empty); the window settles.
    hover(&mut app, wid, 3, LINK_COL);
    settle_compose(&mut app, wid, &mut now);
    assert!(!captioned(&app, wid), "precondition: nothing captioned");

    // Onto the link: the settle's frame must present with no new output.
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

    // And off again: the band's removal is a present too.
    hover(&mut app, wid, 3, LINK_COL);
    now += std::time::Duration::from_millis(16);
    assert!(
        present_compose(&mut app, wid, now),
        "the frame that retires the caption presents"
    );
    assert!(!captioned(&app, wid), "…and the band is gone");
}

/// The frame under a still pointer re-resolves the hover: a wheel scroll
/// that moves a link under it is disclosed with no `CursorMoved`, and the
/// same-pixel `CursorMoved` macOS synthesizes before each wheel tick takes
/// no terminal lock and returns the parked caption.
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
    // The link line, then enough filler to push it into history.
    term_lock(&term).process(PHISH);
    term_lock(&term).process(b"\r\n");
    for _ in 0..rows {
        term_lock(&term).process(b"filler\r\n");
    }
    // Where the link line will land once the viewport scrolls back `rows`.
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

    // The pointer rests on that cell while it still shows filler: no link.
    hover(&mut app, wid, phish_row, LINK_COL);
    assert!(
        !compose(&mut app, wid).concat().contains("example"),
        "precondition: the live row under the pointer carries no link"
    );
    assert!(app.windows[&wid].link_hover.is_none());

    // The viewport scrolls (a wheel glide's tick) — the pointer does not move.
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

    // The synthesized same-pixel CursorMoved before a wheel tick: no lock.
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

/// Tracking-on twin: with a VT mouse mode live, the same-pixel `CursorMoved`
/// returns at the top of the routing (no lock, no bytes to the PTY), and the
/// following wheel tick is still reported.
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
        // A real move onto the link routes fully (and may report under
        // 1003); whatever it wrote is not the stationary event's.
        hover(&mut app, wid, 0, LINK_COL);
        let _ = drain();
        assert_eq!(
            app.windows[&wid].link_hover.map(|h| h.cell),
            Some((0, LINK_COL)),
            "precondition ({mode:?}): the link is disclosed under tracking"
        );

        // The synthesized same-pixel CursorMoved before a wheel tick.
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

        // The wheel tick itself is still the app's to hear.
        app.on_mouse_wheel(wid, winit::event::MouseScrollDelta::LineDelta(0.0, -1.0));
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

/// Keyboard-owner-change twin: the same-pixel early return is keyed on the
/// mapping (`HoverMapping`: front session, focused rect, geometry), not the
/// pixel alone. After focus moves to another pane by keyboard, the first
/// stationary event routes fully and the SGR wheel report carries the cell
/// relative to the new pane; the second takes no locks.
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
    // Session 0 (the observed sink) turns tracking on as the lone pane; the
    // split then puts the new pane A on the right with the keyboard.
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

    // The pointer rests over B while A is focused: the pane-local cell is
    // clamped into A's rect (column 0 of A), not B's cell.
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

    // Keyboard: focus B. No pointer event.
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

    // The mapping changed under the still pointer, so this event routes
    // fully and re-derives the cell for the new owner.
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
    app.on_mouse_wheel(wid, winit::event::MouseScrollDelta::LineDelta(0.0, -1.0));
    let reported = drain();
    let expected_tail = format!(";{};{}M", b_cell.1 + 1, b_cell.0 + 1);
    assert!(
        (reported.starts_with(b"\x1b[<64") || reported.starts_with(b"\x1b[<65"))
            && reported.ends_with(expected_tail.as_bytes()),
        "the wheel report names the pointer's window cell minus B's origin: {reported:?}"
    );

    // Mapping unchanged again: no locks, no bytes, and the next notch
    // still carries B's cell.
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
    app.on_mouse_wheel(wid, winit::event::MouseScrollDelta::LineDelta(0.0, -1.0));
    let reported = drain();
    assert!(
        reported.ends_with(expected_tail.as_bytes()),
        "the notch after the skipped event still names B's cell: {reported:?}"
    );
    unsafe {
        libc::close(pipe[0]);
    }
}

/// The window pixel at the centre of strip column `col` (row 0 of a
/// one-row strip).
fn strip_px(app: &App, wid: WindowId, col: u16) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    (
        app.win_pad(wid) as f64 + cw as f64 * (f64::from(col) + 0.5),
        (app.win_pad_top(wid) + app.win_head(wid)) as f64 + ch as f64 * 0.5,
    )
}

/// The frame-time refresh continues the last routing decision: during a
/// connector drag it must not swap the grab cursor for an I-beam or caption
/// a link under the dragging hand.
#[test]
fn a_connector_drag_crossing_a_link_keeps_the_grab_and_earns_no_caption() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.splice_tab_strip(wid);
    feed(&app, wid, PHISH);
    // The grid arm has resolved once: a memo exists and the I-beam is up.
    hover(&mut app, wid, 0, LINK_COL);
    assert!(
        compose(&mut app, wid)
            .last()
            .expect("rows")
            .contains(TARGET),
        "precondition: the destination is disclosed"
    );
    assert!(
        app.windows[&wid].native_text_cursor,
        "precondition: the grid's I-beam"
    );
    // Press the connector on the strip and drag down across the link cell
    // through the real motion path.
    let (sx, sy) = strip_px(&app, wid, 2);
    app.on_cursor_moved(wid, sx, sy);
    assert!(
        !app.windows[&wid].native_text_cursor && !app.windows[&wid].hover_pointer,
        "the strip resolves the plain arrow"
    );
    assert!(app.conn_drag_arm(wid, 0), "tab 0 has a session to drag");
    let (ox, oy) = app.conn_drag.as_ref().expect("armed").origin;
    app.on_cursor_moved(wid, ox + crate::conn_drag::CONN_DRAG_THRESHOLD_PX * 2.0, oy);
    assert!(
        app.conn_drag.as_ref().expect("armed").dragging,
        "past the threshold: the grab cursor was set"
    );
    hover(&mut app, wid, 0, LINK_COL);
    assert!(app.conn_drag.is_some(), "the drag swallowed the motion");
    // Frames during the drag (the wire repaint): the grab stands, no
    // caption is spliced, no hover is published.
    for frame in 0..2 {
        let rows = compose(&mut app, wid);
        assert!(
            !rows.concat().contains("example"),
            "frame {frame}: no caption mid-drag: {rows:?}"
        );
        assert!(
            app.windows[&wid].link_hover.is_none(),
            "frame {frame}: no hover published under a drag"
        );
        assert!(
            !app.windows[&wid].native_text_cursor && !app.windows[&wid].hover_pointer,
            "frame {frame}: the frame did not replace the grab with an I-beam"
        );
    }
    app.conn_drag_abort();
}

/// The tab context menu owns the pointer while up; a frame must not paint an
/// I-beam or caption for a link under the card. Covers a pointer that walked
/// onto the card and a keyboard opening under a pointer parked on the grid.
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

    // Pointer opening: the pointer walks from the chip down onto the card.
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

    // Keyboard opening under a pointer parked on the grid: the caption
    // splice's owner gate keeps the card's rows clean, and the hover
    // returns the frame the card closes.
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

/// The band replaces the cells of its row, so it never takes the caret's
/// row (usually the prompt on the bottom row); it goes to the other end.
#[test]
fn the_caption_never_takes_the_row_the_caret_is_on() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let bottom = app.windows[&wid].rows - 1;
    // A link mid-screen, and a live command line with the caret in it.
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

    // Control: with no hover the caret is on the bottom row with its text.
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

/// In a split, a pointer over the sibling pane clamps into the focused pane,
/// where a press would focus the sibling: no caption and no hand cursor.
#[test]
fn a_pointer_over_a_sibling_pane_discloses_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Split 1|2; focus lands on the new right-hand pane.
    let right = app.split_active_stub_tab(wid);
    let term = app.pool.get(right).expect("the focused pane").term.clone();
    // The link starts at the focused pane's column 0 — the cell the clamp
    // lands on for every pointer to the left of that pane.
    term_lock(&term)
        .process(b"\x1b]8;;https://evil.example/steal\x1b\\google.com\x1b]8;;\x1b\\ for details");
    let (ro, co, _, _) = app.focused_pane_rect(wid);
    assert!(co > 0, "precondition: the focused pane starts mid-window");
    // The open modifier is down, so the pointer hand is live too.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.mods = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
    }

    // Over the focused pane's own link: disclosed, hand up.
    hover(&mut app, wid, ro, co);
    assert!(
        app.windows[&wid].link_hover.is_some(),
        "precondition: the focused pane's link is resolvable"
    );
    assert!(
        app.windows[&wid].hover_pointer,
        "precondition: and it offers the click"
    );

    // The sibling's first column, clear of the divider; the clamp resolves
    // the focused pane's column 0, where the link is.
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

/// Scrolled back, the probe follows `display_offset`, so the band names the
/// line under the pointer rather than the live row at that index.
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
    // The phishing line first, then enough output to push it into history.
    term_lock(&term).process(PHISH);
    term_lock(&term).process(b"\r\n");
    for _ in 0..rows {
        term_lock(&term).process(b"filler\r\n");
    }
    // Scroll until the phishing line is back on screen, and find where it
    // landed: the arithmetic is the terminal's, not this test's.
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
    // The live row at that index is plain output: a screen-keyed probe
    // would find no link.
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

    // A different link on the live row at the same index (placed by cursor
    // address, so the viewport stays in history): a screen-keyed probe would
    // name the wrong site.
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
    // The click follows the same coordinates, so `ctrl+click opens` is not a
    // promise about a different link.
    assert_eq!(
        app.link_under_pointer(wid).as_deref(),
        Some(TARGET),
        "the press opens exactly what the band named"
    );
}

/// The band takes a window row shared by every pane of a split, so the
/// caret check covers every visible pane, not just the focused one.
#[test]
fn the_caption_never_takes_a_sibling_panes_command_line() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let left = app
        .active_tree(wid)
        .map(crate::pane::PaneTree::focus)
        .expect("the tab's original pane");
    // Split 1|2; focus moves to the new right-hand pane.
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
    // The sibling is mid-command on its bottom row, which is the window's
    // bottom row, the row the band prefers.
    let left_term = app.pool.get(left).expect("the left pane").term.clone();
    for _ in 0..left_rows.saturating_sub(1) {
        term_lock(&left_term).process(b"\r\n");
    }
    term_lock(&left_term).process(b"$ echo sibling half-typed");
    // The focused pane carries the link, two rows down, with its own caret
    // beside it — nowhere near the bottom.
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
