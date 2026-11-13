// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual native custody, isolated from other tests' process-wide child policy.

use super::*;
use crate::build_admit::{self, Admission, Seat, service::Owner};
use crate::build_cache::Tool;
use aterm_capacity::{
    Admit, CohortClaimState, CohortClosureRef, Fixed, Ledger, Pace, Reading,
    cohort_namespace::Namespace,
};
use aterm_uds::spawnfd::{Stdio, command};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const ACTOR: &str = "ATPKG_RETAINED_OPERATION_CASE";

#[path = "build_admit_returning_tests.rs"]
mod returning_tests;

struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn guard() -> (Guard, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    (Guard(Arc::clone(&drops)), drops)
}

fn with_world(run: impl FnOnce(&Path, &Seat<'_>) -> usize) {
    use std::os::unix::fs::DirBuilderExt as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let home = world.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace = Namespace::create_private(world.path(), &home).unwrap();
    let mut reading = Reading::of_gib(256, 200, 128);
    reading.load5 = Some(128.0);
    let machine = Fixed::new(reading);
    let seat = Seat {
        ledger: Some(Ledger::at(namespace.fence())),
        machine: &machine,
        own: Some(world.path().join("memory.json")),
        verify: None,
        mode: Admit::Wait,
        pace: Pace::default(),
        pid: std::process::id(),
        marker: None,
    };
    let expected = run(world.path(), &seat);
    assert_eq!(
        seat.ledger.as_ref().unwrap().list().unwrap().len(),
        expected
    );
}

fn held(world: &Path, seat: &Seat<'_>) -> Held {
    let args = ["--unverified", "build"].map(str::to_owned);
    let build = build_admit::classify(&args, Tool::Targo).unwrap();
    match build_admit::admit_cohort_job(&build, world, seat, &mut |_| {}) {
        Admission::Claimed(held) => *held,
        other => panic!("private retained admission: {other:?}"),
    }
}

/// `Some` once the claim `reference` was retired by its owner's drop or the reaper it was
/// handed to, never by this look: whether it is still active is read without its record's
/// mutation lock, which a drop that meets it leaves held, and leaves the claim to a reaping
/// scan. A non-reaping read then confirms the closure from the archive, once the retiring
/// drop has let go of its archive slot.
fn retired(seat: &Seat<'_>, reference: &CohortClosureRef) -> Option<()> {
    let ledger = seat.ledger.as_ref().unwrap();
    if ledger.cohort_claim_listed(reference).unwrap() {
        return None;
    }
    match ledger.cohort_claim_state(reference) {
        Ok(state) => {
            assert_eq!(state, CohortClaimState::Closed);
            Some(())
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
        Err(error) => panic!("retired cohort claim's archive: {error}"),
    }
}

fn tick(jobs: &mut Jobs) -> Vec<Completion> {
    jobs.tick(&mut |_, _| Ok(JobControl::Wait), &mut |_| {})
        .unwrap()
}

fn until(jobs: &mut Jobs, found: impl Fn(&Jobs) -> bool) -> Vec<Completion> {
    use std::os::fd::AsFd as _;
    let (control, _send) = std::io::pipe().unwrap();
    let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut done = Vec::new();
    loop {
        done.extend(tick(jobs));
        if found(jobs) {
            return done;
        }
        assert!(Instant::now() < by, "retained operation hang");
        jobs.wait(control.as_fd(), by).unwrap();
    }
}

fn cat(
    jobs: &mut Jobs,
    world: &Path,
    seat: &Seat<'_>,
) -> (Ticket, std::process::ChildStdin, Arc<AtomicUsize>) {
    let (retained, drops) = guard();
    let mut command = command("/bin/cat");
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let ticket = jobs
        .submit_retained(held(world, seat), &mut command, Duration::ZERO, retained)
        .unwrap();
    assert!(until(jobs, |jobs| !jobs.jobs[&ticket.id.sequence].starting).is_empty());
    let input = aterm_uds::deadline::cat_echoes(
        jobs.jobs
            .get_mut(&ticket.id.sequence)
            .unwrap()
            .owned
            .as_mut()
            .unwrap()
            .child_mut(),
    );
    (ticket, input, drops)
}

fn preflight_and_launch_failure(world: &Path, seat: &Seat<'_>) {
    let mut jobs = Jobs::new(1).unwrap();
    let (retained, drops) = guard();
    jobs.owner = 0;
    let refusal = jobs
        .submit_retained(
            held(world, seat),
            &mut command("/bin/cat"),
            Duration::ZERO,
            retained,
        )
        .unwrap_err();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert!(jobs.is_empty());
    let (_, original, retained) = refusal.into_parts();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(retained);
    drop(original);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    jobs.owner = std::process::id();

    let (retained, drops) = guard();
    let mut missing = command(world.join("missing-target"));
    let ticket = jobs
        .submit_retained(held(world, seat), &mut missing, Duration::ZERO, retained)
        .unwrap();
    assert!(
        !missing.starts_suspended(),
        "restore caller's command configuration"
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "launch Err is an accepted owned attempt"
    );
    assert!(jobs.jobs[&ticket.id.sequence].owned.is_none());
    let done = tick(&mut jobs);
    assert_eq!(done.len(), 1);
    assert_eq!(
        done[0].result.as_ref().unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(jobs.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

fn installed_before_dispatch_unwind(world: &Path, seat: &Seat<'_>) {
    let mut jobs = Jobs::new(1).unwrap();
    let (retained, drops) = guard();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = jobs.submit_retained_with(
            held(world, seat),
            &mut command("/bin/cat"),
            Duration::ZERO,
            retained,
            |job, _| {
                assert!(job.retained.is_some() && job.held.is_some());
                panic!("dispatch boundary unwind before target birth");
            },
        );
    }));
    assert!(panicked.is_err());
    assert_eq!(jobs.len(), 1, "the owning entry survives dispatch unwind");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(tick(&mut jobs).len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

fn wait_and_settlement_failures(world: &Path, seat: &Seat<'_>) {
    let mut jobs = Jobs::new(1).unwrap();
    let (ticket, input, drops) = cat(&mut jobs, world, seat);
    let job = jobs.jobs.get_mut(&ticket.id.sequence).unwrap();
    assert!(!job.poll_retained_cleanup_with(|_| Ok(None)));
    assert!(!job.poll_retained_cleanup_with(|_| Err(io::Error::other("unknown owning wait"))));
    assert!(job.usage.is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(input);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "positive owning root reap",
        || {
            let usage = job
                .owned
                .as_mut()
                .unwrap()
                .child_mut()
                .try_wait_with_rusage()
                .unwrap();
            if let Some(usage) = usage {
                job.usage = Some(usage);
                Some(())
            } else {
                None
            }
        },
    );
    let now = Instant::now();
    assert!(!job.settle_retained_with(now, |_| Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "retained native cohort"
    ))));
    assert!(
        !job.settle_retained_with(now + SETTLEMENT_RETRY, |_| Err(io::Error::other(
            "unknown native cohort"
        )))
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "positive root reap alone cannot release guards"
    );
    assert!(
        job.settle_retained_with(now + SETTLEMENT_RETRY * 2, |held| held
            .claim
            .finish_cohort())
    );
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "settlement keeps resources until owner completion"
    );
    assert_eq!(tick(&mut jobs).len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

fn root_reaped_member_retained(world: &Path, seat: &Seat<'_>) {
    use std::os::fd::AsFd as _;
    let mut jobs = Jobs::new(1).unwrap();
    let (ticket, input, drops) = cat(&mut jobs, world, seat);
    let job = jobs.jobs.get_mut(&ticket.id.sequence).unwrap();
    let SupervisedChild::Bound(bound) = job.owned.as_mut().unwrap() else {
        panic!("bound actual root")
    };
    let mut member = command("/bin/cat");
    member.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut member = job
        .held
        .as_ref()
        .unwrap()
        .claim
        .launch_cohort_member(bound, &mut member)
        .unwrap();
    let member_input = aterm_uds::deadline::cat_echoes(&mut member);
    drop(input);
    assert!(until(&mut jobs, |jobs| jobs.snapshot().unwrap()[0].root_reaped).is_empty());
    let snapshot = jobs.snapshot().unwrap();
    assert!(snapshot[0].retained_resources && snapshot[0].settlement_failure.is_some());
    assert!(!jobs.is_empty());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    ticket
        .cancel
        .request(crate::build_admit::CancelRequest::Kill)
        .unwrap();
    let (control, _send) = std::io::pipe().unwrap();
    let readiness = jobs.wait(control.as_fd(), Instant::now()).unwrap();
    assert!(
        !readiness.jobs_hint,
        "late root control cannot busy-wake settlement"
    );
    assert!(
        jobs.jobs
            .get_mut(&ticket.id.sequence)
            .unwrap()
            .owned
            .as_mut()
            .unwrap()
            .request_group(CohortSignal::Kill)
            .is_err(),
        "no group authority after root reap"
    );
    drop(member_input);
    assert!(member.wait().unwrap().success());
    let done = until(&mut jobs, Jobs::is_empty);
    assert_eq!(done.len(), 1);
    assert!(done[0].result.as_ref().unwrap().succeeded());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

fn owner_and_background(world: &Path, seat: &Seat<'_>) {
    let mut owner = Owner::new(1, 16).unwrap();
    let (retained, drops) = guard();
    let mut target = command("/bin/cat");
    target.stdin(Stdio::null());
    let held = held(world, seat);
    let reference = held.claim.cohort_closure_ref().unwrap();
    let accepted = owner
        .submit_retained(held, &mut target, Duration::ZERO, retained)
        .unwrap();
    accepted.publication.unwrap();
    drop(owner.controller());
    drop(owner.observer());
    assert_eq!(owner.job_snapshot().unwrap().len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    // Drop transfers actual job, claim and guards together, not a raw PID.
    drop(owner);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "retained background settlement",
        || {
            (drops.load(Ordering::SeqCst) == 1)
                .then(|| retired(seat, &reference))
                .flatten()
        },
    );
}

fn uncertain_arm(world: &Path, seat: &Seat<'_>) {
    use std::os::fd::AsFd as _;
    let mut jobs = Jobs::new(1).unwrap();
    let (retained, drops) = guard();
    let mut unused = None;
    let ticket = jobs
        .submit_retained_with(
            held(world, seat),
            &mut command("/bin/cat"),
            Duration::ZERO,
            retained,
            |job, target| {
                job.dispatch_with(target, |held, _| {
                    unused = Some(held.claim.arm_cohort()?);
                    Err(io::Error::other("dispatch left original ARM uncertain"))
                })
            },
        )
        .unwrap();
    assert!(tick(&mut jobs).is_empty());
    let snapshot = jobs.snapshot().unwrap();
    assert_eq!(snapshot.len(), 1);
    assert!(!snapshot[0].root_reaped);
    assert!(
        snapshot[0]
            .failure
            .as_ref()
            .unwrap()
            .contains("original ARM uncertain")
    );
    assert!(snapshot[0].settlement_failure.is_some());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    ticket
        .cancel
        .request(crate::build_admit::CancelRequest::Kill)
        .unwrap();
    let (control, _send) = std::io::pipe().unwrap();
    assert!(
        !jobs
            .wait(control.as_fd(), Instant::now())
            .unwrap()
            .jobs_hint,
        "no-handle intents are not live cleanup authority"
    );
    // Only the actual unused birth attempt can supply this cancellation receipt.
    let proof = unused.take().unwrap().cancel().unwrap();
    jobs.jobs
        .get_mut(&ticket.id.sequence)
        .unwrap()
        .held
        .as_mut()
        .unwrap()
        .claim
        .cancel_cohort(proof)
        .unwrap();
    let done = until(&mut jobs, Jobs::is_empty);
    assert_eq!(done.len(), 1);
    assert!(
        done[0]
            .result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("original ARM uncertain")
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

fn foreign_owner_keeps_resources(world: &Path, seat: &Seat<'_>) {
    let mut jobs = Jobs::new(1).unwrap();
    let (retained, drops) = guard();
    jobs.submit_retained_with(
        held(world, seat),
        &mut command("/bin/cat"),
        Duration::ZERO,
        retained,
        |_, _| {},
    )
    .unwrap();
    // Model a forked process copy without forking this multithreaded test process.
    // The original owner, not the copy, must eventually settle its original job.
    jobs.owner = 0;
    drop(jobs);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    // The isolated actor intentionally retains this fake foreign copy until exit.
    // No target was launched, and its private claim must still be reserved here.
}

fn cache_guard_until_native_finish(world: &Path, seat: &Seat<'_>) {
    let home = world.join("cargo-home");
    std::fs::create_dir(&home).unwrap();
    let retained = crate::cargo_cache::SourceCacheGuard::try_acquire(&home).unwrap();
    let mut jobs = Jobs::new(1).unwrap();
    let mut target = command("/bin/cat");
    target.stdin(Stdio::piped()).stdout(Stdio::piped());
    let ticket = jobs
        .submit_retained(held(world, seat), &mut target, Duration::ZERO, retained)
        .unwrap();
    assert!(until(&mut jobs, |jobs| !jobs.jobs[&ticket.id.sequence].starting).is_empty());
    let input = aterm_uds::deadline::cat_echoes(
        jobs.jobs
            .get_mut(&ticket.id.sequence)
            .unwrap()
            .owned
            .as_mut()
            .unwrap()
            .child_mut(),
    );
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".package-cache-mutate"))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "independent actual Cargo mutate contender must remain excluded by the native retained operation"
    )]
    let blocked = contender.try_lock();
    assert!(matches!(blocked, Err(std::fs::TryLockError::WouldBlock)));
    drop(input);
    let done = until(&mut jobs, Jobs::is_empty);
    assert!(done[0].result.as_ref().unwrap().succeeded());
    #[expect(
        clippy::disallowed_methods,
        reason = "the actual native finish releases Cargo source-cache in-use custody"
    )]
    contender.try_lock().unwrap();
}

