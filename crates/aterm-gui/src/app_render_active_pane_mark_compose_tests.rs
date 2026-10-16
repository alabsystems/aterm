// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The four glyphs [`active_pane_edge_cell`] can put in a divider gap. No
/// terminal in these fixtures ever prints one, so a cell carrying one is the
/// mark and nothing else.
const MARK_GLYPHS: [char; 4] = ['▁', '▔', '▐', '▌'];

/// The composed frame's ring: every cell outside header chrome carrying a mark glyph, as
/// `(row, col, glyph)`. Each is asserted to stand on a divider cell, so the mark never
/// reaches a cell a pane owns.
fn ring(app: &App, wid: WindowId) -> Vec<(usize, usize, char)> {
    let seam = divider_cell(app.theme);
    let input = &app.windows[&wid].input_scratch;
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("visible split plan");
    let mut out = Vec::new();
    for (r, row) in input.cells.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if MARK_GLYPHS.contains(&cell.ch)
                && plan
                    .header_at(crate::tab_model::LogicalPoint {
                        x: c as f32,
                        y: r as f32,
                    })
                    .is_none()
            {
                assert_eq!(
                    cell.bg, seam.bg,
                    "the mark at ({r}, {c}) is standing on something that is not \
                     a divider gap"
                );
                out.push((r, c, cell.ch));
            }
        }
    }
    out.sort_unstable();
    out
}

/// [`expected_ring`] minus the cells header chrome covers.
fn expected_visible_ring(
    app: &App,
    wid: WindowId,
    rect: (usize, usize, usize, usize),
) -> Vec<(usize, usize, char)> {
    let (rows, cols) = window_dims(app, wid);
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("visible split plan");
    expected_ring(rows, cols, rect)
        .into_iter()
        .filter(|(row, col, _)| {
            plan.header_at(crate::tab_model::LogicalPoint {
                x: *col as f32,
                y: *row as f32,
            })
            .is_none()
        })
        .collect()
}

/// The ring a pane rectangle should produce, from the layout alone: the divider run on each
/// side that exists, inked on the side facing the pane, the horizontal runs reaching the
/// corners.
fn expected_ring(
    rows: usize,
    cols: usize,
    (row, col, pane_rows, pane_cols): (usize, usize, usize, usize),
) -> Vec<(usize, usize, char)> {
    let (row_end, col_end) = (row + pane_rows, col + pane_cols);
    let across = col.saturating_sub(1)..(col_end + 1).min(cols);
    let mut out = Vec::new();
    if row > 0 {
        out.extend(across.clone().map(|c| (row - 1, c, '▁')));
    }
    if row_end < rows {
        out.extend(across.map(|c| (row_end, c, '▔')));
    }
    if col > 0 {
        out.extend((row..row_end).map(|r| (r, col - 1, '▐')));
    }
    if col_end < cols {
        out.extend((row..row_end).map(|r| (r, col_end, '▌')));
    }
    out.sort_unstable();
    out
}

fn window_dims(app: &App, wid: WindowId) -> (usize, usize) {
    let window = &app.windows[&wid];
    (usize::from(window.rows), usize::from(window.cols))
}

/// The canonical content rectangle of the pane that takes keyboard input.
fn focused_pane_rect(app: &App, wid: WindowId) -> (usize, usize, usize, usize) {
    let plan = app
        .active_visible_leaf_plan(wid)
        .expect("visible split plan");
    let rect = plan.leaf(plan.focused).expect("focused leaf").rect;
    (
        rect.origin.y.round() as usize,
        rect.origin.x.round() as usize,
        rect.size.height.round() as usize,
        rect.size.width.round() as usize,
    )
}

fn compose_pure(app: &mut App, wid: WindowId, now: Instant) -> bool {
    let (rows, cols) = window_dims(app, wid);
    app.redraw_compose(wid, rows, cols, false, false, None, 0, now)
        .is_some()
}

/// A two-pane pure-terminal split, side by side, with the window holding the
/// keyboard. `split_active_stub_tab` leaves the new (right) pane focused.
fn pure_split() -> (App, WindowId, u64, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    let left = app
        .active_tree(wid)
        .expect("a pure-terminal tab")
        .sessions()
        .into_iter()
        .find(|s| *s != right)
        .expect("two panes");
    app.windows.get_mut(&wid).expect("test window").focused = true;
    (app, wid, left, right)
}

