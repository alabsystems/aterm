// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IME composition routing across the grid, find field, and inline rename field ([`crate::app_input::PreeditOwner`]).

use crate::{App, WindowId, term_lock};
use aterm_core::terminal::UnderlineStyle;

fn fill_scratch(app: &mut App, wid: WindowId) {
    let (rows, cols) = {
        let ws = app.windows.get(&wid).unwrap();
        (ws.rows as usize, ws.cols as usize)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let ws = app.windows.get_mut(&wid).unwrap();
    let mut term = term_lock(&terminal);
    term.cell_frame_into(&mut ws.input_scratch, rows, cols);
}

#[test]
fn find_field_owns_the_preedit() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"$ ");
    app.search_enter();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .set_query("ab".to_string());
    app.on_ime_preedit(wid, "中".to_string(), Some((0, "中".len())));
    assert_eq!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Find,
        "an open find bar claims the composition"
    );
    fill_scratch(&mut app, wid);
    app.splice_find_bar(wid);
    let ws = app.windows.get(&wid).unwrap();
    let hit = ws.find_bar_hit.as_ref().expect("panel geometry");
    let row = &ws.input_scratch.cells[hit.row];
    let text: String = row[hit.field_cols.clone()].iter().map(|c| c.ch).collect();
    assert!(
        text.starts_with("ab中"),
        "the well displays the composition inline: {text:?}"
    );
    let run_col = hit.field_cols.start + 2 - hit.field_scroll;
    assert_eq!(
        row[run_col].underline,
        UnderlineStyle::Single,
        "the composed char is underlined in the well"
    );
    assert_eq!(
        row[hit.field_cols.start].underline,
        UnderlineStyle::None,
        "settled query text is not underlined"
    );
    assert!(
        ws.last_ime_rect.is_some(),
        "the candidate window anchored on the well's caret"
    );
    assert_eq!(ws.search.as_ref().unwrap().query, "ab");
}

#[test]
fn native_view_composition_stays_out_of_the_find_well() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"$ ");
    app.search_enter();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .set_query("ab".to_string());
    fill_scratch(&mut app, wid);
    let retained_find = app.windows.get_mut(&wid).unwrap().search.take();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    app.windows.get_mut(&wid).unwrap().search = retained_find;
    app.on_ime_preedit(wid, "中".to_string(), Some((0, "中".len())));
    assert_eq!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Native,
        "the frontmost native view claims the composition"
    );
    app.splice_find_bar(wid);
    let ws = app.windows.get(&wid).unwrap();
    let hit = ws.find_bar_hit.as_ref().expect("panel geometry");
    let row = &ws.input_scratch.cells[hit.row];
    let text: String = row[hit.field_cols.clone()].iter().map(|c| c.ch).collect();
    assert!(
        !text.contains('中'),
        "a native view's composition must not splice into the find well: {text:?}"
    );
    assert!(
        text.starts_with("ab"),
        "the settled query still paints: {text:?}"
    );
}

#[test]
fn rename_field_outranks_find_for_the_preedit() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.begin_active_session_rename(wid);
    assert!(app.inline_rename_edit(wid).is_some(), "rename field open");
    app.on_ime_preedit(wid, "に".to_string(), Some((0, 0)));
    assert_eq!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Rename
    );
    fill_scratch(&mut app, wid);
    app.splice_tab_strip(wid);
    let (cw, ch) = app.win_cell_size(wid);
    let (band_x, band_y) = app.frame_origin(wid);
    let strip_row_y = i64::try_from(app.win_pad_top(wid) + app.win_head(wid))
        .unwrap()
        .saturating_add(band_y);
    let ws = app.windows.get(&wid).unwrap();
    let caret_col = ws
        .cached_strip_rename_caret
        .expect("the rename well painted a caret cell");
    let rect = ws
        .last_ime_rect
        .expect("the strip splice anchored the candidate window");
    assert_eq!(
        i64::from(rect.1),
        strip_row_y,
        "the anchor sits on the strip row (frame row 0), not on grid row 0 at y+{ch}"
    );
    assert_eq!(
        i64::from(rect.0),
        i64::try_from(caret_col * cw + app.win_pad(wid))
            .unwrap()
            .saturating_add(band_x),
        "the anchor's column is the well's reverse-video caret cell"
    );
}

#[test]
fn empty_preedit_owns_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.on_ime_preedit(wid, String::new(), None);
    assert_eq!(app.preedit_owner(wid), crate::app_input::PreeditOwner::None);
    assert_eq!(
        crate::preedit_fingerprint(
            &app.windows.get(&wid).unwrap().preedit,
            app.windows.get(&wid).unwrap().preedit_caret,
        ),
        0,
        "no composition ⇒ zero repaint term (idle invariant)"
    );
}

#[test]
fn the_caret_range_is_reduced_to_its_start() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.on_ime_preedit(wid, "日本語".to_string(), Some(("日".len(), "日本".len())));
    assert_eq!(
        app.windows.get(&wid).unwrap().preedit_caret,
        Some("日".len()),
        "the range START is retained; the end is winit's selection extent"
    );
}
