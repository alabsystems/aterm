// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::menu::MenuAction;
use crate::session_chrome::TabMenuEntry;
use crate::{App, WindowId, term_lock};

/// Fill the render scratch from the engine as a redraw does, then prepend
/// the strip. The stub engine is first sized to the window's grid, or
/// `cell_frame_into` would yield empty rows and the assertions pass vacuously.
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

/// A right-click's column opens the composed model, the splice paints it,
/// and a click at a painted row resolves through the painter's recorded rect
/// to that row's `MenuAction` (including `Copy Session ID`, reachable by no
/// other gesture).
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

    // `Copy Session ID` in the model, then the frame row the splice painted for it.
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

/// The repaint key moves when the card pops and when it goes, or the
/// settled-screen early-out would swallow both frames.
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

/// When the window shrinks below any card size and `place` declines, the
/// menu closes rather than leaving an invisible modal whose ↵ could
/// dispatch an unseen row (including `Close Tab`).
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

    // Shrink the frame below `strip_rows + 2`, as a resize drag past the minimum does.
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

/// The card hangs under the strip at the top of the grid band, so
/// `grid_top_row` is raised past it; otherwise the sub-row scroll translation
/// would glide the card with output behind it (the same invariant as
/// `splice_paste_banner`).
#[test]
fn the_card_is_pinned_out_of_the_sub_row_scroll_band() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_tab_context_menu(wid, 0, 0, false));
    composed_frame(&mut app, wid);
    // The strip splice leaves the band starting at the first grid row; give
    // it a real extent so the raise has something to clamp against.
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

    // Closing hands the band back.
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

/// THE TAB MENU'S CARD IS ON TOP TOO (review of surfaces-F3): the card a
/// right-click hangs under the strip is painted after the frame published
/// its overlays, so a caret under it and a pet across its edge would paint
/// over its rows. The splice hides the caret on the card and cuts the pet
/// at the card's edge — its part beside the card stays. NEGATIVE CONTROL:
/// with no card the same caret and pet stand whole.
#[test]
fn the_tab_menus_card_hides_the_caret_and_cuts_the_pet_under_it() {
    use aterm_core::render::FreeSprite;
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
    // A pet four cells wide across the card's right edge, on its second row,
    // and the caret on the card.
    let pet = FreeSprite {
        x: px(right - 2, cw),
        y: px(rect.row + 2, ch),
        w: u16::try_from(4 * cw).unwrap(),
        h: u16::try_from(ch).unwrap(),
        aw: u16::try_from(4 * cw).unwrap(),
        ah: u16::try_from(ch).unwrap(),
        ..FreeSprite::default()
    };
    let publish = |app: &mut App| {
        let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        (s.cursor_row, s.cursor_col, s.cursor_visible) = (rect.row + 2, rect.col + 2, true);
        s.free_sprites = vec![pet];
    };
    composed_frame(&mut app, wid);
    publish(&mut app);
    app.splice_tab_menu(wid);
    let s = &app.windows[&wid].input_scratch;
    assert!(!s.cursor_visible, "the caret under the card is hidden");
    assert_eq!(
        s.free_sprites
            .iter()
            .map(|q| (q.x, q.w, q.ax))
            .collect::<Vec<_>>(),
        [(
            px(right, cw),
            u16::try_from(2 * cw).unwrap(),
            u16::try_from(2 * cw).unwrap()
        )],
        "the pet beside the card stays, the part on it goes"
    );
    // CONTROL: closed, the splice leaves both whole.
    assert!(app.close_tab_menu(wid));
    composed_frame(&mut app, wid);
    publish(&mut app);
    app.splice_tab_menu(wid);
    let s = &app.windows[&wid].input_scratch;
    assert!(s.cursor_visible, "CONTROL: no card, the caret stands");
    assert_eq!(s.free_sprites, [pet], "CONTROL: no card, the pet is whole");
}
