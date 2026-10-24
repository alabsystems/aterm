// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lease ledger (design §11.5): retries, worker loss, cancellation, duplicate completion,
//! stale results and admission, each refused or counted by name.

use super::lease::{
    ClockBackwards, Drain, Effect, Gone, JobState, LIMITS_V1, LeaseRefusal, Ledger, Limits, Nanos,
    Outcome, Token, VoidBy, WorkerId, WorkerState,
};
use aterm_aos::Digest;

const W: Nanos = 1_000;
const GRACE: Nanos = 100;
const HELLO: Nanos = 500;

fn limits() -> Limits {
    Limits {
        max_workers: 4,
        hello_ns: HELLO,
        window_ns: W,
        grace_ns: GRACE,
        max_attempts: 3,
        items: 17,
    }
}

fn d(n: u8) -> Digest {
    Digest([n; 32])
}

/// A ledger with `workers` workers joined and greeted at time 0.
fn ready(workers: u32) -> Ledger {
    let mut l = Ledger::new(limits());
    for _ in 0..workers {
        let w = l.join(0).expect("clock").expect("room");
        l.hello(w, Ok(()), 0).expect("clock");
    }
    l
}

/// Search `search` opened with `jobs` jobs and every idle worker leased.
fn open(l: &mut Ledger, search: u32, jobs: u32, now: Nanos) -> Vec<Effect> {
    let mut e = l.begin(search, jobs, now).expect("clock");
    e.extend(l.assign(now).expect("clock"));
    e
}

/// `effects`, then every idle worker leased.
fn then_assign(l: &mut Ledger, mut effects: Vec<Effect>, now: Nanos) -> Vec<Effect> {
    effects.extend(l.assign(now).expect("clock"));
    effects
}

fn leases(effects: &[Effect]) -> Vec<(WorkerId, Token)> {
    effects
        .iter()
        .filter_map(|e| match *e {
            Effect::Lease(w, t) => Some((w, t)),
            _ => None,
        })
        .collect()
}

fn token(search: u32, slot: u32, attempt: u8, worker: u32) -> Token {
    Token {
        search,
        slot,
        attempt,
        worker,
    }
}

fn void_by(l: &Ledger) -> Option<LeaseRefusal> {
    match l.outcome() {
        Outcome::Void(VoidBy::Ledger(r)) => Some(r),
        _ => None,
    }
}

#[test]
fn limits_v1_are_the_designs() {
    assert_eq!(LIMITS_V1.max_workers, 16);
    assert_eq!(LIMITS_V1.hello_ns, 60_000_000_000);
    assert_eq!(LIMITS_V1.window_ns, 60_000_000_000);
    assert_eq!(LIMITS_V1.grace_ns, 5_000_000_000);
    assert_eq!(LIMITS_V1.max_attempts, 3);
    assert_eq!(LIMITS_V1.items, 17);
}

/// Idle workers take the lowest pending jobs in id order as a search opens, and a claim frees the
/// worker for the next.
#[test]
fn begin_leases_the_lowest_jobs_to_idle_workers() {
    let mut l = ready(2);
    let e = open(&mut l, 1, 3, 10);
    assert_eq!(
        leases(&e),
        [
            (WorkerId(0), token(1, 0, 1, 0)),
            (WorkerId(1), token(1, 1, 1, 1)),
        ]
    );
    assert_eq!(l.outcome(), Outcome::Open);
    let e = l
        .result(WorkerId(1), token(1, 1, 1, 1), d(1), 20)
        .expect("clock");
    let e = then_assign(&mut l, e, 20);
    assert!(e.contains(&Effect::Accepted(WorkerId(1), token(1, 1, 1, 1))));
    assert_eq!(leases(&e), [(WorkerId(1), token(1, 2, 1, 1))]);
    assert_eq!(
        l.job(1),
        Some(JobState::Claimed {
            lease: token(1, 1, 1, 1),
            digest: d(1)
        })
    );
}

