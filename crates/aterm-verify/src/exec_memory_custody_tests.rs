// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual stage admission remains held by unfinished direct and pool-nested bounded workers.

use super::{Unfinished, bounded_wait};
use aterm_capacity::{Admit, Claim, Error, Fixed, Ledger, Need, Pace, Polled, Reading, Waiter};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, mpsc};

const GIB: u64 = 1 << 30;
const HANG: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;
static LEDGER: OnceLock<PathBuf> = OnceLock::new();
static WORK: Mutex<Vec<Work>> = Mutex::new(Vec::new());

struct ThreadEnd(mpsc::Sender<()>);

impl Drop for ThreadEnd {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

thread_local! {
    // Thread teardown follows the bounded closure's stack guards, so this observes the
    // worker's reservation share ending rather than its callback merely sending a result.
    static THREAD_END: std::cell::RefCell<Option<ThreadEnd>> = const { std::cell::RefCell::new(None) };
}

struct Work {
    enter: mpsc::Sender<()>,
    ready: mpsc::Receiver<()>,
    resume: mpsc::Receiver<()>,
    finish: mpsc::Sender<()>,
}

fn machine() -> Fixed {
    Fixed::new(Reading::of_gib(24, 20, 14))
}

fn admit(need: Need, mode: Admit) -> Result<Claim, Error> {
    crate::admit_in(
        &Ledger::at(LEDGER.get().unwrap()),
        &machine(),
        Pace::default(),
        need,
        mode,
        &mut |_| {},
    )
}

fn leave_running(work: Work) {
    let Work {
        enter,
        ready,
        resume,
        finish,
    } = work;
    let result = bounded_wait(
        move || {
            THREAD_END.with(|end| end.replace(Some(ThreadEnd(finish))));
            enter.send(()).unwrap();
            resume.recv_timeout(HANG).unwrap();
        },
        |_| {
            ready.recv_timeout(HANG).unwrap();
            Err(Unfinished::TimedOut(HANG))
        },
    );
    assert_eq!(result, Err(Unfinished::TimedOut(HANG)));
}

fn stage(_: &crate::Ctx, spec: &crate::plan::StageSpec) -> crate::ladder::Report {
    let mut work = std::mem::take(&mut *WORK.lock().unwrap()).into_iter();
    leave_running(work.next().unwrap());
    let nested = Mutex::new(Some(work.next().unwrap()));
    crate::testrun::dispatch_each(1, |_| leave_running(nested.lock().unwrap().take().unwrap()));
    assert!(work.next().is_none());
    let mut report = crate::ladder::Report::new(&spec.title);
    report.cannot_run("the bounded workers did not finish");
    report
}

fn held(ledger: &Ledger) -> bool {
    let held: Vec<_> = ledger
        .list()
        .unwrap()
        .into_iter()
        .filter(|record| record.held.is_some())
        .collect();
    held.len() == 1 && held[0].need.mem == 8 * GIB
}

fn admits_competing_work(ledger: &Ledger) -> bool {
    let machine = machine();
    matches!(
        Waiter::enter(
            ledger,
            &machine,
            Need::new("competing memory").mem(16 * GIB)
        )
        .unwrap()
        .poll(&machine),
        Polled::Admitted(_)
    )
}

#[test]
fn stage_memory_survives_until_direct_and_pool_nested_workers_both_end() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger");
    LEDGER.set(path.clone()).unwrap();
    let ledger = Ledger::at(path);
    let mut gates = Vec::new();
    for _ in 0..2 {
        let (enter, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (finish, done) = mpsc::channel();
        WORK.lock().unwrap().push(Work {
            enter,
            ready,
            resume,
            finish,
        });
        gates.push((release, done));
    }
    let ctx = crate::Ctx::new(
        dir.path().to_owned(),
        crate::Scope::crate_only("aterm-verify"),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_memory(crate::memory::Book::at(
        None,
        Some(24 * GIB),
        Some(43 * GIB / 2),
    ));
    let mut spec = crate::plan::declared(&ctx.scope)
        .into_iter()
        .find(|spec| spec.id == crate::plan::StageId::Tippy)
        .unwrap();
    spec.mem = 8 * GIB;
    let quiet = Mutex::new(crate::Quiet {
        claim: None,
        left: 1,
        admit,
    });
    let early = crate::EarlyClaims::default();
    let (report, ..) = crate::staged(
        &ctx,
        &quiet,
        &spec,
        None,
        crate::Seams {
            stage,
            prepare_cells: None,
            prepare_build_dir: |_| Ok(()),
            admit_quiet: admit,
            admit_memory: admit,
        },
        &early,
        || stage(&ctx, &spec),
    );
    let held_after_stage = held(&ledger);
    let blocked_after_stage = !admits_competing_work(&ledger);
    let mut gates = gates.into_iter();
    let (release, done) = gates.next().unwrap();
    release.send(()).unwrap();
    done.recv_timeout(HANG).unwrap();
    let held_for_nested = held(&ledger);
    let blocked_for_nested = !admits_competing_work(&ledger);
    let (release, done) = gates.next().unwrap();
    release.send(()).unwrap();
    done.recv_timeout(HANG).unwrap();
    aterm_spec::wait::until_within(HANG, "every worker released its stage claim", || {
        ledger.list().unwrap().is_empty().then_some(())
    });
    assert!(
        admits_competing_work(&ledger),
        "finished work returns its capacity"
    );
    assert!(report.outcomes().any(|(outcome, _)| outcome
        == crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun)));
    assert!(
        held_after_stage,
        "stage return released unfinished workers' declared budget"
    );
    assert!(
        blocked_after_stage,
        "unfinished workers allowed competing memory admission"
    );
    assert!(
        held_for_nested,
        "the pool-nested worker lost its stage reservation"
    );
    assert!(
        blocked_for_nested,
        "the nested worker's memory was admitted twice"
    );
}
