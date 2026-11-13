// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Real paid native Root and compiler query through the production Host router.
//! The closed stand-in exercises custody, never compilation or source closure.
use super::*;
use control::native_job as native;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

#[test]
fn actual_native_job_root_query_and_retirement_keep_original_owners() {
    for case in ["native-normal", "native-unknown", "native-cancel"] {
        run_case(case);
    }
}

pub(super) fn plan(value: &wire::WorkloadInvocation) -> native::Plan {
    let selected = PathBuf::from(std::ffi::OsStr::from_bytes(&value.cwd)).join("selected");
    fs::create_dir(&selected).unwrap();
    fs::write(
        selected.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    native::Plan {
        invocation: value.invocation,
        inputs: native::RootInputs {
            sdk_root: value.root.clone(),
            program: value.program.clone(),
            compiler: value.compiler.clone(),
            cwd: value.cwd.clone(),
            cargo_home: value.cargo_home.clone(),
            output_root: value.output_root.clone(),
            observed_host_store: value.observed_host_store.clone(),
            environment: vec![
                (b"LANG".to_vec(), b"C".to_vec()),
                (b"LC_ALL".to_vec(), b"C".to_vec()),
                (b"CARGO".to_vec(), value.program.path.clone()),
                (b"RUSTC".to_vec(), value.compiler.path.clone()),
                (b"CARGO_HOME".to_vec(), value.cargo_home.clone()),
                (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
            ],
        },
        sources: vec![native::Source {
            root: selected.as_os_str().as_bytes().to_vec(),
            manifest: input(&selected.join("Cargo.toml")),
            namespace_sha256: [21; 32],
        }],
        outputs: vec![native::Output {
            name: "fixture".into(),
            root: [value.output_root.as_slice(), b"/fixture"].concat(),
        }],
        nodes: vec![native::Node {
            name: "data-only-selection".into(),
            dependencies: vec![],
            timeout_ns: value.timeout_ns,
            kind: native::Kind::Compile {
                source: 0,
                output: 0,
                unit: native::UnitSelector {
                    package: "fixture".into(),
                    target: "fixture".into(),
                    kind: native::TargetKind::Binary,
                },
                mode: native::CompileMode::Build,
                features: vec![],
                frontend: native::Frontend::OriginalSdk,
            },
        }],
        timeout_ns: value.timeout_ns,
    }
}

pub(super) fn root() -> i32 {
    // Capture the actual inherited originals before any configuration/threads.
    let sdk = control::client::sdk::snapshot_sdk_lease().unwrap();
    let cpus = control::client::workload::snapshot_cpu_set().unwrap();
    let store = control::client::workload::snapshot_host_store().unwrap();
    unsafe { control::client::sdk::capture_sdk_lease(sdk).unwrap() };
    let _early_cpus = unsafe { control::client::workload::capture_cpu_set(cpus).unwrap() };
    let store = unsafe { control::client::workload::capture_host_store(store).unwrap() };
    let mut header = [0; 4];
    std::io::stdin().read_exact(&mut header).unwrap();
    let n = u32::from_be_bytes(header) as usize;
    assert!(n <= control::MAX_PAYLOAD);
    let mut raw = vec![0; n];
    std::io::stdin().read_exact(&mut raw).unwrap();
    let plan = native::Plan::decode(&raw).unwrap();
    assert!(wire::WorkloadInvocation::decode(&raw).is_err());
    let end = Instant::now() + HANG;
    let client = store.discover(end).unwrap();
    let actual_store =
        trust_resource_provenance::Store::open_at(client.original_store_path()).unwrap();
    let helper = std::sync::Arc::new(
        actual_store
            .connect_ancestor(end)
            .unwrap()
            .into_helper(end)
            .unwrap(),
    );
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let request = native::host::ReadyRequest {
        query: control::ContextQuery {
            binding: binding.clone(),
            call_attempt: [34; 16],
        },
        plan: plan.clone(),
    };
    // This stand-in speaks the exact production wire and owns actual received
    // descriptions; it does not construct Targo's opaque native Startup.
    let connection = helper.connect(end).unwrap();
    let socket = CtlStream::from(connection.stream().try_clone().unwrap());
    let mut channel = control::Channel::new(connection.stream().try_clone().unwrap()).unwrap();
    let hello = format!(
        "{{\"version\":3,\"role\":\"{}\",\"session\":\"{}\",\"capabilities\":{}}}",
        native::READY_ROLE,
        binding.session,
        native::host::READY_CAPABILITIES_JSON
    );
    channel.send(hello.as_bytes(), end).unwrap();
    channel.send(&request.encode().unwrap(), end).unwrap();
    let reply = native::host::ReadyReply::decode(&channel.receive(end).unwrap(), &request).unwrap();
    let digest = aterm_digest::Sha256::digest(plan.binding_preimage().unwrap());
    assert_eq!(reply.invocation_sha256, digest);
    assert_ne!(digest, aterm_digest::Sha256::digest(raw));
    assert_eq!(reply.cpu_set().count(), 2);
    control::client::sdk::validate_sdk_keeper(&reply.context.context).unwrap();
    let mut marker = [0; 2];
    let got = loop {
        match aterm_uds::fdpass::recv_with_fds(&socket, &mut marker, 2) {
            Ok(got) => break got,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                aterm_uds::ready::until(socket.as_fd(), aterm_uds::ready::Interest::Read, end)
                    .unwrap();
            }
            Err(e) => panic!("native originals: {e}"),
        }
    };
    assert_eq!(got.bytes, 1);
    assert_eq!(marker[0], host::CPU_SET_MARKER);
    assert_eq!(got.fds.len(), 2);
    for (observed, fd) in reply.cpu_set().zip(&got.fds) {
        let meta = aterm_uds::spawnfd::descriptor_metadata(fd.as_raw_fd()).unwrap();
        assert_eq!(
            (meta.dev(), meta.ino()),
            (observed.backing.device, observed.backing.inode)
        );
        let inherited =
            aterm_uds::spawnfd::descriptor_metadata(observed.inherited_fd as i32).unwrap();
        assert_eq!((meta.dev(), meta.ino()), (inherited.dev(), inherited.ino()));
    }
    connection.revalidate(end).unwrap();
    channel.send(&[1], end).unwrap();
    let source = PathBuf::from(std::ffi::OsStr::from_bytes(&plan.inputs.cwd));
    let case = fs::read_to_string(source.join("step")).unwrap();
    let output = PathBuf::from(std::ffi::OsStr::from_bytes(&plan.inputs.output_root));
    let lock_path = output.join(".cargo-lock");
    let original = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "close-only original fixture guard is transferred without unlocking its same OFD"
    )]
    original.lock().unwrap();
    let stat = original.metadata().unwrap();
    let offer = host::output::Request {
        key: host::output::Key {
            root_attempt: binding.root_attempt,
            invocation_sha256: digest,
            attempt: [37; 16],
        },
        output_root: plan.inputs.output_root.clone(),
        locks: vec![host::output::Lock {
            path: lock_path.as_os_str().as_bytes().to_vec(),
            backing: control::FileIdentity {
                device: stat.dev(),
                inode: stat.ino(),
            },
        }],
    };
    channel.send(&offer.encode().unwrap(), end).unwrap();
    jobserver_utils::descriptor_transfer::send(
        &socket,
        &[original.as_fd()],
        host::output::MARKER,
        end,
    )
    .unwrap();
    assert!(matches!(
        host::output::Reply::decode(&channel.receive(end).unwrap(), &offer).unwrap(),
        host::output::Reply::Retained { .. }
    ));
    drop(original);
    let query_cpu = got.fds[0].try_clone().unwrap();
    let query = native::query::CompilerQuery {
        member: control::Request {
            binding: binding.clone(),
            member_attempt: [42; 16],
            role: control::Role::WorkloadQuery,
            command: control::CommandSpec {
                program: plan.inputs.compiler.path.clone(),
                arg0: None,
                args: vec![b"-vV".to_vec()],
                environment: plan.inputs.environment.clone(),
                cwd: plan.sources[0].root.clone(),
            },
            delegations: control::Delegations {
                cpu: Some(control::Descriptor {
                    source: query_cpu.as_raw_fd() as u32,
                    target: reply.primary.inherited_fd,
                }),
                gnu: control::GnuTransport::None,
                fences: vec![],
            },
            timeout_ns: Some(plan.timeout_ns.min(host::MAX_QUERY_NS)),
        },
        plan_sha256: digest,
        sdk_root: plan.inputs.sdk_root.clone(),
        compiler: plan.inputs.compiler.clone(),
    };
    assert_ne!(query.member.command.cwd, plan.inputs.cwd);
    let mut wrong = query.clone();
    wrong.plan_sha256[0] ^= 1;
    wrong.member.member_attempt = [43; 16];
    let wrong_cpu = got.fds[0].try_clone().unwrap();
    wrong.member.delegations.cpu.as_mut().unwrap().source = wrong_cpu.as_raw_fd() as u32;
    let mismatched = client.native_job_query(
        &wrong,
        control::client::MetadataIo {
            descriptions: vec![wrong_cpu],
        },
        end,
    );
    assert!(
        matches!(mismatched, Err(control::client::MetadataError::Refused)),
        "mismatched native plan must be refused before attachment: {:?}",
        mismatched.as_ref().err(),
    );
    let mut active = client
        .native_job_query(
            &query,
            control::client::MetadataIo {
                descriptions: vec![query_cpu],
            },
            end,
        )
        .unwrap();
    // This closed stand-in is the original bank for exactly one query. It
    // transfers the SAME received description and keeps the original occupied
    // until the Host reports actual member retirement and returns that ticket.
    let mut loaned = false;
    let mut bank_sequence = 0;
    let result = loop {
        if channel.ready_to_receive().unwrap() {
            let request = host::bank::Request::decode(&channel.receive(end).unwrap()).unwrap();
            assert_eq!(request.key.root_attempt, binding.root_attempt);
            assert_eq!(request.key.invocation_sha256, digest);
            assert_eq!(request.key.sequence, bank_sequence + 1);
            bank_sequence = request.key.sequence;
            let decision = match request.operation {
                host::bank::Operation::Primary {
                    member_attempt,
                    slot,
                } => {
                    assert_eq!(member_attempt, query.member.member_attempt);
                    assert_eq!(slot, reply.primary.slot);
                    assert!(!loaned);
                    loaned = true;
                    host::bank::Decision::Granted {
                        member_attempt,
                        loan: [47; 16],
                        slot,
                        backing: reply.primary.backing,
                    }
                }
                host::bank::Operation::Returned {
                    member_attempt,
                    loan,
                    slot,
                } => {
                    assert_eq!(member_attempt, query.member.member_attempt);
                    assert_eq!(slot, reply.primary.slot);
                    assert_eq!(loan, [47; 16]);
                    assert!(loaned);
                    loaned = false;
                    host::bank::Decision::Returned
                }
                _ => panic!("closed one-query fixture cannot issue extra CPU"),
            };
            let granted = matches!(decision, host::bank::Decision::Granted { .. });
            channel
                .send(
                    &host::bank::Reply {
                        key: request.key,
                        decision,
                    }
                    .encode()
                    .unwrap(),
                    end,
                )
                .unwrap();
            if granted {
                jobserver_utils::descriptor_transfer::send(
                    &socket,
                    &[got.fds[0].as_fd()],
                    host::bank::ORIGINAL_MARKER,
                    end,
                )
                .unwrap();
            }
        }
        if let Some(result) = active.poll(end).unwrap() {
            break result;
        }
        std::thread::sleep(POLL);
    };
    assert!(!loaned);
    assert_eq!(bank_sequence, 2);
    assert!(
        matches!(
            result.completion.finished.status,
            control::Status::Finished {
                outcome: control::Outcome::Exit(0),
                ..
            }
        ),
        "actual native query result {result:?}"
    );
    let completed = active.native_job_completion().unwrap();
    assert_eq!(completed.invocation_sha256, digest);
    assert!(active.workload_completion().is_none());
    assert!(active.paired_completion().is_none());
    fs::write(
        source.join("original-lock-dropped"),
        "actual native output guard transferred",
    )
    .unwrap();
    fs::write(source.join("ready"), "actual native root and query ran").unwrap();
    if case.ends_with("unknown") {
        drop(channel);
        drop(socket);
        drop(connection);
        std::thread::sleep(HANG * 2);
        return 9;
    }
    if case.ends_with("cancel") {
        std::thread::sleep(HANG * 2);
        return 9;
    }
    std::io::stdin().read_exact(&mut header).unwrap();
    let mut ack = vec![0; u32::from_be_bytes(header) as usize];
    std::io::stdin().read_exact(&mut ack).unwrap();
    wire::Continuation::decode(&ack)
        .unwrap()
        .check_binding(digest, binding.root_attempt, 1, LOCK_CHECKPOINT)
        .unwrap();
    let drained = host::phase::Request {
        key: host::bank::Key {
            root_attempt: binding.root_attempt,
            invocation_sha256: digest,
            sequence: 1,
        },
        operation: host::phase::Operation::Drained,
    };
    channel.send(&drained.encode().unwrap(), end).unwrap();
    assert!(matches!(
        host::phase::Reply::decode(&channel.receive(end).unwrap(), &drained)
            .unwrap()
            .decision,
        host::phase::Decision::Drained
    ));
    drop(got);
    0
}
