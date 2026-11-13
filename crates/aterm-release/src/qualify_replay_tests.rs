// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn pilot_fixture() -> Vec<aterm_json::Value> {
    let row = |mode: &str, image: &str, compat: f64, native: f64| {
        aterm_json::json!({
            "mode":mode,"verify_sha256":image,"source":"fixture","slots":18,
            "wall_s":compat.max(native),
            "slices":[{"triple":TRIPLES[0],"wall_s":compat},{"triple":TRIPLES[1],"wall_s":native}]
        })
    };
    vec![
        row("alone", "after", 1100.0, 1150.0),
        row("shared", "before", 2200.0, 2400.0),
        row("chain", "after", 1800.0, 2000.0),
        row("chain", "after", 1800.0, 2000.0),
        row("shared", "before", 2200.0, 2400.0),
    ]
}

fn synthetic_cut(exposure: f64, slice_wall: [f64; 2], tree_end: f64) -> Cut {
    Cut {
        exposure,
        compile_cpu: 7800.0,
        test_compile_cpu: 4000.0,
        test_cpu: 7200.0,
        test_tail: 825.0,
        tree_end,
        export_wall: 800.0,
        slice_wall,
        slice_cpu: 4800.0,
        max_compilers: 20,
        first_600s_max_compilers: 20,
    }
}

#[test]
fn independent_solo_and_one_coefficient_fit_both_joins_with_a_non_vacuous_bound() {
    let pilot = pilot_rows(&pilot_fixture()).unwrap();
    let old = synthetic_cut(2000.0, [1600.0, 1650.0], 2760.0);
    // Architecture residuals differ; fitting a separate coefficient to each would
    // conceal them. The common coefficient must preserve both measured solo values.
    let new = synthetic_cut(5600.0, [2400.0, 2650.0], 2000.0);
    let responses = calibrate(&new, pilot.solo).unwrap();
    assert_eq!(responses[0].per_cpu, responses[1].per_cpu);
    assert_eq!(responses[0].alone, 1100.0);
    assert_eq!(responses[1].alone, 1150.0);
    let result = replay_cuts(&old, &new, &pilot).unwrap();
    assert!(report_fits(&result));
    assert!(
        result["runs"][1]["tree_first_inversion"]["chain_s"]
            .as_f64()
            .unwrap()
            - result["runs"][1]["ideal_chain_first"]["chain_s"]
                .as_f64()
                .unwrap()
            > 1000.0
    );
    // Negative control: removing the response cannot fit the stretched slice.
    let mutant = [
        Response {
            per_cpu: 0.0,
            ..responses[0]
        },
        Response {
            per_cpu: 0.0,
            ..responses[1]
        },
    ];
    assert!((mutant[1].wall(new.exposure) - new.slice_wall[1]).abs() > 1000.0);
    let mut bad = result;
    bad.as_object_mut()
        .unwrap()
        .get_mut("runs")
        .unwrap()
        .as_array_mut()
        .unwrap()[1]
        .as_object_mut()
        .unwrap()
        .insert("error_s".into(), aterm_json::json!(1000.0));
    assert!(!report_fits(&bad));
}

#[test]
fn csv_windows_separate_test_compilation_from_overlapping_lint() {
    let dir = aterm_tempfile::tempdir().unwrap();
    for (suffix, text) in [
        (
            "tree-units.csv",
            "call,package,unit,queued_s,started_s,ended_s,cpu_s,peak_bytes\n\
            targo test fixture,p,p lib,,100,300,200,0\n\
            tippy fixture,q,q lib,,500,700,200,0\n",
        ),
        (
            "tree-binaries.csv",
            "test,outcome,tests,width,execution_s,cpu_s,starved_s,waiting_s,peak_bytes\n\
            test,ok,1,1,10,30,0,0,0\n",
        ),
        (
            "tree-stages.csv",
            "stage,started_s,ended_s\ncompile,0,1000\n",
        ),
        (
            "branches.csv",
            "branch_log,seconds\n04-qualify-export.log,80\n",
        ),
        (
            "slices.csv",
            "call,execution_s,cpu_s,peak_bytes,compiled,fresh\n\
            build aterm x86_64-apple-darwin,400,300,0,1,0\n\
            build aterm native,500,400,0,1,0\n",
        ),
    ] {
        std::fs::write(dir.path().join(format!("trace-{suffix}")), text).unwrap();
    }
    let c = cut(dir.path(), "trace").unwrap();
    assert_eq!(c.exposure, 300.0);
    assert_eq!(c.compile_cpu, 400.0);
    assert_eq!(c.test_compile_cpu, 200.0);
    assert_eq!(c.export_wall, 80.0);
    assert_eq!(c.test_tail, 10.0);
    std::fs::write(
        dir.path().join("trace-tree-units.csv"),
        "header\nfixture,p,p lib,,10,10,200,0\n",
    )
    .unwrap();
    assert!(cut(dir.path(), "trace").is_err());
}

#[test]
fn lint_is_not_put_before_the_test_tail_and_reservations_are_charged() {
    let mut c = synthetic_cut(1.0, [100.0; 2], 0.0);
    c.compile_cpu = 1000.0;
    c.test_compile_cpu = 10.0;
    c.test_cpu = 100.0;
    c.test_tail = 200.0;
    c.export_wall = 100.0;
    c.slice_cpu = 200.0;
    let r = [Response {
        alone: 100.0,
        per_cpu: 1.0,
    }; 2];
    let v = forecast(&c, &r, [0.0; 2], 18).unwrap();
    assert!(v["tree_serial_floor_s"].as_f64().unwrap() < 210.0);
    assert!(v["tree_serial_floor_s"].as_f64().unwrap() > 200.0);
    assert_eq!(v["export_reserved_core_s"].as_f64(), Some(400.0));
    assert!(forecast(&c, &r, [0.0; 2], 2).is_err());
}

#[test]
fn incompatible_pilot_sources_are_refused_instead_of_becoming_a_speedup() {
    let mut rows = pilot_fixture();
    assert!(pilot_rows(&rows).is_ok());
    rows[1]
        .as_object_mut()
        .unwrap()
        .insert("source".into(), aterm_json::json!("another source"));
    assert!(pilot_rows(&rows).is_err());
}

#[test]
fn csv_quotes_and_invalid_numbers_are_not_silently_reinterpreted() {
    assert_eq!(
        csv_row("\"a,b\",\"a\"\"b\",2").unwrap(),
        ["a,b", "a\"b", "2"]
    );
    assert!(csv_row("\"unterminated").is_err());
    assert!(number(&["NaN".into()], 0).is_err());
    assert!(median(Vec::new()).is_err());
}
