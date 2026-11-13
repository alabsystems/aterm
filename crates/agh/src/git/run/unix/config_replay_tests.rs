// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn read_only_config_projection_cannot_replay_as_write_admission() {
    use crate::fx::tape::{TapeHeader, TapeMode};

    let root = aterm_tempfile::Builder::new()
        .prefix("agh-config-replay-")
        .tempdir()
        .unwrap();
    let tape = root.path().join("config.tape");
    let slug = Slug::new("fixture-dev", "app").unwrap();
    let input = b"[remote \"origin\"]\npushurl = https://elsewhere.invalid/app\n";
    let query = ConfigQuery::Names { read_only: true };
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
        .git(&spec, &config_names_request(&slug, true, "local"))
        .unwrap();
    assert_eq!(answer.code, Some(0));
    screen_projected(&answer.stdout, &slug).unwrap();
    recording.finish(0).unwrap();
    let (replay, _) = crate::fx::Fx::replay_for_tests(&tape).unwrap();
    let write = spec.config_input(input, ConfigQuery::Names { read_only: false });
    replay
        .git(&write, &config_names_request(&slug, false, "local"))
        .expect_err("read-only pushurl permission must not become write permission");
    assert!(
        replay.finish(0).is_err(),
        "the semantic divergence must stay latched"
    );
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
    for read_only in [false, true] {
        let tape = root.path().join(format!("{read_only}.tape"));
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
        let query = ConfigQuery::Names { read_only };
        let mut transport = spec.config_input(input, query);
        transport.args = query.args().iter().map(|arg| (*arg).into()).collect();
        replay
            .git(&transport, &config_names_request(&slug, read_only, "local"))
            .expect_err(
                "immutable commit metadata permission must not become transport permission",
            );
        assert!(replay.finish(0).is_err());
    }
}
