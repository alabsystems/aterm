// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use aterm_tempfile::TempDir;
use std::time::{Duration, Instant};

use super::AskpassSheet;
use crate::app_update_handoff::SameImageHandoff;
use crate::askpass::{ASKPASS_UNTOUCHED_BACKSTOP, AskpassAnswer};
use crate::cpu_policy_authorization::Starter;
use crate::native_update_lane_hold::LaneHold;
use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
use crate::{App, UpdateHandoffStartError, WindowId};

const WINDOW: WindowId = WindowId(0);

/// Past the untouched-sheet backstop, by a second.
const OVERDUE: Duration = ASKPASS_UNTOUCHED_BACKSTOP.saturating_add(Duration::from_secs(1));

/// Who applies, each with the arguments its caller passes `App::apply_staged_update_now`.
#[derive(Clone, Copy, Debug)]
enum Applier {
    /// The automatic lane's apply of a staged update, with its attempt.
    Lane,
    /// A person's Install and Relaunch (`App::apply_native_update`), which carries its attempt
    /// as the lane's apply does.
    PersonsPress,
    /// A launch-record renewal: automatic, with no update attempt.
    Renewal,
    /// The debug seam's same-binary reexec: immediate, with no update attempt.
    DebugSeam,
}

/// A headless App whose window shows aterm's own password sheet, raised `stood` ago for session
/// 0's `sudo` and touched by a person or not, and the channel that `sudo` hears its answer on.
fn app_with_sheet(stood: Duration, touched: bool) -> (App, Receiver<AskpassAnswer>) {
    let mut app = App::headless_for_test();
    let (reply, answer) = channel();
    let raised = Instant::now()
        .checked_sub(stood)
        .expect("the clock reaches back that far");
    let (sheet, input) = AskpassSheet::headless_for_test(WINDOW, 0, raised, reply);
    if touched {
        input.person_touched();
    }
    app.askpass_sheet = Some(sheet);
    (app, answer)
}

fn apply(app: &mut App, applier: Applier) -> Result<(), UpdateHandoffStartError> {
    let attempt = || Some(ApplyAttemptTicket::for_test(2, "0123456789ab", "00"));
    let (mode, attempt, same_image) = match applier {
        Applier::Lane => (ApplyMode::Automatic, attempt(), None),
        Applier::PersonsPress => (ApplyMode::Immediate, attempt(), None),
        Applier::Renewal => (
            ApplyMode::Automatic,
            None,
            Some(SameImageHandoff::RenewLaunchRecord),
        ),
        Applier::DebugSeam => (ApplyMode::Immediate, None, None),
    };
    app.apply_staged_update_now(
        crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
        mode,
        attempt,
        same_image,
    )
}

/// The hold an apply was deferred for, and the words it said.
fn deferred(result: &Result<(), UpdateHandoffStartError>) -> Option<(LaneHold, &str)> {
    match result {
        Err(UpdateHandoffStartError::ActivityDeferred(hold, words)) => Some((*hold, words)),
        _ => None,
    }
}

/// AN UPDATE'S APPLY, THE LANE'S OR A PERSON'S, REFUSES ATERM'S OWN SHEET NOBODY TOUCHED FOR ITS
/// BACKSTOP, AT THE APPLY ITSELF: the attempt that refuses it is still held by it, so the `sudo`
/// that asked settles before the next attempt parks, and that `sudo` is told how long no sheet is
/// raised, its window's, its session's and every window's spacing starting. Before, the sheet held
/// every attempt until its own deadline.
#[test]
fn an_updates_apply_refuses_the_sheet_nobody_touched_past_its_backstop() {
    for applier in [Applier::Lane, Applier::PersonsPress] {
        let (mut app, answer) = app_with_sheet(OVERDUE, false);
        let held = apply(&mut app, applier);
        assert_eq!(
            deferred(&held),
            Some((
                LaneHold::PasswordPrompt,
                "an untouched sudo password prompt was refused; the next attempt goes on"
            )),
            "{applier:?}: {held:?}"
        );
        assert!(
            app.askpass_sheet.is_none(),
            "{applier:?}: the sheet is taken down"
        );
        let Ok(AskpassAnswer::Quiet(wait)) = answer.try_recv() else {
            panic!("{applier:?}: the sudo that asked hears how long no sheet is raised");
        };
        assert_eq!(wait, crate::askpass::QUIET_BASE, "{applier:?}");
        assert!(
            app.askpass_backoff
                .quiet_for(WINDOW.0, Instant::now())
                .is_some(),
            "{applier:?}: its window waits before it asks again"
        );
        assert!(
            app.askpass_backoff
                .refusal(7, 77, None, Instant::now())
                .is_some(),
            "{applier:?}: so does every window"
        );
    }
}

