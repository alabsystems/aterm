// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The elevation's reading waits for its wake proxy and retries only a failed thread spawn.
/// A started reader runs once, and a launch that owes no reading starts none.
#[test]
fn the_root_steps_read_waits_for_the_wake_proxy_and_runs_once() {
    let mut host = Host::default();
    host.root_steps_unread = true;
    host.root_steps_awaited = true;
    let now = Instant::now();
    let attempts = std::cell::Cell::new(0);
    let attempt = || {
        attempts.set(attempts.get() + 1);
        attempts.get() > 1
    };
    host.start_root_steps_read(false, now, attempt);
    assert_eq!(attempts.get(), 0, "no proxy: no read yet");
    host.start_root_steps_read(true, now, attempt);
    assert_eq!(attempts.get(), 1);
    assert!(host.root_steps_unread, "a failed spawn owes its read");
    assert!(!host.root_steps_awaited, "no answer is in flight");
    assert_eq!(host.deadline(now), Some(now + Duration::from_secs(60)));
    host.start_root_steps_read(true, now + Duration::from_secs(59), attempt);
    assert_eq!(
        attempts.get(),
        1,
        "no spawn on intervening event-loop turns"
    );
    host.start_root_steps_read(false, now + Duration::from_secs(60), attempt);
    assert_eq!(attempts.get(), 1, "a retry still requires the proxy");
    host.start_root_steps_read(true, now + Duration::from_secs(60), attempt);
    assert_eq!(attempts.get(), 2);
    assert!(!host.root_steps_unread, "the reader started");
    assert!(host.root_steps_awaited, "the window awaits its answer");
    assert_eq!(
        host.deadline(now),
        None,
        "a started reader adds no idle clock"
    );
    host.start_root_steps_read(true, now + Duration::from_secs(120), attempt);
    assert_eq!(attempts.get(), 2, "a started reader is not duplicated");
    // Failed starts in a row wait twice as long each time, so a Mac that cannot start
    // threads is not asked every minute.
    let mut failing = Host::default();
    failing.root_steps_unread = true;
    failing.start_root_steps_read(true, now, || false);
    failing.start_root_steps_read(true, now + Duration::from_secs(60), || false);
    assert_eq!(
        failing.deadline(now),
        Some(now + Duration::from_secs(180)),
        "the second failed start waits two minutes"
    );
    Host::default().start_root_steps_read(true, now, || {
        panic!("a launch that owes no reading starts none")
    });
}

/// After an attempt that did not finish, the elevation's other steps are read again where the
/// window reads this Mac (outside a scratch world), and nothing is owed until that reading
/// lands. NEGATIVE CONTROL: a window that reads no Mac, or a scratch world, reads nothing.
#[test]
fn an_attempt_that_did_not_finish_has_the_root_steps_read_again() {
    for (reads_mac, scratch, read) in [
        (true, false, true),
        (false, false, false),
        (true, true, false),
    ] {
        let mut host = Host::default();
        host.developer_tools_reads_mac = reads_mac;
        host.developer_tools_scratch_world = scratch;
        host.developer_tools_root_owed = crate::clt_install::RootSteps {
            debugger: true,
            other: true,
        };
        host.read_root_steps_again();
        assert_eq!(
            host.developer_tools_root_owed,
            crate::clt_install::RootSteps::default()
        );
        assert_eq!(
            (host.root_steps_unread, host.root_steps_awaited),
            (read, read),
            "{reads_mac} {scratch}"
        );
        let started = std::cell::Cell::new(false);
        host.start_root_steps_read(true, Instant::now(), || {
            started.set(true);
            true
        });
        assert_eq!(started.get(), read, "{reads_mac} {scratch}");
    }
}