fn sdk_cache_bundle_until_reaper_finish(world: &Path, seat: &Seat<'_>) {
    let prefix = world.join("sdk-prefix");
    let subject = crate::lease::Subject::build("trust", 17).unwrap();
    let sdk_bin = subject.path(&prefix).join("bin");
    std::fs::create_dir_all(&sdk_bin).unwrap();
    // A private managed subject exercises real retention, not compiler or
    // installed-SDK qualification. Neither unmanaged nor best-effort is held.
    let sdk_lease = match crate::lease::hold_for(
        &prefix,
        &sdk_bin,
        "retained native SDK/cache bundle control",
        Duration::ZERO,
    ) {
        Ok(crate::lease::Hold::Held(lease)) => lease,
        other => panic!("private managed SDK lease refused: {other:?}"),
    };
    assert_eq!(sdk_lease.subject(), &subject);
    sdk_lease.check_current().unwrap();
    let home = world.join("cargo-home");
    std::fs::create_dir(&home).unwrap();
    let cache_guard = crate::cargo_cache::SourceCacheGuard::try_acquire(&home).unwrap();
    cache_guard.check_current().unwrap();
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".package-cache-mutate"))
        .unwrap();
    let assert_held = |claims| {
        match crate::lease::reclaim(&prefix, &subject) {
            crate::lease::Reclaim::Keep(crate::lease::Holders::Held(who)) => {
                assert!(!who.is_empty());
            }
            other => panic!("SDK reclaim did not observe the actual held lease: {other:?}"),
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "independent actual cache mutator must remain blocked by the retained bundle"
        )]
        let blocked = contender.try_lock();
        assert!(matches!(blocked, Err(std::fs::TryLockError::WouldBlock)));
        let observed = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "private cohort inventory observation",
            || match seat.ledger.as_ref().unwrap().list() {
                Ok(records) => Some(records.len()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
                Err(error) => panic!("private cohort inventory: {error}"),
            },
        );
        assert_eq!(observed, claims);
    };
    assert_held(0);

    let mut jobs = Jobs::new(1).unwrap();
    let mut target = command("/bin/cat");
    target.stdin(Stdio::piped()).stdout(Stdio::piped());
    let ticket = jobs
        .submit_retained(
            held(world, seat),
            &mut target,
            Duration::ZERO,
            (sdk_lease, cache_guard),
        )
        .unwrap();
    assert!(until(&mut jobs, |jobs| !jobs.jobs[&ticket.id.sequence].starting).is_empty());
    let job = jobs.jobs.get_mut(&ticket.id.sequence).unwrap();
    let input = aterm_uds::deadline::cat_echoes(job.owned.as_mut().unwrap().child_mut());
    let SupervisedChild::Bound(bound) = job.owned.as_mut().unwrap() else {
        panic!("bound actual root")
    };
    let mut member = command("/bin/cat");
    member.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut member = job
        .held
        .as_ref()
        .unwrap()
        .claim
        .launch_cohort_member(bound, &mut member)
        .unwrap();
    let member_input = aterm_uds::deadline::cat_echoes(&mut member);
    drop(input);
    assert!(until(&mut jobs, |jobs| jobs.snapshot().unwrap()[0].root_reaped).is_empty());
    assert!(
        jobs.jobs[&ticket.id.sequence]
            .usage
            .as_ref()
            .unwrap()
            .status
            .success()
    );
    let snapshot = jobs.snapshot().unwrap();
    assert!(snapshot[0].retained_resources && snapshot[0].settlement_failure.is_some());
    assert_held(1);

    let reference = jobs.jobs[&ticket.id.sequence]
        .held
        .as_ref()
        .unwrap()
        .claim
        .cohort_closure_ref()
        .unwrap();
    // The root is already positively reaped. Drop must transfer the actual
    // member claim and both guards to the existing reaper, not unlock either.
    drop(jobs);
    assert_held(1);
    drop(member_input);
    assert!(member.wait().unwrap().success());
    let reclaimed = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "native SDK/cache bundle and cohort settlement",
        || {
            retired(seat, &reference)?;
            let crate::lease::Reclaim::Clear(reclaimed) = crate::lease::reclaim(&prefix, &subject)
            else {
                return None;
            };
            #[expect(
                clippy::disallowed_methods,
                reason = "actual native settlement must release the cache lock as well as the SDK lease"
            )]
            let released = contender.try_lock();
            match released {
                Ok(()) => Some(reclaimed),
                Err(std::fs::TryLockError::WouldBlock) => None,
                Err(error) => panic!("cache mutator after native settlement: {error}"),
            }
        },
    );
    drop(reclaimed);
}