/// A SHEET THE APPLY DOES NOT REFUSE HOLDS IT, UNTOUCHED: one a person clicked on or typed at is
/// theirs; one still within its backstop is waited for; and an apply that carries no update
/// attempt, a launch-record renewal (automatic) or the debug seam's reexec, never refuses one.
#[test]
fn a_persons_sheet_a_fresh_one_and_an_apply_with_no_attempt_refuse_nothing() {
    for (stood, touched, applier, case) in [
        (OVERDUE, true, Applier::Lane, "a sheet a person touched"),
        (
            OVERDUE,
            true,
            Applier::PersonsPress,
            "a sheet a person touched",
        ),
        (
            Duration::ZERO,
            false,
            Applier::Lane,
            "a sheet within its backstop",
        ),
        (OVERDUE, false, Applier::Renewal, "a launch-record renewal"),
        (
            OVERDUE,
            false,
            Applier::DebugSeam,
            "the debug seam's reexec",
        ),
    ] {
        let (mut app, answer) = app_with_sheet(stood, touched);
        let held = apply(&mut app, applier);
        assert_eq!(
            deferred(&held),
            Some((
                LaneHold::PasswordPrompt,
                "a sudo password prompt is still open"
            )),
            "{case} ({applier:?}): {held:?}"
        );
        assert!(
            app.askpass_sheet.is_some(),
            "{case} ({applier:?}): the sheet stays up"
        );
        assert!(
            matches!(answer.try_recv(), Err(TryRecvError::Empty)),
            "{case} ({applier:?}): the sudo hears nothing"
        );
        assert!(
            app.askpass_backoff
                .quiet_for(WINDOW.0, Instant::now())
                .is_none(),
            "{case} ({applier:?}): no spacing"
        );
    }
}

/// AN UNTOUCHED SHEET IS REFUSED ONLY FOR AN ATTEMPT THAT WOULD GO: while a tab is still starting,
/// which holds the attempt anyway, the attempt names that hold and the sheet stays up, so a
/// `sudo` is never failed for an update that would not go.
#[test]
fn an_untouched_sheet_is_refused_only_once_nothing_else_holds_the_attempt() {
    let (mut app, answer) = app_with_sheet(OVERDUE, false);
    crate::tab_spawn::test_support::book(&mut app, WINDOW, crate::tab_spawn::TabSpawnThen::Nothing);
    let held = apply(&mut app, Applier::Lane);
    assert_eq!(
        deferred(&held).map(|(hold, _)| hold),
        Some(LaneHold::TabStarting),
        "{held:?}"
    );
    assert!(app.askpass_sheet.is_some(), "the sheet stays up");
    assert!(
        matches!(answer.try_recv(), Err(TryRecvError::Empty)),
        "the sudo hears nothing"
    );
}

/// THE PASSWORD SHEET OF A PAGE'S COLLECT HANGS ON THE PAGE'S WINDOW AND GOES WITH THE VIEW: the
/// collection the System Resources page's view started raises its sheet (headless, as no window
/// shows it) on that view's window, standing for the view, with no terminal session in front;
/// bringing a terminal tab in front withdraws the sheet, its ticket and the operation with it.
/// That withdrawal spaces no window, as a closing window's does not, but the sheet nobody touched
/// makes every window wait, unless a person's gesture comes after it.
/// CONTROL: a withdrawal by another view leaves the sheet standing.
#[test]
fn the_password_sheet_of_a_page_collect_hangs_on_the_pages_window_and_goes_with_the_view() {
    use super::SheetOrigin;
    use crate::askpass::{ASKPASS_DEADLINE, AskpassRequest, AskpassTicket};
    use crate::cpu_policy_authorization::{active_for_test, read_identity, slot_for_test};
    use std::sync::Arc;
    let _slot = slot_for_test();
    let mut app = App::headless_for_test();
    let terminal = app.windows[&WINDOW].tab_set.active().expect("a tab").id;
    app.open_resources_tab().expect("the page opens");
    let (_, view) = app
        .active_native_view(WINDOW)
        .expect("the page is in front");
    assert_eq!(
        app.collect_cpu_policy_from_view(WINDOW, view, Starter::Person(Instant::now())),
        Ok(())
    );
    let op = active_for_test().expect("the page's operation");
    // The worker binds the sudo it spawned before sudo may ask; this process stands in for it.
    op.bind_sudo_for_test(read_identity(std::process::id()).expect("this process reads"));
    let ticket = Arc::new(AskpassTicket::new(7, Instant::now() + ASKPASS_DEADLINE));
    let (reply, answer) = channel();
    app.present_askpass_sheet(AskpassRequest {
        ticket: Arc::clone(&ticket),
        attribute_from: 0,
        owned: Some(crate::root_step_authorization::OwnedOperation::CpuPolicy(
            Arc::clone(&op),
        )),
        exact: true,
        command: "aterm cpu-policy-collector".into(),
        purpose: None,
        invoker: "System Resources".into(),
        reply,
    });
    assert!(
        matches!(answer.try_recv(), Err(TryRecvError::Empty)),
        "the sheet is raised, not refused"
    );
    assert_eq!(
        app.askpass_sheet
            .as_ref()
            .map(AskpassSheet::origin_for_test),
        Some((WINDOW, SheetOrigin::View(view))),
        "it hangs on the page's window and stands for its view"
    );

    let other = crate::tab_model::ViewId::from_stored(view.get() + 1000);
    app.withdraw_askpass_view(other);
    assert!(
        app.askpass_sheet.is_some() && ticket.is_live(Instant::now()),
        "CONTROL: another view's withdrawal leaves it standing"
    );

    let back = app.windows[&WINDOW]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.id == terminal)
        .expect("the terminal tab");
    app.switch_tab_in(WINDOW, back);
    assert!(app.askpass_sheet.is_none(), "the sheet goes with the view");
    assert!(!ticket.is_live(Instant::now()), "its ticket is withdrawn");
    assert!(op.cancelled_for_test(), "and the operation is cancelled");
    let now = Instant::now();
    assert_eq!(
        app.askpass_backoff.quiet_for(WINDOW.0, now),
        None,
        "a view's operation is a person's press: its withdrawal spaces no window"
    );
    assert!(
        app.askpass_backoff.refusal(WINDOW.0 + 1, 99, None, now) > Some(Duration::ZERO),
        "the sheet nobody touched makes every window wait"
    );
    assert_eq!(
        app.askpass_backoff
            .unattended_refusal(Some(now + Duration::from_millis(1)), now),
        None,
        "but not a person's gesture after it"
    );
}

