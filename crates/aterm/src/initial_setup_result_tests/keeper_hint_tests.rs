// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each place that changes what a round of the window's keyboard keeper would do hints that
//! keeper. Every hint law runs the real call site beside a keeper registered as the window
//! registers its own, and reads the hint off a wait on that keeper's control. The rest is what
//! the window's own work tells that keeper's wait.

use super::*;

/// NEGATIVE CONTROL: `wake_setup_on_focus` without its `focus_gained` call hears no focus.
#[test]
fn a_focus_gain_hints_the_window_keeper() {
    let _globals = globals();
    let heard = input_attachments::heard_by_a_window_keeper(wake_setup_on_focus);
    assert!(heard.focus, "a window gaining focus");
}

/// NEGATIVE CONTROL: `start_withheld_follow_up` without its `work_requested` call hears no
/// request.
#[test]
fn a_follow_up_request_hints_the_window_keeper() {
    let _globals = globals();
    let heard = input_attachments::heard_by_a_window_keeper(start_withheld_follow_up);
    assert!(heard.request, "a deferred-preference follow-up asked for");
}

/// NEGATIVE CONTROL: `retire_observed_rows_with_history` without its `work_requested` call
/// hears no request.
#[test]
fn a_retired_caps_live_reading_hints_the_window_keeper() {
    use aterm_types::macos_setup::LiveKeyboardMapping;
    let _globals = globals();
    let caps = [Subject::Setting(Setting::CapsLock)];
    let heard = input_attachments::heard_by_a_window_keeper(|| {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.caps_live = Some((
            7,
            LiveKeyboardMapping {
                control: 1,
                explicit_other: 0,
                held: 0,
            },
        ));
        assert!(retire_observed_rows_with_history(
            &mut rows,
            &caps,
            RetiredHistory::Preserve
        ));
        assert_eq!(rows.caps_live, None);
    });
    assert!(heard.request, "a Caps Lock live reading retired");
}

/// NEGATIVE CONTROL: the failed-start branch of `dispatch_observation_publishing` without its
/// `work_requested` call hears no request.
#[test]
fn a_setup_reader_that_could_not_start_hints_the_window_keeper() {
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    assert!(observe_idle());
    let heard = input_attachments::heard_by_a_window_keeper(|| {
        assert!(!dispatch_observation_publishing(
            Some(dock_request()),
            || false,
            || {},
            || 0,
            |_| Err(std::io::Error::other("injected worker spawn refusal")),
        ));
    });
    assert!(heard.request, "a reader whose thread could not start");
}

fn dock_request() -> ObserveRequest {
    ObserveRequest {
        plan: Some(left_as_is()),
        choices: OBSERVED_ROWS.lock().unwrap().choices,
        subjects: vec![Subject::Setting(Setting::Dock)],
        forced: false,
        pending_repositories: false,
    }
}

/// What the keeper's wait is told the setup reader owes: nothing over an idle queue or a
/// running reader, which read no clock, and what is left of its pause for a batch whose reader
/// never started.
/// NEGATIVE CONTROL: with a kept batch owed at once whatever its pause, the batch refused at
/// 1000 ms reads `Some(0ns)` where its first pause is owed.
#[test]
fn a_held_setup_reader_says_how_much_of_its_pause_is_left() {
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    assert!(observe_idle());
    let no_clock = || -> u64 { panic!("an idle or running queue reads no clock") };
    assert_eq!(observation_dispatch_owed_at(no_clock), None, "idle");
    OBSERVE_QUEUE.lock().unwrap().running = true;
    assert_eq!(observation_dispatch_owed_at(no_clock), None, "running");
    OBSERVE_QUEUE.lock().unwrap().running = false;

    assert!(!dispatch_observation_publishing(
        Some(dock_request()),
        || false,
        || {},
        || 1_000,
        |_| Err(std::io::Error::other("injected worker spawn refusal")),
    ));
    let first = first_observe_retry_ms();
    let millis = |left| Some(Duration::from_millis(left));
    assert_eq!(observation_dispatch_owed_at(|| 1_000), millis(first));
    assert_eq!(observation_dispatch_owed_at(|| 999 + first), millis(1));
    assert_eq!(observation_dispatch_owed_at(|| 1_000 + first), millis(0));
    assert_eq!(observation_dispatch_owed_at(|| 9_000 + first), millis(0));
}

/// The window keeper's own round, as its worker runs it: it keeps its two-second timer until
/// the window's loop took the readings it publishes; settled, it waits for the backstop; while
/// a Caps Lock live reading it could not publish is not the one the readings hold, it keeps
/// the timer, and it settles once it published; and while a setup reader that never started is
/// held, it waits no longer than that reader's pause.
/// NEGATIVE CONTROLS: with the round saying the loop always took the readings, the round
/// before the loop's proxy waits 300 s; with the round saying its Caps Lock reading always
/// stands, the deferred publish's round says so; with the round leaving the reader out, the
/// held reader's round names no pause.
#[test]
fn the_window_keepers_round_waits_on_its_caps_reading_and_its_held_reader() {
    use aterm_types::macos_setup::LiveKeyboardMapping;
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    assert!(observe_idle());
    let backstop = Duration::from_secs(300);
    let pace = Duration::from_secs(2);
    let stale = Some((
        7,
        LiveKeyboardMapping {
            control: 1,
            explicit_other: 0,
            held: 0,
        },
    ));

    // The loop a round's publication meets: this thread stands in for it.
    let window = aterm_gui::SetupLoopForTest::before_its_proxy();
    let unread = input_attachments::window_round_off_the_console(|| {});
    assert_eq!(unread.wait, pace, "no loop has taken the readings");
    window.register();

    let settled = input_attachments::window_round_off_the_console(|| {});
    assert!(settled.caps_stand);
    assert_eq!((settled.dispatch, settled.wait), (None, backstop));

    // A reading the round cannot replace: an input readback runs, so its publish is deferred.
    let mut readback = None;
    let deferred = input_attachments::window_round_off_the_console(|| {
        readback = Some(InputReadbackActivity::begin());
        OBSERVED_ROWS.lock().unwrap().caps_live = stale;
    });
    assert!(!deferred.caps_stand);
    assert_eq!(
        deferred.wait, pace,
        "the timer stays until the reading stands"
    );
    assert_eq!(OBSERVED_ROWS.lock().unwrap().caps_live, stale);
    drop(readback);

    let published = input_attachments::window_round_off_the_console(|| {
        OBSERVED_ROWS.lock().unwrap().caps_live = stale;
    });
    assert!(published.caps_stand);
    assert_eq!(published.wait, backstop);
    assert_eq!(OBSERVED_ROWS.lock().unwrap().caps_live, None);

    // A batch whose reader did not start six times running, the last of them now.
    let pause = OBSERVE_AGAIN.after(6);
    assert!(pace < pause && pause < backstop, "{pause:?}");
    let held = input_attachments::window_round_off_the_console(|| {
        let mut queue = OBSERVE_QUEUE.lock().unwrap();
        queue.next = Some(dock_request());
        queue.misses = 6;
        queue.missed_at = observation_millis();
    });
    assert!(held.caps_stand);
    assert!(
        held.dispatch.is_some_and(|left| left <= pause),
        "{:?}",
        held.dispatch
    );
    assert!(pace <= held.wait && held.wait <= pause, "{:?}", held.wait);
}
