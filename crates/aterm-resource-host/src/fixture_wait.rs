// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

pub(crate) const HANG: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) fn deadline(started: std::time::Instant) -> std::time::Instant {
    started + HANG
}

#[test]
fn native_fixture_frame_waits_for_scheduling_delay() {
    let (sender, receiver) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let frame_deadline = deadline(std::time::Instant::now() - std::time::Duration::from_secs(31));
    aterm_ctl::resource_admission_transport::write_frame(
        &sender,
        b"original fixture frame",
        deadline(std::time::Instant::now()),
    )
    .unwrap();
    assert_eq!(
        aterm_ctl::resource_admission_transport::read_frame(&receiver, frame_deadline).unwrap(),
        b"original fixture frame"
    );
    aterm_ctl::resource_admission_transport::write_frame(
        &sender,
        b"expired fixture frame",
        deadline(std::time::Instant::now()),
    )
    .unwrap();
    assert!(
        aterm_ctl::resource_admission_transport::read_frame(
            &receiver,
            deadline(std::time::Instant::now() - std::time::Duration::from_secs(61))
        )
        .is_err()
    );
}
