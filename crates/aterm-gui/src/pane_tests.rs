// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A fresh single-pane tab lays out as one rect covering the whole window.
#[test]
fn single_pane_fills_window() {
    let t = PaneTree::new(7);
    assert_eq!(t.len(), 1);
    assert_eq!(t.focus(), 7);
    let rects = t.compute_layout(24, 80);
    assert_eq!(rects.len(), 1);
    assert_eq!(
        rects[0],
        PaneRect {
            session: 7,
            row_off: 0,
            col_off: 0,
            rows: 24,
            cols: 80
        }
    );
}

/// A multi-pane tab → `to_layout` → `rebuild` with fresh ids keeps the tree
/// shape (dirs, ratios, order), the focused leaf and each leaf's cwd/title; a
/// short spawn list fails safe (`None`).
#[test]
fn layout_round_trips_with_fresh_ids() {
    let mut t = PaneTree::new(1);
    assert!(t.split_focused(SplitDir::Vertical, 2)); // focus → 2
    assert!(t.split_focused(SplitDir::Horizontal, 3)); // focus → 3
    assert_eq!(t.focus(), 3);
    let order = t.sessions(); // [1, 2, 3] in tree order

    let meta = |id: u64| (Some(format!("/s{id}")), format!("t{id}"));
    let layout = t.to_layout(&meta);
    assert_eq!(layout.leaf_count(), 3);

    let fresh = [10u64, 11, 12];
    let rebuilt = PaneTree::rebuild(&layout, &fresh).expect("rebuild");
    assert_eq!(
        rebuilt.sessions(),
        vec![10, 11, 12],
        "same tree order, new ids"
    );
    assert_eq!(rebuilt.focus(), 12);

    let meta2 = |id: u64| {
        let orig = order[fresh.iter().position(|&f| f == id).unwrap()];
        (Some(format!("/s{orig}")), format!("t{orig}"))
    };
    fn structural(l: &crate::restore::PaneLayout) -> crate::restore::PaneLayout {
        use crate::restore::PaneLayout::{Leaf, Split};
        match l {
            Leaf {
                cwd,
                title,
                focused,
                ..
            } => Leaf {
                cwd: cwd.clone(),
                title: title.clone(),
                focused: *focused,
                local_id: None,
            },
            Split {
                dir,
                ratio,
                first,
                second,
            } => Split {
                dir: *dir,
                ratio: *ratio,
                first: Box::new(structural(first)),
                second: Box::new(structural(second)),
            },
        }
    }
    assert_eq!(structural(&rebuilt.to_layout(&meta2)), structural(&layout));
    let rebuilt_layout = rebuilt.to_layout(&meta2);
    assert_eq!(
        rebuilt_layout.leaves()[0].local_id(),
        Some(10),
        "leaf local_id tracks the live session id"
    );
    assert_eq!(
        rebuilt_layout.first_leaf(),
        rebuilt_layout.leaves()[0],
        "first_leaf returns the first leaf in tree order without allocating"
    );
    let mut visited_leaves = Vec::new();
    rebuilt_layout.visit_leaves(&mut |leaf| visited_leaves.push(leaf));
    assert_eq!(visited_leaves, rebuilt_layout.leaves());
    let mut prefix_ids = Vec::new();
    assert!(!rebuilt_layout.all_leaves(&mut |leaf| {
        prefix_ids.push(leaf.local_id());
        leaf.local_id() != Some(11)
    }));
    assert_eq!(
        prefix_ids,
        vec![Some(10), Some(11)],
        "all_leaves short-circuits on the first false leaf"
    );

    assert!(PaneTree::rebuild(&layout, &[10, 11]).is_none());
}

