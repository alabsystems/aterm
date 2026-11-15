// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_ctl::resource_admission_transport as wire;
use aterm_uds::CtlStream;
use std::{
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

fn issued_over_enrolled_protocol() -> String {
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    client.set_nonblocking(true).unwrap();
    server.set_nonblocking(true).unwrap();
    let expected = wire::local_peer(&client).unwrap();
    let worker = std::thread::spawn(move || {
        crate::forward::serve(&server, |peer| {
            if peer == expected {
                Ok(())
            } else {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        })
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    wire::write_frame(&client, &[9], deadline).unwrap();
    let reply = crate::runtime::read_json(&client, deadline).unwrap();
    let permit = reply["permit"].as_str().unwrap().to_owned();
    drop(client);
    assert!(worker.join().unwrap().is_err());
    permit
}

#[test]
fn a_handed_off_observer_permit_cannot_authorize_submit_or_issue_more_authority() {
    let permit = issued_over_enrolled_protocol();
    for op in [2, 4, 5, 6, 7, 9] {
        let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let peer = wire::local_peer(&client).unwrap();
        let hello = aterm_json::json!({"version":3,"role":"resource-observer-v1","permit":permit});
        let worker = std::thread::spawn(move || {
            crate::runtime::serve_observer_hello(
                &server,
                peer,
                &hello,
                Arc::new(AtomicUsize::new(0)),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        assert_eq!(wire::read_frame(&client, deadline).unwrap(), [1]);
        let mut read = vec![8];
        read.extend(u64::MAX.to_be_bytes());
        wire::write_frame(&client, &read, deadline).unwrap();
        let observation = crate::resource_observation::Observation::receive(u64::MAX, || {
            crate::runtime::read_json(&client, deadline)
        })
        .unwrap();
        assert_eq!(
            observation.host.pid,
            std::process::id(),
            "the valid permit grants observations only"
        );
        let submission = aterm_types::resource_admission::Submission {
            binding: aterm_types::resource_admission::Binding {
                session: aterm_types::tab_id::TabId::from_bits(7).to_string(),
                attempt: [7; 16],
            },
            peer_pid: peer.pid,
            peer_birth: peer.birth,
            argv: vec![b"/usr/bin/true".to_vec()],
            cwd: b"/".to_vec(),
            env: Vec::new(),
            cpu_units: 1,
            memory_bytes: 1,
            label: "observer write attempt".into(),
            timeout_ms: None,
        };
        let mut frame = vec![op];
        if matches!(op, 2 | 4 | 5 | 7) {
            frame.extend(submission.encode().unwrap());
        }
        wire::write_frame(&client, &frame, deadline).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "a syntactically valid execution command must be denied before actor admission"
        );
    }
}

#[test]
fn an_unknown_permit_never_gets_a_successful_observer_handshake() {
    let mut permit = issued_over_enrolled_protocol().into_bytes();
    permit[0] = if permit[0] == b'a' { b'b' } else { b'a' };
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let peer = wire::local_peer(&client).unwrap();
    let hello = aterm_json::json!({"version":3,"role":"resource-observer-v1","permit":String::from_utf8(permit).unwrap()});
    assert!(
        crate::runtime::serve_observer_hello(&server, peer, &hello, Arc::new(AtomicUsize::new(0)))
            .is_err()
    );
}
