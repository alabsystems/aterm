// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn assert_window_keeps_rendering(app: &mut App, wid: WindowId, what: &str) {
    let unresolved: Vec<_> = app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .flat_map(|tab| tab.root.leaves())
        .filter(|view| app.view_store.get(*view).is_none())
        .collect();
    assert!(
        unresolved.is_empty(),
        "{what}: canonical tree still names dead view(s) {unresolved:?}"
    );
    let plan = app
        .active_visible_leaf_plan(wid)
        .unwrap_or_else(|| panic!("{what}: the active tab has no layout plan to paint"));
    assert!(!plan.leaves.is_empty(), "{what}: nothing to paint");
    assert!(
        app.active_visible_content_route(wid).is_some(),
        "{what}: no visible content route — redraw_window returns before any present"
    );
    assert!(
        app.structural_invariants_ok(),
        "{what}: structural invariants"
    );
}

fn pane_sessions(app: &App, wid: WindowId) -> Vec<u64> {
    app.windows[&wid].layouts[app.windows[&wid].tabs.active].sessions()
}

fn canonical_leaves(app: &App, wid: WindowId) -> Vec<crate::tab_model::ViewId> {
    app.windows[&wid]
        .tab_set
        .active()
        .expect("active tab")
        .root
        .leaves()
}

#[test]
fn closing_a_pane_keeps_the_window_rendering() {
    type Setup = fn(&mut App, WindowId) -> Vec<u64>;
    let cases: [(&str, Setup); 4] = [
        ("the focused split pane", |app, wid| {
            let closed = app.split_active_stub_tab(wid);
            assert_eq!(pane_sessions(app, wid), vec![0, closed]);
            vec![0]
        }),
        ("the unfocused original pane", |app, wid| {
            let survivor = app.split_active_stub_tab(wid);
            app.focus_pane(pane::FocusDir::Left);
            assert_eq!(app.windows[&wid].layouts[0].focus(), 0);
            vec![survivor]
        }),
        ("a pane whose sibling is a split", |app, wid| {
            let a = app.split_active_stub_tab(wid);
            let b = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
            app.focus_pane(pane::FocusDir::Left);
            assert_eq!(app.windows[&wid].layouts[0].focus(), 0);
            assert_eq!(pane_sessions(app, wid), vec![0, a, b]);
            vec![a, b]
        }),
        ("a tab's last pane beside another tab", |app, wid| {
            let second = app.next_session_id;
            app.push_stub_tab(wid, crate::stub_session(second));
            vec![0]
        }),
    ];
    for (what, setup) in cases {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let survivors = setup(&mut app, wid);
        assert_eq!(app.close_active_tab(), None, "{what}: the window stays");
        assert_eq!(pane_sessions(&app, wid), survivors, "{what}");
        assert_eq!(
            canonical_leaves(&app, wid).len(),
            survivors.len(),
            "{what}: the canonical tree shrank with the pane tree"
        );
        assert_window_keeps_rendering(&mut app, wid, what);
    }
}

#[test]
fn pane_focus_reports_only_whether_focus_actually_moved() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    for dir in [
        pane::FocusDir::Left,
        pane::FocusDir::Right,
        pane::FocusDir::Up,
        pane::FocusDir::Down,
    ] {
        assert!(
            !app.focus_pane_in(wid, dir),
            "a single-pane tab reports no move"
        );
    }

    let split = app.split_active_stub_tab(wid);
    assert_eq!(app.windows[&wid].layouts[0].focus(), split);
    assert!(
        app.focus_pane_in(wid, pane::FocusDir::Left),
        "there is a pane to the left of the new one"
    );
    assert_eq!(
        app.windows[&wid].layouts[0].focus(),
        0,
        "moved=1 means the focused pane really changed"
    );
    assert!(!app.focus_pane_in(wid, pane::FocusDir::Left));
    assert_eq!(
        app.windows[&wid].layouts[0].focus(),
        0,
        "moved=0 means the focused pane really did not change"
    );
    assert!(app.focus_pane_in(wid, pane::FocusDir::Right));
    assert_eq!(app.windows[&wid].layouts[0].focus(), split);
}

#[test]
fn a_close_that_did_not_shrink_the_tree_refuses_to_retire_the_view() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let closed = app.split_active_stub_tab(wid);
    let before = canonical_leaves(&app, wid);
    assert_eq!(before.len(), 2);

    let refused = app.apply_close_outcome(wid, 0, pane::CloseOutcome::Collapsed { closed });

    assert!(!refused, "a refused close never closes the window");
    assert_eq!(
        canonical_leaves(&app, wid),
        before,
        "topology is untouched by a close that did not happen"
    );
    assert!(
        app.pool.get(closed).is_some(),
        "the session keeps its PTY: nothing was torn down"
    );
    assert_window_keeps_rendering(&mut app, wid, "after a refused pane close");
}
