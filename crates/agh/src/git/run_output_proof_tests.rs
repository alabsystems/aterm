// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Complete native proof bytes stay independent of diagnostic caps and redaction.

use super::super::{GitOp, unix};
use crate::error::{Error, Kind, SideEffect};
use crate::fx::child::ChildOut;
use crate::github::refs::{AtomicRefBatch, RefUpdate};
use crate::redact::Redactor;
use crate::slug::Slug;

fn classify(op: &GitOp, out: &ChildOut, redactor: &Redactor) -> Error {
    unix::failed(
        op,
        out,
        redactor,
        &Slug::new("fixture-dev", "app").unwrap(),
        true,
    )
}

fn batch(names: &[String]) -> GitOp {
    GitOp::PushRefs {
        dir: "/unused".into(),
        batch: AtomicRefBatch::new(
            names
                .iter()
                .map(|name| RefUpdate {
                    reference: name.clone(),
                    before: "b".repeat(40),
                    after: "a".repeat(40),
                })
                .collect(),
        )
        .unwrap(),
        dry_run: false,
    }
}

fn rows(names: &[String], first: &str) -> Vec<u8> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            format!(
                "!\t{}:{name}\t{}\n",
                "a".repeat(40),
                if index == 0 {
                    first
                } else {
                    "[rejected] (atomic push failed)"
                }
            )
        })
        .collect::<String>()
        .into_bytes()
}

#[test]
fn maximum_declared_ref_batch_proof_survives_diagnostic_caps_and_secret_redaction() {
    let names: Vec<_> = (0..128)
        .map(|index| format!("refs/heads/{index:03}/{}", "x".repeat(900)))
        .collect();
    let op = batch(&names);
    let out = ChildOut {
        code: Some(1),
        stdout: rows(&names, "[rejected] (stale info)"),
        stderr: Vec::new(),
    };
    assert!(out.stdout.len() > 64 * 1024 && out.stdout.len() < ChildOut::OUTPUT_KEEP);
    let redactor = Redactor::new();
    let error = classify(&op, &out, &redactor);
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
    redactor.learn(names[0].as_bytes());
    redactor.learn("a".repeat(40).as_bytes());
    let redacted = classify(&op, &out, &redactor);
    assert_eq!(
        (redacted.kind, redacted.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
    assert!(!redacted.why.contains(&names[0]));
    assert!(!redacted.why.contains(&"a".repeat(40)));
    if let crate::Sub::GuardFailed { what } = &redacted.sub {
        assert!(!what.contains(&names[0]));
        assert!(!what.contains(&"a".repeat(40)));
    } else {
        panic!("a proved stale lease must retain its typed guard failure");
    }
}

#[test]
fn diagnostic_tails_never_hide_conflicting_prefixes_or_malformed_native_bytes() {
    let names = vec!["refs/heads/main".into(), "refs/heads/other".into()];
    let op = batch(&names);
    let valid = rows(&names, "[rejected] (stale info)");
    let padding = vec![b'\n'; 70 * 1024];
    let mut success_prefix = b"*\tunrequested:refs/heads/unrequested\t[new branch]\n".to_vec();
    success_prefix.extend(&padding);
    success_prefix.extend(&valid);
    let mut malformed_prefix = vec![0xff];
    malformed_prefix.extend(&padding);
    malformed_prefix.extend(&valid);
    for stdout in [success_prefix, malformed_prefix] {
        assert!(stdout.len() < ChildOut::OUTPUT_KEEP);
        let error = classify(
            &op,
            &ChildOut {
                code: Some(1),
                stdout,
                stderr: Vec::new(),
            },
            &Redactor::new(),
        );
        assert_ne!(error.kind, Kind::Conflict);
        assert_eq!(error.side_effect, SideEffect::Unknown);
    }
    let generic = names
        .iter()
        .map(|name| {
            format!(
                "!\t{}:{name}\t[remote rejected] (atomic transaction failed)\n",
                "a".repeat(40)
            )
        })
        .collect::<String>()
        .into_bytes();
    let tail = format!(
        "remote: error: cannot lock ref '{}': is at {} but expected {}\n",
        names[0],
        "c".repeat(40),
        "b".repeat(40)
    );
    for prefix in [
        b"remote: error: cannot lock ref 'refs/heads/unrequested': reference already exists\n"
            .to_vec(),
        vec![0xff],
    ] {
        let mut stderr = prefix;
        stderr.extend(&padding);
        stderr.extend(tail.as_bytes());
        assert!(stderr.len() < ChildOut::OUTPUT_KEEP);
        let error = classify(
            &op,
            &ChildOut {
                code: Some(1),
                stdout: generic.clone(),
                stderr,
            },
            &Redactor::new(),
        );
        assert_ne!(error.kind, Kind::Conflict);
        assert_eq!(error.side_effect, SideEffect::Unknown);
    }
    let out = ChildOut {
        code: Some(1),
        stdout: generic,
        stderr: vec![b'\n'; ChildOut::OUTPUT_KEEP],
    };
    assert_eq!(
        classify(&op, &out, &Redactor::new()).side_effect,
        SideEffect::Unknown
    );
}

#[test]
fn authentication_scope_and_policy_prose_cannot_close_an_unproved_atomic_receive() {
    let names = vec!["refs/heads/main".into(), "refs/heads/other".into()];
    let op = batch(&names);
    for marker in [
        "Authentication failed",
        "could not read Username",
        "told us to quit",
        "refusing to allow an OAuth App to create or update workflow",
    ] {
        let out = ChildOut {
            code: Some(1),
            stdout: rows(&names[..1], "[rejected] (stale info)"),
            stderr: marker.as_bytes().to_vec(),
        };
        assert_eq!(
            classify(&op, &out, &Redactor::new()).side_effect,
            SideEffect::Unknown
        );
    }
    let policy = rows(&names, "[remote rejected] (protected branch hook declined)");
    let known = classify(
        &op,
        &ChildOut {
            code: Some(1),
            stdout: policy.clone(),
            stderr: Vec::new(),
        },
        &Redactor::new(),
    );
    assert_eq!(
        (known.kind, known.side_effect),
        (Kind::Refused, SideEffect::None)
    );
    let partial = ChildOut {
        code: Some(1),
        stdout: rows(
            &names[..1],
            "[remote rejected] (protected branch hook declined)",
        ),
        stderr: Vec::new(),
    };
    assert_eq!(
        classify(&op, &partial, &Redactor::new()).side_effect,
        SideEffect::Unknown
    );
    for reason in [
        "[remote rejected] (protected\0policy)",
        "[remote rejected] (unterminated",
    ] {
        let out = ChildOut {
            code: Some(1),
            stdout: rows(&names, reason),
            stderr: Vec::new(),
        };
        assert_eq!(
            classify(&op, &out, &Redactor::new()).side_effect,
            SideEffect::Unknown
        );
    }
}
