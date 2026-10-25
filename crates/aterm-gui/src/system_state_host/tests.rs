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

#[cfg(target_os = "macos")]
mod queued_panes {
    use super::*;
    use crate::InitialSetupPermissionPolicy;
    use crate::system_state_access::StepsReach;
    use aterm_types::macos_setup::{AccessOutcome, Observation, Standing, Subject};

    thread_local! {
        static QUEUED_PANE_OPENS: RefCell<Vec<Access>> = const { RefCell::new(Vec::new()) };
        static QUEUED_PANE_FRONT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn queued_pane_front(_app: &App) -> bool {
        QUEUED_PANE_FRONT.with(std::cell::Cell::get)
    }
    fn set_queued_pane_front(front: bool) {
        QUEUED_PANE_FRONT.with(|cell| cell.set(front));
    }

    fn queued_pane_privacy(off: Option<&str>) -> crate::app_config::PrivacyConfig {
        let mut privacy = crate::app_config::PrivacyConfig::default();
        match off {
            Some("enabled") => privacy.enabled = Some(false),
            Some("check") => privacy.check = Some(false),
            Some("notice") => privacy.notice = Some(false),
            _ => {}
        }
        privacy
    }

    fn queued_accessibility_status(plan: Plan, standing: Standing) -> SetupStatus {
        SetupStatus {
            plan: Some(plan),
            permissions: Some(InitialSetupPermissionPolicy {
                revision: 1,
                allowed: true,
            }),
            observed: vec![Observation {
                subject: Subject::Access(Access::Accessibility),
                standing,
                now: None,
                after_login: None,
                wanted: Access::Accessibility.wanted().into(),
                note: None,
                detail: None,
            }],
            ..SetupStatus::default()
        }
    }
    fn page_with_queued_accessibility() -> (App, crate::WindowId, crate::native_app::ViewId) {
        use crate::native_app::{ActionInvocation, AppEvent};
        use crate::native_ui::ActionId;
        let mut app = App::headless_for_test();
        // The windowed queue with fully inert reaches: no platform effect is possible.
        app.headless = false;
        app.access_steps.set_reach(StepsReach {
            frontmost: queued_pane_front,
            open_pane: |_, access| {
                QUEUED_PANE_OPENS.with(|opened| opened.borrow_mut().push(access));
                true
            },
            recorded: |_, _| Some(AccessOutcome::Triggered),
            quiet: |_, _| true,
            ..StepsReach::INERT
        });
        app.config.privacy = Some(queued_pane_privacy(None));
        app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
            apply: |_, _, _, receipt| {
                receipt.accept();
                true
            },
            ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
        });
        let wid = crate::WindowId(0);
        assert!(app.open_system_state_tab());
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            queued_accessibility_status(Plan::default(), Standing::NeedsYou),
        );
        app.tick_local_network_step(Instant::now());
        let (_, view) = app.active_native_view(wid).unwrap();
        set_queued_pane_front(false);
        QUEUED_PANE_OPENS.with(|opened| opened.borrow_mut().clear());
        app.with_gesture_source(true, |app| {
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/row/accessibility/open"),
                    value: None,
                }),
            )
        })
        .unwrap();
        app.tick_queued_pane(Instant::now());
        assert!(QUEUED_PANE_OPENS.with(|opened| opened.borrow().is_empty()));
        (app, wid, view)
    }

    #[test]
    fn queued_pane_discards_accessibility_after_accounts_are_left_unmanaged() {
        use crate::native_app::{ActionInvocation, AppEvent};
        use crate::native_ui::ActionId;
        let (mut app, wid, view) = page_with_queued_accessibility();
        app.with_gesture_source(true, |app| {
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("system-state/group/3/leave"),
                    value: None,
                }),
            )
        })
        .unwrap();
        app.resolve_system_state_apply();
        let accepted = app.system_state_host.latest_status().plan.clone().unwrap();
        assert!(!accepted.selection.contains(Setting::Messages));
        assert!(!accepted.selection.contains(Setting::FaceTime));
        // Its old physical reading can still say NeedsYou: the current choice no longer needs it.
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            queued_accessibility_status(accepted, Standing::NeedsYou),
        );
        assert_eq!(
            app.access_revision(),
            Some(1),
            "Permissions stayed approved"
        );
        set_queued_pane_front(true);
        app.tick_queued_pane(Instant::now());
        assert!(
            QUEUED_PANE_OPENS.with(|opened| opened.borrow().is_empty()),
            "the old row action is obsolete"
        );
    }

    #[test]
    fn queued_pane_discards_accessibility_after_a_current_grant_reading() {
        let (mut app, _, _) = page_with_queued_accessibility();
        app.receive_system_state_status(
            Some(app.system_state_host.generation),
            queued_accessibility_status(Plan::default(), Standing::InPlace),
        );
        assert_eq!(app.access_revision(), Some(1));
        set_queued_pane_front(true);
        app.tick_queued_pane(Instant::now());
        assert!(
            QUEUED_PANE_OPENS.with(|opened| opened.borrow().is_empty()),
            "the grant needs no pane now"
        );
    }

    #[test]
    fn queued_pane_current_needs_you_survives_privacy_pause_and_opens_once() {
        for off in ["enabled", "check", "notice"] {
            let (mut app, _, _) = page_with_queued_accessibility();
            set_queued_pane_front(true);
            app.config.privacy = Some(queued_pane_privacy(Some(off)));
            app.tick_queued_pane(Instant::now());
            assert!(QUEUED_PANE_OPENS.with(|opened| opened.borrow().is_empty()));
            app.config.privacy = Some(queued_pane_privacy(None));
            let now = Instant::now();
            app.tick_queued_pane(now);
            assert_eq!(
                QUEUED_PANE_OPENS.with(|opened| opened.borrow().clone()),
                [Access::Accessibility]
            );
            app.tick_queued_pane(now);
            assert_eq!(QUEUED_PANE_OPENS.with(|opened| opened.borrow().len()), 1);
        }
    }
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
fn an_automatic_offer_withdrawn_after_queueing_restores_only_fresh_policy() {
    let policy = || SetupStatus {
        plan: Some(Plan::default()),
        permissions: Some(crate::InitialSetupPermissionPolicy {
            revision: 5,
            allowed: true,
        }),
        ..SetupStatus::default()
    };
    let mut app = App::headless_for_test();
    app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
    let (wid, instance, view) = page(&mut app);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        apply: |_, _, _, _| panic!("a withdrawn startup offer must not become a manual Apply"),
        ..InitialSetupHandlers::inert_for_test()
    });
    fresh(&mut app, policy());
    assert!(!app.workstation_setup.suppresses_permissions());
    assert!(app.admitted_access().is_none());
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    assert!(app.commit_glassless_native_frame(wid));
    let active = app.system_state_presented_active(instance);
    assert!(active, "the real presentation permits automatic admission");
    let before_apply = app.system_state_host.generation;
    assert!(app.native_runtime.tick_system_state(active, true, &[view]));
    assert!(indicators(&app, instance, view).busy);
    assert!(request.pending());
    assert!(receive.try_recv().is_err());

    // The waiting worker can retire its offer after the model's tick queued
    // Apply but before the same host tick delivers the answer.
    request.finish(WorkstationSetupDecision::Unavailable);
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    app.drain_system_state_actions();
    assert!(app.system_state_host.pending.is_none());
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(app.native_runtime.take_system_state_action().is_none());
    assert!(app.system_state_host.action_refusal.is_some());
    assert_ne!(app.system_state_host.generation, before_apply);
    assert!(app.workstation_setup.suppresses_permissions());

    // Neither the cached publication nor a read begun before Apply may
    // restore permission policy, and no unaccepted receipt mints a token.
    app.publish_system_state(policy());
    app.receive_system_state_status(Some(before_apply), policy());
    assert!(app.workstation_setup.suppresses_permissions());
    assert!(app.admitted_access().is_none());
    fresh(&mut app, policy());
    assert!(
        !app.workstation_setup.suppresses_permissions(),
        "an abandoned automatic answer must converge on fresh durable policy"
    );
    assert!(app.admitted_access().is_none());
    assert!(
        receive.try_recv().is_err(),
        "the withdrawn offer stays answered once"
    );
}

