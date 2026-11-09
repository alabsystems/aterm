// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A staged update lands by itself with nobody at the Mac (UPD1): no setup offer nobody touched
//! holds the automatic lane past its ladder, and the hold the lane is under is named once, in the
//! log and in `update status`, for as long as the intent it holds lives.

use super::*;

use crate::native_update_lane_hold::LaneHold;
use crate::workstation_setup::{Request, WorkstationSetupDecision};
use aterm_types::macos_setup::Plan;

/// A test App with an OS window and nobody at this Mac: no HID input, no focus, no key, no click.
fn app_nobody_at_the_mac() -> App {
    let mut app = App::headless_for_test();
    app.user_input_recent = |_| false;
    app.windows
        .get_mut(&WindowId(0))
        .expect("the test App has window 0")
        .os_window_for_test = true;
    for window in app.windows.values_mut() {
        window.focused = false;
    }
    app
}

/// The launch's plan, as the window's own (automatic) answer admits one.
fn launch_plan() -> Plan {
    Plan::default()
}

/// The setup worker is busy (a run going, a Check): its status reads busy, the shape that held
/// the lane on main from 0.148.0 until UPD1, where an untouched offer could not be made
/// again while the worker was busy.
fn the_setup_worker_is_busy(app: &mut App) {
    app.receive_system_state_status(
        Some(app.system_state_host.status_generation_for_test()),
        crate::system_state::SetupStatus {
            busy: true,
            plan: Some(launch_plan()),
            ..crate::system_state::SetupStatus::default()
        },
    );
}

/// The launch's offer of its plan, made as the window takes it.
fn offer_the_launch_plan(
    app: &mut App,
) -> (Request, std::sync::mpsc::Receiver<crate::SetupAnswer>) {
    let (send, receive) = std::sync::mpsc::channel();
    let request = Request::new(launch_plan(), send);
    // The window takes an offer only where it has glass; the update lane below runs as the test
    // App's headless handoff does.
    app.headless = false;
    app.offer_workstation_setup(request.clone());
    app.headless = true;
    assert!(request.pending(), "PRECONDITION: the window took the offer");
    (request, receive)
}

/// The launch's offer with a person's draft typed into it (a vault address), which holds the
/// handoff while it is there.
fn offer_with_a_draft(app: &mut App) {
    use crate::native_app::{ActionInvocation, AppEvent, SemanticInput};
    let wid = WindowId(0);
    let (send, _receive) = std::sync::mpsc::channel();
    app.headless = false;
    app.offer_workstation_setup(Request::new(Plan::default(), send));
    let (_, view) = app
        .active_native_view(wid)
        .expect("PRECONDITION: the launch's offer shows System State");
    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("system-state/url"),
            value: Some(SemanticInput::Text("http://draft.local/v1/team".into())),
        }),
    )
    .expect("PRECONDITION: the draft is typed");
    app.headless = true;
    assert_eq!(
        app.workstation_setup_handoff_hold(),
        Some(LaneHold::SetupDraft),
        "PRECONDITION: a draft holds the handoff, as the person's own"
    );
}

/// How many attempts the armed intent has counted (`None`: the lane dropped its promise).
fn attempts(app: &App) -> Option<u8> {
    app.auto_apply_intent.map(|intent| intent.attempts)
}

/// The lines the lane said of the hold named by `token`, as they reached the App's sink.
fn lines_of(app: &App, token: &str) -> (usize, usize) {
    let kept = app.auto_apply_hold_words.kept();
    let named = format!("({token}) since ");
    let over = format!("the hold by {token} is over after ");
    (
        kept.iter().filter(|line| line.contains(&named)).count(),
        kept.iter().filter(|line| line.contains(&over)).count(),
    )
}

/// The updater status the control socket reads for the staged `build` (`update status`).
fn status_of(app: &App, build: u64) -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        enabled: true,
        installable: true,
        current_build: app.native_updater_service.snapshot().current_build,
        staged_build: Some(build),
        staged_version: Some(format!("1.0.{build}")),
        staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
        staged_dmg_sha256: Some("ab".repeat(32)),
        changelog: None,
        outcome: String::new(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        failing_applies: 0,
        failing_since: String::new(),
        failing_persistent: false,
        failing_checks_kind: String::new(),
        channel_unreadable: false,
        linux: None,
    }
}

