// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use trust_resource_controller::workload::host::bank;

#[test]
fn full_bank_queue_is_typed_pending_but_closed_transport_is_error() {
    let (link, driver) = Link::pair();
    let request = bank::Request {
        key: bank::Key {
            root_attempt: [1; 16],
            invocation_sha256: [2; 32],
            sequence: 1,
        },
        operation: bank::Operation::Returned {
            member_attempt: [3; 16],
            loan: [4; 16],
            slot: 0,
        },
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut originals = Vec::new();
    for _ in 0..128 {
        let pending = link.try_begin(request.clone(), deadline).unwrap().unwrap();
        assert!(pending.poll().unwrap().is_none());
        originals.push(pending);
    }
    assert!(link.try_begin(request.clone(), deadline).unwrap().is_none());
    let accepted = driver.receive.try_recv().unwrap();
    assert_eq!(accepted.request, request);
    assert_eq!(accepted.deadline, deadline);
    drop(accepted);
    assert!(link.try_begin(request.clone(), deadline).unwrap().is_some());
    drop(driver);
    assert_eq!(
        link.try_begin(request, deadline).err().unwrap().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        originals[0].poll().err().unwrap().kind(),
        io::ErrorKind::BrokenPipe
    );
}
