use std::io::Write as _;

use super::*;
use crate::{
    BirthKind, CommandSpec, CompletionCause, Delegations, Descriptor, Identity, JobBinding,
    MetadataCpu, MetadataFile, Outcome, Request, Role, StatusFrame,
};

fn actual_streams() -> (MetadataStreams, UnixStream, std::io::PipeWriter, std::io::PipeWriter) {
    let (events, peer) = UnixStream::pair().unwrap();
    let (output, stdout, stderr) = Capture::new().unwrap();
    (
        MetadataStreams {
            events: Channel::new(events).unwrap(),
            output,
            state: Lifecycle::default(),
            observed: ObservedEvents::default(),
            cancel_sent: false,
        },
        peer,
        stdout,
        stderr,
    )
}

#[test]
fn concurrent_fresh_capture_and_lifeline_transfer_preserve_original_pipe_flags() {
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            for _ in 0..128 {
                let (mut capture, mut stdout, stderr) = Capture::new().unwrap();
                stdout.write_all(b"independent output").unwrap();
                capture.drain().unwrap();
                assert_eq!(capture.bytes[0], b"independent output");
                drop((stdout, stderr));
            }
        });
        barrier.wait();
        for _ in 0..128 {
            let (reader, mut writer) = std::io::pipe().unwrap();
            let before = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
            assert!(before >= 0);
            let (a, b) = UnixStream::pair().unwrap();
            let mut sender = Channel::new(a).unwrap();
            let mut receiver = Channel::new(b).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            sender.send_read_lifeline(reader.as_fd(), deadline).unwrap();
            let actual = receiver.receive_read_lifeline(deadline).unwrap();
            for fd in [reader.as_raw_fd(), actual.as_raw_fd()] {
                let after = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                assert!(after >= 0);
                assert_eq!(
                    crate::tests::caller_status_flags(after),
                    crate::tests::caller_status_flags(before)
                );
                assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
            }
            writer.write_all(b"original").unwrap();
            let mut actual = File::from(actual);
            let mut bytes = [0; 8];
            actual.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"original");
        }
    });
}