/// A staged build armed past the ladder's grace, the setup worker busy, the launch's offer up in
/// System State and unanswered, and nobody at the Mac: the first poll attempts, and counts it.
/// Main held the lane here from 0.148.0 until UPD1 (an untouched offer could not be made
/// again while the worker was busy); 0.147.0 and earlier held it on any open offer until a
/// person's touch let the page answer it. An offer nobody has touched holds nothing now, answered
/// or not, whatever the worker is doing.
#[test]
fn an_unanswered_setup_offer_with_its_run_busy_never_holds_the_lane_past_its_ladder() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = app_nobody_at_the_mac();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    let (request, receive) = offer_the_launch_plan(&mut app);
    the_setup_worker_is_busy(&mut app);
    assert!(
        request.pending(),
        "PRECONDITION: no page tick ran, so the offer is unanswered"
    );

    force_auto_apply_attempt_now(&mut app);
    assert_eq!(
        app.automatic_apply_phase(std::time::Instant::now()),
        crate::native_update_auto_intent::ApplyPhase::Land,
        "PRECONDITION: past the ladder's grace"
    );
    assert_eq!(attempts(&app), Some(0), "PRECONDITION: nothing attempted");
    app.try_pending_native_auto_apply(false);
    assert_eq!(
        attempts(&app),
        Some(1),
        "the first poll past the ladder attempts, and the lane keeps its promise: {:?}",
        app.auto_apply_intent
    );
    assert_eq!(app.auto_apply_hold_token(build), None, "no hold was named");
    assert!(
        app.auto_apply_hold_words.kept().is_empty(),
        "nothing held, so nothing said: {:?}",
        app.auto_apply_hold_words.kept()
    );
    assert!(
        !app.workstation_setup_holds_handoff(),
        "an offer nobody touched holds no handoff"
    );
    assert!(
        receive.try_recv().is_err(),
        "the attempt answered nothing for the page: the successor offers the plan again"
    );
}

/// The page answers the launch's offer on its own clock, at its first tick, with nobody at the
/// Mac and nothing focused, shown or pressed; the System State Apply that answer starts holds the
/// lane only until the setup worker takes it, and then the lane attempts with no person's event
/// at all.
#[test]
fn the_page_answers_an_untouched_offer_by_itself_and_the_lane_then_attempts() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = app_nobody_at_the_mac();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    let (request, receive) = offer_the_launch_plan(&mut app);

    // One about-to-wait's setup tick, as the loop runs it with nobody at the Mac.
    app.headless = false;
    app.tick_system_state(std::time::Instant::now());
    app.headless = true;
    assert!(
        !request.pending(),
        "the page answered the offer at its first tick, nobody at the Mac"
    );
    let mut answer = receive
        .try_recv()
        .expect("the page's own answer reached the setup worker");
    assert!(
        matches!(answer.decision, WorkstationSetupDecision::Begin(ref plan) if *plan == launch_plan()),
        "the plan as offered: {:?}",
        answer.decision
    );
    let receipt = answer
        .receipt
        .take()
        .expect("the answer carries its receipt");
    assert_eq!(
        receipt.admission(),
        crate::InitialSetupAdmission::Automatic,
        "the page's own answer, nobody's press"
    );

    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(
        app.auto_apply_hold_token(build),
        Some(LaneHold::SystemState.token()),
        "the answer's Apply holds the lane while the setup worker takes it, named"
    );
    assert_eq!(attempts(&app), Some(0), "PRECONDITION: held, not attempted");

    // The setup worker takes the answer, as the startup does at once.
    receipt.accept();
    app.resolve_system_state_apply();
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_hold_token(build), None, "the hold is over");
    assert_eq!(
        attempts(&app),
        Some(1),
        "the lane attempts once the worker took it: {:?}",
        app.auto_apply_intent
    );
    assert_eq!(
        lines_of(&app, LaneHold::SystemState.token()),
        (1, 1),
        "the hold was said as it began and as it ended: {:?}",
        app.auto_apply_hold_words.kept()
    );
}

