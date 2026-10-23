// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that a tab menu card's recorded rect answers the pointer only on the grid it was laid on.

use crate::{App, WindowId, term_lock};

/// One frame of window `wid` as a redraw lays it.
fn frame(app: &mut App, wid: WindowId) {
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (ws.rows, ws.cols)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        let mut term = term_lock(&terminal);
        term.resize(rows, cols);
        term.cell_frame_into(&mut ws.input_scratch, usize::from(rows), usize::from(cols));
    }
    app.splice_tab_strip_with(wid, 1);
    app.splice_tab_menu(wid);
}

/// Window `wid`'s pixel at the centre of frame cell `(row, col)` on the grid the window has now.
fn cell_px(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64;
    assert_eq!(
        app.frame_cell_at(wid, x, y),
        Some((row, col)),
        "the pixel names the cell"
    );
    (x, y)
}

/// A font zoom to `size` px.
fn zoom(app: &mut App, size: f32) {
    assert!(app.set_font_size_with(size, |_| true), "zoomed to {size}");
}

/// The open card's lit row.
fn lit(app: &App, wid: WindowId) -> Option<usize> {
    app.windows[&wid]
        .tab_menu
        .as_ref()
        .and_then(|m| m.highlight)
}

/// A RE-GRID LEAVES THE TAB MENU'S LAST RECT ANSWERING NOTHING: the card
/// open from a press (no row lit) and painted, the pointer resting on one of
/// its actions. A font zoom re-grids the window — its cells grow — and puts
/// another action's cell of the last frame's rect under that pixel. Before
/// the next frame a motion there lights nothing and finds no action on the
/// card, so a press there could choose none. The re-grid closes nothing: the
/// next frame places the card again from the window's grid, and a motion
/// onto one of its rows there lights it. A re-grid again, and a press before
/// the frame closes the card without choosing, as a press before its first
/// paint does.
#[test]
fn a_regrid_leaves_the_tab_menus_last_rect_answering_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 3, false));
    frame(&mut app, wid);
    let rect = app.windows[&wid].tab_menu_rect.expect("painted");
    let laid = app.windows[&wid].tab_menu_laid;
    assert_eq!(laid, app.glass_grid(wid), "stamped with its grid");
    let entries = app.windows[&wid]
        .tab_menu
        .as_ref()
        .expect("open")
        .entries
        .clone();
    let action = |(row, col): (usize, usize)| crate::tab_menu::action_at(rect, &entries, row, col);
    let cells: Vec<(usize, (f64, f64))> = (rect.row..rect.row + rect.h)
        .flat_map(|row| (rect.col..rect.col + rect.w).map(move |col| (row, col)))
        .filter_map(|cell| Some((action(cell)?, cell_px(&app, wid, cell))))
        .collect();
    let base = app.font_size;
    let (size, was, now, (x, y)) = (1..=16u8)
        .find_map(|step| {
            let size = base + f32::from(step);
            zoom(&mut app, size);
            let found = cells.iter().find_map(|&(i, (x, y))| {
                let j = action(app.frame_cell_at(wid, x, y)?)?;
                (j != i).then_some((size, i, j, (x, y)))
            });
            zoom(&mut app, base);
            found
        })
        .expect("a zoom puts another action under one's pixel");

    assert_eq!(app.glass_grid(wid), laid, "the zoom undone");
    app.on_cursor_moved(wid, x, y);
    assert_eq!(lit(&app, wid), Some(was), "CONTROL: its action lit");
    assert_eq!(app.tab_menu_action_at(wid, x, y), Some(was));

    zoom(&mut app, size);
    assert_ne!(app.glass_grid(wid), laid, "the zoom re-grids the window");
    app.windows.get_mut(&wid).expect("window").tab_menu_laid = app.glass_grid(wid);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        lit(&app, wid),
        Some(now),
        "CONTROL: unstamped, the stale rect lights another action"
    );
    assert_eq!(
        app.tab_menu_action_at(wid, x, y),
        Some(now),
        "CONTROL: and a press would choose it"
    );
    app.windows.get_mut(&wid).expect("window").tab_menu_laid = laid;
    app.on_cursor_moved(wid, x, y);
    assert_eq!(
        lit(&app, wid),
        None,
        "a rect laid on another grid lights nothing"
    );
    assert_eq!(
        app.tab_menu_action_at(wid, x, y),
        None,
        "and offers no action"
    );
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "the re-grid closes nothing"
    );

    frame(&mut app, wid);
    let relaid = app.windows[&wid].tab_menu_rect.expect("laid again");
    assert_eq!(app.windows[&wid].tab_menu_laid, app.glass_grid(wid));
    let (row, i) = (relaid.row..relaid.row + relaid.h)
        .find_map(|row| {
            crate::tab_menu::action_at(relaid, &entries, row, relaid.col + 2).map(|i| (row, i))
        })
        .expect("an action on the card laid again");
    let (x, y) = cell_px(&app, wid, (row, relaid.col + 2));
    app.on_cursor_moved(wid, x, y);
    assert_eq!(lit(&app, wid), Some(i), "the card laid again answers");

    zoom(&mut app, base);
    assert_eq!(
        app.tab_menu_action_at(wid, x, y),
        None,
        "no action under the press"
    );
    assert!(!app.tab_menu_contains(wid, x, y), "nor the card");
    app.on_mouse_input(
        wid,
        aterm_window::event::ElementState::Pressed,
        aterm_window::event::MouseButton::Left,
    );
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "the press closed the card"
    );
}
