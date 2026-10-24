// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::sync::mpsc;

use aterm_types::macos_setup::Setting;

use super::*;
use crate::WindowId;
use crate::native_app::{
    ActionInvocation, AppEvent, AppIndicators, AppInstanceId, CloseScope, SemanticInput,
};
use crate::native_ui::{ActionId, UiKey};
use crate::system_state::{Admission, fixture_status};
use crate::tab_model::ViewId;

thread_local! {
    static APPLY_RECEIPTS: RefCell<Vec<InitialSetupApplyReceipt>> = const {
        RefCell::new(Vec::new())
    };
    static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn note(call: String) {
    CALLS.with(|calls| calls.borrow_mut().push(call));
}

fn calls() -> Vec<String> {
    CALLS.with(|calls| std::mem::take(&mut *calls.borrow_mut()))
}

/// Handlers that record what they were asked and start everything; an
/// Apply's receipt waits in [`APPLY_RECEIPTS`] for the test to answer.
fn handlers() -> InitialSetupHandlers {
    InitialSetupHandlers {
        apply: |_, _, _, receipt| {
            APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().push(receipt));
            true
        },
        check: |plan| {
            note(format!("check {}", plan.vault_url));
            CheckStarted {
                reading: true,
                check: true,
            }
        },
        restore: || {
            note("restore".to_owned());
            true
        },
        status: SetupStatus::default,
        observe: |plan| {
            note(format!("observe {}", plan.vault_url));
            true
        },
        stop: || note("stop".to_owned()),
        settle: || note("settle".to_owned()),
        sync_on: || {
            note("sync on".to_owned());
            true
        },
    }
}

fn receipt() -> InitialSetupApplyReceipt {
    APPLY_RECEIPTS.with(|receipts| {
        receipts
            .borrow_mut()
            .pop()
            .expect("the Apply reached its handler")
    })
}

fn page(app: &mut App) -> (WindowId, AppInstanceId, ViewId) {
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    let (instance, view) = app.active_native_view(wid).unwrap();
    // The worker's first read names the plan in force (§3.5).
    app.publish_system_state(SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    });
    (wid, instance, view)
}

fn press(app: &mut App, wid: WindowId, view: ViewId, key: &str) {
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key),
            value: None,
        }),
    )
    .unwrap();
}

fn type_into(app: &mut App, wid: WindowId, view: ViewId, key: &str, text: &str) {
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(key),
            value: Some(SemanticInput::Text(text.into())),
        }),
    )
    .unwrap();
}

fn indicators(app: &App, instance: AppInstanceId, view: ViewId) -> AppIndicators {
    app.native_runtime
        .presentation(instance, view)
        .unwrap()
        .indicators
}

/// A status read asked for after everything the host has done so far.
fn fresh(app: &mut App, status: SetupStatus) {
    app.receive_system_state_status(Some(app.system_state_host.generation), status);
}

/// A read of a setup run going for the default plan.
fn running() -> SetupStatus {
    SetupStatus {
        busy: true,
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    }
}

fn take(app: &mut App) -> SystemStateAction {
    app.native_runtime
        .take_system_state_action()
        .expect("the page queued an action")
}

#[test]
fn linked_tabs_follow_status_and_admission_without_grid_locks() {
    use crate::tab_model::{SplitAxis, TabIndicators};
    let mut app = App::headless_for_test();
    let first_window = WindowId(0);
    assert!(app.open_system_state_tab());
    let (instance, first_view) = app.active_native_view(first_window).unwrap();
    let first_tab = app.windows[&first_window].tab_set.active_id().unwrap();
    app.stage_system_state_tab(first_window).unwrap();
    let second_window = app.detach_active_tab_logical().unwrap();
    let second_tab = app.windows[&second_window].tab_set.active_id().unwrap();
    let (session, _) = app.split_active_with_stub_terminal(second_window, SplitAxis::Vertical);
    app.switch_tab_in(first_window, 0);
    assert_ne!(
        app.windows[&first_window].tab_set.active_id(),
        Some(first_tab)
    );
    let tabs = [(first_window, first_tab), (second_window, second_tab)];
    let assert_flags = |app: &App, expected: TabIndicators| {
        for (wid, tab) in tabs {
            assert_eq!(
                app.windows[&wid]
                    .tab_set
                    .get(tab)
                    .unwrap()
                    .presentation
                    .indicators,
                expected
            );
        }
    };

    // Hold the mixed sibling's parser lock while publishing. The deadline
    // releases a regressed blocking read, so it fails instead of hanging.
    let term = app.pool.get(session).unwrap().term.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let parser_holder = std::thread::spawn(move || {
        let _guard = term.lock().unwrap();
        if ready_tx.send(()).is_err() {
            return false;
        }
        matches!(
            release_rx.recv_timeout(Duration::from_secs(60)),
            Err(mpsc::RecvTimeoutError::Timeout)
        )
    });
    ready_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("parser lock holder must become ready");
    app.publish_system_state(SetupStatus {
        busy: true,
        ..SetupStatus::default()
    });
    let busy = TabIndicators {
        busy: true,
        ..TabIndicators::default()
    };
    assert_flags(&app, busy);
    app.refresh_tab_status_indicators(second_window, second_tab);
    assert_flags(&app, busy);

    // A managed row that failed is the only attention a page raises.
    app.publish_system_state(fixture_status("failed").unwrap());
    let failed = TabIndicators {
        attention: true,
        ..TabIndicators::default()
    };
    assert_flags(&app, failed);
    app.refresh_tab_status_indicators(second_window, second_tab);
    assert_flags(&app, failed);

    // Leaving the failed row's group as is takes effect at once; every linked
    // tab is busy until a fresh read shows the admitted run.
    app.native_runtime
        .dispatch(
            instance,
            first_view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("system-state/group/4/leave"),
                value: None,
            }),
        )
        .unwrap();
    let apply = take(&mut app);
    app.run_system_state_action_with_handlers(
        apply,
        Some(InitialSetupHandlers {
            apply: |_, _, _, receipt| {
                receipt.accept();
                true
            },
            ..handlers()
        }),
    );
    assert_flags(&app, busy);
    app.resolve_system_state_apply();
    assert_flags(&app, busy);
    fresh(&mut app, SetupStatus::default());
    assert_flags(&app, TabIndicators::default());
    let _ = release_tx.send(());
    assert!(
        !parser_holder.join().unwrap(),
        "status publication blocked on the parser lock"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn withdrawn_system_state_request_cannot_open_or_hold_a_page() {
    for route in ["window", "attach", "model"] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
        if route != "window" {
            assert!(app.open_system_state_tab());
        }
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            SetupStatus {
                permissions: Some(crate::InitialSetupPermissionPolicy {
                    revision: 5,
                    allowed: true,
                }),
                ..SetupStatus::default()
            },
        );
        assert!(!app.workstation_setup.suppresses_permissions());
        let active = app.windows[&wid].tab_set.active_id();
        let focus = app.windows[&wid].window_focus;
        let instance = app.native_runtime.instance_by_kind(AppKind::SystemState);
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        // The real waiting worker can withdraw its queued wake when an
        // explicit doctor operation completes in another process.
        request.finish(WorkstationSetupDecision::Unavailable);
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        match route {
            "window" => {
                assert!(!app.apprt.probe_preferences);
                assert!(app.windows[&wid].os_window.is_none());
                app.headless = false;
                app.offer_system_state(request);
            }
            "attach" => app.attach_system_state_offer(request),
            _ => assert!(!app.native_runtime.offer_system_state(request)),
        }
        assert_eq!(app.windows[&wid].tab_set.active_id(), active);
        assert_eq!(app.windows[&wid].window_focus, focus);
        assert_eq!(
            app.native_runtime.instance_by_kind(AppKind::SystemState),
            instance
        );
        assert!(app.system_state_host.pending.is_none());
        assert!(!app.workstation_setup.suppresses_permissions());
    }
}

