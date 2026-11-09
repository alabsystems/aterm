// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Offline replay with an independent local solo baseline and one fitted exposure coefficient.
//! Historical slice unit timelines are absent. This is an empirical calibration, not a
//! causal reconstruction of missing memory waits, FIFO order or per-thread scheduling.

use std::path::Path;

const TRIPLES: [&str; 2] = ["x86_64-apple-darwin", "aarch64-apple-darwin"];

#[derive(Clone, Debug)]
struct Cut {
    exposure: f64,
    compile_cpu: f64,
    test_compile_cpu: f64,
    test_cpu: f64,
    test_tail: f64,
    tree_end: f64,
    export_wall: f64,
    slice_wall: [f64; 2],
    slice_cpu: f64,
    max_compilers: usize,
    first_600s_max_compilers: usize,
}

fn table(path: &Path) -> Result<Vec<Vec<String>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines().skip(1).map(csv_row).collect()
}

fn csv_row(line: &str) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("unterminated CSV quote".into());
    }
    fields.push(field);
    Ok(fields)
}

fn valid(n: f64) -> Result<f64, String> {
    if n.is_finite() && n >= 0.0 {
        Ok(n)
    } else {
        Err("time must be finite and nonnegative".into())
    }
}

fn number(row: &[String], index: usize) -> Result<f64, String> {
    valid(
        row.get(index)
            .ok_or("short CSV row")?
            .parse()
            .map_err(|e| format!("CSV number: {e}"))?,
    )
}

