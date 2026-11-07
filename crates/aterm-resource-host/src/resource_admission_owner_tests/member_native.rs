// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real cached helper, private Owner/claim/pool, producer proof and named FIFO.
//! The paired native fixture image is a controlled test recipe, not a signed
//! production compiler SDK or a machine-wide activation qualification.
use super::*;
use aterm_capacity::host_executable::Store as ImageStore;
use aterm_ctl::resource_subgroup_client::acknowledge;
use aterm_uds::{CtlListener, deadline};
use std::{
    fs,
    io::{Read as _, Write as _},
    os::{
        fd::{AsFd as _, AsRawFd as _},
        unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    },
};
use trust_resource_controller::{
    self as control,
    client::{Client, MemberIo},
};
use trust_resource_provenance::Store as ReadStore;

const STORE: &str = "ATERM_MEMBER_FIXTURE_STORE";
const BASE: &str = "ATERM_MEMBER_FIXTURE_BASE";
const LEGACY: &str = "ATERM_MEMBER_FIXTURE_LEGACY";
const SDK: &str = "ATERM_MEMBER_FIXTURE_SDK";
const MARKER: &str = "ATERM_MEMBER_FIXTURE_MARKER";
const MODE: &str = "ATERM_MEMBER_FIXTURE_ACCOUNTING_MODE";

