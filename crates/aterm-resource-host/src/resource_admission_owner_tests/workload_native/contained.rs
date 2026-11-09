// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real private original root, output registration, target, native retirement
//! and opaque client owner. The stand-in is not a real Cargo UnitOutput claim.
use super::*;
use control::client::{ContainedProducedError, MetadataIo};
use host::{contained, produced};

pub(super) fn target() -> i32 {
    assert!(crate::metadata_prelude::no_fork(std::process::id()).unwrap());
    assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
    let fork = unsafe { libc::fork() };
    if fork == 0 {
        unsafe { libc::_exit(93) };
    }
    assert_eq!(fork, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
    println!("actual_contained_case: test");
    fs::write("contained-target-ran", b"actual target past GO").unwrap();
    while std::path::Path::new("hold-contained-target").exists() {
        std::thread::sleep(Duration::from_millis(2));
    }
    0
}

pub(super) struct Retained {
    _operations: Vec<control::client::CompletedProducedOperation>,
    _registrations: Vec<control::client::workload::RegisteredOutput>,
    _programs: Vec<trust_resource_provenance::program::produced::HeldProducedProgram>,
    _output: trust_resource_provenance::program::produced::HeldOutputRoot,
}

pub(super) fn run(
    client: &control::client::Client,
    channel: &mut control::client::workload::WorkloadControl,
    ready: &host::ReadyRequest,
    invocation_sha256: [u8; 32],
    gnu: control::FileIdentity,
    deadline: Instant,
    action_count: u8,
) -> Retained {
    use trust_resource_provenance::program::{HeldProgram, produced::HeldOutputRoot};
    let invocation = &ready.invocation;
    // A failed conversion returns this SAME actual accepted Host. It is not
    // reconstructed from its saved descriptor or process observation.
    let original = client.connect_original_host(deadline).unwrap();
    let fd = original.stream().as_raw_fd();
    let (original, error) = *original.into_helper_preserving(Instant::now()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(original.stream().as_raw_fd(), fd);
    original.revalidate(deadline).unwrap();
    let accepted = original.into_helper_preserving(deadline).unwrap();
    accepted.validate(deadline).unwrap();
    drop(accepted);
    let path = |bytes: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    let output = path(&invocation.output_root);
    let source = path(&invocation.cwd);
    let sdk = std::sync::Arc::new(
        HeldProgram::open(
            &path(&invocation.root),
            &path(&invocation.program.path),
            &invocation.program.sha256,
            deadline,
        )
        .unwrap(),
    );
    let output_owner = HeldOutputRoot::open(&output, deadline).unwrap();
    let executable_dir = output.join("debug");
    fs::create_dir(&executable_dir).unwrap();
    let mut programs = Vec::new();
    let mut registrations = Vec::new();
    // The original executable parent is already retained by the capture.
    // This static fixture has no additional non-system runtime directory.
    let runtime_roots = Vec::new();
    for target in 0..2 {
        fs::copy(
            path(&invocation.program.path),
            executable_dir.join(format!("contained-harness-{target}")),
        )
        .unwrap();
    }
    for target in 0..2 {
        let executable = executable_dir.join(format!("contained-harness-{target}"));
        let program = input(&executable);
        let held = output_owner
            .program(
                std::sync::Arc::clone(&sdk),
                &executable,
                &program.sha256,
                &runtime_roots,
                deadline,
            )
            .unwrap();
        let registration = produced::Registration {
            key: produced::Key {
                root_attempt: ready.query.binding.root_attempt,
                invocation_sha256,
                phase: [35; 16],
                ordinal: 1,
                output: [61 + target; 16],
            },
            program,
            runtime_roots: runtime_roots
                .iter()
                .map(|root| root.as_os_str().as_bytes().to_vec())
                .collect(),
            producer_attempt: None,
        };
        // SAFETY: this private fixture owns the original output directory BEFORE
        // materialization, its transferred close-only lock, actual held SDK and
        // readonly executable. It qualifies live native transport/custody only;
        // it does not impersonate a compiler attempt or real BuildRunner output.
        let registered = unsafe {
            channel.register_output(
                &registration,
                held.executable_descriptor(deadline).unwrap(),
                deadline,
            )
        }
        .unwrap();
        programs.push(held);
        registrations.push(registered);
    }
    let selected = |index: u8| {
        usize::from(if index <= 2 {
            index.saturating_sub(1)
        } else {
            (index - 3) % 2
        })
    };
    let request = |index: u8| {
        let registration = registrations[selected(index)].registration();
        let kind = if index <= 2 {
            host::MemberKind::Inventory
        } else {
            host::MemberKind::Sample
        };
        contained::Request {
            operation: [90 + index; 16],
            gnu,
            produced: produced::ProducedRequest {
                member: control::Request {
                    binding: ready.query.binding.clone(),
                    member_attempt: [80 + index; 16],
                    role: control::Role::WorkloadProduced,
                    command: control::CommandSpec {
                        program: registration.program.path.clone(),
                        arg0: None,
                        args: vec![if kind == host::MemberKind::Inventory {
                            b"--list".to_vec()
                        } else {
                            b"--test-threads=1".to_vec()
                        }],
                        cwd: invocation.cwd.clone(),
                        environment: vec![
                            (b"LANG".to_vec(), b"C".to_vec()),
                            (b"LC_ALL".to_vec(), b"C".to_vec()),
                            (b"CARGO".to_vec(), invocation.program.path.clone()),
                            (b"RUSTC".to_vec(), invocation.compiler.path.clone()),
                            (b"CARGO_HOME".to_vec(), invocation.cargo_home.clone()),
                            (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
                            (
                                b"AY_PB_REFUTATION_PROOF_SRC".to_vec(),
                                invocation.refutation_step.path.clone(),
                            ),
                            (
                                b"AY_PB_REFUTATION_LOOP_PROOF_SRC".to_vec(),
                                invocation.refutation_loop.path.clone(),
                            ),
                            (
                                b"TRUST_SPEC_SRC".to_vec(),
                                invocation.trust_spec_source.path.clone(),
                            ),
                            (b"TRUST_VC_DISABLE_VC_CACHE".to_vec(), b"1".to_vec()),
                            (b"RUST_TEST_THREADS".to_vec(), b"1".to_vec()),
                            (
                                b"TRUST_TOOLCHAIN_BIN".to_vec(),
                                [invocation.root.as_slice(), b"/bin"].concat(),
                            ),
                        ],
                    },
                    delegations: control::Delegations {
                        cpu: None,
                        gnu: control::GnuTransport::None,
                        fences: Vec::new(),
                    },
                    timeout_ns: Some(invocation.timeout_ns),
                },
                key: registration.key.clone(),
                kind,
                program_sha256: registration.program.sha256,
                runtime_roots: registration.runtime_roots.clone(),
            },
        }
    };
    let mut swapped = request(0);
    swapped.gnu.inode += 1;
    assert!(
        matches!(
            client.workload_contained_produced(
                &registrations[0],
                &swapped,
                MetadataIo {
                    descriptions: Vec::new()
                },
                deadline
            ),
            Err(ContainedProducedError::Refused)
        ),
        "GNU substitution must refuse before attachment"
    );
    assert!(!source.join("contained-target-ran").exists());
    let mut completed_owners = Vec::new();
    let mut counts = File::create(source.join("contained-descriptors")).unwrap();
    let (original_limit, baseline) = descriptor_population();
    writeln!(
        counts,
        "contained root soft={original_limit} baseline={baseline}"
    )
    .unwrap();
    for index in 1..=action_count {
        let requested = request(index);
        let registered = &registrations[selected(index)];
        let marker = source.join("contained-target-ran");
        if marker.exists() {
            fs::remove_file(&marker).unwrap();
        }
        let held_case = index >= action_count - 1;
        if held_case {
            fs::write(
                source.join("hold-contained-target"),
                b"original cancellation owner",
            )
            .unwrap();
        }
        let mut member = client
            .workload_contained_produced(
                registered,
                &requested,
                MetadataIo {
                    descriptions: Vec::new(),
                },
                deadline,
            )
            .unwrap();
        if held_case {
            while !marker.is_file() {
                assert!(Instant::now() < deadline);
                assert!(
                    member.poll(deadline).unwrap().is_none(),
                    "held actual target must remain live"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            if index == action_count {
                drop(member);
                continue;
            }
            member.cancel(deadline).unwrap();
        }
        let mut operation = loop {
            if let Some(operation) = member.poll(deadline).unwrap() {
                break operation;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(operation.matches_registration(registered));
        assert_eq!(operation.request().operation, requested.operation);
        let actual = operation.take_observation().unwrap();
        assert!(
            actual
                .stdout
                .windows(b"actual_contained_case".len())
                .any(|word| word == b"actual_contained_case"),
            "contained action {index} produced no execution marker: status {:?}; stdout {:?}; stderr {:?}",
            actual.completion.completed.finished.status,
            String::from_utf8_lossy(&actual.stdout),
            String::from_utf8_lossy(&actual.stderr),
        );
        let control::Status::Finished { outcome, cause, .. } =
            actual.completion.completed.finished.status
        else {
            panic!("native terminal")
        };
        if !held_case {
            assert_eq!(outcome, control::Outcome::Exit(0));
            assert_eq!(cause, control::CompletionCause::Exited);
        } else {
            assert_eq!(cause, control::CompletionCause::Cancelled);
        }
        assert!(
            operation.take_observation().is_none(),
            "output data moves once"
        );
        assert!(
            member.poll(deadline).is_err(),
            "original operation cannot complete twice"
        );
        assert!(
            matches!(
                client.workload_contained_produced(
                    registered,
                    &requested,
                    MetadataIo {
                        descriptions: Vec::new()
                    },
                    deadline
                ),
                Err(ContainedProducedError::Refused)
            ),
            "completed member attempt cannot execute again"
        );
        completed_owners.push(operation);
        let (current_limit, current) = descriptor_population();
        writeln!(
            counts,
            "contained root action={index} retained={} descriptors={current}",
            completed_owners.len()
        )
        .unwrap();
        assert_eq!(
            current_limit, original_limit,
            "the actual original FD limit must not change"
        );
        assert!(
            current <= baseline + 1,
            "actually exhausted streams close while original accepted-derived owners remain"
        );
        if index == action_count - 1 {
            fs::remove_file(source.join("hold-contained-target")).unwrap();
        }
    }
    // The actual phase owner, SDK/output and all original CPU descriptions
    // remain live in root() through positive Leave and final root settlement.
    assert_eq!(completed_owners.len(), usize::from(action_count - 1));
    aterm_durable::durable(&counts).unwrap();
    for operation in &completed_owners {
        operation.validate(deadline).unwrap();
    }
    for held in &programs {
        held.validate(deadline).unwrap();
    }
    Retained {
        _operations: completed_owners,
        _registrations: registrations,
        _programs: programs,
        _output: output_owner,
    }
}
