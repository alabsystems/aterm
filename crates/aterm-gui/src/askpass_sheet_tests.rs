// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use super::AskpassSheet;
use crate::app_update_handoff::SameImageHandoff;
use crate::askpass::{ASKPASS_UNTOUCHED_BACKSTOP, AskpassAnswer};
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
/// that asked settles before the next attempt parks, and that `sudo` is told why, its window's
/// decline spacing starting as a Cancel's does. Before, the sheet held every attempt until its own
/// deadline.
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
        let Ok(AskpassAnswer::Refused(why)) = answer.try_recv() else {
            panic!("{applier:?}: the sudo that asked hears that it was refused");
        };
        assert!(
            why.contains(&format!(
                "nobody answered it within {}s",
                ASKPASS_UNTOUCHED_BACKSTOP.as_secs()
            )),
            "{applier:?}: {why}"
        );
        assert!(
            app.askpass_backoff
                .quiet_for(WINDOW.0, Instant::now())
                .is_some(),
            "{applier:?}: its window waits before it asks again"
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
            "{case} ({applier:?}): no decline spacing"
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
