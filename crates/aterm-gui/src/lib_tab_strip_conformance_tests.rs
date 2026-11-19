// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_gui::tab_strip_model;
use std::collections::BTreeMap;

/// `[count, active, seg_count, selected]`.
pub(crate) fn project(app: &App, wid: WindowId) -> [i64; 4] {
    let Some(ws) = app.windows.get(&wid) else {
        return [0, 0, 0, 0];
    };
    let (seg_count, selected) = ws.strip_shadow.get();
    [
        ws.strip_tab_count() as i64,
        ws.selected_strip_index().map_or(-1, |i| i as i64),
        seg_count as i64,
        selected as i64,
    ]
}

fn validate_transition(action_name: &str, prev: [i64; 4], next: [i64; 4]) -> (bool, String) {
    let m = tab_strip_model();
    let st = |s: [i64; 4]| -> BTreeMap<&'static str, i64> {
        [
            ("count", s[0]),
            ("active", s[1]),
            ("seg_count", s[2]),
            ("selected", s[3]),
        ]
        .into_iter()
        .collect()
    };
    aterm_spec::verify::validate_transition_tiered(
        &m,
        &[("Cap", 1_000_000)],
        &st(prev),
        &st(next),
        Some(action_name),
        "TabStrip parity conformance",
    )
}

#[test]
fn the_real_tab_strip_conforms() {
    crate::conformance_support::on_conformance_stack("tab_strip", run_conformance);
}

fn run_conformance() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-tabstrip-conf-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    let app0 = App::headless_for_test();
    let init = tab_strip_model().init_state();
    let model_init = [
        init["count"],
        init["active"],
        init["seg_count"],
        init["selected"],
    ];
    assert_eq!(
        project(&app0, WindowId(0)),
        model_init,
        "fresh window projection must equal tab_strip_model().init_state() — got {:?}, model {:?}",
        project(&app0, WindowId(0)),
        model_init
    );
    assert_eq!(
        model_init,
        [1, 0, 1, 0],
        "sanity: one tab, selected, strip mirrored"
    );
    drop(app0);

    let mut app = App::headless_for_test();

    let prev = project(&app, WindowId(0));
    let sid1 = app.next_session_id;
    app.push_stub_tab(WindowId(0), stub_session(sid1));
    let next = project(&app, WindowId(0));
    let (ok, out) = validate_transition("NewTab", prev, next);
    assert!(
        ok,
        "real NewTab {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let prev = next;
    app.cycle_tab(true);
    let next = project(&app, WindowId(0));
    let (ok, out) = validate_transition("SelectTab", prev, next);
    assert!(
        ok,
        "real SelectTab {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let sid_b = app.next_session_id;
    let _b = app.insert_logical_window(stub_session(sid_b), 24, 80);
    assert_ne!(
        app.frontmost_window,
        Some(WindowId(0)),
        "WindowId(0) is now non-front"
    );
    let prev = project(&app, WindowId(0));
    assert!(
        !app.close_tab_at(WindowId(0), 1),
        "closing a non-last tab does not signal window-close"
    );
    let next = project(&app, WindowId(0));
    let (ok, out) = validate_transition("Close", prev, next);
    assert!(
        ok,
        "real Close in a NON-FRONT window {prev:?} -> {next:?} must conform — a stale strip \
         (seg_count past the new tab count) would FAIL here\n--- ty ---\n{out}"
    );
    assert_eq!(
        next[0], next[2],
        "after a non-front close, seg_count must mirror the tab count"
    );

    let (bad_ok, _bad) = validate_transition("Close", [2, 1, 2, 1], [1, 0, 2, 1]);
    assert!(
        !bad_ok,
        "corrupted Close [2,1,2,1] -> [1,0,2,1] (stale strip) MUST be ty-REJECTED — \
         the conformance would be vacuous otherwise"
    );

    let _ = std::fs::remove_dir_all(&dir);
    crate::logging::stderr_line!(
        "tab_strip Tier-1 conformance: real NewTab/SelectTab/Close (incl. a NON-FRONT \
         close) keep the native strip in lockstep with the tab model AND the stale-strip \
         negative control was rejected."
    );
}

#[test]
fn a_solo_session_keeps_its_editable_title_in_both_windows_and_only_the_master_offers_new_tab() {
    let mut app = App::headless_for_test();
    app.tab_strip_rows = 1;
    let master = WindowId(0);
    let aux = app.insert_logical_window(stub_session(app.next_session_id), 24, 80);
    for wid in [master, aux] {
        app.splice_tab_strip(wid);
        let ws = &app.windows[&wid];
        let segments = &ws.tab_segments;
        assert!(
            segments
                .iter()
                .any(|s| s.kind == tab_bar::TabHit::Select(0))
        );
        assert_eq!(
            segments.iter().any(|s| s.kind == tab_bar::TabHit::NewTab),
            wid == master
        );
        let tab = ws.tab_set.active_id().unwrap();
        assert!(app.begin_session_rename(wid, tab));
        assert!(!app.inline_rename_edit(wid).unwrap().text.is_empty());
        app.end_session_rename_editor(wid);
    }
    app.close_window_logical(master);
    app.splice_tab_strip(aux);
    assert!(
        app.windows[&aux]
            .tab_segments
            .iter()
            .any(|s| s.kind == tab_bar::TabHit::NewTab)
    );
}

#[test]
fn a_tool_between_sessions_leaves_shortcuts_and_stale_pointer_hits_bound_to_the_sessions() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let tool = app.windows[&wid].tab_set.active_id().unwrap();
    let session = app.next_session_id;
    app.push_stub_tab(wid, stub_session(session));
    app.splice_tab_strip(wid);
    let ws = &app.windows[&wid];
    assert_eq!(ws.strip_index_to_tab_index(1), Some(2));
    let session_tab = ws.tab_set.active_id().unwrap();
    let second = ws
        .tab_segments
        .iter()
        .find(|seg| seg.kind == tab_bar::TabHit::Select(1))
        .unwrap()
        .start_col
        + 1;
    app.switch_shown_tab_in(wid, 0);
    app.switch_shown_tab_in(wid, 1);
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(session_tab));
    app.move_tab(wid, 2, 0);
    let tool_index = app.tab_index_for_id(wid, tool).unwrap();
    app.switch_tab_in(wid, tool_index);
    assert_eq!(app.windows[&wid].selected_strip_index(), None);
    app.handle_tab_strip_click(wid, second, std::time::Instant::now());
    assert_eq!(app.windows[&wid].tab_set.active_id(), Some(session_tab));
}