/// Every job claimed and no worker draining completes the search; the workers stay idle for the
/// next, whose tokens carry its serial.
#[test]
fn a_search_completes_and_the_pool_serves_the_next() {
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.result(WorkerId(0), token(1, 0, 1, 0), d(1), 5)
        .expect("clock");
    assert_eq!(l.outcome(), Outcome::Open, "complete only once finished");
    l.finish(5).expect("clock");
    assert_eq!(l.outcome(), Outcome::Complete);
    assert_eq!(l.worker(WorkerId(0)), Some(WorkerState::Idle));
    let e = open(&mut l, 2, 1, 6);
    assert_eq!(leases(&e), [(WorkerId(0), token(2, 0, 1, 0))]);
    assert_eq!(l.outcome(), Outcome::Open);
}

/// A worker lost mid-job reopens the job with its attempts kept, for the next idle worker; the
/// claims it returned earlier stay.
#[test]
fn eof_mid_job_reopens_it_and_keeps_the_workers_claims() {
    let mut l = ready(2);
    open(&mut l, 1, 3, 0);
    l.result(WorkerId(0), token(1, 0, 1, 0), d(1), 1)
        .expect("clock");
    l.assign(1).expect("clock");
    // Worker 0 now holds job 2.
    let e = l.lost(WorkerId(0), 2).expect("clock");
    assert!(e.contains(&Effect::Kill(WorkerId(0))));
    assert_eq!(l.worker(WorkerId(0)), Some(WorkerState::Gone(Gone::Eof)));
    assert_eq!(l.job(2), Some(JobState::Pending { attempts: 1 }));
    assert_eq!(
        l.job(0),
        Some(JobState::Claimed {
            lease: token(1, 0, 1, 0),
            digest: d(1)
        })
    );
    let e = l
        .result(WorkerId(1), token(1, 1, 1, 1), d(2), 3)
        .expect("clock");
    let e = then_assign(&mut l, e, 3);
    assert_eq!(leases(&e), [(WorkerId(1), token(1, 2, 2, 1))]);
    assert_eq!(l.counts().retries, 1);
    assert_eq!(l.counts().lost, 1);
}

/// A job whose third lease is lost is exhausted and the search void, with no local fallback.
#[test]
fn the_third_lost_lease_exhausts_the_job_and_voids() {
    let mut l = Ledger::new(limits());
    let mut now = 0;
    for _ in 0..4 {
        let w = l.join(now).expect("clock").expect("room");
        l.hello(w, Ok(()), now).expect("clock");
    }
    open(&mut l, 1, 1, now);
    for (attempt, w) in [(1u8, 0u32), (2, 1), (3, 2)] {
        assert_eq!(
            l.job(0),
            Some(JobState::Leased {
                lease: token(1, 0, attempt, w)
            })
        );
        now += 1;
        l.lost(WorkerId(w), now).expect("clock");
        l.assign(now).expect("clock");
    }
    assert_eq!(l.job(0), Some(JobState::Exhausted));
    assert_eq!(void_by(&l), Some(LeaseRefusal::Exhausted { slot: 0 }));
    assert_eq!(l.worker(WorkerId(3)), Some(WorkerState::Idle));
}

/// An expired lease's job is open again at once, the holder draining and sent `Cancel`.
#[test]
fn an_expired_lease_is_released_at_once() {
    let mut l = ready(2);
    open(&mut l, 1, 2, 0);
    l.result(WorkerId(1), token(1, 1, 1, 1), d(1), 10)
        .expect("clock");
    let e = l.tick(W).expect("clock");
    let e = then_assign(&mut l, e, W);
    assert!(e.contains(&Effect::Cancel(WorkerId(0), token(1, 0, 1, 0))));
    assert_eq!(leases(&e), [(WorkerId(1), token(1, 0, 2, 1))]);
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Draining {
            lease: token(1, 0, 1, 0),
            until: W + GRACE,
            why: Drain::Expired,
            k: None,
            answered: false,
        })
    );
    assert_eq!(l.counts().expired, 1);
}

/// A lease lives until its deadline and not at it.
#[test]
fn a_lease_lives_until_its_deadline_and_not_at_it() {
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.tick(W - 1).expect("clock");
    assert!(matches!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Busy { deadline: W, .. })
    ));
    l.tick(W).expect("clock");
    assert!(matches!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Draining { .. })
    ));
}