pub(super) fn run(args: &[OsString], gnu: Option<[OwnedFd; 2]>) -> std::process::ExitCode {
    if matches!(args, [arg] if arg == "-vV") {
        metadata(gnu, "trustc native fixture metadata");
    } else if matches!(args, [arg] if arg == "--print=sysroot" || arg == "--print=target-list") {
        assert!(gnu.is_none());
        metadata(None, args[0].to_str().unwrap());
    } else if matches!(args, [arg] if arg == "fixture-host" || arg == "fixture-host-unknown") {
        host(args[0] == "fixture-host-unknown");
    } else if matches!(args, [arg] if arg == "fixture-root") {
        root();
    } else {
        outer();
    }
    std::process::ExitCode::SUCCESS
}
fn outer() {
    crate::native_fixture::scoped_cpu_control();
    let scratch = aterm_tempfile::Builder::new()
        .prefix("member-")
        .suffix(".noindex")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let path = scratch.path().join("store");
    let store = ImageStore::prepare_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    for role in ["fixture-host", "fixture-host-unknown"] {
        let mut command = spawnfd::command(program.path());
        command.arg(role).env(STORE, &path);
        let result = deadline::output_within(&mut command, HANG);
        let output = match result {
            Ok(output) if output.status.success() => output,
            failure => {
                let retained = scratch.keep();
                panic!(
                    "actual member host {role}: {failure:?}; preserved {}",
                    retained.display()
                );
            }
        };
        assert!(
            output
                .stdout
                .windows(21)
                .any(|v| v == b"real-member-completed")
        );
    }
    for entry in fs::read_dir(path.join("images")).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    println!("actual native member custody and FIFO conservation passed");
}
fn host(unknown: bool) {
    crate::runtime::test_endpoint().unwrap();
    let path = PathBuf::from(std::env::var_os(STORE).unwrap());
    let store = ImageStore::open_at(&path).unwrap();
    let program = store.cache_current().unwrap();
    let reader = ReadStore::open_at(&path).unwrap();
    let native = std::sync::Arc::new(
        reader
            .pin_program(program.path(), Instant::now() + HANG)
            .unwrap(),
    );
    let socket = path.join(if unknown { "h-unknown.sock" } else { "h.sock" });
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).unwrap();
    aterm_uds::atomic_file::set_mode_nofollow(&socket, 0o600).unwrap();
    let _instance = store.publish(&program, &listener).unwrap();
    listener.set_nonblocking(true).unwrap();
    let telemetry = crate::resource_telemetry::Telemetry::new().unwrap();
    let mut world = World::with_scratch(Scratch::at(&path));
    let sdk = world.scratch.path.join("fixture-sdk");
    fs::create_dir_all(sdk.join("bin")).unwrap();
    for name in ["targo", "trustc"] {
        let target = sdk.join("bin").join(name);
        fs::copy(program.path(), &target).unwrap();
        fs::set_permissions(target, fs::Permissions::from_mode(0o555)).unwrap();
    }
    let subject = atpkg::lease::Subject::build("trust", 1234).unwrap();
    let sdk_lease = std::sync::Arc::new(
        atpkg::lease::take(
            &world.scratch.path,
            &subject,
            "actual native fixture SDK",
            Duration::ZERO,
        )
        .unwrap(),
    );
    let marker = world.scratch.path.join("member-marker");
    let (binding, memory, events) = world.queue_sleep(HANG);
    let root_log = world.scratch.path.join("root.stderr");
    let pending = world.owner.pending.back_mut().unwrap();
    pending.events.account().unwrap();
    pending.submission.fds.stderr = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&root_log)
        .unwrap()
        .into();
    let target = sdk.join("bin/targo");
    pending.submission.request.argv = vec![
        target.as_os_str().as_bytes().to_vec(),
        b"fixture-root".to_vec(),
    ];
    pending.recipe = Some(recipe::Recipe {
        kind: recipe::Kind::Targo,
        compiler: Some(compiler::Selection::NativeFixture {
            execution: sdk.clone(),
            build: 1234,
            lease: sdk_lease,
        }),
        program: target,
        arguments: vec!["fixture-root".into()],
        environment: vec![
            (STORE.into(), path.clone().into_os_string()),
            (
                BASE.into(),
                world.scratch.path.join("capacity").into_os_string(),
            ),
            (LEGACY.into(), world.scratch.legacy.clone().into_os_string()),
            (SDK.into(), sdk.into_os_string()),
            (MARKER.into(), marker.clone().into_os_string()),
        ],
        cwd: world.scratch.path.clone(),
        width: 1,
        metadata_sdk: None,
    });
    if unknown {
        pending
            .recipe
            .as_mut()
            .unwrap()
            .environment
            .push((MODE.into(), "unknown".into()));
    }
    assert_eq!(read_event(&events, &binding), Event::Queued);
    world.owner.tick_queue();
    assert!(matches!(
        read_event(&events, &binding),
        Event::Started { .. }
    ));
    let end = Instant::now() + HANG;
    let mut submissions = 0;
    let mut corrupt_after_retirement = None;
    while !world.owner.idle() {
        world.owner.tick();
        if let Some((attempt, _)) = &corrupt_after_retirement
            && world
                .owner
                .jobs
                .get(&(binding.session.clone(), binding.attempt))
                .is_some_and(|job| !job.members.contains_key(attempt))
        {
            // The original target has positively retired and its original
            // resources returned. Corrupt only this held test observer's
            // transport tail, so a known target cannot become measured success.
            let (_, stream) = corrupt_after_retirement.take().unwrap();
            wire::write_all(&stream, &[0], end).unwrap();
            drop(stream);
        }
        assert!(Instant::now() < end, "actual member host failed to drain");
        match aterm_uds::kept_out(|| listener.accept()) {
            Ok((stream, _)) => {
                let peer = match wire::local_peer(&stream) {
                    Ok(peer) => peer,
                    Err(error) if error.kind() == io::ErrorKind::NotConnected => continue,
                    Err(error) => panic!("actual member peer: {error}"),
                };
                let hello = match crate::runtime::read_json(&stream, end) {
                    Ok(hello) => hello,
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => continue,
                    Err(error) => panic!("actual member HELLO: {error}"),
                };
                match hello["role"].as_str() {
                    Some("native-context") => {
                        crate::resource_context::serve(&stream, peer, &hello, end, |peer, query| {
                            world.owner.context(peer, &query)
                        })
                        .unwrap()
                    }
                    Some("native-telemetry") => crate::resource_telemetry::serve(
                        &stream,
                        peer,
                        &hello,
                        end,
                        &telemetry,
                        |peer, query| world.owner.context(peer, &query),
                    )
                    .unwrap(),
                    Some("proxy-member" | "proxy-member-accounted-v1") => {
                        let accounted =
                            hello["role"].as_str() == Some(control::accounting::PROXY_ROLE);
                        native
                            .validate_cached_process(
                                trust_resource_provenance::process::read_identity(peer.pid)
                                    .unwrap(),
                                end,
                            )
                            .unwrap();
                        let job = if accounted {
                            crate::actor::proxy_binding_accounted(&hello)
                        } else {
                            super::super::members::binding_role(&hello, "proxy-member")
                        }
                        .unwrap();
                        wire::write_frame(&stream, aterm_types::resource_subgroup::PREPARE, end)
                            .unwrap();
                        let provisional = |request: &control::Request, helper: Peer| {
                            world.owner.provisional_proxy(request, helper)
                        };
                        let incoming = if accounted {
                            crate::resource_proxy_intake::receive_accounted(
                                &stream,
                                &job,
                                &native,
                                end,
                                provisional,
                            )
                        } else {
                            crate::resource_proxy_intake::receive(
                                &stream,
                                &job,
                                &native,
                                end,
                                provisional,
                            )
                        }
                        .unwrap();
                        if unknown {
                            assert!(corrupt_after_retirement.is_none());
                            let duplicate =
                                aterm_uds::kept_out(|| incoming.rights.events.try_clone()).unwrap();
                            aterm_uds::keep_out_of_children(&duplicate).unwrap();
                            corrupt_after_retirement =
                                Some((incoming.request.member_attempt, CtlStream::from(duplicate)));
                        }
                        world.owner.submit_proxy(incoming);
                        acknowledge(&stream, end).unwrap();
                        submissions += 1;
                    }
                    _ => panic!("unexpected native member role"),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(error) => panic!("actual member accept: {error}"),
        }
    }
    let accounted = aterm_types::resource_admission::AccountedEventFrame::decode(
        &wire::read_frame(&events, Instant::now() + HANG).unwrap(),
        &binding,
    )
    .unwrap();
    assert!(
        matches!(accounted.finished.event, Event::Finished { outcome: Outcome::Exit(code), cause: CompletionCause::Exited, .. } if code == if unknown { 101 } else { 0 }),
        "actual root outcome: {:?}; root stderr: {}",
        accounted.finished.event,
        fs::read_to_string(&root_log).unwrap()
    );
    assert!(
        accounted.observation.cpu_times.is_some() && accounted.observation.peak_rss_bytes.is_some()
    );
    assert_eq!(
        submissions,
        if unknown { 1 } else { 5 },
        "actual root: {}",
        fs::read_to_string(&root_log).unwrap()
    );
    if unknown {
        assert!(
            fs::read_to_string(&root_log)
                .unwrap()
                .contains("actual unknown member has no accounting")
        );
        assert_eq!(fs::read(&marker).unwrap(), b"metadata-executed\n");
    } else {
        assert_eq!(fs::read(&marker).unwrap(), b"metadata-executed\nmetadata-failed\nmetadata-cancel-ready\n--print=sysroot\n--print=target-list\nroot-finished\n");
    }
    assert_eq!(world.owner.debt.reserved().unwrap(), 0);
    assert!(
        world
            .owner
            .pool
            .snapshot()
            .unwrap()
            .iter()
            .all(|state| matches!(
                state,
                aterm_capacity::build_cpu::SlotState::Available { .. }
            ))
    );
    assert_eq!(
        world.owner.ledger.reap_cohort_claim(&memory).unwrap(),
        CohortClaimState::Closed
    );
    assert!(
        matches!(
            atpkg::lease::reclaim(&world.scratch.path, &subject),
            atpkg::lease::Reclaim::Clear(_)
        ),
        "all actual root/member SDK guards must close after scoped retirement"
    );
    println!("real-member-completed");
}
fn root() {
    let store = ReadStore::open_at(&PathBuf::from(std::env::var_os(STORE).unwrap())).unwrap();
    let end = Instant::now() + HANG;
    let client = Client::from_helper(
        store
            .connect_ancestor(end)
            .unwrap()
            .into_helper(end)
            .unwrap()
            .into(),
    );
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let context = client
        .context(
            &control::ContextQuery {
                binding: binding.clone(),
                call_attempt: [9; 16],
            },
            end,
        )
        .unwrap();
    trust_resource_controller::client::sdk::validate_sdk_keeper(&context.context).unwrap();
    assert!(context.context.sdk.is_some());
    let query = control::TelemetryQuery {
        query: control::ContextQuery {
            binding: binding.clone(),
            call_attempt: [10; 16],
        },
    };
    let observed = client.telemetry(&query, end).unwrap();
    assert_eq!(observed.reply.context.context, context.context);
    assert!(observed.age().is_some());
    assert!(observed.reply.snapshot.cpus > 0);
    let mut wrong = query.clone();
    wrong.query.binding.capability[0] ^= 1;
    wrong.query.call_attempt = [11; 16];
    let denied = match client.telemetry(&wrong, end) {
        Ok(_) => panic!("wrong telemetry capability accepted"),
        Err(error) => error,
    };
    assert_eq!(
        denied.delivery,
        control::client::Delivery::ContextUnavailable
    );
    let identity = trust_resource_provenance::process::read_identity(std::process::id()).unwrap();
    assert!(context.context.may_acquire_initial_cpu(control::Identity {
        pid: identity.pid,
        birth: identity.birth,
        birth_kind: control::BirthKind::MacosWallMicroseconds
    }));
    let pool = Pool::open_existing(&PathBuf::from(std::env::var_os(BASE).unwrap()))
        .unwrap()
        .unwrap();
    let retirement = aterm_capacity::build_cpu::LegacyRetirement::open(&PathBuf::from(
        std::env::var_os(LEGACY).unwrap(),
    ))
    .unwrap();
    let debt = ExternalDebt::open(&pool, &retirement).unwrap().unwrap();
    let lease = debt
        .try_acquire_native(aterm_capacity::build_cpu::Owner::current(None), 2)
        .unwrap()
        .unwrap();
    let cpu = aterm_uds::kept_out(|| lease.as_fd().try_clone_to_owned()).unwrap();
    let fifo = PathBuf::from(std::env::var_os(BASE).unwrap()).join("original.fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the CString is live and NUL terminated; this creates only our private FIFO.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut original = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    original.write_all(b"+").unwrap();
    let read = aterm_uds::kept_out(|| original.as_fd().try_clone_to_owned()).unwrap();
    let write = aterm_uds::kept_out(|| original.as_fd().try_clone_to_owned()).unwrap();
    let stat = original.metadata().unwrap();
    let sdk = PathBuf::from(std::env::var_os(SDK).unwrap());
    let marker = std::env::var_os(MARKER).unwrap();
    let request = control::Request {
        binding,
        member_attempt: [8; 16],
        role: control::Role::MetadataVersion,
        command: control::CommandSpec {
            program: sdk.join("bin/trustc").as_os_str().as_bytes().to_vec(),
            arg0: None,
            args: vec![b"-vV".to_vec()],
            environment: vec![
                (
                    MARKER.as_bytes().to_vec(),
                    marker.as_os_str().as_bytes().to_vec(),
                ),
                (
                    STORE.as_bytes().to_vec(),
                    std::env::var_os(STORE)
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                ),
            ],
            cwd: sdk.as_os_str().as_bytes().to_vec(),
        },
        delegations: control::Delegations {
            cpu: Some(control::Descriptor {
                source: cpu.as_raw_fd() as u32,
                target: 80,
            }),
            gnu: control::GnuTransport::NamedFifo {
                locator: fifo.as_os_str().as_bytes().to_vec(),
                device: stat.dev(),
                inode: stat.ino(),
                read: control::Descriptor {
                    source: read.as_raw_fd() as u32,
                    target: 82,
                },
                write: control::Descriptor {
                    source: write.as_raw_fd() as u32,
                    target: 83,
                },
            },
            fences: Vec::new(),
        },
        // These cases judge exit, cancellation and original custody, not
        // execution latency. The host and client also retain their hang bounds.
        timeout_ns: Some(HANG.as_nanos().try_into().unwrap()),
    };
    let mut request = request;
    let unknown = std::env::var(MODE).is_ok_and(|mode| mode == "unknown");
    if unknown {
        request
            .command
            .environment
            .push((MODE.as_bytes().to_vec(), b"unknown".to_vec()));
    }
    let originals = [&cpu, &read, &write].map(|fd| {
        let raw = fd.as_raw_fd();
        // SAFETY: observe flags of a descriptor whose original ownership remains live.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        assert!(flags >= 0);
        (raw, flags)
    });
    let mut child = client
        .member_accounted(
            &request,
            MemberIo {
                stdin: std::process::Stdio::null(),
                stdout: std::process::Stdio::null(),
                stderr: std::process::Stdio::inherit(),
                descriptions: vec![cpu, read, write],
            },
            end,
        )
        .unwrap();
    let status = loop {
        assert!(Instant::now() < end, "actual proxy member did not complete");
        if let Some(status) = child.try_wait(end) {
            break status;
        }
        std::thread::sleep(POLL);
    };
    if unknown {
        assert!(
            matches!(status, control::Status::Unknown { .. }),
            "{status:?}"
        );
        assert!(child.accounting().is_none());
        eprintln!(
            "actual unknown member has no accounting; original root exits failure without retry"
        );
        std::process::exit(101);
    }
    assert!(
        matches!(
            status,
            control::Status::Finished {
                outcome: control::Outcome::Exit(0),
                cause: control::CompletionCause::Exited,
                ..
            }
        ),
        "{status:?}"
    );
    let accounting = child
        .accounting()
        .expect("negotiated original wait observation");
    assert_eq!(
        accounting.scope,
        control::accounting::Scope::ChildAndWaitedDescendantsV1
    );
    assert!(accounting.cpu_times.is_some() && accounting.peak_rss_bytes.is_some());
    for (raw, flags) in originals {
        // SAFETY: ProxyChild retains these exact owned original descriptions until Drop.
        assert_eq!(unsafe { libc::fcntl(raw, libc::F_GETFD) }, flags);
    }
    assert!(
        child
            .started()
            .is_some_and(|s| s.identity.pid != std::process::id())
    );
    drop(child);
    // The same original CPU lease and FIFO back every sample. These are
    // native child outcomes, not fabricated saved terminal measurements.
    for (offset, mode) in ["fail", "cancel"].into_iter().enumerate() {
        let cpu = aterm_uds::kept_out(|| lease.as_fd().try_clone_to_owned()).unwrap();
        let read = aterm_uds::kept_out(|| original.as_fd().try_clone_to_owned()).unwrap();
        let write = aterm_uds::kept_out(|| original.as_fd().try_clone_to_owned()).unwrap();
        let mut sample = request.clone();
        sample.member_attempt = [offset as u8 + 31; 16];
        sample
            .command
            .environment
            .push((MODE.as_bytes().to_vec(), mode.as_bytes().to_vec()));
        sample.delegations.cpu.as_mut().unwrap().source = cpu.as_raw_fd() as u32;
        let control::GnuTransport::NamedFifo {
            read: sample_read,
            write: sample_write,
            ..
        } = &mut sample.delegations.gnu
        else {
            panic!("original FIFO missing")
        };
        sample_read.source = read.as_raw_fd() as u32;
        sample_write.source = write.as_raw_fd() as u32;
        let mut child = client
            .member_accounted(
                &sample,
                MemberIo {
                    stdin: std::process::Stdio::null(),
                    stdout: std::process::Stdio::null(),
                    stderr: std::process::Stdio::inherit(),
                    descriptions: vec![cpu, read, write],
                },
                end,
            )
            .unwrap();
        let mut cancelled = false;
        let status = loop {
            assert!(
                Instant::now() < end,
                "actual accounted failure/cancel member did not drain"
            );
            let status = child.try_wait(end);
            if mode == "cancel"
                && !cancelled
                && fs::read(&marker)
                    .unwrap()
                    .ends_with(b"metadata-cancel-ready\n")
            {
                child.cancel(end).unwrap();
                cancelled = true;
            }
            if let Some(status) = status {
                break status;
            }
            std::thread::sleep(POLL);
        };
        if mode == "fail" {
            assert!(
                matches!(
                    status,
                    control::Status::Finished {
                        outcome: control::Outcome::Exit(7),
                        cause: control::CompletionCause::Exited,
                        ..
                    }
                ),
                "{status:?}"
            );
        } else {
            assert!(cancelled);
            assert!(
                matches!(
                    status,
                    control::Status::Finished {
                        outcome: control::Outcome::Signal(_),
                        cause: control::CompletionCause::Cancelled,
                        ..
                    }
                ),
                "{status:?}"
            );
        }
        let observed = child
            .accounting()
            .expect("known native failure retains original wait observation");
        assert!(observed.cpu_times.is_some() && observed.peak_rss_bytes.is_some());
    }
    let mut token = [0];
    original.read_exact(&mut token).unwrap();
    assert_eq!(token, *b"+");
    assert!(original.read(&mut token).is_err());
    for (index, query) in ["--print=sysroot", "--print=target-list"]
        .into_iter()
        .enumerate()
    {
        probe::run(&client, &request, &lease, query, index as u8 + 20, end);
    }
    fs::OpenOptions::new()
        .append(true)
        .open(marker)
        .unwrap()
        .write_all(b"root-finished\n")
        .unwrap();
}
fn metadata(gnu: Option<[OwnedFd; 2]>, value: &str) {
    let end = Instant::now() + HANG;
    let store = ReadStore::open_at(&PathBuf::from(std::env::var_os(STORE).unwrap())).unwrap();
    let client = Client::from_helper(
        store
            .connect_ancestor(end)
            .unwrap()
            .into_helper(end)
            .unwrap()
            .into(),
    );
    let binding =
        control::JobBinding::from_locator(&std::env::var("ATERM_RESOURCE_JOB_V3").unwrap())
            .unwrap();
    let reply = client
        .context(
            &control::ContextQuery {
                binding,
                call_attempt: [12; 16],
            },
            end,
        )
        .unwrap();
    assert!(matches!(
        reply.context.subject,
        control::Subject::RegisteredMember { .. }
    ));
    trust_resource_controller::client::sdk::validate_sdk_keeper(&reply.context).unwrap();
    assert!(reply.context.sdk.is_some());
    if let Some([read, write]) = gnu {
        let mut input = File::from(read);
        let mut output = File::from(write);
        let mut token = [0];
        input.read_exact(&mut token).unwrap();
        assert_eq!(token, *b"+");
        output.write_all(&token).unwrap();
        let mode = std::env::var(MODE).unwrap_or_default();
        if mode.is_empty() || mode == "unknown" {
            fs::write(std::env::var_os(MARKER).unwrap(), b"metadata-executed\n").unwrap();
        } else {
            let mut marker = fs::OpenOptions::new()
                .append(true)
                .open(std::env::var_os(MARKER).unwrap())
                .unwrap();
            marker
                .write_all(if mode == "fail" {
                    b"metadata-failed\n"
                } else {
                    b"metadata-cancel-ready\n"
                })
                .unwrap();
            marker.flush().unwrap();
            if mode == "fail" {
                std::process::exit(7);
            }
            assert_eq!(mode, "cancel");
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    } else {
        let mut marker = fs::OpenOptions::new()
            .append(true)
            .open(std::env::var_os(MARKER).unwrap())
            .unwrap();
        writeln!(marker, "{value}").unwrap();
    }
    println!("{value}");
}

#[path = "member_native_probe.rs"]
mod probe;
