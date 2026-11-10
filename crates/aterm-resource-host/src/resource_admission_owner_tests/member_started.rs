// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual Borrowed custody and durable journal, with the original target held.
//! This tests recovery of Started intent, not an installed solver policy.
use super::*;

struct PendingStart(std::sync::Arc<std::sync::Mutex<Option<member::Started>>>);
impl member::MemberResources for PendingStart {
    fn configure(&mut self, command: &mut spawnfd::Command) -> io::Result<()> {
        command.footprint_limit(spawnfd::FootprintLimit::new(512)?);
        Ok(())
    }
    fn bind_child(&mut self, _: u32) -> io::Result<()> {
        Ok(())
    }
    fn poll_start(
        &mut self,
        child: &mut spawnfd::Child,
        _: &mut dyn FnMut() -> bool,
        commit: &mut dyn FnMut(
            &member::Started,
            Option<member::RequestedStartLimits>,
        ) -> io::Result<()>,
    ) -> io::Result<member::TargetStart> {
        let (monotonic_ns, unix_ns) = start::clocks()?;
        let birth = process::read_birth(child.id() as i32)
            .ok_or_else(|| io::Error::other("original held member disappeared"))?;
        let started = member::Started {
            pid: child.id(),
            birth: birth.0,
            monotonic_ns,
            unix_ns,
        };
        commit(
            &started,
            Some(member::RequestedStartLimits {
                units: 1,
                footprint_mib: 512,
            }),
        )?;
        *self.0.lock().unwrap() = Some(started);
        Ok(member::TargetStart::Pending { offered: false })
    }
}

#[test]
fn durable_started_intent_keeps_the_known_member_recovery_phase_and_original_claim() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::member_started::durable_started_intent_keeps_the_known_member_recovery_phase_and_original_claim",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut world = World::new();
    let (root, memory, root_events) = world.queue_sleep(HANG);
    world
        .owner
        .pending
        .front_mut()
        .unwrap()
        .events
        .account()
        .unwrap();
    world.owner.tick_queue();
    assert_eq!(read_event(&root_events, &root), Event::Queued);
    let Event::Started { pid, birth, .. } = read_event(&root_events, &root) else {
        panic!("actual root did not start")
    };
    let binding = Binding {
        session: root.session.clone(),
        attempt: [91; 16],
    };
    world.owner.journal.create(&binding).unwrap();
    world
        .owner
        .journal
        .update(
            &binding,
            "member_prepared",
            aterm_json::json!({"memory":memory.to_json()}),
        )
        .unwrap();
    let (channel, interest) = CtlStream::pair().unwrap();
    let events = job::Events::new(OwnedFd::from(channel), binding.clone()).unwrap();
    let selected = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut command = spawnfd::command("/usr/bin/true");
    command.start_suspended(true);
    let key = (root.session.clone(), root.attempt);
    let mut attempt = {
        let Owner {
            jobs,
            fence,
            debt,
            journal,
            ..
        } = &mut world.owner;
        let mut custody = member_custody::Borrowed {
            job: jobs.get_mut(&key).unwrap(),
            fence,
            debt,
            journal,
            peer: Peer { pid, birth },
            proof: None,
            member: binding.attempt,
            reserved_units: 1,
            loan: None,
        };
        let mut attempt = member::OwnedAttempt::launch(
            &mut custody,
            command,
            PendingStart(selected.clone()),
            events,
            binding.clone(),
            Some(HANG),
            || {},
        );
        assert!(attempt.poll(&mut custody, None).ready_to_start);
        let (pid, birth) = attempt.identity().unwrap();
        custody
            .journal
            .update(
                &binding,
                "member_bound",
                aterm_json::json!({"pid":pid,"birth":birth}),
            )
            .unwrap();
        let original_row = custody.journal.outstanding().unwrap();
        let (monotonic_ns, unix_ns) = start::clocks().unwrap();
        let candidate = member::Started {
            pid,
            birth,
            monotonic_ns,
            unix_ns,
        };
        for limits in [
            member::RequestedStartLimits {
                units: 2,
                footprint_mib: 1,
            },
            member::RequestedStartLimits {
                units: 1,
                footprint_mib: u32::try_from(MEMORY / (1024 * 1024) + 1).unwrap(),
            },
        ] {
            assert!(
                member::MemberCustody::commit_started(
                    &mut custody,
                    &binding,
                    &candidate,
                    Some(limits),
                )
                .is_err()
            );
            assert_eq!(custody.journal.outstanding().unwrap(), original_row);
            assert_eq!(attempt.identity(), Some((pid, birth)));
        }
        assert_eq!(
            attempt.start(&mut custody).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        attempt
    };
    let started = selected.lock().unwrap().clone().unwrap();
    let record = world
        .owner
        .journal
        .outstanding()
        .unwrap()
        .into_iter()
        .find(|record| journal::record_binding(record).unwrap() == binding)
        .unwrap();
    assert_eq!(record["phase"].as_str(), Some("member_running"));
    assert_eq!(record["pid"].as_u64(), Some(u64::from(started.pid)));
    assert_eq!(record["birth"].as_u64(), Some(started.birth));
    assert_eq!(record["monotonic_ns"].as_u64(), Some(started.monotonic_ns));
    assert_eq!(record["unix_ns"].as_u64(), Some(started.unix_ns));
    assert_eq!(record["requested_units"].as_u64(), Some(1));
    assert_eq!(record["requested_footprint_mib"].as_u64(), Some(512));
    world.owner.recover().unwrap();
    assert_eq!(world.owner.debt.reserved().unwrap(), 1);
    assert_ne!(
        world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
        CohortClaimState::Closed
    );
    assert_eq!(world.owner.ledger.list().unwrap().len(), 1);
    assert!(!world.owner.journal.fresh(&binding).unwrap());
    let deadline = Instant::now() + HANG;
    loop {
        let Owner {
            jobs,
            fence,
            debt,
            journal,
            ..
        } = &mut world.owner;
        let mut custody = member_custody::Borrowed {
            job: jobs.get_mut(&key).unwrap(),
            fence,
            debt,
            journal,
            peer: Peer { pid, birth },
            proof: None,
            member: binding.attempt,
            reserved_units: 1,
            loan: None,
        };
        if attempt
            .poll(&mut custody, Some(CompletionCause::Cancelled))
            .drained
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(POLL);
    }
    drop(interest);
    drop(root_events);
    world.settle();
    world.owner.recover().unwrap();
    world.assert_settled(&root, &memory, pid);
    assert!(world.owner.journal.outstanding().unwrap().is_empty());
}
