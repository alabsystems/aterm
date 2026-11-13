// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

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
