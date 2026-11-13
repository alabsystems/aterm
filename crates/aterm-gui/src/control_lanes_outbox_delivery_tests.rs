// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CtlStream, Outbox, Segment};
use std::io::Read as _;

#[test]
fn a_secret_answer_is_delivered_whole_and_kept_nowhere() {
    let (server, mut peer) = CtlStream::pair().expect("private control pair");
    let hang_detector = Some(aterm_spec::wait::MUST_HAPPEN);
    server.set_write_timeout(hang_detector).unwrap();
    peer.set_read_timeout(hang_detector).unwrap();
    let reply = b"OK askpass secret=74657374\n";
    let mut out = Outbox::default();
    out.push_secret(aterm_uds::sudo::WireBuffer::from_bytes(reply).expect("a short line"));
    assert!(
        out.segments
            .iter()
            .all(|segment| matches!(segment, Segment::Secret(_))),
        "the secret is moved into its own segment, never copied into a byte one"
    );

    assert!(out.deliver(&server, false).unwrap());
    let mut received = vec![0; reply.len()];
    peer.read_exact(&mut received).unwrap();
    assert_eq!(received, reply, "delivery must preserve every wire byte");
    assert!(out.segments.is_empty(), "a delivered segment is dropped");
    assert_eq!(out.sent, 0);
}
