// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

#[test]
fn native_proxy_submission_reads_its_original_frame_after_scheduling_delay() {
    let (sender, receiver) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    let deadline = submission_deadline(Instant::now() - Duration::from_secs(9));
    transport::write_frame(
        &sender,
        subgroup::READY,
        crate::runtime::control_deadline(Instant::now()),
    )
    .unwrap();
    assert_eq!(
        transport::read_frame(&receiver, deadline).unwrap(),
        subgroup::READY
    );
    transport::write_frame(
        &sender,
        subgroup::READY,
        crate::runtime::control_deadline(Instant::now()),
    )
    .unwrap();
    assert!(
        transport::read_frame(
            &receiver,
            submission_deadline(Instant::now() - Duration::from_secs(61))
        )
        .is_err()
    );
}