#[test]
fn withdrawn_system_state_offer_restores_policy_only_after_a_fresh_read() {
    let policy = || SetupStatus {
        permissions: Some(crate::InitialSetupPermissionPolicy {
            revision: 5,
            allowed: true,
        }),
        ..SetupStatus::default()
    };
    let mut app = App::headless_for_test();
    app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
    app.receive_system_state_status(Some(app.system_state_host.generation), policy());
    assert!(!app.workstation_setup.suppresses_permissions());
    assert!(app.open_system_state_tab());
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    let before_withdrawal = app.system_state_host.generation;
    app.receive_system_state_status(Some(before_withdrawal), policy());
    assert!(app.workstation_setup.suppresses_permissions());
    request.finish(WorkstationSetupDecision::Unavailable);
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    let now = Instant::now();
    app.system_state_host.next_tick = Some(now + Duration::from_secs(60));
    app.tick_system_state(now);
    assert!(app.system_state_host.pending.is_none());
    assert!(!app.native_runtime.system_state_mut().unwrap().pending());
    assert!(app.native_runtime.take_system_state_action().is_none());
    assert!(app.workstation_setup.suppresses_permissions());
    app.publish_system_state(policy());
    app.receive_system_state_status(Some(before_withdrawal), policy());
    assert!(app.workstation_setup.suppresses_permissions());
    fresh(&mut app, policy());
    assert!(
        !app.workstation_setup.suppresses_permissions(),
        "a withdrawn offer cannot retain consent after a fresh allowed snapshot"
    );
    assert_ne!(app.system_state_host.generation, before_withdrawal);
}

#[test]
fn withdrawn_system_state_offer_cannot_release_a_newer_local_decision() {
    let policy = || SetupStatus {
        permissions: Some(crate::InitialSetupPermissionPolicy {
            revision: 6,
            allowed: true,
        }),
        ..SetupStatus::default()
    };
    for key in ["system-state/group/1/leave", "system-state/restore"] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        app.set_system_state_handlers_for_test(InitialSetupHandlers {
            observe: |_| true,
            ..handlers()
        });
        let previous = app.system_state_host.generation;
        press(&mut app, wid, view, key);
        let mut answer = receive.try_recv().unwrap();
        let receipt = answer.receipt.take();
        // A late completion of that old channel cannot retire the newer
        // local action's permission hold, even once its view is idle.
        request.finish(WorkstationSetupDecision::Unavailable);
        app.tick_system_state(Instant::now());
        app.receive_system_state_status(Some(previous), policy());
        fresh(&mut app, policy());
        assert!(app.workstation_setup.suppresses_permissions());
        if let Some(receipt) = receipt {
            assert!(app.system_state_host.pending_apply.is_some());
            let before_ack = app.system_state_host.generation;
            receipt.accept();
            app.resolve_system_state_apply();
            app.receive_system_state_status(Some(before_ack), policy());
            assert!(app.workstation_setup.suppresses_permissions());
            fresh(&mut app, policy());
            assert!(!app.workstation_setup.suppresses_permissions());
        } else {
            assert_eq!(key, "system-state/restore");
            assert_eq!(calls(), ["restore"]);
            assert!(app.system_state_host.pending_apply.is_none());
        }
    }
}

