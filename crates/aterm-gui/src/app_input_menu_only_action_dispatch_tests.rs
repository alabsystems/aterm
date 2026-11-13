// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::keybinding::Action;
use crate::{App, WindowId, term_lock};

fn app_with_three_matches() -> (App, WindowId) {
    let app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit one\r\nhit two\r\nhit three");
    (app, wid)
}

fn current(app: &App, wid: WindowId) -> usize {
    app.windows[&wid]
        .search
        .as_ref()
        .expect("open find bar")
        .current
}

#[test]
fn find_again_actions_step_an_open_bar_and_resume_a_closed_one() {
    let (mut app, wid) = app_with_three_matches();
    app.search_enter();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .query = "hit".into();
    app.search_recompute();
    let first = current(&app, wid);

    app.dispatch_action(wid, Action::FindNext);
    let second = current(&app, wid);
    assert_ne!(second, first, "find_next stepped the open bar");
    app.dispatch_action(wid, Action::FindPrev);
    assert_eq!(current(&app, wid), first, "find_prev stepped back");

    app.search_accept();
    assert!(
        app.windows[&wid].search.is_none(),
        "Enter closed the bar and remembered the query"
    );
    app.dispatch_action(wid, Action::FindNext);
    assert_eq!(
        app.windows[&wid]
            .search
            .as_ref()
            .expect("find_next reopened the accepted query")
            .query,
        "hit",
    );
}

#[test]
fn select_all_action_selects_the_screen_and_defers_to_an_open_find() {
    let (mut app, wid) = app_with_three_matches();
    let term = app.pool.get(0).expect("session 0").term.clone();
    assert!(
        term_lock(&term).selection_to_string().is_none(),
        "negative control: nothing selected yet"
    );
    app.dispatch_action(wid, Action::SelectAll);
    let all = term_lock(&term)
        .selection_to_string()
        .expect("select_all selected the screen");
    assert!(all.contains("hit three"), "{all:?}");

    app.search_enter();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .query = "hit one".into();
    app.search_recompute();
    let marked = term_lock(&term)
        .selection_to_string()
        .expect("the find marked its match");
    app.dispatch_action(wid, Action::SelectAll);
    assert_eq!(
        term_lock(&term).selection_to_string(),
        Some(marked),
        "select_all must not eat the find bar's match highlight",
    );
}
