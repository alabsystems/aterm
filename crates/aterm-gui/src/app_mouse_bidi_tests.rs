// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A selection made with the pointer on a BiDi row highlights the cells it
//! copies (bug hunt 9, the copy lane).
//!
//! The frame paints an RTL run reversed in place, while the selection, the
//! copy, the word rules and find read the grid in logical order. The pointer
//! path anchors at the logical cell under the pointer (`pointer_anchor`) and
//! the renderer's predicate tests each painted cell at its logical column
//! (`RenderInput::selection_hit`), so a sweep over the painted cells of one
//! run selects exactly those cells, and any selection highlights exactly what
//! it copies. The copy-mode cursor, a logical column too, is painted on the
//! cell that holds it (`RenderInput::visual_col`).

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

use crate::{App, WindowId, term_lock};

/// `abc שלום עולם def`, painted `abc םלוע םולש def`: visual cells 4..=7 show
/// עולם (logical 9..=12) and 9..=12 show שלום (logical 4..=7).
const MIXED: &str = "abc \u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD} def";
const OLAM: &str = "\u{05E2}\u{05D5}\u{05DC}\u{05DD}";

/// A headless window whose front terminal shows `text` on its top row.
fn window_with(text: &str) -> (App, WindowId, std::sync::Arc<std::sync::Mutex<Terminal>>) {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(text.as_bytes());
    (app, wid, term)
}

/// Press on painted cell `from` (row 0), sweep to the `side` half of painted
/// cell `to`, release (no clipboard: the release is fenced).
fn sweep(app: &mut App, wid: WindowId, from: u16, to: u16, side: SelectionSide) {
    sweep_on(app, wid, 0, from, to, side);
}

/// [`sweep`] on viewport row `row`.
fn sweep_on(app: &mut App, wid: WindowId, row: u16, from: u16, to: u16, side: SelectionSide) {
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.last_mouse_cell = (row, from);
        ws.last_mouse_side = SelectionSide::Left;
    }
    app.begin_selection(wid, SelectionType::Simple);
    app.windows.get_mut(&wid).expect("window").last_mouse_side = side;
    app.drag_selection(wid, row, to);
    app.finish_selection(wid, true);
}

/// The painted columns of row 0 the renderer's predicate highlights, and the
/// text they hold read in logical order (the order a copy reads).
fn highlight(term: &mut Terminal) -> (Vec<usize>, String) {
    highlight_on(term, 0)
}

/// [`highlight`] on viewport row `r` of the frame the viewport shows.
fn highlight_on(term: &mut Terminal, r: usize) -> (Vec<usize>, String) {
    let cols = usize::from(term.cols());
    let rows = usize::from(term.rows());
    let frame = term.cell_frame(rows, cols);
    let row = &frame.cells[r];
    let lit: Vec<usize> = (0..row.len())
        .filter(|&v| {
            let lead = row.get(v + 1).is_some_and(|c| c.wide);
            frame.selection_contains_cell(r, v, lead, row[v].wide)
        })
        .collect();
    let mut logical: Vec<(usize, char)> = lit
        .iter()
        .map(|&v| (frame.logical_col(r, v), row[v].ch))
        .collect();
    logical.sort_unstable();
    (lit, logical.into_iter().map(|(_, ch)| ch).collect())
}

/// Shift-click on the `side` half of painted cell `col` of selection row
/// `sel_row` (viewport row minus `display_offset`), as `on_mouse_input` does.
fn shift_click(app: &mut App, wid: WindowId, sel_row: i32, col: u16, side: SelectionSide) {
    app.windows.get_mut(&wid).expect("window").last_mouse_side = side;
    assert!(
        app.extend_selection_to(wid, sel_row, col, side),
        "a selection to extend"
    );
}

/// A sweep over the painted cells of one RTL word, in either direction,
/// selects exactly the cells it crossed: the press anchors at the left edge of
/// the pressed cell, so a leftward sweep leaves the pressed cell out, as it
/// does on an LTR row. Before the fix the press anchored logical column 4 and
/// the highlight lit the swept cells while the copy read שלום's.
#[test]
fn a_sweep_over_an_rtl_word_highlights_and_copies_the_same_word() {
    let (mut app, wid, term) = window_with(MIXED);
    sweep(&mut app, wid, 4, 7, SelectionSide::Right);
    let copied = term_lock(&term).selection_to_string();
    assert_eq!(copied.as_deref(), Some(OLAM));
    let (lit, text) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![4, 5, 6, 7], "the swept cells are lit");
    assert_eq!(text, OLAM, "and they hold what was copied");

    // Leftward, from the left edge of ע's painted cell over ל and ו: the
    // pressed cell stays out.
    sweep(&mut app, wid, 7, 5, SelectionSide::Left);
    let copied = term_lock(&term).selection_to_string().unwrap_or_default();
    let (lit, text) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![5, 6]);
    assert_eq!(text, copied);
    assert_eq!(copied, "\u{05D5}\u{05DC}");
}