/// Progress renews the lease by one window; progress that does not advance, or names no
/// training item, voids.
#[test]
fn progress_that_does_not_advance_voids() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.progress(WorkerId(0), t, 3, 50).expect("clock");
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Busy {
            lease: t,
            deadline: 50 + W,
            k: Some(3)
        })
    );
    l.progress(WorkerId(0), t, 3, 60).expect("clock");
    assert_eq!(
        void_by(&l),
        Some(LeaseRefusal::NoProgress {
            k: 3,
            last: Some(3)
        })
    );
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.progress(WorkerId(0), t, 17, 1).expect("clock");
    assert_eq!(
        void_by(&l),
        Some(LeaseRefusal::NoProgress { k: 17, last: None })
    );
}

/// The `k` rule bounds a lease to 18 windows: 17 renewals, then expiry.
#[test]
fn a_lease_cannot_outlive_eighteen_windows() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    let mut now = 0;
    for k in 0..17u16 {
        now += W - 1;
        l.tick(now).expect("clock");
        l.progress(WorkerId(0), t, k, now).expect("clock");
    }
    assert_eq!(l.outcome(), Outcome::Open);
    let walked = l.clone();
    l.tick(now + W).expect("clock");
    assert!(matches!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Draining { .. })
    ));
    // No renewal is accepted after item 16: neither a lower item nor one past the last.
    for k in [15u16, 17] {
        let mut l = walked.clone();
        l.progress(WorkerId(0), t, k, now + 1).expect("clock");
        assert_eq!(
            void_by(&l),
            Some(LeaseRefusal::NoProgress { k, last: Some(16) }),
            "k {k}"
        );
        assert_eq!(
            l.worker(WorkerId(0)),
            Some(WorkerState::Gone(Gone::Refused))
        );
    }
}

/// Progress from a worker draining for its lease is dropped and counted, and voids nothing.
#[test]
fn progress_after_expiry_is_dropped() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(2);
    open(&mut l, 1, 1, 0);
    l.tick(W).expect("clock");
    l.progress(WorkerId(0), t, 4, W + 1).expect("clock");
    assert_eq!(l.counts().late_progress, 1);
    assert_eq!(l.outcome(), Outcome::Open);
}

/// When no worker can take an open job, the search is void.
#[test]
fn no_workers_left_voids() {
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    l.lost(WorkerId(0), 1).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::NoWorkers { pending: 2 }));
    assert_eq!(l.job(0), Some(JobState::Cancelled));
}

/// A void cancels every open job and sends `Cancel` to every holder, which drains.
#[test]
fn cancel_all_drains_every_holder() {
    let mut l = ready(3);
    open(&mut l, 1, 4, 0);
    let e = l.void(VoidBy::Interrupted, 5).expect("clock");
    let cancels: Vec<Effect> = e
        .into_iter()
        .filter(|e| matches!(e, Effect::Cancel(..)))
        .collect();
    assert_eq!(cancels.len(), 3);
    assert!(l.jobs().iter().all(|j| *j == JobState::Cancelled));
    for w in 0..3 {
        assert!(matches!(
            l.worker(WorkerId(w)),
            Some(WorkerState::Draining {
                why: Drain::Cancelled,
                ..
            })
        ));
    }
    assert_eq!(l.outcome(), Outcome::Void(VoidBy::Interrupted));
    assert!(!l.quiescent());
    // A result from a cancelled holder is dropped; its acknowledgement retires it.
    l.result(WorkerId(0), token(1, 0, 1, 0), d(1), 6)
        .expect("clock");
    assert_eq!(l.counts().dropped, 1);
    l.void(VoidBy::Caller, 7).expect("clock");
    assert_eq!(l.outcome(), Outcome::Void(VoidBy::Interrupted));
}

/// A drain past its grace is lost and killed.
#[test]
fn a_drain_past_grace_is_lost_and_killed() {
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.void(VoidBy::Interrupted, 10).expect("clock");
    l.tick(10 + GRACE - 1).expect("clock");
    assert!(matches!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Draining { .. })
    ));
    let e = l.tick(10 + GRACE).expect("clock");
    assert!(e.contains(&Effect::Kill(WorkerId(0))));
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Gone(Gone::DrainTimeout))
    );
    assert!(l.quiescent());
}

