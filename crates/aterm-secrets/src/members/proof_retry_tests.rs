// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::bootstrap::Device;

#[test]
fn restart_and_route_wakes_keep_the_same_member_request_and_pause() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    crate::client::machine_identity(&paths).unwrap();
    let device = Device::open(&paths).unwrap().unwrap();
    let joining = Joining {
        device: &device,
        records_bound: crate::client::records_bound(
            &paths,
            &crate::client::load_records(&paths).unwrap(),
        )
        .unwrap(),
        node: [7; 32],
        name: "newcomer".into(),
        model: String::new(),
    };
    let request = device.request([1; 16], [2; 32]).unwrap();
    let proving = ProofLock::take(&paths, None).unwrap().unwrap();
    assert!(matches!(
        before(&paths, &proving, &joining, 100).unwrap(),
        Round::Fresh
    ));
    sending(
        &paths,
        &proving,
        &joining,
        "127.0.0.1:7",
        [3; 32],
        request.binding(),
        100,
    )
    .unwrap();
    drop(proving);
    let restarted = Paths::for_home(root.path());
    let proving = ProofLock::take(&restarted, None).unwrap().unwrap();
    assert!(matches!(
        before(&restarted, &proving, &joining, 129).unwrap(),
        Round::Cooling
    ));
    let Round::Pinned(peer) = before(&restarted, &proving, &joining, 130).unwrap() else {
        panic!("pin lost across restart")
    };
    assert_eq!(peer.key, [3; 32]);
    assert_eq!(peer.binding, *request.binding());
    assert!(
        sending(
            &restarted,
            &proving,
            &joining,
            "127.0.0.1:8",
            [4; 32],
            request.binding(),
            130
        )
        .is_err()
    );
    assert!(matches!(
        before(&restarted, &proving, &joining, 131).unwrap(),
        Round::Cooling
    ));
    clear(&restarted, &proving).unwrap();
    assert!(matches!(
        before(&restarted, &proving, &joining, 131).unwrap(),
        Round::Fresh
    ));
}