#[test]
fn visit_sessions_and_matches_projected_preserve_order_and_reject_mismatches() {
    let mut tree = PaneTree::new(10);
    assert!(tree.split_focused(SplitDir::Vertical, 20));
    assert!(tree.split_focused(SplitDir::Horizontal, 30));

    let mut visited = Vec::new();
    tree.visit_sessions(&mut |sid| visited.push(sid));
    assert_eq!(visited, tree.sessions());

    let exact = tree.map_sessions(Some);
    assert!(tree.matches_projected(&exact, &|cell| cell));

    let with_native = tree.map_sessions(|sid| (sid != 30).then_some(sid));
    assert!(
        !tree.matches_projected(&with_native, &|cell| cell),
        "a non-terminal leaf (None) must not match a terminal PaneTree leaf"
    );

    let wrong_session = tree.map_sessions(|sid| Some(if sid == 20 { 99 } else { sid }));
    assert!(!tree.matches_projected(&wrong_session, &|cell| cell));

    let mut wrong_axis = exact.clone();
    if let SplitTree::Split { axis, .. } = &mut wrong_axis {
        *axis = SplitAxis::Vertical;
    }
    assert!(!tree.matches_projected(&wrong_axis, &|cell| cell));

    let mut wrong_ratio = exact;
    if let SplitTree::Split { ratio, .. } = &mut wrong_ratio {
        *ratio = 0.6;
    }
    assert!(!tree.matches_projected(&wrong_ratio, &|cell| cell));

    let single_leaf = SplitTree::Leaf(Some(10));
    assert!(!tree.matches_projected(&single_leaf, &|cell| cell));
}

/// Cmd-D vertical split: two panes side by side across a 1-cell divider column, each
/// under a header row.
#[test]
fn vertical_split_geometry() {
    let mut t = PaneTree::new(1);
    assert!(t.split_focused(SplitDir::Vertical, 2));
    assert_eq!(t.focus(), 2, "focus follows the new pane");
    let mut rects = t.compute_layout(24, 80);
    rects.sort_by_key(|r| r.col_off);
    assert_eq!(rects.len(), 2);
    assert_eq!(
        rects[0],
        PaneRect {
            session: 1,
            row_off: 1,
            col_off: 0,
            rows: 23,
            cols: 40
        }
    );
    assert_eq!(
        rects[1],
        PaneRect {
            session: 2,
            row_off: 1,
            col_off: 41,
            rows: 23,
            cols: 39
        }
    );
    assert!(
        rects
            .iter()
            .all(|r| !(r.col_off..r.col_off + r.cols).contains(&40))
    );
}

/// Cmd-Shift-D horizontal split: two full-width panes stacked over a 1-cell divider row,
/// each under a header row.
#[test]
fn horizontal_split_geometry() {
    let mut t = PaneTree::new(1);
    assert!(t.split_focused(SplitDir::Horizontal, 2));
    let mut rects = t.compute_layout(24, 80);
    rects.sort_by_key(|r| r.row_off);
    assert_eq!(rects.len(), 2);
    assert_eq!(
        rects[0],
        PaneRect {
            session: 1,
            row_off: 1,
            col_off: 0,
            rows: 11,
            cols: 80
        }
    );
    assert_eq!(
        rects[1],
        PaneRect {
            session: 2,
            row_off: 14,
            col_off: 0,
            rows: 10,
            cols: 80
        }
    );
    assert!(
        rects
            .iter()
            .all(|r| !(r.row_off..r.row_off + r.rows).contains(&12))
    );
}

/// A 2x2 golden: vertical split, then horizontally split the (focused) right
/// pane.
#[test]
fn nested_2x2_layout() {
    let mut t = PaneTree::new(1);
    assert!(t.split_focused(SplitDir::Vertical, 2)); // focus → 2 (right)
    assert!(t.split_focused(SplitDir::Horizontal, 3)); // split right → top 2 / bottom 3
    assert_eq!(t.len(), 3, "1 | (2 / 3) — three panes");
    assert_eq!(t.sessions(), vec![1, 2, 3]);
    let rects = t.compute_layout(24, 80);
    assert_eq!(rects.len(), 3);
    let left = rects.iter().find(|r| r.session == 1).unwrap();
    assert_eq!(left.rows, 23);
    let top = rects.iter().find(|r| r.session == 2).unwrap();
    let bot = rects.iter().find(|r| r.session == 3).unwrap();
    assert_eq!(top.col_off, bot.col_off);
    assert_eq!(top.cols, bot.cols);
    assert!(top.row_off < bot.row_off);
    assert!(rects_disjoint(&rects));
}