#[test]
fn a_rejected_automatic_offer_restores_fresh_policy_unless_stopped() {
    // Exercise Stop first so the original failure cannot hide its control.
    for stopped in [true, false] {
        let policy = || SetupStatus {
            plan: Some(Plan::default()),
            permissions: Some(crate::InitialSetupPermissionPolicy {
                revision: 5,
                allowed: true,
            }),
            ..SetupStatus::default()
        };
        let mut app = App::headless_for_test();
        app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
        let (wid, instance, view) = page(&mut app);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        app.set_system_state_handlers_for_test(InitialSetupHandlers::inert_for_test());
        fresh(&mut app, policy());
        assert!(!app.workstation_setup.suppresses_permissions());
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.attach_system_state_offer(request.clone());
        assert!(app.commit_glassless_native_frame(wid));
        let active = app.system_state_presented_active(instance);
        assert!(active);
        assert!(app.native_runtime.tick_system_state(active, true, &[view]));
        app.drain_system_state_actions();
        let mut answer = receive.try_recv().unwrap();
        assert_eq!(
            answer.decision,
            WorkstationSetupDecision::Begin(Plan::default())
        );
        let receipt = answer.receipt.take().unwrap();
        assert_eq!(receipt.admission(), Admission::Automatic);
        assert!(!request.pending());
        assert!(app.system_state_host.pending.is_none());
        assert!(indicators(&app, instance, view).busy);
        if stopped {
            press(&mut app, wid, view, "system-state/stop");
        }
        assert_eq!(
            app.system_state_host
                .pending_apply
                .as_ref()
                .unwrap()
                .release_permissions,
            !stopped
        );
        let before_rejection = app.system_state_host.generation;
        // A worker drops the receipt when the exact offered ticket was
        // superseded by a doctor operation before it could be admitted.
        drop(receipt);
        app.resolve_system_state_apply();
        assert!(app.system_state_host.pending_apply.is_none());
        assert!(app.system_state_host.action_refusal.is_some());
        assert_ne!(app.system_state_host.generation, before_rejection);
        app.publish_system_state(policy());
        app.receive_system_state_status(Some(before_rejection), policy());
        assert!(app.workstation_setup.suppresses_permissions());
        fresh(&mut app, policy());
        assert_eq!(app.workstation_setup.suppresses_permissions(), stopped);
        assert!(app.admitted_access().is_none());
        assert!(app.native_runtime.take_system_state_action().is_none());
        assert!(receive.try_recv().is_err());
    }
}

#[test]
fn an_automatic_offer_with_a_closed_answer_receiver_restores_fresh_policy() {
    let policy = || SetupStatus {
        plan: Some(Plan::default()),
        permissions: Some(crate::InitialSetupPermissionPolicy {
            revision: 5,
            allowed: true,
        }),
        ..SetupStatus::default()
    };
    let mut app = App::headless_for_test();
    app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
    let (wid, instance, view) = page(&mut app);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    app.set_system_state_handlers_for_test(InitialSetupHandlers::inert_for_test());
    fresh(&mut app, policy());
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.attach_system_state_offer(request.clone());
    assert!(app.commit_glassless_native_frame(wid));
    let active = app.system_state_presented_active(instance);
    assert!(active);
    assert!(app.native_runtime.tick_system_state(active, true, &[view]));
    assert!(request.pending());
    drop(receive);
    let before_apply = app.system_state_host.generation;
    app.drain_system_state_actions();
    assert!(!request.pending());
    assert!(app.system_state_host.pending.is_none());
    assert!(app.system_state_host.pending_apply.is_none());
    assert!(app.system_state_host.action_refusal.is_some());
    app.publish_system_state(policy());
    app.receive_system_state_status(Some(before_apply), policy());
    assert!(app.workstation_setup.suppresses_permissions());
    fresh(&mut app, policy());
    assert!(!app.workstation_setup.suppresses_permissions());
    assert!(app.admitted_access().is_none());
}

#[test]
fn a_refused_local_decision_keeps_its_permission_hold() {
    for intent in ["person", "controller", "restore"] {
        let mut app = App::headless_for_test();
        app.workstation_setup = crate::workstation_setup::Host::for_launch(Some(|| true), false);
        let (wid, _, view) = page(&mut app);
        let policy = || SetupStatus {
            plan: Some(Plan::default()),
            permissions: Some(crate::InitialSetupPermissionPolicy {
                revision: 5,
                allowed: true,
            }),
            ..SetupStatus::default()
        };
        fresh(&mut app, policy());
        assert!(!app.workstation_setup.suppresses_permissions());
        app.set_system_state_handlers_for_test(InitialSetupHandlers {
            apply: |_, _, _, _| false,
            restore: || false,
            ..InitialSetupHandlers::inert_for_test()
        });
        let key = if intent == "restore" {
            "system-state/restore"
        } else {
            "system-state/group/1/leave"
        };
        app.with_gesture_source(intent == "controller", |app| {
            press(app, wid, view, key);
        });
        assert!(app.system_state_host.action_refusal.is_some());
        assert!(app.system_state_host.pending_apply.is_none());
        fresh(&mut app, policy());
        // A Restore, refused or not, withdraws no permission (design §3.4): it holds none.
        assert_eq!(
            app.workstation_setup.suppresses_permissions(),
            intent != "restore",
            "{intent}"
        );
        assert!(app.admitted_access().is_none(), "{intent}");
    }
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
    press(&mut app, wid, view, "system-state/group/1/leave");
    let mut answer = receive.try_recv().unwrap();
    let receipt = answer.receipt.take().expect("a choice answers the offer");
    // A late completion of that old channel cannot retire the newer
    // local action's permission hold, even once its view is idle.
    request.finish(WorkstationSetupDecision::Unavailable);
    app.tick_system_state(Instant::now());
    app.receive_system_state_status(Some(previous), policy());
    fresh(&mut app, policy());
    assert!(app.workstation_setup.suppresses_permissions());
    assert!(app.system_state_host.pending_apply.is_some());
    let before_ack = app.system_state_host.generation;
    receipt.accept();
    app.resolve_system_state_apply();
    app.receive_system_state_status(Some(before_ack), policy());
    assert!(app.workstation_setup.suppresses_permissions());
    fresh(&mut app, policy());
    assert!(!app.workstation_setup.suppresses_permissions());
}