/// A typed wake into an unfocused window keeps its cursor companion, but Sparkle Words are not
/// presentable.
#[test]
fn a_typed_wake_wakes_the_companion_but_not_sparkle_words_in_a_split() {
    let (mut app, wid, _, _) = pure_split();
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.recompute_sparkle();
    assert!(app.sparkle.is_some(), "fixture: Sparkle Words resolve ON");
    let now = Instant::now();
    // Control: a focused window's words present.
    assert!(compose_pure(&mut app, wid, now));
    assert!(app.windows[&wid].fx.decos().presentable());

    let later = now + Duration::from_millis(16);
    {
        let ws = app.windows.get_mut(&wid).expect("test window");
        ws.focused = false;
        ws.last_key_at = Some(later);
    }
    assert!(
        app.cursor_fx_focus(wid, false, later),
        "the typed wake is live: the companion's focus input is held"
    );
    assert!(compose_pure(&mut app, wid, later));
    assert!(
        !app.windows[&wid].fx.decos().presentable(),
        "the word engine takes the UN-WOKEN focus"
    );
}

/// The single-pane present uses the same predicate ([`sparkle_words_presentable`]) over
/// `motion_focus`, which a typed wake does not move though `cursor_fx_focus` does. This pins
/// the predicate and its input, not that call site (it cannot run glass-less).
#[test]
fn sparkle_words_presentability_ignores_the_typed_wake() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();
    {
        let ws = app.windows.get_mut(&wid).expect("test window");
        ws.focused = false;
        ws.last_key_at = Some(now);
    }
    assert!(
        app.cursor_fx_focus(wid, false, now),
        "the companion is woken"
    );
    assert!(
        !app.motion_focus(wid, false),
        "the words' focus input is not"
    );
    assert!(!sparkle_words_presentable(
        app.motion_focus(wid, false),
        false
    ));
    assert!(sparkle_words_presentable(true, false));
    assert!(
        !sparkle_words_presentable(true, true),
        "suspension still wins"
    );
}

fn focus_pure_pane(app: &mut App, wid: WindowId, session: u64) {
    assert!(
        app.active_tree_mut(wid)
            .is_some_and(|tree| tree.set_focus(session)),
        "the pane is live"
    );
    let active = app.windows[&wid].tabs.active;
    assert!(app.sync_tab_model_from_layout(wid, active));
    app.sync_window(wid);
}

/// A split's ring surrounds the pane taking keystrokes and follows pane focus. The expected
/// ring comes from the layout, so the mark lands only in the divider gaps touching that pane.
#[test]
fn a_pure_split_rings_the_pane_that_takes_keystrokes_and_follows_the_focus() {
    let (mut app, wid, left, right) = pure_split();
    let t0 = Instant::now();

    assert!(compose_pure(&mut app, wid, t0));
    let on_the_right = ring(&app, wid);
    assert_eq!(
        on_the_right,
        expected_visible_ring(&app, wid, focused_pane_rect(&app, wid)),
        "the ring hugs the focused (right) pane"
    );
    assert!(
        on_the_right
            .iter()
            .filter(|(row, _, _)| *row >= focused_pane_rect(&app, wid).0)
            .all(|(_, _, ch)| *ch == '▐'),
        "a side-by-side split gives the right pane exactly one edge — the \
         divider column on its LEFT, inked on the side facing it: {on_the_right:?}"
    );

    focus_pure_pane(&mut app, wid, left);
    assert!(compose_pure(&mut app, wid, t0 + Duration::from_millis(16)));
    let on_the_left = ring(&app, wid);
    assert_eq!(
        on_the_left,
        expected_visible_ring(&app, wid, focused_pane_rect(&app, wid)),
        "the ring followed the keyboard to the left pane"
    );
    assert!(
        on_the_left
            .iter()
            .filter(|(row, _, _)| *row >= focused_pane_rect(&app, wid).0)
            .all(|(_, _, ch)| *ch == '▌'),
        "…and swapped to the other side of the SAME divider column, which is \
         the only thing that says which of two panes sharing it is live: \
         {on_the_left:?}"
    );
    let cells_of =
        |ring: &[(usize, usize, char)]| ring.iter().map(|(r, c, _)| (*r, *c)).collect::<Vec<_>>();
    assert_eq!(
        cells_of(&on_the_left),
        cells_of(&on_the_right),
        "the same cells carry both answers"
    );

    focus_pure_pane(&mut app, wid, right);
    assert!(compose_pure(&mut app, wid, t0 + Duration::from_millis(32)));
    assert_eq!(ring(&app, wid), on_the_right, "and back again");
}

