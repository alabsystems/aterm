// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{ActionInvocation, AppEvent};
use crate::tab_model::{SplitAxis, ViewId, VisibleLeafPlan};

fn fixture() -> (App, WindowId, ViewId, ViewId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.cols = 140;
    window.rows = 40;
    window.scale = 2.0;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    let (_, native) = app.active_native_view(wid).unwrap();
    let (session, terminal) = app.split_active_with_stub_terminal(wid, SplitAxis::Horizontal);
    crate::term_lock(&app.pool.get(session).unwrap().term).process(b"\x1b[2J\x1b[H$ \x1b[?25l");
    (app, wid, native, terminal, session)
}

fn focus(app: &mut App, wid: WindowId, view: ViewId) {
    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(view)
    );
    app.sync_window(wid);
}

fn prepare(app: &mut App, wid: WindowId) -> VisibleLeafPlan {
    let plan = app.prepare_heterogeneous_layout_plan(wid).unwrap();
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan(wid, None, &plan)
            .is_some()
    );
    plan
}

fn present(app: &mut App, wid: WindowId, plan: &VisibleLeafPlan) {
    app.first_present_done = true;
    app.finalize_successful_present(
        wid,
        crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
        0,
        None,
        SuccessfulPresentRoute::Heterogeneous,
        crate::VisibleContentRoute::Heterogeneous,
        HostVisualState::default(),
        plan,
    );
}

#[test]
fn mixed_ime_anchor_follows_presented_native_terminal_native_focus() {
    let (mut app, wid, native, terminal, session) = fixture();
    focus(&mut app, wid, native);
    app.on_ime_preedit(wid, "日本語".to_string(), Some((3, 3)));
    let plan = prepare(&mut app, wid);
    present(&mut app, wid, &plan);
    let native_rect = app.windows[&wid]
        .last_ime_rect
        .expect("native field anchor");

    app.on_ime_preedit(wid, String::new(), None);
    focus(&mut app, wid, terminal);
    crate::term_lock(&app.pool.get(session).unwrap().term)
        .modes_mut()
        .ambiguous_width_double = true;
    app.on_ime_preedit(wid, "·日".to_string(), Some((2, 2)));
    let plan = prepare(&mut app, wid);
    let window = &app.windows[&wid];
    let pane = &window.leaf_render_cache[&terminal].input;
    assert_eq!(pane.cells[0][2].ch, '·');
    assert!(
        pane.cells[0][3].wide,
        "ambiguous glyph uses terminal width mode"
    );
    assert_eq!(pane.cells[0][4].ch, '日');
    assert_eq!(
        pane.cursor_col, 4,
        "platform middle caret, not marked-text end"
    );
    assert!(
        pane.cursor_visible,
        "composition overrides hidden engine cursor"
    );
    assert_eq!(
        window.last_ime_rect,
        Some(native_rect),
        "staging cannot publish an anchor"
    );
    let cell = (
        window.input_scratch.cursor_row,
        window.input_scratch.cursor_col,
    );
    let leaf = plan.leaf(terminal).unwrap();
    assert_eq!(
        cell.0,
        leaf.rect.origin.y.round() as usize + usize::from(app.chrome_rows(wid))
    );
    assert_eq!(cell.1, leaf.rect.origin.x.round() as usize + 4);
    let (cw, ch) = app.win_cell_size(wid);
    let (frame_x, frame_y) = app.frame_origin(wid);
    present(&mut app, wid, &plan);
    let terminal_rect = app.windows[&wid].last_ime_rect.unwrap();
    assert_eq!(
        terminal_rect,
        (
            (frame_x + (app.win_pad(wid) + cell.1 * cw) as i64) as i32,
            (frame_y + (app.win_pad_top(wid) + app.win_head(wid) + cell.0 * ch) as i64) as i32,
            cw as u32,
            ch as u32,
        )
    );
    assert_ne!(
        terminal_rect, native_rect,
        "old native anchor is a negative control"
    );
    assert_eq!(
        crate::term_lock(&app.pool.get(session).unwrap().term)
            .cursor()
            .col,
        2
    );

    app.on_ime_preedit(wid, String::new(), None);
    focus(&mut app, wid, native);
    app.on_ime_preedit(wid, "日本語".to_string(), Some((3, 3)));
    let plan = prepare(&mut app, wid);
    assert_eq!(app.windows[&wid].last_ime_rect, Some(terminal_rect));
    present(&mut app, wid, &plan);
    assert_eq!(app.windows[&wid].last_ime_rect, Some(native_rect));
    let pane = &app.windows[&wid].leaf_render_cache[&terminal].input;
    assert!(!pane.cursor_visible);
    assert!(
        pane.cells[0]
            .iter()
            .all(|cell| cell.ch != '·' && cell.ch != '日')
    );
}

#[test]
fn mixed_ime_paint_respects_width_cancel_history_and_find_ownership() {
    let (mut app, wid, _, terminal, session) = fixture();
    let term = app.pool.get(session).unwrap().term.clone();
    for (ambiguous, caret_col) in [(false, 3), (true, 4)] {
        crate::term_lock(&term).modes_mut().ambiguous_width_double = ambiguous;
        app.on_ime_preedit(wid, "·日".to_string(), Some((2, 2)));
        prepare(&mut app, wid);
        let pane = &app.windows[&wid].leaf_render_cache[&terminal].input;
        assert_eq!(pane.cursor_col, caret_col);
        assert_eq!(pane.cells[0][caret_col].ch, '日');
    }
    app.on_ime_preedit(wid, String::new(), None);
    prepare(&mut app, wid);
    let pane = &app.windows[&wid].leaf_render_cache[&terminal].input;
    assert_eq!(pane.cursor_col, 2);
    assert!(!pane.cursor_visible);
    assert!(
        pane.cells[0]
            .iter()
            .all(|cell| cell.ch != '·' && cell.ch != '日')
    );

    {
        let mut term = crate::term_lock(&term);
        for _ in 0..100 {
            term.process(b"history\r\n");
        }
        term.scroll_display(1);
        assert!(term.grid().display_offset() > 0);
    }
    app.on_ime_preedit(wid, "候補".to_string(), Some((3, 3)));
    prepare(&mut app, wid);
    let pane = &app.windows[&wid].leaf_render_cache[&terminal].input;
    let expected = crate::term_lock(&term).cell_frame(pane.rows, pane.cols);
    assert_eq!(
        pane.cells, expected.cells,
        "composition cannot overwrite history"
    );

    crate::term_lock(&term).scroll_display(-100);
    app.on_ime_preedit(wid, String::new(), None);
    app.search_enter_direction_in(wid, true);
    app.on_ime_preedit(wid, "候補".to_string(), Some((3, 3)));
    assert!(matches!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Find
    ));
    let plan = prepare(&mut app, wid);
    let pane = &app.windows[&wid].leaf_render_cache[&terminal].input;
    let expected = crate::term_lock(&term).cell_frame(pane.rows, pane.cols);
    assert_eq!(
        pane.cells, expected.cells,
        "find field owns its composition"
    );
    let field_rect = (7, 11, 13, 17);
    app.report_ime_cursor_rect(wid, field_rect);
    present(&mut app, wid, &plan);
    assert_eq!(
        app.windows[&wid].last_ime_rect,
        Some(field_rect),
        "terminal report cannot replace the find anchor"
    );
}
