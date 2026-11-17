// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::boot_cost::{Book, Collector, ProcessKey, WakeCount, WakeCounts};
use crate::explain::HistoricalReported;
use crate::sys::json::contract_violations;

fn observation(at_ns: u64) -> Observation {
    Observation {
        at_ns,
        before_ns: Some(at_ns),
        after_ns: Some(at_ns),
        process: Some(ProcessKey {
            pid: 7,
            start: 11,
            version: Some(1),
        }),
        counter_epoch: 3,
        process_cpu: RawCounters {
            own_ns: Some(13),
            inherited_ns: Some(17),
        },
        coalition_id: Some(19),
        coalition_cpu: RawCounters {
            own_ns: Some(23),
            inherited_ns: Some(29),
        },
        historical_reported: HistoricalReported {
            own_ns: 31,
            tabs_ns: 37,
            incomplete: false,
        },
        wakes: Some(WakeCounts {
            epoch: 41,
            variants: vec![
                WakeCount {
                    kind: "Output".to_owned(),
                    count: 43,
                },
                WakeCount {
                    kind: "WarmupResult".to_owned(),
                    count: 47,
                },
            ],
        }),
        ..Observation::default()
    }
}

fn report() -> BootReport {
    let mut collector = Collector::new(Book::default());
    collector.observe(Some("boot-a"), observation(0)).unwrap();
    collector
        .observe(Some("boot-a"), observation(901_000_000_000))
        .unwrap();
    BootReport {
        book: collector.book().clone(),
        boot_id: Some("boot-a".to_owned()),
        persistence: "saved",
        ..BootReport::default()
    }
}

fn array(value: &Jv) -> &[Jv] {
    let Jv::Arr(rows) = value else {
        panic!("expected array: {value:?}")
    };
    rows
}

fn first_boot(value: &Jv) -> &Jv {
    &array(value.get("boots").unwrap())[0]
}

fn first_observation(value: &Jv) -> &Jv {
    &array(first_boot(value).get("observations").unwrap())[0]
}

#[test]
fn one_actual_901_second_cut_serves_both_targets_without_repeating_cpu() {
    let report = report();
    let value = json(Some(&report));
    assert!(
        contract_violations(&value).is_empty(),
        "{}",
        value.to_text()
    );
    let boot = first_boot(&value);
    let observations = array(boot.get("observations").unwrap());
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].get("at_ns_decimal"),
        Some(&Jv::s("901000000000"))
    );
    let targets = array(boot.get("targets").unwrap());
    assert_eq!(targets.len(), 2);
    for (target, expected) in targets.iter().zip(["180000000000", "900000000000"]) {
        assert_eq!(target.get("target_ns_decimal"), Some(&Jv::s(expected)));
        assert_eq!(target.get("observation_index"), Some(&Jv::Int(0)));
        assert!(target.get("process_cpu").is_none());
    }
    let lines = text(Some(&report), 1);
    let observations: Vec<_> = lines
        .iter()
        .filter(|l| l.text.starts_with("boot_observation "))
        .collect();
    assert_eq!(observations.len(), 1);
    assert!(observations[0].text.contains("at_ns=901000000000"));
    assert!(!observations[0].text.contains("at_ns=180000000000"));
    let targets: Vec<_> = lines
        .iter()
        .filter(|l| l.text.starts_with("boot_target "))
        .collect();
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|l| l.text.ends_with("observation=0")));
    assert!(targets.iter().all(|l| !l.text.contains("cpu")));
    let wakes: Vec<_> = lines
        .iter()
        .filter(|l| l.text.starts_with("boot_ui_wake "))
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(wakes[0].text.contains("process=7:11:1 wake_epoch=41"));
    assert!(wakes[0].text.contains("kind=WarmupResult count=47"));
    assert!(lines.iter().any(|l| l.text.ends_with("more=1")));
    let o = first_observation(&value);
    assert_eq!(
        o.get("delivered_ui_wakes").unwrap().get("process"),
        o.get("process")
    );
    assert_eq!(
        array(
            first_observation(&value)
                .get("delivered_ui_wakes")
                .unwrap()
                .get("variants")
                .unwrap()
        )
        .len(),
        2
    );
}