/// A Restore puts back Mac preferences and withdraws none of aterm's permissions (design §3.4):
/// it holds no access step, and one that lets an unanswered offer go leaves the records to
/// decide at the next fresh read.
#[test]
fn a_restore_holds_no_access_step() {
    let policy = || SetupStatus {
        permissions: Some(crate::InitialSetupPermissionPolicy {
            revision: 6,
            allowed: true,
        }),
        ..SetupStatus::default()
    };
    for offered in [false, true] {
        let mut app = App::headless_for_test();
        let (wid, _, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        fresh(&mut app, policy());
        assert!(!app.workstation_setup.suppresses_permissions());
        let previous = app.system_state_host.generation;
        if offered {
            let (send, _receive) = mpsc::channel();
            app.attach_system_state_offer(Request::new(Plan::default(), send));
            assert!(app.workstation_setup.suppresses_permissions());
        }
        press(&mut app, wid, view, "system-state/restore");
        assert_eq!(calls(), ["restore"], "offered={offered}");
        assert_eq!(
            app.workstation_setup.suppresses_permissions(),
            offered,
            "only an unanswered offer's hold waits, for a fresh read"
        );
        // A read asked for before the Restore names no newer plan, so it releases nothing.
        app.receive_system_state_status(Some(previous), policy());
        assert_eq!(app.workstation_setup.suppresses_permissions(), offered);
        fresh(&mut app, policy());
        assert!(
            !app.workstation_setup.suppresses_permissions(),
            "offered={offered}"
        );
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
    app.native_runtime.tick_system_state(true, true, &[view]);
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

/// A choice made during a run reaches the worker, which stops the pass ahead of it as it takes the
/// plan (design §3.3); the window asks for no Stop, then or on a busy read after, so none can reach
/// the choice's own run.
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
fn a_stop_during_admission_keeps_permissions_held_after_the_old_ack() {
    let mut app = App::headless_for_test();
    let (wid, _, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    let allowed = Some(crate::InitialSetupPermissionPolicy {
        revision: 4,
        allowed: true,
    });
    fresh(
        &mut app,
        SetupStatus {
            permissions: allowed,
            ..running()
        },
    );
    press(&mut app, wid, view, "system-state/group/2/leave");
    let before_stop = receipt();
    let chosen = app
        .system_state_host
        .pending_apply
        .as_ref()
        .expect("the choice is awaiting admission")
        .plan
        .clone();
    assert!(!chosen.selection.contains(Setting::Dock));
    press(&mut app, wid, view, "system-state/stop");
    assert_eq!(calls(), ["stop"]);
    assert!(
        app.system_state_host.pending_apply.is_some(),
        "Stop keeps the admission receipt and busy reservation"
    );
    assert_eq!(app.access_revision(), None);
    before_stop.accept();
    app.resolve_system_state_apply();
    assert!(
        app.system_state_host.pending_apply.is_none(),
        "the owned receipt still resolves"
    );
    // The Stop's worker has not saved its file yet, so this otherwise fresh
    // read names the admitted plan and the earlier permission decision.
    fresh(
        &mut app,
        SetupStatus {
            plan: Some(chosen.clone()),
            permissions: allowed,
            ..running()
        },
    );
    assert_eq!(
        app.access_revision(),
        None,
        "an admission sent before Stop cannot reopen permission guidance"
    );
    fresh(
        &mut app,
        SetupStatus {
            plan: Some(chosen),
            permissions: allowed,
            finished: true,
            ..SetupStatus::default()
        },
    );
    assert_eq!(
        app.access_revision(),
        None,
        "even a completed allowed run cannot undo the local Stop"
    );

    press(&mut app, wid, view, "system-state/group/2/set-up");
    receipt().accept();
    app.resolve_system_state_apply();
    fresh(
        &mut app,
        SetupStatus {
            permissions: allowed,
            ..running()
        },
    );
    assert_eq!(
        app.access_revision(),
        Some(4),
        "a new choice after Stop may release its own permission hold"
    );
    assert!(calls().is_empty(), "the new choice does not stop itself");
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

fn accepted_operation_survives_final_close(operation: SetupOperation) {
    let mut app = App::headless_for_test();
    let (wid, original, view) = page(&mut app);
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        apply: |_, _, _, _| {
            note("apply".into());
            false
        },
        ..handlers()
    });
    let key = match operation {
        SetupOperation::Check => "system-state/check",
        SetupOperation::Restore => "system-state/restore",
    };
    press(&mut app, wid, view, key);
    assert!(indicators(&app, original, view).busy);
    assert!(app.commit_glassless_native_frame(wid));
    let headline_key = UiKey::new("settings/page-subtitle/system-state");
    let headline = app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap()
        .compiled
        .semantic(&headline_key)
        .unwrap()
        .label
        .clone();
    let generation = app.system_state_host.generation;
    calls();

    // The worker accepted the operation, but its fresh status is still in
    // flight when the final controller goes away and a new one is opened.
    app.system_state_host.status_in_flight = true;
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.app(original).is_none());
    assert!(app.open_system_state_tab());
    let (reopened, view) = app.active_native_view(wid).unwrap();
    assert_ne!(reopened, original);
    app.tick_system_state(Instant::now());
    calls(); // The newly shown controller starts its prompt-free observation.

    // A native action already in the input queue must obey the operation
    // even before the next status is delivered. Restore also holds groups.
    if operation == SetupOperation::Restore {
        press(&mut app, wid, view, "system-state/group/0/leave");
    }
    for key in [
        "system-state/check",
        "system-state/restore",
        "system-state/vault/join",
    ] {
        press(&mut app, wid, view, key);
    }
    assert!(
        calls().is_empty(),
        "a reopened pending operation permits no second worker"
    );
    assert!(indicators(&app, reopened, view).busy);
    press(&mut app, wid, view, "system-state/row/secrets/disclose");
    assert!(app.commit_glassless_native_frame(wid));
    let ui = &app.windows[&wid].leaf_render_cache[&view]
        .native
        .as_ref()
        .unwrap()
        .compiled;
    assert_eq!(ui.semantic(&headline_key).unwrap().label, headline);
    for key in [
        "system-state/check",
        "system-state/restore",
        "system-state/vault/join",
    ] {
        assert!(
            !ui.semantic(&UiKey::new(key))
                .unwrap()
                .state
                .unwrap()
                .enabled,
            "{key}"
        );
    }
    assert_eq!(
        ui.semantic(&UiKey::new("system-state/group/0/leave"))
            .unwrap()
            .state
            .unwrap()
            .enabled,
        operation == SetupOperation::Check,
        "only Restore holds a newer group choice"
    );

    let idle = SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    };
    app.receive_system_state_status(Some(generation.wrapping_sub(1)), idle.clone());
    assert!(
        indicators(&app, reopened, view).busy,
        "an old idle read cannot finish this operation"
    );
    fresh(
        &mut app,
        SetupStatus {
            busy: true,
            ..idle.clone()
        },
    );
    assert!(indicators(&app, reopened, view).busy);
    fresh(&mut app, idle);
    assert!(!indicators(&app, reopened, view).busy);
    assert!(app.system_state_host.operation.is_none());
    press(&mut app, wid, view, "system-state/group/0/leave");
    assert_eq!(
        calls(),
        ["apply"],
        "a current idle result releases the group choice"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn nonrun_operation_restore_stays_reserved_across_final_close() {
    accepted_operation_survives_final_close(SetupOperation::Restore);
}

#[test]
fn nonrun_operation_check_stays_reserved_across_final_close() {
    accepted_operation_survives_final_close(SetupOperation::Check);
}

#[test]
fn nonrun_operation_refusal_or_reading_alone_reserves_no_check_slot() {
    for (key, reading) in [
        ("system-state/restore", false),
        ("system-state/check", false),
        ("system-state/check", true),
    ] {
        let mut app = App::headless_for_test();
        let (wid, original, view) = page(&mut app);
        app.set_system_state_handlers_for_test(InitialSetupHandlers {
            restore: || false,
            check: if reading {
                |_| CheckStarted {
                    reading: true,
                    check: false,
                }
            } else {
                |_| CheckStarted::default()
            },
            apply: |_, _, _, _| {
                note("apply".into());
                false
            },
            ..handlers()
        });
        press(&mut app, wid, view, key);
        assert!(app.system_state_host.operation.is_none());
        app.system_state_host.status_in_flight = true;
        app.close_active_native_tab(wid).unwrap();
        assert!(app.native_runtime.app(original).is_none());
        assert!(app.open_system_state_tab());
        let (reopened, view) = app.active_native_view(wid).unwrap();
        app.tick_system_state(Instant::now());
        assert!(!indicators(&app, reopened, view).busy);
        calls();
        press(&mut app, wid, view, "system-state/group/0/leave");
        assert_eq!(calls(), ["apply"]);
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn nonrun_operation_reopen_restore_is_disabled_before_the_first_tick() {
    use crate::app_control::{ActRequest, OpenRequest};

    let mut app = App::headless_for_test();
    let (wid, original, view) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    let restore = |view| ActRequest {
        view,
        ui_key: "system-state/restore".into(),
        action: "system-state/restore".into(),
        value: None,
    };
    app.act_app(restore(view)).unwrap();
    assert_eq!(calls(), ["restore"]);
    let generation = app.system_state_host.generation;
    app.system_state_host.status_in_flight = true;
    let poll = (
        app.system_state_host.status_in_flight,
        app.system_state_host.next_status,
        app.system_state_host.next_tick,
    );
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.app(original).is_none());
    app.open_app(OpenRequest::SystemState).unwrap();
    let (reopened, view) = app.active_native_view(wid).unwrap();
    // These are consecutive supported control requests: no park, periodic
    // tick or fresh status has run between opening and this live action.
    assert!(app.act_app(restore(view)).is_err());
    assert!(
        calls().is_empty(),
        "the first compiled controls keep Restore disabled"
    );
    assert!(indicators(&app, reopened, view).busy);
    assert_eq!(
        (
            app.system_state_host.status_in_flight,
            app.system_state_host.next_status,
            app.system_state_host.next_tick,
        ),
        poll,
        "opening a controller does not restart or retire its status read"
    );
    assert!(
        !app.system_state_host.latest_status.busy,
        "the cached worker status stays raw"
    );
    let idle = SetupStatus {
        plan: Some(Plan::default()),
        ..SetupStatus::default()
    };
    app.receive_system_state_status(Some(generation.wrapping_sub(1)), idle.clone());
    assert!(app.act_app(restore(view)).is_err());
    assert!(calls().is_empty());
    fresh(&mut app, idle);
    app.act_app(restore(view)).unwrap();
    assert_eq!(
        calls(),
        ["restore"],
        "a current idle result enables one new Restore"
    );
    assert!(app.structural_invariants_ok());
}

fn queued_offer_respects_host_work(
    start: impl Fn(&mut App, WindowId, ViewId),
    reopen_before_tick: bool,
) {
    // A passive busy controller, then either no remaining controller or a
    // freshly reopened controller that has not received its first tick yet.
    for close in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app.windows[&wid].tab_set.active_index().unwrap();
        let (_, original, view) = page(&mut app);
        app.set_system_state_handlers_for_test(handlers());
        let offered = Plan {
            vault_url: "http://queued-offer.invalid/v1/team".into(),
            ..Plan::default()
        };
        let (send, receive) = mpsc::channel();
        // Its worker already queued this request when the page started work.
        let request = Request::new(offered.clone(), send);
        start(&mut app, wid, view);
        assert!(indicators(&app, original, view).busy);
        let current = app.system_state_host.latest_status.plan.clone();
        app.system_state_host.status_in_flight = true;
        calls();
        if close {
            app.close_active_native_tab(wid).unwrap();
            assert!(app.native_runtime.app(original).is_none());
            if reopen_before_tick {
                assert!(app.open_system_state_tab());
            }
        } else {
            app.switch_tab_in(wid, terminal);
        }
        let before = (
            app.windows[&wid].tab_set.active_id(),
            app.windows[&wid].window_focus,
            app.windows[&wid].tab_set.len(),
        );
        // Exercise the actual windowed preflight, with no OS surface and
        // the headless fixture's inert platform reaches.
        assert!(!app.apprt.probe_preferences);
        assert!(app.windows[&wid].os_window.is_none());
        app.headless = false;
        app.offer_system_state(request.clone());
        assert!(
            !request.pending(),
            "a queued offer must yield to accepted host work"
        );
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert_eq!(
            (
                app.windows[&wid].tab_set.active_id(),
                app.windows[&wid].window_focus,
                app.windows[&wid].tab_set.len(),
            ),
            before,
            "a refused offer neither opens nor focuses a tab"
        );
        assert!(app.system_state_host.pending.is_none());
        assert!(app.native_runtime.take_system_state_action().is_none());
        assert!(calls().is_empty());

        // The same offer is admissible after a current idle read releases
        // the host reservation, including when no controller survived.
        fresh(
            &mut app,
            SetupStatus {
                plan: current,
                ..SetupStatus::default()
            },
        );
        let (send, receive) = mpsc::channel();
        let request = Request::new(offered.clone(), send);
        app.offer_system_state(request.clone());
        assert!(request.pending());
        assert!(app.system_state_host.pending.is_some());
        let Some(NativeApp::SystemState(model)) = app
            .native_runtime
            .app(app.active_native_view(wid).unwrap().0)
        else {
            panic!("the idle offer opens its review")
        };
        assert_eq!(model.reviewed_plan(), offered);
        request.finish(WorkstationSetupDecision::Unavailable);
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(app.structural_invariants_ok());
    }
}

#[test]
fn queued_offer_cannot_take_a_pending_restore_after_controller_close() {
    queued_offer_respects_host_work(
        |app, wid, view| {
            press(app, wid, view, "system-state/restore");
        },
        false,
    );
}

#[test]
fn queued_offer_cannot_take_a_pending_check_after_controller_close() {
    queued_offer_respects_host_work(
        |app, wid, view| {
            press(app, wid, view, "system-state/check");
        },
        false,
    );
}

#[test]
fn queued_offer_cannot_take_an_admission_awaiting_its_run_after_controller_close() {
    queued_offer_respects_host_work(
        |app, wid, view| {
            press(app, wid, view, "system-state/group/2/leave");
            receipt().accept();
            app.resolve_system_state_apply();
        },
        false,
    );
}

#[test]
fn queued_offer_cannot_take_an_observed_busy_worker_after_controller_close() {
    queued_offer_respects_host_work(|app, _, _| fresh(app, running()), false);
}

#[test]
fn queued_offer_cannot_take_a_reopened_restore_before_its_first_tick() {
    queued_offer_respects_host_work(
        |app, wid, view| {
            press(app, wid, view, "system-state/restore");
        },
        true,
    );
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
    assert_eq!(
        app.observe_system_state_with(instance, Some(handlers())),
        Observe::NoPlan
    );
    press(&mut app, wid, view, "system-state/group/1/leave");
    assert!(app.native_runtime.take_system_state_action().is_none());
    assert!(calls().is_empty());
    fresh(&mut app, in_force());
    assert_eq!(
        app.observe_system_state_with(instance, Some(handlers())),
        Observe::Started
    );
    assert_eq!(calls(), ["observe http://saved.local/v1/team"]);
    let Some(state) = app.native_runtime.system_state_mut() else {
        panic!("System State is open");
    };
    assert_eq!(state.reviewed_plan(), saved);

    // Opened after a read: its first controls know that exact saved plan;
    // showing the controller still asks for one fresh observation.
    let mut app = App::headless_for_test();
    app.set_system_state_handlers_for_test(handlers());
    app.publish_system_state(in_force());
    assert!(app.open_system_state_tab());
    let state = app.native_runtime.system_state_mut().unwrap();
    assert!(state.plan_known());
    assert_eq!(state.reviewed_plan(), saved);
    assert!(calls().is_empty(), "bootstrap itself starts no worker");
    let now = Instant::now();
    app.system_state_host.next_status = Some(now + Duration::from_secs(3600));
    app.tick_system_state(now);
    assert_eq!(calls(), ["observe http://saved.local/v1/team"]);
    app.tick_system_state(now + Duration::from_millis(250));
    assert!(
        calls().is_empty(),
        "the accepted observation is not repeated"
    );
    assert_eq!(
        app.native_runtime
            .system_state_mut()
            .unwrap()
            .reviewed_plan(),
        saved
    );
}

mod observation_start {
    use super::*;
    use crate::app_control::ActRequest;

    thread_local! {
        static READS: RefCell<Vec<Plan>> = const { RefCell::new(Vec::new()) };
        static ACCEPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn reads() -> Vec<Plan> {
        READS.with(|reads| reads.borrow().clone())
    }

    fn setup(accept: bool) -> (App, WindowId, AppInstanceId, ViewId, Plan) {
        READS.with(|reads| reads.borrow_mut().clear());
        ACCEPT.with(|accepted| accepted.set(accept));
        let mut app = App::headless_for_test();
        app.set_system_state_handlers_for_test(InitialSetupHandlers {
            observe: |plan| {
                READS.with(|reads| reads.borrow_mut().push(plan));
                ACCEPT.with(std::cell::Cell::get)
            },
            ..InitialSetupHandlers::inert_for_test()
        });
        let (wid, instance, view) = page(&mut app);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        let mut saved = Plan {
            selection: aterm_types::macos_setup::Selection::none(),
            vault_url: "http://saved.invalid:7484/v1/team".into(),
        };
        saved.selection.set(Setting::Dock, true);
        fresh(
            &mut app,
            SetupStatus {
                plan: Some(saved.clone()),
                ..SetupStatus::default()
            },
        );
        (app, wid, instance, view, saved)
    }

    fn tick(app: &mut App, now: Instant) {
        // Status polling is independent of this injected worker-start outcome.
        app.system_state_host.next_status = Some(now + Duration::from_secs(3600));
        app.tick_system_state(now);
    }

    #[test]
    fn observation_start_refusal_retries_the_current_review_then_settles_once() {
        let (mut app, _, _, view, saved) = setup(false);
        let start = Instant::now();
        tick(&mut app, start);
        assert_eq!(reads().as_slice(), std::slice::from_ref(&saved));
        for millis in [250, 500, 750] {
            tick(&mut app, start + Duration::from_millis(millis));
        }
        assert_eq!(reads().len(), 1, "a failed start does not retry each tick");
        for (key, value) in [
            ("system-state/row/secrets/disclose", None),
            (
                "system-state/url",
                Some("http://reviewed.invalid:7484/v1/team"),
            ),
        ] {
            app.act_app(ActRequest {
                view,
                ui_key: key.into(),
                action: key.into(),
                value: value.map(str::to_owned),
            })
            .unwrap();
        }
        ACCEPT.with(|accepted| accepted.set(true));
        tick(&mut app, start + Duration::from_secs(1));
        let reviewed = Plan {
            vault_url: "http://reviewed.invalid:7484/v1/team".into(),
            ..saved.clone()
        };
        assert_eq!(
            reads(),
            [saved, reviewed],
            "the retry reads the current exact review"
        );
        for seconds in 2..=4 {
            tick(&mut app, start + Duration::from_secs(seconds));
        }
        assert_eq!(
            reads().len(),
            2,
            "an accepted read is not repeated while shown"
        );
    }

    #[test]
    fn observation_start_acceptance_belongs_to_the_live_controller() {
        let (mut app, wid, instance, _, saved) = setup(true);
        let start = Instant::now();
        tick(&mut app, start);
        assert_eq!(
            reads().as_slice(),
            std::slice::from_ref(&saved),
            "an initially accepted read runs once"
        );
        app.stage_system_state_tab(wid).unwrap();
        tick(&mut app, start + Duration::from_millis(250));
        assert_eq!(
            reads().len(),
            1,
            "a linked view shares its controller's accepted read"
        );
        app.close_active_native_tab(wid).unwrap();
        tick(&mut app, start + Duration::from_millis(500));
        assert_eq!(reads().len(), 1);
        app.close_active_native_tab(wid).unwrap();
        assert!(app.native_runtime.app(instance).is_none());
        let reopened = Plan {
            vault_url: "http://reopened.invalid:7484/v1/team".into(),
            ..saved.clone()
        };
        app.publish_system_state(SetupStatus {
            plan: Some(reopened.clone()),
            ..SetupStatus::default()
        });
        assert!(app.open_system_state_tab());
        tick(&mut app, start + Duration::from_millis(750));
        assert_eq!(
            reads(),
            [saved, reopened],
            "a new controller reads its own saved plan"
        );
        tick(&mut app, start + Duration::from_secs(2));
        assert_eq!(reads().len(), 2);
    }
}

#[test]
fn unknown_plan_check_waits_for_the_saved_plan_after_reopen() {
    fn saved_plan() -> Plan {
        let mut selection = aterm_types::macos_setup::Selection::none();
        selection.set(Setting::Permissions, true);
        selection.set(Setting::Dock, true);
        Plan {
            selection,
            vault_url: "http://saved.local:7484/v1/team".into(),
        }
    }
    let mut app = App::headless_for_test();
    let (wid, first_instance, _) = page(&mut app);
    app.set_system_state_handlers_for_test(handlers());
    fresh(
        &mut app,
        SetupStatus {
            plan: Some(saved_plan()),
            ..SetupStatus::default()
        },
    );
    app.close_active_native_tab(wid).unwrap();
    assert!(app.native_runtime.app(first_instance).is_none());
    // A current read could not recover the saved plan while no controller
    // existed. Reopening must keep Check gated rather than use its defaults.
    fresh(
        &mut app,
        SetupStatus {
            rows: vec![SetupStatusRow {
                name: "Setup state".into(),
                status: "error".into(),
                detail: "The saved setup plan could not be read.".into(),
            }],
            ..SetupStatus::default()
        },
    );
    assert!(app.open_system_state_tab());
    let (_, view) = app.active_native_view(wid).unwrap();
    assert!(!app.native_runtime.system_state_mut().unwrap().plan_known());

    // The new controller still has no trusted plan, even before any tick.
    press(&mut app, wid, view, "system-state/check");
    assert!(
        calls().is_empty(),
        "no default-plan Check or reading starts"
    );
    assert!(app.native_runtime.take_system_state_action().is_none());

    fresh(
        &mut app,
        SetupStatus {
            plan: Some(saved_plan()),
            ..SetupStatus::default()
        },
    );
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        check: |plan| {
            assert_eq!(plan, saved_plan());
            note("checked saved plan".into());
            CheckStarted {
                reading: true,
                check: true,
            }
        },
        observe: |plan| {
            assert_eq!(plan, saved_plan());
            note("read saved plan".into());
            true
        },
        ..handlers()
    });
    press(&mut app, wid, view, "system-state/check");
    press(&mut app, wid, view, "system-state/check");
    assert_eq!(calls(), ["checked saved plan", "read saved plan"]);
    assert!(app.system_state_host.pending_apply.is_none());
    assert_eq!(
        app.native_runtime
            .system_state_mut()
            .unwrap()
            .reviewed_plan(),
        saved_plan()
    );
    assert!(app.structural_invariants_ok());
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

/// A Check's result belongs to exactly the plan it read, typed vault address included: it can fail
/// with the observed rows in place, and once a later edit hides it, no poll restores it.
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
                observed_vault_url: Some(saved.vault_url.clone()),
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
            // A later field edit hides the note, and a poll of the same status does not restore it.
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
fn vault_readings_follow_the_edited_source_across_polls() {
    use aterm_types::macos_setup::{Standing, Subject};

    let old_code = "OLD-VAULT-APPROVAL-42";
    let new_code = "NEW-VAULT-APPROVAL-83";
    let contains = |app: &App, wid, text: &str| {
        app.compiled_native_ui(wid)
            .unwrap()
            .semantics
            .iter()
            .any(|node| node.label.contains(text))
    };
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    let mut old = fixture_status("set-up").unwrap();
    for row in &mut old.observed {
        match row.subject {
            Subject::Setting(Setting::Secrets) => {
                row.standing = Standing::NeedsYou;
                row.note = Some(old_code.into());
            }
            Subject::Setting(Setting::Repositories) => row.standing = Standing::Failed,
            _ => {}
        }
    }
    fresh(&mut app, old.clone());
    assert!(contains(&app, wid, old_code));
    assert!(indicators(&app, instance, view).attention);
    let independent = app
        .compiled_native_ui(wid)
        .unwrap()
        .semantic(&UiKey::new("system-state/row/caps-lock/info"))
        .unwrap()
        .label
        .clone();
    let edited = "http://new-vault.local:7484/v1/team";
    type_into(
        &mut app,
        wid,
        view,
        "system-state/url",
        &format!("  {edited}  "),
    );
    assert!(!contains(&app, wid, old_code));
    assert!(!indicators(&app, instance, view).attention);
    fresh(&mut app, old.clone());
    assert!(
        !contains(&app, wid, old_code),
        "polls cannot reinstall the old code"
    );
    assert!(!indicators(&app, instance, view).attention);
    assert!(contains(&app, wid, &independent));
    assert!(
        crate::system_state_access::menu_line(&old, false).is_some(),
        "the menu still describes the saved source, not an unsubmitted draft"
    );

    let mut current = old.clone();
    current.plan.as_mut().unwrap().vault_url = edited.into();
    current.observed_vault_url = Some(format!("  {edited}  "));
    for row in &mut current.observed {
        match row.subject {
            Subject::Setting(Setting::Secrets) => row.note = Some(new_code.into()),
            Subject::Setting(Setting::Repositories) => row.standing = Standing::InPlace,
            _ => {}
        }
    }
    fresh(&mut app, current.clone());
    assert!(contains(&app, wid, new_code));
    type_into(
        &mut app,
        wid,
        view,
        "system-state/url",
        &Plan::default().vault_url,
    );
    assert!(!contains(&app, wid, new_code));
    fresh(&mut app, current.clone());
    assert!(!contains(&app, wid, new_code));
    assert!(contains(&app, wid, &independent));

    let mut unknown = current;
    unknown.observed_vault_url = None;
    assert!(crate::system_state_access::menu_line(&unknown, false).is_none());
    fresh(&mut app, unknown);
    type_into(&mut app, wid, view, "system-state/url", edited);
    assert!(!contains(&app, wid, new_code));
    assert!(!indicators(&app, instance, view).attention);
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

    // A pass held the worker when Check again was pressed: no check started, so nothing is
    // labelled one, but its forced reading ran, so the page never says Check again did not start.
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

/// A row's Open acted by a control client (`aterm ctl app act system-state/row/<slug>/open`)
/// spends the press evidence it left, as every other action does; left pending, it would taint
/// the person's next press (a Turn On… that starts nothing, an Apply that mints no admission).
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
        // The accepted pass ends before the next independent action is enabled.
        let accepted = app.system_state_host.latest_status.plan.clone().unwrap();
        fresh(
            &mut app,
            SetupStatus {
                plan: Some(accepted),
                ..SetupStatus::default()
            },
        );
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
    // The startup's offer arrives before the choice's answer: the choice answers for this launch,
    // so the offer is let go and the window still reads the choice's answer.
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
    // Idle ticks with identical worker snapshots and no input keep the shown frame.
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

/// The person's press on the vault row's Turn On Sync asks the worker to install the subscriber
/// they turned off (design §3.3); a control client's press turns nothing on and spends its
/// evidence, so the person's next press is still theirs.
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

/// A page in a minimized, hidden (Cmd-H) or fully covered window (winit's Occluded, recorded by
/// `on_occlusion_changed`) is not shown: its village does not walk and the window is not redrawn
/// for it until the window is back on screen.
#[test]
fn a_page_in_an_occluded_window_is_not_shown() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    let (_, instance, _view) = page(&mut app);
    assert!(app.commit_glassless_native_frame(wid));
    fresh(&mut app, running());
    assert!(
        !app.system_state_shown_views(instance).is_empty(),
        "in front, it is shown"
    );
    app.on_occlusion_changed(wid, true);
    let shown = !app.system_state_shown_views(instance).is_empty();
    let seen = app.system_state_seen_views(instance);
    let walked = (0..8)
        .filter(|_| app.native_runtime.tick_system_state(false, shown, &seen))
        .count();
    assert_eq!(
        (shown, seen.len(), walked),
        (false, 0, 0),
        "(shown, views seen, walk steps asking a repaint in 8 ticks) of a page in an \
         occluded window"
    );
    assert!(
        app.system_state_showing_windows().is_empty(),
        "nothing to redraw"
    );
    app.on_occlusion_changed(wid, false);
    assert!(
        !app.system_state_shown_views(instance).is_empty(),
        "back on screen, it is shown"
    );
    assert_eq!(app.system_state_showing_windows(), [wid]);
}

/// A walking tick redraws only the windows that show the page: one where the
/// page is a background tab is left alone, and one that shows the page in an
/// unfocused split pane is redrawn with the window that shows it in front.
#[test]
fn a_walking_tick_redraws_only_the_windows_that_show_the_page() {
    let mut app = App::headless_for_test();
    let (front, background, split) = (WindowId(0), WindowId(1), WindowId(2));
    app.install_window_state(background, crate::stub_session(91), 90, 160);
    app.install_window_state(split, crate::stub_session(92), 90, 160);
    // The page opens in the background window first, which then goes back to
    // its terminal tab.
    assert!(app.open_system_state_tab_in_window(background));
    app.switch_tab_in(background, 0);
    assert!(app.active_native_view(background).is_none());
    // The split window's page has a terminal beside it, which has the focus.
    assert!(app.open_system_state_tab_in_window(split));
    let _ = app.split_active_with_stub_terminal(split, crate::tab_model::SplitAxis::Vertical);
    assert!(app.active_native_view(split).is_none());
    // A window installed last is frontmost; the front window opens its own
    // view of the page.
    app.frontmost_window = Some(front);
    let (_, _, view) = page(&mut app);
    fresh(&mut app, running());
    assert!(app.commit_glassless_native_frame(front));
    let walks = |app: &App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::SystemState(state)) => state.village_walks(),
        _ => panic!("the front window's view is a System State view"),
    };
    assert!(walks(&app), "the front window draws a walking village");
    let revision = |app: &App| {
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .presentation_revision
    };
    let before = revision(&app);
    app.tick_system_state(Instant::now());
    assert_ne!(revision(&app), before, "the tick walks the shown village");
    assert_eq!(
        app.system_state_showing_windows(),
        [front, split],
        "the window where the page is a background tab is not redrawn"
    );
}

/// A page in an unfocused split pane is seen: a run walks its village though the front window's
/// page is too short for one, because the split pane's own render decides it.
#[test]
fn a_split_panes_village_walks_beside_a_front_page_too_short_for_one() {
    let mut app = App::headless_for_test();
    let split = WindowId(1);
    app.install_window_state(split, crate::stub_session(92), 90, 160);
    assert!(app.open_system_state_tab_in_window(split));
    let _ = app.split_active_with_stub_terminal(split, crate::tab_model::SplitAxis::Vertical);
    assert!(app.active_native_view(split).is_none());
    app.frontmost_window = Some(WindowId(0));
    let (_, instance, front) = page(&mut app);
    fresh(&mut app, running());
    let beside = app
        .active_visible_leaf_plan(split)
        .and_then(|plan| {
            plan.leaves.into_iter().map(|leaf| leaf.view).find(|view| {
                matches!(app.view_store.get(*view),
                    Some(crate::tab_model::View::Native(native)) if native.instance == instance)
            })
        })
        .expect("the split window shows the page beside its terminal");
    let render = |app: &App, view: ViewId, width: f32, height: f32| {
        let viewport = crate::native_ui::LogicalRect::new(0.0, 0.0, width, height);
        let cx = crate::native_app::ViewCx {
            viewport,
            animation_phase_ms: 0,
            motion: crate::native_app::ViewMotionCx::default(),
            terminal_font_px: 13.0,
            terminal_scale: 2.0,
            semantic_font: None,
            document: None,
        };
        app.native_runtime.render(instance, view, &cx).unwrap();
    };
    let walks = |app: &App, view: ViewId| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::SystemState(state)) => state.village_walks(),
        _ => panic!("{view:?} is a System State view"),
    };
    render(&app, beside, 999.0, 900.0);
    render(&app, front, 999.0, 200.0);
    assert!(
        walks(&app, beside),
        "the split pane draws a walking village"
    );
    assert!(!walks(&app, front), "the short front page leaves it out");
    let revision = |app: &App| {
        app.native_runtime
            .view_state(front)
            .unwrap()
            .common()
            .presentation_revision
    };
    let before = revision(&app);
    app.tick_system_state(Instant::now());
    assert_ne!(
        revision(&app),
        before,
        "the run walks the village the split pane shows"
    );
}

