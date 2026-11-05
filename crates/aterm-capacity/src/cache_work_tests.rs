// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};

const RUN: &str = "20261105T103732783Z-1ab3ba0487a929a0";

fn work() -> Value {
    json!({"entries":2,"input_bytes":41,"bytes_hashed":17,"metadata_reads":12,
        "resolution_steps":8,"link_reads":1,"observations":9,"revalidations":7})
}

fn unit(index: u64, measured: bool) -> Value {
    json!({"index":index,"package_id":"path+file:///fixture#sample@0.1.0",
        "target":{"name":"build-script-build","kind":"build-script"},
        "mode":if measured {"run-custom-build"} else {"build"},
        "platform":"test-host","features":["default"],"requested":false,
        "attempts":if measured {2} else {0},"captured_attempts":if measured {1} else {0},
        "uncacheable_attempts":if measured {1} else {0},
        "phases":if measured {vec![json!({"phase":"freshness","attempts":2,
            "captured_attempts":1,"uncacheable_attempts":1,"work":work()})]} else {vec![]},
        "work":if measured {work()} else {Value::Null}})
}

fn fixture() -> Value {
    json!({"schema":SCHEMA,"run_id":RUN,"scope":SCOPE,"availability":"complete",
        "unavailable_reason":null,"build_outcome":"not-recorded",
        "denominators":{"registered_units":2,"run_script_units":1,"observed_units":1,
            "units_without_attempts":1,"attempts_started":2,"attempts_finished":2,
            "captured_attempts":1,"uncacheable_attempts":1},
        "units":[unit(3,false),unit(8,true)],"totals":work()})
}

fn check(value: &Value) -> io::Result<Report> {
    parse(&aterm_json::to_vec(value).unwrap(), RUN)
}

fn field<'a>(value: &'a mut Value, name: &str) -> &'a mut Value {
    value.as_object_mut().unwrap().get_mut(name).unwrap()
}

fn measured(value: &mut Value) -> &mut Value {
    &mut field(value, "units").as_array_mut().unwrap()[1]
}

#[test]
fn complete_report_preserves_all_units_and_partial_uncacheable_costs() {
    let report = check(&fixture()).unwrap();
    assert_eq!(report.run_id(), RUN);
    assert_eq!(report.scope(), SCOPE);
    assert_eq!(report.availability(), Availability::Complete);
    assert_eq!(report.denominators().registered_units, 2);
    assert_eq!(report.units()[0].index(), 3);
    assert!(report.units()[0].work().is_none());
    let unit = &report.units()[1];
    assert_eq!(unit.index(), 8);
    assert_eq!(unit.mode(), Mode::RunCustomBuild);
    assert_eq!(unit.uncacheable_attempts(), 1);
    assert_eq!(unit.phases()[0].phase(), Phase::Freshness);
    assert_eq!(unit.work(), report.totals());
    assert_eq!(report.totals().unwrap().bytes_hashed, 17);
    report.summary().validate().unwrap();
}

#[test]
fn legacy_and_complete_zero_are_distinct() {
    let mut value = fixture();
    *field(&mut value, "availability") = json!("unavailable");
    *field(&mut value, "unavailable_reason") = json!("legacy-log");
    *field(&mut value, "units") = json!([unit(3, false)]);
    *field(&mut value, "denominators") = json!({"registered_units":1,"run_script_units":0,
        "observed_units":0,"units_without_attempts":1,"attempts_started":0,
        "attempts_finished":0,"captured_attempts":0,"uncacheable_attempts":0});
    *field(&mut value, "totals") = Value::Null;
    let report = check(&value).unwrap();
    assert_eq!(report.availability(), Availability::UnavailableLegacyLog);
    assert!(report.totals().is_none());
    *field(&mut value, "availability") = json!("complete");
    *field(&mut value, "unavailable_reason") = Value::Null;
    assert!(check(&value).is_err());
    *field(&mut value, "totals") = json!({"entries":0,"input_bytes":0,"bytes_hashed":0,
        "metadata_reads":0,"resolution_steps":0,"link_reads":0,"observations":0,"revalidations":0});
    assert_eq!(check(&value).unwrap().totals(), Some(Work::default()));
}

#[test]
fn every_denominator_and_metric_is_conserved() {
    for key in [
        "registered_units",
        "run_script_units",
        "observed_units",
        "units_without_attempts",
        "attempts_started",
        "attempts_finished",
        "captured_attempts",
        "uncacheable_attempts",
    ] {
        let mut value = fixture();
        *field(field(&mut value, "denominators"), key) = json!(99);
        assert!(check(&value).is_err(), "denominator {key}");
    }
    for key in [
        "entries",
        "input_bytes",
        "bytes_hashed",
        "metadata_reads",
        "resolution_steps",
        "link_reads",
        "observations",
        "revalidations",
    ] {
        for boundary in [0, 1, 2] {
            let mut value = fixture();
            let work = match boundary {
                0 => field(&mut value, "totals"),
                1 => field(measured(&mut value), "work"),
                _ => field(
                    &mut field(measured(&mut value), "phases")
                        .as_array_mut()
                        .unwrap()[0],
                    "work",
                ),
            };
            *field(work, key) = json!(99);
            assert!(check(&value).is_err(), "metric {key} boundary {boundary}");
        }
    }
}

