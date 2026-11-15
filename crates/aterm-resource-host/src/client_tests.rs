// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

#[test]
fn only_positive_kernel_lifetime_proof_retires_a_restored_source() {
    use aterm_uds::process::{Birth, Identity, ProcessPresence};
    let peer = Peer {
        pid: 200,
        birth: 10,
    };
    let same = Identity {
        pid: 200,
        birth: Birth(10),
        idversion: Some(1),
    };
    let reused = Identity {
        birth: Birth(11),
        ..same
    };
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        assert!(!source_lifetime_ended(peer, None, None, presence));
        assert!(!source_lifetime_ended(
            peer,
            Some(same),
            Some(same),
            presence
        ));
        assert!(!source_lifetime_ended(
            peer,
            Some(same),
            Some(reused),
            presence
        ));
    }
    assert!(source_lifetime_ended(
        peer,
        Some(reused),
        Some(reused),
        ProcessPresence::Present
    ));
    for presence in [ProcessPresence::Absent, ProcessPresence::Exited] {
        assert!(source_lifetime_ended(peer, None, None, presence));
    }
}

#[test]
fn an_ended_restored_source_releases_its_lane_and_reads_a_successors_first_receipt() {
    use crate::resource_observation::{Observation, ProcessKey, SessionUsage, Terminal};
    for ended_before_read in [true, false] {
        let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        server.set_nonblocking(true).unwrap();
        let current = wire::local_peer(&client).unwrap();
        let host_start = aterm_uds::process::read_process_start(current.pid as i32)
            .unwrap()
            .boot
            .0;
        let forwarder = Forwarder::new(client, current).unwrap();
        let source = observer_context(&aterm_json::json!({"version":1,"pid":current.pid,
            "birth":current.birth + 1,"permit":"a".repeat(64)}))
        .unwrap();
        assert!(
            source.instance.is_none(),
            "intake retains retryable context before live provenance exists"
        );
        assert!(
            restored_source_ended(&source),
            "the current native generation disproves the handed-off lifetime"
        );
        let host = ProcessKey {
            pid: current.pid,
            start: host_start,
        };
        let response = Observation {
            host,
            host_child_cpu_ns: Some(10),
            at_unix_ms: 1,
            originals: Vec::new(),
            outstanding_sessions: Vec::new(),
            first_seq: 1,
            next_seq: 1,
            sessions: vec![SessionUsage {
                session: "tacde112200".into(),
                reaped_cpu_ns: 10,
                unread: 0,
            }],
            terminals: vec![Terminal {
                seq: 1,
                session: "tacde112200".into(),
                process: ProcessKey { pid: 200, start: 2 },
                uid: Some(501),
                coalition: None,
                version: None,
                cpu_ns: Some(10),
                child_cpu_ns: Some(0),
                wait_cpu_ns: Some(10),
                reaps_unwaited: Some(false),
                at_unix_ms: 1,
                exit_cut_ns: Some(1),
            }],
            unproven_active: 0,
            lost_origins: 0,
        };
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            assert_eq!(wire::read_frame(&server, deadline).unwrap(), [1]);
            runtime::write_json(
                &server,
                &aterm_json::json!({"resource_observation":"origin-wait-v1"}),
                deadline,
            )
            .unwrap();
            let request = wire::read_frame(&server, deadline).unwrap();
            assert_eq!(request[0], 8);
            assert_eq!(
                u64::from_be_bytes(request[1..].try_into().unwrap()),
                0,
                "the old source's cursor must never skip a successor's first final"
            );
            for frame in response.frames() {
                runtime::write_json(&server, &frame, deadline).unwrap();
            }
        });
        let checks = std::cell::Cell::new(0);
        let retired = std::cell::Cell::new(false);
        let read = observe_restored(
            &source,
            999,
            |source| {
                checks.set(checks.get() + 1);
                (ended_before_read || checks.get() > 1) && restored_source_ended(source)
            },
            |_| {
                retired.set(true);
                true
            },
            |_, _| Err(io::ErrorKind::ConnectionReset.into()),
            |since| forwarder.resource_observation(since),
        );
        assert!(read.source_ended && retired.get());
        assert_eq!(read.result.unwrap().terminals[0].seq, 1);
        worker.join().unwrap();
    }
}

#[test]
fn transient_observer_errors_preserve_restored_authority_and_cursor_binding() {
    let native = aterm_uds::process::read_identity(std::process::id() as i32).unwrap();
    let peer = Peer {
        pid: native.pid as u32,
        birth: native.birth.0,
    };
    let source = observer_context(&aterm_json::json!({"version":1,"pid":peer.pid,
        "birth":peer.birth,"permit":"a".repeat(64)}))
    .unwrap();
    let retired = std::cell::Cell::new(false);
    let read = observe_restored(
        &source,
        44,
        |_| {
            source_lifetime_ended(
                peer,
                Some(native),
                Some(native),
                aterm_uds::process::ProcessPresence::Present,
            )
        },
        |_| {
            retired.set(true);
            true
        },
        |_, since| {
            assert_eq!(since, 44);
            Err(io::ErrorKind::PermissionDenied.into())
        },
        |_| panic!("a temporary provenance failure must not select a different source"),
    );
    assert!(!read.source_ended && !retired.get());
    assert_eq!(
        read.result.unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(cursor_for_source(Some(peer), peer, 44), 44);
    assert_eq!(cursor_for_source(None, peer, 44), 0);
    assert_eq!(
        cursor_for_source(
            Some(peer),
            Peer {
                birth: peer.birth + 1,
                ..peer
            },
            44
        ),
        0
    );
}

#[test]
fn optional_observer_authority_cannot_refuse_a_successful_execution_enrollment() {
    assert!(gui_ack_permit(&[1]).unwrap().is_none());
    assert!(gui_ack_permit(&[1, 255]).unwrap().is_none());
    let mut ack = vec![1];
    ack.extend([b'a'; 64]);
    let permit = gui_ack_permit(&ack).unwrap().unwrap();
    assert_eq!(permit.len(), 64);
    assert!(gui_ack_permit(&[0]).is_err());
    assert!(gui_ack_permit(&[]).is_err());
}

#[test]
fn native_startup_status_survives_scheduling_delay() {
    let (status, sender) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let peer = wire::local_peer(&status).unwrap();
    runtime::write_json(
        &sender,
        &aterm_json::json!({"version":3,"pid":peer.pid,"birth":peer.birth}),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let (born, _) =
        startup_status(&status, peer.pid, Instant::now() - Duration::from_secs(11)).unwrap();
    assert_eq!(born["birth"].as_u64(), Some(peer.birth));
    assert_eq!(
        startup_status(&status, peer.pid, Instant::now() - Duration::from_secs(61))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    for (version, delta) in [(4, 0), (3, 1)] {
        let (status, sender) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let peer = wire::local_peer(&status).unwrap();
        runtime::write_json(
            &sender,
            &aterm_json::json!({"version":version,"pid":peer.pid + delta,"birth":peer.birth}),
            runtime::control_deadline(Instant::now()),
        )
        .unwrap();
        assert_eq!(
            startup_status(&status, peer.pid, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