/// A mixed terminal/native split marks a native leaf like a terminal one: the ring is made of
/// the leaf's layout rectangle and `leaf.focused`, never its content (a native leaf's pixels
/// are a tray blitted over the composite).
#[test]
fn a_mixed_split_rings_the_native_leaf_when_the_native_leaf_takes_keys() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    app.windows.get_mut(&wid).expect("test window").focused = true;
    let plan = app.active_visible_leaf_plan(wid).expect("a mixed plan");
    assert_eq!(
        plan.leaves.len(),
        2,
        "one native leaf and one terminal leaf"
    );
    let native_view = plan
        .leaves
        .iter()
        .map(|leaf| leaf.view)
        .find(|view| *view != terminal_view)
        .expect("the settings leaf");
    let rect_of = |app: &App, view| {
        let leaf = app
            .active_visible_leaf_plan(wid)
            .expect("a mixed plan")
            .leaves
            .into_iter()
            .find(|leaf| leaf.view == view)
            .expect("the leaf is visible");
        (
            leaf.rect.origin.y.round().max(0.0) as usize,
            leaf.rect.origin.x.round().max(0.0) as usize,
            (leaf.rect.size.height.round() as usize).max(1),
            (leaf.rect.size.width.round() as usize).max(1),
        )
    };
    let focus_leaf = |app: &mut App, view| {
        assert!(
            app.windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.active_mut())
                .is_some_and(|tab| tab.set_focus(view)),
            "the leaf is live"
        );
        app.sync_window(wid);
    };

    // The split left the terminal leaf focused.
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let terminal_ring = ring(&app, wid);
    assert_eq!(
        terminal_ring,
        expected_visible_ring(&app, wid, rect_of(&app, terminal_view)),
        "the ring hugs the terminal leaf while the terminal leaf has the keyboard"
    );

    focus_leaf(&mut app, native_view);
    assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
    let native_ring = ring(&app, wid);
    assert_eq!(
        native_ring,
        expected_visible_ring(&app, wid, rect_of(&app, native_view)),
        "and hugs the NATIVE leaf while the native leaf has it — the mark is \
         the leaf's rectangle, so a leaf whose content the compositor cannot \
         touch is marked exactly as well as one whose content it owns"
    );
    assert_ne!(
        terminal_ring, native_ring,
        "the two answers must be different, or the mark says nothing"
    );
    assert!(
        !terminal_ring.is_empty(),
        "fixture guard: a mixed split has a divider gap to mark"
    );
}

/// An unfocused window and the setting off each silence the ring (the first compose is the
/// negative control): of two windows showing a split, only the one with the keyboard claims
/// a pane.
#[test]
fn an_unfocused_window_and_the_setting_off_each_compose_a_plain_seam_grid() {
    let t0 = Instant::now();

    let (mut app, wid, _, _) = pure_split();
    assert!(compose_pure(&mut app, wid, t0));
    assert!(
        !ring(&app, wid).is_empty(),
        "negative control: the fixture marks a pane"
    );

    app.windows.get_mut(&wid).expect("test window").focused = false;
    assert!(compose_pure(&mut app, wid, t0 + Duration::from_millis(16)));
    assert!(
        ring(&app, wid).is_empty(),
        "a window without the keyboard claims no pane"
    );

    let (mut app, wid, _, _) = pure_split();
    app.config.split_focus_mark = Some(false);
    assert!(compose_pure(&mut app, wid, t0));
    assert!(
        ring(&app, wid).is_empty(),
        "the setting off composes the plain seam grid"
    );
}

/// The mark lives in the divider gap, so composing with it on or off leaves every cell
/// outside the ring byte-identical.
#[test]
fn the_mark_leaves_every_cell_of_every_pane_byte_identical() {
    let t0 = Instant::now();
    let compose = |enabled| {
        let (mut app, wid, _, _) = pure_split();
        app.config.split_focus_mark = Some(enabled);
        let term = app.pool.get(0).expect("left pane").term.clone();
        term_lock(&term).process(b"\x1b[31mLEFT\x1b[m");
        assert!(compose_pure(&mut app, wid, t0));
        let rect = focused_pane_rect(&app, wid);
        let (rows, cols) = window_dims(&app, wid);
        (
            app.windows[&wid].input_scratch.cells.clone(),
            rect,
            rows,
            cols,
        )
    };
    let (marked, rect, rows, cols) = compose(true);
    let (plain, plain_rect, ..) = compose(false);
    assert_eq!(rect, plain_rect, "same layout either way");

    let ring_cells: std::collections::BTreeSet<_> = expected_ring(rows, cols, rect)
        .into_iter()
        .map(|(r, c, _)| (r, c))
        .collect();
    assert!(!ring_cells.is_empty(), "fixture guard: there is a ring");
    for r in 0..rows {
        for c in 0..cols {
            if ring_cells.contains(&(r, c)) {
                assert_ne!(
                    marked[r][c], plain[r][c],
                    "({r}, {c}) is the ring and must differ"
                );
            } else {
                assert_eq!(
                    marked[r][c], plain[r][c],
                    "({r}, {c}) is not the ring and must not have moved"
                );
            }
        }
    }
}
