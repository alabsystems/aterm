// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The install's real off-loop landing path, with a pipe in place of a shell.

use super::*;
use crate::WindowId;
use crate::system_state_access::StepsReach;
use crate::tab_spawn::tests::{drain, registered_piped_session};
use crate::tab_spawn::{TabSpawned, test_support};
use aterm_types::macos_setup::{Plan, Setting};

/// The `aterm` the tab's program would run: never run here, as no landed test tab runs one.
const ATERM: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// What the landing types into the tab: the program's go line.
fn go_line() -> Vec<u8> {
    format!("{}\r", aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO).into_bytes()
}

fn aterm() -> Option<std::path::PathBuf> {
    Some(ATERM.into())
}

/// [`App::developer_tools_install_step`] for the status the page shows now.
fn step(app: &mut App) -> Option<ToolsInstall> {
    let status = app.system_state_host.latest_status().clone();
    app.developer_tools_install_step(&status)
}

#[path = "recovery_tests.rs"]
mod recovery_tests;

thread_local! {
    static INSTALL_LAN_TRIGGERS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static INSTALL_EXTERNAL_PROBES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static INSTALL_FDA_DIALOG_UP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn external_installer_running() -> Reading {
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(count.get() + 1));
    installing()
}

/// This Mac with nothing installing the tools.
fn idle() -> Reading {
    Reading::default()
}

/// `softwareupdate` holding the on-demand placeholder: an install runs.
fn installing() -> Reading {
    Reading {
        placeholder: true,
        install_process: true,
        ..Reading::default()
    }
}

/// Apple's own installer window up, its install not started.
fn apple_window() -> Reading {
    Reading {
        apple_window: true,
        ..Reading::default()
    }
}

fn publish_run(app: &mut App, revision: u64, running: Vec<Setting>) {
    let permissions = Some(crate::InitialSetupPermissionPolicy {
        revision,
        allowed: true,
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
    // A person at the page: the install's authorization is a person stage's.
    app.user_input_recent = |_| true;
    let wid = WindowId(0);
    let ws = app.windows.get_mut(&wid).unwrap();
    ws.os_window_for_test = true;
    ws.focused = true;
    ws.rows = 90;
    ws.cols = 160;
    assert!(app.open_system_state_tab());
    app.note_system_state_touch();
    app.note_system_state_apply(crate::system_state::Admission::Person);
    app.accepted_initial_setup_apply();
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
    let id = test_support::book_unasked(app, wid, install_tab_then(revision));
    assert!(
        !app.developer_tools_install_due(true, false, false),
        "a second request must not start another shell while the first is pending"
    );
    id
}

fn tabs(app: &App) -> usize {
    app.windows.values().map(|ws| ws.tab_set.len()).sum()
}

#[test]
fn automatic_install_starts_without_taking_the_current_tabs_focus() {
    let (mut app, wid) = page();
    let active = app.windows[&wid].tab_set.active_id();
    let (window, ask, then) = app
        .begin_developer_tools_install(aterm)
        .expect("automatic install is due");
    assert_eq!(window, wid);
    assert!(ask.background);
    let id = test_support::book_as(&mut app, window, &ask, then);
    let (landed, read) = land(&mut app, window, id);
    assert!(landed);
    assert_eq!(app.windows[&wid].tab_set.active_id(), active);
    assert!(app.system_state_frontmost_in(wid));
    assert_eq!(drain(&read), go_line());
    assert!(app.developer_tools_install_active());
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
    assert_eq!(drain(&read), go_line());
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
        step(&mut app),
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
    app.workstation_setup.developer_tools_probe = Some(idle);
    assert_retry_lands_once(&mut app, wid, 4);
}

#[test]
fn clt_landing_defers_an_installer_that_started_while_the_shell_spawned() {
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_probe = Some(idle);
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
    app.workstation_setup.developer_tools_probe = Some(idle);
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
    app.workstation_setup.developer_tools_probe = Some(idle);
    app.workstation_setup.developer_tools = Install::Pressed;
    let id = test_support::book_unasked(&mut app, wid, pressed_tab_then());
    app.workstation_setup.developer_tools_probe = Some(external_installer_running);
    INSTALL_EXTERNAL_PROBES.with(|count| count.set(0));
    assert_deferred(&mut app, wid, id, Install::Opened);
    assert_eq!(INSTALL_EXTERNAL_PROBES.with(std::cell::Cell::get), 1);
    assert!(!app.workstation_setup.developer_tools_mark.opened());

    // A later explicit press can land once the other installer is gone.
    app.workstation_setup.developer_tools_probe = Some(idle);
    app.workstation_setup.developer_tools = Install::Pressed;
    let before = tabs(&app);
    let id = test_support::book_unasked(&mut app, wid, pressed_tab_then());
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), go_line());
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
    let (window, ask, then) = app.begin_developer_tools_install(aterm).expect("due");
    assert_eq!(window, wid);
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::Opening { wanted: Some(4) }
    );
    assert!(
        app.begin_developer_tools_install(aterm).is_none(),
        "one shell at a time"
    );
    let before = tabs(&app);
    let id = test_support::book_as(&mut app, wid, &ask, then);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), go_line());
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
    let (window, ask, then) = app.begin_developer_tools_install(aterm).expect("due");
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
    app.note_system_state_touch();
    app.note_system_state_apply(crate::system_state::Admission::Person);
    app.accepted_initial_setup_apply();
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
    let crate::tab_spawn::TabSpawnThen::Before { check, then } = install_tab_then(4) else {
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
    assert_eq!(drain(&read), go_line());
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
    assert_eq!(drain(&read), go_line());
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

/// aterm opens the tab by itself once per account on this Mac (design §3.3): its landing writes
/// the account's mark, and a later launch that reads the mark never opens the tab by itself,
/// though its run wants the tools again. Install… is the way then.
#[test]
fn clt_landing_opens_the_tab_by_itself_once_per_account() {
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
    assert!(!app.workstation_setup.developer_tools_mark.opened());
    assert_retry_lands_once(&mut app, wid, 4);
    assert!(mark.is_file(), "the landing marks this account");

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
    assert_eq!(step(&mut app), None);
    let before = tabs(&app);
    let id = test_support::book(&mut app, wid, pressed_tab_then());
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), go_line());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert_eq!(step(&mut app), Some(ToolsInstall::Password));
    app.headless = false;
    assert!(!app.press_developer_tools_install(), "one tab at a time");
    app.headless = true;
    assert_eq!(tabs(&app), before + 1);
}

