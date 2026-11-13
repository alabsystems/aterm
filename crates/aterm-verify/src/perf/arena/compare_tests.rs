// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::start::tests::{LAWS, metrics};
use super::*;

struct FakeBench {
    calls: u32,
    slot_bias: f64,
    edit: Box<dyn Fn(Arm, Value) -> Value>,
    thermal: String,
    cools: Vec<f64>,
    warm_flags: Vec<bool>,
}

impl FakeBench {
    fn new() -> Self {
        Self {
            calls: 0,
            slot_bias: 0.0,
            edit: Box::new(|_, m| m),
            thermal: CLEAN.into(),
            cools: Vec::new(),
            warm_flags: Vec::new(),
        }
    }
}

const CLEAN: &str = "Note: No thermal warning level has been recorded | Note: No performance \
                     warning level has been recorded | Note: No CPU power status has been \
                     recorded";

impl Bench for FakeBench {
    fn trial(&mut self, arm: Arm, warm: bool) -> Result<Observation, String> {
        self.calls += 1;
        self.warm_flags.push(warm);
        let bias = if self.calls.is_multiple_of(2) {
            self.slot_bias
        } else {
            0.0
        };
        Ok(Observation {
            metrics: (self.edit)(arm, metrics(bias)),
            external_ms: 20.0,
            thermal: self.thermal.clone(),
        })
    }

    fn cool(&mut self, seconds: f64) {
        self.cools.push(seconds);
    }
}

fn plan(pairs: u32) -> Plan {
    Plan {
        pairs,
        laws: LAWS,
        cooldown_s: 0.0,
        warm: false,
        chip: "Apple M5 Max".into(),
        negative_control: true,
    }
}

fn total<'a>(c: &'a Comparison, name: &str) -> &'a Compared {
    c.totals
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("{name} compared"))
}

fn without(keys: &'static [&'static str], only: Arm) -> Box<dyn Fn(Arm, Value) -> Value> {
    Box::new(move |arm, mut m| {
        if arm == only {
            for k in keys {
                m.as_object_mut().expect("object").remove(*k);
            }
        }
        m
    })
}

#[test]
fn the_abba_round_fold_cancels_a_slot_bias_that_every_pair_carries() {
    let mut bench = FakeBench::new();
    bench.slot_bias = 2.0;
    let c = schedule(&mut bench, &plan(4)).expect("runs");
    assert_eq!(c.rounds, 2);
    assert!(!c.trailing_unbalanced_pair);
    for name in [
        "internal_first_present_ms",
        "rust_main_to_first_present_ms",
        "rust_main_to_first_visible_ms",
    ] {
        let m = total(&c, name);
        assert_eq!(m.deltas.len(), 4);
        assert!(
            m.deltas.iter().all(|d| (d.abs() - 2.0).abs() < 1e-9),
            "{name}: {:?}",
            m.deltas
        );
        assert_eq!(m.deltas.iter().filter(|d| **d > 0.0).count(), 2);
        let r = m.rounds.as_ref().expect("two rounds");
        assert!(
            r.values.iter().all(|v| v.abs() < 1e-9),
            "{name}: {:?}",
            r.values
        );
        assert!(r.mean.abs() < 1e-9);
        assert_eq!((r.faster, r.ties, r.slower), (0, 2, 0));
    }
    let setup = c
        .attach
        .iter()
        .find(|m| m.name == "startup_attach_window_setup_ms")
        .expect("attach compared");
    assert!(
        setup
            .rounds
            .as_ref()
            .expect("rounds")
            .values
            .iter()
            .all(|v| v.abs() < 1e-9)
    );
    let text = report(&c);
    assert!(
        text.contains("round candidate-faster delta: median=0.00ms"),
        "{text}"
    );
    assert!(text.contains("reported unit: ROUND"), "{text}");
    assert!(
        text.contains("order-contaminated pair-level delta"),
        "{text}"
    );
}

#[test]
fn the_schedule_rotates_and_cools_between_trials_only() {
    let mut bench = FakeBench::new();
    let mut p = plan(2);
    p.cooldown_s = 0.5;
    let c = schedule(&mut bench, &p).expect("runs");
    let order: Vec<(u32, u8, Arm)> = c.records.iter().map(|r| (r.pair, r.slot, r.arm)).collect();
    assert_eq!(
        order,
        vec![
            (1, 1, Arm::Baseline),
            (1, 2, Arm::Candidate),
            (2, 1, Arm::Candidate),
            (2, 2, Arm::Baseline)
        ]
    );
    assert_eq!(
        bench.cools,
        vec![0.5, 0.5, 0.5],
        "a cooldown before every trial but the first"
    );
    assert!(!c.eligibility.cooldown);
}

