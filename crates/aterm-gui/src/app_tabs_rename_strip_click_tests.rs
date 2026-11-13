// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

fn app_with_two_tabs() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.splice_tab_strip(wid);
    (app, wid)
}

fn chip_col(app: &App, wid: WindowId, index: usize) -> u16 {
    let seg = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|seg| seg.kind == tab_bar::TabHit::Select(index))
        .expect("the tab has a segment");
    seg.start_col + 1
}

/// A double click's interval, whose last instant still continues a run of presses.
fn interval() -> Duration {
    Duration::from_millis(
        u64::try_from(crate::multi_click_ms()).expect("a double click's interval"),
    )
}

/// Where a test's stated presses start: a minute ago, far enough from the clock that a press or
/// a stamp that read the clock instead of its stated time lands on the wrong side of a double
/// click's interval.
fn a_minute_ago() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("a minute of uptime")
}

/// A pointer click on strip column `col` whose button went down and up at `when`.
fn click_strip(app: &mut App, wid: WindowId, col: u16, when: Instant) {
    use aterm_window::event::{ElementState, MouseButton};
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + usize::from(col) * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + ch / 2) as f64;
    assert_eq!(
        app.strip_col_at(wid, x, y),
        Some(col),
        "the pixel is on column {col}"
    );
    app.on_cursor_moved(wid, x, y);
    for state in [ElementState::Pressed, ElementState::Released] {
        app.on_mouse_input_at(wid, state, MouseButton::Left, when);
    }
}

use crate::session_pin_for_test as pin;

#[test]
fn a_press_on_a_background_tabs_marks_selects_it() {
    let (mut app, wid) = app_with_two_tabs();
    let front = app.windows[&wid].tab_set.active_index();
    assert_eq!(front, Some(1), "the pushed tab is in front");
    app.windows
        .get_mut(&wid)
        .and_then(|ws| ws.tab_set.tab_at_mut(0))
        .expect("tab 0")
        .presentation
        .indicators
        .busy = true;
    app.splice_tab_strip(wid);
    let seg = app.windows[&wid]
        .tab_segments
        .iter()
        .copied()
        .find(|seg| seg.kind == tab_bar::TabHit::Select(0))
        .expect("tab 0's chip");
    let marks = seg.close_col.map_or(seg.end_col - 2, |close| close - 2);
    assert_eq!(
        tab_bar::hit_test(&app.windows[&wid].tab_segments, marks),
        Some(tab_bar::TabHit::Select(0))
    );

    app.handle_tab_strip_click(wid, marks, Instant::now());
    assert!(app.windows[&wid].tab_menu.is_none(), "no menu");
    assert_eq!(
        app.windows[&wid].tab_set.active_index(),
        Some(0),
        "selected"
    );
    assert_eq!(app.rename_edit_session(wid), None);
}

/// Each press states when it happened, so how long the tab switch between two presses
/// takes never moves the second across a double click's interval.
#[test]
fn a_second_click_on_the_same_chip_opens_the_inline_editor() {
    let (mut app, wid) = app_with_two_tabs();
    let col = chip_col(&app, wid, 0);
    let first = a_minute_ago();

    app.handle_tab_strip_click(wid, col, first);
    assert_eq!(app.windows[&wid].tabs.active, 0, "the first click switches");
    assert_eq!(app.rename_edit_session(wid), None, "and only switches");

    app.handle_tab_strip_click(wid, col, first + interval());
    assert_eq!(
        app.rename_edit_session(wid),
        Some(0),
        "the second click on the SAME chip, at the interval's last instant, edits its \
         focused session"
    );

    let (mut app, wid) = app_with_two_tabs();
    app.handle_tab_strip_click(wid, chip_col(&app, wid, 0), first);
    app.handle_tab_strip_click(wid, chip_col(&app, wid, 1), first + interval());
    assert_eq!(app.rename_edit_session(wid), None, "two chips, two clicks");
    assert_eq!(app.windows[&wid].tabs.active, 1);

    let (mut app, wid) = app_with_two_tabs();
    app.handle_tab_strip_click(wid, col, first);
    app.handle_tab_strip_click(wid, col, first + interval() + Duration::from_millis(1));
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "a press a millisecond after the interval"
    );

    for settings in [false, true] {
        for (second, edits) in [
            (first + interval(), Some(0)),
            (first + interval() + Duration::from_millis(1), None),
        ] {
            let (mut app, wid) = app_with_two_tabs();
            if settings {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
                app.splice_tab_strip(wid);
            }
            let col = chip_col(&app, wid, 0);
            click_strip(&mut app, wid, col, first);
            click_strip(&mut app, wid, col, second);
            assert_eq!(
                app.rename_edit_session(wid),
                edits,
                "the pointer's second press on the chip, {:?} after the first (the first \
                 from a Settings page in front: {settings})",
                second - first
            );
        }
    }
}

#[test]
fn selecting_another_chip_commits_while_the_edited_chip_keeps_editing() {
    let (mut app, wid) = app_with_two_tabs();
    let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
    assert!(app.begin_session_rename(wid, tab0));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));

    app.handle_tab_strip_click(wid, chip_col(&app, wid, 0), Instant::now());
    assert_eq!(app.rename_edit_session(wid), Some(0), "still editing");
    assert_eq!(pin(&app, 0), None, "nothing committed yet");

    app.handle_tab_strip_click(wid, chip_col(&app, wid, 1), Instant::now());
    assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
    assert_eq!(pin(&app, 0).as_deref(), Some("kept"));
    assert_eq!(app.windows[&wid].tabs.active, 1, "and the tab switched");
}