/// Directional focus over the 2x2 golden `1 | (2 / 3)`: from each pane, the
/// neighbor in each direction is the adjacent pane (or None at an edge).
#[test]
fn focus_neighbor_directions() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
    t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3), focus 3 (bottom-right)
    assert_eq!(t.focus(), 3);
    assert_eq!(t.focus_neighbor(FocusDir::Up, 24, 80), Some(2));
    assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), Some(1));
    assert_eq!(t.focus_neighbor(FocusDir::Right, 24, 80), None);
    assert_eq!(t.focus_neighbor(FocusDir::Down, 24, 80), None);
    assert!(t.set_focus(2));
    assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), Some(1));
    assert_eq!(t.focus_neighbor(FocusDir::Down, 24, 80), Some(3));
    assert_eq!(t.focus_neighbor(FocusDir::Up, 24, 80), None);
    assert_eq!(t.focus_neighbor(FocusDir::Right, 24, 80), None);
    assert!(t.set_focus(1));
    assert!(matches!(
        t.focus_neighbor(FocusDir::Right, 24, 80),
        Some(2 | 3)
    ));
    assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), None);
}

/// Zoom shows only the focused pane full-window; unzoom restores; a single-pane
/// tab can't zoom; and a structural change (split) exits zoom.
#[test]
fn zoom_focused_pane_fills_and_restores() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
    assert_eq!(t.compute_layout(24, 80).len(), 2);

    assert!(t.toggle_zoom(), "zoom on (multi-pane)");
    let z = t.compute_layout(24, 80);
    assert_eq!(z.len(), 1, "zoom shows only the focused pane");
    assert_eq!(z[0].session, 2);
    assert_eq!(
        (z[0].row_off, z[0].col_off, z[0].rows, z[0].cols),
        (0, 0, 24, 80)
    );

    assert!(!t.toggle_zoom(), "toggle off");
    assert_eq!(t.compute_layout(24, 80).len(), 2, "unzoom restores layout");

    let mut s = PaneTree::new(9);
    assert!(!s.toggle_zoom());
    assert_eq!(s.compute_layout(24, 80).len(), 1);

    let mut x = PaneTree::new(1);
    x.split_focused(SplitDir::Vertical, 2);
    assert!(x.toggle_zoom());
    x.split_focused(SplitDir::Horizontal, 3);
    assert_eq!(
        x.compute_layout(24, 80).len(),
        3,
        "split exits zoom -> all panes shown"
    );
}

/// Split, then close the new pane: back to the original single pane, focused.
#[test]
fn split_then_close_round_trips() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2);
    assert_eq!(t.focus(), 2);
    let outcome = t.close_focused();
    assert_eq!(outcome, CloseOutcome::Collapsed { closed: 2 });
    assert_eq!(t.len(), 1, "collapsed back to one pane");
    assert_eq!(t.sessions(), vec![1], "the sibling survives");
    assert_eq!(t.focus(), 1, "focus re-seats on the survivor");
    assert_eq!(
        t.compute_layout(24, 80),
        PaneTree::new(1).compute_layout(24, 80)
    );
}

