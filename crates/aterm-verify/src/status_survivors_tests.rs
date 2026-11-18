// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::event_log::{EventKind, EventLog, event_value};
use crate::runs::{Survivor, SurvivorObservation, SurvivorReason, SurvivorScope};
use aterm_uds::process::{Birth, EndRefusal, Ended, Identity, Lineage};

fn survivor(pid: i32) -> SurvivorObservation {
    SurvivorObservation {
        run_id: "41983-9007199254740993".into(),
        boot_id: "saved-boot".into(),
        scope: SurvivorScope::Run,
        survivor: Survivor {
            pid,
            lineage: Some(Lineage {
                identity: Identity {
                    pid,
                    birth: Birth(9_007_199_254_740_993),
                    idversion: Some(i32::MIN),
                },
                uniqueid: u64::MAX,
                parent: 1,
                pgid: 600,
                launcher_version: Some(1),
                tty: None,
            }),
            executable: Some(PathBuf::from("/PRIVATE-executable-SENTINEL")),
            reason: SurvivorReason::CustodyNotProven,
            observed_ms: 123,
        },
    }
}

fn survivor_value(observation: &SurvivorObservation) -> aterm_json::Value {
    event_value(EventKind::ProcessSurvivor, observation.payload())
}

fn survivor_line(observation: &SurvivorObservation) -> String {
    format!("verify-event: {}\n", survivor_value(observation))
}

fn survivor_writer() -> EventLog {
    let ctx = crate::Ctx::new(
        PathBuf::from("/fixture"),
        crate::Scope::crate_only("aterm-verify"),
        crate::EnvSnapshot::default(),
        PathBuf::from("/fixture/scratch"),
    )
    .with_checks(&[crate::Check::Census]);
    let mut writer = EventLog::unrecorded();
    writer.bind(crate::event_log::Context::captured(&ctx));
    writer
}

fn survivor_terminal() -> String {
    let result = crate::report::CheckResult {
        check: crate::Check::Census,
        outcome: crate::Outcome::Ok,
        seconds: 1.0,
        detail: "PRIVATE-detail-SENTINEL".into(),
        findings: Vec::new(),
        rerun: "PRIVATE-rerun-SENTINEL".into(),
        stopped: false,
    };
    format!(
        "{}{}",
        crate::report::check_event(&result, &[]),
        crate::report::run_event(&[result], crate::exit::PASS)
    )
}

fn changed_survivor(key: &str, value: aterm_json::Value) -> String {
    let mut event = survivor_value(&survivor(900));
    event.as_object_mut().unwrap().insert(key.into(), value);
    format!("verify-event: {event}\n")
}

fn appended_field(json: &str, key: &str, value: &aterm_json::Value) -> String {
    format!("{},\"{key}\":{value}}}", json.strip_suffix('}').unwrap())
}

#[test]
fn saved_survivors_keep_original_identities_file_order_and_invocation_partitions() {
    let first = survivor_writer();
    let second = survivor_writer();
    let writers = [&first, &second];
    let mut text = String::new();
    let mut expected = Vec::new();
    for index in 0..21 {
        let pid = 900 + index;
        let mut original = survivor(pid);
        original.survivor.observed_ms = u64::try_from(100 - index).unwrap();
        if index % 2 == 0 {
            original.scope = SurvivorScope::Stage {
                stage: "fmt --fix".into(),
                pgid: 600,
            };
        }
        if index % 5 == 0 {
            original.survivor.lineage = None;
            original.survivor.reason = SurvivorReason::IdentityUnreadable;
        }
        let unbound = survivor_line(&original);
        let invocation = if index % 3 == 2 {
            text.push_str(&unbound);
            None
        } else {
            let writer = writers[usize::try_from(index % 3).unwrap()];
            text.push_str(&writer.prepare(&unbound));
            writer.id().map(str::to_owned)
        };
        let lineage = original.survivor.lineage.map(|lineage| {
            aterm_json::json!({
                "birth": lineage.identity.birth.0,
                "idversion": lineage.identity.idversion,
                "uniqueid": lineage.uniqueid,
            })
        });
        let stage = index % 2 == 0;
        expected.push(aterm_json::json!({
            "invocation_id": invocation,
            "run_id": original.run_id,
            "boot_id": original.boot_id,
            "pid": pid,
            "lineage": lineage,
            "observed_ms": original.survivor.observed_ms,
            "scope": if stage { "stage" } else { "run" },
            "stage": stage.then_some("fmt --fix"),
            "group": stage.then_some(600_u32),
            "reason": original.survivor.reason.word(),
        }));
    }
    let terminal = survivor_terminal();
    let baseline = observe(&terminal);
    let observed = observe(&format!("{text}{terminal}"));
    let json = observed.json();
    assert_eq!(json["process_survivors"].as_array().unwrap(), &expected);
    assert_eq!(json["process_survivor_records"].as_u64(), Some(21));
    assert_eq!(json["process_survivor_omitted_records"].as_u64(), Some(0));
    assert_eq!(json["process_survivor_row_cap"].as_u64(), Some(64));
    assert_eq!(
        json["process_survivor_observations_complete"].as_bool(),
        Some(false)
    );
    assert_eq!(observed.invocations.len(), 2);
    for group in &observed.invocations {
        let retained = group.observed.json();
        let expected_group: Vec<_> = expected
            .iter()
            .filter(|row| row["invocation_id"].as_str() == Some(group.id.as_str()))
            .cloned()
            .collect();
        assert_eq!(
            retained["process_survivors"].as_array().unwrap(),
            &expected_group
        );
        assert_eq!(retained["process_survivor_records"].as_u64(), Some(7));
    }
    let unknown = observed.unattributed.as_ref().unwrap().json();
    let expected_unknown: Vec<_> = expected
        .iter()
        .filter(|row| row["invocation_id"].is_null())
        .cloned()
        .collect();
    assert_eq!(
        unknown["process_survivors"].as_array().unwrap(),
        &expected_unknown
    );
    assert_eq!(observed.checks, baseline.checks);
    assert_eq!(observed.early_cases, baseline.early_cases);
    assert_eq!(observed.terminal_consistency, baseline.terminal_consistency);
    assert_eq!(observed.malformed_complete_records, 0);
    assert_eq!(observed.unprojected_records, 0);
    assert!(!json.to_string().contains("PRIVATE-"));
    assert!(!observed.render().contains("PRIVATE-"));
}

