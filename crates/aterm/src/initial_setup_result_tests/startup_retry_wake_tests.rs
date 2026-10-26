// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn startup()
-> LiveSetupStartup<impl FnMut(Plan, StartupLook, StartupHandoff) -> aterm_gui::SetupAnswer> {
    startup_answering(|_, _, _| aterm_gui::WorkstationSetupDecision::Unavailable.into())
}

fn startup_answering(
    offer: impl FnMut(Plan, StartupLook, StartupHandoff) -> aterm_gui::SetupAnswer,
) -> LiveSetupStartup<impl FnMut(Plan, StartupLook, StartupHandoff) -> aterm_gui::SetupAnswer> {
    LiveSetupStartup {
        offer,
        review: Review::Offered,
        receipt: None,
        authorization: None,
        last_failure: None,
        observation: None,
        offered: None,
        chosen: false,
        vault_typed: false,
        sent: StartupSent::default(),
        retry_seen: SETUP_WAKES.count(),
        recovered: false,
        reviewed: None,
    }
}

#[test]
fn startup_retry_wake_check_again_releases_recovery_without_another_focus_gain() {
    let _globals = globals();
    let _io = TestIo::install();
    watch_setup_signals();
    let mut host = startup();
    let focus = aterm_gui::window_focus_gains();
    // A reserved setup slot refuses the backend Check, while the real Check again
    // entry still accepts the forced, inert reading and wakes setup's recovery.
    let busy = take_initial_setup_slot().unwrap();
    let checked = check_initial_setup(Plan::default());
    assert!(checked.reading && !checked.check);
    wait_until("the forced reading finishes", observe_idle);
    assert_eq!(aterm_gui::window_focus_gains(), focus);
    assert!(
        host.backoff_for(Duration::ZERO),
        "an already delivered recovery wake must not wait for the minute timer"
    );
    assert!(
        !host.backoff_for(Duration::ZERO),
        "the same Check does not repeatedly wake failed startup"
    );
    drop(busy);
}

#[test]
fn startup_retry_wake_focus_still_releases_recovery_once() {
    let _globals = globals();
    let _io = TestIo::install();
    watch_setup_signals();
    aterm_gui::gain_window_focus_for_test();
    let mut host = startup();
    assert_eq!(
        host.offer(Plan::default()),
        aterm_gui::WorkstationSetupDecision::Unavailable
    );
    assert!(
        !host.backoff_for(Duration::ZERO),
        "a focus gain before the offer does not release the later backoff"
    );
    aterm_gui::gain_window_focus_for_test();
    assert!(host.backoff_for(Duration::ZERO));
    assert!(!host.backoff_for(Duration::ZERO));
}

#[test]
fn startup_retry_wake_a_covered_stop_never_enters_an_admission_retry_wait() {
    let _globals = globals();
    let _io = TestIo::install();
    let sent = send_admission();
    let guard = UnderWay::new(sent);
    let mut host = startup();
    host.sent.sent = sent;
    let mut seen = SETUP_WAKES.count();
    let _ = stop_initial_setup();
    assert!(
        host.stopped(),
        "the live startup recognizes its covered admission"
    );
    let mut waited = false;
    wait_before_admission_retry(sent, &mut seen, SETUP_RETRY_WAIT, |_, _| waited = true);
    assert!(
        !waited,
        "Stop during persistence must release its pending answer before any retry wait"
    );
    drop(guard);
    // A fresh choice, sent after Stop, still waits after a failure rather than spinning.
    let fresh = send_admission();
    wait_before_admission_retry(fresh, &mut seen, SETUP_RETRY_WAIT, |_, _| waited = true);
    assert!(waited);
}

#[test]
fn startup_retry_wake_a_person_waiting_for_the_claim_still_takes_it() {
    let _globals = globals();
    let _io = TestIo::install();
    let mut host = startup();
    host.sent.send();
    let claim = StartupClaim::claim().unwrap().unwrap();
    let awaiting = AwaitingClaim::new();
    assert!(host.wait_holding_claim_for(Duration::ZERO));
    drop(claim);
    assert!(!startup_holds_claim());
    drop(awaiting);
    assert!(!host.wait_holding_claim_for(Duration::ZERO));
}

#[test]
fn startup_retry_wake_a_refused_local_choice_releases_startup_recovery() {
    let _globals = globals();
    let _io = TestIo::install();
    let mut host = startup();
    change(|script| script.persist_hold = true);
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(Plan::default(), false, None, receipt));
    wait_until("the local choice holds its claim while persisting", || {
        script().persists == 1
    });
    // Startup can already have answered the earlier wake while this choice
    // still held the claim. Its terminal refusal needs a later recovery signal.
    host.retry_seen = SETUP_WAKES.count();
    change(|script| script.persist_hold = false);
    assert!(!answer.recv_timeout(HANG).unwrap());
    wait_until("the refused choice returns its setup slot", || {
        !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
    });
    assert_eq!(script().persists, 1);
    assert!(script().runs.is_empty());
    assert!(
        host.backoff_for(Duration::ZERO),
        "recovery must hear the refusal after the choice releases its claim"
    );
    assert!(!host.backoff_for(Duration::ZERO));
}

/// The review comes to the front as it is offered, which is a setup wake: when no window could
/// keep it (Unavailable), the wait before the next offer still runs its course, so an offer is
/// never shown again at once on the strength of its own showing.
#[test]
fn startup_retry_wake_an_offers_own_coming_to_the_front_does_not_end_its_retry_wait() {
    let _globals = globals();
    let _io = TestIo::install();
    watch_setup_signals();
    aterm_types::macos_setup::set_system_state_front(false);
    let mut host = startup_answering(|_, _, _| {
        aterm_types::macos_setup::set_system_state_front(true);
        aterm_types::macos_setup::set_system_state_front(false);
        aterm_gui::WorkstationSetupDecision::Unavailable.into()
    });
    assert_eq!(
        host.offer(Plan::default()),
        aterm_gui::WorkstationSetupDecision::Unavailable
    );
    assert!(
        !host.backoff_for(Duration::ZERO),
        "the review's own coming to the front ended the wait before the next offer"
    );
}
