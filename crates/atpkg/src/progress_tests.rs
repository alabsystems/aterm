// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn occupied_legacy_temps_never_disable_progress_or_touch_their_targets() {
    let ordinary = crate::testdir::layout("progress-temp-positive-");
    let sink = ProgressSink::create(&ordinary.progress_file(), "install").unwrap();
    assert!(sink.plan(&[("codex".into(), 100)]));
    assert_eq!(read_progress(&ordinary).unwrap().queue, ["codex"]);
    let mut outcomes = Vec::new();
    for alias in [false, true] {
        let layout = crate::testdir::layout("progress-occupied-temp-");
        let target = layout.prefix.join("unrelated");
        std::fs::write(&target, b"do not touch").unwrap();
        let tmp = layout
            .prefix
            .join(format!("progress.json.tmp-{}", std::process::id()));
        if alias {
            std::os::unix::fs::symlink(&target, &tmp).unwrap();
        } else {
            std::fs::write(&tmp, b"previous writer's temporary").unwrap();
        }
        let sink = ProgressSink::create(&layout.progress_file(), "install");
        let published = sink
            .as_ref()
            .is_some_and(|sink| sink.plan(&[("codex".into(), 100)]));
        let fresh = read_progress(&layout).is_some_and(|file| file.queue == ["codex"]);
        outcomes.push((alias, published && fresh));
        assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
        if alias {
            assert_eq!(std::fs::read_link(&tmp).unwrap(), target);
        } else {
            assert_eq!(std::fs::read(&tmp).unwrap(), b"previous writer's temporary");
        }
    }
    assert!(
        outcomes.iter().all(|(_, fresh)| *fresh),
        "regular and alias cases both attempted: {outcomes:?}"
    );
}

#[test]
fn the_v1_example_snapshot_round_trips() {
    let json = r#"{"v":1, "pid":41234, "pass":"net", "started_unix":0, "heartbeat_unix":0,
             "overall":{"programs_done":2,"programs_total":9,"bytes_done":18022400,"bytes_total":96411648},
             "queue":["trust","robi"],
             "programs":{"trust":{"phase":"download","bytes_done":4300800,"bytes_total":30104576,
                                  "build":210,"bumped":true,"error":null}}}"#;
    let parsed: ProgressFile = aterm_json::from_str(json).unwrap();
    assert_eq!(parsed.v, PROGRESS_VERSION);
    assert_eq!(parsed.pid, Some(41234));
    assert_eq!(parsed.pass, "net");
    assert_eq!(parsed.overall.programs_total, 9);
    assert_eq!(parsed.queue, vec!["trust".to_string(), "robi".to_string()]);
    let trust = &parsed.programs["trust"];
    assert_eq!(trust.phase, Phase::Download);
    assert_eq!(trust.bytes_done, 4_300_800);
    assert_eq!(trust.build, Some(210));
    assert!(trust.bumped && trust.error.is_none());
    let reparsed: ProgressFile =
        aterm_json::from_str(&aterm_json::to_string(&parsed).unwrap()).unwrap();
    assert_eq!(reparsed, parsed);
}

#[test]
fn phase_strings_are_the_documented_lowercase_vocabulary() {
    for (phase, wire) in [
        (Phase::Queued, "\"queued\""),
        (Phase::Download, "\"download\""),
        (Phase::Verify, "\"verify\""),
        (Phase::Extract, "\"extract\""),
        (Phase::Link, "\"link\""),
        (Phase::Done, "\"done\""),
        (Phase::Failed, "\"failed\""),
        (Phase::Skipped, "\"skipped\""),
    ] {
        assert_eq!(aterm_json::to_string(&phase).unwrap(), wire);
        assert_eq!(aterm_json::from_str::<Phase>(wire).unwrap(), phase);
    }
}

#[test]
fn unknown_fields_are_ignored_and_missing_fields_fail_safe() {
    let future = r#"{"v":1, "hologram":true,
             "programs":{"trust":{"phase":"done","shiny_new_meter":9000}}}"#;
    let parsed: ProgressFile = aterm_json::from_str(future).unwrap();
    assert_eq!(parsed.pid, None, "no pid claim without a pid field");
    assert_eq!(
        parsed.heartbeat_unix, 0,
        "a missing heartbeat is epoch-stale — the safe direction"
    );
    assert_eq!(parsed.programs["trust"].phase, Phase::Done);
    let v2: ProgressFile = aterm_json::from_str(r#"{"v":2}"#).unwrap();
    assert_ne!(v2.v, PROGRESS_VERSION);
}