/// THE HOLD IS NAMED. A hold past the ladder (here a setup offer with a person's draft in it,
/// which is the person's until it settles) is said once to the log as it begins, however many
/// polls it holds, and `update status` prints it as `apply_held=` under an automatic posture; it
/// writes no standing refusal over the lane's own (`apply_refusal=`); its end is said once, and
/// the lane attempts.
#[test]
fn a_hold_past_the_ladder_is_named_once_and_in_the_status() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = app_nobody_at_the_mac();
    let current = app.native_updater_service.snapshot().current_build;
    let build = current + 1;
    stage_one_build_for_test(&mut app, build);
    offer_with_a_draft(&mut app);

    let token = LaneHold::SetupDraft.token();
    let mut since = None;
    for poll in 0..20 {
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        let held = app
            .auto_apply_intent
            .and_then(|intent| intent.held.current())
            .unwrap_or_else(|| panic!("poll {poll}: the hold is named"));
        assert_eq!(held.hold, LaneHold::SetupDraft, "poll {poll}");
        assert_eq!(
            *since.get_or_insert(held.since),
            held.since,
            "poll {poll}: since its start"
        );
        assert_eq!(attempts(&app), Some(0), "poll {poll}: held");
    }
    assert_eq!(
        lines_of(&app, token),
        (1, 0),
        "twenty polls under one hold say it once: {:?}",
        app.auto_apply_hold_words.kept()
    );

    // `update status`: the headless test App has no handoff, so its live posture is not the
    // automatic one; under the automatic posture a windowed aterm has, `apply_held=` is the hold.
    let status = status_of(&app, build);
    let live = crate::update_control::Snapshot::capture(&app);
    assert_eq!(live.apply_posture(&status), "handoff-unavailable");
    let windowed = crate::update_control::Snapshot::capture_with(
        &app,
        crate::app_update_screen::ApplyPostureEnv {
            handoff_unavailable: None,
            ..crate::app_update_screen::ApplyPostureEnv::observe(&app)
        },
    );
    assert_eq!(windowed.apply_posture(&status), "automatic");
    assert_eq!(
        windowed.apply_held(&status),
        Some(token),
        "`update status` names the hold as apply_held="
    );
    let report = aterm_update::apply_lane_report(current).expect("the scratch ledger");
    assert!(
        !report.last_refusal.contains(token),
        "the hold leaves the lane's own standing refusal as it was: {report:?}"
    );

    // The offer is settled (here as a quit settles it, Unavailable): the hold ends, said once.
    app.finish_system_state_shutdown();
    assert!(!app.workstation_setup_holds_handoff(), "PRECONDITION");
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_hold_token(build), None);
    assert_eq!(
        lines_of(&app, token),
        (1, 1),
        "the end is said once: {:?}",
        app.auto_apply_hold_words.kept()
    );
    assert_eq!(attempts(&app), Some(1), "{:?}", app.auto_apply_intent);
}

/// A named hold never outlives its intent: when the lane drops its promise outside the poll (here
/// auto-apply turned off, the arm's Clear), the hold's end is said, and when the lane is armed
/// again for the same build under the same hold, that arming's hold is named afresh.
#[test]
fn a_hold_ends_with_its_intent_and_a_new_arming_names_its_own() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = app_nobody_at_the_mac();
    let build = app.native_updater_service.snapshot().current_build + 1;
    stage_one_build_for_test(&mut app, build);
    offer_with_a_draft(&mut app);
    let token = LaneHold::SetupDraft.token();
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(lines_of(&app, token), (1, 0), "PRECONDITION: named");

    let config = app.config.update.take();
    app.config.update = Some(crate::app_config::UpdateConfig {
        auto_apply: Some(false),
        ..crate::app_config::UpdateConfig::default()
    });
    assert!(!app.arm_native_auto_apply(build, &"ab".repeat(32)));
    assert!(
        app.auto_apply_intent.is_none(),
        "PRECONDITION: the arm's Clear"
    );
    assert_eq!(
        lines_of(&app, token),
        (1, 1),
        "the dropped intent's hold is over, said once: {:?}",
        app.auto_apply_hold_words.kept()
    );

    app.config.update = config;
    assert!(app.arm_native_auto_apply(build, &"ab".repeat(32)));
    assert_eq!(
        app.auto_apply_hold_token(build),
        None,
        "a new arming holds nothing yet"
    );
    force_auto_apply_attempt_now(&mut app);
    app.try_pending_native_auto_apply(false);
    assert_eq!(app.auto_apply_hold_token(build), Some(token));
    assert_eq!(
        lines_of(&app, token),
        (2, 1),
        "the new arming's hold is named as it begins: {:?}",
        app.auto_apply_hold_words.kept()
    );
}