/// Present the one askpass request of the CPU policy operation `op`, whose sudo this process
/// stands in for, and return what that sudo hears.
fn present_cpu_policy_request(
    app: &mut App,
    op: &std::sync::Arc<crate::cpu_policy_authorization::Operation>,
    id: u64,
) -> Receiver<AskpassAnswer> {
    use crate::askpass::{ASKPASS_DEADLINE, AskpassRequest, AskpassTicket};
    use std::sync::Arc;
    // The worker binds the sudo it spawned before sudo may ask; this process stands in for it.
    op.bind_sudo_for_test(
        crate::cpu_policy_authorization::read_identity(std::process::id())
            .expect("this process reads"),
    );
    let (reply, answer) = channel();
    app.present_askpass_sheet(AskpassRequest {
        ticket: Arc::new(AskpassTicket::new(id, Instant::now() + ASKPASS_DEADLINE)),
        attribute_from: 0,
        owned: Some(crate::root_step_authorization::OwnedOperation::CpuPolicy(
            Arc::clone(op),
        )),
        exact: true,
        command: "aterm cpu-policy-collector".into(),
        purpose: None,
        invoker: "System Resources".into(),
        reply,
    });
    answer
}

/// A PERSON'S PRESS ON THE PAGE NEVER WAITS OUT A PROGRAM'S SPACING: a terminal `sudo`'s sheet
/// left unanswered spaces its window's next program sheet, and a Collect pressed on the System
/// Resources page in that window still raises its sheet there, standing for the page's view. Only
/// a person's press starts a page's operation, and it asks once, so no program re-asks through it.
/// The sheet nobody touched still holds a press made before it ended, as it holds every window.
/// CONTROL: the same collection started by a person's key from the terminal session in front of
/// that window, inside the same quiet window, is refused for the spacing and raises nothing; and
/// so is the page's press made before the program's sheet ended.
#[test]
fn a_page_press_raises_its_sheet_in_a_window_a_programs_unanswered_sheet_spaced() {
    use super::SheetOrigin;
    use crate::cpu_policy_authorization::{active_for_test, slot_for_test};
    let slot = slot_for_test();
    let (mut app, _program) = app_with_sheet(Duration::ZERO, false);
    let before = Instant::now();
    app.withdraw_askpass_sheet(1, Instant::now());
    assert!(
        app.askpass_sheet.is_none()
            && app
                .askpass_backoff
                .quiet_for(WINDOW.0, Instant::now())
                .is_some(),
        "PRECONDITION: the program's sheet went unanswered and spaces its window"
    );

    assert_eq!(
        app.collect_cpu_policy(WINDOW, Starter::Person(Instant::now())),
        Ok(())
    );
    let op = active_for_test().expect("the session's operation");
    let answer = present_cpu_policy_request(&mut app, &op, 7);
    let heard = answer.try_recv();
    assert!(
        matches!(heard, Ok(AskpassAnswer::Quiet(_))),
        "CONTROL: the session's collection is refused for the spacing: {heard:?}"
    );
    assert!(app.askpass_sheet.is_none(), "CONTROL: and raises nothing");

    slot.empty();
    app.open_resources_tab().expect("the page opens");
    let (_, view) = app
        .active_native_view(WINDOW)
        .expect("the page is in front");
    assert_eq!(
        app.collect_cpu_policy_from_view(WINDOW, view, Starter::Person(before)),
        Ok(())
    );
    let op = active_for_test().expect("the page's early operation");
    let heard = present_cpu_policy_request(&mut app, &op, 8).try_recv();
    assert!(
        matches!(heard, Ok(AskpassAnswer::Quiet(_))),
        "CONTROL: a press made before the program's sheet ended waits: {heard:?}"
    );
    assert!(app.askpass_sheet.is_none(), "CONTROL: and raises nothing");

    slot.empty();
    assert_eq!(
        app.collect_cpu_policy_from_view(WINDOW, view, Starter::Person(Instant::now())),
        Ok(())
    );
    let op = active_for_test().expect("the page's operation");
    let answer = present_cpu_policy_request(&mut app, &op, 9);
    assert!(
        matches!(answer.try_recv(), Err(TryRecvError::Empty)),
        "the page's sheet is raised, not refused"
    );
    assert_eq!(
        app.askpass_sheet
            .as_ref()
            .map(AskpassSheet::origin_for_test),
        Some((WINDOW, SheetOrigin::View(view))),
        "it hangs on the spaced window and stands for the page's view"
    );
}

