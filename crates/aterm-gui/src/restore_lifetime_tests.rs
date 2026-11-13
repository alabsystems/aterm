// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::AgentRestore;
use aterm_agent::harness::relaunch::ProcessLifetime;

pub(super) fn saved_lifetime(pid: u32) -> ProcessLifetime {
    aterm_json::from_str(&format!(
        r#"{{"host":"darwin:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","boot":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","pid":{pid},"birth":456}}"#
    ))
    .expect("canonical saved lifetime")
}

fn record(lifetime: Option<ProcessLifetime>) -> AgentRestore {
    AgentRestore {
        pid: 4242,
        start: "Thu Nov 05 01:00:00 2026".into(),
        lifetime,
        program: "/fixture/claude".into(),
        argv: vec!["/fixture/claude".into()],
        session: Some("0badf00d-1111-2222-3333-444455556666".into()),
        cwd: "/fixture/work".into(),
        version: None,
        home: None,
        login: None,
        claude_auth: None,
        run: None,
        word: "claude".into(),
        cause: None,
        tab: Some("s-original".into()),
    }
}

#[test]
fn a_saved_lifetime_survives_both_codecs_and_cold_or_handed_snapshots() {
    let linux: ProcessLifetime = aterm_json::from_str(
        r#"{"host":"linux:cccccccccccccccccccccccccccccccc","boot":"dddddddddddddddddddddddddddddddd","namespace":"1234","pid":4242,"birth":789}"#,
    )
    .expect("canonical Linux lifetime");
    for lifetime in [Some(saved_lifetime(4242)), Some(linux), None] {
        let before = record(lifetime.clone());
        let json = aterm_json::to_string(&before).unwrap();
        let toml = aterm_toml::to_string(&before).unwrap();
        if lifetime.is_none() {
            assert!(!json.contains("lifetime"), "{json}");
            assert!(!toml.contains("lifetime"), "{toml}");
        }
        for decoded in [
            aterm_json::from_str::<AgentRestore>(&json).unwrap(),
            aterm_toml::from_str::<AgentRestore>(&toml).unwrap(),
        ] {
            assert_eq!(decoded, before);
            let cold = decoded
                .clone()
                .snapshot("s-reopened".into(), 41)
                .expect("the captured Claude relaunch");
            assert_eq!(cold.lifetime, lifetime);
            assert_eq!(cold.pid, 4242);
            assert_eq!(cold.shell, 41);
            assert_eq!(AgentRestore::of(cold).lifetime, lifetime);
            for adopted in [41, 4242] {
                let handed = decoded
                    .clone()
                    .handed_snapshot("s-reopened".into(), adopted)
                    .expect("the handed Claude relaunch");
                assert_eq!(handed.lifetime, lifetime);
                assert_eq!(handed.pid, 4242);
                assert_eq!(AgentRestore::of(handed).lifetime, lifetime);
            }
        }
    }
}

#[test]
fn a_saved_lifetime_for_another_pid_is_rejected_by_both_codecs() {
    let mismatch = record(Some(saved_lifetime(4343)));
    assert!(
        aterm_json::from_str::<AgentRestore>(&aterm_json::to_string(&mismatch).unwrap()).is_err()
    );
    assert!(
        aterm_toml::from_str::<AgentRestore>(&aterm_toml::to_string(&mismatch).unwrap()).is_err()
    );
}

#[test]
fn an_invalid_saved_lifetime_is_rejected_instead_of_becoming_unknown() {
    let before = record(Some(saved_lifetime(4242)));
    let json = aterm_json::to_string(&before).unwrap();
    let toml = aterm_toml::to_string(&before).unwrap();
    for word in [
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "darwin:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert!(json.contains(word), "{json}");
        assert!(toml.contains(word), "{toml}");
        assert!(aterm_json::from_str::<AgentRestore>(&json.replace(word, "")).is_err());
        assert!(aterm_toml::from_str::<AgentRestore>(&toml.replace(word, "")).is_err());
    }
}
