// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_toolchain::record_lock::RecordLock;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use trust_resource_controller::{ContextQuery, JobBinding, runtime_input_retention as inputs};

pub(super) fn input(kind: inputs::Kind, path: &std::path::Path, logical: &[u8]) -> inputs::Input {
    let metadata = std::fs::metadata(path).unwrap();
    inputs::Input {
        kind,
        logical_path: logical.to_vec(),
        target: path.canonicalize().unwrap().as_os_str().as_bytes().to_vec(),
        alias: None,
        backing: trust_resource_controller::FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        size: (kind == inputs::Kind::RuntimeFile).then_some(metadata.len()),
        sha256: (kind == inputs::Kind::RuntimeFile)
            .then(|| aterm_digest::Sha256::digest(std::fs::read(path).unwrap())),
        executable: !kind.directory() && metadata.mode() & 0o111 != 0,
    }
}

#[test]
fn actual_preparation_and_received_output_lock_survive_root_reap_until_original_settlement() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::tests::runtime_inputs::actual_preparation_and_received_output_lock_survive_root_reap_until_original_settlement",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    for retain in [false, true] {
        let mut world = World::new();
        let runtime = world.scratch.path.join("runtime");
        let output = world.scratch.path.join("output");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::create_dir(&output).unwrap();
        let program = runtime.join("program");
        std::fs::write(&program, b"held input bytes").unwrap();
        let lock_path = output.join("lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .unwrap();
        let lock = RecordLock::try_lock(lock, 0).unwrap().into_close_only();
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        let (binding, memory, events) = world.queue_sleep(HANG);
        world
            .owner
            .pending
            .front_mut()
            .unwrap()
            .events
            .account()
            .unwrap();
        world.owner.tick_queue();
        assert_eq!(read_event(&events, &binding), Event::Queued);
        let Event::Started { pid, birth, .. } = read_event(&events, &binding) else {
            panic!("actual original root absent")
        };
        let key = (binding.session.clone(), binding.attempt);
        let capability = *world.owner.jobs[&key].delegation.capability.as_bytes();
        let request = inputs::Request {
            query: ContextQuery {
                binding: JobBinding {
                    session: binding.session.clone(),
                    root_attempt: binding.attempt,
                    capability,
                },
                call_attempt: [91; 16],
            },
            transfer: [92; 16],
            inputs: vec![
                input(inputs::Kind::RuntimeRoot, &runtime, b""),
                input(inputs::Kind::RuntimeFile, &program, b"program"),
                input(inputs::Kind::OutputRoot, &output, b""),
                input(inputs::Kind::OutputLock, &lock_path, b"lock"),
            ],
        };
        let peer = Peer { pid, birth };
        let mut foreign = request.clone();
        foreign.query.binding.capability[0] ^= 1;
        assert!(world.owner.runtime_inputs_begin(peer, foreign).is_err());
        let mut preparing = Some(
            world
                .owner
                .runtime_inputs_begin(peer, request.clone())
                .unwrap(),
        );
        let original_context = preparing.as_ref().unwrap().context().clone();
        assert!(
            original_context.context.sdk.is_none(),
            "inputs invented an SDK"
        );
        assert!(
            world.owner.runtime_inputs_begin(peer, request).is_err(),
            "transfer replay accepted"
        );
        let (transport, remote) = CtlStream::pair().unwrap();
        if retain {
            let descriptions: Vec<OwnedFd> = vec![
                File::open(&runtime).unwrap().into(),
                File::open(&program).unwrap().into(),
                File::open(&output).unwrap().into(),
                lock.try_clone_fd().unwrap(),
            ];
            let retained_context = world
                .owner
                .runtime_inputs_retain(
                    preparing.as_mut().unwrap().attachment().unwrap(),
                    descriptions,
                    transport.into(),
                )
                .unwrap();
            assert_eq!(retained_context, original_context);
            assert!(preparing.as_mut().unwrap().attachment().is_err());
            // The actual actor may finish after its RPC receiver timed out.
            // Dropping that reply cannot end the still-local handler owner.
            let (reply, receiver) = std::sync::mpsc::sync_channel(1);
            drop(receiver);
            assert!(
                reply
                    .try_send(Ok::<_, io::Error>(retained_context))
                    .is_err()
            );
            drop(lock);
            assert!(
                matches!(
                    RecordLock::try_lock(contender.try_clone().unwrap(), 0),
                    Err(std::fs::TryLockError::WouldBlock)
                ),
                "retaining inputs unlocked the producer's actual OFD"
            );
        } else {
            drop(lock);
        }
        world.owner.jobs.get_mut(&key).unwrap().settlement_at = Some(Instant::now() + HANG);
        wire::send_cancel(
            &events,
            aterm_types::resource_admission::Cancel {
                binding: binding.clone(),
            },
            Instant::now() + HANG,
        )
        .unwrap();
        let deadline = Instant::now() + HANG;
        while !world.owner.jobs[&key].reaped {
            world.owner.tick();
            assert!(Instant::now() < deadline, "original root failed to reap");
            std::thread::sleep(POLL);
        }
        let Owner {
            jobs,
            fence,
            journal,
            ..
        } = &mut world.owner;
        let job = jobs.get_mut(&key).unwrap();
        assert!(job.closed.is_none(), "root reap fabricated closure");
        if retain {
            assert!(
                matches!(
                    RecordLock::try_lock(contender.try_clone().unwrap(), 0),
                    Err(std::fs::TryLockError::WouldBlock)
                ),
                "root reap released original input custody"
            );
        }
        assert!(
            settlement::settle(job, fence, journal).is_err(),
            "live receive or final response worker was ignored"
        );
        assert!(job.closed.is_none());
        drop(preparing.take());
        settlement::settle(job, fence, journal).unwrap();
        assert!(job.is_settled());
        drop(
            RecordLock::try_lock(contender, 0)
                .expect("positive original settlement did not close the retained lock copy"),
        );
        drop(remote);
        world.settle();
        world.assert_settled(&binding, &memory, pid);
    }
}
