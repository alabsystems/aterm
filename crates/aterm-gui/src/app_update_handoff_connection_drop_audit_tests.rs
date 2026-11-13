// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::session_store::ConnectionCarry;

fn row(src: &str, op: &str) -> ConnectionCarry {
    ConnectionCarry {
        src: src.to_string(),
        dst: "s-0000000001000007d7d7".to_string(),
        op: op.to_string(),
    }
}

#[test]
fn the_attempt_carries_what_its_manifest_left_out_to_the_commit_which_audits_it() {
    crate::test_log::install();
    let src = "s-0000000001000006c6c6";
    let arbiter = crate::HandoffAttemptArbiter::new();
    assert!(arbiter.connections_left_out().is_empty(), "nothing yet");
    arbiter.set_connections_left_out(Vec::new());
    assert!(arbiter.connections_left_out().is_empty(), "none left out");
    let left_out = vec![row(src, "read-screen"), row(src, "signal")];
    arbiter.set_connections_left_out(left_out.clone());
    arbiter.set_connections_left_out(vec![row(src, "write-input")]);
    assert_eq!(
        arbiter.connections_left_out(),
        left_out.as_slice(),
        "one manifest per attempt: the first record stands"
    );
    let mark = format!("EDGE: action=drop origin=handoff src={src}");
    assert!(crate::test_log::containing(&mark).is_empty());
    crate::App::audit_connections_left_out(arbiter.connections_left_out());
    let audited = crate::test_log::containing(&mark);
    assert_eq!(audited.len(), 2, "one drop per row: {audited:?}");
    assert!(audited.iter().all(|record| record.target == "session_edge"));
    assert!(
        audited[0].message.ends_with("op=read-screen"),
        "{audited:?}"
    );
    assert!(audited[1].message.ends_with("op=signal"), "{audited:?}");
}
