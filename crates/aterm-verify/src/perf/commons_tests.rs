// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn report() -> Value {
    json!({"schema":1,"warmup_rounds":2,"rounds":8,"process_cpu_ns":800,
        "per_round_cpu_ns":100,"clock_read_cpu_ns":1,"moved_records":0,
        "workload":"explicit","tables":1})
}

#[test]
fn measurement_requires_one_complete_consistent_quiet_report() {
    let valid = format!("{PREFIX}{}", report());
    assert!(parse_report(&valid, Workload::Explicit).is_ok());
    assert!(parse_report(&valid, Workload::Catalog).is_err());
    assert!(parse_report(&valid, Workload::LongLog).is_err());
    assert!(parse_report(&valid, Workload::LateJoiner).is_err());
    assert!(parse_report("running 0 tests", Workload::Explicit).is_err());
    assert!(parse_report(&format!("{valid}\n{valid}"), Workload::Explicit).is_err());
    for (key, value) in [
        ("schema", 2),
        ("rounds", 0),
        ("per_round_cpu_ns", 99),
        ("moved_records", 1),
        ("tables", 2),
    ] {
        let mut invalid = report();
        invalid
            .as_object_mut()
            .unwrap()
            .insert(key.into(), json!(value));
        assert!(
            parse_report(&format!("{PREFIX}{invalid}"), Workload::Explicit).is_err(),
            "{key}"
        );
    }
    let mut missing = report();
    missing.as_object_mut().unwrap().remove("clock_read_cpu_ns");
    assert!(parse_report(&format!("{PREFIX}{missing}"), Workload::Explicit).is_err());
    let mut catalog = report();
    let fields = catalog.as_object_mut().unwrap();
    fields.insert("workload".into(), json!("catalog"));
    fields.insert("tables".into(), json!(2));
    assert!(parse_report(&format!("{PREFIX}{catalog}"), Workload::Catalog).is_ok());
    assert!(parse_report(&format!("{PREFIX}{catalog}"), Workload::Explicit).is_err());
    let mut long = report();
    long.as_object_mut()
        .unwrap()
        .insert("workload".into(), json!("long-log"));
    assert!(parse_report(&format!("{PREFIX}{long}"), Workload::LongLog).is_ok());
    assert!(parse_report(&format!("{PREFIX}{long}"), Workload::Explicit).is_err());
    let mut late = report();
    late.as_object_mut()
        .unwrap()
        .insert("workload".into(), json!("late-joiner"));
    assert!(parse_report(&format!("{PREFIX}{late}"), Workload::LateJoiner).is_ok());
    assert!(parse_report(&format!("{PREFIX}{late}"), Workload::LongLog).is_err());
    let mut large = report();
    large
        .as_object_mut()
        .unwrap()
        .insert("workload".into(), json!("large-table"));
    assert!(parse_report(&format!("{PREFIX}{large}"), Workload::LargeTable).is_ok());
    assert!(parse_report(&format!("{PREFIX}{large}"), Workload::Explicit).is_err());
}

/// Each set of flags names one workload and its fixture test, or is refused.
#[test]
fn the_flags_name_one_workload_and_its_test() {
    let of = |set: [bool; 4]| Workload::of(set[0], set[1], set[2], set[3]);
    assert_eq!(of([false; 4]).unwrap(), Workload::Explicit);
    let named = [
        Workload::Catalog,
        Workload::LongLog,
        Workload::LateJoiner,
        Workload::LargeTable,
    ];
    for (at, workload) in named.into_iter().enumerate() {
        let mut set = [false; 4];
        set[at] = true;
        assert_eq!(of(set).unwrap(), workload);
        for other in (0..4).filter(|other| *other != at) {
            let mut both = set;
            both[other] = true;
            assert!(of(both).is_err(), "{both:?}");
        }
    }
    assert_eq!(Workload::LongLog.test(false), LONG_LOG_TEST);
    assert_eq!(Workload::LongLog.test(true), SUSTAINED_LONG_LOG_TEST);
    assert_eq!(Workload::LateJoiner.test(false), LATE_JOINER_TEST);
    assert_eq!(Workload::LateJoiner.test(true), SUSTAINED_LATE_JOINER_TEST);
    assert_eq!(Workload::LargeTable.test(false), LARGE_TABLE_TEST);
    assert_eq!(Workload::LargeTable.test(true), SUSTAINED_LARGE_TABLE_TEST);
}
