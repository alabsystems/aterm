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
        ws.tab_set.shown_len() as i64,
        ws.tab_set.strip_active().unwrap_or(0) as i64,
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
