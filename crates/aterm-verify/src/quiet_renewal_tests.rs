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

/// `need` admitted at once on a machine every core of which is busy.
fn busy_claim(ledger: &Ledger, need: Need, mode: Admit) -> Result<Claim, Error> {
    crate::admit_in(
        ledger,
        &crate::timing_test_machine::Measured::busy(&Fixed::new(Reading::of_gib(24, 20, 14))),
        Pace::default(),
        need.patience(aterm_capacity::Patience {
            reserve: std::time::Duration::ZERO,
            start: std::time::Duration::ZERO,
            hold: aterm_capacity::QUIET_PATIENCE.hold,
        }),
        mode,
        &mut |_| {},
    )
}

/// A TIMED STAGE ON A MACHINE THAT STAYED BUSY THROUGH ITS PATIENCE RUNS, AND ITS TIME LINE SAYS
/// IT RAN WITHOUT A QUALIFIED QUIET ADMISSION (a busy machine never blocks a release: the row is
/// named, not counted red). NEGATIVE CONTROL: a strict need on the same machine is refused, and
/// the stage starts nothing.
#[test]
fn a_loaded_stage_admission_runs_the_stage_and_says_so() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let fresh = || {
        Mutex::new(Quiet {
            claim: None,
            left: 1,
            admit: unused_admit,
        })
    };
    let ran = std::cell::Cell::new(false);
    let ((), said) = quietly_with(
        &context(dir.path()),
        &fresh(),
        "loaded",
        || ran.set(true),
        Claim::timing_protection,
        |_, need, mode| busy_claim(&ledger, need, mode),
    )
    .unwrap();
    assert!(ran.get());
    let said = said.expect("the stage's time line");
    assert!(
        said.contains("acquired loaded timing reservation")
            && said.contains("ran without a qualified quiet admission"),
        "{said}"
    );
    let refused = quietly_with(
        &context(dir.path()),
        &fresh(),
        "strict",
        || panic!("a strict need on a busy machine starts nothing"),
        Claim::timing_protection,
        |_, mut need, mode| {
            need.at_bound = aterm_capacity::AtBound::Refuse;
            busy_claim(&ledger, need, mode)
        },
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("started nothing")),
        "{refused:?}"
    );
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
        Claim::timing_protection,
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
                c.timing_protection()?;
                if key(c) == id {
                    Err(TimingIssue::Expired)
                } else {
                    c.timing_protection()
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
            Claim::timing_protection,
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
        Claim::timing_protection,
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
                        let real = c.timing_protection()?;
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

/// A timed stage that panics passes its turn on the unwind (`sched::run_stages` re-raises a
/// stage's panic): the last one's turn lets the run's quiet claim go, on both the qualified path
/// and AppKit's `--admit now` window path, so the per-user ledger no longer holds it. NEGATIVE
/// CONTROL: while the stage ran, the ledger held its claim.
#[test]
fn a_panicking_timed_stage_lets_the_runs_quiet_claim_go() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let quiet = Mutex::new(Quiet {
        claim: None,
        left: 1,
        admit: unused_admit,
    });
    let held = std::cell::Cell::new(false);
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        quietly_with(
            &context(dir.path()),
            &quiet,
            "panics",
            || {
                held.set(crate::current_quiet_claim().is_some());
                panic!("a stage's panic")
            },
            Claim::timing_protection,
            |_, need, mode| claim(&ledger, need, mode),
        )
    }));
    assert!(panicked.is_err());
    assert!(held.get(), "the stage ran under the claim");
    let q = quiet.lock().unwrap();
    assert_eq!(q.left, 0, "the panicking stage passed its turn");
    assert!(q.claim.is_none(), "and the run's claim went with the last");
    drop(q);
    assert!(crate::current_quiet_claim().is_none());
    assert!(
        ledger.list().unwrap().is_empty(),
        "the claim left the ledger"
    );

    let machine = Fixed::new(Reading::of_gib(48, 40, 14));
    let ctx = Ctx::new(
        dir.path().to_owned(),
        Scope::crate_only("aterm-gui"),
        EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_quiet(Some(Admit::Now))
    .with_measure_artifact(Some(crate::MeasureArtifact {
        app: dir.path().join("fixture.app"),
        version: None,
        commit: None,
    }));
    let window = Ledger::at(dir.path().join("window-ledger"));
    let listed = || window.list().unwrap().len();
    let appkit = Mutex::new(Quiet {
        claim: None,
        left: 1,
        admit: unused_admit,
    });
    let excluded = std::cell::Cell::new(false);
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        crate::run_timed_with(
            &ctx,
            &appkit,
            crate::plan::StageId::RedrawConformance,
            "AppKit redraw",
            |need| aterm_capacity::admit::try_admit_with(&window, &machine, need),
            || -> Result<(), String> {
                excluded.set(listed() == 1);
                panic!("an AppKit stage's panic")
            },
        )
    }));
    assert!(panicked.is_err());
    assert!(
        excluded.get(),
        "CONTROL: the stage held the per-user window claim"
    );
    let q = appkit.lock().unwrap();
    assert_eq!(q.left, 0);
    assert!(q.claim.is_none());
    drop(q);
    assert_eq!(listed(), 0, "the window claim went with the panic");
}
