// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) fn successful_producer(world: &Path, seat: &Seat<'_>) {
    let producer = crate::private_input::Producer::create(world, "returning-producer").unwrap();
    producer.check_current().unwrap();
    let destination = producer.path().to_path_buf();
    let mut owner = Owner::new(1, 32).unwrap();
    let mut target = command("/bin/sh");
    target.args(["-c", "printf materialized > \"$1/payload\"", "materialize"]);
    target.arg(&destination);
    let accepted = owner
        .submit_returning(held(world, seat), &mut target, Duration::ZERO, producer)
        .unwrap();
    assert!(accepted.publication.is_ok());
    let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut completed = loop {
        let mut done = owner
            .tick(&mut |_, _| Ok(JobControl::Wait), &mut |_| {})
            .unwrap();
        if let Some(done) = done.pop() {
            break done;
        }
        assert!(Instant::now() < by, "returning materialization hang");
        owner.wait(by).unwrap();
    };
    assert_eq!(completed.id.sequence(), accepted.attempt.sequence);
    assert!(completed.result.as_ref().unwrap().succeeded());
    // A wrong typed lookup must retain the same native capability and original
    // producer. There is no fallback to a pathname or deserialized lifecycle row.
    completed = completed.try_into_retained::<String>().unwrap_err();
    assert!(
        std::fs::read_to_string(world.join("returning-producer/.input-owner"))
            .unwrap()
            .contains("materializing")
    );
    let producer = completed
        .try_into_retained::<crate::private_input::Producer>()
        .unwrap();
    producer.check_current().unwrap();
    assert_eq!(producer.path(), destination);
    assert_eq!(
        std::fs::read(destination.join("payload")).unwrap(),
        b"materialized"
    );
    let input = producer.finish().unwrap();
    input.check_current().unwrap();
    input.retire().unwrap().check_current().unwrap();
}

pub(super) fn failed_result_cannot_be_rewritten(world: &Path, seat: &Seat<'_>) {
    let producer = crate::private_input::Producer::create(world, "failed-producer").unwrap();
    let mut jobs = Jobs::new(1).unwrap();
    let mut target = command("/bin/sh");
    target.args(["-c", "exit 7"]);
    let _failed_ticket = jobs
        .submit_returning(held(world, seat), &mut target, Duration::ZERO, producer)
        .unwrap();
    let mut done = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert!(!done.result.as_ref().unwrap().succeeded());
    // Same actual table, DIFFERENT successful job: its public status does not
    // authorize the failed materialization's original producer.
    let _success_ticket = jobs
        .submit_retained(
            held(world, seat),
            &mut command("/usr/bin/true"),
            Duration::ZERO,
            (),
        )
        .unwrap();
    let success = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert!(success.result.as_ref().unwrap().succeeded());
    assert_ne!(done.id, success.id);
    done.id = success.id;
    done.result = success.result;
    let refused = done
        .try_into_retained::<crate::private_input::Producer>()
        .err()
        .expect("a different job's status cannot authorize the original producer");
    assert!(
        std::fs::read_to_string(world.join("failed-producer/.input-owner"))
            .unwrap()
            .contains("materializing")
    );
    drop(refused);
    assert!(
        std::fs::read_to_string(world.join("failed-producer/.input-owner"))
            .unwrap()
            .contains("quarantined")
    );
}

pub(super) fn launch_error_keeps_original_until_completion(world: &Path, seat: &Seat<'_>) {
    let (resource, drops) = guard();
    let mut jobs = Jobs::new(1).unwrap();
    jobs.submit_returning(
        held(world, seat),
        &mut command(world.join("missing-return-target")),
        Duration::ZERO,
        resource,
    )
    .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let completed = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert!(completed.result.is_err());
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "completion owns the same failed bundle"
    );
    let refused = completed
        .try_into_retained::<Guard>()
        .err()
        .expect("launch failure cannot authorize a typed return");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(refused);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