/// A person's Install… never waits behind a helper panel they cannot see (e2e145: with aterm in
/// front the Accessibility panel was gone, its dialog guard held, and the press sat queued with
/// nothing on screen to answer): the panel gives way, the guard (this process's real count of
/// macOS dialogs) goes, and the press goes on at once.
/// NEGATIVE CONTROL: an Install… that does not meet the panel first, or a give-way that keeps the
/// guard, queues the press behind it (`Install::PressedQueued`).
#[cfg(target_os = "macos")]
#[test]
fn clt_landing_a_press_never_waits_behind_a_panel_out_of_sight() {
    use crate::devtools_helper::{Lane, Origin, Page, Reading, Show, Wake, test_arms};
    use aterm_types::macos_setup::macos_dialog_up;
    use std::time::Instant;

    if !crate::test_alone::alone() {
        return;
    }
    assert!(!macos_dialog_up());
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    std::fs::write(&mark, "opened\n").expect("a mark from an earlier launch");
    let (mut app, _) = page();
    // No install of the run's own is due once the guard frees: the press alone opens the tab.
    app.workstation_setup.developer_tools_mark = Mark::at(mark);
    app.workstation_setup.developer_tools_probe = Some(idle);
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        dialog_up: macos_dialog_up,
        ..StepsReach::INERT
    });
    // System Settings is not the active app: aterm is, where the press is made.
    app.devtools_helper = Lane::windowed_with_fda_for_test(
        test_arms::opening(),
        test_arms::fda_opening(),
        test_arms::counted(|_, _| true, |_| {}, || {}, || false),
    );
    let wake: Wake = std::sync::Arc::new(|| {});
    let accessibility = Show {
        page: Page::Accessibility,
        origin: Origin::Settings,
        reading: Reading::UNREAD,
        listed: None,
        bundle: None,
        skips: false,
        step: None,
    };
    assert!(
        app.devtools_helper
            .show(accessibility, Instant::now(), wake)
    );
    assert!(app.reserve_hand_over_dialog(Origin::Settings));
    assert!(macos_dialog_up(), "the panel holds the window's guard");

    app.headless = false;
    let pressed = app.press_developer_tools_install();
    app.headless = true;
    assert!(app.devtools_helper.panel().is_none(), "it gave way");
    assert!(
        pressed
            && matches!(
                app.workstation_setup.developer_tools,
                Install::Pressed | Install::Opened
            ),
        "{pressed}: {:?}",
        app.workstation_setup.developer_tools
    );
}

/// Pressing Install… while Full Disk Access holds the dialog slot queues the press, updates the
/// Repositories row with why it waits, and opens the tab once the slot frees; negative control:
/// the pre-C2 drop-press policy returns false, leaves no reason on the row, and opens nothing.
#[test]
fn clt_landing_a_press_during_full_disk_access_queues_with_reason_and_runs_once_the_slot_frees() {
    use aterm_types::macos_setup::{
        Access, COMMAND_LINE_TOOLS_MISSING, Observation, Standing, Subject,
    };

    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    std::fs::write(&mark, "opened\n").expect("a mark from an earlier launch");
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark);
    app.access_steps.set_reach(StepsReach {
        dialog_up: || INSTALL_FDA_DIALOG_UP.with(std::cell::Cell::get),
        ..StepsReach::INERT
    });
    INSTALL_FDA_DIALOG_UP.with(|up| up.set(true));

    let plan = Plan::default();
    let mut status = app.system_state_host.latest_status().clone();
    status.plan = Some(plan.clone());
    status.observed_vault_url = Some(plan.vault_url.clone());
    status.observed = vec![
        Observation {
            subject: Subject::Access(Access::FullDiskAccess),
            standing: Standing::NeedsYou,
            now: Some("Not confirmed".into()),
            after_login: None,
            wanted: Subject::Access(Access::FullDiskAccess).wanted().to_owned(),
            note: None,
            detail: None,
        },
        Observation {
            subject: Subject::Setting(Setting::Repositories),
            standing: Standing::NeedsYou,
            now: None,
            after_login: None,
            wanted: Subject::Setting(Setting::Repositories).wanted().to_owned(),
            note: Some(COMMAND_LINE_TOOLS_MISSING.into()),
            detail: None,
        },
    ];
    app.publish_system_state(status);
    assert_eq!(
        app.system_state_host.latest_status().developer_tools,
        None,
        "before the press, the row offers Install…"
    );

    // Negative control: dropping the press while a dialog is up refuses the press, leaves
    // `developer_tools` at `None` (no reason on the row), and opens 0 tabs when the slot frees.
    let drop_press = |dialog_up: bool| -> Option<ToolsInstall> {
        if dialog_up {
            None
        } else {
            Some(ToolsInstall::Password)
        }
    };
    assert_eq!(
        drop_press(true),
        None,
        "CONTROL: dropping the press gives no queued state or reason"
    );

    let before = tabs(&app);
    app.headless = false;
    assert!(
        app.press_developer_tools_install(),
        "a press during Full Disk Access is queued, not dropped"
    );
    assert!(
        !app.press_developer_tools_install(),
        "a second press while queued starts no duplicate request"
    );
    app.headless = true;

    assert_eq!(tabs(&app), before, "no tab opens while FDA holds the slot");
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::PressedQueued { wanted: Some(4) }
    );
    assert_eq!(step(&mut app), Some(ToolsInstall::Queued));
    assert_eq!(
        app.system_state_host.latest_status().developer_tools,
        Some(ToolsInstall::Queued),
        "System State republishes immediately so the Repositories row says why it waits"
    );
    assert!(
        app.begin_developer_tools_install(aterm).is_none(),
        "still held while Full Disk Access owns the dialog slot"
    );

    // Full Disk Access releases the dialog slot: the queued press runs even with `Mark::opened`.
    INSTALL_FDA_DIALOG_UP.with(|up| up.set(false));
    let (window, ask, then) = app
        .begin_developer_tools_install(aterm)
        .expect("the queued press dispatches once the dialog slot frees");
    assert_eq!(window, wid);
    assert_eq!(app.workstation_setup.developer_tools, Install::Pressed);
    let id = test_support::book_as(&mut app, wid, &ask, then);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), go_line());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert_eq!(step(&mut app), Some(ToolsInstall::Password));
    assert_eq!(
        app.system_state_host.latest_status().developer_tools,
        Some(ToolsInstall::Password)
    );
}