#[test]
fn the_new_tab_affordance_settles_the_edit_and_breaks_the_streak() {
    let (mut app, wid) = app_with_two_tabs();
    let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
    let plus = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|seg| seg.kind == tab_bar::TabHit::NewTab)
        .map(|seg| seg.start_col + 1)
        .expect("the strip has a + affordance");
    assert!(app.begin_session_rename(wid, tab0));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));

    app.handle_tab_strip_click(wid, plus, Instant::now());
    assert_eq!(app.rename_edit_session(wid), None);
    assert_eq!(pin(&app, 0).as_deref(), Some("kept"));
    assert!(app.windows[&wid].strip_press.is_none(), "streak broken");
}

#[test]
fn non_left_strip_and_native_page_presses_settle_rename_and_middle_click_closes_from_a_native_tab()
{
    use aterm_window::event::{ElementState, MouseButton};

    let (mut app, wid) = app_with_two_tabs();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    app.splice_tab_strip(wid);
    assert!(app.active_native_view(wid).is_some());
    assert_eq!(app.windows[&wid].tab_set.len(), 3);

    let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
    assert!(app.begin_session_rename(wid, tab0));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));

    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let col2 = chip_col(&app, wid, 2);
    let x = ox as f64 + (app.win_pad(wid) + usize::from(col2) * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + ch / 2) as f64;
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Middle);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Middle);
    assert_eq!(app.rename_edit_session(wid), None);
    assert_eq!(pin(&app, 0).as_deref(), Some("kept"));
    assert_eq!(
        app.windows[&wid].tab_set.len(),
        2,
        "middle-clicking the Settings chip while Settings is active closes it"
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    app.splice_tab_strip(wid);
    assert!(app.begin_session_rename(wid, tab0));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert(" body".into()));
    let (bx, by) = app.cell_centre_px(wid, 4, 4).expect("native body cell");
    app.on_cursor_moved(wid, bx, by);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "a right press on an active native page settles the inline rename"
    );
    assert_eq!(pin(&app, 0).as_deref(), Some("kept body"));
}

#[cfg(windows)]
#[test]
fn a_version_press_is_a_button_not_caption() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let (mut app, wid) = app_with_two_tabs();
    let label = app.windows[&wid]
        .tab_segments
        .iter()
        .find(|seg| matches!(seg.kind, tab_bar::TabHit::Version { .. }))
        .map(|seg| seg.start_col + 2)
        .expect("the strip lays the version label out once the UI face is in");
    let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
    assert!(app.begin_session_rename(wid, tab0));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));
    let (tabs, active) = (
        app.windows[&wid].tab_set.len(),
        app.windows[&wid].tabs.active,
    );

    for press in 0..2 {
        app.handle_tab_strip_click(wid, label, Instant::now());
        let ws = &app.windows[&wid];
        assert!(
            ws.strip_band_press.is_none(),
            "press {press}: not a caption press"
        );
        assert!(ws.strip_press.is_none(), "press {press}: not a chip press");
        assert!(ws.strip_drag.is_none(), "press {press}: grabs no tab");
        assert_eq!(
            ws.tab_set.len(),
            tabs,
            "press {press}: no tab opened or closed"
        );
        assert_eq!(ws.tabs.active, active, "press {press}: no tab switched");
    }
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "the open rename settled"
    );
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some("kept"),
        "…keeping what was typed"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

/// Each press states when it happened, so how long the work between presses takes never
/// moves one across a double click's interval.
#[test]
fn bare_band_presses_keep_their_own_double_click_streak() {
    let (mut app, wid) = app_with_two_tabs();
    let band = 999u16;
    assert!(tab_bar::hit_test(&app.windows[&wid].tab_segments, band).is_none());
    let first = a_minute_ago();

    app.handle_tab_strip_click(wid, band, first);
    assert!(
        app.windows[&wid].strip_band_press.is_some(),
        "the first band press arms"
    );
    app.handle_tab_strip_click(wid, band, first + interval());
    assert!(
        app.windows[&wid].strip_band_press.is_none(),
        "the repeat, at the interval's last instant, is consumed — a third press is a fresh \
         gesture"
    );

    app.handle_tab_strip_click(wid, band, first + interval() * 2);
    assert!(app.windows[&wid].strip_band_press.is_some());
    app.handle_tab_strip_click(wid, chip_col(&app, wid, 0), first + interval() * 2);
    assert!(
        app.windows[&wid].strip_band_press.is_none(),
        "a chip press disarms the band streak"
    );

    app.handle_tab_strip_click(wid, band, first + interval() * 3);
    assert!(app.windows[&wid].strip_band_press.is_some());
    app.handle_tab_strip_click(wid, band, first + interval() * 4 + Duration::from_millis(1));
    assert!(
        app.windows[&wid].strip_band_press.is_some(),
        "a press a millisecond after the interval re-arms instead of consuming"
    );
}

#[test]
fn a_vanished_tab_reaps_the_editor_instead_of_leaving_a_modal() {
    let (mut app, wid) = app_with_two_tabs();
    let tab1 = app.windows[&wid].tab_set.tabs()[1].id;
    assert!(app.begin_session_rename(wid, tab1));
    app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("ghost".into()));

    app.close_tab_at(wid, 1);
    let _ = app.refresh_window_tabs(wid);
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "no invisible field is left owning the keyboard"
    );
}
