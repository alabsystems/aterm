// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn route_follows_zoom_aware_visible_content_not_hidden_root_membership() {
    let wid = WindowId(0);

    let mut terminal = App::headless_for_test();
    assert_eq!(
        terminal.active_visible_content_route(wid),
        Some(VisibleContentRoute::Terminal { composed: false })
    );
    terminal.split_active_stub_tab(wid);
    assert_eq!(
        terminal.active_visible_content_route(wid),
        Some(VisibleContentRoute::Terminal { composed: true })
    );
    assert!(
        terminal
            .windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("terminal tab")
            .toggle_zoom()
    );
    assert_eq!(
        terminal.active_visible_content_route(wid),
        Some(VisibleContentRoute::Terminal { composed: true }),
        "a zoomed terminal split keeps the composed terminal coordinate space"
    );

    let mut native = App::headless_for_test();
    assert!(native.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, native_view) = native.active_native_view(wid).expect("native front");
    assert_eq!(
        native.active_visible_content_route(wid),
        Some(VisibleContentRoute::Native {
            instance,
            view: native_view,
        })
    );

    let (_, terminal_view) =
        native.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert_eq!(
        native.active_visible_content_route(wid),
        Some(VisibleContentRoute::Heterogeneous),
        "both visible content kinds select the retained leaf compositor"
    );
    assert!(
        native
            .windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("mixed tab")
            .toggle_zoom()
    );
    assert_eq!(
        native.active_visible_content_route(wid),
        Some(VisibleContentRoute::Terminal { composed: true }),
        "a hidden native sibling cannot divert the zoomed terminal"
    );

    assert!(
        !native
            .windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("mixed tab")
            .toggle_zoom(),
        "first toggle restores the full mixed layout"
    );
    assert!(
        native
            .windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("mixed tab")
            .set_focus(native_view)
    );
    native.sync_window(wid);
    assert!(
        native
            .windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("mixed tab")
            .toggle_zoom()
    );
    assert_eq!(
        native.active_visible_content_route(wid),
        Some(VisibleContentRoute::Native {
            instance,
            view: native_view,
        }),
        "a hidden terminal sibling cannot divert the zoomed native view"
    );
    assert_ne!(native_view, terminal_view);

    let stale_view = native.windows[&wid]
        .tab_set
        .active()
        .expect("mixed tab")
        .focus;
    assert!(native.view_store.remove(stale_view).is_some());
    assert_eq!(
        native.active_visible_content_route(wid),
        None,
        "a stale visible identity fails closed"
    );
}

#[test]
fn zoom_hidden_terminal_is_not_displayed_until_unzoom() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let focused = app.split_active_stub_tab(wid);
    let hidden = 0;

    assert!(active_tab_displays_session(
        &app.windows[&wid],
        &app.view_store,
        hidden,
    ));
    assert!(active_tab_displays_session(
        &app.windows[&wid],
        &app.view_store,
        focused,
    ));
    assert_eq!(
        app.windows_displaying(hidden).collect::<Vec<_>>(),
        vec![wid]
    );
    assert!(app.is_visible_session(hidden));

    assert!(
        app.windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("split tab")
            .toggle_zoom()
    );
    assert!(!active_tab_displays_session(
        &app.windows[&wid],
        &app.view_store,
        hidden,
    ));
    assert!(active_tab_displays_session(
        &app.windows[&wid],
        &app.view_store,
        focused,
    ));
    assert!(
        app.windows_displaying(hidden).next().is_none(),
        "a zoom-hidden sibling is not an image/output viewer"
    );
    assert!(!app.is_visible_session(hidden));
    assert_eq!(
        app.windows_displaying(focused).collect::<Vec<_>>(),
        vec![wid]
    );

    assert!(
        !app.windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("split tab")
            .toggle_zoom()
    );
    assert_eq!(
        app.windows_displaying(hidden).collect::<Vec<_>>(),
        vec![wid]
    );
    assert!(app.is_visible_session(hidden));
}

#[test]
fn output_redraw_admission_stamps_headless_co_viewers_and_skips_background_tabs() {
    let mut app = App::headless_for_test();
    let original = WindowId(0);
    let viewer = app
        .open_active_session_in_new_window_logical()
        .expect("session can be shared into a second window");
    let first = Instant::now();

    assert!(app.admit_output_redraws(0, first));
    for wid in [original, viewer] {
        let ws = &app.windows[&wid];
        assert_eq!(ws.pending_deco_birth, Some(first));
        assert!(
            ws.os_window.is_none(),
            "the test must exercise headless viewers"
        );
        assert!(
            !ws.content_pending,
            "no OS surface can have a pending frame"
        );
    }

    // The original window switches to a background tab, while the second
    // window still displays the same session. Only that viewer receives the
    // next output stamp; visibility remains true for title/search gating.
    let sid = app.next_session_id;
    app.push_stub_tab(original, stub_session(sid));
    for ws in app.windows.values_mut() {
        ws.pending_deco_birth = None;
    }
    let second = Instant::now();
    assert!(app.admit_output_redraws(0, second));
    assert_eq!(app.windows[&original].pending_deco_birth, None);
    assert_eq!(app.windows[&viewer].pending_deco_birth, Some(second));

    // Once both copies are background, optional visible-only work can be
    // skipped, and neither window is marked for an output-driven present.
    let sid = app.next_session_id;
    app.push_stub_tab(viewer, stub_session(sid));
    app.windows.get_mut(&viewer).unwrap().pending_deco_birth = None;
    assert!(!app.admit_output_redraws(0, Instant::now()));
    assert_eq!(app.windows[&original].pending_deco_birth, None);
    assert_eq!(app.windows[&viewer].pending_deco_birth, None);
}

