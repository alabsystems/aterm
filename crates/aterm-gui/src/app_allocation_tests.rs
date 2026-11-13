// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn allocation_open_cancels_a_deferred_last_tab_window_close() {
    for split in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal_view = app.windows[&wid].tab_set.active().unwrap().focus;
        let progress = {
            let _closing = crate::session_store::CloseAttribution::enter(
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Unknown,
            );
            app.close_session_by_id(0).unwrap()
        };
        assert_eq!(progress, crate::app_tabs::CloseProgress::Deferred(wid));
        assert!(app.windows[&wid].pending_close);
        assert!(app.windows[&wid].pending_close_attribution.is_some());
        app.open_allocation(split).unwrap();
        assert!(!app.windows[&wid].pending_close);
        assert!(app.windows[&wid].pending_close_attribution.is_none());
        assert!(app.pool.get(0).is_some());
        assert!(app.view_store.get(terminal_view).is_some());
        let (instance, _) = app.active_native_view(wid).unwrap();
        assert_eq!(
            app.native_runtime.app(instance).unwrap().kind(),
            AppKind::Allocation
        );
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn allocation_split_preserves_terminal_identity_and_reports_its_real_grid() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let next = app.next_session_id;
    let original_tab = app.windows[&wid].tab_set.active_id();
    app.open_allocation(true).unwrap();
    assert_eq!(
        app.next_session_id, next,
        "a native split must not spawn a PTY"
    );
    assert_eq!(app.windows[&wid].tab_set.active_id(), original_tab);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    assert_eq!(plan.leaves.len(), 2);
    let terminal = plan
        .leaves
        .iter()
        .find_map(|leaf| {
            View::terminal_session(*app.view_store.get(leaf.view)?).map(|session| (leaf, session))
        })
        .unwrap();
    let session = app.pool.get(terminal.1).unwrap();
    let term = crate::term_lock(&session.term);
    assert_eq!(
        (term.rows(), term.cols()),
        (
            (terminal.0.rect.size.height.round() as u16).max(1),
            (terminal.0.rect.size.width.round() as u16).max(1)
        )
    );
    drop(term);
    app.open_allocation(false).unwrap();
    assert_eq!(app.windows[&wid].tab_set.active_id(), original_tab);
    assert!(app.structural_invariants_ok());
}

/// A split request on a focused Settings tab opens Allocation in a tab of its own, so Settings
/// keeps the whole view its window's least size holds for it. It split the Settings tab, and
/// Settings stood in about half its least width.
#[test]
fn an_allocation_split_request_leaves_a_focused_settings_tab_whole() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let settings = app.windows[&wid].tab_set.active_id().unwrap();
    app.open_allocation(true).unwrap();
    assert_eq!(
        app.windows[&wid]
            .tab_set
            .get(settings)
            .unwrap()
            .root
            .leaves()
            .len(),
        1,
        "the Settings tab keeps its one leaf"
    );
    assert_ne!(app.windows[&wid].tab_set.active_id(), Some(settings));
    let (instance, _) = app.active_native_view(wid).unwrap();
    assert_eq!(
        app.native_runtime.app(instance).unwrap().kind(),
        AppKind::Allocation
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_narrow_allocation_request_opens_a_tab_without_splitting_the_terminal() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().cols = 20;
    let next = app.next_session_id;
    let old_tab = app.windows[&wid].tab_set.active_id().unwrap();
    app.open_allocation(true).unwrap();
    assert_eq!(app.next_session_id, next);
    assert_eq!(
        app.windows[&wid]
            .tab_set
            .get(old_tab)
            .unwrap()
            .root
            .leaves()
            .len(),
        1
    );
    assert_ne!(app.windows[&wid].tab_set.active_id(), Some(old_tab));
    assert!(app.structural_invariants_ok());
}