/// Closing the focused pane focuses the survivor that absorbs its cells, not
/// the tree's `first_leaf()`: the pane that grows into the hole takes the next
/// keystroke.
#[test]
fn close_focuses_the_pane_that_absorbs_the_freed_space() {
    fn tree(build: &dyn Fn(&mut PaneTree), focus_on: u64) -> (PaneTree, Vec<PaneRect>) {
        let mut t = PaneTree::new(1);
        build(&mut t);
        assert!(t.set_focus(focus_on), "the pane to close must exist");
        let before = t.compute_layout(40, 120);
        (t, before)
    }
    let rect = |layout: &[PaneRect], session: u64| -> PaneRect {
        *layout
            .iter()
            .find(|r| r.session == session)
            .expect("laid-out pane")
    };

    let (mut t, before) = tree(
        &|t| {
            t.split_focused(SplitDir::Vertical, 2); // 1 | 2
            t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3)
        },
        3,
    );
    let closed_rect = rect(&before, 3);
    let survivor_before = rect(&before, 2);
    assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 3 });
    assert_eq!(t.focus(), 2, "1 | (2 / 3), close 3 -> focus 2");
    let after = t.compute_layout(40, 120);
    let survivor_after = rect(&after, 2);
    assert!(
        survivor_after.rows > survivor_before.rows,
        "the pane that gets focus is the one that GREW: {survivor_before:?} -> \
         {survivor_after:?}"
    );
    assert!(
        survivor_after.row_off <= closed_rect.row_off
            && survivor_after.row_off + survivor_after.rows
                >= closed_rect.row_off + closed_rect.rows
            && survivor_after.col_off <= closed_rect.col_off,
        "and it now covers the closed pane's cells: {closed_rect:?} inside \
         {survivor_after:?}"
    );

    let (mut t, _) = tree(
        &|t| {
            t.split_focused(SplitDir::Vertical, 2); // 1 | 2
            t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3)
            assert!(t.set_focus(2));
            t.split_focused(SplitDir::Vertical, 4); // 1 | ((2 | 4) / 3)
        },
        3,
    );
    assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 3 });
    assert_eq!(
        t.focus(),
        4,
        "1 | ((2 | 4) / 3), close 3 -> focus 4, the leaf against the seam"
    );

    let (mut t, _) = tree(
        &|t| {
            t.split_focused(SplitDir::Horizontal, 2); // 1 / 2
            t.split_focused(SplitDir::Vertical, 3); // 1 / (2 | 3)
        },
        2,
    );
    assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 2 });
    assert_eq!(t.focus(), 3, "1 / (2 | 3), close 2 -> focus 3");

    let (mut t, _) = tree(
        &|t| {
            t.split_focused(SplitDir::Vertical, 2); // 1 | 2
            assert!(t.set_focus(1));
            t.split_focused(SplitDir::Horizontal, 3); // (1 / 3) | 2
        },
        3,
    );
    assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 3 });
    assert_eq!(t.focus(), 1, "(1 / 3) | 2, close 3 -> focus 1");
}

/// Closing a background pane (reader EOF) collapses it without taking focus.
#[test]
fn close_background_pane_keeps_focus() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
    assert!(t.set_focus(1));
    let outcome = t.close_pane(2);
    assert_eq!(outcome, CloseOutcome::Collapsed { closed: 2 });
    assert_eq!(t.sessions(), vec![1]);
    assert_eq!(t.focus(), 1, "focus stays on the pane the user is using");
}

/// Closing the last pane reports `LastPane` (the tab closes), not a collapse.
#[test]
fn close_last_pane_signals_tab_close() {
    let mut t = PaneTree::new(9);
    let outcome = t.close_focused();
    assert_eq!(outcome, CloseOutcome::LastPane { closed: 9 });
    assert_eq!(outcome.closed(), 9);
}

/// `set_focus` moves focus to a live leaf and rejects an unknown id.
#[test]
fn set_focus_follows_live_leaves() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2);
    assert!(t.set_focus(1));
    assert_eq!(t.focus(), 1);
    assert!(!t.set_focus(999), "unknown id is rejected, focus unchanged");
    assert_eq!(t.focus(), 1);
}

/// An odd width: 81 cols → 80 splittable → 40 | 40, divider at col 40.
#[test]
fn vertical_split_odd_width() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2);
    let mut rects = t.compute_layout(24, 81);
    rects.sort_by_key(|r| r.col_off);
    assert_eq!(rects[0].cols, 40);
    assert_eq!(rects[1].col_off, 41);
    assert_eq!(rects[1].cols, 40);
}

#[test]
fn subtab_content_geometry_matches_the_canonical_plan() {
    use crate::tab_model::{LeafSizing, LogicalSize, Tab, TabId, TabPresentation, ViewId};

    let mut tree = PaneTree::new(1);
    tree.split_focused(SplitDir::Vertical, 2);
    tree.split_focused(SplitDir::Horizontal, 3);
    let tab = Tab::from_root(
        TabId::from_stored(1),
        tree.map_sessions(ViewId::from_stored),
        ViewId::from_stored(tree.focus()),
        false,
        TabPresentation::terminal("shell"),
    );
    for rows in 4..=48 {
        for cols in [8, 33, 80, 121] {
            let mut plan = tab.visible_plan(
                LogicalRect::new(0.0, 0.0, f32::from(cols), f32::from(rows)),
                1.0,
                |_| LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0)),
            );
            plan.reserve_pane_headers();
            for (body, leaf) in tree.compute_layout(rows, cols).iter().zip(&plan.leaves) {
                assert_eq!(
                    LogicalRect::new(
                        f32::from(body.col_off),
                        f32::from(body.row_off),
                        f32::from(body.cols),
                        f32::from(body.rows),
                    ),
                    leaf.rect,
                    "canonical sizing/input and compatibility blits must agree at {cols}x{rows}"
                );
            }
        }
    }
}