/// A reading asked for again after a start that failed (its next try a minute or more away)
/// starts its reader at the window's next pass, so the reading the window says it awaits is
/// one that runs, not one held for up to ten minutes behind the old failure's pause; a start
/// that fails again still waits twice as long as the one before.
/// NEGATIVE CONTROL: with no new reading asked for, the failed start's pause holds.
#[test]
fn a_reading_asked_for_again_starts_at_once_after_a_failed_start() {
    let now = Instant::now();
    let later = now + Duration::from_secs(1);
    for again in [false, true] {
        let mut host = Host::default();
        host.developer_tools_reads_mac = true;
        host.root_steps_unread = true;
        host.start_root_steps_read(true, now, || false);
        assert_eq!(host.deadline(now), Some(now + Duration::from_secs(60)));
        if again {
            host.read_root_steps_again();
            assert!(host.root_steps_awaited, "the window awaits the reading");
        }
        let started = std::cell::Cell::new(false);
        host.start_root_steps_read(true, later, || {
            started.set(true);
            true
        });
        assert_eq!(started.get(), again, "{again}");
    }
    let mut host = Host::default();
    host.developer_tools_reads_mac = true;
    host.root_steps_unread = true;
    host.start_root_steps_read(true, now, || false);
    host.read_root_steps_again();
    host.start_root_steps_read(true, later, || false);
    assert_eq!(
        host.deadline(later),
        Some(later + Duration::from_secs(120)),
        "the second failed start in a row waits two minutes"
    );
}

/// F6: a due of the account's record that passed, for a want it holds, wakes one look at the
/// automatic tab with no status or watch to ride; the same due wakes no second one.
/// NEGATIVE CONTROL: no want, no look.
#[test]
fn a_passed_due_of_the_record_wakes_one_look() {
    let mut host = Host::default();
    host.developer_tools = crate::clt_install::Install::Wanted(4);
    assert!(
        host.developer_tools_due_passed(),
        "the default record is due at once"
    );
    assert!(!host.developer_tools_due_passed(), "once per due");
    let mut unwanted = Host::default();
    assert!(!unwanted.developer_tools_due_passed());
}

#[test]
fn automatic_setup_starts_permission_steps() {
    let mut gate = PermissionGate::default();
    gate.apply_started(true, PagePress::Nobody);
    gate.accepted();
    gate.observe(policy(7, true));
    assert!(gate.own_admission_stands());
    assert!(gate.steps_admitted());

    gate.apply_started(false, PagePress::Person);
    gate.accepted();
    gate.observe(policy(8, true));
    assert!(gate.steps_admitted());
}

#[test]
fn permission_policy_holds_until_admission_and_a_fresh_authoritative_snapshot() {
    let allowed = Some(crate::InitialSetupPermissionPolicy {
        revision: 7,
        allowed: true,
    });
    let mut gate = PermissionGate::default();
    assert!(!gate.allowed());
    gate.observe(allowed);
    assert!(
        gate.allowed(),
        "completed startup can restore guidance without an offer"
    );
    gate.hold();
    gate.observe(allowed);
    assert!(
        !gate.allowed(),
        "an old durable plan cannot undo a pending Apply"
    );
    gate.accepted();
    assert!(
        !gate.allowed(),
        "admission cannot release a cached earlier snapshot"
    );
    gate.observe(allowed);
    assert!(gate.allowed());
    gate.observe(None);
    assert!(!gate.allowed(), "an unreadable snapshot holds guidance");
    gate.observe(Some(crate::InitialSetupPermissionPolicy {
        revision: 8,
        allowed: false,
    }));
    gate.observe(allowed);
    assert!(!gate.allowed(), "an older run cannot re-enable guidance");
}

