// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The install's real off-loop landing path, with a pipe in place of a shell.

use super::*;
use crate::WindowId;
use crate::system_state_access::StepsReach;
use crate::tab_spawn::tests::{drain, registered_piped_session};
use crate::tab_spawn::{TabSpawned, test_support};
use aterm_types::macos_setup::{Plan, Setting};

const LINE: &str = "aterm init --only base.clt";

thread_local! {
    static INSTALL_LAN_TRIGGERS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static INSTALL_EXTERNAL_PROBES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn external_installer_running() -> bool {
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(count.get() + 1));
    true
}

fn publish_run(app: &mut App, revision: u64, running: Vec<Setting>) {
    let permissions = Some(crate::InitialSetupPermissionPolicy {
        revision,
        allowed: false,
    });
    app.observe_initial_setup_permissions(permissions);
    app.publish_system_state(crate::SetupStatus {
        plan: Some(Plan::default()),
        permissions,
        run: Some(crate::InitialSetupRunPolicy {
            revision,
            allowed: true,
        }),
        running,
        ..crate::SetupStatus::default()
    });
}

fn page() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    app.set_system_state_handlers_for_test(crate::InitialSetupHandlers::inert_for_test());
    app.access_steps.set_reach(StepsReach::INERT);
    let wid = WindowId(0);
    let ws = app.windows.get_mut(&wid).unwrap();
    ws.os_window_for_test = true;
    ws.focused = true;
    ws.rows = 90;
    ws.cols = 160;
    assert!(app.open_system_state_tab());
    publish_run(&mut app, 4, Vec::new());
    assert!(app.commit_glassless_native_frame(wid));
    assert!(app.system_state_frontmost_in(wid));
    app.workstation_setup.developer_tools = Install::Wanted(4);
    assert!(app.developer_tools_install_due(true, false, false));
    (app, wid)
}

fn book(app: &mut App, wid: WindowId, revision: u64) -> u64 {
    app.workstation_setup.developer_tools = Install::Opening {
        wanted: Some(revision),
    };
    let id = test_support::book_unasked(app, wid, install_tab_then(revision, LINE.into()));
    assert!(
        !app.developer_tools_install_due(true, false, false),
        "a second request must not start another shell while the first is pending"
    );
    id
}

fn tabs(app: &App) -> usize {
    app.windows.values().map(|ws| ws.tab_set.len()).sum()
}

fn land(app: &mut App, wid: WindowId, id: u64) -> (bool, std::fs::File) {
    let (session, read) = registered_piped_session(id);
    let landed = app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None,
    });
    (landed, read)
}

fn assert_deferred(app: &mut App, wid: WindowId, id: u64, wanted: Install) {
    let before = tabs(app);
    let active = app.windows.get(&wid).and_then(|ws| ws.tab_set.active_id());
    let notices = app.messages.live_rows().count();
    let (landed, read) = land(app, wid, id);
    assert!(!landed, "a no-longer-quiet install must not attach its tab");
    assert_eq!(tabs(app), before, "no visible empty install tab");
    assert_eq!(
        app.windows.get(&wid).and_then(|ws| ws.tab_set.active_id()),
        active,
        "the page the person is using keeps focus"
    );
    assert!(
        app.pool.get(id).is_none(),
        "the unused shell was not installed"
    );
    assert!(
        drain(&read).is_empty(),
        "no install command reached the shell"
    );
    assert_eq!(
        app.messages.live_rows().count(),
        notices,
        "deferral needs no alert"
    );
    assert_eq!(app.workstation_setup.developer_tools, wanted);
}

