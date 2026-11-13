// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn tools_arrival_ignores_a_projects_developer_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = aterm_tempfile::tempdir().unwrap();
    let shim = root.path().join("shim");
    let selected = root.path().join("selected developer");
    for file in [shim.join("cc"), selected.join("usr/bin/clang")] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"installed executable").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let link = root.path().join("xcode_select_link");
    std::os::unix::fs::symlink(&selected, &link).unwrap();
    let host = atpkg::prereq::CcHost {
        shim_dir: Some(shim.clone()),
        xcode_select_link: link,
        default_developer_dir: root.path().join("no-default-tools"),
        xcode_developer_dir: None,
        developer_dir_override: Some(root.path().join("missing project Xcode")),
        ..atpkg::prereq::CcHost::this_machine()
    };
    let path = shim.as_os_str();
    assert_eq!(
        atpkg::prereq::installed_with(Some(path), &host),
        Some(false)
    );
    assert!(developer_tools_ready_on(true, path, host.clone()));
    assert!(!developer_tools_ready_on(false, path, host.clone()));
    std::fs::remove_file(selected.join("usr/bin/clang")).unwrap();
    let project_clang = host
        .developer_dir_override
        .as_ref()
        .unwrap()
        .join("usr/bin/clang");
    std::fs::create_dir_all(project_clang.parent().unwrap()).unwrap();
    std::fs::write(&project_clang, b"project compiler").unwrap();
    std::fs::set_permissions(&project_clang, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(atpkg::prereq::installed_with(Some(path), &host), Some(true));
    assert!(!developer_tools_ready_on(true, path, host));
}

#[test]
fn unattended_setup_starts_the_selected_tools_install_without_another_press() {
    let mut app = App::headless_for_test();
    let permissions = Some(crate::InitialSetupPermissionPolicy {
        revision: 4,
        allowed: true,
    });
    app.observe_initial_setup_permissions(permissions);
    app.publish_system_state(crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        permissions,
        run: Some(crate::InitialSetupRunPolicy {
            revision: 4,
            allowed: true,
        }),
        ..crate::SetupStatus::default()
    });
    app.want_developer_tools_install(4, true);
    assert!(app.developer_tools_install_due(false));
    app.note_system_state_touch();
    app.note_system_state_apply(crate::system_state::Admission::Person);
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(permissions);
    assert!(
        app.developer_tools_install_due(false),
        "an additional press does not change the automatic admission"
    );
}

fn policy(revision: u64) -> Option<crate::InitialSetupPermissionPolicy> {
    Some(crate::InitialSetupPermissionPolicy {
        revision,
        allowed: true,
    })
}

fn observe_run(
    app: &mut App,
    revision: u64,
    allowed: bool,
    permissions: Option<crate::InitialSetupPermissionPolicy>,
) {
    let mut status = app.system_state_host.latest_status().clone();
    status
        .plan
        .get_or_insert_with(aterm_types::macos_setup::Plan::default);
    status.permissions = permissions;
    status.run = Some(crate::InitialSetupRunPolicy { revision, allowed });
    if allowed {
        app.note_system_state_touch();
        app.note_system_state_apply(crate::system_state::Admission::Person);
        app.accepted_initial_setup_apply();
    }
    app.observe_initial_setup_permissions(permissions);
    app.publish_system_state(status);
}

fn stopped() -> App {
    let mut app = App::headless_for_test();
    observe_run(&mut app, 4, true, policy(4));
    app.stop_initial_setup_permissions();
    app
}

fn accept_later_apply(app: &mut App) {
    use crate::native_app::{ActionInvocation, AppEvent};
    use crate::native_ui::ActionId;
    use crate::workstation_setup::{Request, WorkstationSetupDecision};
    use aterm_types::macos_setup::{Plan, Selection, Setting};

    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    app.set_system_state_handlers_for_test(crate::InitialSetupHandlers::inert_for_test());
    let mut plan = Plan {
        selection: Selection::none(),
        ..Plan::default()
    };
    plan.selection.set(Setting::Repositories, true);
    let (send, receive) = std::sync::mpsc::channel();
    let request = Request::new(plan, send);
    app.headless = false;
    app.offer_system_state(request.clone());
    app.headless = true;
    assert!(request.pending());
    let (_, view) = app.active_native_view(wid).unwrap();
    app.with_gesture_source(false, |app| {
        app.interact_system_state(wid);
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                // A press on a group answers the offer.
                id: ActionId::new("system-state/group/1/leave"),
                value: None,
            }),
        )
    })
    .unwrap();
    let mut answer = receive.try_recv().unwrap();
    assert!(matches!(
        answer.decision,
        WorkstationSetupDecision::Begin(_)
    ));
    answer.receipt.take().unwrap().accept();
    app.resolve_system_state_apply();
}

