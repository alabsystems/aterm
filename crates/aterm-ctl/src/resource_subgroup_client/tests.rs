// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn binding(attempt: u8) -> admission::Binding {
    admission::Binding {
        session: "s-ab12".into(),
        attempt: [attempt; 16],
    }
}
fn pair() -> (RemoteChild, CtlStream) {
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    (
        RemoteChild::new(client, binding(2), None, None, None).unwrap(),
        server,
    )
}
fn frame(event: admission::Event) -> Vec<u8> {
    let bytes = admission::EventFrame {
        binding: binding(2),
        event,
    }
    .encode()
    .unwrap();
    let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
    framed.extend(bytes);
    framed
}
fn started() -> admission::Event {
    admission::Event::Started {
        pid: 42,
        birth: 43,
        monotonic_ns: 44,
        unix_ns: 45,
    }
}
fn finished() -> admission::Event {
    admission::Event::Finished {
        outcome: admission::Outcome::Exit(7),
        cause: admission::CompletionCause::Timeout,
        runtime_ns: 90,
    }
}
#[test]
fn split_frames_keep_state_and_cancel_is_exact_without_queue_timeout() {
    let (mut child, mut server) = pair();
    let queued = frame(admission::Event::Queued);
    for byte in queued {
        server.write_all(&[byte]).unwrap();
        assert!(child.try_wait().unwrap().is_none());
    }
    assert!(child.identity().is_none());
    assert_eq!(
        child.wait_until(Instant::now()).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    child.cancel(deadline()).unwrap();
    child.cancel(deadline()).unwrap();
    assert_eq!(
        transport::read_cancel(&server, &binding(2), deadline())
            .unwrap()
            .binding,
        binding(2)
    );
    let launch = frame(started());
    for byte in launch {
        server.write_all(&[byte]).unwrap();
        assert!(child.try_wait().unwrap().is_none());
    }
    assert_eq!(child.identity().unwrap().birth, Birth(43));
    let done = frame(finished());
    server.write_all(&done[..done.len() - 1]).unwrap();
    assert!(child.try_wait().unwrap().is_none());
    server.write_all(&done[done.len() - 1..]).unwrap();
    assert_eq!(
        child.try_wait().unwrap(),
        Some(Terminal::Finished(Completed {
            outcome: admission::Outcome::Exit(7),
            cause: admission::CompletionCause::Timeout,
            runtime_ns: 90,
            accounting: None,
        }))
    );
    server.write_all(&[0]).unwrap();
    assert!(child.try_wait().is_err());
    assert!(child.try_wait().is_err());
}
#[test]
fn stale_oversized_truncated_and_out_of_order_facts_poison_observation() {
    let mut wrong = admission::EventFrame {
        binding: binding(3),
        event: started(),
    }
    .encode()
    .unwrap();
    let mut stale = (wrong.len() as u32).to_be_bytes().to_vec();
    stale.append(&mut wrong);
    let mut duplicate = frame(admission::Event::Queued);
    duplicate.extend(frame(admission::Event::Queued));
    for bytes in [
        stale,
        frame(finished()),
        duplicate,
        ((admission::MAX_PAYLOAD + 1) as u32).to_be_bytes().to_vec(),
        frame(started())[..5].to_vec(),
    ] {
        let (mut child, mut server) = pair();
        server.write_all(&bytes).unwrap();
        drop(server);
        assert!(child.try_wait().is_err());
        assert!(child.try_wait().is_err());
    }
}
fn request() -> wire::Request {
    let identity = process::read_identity(std::process::id() as i32).unwrap();
    wire::Request {
        job: binding(1),
        capability: wire::Capability::from_bytes([7; 32]),
        member: admission::Submission {
            binding: binding(2),
            peer_pid: std::process::id(),
            peer_birth: identity.birth.0,
            argv: vec![b"/bin/tool".to_vec()],
            cwd: b"/".to_vec(),
            env: vec![],
            cpu_units: 1,
            memory_bytes: 100,
            label: "member".into(),
            timeout_ms: Some(100),
        },
        cpu: wire::CpuDelegation::FixedBorrow { units: 1 },
        gnu: wire::GnuTransport::None,
        fence_targets: vec![],
    }
}
fn fixture() -> (aterm_tempfile::TempDir, aterm_uds::CtlListener, Endpoint) {
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join("s");
    let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&path)).unwrap();
    let endpoint = Endpoint {
        path,
        host_pid: std::process::id(),
        host_birth: process::read_birth(std::process::id() as i32).unwrap(),
        host_uid: aterm_uds::peer::our_uid(),
    };
    (temp, listener, endpoint)
}
fn hello(stream: &CtlStream) {
    assert_eq!(
        transport::read_frame(stream, deadline()).unwrap(),
        wire::hello(&binding(1)).unwrap()
    );
    transport::write_frame(stream, wire::PREPARE, deadline()).unwrap();
}
fn accept(listener: &aterm_uds::CtlListener) -> CtlStream {
    aterm_uds::accept(
        listener,
        aterm_uds::AcceptWait::Until(deadline()),
        aterm_uds::AcceptedMode::Nonblocking,
    )
    .unwrap()
    .0
}
fn no_delegations() -> Delegations {
    Delegations {
        cpu: None,
        gnu: vec![],
        fences: vec![],
    }
}
#[test]
fn wrong_host_birth_refuses_before_any_payload_or_rights() {
    let (_temp, listener, mut endpoint) = fixture();
    endpoint.host_birth = Birth(1);
    let result = RemoteChild::submit(
        &endpoint,
        &request(),
        no_delegations(),
        StdioConfig::default(),
        deadline(),
    );
    assert!(matches!(result, Err(SubmitError::NotSubmitted(_))));
    let (mut stream, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    let mut byte = [0];
    stream.set_nonblocking(true).unwrap();
    ready::until(stream.as_fd(), Interest::Read, deadline()).unwrap();
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
}
#[test]
fn lost_ack_keeps_actual_facts_and_piped_output_unknown_never_refused() {
    let (_temp, listener, endpoint) = fixture();
    let seen = Arc::new(AtomicBool::new(false));
    let recorded = seen.clone();
    let server = std::thread::spawn(move || {
        let stream = accept(&listener);
        hello(&stream);
        let authorized = receive(&stream, &binding(1), deadline(), |r, peer| {
            recorded.store(true, Ordering::SeqCst);
            r.capability.as_bytes() == &[7; 32] && peer.pid == std::process::id()
        })
        .unwrap();
        let mut output = File::from(authorized.rights.stdout);
        output.write_all(b"actual output").unwrap();
        drop(output);
        let events = CtlStream::from(authorized.rights.events);
        for event in [admission::Event::Queued, started(), finished()] {
            transport::send_event(
                &events,
                &admission::EventFrame {
                    binding: binding(2),
                    event,
                },
                deadline(),
            )
            .unwrap();
        }
        // Deliberately lose ACK after rights and actual lifecycle facts were delivered.
    });
    let result = RemoteChild::submit(
        &endpoint,
        &request(),
        no_delegations(),
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Null,
        },
        deadline(),
    );
    let mut child = match result {
        Err(SubmitError::Unknown { child, .. }) => *child,
        other => panic!("expected unknown custody, got {:?}", other.err()),
    };
    server.join().unwrap();
    assert!(seen.load(Ordering::SeqCst));
    assert!(matches!(
        child.wait_until(deadline()).unwrap(),
        Terminal::Finished(_)
    ));
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(output, "actual output");
    // Receiver closed: this must be EPIPE, never process-fatal SIGPIPE.
    aterm_uds::deadline::once_released(|| {
        if aterm_uds::hangup::peer_closed(&child.stdin.as_ref().unwrap().0) {
            Ok(())
        } else {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    });
    assert!(child.stdin.as_mut().unwrap().write_all(b"closed").is_err());
}
#[test]
fn host_authorization_refuses_before_rights_and_inherit_null_do_not_create_pipes() {
    let (_temp, listener, endpoint) = fixture();
    let server = std::thread::spawn(move || {
        let stream = accept(&listener);
        hello(&stream);
        assert!(receive(&stream, &binding(1), deadline(), |_, _| false).is_err());
    });
    assert!(matches!(
        RemoteChild::submit(
            &endpoint,
            &request(),
            no_delegations(),
            StdioConfig {
                stdin: Stdio::Null,
                stdout: Stdio::Inherit,
                stderr: Stdio::Inherit
            },
            deadline()
        ),
        Err(SubmitError::NotSubmitted(_))
    ));
    server.join().unwrap();
    let (_, input) = prepare_input(Stdio::Null).unwrap();
    assert!(input.is_none());
    let (_, output) = prepare_output(Stdio::Inherit, 1).unwrap();
    assert!(output.is_none());
}

#[test]
fn delegated_cpu_gnu_and_fences_keep_original_open_descriptions() {
    use std::io::{Seek as _, SeekFrom};
    let (temp, listener, endpoint) = fixture();
    let mut cpu = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temp.path().join("cpu"))
        .unwrap();
    let mut fence = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temp.path().join("fence"))
        .unwrap();
    cpu.write_all(b"cpu").unwrap();
    cpu.seek(SeekFrom::Start(0)).unwrap();
    fence.write_all(b"fence").unwrap();
    fence.seek(SeekFrom::Start(0)).unwrap();
    let (read, write) = aterm_uds::pipe().unwrap();
    let delegated = Delegations {
        cpu: Some(aterm_uds::kept_out(|| cpu.as_fd().try_clone_to_owned()).unwrap()),
        gnu: vec![read.into(), write.into()],
        fences: vec![aterm_uds::kept_out(|| fence.as_fd().try_clone_to_owned()).unwrap()],
    };
    let mut r = request();
    r.cpu = wire::CpuDelegation::SharedBorrow { target_fd: 8 };
    r.gnu = wire::GnuTransport::NumericPipe {
        read_target: 4,
        write_target: 5,
    };
    r.fence_targets = vec![9];
    let server = std::thread::spawn(move || {
        let stream = accept(&listener);
        hello(&stream);
        let mut authorized = receive(&stream, &binding(1), deadline(), |_, _| true).unwrap();
        let mut byte = [0];
        File::from(authorized.rights.delegated.cpu.take().unwrap())
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(byte, *b"c");
        File::from(authorized.rights.delegated.fences.pop().unwrap())
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(byte, *b"f");
        let mut gnu = authorized.rights.delegated.gnu.into_iter();
        let mut read = File::from(gnu.next().unwrap());
        let mut write = File::from(gnu.next().unwrap());
        write.write_all(b"+").unwrap();
        read.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"+");
        acknowledge(&stream, deadline()).unwrap();
        let events = CtlStream::from(authorized.rights.events);
        for event in [started(), finished()] {
            transport::send_event(
                &events,
                &admission::EventFrame {
                    binding: binding(2),
                    event,
                },
                deadline(),
            )
            .unwrap();
        }
    });
    let mut child = RemoteChild::submit(
        &endpoint,
        &r,
        delegated,
        StdioConfig {
            stdin: Stdio::Null,
            stdout: Stdio::Null,
            stderr: Stdio::Null,
        },
        deadline(),
    )
    .unwrap();
    assert!(matches!(
        child.wait_until(deadline()).unwrap(),
        Terminal::Finished(_)
    ));
    server.join().unwrap();
    assert_eq!(cpu.stream_position().unwrap(), 1);
    assert_eq!(fence.stream_position().unwrap(), 1);
}