/// A page key moves System State's list a page of its room, as it moves Settings'.
#[test]
fn a_page_key_moves_the_list_a_page_of_its_room() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let press = |key| crate::input::InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    fresh(&mut app, fixture_status("check-failed").expect("a fixture"));
    let render = |app: &App| {
        let cx = crate::native_app::ViewCx {
            viewport: crate::native_ui::LogicalRect::new(0.0, 0.0, 720.0, 124.0),
            animation_phase_ms: 0,
            motion: crate::native_app::ViewMotionCx::default(),
            terminal_font_px: 13.0,
            terminal_scale: 2.0,
            semantic_font: None,
            document: None,
        };
        app.native_runtime.render(instance, view, &cx).unwrap();
    };
    let list = |app: &App| match app.native_runtime.view_state(view) {
        Some(crate::native_app::AppViewState::SystemState(state)) => state.list_scroll(),
        _ => panic!("{view:?} is a System State view"),
    };
    render(&app);
    let (top, room) = list(&app);
    assert!(app.native_input_event(wid, &press(NamedKey::PageDown)));
    let (down, _) = list(&app);
    assert!(
        down > top + 0.5 && down - top <= room + 0.01,
        "PageDown moved the list from {top} to {down} in a room of {room}"
    );
    assert!(app.native_input_event(wid, &press(NamedKey::PageUp)));
    let (up, _) = list(&app);
    assert!(
        up < down - 0.5 && down - up <= room + 0.01,
        "PageUp moved the list from {down} to {up} in a room of {room}"
    );
}