#[test]
fn clt_install_a_pass_withdraws_its_own_revisions_want_and_never_a_newer_one() {
    assert_eq!(Install::Wanted(4).withdrawn_by(4), Install::Unasked);
    assert_eq!(Install::Wanted(4).withdrawn_by(5), Install::Unasked);
    assert_eq!(Install::Wanted(5).withdrawn_by(4), Install::Wanted(5));
    let opening = |wanted| Install::Opening { wanted };
    assert_eq!(opening(Some(4)).withdrawn_by(4), opening(None));
    assert_eq!(opening(Some(5)).withdrawn_by(4), opening(Some(5)));
    assert_eq!(Install::Opened.withdrawn_by(9), Install::Opened);
}

#[test]
fn clt_install_a_delayed_want_cannot_rearm_a_stopped_run() {
    let mut app = stopped();
    app.want_developer_tools_install(4, true);
    assert!(
        !app.developer_tools_install_due(false),
        "a delayed pass callback must not reopen installation after Stop"
    );
}

/// A scratch world's launch (a check's or a lab's bundle, `ATERM_STATE_HOME` or `CI` set)
/// never opens the automatic tab, for missing tools or owed root steps alike: it would ask
/// the desk's person for a password and take the keys the check types into its own session
/// (the 0.154.0 cut's measure, on a Mac without the name rule: 30 keys queued, none written).
#[test]
fn clt_install_a_scratch_world_never_opens_the_tab() {
    let mut app = App::headless_for_test();
    observe_run(&mut app, 4, true, policy(4));
    app.want_developer_tools_install(4, true);
    app.workstation_setup.developer_tools_root_owed = RootSteps::OTHER;
    assert!(
        app.developer_tools_install_due(false),
        "CONTROL: outside a scratch world the owed tab is due"
    );
    app.workstation_setup.developer_tools_scratch_world = true;
    assert!(!app.developer_tools_install_due(false));
}

/// A person typing (a vault password into System State, say) holds the tab back until the
/// keyboard was quiet for 2.5 s, so the tab that takes the window's keys catches none of it.
#[test]
fn clt_install_the_tab_waits_out_a_typing_person() {
    let mut app = App::headless_for_test();
    observe_run(&mut app, 4, true, policy(4));
    app.want_developer_tools_install(4, true);
    assert!(app.developer_tools_install_due(false));
    let now = std::time::Instant::now();
    app.last_keystroke_at = Some(now);
    assert!(
        !app.developer_tools_install_due(false),
        "a key typed just now holds the tab"
    );
    app.last_keystroke_at = now.checked_sub(std::time::Duration::from_secs(3));
    assert!(
        app.developer_tools_install_due(false),
        "a quiet keyboard lets it open"
    );
}

#[test]
fn clt_install_a_run_can_keep_an_older_permissions_approval() {
    let mut app = App::headless_for_test();
    let permissions = Some(crate::InitialSetupPermissionPolicy {
        revision: 4,
        allowed: true,
    });
    app.note_system_state_touch();
    app.note_system_state_apply(crate::system_state::Admission::Person);
    app.accepted_initial_setup_apply();
    app.observe_initial_setup_permissions(permissions);
    app.publish_system_state(crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        permissions,
        run: Some(crate::InitialSetupRunPolicy {
            revision: 5,
            allowed: true,
        }),
        ..crate::SetupStatus::default()
    });
    app.want_developer_tools_install(5, true);
    assert!(
        app.developer_tools_install_due(false),
        "the current run's repository tools are independent of its carried Permissions approval"
    );
}

#[test]
fn clt_install_a_durable_stop_does_not_reopen_tools_without_a_local_stop() {
    let mut app = App::headless_for_test();
    app.observe_initial_setup_permissions(policy(4));
    app.publish_system_state(crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        permissions: policy(4),
        run: Some(crate::InitialSetupRunPolicy {
            revision: 4,
            allowed: false,
        }),
        finished: true,
        ..crate::SetupStatus::default()
    });
    app.want_developer_tools_install(4, true);
    assert!(
        !app.developer_tools_install_due(false),
        "a Stop read from durable records withdraws installation even when another process asked for it"
    );
}