/// A press queued behind another macOS dialog is no longer this moment's press, and its tab takes
/// the window's keys: once the dialog slot frees, it still waits while the person types, and
/// starts its one shell after 2.5 s of quiet keyboard, as the automatic tab does.
/// NEGATIVE CONTROL: a queued press that ignores the keyboard dispatches while a key was just
/// typed.
#[test]
fn clt_landing_a_queued_press_waits_for_a_quiet_keyboard_once_the_slot_frees() {
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    std::fs::write(&mark, "opened\n").expect("a mark from an earlier launch");
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark);
    app.access_steps.set_reach(StepsReach {
        dialog_up: || INSTALL_FDA_DIALOG_UP.with(std::cell::Cell::get),
        ..StepsReach::INERT
    });
    INSTALL_FDA_DIALOG_UP.with(|up| up.set(true));
    app.headless = false;
    assert!(app.press_developer_tools_install(), "queued, not dropped");
    app.headless = true;
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::PressedQueued { wanted: Some(4) }
    );

    // The slot frees while the person types on the page.
    INSTALL_FDA_DIALOG_UP.with(|up| up.set(false));
    let before = tabs(&app);
    let now = std::time::Instant::now();
    app.last_keystroke_at = Some(now);
    assert!(
        app.begin_developer_tools_install(aterm).is_none(),
        "a key typed just now holds the queued press"
    );
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::PressedQueued { wanted: Some(4) },
        "the press stays queued"
    );
    assert_eq!(step(&mut app), Some(ToolsInstall::Queued));

    // The keyboard rests: the queued press books its one shell, which lands once.
    app.last_keystroke_at = now.checked_sub(std::time::Duration::from_secs(3));
    let (window, ask, then) = app
        .begin_developer_tools_install(aterm)
        .expect("the queued press dispatches once the keyboard is quiet");
    assert_eq!(window, wid);
    assert_eq!(app.workstation_setup.developer_tools, Install::Pressed);
    let id = test_support::book_as(&mut app, wid, &ask, then);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(tabs(&app), before + 1);
    assert_eq!(drain(&read), go_line());
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
}

thread_local! {
    static TOOLS_INSTALLED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The page's status with the GitHub repositories reading that Apple's Command Line Tools are
/// missing, as a pass leaves it.
fn tools_missing(app: &App) -> crate::SetupStatus {
    use aterm_types::macos_setup::{Observation, Standing, Subject};
    let mut status = app.system_state_host.latest_status().clone();
    let plan = status.plan.clone().unwrap_or_default();
    status.observed_vault_url = Some(plan.vault_url);
    status.observed = vec![Observation {
        subject: Subject::Setting(Setting::Repositories),
        standing: Standing::NeedsYou,
        now: None,
        after_login: None,
        wanted: Subject::Setting(Setting::Repositories).wanted().to_owned(),
        note: Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_MISSING.to_owned()),
        detail: None,
    }];
    status.observed_generation += 1;
    status.developer_tools = None;
    status
}

/// The same status once the reading says the tools are there.
fn tools_there(missing: &crate::SetupStatus) -> crate::SetupStatus {
    let mut status = missing.clone();
    for reading in &mut status.observed {
        reading.standing = aterm_types::macos_setup::Standing::InPlace;
        reading.note = None;
    }
    status.observed_generation += 1;
    status
}

/// The menu line the page's status says now, label and note.
fn menu(app: &App) -> Option<(String, Option<String>)> {
    crate::system_state_access::menu_line(app.system_state_host.latest_status(), false)
        .map(|line| (line.label, line.note))
}

fn shown(app: &App) -> Option<ToolsInstall> {
    app.system_state_host.latest_status().developer_tools
}

