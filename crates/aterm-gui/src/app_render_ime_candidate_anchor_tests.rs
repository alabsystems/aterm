// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};
use std::time::{Duration, Instant};

fn split_fixture() -> (App, WindowId, usize, usize) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let _ = app.split_active_stub_tab(wid);
    let (rows, cols) = {
        let ws = app.windows.get(&wid).expect("test window");
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    (app, wid, rows, cols)
}

fn present(
    app: &mut App,
    wid: WindowId,
    size: (usize, usize),
    frame: u64,
) -> Option<(i32, i32, u32, u32)> {
    let at = Instant::now() + Duration::from_millis(16 * frame);
    assert!(
        app.redraw_compose(wid, size.0, size.1, false, false, None, 0, at)
            .is_some(),
        "fixture: the composed present path ran"
    );
    app.windows[&wid].last_ime_rect
}

#[test]
fn a_split_anchors_the_candidate_list_on_the_composition_not_its_start() {
    let (mut app, wid, rows, cols) = split_fixture();
    let engine = present(&mut app, wid, (rows, cols), 0)
        .expect("the engine caret rect is reported on an ordinary frame");

    app.on_ime_preedit(wid, "にほん".to_string(), None);
    let composing = present(&mut app, wid, (rows, cols), 1).expect("the composing caret rect");
    assert!(
        app.windows[&wid].preedit_shown,
        "fixture: the composed frame really painted the composition"
    );
    let cw = i32::try_from(app.win_cell_size(wid).0).expect("cell width");
    assert_eq!(
        (composing.0 - engine.0, composing.1),
        (6 * cw, engine.1),
        "six columns of kana on the same row: the candidate list follows the \
         caret, not the column the composition started at"
    );

    app.on_ime_preedit(wid, String::new(), None);
    assert_eq!(present(&mut app, wid, (rows, cols), 2), Some(engine));
}

#[test]
fn a_hidden_terminal_cursor_still_anchors_the_composition() {
    let (mut app, wid, rows, cols) = split_fixture();
    let session = app.windows[&wid].layouts[app.windows[&wid].tabs.active].focus();
    let term = app
        .pool
        .get(session)
        .expect("focused leaf terminal")
        .term
        .clone();
    crate::term_lock(&term).process(b"\x1b[?25l");
    assert_eq!(
        present(&mut app, wid, (rows, cols), 0),
        None,
        "control: a hidden cursor with no composition reports nothing"
    );
    app.on_ime_preedit(wid, "に".to_string(), None);
    assert!(
        present(&mut app, wid, (rows, cols), 1).is_some(),
        "the composition is on screen, so its caret rectangle must go out"
    );
}