#[test]
fn refused_automatic_offer_does_not_take_the_active_tab_or_focus() {
    for busy in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app.windows[&wid].tab_set.active_index().unwrap();
        let (_, instance, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        if busy {
            press(&mut app, wid, view, "system-state/check");
        } else {
            // A vault address typed here and not joined yet is the person's.
            type_into(
                &mut app,
                wid,
                view,
                "system-state/url",
                "http://draft.local/v1/team",
            );
        }
        app.switch_tab_in(wid, terminal);
        let active = app.windows[&wid].tab_set.active_id();
        let focus = app.windows[&wid].window_focus;
        let snapshot = |app: &App| {
            let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
                panic!("the page must remain live")
            };
            (
                model.reviewed_plan(),
                model.field_projection("system-state/url").unwrap(),
                indicators(app, instance, view),
                app.native_runtime
                    .view_state(view)
                    .unwrap()
                    .common()
                    .last_focus
                    .clone(),
            )
        };
        let before = snapshot(&app);
        assert_eq!(before.2.busy, busy);

        // Exercise the windowed offer route without an OS surface. The
        // fixture's injected runtime keeps native preference probes inert.
        assert!(!app.apprt.probe_preferences);
        assert!(app.windows[&wid].os_window.is_none());
        app.headless = false;
        let (send, receive) = mpsc::channel();
        app.offer_system_state(Request::new(Plan::default(), send));
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert_eq!(app.windows[&wid].tab_set.active_id(), active);
        assert_eq!(app.windows[&wid].window_focus, focus);
        assert!(app.active_native_view(wid).is_none());
        assert_eq!(snapshot(&app), before);
        assert!(app.system_state_host.pending.is_none());
        assert!(app.native_runtime.take_system_state_action().is_none());
        let asked = calls();
        if busy {
            assert_eq!(
                asked,
                [
                    format!("check {}", Plan::default().vault_url),
                    format!("observe {}", Plan::default().vault_url)
                ]
            );
        } else {
            assert!(asked.is_empty());
        }
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(!app.apprt.probe_preferences);
    assert!(app.windows[&wid].os_window.is_none());
    app.headless = false;
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.offer_system_state(request.clone());
    let (instance, _) = app.active_native_view(wid).unwrap();
    assert!(matches!(
        app.native_runtime.app(instance),
        Some(NativeApp::SystemState(_))
    ));
    assert!(request.pending());
    assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(
        app.system_state_host
            .pending
            .as_ref()
            .is_some_and(Request::pending)
    );
}

#[test]
fn late_automatic_offer_cannot_replace_a_choice_awaiting_its_run() {
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/group/2/leave");
    let receipt = receipt();
    let flags = indicators(&app, instance, view);
    assert!(!flags.dirty && flags.busy, "the choice owns admission");

    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(!request.pending());
    assert!(app.system_state_host.pending.is_none());
    assert!(app.system_state_host.pending_apply.is_some());
    assert!(indicators(&app, instance, view).busy);

    receipt.accept();
    app.resolve_system_state_apply();
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(
        indicators(&app, instance, view).busy,
        "admitted, but its run has not shown up in a read yet"
    );
    fresh(&mut app, SetupStatus::default());
    let flags = indicators(&app, instance, view);
    assert!(!flags.dirty && !flags.busy);
}

#[test]
fn late_automatic_offer_cannot_replace_a_check_already_started() {
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/check");
    let (send, receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert_eq!(
        calls(),
        [
            format!("check {}", Plan::default().vault_url),
            format!("observe {}", Plan::default().vault_url)
        ]
    );
    let flags = indicators(&app, instance, view);
    assert!(!flags.dirty && flags.busy);
    assert!(app.system_state_host.pending.is_none());
}

#[test]
fn check_again_reads_every_row_for_the_pages_plan_and_says_when_it_did_not_start() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    let url = "http://checked.local/v1/team";
    type_into(&mut app, wid, view, "system-state/url", url);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/check");
    assert_eq!(calls(), [format!("check {url}"), format!("observe {url}")]);
    assert!(app.system_state_host.action_refusal.is_none());

    fresh(&mut app, SetupStatus::default());
    // Neither the forced reading nor the check started.
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        check: |_| CheckStarted::default(),
        ..handlers()
    });
    press(&mut app, wid, view, "system-state/check");
    let refusal = app.system_state_host.action_refusal.clone().unwrap();
    assert_eq!(
        (refusal.name.as_str(), refusal.status.as_str()),
        ("Check again", "not started")
    );
    assert!(app.commit_glassless_native_frame(wid));
    let compiled = &app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap()
        .compiled;
    assert!(
        compiled
            .semantics
            .iter()
            .any(|node| node.label.contains("Check again did not start. Try again."))
    );
    let Some(state) = app.native_runtime.system_state_mut() else {
        panic!("System State is open");
    };
    assert_eq!(
        state.field_projection("system-state/url").unwrap().text,
        url
    );
}

#[test]
fn restore_lets_the_first_run_offer_go_for_good() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    let (send, receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/restore");
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert_eq!(calls(), ["restore"]);
    assert!(app.system_state_host.pending.is_none());
    fresh(&mut app, SetupStatus::default());
    app.native_runtime.tick_system_state(true);
    assert!(
        app.native_runtime.take_system_state_action().is_none(),
        "no later tick applies an offer Restore let go"
    );
}

#[test]
fn the_first_run_offer_applies_once_presented_and_waits_for_its_run() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    let (_, instance, view) = page(&mut app);
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    let start = Instant::now();
    app.tick_system_state(start);
    assert!(request.pending(), "not before the page reached glass");
    assert!(app.commit_glassless_native_frame(wid));
    app.tick_system_state(start + Duration::from_secs(1));
    let mut answer = receive.try_recv().unwrap();
    assert_eq!(
        answer.decision,
        WorkstationSetupDecision::Begin(Plan::default())
    );
    assert!(indicators(&app, instance, view).busy);
    answer.receipt.take().unwrap().accept();
    app.resolve_system_state_apply();
    assert!(indicators(&app, instance, view).busy);
    fresh(
        &mut app,
        SetupStatus {
            busy: true,
            ..SetupStatus::default()
        },
    );
    assert!(indicators(&app, instance, view).busy);
    fresh(&mut app, SetupStatus::default());
    assert!(!indicators(&app, instance, view).busy);
    app.tick_system_state(start + Duration::from_secs(2));
    assert!(!request.pending());
    assert!(receive.try_recv().is_err(), "the offer is answered once");
}