#[test]
fn an_odd_pair_is_excluded_and_said_out_loud() {
    let mut bench = FakeBench::new();
    bench.slot_bias = 2.0;
    let c = schedule(&mut bench, &plan(3)).expect("runs");
    assert_eq!(c.rounds, 1);
    assert!(c.trailing_unbalanced_pair);
    let m = total(&c, "internal_first_present_ms");
    assert_eq!(m.deltas.len(), 3);
    assert_eq!(m.rounds.as_ref().expect("one round").values.len(), 1);
    assert!(report(&c).contains("is odd; pair 3 has no partner"));
}

#[test]
fn one_pair_forms_no_round_and_the_report_refuses_to_dress_it_up() {
    let mut bench = FakeBench::new();
    bench.slot_bias = 2.0;
    let c = schedule(&mut bench, &plan(1)).expect("runs");
    assert_eq!(c.rounds, 0);
    let m = total(&c, "internal_first_present_ms");
    assert!(m.rounds.is_none());
    assert!((m.deltas[0] + 2.0).abs() < 1e-9, "{:?}", m.deltas);
    assert!(report(&c).contains("NO ORDER-BALANCED ROUND WAS FORMED"));
}

#[test]
fn a_legacy_baseline_compares_only_the_shared_totals_and_only_when_allowed() {
    let legacy: &'static [&'static str] = &[
        "rust_main_to_first_present_ms",
        "startup_phase_schema",
        "startup_attach_schema",
    ];
    let mut bench = FakeBench::new();
    bench.edit = without(legacy, Arm::Baseline);
    let mut p = plan(2);
    p.laws.allow_legacy = true;
    let c = schedule(&mut bench, &p).expect("runs");
    assert!(c.legacy_totals_only);
    assert!(!c.router_by_arm[&Arm::Baseline]);
    assert!(c.router_by_arm[&Arm::Candidate]);
    assert!(
        c.totals
            .iter()
            .all(|m| m.name != "rust_main_to_first_present_ms")
    );
    assert!(c.phases.is_empty() && c.attach.is_empty());
    let mut bench = FakeBench::new();
    bench.edit = without(legacy, Arm::Baseline);
    let e = schedule(&mut bench, &plan(2)).expect_err("needs the switch");
    assert!(e.contains("rust_main_to_first_present_ms is absent"), "{e}");
}

#[test]
fn a_baseline_without_attach_drops_every_partition() {
    let mut bench = FakeBench::new();
    bench.edit = without(&["startup_attach_schema"], Arm::Baseline);
    let mut p = plan(2);
    p.laws.allow_legacy = true;
    let c = schedule(&mut bench, &p).expect("runs");
    assert!(c.legacy_totals_only);
    assert!(
        c.totals
            .iter()
            .any(|m| m.name == "rust_main_to_first_present_ms")
    );
    assert!(c.phases.is_empty() && c.attach.is_empty());
}

#[test]
fn a_missing_reveal_stamp_drops_only_its_own_comparison() {
    let mut bench = FakeBench::new();
    bench.edit = without(&["rust_main_to_first_visible_ms"], Arm::Baseline);
    let c = schedule(&mut bench, &plan(2)).expect("valid without --allow-legacy-metric");
    assert!(!c.legacy_totals_only);
    assert!(
        c.totals
            .iter()
            .all(|m| m.name != "rust_main_to_first_visible_ms")
    );
    assert!(!c.attach.is_empty());
    assert!(report(&c).contains("rust_main_to_first_visible_ms: not compared"));
    let mut bench = FakeBench::new();
    let c = schedule(&mut bench, &plan(2)).expect("runs");
    let v = total(&c, "rust_main_to_first_visible_ms");
    assert_eq!(stats::median(&v.baseline), 9.0);
    assert!(
        stats::median(&v.baseline)
            < stats::median(&total(&c, "rust_main_to_first_present_ms").baseline)
    );
}

#[test]
fn warm_reaches_every_trial() {
    let mut bench = FakeBench::new();
    let mut p = plan(2);
    p.warm = true;
    let c = schedule(&mut bench, &p).expect("runs");
    assert_eq!(bench.warm_flags, vec![true; 4]);
    assert_eq!(
        result_json(&c, Value::Null).get("warm"),
        Some(&Value::Bool(true))
    );
}