/// An acknowledgement of `Cancel` retires the worker: it takes no new lease.
#[test]
fn an_ack_retires_the_worker() {
    let mut l = ready(2);
    open(&mut l, 1, 2, 0);
    l.result(WorkerId(1), token(1, 1, 1, 1), d(1), 1)
        .expect("clock");
    l.tick(W).expect("clock");
    l.assign(W).expect("clock");
    let e = l
        .cancelled(WorkerId(0), token(1, 0, 1, 0), W + 1)
        .expect("clock");
    assert!(e.contains(&Effect::Kill(WorkerId(0))));
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Gone(Gone::Retired))
    );
    assert_eq!(l.counts().cancelled, 1);
    assert_eq!(
        l.job(0),
        Some(JobState::Leased {
            lease: token(1, 0, 2, 1)
        })
    );
}

/// A `Cancelled` with no `Cancel` sent voids.
#[test]
fn an_unasked_cancelled_voids() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.cancelled(WorkerId(0), t, 1).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::Unasked { sent: t }));
}

/// A resend equal to the accepted claim is counted and changes nothing.
#[test]
fn an_equal_resend_changes_nothing() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    l.result(WorkerId(0), t, d(1), 1).expect("clock");
    let before = l.jobs().to_vec();
    let e = l.result(WorkerId(0), t, d(1), 2).expect("clock");
    assert!(e.is_empty());
    assert_eq!(l.jobs(), before.as_slice());
    assert_eq!(l.counts().duplicate, 1);
    assert_eq!(l.outcome(), Outcome::Open);
}

/// A resend that differs from the accepted claim voids.
#[test]
fn a_different_resend_voids() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    l.result(WorkerId(0), t, d(1), 1).expect("clock");
    l.result(WorkerId(0), t, d(2), 2).expect("clock");
    assert_eq!(
        void_by(&l),
        Some(LeaseRefusal::Differs { search: 1, slot: 0 })
    );
}

/// A second resend on one lease is unasked.
#[test]
fn a_second_resend_is_unasked() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    l.result(WorkerId(0), t, d(1), 1).expect("clock");
    l.result(WorkerId(0), t, d(1), 2).expect("clock");
    l.result(WorkerId(0), t, d(1), 3).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::Unasked { sent: t }));
}

/// A resend that arrives after its search ended is that search's duplicate, compared with its
/// accepted claim: equal is counted, different voids.
#[test]
fn a_resend_after_its_search_ended_is_a_duplicate() {
    let t = token(1, 0, 1, 0);
    for (resent, void) in [
        (d(1), None),
        (d(2), Some(LeaseRefusal::Differs { search: 1, slot: 0 })),
    ] {
        let mut l = ready(1);
        open(&mut l, 1, 1, 0);
        l.result(WorkerId(0), t, d(1), 1).expect("clock");
        l.finish(1).expect("clock");
        assert_eq!(l.outcome(), Outcome::Complete);
        open(&mut l, 2, 1, 2);
        l.result(WorkerId(0), t, resent, 3).expect("clock");
        assert_eq!(void_by(&l), void);
        if void.is_none() {
            assert_eq!(l.counts().duplicate, 1);
            assert_eq!(l.outcome(), Outcome::Open);
        }
    }
}

/// A late result equal to the job's accepted claim is counted stale, whichever arrives first,
/// and retires its sender.
#[test]
fn an_equal_late_result_is_counted_stale() {
    let late = token(1, 0, 1, 0);
    let retry = token(1, 0, 2, 1);
    for late_first in [true, false] {
        let mut l = ready(2);
        open(&mut l, 1, 2, 0);
        l.result(WorkerId(1), token(1, 1, 1, 1), d(9), 1)
            .expect("clock");
        l.tick(W).expect("clock");
        l.assign(W).expect("clock");
        assert_eq!(l.job(0), Some(JobState::Leased { lease: retry }));
        if late_first {
            l.result(WorkerId(0), late, d(1), W + 1).expect("clock");
            assert_eq!(l.counts().stale, 0);
            l.result(WorkerId(1), retry, d(1), W + 2).expect("clock");
        } else {
            l.result(WorkerId(1), retry, d(1), W + 1).expect("clock");
            l.finish(W + 1).expect("clock");
            assert_eq!(l.outcome(), Outcome::Open, "a drain is still open");
            l.result(WorkerId(0), late, d(1), W + 2).expect("clock");
        }
        assert_eq!(l.counts().stale, 1);
        assert_eq!(
            l.worker(WorkerId(0)),
            Some(WorkerState::Gone(Gone::Retired))
        );
        l.finish(W + 3).expect("clock");
        assert_eq!(l.outcome(), Outcome::Complete);
    }
}