/// THE PAGE'S CANCEL TAKES ITS SHEET DOWN AS A PERSON'S CANCEL: the System Resources page's
/// Cancel of a CPU collection whose sheet stands, untouched, ends that sheet through
/// `end_askpass_sheet` as Declined. Its window waits, as after a Cancel on the sheet; no other
/// window does, since a person's no is to that operation; and the record no longer names the
/// sheet as standing, so no later launch counts it as a crash's.
/// NEGATIVE CONTROLS: ended Lapsed (a withdrawal), the untouched sheet would make every window
/// wait; ended Gone, its window would not wait; dropped with no end, its window would not wait
/// either and the record would still name it standing.
#[test]
fn the_pages_cancel_takes_its_sheet_down_as_a_persons_cancel() {
    use crate::cpu_policy_authorization::{active_for_test, slot_for_test};
    let _slot = slot_for_test();
    let mut app = App::headless_for_test();
    let dir = TempDir::new().expect("a scratch state dir");
    app.askpass_record = Some(dir.path().to_path_buf());
    app.open_resources_tab().expect("the page opens");
    let (_, view) = app
        .active_native_view(WINDOW)
        .expect("the page is in front");
    assert_eq!(
        app.collect_cpu_policy_from_view(WINDOW, view, Starter::Person(Instant::now())),
        Ok(())
    );
    let op = active_for_test().expect("the page's operation");
    let _answer = present_cpu_policy_request(&mut app, &op, 7);
    assert!(
        app.askpass_sheet.is_some(),
        "PRECONDITION: the sheet stands"
    );
    assert!(
        record_in(&dir).is_some_and(|(_, _, standing)| standing.is_some()),
        "PRECONDITION: the record names it standing"
    );

    app.cancel_cpu_policy_from_page();
    let now = Instant::now();
    assert!(
        app.askpass_sheet.is_none(),
        "the Cancel takes the sheet down"
    );
    assert!(op.cancelled_for_test(), "and ends its operation");
    assert!(
        app.askpass_backoff.quiet_for(WINDOW.0, now).is_some(),
        "its window waits, as after a Cancel"
    );
    assert_eq!(
        app.askpass_backoff.refusal(WINDOW.0 + 1, 99, None, now),
        None,
        "no other window waits: a person's no is to that operation"
    );
    assert_eq!(
        record_in(&dir),
        None,
        "the record no longer names the sheet standing, and holds no unattended wait"
    );
}

/// A sheet nobody answered (its deadline passed, its caller hung up, its request was withdrawn:
/// all one wake from the waiting lane, which finds the sheet still standing) spaces its window's
/// next sheet exactly as a Cancel does, and a second one, raised as that quiet time ran out and
/// left to its own deadline, widens it.
/// NEGATIVE CONTROLS: a withdrawal that names another request leaves the sheet and spaces
/// nothing; the lane's late withdrawal of a sheet a person already cancelled counts nothing
/// more; a session that closes under its sheet spaces the window, another session's close does
/// nothing, and a closing window drops its sheet and spaces no window of its own.
#[test]
fn a_sheet_withdrawn_unanswered_spaces_its_window_as_a_cancel_does() {
    use crate::askpass::{QUIET_BASE, QUIET_MAX};
    let now = Instant::now();
    let quiet = |app: &App| app.askpass_backoff.quiet_for(WINDOW.0, now);
    let streak = |app: &App| app.askpass_backoff.carried(WINDOW.0, now).map(|(n, _)| n);

    let (mut app, answer) = app_with_sheet(Duration::ZERO, false);
    app.withdraw_askpass_sheet(1, now);
    assert!(app.askpass_sheet.is_none());
    assert_eq!(quiet(&app), Some(QUIET_BASE));
    assert!(matches!(answer.try_recv(), Err(TryRecvError::Disconnected)));
    let (reply, _answer) = channel();
    let raised = now + QUIET_BASE;
    let deadline = raised + crate::askpass::ASKPASS_DEADLINE;
    app.askpass_sheet = Some(AskpassSheet::headless_for_test(WINDOW, 0, raised, reply).0);
    app.withdraw_askpass_sheet(1, deadline);
    assert_eq!(
        app.askpass_backoff.quiet_for(WINDOW.0, deadline),
        Some(QUIET_BASE * 2)
    );
    assert!(QUIET_BASE * 2 < QUIET_MAX);

    let (mut app, _answer) = app_with_sheet(Duration::ZERO, false);
    app.withdraw_askpass_sheet(2, now);
    assert!(app.askpass_sheet.is_some());
    assert_eq!(quiet(&app), None);

    // A person's Cancel takes the sheet and counts it; the lane's wake then finds nothing.
    let (mut app, answer) = app_with_sheet(Duration::ZERO, false);
    app.answer_askpass_sheet(1, false);
    assert!(matches!(answer.try_recv(), Ok(AskpassAnswer::Cancel)));
    assert_eq!(streak(&app), Some(1));
    app.withdraw_askpass_sheet(1, now);
    assert_eq!(streak(&app), Some(1), "one sheet, counted once");

    let (mut app, _answer) = app_with_sheet(Duration::ZERO, false);
    app.withdraw_askpass_session(99, now);
    assert!(app.askpass_sheet.is_some());
    assert_eq!(quiet(&app), None);
    app.withdraw_askpass_session(0, now);
    assert!(app.askpass_sheet.is_none());
    assert_eq!(quiet(&app), Some(QUIET_BASE));

    let (mut app, _answer) = app_with_sheet(Duration::ZERO, false);
    app.withdraw_askpass_window(WINDOW);
    assert!(app.askpass_sheet.is_none());
    assert_eq!(quiet(&app), None);
}