#[test]
fn clt_install_a_new_apply_only_rearms_its_own_repository_request() {
    let mut app = stopped();
    accept_later_apply(&mut app);
    app.want_developer_tools_install(5, true);
    assert!(
        !app.developer_tools_install_due(false),
        "the new admission still needs its fresh policy"
    );
    observe_run(&mut app, 5, true, policy(5));
    assert!(
        app.developer_tools_install_due(false),
        "Repositories may install tools with Permissions left as is"
    );
    app.want_developer_tools_install(4, false);
    assert!(
        app.developer_tools_install_due(false),
        "an older pass cannot withdraw the new run's request"
    );
    app.want_developer_tools_install(4, true);
    assert!(
        app.developer_tools_install_due(false),
        "an older pass cannot replace the new run's request"
    );
    assert_eq!(app.workstation_setup.developer_tools, Install::Wanted(5));
    observe_run(&mut app, 4, false, policy(4));
    assert!(
        !app.developer_tools_install_due(false),
        "an older denied run policy cannot authorize the current run's install"
    );
    observe_run(&mut app, 5, true, policy(5));
    assert!(app.developer_tools_install_due(false));
    app.want_developer_tools_install(6, false);
    assert!(
        !app.developer_tools_install_due(false),
        "a newer pass that no longer waits withdraws the older want"
    );
}

#[test]
fn clt_install_a_stopped_or_replaced_tab_never_starts_the_install_command() {
    for replaced in [false, true] {
        let mut app = App::headless_for_test();
        observe_run(&mut app, 4, true, policy(4));
        app.workstation_setup.developer_tools = Install::Opened;
        let mut commands = 0;
        app.dispatch_developer_tools_install(4, |_| commands += 1);
        assert_eq!(commands, 1, "the live install has an inert command sink");
        app.stop_initial_setup_permissions();
        if replaced {
            accept_later_apply(&mut app);
            observe_run(&mut app, 5, true, policy(5));
        }
        app.dispatch_developer_tools_install(4, |_| commands += 1);
        assert_eq!(
            commands, 1,
            "Stop or a newer accepted plan retires the old tab's command"
        );
        app.want_developer_tools_install(5, true);
        assert_eq!(
            app.workstation_setup.developer_tools,
            Install::Opened,
            "an opened tab retains the once-per-launch bound"
        );
    }
}

#[test]
fn clt_install_a_changing_read_withholds_only_run_eligibility_until_a_settled_read() {
    let mut app = App::headless_for_test();
    let status = crate::SetupStatus {
        plan: Some(aterm_types::macos_setup::Plan::default()),
        permissions: policy(4),
        run: Some(crate::InitialSetupRunPolicy {
            revision: 5,
            allowed: true,
        }),
        ..crate::SetupStatus::default()
    };
    app.note_system_state_touch();
    app.note_system_state_apply(crate::system_state::Admission::Person);
    app.accepted_initial_setup_apply();
    app.receive_system_state_status(Some(0), status.clone());
    app.want_developer_tools_install(5, true);
    assert!(app.developer_tools_install_due(false));
    let changing = crate::SetupStatus {
        changing: true,
        ..crate::SetupStatus::default()
    };
    app.receive_system_state_status(Some(1), changing.clone());
    assert!(
        app.developer_tools_install_due(false),
        "a stale status generation cannot withdraw the current run"
    );
    app.receive_system_state_status(Some(0), changing);
    assert!(!app.developer_tools_install_due(false));
    let mut commands = 0;
    app.dispatch_developer_tools_install(5, |_| commands += 1);
    assert_eq!(
        commands, 0,
        "a tab landing during the unknown read stays untyped"
    );
    assert_eq!(app.system_state_host.latest_status().plan, status.plan);
    assert_eq!(
        app.system_state_host.latest_status().permissions,
        status.permissions
    );
    app.receive_system_state_status(Some(0), status);
    assert!(app.developer_tools_install_due(false));
    app.dispatch_developer_tools_install(5, |_| commands += 1);
    assert_eq!(
        commands, 1,
        "the settled current run restores the same deferred work"
    );
}

