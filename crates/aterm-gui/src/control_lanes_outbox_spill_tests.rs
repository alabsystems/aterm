// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CtlStream, Outbox, SPILL_FROM, Segment};
use std::io::Read as _;

#[cfg(debug_assertions)]
#[test]
fn source_fixture_requires_actual_exact_cleanup_reply() {
    let request = "@self native-act codex-auth member-retire-owned schema=1 request=req operation=op member=member";
    let mut out = Outbox::default();
    assert!(out.fixture_source_outcome(request).is_none());
    out.push_owned(b"WAIT codex-auth member-retire-owned\n".to_vec());
    assert!(out.fixture_source_outcome(request).is_none());
    out.segments.clear();
    out.push_owned(b"OK codex-auth member-retire-owned {\"schema\":1,\"request\":\"req\",\"operation\":\"op\",\"member\":\"member\"}\n".to_vec());
    assert!(out.fixture_source_outcome(request).is_some());
    assert!(
        out.fixture_source_outcome(&request.replace("request=req", "request=foreign"))
            .is_none()
    );
    assert!(
        out.fixture_source_outcome(&request.replace("member=member", "member=foreign"))
            .is_none()
    );
    out.segments.clear();
    let ready = format!(
        "member-rebound-ready schema=1 request=req operation=op member=member registration=reg preload_sequence=seq credential_generation=4 digest={} account={} user={} auth_generation=1 owner_generation=2 source_generation=3 login_generation=4",
        "a".repeat(64),
        "b".repeat(64),
        "c".repeat(64)
    );
    out.push_owned(b"OK codex-auth member-rebound-ready {\"schema\":1,\"request\":\"req\",\"operation\":\"op\",\"member\":\"member\",\"registration\":\"reg\",\"preload_sequence\":\"seq\"}\n".to_vec());
    let event = out.fixture_source_outcome(&ready).unwrap();
    assert_eq!(event["credential_generation"].as_u64(), Some(4));
    assert_eq!(event["report"], aterm_json::json!([1, 2, 3, 4]));
    for (field, original) in [
        ("digest", "a".repeat(64)),
        ("account", "b".repeat(64)),
        ("user", "c".repeat(64)),
        ("auth_generation", "1".into()),
    ] {
        let changed = ready.replace(
            &format!("{field}={original}"),
            &format!(
                "{field}={}",
                if field == "auth_generation" {
                    "9".into()
                } else {
                    "d".repeat(64)
                }
            ),
        );
        let other = out.fixture_source_outcome(&changed).unwrap();
        assert_ne!(
            event["body_digest"], other["body_digest"],
            "changed {field} cannot replay the same semantic Ready body"
        );
    }
    assert!(
        out.fixture_source_outcome(&format!("{ready} digest={}", "d".repeat(64)))
            .is_none()
    );

    assert!(
        out.fixture_source_outcome(&ready.replace("registration=reg", "registration=foreign"))
            .is_none()
    );
    assert!(
        out.fixture_source_outcome(
            &ready.replace("preload_sequence=seq", "preload_sequence=foreign")
        )
        .is_none()
    );
}

#[test]
fn a_large_unsent_answer_leaves_the_heap_and_arrives_whole() {
    let (server, mut peer) = CtlStream::pair().expect("private control pair");
    let hang_detector = Some(aterm_spec::wait::MUST_HAPPEN);
    server.set_write_timeout(hang_detector).unwrap();
    peer.set_read_timeout(hang_detector).unwrap();
    let answer: Vec<u8> = (0..256 * 1024_u32)
        .map(|i| u8::try_from(i % 251).expect("below 251"))
        .collect();
    let mut out = Outbox::default();
    out.push_owned(answer[..1024].to_vec());
    out.push_owned(answer[1024..].to_vec());
    server.set_nonblocking(true).unwrap();
    assert!(
        !out.deliver(&server, true).unwrap(),
        "the socket takes part of it"
    );
    let unsent = out
        .segments
        .iter()
        .map(|segment| segment.as_bytes().len())
        .sum::<usize>()
        - out.sent;
    out.spill();
    assert!(
        out.segments.len() == 1 && matches!(out.segments.front(), Some(Segment::Spilled(_))),
        "one mapping, no heap segment left"
    );
    assert_eq!(out.held(), unsent, "the mapping holds the unsent rest");
    assert!(out.owed());
    let reader = std::thread::spawn(move || {
        let mut got = vec![0u8; answer.len()];
        peer.read_exact(&mut got).expect("the whole answer");
        (answer, got)
    });
    server.set_nonblocking(false).unwrap();
    assert!(out.deliver(&server, false).unwrap());
    assert!(out.segments.is_empty(), "the mapping goes once delivered");
    let (answer, got) = reader.join().expect("the reader");
    assert!(got == answer, "every byte, in order");

    let mut small = Outbox::default();
    small.push_owned(vec![b'x'; SPILL_FROM - 1]);
    small.spill();
    assert!(
        small
            .segments
            .iter()
            .all(|segment| matches!(segment, Segment::Bytes(_))),
        "a short answer stays on the heap"
    );
    let mut secret = Outbox::default();
    secret.push_secret(
        aterm_uds::sudo::WireBuffer::from_bytes(b"OK askpass secret=74657374\n")
            .expect("a short line"),
    );
    secret.push_owned(vec![b'x'; SPILL_FROM]);
    secret.spill();
    assert!(
        secret
            .segments
            .iter()
            .all(|segment| !matches!(segment, Segment::Spilled(_)))
            && matches!(secret.segments.front(), Some(Segment::Secret(_))),
        "an answer holding a secret keeps it in its guarded storage"
    );
}
