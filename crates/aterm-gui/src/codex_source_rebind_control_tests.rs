// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn base() -> String {
    format!(
        "member-work-rebind schema=1 request=0123456789abcdef0000000000000001 operation={} member={}",
        "a".repeat(32),
        "b".repeat(32)
    )
}
#[test]
fn parser_and_bound_reply_preserve_each_exact_original_body_without_new_source_authority() {
    for role in [
        "open_kind=ordinary".into(),
        format!(
            "open_kind=derived use_member={} use_operation=11111111111111110000000000000001 capability={}",
            "c".repeat(32),
            "d".repeat(64)
        ),
        format!(
            "open_kind=target assignment={} context={}",
            "c".repeat(32),
            "d".repeat(64)
        ),
    ] {
        let line = format!("{} {role}", base());
        let request = Request::parse(&line).unwrap();
        let Action::Rebind(original) = &request.action else {
            panic!("exact rebind parser");
        };
        let response = response(&request, &super::super::tests::caller(), "host-epoch").unwrap();
        let value: aterm_json::Value = aterm_json::from_str(
            response
                .strip_prefix("OK codex-auth member-work-rebind ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["state"].as_str(), Some("bound"));
        let mut expected = aterm_json::Map::new();
        original.echo(&mut expected);
        for (key, expected) in expected {
            assert_eq!(value.get(key.as_str()), Some(&expected));
        }
        for forbidden in [
            "source",
            "registration",
            "preload_sequence",
            "credential_generation",
            "auth_generation",
        ] {
            assert!(value.get(forbidden).is_none());
        }
        assert!(Request::parse(&format!("{line} unknown=1")).is_none());
        assert!(
            Request::parse(&format!(
                "{line} preload_sequence=11111111111111110000000000000001"
            ))
            .is_none()
        );
    }
    assert!(
        Request::parse(&format!(
            "{} open_kind=ordinary capability={}",
            base(),
            "d".repeat(64)
        ))
        .is_none()
    );
    assert!(Request::parse(&format!("{} open_kind=derived use_member={} use_operation=11111111111111110000000000000000 capability={}",base(),"c".repeat(32),"d".repeat(64))).is_none());
}
#[test]
fn prototype_target_ready_vocabulary_is_not_a_compatibility_alias() {
    let common = format!(
        "schema=1 request=0123456789abcdef0000000000000001 operation={} member={} assignment={} context={} assignment_generation=3 registration={} preload_sequence=11111111111111110000000000000001",
        "a".repeat(32),
        "b".repeat(32),
        "c".repeat(32),
        "d".repeat(64),
        "e".repeat(32)
    );
    assert!(Request::parse(&format!("member-unavailable-target {common}")).is_some());
    assert!(Request::parse(&format!("member-target-unavailable {common}")).is_none());
}
