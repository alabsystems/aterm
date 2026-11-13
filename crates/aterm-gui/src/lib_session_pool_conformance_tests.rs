// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_gui::session_pool_model;
use std::collections::BTreeMap;

fn project(app: &App, sid: u64) -> [i64; 2] {
    let mut refcount = 0i64;
    for window in app.windows.values() {
        for tab in window.tab_set.tabs() {
            tab.root.visit(&mut |&view| {
                if app
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    == Some(sid)
                {
                    refcount += 1;
                }
            });
        }
    }
    let closed = i64::from(app.pool.views(sid).is_none());
    [refcount, closed]
}

fn validate_transition(action_name: &str, prev: [i64; 2], next: [i64; 2]) -> (bool, String) {
    let m = session_pool_model();
    let st = |s: [i64; 2]| -> BTreeMap<&'static str, i64> {
        [("refcount", s[0]), ("closed", s[1])].into_iter().collect()
    };
    aterm_spec::verify::validate_transition_tiered(
        &m,
        &[("Cap", 1_000_000)],
        &st(prev),
        &st(next),
        Some(action_name),
        "SessionPool refcount conformance",
    )
}

#[test]
fn the_real_session_pool_conforms() {
    crate::conformance_support::on_conformance_stack("session_pool", run_conformance);
}

fn run_conformance() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-sesspool-conf-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    let app0 = App::headless_for_test();
    let init = session_pool_model().init_state();
    let model_init = [init["refcount"], init["closed"]];
    assert_eq!(
        project(&app0, 0),
        model_init,
        "fresh App pool projection must equal session_pool_model().init_state() — got {:?}, model {:?}",
        project(&app0, 0),
        model_init
    );
    assert_eq!(
        model_init,
        [1, 0],
        "sanity: model init is one viewer, not closed"
    );
    drop(app0);

    let mut app = App::headless_for_test();

    let prev = project(&app, 0);
    let new_wid = app
        .open_active_session_in_new_window_logical()
        .expect("the front window has an active session to open in a new window");
    assert_ne!(new_wid, WindowId(0));
    assert!(app.windows.contains_key(&new_wid));
    let next = project(&app, 0);
    let (ok, out) = validate_transition("Acquire", prev, next);
    assert!(
        ok,
        "real Acquire {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(
        project(&app, 0),
        next,
        "pure tab migration preserves both shared-session view edges"
    );
    assert_eq!(app.windows.len(), 1);

    let prev = next;
    let to_close = app.exit_session_logical(0);
    assert_eq!(
        to_close,
        vec![WindowId(0)],
        "the final occurrence is handed to atomic window teardown"
    );
    let next = project(&app, 0);
    let (ok, out) = validate_transition("Release", prev, next);
    assert!(
        ok,
        "real Release (survive) {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let prev = next;
    assert_eq!(
        app.close_window_logical(WindowId(0)),
        CloseOutcome::Exit,
        "closing the last viewer exits the app"
    );
    let next = project(&app, 0);
    let (ok, out) = validate_transition("Release", prev, next);
    assert!(
        ok,
        "real Release (retire) {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    assert_eq!(
        next,
        [0, 1],
        "after the last viewer leaves, session 0 is retired (refcount 0, closed)"
    );

    let (bad_ok, _bad) = validate_transition("Release", [2, 0], [1, 1]);
    assert!(
        !bad_ok,
        "corrupted Release [2,0] -> [1,1] (retire-while-viewed) MUST be ty-REJECTED — \
         the conformance would be vacuous otherwise"
    );

    let _ = std::fs::remove_dir_all(&dir);
    crate::logging::stderr_line!(
        "session_pool Tier-1 conformance: real open-in-new-window / close-window \
         attach/detach transitions conform AND the retire-while-viewed negative \
         control was rejected."
    );
}