#[test]
fn actual_partial_output_and_native_frames_survive_event_eof_without_completion() {
    let (mut streams, peer, mut stdout, mut stderr) = actual_streams();
    let mut peer = Channel::new(peer).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut original_events = Vec::new();
    for event in [
        status(Status::Attached),
        MetadataEvent::Prepared(prepared()),
        status(Status::Started(started())),
    ] {
        let frame = event.encode().unwrap();
        original_events.extend((frame.len() as u32).to_be_bytes());
        original_events.extend(&frame);
        peer.send(&frame, deadline).unwrap();
    }
    stdout.write_all(b"actual partial stdout\0").unwrap();
    stderr.write_all(b"actual partial stderr\xff").unwrap();
    drop((stdout, stderr, peer));
    loop {
        match streams.poll(&request(), deadline) {
            Ok(StreamReady::Pending) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => break,
            _ => panic!("incomplete native event stream became terminal"),
        }
    }
    assert!(streams.state.completed.is_none(), "EOF cannot become Completed");
    assert!(streams.state.started.is_some());
    // Parallel fork controls can briefly retain inherited pipe writers. Wait
    // for the actual EOF instead of equating local writer Drop with kernel EOF.
    while !streams.output.eof.iter().all(|eof| *eof) && Instant::now() < deadline {
        streams.output.drain().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    let captured = streams.take_failure_observation(&request());
    assert_eq!(captured.stdout, b"actual partial stdout\0");
    assert_eq!(captured.stderr, b"actual partial stderr\xff");
    assert!(captured.stdout_eof && captured.stderr_eof);
    assert_eq!(captured.event_prefix, original_events);
    assert_eq!(captured.event_bytes_seen, original_events.len() as u64);
    assert!(!captured.event_prefix_truncated());
    assert_eq!(MetadataFailureObservation::decode(&captured.encode().unwrap()).unwrap(), captured);
    assert!(streams.state.completed.is_none());
}

#[test]
fn actual_malformed_partial_event_preserves_received_prefix_and_output() {
    let (mut streams, mut peer, mut stdout, stderr) = actual_streams();
    let partial = [0, 0, 0, 8, b'x', b'y'];
    // This is a real short native socket write followed by EOF, not a decoded
    // terminal fixture. The production receiver must retain the bytes it read.
    peer.write_all(&partial).unwrap();
    stdout.write_all(b"before malformed event").unwrap();
    drop((peer, stdout, stderr));
    assert!(streams.poll(&request(), Instant::now() + Duration::from_secs(10)).is_err());
    let captured = streams.take_failure_observation(&request());
    assert_eq!(captured.stdout, b"before malformed event");
    assert_eq!(captured.event_prefix, partial);
    assert_eq!(captured.event_bytes_seen, partial.len() as u64);
    assert!(streams.state.completed.is_none());
}

fn request() -> SealedMetadataRequest {
    SealedMetadataRequest {
        member: Request {
            binding: JobBinding {
                session: "s-ab".into(),
                root_attempt: [1; 16],
                capability: [2; 32],
            },
            member_attempt: [3; 16],
            role: Role::SealedMetadata,
            command: CommandSpec {
                program: b"/sealed/bin/program".to_vec(),
                arg0: None,
                args: vec![b"-vV".to_vec()],
                environment: vec![],
                cwd: b"/sealed".to_vec(),
            },
            delegations: Delegations {
                cpu: None,
                fences: vec![],
                gnu: GnuTransport::NumericPipe {
                    read: Descriptor { source: 5, target: 3 },
                    write: Descriptor { source: 6, target: 4 },
                },
            },
            timeout_ns: Some(30_000_000_000),
        },
        cpu: MetadataCpu::FixedBorrow,
        root: b"/sealed".to_vec(),
        program_sha256: [4; 32],
    }
}
fn prepared() -> MetadataPrepared {
    let request = request();
    let (host, boot) = process::host_boot().unwrap();
    MetadataPrepared {
        root_attempt: request.member.binding.root_attempt,
        member_attempt: request.member.member_attempt,
        root: request.root.clone(),
        policy: program::RUNTIME_POLICY.into(),
        scope: program::INPUT_SCOPE.into(),
        executable: MetadataFile {
            path: request.member.command.program.clone(),
            sha256: [4; 32],
            bytes: 100,
            device: 1,
            inode: 2,
        },
        runtime: vec![],
        loader_paths: vec![],
        system_dependencies: vec![],
        host,
        boot,
    }
}
fn started() -> Started {
    Started {
        identity: Identity {
            pid: 42,
            birth: 123,
            birth_kind: if cfg!(target_os = "macos") {
                BirthKind::MacosWallMicroseconds
            } else {
                BirthKind::LinuxStartTicks
            },
        },
        monotonic_ns: 100,
        unix_ns: 200,
    }
}
fn status(value: Status) -> MetadataEvent {
    MetadataEvent::Status(StatusFrame {
        root_attempt: [1; 16],
        member_attempt: [3; 16],
        status: value,
    })
}
fn completed(result: Result<(), String>) -> MetadataEvent {
    MetadataEvent::Completed(MetadataCompletion {
        finished: StatusFrame {
            root_attempt: [1; 16],
            member_attempt: [3; 16],
            status: Status::Finished {
                outcome: Outcome::Exit(7),
                cause: CompletionCause::Exited,
                runtime_ns: 999,
            },
        },
        inputs_unchanged: result,
    })
}
fn through_start() -> Lifecycle {
    let mut state = Lifecycle::default();
    for event in [
        status(Status::Attached),
        MetadataEvent::Prepared(prepared()),
        status(Status::Started(started())),
    ] {
        state.accept(event, &request()).unwrap();
    }
    state
}

#[test]
fn actual_exit_and_changed_input_result_remain_independent() {
    for result in [Ok(()), Err("original executable changed".into())] {
        let mut state = through_start();
        state.accept(completed(result.clone()), &request()).unwrap();
        assert_eq!(state.completed.as_ref().unwrap().inputs_unchanged, result);
        assert!(matches!(
            state.completed.as_ref().unwrap().finished.status,
            Status::Finished { outcome: Outcome::Exit(7), runtime_ns: 999, .. }
        ));
        assert!(state.accept(completed(Ok(())), &request()).is_err());
    }
}

#[test]
fn every_missing_or_duplicate_phase_refuses() {
    let events = [
        status(Status::Attached),
        MetadataEvent::Prepared(prepared()),
        status(Status::Started(started())),
        completed(Ok(())),
    ];
    for omitted in 0..3 {
        let mut state = Lifecycle::default();
        assert!(
            events
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != omitted)
                .try_for_each(|(_, event)| state.accept(event.clone(), &request()))
                .is_err(),
            "omission {omitted}"
        );
    }
    for duplicate in 0..4 {
        let mut state = Lifecycle::default();
        for (index, event) in events.iter().enumerate() {
            state.accept(event.clone(), &request()).unwrap();
            if index == duplicate {
                assert!(state.accept(event.clone(), &request()).is_err(), "duplicate {duplicate}");
                break;
            }
        }
    }
}