/// A hit on a vertical split's divider column yields a `DividerHit`, and
/// `set_divider_ratio` moves the boundary.
#[test]
fn vertical_divider_drag_moves_boundary() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2); // 80 cols -> 40 | divider(40) | 39
    let hit = t.divider_at(5, 40, 24, 80).expect("divider hit at col 40");
    assert_eq!(hit.dir, SplitDir::Vertical);
    assert!(t.divider_at(5, 10, 24, 80).is_none());
    assert!(t.divider_at(5, 60, 24, 80).is_none());
    let ratio = t.ratio_for_pointer(&hit, 5, 20).expect("ratio for pointer");
    assert!((ratio - 20.0 / 79.0).abs() < 1e-3, "ratio {ratio}");
    assert!(t.set_divider_ratio(&hit, ratio));
    let mut rects = t.compute_layout(24, 80);
    rects.sort_by_key(|r| r.col_off);
    assert_eq!(rects[0].session, 1);
    assert_eq!(rects[0].cols, 20);
    assert_eq!(rects[1].col_off, 21);
    assert_eq!(rects[1].cols, 59); // 79 splittable - 20 first
}

/// `set_divider_ratio` clamps to `[MIN_RATIO, MAX_RATIO]`: dragging the divider
/// to (or past) an edge never collapses a pane to zero.
#[test]
fn divider_ratio_clamps() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2);
    let hit = t.divider_at(5, 40, 24, 80).expect("divider hit");
    assert!(t.set_divider_ratio(&hit, 0.0));
    for r in t.compute_layout(24, 80) {
        assert!(r.cols >= 1, "no zero-width pane after min clamp: {r:?}");
    }
    let left = t
        .compute_layout(24, 80)
        .into_iter()
        .min_by_key(|r| r.col_off)
        .unwrap();
    assert!(left.cols >= (MIN_RATIO * 79.0).floor() as u16);
    assert!(t.set_divider_ratio(&hit, 1.0));
    for r in t.compute_layout(24, 80) {
        assert!(r.cols >= 1, "no zero-width pane after max clamp: {r:?}");
    }
}

/// Headless hit-test → ratio mapping over a horizontal split: the divider row is
/// found and a pointer maps to the proportional ratio along the rows.
#[test]
fn horizontal_divider_hit_and_ratio() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Horizontal, 2); // 24 rows -> 12 | divider(12) | 11
    let hit = t.divider_at(12, 30, 24, 80).expect("divider hit at row 12");
    assert_eq!(hit.dir, SplitDir::Horizontal);
    assert!(t.divider_at(3, 30, 24, 80).is_none());
    let ratio = t.ratio_for_pointer(&hit, 6, 30).expect("ratio");
    assert!((ratio - 6.0 / 23.0).abs() < 1e-3, "ratio {ratio}");
    assert!(t.set_divider_ratio(&hit, ratio));
    let mut rects = t.compute_layout(24, 80);
    rects.sort_by_key(|r| r.row_off);
    assert_eq!(rects[0].rows, 5);
    assert_eq!(rects[1].row_off, 8);
}

/// In a nested tree the hit-test targets the inner split: dragging its divider
/// leaves the outer split untouched.
#[test]
fn nested_divider_targets_inner_split() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2);
    t.split_focused(SplitDir::Horizontal, 3);
    let rects = t.compute_layout(24, 80);
    let top = rects.iter().find(|r| r.session == 2).unwrap();
    let bot = rects.iter().find(|r| r.session == 3).unwrap();
    let div_row = top.row_off + top.rows;
    let probe_col = top.col_off + 1;
    let hit = t
        .divider_at(div_row, probe_col, 24, 80)
        .expect("inner divider hit");
    assert_eq!(hit.dir, SplitDir::Horizontal, "inner split is horizontal");
    let left_before = rects.iter().find(|r| r.session == 1).unwrap().cols;
    let ratio = t
        .ratio_for_pointer(&hit, top.row_off + 2, probe_col)
        .unwrap();
    assert!(t.set_divider_ratio(&hit, ratio));
    let after = t.compute_layout(24, 80);
    let left_after = after.iter().find(|r| r.session == 1).unwrap().cols;
    assert_eq!(left_before, left_after, "outer split untouched");
    let top_after = after.iter().find(|r| r.session == 2).unwrap();
    assert_ne!(top_after.rows, top.rows, "inner first child resized");
    assert_eq!(after.len(), 3);
    assert!(rects_disjoint(&after));
    let _ = bot;
}