/// A sweep that crosses from LTR text into an RTL run is one logical range,
/// so its highlight need not be contiguous on screen, but it lights exactly
/// the text the copy takes (the finding's `abc אבג def` sweep over cells 2..=5
/// painted `c גב` and copied `c אב`).
#[test]
fn a_sweep_across_the_run_boundary_highlights_what_it_copies() {
    let (mut app, wid, term) = window_with("abc \u{05D0}\u{05D1}\u{05D2} def");
    for (from, to, side) in [
        (2, 5, SelectionSide::Right),
        (2, 5, SelectionSide::Left),
        (5, 9, SelectionSide::Right),
        (9, 4, SelectionSide::Left),
    ] {
        sweep(&mut app, wid, from, to, side);
        let copied = term_lock(&term).selection_to_string().unwrap_or_default();
        let (lit, text) = highlight(&mut term_lock(&term));
        assert_eq!(
            text, copied,
            "sweep {from}->{to} {side:?}: lit {lit:?} must hold the copy"
        );
        assert!(!copied.is_empty(), "sweep {from}->{to} selected something");
    }
}

/// A double-click on a painted RTL word selects that word (the word rules
/// run at the logical cell under the pointer), and the highlight covers its
/// painted cells. Before the fix, a double-click on painted עולם selected and
/// copied שלום, the word at the same LOGICAL columns.
#[test]
fn a_double_click_on_an_rtl_word_copies_that_word() {
    let (mut app, wid, term) = window_with(MIXED);
    app.select_word_click(wid, 0, 5);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(OLAM)
    );
    let (lit, text) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![4, 5, 6, 7]);
    assert_eq!(text, OLAM);
    // A still release keeps the word: the gesture arms its drag on the press
    // (`arm_gesture_drag`), so this release completes, whatever the press cell.
    app.finish_selection(wid, true);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(OLAM)
    );
}

/// Control: on a pure-LTR row the pointer's cell is its logical cell, so a
/// sweep and a double-click behave exactly as before.
#[test]
fn an_ltr_row_selects_the_cells_under_the_pointer() {
    let (mut app, wid, term) = window_with("abc hello world def");
    sweep(&mut app, wid, 4, 8, SelectionSide::Right);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("hello")
    );
    let (lit, _) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![4, 5, 6, 7, 8]);
    sweep(&mut app, wid, 8, 4, SelectionSide::Left);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("hell")
    );
    app.select_word_click(wid, 0, 13);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some("world")
    );
    let (lit, _) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![10, 11, 12, 13, 14]);
}

/// A shift-click extends to the logical cell under the pointer, with the
/// half-cell side flipped on an RTL cell: after sweeping visual 4..=5 (the
/// last two letters of עולם, painted at its left end), a shift-click on the
/// right half of the cell painting its first letter (visual 7) takes the
/// whole word, and the highlight is the word's painted cells. Before the fix
/// the click extended to logical 7, the last letter of שלום.
#[test]
fn a_shift_click_on_an_rtl_cell_extends_to_that_cell() {
    let (mut app, wid, term) = window_with(MIXED);
    sweep(&mut app, wid, 4, 5, SelectionSide::Right);
    let (lit, _) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![4, 5]);
    shift_click(&mut app, wid, 0, 7, SelectionSide::Right);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(OLAM)
    );
    let (lit, text) = highlight(&mut term_lock(&term));
    assert_eq!(lit, vec![4, 5, 6, 7]);
    assert_eq!(text, OLAM);
}

