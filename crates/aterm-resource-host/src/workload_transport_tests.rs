// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use trust_resource_controller::workload::host::{bank::Key, phase};

#[test]
fn a_native_workload_frame_survives_scheduling_delay() {
    let (client, server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let peer = transport::local_peer(&server).unwrap();
    let request = phase::Request {
        key: Key {
            root_attempt: [1; 16],
            invocation_sha256: [2; 32],
            sequence: 1,
        },
        operation: phase::Operation::Drained,
    };
    let root_deadline = Instant::now() + Duration::from_secs(60);
    transport::write_frame(&client, &request.encode().unwrap(), root_deadline).unwrap();
    let limit = incoming_deadline(root_deadline, Instant::now() - Duration::from_secs(1));
    let received = transport::read_frame(&server, limit).unwrap();
    assert_eq!(phase::Request::decode(&received).unwrap(), request);
    assert_eq!(transport::local_peer(&server).unwrap(), peer);
    assert!(phase::Request::decode(&[0]).is_err());
    transport::write_frame(&client, &request.encode().unwrap(), root_deadline).unwrap();
    let expired = incoming_deadline(Instant::now(), Instant::now());
    assert_eq!(
        transport::read_frame(&server, expired).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}
