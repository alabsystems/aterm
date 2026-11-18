// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use crate::{Kind, SideEffect, Sub};
use std::path::Path;

fn request() -> GitRun {
    GitRun::fetch(
        &crate::slug::Slug::new("fixture-owner", "repo").unwrap(),
        Path::new("unused"),
    )
}

#[test]
fn checkout_pack_accepts_only_a_successful_complete_hash() {
    let oid = "a".repeat(40);
    let out = ChildOut {
        code: Some(0),
        stdout: format!("{oid}\n").into_bytes(),
        stderr: b"a harmless pack warning\n".to_vec(),
    };
    assert_eq!(hash(&out, &request(), &Redactor::new(), 0, 1).unwrap(), oid);
}

#[test]
fn checkout_pack_failure_preserves_exit_stderr_and_redaction_even_with_a_hash() {
    let redactor = Redactor::new();
    let secret = "fixture-private-pack-value";
    redactor.learn(secret.as_bytes());
    let out = ChildOut {
        code: Some(128),
        stdout: format!("{}\n", "a".repeat(40)).into_bytes(),
        stderr: format!("fatal: unable to write pack: No space left on device {secret}\n")
            .into_bytes(),
    };
    let error = hash(&out, &request(), &redactor, 7, 16_384)
        .unwrap_err()
        .into_data();
    assert_eq!(error.kind, Kind::CouldNotRun);
    assert_eq!(error.sub, Sub::Git { exit: Some(128) });
    assert_eq!(error.side_effect, SideEffect::None);
    assert!(
        error
            .why
            .contains("checkout blob batch 7 (16384 objects), git pack-objects")
    );
    assert!(error.why.contains("exited 128"));
    assert!(error.why.contains("No space left on device"));
    assert!(error.why.contains("[redacted]"));
    assert!(!error.why.contains(secret));
}

#[test]
fn checkout_pack_signal_is_a_child_failure_not_a_protocol_failure() {
    let out = ChildOut {
        code: None,
        stdout: Vec::new(),
        stderr: b"killed by operating system\n".to_vec(),
    };
    let error = hash(&out, &request(), &Redactor::new(), 2, 3)
        .unwrap_err()
        .into_data();
    assert_eq!(error.sub, Sub::Git { exit: None });
    assert_eq!(error.side_effect, SideEffect::None);
    assert!(error.why.contains("on a signal"));
    assert!(error.why.contains("killed by operating system"));
}

#[test]
fn checkout_pack_malformed_success_keeps_its_distinct_protocol_reason() {
    for stdout in [
        Vec::new(),
        b"not a hash\n".to_vec(),
        vec![0xff],
        "a".repeat(40).into_bytes(),
    ] {
        let out = ChildOut {
            code: Some(0),
            stdout,
            stderr: b"unexpected packer output\n".to_vec(),
        };
        let error = hash(&out, &request(), &Redactor::new(), 4, 5)
            .unwrap_err()
            .into_data();
        assert_eq!(error.sub, Sub::Protocol);
        assert_eq!(error.side_effect, SideEffect::None);
        assert!(error.why.contains("an invalid pack hash"));
        assert!(error.why.contains("unexpected packer output"));
    }
}

#[test]
fn checkout_pack_truncated_output_is_bounded_and_redacted() {
    let redactor = Redactor::new();
    let secret = "fixture-private-pack-value";
    redactor.learn(secret.as_bytes());
    let mut stderr = vec![b'x'; ChildOut::OUTPUT_KEEP];
    stderr.extend_from_slice(secret.as_bytes());
    let out = ChildOut {
        code: Some(0),
        stdout: vec![b'a'; ChildOut::OUTPUT_KEEP],
        stderr,
    };
    let error = hash(&out, &request(), &redactor, 6, 9)
        .unwrap_err()
        .into_data();
    assert_eq!(error.sub, Sub::Protocol);
    assert!(error.why.contains("truncated stdout"));
    assert!(error.why.contains("[redacted]"));
    assert!(!error.why.contains(secret));
    assert!(error.why.len() < 2 * ChildOut::OUTPUT_KEEP + 512);
}