#[test]
fn an_invalid_trial_names_its_pair_and_arm() {
    let mut bench = FakeBench::new();
    bench.edit = Box::new(|arm, mut m| {
        if arm == Arm::Candidate {
            m.as_object_mut()
                .expect("object")
                .insert("startup_attach_finish_ms".into(), json!(0.2));
        }
        m
    });
    let e = schedule(&mut bench, &plan(2)).expect_err("refused");
    assert!(
        e.starts_with("pair 01 candidate: startup attach sum mismatch"),
        "{e}"
    );
    let mut bench = FakeBench::new();
    bench.edit = Box::new(|arm, mut m| {
        if arm == Arm::Candidate {
            m.as_object_mut()
                .expect("object")
                .insert("backend".into(), json!("cpu"));
        }
        m
    });
    let e = schedule(&mut bench, &plan(2)).expect_err("refused");
    assert!(e.contains("mixed actual backends"), "{e}");
}

#[test]
fn eligibility_is_exact_and_fails_closed() {
    let eligible_plan = |chip: &str, pairs, cooldown_s| Plan {
        pairs,
        laws: LAWS,
        cooldown_s,
        warm: false,
        chip: chip.into(),
        negative_control: false,
    };
    let c = schedule(
        &mut FakeBench::new(),
        &eligible_plan("Apple M4 Max", 20, 5.0),
    )
    .expect("runs");
    assert!(c.eligibility.all(), "{:?}", c.eligibility);
    assert!(report(&c).contains("automated Rule-Zero inputs eligible"));
    let c = schedule(
        &mut FakeBench::new(),
        &eligible_plan("Apple M4 Max", 2, 4.999),
    )
    .expect("runs");
    assert!(!c.eligibility.cooldown && !c.eligibility.sample_count);
    let c = schedule(
        &mut FakeBench::new(),
        &eligible_plan("Virtual Apple M4 Max", 2, 5.0),
    )
    .expect("runs");
    assert!(!c.eligibility.hardware);
    let mut p = eligible_plan("Apple M4 Max", 20, 5.0);
    p.negative_control = true;
    let c = schedule(&mut FakeBench::new(), &p).expect("runs");
    assert!(!c.eligibility.all());
    assert!(
        c.eligibility
            .reasons()
            .contains(&"identical-artifact negative control")
    );
}

#[test]
fn thermal_absence_warnings_and_novel_shapes_fail_closed() {
    assert_eq!(classify_thermal(CLEAN), Thermal::Clean);
    assert_eq!(classify_thermal("unavailable"), Thermal::Unavailable);
    assert_eq!(classify_thermal(""), Thermal::Unavailable);
    assert_eq!(
        classify_thermal("CPU_Speed_Limit = 50"),
        Thermal::WarningOrUnrecognized
    );
    assert_eq!(
        classify_thermal(&format!("{CLEAN} | CPU_Speed_Limit = 50")),
        Thermal::WarningOrUnrecognized
    );
    for (thermal, available) in [("unavailable", false), ("CPU_Speed_Limit = 50", true)] {
        let mut bench = FakeBench::new();
        bench.thermal = thermal.into();
        let mut p = plan(1);
        p.chip = "Apple M4 Max".into();
        let c = schedule(&mut bench, &p).expect("runs");
        assert!(!c.eligibility.thermal(), "{thermal}");
        assert_eq!(c.eligibility.thermal_available, available, "{thermal}");
    }
}

#[test]
fn the_result_records_the_round_unit_and_every_raw_trial() {
    let mut bench = FakeBench::new();
    bench.slot_bias = 2.0;
    let c = schedule(&mut bench, &plan(4)).expect("runs");
    let j = result_json(&c, json!({"harness": "x"}));
    assert_eq!(
        j.get("reported_unit").and_then(Value::as_str),
        Some("round")
    );
    assert_eq!(
        j.get("records").and_then(Value::as_array).map(Vec::len),
        Some(8)
    );
    let rounds = j
        .pointer("/summary/internal_first_present_ms/rounds_candidate_faster_ms/values")
        .and_then(Value::as_array)
        .expect("rounds recorded");
    assert_eq!(rounds.len(), 2);
    assert_eq!(
        j.pointer(
            "/summary/internal_first_present_ms/paired_candidate_faster_ms/order_contaminated"
        ),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        j.pointer("/automated_rule_zero_inputs/negative_control/eligible"),
        Some(&Value::Bool(false))
    );
}