/// A late result that differs from the accepted claim voids, whichever arrives first.
#[test]
fn a_different_late_result_voids() {
    for late_first in [true, false] {
        let mut l = ready(2);
        open(&mut l, 1, 1, 0);
        l.tick(W).expect("clock");
        l.assign(W).expect("clock");
        let (late, retry) = (token(1, 0, 1, 0), token(1, 0, 2, 1));
        if late_first {
            l.result(WorkerId(0), late, d(1), W + 1).expect("clock");
            l.result(WorkerId(1), retry, d(2), W + 2).expect("clock");
        } else {
            l.result(WorkerId(1), retry, d(2), W + 1).expect("clock");
            l.result(WorkerId(0), late, d(1), W + 2).expect("clock");
        }
        assert_eq!(
            void_by(&l),
            Some(LeaseRefusal::Differs { search: 1, slot: 0 })
        );
    }
}

/// A search with every job claimed is not complete while a worker drains: step 5 waits for the
/// drain to end by an acknowledgement, a late result or its grace.
#[test]
fn step_five_waits_for_every_drain() {
    let mut l = ready(2);
    open(&mut l, 1, 1, 0);
    l.tick(W).expect("clock");
    l.assign(W).expect("clock");
    l.result(WorkerId(1), token(1, 0, 2, 1), d(1), W + 1)
        .expect("clock");
    l.finish(W + 1).expect("clock");
    assert_eq!(l.outcome(), Outcome::Open);
    l.tick(W + GRACE).expect("clock");
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Gone(Gone::DrainTimeout))
    );
    l.finish(W + GRACE).expect("clock");
    assert_eq!(l.outcome(), Outcome::Complete);
}

/// Another worker's token is not held by its sender.
#[test]
fn another_workers_token_is_not_held() {
    let mut l = ready(2);
    open(&mut l, 1, 2, 0);
    let theirs = token(1, 1, 1, 1);
    l.result(WorkerId(0), theirs, d(1), 1).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::NotHeld { sent: theirs }));
}

/// A token never issued, though it names its sender, is not held.
#[test]
fn a_replayed_token_is_not_held() {
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    let forged = token(1, 0, 2, 0);
    l.progress(WorkerId(0), forged, 0, 1).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::NotHeld { sent: forged }));
    let mut l = ready(1);
    open(&mut l, 2, 1, 0);
    let earlier = token(1, 0, 1, 0);
    l.result(WorkerId(0), earlier, d(1), 1).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::NotHeld { sent: earlier }));
}

/// Progress for the sender's own claimed lease is unasked.
#[test]
fn progress_for_a_claimed_lease_is_unasked() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    l.result(WorkerId(0), t, d(1), 1).expect("clock");
    l.progress(WorkerId(0), t, 5, 2).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::Unasked { sent: t }));
}

/// A foreign hello refuses that worker alone, killed; the search goes on with the rest.
#[test]
fn a_foreign_hello_refuses_that_worker_alone() {
    let mut l = Ledger::new(limits());
    let a = l.join(0).expect("clock").expect("room");
    let b = l.join(0).expect("clock").expect("room");
    open(&mut l, 1, 1, 0);
    let e = l.hello(a, Err("build"), 1).expect("clock");
    assert_eq!(e, [Effect::Kill(a)]);
    assert_eq!(l.worker(a), Some(WorkerState::Gone(Gone::Foreign("build"))));
    assert_eq!(l.outcome(), Outcome::Open);
    let e = l.hello(b, Ok(()), 2).expect("clock");
    let e = then_assign(&mut l, e, 2);
    assert_eq!(leases(&e), [(b, token(1, 0, 1, 1))]);
    assert_eq!(l.counts().foreign, 1);
}

