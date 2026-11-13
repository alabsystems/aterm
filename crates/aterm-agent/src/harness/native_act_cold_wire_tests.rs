// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The fixture's working directory, absolute on this host (`C:/fixture` on Windows).
fn fixture() -> String {
    crate::harness::host_path::abs("/fixture")
}

#[test]
fn cold_facts_keep_credentials_local_and_preserve_known_empty() {
    let intent = ColdIntent {
        login: None,
        home: None,
        auth_observed: false,
        route: crate::harness::accounts::AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: super::super::accounts::Mode::Wall,
        billing: ColdBilling::Keep,
    };
    assert!(
        ColdEnvironmentFacts::observe(
            &["HOME=/one".into(), "HOME=/two".into()],
            &intent,
            std::path::Path::new(&fixture())
        )
        .is_none()
    );
    let empty =
        ColdEnvironmentFacts::observe(&[], &intent, std::path::Path::new(&fixture())).unwrap();
    assert!(empty.default_home && empty.route == super::super::accounts::AuthRoute::Stored);
    assert_eq!(empty.selection, super::super::profile::Selection::Standard);
    assert_eq!(
        super::super::profile::Selection::of_exec_env(&[]),
        super::super::profile::Selection::Unknown
    );
    let secret = "synthetic-credential-never-on-wire";
    let facts = ColdEnvironmentFacts::observe(
        &[format!("ANTHROPIC_API_KEY={secret}")],
        &intent,
        std::path::Path::new(&fixture()),
    )
    .unwrap();
    assert_eq!(facts.route, super::super::accounts::AuthRoute::Outside);
    let encoded = facts.encode().unwrap();
    assert!(!encoded.contains(secret));
    assert!(!format!("{facts:?}").contains(secret));
    assert_eq!(ColdEnvironmentFacts::parse(&encoded), Some(facts));
    let encoded = intent.encode().unwrap();
    assert_eq!(ColdIntent::parse(&encoded), Some(intent));
}

/// A COLD RECORD OF v0.157.0 AND OLDER STILL READS: an intent or facts in flight across the
/// update that brought the token route carry `stored_login`, which reads as the route
/// (`true` stored, `false` outside), as `ClaudeAuthRestore` reads it; what they encode again
/// carries `route` alone. CONTROLS: both fields, or neither, is no record.
#[test]
fn a_cold_record_of_an_older_build_reads_its_stored_login_as_the_route() {
    use super::super::accounts::AuthRoute;
    let facts = |route: &str| {
        format!(
            r#"{{"login":null,"default_home":true,{route}"home":null,"config":null,"profile_configs":null,"selection":"Standard","inherited_named":false,"cwd":"{cwd}"}}"#,
            cwd = fixture()
        )
    };
    for (old, route) in [("true", AuthRoute::Stored), ("false", AuthRoute::Outside)] {
        let read = ColdEnvironmentFacts::parse(&facts(&format!(r#""stored_login":{old},"#)))
            .unwrap_or_else(|| panic!("stored_login {old} reads"));
        assert_eq!(read.route, route);
        let again = read.encode().unwrap();
        assert!(!again.contains("stored_login"), "{again}");
        assert_eq!(ColdEnvironmentFacts::parse(&again), Some(read));
    }
    assert!(ColdEnvironmentFacts::parse(&facts("")).is_none());
    assert!(
        ColdEnvironmentFacts::parse(&facts(r#""route":"stored","stored_login":true,"#)).is_none()
    );
    let intent = |route: &str| {
        format!(
            r#"{{"login":null,"home":null,"auth_observed":true,{route}"conversation":null,"profile":null,"mode":"wall","billing":"Keep"}}"#
        )
    };
    let current = ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: super::super::accounts::Mode::Wall,
        billing: ColdBilling::Keep,
    }
    .encode()
    .unwrap();
    assert_eq!(
        current,
        intent(r#""route":"stored","#),
        "the wire as written"
    );
    for (old, route) in [("true", AuthRoute::Stored), ("false", AuthRoute::Outside)] {
        let legacy = intent(&format!(r#""stored_login":{old},"#));
        let read = ColdIntent::parse(&legacy).unwrap_or_else(|| panic!("stored_login {old} reads"));
        assert_eq!(read.route, route);
        assert!(!read.encode().unwrap().contains("stored_login"));
        assert_eq!(read.encode_for_launcher(), Some(legacy));
    }
    let mut token = ColdIntent::parse(&current).unwrap();
    token.route = AuthRoute::VaultToken {
        entry: super::super::accounts::TokenEntry::parse("aterm-claude-token-ac-0123456789abcdef")
            .unwrap(),
    };
    assert!(token.encode_legacy().is_none());
    assert_eq!(token.encode_for_launcher(), token.encode());
    assert!(ColdIntent::parse(&intent("")).is_none());
    assert!(ColdIntent::parse(&intent(r#""route":"outside","stored_login":false,"#)).is_none());
}

#[test]
fn cold_facts_refuse_unknown_duplicate_and_oversize_authority_fields() {
    let text = &format!(
        r#"{{"login":null,"default_home":true,"route":"stored","home":null,"config":null,"profile_configs":null,"selection":"Standard","inherited_named":false,"cwd":"{}"}}"#,
        fixture()
    );
    assert!(ColdEnvironmentFacts::parse(text).is_some());
    assert!(
        ColdEnvironmentFacts::parse(&text.replacen("{", "{\"config\":\"/other\",", 1)).is_none()
    );
    assert!(
        ColdEnvironmentFacts::parse(
            &text.replace("\"default_home\":true", "\"default_home\":false")
        )
        .is_none()
    );
    assert!(ColdEnvironmentFacts::parse(&text.replacen("{", "{\"login\":\"other\",", 1)).is_none());
    assert!(ColdEnvironmentFacts::parse(&text.replace("Standard", "future-selector")).is_none());
    assert!(
        ColdEnvironmentFacts::parse(&text.replace(
            "\"home\":null",
            &format!("\"home\":\"{}\"", "x".repeat(MAX_PATH + 1))
        ))
        .is_none()
    );
}