fn assert_retry_lands_once(app: &mut App, wid: WindowId, revision: u64) {
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::Wanted(revision)
    );
    assert!(app.system_state_frontmost_in(wid));
    assert!(app.developer_tools_install_due(true, false, false));
    let before = tabs(app);
    let id = book(app, wid, revision);
    let (landed, read) = land(app, wid, id);
    assert!(landed);
    assert_eq!(tabs(app), before + 1);
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    app.want_developer_tools_install(revision, true);
    app.want_developer_tools_install(revision + 1, true);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert!(!app.developer_tools_install_due(true, false, false));
    assert_eq!(
        tabs(app),
        before + 1,
        "later callbacks cannot create a second tab"
    );
    assert!(drain(&read).is_empty(), "the command was typed once");
}

#[test]
fn clt_landing_defers_a_run_that_started_another_stage() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    publish_run(&mut app, 4, vec![Setting::Spotlight]);
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    publish_run(&mut app, 4, Vec::new());
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_an_external_installer_holds_the_automatic_request() {
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_probe = Some(external_installer_running);
    assert_eq!(
        app.developer_tools_install_step(),
        Some(ToolsInstall::Installing),
        "the same local evidence already tells the page installation is running"
    );
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(0));
    assert!(
        !app.developer_tools_install_due(true, false, false),
        "an external installer needs no competing automatic password tab"
    );
    assert_eq!(INSTALL_EXTERNAL_PROBES.with(std::cell::Cell::get), 1);
    assert_eq!(app.workstation_setup.developer_tools, Install::Wanted(4));
    assert!(!app.workstation_setup.developer_tools_mark.opened());
    app.workstation_setup.developer_tools_probe = Some(|| false);
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_defers_an_installer_that_started_while_the_shell_spawned() {
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_probe = Some(|| false);
    assert!(app.developer_tools_install_due(true, false, false));
    let id = book(&mut app, wid, 4);
    app.workstation_setup.developer_tools_probe = Some(external_installer_running);
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(0));
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    assert_eq!(INSTALL_EXTERNAL_PROBES.with(std::cell::Cell::get), 1);
    assert!(
        !app.workstation_setup.developer_tools_mark.opened(),
        "a refused landing consumes no once-per-Mac automatic attempt"
    );
    app.workstation_setup.developer_tools_probe = Some(|| false);
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_a_stale_manual_install_press_defers_to_the_live_installer() {
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_probe = Some(external_installer_running);
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(0));
    let before = tabs(&app);
    app.headless = false;
    let started = app.press_developer_tools_install();
    app.headless = true;
    assert!(!started, "a stale Install button starts no competing shell");
    assert_eq!(INSTALL_EXTERNAL_PROBES.with(std::cell::Cell::get), 1);
    assert_eq!(tabs(&app), before);
    assert_eq!(app.workstation_setup.developer_tools, Install::Wanted(4));
    assert!(!app.workstation_setup.developer_tools_mark.opened());
    assert!(app.system_state_frontmost_in(wid));
}

#[test]
fn clt_landing_a_manual_shell_defers_when_an_installer_started_since_the_press() {
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_probe = Some(|| false);
    app.workstation_setup.developer_tools = Install::Pressed;
    let id = test_support::book_unasked(&mut app, wid, pressed_tab_then(LINE.into()));
    app.workstation_setup.developer_tools_probe = Some(external_installer_running);
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(0));
    assert_deferred(&mut app, wid, id, Install::Opened);
    assert_eq!(INSTALL_EXTERNAL_PROBES.with(std::cell::Cell::get), 1);
    assert!(!app.workstation_setup.developer_tools_mark.opened());

    // A later explicit press can land once the other installer is gone.
    app.workstation_setup.developer_tools_probe = Some(|| false);
    app.workstation_setup.developer_tools = Install::Pressed;
    let before = tabs(&app);
    let id = test_support::book_unasked(&mut app, wid, pressed_tab_then(LINE.into()));
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert!(app.developer_tools_install_active());
    assert!(drain(&read).is_empty(), "the command was submitted once");
}

/// A key typed while the shell starts defers the landing: the tab would take the next keys,
/// and those were meant for System State. Once the keys are quiet again, it lands once.
#[test]
fn clt_landing_defers_while_the_person_types() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    let now = std::time::Instant::now();
    app.last_keystroke_at = Some(now);
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    app.last_keystroke_at = now.checked_sub(std::time::Duration::from_secs(3));
    assert_retry_lands_once(&mut app, wid, 4);
}