/// A single-pane (and a zoomed) tab has no draggable divider.
#[test]
fn no_divider_when_single_or_zoomed() {
    let t = PaneTree::new(1);
    assert!(
        t.divider_at(5, 5, 24, 80).is_none(),
        "single pane: no divider"
    );
    let mut z = PaneTree::new(1);
    z.split_focused(SplitDir::Vertical, 2);
    assert!(
        z.divider_at(5, 40, 24, 80).is_some(),
        "split: has a divider"
    );
    z.toggle_zoom();
    assert!(
        z.divider_at(5, 40, 24, 80).is_none(),
        "zoomed: focused pane fills window, no divider"
    );
}

/// A stale `DividerHit` whose path no longer names a split is a safe no-op
/// (`set_divider_ratio` returns false, the tree is untouched).
#[test]
fn stale_divider_hit_is_noop() {
    let mut t = PaneTree::new(1);
    t.split_focused(SplitDir::Vertical, 2);
    let hit = t.divider_at(5, 40, 24, 80).unwrap();
    t.close_pane(2);
    let before = t.compute_layout(24, 80);
    assert!(!t.set_divider_ratio(&hit, 0.3), "stale path → no write");
    assert_eq!(before, t.compute_layout(24, 80), "tree untouched");
}

/// [`split_fits_in`]'s closed form agrees with [`PaneTree::compute_layout`]
/// over every window size in a sweep, both directions: `true` iff both panes
/// of a real split clear the minimum.
#[test]
fn pane_tree_min_fit_matches_the_layout_engine() {
    let mut checked_true = 0u32;
    let mut checked_false = 0u32;
    for rows in 0u16..=48 {
        for cols in 0u16..=120 {
            for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
                let mut t = PaneTree::new(1);
                assert!(t.split_focused(dir, 2));
                let rects = t.compute_layout(rows, cols);
                let engine_ok = rects
                    .iter()
                    .all(|r| r.rows >= MIN_PANE_ROWS && r.cols >= MIN_PANE_COLS);
                let predicted = split_fits_in(dir, rows.max(1), cols.max(1));
                assert_eq!(
                    predicted, engine_ok,
                    "{dir:?} split of {cols}x{rows}: predicate said {predicted}, \
                     layout produced {rects:?}"
                );
                if predicted {
                    checked_true += 1;
                } else {
                    checked_false += 1;
                }
            }
        }
    }
    assert!(
        checked_true > 0 && checked_false > 0,
        "sweep covers both verdicts"
    );
}

/// The exact boundary: a left/right split needs `2 * MIN_PANE_COLS + 1` columns (two panes
/// plus the divider) and `MIN_PANE_ROWS + 1` rows (content plus each child's header row).
#[test]
fn split_fit_boundary_is_two_panes_plus_the_divider() {
    let pane_rows = MIN_PANE_ROWS + 1;
    let need = 2 * MIN_PANE_COLS + 1;
    assert!(split_fits_in(SplitDir::Vertical, pane_rows, need));
    assert!(!split_fits_in(SplitDir::Vertical, pane_rows, need - 1));
    assert!(
        !split_fits_in(SplitDir::Vertical, pane_rows - 1, need),
        "the undivided axis must already clear the minimum"
    );
    let need_rows = 2 * pane_rows + 1;
    assert!(split_fits_in(
        SplitDir::Horizontal,
        need_rows,
        MIN_PANE_COLS
    ));
    assert!(!split_fits_in(
        SplitDir::Horizontal,
        need_rows - 1,
        MIN_PANE_COLS
    ));
    assert!(!split_fits_in(
        SplitDir::Horizontal,
        need_rows,
        MIN_PANE_COLS - 1
    ));
    assert!(!split_fits_in(SplitDir::Vertical, 0, 0));
    assert!(!split_fits_in(SplitDir::Horizontal, 0, 0));
    assert_eq!(split_needs(SplitDir::Vertical), (pane_rows, need));
    assert_eq!(
        split_needs(SplitDir::Horizontal),
        (need_rows, MIN_PANE_COLS)
    );
}