/// A launch's or a pass's ask makes the install wanted; its tab opens unless a macOS dialog is
/// up, whatever the window shows, and never twice in a launch. A Stop drops a want not yet
/// opened, and a pass's later word never drops a launch's.
#[test]
fn the_install_tab_opens_once_unless_a_dialog_is_up() {
    let wanted = Install::default().asked(4);
    assert_eq!(wanted, Install::Wanted(4));
    assert_eq!(wanted.asked(4), Install::Wanted(4));
    assert!(wanted.due(false));
    assert!(!wanted.due(true), "a macOS dialog is up");
    assert!(!Install::Unasked.due(false), "nobody asked");
    assert_eq!(
        wanted.withdrawn(),
        Install::Unasked,
        "withdrawn before it opened"
    );
    assert_eq!(
        Install::Opened.asked(4),
        Install::Opened,
        "never twice a launch"
    );
    assert_eq!(Install::Opened.withdrawn(), Install::Opened);
    assert!(!Install::Opened.due(false));

    let launch = Install::Unasked.asked(LAUNCH_WANT);
    assert_eq!(launch.asked(9), launch, "a pass replaces no launch's want");
    assert_eq!(launch.withdrawn_by(9), launch, "nor withdraws it");
    assert_eq!(launch.withdrawn(), Install::Unasked, "a Stop does");
}

/// A launch's want asks nobody first: no run, no person's admission, no page, as a new Mac's
/// one password comes first. A hold and a Stop kept in the run's records still stand.
/// NEGATIVE CONTROL: durable and local Stop records hold both launch and pass requests.
#[test]
fn a_launchs_want_opens_with_no_run_and_no_person() {
    let mut app = App::headless_for_test();
    app.want_developer_tools_install(LAUNCH_WANT, true);
    assert!(
        app.developer_tools_install_due(false),
        "with nothing published and nobody at the page"
    );
    assert!(!app.developer_tools_install_due(true), "a dialog is up");

    let mut stopped = App::headless_for_test();
    stopped.publish_system_state(crate::SetupStatus {
        run: Some(crate::InitialSetupRunPolicy {
            revision: 4,
            allowed: false,
        }),
        finished: true,
        ..crate::SetupStatus::default()
    });
    stopped.want_developer_tools_install(LAUNCH_WANT, true);
    assert!(
        !stopped.developer_tools_install_due(false),
        "a durable Stop"
    );

    let mut local = App::headless_for_test();
    local.want_developer_tools_install(LAUNCH_WANT, true);
    local.stop_initial_setup_permissions();
    assert_eq!(local.workstation_setup.developer_tools, Install::Unasked);
    assert!(!local.developer_tools_install_due(false), "a person's Stop");
    local.want_developer_tools_install(4, true);
    assert!(
        !local.developer_tools_install_due(false),
        "a delayed pass rearms nothing"
    );
}

/// A launch with a window asks for the tools where they are missing and the account never had
/// the tab; the watch that takes the shim off runs where it reads this Mac, whether or not the
/// tab may open.
/// NEGATIVE CONTROL: a Mac with them, or a headless instance, wants nothing, and a test's
/// reading starts no watch.
#[test]
fn a_launch_asks_only_on_a_mac_without_the_tools_and_only_once_per_account() {
    let at = |window, missing, opened| launch_install(window, window, missing, opened);
    assert_eq!(
        at(true, true, false),
        LaunchInstall {
            watch: true,
            want: true
        }
    );
    assert_eq!(
        at(true, true, true),
        LaunchInstall {
            watch: true,
            want: false
        }
    );
    for (window, missing) in [(true, false), (false, true), (false, false)] {
        assert_eq!(
            at(window, missing, false),
            LaunchInstall {
                watch: false,
                want: false
            }
        );
    }
    assert_eq!(
        launch_install(true, false, true, false),
        LaunchInstall {
            watch: false,
            want: true
        },
        "a test's window wants them and starts no watch"
    );
}

