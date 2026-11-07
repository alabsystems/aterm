// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;

struct Never;
impl Backend for Never {
    fn submit(
        &mut self,
        _: wire::Request,
        _: OwnedFd,
        _: StdioConfig,
        _: Delegations,
        _: bool,
    ) -> Result<RemoteChild, SubmitError> {
        panic!("non-entry must not touch backend")
    }
}
#[test]
fn only_exact_entry_may_touch_private_descriptors() {
    for args in [
        vec![],
        wire::HELPER_ARGUMENTS
            .iter()
            .chain(std::iter::once(&"extra"))
            .map(OsString::from)
            .collect(),
    ] {
        if args
            .first()
            .is_some_and(|arg| arg == "__resource-member-v3")
        {
            assert_eq!(entry(&args, &mut Never), Some(ExitCode::from(75)));
        } else {
            assert!(entry(&args, &mut Never).is_none());
        }
    }
}
#[test]
fn actual_terminal_mapping_keeps_target_outcome_separate_from_helper_health() {
    let status = completed(Terminal::Finished(
        aterm_ctl::resource_subgroup_client::Completed {
            outcome: admission::Outcome::Signal(9),
            cause: admission::CompletionCause::ResourceLimit,
            runtime_ns: 11,
            accounting: None,
        },
    ));
    assert_eq!(
        status,
        wire::Status::Finished {
            outcome: wire::Outcome::Signal(9),
            cause: wire::CompletionCause::ResourceLimit,
            runtime_ns: 11
        }
    );
    assert_eq!(
        completed(Terminal::Refused {
            reason: "prelaunch".into()
        }),
        wire::Status::Refused {
            reason: "prelaunch".into()
        }
    );
}

/// A real RemoteChild and canonical descriptor server exercise the proxy's
/// ACK-loss branch, rather than manufacturing a RemoteChild or trusting helper exit.
#[test]
fn lost_attachment_ack_forwards_actual_lifecycle_without_fabricated_attached() {
    use aterm_ctl::resource_admission_transport as transport;
    use aterm_ctl::resource_subgroup_client as remote;
    use aterm_types::resource_subgroup as subgroup;
    use std::os::fd::AsFd;
    let deadline = || Instant::now() + EXCHANGE;
    let binding = |n| admission::Binding {
        session: "s-ab12".into(),
        attempt: [n; 16],
    };
    let root = binding(1);
    let member = binding(2);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("host");
    let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&path)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let native = aterm_uds::process::read_identity(std::process::id() as i32).unwrap();
    let endpoint = remote::Endpoint {
        path,
        host_pid: std::process::id(),
        host_birth: native.birth,
        host_uid: aterm_uds::peer::our_uid(),
    };
    let request = subgroup::Request {
        job: root.clone(),
        capability: subgroup::Capability::from_bytes([7; 32]),
        member: admission::Submission {
            binding: member.clone(),
            peer_pid: std::process::id(),
            peer_birth: native.birth.0,
            argv: vec![b"/bin/fixture".to_vec()],
            cwd: b"/".to_vec(),
            env: vec![],
            cpu_units: 1,
            memory_bytes: 100,
            label: "proxy transport fixture".into(),
            timeout_ms: None,
        },
        cpu: subgroup::CpuDelegation::FixedBorrow { units: 1 },
        gnu: subgroup::GnuTransport::None,
        fence_targets: vec![],
    };
    let server = std::thread::spawn(move || {
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => aterm_uds::ready::until(
                    listener.as_fd(),
                    aterm_uds::ready::Interest::Read,
                    deadline(),
                )
                .unwrap(),
                Err(e) => panic!("accept: {e}"),
            }
        };
        assert_eq!(
            transport::read_frame(&stream, deadline()).unwrap(),
            subgroup::hello(&root).unwrap()
        );
        transport::write_frame(&stream, subgroup::PREPARE, deadline()).unwrap();
        let received = remote::receive(&stream, &root, deadline(), |_, peer| {
            peer.pid == std::process::id()
        })
        .unwrap();
        let events = aterm_uds::CtlStream::from(received.rights.events);
        drop(received.rights.stdout);
        drop(received.rights.stdin);
        drop(received.rights.stderr);
        // Close only attachment, retaining the original lifecycle/cancellation lane.
        drop(stream);
        transport::send_event(
            &events,
            &admission::EventFrame {
                binding: member.clone(),
                event: admission::Event::Started {
                    pid: 42,
                    birth: 43,
                    monotonic_ns: 44,
                    unix_ns: 45,
                },
            },
            deadline(),
        )
        .unwrap();
        transport::read_cancel(&events, &member, deadline()).unwrap();
        transport::send_event(
            &events,
            &admission::EventFrame {
                binding: member,
                event: admission::Event::Finished {
                    outcome: admission::Outcome::Exit(7),
                    cause: admission::CompletionCause::Cancelled,
                    runtime_ns: 11,
                },
            },
            deadline(),
        )
        .unwrap();
        // Rights arrived, but no attachment ACK is sent.
    });
    let mut child = match RemoteChild::submit(
        &endpoint,
        &request,
        Delegations {
            cpu: None,
            gnu: vec![],
            fences: vec![],
        },
        StdioConfig {
            stdin: Stdio::Null,
            stdout: Stdio::Piped,
            stderr: Stdio::Null,
        },
        deadline(),
    ) {
        Err(SubmitError::Unknown { child, .. }) => *child,
        _ => panic!("expected ambiguous ACK"),
    };
    use std::io::Read;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert!(
        output.is_empty(),
        "delivered output writer closed before completion"
    );
    let (helper, parent) = UnixStream::pair().unwrap();
    let context = Context {
        root: [1; 16],
        member: [2; 16],
        accounted: false,
    };
    let reader = std::thread::spawn(move || {
        let mut channel = wire::Channel::new(parent).unwrap();
        let first =
            wire::StatusFrame::decode(&channel.receive(deadline()).unwrap(), [1; 16], [2; 16])
                .unwrap();
        assert!(matches!(first.status, wire::Status::Started(_)));
        channel
            .send(
                &wire::Cancel {
                    root_attempt: [1; 16],
                    member_attempt: [2; 16],
                }
                .encode()
                .unwrap(),
                deadline(),
            )
            .unwrap();
        let last =
            wire::StatusFrame::decode(&channel.receive(deadline()).unwrap(), [1; 16], [2; 16])
                .unwrap();
        assert!(matches!(
            last.status,
            wire::Status::Finished {
                outcome: wire::Outcome::Exit(7),
                cause: wire::CompletionCause::Cancelled,
                ..
            }
        ));
    });
    observe(
        &context,
        &mut wire::Channel::new(helper).unwrap(),
        &mut child,
        false,
    )
    .unwrap();
    reader.join().unwrap();
    server.join().unwrap();
}

#[test]
fn connected_inet_stream_is_not_a_private_unix_control() {
    use std::os::fd::AsRawFd;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_accepted, _) = listener.accept().unwrap();
    assert!(socket(stream.as_raw_fd() as u32).is_err());
    assert!(
        stream.peer_addr().is_ok(),
        "rejected descriptor was never taken or closed"
    );
}
