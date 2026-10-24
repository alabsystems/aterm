// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::menu::MenuAction;
use crate::session_chrome::TabMenuEntry;
use crate::{App, WindowId, term_lock};

fn composed_frame(app: &mut App, wid: WindowId) {
    let (rows, cols) = {
        let ws = app.windows.get(&wid).unwrap();
        (ws.rows, ws.cols)
    };
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        let mut term = term_lock(&terminal);
        term.resize(rows, cols);
        term.cell_frame_into(&mut ws.input_scratch, rows as usize, cols as usize);
        ws.input_scratch.fx_occluders.clear(); // as the redraw does before it splices
    }
    app.splice_tab_strip_with(wid, 1);
}

fn frame_row_text(app: &App, wid: WindowId, row: usize) -> String {
    app.windows[&wid]
        .input_scratch
        .cells
        .get(row)
        .map(|r| r.iter().map(|c| c.ch).collect())
        .unwrap_or_default()
}

#[test]
fn the_painted_card_hit_tests_back_to_the_models_action() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;

    composed_frame(&mut app, wid);
    let before = app.windows[&wid].input_scratch.cells.clone();
    app.splice_tab_menu(wid);
    assert_eq!(
        app.windows[&wid].input_scratch.cells, before,
        "no menu ⇒ the frame is untouched"
    );
    assert!(app.windows[&wid].tab_menu_rect.is_none());

    assert!(app.open_tab_context_menu(wid, 0, 3, false));
    composed_frame(&mut app, wid);
    app.splice_tab_menu(wid);
    let rect = app.windows[&wid]
        .tab_menu_rect
        .expect("the painter recorded where the card landed");
    assert_eq!(rect.row, 1, "the card hangs under the one-row band");

    let entries = app.windows[&wid].tab_menu.as_ref().unwrap().entries.clone();
    let i = entries
        .iter()
        .position(|e| {
            matches!(e, TabMenuEntry::Action { action, .. } if *action == MenuAction::CopySessionId)
        })
        .expect("the composed model always offers the session-id copy");
    assert!(
        i < crate::tab_menu::visible_entries(rect),
        "and it is shown"
    );
    let painted = frame_row_text(&app, wid, rect.row + 1 + i);
    assert!(
        painted.contains("Copy Session ID"),
        "the row is on the glass: {painted:?}"
    );
    assert!(
        painted.starts_with(&" ".repeat(rect.col)),
        "…and only inside the card's columns: {painted:?}"
    );

    let hit = crate::tab_menu::action_at(rect, &entries, rect.row + 1 + i, rect.col + 2)
        .expect("a live action row");
    assert_eq!(hit, i);
    match &entries[hit] {
        TabMenuEntry::Action { action, .. } => {
            assert_eq!(*action, MenuAction::CopySessionId);
        }
        other => panic!("expected an action row, got {other:?}"),
    }

    assert!(app.close_tab_menu(wid));
    composed_frame(&mut app, wid);
    app.splice_tab_menu(wid);
    assert_eq!(
        app.windows[&wid].input_scratch.cells, before,
        "the erase frame restores the pre-menu grid"
    );
}

#[test]
fn the_repaint_key_term_moves_on_open_highlight_and_close() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let fp = |app: &App| {
        let ws = &app.windows[&wid];
        crate::tab_menu::fingerprint(ws.tab_menu.as_ref(), ws.tab_menu_rect)
    };
    assert_eq!(fp(&app), 0, "closed is the byte-identical sentinel");
    assert!(app.open_tab_context_menu(wid, 0, 0, false));
    composed_frame(&mut app, wid);
    app.splice_tab_menu(wid);
    let open = fp(&app);
    assert_ne!(open, 0);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_menu
        .as_mut()
        .unwrap()
        .highlight = Some(0);
    assert_ne!(fp(&app), open, "a highlight step forces its own frame");
    app.close_tab_menu(wid);
    assert_eq!(fp(&app), 0);
}

#[test]
fn a_resize_that_leaves_no_room_closes_the_card_instead_of_hiding_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 0, true));
    composed_frame(&mut app, wid);
    app.splice_tab_menu(wid);
    assert!(
        app.windows[&wid].tab_menu_rect.is_some(),
        "it fits at the fixture size"
    );
    assert!(
        app.windows[&wid]
            .tab_menu
            .as_ref()
            .unwrap()
            .highlight
            .is_some(),
        "a keyboard open seeds a highlight — the row ↵ would fire"
    );

    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.input_scratch.cells.truncate(2);
    }
    app.splice_tab_menu(wid);
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "no room for the card ⇒ the card is CLOSED, not merely unpainted"
    );
    assert!(app.windows[&wid].tab_menu_rect.is_none());
}