/// [`app_with_sheet`], its spacing record kept in a scratch dir it returns.
fn app_with_record(stood: Duration, touched: bool) -> (App, Receiver<AskpassAnswer>, TempDir) {
    let (mut app, answer) = app_with_sheet(stood, touched);
    let dir = TempDir::new().expect("a scratch state dir");
    app.askpass_record = Some(dir.path().to_path_buf());
    (app, answer, dir)
}

/// The record's streak, end and standing raise, if there is one.
fn record_in(dir: &TempDir) -> Option<(u32, Option<u64>, Option<u64>)> {
    let bytes = std::fs::read(dir.path().join(crate::askpass_record::NAME)).ok()?;
    Some(crate::askpass_record::parse(&bytes).expect("a well-formed record"))
}

/// The ways a sheet comes down unanswered, each driven as the App drives it.
#[derive(Clone, Copy, Debug)]
enum Lapse {
    Withdrawn,
    SessionClosed,
    WindowClosed,
    Backstop,
}

impl Lapse {
    fn run(self, app: &mut App, now: Instant) {
        match self {
            Self::Withdrawn => app.withdraw_askpass_sheet(1, now),
            Self::SessionClosed => app.withdraw_askpass_session(0, now),
            Self::WindowClosed => app.withdraw_askpass_window(WINDOW),
            Self::Backstop => assert!(app.refuse_untouched_askpass_sheet(now)),
        }
    }
}

/// C1. EVERY END OF A SHEET NOBODY TOUCHED MAKES EVERY WINDOW WAIT AND IS RECORDED: its lane's
/// withdrawal, its session closing, its window closing and the update backstop each space a
/// window never seen (7) and write the unattended part. A sheet a person touched spaces only its
/// own window and session, and records nothing.
/// NEGATIVE CONTROLS: without the `!touched` test a touched end spaces window 7; without
/// `on_unattended` no lapse spaces it; without the window-close feed closing the window escapes.
#[test]
fn every_end_of_a_sheet_nobody_touched_spaces_every_window_and_is_recorded() {
    for lapse in [
        Lapse::Withdrawn,
        Lapse::SessionClosed,
        Lapse::WindowClosed,
        Lapse::Backstop,
    ] {
        let (mut app, _answer, dir) = app_with_record(OVERDUE, false);
        let now = Instant::now();
        lapse.run(&mut app, now);
        assert!(app.askpass_sheet.is_none(), "{lapse:?}: taken down");
        assert!(
            app.askpass_backoff
                .refusal(7, 77, None, Instant::now())
                .is_some(),
            "{lapse:?}: window 7 waits"
        );
        let Some((1, Some(_), None)) = record_in(&dir) else {
            panic!(
                "{lapse:?}: recorded, with no sheet standing: {:?}",
                record_in(&dir)
            );
        };
    }
    for lapse in [Lapse::Withdrawn, Lapse::SessionClosed, Lapse::WindowClosed] {
        let (mut app, _answer, dir) = app_with_record(Duration::ZERO, true);
        let now = Instant::now();
        lapse.run(&mut app, now);
        assert_eq!(
            app.askpass_backoff.refusal(7, 77, None, now),
            None,
            "{lapse:?}: a person's sheet spaces no other window"
        );
        assert_eq!(record_in(&dir), None, "{lapse:?}: nothing recorded");
    }
    let (mut app, _answer, _dir) = app_with_record(Duration::ZERO, true);
    let now = Instant::now();
    app.withdraw_askpass_sheet(1, now);
    assert!(
        app.askpass_backoff
            .refusal(WINDOW.0, 99, None, now)
            .is_some()
    );
    let other_window = WINDOW.0 + 1;
    assert!(
        app.askpass_backoff
            .refusal(other_window, 0, None, now)
            .is_some()
    );
}