/// A pending offer with no System State page has no draft and no answer in flight: a
/// page's close lets its offer go, and the successor offers the plan again (§6.1).
#[cfg(unix)]
#[test]
fn a_pending_setup_offer_with_no_page_holds_no_handoff() {
    let mut app = App::headless_for_test();
    let (send, _receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.workstation_setup.request = Some(request.clone());
    assert!(request.pending(), "PRECONDITION: the offer is unanswered");
    assert!(
        app.native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .is_none(),
        "PRECONDITION: no System State page"
    );
    assert!(!app.workstation_setup_holds_handoff());

    let mut app = App::headless_for_test();
    app.headless = false;
    app.user_input_recent = |_| false;
    app.windows
        .get_mut(&crate::WindowId(0))
        .expect("window 0")
        .os_window_for_test = true;
    let (send, receive) = mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.offer_workstation_setup(request.clone());
    let instance = app
        .native_runtime
        .instance_by_kind(crate::native_app::AppKind::SystemState)
        .expect("the offer opened its page");
    assert!(request.pending());
    app.finish_system_state_close(instance, crate::native_app::CloseScope::Tab);
    assert!(!request.pending(), "a page's close lets its offer go");
    assert_eq!(
        receive.try_recv().map(|answer| answer.decision),
        Ok(WorkstationSetupDecision::Unavailable),
        "the launch's offer is made again where a page can answer it"
    );
    assert!(!app.workstation_setup_holds_handoff());
}

fn policy(revision: u64, allowed: bool) -> Option<crate::InitialSetupPermissionPolicy> {
    Some(crate::InitialSetupPermissionPolicy { revision, allowed })
}

/// One Apply through the gate as the host runs it: the hold, who it came
/// from, the receipt, and the fresh status naming `revision`. Returns
/// whether that status established the window's own admission.
fn apply(gate: &mut PermissionGate, offer: bool, press: PagePress, revision: u64) -> bool {
    gate.hold();
    gate.apply_started(offer, press);
    gate.accepted();
    gate.observe(policy(revision, true))
}

/// A person's admission is the window's own Apply of a person's press, for the revision it
/// named, while that plan stands; the page's automatic answer is the window's own admission
/// but no person's.
#[test]
fn the_standing_yes_is_the_windows_own_admission_of_the_plan_in_force() {
    let mut gate = PermissionGate::default();
    assert!(apply(&mut gate, true, PagePress::Nobody, 3));
    assert!(
        gate.own_admission_stands() && gate.steps_admitted(),
        "automatic setup starts permission acquisition"
    );

    let mut gate = PermissionGate::default();
    assert!(apply(&mut gate, false, PagePress::Person, 4));
    assert!(gate.steps_admitted(), "a person's press on the page");

    // A newer status naming another plan, a held intent, a plan withdrawn
    // or an unreadable status each end it.
    gate.observe(policy(5, true));
    assert!(
        gate.allowed() && !gate.own_admission_stands(),
        "another plan"
    );
    assert!(apply(&mut gate, true, PagePress::Nobody, 6));
    gate.hold();
    assert!(!gate.own_admission_stands(), "a newer intent held");
    gate.accepted();
    gate.observe(policy(6, true));
    assert!(!gate.own_admission_stands(), "an Apply of nobody's own");
    assert!(apply(&mut gate, true, PagePress::Nobody, 7));
    gate.observe(policy(7, false));
    assert!(!gate.own_admission_stands(), "Permissions not admitted");
    assert!(apply(&mut gate, true, PagePress::Nobody, 8));
    gate.observe(None);
    assert!(!gate.own_admission_stands(), "an unreadable status");
}

/// A control client's Apply, a status poll or a hand-edited record allows the page's
/// guidance but makes no admission of the window's own.
#[test]
fn no_control_client_status_poll_or_record_mints_the_standing_yes() {
    let mut gate = PermissionGate::default();
    assert!(!apply(&mut gate, true, PagePress::Controller, 2));
    assert!(gate.allowed(), "the plan is admitted");
    assert!(!gate.own_admission_stands(), "a control client's Apply");

    let mut gate = PermissionGate::default();
    assert!(!gate.observe(policy(2, true)), "a status poll");
    assert!(gate.allowed() && !gate.own_admission_stands());

    let mut gate = PermissionGate::default();
    assert!(!apply(&mut gate, false, PagePress::Nobody, 2));
    assert!(
        !gate.own_admission_stands(),
        "an Apply that answers no offer"
    );
}

/// A control client's gesture since the page's previous action, anywhere or on the page,
/// taints the press; a person's touch alone is a person's press, and no touch is nobody's.
/// Each take starts afresh.
#[test]
fn a_control_clients_gesture_is_never_a_persons_press() {
    let mut gate = PermissionGate::default();
    assert_eq!(gate.take_press(0), PagePress::Nobody);
    gate.note_touch(false);
    assert_eq!(gate.take_press(0), PagePress::Person);
    assert_eq!(gate.take_press(0), PagePress::Nobody, "taken once");
    gate.note_touch(false);
    assert_eq!(gate.take_press(1), PagePress::Controller, "the count moved");
    gate.note_touch(true);
    assert_eq!(
        gate.take_press(1),
        PagePress::Controller,
        "a controller's touch on the page"
    );
    assert_eq!(gate.take_press(1), PagePress::Nobody, "taken once");
    assert_eq!(gate.take_press(u64::MAX), PagePress::Controller);
    assert_eq!(gate.take_press(u64::MAX), PagePress::Nobody);
}

/// The page's own answer to the startup offer, end to end through the host, comes on the
/// first tick, as the offered plan stands, even after a control
/// client touched the page: a touch holds no answer since design revision 4. It makes the
/// window's own admission (`AdmittedBy::Window`) while a control client drives another tab;
/// a control client's touch on the page itself leaves the plan standing with no standing
/// yes for the access steps.
#[test]
fn the_pages_own_answer_is_the_windows_own_admission() {
    for (elsewhere, on_page) in [(false, false), (true, false), (false, true)] {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.user_input_recent = |_| true;
        let wid = crate::WindowId(0);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        let (send, receive) = std::sync::mpsc::channel();
        app.offer_system_state(Request::new(Plan::default(), send));
        if elsewhere {
            app.with_gesture_source(true, |_| ());
        }
        if on_page {
            app.with_gesture_source(true, |app| app.interact_system_state(wid));
        }
        let start = Instant::now();
        app.tick_system_state(start);
        let mut answer = receive
            .try_recv()
            .expect("the first tick answers the offer");
        assert_eq!(
            answer.decision,
            WorkstationSetupDecision::Begin(Plan::default())
        );
        let receipt = answer.receipt.take().expect("an answer's receipt");
        assert_eq!(
            receipt.admission(),
            crate::system_state::Admission::Automatic
        );
        receipt.accept();
        app.tick_system_state(start + Duration::from_secs(1));
        app.observe_initial_setup_permissions(policy(3, true));
        let admitted = app.permission_steps_admitted();
        if on_page {
            assert!(admitted, "the admitted plan starts automatically");
            assert!(
                app.workstation_setup.permissions.allowed(),
                "the plan stands"
            );
        } else {
            assert!(admitted, "elsewhere {elsewhere}");
        }
    }
}

fn stopped_launch_offer() -> (App, Request, std::sync::mpsc::Receiver<SetupAnswer>) {
    let mut app = App::headless_for_test();
    app.headless = false;
    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    app.workstation_setup = Host::for_launch(
        Some(|| true),
        false,
        crate::clt_shim::Shim::default(),
        false,
    );
    app.observe_initial_setup_permissions(policy(4, true));
    assert_eq!(app.access_revision(), Some(4));
    app.stop_initial_setup_permissions();
    assert!(app.access_revision().is_none());
    let (send, receive) = std::sync::mpsc::channel();
    let request = Request::new(Plan::default(), send);
    app.offer_system_state(request.clone());
    assert!(app.prepare_native_input_scratch(wid));
    assert!(app.commit_glassless_native_frame(wid));
    assert!(request.pending());
    assert!(app.access_revision().is_none());
    (app, request, receive)
}

#[test]
fn a_withdrawn_launch_offer_cannot_release_an_earlier_local_stop() {
    let (mut app, request, receive) = stopped_launch_offer();
    request.finish(WorkstationSetupDecision::Unavailable);
    app.tick_system_state(Instant::now());
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(!request.pending());
    // This is the unchanged policy from before Stop, read after the
    // withdrawn offer released its temporary hold. It admits no choice.
    app.observe_initial_setup_permissions(policy(4, true));
    assert!(
        app.access_revision().is_none(),
        "a withdrawn offer must not release the earlier local Stop"
    );
    assert!(!app.permission_steps_admitted());
    assert!(app.workstation_setup.suppresses_permissions());
}

#[test]
fn an_accepted_press_after_a_stop_rearms_after_its_fresh_policy() {
    let (mut app, _, receive) = stopped_launch_offer();
    let wid = crate::WindowId(0);
    let (_, view) = app.active_native_view(wid).unwrap();
    app.with_gesture_source(false, |app| {
        app.interact_system_state(wid);
        app.dispatch_native_view_event(
            wid,
            view,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("system-state/group/1/leave"),
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
    let receipt = answer.receipt.take().unwrap();
    assert_eq!(receipt.admission(), crate::system_state::Admission::Person);
    app.observe_initial_setup_permissions(policy(4, true));
    assert!(
        app.access_revision().is_none(),
        "delivery is not acceptance"
    );
    receipt.accept();
    app.resolve_system_state_apply();
    assert!(
        app.access_revision().is_none(),
        "acceptance still needs a fresh read"
    );
    app.observe_initial_setup_permissions(policy(5, true));
    assert_eq!(app.access_revision(), Some(5));
    assert!(app.permission_steps_admitted(), "a person's own admission");
}

#[test]
fn startup_waits_for_both_first_paint_and_commit() {
    assert!(!may_start(false, false, false));
    assert!(!may_start(false, true, true));
    assert!(!may_start(true, true, false));
    assert!(may_start(false, true, false));
}

#[test]
fn command_payload_only_qualifies_when_carried_by_a_validated_handoff() {
    assert!(eligible_launch_command(true, true, false, false));
    assert!(eligible_launch_command(true, false, true, true));
    assert!(!eligible_launch_command(true, false, true, false));
    assert!(!eligible_launch_command(true, false, false, true));
    assert!(!eligible_launch_command(true, false, false, false));
}

/// A copy that goes away (its posture is not Installed) never offers setup, however it was
/// launched — it installs itself and relaunches first (design §6.8).
#[test]
fn a_copy_that_goes_away_never_offers_setup() {
    assert!(!eligible_launch_command(false, true, false, false));
    assert!(!eligible_launch_command(false, false, true, true));
    assert!(!eligible_launch_command(false, true, true, true));
}

#[test]
fn preview_replaces_production_only_in_a_private_window() {
    assert_eq!(preview_mode(true, false, true), PreviewMode::Preview);
    assert_eq!(preview_mode(true, true, true), PreviewMode::Refused);
    assert_eq!(preview_mode(true, false, false), PreviewMode::Refused);
    assert_eq!(preview_mode(false, false, false), PreviewMode::Ordinary);
}

#[test]
fn a_failed_worker_spawn_is_retried_after_backoff_and_success_consumes_startup() {
    thread_local! { static ATTEMPTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
    fn attempt() -> bool {
        ATTEMPTS.set(ATTEMPTS.get() + 1);
        ATTEMPTS.get() > 1
    }
    ATTEMPTS.set(0);
    let now = Instant::now();
    let mut host = Host::default();
    host.startup = Some(attempt);
    host.start(now);
    assert_eq!(ATTEMPTS.get(), 1);
    assert_eq!(host.deadline(now), Some(now + Duration::from_secs(60)));
    host.start(now + Duration::from_secs(59));
    assert_eq!(ATTEMPTS.get(), 1);
    host.start(now + Duration::from_secs(60));
    host.start(now + Duration::from_secs(120));
    assert_eq!(ATTEMPTS.get(), 2);
    assert!(host.startup.is_none());
    assert_eq!(host.deadline(now), None);
}

#[test]
fn host_maintenance_survives_startup_and_keeps_only_its_owed_deadline() {
    let now = Instant::now();
    let due = now + Duration::from_secs(5);
    let mut first = true;
    let mut host = Host::default().with_maintenance(Some(Box::new(move || {
        std::mem::take(&mut first).then_some(due)
    })));
    host.startup = Some(|| true);
    host.start(now);
    assert!(host.startup.is_none());
    assert_eq!(host.deadline(now), Some(due));
    host.start(due);
    assert_eq!(
        host.deadline(due),
        None,
        "a healthy keeper adds no idle clock"
    );
    assert!(
        host.maintenance.is_some(),
        "setup completion retains the host's owner"
    );
}

#[test]
fn completion_elsewhere_withdraws_a_still_waiting_offer() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(preview_subset(), send);
    assert_eq!(request.initial.selection, preview_subset());
    assert_eq!(
        wait_for_decision(
            Arc::downgrade(&request.reply),
            receive,
            || true,
            Duration::ZERO
        )
        .decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(!request.pending());
    request.finish(WorkstationSetupDecision::Begin(Selection::all().into()));
}

#[test]
fn closing_the_event_loop_before_dispatch_does_not_leave_the_worker_waiting() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Selection::all(), send);
    let pending = Arc::downgrade(&request.reply);
    drop(request); // the queued Wake was discarded when the event loop closed
    assert_eq!(
        wait_for_decision(pending, receive, || false, Duration::from_secs(60)).decision,
        WorkstationSetupDecision::Unavailable
    );
}

#[test]
fn a_closed_window_cannot_be_overtaken_by_a_late_choice() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Selection::all(), send);
    request.finish(WorkstationSetupDecision::Unavailable);
    request.finish(WorkstationSetupDecision::Begin(Selection::all().into()));
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(!request.pending());
}

#[test]
fn the_first_choice_cannot_be_replaced_by_a_closing_window_or_another_choice() {
    let (send, receive) = mpsc::channel();
    let request = Request::new(Selection::all(), send);
    let chosen = preview_subset();
    assert!(request.finish_answer(WorkstationSetupDecision::Begin(chosen.into()).into()));
    assert!(!request.finish_answer(WorkstationSetupDecision::Unavailable.into()));
    assert!(
        !request.finish_answer(WorkstationSetupDecision::Begin(Selection::none().into()).into())
    );
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Begin(chosen.into())
    );
    assert!(!request.pending());

    let (send, receive) = mpsc::channel();
    let request = Request::new(Selection::all(), send);
    drop(receive);
    assert!(!request.finish_answer(WorkstationSetupDecision::Begin(chosen.into()).into()));
    assert!(!request.pending());
}