/// The tab's program outlives the installer it ran: it reads the tools back and says so. Nothing
/// asks for a password then, so the row stays calm, and so do the menu line and the diamond. A
/// tab that never saw an installer run is still asking for the password.
#[test]
fn the_tab_outliving_the_installer_stays_calm() {
    let (mut app, _) = page();
    app.workstation_setup.developer_tools_session = vec![Terminal::running(77, false, None)];
    app.workstation_setup.developer_tools_probe = Some(idle);
    let missing = tools_missing(&app);
    app.publish_system_state(missing.clone());
    assert_eq!(shown(&app), Some(ToolsInstall::Password));
    assert_eq!(
        menu(&app).and_then(|(_, note)| note).as_deref(),
        Some(crate::system_state::TOOLS_PASSWORD_HEADLINE),
        "the tab asks for the password"
    );

    app.workstation_setup.developer_tools_probe = Some(installing);
    app.publish_system_state(missing.clone());
    assert_eq!(shown(&app), Some(ToolsInstall::Installing));
    assert_eq!(menu(&app), None, "installing is calm");

    app.workstation_setup.developer_tools_probe = Some(idle);
    app.publish_system_state(missing);
    assert_eq!(
        shown(&app),
        Some(ToolsInstall::Installing),
        "the installer finished; the tab's program reads them back"
    );
    assert_eq!(menu(&app), None, "nothing asks for a password now");
}

/// The tab's program said the tools are installed (exit 0): the GitHub repositories row is read
/// again at once, and it stays calm until that reading lands. A reading that says the tools are
/// there ends it; one that says they are missing after that offers Install… again. A program
/// that said the install did not finish leaves Install… at once.
#[test]
fn the_row_stays_calm_after_the_tab_installed_them_until_it_is_read_again() {
    let (mut app, _) = page();
    let mut handlers = crate::InitialSetupHandlers::inert_for_test();
    handlers.tools_installed = || TOOLS_INSTALLED.with(|count| count.set(count.get() + 1));
    app.set_system_state_handlers_for_test(handlers);
    app.workstation_setup.developer_tools_probe = Some(idle);
    let missing = tools_missing(&app);
    app.publish_system_state(missing.clone());
    app.workstation_setup.developer_tools_session = vec![Terminal::running(77, false, None)];
    app.workstation_setup.developer_tools_attempt = Some(77);
    TOOLS_INSTALLED.with(|count| count.set(0));

    app.developer_tools_install_ended(77, Some(aterm_pty::ChildExit::Code(0)));
    assert_eq!(
        TOOLS_INSTALLED.with(std::cell::Cell::get),
        1,
        "the row is read again at once"
    );
    app.publish_system_state(missing.clone());
    assert_eq!(shown(&app), Some(ToolsInstall::Installing));
    assert_eq!(menu(&app), None, "the install succeeded");

    app.publish_system_state(tools_there(&missing));
    assert!(
        app.workstation_setup
            .developer_tools_after
            .settling
            .is_none()
    );
    assert_eq!(menu(&app), None);
    app.publish_system_state(missing.clone());
    assert_eq!(
        shown(&app),
        None,
        "a later reading of them missing offers Install…"
    );
    assert!(menu(&app).is_some());

    for exit in [
        aterm_pty::ChildExit::Code(1),
        aterm_pty::ChildExit::Signal(9),
    ] {
        app.workstation_setup.developer_tools_session = vec![Terminal::running(78, false, None)];
        app.workstation_setup.developer_tools_attempt = Some(78);
        app.developer_tools_install_ended(78, Some(exit));
        app.publish_system_state(missing.clone());
        assert_eq!(shown(&app), None, "{exit:?} leaves Install…");
        assert!(menu(&app).is_some(), "{exit:?}");
    }
    assert_eq!(
        TOOLS_INSTALLED.with(std::cell::Cell::get),
        1,
        "only a program that installed them asks for the reading"
    );
}

/// The page says "Enter your password in the Command Line Tools tab", and its Go to tab brings
/// that tab to the front: the tab is named so, whatever its program titles it.
#[test]
fn the_tab_the_page_names_carries_that_name() {
    let (mut app, wid) = page();
    let (window, ask, then) = app.begin_developer_tools_install(aterm).expect("due");
    let id = test_support::book_as(&mut app, window, &ask, then);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(drain(&read), go_line());
    let titles = app.tab_titles(wid);
    assert!(
        titles.iter().any(|title| title == TAB_TITLE),
        "no tab is named for the Command Line Tools: {titles:?}"
    );

    assert!(app.open_system_state_tab_in_window(wid));
    assert!(app.system_state_frontmost_in(wid));
    assert!(app.go_to_developer_tools_tab());
    assert_eq!(
        app.focused_session_id(wid),
        Some(id),
        "Go to tab selects it"
    );
}

/// A program that ended without saying how (a signal, an exit nobody gave) leaves the window's
/// own end line in the tab, in plain words; one that said why itself gets no second line.
#[test]
fn the_tab_ends_on_a_plain_line_when_its_program_said_nothing() {
    use aterm_types::macos_setup::{COMMAND_LINE_TOOLS_NOT_FINISHED, COMMAND_LINE_TOOLS_TRY_AGAIN};
    let end = format!("{COMMAND_LINE_TOOLS_NOT_FINISHED}. {COMMAND_LINE_TOOLS_TRY_AGAIN}");
    for (exit, written) in [
        (aterm_pty::ChildExit::Signal(9), true),
        (aterm_pty::ChildExit::Code(127), true),
        (aterm_pty::ChildExit::Code(1), false),
    ] {
        let (mut app, wid) = page();
        let (window, ask, then) = app.begin_developer_tools_install(aterm).expect("due");
        let id = test_support::book_as(&mut app, window, &ask, then);
        let (landed, _read) = land(&mut app, wid, id);
        assert!(landed);
        app.developer_tools_install_ended(id, Some(exit));
        let text: Vec<String> = {
            let term = crate::term_lock(&app.pool.get(id).expect("the tab stays").term);
            (0..200).filter_map(|row| term.row_text(row)).collect()
        };
        assert_eq!(
            text.iter().filter(|line| line.contains(&end)).count(),
            usize::from(written),
            "{exit:?}: {text:?}"
        );
    }
}