#[test]
fn the_offer_counts_as_presented_only_on_glass_in_a_focused_window() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    let (_, instance, view) = page(&mut app);
    let (send, _receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    assert!(!app.system_state_presented_active(instance));
    assert!(app.prepare_native_input_scratch(wid));
    assert!(!app.system_state_presented_active(instance));
    assert!(app.commit_glassless_native_frame(wid));
    assert!(app.system_state_presented_active(instance));

    // A repaint the status asks for does not take the presentation back.
    app.publish_system_state(fixture_status("checking").unwrap());
    assert!(app.retained_native_leaf_artifact(wid, view, true).is_none());
    assert!(app.system_state_presented_active(instance));
    app.windows.get_mut(&wid).unwrap().focused = false;
    assert!(!app.system_state_presented_active(instance));
    app.windows.get_mut(&wid).unwrap().focused = true;

    request.finish(WorkstationSetupDecision::Unavailable);
    app.system_state_host.pending = None;
    let (send, _receive_next) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    assert!(!app.system_state_host.offer_presented);
    assert!(!app.system_state_presented_active(instance));
}

/// A choice made while a run is going reaches the worker, which stops the
/// pass ahead of it as it takes the plan (design §3.3): the window asks for
/// no Stop, then or on a busy read after, so none can reach the choice's own
/// run, however soon after its admission the read comes.
#[test]
fn a_choice_during_a_run_leaves_the_pass_ahead_to_the_worker() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    fresh(&mut app, running());
    press(&mut app, wid, view, "system-state/group/3/leave");
    let first = receipt();
    assert!(calls().is_empty(), "the window asks for no Stop");
    fresh(&mut app, running());
    // A second choice while the first is still being admitted.
    press(&mut app, wid, view, "system-state/group/2/leave");
    drop(first);
    let second = receipt();
    fresh(&mut app, running());
    second.accept();
    app.resolve_system_state_apply();
    fresh(&mut app, running());
    assert!(calls().is_empty(), "nor on any busy read");
}

/// Stop is asked of the worker once, as it is pressed: the worker keeps it
/// for a run whose plan is still being admitted (design §3.3), so a busy
/// read after it asks nothing more.
#[test]
fn a_pressed_stop_is_asked_once() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    fresh(&mut app, running());
    let generation = app.system_state_host.generation;
    press(&mut app, wid, view, "system-state/stop");
    assert_eq!(calls(), ["stop"]);
    assert_eq!(
        app.system_state_host.generation, generation,
        "Stop changes no plan"
    );
    fresh(&mut app, running());
    fresh(&mut app, running());
    assert!(calls().is_empty(), "the worker keeps it");
}

#[test]
fn intent_handoff_stop_survives_an_immediate_close_or_quit() {
    for close in [false, true] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        fresh(&mut app, running());
        press(&mut app, wid, view, "system-state/stop");
        // Neither the next periodic tick nor another event-loop park has run.
        if close {
            app.close_active_native_tab(wid).unwrap();
        }
        app.finish_system_state_shutdown();
        assert_eq!(calls(), ["stop", "settle"]);
        assert!(app.native_runtime.take_system_state_action().is_none());
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn intent_handoff_group_choice_survives_an_immediate_close() {
    for accepted in [false, true] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        app.set_system_state_handlers_for_test(InitialSetupHandlers {
            apply: if accepted {
                |plan, typed, password, receipt| {
                    assert!(!typed && password.is_none());
                    assert!(!plan.selection.contains(Setting::Messages));
                    assert!(!plan.selection.contains(Setting::FaceTime));
                    note("apply communications=false".into());
                    APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().push(receipt));
                    true
                }
            } else {
                |plan, typed, password, _| {
                    assert!(!typed && password.is_none());
                    assert!(!plan.selection.contains(Setting::Messages));
                    assert!(!plan.selection.contains(Setting::FaceTime));
                    note("apply communications=false".into());
                    false
                }
            },
            ..handlers()
        });
        fresh(&mut app, running());
        press(&mut app, wid, view, "system-state/group/3/leave");
        app.close_active_native_tab(wid).unwrap();
        app.finish_system_state_shutdown();
        assert_eq!(calls(), ["apply communications=false", "settle"]);
        assert_eq!(app.system_state_host.pending_apply.is_some(), accepted);
        if accepted {
            receipt().accept();
            app.resolve_system_state_apply();
        }
        assert!(app.system_state_host.pending_apply.is_none());
        assert!(app.native_runtime.take_system_state_action().is_none());
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn intent_handoff_check_and_restore_are_submitted_before_the_page_closes() {
    for key in ["system-state/check", "system-state/restore"] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        press(&mut app, wid, view, key);
        app.close_active_native_tab(wid).unwrap();
        app.finish_system_state_shutdown();
        let expected = if key.ends_with("check") {
            vec![
                format!("check {}", Plan::default().vault_url),
                format!("observe {}", Plan::default().vault_url),
                "settle".to_owned(),
            ]
        } else {
            vec!["restore".to_owned(), "settle".to_owned()]
        };
        assert_eq!(calls(), expected);
        assert!(app.native_runtime.take_system_state_action().is_none());
    }
}

#[test]
#[cfg(any(debug_assertions, feature = "dev-seams"))]
fn intent_handoff_keeps_development_scenarios_inert() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    app.system_state_host.fixture = Some(fixture_status("running").unwrap());
    app.system_state_host.fixture_read = true;
    fresh(&mut app, running());
    press(&mut app, wid, view, "system-state/group/3/leave");
    press(&mut app, wid, view, "system-state/stop");
    assert!(calls().is_empty());
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(app.native_runtime.take_system_state_action().is_none());
}