/// An offer its page let go ([`Request::let_go`]) admits nothing: it comes back as one no
/// window showed, so the worker makes it again and the page answers it at its first tick,
/// and no Begin handoff runs.
#[test]
fn an_offer_its_page_let_go_admits_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let (send, receive) = mpsc::channel();
    let request = Request::with_handoff(Plan::default(), send, move || {
        called.fetch_add(1, Ordering::AcqRel);
    });
    request.let_go();
    assert_eq!(
        receive.try_recv().unwrap().decision,
        WorkstationSetupDecision::Unavailable
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert!(!request.finish_answer(WorkstationSetupDecision::Begin(Plan::default()).into()));
    assert!(receive.try_recv().is_err());
}

#[test]
fn a_delivered_answer_handoff_precedes_the_return_and_preserves_the_receipt() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let handoffs = Arc::new(AtomicUsize::new(0));
    let count = handoffs.clone();
    let (send, receive) = mpsc::channel();
    let request = Request::with_handoff(Plan::default(), send, move || {
        count.fetch_add(1, Ordering::AcqRel);
    });
    let (receipt, accepted) = crate::InitialSetupApplyReceipt::detached(true);
    let mut answer: SetupAnswer = WorkstationSetupDecision::Begin(Plan::default()).into();
    answer.receipt = Some(receipt);
    assert!(request.finish_answer(answer));
    assert_eq!(handoffs.load(Ordering::Acquire), 1);
    let mut answer = receive.recv_timeout(Duration::from_secs(60)).unwrap();
    assert!(matches!(
        answer.decision,
        WorkstationSetupDecision::Begin(_)
    ));
    assert_eq!(accepted.try_recv(), Err(mpsc::TryRecvError::Empty));
    answer.receipt.take().unwrap().accept();
    assert_eq!(accepted.recv_timeout(Duration::from_secs(60)), Ok(true));
    assert!(
        !request
            .clone()
            .finish_answer(WorkstationSetupDecision::Unavailable.into())
    );
    assert!(!request.finish_answer(WorkstationSetupDecision::Begin(Plan::default()).into()));
    assert_eq!(handoffs.load(Ordering::Acquire), 1);
}