#[test]
fn survivor_numbers_remain_exact_and_each_saved_reason_is_only_metadata() {
    let reasons = [
        SurvivorReason::CustodyNotProven,
        SurvivorReason::IdentityUnreadable,
        SurvivorReason::EndRefused(EndRefusal::Stale),
        SurvivorReason::EndRefused(EndRefusal::NotProven),
        SurvivorReason::EndRefused(EndRefusal::SignalFailed),
        SurvivorReason::ExitUnconfirmedAfterEnd(Ended {
            termed: true,
            killed: false,
            gone: false,
        }),
        SurvivorReason::ExecutionUnobserved,
    ];
    let mut text = String::new();
    for reason in reasons {
        let mut original = survivor(i32::MAX);
        original.survivor.reason = reason;
        original.survivor.observed_ms = u64::MAX;
        let lineage = original.survivor.lineage.as_mut().unwrap();
        lineage.identity.birth = Birth(u64::MAX);
        lineage.identity.idversion = None;
        text.push_str(&survivor_line(&original));
    }
    let observed = observe(&text);
    let json = observed.json();
    let rows = json["process_survivors"].as_array().unwrap();
    assert_eq!(rows.len(), reasons.len());
    for (row, reason) in rows.iter().zip(reasons) {
        assert_eq!(row["reason"].as_str(), Some(reason.word()));
        assert_eq!(row["pid"].as_i64(), Some(i64::from(i32::MAX)));
        assert_eq!(row["observed_ms"].as_u64(), Some(u64::MAX));
        assert_eq!(row["lineage"]["birth"].as_u64(), Some(u64::MAX));
        assert_eq!(row["lineage"]["uniqueid"].as_u64(), Some(u64::MAX));
        assert!(row["lineage"]["idversion"].is_null());
        assert!(row.get("end").is_none());
    }
    assert!(json.to_string().contains("18446744073709551615"));
    assert!(observed.checks.is_empty());
    assert_eq!(observed.terminal_records, 0);
}