/// A frame other than hello before hello voids, and so does a second hello.
#[test]
fn a_frame_before_hello_and_a_second_hello_void() {
    let mut l = Ledger::new(limits());
    let a = l.join(0).expect("clock").expect("room");
    l.progress(a, token(0, 0, 1, 0), 0, 1).expect("clock");
    assert_eq!(void_by(&l), Some(LeaseRefusal::NotJoined { worker: a }));
    let mut l = ready(1);
    l.hello(WorkerId(0), Ok(()), 1).expect("clock");
    assert_eq!(
        void_by(&l),
        Some(LeaseRefusal::SecondHello {
            worker: WorkerId(0)
        })
    );
}

/// A worker that does not say hello within the hello bound is lost and killed.
#[test]
fn hello_timeout_is_lost() {
    let mut l = Ledger::new(limits());
    let a = l.join(10).expect("clock").expect("room");
    l.tick(10 + HELLO - 1).expect("clock");
    assert_eq!(l.worker(a), Some(WorkerState::Joining { since: 10 }));
    let e = l.tick(10 + HELLO).expect("clock");
    assert_eq!(e, [Effect::Kill(a)]);
    assert_eq!(l.worker(a), Some(WorkerState::Gone(Gone::HelloTimeout)));
}

/// No more than `max_workers` join.
#[test]
fn no_more_than_max_workers_join() {
    let mut l = Ledger::new(limits());
    for _ in 0..4 {
        assert!(l.join(0).expect("clock").is_some());
    }
    assert_eq!(l.join(0).expect("clock"), None);
}

/// Frames from a gone worker are dropped unread and counted.
#[test]
fn frames_from_a_gone_worker_are_dropped() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(2);
    open(&mut l, 1, 2, 0);
    l.lost(WorkerId(0), 1).expect("clock");
    l.result(WorkerId(0), t, d(1), 2).expect("clock");
    l.progress(WorkerId(0), t, 1, 3).expect("clock");
    assert_eq!(l.counts().dropped, 2);
    assert_eq!(l.outcome(), Outcome::Open);
}

/// A call with an earlier `now` is refused and changes nothing.
#[test]
fn time_never_runs_backwards() {
    let mut l = ready(1);
    open(&mut l, 1, 1, 100);
    let before = (l.workers().to_vec(), l.jobs().to_vec(), l.counts());
    let refused = ClockBackwards {
        latest: 100,
        given: 99,
    };
    assert_eq!(l.tick(99), Err(refused));
    assert_eq!(
        l.result(WorkerId(0), token(1, 0, 1, 0), d(1), 99),
        Err(refused)
    );
    assert_eq!(l.void(VoidBy::Caller, 99), Err(refused));
    assert_eq!(
        (l.workers().to_vec(), l.jobs().to_vec(), l.counts()),
        before
    );
}

/// A drain is bound by the `k` rule and one result: progress that does not advance, or a second
/// result after a cancel, voids and kills its sender; so a draining worker cannot flood.
#[test]
fn a_draining_worker_cannot_flood() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(2);
    open(&mut l, 1, 2, 0);
    l.progress(WorkerId(0), t, 3, 1).expect("clock");
    // Worker 1 stays live, so the void below is the flood's and not the pool's.
    l.progress(WorkerId(1), token(1, 1, 1, 1), 0, 2)
        .expect("clock");
    l.tick(W + 1).expect("clock");
    l.progress(WorkerId(0), t, 4, W + 2).expect("clock");
    assert_eq!(l.counts().late_progress, 1);
    l.progress(WorkerId(0), t, 4, W + 3).expect("clock");
    assert_eq!(
        void_by(&l),
        Some(LeaseRefusal::NoProgress {
            k: 4,
            last: Some(4)
        })
    );
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Gone(Gone::Refused))
    );

    // A draining worker's k must still increase: a lower one voids too.
    let mut l = ready(2);
    open(&mut l, 1, 2, 0);
    l.progress(WorkerId(0), t, 3, 1).expect("clock");
    l.progress(WorkerId(1), token(1, 1, 1, 1), 0, 2)
        .expect("clock");
    l.tick(W + 1).expect("clock");
    l.progress(WorkerId(0), t, 4, W + 2).expect("clock");
    l.progress(WorkerId(0), t, 3, W + 3).expect("clock");
    assert_eq!(
        void_by(&l),
        Some(LeaseRefusal::NoProgress {
            k: 3,
            last: Some(4)
        })
    );

    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.void(VoidBy::Interrupted, 1).expect("clock");
    l.result(WorkerId(0), t, d(1), 2).expect("clock");
    assert_eq!(
        l.counts().dropped,
        1,
        "the result crossing the cancel is dropped"
    );
    l.result(WorkerId(0), t, d(1), 3).expect("clock");
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Gone(Gone::Refused))
    );
}