fn private_input_bundle_before_birth_and_until_native_finish(world: &Path, seat: &Seat<'_>) {
    use crate::{build_admit::service::PreparationInputs, private_input::Producer};
    use std::io::Write as _;

    let lost = Producer::create(world, "lost-before-dispatch")
        .unwrap()
        .finish()
        .unwrap();
    let unused_home = world.join("refused-input-cargo-home");
    std::fs::create_dir(&unused_home).unwrap();
    let unused = PreparationInputs::new(
        vec![lost.lend().unwrap()],
        crate::cargo_cache::SourceCacheGuard::try_acquire(&unused_home).unwrap(),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    drop(lost);
    let mut before_birth = Owner::new(1, 16).unwrap();
    let marker = world.join("refused-child-must-not-run");
    let mut refused_target = command("/usr/bin/touch");
    refused_target.arg(&marker);
    let refusal = before_birth
        .submit_retained_inputs(
            held(world, seat),
            &mut refused_target,
            Duration::ZERO,
            unused,
        )
        .expect_err("lost original guard refuses before child birth");
    assert!(before_birth.is_empty());
    assert!(!marker.exists());
    drop(refusal.into_parts());

    let input_owner = Producer::create(world, "new-materialization")
        .unwrap()
        .finish()
        .unwrap();
    let home = world.join("private-input-cargo-home");
    std::fs::create_dir(&home).unwrap();
    let cache = crate::cargo_cache::SourceCacheGuard::try_acquire(&home).unwrap();
    let expired = PreparationInputs::new(vec![input_owner.lend().unwrap()], cache, Instant::now())
        .err()
        .expect("expired endpoint refuses before any dispatch");
    assert_eq!(expired.error().kind(), io::ErrorKind::TimedOut);
    let (_, (inputs, cache)) = expired.into_parts();
    let bundle = PreparationInputs::new(
        inputs,
        cache,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let (reader, mut writer) = std::io::pipe().unwrap();
    let output = world.join("actual-private-input-child.stdout");
    let mut target = command("/bin/cat");
    target
        .stdin(reader)
        .stdout(std::fs::File::create(&output).unwrap());
    let held = held(world, seat);
    let reference = held.claim.cohort_closure_ref().unwrap();
    let mut owner = Owner::new(1, 16).unwrap();
    owner
        .submit_retained_inputs(held, &mut target, Duration::ZERO, bundle)
        .unwrap()
        .publication
        .unwrap();
    drop(target);
    writer.write_all(b"actual-child-ran").unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "actual input user startup",
        || {
            assert!(
                owner
                    .tick(&mut |_, _| Ok(JobControl::Wait), &mut |_| {})
                    .unwrap()
                    .is_empty()
            );
            (std::fs::read(&output).unwrap() == b"actual-child-ran").then_some(())
        },
    );
    assert!(
        input_owner.retire().is_err(),
        "retained native use prevents local retirement"
    );
    // The failed consuming retirement above drops its returned original Owner:
    // the input is now quarantined, while the accepted Use keeps its actual lock.
    let container = world.join("new-materialization");
    assert!(
        std::fs::read_to_string(container.join(".input-owner"))
            .unwrap()
            .contains("quarantined")
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(container.join(".input-owner.lock"))
        .unwrap();
    let mutate = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".package-cache-mutate"))
        .unwrap();
    assert!(matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(lock.try_clone().unwrap(), 0),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert!(matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(mutate.try_clone().unwrap(), 0),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(owner); // Existing in-process reaper receives the same actual job+bundle.
    // Owner drop may already cancel and settle this actual cat. An available
    // input lock is valid only with positive original cohort settlement; never
    // assume a fixed cleanup latency or demand an artificially pending child.
    match aterm_toolchain::record_lock::RecordLock::try_lock(lock.try_clone().unwrap(), 0) {
        Ok(_released) => assert_eq!(
            seat.ledger
                .as_ref()
                .unwrap()
                .cohort_claim_state(&reference)
                .unwrap(),
            CohortClaimState::Closed,
        ),
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(error) => panic!("original input lock observation: {error}"),
    }
    drop(writer);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "actual input cohort settlement",
        || {
            retired(seat, &reference)?;
            let _input =
                aterm_toolchain::record_lock::RecordLock::try_lock(lock.try_clone().unwrap(), 0)
                    .ok()?;
            let _cache =
                aterm_toolchain::record_lock::RecordLock::try_lock(mutate.try_clone().unwrap(), 0)
                    .ok()?;
            Some(())
        },
    );
}

#[test]
fn retained_actor() {
    let Ok(case) = std::env::var(ACTOR) else {
        return;
    };
    with_world(|world, seat| {
        match case.as_str() {
            "return-producer" => returning_tests::successful_producer(world, seat),
            "return-failure" => returning_tests::failed_result_cannot_be_rewritten(world, seat),
            "return-launch" => {
                returning_tests::launch_error_keeps_original_until_completion(world, seat)
            }
            "return-member" => returning_tests::surviving_member_prevents_return(world, seat),
            "return-foreign" => returning_tests::foreign_completion_retains_resources(world, seat),
            "return-observer" => {
                returning_tests::settlement_observer_failure_refuses_return(world, seat)
            }
            "refusal" => preflight_and_launch_failure(world, seat),
            "unwind" => installed_before_dispatch_unwind(world, seat),
            "wait" => wait_and_settlement_failures(world, seat),
            "member" => root_reaped_member_retained(world, seat),
            "owner" => owner_and_background(world, seat),
            "arm" => uncertain_arm(world, seat),
            "cache" => cache_guard_until_native_finish(world, seat),
            "sdk-cache-member" => sdk_cache_bundle_until_reaper_finish(world, seat),
            "private-input-bundle" => {
                private_input_bundle_before_birth_and_until_native_finish(world, seat)
            }
            "foreign" => {
                foreign_owner_keeps_resources(world, seat);
                return 1;
            }
            _ => panic!("unknown retained actor case"),
        }
        0
    });
}

fn actor(case: &str) {
    let mut child = command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "build_admit::host::retained_tests::retained_actor",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ACTOR, case);
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn retained_preflight_returns_resources_and_launch_failure_keeps_its_ticket() {
    actor("refusal");
}

#[test]
fn returning_completion_hands_the_original_producer_to_its_postchecks() {
    actor("return-producer");
}

#[test]
fn returning_completion_refuses_a_different_successful_jobs_status() {
    actor("return-failure");
}

#[test]
fn returning_launch_failure_keeps_the_exact_bundle_in_its_completion() {
    actor("return-launch");
}

#[test]
fn returning_root_exit_keeps_the_original_bundle_until_its_member_settles() {
    actor("return-member");
}

#[test]
fn returning_foreign_completion_refuses_lookup_and_cannot_unlock_original_resources() {
    actor("return-foreign");
}

#[test]
fn returning_settlement_observer_failure_keeps_the_failed_bundle_until_drop() {
    actor("return-observer");
}
#[test]
fn retained_resources_are_installed_before_dispatch_unwinds() {
    actor("unwind");
}
#[test]
fn retained_wait_errors_and_unknown_settlement_never_release_resources() {
    actor("wait");
}
#[test]
fn retained_root_reap_keeps_resources_until_its_surviving_member_exits() {
    actor("member");
}
#[test]
fn retained_owner_drop_hands_actual_custody_to_the_existing_reaper() {
    actor("owner");
}
#[test]
fn retained_uncertain_arm_requires_its_original_cancellation_receipt() {
    actor("arm");
}
#[test]
fn retained_foreign_owner_copy_cannot_drop_original_resources() {
    actor("foreign");
}

#[test]
fn actual_cargo_cache_guard_stays_owned_until_native_finish() {
    actor("cache");
}

#[test]
fn actual_managed_sdk_and_cache_bundle_survives_root_reap_and_reaper_handoff() {
    actor("sdk-cache-member");
}

#[test]
fn actual_private_input_bundle_refuses_expired_preflight_and_survives_owner_loss() {
    actor("private-input-bundle");
}