/// The tick's decision books exactly one shell, which lands once and types the install; a
/// program with no line to type spends the attempt and books nothing.
#[test]
fn clt_landing_a_due_tick_books_one_shell_that_lands_once() {
    let (mut app, wid) = page();
    let (window, ask, then) = app
        .begin_developer_tools_install(|| Some(LINE.into()))
        .expect("due");
    assert_eq!(window, wid);
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::Opening { wanted: Some(4) }
    );
    assert!(
        app.begin_developer_tools_install(|| Some(LINE.into()))
            .is_none(),
        "one shell at a time"
    );
    let before = tabs(&app);
    let id = test_support::book_as(&mut app, wid, &ask, then);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);

    let (mut app, _) = page();
    assert!(app.begin_developer_tools_install(|| None).is_none());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
}

/// A shell that never started posts no "open the tab again" notice: nobody asked for this tab.
/// The attempt is spent, as any opening's is. A tab a person asked for still says it failed.
#[test]
fn clt_landing_a_failed_shell_tells_nobody_to_open_it_again() {
    let failed = |app: &mut App, wid: WindowId, id: u64| {
        app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Err(std::io::Error::other(
                "fork: Resource temporarily unavailable",
            )),
            reader_gate: None,
        })
    };
    // Booked as the tick asks, so the tick's ask is what keeps it quiet.
    let (mut app, wid) = page();
    let (window, ask, then) = app
        .begin_developer_tools_install(|| Some(LINE.into()))
        .expect("due");
    let id = test_support::book_as(&mut app, window, &ask, then);
    let notices = app.messages.live_rows().count();
    assert!(!failed(&mut app, wid, id));
    assert_eq!(app.messages.live_rows().count(), notices);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert!(!app.developer_tools_install_due(true, false, false));

    let (mut app, wid) = page();
    let notices = app.messages.live_rows().count();
    let asked = test_support::book(&mut app, wid, crate::tab_spawn::TabSpawnThen::Nothing);
    assert!(!failed(&mut app, wid, asked));
    assert_eq!(
        app.messages.live_rows().count(),
        notices + 1,
        "a person's own tab still says it failed"
    );
}

/// An unsettled read of the run withholds the install until a settled one returns: the landing
/// defers, the request stays wanted and its attempt unspent.
#[test]
fn clt_landing_defers_while_the_runs_read_is_unsettled() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    let mut unsettled = app.system_state_host.latest_status().clone();
    unsettled.changing = true;
    app.publish_system_state(unsettled);
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    publish_run(&mut app, 4, Vec::new());
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_defers_a_new_dialog_without_losing_the_install() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    app.access_steps.set_reach(StepsReach {
        dialog_up: || true,
        ..StepsReach::INERT
    });
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    app.access_steps.set_reach(StepsReach::INERT);
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_keeps_the_terminal_the_person_selected() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    app.switch_tab_in(wid, 0);
    assert!(!app.system_state_frontmost_in(wid));
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    assert!(app.open_system_state_tab_in_window(wid));
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_waits_while_its_window_is_occluded_or_unfocused() {
    for occluded in [false, true] {
        let (mut app, wid) = page();
        let id = book(&mut app, wid, 4);
        if occluded {
            app.on_occlusion_changed(wid, true);
        } else {
            app.windows.get_mut(&wid).unwrap().focused = false;
        }
        assert!(!app.system_state_frontmost_in(wid));
        assert_deferred(&mut app, wid, id, Install::Wanted(4));
        if occluded {
            app.on_occlusion_changed(wid, false);
        } else {
            app.windows.get_mut(&wid).unwrap().focused = true;
        }
        assert_retry_lands_once(&mut app, wid, 4);
    }
}