/// A press queued behind a macOS dialog is let go when the tools arrived meanwhile: no tab opens
/// for tools that are there, and the row has nothing to say.
#[test]
fn a_queued_press_lets_go_once_the_tools_arrived() {
    let (mut app, _) = page();
    app.workstation_setup.developer_tools = Install::PressedQueued { wanted: None };
    let missing = tools_missing(&app);
    app.publish_system_state(tools_there(&missing));
    assert!(app.begin_developer_tools_install(aterm).is_none());
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::Unasked,
        "tools that arrived let the press go"
    );
}

/// The tab a person's press asked for is theirs: a spawn that fails tells them, as any tab they
/// open does, and the row offers Install… again.
#[test]
fn a_pressed_tab_that_fails_to_spawn_says_so() {
    let (mut app, wid) = page();
    app.publish_system_state(tools_missing(&app));
    let (ask, then) = app.begin_pressed_install(aterm).expect("a program to run");
    assert!(ask.unasked, "keys typed meanwhile stay in System State");
    let id = test_support::book_as(&mut app, wid, &ask, then);
    let notices = app.messages.live_rows().count();
    assert!(!app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Err(std::io::Error::other(
            "fork: Resource temporarily unavailable"
        )),
        reader_gate: None,
    }));
    assert_eq!(
        app.messages.live_rows().count(),
        notices + 1,
        "the person hears the tab did not open"
    );
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert_eq!(
        app.system_state_host
            .latest_status()
            .repositories_tools_wait(),
        Some(crate::system_state::ToolsWait::Missing { refused: None }),
        "Install… is there again"
    );
}

/// A pressed tab that cannot land opens nothing, and the press is never dropped without a word:
/// System State left behind says why on the row, with Install… there again, until the next
/// press; a macOS dialog that came up meanwhile keeps the press for when it closes.
#[test]
fn a_pressed_tab_that_cannot_land_says_why_on_the_row() {
    use crate::system_state::ToolsWait;
    let (mut app, wid) = page();
    app.publish_system_state(tools_missing(&app));
    let (ask, then) = app.begin_pressed_install(aterm).expect("a program to run");
    let id = test_support::book_as(&mut app, wid, &ask, then);
    app.switch_tab_in(wid, 0);
    assert!(!app.system_state_frontmost_in(wid));
    let before = tabs(&app);
    let notices = app.messages.live_rows().count();
    let (landed, read) = land(&mut app, wid, id);
    assert!(!landed);
    assert_eq!(tabs(&app), before, "no empty tab");
    assert!(drain(&read).is_empty());
    assert_eq!(app.messages.live_rows().count(), notices, "the row says it");
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert_eq!(
        shown(&app),
        Some(ToolsInstall::NotOpened(NotOpened::LeftPage))
    );
    assert_eq!(
        app.system_state_host
            .latest_status()
            .repositories_tools_wait(),
        Some(ToolsWait::Missing {
            refused: Some(NotOpened::LeftPage)
        }),
        "Install… is there, with why"
    );
    assert!(app.open_system_state_tab_in_window(wid));
    app.access_steps.set_reach(StepsReach {
        dialog_up: || true,
        ..StepsReach::INERT
    });
    app.headless = false;
    assert!(app.press_developer_tools_install(), "a new press is taken");
    app.headless = true;
    assert_eq!(
        shown(&app),
        Some(ToolsInstall::Queued),
        "the new press clears the note"
    );

    let (mut app, wid) = page();
    app.publish_system_state(tools_missing(&app));
    let (ask, then) = app.begin_pressed_install(aterm).expect("a program to run");
    let id = test_support::book_as(&mut app, wid, &ask, then);
    app.access_steps.set_reach(StepsReach {
        dialog_up: || true,
        ..StepsReach::INERT
    });
    let (landed, _read) = land(&mut app, wid, id);
    assert!(!landed);
    assert_eq!(
        app.workstation_setup.developer_tools,
        Install::PressedQueued { wanted: None },
        "a dialog that came up keeps the press"
    );
    assert_eq!(shown(&app), Some(ToolsInstall::Queued));
}

