// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::Report;
use std::net::Ipv4Addr;

const HOME: Routes = [Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))), None, None];
const AWAY: Routes = [Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))), None, None];
const MINUTE: u64 = 60 * 1000;

fn applied() -> SyncOutcome {
    SyncOutcome::Applied {
        seq: 3,
        report: Report::default(),
    }
}

fn unchanged() -> SyncOutcome {
    SyncOutcome::Unchanged { seq: 3 }
}

/// One start as the subscriber starts, one more for each version it applies, none for a pass
/// that changed nothing on the same routes inside the recheck, one when the routes move and
/// one once the recheck passes (or the clock goes back).
/// NEGATIVE CONTROL: an `after_pass` that reads an applied version as a plain round starts
/// nothing for it.
#[test]
fn the_subscriber_starts_a_runner_at_start_per_applied_version_and_when_due() {
    let mut due = Due::default();
    let starts = std::cell::Cell::new(0);
    let at = |started: bool| starts.set(starts.get() + usize::from(started));
    at(due.at_start(0, HOME, Last::None));
    assert_eq!(starts.get(), 1, "one at start");
    at(due.after_pass(&applied(), MINUTE, HOME, Last::Ended));
    assert_eq!(starts.get(), 2, "+1 for an applied version");
    at(due.after_pass(&applied(), 2 * MINUTE, HOME, Last::Ended));
    assert_eq!(starts.get(), 3, "+1 for each");
    for minute in 3..30 {
        at(due.after_pass(&unchanged(), minute * MINUTE, HOME, Last::Ended));
    }
    assert_eq!(starts.get(), 3, "0 for passes that changed nothing");
    at(due.after_pass(&unchanged(), 30 * MINUTE, AWAY, Last::Ended));
    assert_eq!(starts.get(), 4, "+1 when the routes move");
    at(due.after_pass(&unchanged(), 31 * MINUTE, AWAY, Last::Ended));
    assert_eq!(starts.get(), 4, "not again on the routes it started on");
    at(due.after_pass(&unchanged(), 60 * MINUTE, AWAY, Last::Ended));
    assert_eq!(starts.get(), 5, "+1 once the recheck passed");
    at(due.after_pass(&unchanged(), MINUTE, AWAY, Last::Ended));
    assert_eq!(starts.get(), 6, "+1 for a clock that went back");
}

/// A start that comes due while the last runner runs waits for it to end rather than start a
/// second beside it, and the owed start follows that end once.
#[test]
fn a_start_due_while_a_runner_runs_is_owed_to_the_next_round() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    assert!(
        !due.after_pass(&applied(), MINUTE, HOME, Last::Running),
        "never two at once"
    );
    assert!(
        !due.after_pass(&unchanged(), 2 * MINUTE, HOME, Last::Running),
        "still running"
    );
    assert!(
        due.after_pass(&unchanged(), 3 * MINUTE, HOME, Last::Ended),
        "the owed start, once it ended"
    );
    assert!(!due.after_pass(&unchanged(), 4 * MINUTE, HOME, Last::Ended));
}

/// A RUNNER THAT WAITS FOR THE LOCK ANOTHER PASS HOLDS IS OWED ITS START AT ITS END, NOT AT A
/// CLOCK: on m38 (2026-11-15) one pass held the lock for 40 minutes. A runner started for the
/// subscriber waits for it, so for those 40 minutes, looked at every second while an input
/// look asks for a repair each minute and the routes move once, it reads as running: nothing
/// starts, the 5 s retry and the 30-minute recheck among them, and no time is owed but the
/// recheck's. Its end starts exactly one runner, carrying the strongest work owed meanwhile (a
/// refresh, for the moved routes); the next looks start nothing.
/// NEGATIVE CONTROL: a starter that reads its waiting runner as gone (`Last::None` at each look,
/// as it read a runner that ended held at once) starts one for the first minute's repair, at
/// 60 s, where none is wanted.
#[test]
fn a_runner_waiting_for_the_lock_is_owed_its_start_at_its_end_not_at_a_clock() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    let mut starts = 0;
    for second in 1..=40 * 60 {
        let at = second * 1000;
        if at % MINUTE == 0 {
            due.because(Causes::INPUT_REPAIR);
            due.request(RepositoryRequest::Repair);
        }
        let routes = if second < 10 * 60 { HOME } else { AWAY };
        starts += usize::from(due.poll(at, routes, Last::Running));
        assert_eq!(
            due.next_at(),
            Some(RECHECK_MS),
            "no clock but the recheck's, at {second} s"
        );
    }
    assert_eq!(starts, 0, "a waiting runner is still this starter's one");
    assert!(
        due.poll(40 * MINUTE + 1, AWAY, Last::Ended),
        "its end starts the start owed meanwhile"
    );
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    assert_eq!(
        caused(&due),
        causes([Causes::INPUT_REPAIR, Causes::ROUTES, Causes::RECHECK])
    );
    for at in [
        40 * MINUTE + 2,
        40 * MINUTE + FIRST_RETRY_MS + 2,
        41 * MINUTE,
    ] {
        assert!(!due.poll(at, AWAY, Last::Ended), "once, at {at}");
    }
}

/// The first healthy pass after failed ones starts a runner, on the routes and inside the
/// recheck of the last start, and the one after it does not.
/// NEGATIVE CONTROL: an `after_pass` that forgets the failed passes starts nothing.
#[test]
fn a_healthy_pass_after_failed_ones_is_due_once() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    due.failed();
    due.failed();
    assert!(
        due.after_pass(&unchanged(), MINUTE, HOME, Last::Ended),
        "back after errors"
    );
    assert!(!due.after_pass(&unchanged(), 2 * MINUTE, HOME, Last::Ended));
    due.failed();
    assert!(
        !due.after_pass(&unchanged(), 3 * MINUTE, HOME, Last::Running),
        "never two at once"
    );
    assert!(
        due.after_pass(&unchanged(), 4 * MINUTE, HOME, Last::Ended),
        "owed once the runner ended"
    );
}

