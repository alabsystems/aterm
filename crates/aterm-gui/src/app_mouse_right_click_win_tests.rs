// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows right-click on the grid, through the function a real right press calls
//! (`App::on_mouse_input`), under the platform default (`right_click` unset: copy/paste, the
//! conhost and Windows Terminal convention) and the test build's clipboard board, never the
//! person's clipboard. The unix suite (`app_mouse_pointer_route_tests`) reads the paste off a
//! pipe; Windows has no such observer, so here the copy half is read off the board and the
//! selection.

use aterm_window::event::{ElementState, MouseButton};

use crate::{App, WindowId, term_lock};

/// A right press and release at window cell `(row, col)`.
fn right_click_at(app: &mut App, wid: WindowId, (row, col): (u16, u16)) {
    let (x, y) = app
        .cell_centre_px(wid, row, col)
        .expect("a cell on the grid");
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
}

/// With a selection and no mouse-tracking program, a right-click copies the selection to the
/// clipboard and clears it, under Windows' default `right_click`; with tracking on the press is
/// the program's and nothing is copied.
#[test]
fn a_right_click_copies_the_selection_by_default_on_windows() {
    assert_eq!(
        crate::app_config::RightClickGesture::PLATFORM_DEFAULT,
        crate::app_config::RightClickGesture::CopyPaste
    );
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert_eq!(app.config.right_click, None, "the platform default");
    let term = app.pool.get(0).expect("session 0").term.clone();
    let select_hello = || {
        let mut t = term_lock(&term);
        t.process(b"\x1b[Hhello world");
        let selection = t.text_selection_mut();
        selection.start_selection(
            0,
            0,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        selection.update_selection(0, 4, aterm_core::selection::SelectionSide::Right);
        selection.complete_selection();
    };
    select_hello();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("before".into()));
    let (row, col) = app
        .visible_session_rect(wid, 0)
        .map_or((0, 0), |(r, c, ..)| (r, c));
    right_click_at(&mut app, wid, (row, col + 2));
    let copied = crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    assert_eq!(copied.as_deref(), Some("hello"));
    assert_eq!(
        term_lock(&term)
            .selection_to_string()
            .filter(|text| !text.is_empty()),
        None,
        "the copy takes the selection"
    );

    // A program tracking the mouse gets the press; the clipboard is left alone.
    term_lock(&term).process(b"\x1b[?1000h");
    select_hello();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("before".into()));
    right_click_at(&mut app, wid, (row, col + 2));
    let kept = crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    assert_eq!(kept.as_deref(), Some("before"));
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
}