#[test]
fn output_redraw_admission_obeys_zoom_and_stamps_before_os_window_check() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let focused = app.split_active_stub_tab(wid);
    let hidden = 0;
    assert!(
        app.windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("split tab")
            .toggle_zoom()
    );

    assert!(!app.admit_output_redraws(hidden, Instant::now()));
    assert_eq!(app.windows[&wid].pending_deco_birth, None);
    let first = Instant::now();
    assert!(app.admit_output_redraws(focused, first));
    assert_eq!(app.windows[&wid].pending_deco_birth, Some(first));

    assert!(
        !app.windows
            .get_mut(&wid)
            .expect("window")
            .tab_set
            .active_mut()
            .expect("split tab")
            .toggle_zoom()
    );
    let second = Instant::now();
    assert!(app.admit_output_redraws(hidden, second));
    assert_eq!(app.windows[&wid].pending_deco_birth, Some(second));
}

/// Keypress-to-redraw work is O(visible windows) at 1 and 100 background sessions, with
/// one background terminal's lock held elsewhere, through the real `on_output_wake`,
/// measured with the thread-local census:
///
/// * before redraw admission nothing walks the pool or takes a terminal lock, and
///   admission examines exactly the windows;
/// * the priming wake walks every session after admission and skips the contended one
///   without waiting (checked causally: the holder releases only when the test says);
/// * a steady keystroke wake inside the status interval walks no session and takes the
///   same number of locks at every session count.
#[test]
fn output_wake_redraw_admission_is_o_visible_windows_at_any_session_count() {
    let mut steady_locks: Option<u64> = None;
    for background in [1usize, 100] {
        let mut app = App::headless_for_test();
        let original = WindowId(0);
        let focused = 0u64;
        for _ in 0..background {
            let sid = app.next_session_id;
            app.push_stub_tab(original, stub_session(sid));
        }
        app.switch_tab_in(original, 0);
        // A second visible window co-viewing the focused session.
        let viewer = app
            .open_active_session_in_new_window_logical()
            .expect("the focused session can be shared into a second window");
        let windows = app.windows.len() as u64;
        assert_eq!(windows, 2);

        // A background session's terminal, locked on another thread for
        // the whole wake (a reader mid-batch, a control verb).
        let contended = app.pool.get(1).expect("background session").term.clone();
        let probe = contended.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _guard = contended.lock().unwrap_or_else(|p| p.into_inner());
            locked_tx.send(()).expect("test alive");
            // Released by the test, or after a bound so a regression that waits gets
            // the lock and fails the still-held check instead of hanging.
            let _ = release_rx.recv_timeout(Duration::from_secs(60));
        });
        locked_rx.recv().expect("holder took the lock");

        // Wake 1 — the status sweep is due (the pool epoch moved).
        for ws in app.windows.values_mut() {
            ws.pending_deco_birth = None;
        }
        let t0 = Instant::now();
        let (pool0, locks0, win0) = (
            wake_census::pool_visits(),
            term_lock_acquisitions_on_this_thread(),
            wake_census::admission_windows(),
        );
        app.on_output_wake(focused, t0);
        let (pool_at, locks_at) = wake_census::take_at_admission().expect("redraw admission ran");
        assert_eq!(
            pool_at - pool0,
            0,
            "{background} background sessions: no pool walk before admission"
        );
        assert_eq!(
            locks_at - locks0,
            0,
            "{background} background sessions: no terminal lock before admission"
        );
        assert_eq!(
            wake_census::admission_windows() - win0,
            windows,
            "admission examines exactly the windows"
        );
        for wid in [original, viewer] {
            assert_eq!(
                app.windows[&wid].pending_deco_birth,
                Some(t0),
                "every visible viewer is admitted"
            );
        }
        assert!(
            wake_census::pool_visits() - pool0 > background as u64,
            "the due sweep walks every session — after admission"
        );
        assert!(
            matches!(probe.try_lock(), Err(std::sync::TryLockError::WouldBlock)),
            "{background} background sessions: the wake returned only after the \
             contended background lock came free — it waited on it"
        );

        // Wake 2 — a steady keystroke inside the status interval.
        let t1 = t0 + Duration::from_millis(1);
        let (pool1, locks1) = (
            wake_census::pool_visits(),
            term_lock_acquisitions_on_this_thread(),
        );
        app.on_output_wake(focused, t1);
        let (pool_at, locks_at) = wake_census::take_at_admission().expect("redraw admission ran");
        assert_eq!((pool_at - pool1, locks_at - locks1), (0, 0));
        assert_eq!(
            wake_census::pool_visits() - pool1,
            0,
            "{background} background sessions: a steady wake walks no session"
        );
        let locks = term_lock_acquisitions_on_this_thread() - locks1;
        match steady_locks {
            None => steady_locks = Some(locks),
            Some(expected) => assert_eq!(
                locks, expected,
                "{background} background sessions: a steady wake takes the same \
                 terminal locks as with one"
            ),
        }
        for wid in [original, viewer] {
            assert_eq!(app.windows[&wid].pending_deco_birth, Some(t1));
        }
        assert!(
            !app.session_status.knows(1),
            "the contended newcomer is still unclassified"
        );

        // Released: the next wake classifies the newcomer by retrying it alone, not by
        // walking the pool.
        release_tx.send(()).expect("holder alive");
        holder.join().expect("holder thread");
        let pool2 = wake_census::pool_visits();
        app.on_output_wake(focused, t0 + Duration::from_millis(2));
        assert!(
            app.session_status.knows(1),
            "{background} background sessions: the released newcomer is classified"
        );
        assert_eq!(
            wake_census::pool_visits() - pool2,
            0,
            "{background} background sessions: by a retry of it alone"
        );
    }
}