#[test]
fn equal_wake_epochs_with_rising_counts_keep_their_different_process_sources() {
    let mut collector = Collector::new(Book::default());
    let mut first = observation(180_000_000_000);
    first.wakes.as_mut().unwrap().epoch = 1;
    collector.observe(Some("boot-a"), first).unwrap();
    let mut second = observation(900_000_000_000);
    second.process = Some(ProcessKey {
        pid: 8,
        start: 12,
        version: Some(2),
    });
    second.wakes.as_mut().unwrap().epoch = 1;
    for variant in &mut second.wakes.as_mut().unwrap().variants {
        variant.count += 100;
    }
    collector.observe(Some("boot-a"), second).unwrap();
    let report = BootReport {
        book: collector.book().clone(),
        ..report()
    };
    let value = json(Some(&report));
    let observations = array(first_boot(&value).get("observations").unwrap());
    assert_eq!(observations.len(), 2);
    for o in observations {
        let wakes = o.get("delivered_ui_wakes").unwrap();
        assert_eq!(wakes.get("epoch_decimal"), Some(&Jv::s("1")));
        assert_eq!(wakes.get("process"), o.get("process"));
    }
    assert_ne!(
        observations[0].get("process"),
        observations[1].get("process")
    );
    let lines = text(Some(&report), 1);
    assert!(
        lines
            .iter()
            .any(|l| l.text.contains("process=7:11:1 wake_epoch=1"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.text.contains("process=8:12:2 wake_epoch=1"))
    );
    assert!(contract_violations(&value).is_empty());
}

#[test]
fn raw_channels_remain_independent_published_accounting_without_execution_assurance() {
    let report = report();
    let value = json(Some(&report));
    assert_eq!(
        value.get("accounting_basis"),
        Some(&Jv::s("published_counters"))
    );
    assert_eq!(
        value.get("execution_window"),
        Some(&Jv::why("unavailable:execution_window_unknown"))
    );
    assert_eq!(
        value.get("cpu_channels"),
        Some(&Jv::s("independent_do_not_sum"))
    );
    assert_eq!(
        value.get("wake_scope"),
        Some(&Jv::s("delivered_ui_proxy_events"))
    );
    let o = first_observation(&value);
    for (channel, own, inherited) in [("process_cpu", "13", "17"), ("coalition_cpu", "23", "29")] {
        let channel = o.get(channel).unwrap();
        assert_eq!(channel.get("own_ns_decimal"), Some(&Jv::s(own)));
        assert_eq!(channel.get("inherited_ns_decimal"), Some(&Jv::s(inherited)));
        assert!(channel.get("total_ns_decimal").is_none());
    }
    assert_eq!(
        o.get("historical_reported").unwrap().get("tabs_ns_decimal"),
        Some(&Jv::s("37"))
    );
    assert!(o.get("total_cpu_ns_decimal").is_none());
    assert!(text(Some(&report), 8).iter().any(|l| {
        l.text
            .contains("execution_window=unavailable:execution_window_unknown")
    }));
}

#[test]
fn zero_readings_and_empty_wake_census_stay_distinct_from_missing_sources() {
    let mut report = report();
    let o = &mut report.book.boots[0].observations[0];
    o.process_cpu = RawCounters {
        own_ns: Some(0),
        inherited_ns: None,
    };
    o.coalition_cpu = RawCounters {
        own_ns: None,
        inherited_ns: Some(0),
    };
    o.before_ns = None;
    o.after_ns = None;
    o.coalition_id = None;
    o.wakes = Some(WakeCounts::default());
    o.coverage = o
        .coverage
        .with(Coverage::PARTIAL)
        .with(Coverage::COUNTERS_MISSING);
    report.book.boots[0].coverage = report.book.boots[0]
        .coverage
        .with(Coverage::COUNTERS_MISSING);
    let value = json(Some(&report));
    let o = first_observation(&value);
    assert_eq!(
        o.get("process_cpu").unwrap().get("own_ns_decimal"),
        Some(&Jv::s("0"))
    );
    assert_eq!(
        o.get("process_cpu").unwrap().get("inherited_ns_decimal"),
        Some(&Jv::why("not_sampled:inherited_counter"))
    );
    assert_eq!(
        o.get("coalition_cpu").unwrap().get("own_ns_decimal"),
        Some(&Jv::why("not_sampled:own_counter"))
    );
    assert_eq!(
        o.get("coalition_cpu").unwrap().get("inherited_ns_decimal"),
        Some(&Jv::s("0"))
    );
    assert_eq!(
        o.get("coalition_id_decimal"),
        Some(&Jv::why("not_sampled:coalition_identity"))
    );
    assert!(
        array(
            o.get("delivered_ui_wakes")
                .unwrap()
                .get("variants")
                .unwrap()
        )
        .is_empty()
    );
    report.book.boots[0].observations[0].wakes = None;
    report.book.boots[0].observations[0].coverage = report.book.boots[0].observations[0]
        .coverage
        .with(Coverage::WAKE_MISSING);
    report.book.boots[0].coverage = report.book.boots[0].coverage.with(Coverage::WAKE_MISSING);
    assert_eq!(
        first_observation(&json(Some(&report))).get("delivered_ui_wakes"),
        Some(&Jv::why("not_sampled:delivered_ui_wakes"))
    );
    assert_eq!(json(None), Jv::why("not_sampled:boot_cost"));
    assert!(
        text(None, 8)[0]
            .text
            .contains("unavailable(not_sampled:boot_cost)")
    );
    assert!(contract_violations(&value).is_empty());
    report.book.boots[0].observations.clear();
    report.book.boots[0].targets = [None, None];
    let value = json(Some(&report));
    assert!(array(first_boot(&value).get("observations").unwrap()).is_empty());
    assert!(
        array(first_boot(&value).get("targets").unwrap())
            .iter()
            .all(|target| target.get("observation_index") == Some(&Jv::why("not_sampled:target")))
    );
}

#[test]
fn all_u64_raw_channels_and_identities_survive_above_signed_json_integer_range() {
    let mut report = report();
    report.book.boots[0].first_seen_ns = u64::MAX - 2;
    let o = &mut report.book.boots[0].observations[0];
    o.at_ns = u64::MAX;
    o.before_ns = Some(u64::MAX - 1);
    o.after_ns = Some(u64::MAX);
    o.counter_epoch = u64::MAX;
    o.process.as_mut().unwrap().start = u64::MAX;
    o.process_cpu.own_ns = Some(u64::MAX);
    o.process_cpu.inherited_ns = Some(i64::MAX as u64 + 1);
    o.coalition_id = Some(u64::MAX);
    o.coalition_cpu.own_ns = Some(u64::MAX);
    o.coalition_cpu.inherited_ns = Some(u64::MAX);
    o.historical_reported.own_ns = u64::MAX;
    o.historical_reported.tabs_ns = u64::MAX;
    o.wakes.as_mut().unwrap().epoch = u64::MAX;
    o.wakes.as_mut().unwrap().variants[0].count = u64::MAX;
    let value = json(Some(&report));
    let o = first_observation(&value);
    for key in [
        "at_ns_decimal",
        "after_ns_decimal",
        "counter_epoch_decimal",
        "coalition_id_decimal",
    ] {
        assert_eq!(o.get(key), Some(&Jv::s(u64::MAX.to_string())));
    }
    assert_eq!(
        o.get("before_ns_decimal"),
        Some(&Jv::s((u64::MAX - 1).to_string()))
    );
    assert_eq!(
        o.get("process").unwrap().get("start_decimal"),
        Some(&Jv::s(u64::MAX.to_string()))
    );
    assert_eq!(
        o.get("process_cpu").unwrap().get("inherited_ns_decimal"),
        Some(&Jv::s("9223372036854775808"))
    );
    assert_eq!(
        o.get("historical_reported").unwrap().get("tabs_ns_decimal"),
        Some(&Jv::s(u64::MAX.to_string()))
    );
    assert_eq!(
        array(
            o.get("delivered_ui_wakes")
                .unwrap()
                .get("variants")
                .unwrap()
        )[0]
        .get("count_decimal"),
        Some(&Jv::s(u64::MAX.to_string()))
    );
    assert!(contract_violations(&value).is_empty());
    assert!(
        text(Some(&report), 8)
            .iter()
            .any(|l| l.text.contains("process_own_ns=18446744073709551615"))
    );
}

#[test]
fn restored_collection_qualifications_and_persistence_failure_are_kept_separate() {
    let report = report();
    let mut initial = Collector::new(Book::default());
    initial.observe(Some("boot-a"), observation(0)).unwrap();
    initial
        .observe(Some("boot-a"), observation(180_000_000_000))
        .unwrap();
    let mut collector = Collector::new(initial.book().clone());
    collector
        .observe(Some("boot-a"), observation(902_000_000_000))
        .unwrap();
    let report = BootReport {
        book: collector.book().clone(),
        persistence: "published_durability_unknown",
        dirty: true,
        failure: Some("flush_directory:Other".to_owned()),
        ..report
    };
    let value = json(Some(&report));
    let coverage = array(first_boot(&value).get("coverage").unwrap());
    assert!(coverage.contains(&Jv::s("restored_gap")));
    assert!(coverage.contains(&Jv::s("collection_gap")));
    assert_eq!(value.get("dirty"), Some(&Jv::Bool(true)));
    assert_eq!(
        value.get("persistence"),
        Some(&Jv::s("published_durability_unknown"))
    );
    assert_eq!(value.get("failure"), Some(&Jv::s("flush_directory:Other")));
    assert_eq!(
        value.get("execution_window"),
        Some(&Jv::why("unavailable:execution_window_unknown"))
    );
    assert!(
        text(Some(&report), 8)
            .iter()
            .any(|l| l.text.contains("coverage=partial") && l.text.contains("restored_gap"))
    );
}