/// The launch's want is made where the window's setup host is: a host built over a shim that
/// finds the tools missing wants them as [`LAUNCH_WANT`] from its first moment, and an app
/// that keeps it opens the tab with no pass, no run and no page. That is the host the launch
/// builds ([`crate::workstation_setup::Host::for_launch`]), over the shim it reconciled
/// ([`crate::clt_shim::Shim::for_launch`]), so no launch can drop the step and keep the host.
/// NEGATIVE CONTROL: the same host over a shim that finds the tools, in a headless instance,
/// or with the account's mark, wants nothing.
#[cfg(unix)]
#[test]
fn the_launchs_setup_host_wants_the_tools_from_its_start_where_they_are_missing() {
    let tmp = aterm_tempfile::tempdir().expect("a state directory");
    let host = |headless: bool, present: fn() -> bool, mark: &str| {
        crate::workstation_setup::Host::for_launch_marked(
            None,
            headless,
            Mark::at(tmp.path().join(mark)),
            crate::clt_shim::Shim::at(tmp.path().join("clt-shim"), present),
        )
    };
    let missing = host(false, || false, "no-mark");
    assert_eq!(missing.developer_tools, Install::Wanted(LAUNCH_WANT));
    assert_eq!(missing.developer_tools_want, Some(LAUNCH_WANT));
    let mut app = App::headless_for_test();
    app.workstation_setup = missing;
    assert!(
        app.developer_tools_install_due(false),
        "the launch's tab is due with nothing published"
    );

    for (headless, present, mark) in [
        (false, (|| true) as fn() -> bool, "no-mark"),
        (true, || false, "no-mark"),
    ] {
        let host = host(headless, present, mark);
        assert_eq!(host.developer_tools, Install::Unasked, "{headless}");
        assert_eq!(host.developer_tools_want, None, "{headless}");
    }
    std::fs::write(
        tmp.path().join("declined"),
        format!("declined {} {} 0 1\n", aterm_stamp::build_id(), unix_now()),
    )
    .unwrap();
    let declined = host(false, || false, "declined");
    assert_eq!(
        declined.developer_tools,
        Install::Unasked,
        "a person declined the last ask, inside its pause"
    );
    std::fs::write(
        tmp.path().join("lapsed"),
        format!("declined {} 1 0 1\n", aterm_stamp::build_id()),
    )
    .unwrap();
    assert_eq!(
        host(false, || false, "lapsed").developer_tools,
        Install::Wanted(LAUNCH_WANT),
        "a decline whose pause passed is asked again by itself"
    );
    super::landing_tests::asking_now(&tmp.path().join("opened"));
    let asking = host(false, || false, "opened");
    assert_eq!(
        asking.developer_tools,
        Install::Wanted(LAUNCH_WANT),
        "another attempt runs: the want stands, its tab held"
    );
    assert!(asking.developer_tools_mark.held().is_some());
}

/// On a Mac that has the tools, the launch's reading that the elevation still owes a root
/// step (developer mode, the naming rule) wants the tab as [`LAUNCH_WANT`] by itself, with no
/// press, and the tools' watch ending keeps that want. NEGATIVE CONTROLS: a reading that owes
/// nothing leaves the host unasked and no tab due; another attempt running holds an owed
/// reading's tab; and an unreadable step owes nothing.
#[cfg(unix)]
#[test]
fn a_mac_with_the_tools_still_gets_the_one_elevation_for_what_is_owed() {
    let tmp = aterm_tempfile::tempdir().expect("a state directory");
    let app_with = |mark: &str| {
        let mut app = App::headless_for_test();
        app.workstation_setup = crate::workstation_setup::Host::for_launch_marked(
            None,
            false,
            Mark::at(tmp.path().join(mark)),
            crate::clt_shim::Shim::at(tmp.path().join("clt-shim"), || true),
        );
        app
    };
    let mut owed = app_with("no-mark");
    assert_eq!(owed.workstation_setup.developer_tools, Install::Unasked);
    owed.root_steps_owed(RootSteps::OTHER);
    assert_eq!(
        owed.workstation_setup.developer_tools,
        Install::Wanted(LAUNCH_WANT)
    );
    assert_eq!(
        owed.workstation_setup.developer_tools_want,
        Some(LAUNCH_WANT)
    );
    owed.workstation_setup.developer_tools_low_data = Some(true);
    assert!(
        owed.developer_tools_install_due(false),
        "due with no press, Low Data Mode or not: nothing downloads"
    );
    owed.developer_tools_watch(None);
    assert_eq!(
        owed.workstation_setup.developer_tools_want,
        Some(LAUNCH_WANT),
        "the tools being there ends no want the elevation still has"
    );

    let mut settled = app_with("no-mark");
    settled.root_steps_owed(RootSteps::NONE);
    assert_eq!(settled.workstation_setup.developer_tools, Install::Unasked);
    assert!(!settled.developer_tools_install_due(false));

    super::landing_tests::asking_now(&tmp.path().join("opened"));
    let mut marked = app_with("opened");
    marked.root_steps_owed(RootSteps::OTHER);
    assert!(
        !marked.developer_tools_install_due(false),
        "another attempt runs"
    );

    let gone = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    let denied = || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    use aterm_containment::consent::DebuggerMember::Admin;
    use aterm_containment::consent::DebuggerMode::{Disabled, Enabled, Unknown};
    assert!(root_steps_owed_by(Disabled, Admin, Ok((0, 0o440, true)), false, false).any());
    assert!(root_steps_owed_by(Enabled, Admin, gone(), false, false).any());
    assert!(root_steps_owed_by(Enabled, Admin, Ok((501, 0o440, true)), false, false).any());
    assert!(!root_steps_owed_by(Enabled, Admin, Ok((0, 0o440, true)), false, false).any());
    assert!(
        root_steps_owed_by(Enabled, Admin, Ok((0, 0o440, true)), false, true).any(),
        "the broker alone owes the elevation"
    );
    assert!(!root_steps_owed_by(Unknown, Admin, denied(), false, false).any());
    assert!(
        !root_steps_owed_by(Enabled, Admin, gone(), true, false).any(),
        "refused stands"
    );
}

