// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::Write;

const SID: &str = "t0f1xtvre00";
const OPERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn expectation() -> Expectation {
    Expectation {
        sid: SID.into(),
        primary: "00000000-0000-4000-8000-000000000001".into(),
        source_build: 2026111701,
        build: 2026111801,
        pid: 42,
        birth: 100,
    }
}

fn native(expected: &Expectation, generation: i32) -> NativeObservation<'_> {
    NativeObservation {
        sid: &expected.sid,
        operation: OPERATION,
        primary: &expected.primary,
        source_build: expected.source_build,
        build: expected.build,
        pid: expected.pid,
        birth: expected.birth,
        generation,
    }
}

#[test]
fn settlement_ack_loss_matches_one_exact_commit_and_ordinary_eof() {
    let expected = expectation();
    let fixture = Arc::new(Fault::new(SID.into()));
    let request = format!("@{SID} native-act settle source-exec operation={OPERATION}");
    let reply = format!(
        "OK native source-exec state=settled operation={OPERATION} primary={} build={}\n",
        expected.primary, expected.build
    );
    assert!(fixture.select(&request, expected.pid, 81).is_none());
    fixture.arm(expected.clone()).unwrap();
    assert!(fixture.select(&request, expected.pid, 81).is_none());
    record_prepare(native(&expected, 1));
    assert!(fixture.select(&request, expected.pid, 81).is_none());
    observe_dispatch(&fixture, || {
        record_prepare(NativeObservation {
            birth: expected.birth + 1,
            ..native(&expected, 1)
        });
    });
    assert!(fixture.select(&request, expected.pid, 81).is_none());
    observe_dispatch(&fixture, || record_prepare(native(&expected, 1)));
    observe_dispatch(&fixture, || {
        record_prepare(NativeObservation {
            operation: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ..native(&expected, 1)
        });
    });
    assert!(fixture.select(&request, expected.pid + 1, 81).is_none());
    assert!(
        fixture
            .select(&request.replace("settle ", "settled "), expected.pid, 81)
            .is_none()
    );
    let attempt = fixture.select(&request, expected.pid, 81).unwrap();
    let (_, foreign) = witness(&attempt, || {
        record_commit(SID, OPERATION, expected.pid, 82, 2)
    });
    assert_eq!(foreign, None);
    let (_, committed) = witness(&attempt, || {
        record_commit(SID, OPERATION, expected.pid, 81, 2)
    });
    assert_eq!(committed, Some(Witness::Commit(2)));
    assert!(!fixture.hold(&attempt, reply.replace("settled", "waiting").as_bytes(), 2));
    assert!(!fixture.hold(
        &attempt,
        reply.replace(&expected.build.to_string(), "1").as_bytes(),
        2,
    ));
    assert!(!fixture.hold(&attempt, reply.as_bytes(), 1));
    assert!(fixture.hold(&attempt, reply.as_bytes(), 2));
    assert!(!fixture.hold(&attempt, reply.as_bytes(), 2));
    assert!(fixture.observation().unwrap().cleanup.is_none());

    let (_, foreign) = witness(&attempt, || {
        record_eof(SID, OPERATION, expected.pid, 82, 4, 100)
    });
    assert_eq!(foreign, None);
    let (_, retained) = witness(&attempt, || {
        record_eof(SID, OPERATION, expected.pid, 81, 4, 100)
    });
    let Some(Witness::Eof(claim)) = retained else {
        panic!("the exact SDK socket must match the cleanup witness");
    };
    fixture.record_cleanup(&attempt, Some(claim));
    observe_dispatch(&fixture, || {
        record_readback(native(&expected, 2), 81);
        record_readback(native(&expected, 3), 82);
        record_readback(
            NativeObservation {
                operation: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ..native(&expected, 2)
            },
            82,
        );
    });
    assert!(fixture.observation().unwrap().readback.is_none());
    observe_dispatch(&fixture, || record_readback(native(&expected, 2), 82));

    let observed = fixture.observation().unwrap();
    assert_eq!(observed.attempt.connection, 81);
    assert_eq!(observed.commits, 1);
    assert_eq!(observed.settle_requests, 1);
    let recovered = observed.readback.unwrap();
    assert_eq!(recovered.operation, OPERATION);
    assert_eq!(recovered.connection, 82);
    assert_eq!(recovered.generation, 2);
    assert_eq!(recovered.state, "settled");
    assert_eq!(
        observed.cleanup.unwrap().retained_claim,
        Some(RetainedClaim {
            revision: 4,
            expires_us: 100
        })
    );
    assert!(fixture.select(&request, expected.pid, 82).is_none());
    assert_eq!(fixture.observation().unwrap().settle_requests, 2);
}

#[test]
fn settlement_ack_loss_requires_peer_eof_on_the_held_socket() {
    let deadline = Instant::now() + super::super::MUST_HAPPEN;
    let running = AtomicBool::new(false);

    let (socket, peer) = aterm_uds::CtlStream::pair().unwrap();
    drop(peer);
    assert!(await_peer_eof(&socket, false, &running, deadline).is_ok());

    let (socket, mut peer) = aterm_uds::CtlStream::pair().unwrap();
    peer.write_all(b"x").unwrap();
    drop(peer);
    assert!(await_peer_eof(&socket, false, &running, deadline).is_err());

    let (socket, _peer) = aterm_uds::CtlStream::pair().unwrap();
    assert!(await_peer_eof(&socket, true, &running, deadline).is_err());
    let stopping = AtomicBool::new(true);
    socket.shutdown(std::net::Shutdown::Both).unwrap();
    assert!(await_peer_eof(&socket, false, &stopping, deadline).is_err());
}