/// C2. THE UNATTENDED WAIT CROSSES PROCESSES BY THE RECORD: a second App on the same state dir
/// takes up what the first recorded; an update's successor takes it up at its activation, as
/// the outgoing process left it; a sheet left standing by a crash is one more end, written back
/// as that end; and an answer removes the record.
/// NEGATIVE CONTROLS: no launch read, no activation read, no standing write when a sheet goes up,
/// or no removal on an answer, each fails its step.
#[test]
fn the_unattended_wait_crosses_a_relaunch_a_crash_and_an_activation() {
    use crate::askpass::QUIET_BASE;
    let (mut first, _answer, dir) = app_with_record(Duration::ZERO, false);
    first.withdraw_askpass_sheet(1, Instant::now());
    let mut second = App::headless_for_test();
    second.askpass_record = Some(dir.path().to_path_buf());
    second.take_up_askpass_record_at_launch();
    let left = second
        .askpass_backoff
        .refusal(7, 77, None, Instant::now())
        .expect("taken up at launch");
    assert!(
        left > QUIET_BASE - Duration::from_secs(1) && left <= QUIET_BASE,
        "{left:?}"
    );

    // An update's successor launches; the outgoing process then records a second end.
    let mut successor = App::headless_for_test();
    successor.askpass_record = Some(dir.path().to_path_buf());
    successor.incoming_handoff_pending = true;
    successor.take_up_askpass_record_at_launch();
    let (reply, _answer) = channel();
    let raised = Instant::now();
    first.stand_askpass_sheet(AskpassSheet::headless_for_test(WINDOW, 0, raised, reply).0);
    first.withdraw_askpass_sheet(1, raised);
    assert!(matches!(record_in(&dir), Some((2, Some(_), None))));
    successor.incoming_handoff_pending = false;
    successor.take_carried_person_stamps();
    let left = successor
        .askpass_backoff
        .refusal(7, 77, None, Instant::now())
        .expect("taken up at activation");
    assert!(left > QUIET_BASE, "the second wait: {left:?}");

    // A crash under a standing sheet: the next launch counts it unanswered, once.
    let (mut crashed, _answer, dir) = app_with_record(Duration::ZERO, false);
    let sheet = crashed.askpass_sheet.take().expect("the sheet");
    crashed.stand_askpass_sheet(sheet);
    assert!(matches!(record_in(&dir), Some((0, None, Some(_)))));
    drop(crashed);
    let mut relaunched = App::headless_for_test();
    relaunched.askpass_record = Some(dir.path().to_path_buf());
    relaunched.take_up_askpass_record_at_launch();
    let left = relaunched
        .askpass_backoff
        .refusal(7, 77, None, Instant::now())
        .expect("one end, raised when it went up and ended at the launch");
    assert!(
        left > QUIET_BASE - Duration::from_secs(1) && left <= QUIET_BASE,
        "the first of a streak: {left:?}"
    );
    assert!(
        matches!(record_in(&dir), Some((1, Some(_), None))),
        "written back as that end, with no standing sheet: {:?}",
        record_in(&dir)
    );

    // An answer clears every part, and the record goes.
    let (mut answered, _answer, dir) = app_with_record(Duration::ZERO, false);
    answered.withdraw_askpass_sheet(1, Instant::now());
    assert!(record_in(&dir).is_some());
    let (reply, _answer) = channel();
    let (sheet, _input) = AskpassSheet::headless_for_test(WINDOW, 0, Instant::now(), reply);
    answered.end_askpass_sheet(sheet, super::SheetEnd::Answered, Instant::now());
    assert_eq!(record_in(&dir), None);
    assert!(
        !dir.path().join(crate::askpass_record::NAME).exists(),
        "removed, not rewritten empty"
    );
}

/// C6. A CRASH UNDER A SHEET IS COUNTED ONCE: the launch that takes up the crash's record counts
/// its standing sheet as one end and writes that end back with no standing field, so a launch
/// 99 s later by the wall clock, still inside that sheet's deadline, finds the quiet window over
/// and refuses nothing new, and rewrites nothing.
/// NEGATIVE CONTROL: without the write-back the later launch counts the same sheet again as an
/// end at its own launch, and refuses 10 s.
#[test]
fn a_crash_under_a_sheet_is_counted_once_however_many_launches_read_it() {
    let (mut crashed, _answer, dir) = app_with_record(Duration::ZERO, false);
    let sheet = crashed.askpass_sheet.take().expect("the sheet");
    crashed.stand_askpass_sheet(sheet);
    let Some((0, None, Some(raised))) = record_in(&dir) else {
        panic!("PRECONDITION: a standing sheet: {:?}", record_in(&dir));
    };
    drop(crashed);
    let launch_at = |wall_ms: u64| {
        let mut app = App::headless_for_test();
        app.askpass_record = Some(dir.path().to_path_buf());
        app.take_up_askpass_record(Instant::now(), wall_ms);
        app
    };
    let first = launch_at(raised + 1_000);
    assert!(
        first
            .askpass_backoff
            .refusal(7, 77, None, Instant::now())
            .is_some(),
        "PRECONDITION: the first launch counts the crash's sheet"
    );
    let second = launch_at(raised + 100_000);
    assert_eq!(
        second.askpass_backoff.refusal(7, 77, None, Instant::now()),
        None,
        "nothing new at the second launch"
    );
    assert_eq!(
        record_in(&dir),
        Some((1, Some(raised + 1_000), None)),
        "the end the first launch wrote back, and nothing since"
    );
}

/// Deliver `wake` as the event loop does (`App::on_user_event`): its class noted against a
/// pending handoff, then its handler.
fn deliver(app: &mut App, wake: crate::Wake) {
    app.note_update_handoff_class(crate::update_handoff_wake_class(&wake));
    match wake {
        crate::Wake::Askpass(request) => app.present_askpass_sheet(*request),
        crate::Wake::AskpassAnswered { id, proceed } => app.answer_askpass_sheet(id, proceed),
        crate::Wake::AskpassWithdraw { id } => app.withdraw_askpass_sheet(id, Instant::now()),
        other => panic!("not a password wake: {other:?}"),
    }
}

/// A plain request from session 0's shell, `pid`, answering on `reply`.
fn request_from(pid: i32, id: u64, reply: Sender<AskpassAnswer>) -> crate::Wake {
    crate::Wake::Askpass(Box::new(crate::askpass::AskpassRequest {
        ticket: std::sync::Arc::new(crate::askpass::AskpassTicket::new(
            id,
            Instant::now() + crate::askpass::ASKPASS_DEADLINE,
        )),
        attribute_from: pid,
        owned: None,
        exact: true,
        command: "/usr/bin/true".into(),
        purpose: None,
        invoker: "a test".into(),
        reply,
    }))
}