#[test]
fn a_choice_reaches_the_worker_even_when_the_page_closes_at_once() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    fresh(&mut app, running());
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        apply: |plan, _, _, receipt| {
            note(format!(
                "apply messages={}",
                plan.selection.contains(Setting::Messages)
            ));
            APPLY_RECEIPTS.with(|receipts| receipts.borrow_mut().push(receipt));
            true
        },
        ..handlers()
    });
    press(&mut app, wid, view, "system-state/group/3/leave");
    // The worker holds the choice before the view can go, and stops the run
    // ahead of it itself: nothing waits on the page.
    assert_eq!(calls(), ["apply messages=false"]);
    app.close_active_native_tab(wid).unwrap();
    app.finish_system_state_shutdown();
    receipt().accept();
    app.resolve_system_state_apply();
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(app.native_runtime.take_system_state_action().is_none());
    assert!(app.structural_invariants_ok());
}

#[test]
fn a_page_opened_by_hand_shows_and_reads_the_plan_in_force() {
    let mut saved = Plan::default();
    saved.selection.set(Setting::Repositories, false);
    saved.vault_url = "http://saved.local/v1/team".into();
    let in_force = || SetupStatus {
        plan: Some(saved.clone()),
        ..SetupStatus::default()
    };

    // Opened before any read: no guess is shown, submitted or read for.
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_system_state_tab());
    let (instance, view) = app.active_native_view(wid).unwrap();
    assert!(!app.observe_system_state_with(instance, Some(handlers())));
    press(&mut app, wid, view, "system-state/group/1/leave");
    assert!(app.native_runtime.take_system_state_action().is_none());
    assert!(calls().is_empty());
    fresh(&mut app, in_force());
    assert!(app.observe_system_state_with(instance, Some(handlers())));
    assert_eq!(calls(), ["observe http://saved.local/v1/team"]);
    let Some(state) = app.native_runtime.system_state_mut() else {
        panic!("System State is open");
    };
    assert_eq!(state.reviewed_plan(), saved);

    // Opened after a read: the next tick gives it the plan that read named.
    let mut app = App::headless_for_test();
    app.publish_system_state(in_force());
    assert!(app.open_system_state_tab());
    assert!(!app.native_runtime.system_state_mut().unwrap().plan_known());
    app.tick_system_state(Instant::now());
    let state = app.native_runtime.system_state_mut().unwrap();
    assert!(state.plan_known());
    assert_eq!(state.reviewed_plan(), saved);
}

#[test]
fn check_again_and_restore_are_not_shown_as_a_run() {
    for (key, words) in [
        ("system-state/check", "Checking this Mac…"),
        ("system-state/restore", "Restoring previous settings…"),
    ] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        press(&mut app, wid, view, key);
        calls();
        fresh(&mut app, running());
        let compiled = app.compiled_native_ui(wid).unwrap();
        assert_eq!(
            compiled
                .semantic(&UiKey::new("settings/page-subtitle/system-state"))
                .unwrap()
                .label,
            words
        );
        for absent in ["system-state/stop", "system-state/meter"] {
            assert!(compiled.semantic(&UiKey::new(absent)).is_none(), "{absent}");
        }
        // Done: the next run is a run again.
        fresh(&mut app, SetupStatus::default());
        assert!(app.system_state_host.operation.is_none());
        fresh(&mut app, running());
        let compiled = app.compiled_native_ui(wid).unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("system-state/stop"))
                .is_some()
        );
    }
}

/// A requested Check can fail even when the separately observed rows are in
/// place. Its result belongs to exactly the plan it read, including a typed
/// vault address; a later edit must not reinstall it from the next poll.
#[test]
fn check_notes_follow_the_exact_dispatched_plan() {
    use crate::system_state::SetupCheck;

    let detail = "injected selected-check observation could not be read";
    let has_detail = |app: &App, wid| {
        app.compiled_native_ui(wid)
            .unwrap()
            .semantics
            .iter()
            .any(|node| !node.offscreen && node.label.contains(detail))
    };
    for outcome in ["failed", "unverified"] {
        for typed_url in [false, true] {
            let mut app = App::headless_for_test();
            let (wid, instance, view) = page(&mut app);
            app.windows.get_mut(&wid).unwrap().rows = 52;
            app.windows.get_mut(&wid).unwrap().cols = 130;
            let mut saved = Plan {
                vault_url: "http://saved.local:7484/v1/team".into(),
                ..Plan::default()
            };
            saved.selection.set(Setting::Messages, false);
            let in_place = SetupStatus {
                plan: Some(saved.clone()),
                ..fixture_status("set-up").unwrap()
            };
            fresh(&mut app, in_place.clone());
            if typed_url {
                type_into(
                    &mut app,
                    wid,
                    view,
                    "system-state/url",
                    "http://reviewed.local:7484/v1/team",
                );
            }
            app.set_system_state_handlers_for_test(handlers());
            let checked = match app.native_runtime.app(instance).unwrap() {
                NativeApp::SystemState(state) => state.reviewed_plan(),
                _ => unreachable!(),
            };
            press(&mut app, wid, view, "system-state/check");
            assert_eq!(
                calls(),
                [
                    format!("check {}", checked.vault_url),
                    format!("observe {}", checked.vault_url),
                ],
                "the real dispatch starts Check and its forced observation once"
            );

            let result = |plan| SetupStatus {
                check: Some(SetupCheck {
                    plan,
                    rows: vec![SetupStatusRow {
                        name: "Check".into(),
                        status: outcome.into(),
                        detail: detail.into(),
                    }],
                }),
                ..in_place.clone()
            };
            let mut other_selection = checked.clone();
            other_selection.selection.set(Setting::Desktop, false);
            let mut other_url = checked.clone();
            other_url.vault_url = "http://other.local:7484/v1/team".into();
            for stale in [other_selection, other_url] {
                fresh(&mut app, result(stale));
                assert!(!has_detail(&app, wid), "a different Check stays hidden");
                assert!(!indicators(&app, instance, view).attention);
            }

            let matching = result(checked.clone());
            fresh(&mut app, matching.clone());
            assert!(
                has_detail(&app, wid),
                "the requested Check's reason is visible"
            );
            assert_eq!(
                indicators(&app, instance, view).attention,
                outcome == "failed"
            );
            let tab = app.windows[&wid].tab_set.active().unwrap();
            assert_eq!(tab.presentation.indicators.attention, outcome == "failed");
            assert_eq!(
                crate::system_state_access::menu_line(&matching, false).is_some(),
                outcome == "failed" && !typed_url,
                "without a review context, the menu matches only the saved plan"
            );
            // Both URL and selection mismatches were checked above; exercise a
            // real subsequent field edit and repeated current status, too.
            type_into(
                &mut app,
                wid,
                view,
                "system-state/url",
                "http://next-review.local:7484/v1/team",
            );
            assert!(!has_detail(&app, wid));
            fresh(&mut app, matching);
            assert!(
                !has_detail(&app, wid),
                "polls cannot restore a mismatched result"
            );
            assert!(!indicators(&app, instance, view).attention);
            assert!(
                !app.windows[&wid]
                    .tab_set
                    .active()
                    .unwrap()
                    .presentation
                    .indicators
                    .attention
            );
        }
    }
}