/// An enabled developer mode still needs the root step when this account is in neither
/// permitted group. Admins and developers already have authorization, and missing readings
/// never cause a password request of their own.
#[cfg(unix)]
#[test]
fn enabled_mode_owes_only_a_definitely_unauthorized_console_account() {
    use aterm_containment::consent::DebuggerMember::{Admin, Developer, NotMember, Unknown};
    use aterm_containment::consent::DebuggerMode;
    for (mode, member, owed) in [
        (DebuggerMode::Enabled, NotMember, true),
        (DebuggerMode::Enabled, Admin, false),
        (DebuggerMode::Enabled, Developer, false),
        (DebuggerMode::Enabled, Unknown, false),
        (DebuggerMode::Unknown, NotMember, false),
    ] {
        assert_eq!(
            root_steps_owed_by(mode, member, Ok((0, 0o440, true)), false, false),
            RootSteps {
                debugger: owed,
                other: false
            },
            "{mode:?} {member:?}"
        );
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn assert_debugger_setup_admission(
    name: &str,
    ran: aterm_machines::port::Ran,
    mode: aterm_containment::consent::DebuggerMode,
    wanted: bool,
) {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, ProbeGate, ToolRun, probe_debugger_with,
    };
    use aterm_machines::init_clt::{CommandLineTools, Found};
    use aterm_machines::items::{Ctx, Item as _};
    use aterm_machines::model::{HostFacts, Probe};
    use aterm_machines::port::{Fake, Stat};
    use aterm_machines::root_steps::{self, Owed};
    use std::path::Path;

    let tmp = aterm_tempfile::tempdir().expect("isolated launch");
    let debugger = probe_debugger_with(
        ProbeGate::on(),
        Some(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
        |_| ToolRun::Exited {
            code: ran.status,
            stdout: String::from_utf8(ran.stdout.clone()).expect("fixture stdout"),
            stderr: String::from_utf8(ran.stderr.clone()).expect("fixture stderr"),
        },
        || DebuggerMember::Admin,
    );
    assert_eq!(debugger.mode, mode, "{name}");
    let enabled = match mode {
        DebuggerMode::Enabled => Some(true),
        DebuggerMode::Disabled => Some(false),
        DebuggerMode::Unrecognized | DebuggerMode::Unknown => None,
    };
    let child_mode = root_steps::developer_mode(&ran);
    let sys = Fake::root();
    sys.on("/usr/sbin/DevToolsSecurity -status", ran).stat_of(
        aterm_types::macos_setup::ADMITTED_NAME_RULE,
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o440,
            dir: false,
            symlink: false,
            len: 1,
        },
    );
    let child_owed = root_steps::owed(&sys).contains(&Owed::Debugger);
    let child_ready = if !wanted {
        let item = CommandLineTools::new(
            Box::new(|_| Found::Present("tools already installed".to_owned())),
            tmp.path().join("aterm"),
        )
        .with_owed(Box::new(root_steps::owed));
        let ctx = Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: String::new(),
            cache_dir: tmp.path().to_owned(),
            disrupt: false,
        };
        Some(matches!(item.probe(&sys, &ctx), Probe::Ok { .. }))
    } else {
        None
    };
    let mut app = App::headless_for_test();
    app.workstation_setup = crate::workstation_setup::Host::for_launch_marked(
        None,
        false,
        Mark::at(tmp.path().join(name)),
        crate::clt_shim::Shim::at(tmp.path().join("clt-shim"), || true),
    );
    app.root_steps_owed(root_steps_owed_by(
        debugger.mode,
        debugger.member,
        Ok((0, 0o440, true)),
        false,
        false,
    ));
    assert_eq!(
        (
            child_mode,
            child_owed,
            app.developer_tools_install_due(false)
        ),
        (enabled, wanted, wanted),
        "{name}: canonical mode, child root debt, automatic GUI tab"
    );
    assert!(child_ready.is_none_or(|ready| ready), "{name}");
    assert!(!app.workstation_setup.developer_tools_mark.held().is_some());
}