/// A result that crosses `Cancel` is dropped, and the acknowledgement that follows retires the
/// worker: no drain timeout.
#[test]
fn a_result_crossing_a_cancel_then_its_ack_retires() {
    let t = token(1, 0, 1, 0);
    let mut l = ready(1);
    open(&mut l, 1, 1, 0);
    l.void(VoidBy::Interrupted, 1).expect("clock");
    l.result(WorkerId(0), t, d(1), 2).expect("clock");
    l.cancelled(WorkerId(0), t, 3).expect("clock");
    assert_eq!(
        l.worker(WorkerId(0)),
        Some(WorkerState::Gone(Gone::Retired))
    );
    assert_eq!(l.counts().cancelled, 1);
    assert_eq!(l.counts().lost, 0);
    assert!(l.quiescent());
}

/// The sender of every voiding frame is killed before the void cancels the rest, so it is sent
/// no `Cancel` and never drained.
#[test]
fn the_sender_of_a_voiding_frame_is_killed_not_drained() {
    type Call<'a> = (&'a str, &'a dyn Fn(&mut Ledger) -> Vec<Effect>);
    let calls: [Call<'_>; 5] = [
        ("NotHeld", &|l| {
            l.result(WorkerId(0), token(1, 1, 1, 1), d(1), 5)
                .expect("clock")
        }),
        ("NoProgress", &|l| {
            l.progress(WorkerId(0), token(1, 0, 1, 0), 17, 5)
                .expect("clock")
        }),
        ("Unasked", &|l| {
            l.cancelled(WorkerId(0), token(1, 0, 1, 0), 5)
                .expect("clock")
        }),
        ("SecondHello", &|l| {
            l.hello(WorkerId(0), Ok(()), 5).expect("clock")
        }),
        ("Caller", &|l| {
            l.void_from(WorkerId(0), VoidBy::Caller, 5).expect("clock")
        }),
    ];
    for (what, call) in calls {
        let mut l = ready(2);
        open(&mut l, 1, 2, 0);
        let e = call(&mut l);
        assert!(e.contains(&Effect::Kill(WorkerId(0))), "{what}");
        assert!(
            !e.contains(&Effect::Cancel(WorkerId(0), token(1, 0, 1, 0))),
            "{what}"
        );
        assert!(
            e.contains(&Effect::Cancel(WorkerId(1), token(1, 1, 1, 1))),
            "{what}"
        );
        assert_eq!(
            l.worker(WorkerId(0)),
            Some(WorkerState::Gone(Gone::Refused)),
            "{what}"
        );
        assert!(matches!(l.outcome(), Outcome::Void(_)), "{what}");
    }
}

/// `finish` completes only when every job is claimed and no worker drains, and changes nothing
/// otherwise.
#[test]
fn finish_completes_only_a_claimed_quiet_search() {
    let mut l = ready(1);
    open(&mut l, 1, 2, 0);
    let before = (l.workers().to_vec(), l.jobs().to_vec());
    l.finish(1).expect("clock");
    assert_eq!(l.outcome(), Outcome::Open);
    assert_eq!((l.workers().to_vec(), l.jobs().to_vec()), before);
}