#[test]
fn malformed_survivor_payloads_stay_unprojected_without_changing_check_evidence() {
    let base = survivor_value(&survivor(900));
    let base_json = base.to_string();
    let mut invalid = Vec::<(String, String)>::new();
    for (key, value) in [
        ("run_id", aterm_json::json!("0-1")),
        ("run_id", aterm_json::json!("2147483648-1")),
        ("run_id", aterm_json::json!("1-18446744073709551616")),
        ("run_id", aterm_json::json!("1-1x")),
        ("boot_id", aterm_json::json!("")),
        ("boot_id", aterm_json::json!("b".repeat(257))),
        ("pid", aterm_json::json!(0)),
        ("pid", aterm_json::json!(-1)),
        ("pid", aterm_json::json!(2_147_483_648_u64)),
        ("pid", aterm_json::json!(900.0)),
        ("pid", aterm_json::json!("900")),
        ("observed_ms", aterm_json::json!(-1)),
        ("observed_ms", aterm_json::json!(1.0)),
        ("observed_ms", aterm_json::json!("1")),
        ("scope", aterm_json::json!("unknown")),
        ("stage", aterm_json::json!("not a run field")),
        ("group", aterm_json::json!(600)),
        ("reason", aterm_json::json!("unknown")),
        ("lineage", aterm_json::json!({})),
        ("lineage", aterm_json::json!({"birth": 1})),
        ("lineage", aterm_json::json!([1, null, 2])),
        (
            "lineage",
            aterm_json::json!({"birth": null,"idversion": null,"uniqueid": 2}),
        ),
        (
            "lineage",
            aterm_json::json!({"birth": 1,"idversion": 2_147_483_648_u64,"uniqueid": 2}),
        ),
    ] {
        invalid.push((key.into(), changed_survivor(key, value)));
    }
    for key in [
        "run_id",
        "boot_id",
        "pid",
        "observed_ms",
        "scope",
        "stage",
        "group",
        "reason",
        "lineage",
    ] {
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove(key);
        invalid.push((
            format!("missing {key}"),
            format!("verify-event: {missing}\n"),
        ));
        let duplicate = appended_field(&base_json, key, &base[key]);
        invalid.push((
            format!("duplicate {key}"),
            format!("verify-event: {duplicate}\n"),
        ));
    }
    let lineage = base["lineage"].to_string();
    for key in ["birth", "idversion", "uniqueid"] {
        let mut missing = base.clone();
        missing
            .as_object_mut()
            .unwrap()
            .get_mut("lineage")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        invalid.push((
            format!("missing lineage {key}"),
            format!("verify-event: {missing}\n"),
        ));
        let duplicate = appended_field(&lineage, key, &base["lineage"][key]);
        let json = base_json.replace(
            &format!("\"lineage\":{lineage}"),
            &format!("\"lineage\":{duplicate}"),
        );
        invalid.push((
            format!("duplicate lineage {key}"),
            format!("verify-event: {json}\n"),
        ));
    }
    let escaped_pid = appended_field(&base_json, "\\u0070id", &base["pid"]);
    invalid.push((
        "escaped duplicate pid".into(),
        format!("verify-event: {escaped_pid}\n"),
    ));
    let escaped_birth = appended_field(&lineage, "\\u0062irth", &base["lineage"]["birth"]);
    let escaped_birth = base_json.replace(
        &format!("\"lineage\":{lineage}"),
        &format!("\"lineage\":{escaped_birth}"),
    );
    invalid.push((
        "escaped duplicate birth".into(),
        format!("verify-event: {escaped_birth}\n"),
    ));
    for token in ["-0", "1e3", "18446744073709551616"] {
        let json = base_json.replace("\"observed_ms\":123", &format!("\"observed_ms\":{token}"));
        assert_ne!(json, base_json);
        invalid.push((
            format!("observed_ms {token}"),
            format!("verify-event: {json}\n"),
        ));
    }
    for key in ["birth", "uniqueid"] {
        for token in ["-1", "-0", "1.0", "1e3", "18446744073709551616", "\"1\""] {
            let needle = format!("\"{key}\":{}", base["lineage"][key]);
            let replacement = format!("\"{key}\":{token}");
            let bad_lineage = lineage.replace(&needle, &replacement);
            assert_ne!(bad_lineage, lineage);
            let json = base_json.replace(
                &format!("\"lineage\":{lineage}"),
                &format!("\"lineage\":{bad_lineage}"),
            );
            invalid.push((
                format!("lineage {key} {token}"),
                format!("verify-event: {json}\n"),
            ));
        }
    }
    let mut stage = survivor(900);
    stage.scope = SurvivorScope::Stage {
        stage: "fmt".into(),
        pgid: 600,
    };
    for (key, value) in [
        ("stage", aterm_json::Value::Null),
        ("stage", aterm_json::json!("s".repeat(4097))),
        ("group", aterm_json::Value::Null),
        ("group", aterm_json::json!(1)),
        ("group", aterm_json::json!(-1)),
        ("group", aterm_json::json!(4_294_967_296_u64)),
    ] {
        let mut value_of_stage = survivor_value(&stage);
        value_of_stage
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        invalid.push((
            format!("stage {key}"),
            format!("verify-event: {value_of_stage}\n"),
        ));
    }
    let terminal = survivor_terminal();
    let baseline = observe(&terminal);
    for (label, line) in invalid {
        let observed = observe(&format!("{terminal}{line}"));
        let json = observed.json();
        assert!(
            json["process_survivors"].as_array().unwrap().is_empty(),
            "{label}"
        );
        assert_eq!(
            json["process_survivor_records"].as_u64(),
            Some(0),
            "{label}"
        );
        assert_eq!(observed.unprojected_records, 1, "{label}");
        assert_eq!(observed.malformed_complete_records, 0, "{label}");
        assert_eq!(observed.checks, baseline.checks, "{label}");
        assert_eq!(
            observed.terminal_consistency, baseline.terminal_consistency,
            "{label}"
        );
    }
    for (key, value) in [
        ("schema", aterm_json::json!(1)),
        ("\\u0073chema", aterm_json::json!(1)),
        ("event", aterm_json::json!("process-survivor")),
        ("\\u0065vent", aterm_json::json!("process-survivor")),
    ] {
        let duplicate = appended_field(&base_json, key, &value);
        let observed = observe(&format!("{terminal}verify-event: {duplicate}\n"));
        assert_eq!(observed.malformed_complete_records, 1, "{key}");
        assert_eq!(observed.unprojected_records, 0, "{key}");
        assert_eq!(observed.checks, baseline.checks, "{key}");
        assert_eq!(observed.terminal_consistency, "unavailable", "{key}");
    }
    let unsupported = observe(&changed_survivor("schema", aterm_json::json!(2)));
    assert_eq!(unsupported.malformed_complete_records, 1);
    assert_eq!(unsupported.unprojected_records, 0);
}

