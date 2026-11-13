// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// Evidence hashes the linked executable. Wrong targets, a missing executable, and one
/// changed harness byte cannot authorize reuse of the same measurement.
#[test]
fn measure_evidence_is_the_linked_harness_content() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let executable = dir.path().join("paint");
    std::fs::write(&executable, b"first linked harness").unwrap();
    let artifact = |kind: &str| aterm_json::json!({"reason":"compiler-artifact", "manifest_path":"/r/crates/aterm-conformance/Cargo.toml", "target":{"name":"paint","kind":[kind]}, "executable":executable});
    let wire = artifact("bench");
    let digest = |wire: &aterm_json::Value| {
        let line = super::compile_unit_events("compile", &wire.to_string())
            .pop()
            .unwrap();
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        event["measure_harness_sha256"].as_str().map(str::to_owned)
    };
    let first = digest(&wire).unwrap();
    std::fs::write(&executable, b"second linked harness").unwrap();
    assert_ne!(digest(&wire).unwrap(), first);
    assert!(digest(&artifact("test")).is_none());
    std::fs::remove_file(executable).unwrap();
    assert!(digest(&wire).is_none());
}
use super::*;

#[test]
fn provisional_messages_expose_only_strict_bounded_metadata() {
    let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
    let dispatch = capture.lines().next().unwrap();
    let artifact = capture.lines().nth(1).unwrap();
    for line in [dispatch, artifact] {
        let wire = provisional_compile_event("test compile", line).unwrap();
        let value: aterm_json::Value =
            aterm_json::from_str(wire.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(value["provisional"].as_bool(), Some(true));
        assert_eq!(value["sample_complete"].as_bool(), Some(false));
        for key in [
            "fresh",
            "dirty_reason",
            "producer_unit_sha256",
            "manifest_path",
            "package_id",
            "executable",
            "artifact_reported",
        ] {
            assert!(value.get(key).is_none(), "{key}");
        }
        assert!(!wire.contains("/Users/"));
    }
    for wire in [
        dispatch.replace("\"build\"", "\"unknown-mode\""),
        dispatch.replace("\"host\"", "\"unknown-kind\""),
        dispatch.replace("\"changed-env\"", "\"unknown-cause\""),
        dispatch.replace("\"reason\":", "\"extra\":true,\"reason\":"),
        dispatch.replace("\"fresh\":false,", ""),
        dispatch.replace(
            "3585558e13bc333691c3e05830d0ce2175baa184bf4ff01aa9f76465b5784241",
            "invalid",
        ),
        dispatch.replace("\"reason\":", "\"reason\":\"build-finished\",\"reason\":"),
        artifact.replace(
            "\"name\":\"dispatch_serialization_probe\"",
            "\"name\":\"/private/path\"",
        ),
        artifact.replace("\"kind\":[\"lib\"]", "\"kind\":[\"unrecognized\"]"),
        artifact.replace("\"target\":{", "\"target\":{\"name\":\"duplicate\","),
        artifact.replace("\"features\":[]", "\"features\":[{\"x\":1,\"x\":2}]"),
    ] {
        assert!(provisional_compile_event("test compile", &wire).is_none());
    }
}

#[test]
fn duplicate_wire_fields_cannot_establish_a_unit_cause_or_artifact_match() {
    let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
    let dispatch = capture.lines().next().unwrap();
    let artifact = capture.lines().nth(1).unwrap();
    for (name, first, second) in [
        (
            "dispatch cause",
            dispatch.replace(
                "\"dirty_reason\":\"changed-env\"",
                "\"dirty_reason\":\"forced\",\"dirty_reason\":\"changed-env\"",
            ),
            artifact.to_owned(),
        ),
        (
            "artifact freshness",
            dispatch.to_owned(),
            artifact.replace("\"fresh\":false", "\"fresh\":true,\"fresh\":false"),
        ),
        (
            "artifact reason",
            dispatch.to_owned(),
            artifact.replacen(
                "\"reason\":\"compiler-artifact\"",
                "\"reason\":\"build-finished\",\"reason\":\"compiler-artifact\"",
                1,
            ),
        ),
        (
            "hidden artifact reason",
            dispatch.to_owned(),
            artifact.replacen(
                "\"reason\":\"compiler-artifact\"",
                "\"reason\":\"compiler-artifact\",\"reason\":\"build-finished\"",
                1,
            ),
        ),
    ] {
        let events = compile_unit_events("ambiguous wire", &format!("{first}\n{second}\n"));
        let values: Vec<aterm_json::Value> = events
            .iter()
            .map(|line| {
                aterm_json::from_str::<aterm_json::Value>(
                    line.strip_prefix("verify-event: ").unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(values.len(), 2, "{name}: {events:?}");
        assert!(
            values.iter().all(|event| event
                .get("rebuild_reason")
                .is_none_or(|reason| *reason == aterm_json::Value::Null)),
            "{name}: {events:?}"
        );
        assert!(
            values.iter().all(|event| event
                .get("artifact_reported")
                .is_none_or(|reported| *reported == aterm_json::Value::Null)),
            "{name}: {events:?}"
        );
    }

    // An ambiguous identity field can name two units. Invalidate both exact observed
    // identities without erasing an independent third unit's valid evidence.
    let x = aterm_json::from_str::<aterm_json::Value>(dispatch).unwrap();
    let x = x["trust_unit_identity_sha256"].as_str().unwrap();
    let y = "b".repeat(64);
    let z = "c".repeat(64);
    let ambiguous = dispatch.replacen(
        &format!("\"trust_unit_identity_sha256\":\"{x}\""),
        &format!("\"trust_unit_identity_sha256\":\"{x}\",\"trust_unit_identity_sha256\":\"{y}\""),
        1,
    );
    assert_ne!(ambiguous, dispatch);
    let output = [
        dispatch.to_owned(),
        artifact.to_owned(),
        producer_dispatch(&y).to_string(),
        producer_artifact(&y).to_string(),
        producer_dispatch(&z).to_string(),
        producer_artifact(&z).to_string(),
        ambiguous.clone(),
    ]
    .join("\n");
    let events: Vec<aterm_json::Value> = compile_unit_events("scoped ambiguity", &output)
        .iter()
        .map(|line| {
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()
        })
        .collect();
    for identity in [x, y.as_str()] {
        let affected: Vec<_> = events
            .iter()
            .filter(|event| event["producer_unit_sha256"].as_str() == Some(identity))
            .collect();
        assert_eq!(affected.len(), 2);
        assert!(affected.iter().all(|event| {
            event
                .get("rebuild_reason")
                .is_none_or(|v| *v == aterm_json::Value::Null)
                && event
                    .get("artifact_reported")
                    .is_none_or(|v| *v == aterm_json::Value::Null)
        }));
    }
    let independent = events
        .iter()
        .find(|event| {
            event["producer_unit_sha256"].as_str() == Some(z.as_str())
                && event["event"].as_str() == Some("compile-unit-finished")
        })
        .unwrap();
    assert_eq!(independent["rebuild_reason"].as_str(), Some("changed-file"));
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"].as_str() == Some("compile-unit-message-unavailable"))
            .count(),
        1
    );

    let no_identity = ambiguous.replace(x, "invalid").replace(&y, "invalid-too");
    let output = [
        producer_dispatch(&z).to_string(),
        producer_artifact(&z).to_string(),
        no_identity,
    ]
    .join("\n");
    let events = compile_unit_events("unidentified ambiguity", &output);
    assert!(
        events
            .iter()
            .any(|event| event.contains("\"rebuild_reason\":\"changed-file\""))
    );
    // No taint survives this invocation.
    let healthy = compile_unit_events("healthy", capture);
    assert!(
        healthy
            .iter()
            .any(|event| event.contains("\"rebuild_reason\":\"changed-env\""))
    );
}

#[test]
fn decoded_ambiguous_artifact_kind_preserves_every_identity_taint() {
    let x = "a".repeat(64);
    let y = "b".repeat(64);
    let z = "c".repeat(64);
    let ambiguous = format!(
        r#"{{"re\u0061son":"compiler-\u0061rtifact","trust_unit_identity_sha256":"{x}","trust_unit_identity_sha256":"{y}"}}"#
    );
    let output = [
        producer_dispatch(&x).to_string(),
        producer_artifact(&x).to_string(),
        producer_dispatch(&y).to_string(),
        producer_artifact(&y).to_string(),
        producer_dispatch(&z).to_string(),
        producer_artifact(&z).to_string(),
        ambiguous,
    ]
    .join("\n");
    let events: Vec<aterm_json::Value> = compile_unit_events("escaped ambiguous artifact", &output)
        .iter()
        .map(|line| aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap())
        .collect();
    for identity in [&x, &y, &z] {
        let event = events
            .iter()
            .find(|event| {
                event["event"].as_str() == Some("compile-unit-finished")
                    && event["producer_unit_sha256"].as_str() == Some(identity.as_str())
            })
            .unwrap();
        if identity == &z {
            assert_eq!(event["rebuild_reason"].as_str(), Some("changed-file"));
        } else {
            assert!(event["rebuild_reason"].is_null());
            assert_eq!(
                event["rebuild_reason_status"].as_str(),
                Some("ambiguous-message")
            );
        }
    }
    assert_eq!(crate::compile_account::read(&output).units.unknown, 1);
}

fn producer_artifact(identity: &str) -> aterm_json::Value {
    aterm_json::json!({
        "reason": "compiler-artifact", "trust_unit_identity_sha256": identity,
        "trust_compile_mode": "test", "trust_compile_kind": "host", "fresh": false,
    })
}

// The producer's bounded targo.compilation-unit-dispatched.v1 wire envelope.
fn producer_dispatch(identity: &str) -> aterm_json::Value {
    aterm_json::json!({
        "reason": "compilation-unit-dispatched", "schema": "targo.compilation-unit-dispatched.v1",
        "trust_unit_identity_sha256": identity, "trust_compile_mode": "test",
        "trust_compile_kind": "host", "fresh": false, "dirty_reason": "changed-file",
    })
}

fn retained_units(messages: &[aterm_json::Value]) -> Vec<aterm_json::Value> {
    let output = messages
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    compile_unit_events("compile", &output)
        .iter()
        .map(|line| {
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()
        })
        .collect()
}

#[test]
fn compile_units_replay_a_real_producer_pair_but_do_not_invent_its_missing_dispatch() {
    // Exact records 1 and 3 from the controlled producer capture documented in
    // docs/measured/compiler-unit-evidence-2026-11-04.md, without path redaction.
    let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
    let events: Vec<aterm_json::Value> = compile_unit_events("captured producer", capture)
        .iter()
        .map(|line| {
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()
        })
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["artifact_reported"].as_bool(), Some(true));
    assert_eq!(events[1]["rebuild_reason"].as_str(), Some("changed-env"));
    assert_eq!(
        events[0]["producer_unit_sha256"],
        events[1]["producer_unit_sha256"]
    );
    assert_eq!(events[1]["fresh"].as_bool(), Some(false));
    let artifact = capture.lines().nth(1).unwrap();
    let without = compile_unit_events("captured artifact only", artifact);
    let without: aterm_json::Value =
        aterm_json::from_str(without[0].strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(without["rebuild_reason"], aterm_json::Value::Null);
    assert_eq!(
        without["rebuild_reason_status"].as_str(),
        Some("not-reported-by-producer")
    );
}

#[test]
fn nested_wire_ambiguity_taints_an_otherwise_matching_unit() {
    let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
    let artifact = capture.lines().nth(1).unwrap();
    let ambiguous = artifact.replacen('{', r#"{"extra":{"nested":[{"key":1,"key":2}]},"#, 1);
    let events = compile_unit_events("nested ambiguity", &format!("{capture}\n{ambiguous}"));
    let finished: Vec<aterm_json::Value> = events
        .iter()
        .map(|line| aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap())
        .collect();
    assert_eq!(
        finished
            .iter()
            .filter(|event| event["event"].as_str() == Some("compile-unit-finished"))
            .count(),
        1
    );
    assert!(
        finished
            .iter()
            .any(|event| event["event"].as_str() == Some("compile-unit-message-unavailable"))
    );
    assert!(
        finished
            .iter()
            .filter(|event| event["event"].as_str() == Some("compile-unit-finished"))
            .all(|event| event["rebuild_reason"].is_null()
                && event["rebuild_reason_status"].as_str() == Some("ambiguous-message"))
    );
}

#[test]
fn compile_units_join_only_matching_producer_dispatches_in_this_call() {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    let a = producer_dispatch(&first);
    let mut b = producer_dispatch(&second);
    b.as_object_mut()
        .unwrap()
        .insert("dirty_reason".into(), aterm_json::json!("features-changed"));
    let events = retained_units(&[
        a.clone(),
        b,
        producer_artifact(&second),
        producer_artifact(&first),
        a,
    ]);
    assert_eq!(
        events[2]["rebuild_reason"].as_str(),
        Some("features-changed")
    );
    assert_eq!(events[3]["rebuild_reason"].as_str(), Some("changed-file"));
    assert_eq!(events[0]["artifact_reported"].as_bool(), Some(true));

    // A dispatched build script need not emit any compiler artifact. Neither a dispatch
    // nor a later invocation's artifact establishes completion in this invocation.
    let mut script = producer_dispatch(&first);
    script.as_object_mut().unwrap().insert(
        "trust_compile_mode".into(),
        aterm_json::json!("run-custom-build"),
    );
    assert_eq!(
        retained_units(&[script])[0]["artifact_reported"],
        aterm_json::Value::Null
    );
    assert_eq!(
        retained_units(&[producer_artifact(&first)])[0]["rebuild_reason"],
        aterm_json::Value::Null
    );
}

#[test]
fn compile_units_refuse_mismatched_or_conflicting_dispatch_evidence() {
    use aterm_json::{Value, json};
    let identity = "a".repeat(64);
    for (field, invalid) in [
        ("schema", json!("targo.compilation-unit-dispatched.v2")),
        ("trust_unit_identity_sha256", json!("b".repeat(64))),
        ("trust_unit_identity_sha256", json!("invalid")),
        ("trust_compile_mode", json!("build")),
        ("trust_compile_kind", json!("target")),
        ("fresh", json!(true)),
        ("dirty_reason", json!("unsupported-sensitive-prose")),
        ("extra", json!("unsupported-sensitive-prose")),
    ] {
        let mut dispatch = producer_dispatch(&identity);
        dispatch
            .as_object_mut()
            .unwrap()
            .insert(field.into(), invalid);
        let events = retained_units(&[dispatch, producer_artifact(&identity)]);
        assert_eq!(
            events[1]["rebuild_reason"],
            Value::Null,
            "{field}: {events:?}"
        );
        assert_eq!(
            events[0]["artifact_reported"],
            Value::Null,
            "{field}: {events:?}"
        );
        assert!(
            !events[0]
                .to_string()
                .contains("unsupported-sensitive-prose")
        );
    }
    let mut conflicting = producer_dispatch(&identity);
    conflicting
        .as_object_mut()
        .unwrap()
        .insert("dirty_reason".into(), json!("forced"));
    let events = retained_units(&[
        producer_dispatch(&identity),
        conflicting,
        producer_artifact(&identity),
    ]);
    assert_eq!(events[2]["rebuild_reason"], Value::Null);
    assert_eq!(
        events[2]["rebuild_reason_status"].as_str(),
        Some("conflicting-dispatch")
    );
    assert_eq!(events[0]["artifact_reported"], Value::Null);

    let mut conflicting = producer_artifact(&identity);
    conflicting
        .as_object_mut()
        .unwrap()
        .insert("trust_compile_kind".into(), json!("target"));
    let events = retained_units(&[
        producer_dispatch(&identity),
        producer_artifact(&identity),
        conflicting,
    ]);
    assert_eq!(events[1]["rebuild_reason"], Value::Null);
    assert_eq!(events[0]["artifact_reported"], Value::Null);
}

#[test]
fn compile_units_distinguish_fresh_work_unknown_causes_and_fresh_build_classification() {
    use aterm_json::{Value, json};
    let identity = "a".repeat(64);
    let mut dispatch = producer_dispatch(&identity);
    dispatch
        .as_object_mut()
        .unwrap()
        .insert("dirty_reason".into(), json!("unknown"));
    let events = retained_units(&[dispatch.clone(), producer_artifact(&identity)]);
    assert_eq!(events[1]["rebuild_reason"], Value::Null);
    assert_eq!(
        events[1]["rebuild_reason_status"].as_str(),
        Some("producer-unknown")
    );
    for cause in ["fresh-build", "output-integrity"] {
        dispatch
            .as_object_mut()
            .unwrap()
            .insert("dirty_reason".into(), json!(cause));
        assert_eq!(
            retained_units(&[dispatch.clone(), producer_artifact(&identity)])[1]["rebuild_reason"]
                .as_str(),
            Some(cause)
        );
    }
    dispatch
        .as_object_mut()
        .unwrap()
        .insert("fresh".into(), json!(true));
    dispatch
        .as_object_mut()
        .unwrap()
        .insert("dirty_reason".into(), Value::Null);
    let mut artifact = producer_artifact(&identity);
    artifact
        .as_object_mut()
        .unwrap()
        .insert("fresh".into(), json!(true));
    let events = retained_units(&[dispatch, artifact]);
    assert_eq!(events[1]["rebuild_reason"], Value::Null);
    assert_eq!(
        events[1]["rebuild_reason_status"].as_str(),
        Some("not-applicable")
    );
    assert_eq!(events[0]["artifact_reported"].as_bool(), Some(true));
}

#[test]
fn compile_units_retain_producer_context_without_inventing_a_rebuild_cause() {
    let unit = aterm_json::json!({
        "reason": "compiler-artifact", "package_id": "path+file:///r/lib#1",
        "manifest_path": "/r/lib/Cargo.toml",
        "target": {"name": "lib", "kind": ["lib"], "src_path": "/r/lib/src/lib.rs", "extra": "unreported"},
        "profile": {"opt_level": "0", "test": true}, "features": ["a"],
        "trust_compile_target": "aarch64-apple-darwin", "trust_compile_kind": "host",
        "trust_compile_mode": "test", "trust_unit_identity_sha256": "a".repeat(64),
        "fresh": false, "dirty_reason": "source-changed", "extra": "unreported",
    });
    let events = compile_unit_events("test compile", &unit.to_string());
    assert_eq!(events.len(), 1);
    let event: aterm_json::Value =
        aterm_json::from_str(events[0].strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["package_id"], unit["package_id"]);
    assert_eq!(event["target"]["name"], unit["target"]["name"]);
    assert_eq!(event["profile"], unit["profile"]);
    assert_eq!(event["features"], unit["features"]);
    assert_eq!(
        event["producer_unit_sha256"],
        unit["trust_unit_identity_sha256"]
    );
    assert_eq!(event["compile_target"], unit["trust_compile_target"]);
    assert_eq!(event["fresh"].as_bool(), Some(false));
    assert_eq!(event["rebuild_reason"], aterm_json::Value::Null);
    assert_eq!(
        event["rebuild_reason_status"].as_str(),
        Some("not-reported-by-producer")
    );
    assert!(!events[0].contains("unreported"));
    assert!(!events[0].contains("source-changed"));
}

#[test]
fn compile_units_keep_unknown_freshness_distinct_from_a_rebuild() {
    let output = concat!(
        "not JSON\n{broken\n",
        "{\"reason\":\"build-finished\",\"fresh\":false}\n",
        "{\"reason\":\"compiler-artifact\",\"fresh\":\"false\",\"trust_unit_identity_sha256\":\"invalid\"}\n",
        "{\"reason\":\"compiler-artifact\",\"fresh\":true}\n",
    );
    let events = compile_unit_events("compile", output);
    assert_eq!(events.len(), 2);
    let read = |line: &str| {
        aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
            .unwrap()
    };
    let unknown = read(&events[0]);
    assert_eq!(unknown["fresh"], aterm_json::Value::Null);
    assert_eq!(unknown["producer_unit_sha256"], aterm_json::Value::Null);
    assert_eq!(
        unknown["rebuild_reason_status"].as_str(),
        Some("freshness-unknown")
    );
    assert_eq!(
        read(&events[1])["rebuild_reason_status"].as_str(),
        Some("not-applicable")
    );
}

#[test]
fn a_started_stage_carries_its_index_without_claiming_a_result() {
    let line = stage_started(7, "compile \"helper\"");
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["event"].as_str(), Some("stage-started"));
    assert_eq!(event["stage_index"].as_u64(), Some(7));
    assert_eq!(event["stage"].as_str(), Some("compile \"helper\""));
    assert!(event["started_at_unix_ms"].as_u64().is_some());
    assert_eq!(event["at_unix_ms"], event["started_at_unix_ms"]);
    assert_eq!(event["seconds"].as_f64(), Some(0.0));
    assert!(event["cpu_s"].is_null());
    assert!(event.get("decisions").is_none());
    assert!(event.get("outcome").is_none());
}

#[test]
fn live_stage_event_keeps_findings_and_unavailable_checks_distinct() {
    let mut report = Report::new("measurement");
    report.fail("throughput below floor");
    report.cannot_run("window never focused");
    let line = stage_event(
        7,
        Some(Check::Measure),
        &report,
        Duration::from_secs(12),
        Duration::from_secs(3),
        None,
    );
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["waiting_s"].as_f64(), Some(12.0));
    assert_eq!(event["stage_index"].as_u64(), Some(7));
    assert_eq!(event["execution_s"].as_f64(), Some(3.0));
    assert_eq!(event["seconds"].as_f64(), Some(3.0));
    assert_eq!(event["at_unix_ms"], event["ended_at_unix_ms"]);
    // When it ended, so a release's timeline dates a stage's red where it happened.
    assert!(
        event["ended_at_unix_ms"].as_u64().is_some_and(|ms| ms > 0),
        "{event}"
    );
    assert!(event["child_cpu"].is_null());
    assert!(event["cpu_s"].is_null());
    assert_eq!(event["decisions"][0]["outcome"].as_str(), Some("FAIL"));
    assert_eq!(event["decisions"][1]["outcome"].as_str(), Some("NOT RUN"));
}

#[test]
fn row_events_keep_wall_cpu_and_unread_cpu_distinct() {
    let read = |line: String| {
        aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
            .unwrap()
    };
    let start = read(row_event("objc drives", "ime", None));
    assert_eq!(start["event"].as_str(), Some("row-started"));
    assert_eq!(start["seconds"].as_f64(), Some(0.0));
    assert!(start["cpu_s"].is_null());
    let end = read(row_event(
        "objc drives",
        "ime",
        Some((
            Duration::from_secs(3),
            Some(crate::exec::TreeUsage {
                cpu: Duration::from_secs(2),
                ..Default::default()
            }),
        )),
    ));
    assert_eq!(end["event"].as_str(), Some("row-finished"));
    assert_eq!(end["seconds"].as_f64(), Some(3.0));
    assert_eq!(end["cpu_s"].as_f64(), Some(2.0));
    assert_eq!(end["stage"].as_str(), Some("objc drives"));
    assert_eq!(end["row"].as_str(), Some("ime"));
    assert!(end["at_unix_ms"].as_u64().is_some());
    let absent = read(row_event(
        "objc drives",
        "ime",
        Some((Duration::from_secs(3), None)),
    ));
    assert!(absent["cpu_s"].is_null());
}

#[test]
fn only_measurement_row_events_are_forwarded_with_original_timestamps() {
    let row = "verify-event: {\"schema\":1,\"event\":\"measure-row-finished\",\"matrix\":\"fixture\",\"row\":\"sample\",\"at_unix_ms\":123,\"seconds\":2,\"cpu_s\":null,\"cpu_scope\":\"runner-process-and-reaped-descendants\",\"cpu_intervals_overlap_for_concurrent_rows\":true}";
    let text = format!(
        "ordinary output\nverify-event: broken\nverify-event: {{\"event\":\"stage-finished\"}}\n{row}\n"
    );
    assert_eq!(measurement_events(&text), [row]);
    for invalid in [
        row.replace("\"schema\":1", "\"schema\":2"),
        row.replace("\"schema\":1", "\"schema\":1,\"schema\":1"),
        row.replace("measure-row-finished", "unknown-measurement-kind"),
    ] {
        assert!(measurement_events(&invalid).is_empty());
    }
}

#[test]
fn observed_cpu_is_a_separate_lower_bound_not_execution_wall_time() {
    let line = stage_event(
        8,
        None,
        &Report::new("parallel build"),
        Duration::from_secs(12),
        Duration::from_secs(3),
        Some(crate::memory::ObservedCpu {
            nanoseconds: 8_000_000_000,
            processes: 4,
        }),
    );
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["child_cpu"]["observed_s"].as_f64(), Some(8.0));
    assert_eq!(event["child_cpu"]["observed_processes"].as_u64(), Some(4));
    assert_eq!(event["child_cpu"]["coverage"].as_str(), Some("lower-bound"));
    assert_eq!(event["execution_s"].as_f64(), Some(3.0));
}

fn result(check: Check, outcome: Outcome) -> CheckResult {
    CheckResult {
        check,
        outcome,
        seconds: 1.0,
        detail: String::new(),
        findings: Vec::new(),
        rerun: format!("targo --unverified verify {}", check.name()),
        stopped: false,
    }
}

#[test]
fn the_worst_row_decides_and_a_failed_test_is_its_own_finding() {
    let mut lint = Report::new("tippy");
    lint.pass("tippy -p a");
    let r = judge(Check::Lint, &[&lint], 2.0, "-p a", "rerun".into(), None);
    assert_eq!(r.outcome, Outcome::Ok);

    let mut skipped = Report::new("doc");
    skipped.skip("targo test --doc (no library)");
    let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into(), None);
    assert_eq!(r.outcome, Outcome::Skip);
    skipped.skip("another (no tool)");
    let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into(), None);
    assert_eq!(r.findings.len(), 2, "each reason listed under it");

    let mut test = Report::new("test");
    test.pass("compile");
    test.fail("a row");
    test.cannot_run("no disk");
    let r = judge(Check::Test, &[&test], 0.0, "", "rerun".into(), None);
    assert_eq!(r.outcome, Outcome::Fail(Severity::GateFailed));
    let ids: Vec<&str> = r.findings.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        ["a row", "NOT RUN no disk"],
        "the failure, then what could not run beside it under its own word"
    );
    assert_eq!(r.detail, "a row");
    assert_eq!(duration(0.0), "0.0s");
    assert_eq!(duration(-1.5), "0.0s");
    assert_eq!(duration(f64::NAN), "0.0s");
    assert!(!duration(f64::INFINITY).is_empty());
}