/// The launch's real debugger probe must not want a setup tab that its own `base.clt`
/// program finds has nothing to do. A recognized unknown rule stays distinct from a
/// failed reading, but neither grants elevation. A definitely disabled rule still does.
#[cfg(all(target_os = "macos", debug_assertions))]
#[test]
fn an_unknown_debugger_rule_does_not_open_a_setup_tab_with_no_actionable_root_step() {
    use aterm_containment::consent::DebuggerMode;
    use aterm_machines::port::Ran;
    for (name, status, mode, wanted) in [
        (
            "unknown-rule",
            "Developer mode rights are currently in an unknown state.\n",
            DebuggerMode::Unrecognized,
            false,
        ),
        (
            "unread",
            "the authorization rule could not be read\n",
            DebuggerMode::Unknown,
            false,
        ),
        (
            "disabled",
            "Developer mode is currently disabled.\n",
            DebuggerMode::Disabled,
            true,
        ),
    ] {
        assert_debugger_setup_admission(name, Ran::ok(status), mode, wanted);
    }
}

/// Both startup producers recognize the supported status sentences, including stderr.
#[cfg(all(target_os = "macos", debug_assertions))]
#[test]
fn debugger_setup_uses_supported_status_formats_in_both_producers() {
    use aterm_containment::consent::DebuggerMode;
    use aterm_machines::port::Ran;
    assert_debugger_setup_admission(
        "disabled-no-adverb",
        Ran::ok("Developer mode is disabled.\n"),
        DebuggerMode::Disabled,
        true,
    );
    assert_debugger_setup_admission(
        "disabled-stderr",
        Ran::exit(0, "Developer mode is now disabled.\n"),
        DebuggerMode::Disabled,
        true,
    );
    assert_debugger_setup_admission(
        "enabled-no-adverb",
        Ran::ok("Developer mode is enabled.\n"),
        DebuggerMode::Enabled,
        false,
    );
}

/// A malformed substring or disagreeing streams cannot admit a root effect or a setup tab.
#[cfg(all(target_os = "macos", debug_assertions))]
#[test]
fn debugger_setup_rejects_contradictory_or_malformed_status_in_both_producers() {
    use aterm_containment::consent::DebuggerMode;
    use aterm_machines::port::Ran;
    assert_debugger_setup_admission(
        "contradictory-streams",
        Ran {
            status: Some(0),
            stdout: b"Developer mode is currently disabled.\n".to_vec(),
            stderr: b"Developer mode is currently enabled.\n".to_vec(),
        },
        DebuggerMode::Unknown,
        false,
    );
    assert_debugger_setup_admission(
        "malformed-prefix",
        Ran::ok("diagnostic: Developer mode is currently disabled.\n"),
        DebuggerMode::Unknown,
        false,
    );
}

/// The account episode is durable through its directory entry, so a power loss after
/// landing cannot grant another automatic request. A second claim while the first attempt's
/// asker runs writes nothing.
#[test]
#[cfg(unix)]
fn the_ask_episode_is_written_durably_with_its_directory() {
    let dir = aterm_tempfile::tempdir().expect("a state directory");
    let path = dir.path().join("command-line-tools-tab");
    let mut mark = Mark::at(path.clone());
    assert!(!mark.held().is_some());
    let recording = aterm_durable::seam::Recording::start();
    assert!(mark.claim());
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Durable, aterm_durable::Flush::Durable],
        "the mark's file, then its directory"
    );
    assert!(mark.held().is_some() && path.is_file());
    assert!(!mark.claim(), "an attempt runs");
    assert!(recording.take().is_empty(), "written once");
}