/// The vault's address field takes the keys a Settings field takes: the
/// readline keys, ⌥←/→ by word, and Home/End, where they reached nothing,
/// and the find bar's ⌥⌫/⌥⌦ by word and ⌘⌫/⌘←/⌘→ to the edges.
#[test]
fn the_vault_address_takes_the_text_keys_a_settings_field_takes() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let key = |key, mods| crate::input::InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let ctrl = |c| key(Key::Character(c), Modifiers::CTRL);
    let mut app = App::headless_for_test();
    let (wid, instance, view) = page(&mut app);
    let url = UiKey::new("system-state/url");
    app.dispatch_native_view_event(wid, view, AppEvent::FocusChanged(Some(url)))
        .unwrap();
    for event in [
        crate::native_app::TextInputEvent::SelectAll,
        crate::native_app::TextInputEvent::Commit("vault.example/one two".into()),
    ] {
        app.dispatch_native_view_event(wid, view, AppEvent::TextInput(event))
            .unwrap();
    }
    let field = |app: &App| match app.native_runtime.app(instance) {
        Some(NativeApp::SystemState(page)) => {
            let input = page.text_input("system-state/url").expect("the address");
            (input.value().to_owned(), input.selection().range())
        }
        _ => panic!("a System State page"),
    };
    let end = "vault.example/one two".len();
    assert_eq!(field(&app), ("vault.example/one two".to_owned(), end..end));
    assert!(app.native_input_event(wid, &ctrl('a')));
    assert_eq!(field(&app).1, 0..0, "Ctrl-A");
    assert!(app.native_input_event(wid, &ctrl('e')));
    assert_eq!(field(&app).1, end..end, "Ctrl-E");
    assert!(app.native_input_event(wid, &key(Key::Named(NamedKey::ArrowLeft), Modifiers::ALT)));
    assert_eq!(field(&app).1, end - 3..end - 3, "⌥← to the word's start");
    assert!(app.native_input_event(wid, &ctrl('e')));
    assert!(app.native_input_event(wid, &key(Key::Named(NamedKey::ArrowLeft), Modifiers::META)));
    assert_eq!(field(&app).1, end - 3..end - 3, "Meta-← as ⌥←");
    assert!(app.native_input_event(wid, &key(Key::Named(NamedKey::Home), Modifiers::empty())));
    assert_eq!(field(&app).1, 0..0, "Home");
    assert!(app.native_input_event(wid, &key(Key::Named(NamedKey::End), Modifiers::empty())));
    assert_eq!(field(&app).1, end..end, "End");
    assert!(app.native_input_event(wid, &key(Key::Named(NamedKey::Home), Modifiers::SUPER)));
    assert_eq!(
        field(&app).1,
        end..end,
        "⌘-Home scrolls the list, not the caret"
    );
    assert!(app.native_input_event(wid, &ctrl('w')));
    assert_eq!(field(&app).0, "vault.example/one ", "Ctrl-W");
    assert!(app.native_input_event(wid, &ctrl('u')));
    assert_eq!(field(&app).0, "", "Ctrl-U");
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
            "one two three".into(),
        )),
    )
    .unwrap();
    let named = |named, mods| key(Key::Named(named), mods);
    assert!(app.native_input_event(wid, &named(NamedKey::Backspace, Modifiers::ALT)));
    assert_eq!(field(&app).0, "one two ", "⌥⌫");
    assert!(app.native_input_event(wid, &ctrl('h')));
    assert_eq!(field(&app).0, "one two", "Ctrl-H");
    assert!(app.native_input_event(wid, &named(NamedKey::ArrowLeft, Modifiers::SUPER)));
    assert_eq!(field(&app).1, 0..0, "⌘←");
    assert!(app.native_input_event(wid, &named(NamedKey::Delete, Modifiers::ALT)));
    assert_eq!(field(&app), (" two".to_owned(), 0..0), "⌥⌦");
    assert!(app.native_input_event(wid, &named(NamedKey::ArrowRight, Modifiers::SUPER)));
    assert_eq!(field(&app).1, 4..4, "⌘→");
    assert!(app.native_input_event(wid, &named(NamedKey::Backspace, Modifiers::SUPER)));
    assert_eq!(field(&app).0, "", "⌘⌫");
}

