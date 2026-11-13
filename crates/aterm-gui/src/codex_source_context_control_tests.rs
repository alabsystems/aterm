// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn base() -> String {
    format!(
        "schema=1 request=0123456789abcdef0000000000000001 operation={} member={}",
        "a".repeat(32),
        "b".repeat(32)
    )
}
#[test]
fn rebound_routes_are_distinct_manager_only_closed_bodies() {
    let preload = format!("member-preload-rebound {} {}", base(), sequence());
    let request = Request::parse(&preload).unwrap();
    assert!(matches!(
        request.action,
        super::super::Action::Context(Action::ReboundPreload)
    ));
    assert_eq!(request.name(), "member-preload-rebound");
    assert!(Request::parse(&format!("{preload} {}", parent())).is_none());
    assert!(Request::parse(&format!("{preload} context={}", "1".repeat(64))).is_none());
    assert!(Request::parse(&preload.replace(sequence(), "")).is_none());
    let report = format!(
        "registration={} credential_generation=7 digest={} account={} user={} auth_generation=1 owner_generation=2 source_generation=3 login_generation=4",
        "c".repeat(32),
        "d".repeat(64),
        "e".repeat(64),
        "f".repeat(64)
    );
    let ready = format!("member-rebound-ready {} {} {report}", base(), sequence());
    let request = Request::parse(&ready).unwrap();
    assert_eq!(request.name(), "member-rebound-ready");
    assert!(Request::parse(&ready.replace("auth_generation=1", "")).is_none());
    assert!(Request::parse(&format!("{ready} {}", parent())).is_none());
    let pid = std::process::id();
    let process = Process::observe(pid).unwrap();
    let caller = Caller {
        caller: slot::Caller {
            sid: "s-rebound-frame".into(),
            pid,
            birth: process.birth,
        },
        process,
        start: restart_stamp::kernel_start(pid).unwrap(),
    };
    assert_eq!(
        response(&request, &caller, "test-epoch", Outcome::ReboundChanged).unwrap(),
        "ERR codex-auth rebound changed\n"
    );
}
fn parent() -> String {
    format!(
        "use_member={} use_operation=1234567890abcdef0000000000000001 capability={}",
        "b".repeat(32),
        "c".repeat(64)
    )
}
fn sequence() -> &'static str {
    "preload_sequence=0123456789abcdef0000000000000001"
}
fn predecessor() -> String {
    format!(
        "previous_registration={} expected_credential_generation=7 expected_digest={}",
        "d".repeat(32),
        "e".repeat(64)
    )
}
#[test]
fn actual_parser_requires_exact_owned_sequence_role_and_closed_reports() {
    let lines = [
        format!("member-preload-use {} {} {}", base(), sequence(), parent()),
        format!(
            "member-preload-use-resample {} {} {} {}",
            base(),
            sequence(),
            parent(),
            predecessor()
        ),
        format!(
            "member-preload-resample {} {} {}",
            base(),
            sequence(),
            predecessor()
        ),
        format!(
            "member-preload-target-resample {} {} {} assignment={} context={}",
            base(),
            sequence(),
            predecessor(),
            "f".repeat(32),
            "a".repeat(64)
        ),
        format!(
            "member-unavailable-use {} {} {} registration={}",
            base(),
            sequence(),
            parent(),
            "d".repeat(32)
        ),
        format!(
            "member-reader-ready-use {} {} {} registration={} credential_generation=7 digest={} account={} user={}",
            base(),
            sequence(),
            parent(),
            "d".repeat(32),
            "e".repeat(64),
            "f".repeat(64),
            "a".repeat(64)
        ),
        format!(
            "member-ready-use {} {} {} registration={} credential_generation=7 digest={} account={} user={} auth_generation=3 owner_generation=4 source_generation=5 login_generation=6",
            base(),
            sequence(),
            parent(),
            "d".repeat(32),
            "e".repeat(64),
            "f".repeat(64),
            "a".repeat(64)
        ),
        format!(
            "member-refresh-use {} {} slot=736c6f74 expected_digest={}",
            base(),
            parent(),
            "e".repeat(64)
        ),
    ];
    for line in lines {
        let parsed = Request::parse(&line).expect("current exact SDK route");
        assert!(matches!(parsed.action, super::super::Action::Context(_)));
        assert!(Request::parse(&format!("{line} unknown=1")).is_none());
        assert!(Request::parse(&format!("{line} operation={}", "a".repeat(32))).is_none());
        if line.contains("preload_sequence=") {
            assert!(Request::parse(&line.replace(sequence(), "")).is_none());
            assert!(
                Request::parse(&line.replace(
                    "preload_sequence=0123456789abcdef0000000000000001",
                    "preload_sequence=0123456789abcdef0000000000000000"
                ))
                .is_none()
            );
        }
    }
    let unavailable = format!(
        "member-unavailable-use {} {} {} registration={} credential_generation=7",
        base(),
        sequence(),
        parent(),
        "d".repeat(32)
    );
    assert!(
        Request::parse(&unavailable).is_none(),
        "unavailable cannot report a credential observation"
    );
}
#[test]
fn uncounted_context_cannot_schedule_and_same_rpc_cannot_change_its_role_body() {
    let request = Request::parse(&format!(
        "member-preload-use {} {} {}",
        base(),
        sequence(),
        parent()
    ))
    .unwrap();
    let caller = super::super::tests::caller();
    let mut jobs = Jobs::default();
    let (job, start) = jobs.admit(8001, &caller, &request).unwrap();
    assert!(!start);
    assert!(
        !jobs.reserve_ready(job).1,
        "native command custody is required before asynchronous scheduling"
    );
    for changed in [
        format!(
            "member-preload-use {} {} {}",
            base().replace(&"a".repeat(32), &"d".repeat(32)),
            sequence(),
            parent()
        ),
        format!(
            "member-preload-use {} preload_sequence=0123456789abcdef0000000000000002 {}",
            base(),
            parent()
        ),
        format!(
            "member-preload-use {} {} {}",
            base(),
            sequence(),
            parent().replace(&"c".repeat(64), &"f".repeat(64))
        ),
    ] {
        assert!(matches!(
            jobs.admit(8001, &caller, &Request::parse(&changed).unwrap()),
            Err(AdmissionError::Changed)
        ));
    }
}
#[test]
fn ready_committed_echoes_only_exact_predecessor_not_source_or_readiness() {
    let request = Request::parse(&format!(
        "member-preload-use-resample {} {} {} {}",
        base(),
        sequence(),
        parent(),
        predecessor()
    ))
    .unwrap();
    let previous = registry::PreloadPredecessor {
        registration: "d".repeat(32),
        credential_generation: 7,
        digest: "e".repeat(64),
    };
    let line = response(
        &request,
        &super::super::tests::caller(),
        "native-epoch",
        Outcome::Preload(registry::PreloadResample::ReadyCommitted { previous }),
    )
    .unwrap();
    let value: aterm_json::Value = aterm_json::from_str(
        line.strip_prefix("OK codex-auth member-preload-use-resample ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["state"].as_str(), Some("ready_committed"));
    assert_eq!(
        value["preload_sequence"].as_str(),
        Some("0123456789abcdef0000000000000001")
    );
    assert_eq!(
        value["previous_registration"].as_str(),
        Some("dddddddddddddddddddddddddddddddd")
    );
    assert_eq!(value["expected_credential_generation"].as_u64(), Some(7));
    assert!(value.get("source").is_none());
    assert!(value.get("registration").is_none());
}