#[test]
fn an_unfinished_runner_retries_unchanged_inputs_and_success_ends_the_retry() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    // The subscriber reaps before its source round, even if that round is unhealthy.
    due.reaped(Last::Unfinished, 1_000);
    for at in [1_000, 1_001, 5_999] {
        assert!(!due.after_pass(&unchanged(), at, HOME, Last::None));
    }
    assert!(
        due.after_pass(&unchanged(), 6_000, HOME, Last::None),
        "unfinished work gets another pass without a new route, token or inventory"
    );
    assert!(
        !due.after_pass(&unchanged(), 6_001, HOME, Last::Running),
        "the retry remains the only runner"
    );
    due.reaped(Last::Ended, 6_002);
    for at in [6_003, 11_000, RECHECK_MS] {
        assert!(
            !due.after_pass(&unchanged(), at, HOME, Last::None),
            "success leaves only the ordinary recheck due"
        );
    }
    assert!(due.after_pass(&unchanged(), 6_000 + RECHECK_MS, HOME, Last::None));
    // Success reset the failure streak: another failure begins at five seconds again.
    let failed_at = 6_001 + RECHECK_MS;
    due.reaped(Last::Failed, failed_at);
    assert!(!due.after_pass(&unchanged(), failed_at + 4_999, HOME, Last::None));
    assert!(due.after_pass(&unchanged(), failed_at + 5_000, HOME, Last::None));
}

#[test]
fn consecutive_runner_failures_back_off_without_a_hot_loop_and_stay_bounded() {
    let mut due = Due::default();
    let mut at = 0;
    assert!(due.at_start(at, HOME, Last::None));
    for seconds in [5, 10, 20, 40, 80, 160, 300, 300] {
        at += 1;
        due.reaped(Last::Failed, at);
        let ready = at + seconds * 1000;
        assert!(!due.after_pass(&unchanged(), at, HOME, Last::None));
        assert!(!due.after_pass(&unchanged(), ready - 1, HOME, Last::None));
        assert!(due.after_pass(&unchanged(), ready, HOME, Last::None));
        at = ready;
    }
}

/// `next_at` is the moment time alone starts a runner: a poll a millisecond before it starts
/// none, and a poll at it starts one, for the recheck after a start and for a failed runner's
/// retry, which comes first.
/// NEGATIVE CONTROL: a `next_at` that leaves out the retry names the recheck, and the poll a
/// millisecond before that starts the retry's runner.
#[test]
fn next_at_is_when_time_alone_starts_a_runner() {
    let mut due = Due::default();
    assert_eq!(
        due.next_at(),
        None,
        "nothing is owed before the first start"
    );
    assert!(due.at_start(0, HOME, Last::None));
    let recheck = due.next_at().expect("a start owes its recheck");
    assert_eq!(recheck, RECHECK_MS);
    assert!(!due.poll(recheck - 1, HOME, Last::Ended));
    assert!(
        due.poll(recheck, HOME, Last::None),
        "the recheck starts one"
    );
    let failed = recheck + 1;
    due.reaped(Last::Failed, failed);
    let retry = due.next_at().expect("a failure owes its retry");
    assert_eq!(
        retry,
        failed + FIRST_RETRY_MS,
        "the retry comes before the recheck"
    );
    assert!(!due.poll(retry - 1, HOME, Last::None));
    assert!(due.poll(retry, HOME, Last::None), "the retry starts one");
}

/// Identity's own retry is a time alone that starts a runner too: `next_at` names it where it
/// comes first, and `owes_start` reads it due from that moment, as `poll` does.
/// NEGATIVE CONTROL: a `next_at` that leaves it out names the recheck, a half hour late.
#[test]
fn next_at_names_identitys_own_retry() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    assert!(!due.poll(1, HOME, Last::Ended));
    due.identity_retry.reaped(Last::Failed, 1);
    let retry = 1 + FIRST_RETRY_MS;
    assert_eq!(due.next_at(), Some(retry));
    assert!(!due.owes_start(retry - 1));
    assert!(due.owes_start(retry));
    assert!(!due.poll(retry - 1, HOME, Last::None));
    assert!(
        due.poll(retry, HOME, Last::None),
        "identity's retry starts one"
    );
}

/// A HELD END IS RETRIED AS A FAILED ONE, NEVER ON A FLAT CLOCK: no runner's exit reads as held
/// ([`Last::of`]), and a held end handed to [`Due::reaped`] all the same backs off as a failure
/// does, its work kept and its cause `retry-failed`: the second in a row waits twice as long as
/// the first.
/// NEGATIVE CONTROL: a `Due` that hands the held end to [`RunnerRetry`] as it is retries the
/// second after [`FIRST_RETRY_MS`] again, where twice that is wanted.
#[test]
fn a_held_end_is_retried_as_a_failed_one_never_on_a_flat_clock() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    due.reaped(Last::Held, 1);
    assert!(!due.poll(FIRST_RETRY_MS, HOME, Last::None));
    assert!(due.poll(1 + FIRST_RETRY_MS, HOME, Last::None));
    assert_eq!(caused(&due), Causes::RETRY_FAILED);
    assert_eq!(
        due.repository_work(),
        RepositoryWork::Refresh,
        "its work is kept"
    );
    due.reaped(Last::Held, 2 + FIRST_RETRY_MS);
    let again = 2 + 3 * FIRST_RETRY_MS;
    assert!(!due.poll(again - 1, HOME, Last::None));
    assert!(due.poll(again, HOME, Last::None));
}

