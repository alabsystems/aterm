// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Later-stage reservation renewal, distinct from each collector's postcompile entry.

use std::sync::{Arc, Mutex};

use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Reading, TimingIssue};

use crate::{Ctx, EnvSnapshot, Quiet, Scope, quietly_with};

fn unused_admit(_: Need, _: Admit) -> Result<Claim, Error> {
    panic!("fixture supplies its own ledger admission")
}

fn claim(ledger: &Ledger, need: Need, mode: Admit) -> Result<Claim, Error> {
    crate::admit_in(
        ledger,
        &crate::timing_test_machine::Measured::new(&Fixed::new(Reading::of_gib(24, 20, 14))),
        Pace::default(),
        need,
        mode,
        &mut |_| {},
    )
}

fn context(dir: &std::path::Path) -> Ctx {
    Ctx::new(
        dir.to_owned(),
        Scope::crate_only("aterm-verify"),
        EnvSnapshot::default(),
        dir.join("scratch"),
    )
    .with_quiet(Some(Admit::Wait))
}

fn key(c: &Claim) -> (u64, u64) {
    (c.record().start_ms, c.record().seq)
}

fn cached(c: Claim, left: usize) -> Mutex<Quiet> {
    Mutex::new(Quiet {
        claim: Some(Arc::new(c)),
        left,
        admit: unused_admit,
    })
}

#[test]
fn a_valid_cached_claim_is_reused_without_admission() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let old = claim(&ledger, Need::timing("old"), Admit::Wait).unwrap();
    let id = key(&old);
    let quiet = cached(old, 1);
    quietly_with(
        &context(dir.path()),
        &quiet,
        "reuse",
        || assert_eq!(key(&crate::current_quiet_claim().unwrap()), id),
        Claim::timing_admission,
        |_, _, _| panic!("valid cache must not readmit"),
    )
    .unwrap();
    assert!(quiet.lock().unwrap().claim.is_none());
    assert!(crate::current_quiet_claim().is_none());
}

#[test]
fn only_expiry_replaces_a_cached_claim_and_failed_replacement_starts_nothing() {
    for failure in ["ledger", "now", "unknown"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(dir.path().join("ledger"));
        let old = claim(&ledger, Need::timing("old"), Admit::Wait).unwrap();
        let id = key(&old);
        let quiet = cached(old, 2);
        let mut attempts = 0;
        let result = quietly_with(
            &context(dir.path()),
            &quiet,
            failure,
            || panic!("failed replacement must not start callback"),
            |c| {
                c.timing_admission()?;
                if key(c) == id {
                    Err(TimingIssue::Expired)
                } else {
                    c.timing_admission()
                }
            },
            |_, need, _| {
                attempts += 1;
                match failure {
                    "ledger" => Err(Error::Ledger(std::io::Error::other(
                        "injected write failure",
                    ))),
                    "now" => claim(&ledger, need, Admit::Now),
                    // An actual load-only claim has no measured Timing proof; do not invent
                    // an Unknown result on a valid new claim or a ready fake CPU observation.
                    _ => claim(&ledger, Need::new("unmeasured").quiet(100.0), Admit::Wait),
                }
            },
        );
        assert!(result.is_err(), "{failure}");
        assert_eq!(attempts, 1);
        let q = quiet.lock().unwrap();
        assert!(q.claim.is_none());
        assert_eq!(q.left, 1);
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let unknown = claim(&ledger, Need::new("load only").quiet(100.0), Admit::Wait).unwrap();
    let quiet = cached(unknown, 1);
    assert!(
        quietly_with(
            &context(dir.path()),
            &quiet,
            "unknown is not expiry",
            || panic!("unknown cache cannot qualify callback"),
            Claim::timing_admission,
            |_, _, _| panic!("unknown provenance must not be retried"),
        )
        .is_err()
    );
}

#[test]
fn concurrent_stages_replace_once_and_preserve_the_old_worker_share() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let old = claim(&ledger, Need::timing("old"), Admit::Wait).unwrap();
    let id = key(&old);
    let stamp = old.record().timing.admitted_ms;
    let quiet = cached(old, 3);
    let ctx = context(dir.path());
    let (entered, ready) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    // Run the actual bounded worker while the old claim is installed in stage TLS.
    quietly_with(
        &ctx,
        &quiet,
        "old worker stage",
        || {
            let walk = move || {
                let held = crate::current_quiet_claim().unwrap();
                assert_eq!(key(&held), id);
                entered.send(()).unwrap();
                resume.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
                assert_eq!(held.record().timing.admitted_ms, stamp);
                done.send(()).unwrap();
            };
            // The worker is intentionally left running after a real bounded call. Its
            // completion channel, not elapsed latency, controls the rest of the fixture.
            assert!(crate::exec::bounded(Some(std::time::Duration::ZERO), walk).is_err());
            ready.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
        },
        Claim::timing_admission,
        |_, _, _| panic!("valid old stage must reuse"),
    )
    .unwrap();
    let attempts = AtomicUsize::new(0);
    let (both, arrivals) = mpsc::channel();
    let (go, proceed) = mpsc::channel();
    let proceed = Mutex::new(proceed);
    let new_ids = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                quietly_with(
                    &ctx,
                    &quiet,
                    "new stage",
                    || {
                        new_ids
                            .lock()
                            .unwrap()
                            .push(key(&crate::current_quiet_claim().unwrap()));
                        both.send(()).unwrap();
                        proceed
                            .lock()
                            .unwrap()
                            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                            .unwrap();
                    },
                    |c| {
                        let real = c.timing_admission()?;
                        if key(c) == id {
                            Err(TimingIssue::Expired)
                        } else {
                            Ok(real)
                        }
                    },
                    |_, need, mode| {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        claim(&ledger, need, mode)
                    },
                )
                .unwrap();
            });
        }
        for _ in 0..2 {
            arrivals
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        }
        for _ in 0..2 {
            go.send(()).unwrap();
        }
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    let ids = new_ids.into_inner().unwrap();
    assert_eq!(ids[0], ids[1]);
    assert_ne!(ids[0], id);
    assert!(quiet.lock().unwrap().claim.is_none());
    assert!(
        ledger
            .list()
            .unwrap()
            .iter()
            .any(|r| (r.start_ms, r.seq) == id)
    );
    release.send(()).unwrap();
    finished
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "old worker releases original claim",
        || {
            (!ledger
                .list()
                .unwrap()
                .iter()
                .any(|r| (r.start_ms, r.seq) == id))
            .then_some(())
        },
    );
}