/// A change of the account's record made on the window's loop gives up on an asker that
/// holds the record's lock and does not let go (an `aterm init` stopped, swapped out or on a
/// stuck disk) within [`aterm_types::clt_mark::LOOP_LOCK_WAIT`], rather than wait out a
/// terminal's minute-long hang detector with every window frozen behind it. The claim that
/// failed wrote nothing, and holds the automatic ask on its own schedule.
/// NEGATIVE CONTROL: once that asker lets go, the same window's claim takes the episode. A
/// window whose change waited like a terminal's would still be waiting at half the
/// terminal's wait, where this test's stand-in for the stalled asker lets go (so the mutant
/// costs half a minute, never the whole one): that change then succeeds, and is caught.
#[test]
#[cfg(unix)]
fn a_stalled_holder_of_the_records_lock_never_holds_the_windows_loop() {
    let dir = aterm_tempfile::tempdir().expect("a state directory");
    let path = dir.path().join("command-line-tools-tab");
    let mut mark = Mark::at(path.clone());
    assert!(mark.held().is_none(), "PRECONDITION: an ask may go");
    let holder = std::rc::Rc::new(std::cell::RefCell::new(Some(
        aterm_types::clt_mark::Files::lock(&aterm_types::clt_mark::Live, &path).unwrap(),
    )));
    let outwaited = std::rc::Rc::new(std::cell::Cell::new(false));
    let entered = std::time::Instant::now();
    let claimed = aterm_uds::file_lock::with_lock_wait_observer(
        {
            let (holder, outwaited) = (holder.clone(), outwaited.clone());
            move |_| {
                if entered.elapsed() >= aterm_types::clt_mark::LOCK_WAIT / 2 {
                    outwaited.set(true);
                    drop(holder.borrow_mut().take());
                }
            }
        },
        || mark.claim(),
    );
    assert!(
        !outwaited.get(),
        "the window's change waited for the lock like a terminal's"
    );
    assert!(!claimed, "a stalled holder fails the window's claim");
    assert!(!path.exists(), "the claim that failed wrote nothing");
    assert_eq!(
        mark.held(),
        Some("the last claim of the ask failed; it is tried again on its schedule")
    );
    drop(holder.borrow_mut().take());
    mark.missed = 0;
    assert!(
        mark.claim(),
        "once the holder lets go, the claim takes the episode"
    );
}

/// The tab's own program is the install item alone, which asks the one password, under the
/// tab's variable and with no shell between: nothing is typed but its go line.
#[test]
fn the_tab_runs_the_command_line_tools_item_as_its_own_program() {
    assert_eq!(
        tab_command(
            std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
            false
        ),
        [
            "/usr/bin/env",
            "ATERM_COMMAND_LINE_TOOLS_TAB=1",
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "init",
            "--only",
            "base.clt",
        ]
        .map(std::ffi::OsString::from)
    );
}

/// A placeholder counts as an install only while an installer holds it or a live run of
/// aterm's root half stamped it, and Apple's own window, beside the placeholder it made as it
/// opened or with none (it opened beside another install's, ended since), is where the person
/// finishes, at any age (design §3.3).
#[test]
fn the_placeholder_is_an_install_only_while_something_installs() {
    let read = |placeholder, install_process, apple_window| {
        activity(Reading {
            placeholder,
            install_process,
            apple_window,
            ..Reading::default()
        })
    };
    let stamped = |placeholder| {
        activity(Reading {
            placeholder,
            live_run: true,
            ..Reading::default()
        })
    };
    assert_eq!(
        stamped(true),
        Activity::Installing,
        "a live run between its steps holds it"
    );
    assert_eq!(stamped(false), Activity::Idle, "no placeholder, no install");
    assert_eq!(
        read(true, false, false),
        Activity::Idle,
        "a stale placeholder"
    );
    assert_eq!(
        read(false, true, false),
        Activity::Idle,
        "an update of something else"
    );
    assert_eq!(read(true, true, false), Activity::Installing);
    assert_eq!(
        read(true, true, true),
        Activity::Installing,
        "an install under way beside the window"
    );
    assert_eq!(
        read(false, false, true),
        Activity::AppleWindow,
        "with no placeholder beside it"
    );
    assert_eq!(
        read(true, false, true),
        Activity::AppleWindow,
        "beside the placeholder it made as it opened, pressed or not, at any age"
    );
    assert_eq!(read(false, false, false), Activity::Idle);
}