#[test]
fn a_pass_overlapping_check_again_is_shown_as_a_run_with_its_stop() {
    let headline = |app: &App, wid: WindowId| {
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(&UiKey::new("settings/page-subtitle/system-state"))
            .unwrap()
            .label
            .clone()
    };
    let stop_shown = |app: &App, wid: WindowId| {
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(&UiKey::new("system-state/stop"))
            .is_some()
    };
    // Check again woke a parked run; its pass took the worker as the check
    // ended, before the window read it idle.
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/check");
    calls();
    fresh(
        &mut app,
        SetupStatus {
            running: vec![Setting::Spotlight],
            ..running()
        },
    );
    assert_ne!(headline(&app, wid), "Checking this Mac…");
    assert!(stop_shown(&app, wid), "the pass can be stopped");
    assert!(app.system_state_host.operation.is_none());
    // Between its stages it is still a run, not a check.
    fresh(&mut app, running());
    assert!(stop_shown(&app, wid));

    // A pass held the worker when Check again was pressed: the check did not
    // start, so nothing is labelled as one. Its forced reading ran, so Check
    // again did start, and the page says nothing about it not starting.
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        check: |_| CheckStarted {
            reading: true,
            check: false,
        },
        ..handlers()
    });
    press(&mut app, wid, view, "system-state/check");
    calls();
    assert!(app.system_state_host.operation.is_none());
    assert!(app.system_state_host.action_refusal.is_none());
    fresh(&mut app, running());
    assert!(stop_shown(&app, wid));
    let page = app.compiled_native_ui(wid).unwrap();
    assert!(
        !page
            .semantics
            .iter()
            .any(|node| node.label.contains("Check again did not start")),
        "the forced reading ran: Check again started"
    );
}

#[test]
fn a_group_choice_reaches_the_worker_as_the_choosers_plan() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/group/1/leave");
    assert!(
        receipt().chosen(),
        "a choice back to aterm's plan stays the person's"
    );
}

#[test]
fn an_admitted_choice_is_not_undone_by_the_read_taken_before_it() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/group/2/leave");
    receipt().accept();
    // The last read still names the plan before the choice; the window shows
    // it again once the choice is admitted.
    app.resolve_system_state_apply();
    let Some(state) = app.native_runtime.system_state_mut() else {
        panic!("System State is open");
    };
    assert!(!state.reviewed_plan().selection.contains(Setting::Dock));
    assert!(
        app.system_state_host
            .latest_status
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.selection.contains(Setting::Dock))
    );
}

#[test]
fn a_control_clients_press_is_never_a_persons() {
    for client in [false, true] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        app.with_gesture_source(client, |app| {
            press(app, wid, view, "system-state/group/1/leave");
        });
        let expected = if client {
            Admission::Client
        } else {
            Admission::Person
        };
        // The worker's handler hears whose choice it is, for the gate.
        assert_eq!(receipt().admission(), expected);
    }
}

#[test]
fn stop_and_open_change_no_plan() {
    let mut app = App::headless_for_test();
    let (_, _, _) = page(&mut app);
    let generation = app.system_state_host.generation;
    app.run_system_state_action_with_handlers(SystemStateAction::Stop, Some(handlers()));
    app.run_system_state_action_with_handlers(
        SystemStateAction::OpenAccess {
            access: Access::FullDiskAccess,
            by_person: true,
        },
        Some(handlers()),
    );
    assert_eq!(calls(), ["stop"]);
    assert_eq!(app.system_state_host.generation, generation);
    assert!(app.system_state_host.action_refusal.is_none());
    assert!(app.system_state_host.pending_apply.is_none());
}

/// A CONTROL CLIENT'S OPEN SPENDS ITS OWN EVIDENCE: a row's Open acted by a
/// control client (`aterm ctl app act system-state/row/<slug>/open`) takes the
/// press evidence it left, as every other action does, so the person's next
/// press on the page is still theirs. Left pending, the client's gesture would
/// taint it: a Turn On… that starts nothing, an Apply that mints no admission.
#[test]
fn a_clients_open_spends_its_evidence_so_the_persons_next_press_is_theirs() {
    use crate::workstation_setup::PagePress;
    for access in [Access::FullDiskAccess, Access::Debugger] {
        let mut app = App::headless_for_test();
        let (wid, _, _) = page(&mut app);
        app.with_gesture_source(true, |app| {
            app.interact_system_state(wid);
            app.run_system_state_action_with_handlers(
                SystemStateAction::OpenAccess {
                    access,
                    by_person: false,
                },
                Some(handlers()),
            );
        });
        // The person's own touch, then their press.
        app.interact_system_state(wid);
        assert_eq!(
            app.take_system_state_press(),
            PagePress::Person,
            "{access:?}: the person's next press is theirs"
        );
    }
}