#[test]
fn exact_134_right_bundle_preserves_all_original_fences_without_launch_claim() {
    use std::io::{Seek as _, SeekFrom};
    // Both sender and receiver coexist in this one fixture process. Raise only its
    // soft limit within the OS hard ceiling, as the real native custodian does.
    aterm_uds::spawnfd::raise_soft_nofile();
    let (temp, listener, endpoint) = fixture();
    let mut source = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temp.path().join("held"))
        .unwrap();
    source.write_all(&[b'+'; 128]).unwrap();
    source.seek(SeekFrom::Start(0)).unwrap();
    let duplicate = || aterm_uds::kept_out(|| source.as_fd().try_clone_to_owned()).unwrap();
    let (read, write) = aterm_uds::pipe().unwrap();
    let delegated = Delegations {
        cpu: Some(duplicate()),
        gnu: vec![read.into(), write.into()],
        fences: (0..wire::MAX_FENCES).map(|_| duplicate()).collect(),
    };
    let mut r = request();
    r.cpu = wire::CpuDelegation::SharedBorrow { target_fd: 8 };
    r.gnu = wire::GnuTransport::NumericPipe {
        read_target: 4,
        write_target: 5,
    };
    r.fence_targets = (10..10 + wire::MAX_FENCES as u32).collect();
    assert_eq!(r.fd_count().unwrap(), wire::MAX_RIGHTS);
    let server = std::thread::spawn(move || {
        let stream = accept(&listener);
        hello(&stream);
        let mut authorized = receive(&stream, &binding(1), deadline(), |_, _| true).unwrap();
        assert_eq!(authorized.rights.borrowed().len(), 134);
        assert_eq!(authorized.rights.delegated.gnu.len(), 2);
        assert_eq!(authorized.rights.delegated.fences.len(), 127);
        let cpu = authorized.rights.delegated.cpu.take().unwrap();
        for fd in std::iter::once(cpu).chain(authorized.rights.delegated.fences.drain(..)) {
            let mut byte = [0];
            File::from(fd).read_exact(&mut byte).unwrap();
            assert_eq!(byte, *b"+");
        }
        acknowledge(&stream, deadline()).unwrap();
        transport::send_event(
            &CtlStream::from(authorized.rights.events),
            &admission::EventFrame {
                binding: binding(2),
                event: admission::Event::Refused {
                    reason: "fixture accepted attachment only".into(),
                },
            },
            deadline(),
        )
        .unwrap();
    });
    let mut child = RemoteChild::submit(
        &endpoint,
        &r,
        delegated,
        StdioConfig {
            stdin: Stdio::Null,
            stdout: Stdio::Null,
            stderr: Stdio::Null,
        },
        deadline(),
    )
    .unwrap();
    assert!(child.identity().is_none());
    assert!(matches!(
        child.wait_until(deadline()).unwrap(),
        Terminal::Refused { .. }
    ));
    assert!(child.identity().is_none());
    server.join().unwrap();
    assert_eq!(source.stream_position().unwrap(), 128);
}