/// A retry that started a runner which then waits for the lock keeps the failure streak through
/// the wait: nothing starts on the retry's clock while it runs, and a second failure after it
/// backs off twice as long as the first.
/// NEGATIVE CONTROL: a reap that resets the backoff on a long-running runner retries the second
/// failure after [`FIRST_RETRY_MS`] where twice that is wanted.
#[test]
fn waiting_and_running_runners_preserve_failed_retry_and_coalesced_work() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    due.reaped(Last::Failed, 1);
    assert!(due.after_pass(&unchanged(), 5_001, HOME, Last::None));
    for at in [5_002, 10_001, 10_002, 15_003, 600_000] {
        assert!(
            !due.after_pass(&unchanged(), at, HOME, Last::Running),
            "a runner waiting for the lock starts nothing at {at}"
        );
    }
    due.reaped(Last::Failed, 600_001);
    assert!(!due.after_pass(&unchanged(), 610_000, HOME, Last::None));
    assert!(
        due.after_pass(&unchanged(), 610_001, HOME, Last::None),
        "the wait did not erase the failure streak"
    );
    assert!(!due.after_pass(&applied(), 610_002, HOME, Last::Running));
    assert!(!due.after_pass(&unchanged(), 610_003, HOME, Last::Running));
    assert!(due.after_pass(&unchanged(), 610_004, HOME, Last::Ended));
    assert!(!due.after_pass(&unchanged(), 610_005, HOME, Last::Ended));
}