#[test]
fn clt_landing_another_front_page_does_not_authorize_the_old_destination() {
    for close in [false, true] {
        let (mut app, wid) = page();
        let id = book(&mut app, wid, 4);
        let other = WindowId(1);
        app.install_window_state(other, crate::stub_session(60_000), 90, 160);
        app.windows.get_mut(&other).unwrap().os_window_for_test = true;
        assert!(app.open_system_state_tab_in_window(other));
        app.windows.get_mut(&wid).unwrap().focused = false;
        app.windows.get_mut(&other).unwrap().focused = true;
        app.frontmost_window = Some(other);
        app.sync_active_session();
        assert!(app.system_state_frontmost_in(other));
        if close {
            assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Stay);
        }
        assert_deferred(&mut app, wid, id, Install::Wanted(4));
        assert_retry_lands_once(&mut app, other, 4);
    }
}

#[test]
fn clt_landing_a_newer_request_survives_the_old_shells_completion() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    app.want_developer_tools_install(5, true);
    app.want_developer_tools_install(4, false);
    publish_run(&mut app, 5, Vec::new());
    assert_deferred(&mut app, wid, id, Install::Wanted(5));
    assert_retry_lands_once(&mut app, wid, 5);
}

#[test]
fn clt_landing_withdrawal_and_stop_leave_no_empty_tab() {
    for stop in [false, true] {
        let (mut app, wid) = page();
        let id = book(&mut app, wid, 4);
        if stop {
            app.stop_initial_setup_permissions();
        } else {
            app.want_developer_tools_install(4, false);
        }
        assert_deferred(&mut app, wid, id, Install::Unasked);
        assert!(!app.developer_tools_install_due(true, false, false));
    }
}