/// The same gestures on a row scrolled back into history: the pointer's
/// viewport row is `sel_row + display_offset`, and its map is that row's, so
/// a double-click and a shift-click land on the painted RTL word there too.
/// The row sits on viewport row 1 under a line of filler, so a lookup that
/// forgot the offset would read another row (or none).
#[test]
fn a_scrolled_back_rtl_row_double_clicks_and_shift_clicks_its_painted_word() {
    let mut text = String::from("filler\r\n");
    text.push_str(MIXED);
    text.push_str(&"\r\n".repeat(60));
    let (mut app, wid, term) = window_with(&text);
    let offset = {
        let mut t = term_lock(&term);
        t.scroll_to_top();
        let offset = t.grid().display_offset();
        let (rows, cols) = (usize::from(t.rows()), usize::from(t.cols()));
        let frame = t.cell_frame(rows, cols);
        let row: String = frame.cells[1].iter().map(|c| c.ch).collect();
        assert!(
            row.starts_with("abc \u{05DD}\u{05DC}\u{05D5}\u{05E2}"),
            "{row:?}"
        );
        i32::try_from(offset).expect("offset")
    };
    assert!(offset > 1, "the row is in history");
    let sel_row = 1 - offset;

    app.select_word_click(wid, sel_row, 5);
    app.finish_selection(wid, true);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(OLAM)
    );
    let (lit, text) = highlight_on(&mut term_lock(&term), 1);
    assert_eq!(lit, vec![4, 5, 6, 7]);
    assert_eq!(text, OLAM);

    sweep_on(&mut app, wid, 1, 4, 5, SelectionSide::Right);
    let (lit, _) = highlight_on(&mut term_lock(&term), 1);
    assert_eq!(lit, vec![4, 5]);
    shift_click(&mut app, wid, sel_row, 7, SelectionSide::Right);
    assert_eq!(
        term_lock(&term).selection_to_string().as_deref(),
        Some(OLAM)
    );
    let (lit, text) = highlight_on(&mut term_lock(&term), 1);
    assert_eq!(lit, vec![4, 5, 6, 7]);
    assert_eq!(text, OLAM);
}

/// A click that stays on its cell deselects, whichever half the release is
/// on, on an RTL cell as on an LTR one: the press and the drag compare
/// logical cells (`sel_press_cell` is stored logical), so jitter across the
/// middle of the pressed RTL cell is not a drag. Storing the painted column
/// made the drag's logical cell differ from it, and the click completed a
/// selection.
#[test]
fn a_still_click_on_an_rtl_cell_deselects_as_on_an_ltr_cell() {
    for (row, pressed) in [(MIXED, 4), ("abc hello world def", 4)] {
        let (mut app, wid, term) = window_with(row);
        sweep(&mut app, wid, pressed, pressed, SelectionSide::Right);
        assert!(
            !term_lock(&term).text_selection().has_selection(),
            "{row:?}: a click inside cell {pressed} leaves no selection"
        );
    }
}

/// Put `term`'s copy-mode (vi) cursor on logical column `logical` of row 0
/// of `text`, then park the terminal's own cursor at column 0, so the vi
/// override is the only thing that can paint a cursor elsewhere.
fn vi_cursor_on(term: &std::sync::Arc<std::sync::Mutex<Terminal>>, text: &str, logical: u16) {
    let mut t = term_lock(term);
    t.process(text.as_bytes());
    t.process(format!("\x1b[{}G", logical + 1).as_bytes());
    t.vi_toggle();
    t.process(b"\x1b[1G");
    assert!(t.vi_is_active());
    assert_eq!(t.vi_cursor_point().col, logical);
}

/// The copy-mode cursor is kept in logical columns, as the selection it
/// steers is, so on a BiDi row the frame paints it on the cell that holds its
/// column: on logical 4 (ש, the first letter of שלום) it is painted at visual
/// 12, the cell showing ש, not at visual 4 (ם of עולם). A pure-LTR row paints
/// it at its own column. Single-pane route (`redraw_window`).
#[test]
fn the_copy_mode_cursor_is_painted_on_the_cell_it_names() {
    use crate::scripted_redraw_fixture::scripted_app;
    for (text, logical, painted, ch) in [
        (MIXED, 4, 12, '\u{05E9}'),
        ("abc hello world def", 4, 4, 'h'),
    ] {
        let mut app = scripted_app();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        vi_cursor_on(&term, text, logical);
        app.redraw_window(wid);
        let scratch = &app.windows[&wid].input_scratch;
        assert_eq!(scratch.cursor_col, painted, "{text:?}");
        assert_eq!(
            scratch.cells[scratch.cursor_row][painted].ch, ch,
            "{text:?}"
        );
    }
}

/// The composed (split) route paints the focused pane's copy-mode cursor
/// through the composite's map, which holds the pane's at its window
/// columns: the cursor lands on the cell showing ש in the right pane.
#[test]
fn the_copy_mode_cursor_is_painted_on_the_cell_it_names_in_a_split() {
    for (text, logical, ch) in [(MIXED, 4, '\u{05E9}'), ("abc hello world def", 4, 'h')] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let right = app.split_active_stub_tab(wid);
        let term = app.pool.get(right).expect("right pane").term.clone();
        vi_cursor_on(&term, text, logical);
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                std::time::Instant::now()
            )
            .is_some(),
            "the composed frame presents"
        );
        let scratch = &app.windows[&wid].input_scratch;
        let (row, col) = (scratch.cursor_row, scratch.cursor_col);
        assert!(col > usize::from(logical), "{text:?}: in the right pane");
        assert_eq!(scratch.cells[row][col].ch, ch, "{text:?}");
    }
}