fn cut(root: &Path, version: &str) -> Result<Cut, String> {
    let mut exposure = 0.0;
    let mut compile_cpu = 0.0;
    let mut test_compile_cpu = 0.0;
    let mut events = Vec::new();
    for row in table(&root.join(format!("{version}-tree-units.csv")))? {
        let start = number(&row, 4)?;
        let end = number(&row, 5)?;
        let cpu = number(&row, 6)?;
        if end <= start {
            return Err("compile window has no duration".into());
        }
        compile_cpu += cpu;
        if row[0].starts_with("targo test ") {
            test_compile_cpu += cpu;
        }
        // CPU phase boundaries were not retained. Integrate uniform density within a unit.
        exposure += cpu * (end.min(600.0) - start.min(600.0)).max(0.0) / (end - start);
        events.push((start, 1_i32));
        events.push((end, -1_i32));
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut live = 0;
    let mut max_compilers = 0;
    let mut first_600s_max_compilers = 0;
    for (at, change) in events {
        live += change;
        max_compilers = max_compilers.max(live as usize);
        if at < 600.0 {
            first_600s_max_compilers = first_600s_max_compilers.max(live as usize);
        }
    }
    let mut test_cpu = 0.0;
    let mut test_tail: f64 = 0.0;
    for row in table(&root.join(format!("{version}-tree-binaries.csv")))? {
        test_cpu += number(&row, 5)?;
        test_tail = test_tail.max(number(&row, 4)?);
    }
    let mut tree_end: f64 = 0.0;
    for row in table(&root.join(format!("{version}-tree-stages.csv")))? {
        tree_end = tree_end.max(number(&row, 2)?);
    }
    let mut export_wall = 0.0;
    for row in table(&root.join(format!("{version}-branches.csv")))? {
        if row[0].contains("export") {
            export_wall = number(&row, 1)?;
        }
    }
    let mut slice_wall = [0.0; 2];
    let mut slice_cpu = 0.0;
    let slices = table(&root.join(format!("{version}-slices.csv")))?;
    if slices.len() != 2 {
        return Err("expected two release slices".into());
    }
    for row in slices {
        let index = if row[0].contains("x86_64") {
            0
        } else if row[0].contains("native") {
            1
        } else {
            return Err("unknown release slice".into());
        };
        slice_wall[index] = number(&row, 1)?;
        slice_cpu += number(&row, 2)?;
    }
    Ok(Cut {
        exposure,
        compile_cpu,
        test_compile_cpu,
        test_cpu,
        test_tail,
        tree_end,
        export_wall,
        slice_wall,
        slice_cpu,
        max_compilers,
        first_600s_max_compilers,
    })
}

fn value_number(value: &aterm_json::Value, field: &str) -> Result<f64, String> {
    valid(
        value[field]
            .as_f64()
            .ok_or_else(|| format!("missing {field}"))?,
    )
}

fn slice_wall(value: &aterm_json::Value, index: usize) -> Result<f64, String> {
    let row = value["slices"]
        .as_array()
        .ok_or("missing slices")?
        .iter()
        .find(|r| r["triple"].as_str() == Some(TRIPLES[index]))
        .ok_or("missing triple")?;
    value_number(row, "wall_s")
}

fn median(mut values: Vec<f64>) -> Result<f64, String> {
    if values.is_empty() {
        return Err("no comparable pilot takes".into());
    }
    for n in &values {
        valid(*n)?;
    }
    values.sort_by(f64::total_cmp);
    Ok((values[(values.len() - 1) / 2] + values[values.len() / 2]) / 2.0)
}

struct Pilot {
    solo: [f64; 2],
    residual: [f64; 2],
    before_wall: f64,
    after_wall: f64,
    priority_controls: Vec<f64>,
}

fn pilot(path: &Path) -> Result<Pilot, String> {
    let data: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    pilot_rows(data.as_array().ok_or("pilot data must be an array")?)
}

fn pilot_rows(rows: &[aterm_json::Value]) -> Result<Pilot, String> {
    let solos: Vec<_> = rows
        .iter()
        .filter(|r| r["mode"].as_str() == Some("alone"))
        .collect();
    if solos.len() != 1 {
        return Err("pilot needs exactly one independent solo baseline".into());
    }
    let solo_row = solos[0];
    let source = solo_row["source"]
        .as_str()
        .ok_or("pilot lacks source identity")?;
    if rows
        .iter()
        .any(|r| r["source"].as_str() != Some(source) || r["slots"].as_u64() != Some(18))
    {
        return Err("pilot takes must share a source and S=18".into());
    }
    let before: Vec<_> = rows
        .iter()
        .filter(|r| r["mode"].as_str() == Some("shared"))
        .collect();
    let after: Vec<_> = rows
        .iter()
        .filter(|r| r["mode"].as_str() == Some("chain"))
        .collect();
    if before.len() < 2 || after.len() < 2 {
        return Err("pilot needs repeated before/after takes".into());
    }
    let mut solo = [0.0; 2];
    let mut residual = [0.0; 2];
    for index in 0..2 {
        solo[index] = slice_wall(solo_row, index)?;
        let a = median(
            before
                .iter()
                .map(|r| slice_wall(r, index))
                .collect::<Result<_, _>>()?,
        )?;
        let b = median(
            after
                .iter()
                .map(|r| slice_wall(r, index))
                .collect::<Result<_, _>>()?,
        )?;
        if a <= solo[index] {
            return Err("pilot has no positive interference to calibrate".into());
        }
        // Do not force a gain: values above one explicitly predict a regression.
        residual[index] = (b - solo[index]) / (a - solo[index]);
        valid(residual[index])?;
    }
    let before_wall = median(
        before
            .iter()
            .map(|r| value_number(r, "wall_s"))
            .collect::<Result<_, _>>()?,
    )?;
    let after_wall = median(
        after
            .iter()
            .map(|r| value_number(r, "wall_s"))
            .collect::<Result<_, _>>()?,
    )?;
    let after_image = after[0]["verify_sha256"]
        .as_str()
        .ok_or("missing verifier image")?;
    if after
        .iter()
        .any(|r| r["verify_sha256"].as_str() != Some(after_image))
    {
        return Err("after takes used different verifier images".into());
    }
    let priority_controls = rows
        .iter()
        .filter(|r| {
            r["mode"].as_str() == Some("tree") && r["verify_sha256"].as_str() == Some(after_image)
        })
        .map(|r| value_number(r, "wall_s"))
        .collect::<Result<_, _>>()?;
    Ok(Pilot {
        solo,
        residual,
        before_wall,
        after_wall,
        priority_controls,
    })
}

#[derive(Clone, Copy)]
struct Response {
    alone: f64,
    per_cpu: f64,
}
impl Response {
    fn wall(self, exposure: f64) -> f64 {
        self.alone + self.per_cpu * exposure
    }
}

/// One common response coefficient is calibrated on 0.181.0; the local solo baselines
/// are measured independently. 0.180.0 is not used to fit the coefficient.
fn calibrate(cut: &Cut, solo: [f64; 2]) -> Result<[Response; 2], String> {
    let per_cpu =
        ((cut.slice_wall[0] - solo[0]) + (cut.slice_wall[1] - solo[1])) / (2.0 * cut.exposure);
    if !per_cpu.is_finite() || per_cpu <= 0.0 {
        return Err("nonphysical exposure calibration".into());
    }
    Ok(solo.map(|alone| Response { alone, per_cpu }))
}

/// Fluid service left after each slice's mean CPU demand and the export's token reservation.
/// This is a budget model, not a reconstruction of absent Cargo dependency edges.
fn forecast(
    cut: &Cut,
    responses: &[Response; 2],
    residual: [f64; 2],
    cores: u32,
) -> Result<aterm_json::Value, String> {
    let chain = responses
        .iter()
        .zip(residual)
        .map(|(r, share)| r.wall(cut.exposure * share))
        .fold(0.0, f64::max);
    let rate = cut.slice_cpu / chain;
    let export_cores = crate::release_steps::export_share(cores, true) as f64;
    let left = cores as f64 - rate;
    if left <= export_cores {
        return Err("slice service is infeasible beside the export reservation".into());
    }
    let free = |time: f64| {
        cores as f64 * time
            - cut.slice_cpu * (time / chain).min(1.0)
            - export_cores * time.min(cut.export_wall)
    };
    let finish = |work: f64| {
        let mut lo = 0.0;
        let mut hi = (work + cut.slice_cpu + export_cores * cut.export_wall) / (cores as f64)
            + chain
            + cut.export_wall
            + 1.0;
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            if free(mid) >= work {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    };
    let fluid = finish(cut.compile_cpu + cut.test_cpu);
    // Lint can overlap tests. Only test compilation precedes the longest binary.
    let serial = finish(cut.test_compile_cpu) + cut.test_tail;
    let tree = fluid.max(serial);
    let qualify = (chain + 146.0).max(tree + 50.0).max(cut.export_wall + 50.0);
    Ok(
        aterm_json::json!({"chain_s":chain,"chain_mean_cores":rate,"tree_cores_left_after_export":left,"export_reserved_core_s":export_cores*cut.export_wall,"tree_fluid_s":fluid,"tree_serial_floor_s":serial,"tree_service_s":tree,"ideal_max_s":chain.max(fluid),"forecast_qualify_s":qualify,"tree_before_chain":tree<=chain}),
    )
}

pub(crate) fn run(root: &Path, takes: &Path) -> Result<(), String> {
    let mut report = replay(root, &pilot(takes)?)?;
    let fits = report_fits(&report);
    report
        .as_object_mut()
        .ok_or("replay is not an object")?
        .insert("fit_passed".into(), aterm_json::Value::Bool(fits));
    println!("{report}");
    if fits {
        Ok(())
    } else {
        Err("replay exceeds its declared fit bounds".into())
    }
}

fn replay(root: &Path, pilot: &Pilot) -> Result<aterm_json::Value, String> {
    let old = cut(root, "v0.180.0")?;
    let new = cut(root, "v0.181.0")?;
    replay_cuts(&old, &new, pilot)
}

fn report_fits(report: &aterm_json::Value) -> bool {
    report["runs"].as_array().is_some_and(|rows| {
        !rows.is_empty()
            && rows.iter().all(|r| {
                r["error_s"]
                    .as_f64()
                    .is_some_and(|error| error.abs() < 75.0)
                    && r["fitted_slice_wall_s"]
                        .as_array()
                        .zip(r["measured_slice_wall_s"].as_array())
                        .is_some_and(|(fit, real)| {
                            fit.len() == 2
                                && real.len() == 2
                                && fit.iter().zip(real).all(|(a, b)| {
                                    a.as_f64()
                                        .zip(b.as_f64())
                                        .is_some_and(|(a, b)| (a - b).abs() < 220.0)
                                })
                        })
            })
    })
}

fn replay_cuts(old: &Cut, new: &Cut, pilot: &Pilot) -> Result<aterm_json::Value, String> {
    let responses = calibrate(new, pilot.solo)?;
    let mut rows = Vec::new();
    for (version, c, measured) in [("0.180.0", old, 2773.0), ("0.181.0", new, 2711.0)] {
        let fitted: Vec<_> = responses.iter().map(|r| r.wall(c.exposure)).collect();
        // The 146 s slice startup/assembly allowance is calibrated from the newer cut's
        // aggregates. It is not an observed per-unit start offset.
        let predicted = c
            .tree_end
            .max(fitted.iter().copied().fold(0.0, f64::max) + 146.0);
        rows.push(aterm_json::json!({"version":version,"first_600s_compile_cpu_s":c.exposure,"first_600s_mean_compile_cores":c.exposure/600.0,"first_600s_max_compiler_windows":c.first_600s_max_compilers,"max_compiler_windows":c.max_compilers,"tree_compile_cpu_s":c.compile_cpu,"tree_test_compile_cpu_s":c.test_compile_cpu,"tree_binary_cpu_s":c.test_cpu,"fitted_slice_wall_s":fitted,"measured_slice_wall_s":c.slice_wall,"qualify_replay_s":predicted,"measured_qualify_s":measured,"error_s":predicted-measured,"ideal_chain_first":forecast(c,&responses,[0.0;2],18)?,"measured_rule_projection":forecast(c,&responses,pilot.residual,18)?,"tree_first_inversion":forecast(c,&responses,[1.0;2],18)?}));
    }
    Ok(aterm_json::json!({
        "provenance":"Independent m112 paired-slice solo baseline; one common exposure coefficient trained on 0.181.0. Uniform CPU density within retained tree unit windows. Startup/assembly allowance calibrated from aggregates. Historical slice unit timelines, FIFO order and per-thread counters are absent, so this fit is not independent causal proof.",
        "fit_bounds_s":{"qualify":75,"slice":220},
        "pilot":{"before_median_s":pilot.before_wall,"after_median_s":pilot.after_wall,"wall_ratio":pilot.after_wall/pilot.before_wall,"residual_interference_by_slice":pilot.residual,"priority_only_control_s":pilot.priority_controls},
        "solo_wall_s":pilot.solo,
        "wall_per_exposure_cpu_s":responses[0].per_cpu,
        "direct_wall_ratio_projection_0181_s":2711.0*pilot.after_wall/pilot.before_wall,
        "runs":rows
    }))
}

#[cfg(test)]
mod tests {
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
}
