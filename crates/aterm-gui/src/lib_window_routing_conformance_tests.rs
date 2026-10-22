// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_session::window_routing_model;
use std::collections::BTreeMap;

/// The real `App` projected onto `[win_count, frontmost, next_id, exited]`. `frontmost`
/// and `next_id` are shifted by one, since the model reserves `0` for "no frontmost
/// window" (`FrontmostLive`, `FrontmostAllocated`).
fn project(app: &App, exited: bool) -> [i64; 4] {
    let win_count = app.windows.len() as i64;
    let frontmost = app.frontmost_window.map_or(0, |WindowId(n)| n as i64 + 1);
    let next_id = app.next_window_id as i64 + 1;
    [win_count, frontmost, next_id, exited as i64]
}

/// Validate one real transition against `window_routing_model()`; returns (conforms, output).
/// Tiered (`verify::validate_transition_tiered`): the in-process `Next` check always runs,
/// `ty trace validate` too where installed, and the verdicts must agree. `MaxWin`/`MaxId` are
/// raised so a real multi-window run's guards never spuriously reject; `Buggy` stays 0.
fn validate_transition(action_name: &str, prev: [i64; 4], next: [i64; 4]) -> (bool, String) {
    let m = window_routing_model();
    let st = |s: [i64; 4]| -> BTreeMap<&'static str, i64> {
        [
            ("win_count", s[0]),
            ("frontmost", s[1]),
            ("next_id", s[2]),
            ("exited", s[3]),
        ]
        .into_iter()
        .collect()
    };
    aterm_spec::verify::validate_transition_tiered(
        &m,
        &[("MaxWin", 1_000_000), ("MaxId", 1_000_000_000)],
        &st(prev),
        &st(next),
        Some(action_name),
        "App window-routing conformance",
    )
}

/// The window-routing Tier-1 body: if the real close→exit routing diverges from
/// `WindowRouting.Next`, this fails.
#[test]
fn the_real_window_routing_conforms() {
    crate::conformance_support::on_conformance_stack("window_routing", run_conformance);
}

