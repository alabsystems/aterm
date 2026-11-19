// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::app_workers_tests::{land_worker, person_tab};
use crate::{App, WindowId, stub_session};

const WID: WindowId = WindowId(0);

#[test]
fn local_panel_roster_counts_local_addressable_sids_once_including_a_parked_resident() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = app.focused_session_id(WID).unwrap();
    let mirror = app.open_active_session_in_new_window_logical().unwrap();
    assert_ne!(mirror, WID);
    assert_eq!(app.focused_session_id(mirror), Some(first));
    app.frontmost_window = Some(WID);
    let resident = land_worker(&mut app);
    app.pool
        .get(resident)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .role = Some("resident".into());
    assert!(app.session_is_parked(resident));
    let other_window = app.next_session_id;
    let mut shell = stub_session(other_window);
    let started = std::time::Instant::now();
    shell.shell_start = Some(crate::spawn::ShellStart {
        at: started,
        name: "sh".to_owned(),
        input: shell.ctx.sink.driven_input(),
    });
    shell.pty_closed_at.set(started).unwrap();
    shell.child_exit.set(aterm_pty::ChildExit::Code(1)).unwrap();
    app.push_stub_tab(mirror, shell);
    let spare = stub_session(app.next_session_id);
    app.next_session_id += 1;
    App::register_session(&app.store, &spare, None);
    app.open_navigator(false).unwrap();
    // An independent aterm process may use the same numeric local address for a different SID.
    let foreign = App::headless_for_test();
    let remote = &foreign.pool.get(first).unwrap().ctx.self_id;
    assert_ne!(remote, &app.pool.get(first).unwrap().ctx.self_id);
    app.refresh_local_panel_roster();
    assert_eq!(app.local_panel_roster.len(), 3);
    for id in [first, resident, other_window] {
        assert!(app.local_panel_roster.number(id).is_some());
    }
    assert!(app.local_panel_roster.number(spare.id).is_none());
    assert!(!app.activate_local_panel_sid(WID, remote));

    // Keep the last terminal's view present after exit: it is visible but no longer addressable.
    assert!(app.exit_session_logical(other_window).is_empty());
    assert!(app.window_contains_session(mirror, other_window));
    app.refresh_local_panel_roster();
    assert_eq!(app.local_panel_roster.len(), 2);
    assert!(!app.activate_local_panel_session(WID, other_window));
    assert!(!app.activate_local_panel_session(WID, spare.id));

    // A registry replacement under the same SID keeps its number while stale views retire.
    let sid = app.pool.get(resident).unwrap().ctx.self_id.clone();
    let kept = app.local_panel_roster.number(resident);
    let restored = app.next_session_id;
    app.push_stub_tab(mirror, crate::stub_session_as(restored, sid.clone()));
    app.refresh_local_panel_roster();
    assert_eq!(app.local_panel_roster.len(), 2);
    assert_eq!(app.local_panel_roster.number(restored), kept);
    assert!(!app.activate_local_panel_session(WID, resident));
    assert!(app.activate_local_panel_sid(WID, &sid));
    let preview = app.windows[&WID].active_session_preview().unwrap();
    assert!(preview.borrowed);
    assert_eq!(preview.session, restored);
    assert_eq!(app.focused_session_id(WID), Some(restored));
    assert_eq!(app.frontmost_window, Some(WID));
    assert_eq!(app.pool.views(restored), Some(2));
}

#[test]
fn local_panel_roster_shortcuts_reopen_the_same_session_after_reordering_and_keep_the_utility() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = App::headless_for_test();
    let first = app.focused_session_id(WID).unwrap();
    let second = person_tab(&mut app);
    let parked = land_worker(&mut app);
    let parked_term = app.pool.get(parked).unwrap().term.clone();
    crate::term_lock(&parked_term).process(b"$ command still being edited");
    app.open_navigator(false).unwrap();
    let utility = app.windows[&WID].tab_set.active().unwrap().focus;
    assert!(app.view_is_unrestored_page(utility));
    for _ in 0..7 {
        person_tab(&mut app);
    }
    app.refresh_local_panel_roster();
    assert_eq!(app.local_panel_roster.len(), 10);
    assert_eq!(app.local_panel_roster.number(first), Some(1));
    assert_eq!(app.local_panel_roster.number(second), Some(2));
    assert_eq!(app.local_panel_roster.number(parked), Some(3));
    let tenth = *app.local_panel_roster.sessions.last_key_value().unwrap().0;
    assert_eq!(app.local_panel_roster.number(tenth), None);

    app.move_tab(WID, 1, 0);
    assert!(app.activate_local_panel_number(WID, 2));
    assert_eq!(app.focused_session_id(WID), Some(second));
    assert!(app.activate_local_panel_number(WID, 3));
    assert_eq!(app.focused_session_id(WID), Some(parked));
    assert!(app.windows[&WID].tab_set.active_is_parked());
    assert_eq!(
        app.windows[&WID].active_session_preview().unwrap().session,
        parked
    );
    let live = app.pool.get(parked).unwrap();
    assert!(std::sync::Arc::ptr_eq(&parked_term, &live.term));
    assert!(
        crate::term_lock(&live.term)
            .row_text(0)
            .unwrap()
            .contains("command still being edited")
    );
    assert!(
        app.windows[&WID]
            .tab_set
            .tabs()
            .iter()
            .any(|tab| tab.root.contains(utility))
    );
    assert!(
        app.activate_local_panel_session(WID, tenth),
        "a row beyond nine still opens"
    );
    assert_eq!(app.focused_session_id(WID), Some(tenth));
    assert!(!app.activate_local_panel_number(WID, 0));
    assert!(!app.activate_local_panel_number(WID, 10));
    assert_eq!(app.focused_session_id(WID), Some(tenth));

    // Only the departing session's shortcut is reused.
    assert!(app.close_session_tab_for_test(second));
    app.refresh_local_panel_roster();
    assert_eq!(app.local_panel_roster.number(tenth), Some(2));
    assert_eq!(app.local_panel_roster.number(parked), Some(3));
    assert!(app.activate_local_panel_number(WID, 2));
    assert_eq!(app.focused_session_id(WID), Some(tenth));
}
