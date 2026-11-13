// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, LocalRepeatAction, SearchRepeatAction, WindowId, term_lock};
use aterm_window::keyboard::{KeyCode, PhysicalKey};

#[test]
fn search_repeat_stays_on_press_window_after_focus_changes() {
    let mut app = App::headless_for_test();
    let press_window = WindowId(0);
    let terminal = app
        .front_terminal(press_window)
        .expect("press terminal")
        .term
        .clone();
    term_lock(&terminal).process(b"focus_routing_needle");
    app.search_enter();

    let session = app
        .front_terminal(press_window)
        .expect("press terminal")
        .session;
    let physical = PhysicalKey::Code(KeyCode::KeyQ);
    app.note_local_repeat_press(
        press_window,
        physical,
        LocalRepeatAction::Search {
            session,
            action: SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
                "focus_routing_needle".to_string(),
            )),
        },
    );

    let next_session = app.next_session_id;
    let arrival_window = app.insert_logical_window(crate::stub_session(next_session), 24, 80);
    assert_eq!(app.frontmost_window, Some(arrival_window));

    app.route_physical_repeat(arrival_window, physical);

    let search = app.windows[&press_window]
        .search
        .as_ref()
        .expect("press-window search remains open");
    assert_eq!(search.query, "focus_routing_needle");
    assert!(
        !search.matches.is_empty(),
        "repeat edit must recompute and navigate its press-time window"
    );
    assert!(app.windows[&arrival_window].search.is_none());
    assert!(app.take_local_repeat_release(arrival_window, physical));
}

#[test]
fn palette_repeat_stops_when_its_press_time_overlay_closes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.palette_enter();
    let physical = PhysicalKey::Code(KeyCode::KeyN);
    app.note_local_repeat_press(
        wid,
        physical,
        LocalRepeatAction::Palette(crate::app_palette::PaletteKey::Edit(
            crate::app_search::SearchEdit::Insert("n".to_string()),
        )),
    );
    app.route_physical_repeat(wid, physical);
    let before_close = app.windows[&wid]
        .palette()
        .expect("palette")
        .controls_lines();
    assert!(before_close[0].contains("query=\"n\""), "{before_close:?}");

    app.palette_exit();
    app.route_physical_repeat(wid, physical);
    assert!(app.windows[&wid].palette().is_none());
    assert!(app.take_local_repeat_release(wid, physical));
}