#[test]
fn inventory_policy_host_scope_root_and_image_must_match() {
    for change in 0..7 {
        let mut record = prepared();
        match change {
            0 => record.policy.push('x'),
            1 => record.scope.push('x'),
            2 => record.host.push('x'),
            3 => record.boot.push('x'),
            4 => record.root = b"/foreign".to_vec(),
            5 => record.executable.sha256[0] ^= 1,
            6 => record.member_attempt[0] ^= 1,
            _ => unreachable!(),
        }
        let mut state = Lifecycle::default();
        state.accept(status(Status::Attached), &request()).unwrap();
        assert!(
            state.accept(MetadataEvent::Prepared(record), &request()).is_err(),
            "change {change}"
        );
    }
}

#[test]
fn plain_finished_unknown_and_late_refusal_cannot_mint_completion() {
    for event in [
        status(Status::Finished {
            outcome: Outcome::Exit(0),
            cause: CompletionCause::Exited,
            runtime_ns: 1,
        }),
        status(Status::Unknown { reason: "lost".into(), started: Some(started()) }),
        status(Status::Refused { reason: "late".into() }),
    ] {
        assert!(through_start().accept(event, &request()).is_err());
    }
    let mut state = Lifecycle::default();
    state.accept(status(Status::Refused { reason: "not admitted".into() }), &request()).unwrap();
    assert_eq!(state.refused.as_deref(), Some("not admitted"));
    assert!(state.accept(status(Status::Attached), &request()).is_err());
}

#[test]
fn combined_output_bound_does_not_replace_actual_eof() {
    let (mut capture, mut stdout, mut stderr) = Capture::new().unwrap();
    for _ in 0..16 {
        stdout.write_all(&[b'a'; 2048]).unwrap();
        stderr.write_all(&[b'b'; 2048]).unwrap();
        capture.drain().unwrap();
    }
    assert_eq!(capture.bytes[0].len() + capture.bytes[1].len(), METADATA_MAX_OUTPUT);
    assert!(!capture.exceeded);
    stdout.write_all(b"one more").unwrap();
    capture.drain().unwrap();
    assert!(capture.exceeded);
    assert_eq!(capture.eof, [false; 2]);
    drop((stdout, stderr));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !capture.eof.iter().all(|eof| *eof) && Instant::now() < deadline {
        capture.drain().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(capture.eof, [true; 2]);
    assert_eq!(capture.bytes[0].len() + capture.bytes[1].len(), METADATA_MAX_OUTPUT);
}

#[test]
fn clean_event_eof_does_not_consume_or_hide_partial_trailing_frame() {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    let mut channel = Channel::new(reader).unwrap();
    assert!(!channel.at_eof().unwrap());
    writer.write_all(&[0, 0]).unwrap();
    drop(writer);
    assert!(!channel.at_eof().unwrap());
    assert!(channel.receive(Instant::now() + Duration::from_secs(1)).is_err());
    assert!(channel.poisoned());
    let (reader, writer) = UnixStream::pair().unwrap();
    let channel = Channel::new(reader).unwrap();
    drop(writer);
    assert!(channel.at_eof().unwrap());
}

#[test]
fn existing_original_descriptions_are_ordered_never_reconstructed() {
    let (read, write) = io::pipe().unwrap();
    fn copy(source: std::os::fd::BorrowedFd<'_>) -> OwnedFd {
        use std::os::fd::FromRawFd as _;
        // SAFETY: new owned CLOEXEC copy at a source slot allowed by the
        // existing Request contract. It retains the SAME original pipe.
        let fd = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
        assert!(fd >= 5);
        unsafe { OwnedFd::from_raw_fd(fd) }
    }
    let read = copy(read.as_fd());
    let write = copy(write.as_fd());
    let mut request = request();
    request.member.delegations.gnu = GnuTransport::NumericPipe {
        read: Descriptor { source: read.as_raw_fd() as u32, target: 3 },
        write: Descriptor { source: write.as_raw_fd() as u32, target: 4 },
    };
    let originals = vec![OwnedFd::from(write), OwnedFd::from(read)];
    let expected = match request.member.delegations.gnu {
        GnuTransport::NumericPipe { read, write } => [read.source, write.source],
        _ => unreachable!(),
    };
    let handles = ordered(&request, originals).unwrap();
    assert_eq!(handles.iter().map(|fd| fd.as_raw_fd() as u32).collect::<Vec<_>>(), expected);
    assert!(ordered(&request, vec![]).is_err());
    assert!(ordered(&request, vec![OwnedFd::from(File::open("/dev/null").unwrap())]).is_err());
}

#[test]
fn explicit_failed_discovery_never_creates_a_store_or_falls_back_to_account() {
    let path = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("missing-native-metadata-store-{}", std::process::id()));
    assert!(!path.exists());
    let error = Client::discover_at(&path, Instant::now() + Duration::from_secs(1))
        .err()
        .expect("explicit discovery must fail");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!path.exists());
}