#[test]
fn survivor_rows_are_bounded_without_deduplication_and_require_complete_records() {
    let mut text = String::new();
    for index in 0..66 {
        text.push_str(&survivor_line(&survivor(500 + index / 2)));
    }
    let observed = observe(&text);
    let json = observed.json();
    let rows = json["process_survivors"].as_array().unwrap();
    assert_eq!(rows.len(), 64);
    assert_eq!(json["process_survivor_records"].as_u64(), Some(66));
    assert_eq!(json["process_survivor_omitted_records"].as_u64(), Some(2));
    assert_eq!(rows[0], rows[1]);
    assert_eq!(rows[0]["pid"].as_i64(), Some(500));
    assert_eq!(rows[63]["pid"].as_i64(), Some(531));
    let one = survivor_line(&survivor(900));
    let crlf = observe(&one.replace('\n', "\r\n"));
    assert_eq!(crlf.json()["process_survivor_records"].as_u64(), Some(1));
    let partial = observe(one.trim_end_matches('\n'));
    assert!(partial.trailing_partial);
    assert_eq!(partial.json()["process_survivor_records"].as_u64(), Some(0));
    let mut oversized = survivor_value(&survivor(900));
    oversized.as_object_mut().unwrap().insert(
        "padding".into(),
        aterm_json::json!("x".repeat(MAX_RECORD_BYTES + 1)),
    );
    let oversized = observe(&format!("verify-event: {oversized}\n"));
    assert_eq!(oversized.malformed_complete_records, 1);
    assert_eq!(
        oversized.json()["process_survivor_records"].as_u64(),
        Some(0)
    );
}

#[test]
fn survivor_presentation_quotes_full_bounded_labels_and_omits_private_fields() {
    let mut stage = "\n\u{1b}\\\"界".repeat(100);
    stage.push_str(&"x".repeat(4096 - stage.len()));
    let mut original = survivor(900);
    original.boot_id = "b".repeat(256);
    original.scope = SurvivorScope::Stage {
        stage: stage.clone(),
        pgid: u32::MAX,
    };
    let mut value = survivor_value(&original);
    for key in [
        "executable",
        "word",
        "end",
        "executable_native",
        "output",
        "detail",
        "rerun",
    ] {
        value.as_object_mut().unwrap().insert(
            key.into(),
            aterm_json::json!(format!("PRIVATE-{key}-SENTINEL")),
        );
    }
    let observed = observe(&format!("verify-event: {value}\n"));
    let json = observed.json();
    let row = &json["process_survivors"][0];
    assert_eq!(row["stage"].as_str(), Some(stage.as_str()));
    assert_eq!(row["boot_id"].as_str(), Some(original.boot_id.as_str()));
    assert_eq!(row["group"].as_u64(), Some(u64::from(u32::MAX)));
    let rendered = observed.render();
    assert!(!rendered.contains('\u{1b}'));
    assert!(rendered.contains(&aterm_json::Value::String(stage).to_string()));
    assert!(!rendered.contains("PRIVATE-"));
    assert!(!json.to_string().contains("PRIVATE-"));
    for key in [
        "executable",
        "word",
        "end",
        "executable_native",
        "output",
        "detail",
        "rerun",
    ] {
        assert!(row.get(key).is_none(), "{key}");
    }
}