#[test]
fn a_withdrawn_or_disconnected_answer_owns_no_handoff() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for disconnected in [false, true] {
        let handoffs = Arc::new(AtomicUsize::new(0));
        let count = handoffs.clone();
        let (send, receive) = mpsc::channel();
        let request = Request::with_handoff(Plan::default(), send, move || {
            count.fetch_add(1, Ordering::AcqRel);
        });
        if disconnected {
            drop(receive);
        } else {
            request.finish(WorkstationSetupDecision::Unavailable);
            assert_eq!(
                receive
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap()
                    .decision,
                WorkstationSetupDecision::Unavailable
            );
        }
        assert!(!request.finish_answer(WorkstationSetupDecision::Begin(Plan::default()).into()));
        assert!(!request.pending());
        assert_eq!(handoffs.load(Ordering::Acquire), 0);
    }
}

#[test]
fn closing_window_releases_a_waiting_worker() {
    let (send, receive) = mpsc::channel();
    let mut host = Host::default();
    host.offer(Request::new(Selection::all(), send));
    drop(host);
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Unavailable
    );
}

#[test]
fn losing_the_last_window_releases_the_worker_while_the_app_lives() {
    let (send, receive) = mpsc::channel();
    let mut app = App::headless_for_test();
    app.workstation_setup
        .offer(Request::new(Selection::all(), send));
    app.tick_workstation_setup(Instant::now());
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(!app.workstation_setup.pending());
}