fn resolver_request() -> crate::resolver::SealedResolverQueryRequest {
    let old = request();
    let mut member = old.member;
    member.role = Role::ResolverQuery;
    member.command.program = b"/sealed/bin/trustc".to_vec();
    member.command.environment = vec![
        (b"LANG".to_vec(), b"C".to_vec()),
        (b"LC_ALL".to_vec(), b"C".to_vec()),
        (b"CARGO".to_vec(), b"/sealed/bin/targo".to_vec()),
        (b"RUSTC".to_vec(), b"/sealed/bin/trustc".to_vec()),
        (b"CARGO_HOME".to_vec(), b"/cargo".to_vec()),
        (b"CARGO_CACHE_RUSTC_INFO".to_vec(), b"0".to_vec()),
    ];
    crate::resolver::SealedResolverQueryRequest {
        member,
        root: old.root,
        program_sha256: old.program_sha256,
    }
}
fn resolver_prepared() -> MetadataPrepared {
    let mut value = prepared();
    value.executable.path = b"/sealed/bin/trustc".to_vec();
    value
}
#[test]
fn resolver_query_retains_distinct_purpose_through_same_strict_lifecycle() {
    let request = resolver_request();
    request.validate().unwrap();
    request.validate_prepared(&resolver_prepared()).unwrap();
    assert!(resolver_prepared().validate_for(&super::tests::request()).is_err());
    let mut state = Lifecycle::default();
    for event in [
        status(Status::Attached),
        MetadataEvent::Prepared(resolver_prepared()),
        status(Status::Started(started())),
        completed(Err("actual original driver changed".into())),
    ] {
        state.accept(event, &request).unwrap();
    }
    assert!(matches!(
        state.completed.as_ref().unwrap().finished.status,
        Status::Finished { outcome: Outcome::Exit(7), runtime_ns: 999, .. }
    ));
    assert!(state.completed.unwrap().inputs_unchanged.is_err());
    let mut wrong = request.clone();
    wrong.member.role = Role::SealedMetadata;
    assert!(wrong.validate_prepared(&resolver_prepared()).is_err());
    let mut wrong = resolver_prepared();
    wrong.executable.sha256[0] ^= 1;
    assert!(request.validate_prepared(&wrong).is_err());
    let native = NativeQueryRequest::Resolver(request);
    assert!(crate::SealedMetadataRequest::decode(&native.payload().unwrap()).is_err());
    assert!(
        String::from_utf8(native.purpose_hello().unwrap())
            .unwrap()
            .contains("sealed-resolver-query-v1")
    );
}
#[test]
fn resolver_query_incomplete_native_stream_retains_actual_failed_bytes_without_completion() {
    let request = resolver_request();
    let (mut streams, mut peer, mut stdout, stderr) = actual_streams();
    let partial = [0, 0, 0, 8, b'R', b'5'];
    peer.write_all(&partial).unwrap();
    stdout.write_all(b"actual resolver query bytes\0").unwrap();
    drop((peer, stdout, stderr));
    assert!(streams.poll(&request, Instant::now() + Duration::from_secs(10)).is_err());
    let failed = streams.take_failure_observation(&request);
    assert_eq!(failed.stdout, b"actual resolver query bytes\0");
    assert_eq!(failed.event_prefix, partial);
    assert_eq!(failed.member_attempt, request.member.member_attempt);
    assert!(streams.state.completed.is_none());
}