/// Apple's own installer window, waiting for the person's press, needs them there; a
/// placeholder nothing holds is no install, and the row offers Install….
#[test]
fn apples_installer_window_needs_the_person_and_a_stale_placeholder_does_not_install() {
    let (mut app, _) = page();
    let missing = tools_missing(&app);
    app.workstation_setup.developer_tools_probe = Some(apple_window);
    app.publish_system_state(missing.clone());
    assert_eq!(shown(&app), Some(ToolsInstall::AppleWindow));
    assert_eq!(
        menu(&app).and_then(|(_, note)| note).as_deref(),
        Some(crate::system_state::APPLE_WINDOW_NOTE)
    );
    app.workstation_setup.developer_tools_probe = Some(|| Reading {
        placeholder: true,
        ..Reading::default()
    });
    app.publish_system_state(missing.clone());
    assert_eq!(shown(&app), None, "Install… again");
    assert!(menu(&app).is_some());

    // The tab's program opened Apple's window (Apple's catalog listed no tools) and asks the
    // person to press Install there, with its placeholder held: they are needed there, whatever
    // the placeholder says, until the window goes.
    app.workstation_setup.developer_tools_session = vec![Terminal::running(77, false, None)];
    app.workstation_setup.developer_tools_probe = Some(|| Reading {
        placeholder: true,
        apple_window: true,
        ..Reading::default()
    });
    app.publish_system_state(missing.clone());
    assert_eq!(shown(&app), Some(ToolsInstall::AppleWindow));
    assert_eq!(
        menu(&app).and_then(|(_, note)| note).as_deref(),
        Some(crate::system_state::APPLE_WINDOW_NOTE)
    );
    app.workstation_setup.developer_tools_probe = Some(idle);
    app.publish_system_state(missing);
    assert_eq!(
        shown(&app),
        Some(ToolsInstall::Installing),
        "once the window goes, the program reads the tools back"
    );
}

thread_local! {
    /// Whether the developer tools are installed, as a test's shim reads it ([`tools_present`]).
    static TOOLS_PRESENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn tools_present() -> bool {
    TOOLS_PRESENT.with(std::cell::Cell::get)
}

/// `app`'s shim laid at `dir`'s `clt-shim` (a test's own, never this account's), the tools
/// missing until the test says they arrived ([`TOOLS_PRESENT`]).
fn shim_in(app: &mut App, dir: &std::path::Path) -> std::path::PathBuf {
    let shim = dir.join("clt-shim");
    TOOLS_PRESENT.with(|present| present.set(false));
    app.workstation_setup.developer_tools_shim =
        crate::clt_shim::Shim::at(shim.clone(), tools_present);
    shim
}

/// Whether new shells get the shim laid at `shim`: its wanted mark is there.
fn wanted(shim: &std::path::Path) -> bool {
    aterm_types::clt_shim::wanted_mark(shim).is_file()
}

/// A person's Stop ends the run's want of the tools, and the git-and-friends shim comes off new
/// shells with it: no shell opened after it is told the tools are on their way while nothing
/// installs them. The directory stays for the shells that have it, until the next launch.
#[test]
fn a_stop_takes_the_shim_off_new_shells() {
    let dir = aterm_tempfile::tempdir().expect("a state directory");
    let (mut app, _) = page();
    let shim = shim_in(&mut app, dir.path());
    app.want_developer_tools_install(4, true);
    assert!(
        shim.is_dir() && wanted(&shim),
        "a pass that wants the tools lays the shim"
    );
    app.stop_initial_setup_permissions();
    assert_eq!(app.workstation_setup.developer_tools, Install::Unasked);
    assert!(
        !wanted(&shim),
        "Stop withdrew the want: new shells do not get it"
    );
    assert!(shim.is_dir(), "the directory goes at the next launch");
}

/// The tab opening spends the automatic attempt, not the want: once the tools arrived, the next
/// pass says it no longer waits on them, and the shim comes off new shells, though the tab stays
/// opened for the launch. An older pass's word withdraws no newer want.
#[test]
fn tools_that_arrived_take_the_shim_off_new_shells_after_the_tab_opened() {
    let dir = aterm_tempfile::tempdir().expect("a state directory");
    let (mut app, _) = page();
    let shim = shim_in(&mut app, dir.path());
    app.want_developer_tools_install(4, true);
    assert!(wanted(&shim));
    app.workstation_setup.developer_tools = Install::Opened;
    app.want_developer_tools_install(3, false);
    assert!(wanted(&shim), "an older pass withdraws no newer want");
    app.want_developer_tools_install(5, false);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert!(
        !wanted(&shim),
        "no pass wants the tools: new shells do not get it"
    );
    assert!(shim.is_dir(), "the directory goes at the next launch");
}

/// The tab's program said the tools are installed: the shim comes off new shells at once, ahead
/// of the next pass's word, though a pass still wants them. A shell that has it runs the tools'
/// own programs through it until the next launch removes it.
#[test]
fn tools_the_tab_installed_take_the_shim_off_new_shells_at_once() {
    let dir = aterm_tempfile::tempdir().expect("a state directory");
    let (mut app, _) = page();
    let shim = shim_in(&mut app, dir.path());
    app.want_developer_tools_install(4, true);
    assert!(wanted(&shim));
    app.workstation_setup.developer_tools = Install::Opened;
    app.workstation_setup.developer_tools_session = vec![Terminal::running(77, false, None)];
    app.workstation_setup.developer_tools_attempt = Some(77);
    TOOLS_PRESENT.with(|present| present.set(true));
    app.developer_tools_install_ended(77, Some(aterm_pty::ChildExit::Code(0)));
    assert_eq!(app.workstation_setup.developer_tools_want, Some(4));
    assert!(
        !wanted(&shim),
        "the tab installed them: new shells do not get it"
    );
    assert!(shim.is_dir(), "the directory goes at the next launch");
}

/// A launch reconciles the shim an earlier launch left (one that ended mid-install, stopped, or
/// crashed): no pass wants the tools yet, so it goes with its mark, whether or not they are
/// installed.
#[test]
fn a_launch_removes_the_shim_an_earlier_launch_left() {
    for present in [false, true] {
        let dir = aterm_tempfile::tempdir().expect("a state directory");
        let shim = dir.path().join("clt-shim");
        std::fs::create_dir_all(&shim).unwrap();
        std::os::unix::fs::symlink("/nonexistent/aterm", shim.join("git")).unwrap();
        std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
        TOOLS_PRESENT.with(|tools| tools.set(present));
        let kept = crate::clt_shim::Shim::at(shim.clone(), tools_present);
        kept.reconcile(false);
        assert!(!kept.laid() && !shim.exists(), "present: {present}");
        assert!(!wanted(&shim), "present: {present}");
    }
}

/// An install that ended without the tools (the program said it did not finish, the account
/// proved no administrator's, or it ended any other way, or nobody knows how) leaves nothing
/// installing them: the shim comes off new shells at once, though the pass's want stands, and a
/// later pass that wants them lays it for no shell until an install starts again. Its directory
/// stays for the shells that have it, which a shim with no mark hands to Apple's own route.
/// NEGATIVE CONTROL: a shim that follows the want alone stays on new shells after each of these
/// ends, telling them the tools are on their way while nothing installs them.
#[test]
fn an_install_that_ended_unfinished_takes_the_shim_off_new_shells() {
    use aterm_pty::ChildExit;
    let not_admin = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN);
    for end in [
        Some(ChildExit::Code(1)),
        Some(ChildExit::Code(not_admin)),
        Some(ChildExit::Code(127)),
        Some(ChildExit::Signal(9)),
        None,
    ] {
        let dir = aterm_tempfile::tempdir().expect("a state directory");
        let (mut app, _) = page();
        let shim = shim_in(&mut app, dir.path());
        app.want_developer_tools_install(4, true);
        assert!(
            wanted(&shim),
            "{end:?}: a pass that wants the tools lays it"
        );
        app.workstation_setup.developer_tools = Install::Opened;
        app.workstation_setup.developer_tools_session = vec![Terminal::running(77, false, None)];
        app.workstation_setup.developer_tools_attempt = Some(77);
        app.developer_tools_install_ended(77, end);
        assert_eq!(
            app.workstation_setup.developer_tools_want,
            Some(4),
            "{end:?}: the want stands"
        );
        assert!(!wanted(&shim), "{end:?}: nothing installs them");
        assert!(
            shim.is_dir(),
            "{end:?}: the directory goes at the next launch"
        );
        app.want_developer_tools_install(5, true);
        assert!(
            !wanted(&shim),
            "{end:?}: a later pass's want alone puts it back on no shell"
        );
    }
}