#[test]
fn clt_landing_a_real_spawn_failure_keeps_the_one_attempt_bound() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    let before = tabs(&app);
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other("inert shell spawn failed")),
        reader_gate: None,
    }));
    assert_eq!(tabs(&app), before);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    app.want_developer_tools_install(5, true);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
}
/// The dialog probe can become stale before landing: the atomic reservation still refuses
/// a worker's slot. A successful install keeps the slot until its exact session exits,
/// including when the person returns to System State while the password waits in the tab.
#[test]
fn clt_landing_holds_the_dialog_slot_until_the_installer_exits() {
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up, system_state_front};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    if !crate::test_alone::alone() {
        return;
    }
    assert!(!macos_dialog_up());
    let (mut app, wid) = page();

    // The inexpensive probe read free, but another worker owns the slot when the
    // actual landing tries to reserve it. No empty tab or command may escape.
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        dialog_up: || false,
        ..StepsReach::INERT
    });
    let worker = MacosDialog::when_none_up(Duration::ZERO).unwrap();
    let id = book(&mut app, wid, 4);
    assert_deferred(&mut app, wid, id, Install::Wanted(4));
    assert!(macos_dialog_up(), "deferral keeps the worker's ownership");
    drop(worker);

    app.access_steps.set_reach(StepsReach {
        trigger_local_network: || {
            INSTALL_LAN_TRIGGERS.with(|count| count.set(count.get() + 1));
            true
        },
        frontmost: App::system_state_frontmost,
        quiet: |_, _| true,
        holds_dialog: true,
        dialog_up: macos_dialog_up,
        ..StepsReach::INERT
    });
    INSTALL_LAN_TRIGGERS.with(|count| count.set(0));
    let permissions = Some(crate::InitialSetupPermissionPolicy {
        revision: 4,
        allowed: true,
    });
    app.observe_initial_setup_permissions(permissions);
    let mut status = app.system_state_host.latest_status().clone();
    status.permissions = permissions;
    app.publish_system_state(status);
    assert_eq!(app.access_revision(), Some(4));
    app.publish_system_state_front();
    assert!(system_state_front());
    let crate::tab_spawn::TabSpawnThen::Before { check, then } = install_tab_then(4, LINE.into())
    else {
        panic!("the install's actual callback checks before landing");
    };
    let entered = Rc::new(Cell::new(false));
    let noted = entered.clone();
    let wrapped = crate::tab_spawn::TabSpawnThen::Before {
        check,
        then: Box::new(move |app, outcome| {
            assert!(outcome.is_ok(), "the actual tab installed: {outcome:?}");
            assert!(
                MacosDialog::when_none_up(Duration::ZERO).is_none(),
                "installation retains the slot before the real command callback"
            );
            assert!(!app.system_state_frontmost_in(wid));
            noted.set(true);
            then(app, outcome);
        }),
    };
    app.workstation_setup.developer_tools = Install::Opening { wanted: Some(4) };
    let before = tabs(&app);
    let id = test_support::book(&mut app, wid, wrapped);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert!(
        entered.get(),
        "the real landing reached its command callback"
    );
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    let result_tab = app.windows[&wid].tab_set.active_id().unwrap();
    assert!(
        app.system_state_handoff_pending(),
        "a live installer defers update handoff"
    );
    assert!(
        !system_state_front(),
        "the worker sees the new terminal front"
    );
    assert!(
        macos_dialog_up(),
        "the install's password still owns the slot"
    );
    assert!(app.open_system_state_tab_in_window(wid));
    let now = std::time::Instant::now();
    app.release_unused_access_dialog(now);
    app.tick_local_network_step(now);
    assert_eq!(INSTALL_LAN_TRIGGERS.with(std::cell::Cell::get), 0);
    assert!(
        macos_dialog_up(),
        "returning to setup must not release the installer"
    );
    assert!(app.exit_session_logical(0).is_empty());
    app.tick_local_network_step(now);
    assert_eq!(
        INSTALL_LAN_TRIGGERS.with(std::cell::Cell::get),
        0,
        "an unrelated shell's exit does not end the install"
    );
    assert!(macos_dialog_up());

    let before_exit = tabs(&app);
    assert!(app.exit_session_logical(id).is_empty());
    assert!(!app.system_state_handoff_pending());
    assert!(
        !macos_dialog_up(),
        "the exact install exit releases the slot"
    );
    assert_eq!(
        tabs(&app),
        before_exit,
        "the completed result stays available"
    );
    assert!(app.windows[&wid].tab_set.get(result_tab).is_some());
    assert!(matches!(
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(id)
            .map(|handle| handle.state),
        Some(crate::session_store::SessionState::Exited)
    ));
    assert!(
        app.exit_session_logical(id).is_empty(),
        "EOF may repeat the exit wake"
    );
    assert_eq!(tabs(&app), before_exit);
    assert!(app.windows[&wid].tab_set.get(result_tab).is_some());
    let next_worker = MacosDialog::when_none_up(Duration::ZERO)
        .expect("the worker can acquire after the install process exits");
    drop(next_worker);
    app.tick_local_network_step(now);
    assert_eq!(
        INSTALL_LAN_TRIGGERS.with(std::cell::Cell::get),
        1,
        "the same approved step proceeds after the install, without another approval"
    );
    assert!(
        drain(&read).is_empty(),
        "the install command was submitted once"
    );
}

#[test]
fn clt_landing_only_the_last_linked_installer_view_close_releases_its_slot() {
    use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
    use std::time::Duration;

    if !crate::test_alone::alone() {
        return;
    }
    assert!(!macos_dialog_up());
    let (mut app, wid) = page();
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        dialog_up: macos_dialog_up,
        ..StepsReach::INERT
    });
    let id = book(&mut app, wid, 4);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert!(macos_dialog_up());
    assert!(app.system_state_handoff_pending());
    let linked = app.open_active_session_in_new_window_logical().unwrap();
    assert_eq!(app.pool.views(id), Some(2));
    assert_eq!(app.close_window_logical(linked), crate::CloseOutcome::Stay);
    assert_eq!(app.pool.views(id), Some(1));
    assert!(macos_dialog_up(), "one installer view remains attached");
    assert!(app.system_state_handoff_pending());
    assert!(MacosDialog::when_none_up(Duration::ZERO).is_none());
    assert_eq!(app.focused_session_id(wid), Some(id));

    assert!(app.close_active_tab_in(wid).is_none());
    assert!(app.pool.get(id).is_none());
    assert!(!app.system_state_handoff_pending());
    assert!(
        !macos_dialog_up(),
        "closing the final installer attachment releases its slot"
    );
    let next_worker = MacosDialog::when_none_up(Duration::ZERO)
        .expect("the final close no longer blocks other automatic steps");
    drop(next_worker);
}