#[test]
fn identities_order_modes_and_unobserved_nulls_are_strict() {
    for (key, replacement) in [
        ("schema", json!("other")),
        ("scope", json!("generated-outputs")),
        ("build_outcome", json!("success")),
        ("run_id", json!("20261105T103732783Z-ffffffffffffffff")),
        ("unavailable_reason", json!("legacy-log")),
    ] {
        let mut value = fixture();
        *field(&mut value, key) = replacement;
        assert!(check(&value).is_err());
    }
    for (key, replacement) in [
        ("mode", json!("build")),
        ("index", json!(3)),
        ("attempts", json!(1)),
        ("work", Value::Null),
        ("phases", json!([])),
    ] {
        let mut value = fixture();
        *field(measured(&mut value), key) = replacement;
        assert!(check(&value).is_err());
    }
    let mut value = fixture();
    let rows = field(&mut value, "units").as_array_mut().unwrap();
    rows.swap(0, 1);
    assert!(check(&value).is_err());
    let mut value = fixture();
    let first = &mut field(&mut value, "units").as_array_mut().unwrap()[0];
    *field(first, "work") = work();
    assert!(check(&value).is_err());
    let mut value = fixture();
    field(measured(&mut value), "target")
        .as_object_mut()
        .unwrap()
        .insert("kind".into(), json!("bin"));
    assert!(check(&value).is_err());
    let mut value = fixture();
    let phases = field(measured(&mut value), "phases")
        .as_array_mut()
        .unwrap();
    phases.push(phases[0].clone());
    assert!(check(&value).is_err());
}

#[test]
fn duplicated_unknown_missing_and_positional_fields_are_refused() {
    let bytes = aterm_json::to_string(&fixture()).unwrap();
    for (from, to) in [
        ("\"schema\":", "\"schema\":\"ignored\",\"schema\":"),
        (
            "\"registered_units\":",
            "\"registered_units\":0,\"registered_units\":",
        ),
        (
            "\"package_id\":",
            "\"package_id\":\"ignored\",\"package_id\":",
        ),
        ("\"name\":", "\"name\":\"ignored\",\"name\":"),
        ("\"phase\":", "\"phase\":\"publication\",\"phase\":"),
        (
            "\"metadata_reads\":",
            "\"metadata_reads\":0,\"metadata_reads\":",
        ),
    ] {
        assert!(parse(bytes.replacen(from, to, 1).as_bytes(), RUN).is_err());
    }
    for key in ["unavailable_reason", "totals"] {
        let mut value = fixture();
        value.as_object_mut().unwrap().remove(key);
        assert!(check(&value).is_err());
    }
    let mut value = fixture();
    measured(&mut value).as_object_mut().unwrap().remove("work");
    assert!(check(&value).is_err());
    let mut value = fixture();
    value
        .as_object_mut()
        .unwrap()
        .insert("unknown".into(), json!(1));
    assert!(check(&value).is_err());
    let mut value = fixture();
    *field(&mut value, "totals") = json!([2, 41, 17, 12, 8, 1, 9, 7]);
    assert!(check(&value).is_err());
    let mut value = fixture();
    *field(measured(&mut value), "target") = json!(["build-script-build", "build-script"]);
    assert!(check(&value).is_err());
}

#[test]
fn malformed_numeric_overflow_and_resource_bounds_fail_closed() {
    let bytes = aterm_json::to_string(&fixture()).unwrap();
    for number in ["-1", "1.0", "1e0", "18446744073709551616"] {
        let changed = bytes.replacen(
            "\"metadata_reads\":12",
            &format!("\"metadata_reads\":{number}"),
            1,
        );
        assert!(parse(changed.as_bytes(), RUN).is_err(), "{number}");
    }
    for changed in [
        bytes[..bytes.len() - 1].to_owned(),
        format!("{bytes}{{}}"),
        "[]".into(),
    ] {
        assert!(parse(changed.as_bytes(), RUN).is_err());
    }
    assert!(parse(&vec![b' '; MAX_REPORT_BYTES + 1], RUN).is_err());
    let mut value = fixture();
    *field(measured(&mut value), "features") = json!(vec!["x"; MAX_FEATURES + 1]);
    assert!(check(&value).is_err());
    let mut value = fixture();
    *field(measured(&mut value), "package_id") = json!("x".repeat(MAX_TEXT_BYTES + 1));
    assert!(check(&value).is_err());
    let mut value = fixture();
    let phases = field(measured(&mut value), "phases")
        .as_array_mut()
        .unwrap();
    let phase = phases[0].clone();
    phases.extend([phase.clone(), phase.clone(), phase]);
    assert!(check(&value).is_err());
    let mut value = fixture();
    let first = field(measured(&mut value), "phases")
        .as_array_mut()
        .unwrap()[0]
        .clone();
    let mut second = first.clone();
    *field(&mut second, "phase") = json!("publication");
    *field(field(&mut second, "work"), "bytes_hashed") = json!(u64::MAX);
    *field(measured(&mut value), "phases") = json!([first, second]);
    assert!(check(&value).is_err());
}

#[test]
fn summary_dto_validation_cannot_label_missing_costs_complete() {
    let mut summary = check(&fixture()).unwrap().summary();
    summary.totals = None;
    assert!(summary.validate().is_err());
    summary.availability = Availability::UnavailableLegacyLog;
    assert!(summary.validate().is_err());
    let mut summary = check(&fixture()).unwrap().summary();
    summary.denominators.captured_attempts = u64::MAX;
    summary.denominators.uncacheable_attempts = 1;
    assert!(summary.validate().is_err());
}