/// A focused window macOS reports occluded (the screen locked, the display asleep, a
/// screen saver up: the key window stays key) shows nothing, so System State is not in
/// front there: the sign-outs, the access dialogs and the debugger step, which all read
/// `system_state_frontmost`, wait for a page someone sees.
#[test]
fn an_occluded_focused_window_is_not_in_front_for_on_screen_steps() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    app.windows.get_mut(&wid).unwrap().focused = true;
    let (_, _instance, _view) = page(&mut app);
    assert!(app.commit_glassless_native_frame(wid));
    fresh(&mut app, running());
    assert!(
        app.system_state_frontmost(),
        "precondition: the page is in front"
    );
    app.on_occlusion_changed(wid, true);
    assert!(
        !app.system_state_frontmost(),
        "an occluded window puts the sign-outs and macOS prompts on a screen nobody sees"
    );
    app.on_occlusion_changed(wid, false);
    assert!(
        app.system_state_frontmost(),
        "back on screen, it is in front"
    );
}

fn page_publishing_front_with_a_background_copy() -> (App, WindowId, WindowId, AppInstanceId) {
    // Each caller runs alone in a child: unrelated tests also publish this
    // process-wide signal, which is the setup worker's on_screen port.
    aterm_types::macos_setup::set_system_state_front(false);
    let mut app = App::headless_for_test();
    app.set_system_state_handlers_for_test(InitialSetupHandlers::inert_for_test());
    let (front, instance, _) = page(&mut app);
    let background = WindowId(1);
    app.install_window_state(background, crate::stub_session(91), 90, 160);
    app.stage_system_state_tab(background).unwrap();
    app.switch_tab_in(background, 0);
    app.windows.get_mut(&front).unwrap().os_window_for_test = true;
    app.windows.get_mut(&front).unwrap().focused = true;
    app.windows.get_mut(&background).unwrap().focused = false;
    app.frontmost_window = Some(front);
    fresh(&mut app, running());
    assert!(app.commit_glassless_native_frame(front));
    assert!(app.system_state_frontmost());
    app.tick_system_state(Instant::now());
    assert!(aterm_types::macos_setup::system_state_front());
    (app, front, background, instance)
}