#[test]
fn clt_landing_system_state_shutdown_cannot_republish_a_retired_front_page() {
    use aterm_types::macos_setup::system_state_front;

    if !crate::test_alone::alone() {
        return;
    }
    let (mut app, wid) = page();
    let terminal = test_support::book(&mut app, wid, crate::tab_spawn::TabSpawnThen::Nothing);
    let (landed, read) = land(&mut app, wid, terminal);
    assert!(landed);
    assert!(app.open_system_state_tab_in_window(wid));
    assert!(drain(&read).is_empty());
    let pending = book(&mut app, wid, 4);
    let before = tabs(&app);
    let active = app.windows[&wid].tab_set.active_id();
    let notices = app.messages.live_rows().count();
    app.publish_system_state_front();
    assert!(system_state_front());

    // Cmd-Q retains the windows while setup settles, then refuses pending spawns.
    // Their error callbacks must not restore the worker's on-screen authority.
    app.finish_system_state_shutdown();
    assert!(app.system_state_frontmost_in(wid));
    assert!(!system_state_front());
    app.shutdown_tab_spawns();

    assert!(
        !system_state_front(),
        "a refused install must not authorize on-screen work after shutdown"
    );
    assert!(app.system_state_frontmost_in(wid));
    assert_eq!(tabs(&app), before);
    assert_eq!(app.windows[&wid].tab_set.active_id(), active);
    assert!(app.pool.get(pending).is_none());
    assert!(drain(&read).is_empty(), "shutdown typed no install command");
    assert_eq!(app.messages.live_rows().count(), notices);
}

/// aterm opens the tab by itself once per Mac (design §3.3): its landing writes this Mac's
/// mark, and a later launch that reads the mark never opens the tab by itself, though its run
/// wants the tools again. Install… is the way then.
#[test]
fn clt_landing_opens_the_tab_by_itself_once_per_mac() {
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
    assert!(!app.workstation_setup.developer_tools_mark.opened());
    assert_retry_lands_once(&mut app, wid, 4);
    assert!(mark.is_file(), "the landing marks this Mac");

    let (mut later, _) = page();
    later.workstation_setup.developer_tools_mark = Mark::at(mark);
    assert!(later.workstation_setup.developer_tools_mark.opened());
    assert_eq!(later.workstation_setup.developer_tools, Install::Wanted(4));
    assert!(
        !later.developer_tools_install_due(true, false, false),
        "a later launch never opens it by itself"
    );
}

/// Install… opens the tab on a person's press (design §3.3), whatever this Mac's mark says and
/// with no run asking: the line is typed once, the page then reads the tab asking for the
/// password, and a second press opens no second tab while it is open.
#[test]
fn clt_landing_install_opens_the_tab_on_a_press() {
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    std::fs::write(&mark, "opened\n").expect("a mark from an earlier launch");
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark);
    app.workstation_setup.developer_tools = Install::Pressed;
    assert_eq!(app.developer_tools_install_step(), None);
    let before = tabs(&app);
    let id = test_support::book(&mut app, wid, pressed_tab_then(LINE.into()));
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert_eq!(
        app.developer_tools_install_step(),
        Some(ToolsInstall::Password)
    );
    app.headless = false;
    assert!(!app.press_developer_tools_install(), "one tab at a time");
    app.headless = true;
    assert_eq!(tabs(&app), before + 1);
}
