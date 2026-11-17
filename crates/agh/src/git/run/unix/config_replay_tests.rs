// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

/// An admission recorded for a narrower access never replays as a wider one: a read's inert
/// `pushurl` is no write's, and an object source's inert hook path is no fetch's.
#[test]
fn a_config_projection_cannot_replay_as_a_wider_admission() {
    use crate::fx::tape::{TapeHeader, TapeMode};

    let root = aterm_tempfile::Builder::new()
        .prefix("agh-config-replay-")
        .tempdir()
        .unwrap();
    let slug = Slug::new("fixture-dev", "app").unwrap();
    let cases: [(ConfigAccess, ConfigAccess, &[u8]); 2] = [
        (
            ConfigAccess::Read,
            ConfigAccess::Write,
            b"[remote \"origin\"]\npushurl = https://elsewhere.invalid/app\n",
        ),
        (
            ConfigAccess::ObjectSource,
            ConfigAccess::Read,
            b"[core]\nhooksPath = scripts/hooks\n",
        ),
    ];
    for (narrow, wide, input) in cases {
        let tape = root.path().join(format!("{}.tape", narrow.word()));
        let query = ConfigQuery::Names { access: narrow };
        let mut spec = ChildSpec::new(&find_git(&crate::identity::ProcessEnv).unwrap())
            .cwd(root.path())
            .config_input(input, query);
        for arg in query.args() {
            spec = spec.arg(*arg);
        }
        let mut header = TapeHeader::for_run(
            &["config-screen".into()],
            &crate::wire::Endpoints::github_com(),
        );
        header.mode = TapeMode::Credentialed;
        let recording = crate::fx::Fx::record_for_tests(&tape, &header).unwrap();
        let answer = recording
            .git(&spec, &config_names_request(&slug, narrow, "local"))
            .unwrap();
        assert_eq!(answer.code, Some(0));
        screen_projected(&answer.stdout, &slug).unwrap();
        recording.finish(0).unwrap();
        let (replay, _) = crate::fx::Fx::replay_for_tests(&tape).unwrap();
        let wider = spec.config_input(input, ConfigQuery::Names { access: wide });
        replay
            .git(&wider, &config_names_request(&slug, wide, "local"))
            .expect_err("a narrower admission must not become a wider one");
        assert!(
            replay.finish(0).is_err(),
            "the semantic divergence must stay latched"
        );
    }
}

#[test]
fn config_verdict_rejects_unknown_protocol_fields() {
    let slug = Slug::new("fixture-dev", "app").unwrap();
    assert!(screen_projected(br#"{"Allowed":{"keys":1,"extra":true}}"#, &slug).is_err());
}

#[test]
fn commit_config_permission_cannot_replay_as_transport_admission() {
    use crate::fx::tape::{TapeHeader, TapeMode};

    let root = aterm_tempfile::Builder::new()
        .prefix("agh-commit-config-replay-")
        .tempdir()
        .unwrap();
    let slug = Slug::new("fixture-dev", "app").unwrap();
    let input = b"[remote \"origin\"]\npromisor = true\npartialclonefilter = blob:none\n[user]\nname = private-config-marker\n";
    for access in [
        ConfigAccess::Write,
        ConfigAccess::Read,
        ConfigAccess::ObjectSource,
    ] {
        let tape = root.path().join(format!("{}.tape", access.word()));
        let query = ConfigQuery::CommitRead;
        let mut spec = ChildSpec::new(&find_git(&crate::identity::ProcessEnv).unwrap())
            .cwd(root.path())
            .config_input(input, query);
        for arg in query.args() {
            spec = spec.arg(*arg);
        }
        let mut header = TapeHeader::for_run(
            &["commit-config-screen".into()],
            &crate::wire::Endpoints::github_com(),
        );
        header.mode = TapeMode::Credentialed;
        let recording = crate::fx::Fx::record_for_tests(&tape, &header).unwrap();
        let answer = recording
            .git(&spec, &commit_config_request(&slug, "local"))
            .unwrap();
        assert_eq!(answer.code, Some(0));
        screen_projected(&answer.stdout, &slug).unwrap();
        recording.finish(0).unwrap();
        assert!(
            !String::from_utf8_lossy(&std::fs::read(&tape).unwrap())
                .contains("private-config-marker")
        );
        let (replay, _) = crate::fx::Fx::replay_for_tests(&tape).unwrap();
        let query = ConfigQuery::Names { access };
        let mut transport = spec.config_input(input, query);
        transport.args = query.args().iter().map(|arg| (*arg).into()).collect();
        replay
            .git(&transport, &config_names_request(&slug, access, "local"))
            .expect_err(
                "immutable commit metadata permission must not become transport permission",
            );
        assert!(replay.finish(0).is_err());
    }
}