#[test]
fn a_window_gaining_focus_counts_as_a_focus_gain() {
    let mut app = App::headless_for_test();
    let seen = window_focus_gains();
    app.window_focus_event(crate::WindowId(0), true);
    assert!(window_focus_gains() > seen);
}

/// An offer that reaches a window nobody has focused is shown in System State at once, not
/// answered Unavailable for the worker to try again a minute later, and the page answers it
/// on its first tick.
#[test]
fn an_offer_to_an_unfocused_window_is_shown_and_answered_at_once() {
    let (send, receive) = mpsc::channel();
    let mut app = App::headless_for_test();
    app.headless = false;
    app.user_input_recent = |_| true;
    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
    app.window_focus_event(wid, false);
    assert!(!app.any_os_window_focused());
    let request = Request::new(Plan::default(), send);
    app.offer_workstation_setup(request.clone());
    assert!(receive.try_recv().is_err(), "not refused for want of focus");
    assert!(app.workstation_setup.pending());
    assert!(
        app.native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .is_some(),
        "System State shows it"
    );
    app.tick_system_state(Instant::now());
    let mut answer = receive.try_recv().expect("the first tick answers it");
    assert_eq!(
        answer.decision,
        WorkstationSetupDecision::Begin(Plan::default())
    );
    answer.receipt.take().unwrap().accept();
    app.resolve_system_state_apply();
    assert!(!request.pending());
}