#[test]
fn routed_hello_cannot_authorize_a_different_binary_job() {
    let (_temp, listener, endpoint) = fixture();
    let routed = binding(3);
    let greeting = wire::hello(&routed).unwrap();
    assert!(
        String::from_utf8(greeting.clone())
            .unwrap()
            .contains("\"attempt\":\"03030303030303030303030303030303\"")
    );
    assert!(
        wire::hello(&admission::Binding {
            session: "s-\"bad".into(),
            attempt: [3; 16]
        })
        .is_err()
    );
    let server = std::thread::spawn(move || {
        let stream = accept(&listener);
        assert_eq!(
            transport::read_frame(&stream, deadline()).unwrap(),
            greeting
        );
        transport::write_frame(&stream, wire::PREPARE, deadline()).unwrap();
        // The runtime selected this existing job from the untrusted greeting.
        assert!(
            receive(&stream, &routed, deadline(), |_, _| panic!(
                "mismatched job reached authorization"
            ))
            .is_err()
        );
    });
    let stream = aterm_uds::connect::until(&endpoint.path, deadline()).unwrap();
    pin(&stream, &endpoint).unwrap();
    transport::write_frame(&stream, &wire::hello(&binding(3)).unwrap(), deadline()).unwrap();
    assert_eq!(
        transport::read_frame(&stream, deadline()).unwrap(),
        wire::PREPARE
    );
    transport::write_frame(&stream, &request().encode().unwrap(), deadline()).unwrap();
    assert!(transport::read_frame(&stream, deadline()).is_err());
    server.join().unwrap();
}
