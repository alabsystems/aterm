// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn resource() -> String {
    format!(
        "member-target-resource-ready schema=1 request=0123456789abcdef0000000000000001 operation={} member={} assignment={} context={} registration={} boundary={} assignment_generation=7 credential_generation=11 digest={} account={} user={} auth_generation=13 owner_generation=17 source_generation=19 login_generation=23",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32),
        "d".repeat(64),
        "e".repeat(32),
        "f".repeat(32),
        "a".repeat(64),
        "b".repeat(64),
        "c".repeat(64)
    )
}
fn ack() -> String {
    format!(
        "member-target-ack schema=1 request=0123456789abcdef0000000000000001 operation={} context={} resource_receipt={}",
        "c".repeat(32),
        "d".repeat(64),
        "e".repeat(64)
    )
}
#[test]
fn sdk_resource_and_ack_grammar_has_exact_roles_and_no_invented_source_completion_fields() {
    let resource = resource();
    let ack = ack();
    let request = Request::parse(&resource).unwrap();
    assert_eq!(request.name(), "member-target-resource-ready");
    assert!(request.member.is_some());
    assert!(request.preload_sequence.is_none());
    let super::super::Action::Resource(Action::Resources { report, .. }) = &request.action else {
        panic!("resource parser");
    };
    let report = report.native(&request);
    assert_eq!(report.operation, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(report.auth.source, 19);
    let request = Request::parse(&ack).unwrap();
    assert_eq!(request.name(), "member-target-ack");
    assert!(request.member.is_none());
    for invalid in [
        format!("{resource} preload_sequence=11111111111111110000000000000001"),
        resource.replace("credential_generation=11", "credential_generation=0"),
        resource.replace("assignment_generation=7", "assignment_generation=0"),
        resource.replace(" auth_generation=13", ""),
        format!("{ack} member={}", "b".repeat(32)),
        format!("{ack} assignment={}", "c".repeat(32)),
        format!("{ack} credential_generation=11"),
        format!("{ack} resource_receipt={}", "e".repeat(64)),
    ] {
        assert!(
            Request::parse(&invalid).is_none(),
            "strict closed action body"
        );
    }
}
#[test]
fn immutable_resource_body_cannot_change_and_neither_action_schedules_before_native_custody() {
    let caller = super::super::tests::caller();
    for line in [resource(), ack()] {
        let request = Request::parse(&line).unwrap();
        let mut jobs = Jobs::default();
        let (job, start) = jobs.admit(78001, &caller, &request).unwrap();
        assert!(!start);
        assert!(!jobs.reserve_ready(job).1);
        let changed = if line.starts_with("member-target-ack") {
            line.replace(&"e".repeat(64), &"f".repeat(64))
        } else {
            line.replace("source_generation=19", "source_generation=20")
        };
        assert!(matches!(
            jobs.admit(78001, &caller, &Request::parse(&changed).unwrap()),
            Err(AdmissionError::Changed)
        ));
    }
}