/// EOF may arrive before an install's status. A retry can start in between: the old result is
/// retained without changing the retry's shim, account mark, wait or ready state.
#[test]
fn a_late_install_result_does_not_replace_the_retry_state() {
    use aterm_pty::ChildExit;
    use std::sync::atomic::Ordering;

    let low_data = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA);
    let not_admin = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN);
    for exit in [
        Some(ChildExit::Code(1)),
        Some(ChildExit::Code(0)),
        Some(ChildExit::Code(low_data)),
        Some(ChildExit::Code(not_admin)),
        Some(ChildExit::Code(127)),
        Some(ChildExit::Signal(9)),
        None,
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mark = dir.path().join("command-line-tools-tab");
        let (mut app, wid) = page();
        app.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        app.workstation_setup.developer_tools_probe = Some(idle);
        let shim = shim_in(&mut app, dir.path());
        let mut handlers = crate::InitialSetupHandlers::inert_for_test();
        handlers.tools_installed = || TOOLS_INSTALLED.with(|count| count.set(count.get() + 1));
        app.set_system_state_handlers_for_test(handlers);
        TOOLS_INSTALLED.with(|count| count.set(0));
        app.want_developer_tools_install(4, true);

        let previous = book(&mut app, wid, 4);
        let (landed, previous_read) = land(&mut app, wid, previous);
        assert!(landed);
        assert_eq!(drain(&previous_read), go_line());
        // The status has not been published yet. No process is consulted by exit_status.
        app.pool
            .get(previous)
            .unwrap()
            .child_reaped
            .store(true, Ordering::Release);
        assert!(app.exit_session_logical(previous).is_empty());
        assert!(!app.developer_tools_install_active());
        assert!(
            app.workstation_setup
                .developer_tools_session
                .iter()
                .any(|terminal| {
                    terminal.session == previous && terminal.finished && !terminal.ended
                })
        );

        assert!(app.open_system_state_tab());
        app.workstation_setup.developer_tools = Install::Pressed;
        let retry = test_support::book(&mut app, wid, pressed_tab_then());
        let (landed, retry_read) = land(&mut app, wid, retry);
        assert!(landed);
        assert_eq!(drain(&retry_read), go_line());
        assert!(app.developer_tools_install_active());
        assert!(wanted(&shim));

        if let Some(exit) = exit {
            app.pool
                .get(previous)
                .unwrap()
                .child_exit
                .set(exit)
                .unwrap();
            app.read_developer_tools_exit();
        } else {
            app.developer_tools_install_ended(previous, None);
        }
        assert!(
            app.workstation_setup
                .developer_tools_session
                .iter()
                .any(|terminal| {
                    terminal.session == previous && terminal.finished && terminal.ended
                })
        );
        assert_eq!(app.workstation_setup.developer_tools_attempt, Some(retry));
        assert!(
            !app.workstation_setup.developer_tools_unfinished,
            "{exit:?}"
        );
        assert!(!app.workstation_setup.developer_tools_not_admin, "{exit:?}");
        assert_eq!(
            app.workstation_setup.developer_tools_low_data, None,
            "{exit:?}"
        );
        assert!(
            app.workstation_setup
                .developer_tools_after
                .settling
                .is_none(),
            "{exit:?}"
        );
        assert_eq!(TOOLS_INSTALLED.with(std::cell::Cell::get), 0, "{exit:?}");
        assert_eq!(
            app.workstation_setup.developer_tools,
            Install::Opened,
            "{exit:?}"
        );
        assert_eq!(
            app.workstation_setup.developer_tools_want,
            Some(4),
            "{exit:?}"
        );
        assert!(mark.is_file(), "{exit:?}: the retry still owns the opening");
        assert!(wanted(&shim), "{exit:?}: the retry is still installing");
        assert_eq!(step(&mut app), Some(ToolsInstall::Password), "{exit:?}");
        assert!(drain(&retry_read).is_empty());

        app.developer_tools_install_ended(retry, Some(ChildExit::Code(1)));
        assert!(app.workstation_setup.developer_tools_unfinished);
        assert!(
            !wanted(&shim),
            "the current attempt's failure still ends its shim"
        );
    }
}

