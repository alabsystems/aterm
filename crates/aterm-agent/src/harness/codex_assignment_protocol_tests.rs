// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn assignment_status_keeps_unknown_cancellation_distinct_from_an_assignment() {
    let session = SessionBinding {
        sid: "s-fixture".into(),
        process_pid: 42,
        process_birth: "2026-11-06 12:00:00 UTC".into(),
        epoch: "opaque-host-epoch".into(),
    };
    let mut status = Status {
        operation: "a".repeat(32),
        phase: Phase::Cancelled,
        boundary: None,
        assignment_generation: None,
        credential_generation: None,
        target_ref: None,
        selection_mode: None,
        reason: Some(Reason::Cancelled),
    };
    let line = status.encode(&session).unwrap();
    let body = line
        .strip_prefix("OK codex-accounts-change assignment ")
        .unwrap()
        .trim_end();
    let value: aterm_json::Value = aterm_json::from_str(body).unwrap();
    assert_eq!(value["selection_mode"], aterm_json::Value::Null);
    assert_eq!(value["assignment_generation"], aterm_json::Value::Null);
    assert_eq!(value["native_intent"], value["operation"]);
    assert!(value.get("path").is_none());
    assert!(value.get("digest").is_none());
    status.target_ref = Some("b".repeat(64));
    assert!(
        status.encode(&session).is_none(),
        "unknown cancellation cannot invent a target"
    );
    status.selection_mode = Some(SelectionMode::Manual);
    status.phase = Phase::Prepared;
    status.reason = None;
    assert!(status.encode(&session).is_some());
    status.assignment_generation = Some(3);
    assert!(
        status.encode(&session).is_none(),
        "prepare never reserves an assignment"
    );
    status.phase = Phase::AwaitingAck;
    status.boundary = Some("c".repeat(32));
    status.credential_generation = Some(13);
    assert!(status.encode(&session).is_some());
    status.selection_mode = None;
    assert!(
        status.encode(&session).is_none(),
        "installed state needs actual selection mode"
    );
}

#[test]
fn assignment_requests_keep_counter_domains_and_refuse_unbound_fields() {
    let op = "a".repeat(32);
    let boundary = "b".repeat(32);
    let digest = "c".repeat(64);
    let common = format!("schema=1 operation={op}");
    let auth = "auth_generation=19 owner_generation=23 source_generation=29 login_generation=31";
    let prepare = format!(
        "assignment-prepare {common} pool_revision={digest} expected_assignment=3 selection=auto"
    );
    assert!(matches!(
        Request::parse(&prepare).unwrap().command,
        Command::Prepare {
            expected_assignment: 3,
            selection: Selection::Auto,
            ..
        }
    ));
    let commit = format!(
        "assignment-commit {common} boundary={boundary} source_credential_generation=13 {auth}"
    );
    assert!(matches!(
        Request::parse(&commit).unwrap().command,
        Command::Commit {
            source_credential_generation: 13,
            auth: AuthState {
                generation: 19,
                owner: 23,
                source: 29,
                login: 31
            },
            ..
        }
    ));
    let ack = format!(
        "assignment-ack {common} boundary={boundary} assignment_generation=3 credential_generation=13 digest={digest} account={digest} user={digest} {auth}"
    );
    assert!(matches!(
        Request::parse(&ack).unwrap().command,
        Command::Ack {
            assignment_generation: 3,
            credential_generation: 13,
            ..
        }
    ));
    for invalid in [
        format!("{prepare} path=/private/canary"),
        format!("{prepare} operation={op}"),
        prepare.replace("expected_assignment=3", "expected_assignment=03"),
        prepare.replace("schema=1", "schema=2"),
        commit.replace(
            "source_credential_generation=13",
            "source_credential_generation=0",
        ),
        ack.replace("assignment_generation=3", "assignment_generation=0"),
        ack.replace("owner_generation=23", "owner_generation=-1"),
        ack.replace(
            "login_generation=31",
            "login_generation=18446744073709551616",
        ),
        format!("assignment-poll {common} selection=auto"),
        format!("assignment-cancel {common}\n"),
        format!("assignment-poll  {common}"),
        format!("assignment-poll {common} token=private-canary"),
    ] {
        assert!(
            Request::parse(&invalid).is_none(),
            "invalid request accepted"
        );
    }
    assert!(matches!(
        Request::parse(&format!("assignment-poll {common}"))
            .unwrap()
            .command,
        Command::Poll
    ));
    assert!(matches!(
        Request::parse(&format!("assignment-cancel {common}"))
            .unwrap()
            .command,
        Command::Cancel
    ));
}