#[test]
fn an_offer_after_the_last_window_closed_is_unavailable() {
    let (send, receive) = mpsc::channel();
    let mut app = App::headless_for_test();
    app.headless = false;
    app.offer_workstation_setup(Request::new(Selection::all(), send));
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
            .decision,
        WorkstationSetupDecision::Unavailable
    );
    assert!(!app.workstation_setup.pending());
}

/// Past `SETUP_ANSWER_BACKSTOP` the page's own answer holds nothing, and a person's draft in the
/// offer still holds as theirs; no other hold is touched, and inside the backstop nothing is.
#[test]
fn the_pages_own_answer_holds_no_update_past_its_backstop_but_a_draft_does() {
    use crate::native_update_lane_hold::LaneHold;
    use crate::workstation_setup::offer_hold_past_backstop;
    let answer = Some(LaneHold::SetupAnswer);
    assert_eq!(offer_hold_past_backstop(answer, false, || true), answer);
    assert_eq!(offer_hold_past_backstop(answer, true, || false), None);
    assert_eq!(
        offer_hold_past_backstop(answer, true, || true),
        Some(LaneHold::SetupDraft),
        "the person's draft stays theirs"
    );
    for raw in [None, Some(LaneHold::SetupDraft)] {
        assert_eq!(
            offer_hold_past_backstop(raw, true, || unreachable!("no answer to look past")),
            raw
        );
    }
}