#[test]
fn the_exit_code_is_the_worst_check_and_a_release_skip_is_could_not_run() {
    let ok = result(Check::Fmt, Outcome::Ok);
    let skip = result(Check::Doc, Outcome::Skip);
    let fail = result(Check::Lint, Outcome::Fail(Severity::GateFailed));
    let unrun = result(Check::Test, Outcome::Fail(Severity::CouldNotRun));
    let code = |results: &[CheckResult], release: bool| verdict(results, release, 1.0).1;
    assert_eq!(code(&[ok.clone(), skip.clone()], false), crate::exit::PASS);
    assert_eq!(
        code(&[ok.clone(), skip], true),
        crate::exit::COULD_NOT_RUN,
        "`release` needs every check"
    );
    assert_eq!(
        code(&[ok.clone(), fail, unrun.clone()], false),
        crate::exit::FAILED
    );
    assert_eq!(code(&[ok, unrun], false), crate::exit::COULD_NOT_RUN);
    assert_eq!(duration(f64::NAN), "0.0s");
    assert_eq!(duration(-1.0), "0.0s");
    assert_eq!(duration(f64::INFINITY), "0.0s");
    assert!(duration(1e30).ends_with("15s"));
    assert_eq!(duration(185.0), "3m05s");
}

#[test]
fn a_check_the_first_red_stopped_is_counted_apart_from_what_could_not_run() {
    let words = "stopped: the release failed at fmt";
    let mut halted = Report::new("tippy");
    halted.cannot_run(format!("tippy — {words}"));
    let mut machine = Report::new("doc");
    machine.cannot_run("targo test --doc — no disk");
    let lint = judge(
        Check::Lint,
        &[&halted],
        0.0,
        "",
        "rerun".into(),
        Some(words),
    );
    let doc = judge(
        Check::Doc,
        &[&machine],
        0.0,
        "",
        "rerun".into(),
        Some(words),
    );
    assert!(lint.stopped, "every NOT RUN row says the halt's words");
    assert!(
        !doc.stopped,
        "the machine kept it from running, not the halt"
    );
    let fmt = result(Check::Fmt, Outcome::Fail(Severity::GateFailed));
    let (line, code) = verdict(&[fmt, lint.clone(), doc], true, 1.0);
    assert_eq!(code, crate::exit::FAILED);
    assert_eq!(
        line,
        "verify: FAIL — fmt failed; doc could not run; \
         1 not run (stopped at the first red: lint); 0 ok in 1.0s"
    );
    let (line, code) = verdict(&[lint], true, 1.0);
    assert_eq!(
        (line.as_str(), code),
        (
            "verify: COULD NOT RUN — lint could not run; 0 ok in 1.0s",
            crate::exit::COULD_NOT_RUN
        ),
        "beside no failure nothing was stopped at a red"
    );
    let bare = judge(Check::Lint, &[&halted], 0.0, "", "rerun".into(), None);
    assert!(!bare.stopped, "a run with no halt stopped nothing");
}

#[test]
fn a_checkout_that_moved_mid_run_could_not_run_whatever_its_checks_said() {
    let fail = result(Check::Cells, Outcome::Fail(Severity::GateFailed));
    let ran = verdict(&[fail], false, 1.0);
    let (line, code) = moved_checkout(ran.clone(), Some("fe2021965c11"), Some("b46f0cf64d22"));
    assert_eq!(code, crate::exit::COULD_NOT_RUN);
    assert!(
        line.contains("moved from fe2021965c to b46f0cf64d") && line.contains("cells failed"),
        "names both commits and keeps the run's own verdict: {line}"
    );
    let pass = verdict(&[result(Check::Fmt, Outcome::Ok)], false, 1.0);
    assert_eq!(
        moved_checkout(pass, Some("a"), Some("b")).1,
        crate::exit::COULD_NOT_RUN,
        "a pass on two trees is no pass"
    );

    assert_eq!(
        moved_checkout(ran.clone(), Some("fe2021965c11"), Some("fe2021965c11")),
        ran,
        "one HEAD throughout keeps the run's verdict"
    );
    assert_eq!(
        moved_checkout(ran.clone(), None, Some("b46f0cf64d22")),
        ran,
        "an unread HEAD is no move"
    );
}