#[test]
fn published_frontness_final_window_close_retires_on_screen_work() {
    if !crate::test_alone::alone() {
        return;
    }
    let (mut app, front, background, instance) = page_publishing_front_with_a_background_copy();
    // Disposing of a passive linked window must not withdraw the surviving
    // focused page's authority.
    assert_eq!(
        app.close_window_logical(background),
        crate::CloseOutcome::Stay
    );
    assert!(app.system_state_frontmost());
    assert!(aterm_types::macos_setup::system_state_front());
    assert!(app.structural_invariants_ok());
    assert_eq!(app.close_window_logical(front), crate::CloseOutcome::Exit);
    assert!(app.native_runtime.app(instance).is_none());
    assert!(!app.system_state_frontmost());
    assert!(
        !aterm_types::macos_setup::system_state_front(),
        "a worker must not start an on-screen step after the final window closes"
    );
    assert!(app.windows.is_empty());
    assert!(app.frontmost_window.is_none());
    assert!(app.view_store.iter().next().is_none());
}

#[test]
fn published_frontness_front_tab_close_retires_work_with_a_background_copy() {
    if !crate::test_alone::alone() {
        return;
    }
    let (mut app, front, _, instance) = page_publishing_front_with_a_background_copy();
    app.close_active_native_tab(front).unwrap();
    assert!(
        app.native_runtime.app(instance).is_some(),
        "the background copy survives"
    );
    assert!(!app.system_state_frontmost());
    assert!(
        !aterm_types::macos_setup::system_state_front(),
        "a hidden linked controller cannot authorize an on-screen step"
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn published_frontness_quit_retires_work_before_settling_workers() {
    if !crate::test_alone::alone() {
        return;
    }
    let (mut app, front, _, instance) = page_publishing_front_with_a_background_copy();
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        settle: || {
            assert!(
                !aterm_types::macos_setup::system_state_front(),
                "shutdown must withdraw on-screen work before it waits for workers"
            );
            note("settled".into());
        },
        ..InitialSetupHandlers::inert_for_test()
    });
    app.finish_system_state_shutdown();
    assert_eq!(calls(), ["settled"]);
    // Cmd-Q need not dispose of the windows/models before worker settlement.
    assert!(app.windows.contains_key(&front));
    assert!(app.native_runtime.app(instance).is_some());
    assert!(app.system_state_frontmost());
    assert!(!aterm_types::macos_setup::system_state_front());
}