/// A launch whose account's tab opened at an earlier launch reads the tools unfinished: a pass
/// that wants them lays the shim for no new shell, as nothing installs them, until a person's
/// Install… starts an install, which puts it back.
/// NEGATIVE CONTROL: a launch that ignores the mark tells new shells the tools are on their way
/// with no tab open; a start that keeps them unfinished leaves the install's shells with Apple's
/// dialog.
#[test]
fn a_launch_after_an_unfinished_install_lays_the_shim_once_an_install_starts() {
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    std::fs::write(&mark, "opened\n").expect("a mark from an earlier launch");
    let launched =
        crate::workstation_setup::Host::for_launch_marked(None, true, Mark::at(mark.clone()));
    assert!(launched.developer_tools_unfinished, "the mark says so");
    let fresh = crate::workstation_setup::Host::for_launch_marked(
        None,
        true,
        Mark::at(dir.path().join("no-mark")),
    );
    assert!(!fresh.developer_tools_unfinished, "no tab opened before");

    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark);
    app.workstation_setup.developer_tools_unfinished = launched.developer_tools_unfinished;
    let shim = shim_in(&mut app, dir.path());
    app.want_developer_tools_install(4, true);
    assert!(!wanted(&shim), "nothing installs them yet");
    app.workstation_setup.developer_tools = Install::Pressed;
    let id = test_support::book(&mut app, wid, pressed_tab_then());
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(drain(&read), go_line());
    assert!(wanted(&shim), "the install started: new shells get it");
}

/// Low Data Mode holds the download back: the tab's program says so and exits with its own code,
/// and that opening is not spent. The account's mark is taken back, the run's want stands, the
/// row waits calmly, and the automatic tab waits while the mode is on and opens by itself once
/// the network watch reads it off.
#[test]
fn low_data_mode_spends_no_automatic_opening() {
    let dir = aterm_tempfile::tempdir().expect("a config directory");
    let mark = dir.path().join("command-line-tools-tab");
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
    app.want_developer_tools_install(4, true);
    let (window, ask, then) = app.begin_developer_tools_install(aterm).expect("due");
    let id = test_support::book_as(&mut app, window, &ask, then);
    let (landed, _read) = land(&mut app, wid, id);
    assert!(landed);
    assert!(mark.is_file(), "the landing spent the automatic opening");
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);

    app.developer_tools_install_ended(
        id,
        Some(aterm_pty::ChildExit::Code(i32::from(
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA,
        ))),
    );
    assert!(!mark.exists(), "that opening installed nothing: not spent");
    assert!(!app.workstation_setup.developer_tools_mark.opened());
    assert_eq!(app.workstation_setup.developer_tools, Install::Wanted(4));
    assert_eq!(step(&mut app), Some(ToolsInstall::LowData));
    let missing = tools_missing(&app);
    app.publish_system_state(missing);
    assert_eq!(shown(&app), Some(ToolsInstall::LowData));
    assert_eq!(menu(&app), None, "a calm wait: no menu line");
    assert!(
        !app.developer_tools_install_due(true, false, false),
        "the automatic tab waits while Low Data Mode is on"
    );

    app.workstation_setup.developer_tools_network = Some(NetworkWatch::stand_in());
    app.developer_tools_low_data(false);
    assert!(
        app.developer_tools_install_due(true, false, false),
        "the tab opens by itself once Low Data Mode is off"
    );
    assert_ne!(step(&mut app), Some(ToolsInstall::LowData));
}

/// An account that is none of the Mac's administrators may not install the tools, as the tab's
/// program found: the row says so in place of Install…, with no menu line, and no tab opens by
/// itself again.
#[test]
fn a_non_administrator_is_told_and_no_tab_opens_by_itself() {
    let (mut app, _) = page();
    app.want_developer_tools_install(4, true);
    app.workstation_setup.developer_tools = Install::Opened;
    app.workstation_setup.developer_tools_session = vec![Terminal::running(77, false, None)];
    app.workstation_setup.developer_tools_attempt = Some(77);
    app.developer_tools_install_ended(
        77,
        Some(aterm_pty::ChildExit::Code(i32::from(
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN,
        ))),
    );
    app.publish_system_state(tools_missing(&app));
    assert_eq!(shown(&app), Some(ToolsInstall::NotAdmin));
    assert_eq!(menu(&app), None, "nothing here changes it");
    app.workstation_setup.developer_tools = Install::Wanted(4);
    assert!(!app.developer_tools_install_due(true, false, false));
}