#[test]
fn local_repairs_and_unrelated_versions_do_not_postpone_the_remote_recheck() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    assert!(due.after_pass(&applied(), 1, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    due.failed();
    assert!(due.after_pass(&unchanged(), 2, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    assert!(due.after_local_progress(RepositoryWork::Repair, RECHECK_MS - 1, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    assert!(due.poll(RECHECK_MS, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
}

#[test]
fn pending_and_failed_work_keeps_the_strongest_refresh_request() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    assert!(due.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    assert!(!due.after_local_progress(RepositoryWork::Refresh, 2, HOME, Last::Running));
    due.identity_changed();
    assert!(!due.after_local_progress(RepositoryWork::Repair, 3, HOME, Last::Running));
    assert!(due.poll(4, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    assert!(!due.poll(5, HOME, Last::Ended));
    due.reaped(Last::Failed, 6);
    assert!(due.after_local_progress(RepositoryWork::Repair, 7, HOME, Last::None));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    assert!(!due.poll(5_005, HOME, Last::Ended));
    assert!(due.poll(5_006, HOME, Last::None));
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
}

#[test]
fn checkout_repairs_preserve_a_failed_refresh_deadline_and_backoff() {
    for refresh_end in [Last::Failed, Last::Unfinished] {
        for repair_end in [Last::Ended, Last::Failed, Last::Unfinished] {
            let mut due = Due::default();
            assert!(due.at_start(0, HOME, Last::None));
            due.reaped(refresh_end, 1_000);
            assert!(due.after_local_progress(RepositoryWork::Repair, 1_001, HOME, Last::None));
            assert_eq!(due.repository_work(), RepositoryWork::Repair);
            due.reaped(repair_end, 1_002);
            assert!(!due.poll(5_999, HOME, Last::None));
            assert!(due.poll(6_000, HOME, Last::None));
            assert_eq!(due.repository_work(), RepositoryWork::Refresh);
            due.reaped(Last::Failed, 6_001);
            assert!(!due.poll(16_000, HOME, Last::None));
            assert!(due.poll(16_001, HOME, Last::None));
            assert_eq!(due.repository_work(), RepositoryWork::Refresh);
        }
    }
}

#[test]
fn a_new_source_refresh_starts_before_retained_refresh_retry_is_due() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    due.reaped(Last::Failed, 1_000);
    assert!(due.after_local_progress(RepositoryWork::Repair, 1_001, HOME, Last::None));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    assert!(!due.after_local_progress(RepositoryWork::Refresh, 1_002, HOME, Last::Running));
    assert!(!due.after_local_progress(RepositoryWork::Repair, 1_003, HOME, Last::Running));
    assert!(due.poll(1_004, HOME, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
    assert!(!due.poll(6_000, HOME, Last::Ended));
}

#[test]
fn a_failed_repair_retries_locally_until_routes_request_a_refresh() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    assert!(due.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::Ended));
    due.reaped(Last::Unfinished, 2);
    assert!(!due.poll(1 + FIRST_RETRY_MS, HOME, Last::None));
    assert!(due.poll(2 + FIRST_RETRY_MS, HOME, Last::None));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    assert!(!due.poll(3 + FIRST_RETRY_MS, AWAY, Last::Running));
    assert!(due.poll(4 + FIRST_RETRY_MS, AWAY, Last::Ended));
    assert_eq!(due.repository_work(), RepositoryWork::Refresh);
}

#[test]
fn producers_share_the_remote_wave_and_keep_their_generic_passes() {
    let scope = RefreshScope::new([1; 32]);
    let before = RefreshObservation {
        version: RefreshVersion {
            input: 1,
            attempt: 1,
            fulfilled: 1,
        },
        scope: Some(RefreshScope::new([0; 32])),
        recheck_ms: Some(RECHECK_MS),
        ..RefreshObservation::default()
    };
    let request = RepositoryRequest::Refresh(RefreshRequest::inputs(scope, before.version));
    let mut node = Due::default();
    let mut subscriber = Due::default();
    for due in [&mut node, &mut subscriber] {
        due.observe(before);
        assert!(
            due.at_start(0, HOME, Last::None),
            "startup without a current scope cannot inherit the old source's receipt"
        );
        assert!(
            due.after_request(
                RepositoryRequest::Refresh(RefreshRequest::ensure(before.scope, before.version)),
                1,
                HOME,
                Last::Ended,
            ),
            "a known matching ensure still owes its own generic pass"
        );
        assert_eq!(due.repository_work(), RepositoryWork::Repair);
        assert!(due.after_request(request, 2, HOME, Last::Ended));
        assert_eq!(due.repository_request(), request);
    }
    // The follower's runner waits for the lock while the node's runner refreshes.
    assert!(!subscriber.poll(3, HOME, Last::Running));
    let finished = RefreshObservation {
        version: RefreshVersion {
            input: 2,
            attempt: 2,
            fulfilled: 2,
        },
        scope: Some(scope),
        recheck_ms: Some(RECHECK_MS - 100),
        ..before
    };
    subscriber.observe(finished);
    assert!(
        !subscriber.poll(100, HOME, Last::Ended),
        "the follower's own pass, run once the lock was let go, shared the remote wave"
    );
    for at in [101, FIRST_RETRY_MS + 3, RECHECK_MS - 1] {
        assert!(
            !subscriber.poll(at, HOME, Last::None),
            "nothing more is owed at {at}"
        );
    }
    subscriber.observe(RefreshObservation {
        recheck_ms: Some(0),
        ..finished
    });
    assert!(
        subscriber.poll(RECHECK_MS, HOME, Last::None),
        "a delayed acknowledgment preserves the shared deadline"
    );

    let mut startup = Due::default();
    startup.observe(finished);
    startup.request(RepositoryRequest::Refresh(RefreshRequest::ensure(
        finished.scope,
        finished.version,
    )));
    assert!(startup.at_start(0, HOME, Last::None));
    assert_eq!(startup.repository_work(), RepositoryWork::Repair);
    assert!(!startup.poll(1, HOME, Last::Ended));
    assert!(!startup.poll(2, HOME, Last::None));

    startup.identity_changed();
    assert!(startup.after_pass(&unchanged(), 3, HOME, Last::None));
    assert_eq!(startup.repository_request(), RepositoryRequest::Repair);
    startup.identity_changed();
    startup.observe(RefreshObservation {
        version: RefreshVersion {
            input: 3,
            attempt: 3,
            fulfilled: 3,
        },
        ..finished
    });
    assert!(!startup.after_pass(&unchanged(), 4, HOME, Last::Running));
    assert!(
        startup.after_pass(&unchanged(), 5, HOME, Last::Ended),
        "a newer repository receipt and the old child's success cannot consume identity debt"
    );
    assert_eq!(startup.repository_request(), RepositoryRequest::Repair);
    assert!(!startup.after_pass(&unchanged(), 6, HOME, Last::Ended));
    startup.identity_changed();
    assert!(startup.after_pass(&unchanged(), 7, HOME, Last::None));
    assert_eq!(startup.repository_request(), RepositoryRequest::Repair);
    assert!(!startup.after_pass(&unchanged(), 8, HOME, Last::Failed));
    assert!(!startup.poll(8 + FIRST_RETRY_MS - 1, HOME, Last::None));
    assert!(startup.poll(8 + FIRST_RETRY_MS, HOME, Last::None));
    assert_eq!(startup.repository_request(), RepositoryRequest::Repair);
    assert!(!startup.poll(9 + FIRST_RETRY_MS, HOME, Last::Ended));

    let mut unknown = Due::default();
    unknown.observe(RefreshObservation {
        recheck_ms: Some(0),
        ..finished
    });
    assert!(unknown.at_start(10, HOME, Last::None));
    for (at, last) in [
        (11, Last::Ended),
        (12, Last::None),
        (RECHECK_MS + 9, Last::None),
    ] {
        assert!(
            !unknown.poll(at, HOME, last),
            "an expired old receipt and successful NotApplicable cannot create a per-look loop"
        );
    }
    assert!(unknown.poll(RECHECK_MS + 10, HOME, Last::None));
    assert!(
        unknown.after_request(
            RepositoryRequest::Refresh(RefreshRequest::inputs(scope, finished.version)),
            RECHECK_MS + 11,
            HOME,
            Last::Ended,
        ),
        "a real source transition interrupts ordinary unknown-source pacing"
    );
}

#[test]
fn shared_refresh_retry_allows_local_repair_without_losing_its_original_request() {
    let refresh = RefreshRequest::inputs(RefreshScope::new([1; 32]), RefreshVersion::default());
    let request = RepositoryRequest::Refresh(refresh);
    let mut due = Due::default();
    let failed = RefreshObservation {
        pending: Some(refresh),
        retry_ms: Some(5_000),
        ..RefreshObservation::default()
    };
    due.observe(failed);
    assert!(!due.after_request(request, 0, HOME, Last::None));
    due.observe(RefreshObservation {
        retry_ms: Some(4_999),
        ..failed
    });
    assert!(due.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::None));
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    due.observe(RefreshObservation {
        retry_ms: Some(4_998),
        ..failed
    });
    assert!(
        !due.poll(2, HOME, Last::Ended),
        "a repair that ran beside it does not spin or move the refresh deadline"
    );
    due.observe(RefreshObservation {
        retry_ms: Some(0),
        ..failed
    });
    assert!(due.poll(5_000, HOME, Last::None));
    assert_eq!(due.repository_request(), request);

    let fulfilled = RefreshObservation {
        scope: refresh.scope,
        version: RefreshVersion {
            input: 1,
            attempt: 1,
            fulfilled: 1,
        },
        recheck_ms: Some(RECHECK_MS),
        ..RefreshObservation::default()
    };
    // This owner's full job can fail after its Native repository row succeeds.
    // The repository receipt does not acknowledge the remaining generic rows.
    for outcome in [Last::Failed, Last::Unfinished] {
        let mut owned = Due::default();
        assert!(owned.after_request(request, 0, HOME, Last::None));
        owned.observe(fulfilled);
        assert!(!owned.poll(1, HOME, outcome));
        assert!(!owned.poll(FIRST_RETRY_MS, HOME, Last::None));
        assert!(owned.poll(FIRST_RETRY_MS + 1, HOME, Last::None));
        assert_eq!(owned.repository_work(), RepositoryWork::Repair);
        assert!(!owned.poll(FIRST_RETRY_MS + 2, HOME, Last::Ended));

        // The successful owned generic job clears its captured debt. A later forced
        // refresh whose runner waited for the lock while another owner fulfilled it runs its
        // own generic pass once the lock is let go, and is owed nothing after it.
        let forced =
            RepositoryRequest::Refresh(RefreshRequest::after(fulfilled.scope, fulfilled.version));
        assert!(owned.after_request(forced, FIRST_RETRY_MS + 1_000, HOME, Last::None));
        assert!(!owned.poll(FIRST_RETRY_MS + 1_001, HOME, Last::Running));
        owned.observe(RefreshObservation {
            version: RefreshVersion {
                attempt: 2,
                fulfilled: 2,
                ..fulfilled.version
            },
            ..fulfilled
        });
        assert!(!owned.poll(FIRST_RETRY_MS + 1_002, HOME, Last::Ended));
        assert!(!owned.poll(2 * FIRST_RETRY_MS + 1_002, HOME, Last::None));
    }

    // Repair can run beside another owner's deferred Refresh without spending
    // that Refresh's failure streak. Its own non-repository failure still survives.
    for outcome in [Last::Ended, Last::Failed, Last::Unfinished] {
        let mut local = Due::default();
        local.observe(failed);
        assert!(!local.after_request(request, 0, HOME, Last::None));
        local.observe(RefreshObservation {
            retry_ms: Some(FIRST_RETRY_MS - 1),
            ..failed
        });
        assert!(local.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::None));
        local.observe(fulfilled);
        assert!(!local.poll(2, HOME, outcome));
        assert!(!local.poll(FIRST_RETRY_MS - 1, HOME, Last::None));
        assert!(local.poll(FIRST_RETRY_MS, HOME, Last::None));
        assert_eq!(local.repository_work(), RepositoryWork::Repair);
        assert!(!local.poll(FIRST_RETRY_MS + 1, HOME, Last::Ended));
    }

    let mut during = Due::default();
    assert!(during.after_request(request, 0, HOME, Last::None));
    assert!(!during.after_local_progress(RepositoryWork::Repair, 1, HOME, Last::Running));
    during.observe(fulfilled);
    assert!(!during.after_request(
        RepositoryRequest::Refresh(RefreshRequest::ensure(fulfilled.scope, fulfilled.version)),
        2,
        HOME,
        Last::Running,
    ));
    assert!(during.poll(3, HOME, Last::Ended));
    assert_eq!(during.repository_work(), RepositoryWork::Repair);
    assert!(!during.poll(4, HOME, Last::Ended));
    assert!(!during.poll(5, HOME, Last::None));

    // A completed new source consumes the old source's retained retry before
    // its Refresh becomes Repair, so the obsolete source cannot revive later.
    let mut superseded = Due::default();
    superseded.observe(failed);
    assert!(!superseded.after_request(request, 0, HOME, Last::None));
    let winner = RefreshObservation {
        scope: Some(RefreshScope::new([2; 32])),
        version: RefreshVersion {
            input: 2,
            attempt: 2,
            fulfilled: 2,
        },
        recheck_ms: Some(RECHECK_MS),
        ..RefreshObservation::default()
    };
    superseded.observe(winner);
    assert!(superseded.after_request(
        RepositoryRequest::Refresh(RefreshRequest::inputs(
            winner.scope.unwrap(),
            fulfilled.version,
        )),
        1,
        HOME,
        Last::None,
    ));
    assert_eq!(superseded.repository_work(), RepositoryWork::Repair);
    assert!(!superseded.poll(2, HOME, Last::Ended));
    assert!(!superseded.poll(FIRST_RETRY_MS, HOME, Last::None));

    let mut identity = Due::default();
    identity.observe(failed);
    identity.request(request);
    identity.identity_changed();
    assert!(
        identity.after_pass(&unchanged(), 0, HOME, Last::None),
        "repository backoff cannot defer the independent identity subject"
    );
    assert_eq!(identity.repository_request(), RepositoryRequest::Repair);
    identity.observe(RefreshObservation {
        retry_ms: Some(FIRST_RETRY_MS - 1),
        ..failed
    });
    identity.identity_changed();
    assert!(!identity.after_pass(&unchanged(), 1, HOME, Last::Running));
    identity.observe(RefreshObservation {
        retry_ms: Some(FIRST_RETRY_MS - 2),
        ..failed
    });
    assert!(identity.after_pass(&unchanged(), 2, HOME, Last::Ended));
    assert_eq!(identity.repository_request(), RepositoryRequest::Repair);
    identity.observe(RefreshObservation {
        retry_ms: Some(FIRST_RETRY_MS - 3),
        ..failed
    });
    assert!(!identity.poll(3, HOME, Last::Ended));
    identity.observe(RefreshObservation {
        retry_ms: Some(0),
        ..failed
    });
    assert!(identity.poll(FIRST_RETRY_MS, HOME, Last::None));
    assert_eq!(identity.repository_request(), request);
    identity.observe(fulfilled);
    identity.identity_changed();
    assert!(identity.after_pass(&unchanged(), FIRST_RETRY_MS + 1, HOME, Last::Ended));
    assert_eq!(identity.repository_request(), RepositoryRequest::Repair);
    assert!(!identity.after_pass(&unchanged(), FIRST_RETRY_MS + 2, HOME, Last::Ended));

    for (outcome, ended_at) in [
        (Last::Failed, 0), // The starter refused the selected plan.
        (Last::Failed, 1),
        (Last::Unfinished, 1),
    ] {
        let remote_at = 3 * FIRST_RETRY_MS;
        let retry_at = ended_at + FIRST_RETRY_MS;
        let mut local = Due::default();
        local.observe(RefreshObservation {
            retry_ms: Some(remote_at),
            ..failed
        });
        local.request(request);
        local.identity_changed();
        assert!(local.after_pass(&unchanged(), 0, HOME, Last::None));
        assert_eq!(local.repository_request(), RepositoryRequest::Repair);
        local.reaped(outcome, ended_at);
        local.observe(RefreshObservation {
            retry_ms: Some(remote_at - ended_at),
            ..failed
        });
        assert!(!local.poll(ended_at, HOME, Last::None));
        local.observe(RefreshObservation {
            retry_ms: Some(remote_at - (retry_at - 1)),
            ..failed
        });
        assert!(!local.poll(retry_at - 1, HOME, Last::None));
        local.observe(RefreshObservation {
            retry_ms: Some(remote_at - retry_at),
            ..failed
        });
        assert!(
            local.poll(retry_at, HOME, Last::None),
            "identity {outcome:?} at {ended_at} retains its own retry before repository backoff"
        );
        assert_eq!(local.repository_request(), RepositoryRequest::Repair);
        local.observe(RefreshObservation {
            retry_ms: Some(remote_at - (retry_at + 1)),
            ..failed
        });
        assert!(!local.poll(retry_at + 1, HOME, Last::Ended));
        local.observe(RefreshObservation {
            retry_ms: Some(1),
            ..failed
        });
        assert!(!local.poll(remote_at - 1, HOME, Last::None));
        local.observe(RefreshObservation {
            retry_ms: Some(0),
            ..failed
        });
        assert!(local.poll(remote_at, HOME, Last::None));
        assert_eq!(local.repository_request(), request);
        local.observe(fulfilled);
        assert!(!local.poll(remote_at + 1, HOME, Last::Ended));
    }
}

#[test]
fn a_route_event_captures_one_boundary_while_the_lan_is_held() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    let observed = RefreshObservation {
        version: RefreshVersion {
            attempt: 2,
            ..RefreshVersion::default()
        },
        ..RefreshObservation::default()
    };
    due.observe(observed);
    due.hold(1, AWAY, Last::Running);
    due.observe(RefreshObservation {
        version: RefreshVersion {
            attempt: 3,
            ..observed.version
        },
        ..observed
    });
    due.hold(2, AWAY, Last::Running);
    assert!(due.poll(3, AWAY, Last::Ended));
    let RepositoryRequest::Refresh(request) = due.repository_request() else {
        panic!("a route move requests refresh");
    };
    assert_eq!(
        request.after_attempt,
        Some(2),
        "waiting must not move the event's boundary"
    );
}

/// The runner keeps its starting app's privacy responsibility and leaves only the
/// subscriber's background band. A started child below proves its own session.
#[test]
fn the_runner_is_init_converge_the_subscribers_out_of_the_band_never_disclaiming() {
    let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    for trigger in [SUBSCRIBER, WINDOW, SETUP] {
        for work in [RepositoryWork::Repair, RepositoryWork::Refresh] {
            let command = runner(exe, trigger, work);
            assert_eq!(command.get_program(), exe);
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(
                args,
                [
                    "init",
                    "--converge",
                    "--trigger",
                    trigger,
                    "--repository-work",
                    work.word()
                ]
            );
            assert!(
                !command.get_disclaim_responsibility(),
                "the runner's privacy-gated acts stay the starting app's"
            );
            assert_eq!(
                command.leaves_background_band(),
                trigger == SUBSCRIBER,
                "only the subscriber's runner leaves its starter's background band ({trigger})"
            );
        }
        let request = RepositoryRequest::Refresh(RefreshRequest::after(
            Some(RefreshScope::new([7; 32])),
            RefreshVersion {
                input: 2,
                attempt: 4,
                fulfilled: 3,
            },
        ));
        let command = runner_requested(exe, trigger, request);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[4], "--repository-request");
        assert_eq!(
            aterm_json::from_str::<RepositoryRequest>(args[5].to_str().unwrap()).unwrap(),
            request,
            "the original request boundary survives runner IPC"
        );
        assert!(!command.get_disclaim_responsibility());
        assert_eq!(command.leaves_background_band(), trigger == SUBSCRIBER);
    }
}

/// A runner that does not end by itself leaves the start, and the reading of how it stands,
/// returning at once: the subscriber's long-poll never waits on a pass. The child leads a
/// process group of its own (the session `setsid` made), and one that ends with [`HELD`]
/// reads as failed: a runner a starter started waits for the lock rather than end so, and one
/// that ends so all the same (an older installed app's pass) is retried on the failed backoff.
#[cfg(unix)]
#[test]
fn starting_a_runner_never_waits_for_it_and_gives_it_its_own_session() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    let script = |name: &str, body: String| {
        let path = scratch.path().join(name);
        std::fs::write(&path, format!("#!{}\n{body}\n", sh.display())).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        path
    };
    let endless = script("endless", format!("exec {} 600", sleep.display()));
    let mut runner =
        Some(start(&endless, "subscriber", RepositoryWork::Refresh).expect("start a runner"));
    assert_eq!(Last::of(&mut runner), Last::Running);
    let child = runner.as_mut().unwrap();
    let pid = i32::try_from(child.id()).expect("a pid");
    let lineage = aterm_uds::process::read_lineage(pid).expect("its lineage");
    assert_eq!(
        lineage.pgid,
        child.id(),
        "it leads a group (and the session setsid made) of its own"
    );
    child.kill().expect("kill the stand-in");
    child.wait().expect("reap it");
    assert_eq!(Last::of(&mut runner), Last::Failed);
    assert!(runner.is_none());
    assert_eq!(
        Last::of(&mut runner),
        Last::None,
        "a signal is consumed once"
    );
    for (code, expected) in [
        (0, Last::Ended),
        (1, Last::Failed),
        (HELD, Last::Failed),
        (UNFINISHED, Last::Unfinished),
    ] {
        let exe = script(&format!("exit-{code}"), format!("exit {code}"));
        let mut runner =
            Some(start(&exe, "subscriber", RepositoryWork::Refresh).expect("start a runner"));
        runner.as_mut().unwrap().wait().expect("reap it");
        assert_eq!(Last::of(&mut runner), expected);
        assert_eq!(
            Last::of(&mut runner),
            Last::None,
            "exit {code} is consumed once"
        );
    }
}

/// The causes of the start `due` last chose.
fn caused(due: &Due) -> Causes {
    due.note().causes
}

fn causes<const N: usize>(set: [Causes; N]) -> Causes {
    set.into_iter().fold(Causes::default(), Causes::with)
}

/// EACH SUBSCRIBER START NOTES EXACTLY WHAT CAUSED IT: every cause is noted where its work is
/// requested and leaves with the start it caused, so each set below is the whole set of that
/// start. The first start has no refresh boundary yet, so it is `start` and `recheck`; a
/// retry names what set its time; identity's own retry is named beside the runner's.
/// NEGATIVE CONTROL: causes left in place at a start make the second start's set
/// `{start, recheck, applied}`.
#[test]
fn each_subscriber_start_notes_exactly_its_causes() {
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    assert_eq!(caused(&due), causes([Causes::START, Causes::RECHECK]));
    assert!(due.after_pass(&applied(), MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::APPLIED);
    due.failed();
    assert!(due.after_pass(&unchanged(), 2 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::RECOVERED);
    assert!(due.after_unreachable_pass(&applied(), 3 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::APPLIED);
    assert!(due.after_local_progress(RepositoryWork::Repair, 4 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::LOCAL);
    due.paused();
    assert!(due.poll(5 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::ACTIVATION);
    // The projection notes an observer's request beside the request itself.
    due.because(Causes::INPUT_REPAIR);
    due.request(RepositoryRequest::Repair);
    assert!(due.poll(6 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::INPUT_REPAIR);
    due.because(Causes::INPUT_REFRESH);
    due.request(RepositoryRequest::Refresh(RefreshRequest::after(
        None,
        RefreshVersion::default(),
    )));
    assert!(due.poll(7 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::INPUT_REFRESH);
    due.identity_changed();
    assert!(due.poll(8 * MINUTE, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::IDENTITY);
    assert!(due.poll(9 * MINUTE, AWAY, Last::Ended));
    assert_eq!(caused(&due), Causes::ROUTES);
    assert!(due.poll(MINUTE, AWAY, Last::Ended));
    assert_eq!(caused(&due), Causes::CLOCK);
    assert!(!due.poll(MINUTE + RECHECK_MS - 1, AWAY, Last::Ended));
    assert!(due.poll(MINUTE + RECHECK_MS, AWAY, Last::Ended));
    assert_eq!(caused(&due), Causes::RECHECK);

    let failed = 40 * MINUTE;
    assert!(!due.poll(failed, AWAY, Last::Failed));
    assert!(!due.poll(failed + FIRST_RETRY_MS - 1, AWAY, Last::None));
    assert!(due.poll(failed + FIRST_RETRY_MS, AWAY, Last::None));
    assert_eq!(caused(&due), Causes::RETRY_FAILED);
    let unfinished = 41 * MINUTE;
    assert!(!due.poll(unfinished, AWAY, Last::Unfinished));
    assert!(due.poll(unfinished + 2 * FIRST_RETRY_MS, AWAY, Last::None));
    assert_eq!(caused(&due), Causes::RETRY_UNFINISHED);

    // Identity's own retry and the runner's come due together after a failed pass that
    // carried identity debt.
    let mut due = Due::default();
    assert!(due.at_start(0, HOME, Last::None));
    due.identity_changed();
    assert!(due.poll(1000, HOME, Last::Ended));
    assert_eq!(caused(&due), Causes::IDENTITY);
    assert!(!due.poll(2000, HOME, Last::Failed));
    assert!(due.poll(2000 + FIRST_RETRY_MS, HOME, Last::None));
    assert_eq!(
        caused(&due),
        causes([Causes::RETRY_FAILED, Causes::RETRY_IDENTITY])
    );

    // The shared wave's retry: the request waits for it, and the start it makes names it
    // beside the boundary that was owed with it.
    let refresh = RefreshRequest::inputs(RefreshScope::new([1; 32]), RefreshVersion::default());
    let waiting = RefreshObservation {
        pending: Some(refresh),
        retry_ms: Some(5_000),
        ..RefreshObservation::default()
    };
    let mut due = Due::default();
    due.observe(waiting);
    assert!(!due.after_request(RepositoryRequest::Refresh(refresh), 0, HOME, Last::None));
    due.observe(RefreshObservation {
        retry_ms: Some(0),
        ..waiting
    });
    assert!(due.poll(5_000, HOME, Last::None));
    assert_eq!(caused(&due), causes([Causes::RECHECK, Causes::RETRY_WAVE]));
}

/// The subscriber's starter over a ten-minute hold of `align.lock`, looked at every second
/// while an input look asks for a repair each minute: how many runners of `runner` it started,
/// with the starter and the runner it keeps at the end. A runner that `ends_at_once` is let end
/// before the next look, so its end is read there as the live starter reads it; one that waits
/// for the lock still runs at every look.
#[cfg(target_vendor = "apple")]
fn ten_minutes_held(runner: &Path, ends_at_once: bool) -> (usize, Due, Option<Child>) {
    let mut due = Due::default();
    let mut child: Option<Child> = None;
    let mut ended = None;
    let mut starts = 0;
    for second in 0..=10 * 60 {
        let at = second * 1000;
        let last = ended.take().unwrap_or_else(|| Last::of(&mut child));
        let starting = if second == 0 {
            due.at_start(at, HOME, last)
        } else {
            if at % MINUTE == 0 {
                due.request(RepositoryRequest::Repair);
            }
            due.poll(at, HOME, last)
        };
        if starting {
            child = Some(start(runner, SUBSCRIBER, due.repository_work()).expect("start a runner"));
            starts += 1;
            if ends_at_once {
                ended = Some(runner_ended(&mut child, "a runner that does not wait ends"));
            }
        }
    }
    (starts, due, child)
}

/// How `child` ended, once it has: a one-minute hang detector on its progress, not a budget.
#[cfg(target_vendor = "apple")]
fn runner_ended(child: &mut Option<Child>, what: &str) -> Last {
    let pid = child.as_ref().expect("a runner").id();
    aterm_spec::wait::until_while_working(pid, what, || match Last::of(child) {
        Last::Running => None,
        last => Some(last),
    })
}

/// WHILE ANOTHER PROCESS HOLDS THE PASS'S LOCK, A STARTER STARTS NO FURTHER RUNNER: a runner
/// that waits for `align.lock` in the kernel, as one a starter started does
/// (`aterm_machines::sync::wait_lock`; here `lockf`, which takes the same BSD lock on the same
/// file), reads as running for as long as another holds the lock. So the subscriber's starter,
/// looked at every second over a ten-minute hold while an input look asks for a repair each
/// minute, starts none after its first. Once the holder lets go, that runner takes the lock and
/// ends, and the starter starts exactly one more for the repairs owed meanwhile, which finds the
/// lock free and ends at once; nothing follows it.
/// NEGATIVE CONTROL, run here: the same ten minutes with a runner that ends at once for the held
/// lock (`lockf -t 0` exits [`HELD`], 75, as every runner did before started ones waited). The
/// starter reads that end as a failure, so 17 runners start where the waiting one's 1 is wanted:
/// the first at 0 s, its retries on the failed backoff (5 s doubling) at 6, 17, 38, 79, 160 and
/// 321 s, and one for each minute's repair. Before, [`Last::of`] read exit 75 as
/// [`Last::Held`], and the starter restarted the runner five seconds after each such end, with
/// no backoff.
#[cfg(target_vendor = "apple")]
#[test]
fn while_another_process_holds_align_lock_a_starter_starts_no_further_runner() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let converge = scratch.path().join("converge");
    aterm_uds::private_file::ensure_private_dir(&converge).expect("a private folder");
    let holder = aterm_uds::private_file::PrivateLock::take(
        &converge,
        "align.lock",
        std::time::Duration::ZERO,
        "held by the test",
    )
    .expect("the lock is free");
    let stand_in = |path: &str| aterm_uds::deadline::stand_in(Path::new(path));
    let runner = |name: &str, lockf_flags: &str| {
        let path = scratch.path().join(name);
        std::fs::write(
            &path,
            format!(
                "#!{}\nexec {} {lockf_flags} {} {}\n",
                stand_in("/bin/sh").display(),
                stand_in("/usr/bin/lockf").display(),
                converge.join("align.lock").display(),
                stand_in("/usr/bin/true").display()
            ),
        )
        .expect("the runner");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        path
    };
    let tries_once = runner("tries-once", "-k -t 0");
    let waits = runner("waits", "-k");

    let (starts, _, _) = ten_minutes_held(&tries_once, true);
    assert_eq!(
        starts, 17,
        "a runner that ends at once for the held lock: 7 on the failed backoff, 10 for repairs"
    );

    let (starts, mut due, mut child) = ten_minutes_held(&waits, false);
    assert_eq!(
        starts, 1,
        "a runner waiting for the lock is the starter's one"
    );
    drop(holder);
    let last = runner_ended(
        &mut child,
        "the runner takes the lock once it is let go and ends",
    );
    assert_eq!(last, Last::Ended);
    assert!(
        due.poll(601_000, HOME, last),
        "its end starts the repair owed meanwhile"
    );
    assert_eq!(due.repository_work(), RepositoryWork::Repair);
    let mut second = Some(start(&waits, SUBSCRIBER, due.repository_work()).expect("a runner"));
    let mut last = runner_ended(&mut second, "a runner finding the lock free ends at once");
    assert_eq!(last, Last::Ended);
    for at in [601_001, 601_000 + FIRST_RETRY_MS, 20 * MINUTE] {
        assert!(!due.poll(at, HOME, last), "nothing more is owed at {at}");
        last = Last::of(&mut second);
    }
}