#[test]
fn refused_actions_show_one_note_and_identical_reads_do_not_repaint() {
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    let (send, receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    let completed = fixture_status("set-up").unwrap();
    app.publish_system_state(completed.clone());
    press(&mut app, wid, view, "system-state/restore");
    // Library tests install no handlers: Restore cannot start.
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(app.system_state_host.pending.is_none());
    let refusal = app.system_state_host.action_refusal.clone().unwrap();
    assert_eq!(refusal.name, "Restore previous settings");
    let notes = |app: &App| {
        app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .semantics
            .iter()
            .filter(|node| {
                !node.offscreen
                    && node.key.as_str().starts_with("system-state/note/")
                    && node
                        .label
                        .contains("Restore previous settings did not start. Try again.")
            })
            .count()
    };
    assert_eq!(app.system_state_host.latest_status, completed);
    assert!(app.commit_glassless_native_frame(wid));
    assert_eq!(notes(&app), 1);
    let revision = app
        .native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .presentation_revision;
    for _ in 0..3 {
        fresh(&mut app, completed.clone());
        assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .presentation_revision,
            revision
        );
        assert_eq!(notes(&app), 1);
    }
    assert!(!indicators(&app, instance, view).attention);

    // The next action clears it.
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/check");
    calls();
    fresh(&mut app, completed);
    assert!(app.commit_glassless_native_frame(wid));
    assert_eq!(notes(&app), 0);
}

#[test]
fn a_refused_choice_puts_the_groups_back_and_keeps_the_password() {
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    type_into(
        &mut app,
        wid,
        view,
        "system-state/password",
        "inert-password",
    );
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        apply: |_, _, _, _refused| false,
        ..handlers()
    });
    press(&mut app, wid, view, "system-state/group/2/leave");
    let refusal = app.system_state_host.action_refusal.clone().unwrap();
    assert_eq!(
        (refusal.name.as_str(), refusal.status.as_str()),
        ("Setup", "not started")
    );
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(!indicators(&app, instance, view).busy);
    let Some(state) = app.native_runtime.system_state_mut() else {
        panic!("System State is open");
    };
    assert!(
        state.reviewed_plan().selection.contains(Setting::Dock),
        "the groups show what is taking effect"
    );
    assert_eq!(
        state
            .field_projection("system-state/password")
            .unwrap()
            .text,
        "•".repeat("inert-password".len())
    );
}

/// The worker hears which vault the person typed: a Join's, never a group choice's,
/// even with an edited field (§3.3). Negative control: a flag read off the field would
/// say both were typed.
#[test]
fn only_a_join_tells_the_worker_its_vault_was_typed() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    type_into(
        &mut app,
        wid,
        view,
        "system-state/url",
        "http://typed.local/v1/team",
    );
    for key in ["system-state/group/2/leave", "system-state/vault/join"] {
        app.set_system_state_handlers_for_test(InitialSetupHandlers {
            apply: |plan, typed, _, receipt| {
                note(format!("apply typed={typed} {}", plan.vault_url));
                receipt.accept();
                true
            },
            ..handlers()
        });
        press(&mut app, wid, view, key);
        app.resolve_system_state_apply();
    }
    let applied: Vec<String> = calls()
        .into_iter()
        .filter(|call| call.starts_with("apply"))
        .collect();
    assert_eq!(
        applied,
        [
            format!("apply typed=false {}", Plan::default().vault_url),
            "apply typed=true http://typed.local/v1/team".to_owned(),
        ]
    );
}

#[test]
fn an_offer_clears_a_remembered_control_in_every_linked_view() {
    let mut app = App::headless_for_test();
    let (wid, _, first) = page(&mut app);
    app.stage_system_state_tab(wid).unwrap();
    let (_, second) = app.active_native_view(wid).unwrap();
    assert_ne!(first, second);
    for view in [first, second] {
        let state = app.native_runtime.view_state_mut(view).unwrap();
        state.common_mut().last_focus = Some(UiKey::new("system-state/vault/join"));
        state.common_mut().focus_visible = true;
    }
    let (send, _receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    for view in [first, second] {
        let common = app.native_runtime.view_state(view).unwrap().common();
        assert!(common.last_focus.is_none());
        assert!(!common.focus_visible);
    }
}

#[test]
fn closing_the_page_never_cancels_and_never_holds_an_update() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    type_into(
        &mut app,
        wid,
        view,
        "system-state/url",
        "http://draft.local/v1/team",
    );
    type_into(
        &mut app,
        wid,
        view,
        "system-state/password",
        "inert-password",
    );
    for scope in [CloseScope::Relaunch, CloseScope::CarriedRelaunch] {
        assert!(
            app.prepare_all_native_shutdown(
                scope,
                crate::app_tabs::ClosePreflightVisibility::Quiet
            )
            .unwrap()
        );
    }
    assert!(request.pending(), "a preflight answers nothing");
    app.close_active_native_tab(wid).unwrap();
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    app.tick_system_state(Instant::now());
    assert!(app.system_state_host.pending.is_none());
    assert!(app.system_state_host.next_tick.is_none());
    assert!(app.structural_invariants_ok());
}