/// A headless App whose session 0 is this test's own process, so a request from it attributes.
fn app_of_this_process() -> (App, i32) {
    let mut app = App::headless_for_test();
    let pid = i32::try_from(std::process::id()).expect("a pid");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .pid = pid;
    (app, pid)
}

/// C3. A REQUEST REFUSED WITH NO SHEET MOVES NO PENDING HANDOFF: its `Wake::Askpass`, and the
/// lane's answer and withdrawal that find no sheet, leave the activity epoch and the cancel
/// channel alone. Only a sheet going up or coming down stands the handoff down, except the
/// update's own backstop.
/// NEGATIVE CONTROLS: the askpass wakes back in Revoking move the epoch on the refused request;
/// `stand_askpass_sheet` without its note leaves the epoch; a note in the withdrawal's no-sheet
/// branch moves it.
#[test]
fn a_request_refused_with_no_sheet_moves_no_pending_handoff() {
    let (mut app, pid) = app_of_this_process();
    let cancelled = crate::handoff_test_support::arm_pending_overlap(&mut app);
    let now = Instant::now();
    app.askpass_backoff.on_cancel(WINDOW.0, Some(0), now, now);
    let epoch = app.update_handoff_activity_epoch;
    let (reply, answer) = channel();
    deliver(&mut app, request_from(pid, 9, reply));
    assert!(
        matches!(answer.try_recv(), Ok(AskpassAnswer::Quiet(_))),
        "PRECONDITION: refused inside the quiet window"
    );
    deliver(&mut app, crate::Wake::AskpassWithdraw { id: 9 });
    deliver(
        &mut app,
        crate::Wake::AskpassAnswered {
            id: 9,
            proceed: false,
        },
    );
    assert_eq!(app.update_handoff_activity_epoch, epoch, "the epoch");
    assert!(
        matches!(cancelled.try_recv(), Err(TryRecvError::Empty)),
        "no cancel"
    );

    let (reply, _answer) = channel();
    let (sheet, _input) = AskpassSheet::headless_for_test(WINDOW, 0, Instant::now(), reply);
    app.stand_askpass_sheet(sheet);
    assert_eq!(app.update_handoff_activity_epoch, epoch + 1, "a sheet up");
    assert!(cancelled.try_recv().is_ok());
    deliver(&mut app, crate::Wake::AskpassWithdraw { id: 1 });
    assert_eq!(app.update_handoff_activity_epoch, epoch + 2, "a sheet down");
    assert!(cancelled.try_recv().is_ok());

    let (reply, _answer) = channel();
    let raised = Instant::now()
        .checked_sub(OVERDUE)
        .expect("the clock reaches back");
    let (sheet, _input) = AskpassSheet::headless_for_test(WINDOW, 0, raised, reply);
    app.stand_askpass_sheet(sheet);
    assert!(cancelled.try_recv().is_ok());
    assert!(app.refuse_untouched_askpass_sheet(Instant::now()));
    assert_eq!(
        app.update_handoff_activity_epoch,
        epoch + 3,
        "the backstop revokes nothing"
    );
    assert!(matches!(cancelled.try_recv(), Err(TryRecvError::Empty)));
}

/// C4. A PERSON THERE IS NOT HELD BY THE UNATTENDED WAIT: a session's person's gesture after it
/// lets that session's request through, and so does an owned CPU collection a person's own key
/// started, while one a control client started is refused. `askpass_person_at` reads the
/// session's last gesture. NEGATIVE CONTROL: with no gesture, and for a control client's
/// collection, the request is refused with the wait.
#[test]
fn a_persons_own_request_is_not_held_by_the_unattended_wait() {
    use super::SheetEnd;
    use crate::cpu_policy_authorization::{Starter, bound_operation_for_test};
    let (mut app, pid) = app_of_this_process();
    let ended = Instant::now();
    app.askpass_backoff.on_unattended(ended, ended, 0);
    assert_eq!(app.askpass_person_at(0, Instant::now()), None);
    // What the request's `sudo` heard: `None` where it was let through and a (headless) sheet
    // went up, which is then taken down with nothing counted, so the next request finds none.
    let asked = |app: &mut App, wake: crate::Wake, answer: &Receiver<AskpassAnswer>| {
        deliver(app, wake);
        match answer.try_recv() {
            Ok(heard) => Some(heard),
            Err(_) => {
                let sheet = app.askpass_sheet.take().expect("a sheet went up");
                app.end_askpass_sheet(sheet, SheetEnd::Gone, Instant::now());
                None
            }
        }
    };

    let (reply, answer) = channel();
    let refused = asked(&mut app, request_from(pid, 2, reply), &answer);
    assert!(
        matches!(refused, Some(AskpassAnswer::Quiet(_))),
        "{refused:?}"
    );

    let sid = {
        let store = app.store.read().unwrap_or_else(|e| e.into_inner());
        store
            .by_local(0)
            .expect("session 0")
            .sid
            .as_str()
            .to_owned()
    };
    for (starter, exempt) in [
        (Starter::Control, false),
        (Starter::Person(ended + Duration::from_millis(1)), true),
    ] {
        let (reply, answer) = channel();
        let ticket = std::sync::Arc::new(crate::askpass::AskpassTicket::new(
            3,
            Instant::now() + crate::askpass::ASKPASS_DEADLINE,
        ));
        let wake = crate::Wake::Askpass(Box::new(crate::askpass::AskpassRequest {
            ticket,
            attribute_from: pid,
            owned: Some(crate::root_step_authorization::OwnedOperation::CpuPolicy(
                bound_operation_for_test(app.cpu_policy_owner(), WINDOW, 0, &sid, starter),
            )),
            exact: false,
            command: "Read CPU policy flags".into(),
            purpose: None,
            invoker: "aterm system resource diagnostics".into(),
            reply,
        }));
        let got = asked(&mut app, wake, &answer);
        assert_eq!(
            matches!(got, Some(AskpassAnswer::Quiet(_))),
            !exempt,
            "{starter:?}: {got:?}"
        );
    }

    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
    let now = Instant::now();
    let at = app.askpass_person_at(0, now).expect("a person's gesture");
    assert!(at <= now && at >= ended, "{at:?}");
    let (reply, answer) = channel();
    let through = asked(&mut app, request_from(pid, 4, reply), &answer);
    assert!(
        !matches!(through, Some(AskpassAnswer::Quiet(_))),
        "a person typed after the end: {through:?}"
    );
}