fn run_conformance() {
    // A scratch dir per call, not per process: other callers in this test binary share the
    // pid and would race on the spec, cfg and trace files.
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-winroute-conf-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    // Projection-drift guard: the headless projection must equal the model's `init_state`.
    let app0 = App::headless_for_test();
    let init = window_routing_model().init_state();
    let model_init = [
        init["win_count"],
        init["frontmost"],
        init["next_id"],
        init["exited"],
    ];
    assert_eq!(
        project(&app0, false),
        model_init,
        "headless App initial projection must equal window_routing_model().init_state() \
         — got {:?}, model init {:?} (the +1 frontmost/next_id remap is load-bearing)",
        project(&app0, false),
        model_init
    );
    assert_eq!(
        model_init,
        [1, 1, 2, 0],
        "sanity: model init is the documented tuple"
    );
    drop(app0);

    // A real create/raise/close sequence through the production seams, each transition
    // validated against the derived `Next`.
    let mut app = App::headless_for_test();
    let mut exited = false;
    let mut validated = 0usize;

    // CreateWindow: a 2nd window.
    let prev = project(&app, exited);
    let sid1 = app.next_session_id;
    let _wid1 = app.insert_logical_window(stub_session(sid1), 24, 80);
    assert!(
        app.structural_invariants_ok(),
        "invariants hold after 1st create"
    );
    let next = project(&app, false);
    let (ok, out) = validate_transition("CreateWindow", prev, next);
    assert!(
        ok,
        "real CreateWindow {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // CreateWindow: a 3rd window (a strictly greater id, never reused).
    let prev = next;
    let sid2 = app.next_session_id;
    let _wid2 = app.insert_logical_window(stub_session(sid2), 24, 80);
    assert!(
        app.structural_invariants_ok(),
        "invariants hold after 2nd create"
    );
    let next = project(&app, false);
    let (ok, out) = validate_transition("CreateWindow", prev, next);
    assert!(
        ok,
        "real CreateWindow {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // ConnectedCreateWindow: a fourth window validated against the connections mint arm,
    // through the logical install a window's landing ends in (`install_window_state`);
    // the hosted-origin guard (`win_count > 0`) holds with three live windows. The real
    // connected window's booking and landing are validated below; origin resolution is
    // pinned by `connected_spawn_tests`; negative control (d) keeps this non-vacuous.
    let prev = next;
    let sid3 = app.next_session_id;
    let _wid3 = app.insert_logical_window(stub_session(sid3), 24, 80);
    assert!(
        app.structural_invariants_ok(),
        "invariants hold after the connected window mint"
    );
    let next = project(&app, false);
    let (ok, out) = validate_transition("ConnectedCreateWindow", prev, next);
    assert!(
        ok,
        "real connected window mint {prev:?} -> {next:?} must conform to \
         ConnectedCreateWindow\n--- ty ---\n{out}"
    );
    validated += 1;

    // A window a person opens (Cmd-N, the menu, a picker row, `spawn connected=
    // place=window`) is spawned off the loop (`tab_spawn`): the turn books it,
    // minting its id, and the landing makes it frontmost. The pair is one
    // create, validated across both through the real booking
    // (`App::reserve_window`) and landing (`App::install_spawned`, the inert OS
    // arm). Negative control (e) keeps it non-vacuous.
    let mut next = next;
    for action in ["CreateWindow", "ConnectedCreateWindow"] {
        let prev = next;
        let (sid, wid) = crate::tab_spawn::test_support::book_window(
            &mut app,
            None,
            crate::tab_spawn::SpawnAdmission::Plain,
            crate::tab_spawn::TabSpawnThen::Nothing,
        );
        assert!(
            app.install_spawned_tab(crate::tab_spawn::TabSpawned {
                id: sid,
                window: wid,
                session: Ok(stub_session(sid)),
                reader_gate: None,
            }),
            "the booked window lands"
        );
        assert!(
            app.structural_invariants_ok(),
            "invariants hold after a window lands"
        );
        next = project(&app, false);
        let (ok, out) = validate_transition(action, prev, next);
        assert!(
            ok,
            "real off-loop window booking + landing {prev:?} -> {next:?} must conform \
             to {action}\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    // RaiseSession through the real `raise_session_by_id`. Headless there is no OS focus, so
    // the step may be the admitted `frontmost' = frontmost` stutter; what is validated is that
    // a raise never mints, destroys or exits. Negative control (c) keeps it non-vacuous.
    let prev = next;
    app.raise_session_by_id(0)
        .expect("session 0 is hosted by the first window");
    let next = project(&app, false);
    let (ok, out) = validate_transition("RaiseSession", prev, next);
    assert!(
        ok,
        "real RaiseSession {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // CloseWindow: a non-last window (WindowId(0)); the app stays.
    let prev = next;
    let outcome = app.close_window_logical(WindowId(0));
    assert_eq!(
        outcome,
        CloseOutcome::Stay,
        "closing a non-last window keeps the app"
    );
    assert!(
        app.structural_invariants_ok(),
        "invariants hold after a non-last close"
    );
    let next = project(&app, false);
    let (ok, out) = validate_transition("CloseWindow", prev, next);
    assert!(
        ok,
        "real CloseWindow (non-last) {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // CloseWindow on the frontmost with a lower-id survivor: the app re-points to the lowest
    // live id, which the model's nondeterministic re-point admits.
    if app.windows.len() >= 2 {
        let prev = project(&app, exited);
        let front = app
            .frontmost_window
            .expect("a frontmost window exists with >=2 windows");
        let outcome = app.close_window_logical(front);
        assert_eq!(
            outcome,
            CloseOutcome::Stay,
            "closing the frontmost with a survivor keeps the app"
        );
        assert!(
            app.structural_invariants_ok(),
            "invariants hold after a frontmost close"
        );
        let next = project(&app, exited);
        let (ok, out) = validate_transition("CloseWindow", prev, next);
        assert!(
            ok,
            "real CloseWindow (frontmost, survivor remains) {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }

    // Close down to the last window, lowest id first. `structural_invariants_ok` needs a
    // frontmost, so it is asserted only after creates.
    while let Some(&wid) = app.windows.keys().next() {
        let prev = project(&app, exited);
        let outcome = app.close_window_logical(wid);
        if matches!(outcome, CloseOutcome::Exit) {
            exited = true;
        }
        let next = project(&app, exited);
        let (ok, out) = validate_transition("CloseWindow", prev, next);
        assert!(
            ok,
            "real CloseWindow {prev:?} -> {next:?} (outcome {outcome:?}) must conform\n--- ty ---\n{out}"
        );
        validated += 1;
    }
    assert!(app.windows.is_empty(), "every window closed");
    assert!(exited, "closing the last window set exited (ExitIffEmpty)");

    // Negative controls: each must be rejected, or the binding proves nothing.
    // (a) Missed exit: closing the last window while `exited=0` (the `Buggy=1` defect).
    let prev_missed = [1, 1, 2, 0];
    let next_missed = [0, 0, 2, 0]; // win_count 1->0 but exited stays 0
    let (ok, o) = validate_transition("CloseWindow", prev_missed, next_missed);
    assert!(
        !ok,
        "NEGATIVE CONTROL (missed exit) {prev_missed:?} -> {next_missed:?} MUST be rejected \
         — a CloseWindow to an empty set without exiting is the Buggy defect\n--- ty ---\n{o}"
    );

    // (b) Early exit: `exited=1` while a window remains (`ExitIffEmpty`).
    let prev_early = [2, 1, 3, 0];
    let next_early = [1, 1, 3, 1]; // win_count 2->1 (survivor) but exited flipped to 1
    let (ok, o) = validate_transition("CloseWindow", prev_early, next_early);
    assert!(
        !ok,
        "NEGATIVE CONTROL (early exit) {prev_early:?} -> {next_early:?} MUST be rejected \
         — exiting while a window remains violates ExitIffEmpty\n--- ty ---\n{o}"
    );

    // (c) Raise to an unallocated id (`frontmost' = next_id`): `FrontmostAllocated` forbids it.
    let prev_raise = [2, 1, 3, 0];
    let next_raise = [2, 3, 3, 0]; // frontmost -> 3 = next_id (unallocated)
    let (ok, o) = validate_transition("RaiseSession", prev_raise, next_raise);
    assert!(
        !ok,
        "NEGATIVE CONTROL (raise to unallocated) {prev_raise:?} -> {next_raise:?} MUST be \
         rejected — frontmost may never re-point to a future/unallocated id\n--- ty ---\n{o}"
    );

    // (d) Connected mint after exit: the hosted-origin guard forbids it.
    let prev_dead = [0, 0, 2, 1];
    let next_dead = [1, 2, 3, 1]; // a mint out of the exited state
    let (ok, o) = validate_transition("ConnectedCreateWindow", prev_dead, next_dead);
    assert!(
        !ok,
        "NEGATIVE CONTROL (connected mint after exit) {prev_dead:?} -> {next_dead:?} MUST be \
         rejected — a connected window mint requires a live hosted origin\n--- ty ---\n{o}"
    );

    // (e) A window that lands without taking the front: its booking minted the
    // id, but the landing left `frontmost` where it was.
    let prev_behind = [2, 1, 3, 0];
    let next_behind = [3, 1, 4, 0];
    let (ok, o) = validate_transition("CreateWindow", prev_behind, next_behind);
    assert!(
        !ok,
        "NEGATIVE CONTROL (landed behind) {prev_behind:?} -> {next_behind:?} MUST be \
         rejected — a new window becomes frontmost\n--- ty ---\n{o}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    crate::logging::stderr_line!(
        "App window-routing Tier-1 conformance: {validated} real transitions \
         (2 CreateWindow + the ConnectedCreateWindow mint + 2 off-loop window \
         booking-and-landing pairs + a RaiseSession + the close-down-to-exit chain) \
         strictly validated against the WindowRouting spec; negative controls (missed \
         exit, early exit, raise-to-unallocated, mint-after-exit, landed-behind) all \
         rejected."
    );
}
