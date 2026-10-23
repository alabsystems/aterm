// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IME composition on the split present path.

use crate::{App, WindowId, pane::PaneRect, term_lock};
use aterm_core::terminal::UnderlineStyle;
use std::time::{Duration, Instant};

fn split_app() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.split_active_stub_tab(wid);
    app.windows.get_mut(&wid).expect("window").focused = true;
    for s in [0u64, sid] {
        let term = app.pool.get(s).expect("pane session").term.clone();
        term_lock(&term).process(b"abcdefgh\r");
    }
    (app, wid)
}

fn compose(app: &mut App, wid: WindowId, now: Instant) -> bool {
    app.redraw_compose(wid, 24, 80, false, false, None, 0, now)
        .is_some()
}

fn focused_and_other(app: &App, wid: WindowId) -> (PaneRect, PaneRect) {
    let ws = app.windows.get(&wid).expect("window");
    let tree = &ws.layouts[ws.tabs.active];
    let focus = tree.focus();
    let rects = tree.compute_layout(ws.rows, ws.cols);
    assert_eq!(rects.len(), 2, "the fixture is a two-leaf split: {rects:?}");
    let focused = *rects
        .iter()
        .find(|r| r.session == focus)
        .expect("the focused leaf is laid out");
    let other = *rects
        .iter()
        .find(|r| r.session != focus)
        .expect("the background leaf is laid out");
    (focused, other)
}

fn pane_text(app: &App, wid: WindowId, r: &PaneRect) -> String {
    let cells = &app.windows.get(&wid).expect("window").input_scratch.cells;
    let mut out = String::new();
    let row_end = (usize::from(r.row_off) + usize::from(r.rows)).min(cells.len());
    for cols in cells.iter().take(row_end).skip(usize::from(r.row_off)) {
        let col_end = (usize::from(r.col_off) + usize::from(r.cols)).min(cols.len());
        for cell in &cols[usize::from(r.col_off).min(col_end)..col_end] {
            if !cell.wide {
                out.push(cell.ch);
            }
        }
    }
    out
}

fn diag(app: &App, wid: WindowId) -> String {
    let ws = app.windows.get(&wid).expect("window");
    let tree = &ws.layouts[ws.tabs.active];
    let head = |ri: &aterm_core::render::RenderInput| {
        let row: String = ri
            .cells
            .first()
            .map(|r| r.iter().take(8).map(|c| c.ch).collect())
            .unwrap_or_default();
        format!(
            "{}x{} off={} cur=({},{}) vis={} head={row:?}",
            ri.rows, ri.cols, ri.display_offset, ri.cursor_row, ri.cursor_col, ri.cursor_visible
        )
    };
    format!(
        "owner={:?} preedit={:?} caret={:?} shown={} focus={} rects={:?} \
         pool_has_focus={} win={}x{} | focus_scratch[{}] | pane_scratch[{}] | input[{}]",
        app.preedit_owner(wid),
        ws.preedit,
        ws.preedit_caret,
        ws.preedit_shown,
        tree.focus(),
        tree.compute_layout(ws.rows, ws.cols),
        app.pool.get(tree.focus()).is_some(),
        ws.rows,
        ws.cols,
        head(&ws.composed_focus_scratch),
        head(&ws.pane_scratch),
        head(&ws.input_scratch),
    )
}

#[test]
fn split_present_paints_the_composition_in_the_focused_leaf_only() {
    let (mut app, wid) = split_app();
    let mut now = Instant::now();
    assert!(
        compose(&mut app, wid, now),
        "the establishing split frame presents"
    );
    let caret = "日本".len();
    app.on_ime_preedit(wid, "日本".to_string(), Some((caret, caret)));
    assert_eq!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Grid,
        "no field is open, so the GRID owns the composition"
    );
    now += Duration::from_millis(16);
    assert!(
        compose(&mut app, wid, now),
        "the composition moves the RepaintKey's preedit term, so the frame presents"
    );
    let (focused, other) = focused_and_other(&app, wid);
    let hot = pane_text(&app, wid, &focused);
    let cold = pane_text(&app, wid, &other);
    assert!(
        hot.starts_with("日本efgh"),
        "the focused leaf carries the composition, spliced OVER the row it \
         covers and no further: {hot:?}\n{}",
        diag(&app, wid)
    );
    assert!(
        !cold.contains('日') && !cold.contains('本'),
        "the background leaf — identically materialized — is untouched: {cold:?}"
    );
    assert!(
        cold.starts_with("abcdefgh"),
        "…and still shows its own row: {cold:?}"
    );
    let ws = app.windows.get(&wid).expect("window");
    let (crow, ccol) = (usize::from(focused.row_off), usize::from(focused.col_off));
    assert_eq!(
        ws.input_scratch.cells[crow][ccol].underline,
        UnderlineStyle::Single,
        "the composed run is underlined"
    );
    assert!(
        ws.input_scratch.cursor_visible,
        "the focused leaf still draws the solid cursor"
    );
    assert_eq!(
        (ws.input_scratch.cursor_row, ws.input_scratch.cursor_col),
        (crow, ccol + 4),
        "the window-coords cursor follows the caret PAST the two wide \
         composed cells (4 columns), not the pre-composition cell"
    );
    assert!(
        ws.preedit_shown,
        "the split path records that this frame drew a composition"
    );
}

#[test]
fn a_split_composition_that_ends_leaves_no_residue() {
    let (mut app, wid) = split_app();
    let mut now = Instant::now();
    assert!(compose(&mut app, wid, now), "establishing frame");
    app.on_ime_preedit(wid, "日本".to_string(), Some((0, 0)));
    now += Duration::from_millis(16);
    assert!(compose(&mut app, wid, now), "the composition presents");
    let (focused, _) = focused_and_other(&app, wid);
    assert!(
        pane_text(&app, wid, &focused).contains("日本"),
        "drawn\n{}",
        diag(&app, wid)
    );
    app.on_ime_preedit(wid, String::new(), None);
    now += Duration::from_millis(16);
    assert!(
        compose(&mut app, wid, now),
        "the frame that ERASES the composition must not take the early-out"
    );
    let after = pane_text(&app, wid, &focused);
    assert!(
        !after.contains('日') && !after.contains('本'),
        "the covered cells repainted from the pane's own snapshot: {after:?}"
    );
    assert!(
        after.starts_with("abcdefgh"),
        "…back to exactly the engine's row: {after:?}"
    );
    assert!(
        !app.windows[&wid].preedit_shown,
        "and the shared bookkeeping flag follows the erase"
    );
}

#[test]
fn a_field_owned_composition_never_paints_in_a_split_leaf() {
    let (mut app, wid) = split_app();
    let mut now = Instant::now();
    assert!(compose(&mut app, wid, now), "establishing frame");
    app.search_enter();
    app.on_ime_preedit(wid, "日本".to_string(), Some((0, 0)));
    assert_eq!(
        app.preedit_owner(wid),
        crate::app_input::PreeditOwner::Find,
        "an open find bar claims the composition"
    );
    now += Duration::from_millis(16);
    compose(&mut app, wid, now);
    let (focused, other) = focused_and_other(&app, wid);
    for (name, r) in [("focused", focused), ("background", other)] {
        let text = pane_text(&app, wid, &r);
        assert!(
            !text.contains('日') && !text.contains('本'),
            "the {name} leaf stays clean while the find well owns it: {text:?}"
        );
    }
    assert!(
        !app.windows[&wid].preedit_shown,
        "no grid composition was drawn"
    );
}