#[cfg(target_os = "macos")]
#[test]
fn published_frontness_handoff_wait_holds_work_until_the_live_page_is_republished() {
    if !crate::test_alone::alone() {
        return;
    }
    let (mut app, _, _, _) = page_publishing_front_with_a_background_copy();
    app.set_system_state_handlers_for_test(InitialSetupHandlers {
        settle: || {
            assert!(!aterm_types::macos_setup::system_state_front());
            note("settled".into());
        },
        ..InitialSetupHandlers::inert_for_test()
    });
    app.settle_system_state_before_handoff();
    assert_eq!(calls(), ["settled"]);
    assert!(!aterm_types::macos_setup::system_state_front());
    assert!(app.system_state_frontmost());
    // A handoff can be refused: the UI is still live, and an ordinary tick
    // may publish its focused page again without inventing another admission.
    app.tick_system_state(Instant::now());
    assert!(aterm_types::macos_setup::system_state_front());
}

/// How many readings of the page's rows `calls` asked for.
fn observes(calls: &[String]) -> usize {
    calls
        .iter()
        .filter(|call| call.starts_with("observe"))
        .count()
}

/// A page opened by hand before the first status names the plan waits for it (§3.5), with no
/// guess read for; the first tick that knows the plan reads the rows, never a second later
/// (that wait is a refused start's alone).
#[test]
fn a_page_opened_before_the_plan_reads_at_the_first_tick_that_knows_it() {
    let mut app = App::headless_for_test();
    app.set_system_state_handlers_for_test(handlers());
    let window = app.windows.get_mut(&WindowId(0)).unwrap();
    window.rows = 90;
    window.cols = 160;
    assert!(app.open_system_state_tab());
    let start = Instant::now();
    app.system_state_host.next_status = Some(start + Duration::from_secs(3600));
    app.tick_system_state(start);
    assert_eq!(observes(&calls()), 0, "no guess is read for");
    fresh(
        &mut app,
        SetupStatus {
            plan: Some(Plan::default()),
            ..SetupStatus::default()
        },
    );
    app.system_state_host.next_status = Some(start + Duration::from_secs(3600));
    app.tick_system_state(start + Duration::from_millis(250));
    assert_eq!(
        observes(&calls()),
        1,
        "the first tick that knows the plan reads the rows"
    );
}

/// A plan changed outside the page (`aterm doctor --restore` in a terminal leaves Keyboard &
/// Trackpad and Desktop & Dock as is) reaches a shown page only as a status naming that plan:
/// the page reads its rows once more, so it never shows the values from before as the Mac's.
#[test]
fn a_plan_changed_outside_the_page_reads_the_rows_again() {
    let mut app = App::headless_for_test();
    app.set_system_state_handlers_for_test(handlers());
    let (_, _, _) = page(&mut app);
    let start = Instant::now();
    app.system_state_host.next_status = Some(start + Duration::from_secs(3600));
    app.tick_system_state(start);
    assert_eq!(observes(&calls()), 1, "shown: one reading");
    let mut restored = Plan::default();
    for group in [
        aterm_types::macos_setup::Group::Input,
        aterm_types::macos_setup::Group::Desktop,
    ] {
        for setting in group.settings() {
            restored.selection.set(*setting, false);
        }
    }
    fresh(
        &mut app,
        SetupStatus {
            plan: Some(restored),
            finished: true,
            ..SetupStatus::default()
        },
    );
    for millis in [250, 500, 750, 1000, 2000] {
        app.system_state_host.next_status = Some(start + Duration::from_secs(3600));
        app.tick_system_state(start + Duration::from_millis(millis));
    }
    assert_eq!(
        observes(&calls()),
        1,
        "a plan changed outside the page is read again, once"
    );
}