#[test]
fn shutdown_lets_an_unanswered_offer_go() {
    let mut app = App::headless_for_test();
    page(&mut app);
    let (send, receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    app.finish_system_state_shutdown();
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(app.system_state_host.pending.is_none());
}

#[test]
fn a_quit_waits_for_what_a_stop_owes_the_records() {
    // The Stop's request is saved off the UI thread: the quit settles it.
    let mut app = App::headless_for_test();
    page(&mut app);
    app.finish_system_state_shutdown_with(Some(handlers()));
    assert_eq!(calls(), ["settle"]);
}

#[test]
fn no_offer_is_shown_over_a_choice_still_being_admitted() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    press(&mut app, wid, view, "system-state/group/3/leave");
    assert!(app.system_state_host.pending_apply.is_some());
    // The person closes the page and opens it again: the new page waits on
    // nothing, but the window still waits for the choice's answer.
    app.close_active_native_tab(wid).unwrap();
    page(&mut app);
    // The startup's offer reaches the window before the choice's answer: the
    // choice answers for this launch, so the offer is let go, and the window
    // still reads the choice's answer when it comes.
    let (send, receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(app.system_state_host.pending.is_none());
    receipt().accept();
    app.resolve_system_state_apply();
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(
        app.system_state_host.awaiting_run,
        "the choice was admitted"
    );
}

#[test]
fn a_pointer_click_on_a_group_choice_takes_effect() {
    use crate::app_mouse::{NativePointerTarget, PointerAction};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 60;
    window.cols = 120;
    assert!(app.open_system_state_tab());
    app.publish_system_state(SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    });
    app.set_system_state_handlers_for_test(handlers());
    let key = "system-state/group/1/leave";
    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
        key.to_owned(),
    )))
    .unwrap();
    app.pointer_cmd(PointerAction::Click).unwrap();
    assert!(
        app.compiled_native_ui(wid)
            .unwrap()
            .semantic(&UiKey::new(key))
            .unwrap()
            .state
            .is_some_and(|state| state.selected)
    );
    assert!(
        !app.system_state_host
            .pending_apply
            .as_ref()
            .unwrap()
            .plan
            .selection
            .contains(Setting::CapsLock)
    );
    assert!(receipt().chosen(), "the click reached the worker");
}

#[test]
fn idle_ticks_keep_the_shown_frame() {
    use crate::app_mouse::{NativePointerTarget, PointerAction, WheelUnit};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let window = app.windows.get_mut(&wid).unwrap();
    window.rows = 60;
    window.cols = 120;
    window.os_window_for_test = true;
    assert!(app.open_system_state_tab());
    let (_, view) = app.active_native_view(wid).unwrap();
    let (send, receive) = mpsc::channel();
    app.attach_system_state_offer(Request::new(Plan::default(), send));
    assert!(app.commit_glassless_native_frame(wid));
    let start = Instant::now();
    app.tick_system_state(start);
    let mut answer = receive.try_recv().unwrap();
    assert!(matches!(
        answer.decision,
        WorkstationSetupDecision::Begin(_)
    ));
    answer.receipt.take().unwrap().accept();
    app.resolve_system_state_apply();
    let status = fixture_status("set-up").unwrap();
    fresh(&mut app, status.clone());
    assert!(app.commit_glassless_native_frame(wid));
    let revision = app
        .native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .presentation_revision;
    // No repaint or input occurs during these idle ticks. Repeated identical
    // worker snapshots also leave the shown frame valid.
    for tick in 1..=40 {
        app.tick_system_state(start + Duration::from_millis(tick * 250));
        app.publish_system_state(status.clone());
        assert!(app.retained_native_leaf_artifact(wid, view, true).is_some());
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .presentation_revision,
            revision
        );
    }
    app.pointer_cmd(PointerAction::Wheel {
        dx: 0.0,
        dy: 1.0,
        unit: WheelUnit::Lines,
        aim: Some(NativePointerTarget::Key("system-state/group/0".into())),
    })
    .unwrap();
    app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
        "system-state/check".to_owned(),
    )))
    .unwrap();
    app.set_system_state_handlers_for_test(handlers());
    app.pointer_cmd(PointerAction::Click).unwrap();
    assert_eq!(
        calls(),
        [
            format!("check {}", Plan::default().vault_url),
            format!("observe {}", Plan::default().vault_url)
        ]
    );
}

/// TURN ON SYNC IS THE PERSON'S ALONE: their press on the Secrets vault row's
/// Turn On Sync asks the worker to install the subscriber they turned off
/// (design §3.3); a control client's press turns nothing on, since only the
/// person undoes their own choice, and spends its evidence, so the person's
/// next press is still theirs.
#[test]
fn only_a_persons_turn_on_sync_turns_the_sync_on() {
    use crate::workstation_setup::PagePress;
    for client in [false, true] {
        let mut app = App::headless_for_test();
        let (wid, _, _) = page(&mut app);
        let generation = app.system_state_host.generation;
        app.with_gesture_source(client, |app| {
            app.interact_system_state(wid);
            app.run_system_state_action_with_handlers(
                SystemStateAction::TurnSyncOn { by_person: !client },
                Some(handlers()),
            );
        });
        let asked = calls();
        if client {
            assert!(asked.is_empty(), "a client's press: {asked:?}");
        } else {
            assert_eq!(asked, ["sync on"]);
        }
        assert_eq!(app.system_state_host.generation, generation, "no plan");
        app.interact_system_state(wid);
        assert_eq!(app.take_system_state_press(), PagePress::Person);
    }
}

#[test]
fn a_status_due_before_the_next_tick_is_asked_for_and_owes_no_wake_until_the_tick() {
    let now = Instant::now();
    let tick = now + Duration::from_millis(250);
    let in_flight = Host {
        next_status: Some(now),
        status_in_flight: true,
        next_tick: Some(tick),
        ..Host::default()
    };
    assert_eq!(
        in_flight.deadline(now),
        Some(tick),
        "a status in flight owes no wake"
    );

    let mut app = App::headless_for_test();
    app.system_state_host.next_tick = Some(tick);
    app.system_state_host.next_status = Some(now);
    assert_eq!(app.system_state_host.deadline(now), Some(now));
    app.tick_system_state(now);
    assert_eq!(
        app.system_state_host.deadline(now),
        Some(tick),
        "one turn serves the due status, so the next wake is the tick"
    );
}
