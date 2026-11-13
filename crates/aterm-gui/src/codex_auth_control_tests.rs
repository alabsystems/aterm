// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn source_waiting_contains_only_caller_and_operation_correlation() {
    let caller = Caller {
        proof: crate::self_claim::Proven {
            pid: 42,
            start: "Thu Jan  1 00:00:00 1970".into(),
        },
        process: Process {
            pid: 42,
            birth: 123,
            parent: 2,
            group: 42,
            session: 42,
            image: Image {
                path: b"/private/codex".to_vec(),
                device: 1,
                inode: 2,
            },
        },
    };
    let operation = "a".repeat(32);
    let reply = source_reply(
        "s-cafe",
        &caller,
        "epoch",
        &operation,
        slot::SourceProgress::Waiting,
    )
    .unwrap();
    let json: aterm_json::Value = aterm_json::from_str(
        reply
            .strip_prefix("OK codex-auth source-waiting ")
            .unwrap()
            .trim_end(),
    )
    .unwrap();
    assert_eq!(json.as_object().unwrap().len(), 3);
    assert_eq!(json["operation"].as_str(), Some(operation.as_str()));
    assert_eq!(json["session"]["process_pid"].as_u64(), Some(42));
    assert_eq!(
        json["session"]["process_birth"].as_str(),
        Some(caller.proof.start.as_str())
    );
    assert!(!reply.contains("/private/codex"));
    let mut replaced = caller.clone();
    replaced.process.image.inode += 1;
    assert!(replaced != caller);
    replaced = caller.clone();
    replaced.process.birth += 1;
    assert!(replaced != caller);
}

#[test]
fn requests_bind_one_exact_operation_and_accept_no_credential_paths() {
    let operation = "a".repeat(32);
    let digest = "b".repeat(64);
    let source = format!("source operation={operation}");
    let refresh = format!("refresh slot=6162 operation={operation} expected_digest={digest}");
    assert!(matches!(
        Request::parse(&source),
        Some(Request::Source { .. })
    ));
    assert!(matches!(
        Request::parse(&refresh),
        Some(Request::Refresh { .. })
    ));
    for invalid in [
        format!("{source} path=/private/credential"),
        format!("{source} operation={operation}"),
        source.replace(&operation, &"A".repeat(32)),
        source.replace(&operation, "short"),
        refresh.replace("slot=6162", "slot=/private/credential"),
        refresh.replace("slot=6162", "slot=abc"),
        refresh.replace("slot=6162", "slot="),
        refresh.replace(&digest, &"b".repeat(63)),
        format!("{refresh} state=ready"),
        format!("source operation={}", "a".repeat(513)),
    ] {
        assert!(Request::parse(&invalid).is_none());
    }
}

#[test]
fn refresh_reply_preserves_six_field_protocol_and_requires_a_real_revision() {
    let request = Request::Refresh {
        slot: "6162".into(),
        operation: "a".repeat(32),
        expected_digest: "b".repeat(64),
    };
    for generation in [0, 1] {
        let reply = refresh_reply(
            &request,
            &slot::RefreshProgress::Ready(slot::Revision {
                generation,
                digest: "c".repeat(64),
            }),
        );
        if generation == 0 {
            assert!(reply.is_none());
        } else {
            let reply = reply.unwrap();
            let fields: Vec<_> = reply
                .strip_prefix("OK codex-auth ")
                .unwrap()
                .split_ascii_whitespace()
                .collect();
            assert_eq!(fields.len(), 6);
            assert!(fields.contains(&"state=ready"));
            assert!(fields.contains(&"generation=1"));
            assert!(fields.contains(&"slot=6162"));
        }
    }
}