#[test]
fn the_card_is_pinned_out_of_the_sub_row_scroll_band() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 0, false));
    composed_frame(&mut app, wid);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.input_scratch.grid_top_row = 1;
        ws.input_scratch.grid_bot_row = ws.input_scratch.cells.len();
    }
    app.splice_tab_menu(wid);
    let rect = app.windows[&wid].tab_menu_rect.expect("painted");
    let top = app.windows[&wid].input_scratch.grid_top_row;
    assert!(
        top >= rect.row + rect.h,
        "the glide band must start BELOW the card: top={top} card=[{}, {})",
        rect.row,
        rect.row + rect.h
    );
    assert!(
        top <= app.windows[&wid].input_scratch.grid_bot_row,
        "and the band must never invert"
    );

    app.close_tab_menu(wid);
    composed_frame(&mut app, wid);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.input_scratch.grid_top_row = 1;
        ws.input_scratch.grid_bot_row = ws.input_scratch.cells.len();
    }
    app.splice_tab_menu(wid);
    assert_eq!(app.windows[&wid].input_scratch.grid_top_row, 1);
}

#[test]
fn the_tab_menus_card_hides_the_caret_and_cuts_the_pet_under_it() {
    use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers, is_sprite_layer, published};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 3, false));
    composed_frame(&mut app, wid);
    app.splice_tab_menu(wid);
    let rect = app.windows[&wid].tab_menu_rect.expect("painted");
    let (cw, ch) = app.win_cell_size(wid);
    let px = |cells: usize, size: usize| i32::try_from(cells * size).unwrap();
    let right = rect.col + rect.w;
    assert!(
        right + 3 < app.windows[&wid].input_scratch.cols,
        "room beside it"
    );
    let body = aterm_render::fx::bitmap(
        aterm_core::render::FxTexels::Rgba8Straight,
        u16::try_from(4 * cw).unwrap(),
        u16::try_from(ch).unwrap(),
        vec![255; 4 * cw * ch * 4],
    )
    .expect("a body");
    let pet = FreeSprite {
        z: FreeZ::UnderText,
        ..FreeSprite::whole(px(right - 2, cw), px(rect.row + 2, ch), body)
    };
    let publish = |app: &mut App| {
        let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        (s.cursor_row, s.cursor_col, s.cursor_visible) = (rect.row + 2, rect.col + 2, true);
        s.fx_layers.retain(|l| !is_sprite_layer(l));
        SpriteLayers::new(0x7461_626D).publish(std::slice::from_ref(&pet), &mut s.fx_layers);
    };
    let card_occluder = |s: &aterm_render::RenderInput| {
        s.fx_occluders
            .iter()
            .find(|o| o.x1 - o.x0 == px(rect.w, cw) && o.y1 - o.y0 == px(rect.h, ch))
            .copied()
    };
    composed_frame(&mut app, wid);
    publish(&mut app);
    app.splice_tab_menu(wid);
    let s = &app.windows[&wid].input_scratch;
    assert!(!s.cursor_visible, "the caret under the card is hidden");
    assert_eq!(
        published(&s.fx_layers),
        std::slice::from_ref(&pet),
        "the pet's layer stands whole"
    );
    let occ = card_occluder(s).expect("the card occludes every fx layer");
    let (pad, top) = (occ.x0 - px(rect.col, cw), occ.y0 - px(rect.row, ch));
    let (x0, x1) = (pad + pet.x, pad + pet.x + i32::from(pet.w));
    let (y0, y1) = (top + pet.y, top + pet.y + i32::from(pet.h));
    assert!(
        occ.x0 <= x0 && occ.x1 == pad + px(right, cw) && occ.x1 < x1,
        "the card cuts the part of the pet on it and leaves the part beside it"
    );
    assert!(occ.y0 <= y0 && y1 <= occ.y1, "over all of the pet's rows");
    assert!(app.close_tab_menu(wid));
    composed_frame(&mut app, wid);
    publish(&mut app);
    app.splice_tab_menu(wid);
    let s = &app.windows[&wid].input_scratch;
    assert!(s.cursor_visible, "CONTROL: no card, the caret stands");
    assert_eq!(published(&s.fx_layers), [pet], "CONTROL: the pet is whole");
    assert!(
        card_occluder(s).is_none(),
        "CONTROL: no card, nothing occludes it"
    );
}