/// [`PaneTree::focused_rect`] reports the unzoomed rect while zoomed: a split
/// clears zoom and divides that rect, as the split's two children show.
#[test]
fn focused_rect_ignores_zoom_because_splitting_unzooms() {
    let mut t = PaneTree::new(1);
    assert!(t.split_focused(SplitDir::Vertical, 2)); // 40 -> 20 | div | 19
    let unzoomed = t.focused_rect(24, 40).expect("focused leaf is in the tree");
    assert_eq!(
        (unzoomed.col_off, unzoomed.cols, unzoomed.rows),
        (21, 19, 24),
        "{unzoomed:?}"
    );

    assert!(t.toggle_zoom(), "the split tab zooms");
    assert_eq!(
        t.compute_layout(24, 40),
        vec![PaneRect {
            session: 2,
            row_off: 0,
            col_off: 0,
            rows: 24,
            cols: 40,
        }],
        "a zoomed pane fills the window on screen"
    );
    assert!(split_fits_in(SplitDir::Vertical, 24, 40));

    let measured = t.focused_rect(24, 40).expect("focused leaf is in the tree");
    assert_eq!(
        measured, unzoomed,
        "zoom must not inflate the pane a split measures"
    );
    assert!(
        !split_fits_in(SplitDir::Vertical, measured.rows, measured.cols),
        "19 columns cannot hold two 16-column panes plus a divider"
    );

    assert!(t.split_focused(SplitDir::Vertical, 3));
    assert!(!t.zoomed, "a split exits zoom");
    let children: Vec<_> = t
        .compute_layout(24, 40)
        .into_iter()
        .filter(|r| r.session == 2 || r.session == 3)
        .collect();
    let left = children.iter().map(|r| r.col_off).min().expect("two kids");
    let right = children
        .iter()
        .map(|r| r.col_off + r.cols)
        .max()
        .expect("two kids");
    assert_eq!(
        (left, right - left),
        (unzoomed.col_off, unzoomed.cols),
        "the split divided the UNZOOMED rect: {children:?}"
    );
}

/// A window shrunk below what its layout needs clamps: every rect stays inside
/// the grid, and the untouched tree restores the exact geometry on regrowth.
#[test]
fn shrinking_below_the_minimum_clamps_inside_the_grid() {
    let mut t = PaneTree::new(1);
    for (i, dir) in [
        SplitDir::Vertical,
        SplitDir::Horizontal,
        SplitDir::Vertical,
        SplitDir::Horizontal,
        SplitDir::Vertical,
        SplitDir::Horizontal,
        SplitDir::Vertical,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(t.split_focused(dir, i as u64 + 2));
    }
    assert_eq!(t.len(), 8);
    let roomy = t.compute_layout(48, 200);

    for rows in 0u16..=8 {
        for cols in 0u16..=8 {
            let win_rows = rows.max(1);
            let win_cols = cols.max(1);
            let rects = t.compute_layout(rows, cols);
            assert_eq!(rects.len(), 8, "shrinking never drops a pane");
            for r in &rects {
                assert!(r.rows >= 1 && r.cols >= 1, "no 0-extent pane: {r:?}");
                assert!(
                    r.row_off < win_rows && r.col_off < win_cols,
                    "origin inside the {win_cols}x{win_rows} grid: {r:?}"
                );
                assert!(
                    r.row_off + r.rows <= win_rows && r.col_off + r.cols <= win_cols,
                    "extent inside the {win_cols}x{win_rows} grid: {r:?}"
                );
            }
        }
    }

    assert_eq!(
        t.compute_layout(48, 200),
        roomy,
        "shrink → grow round-trips"
    );
    assert_eq!(t.len(), 8, "and no pane was collapsed on the way");
}

/// Whether no two rects claim the same cell (dividers belong to neither).
fn rects_disjoint(rects: &[PaneRect]) -> bool {
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let rows_overlap = a.row_off < b.row_off + b.rows && b.row_off < a.row_off + a.rows;
            let cols_overlap = a.col_off < b.col_off + b.cols && b.col_off < a.col_off + a.cols;
            if rows_overlap && cols_overlap {
                return false;
            }
        }
    }
    true
}