pub(super) fn surviving_member_prevents_return(world: &Path, seat: &Seat<'_>) {
    let (resource, drops) = guard();
    let mut jobs = Jobs::new(1).unwrap();
    let mut target = command("/bin/cat");
    target.stdin(Stdio::piped()).stdout(Stdio::piped());
    let ticket = jobs
        .submit_returning(held(world, seat), &mut target, Duration::ZERO, resource)
        .unwrap();
    assert!(until(&mut jobs, |jobs| !jobs.jobs[&ticket.id.sequence].starting).is_empty());
    let job = jobs.jobs.get_mut(&ticket.id.sequence).unwrap();
    let input = aterm_uds::deadline::cat_echoes(job.owned.as_mut().unwrap().child_mut());
    let SupervisedChild::Bound(bound) = job.owned.as_mut().unwrap() else {
        panic!("actual bound returning root");
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
    assert!(jobs.snapshot().unwrap()[0].retained_resources);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "positive root reap supplies no producer return"
    );
    drop(member_input);
    assert!(member.wait().unwrap().success());
    let completed = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "same original bundle stays in its completion"
    );
    let resource = completed.try_into_retained::<Guard>().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(resource);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

pub(super) fn foreign_completion_retains_resources(world: &Path, seat: &Seat<'_>) {
    let home = world.join("returning-cargo-home");
    std::fs::create_dir(&home).unwrap();
    let cache = crate::cargo_cache::SourceCacheGuard::try_acquire(&home).unwrap();
    let (resource, drops) = guard();
    let mut jobs = Jobs::new(1).unwrap();
    let _ticket = jobs
        .submit_returning(
            held(world, seat),
            &mut command("/usr/bin/true"),
            Duration::ZERO,
            (resource, cache),
        )
        .unwrap();
    let mut completed = until(&mut jobs, Jobs::is_empty).pop().unwrap();
    assert!(completed.result.as_ref().unwrap().succeeded());
    // Model the process check without forking a multithreaded Rust runtime.
    // The same actual native job already settled; the foreign copy must neither
    // recover its original bundle nor run a shared-description lock destructor.
    completed.returned.as_mut().unwrap().process = std::process::id().wrapping_add(1);
    let refused = completed
        .try_into_retained::<(Guard, crate::cargo_cache::SourceCacheGuard)>()
        .err()
        .expect("a foreign completion cannot recover its original resources");
    drop(refused);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".package-cache-mutate"))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "a foreign completion must retain the actual original Cargo source-cache lock"
    )]
    let blocked = contender.try_lock();
    assert!(matches!(blocked, Err(std::fs::TryLockError::WouldBlock)));
    // The isolated actor retains the modeled foreign descriptors until exit.
    // This is actual lock behavior plus process-identity injection, not a fork run.
}

pub(super) fn settlement_observer_failure_refuses_return(world: &Path, seat: &Seat<'_>) {
    use std::os::fd::AsFd as _;
    let (resource, drops) = guard();
    let mut jobs = Jobs::new(1).unwrap();
    let _ticket = jobs
        .submit_returning(
            held(world, seat),
            &mut command("/usr/bin/true"),
            Duration::ZERO,
            resource,
        )
        .unwrap();
    let (control, _send) = std::io::pipe().unwrap();
    let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let completed = loop {
        let mut completed = jobs
            .tick(
                &mut |_, event| {
                    if event == SupervisedEvent::CohortDrained {
                        Err(io::Error::other("native settlement observer refused"))
                    } else {
                        Ok(JobControl::Wait)
                    }
                },
                &mut |_| {},
            )
            .unwrap();
        if let Some(completed) = completed.pop() {
            break completed;
        }
        assert!(Instant::now() < by, "returning observer failure hang");
        jobs.wait(control.as_fd(), by).unwrap();
    };
    assert!(jobs.is_empty());
    assert!(
        completed
            .result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("native settlement observer refused")
    );
    let refused = completed
        .try_into_retained::<Guard>()
        .err()
        .expect("failed settlement observation cannot authorize a return");
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(refused);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