/// C7. AN UPDATE'S SUCCESSOR LEAVES THE RECORD TO THE OUTGOING PROCESS UNTIL ITS ACTIVATION: at
/// its launch, while the outgoing process has a sheet up, it takes nothing up and writes nothing,
/// so that sheet is not counted unanswered nor its standing field taken from its owner.
/// NEGATIVE CONTROL: a successor that took the record up at its launch settles the outgoing
/// process's live sheet as a crash's and writes the record.
#[test]
fn an_updates_successor_takes_nothing_up_and_writes_nothing_at_its_launch() {
    let (mut outgoing, _answer, dir) = app_with_record(Duration::ZERO, false);
    let sheet = outgoing.askpass_sheet.take().expect("the sheet");
    outgoing.stand_askpass_sheet(sheet);
    let standing = record_in(&dir);
    assert!(
        matches!(standing, Some((0, None, Some(_)))),
        "PRECONDITION: {standing:?}"
    );
    let mut successor = App::headless_for_test();
    successor.askpass_record = Some(dir.path().to_path_buf());
    successor.incoming_handoff_pending = true;
    successor.take_up_askpass_record_at_launch();
    assert_eq!(
        successor
            .askpass_backoff
            .refusal(7, 77, None, Instant::now()),
        None,
        "nothing taken up"
    );
    assert_eq!(record_in(&dir), standing, "nothing written");
}

/// C8. A PERSON'S ANSWER OR CANCEL IN AN UPDATE'S OUTGOING PROCESS SPACES NO OTHER WINDOW AFTER
/// IT: the successor launched while the sheet stood, the person answered or cancelled it there,
/// and at the activation the successor refuses nothing to a window and session it never saw.
/// NEGATIVE CONTROL: the launch read in a successor together with a take-up that keeps the
/// longer wait held (the code as first committed) leaves that sheet counted unanswered.
#[test]
fn a_sheet_answered_in_the_outgoing_process_spaces_nothing_after_the_update() {
    for end in [super::SheetEnd::Answered, super::SheetEnd::Declined] {
        let (mut outgoing, _answer, dir) = app_with_record(Duration::ZERO, false);
        let sheet = outgoing.askpass_sheet.take().expect("the sheet");
        outgoing.stand_askpass_sheet(sheet);
        let mut successor = App::headless_for_test();
        successor.askpass_record = Some(dir.path().to_path_buf());
        successor.incoming_handoff_pending = true;
        successor.take_up_askpass_record_at_launch();
        let sheet = outgoing.askpass_sheet.take().expect("still up");
        outgoing.end_askpass_sheet(sheet, end, Instant::now());
        successor.incoming_handoff_pending = false;
        successor.take_carried_person_stamps();
        assert_eq!(
            successor
                .askpass_backoff
                .refusal(7, 77, None, Instant::now()),
            None,
            "{end:?}"
        );
    }
}

/// C9. A QUIT TAKES A STANDING SHEET DOWN AND RECORDS HOW IT ENDED: one nobody touched as an
/// end that spaces every window, one a person touched as nothing; neither leaves a standing
/// field for the next launch to count as a crash's.
/// NEGATIVE CONTROL: a quit that leaves the sheet up leaves its standing field.
#[test]
fn a_quit_takes_a_standing_sheet_down_and_records_how_it_ended() {
    for touched in [false, true] {
        let (mut app, _answer, dir) = app_with_record(Duration::ZERO, touched);
        let sheet = app.askpass_sheet.take().expect("the sheet");
        app.stand_askpass_sheet(sheet);
        app.end_askpass_sheet_at_quit();
        assert!(app.askpass_sheet.is_none(), "touched {touched}: taken down");
        let recorded = record_in(&dir);
        if touched {
            assert_eq!(recorded, None, "a person's sheet: nothing recorded");
        } else {
            assert!(
                matches!(recorded, Some((1, Some(_), None))),
                "nobody's: its end, and no standing sheet: {recorded:?}"
            );
        }
    }
}
