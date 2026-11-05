// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process CPU for an isolated, converged Commons repair pair (MEASURED).

use std::path::Path;

use aterm_json::{Value, json};
use aterm_uds::spawnfd::command;

use super::arena::{Halt, chip, outcome, stdout_of, thermal};
use super::bench_build::{self, Targo, load_average};
use super::driver::cargo_driver;
use super::{LaneRun, workspace_root};

const TEST: &str = "replica::tests::compatibility::a_round_that_moves_nothing_reports_its_cpu";
const SUSTAINED_TEST: &str =
    "replica::tests::compatibility::a_sustained_quiet_pair_reports_its_cpu";
const CATALOG_TEST: &str = "replica::tests::compatibility::a_quiet_catalog_pair_reports_its_cpu";
const SUSTAINED_CATALOG_TEST: &str =
    "replica::tests::compatibility::a_sustained_quiet_catalog_pair_reports_its_cpu";
const PREFIX: &str = "COMMONS-QUIET ";

pub(crate) fn run(json_out: Option<&Path>, sustained: bool, catalog: bool) -> LaneRun {
    outcome(measure(json_out, sustained, catalog))
}

fn measure(json_out: Option<&Path>, sustained: bool, catalog: bool) -> Result<String, Halt> {
    let driver = cargo_driver()
        .map_err(|why| Halt::skip(why, "run it with the pinned toolchain's targo"))?;
    let root = workspace_root();
    let targo = Targo::new(driver);
    let flags = bench_build::config_rustflags(&targo)?;
    if let Some(why) = bench_build::flags::refusal(&flags) {
        return Err(Halt::Fail(why));
    }
    let plan = bench_build::audit(
        &targo,
        "test",
        &["--profile", "opt-test", "-p", "aterm-db", "--lib"],
    )?;
    if let Some(why) = plan.refusal(bench_build::units::Promise::of(
        bench_build::bench_profile::Profile::Opt,
    )) {
        return Err(Halt::Fail(why));
    }
    let args = vec![
        "--profile",
        "opt-test",
        "-p",
        "aterm-db",
        "--lib",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ];
    let build = targo.display("test", &args);
    eprintln!("  {}", plan.summary());
    eprintln!("  $ {build}");
    let built = targo
        .command("test")
        .args(args)
        .current_dir(&root)
        .output()
        .map_err(|error| format!("could not build Commons measurement: {error}"))?;
    if !built.status.success() {
        return Err(Halt::Fail(format!(
            "Commons measurement build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        )));
    }
    let executable =
        bench_build::built_executable(&String::from_utf8_lossy(&built.stdout), "aterm_db", "lib")?;
    let test = match (catalog, sustained) {
        (false, false) => TEST,
        (false, true) => SUSTAINED_TEST,
        (true, false) => CATALOG_TEST,
        (true, true) => SUSTAINED_CATALOG_TEST,
    };
    let mut run = command(executable);
    targo.environment(&mut run);
    // Every measurement is ignored in a plain run; `--exact` keeps this to the one named.
    run.current_dir(&root).args([
        test,
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    let entry = super::measurement_entry().map_err(Halt::Timing)?;
    let before = load_average();
    let output = run
        .output()
        .map_err(|error| format!("could not run Commons measurement: {error}"))?;
    entry
        .timing_admission()
        .map_err(|why| Halt::Timing(why.to_string()))?;
    let after = load_average();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        let lines: Vec<_> = text.lines().rev().take(30).collect();
        return Err(Halt::Fail(format!(
            "Commons measurement exited {:?}\n{}",
            output.status.code(),
            lines.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    let workload = if catalog { "catalog" } else { "explicit" };
    let measurement = parse_report(&text, workload)?;
    let cpu_ns = measurement["per_round_cpu_ns"]
        .as_u64()
        .expect("validated CPU");
    let summary = format!(
        "{:.3} ms mean process CPU per {workload} quiet round, both peers, over {} rounds (opt-test)",
        cpu_ns as f64 / 1_000_000.0,
        measurement["rounds"].as_u64().expect("validated rounds")
    );
    let head = git(&["rev-parse", "HEAD"])?;
    let dirty = git(&["status", "--porcelain"])?;
    let report = json!({
        "schema": 1,
        "lane": "commons-quiet",
        "commit": head,
        "dirty_paths": dirty.lines().count(),
        "profile": "opt-test",
        "sustained": sustained,
        "workload": workload,
        "build": build,
        "build_plan": plan.summary(),
        "rustflags": bench_build::flags::describe(&flags),
        "toolchain": stdout_of(command(&driver.program).args(["--unverified", "--version"])),
        "machine": chip(),
        "logical_cpus": std::thread::available_parallelism().ok().map(|count| count.get()),
        "load_before_command": before,
        "load_after_command": after,
        "thermal": thermal(),
        "measurement": measurement,
    });
    if let Some(path) = json_out {
        crate::write_fresh(path, format!("{report}\n"))
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok(format!("{summary}; report in {}", path.display()))
    } else {
        Ok(format!("{summary}; --json-out PATH keeps provenance"))
    }
}

fn git(args: &[&str]) -> Result<String, Halt> {
    stdout_of(
        command(aterm_uds::os_path!("git"))
            .args(args)
            .current_dir(workspace_root()),
    )
    .ok_or_else(|| {
        Halt::Fail(format!(
            "git {} did not report build provenance",
            args.join(" ")
        ))
    })
}

fn parse_report(text: &str, workload: &str) -> Result<Value, String> {
    let mut reports = text.lines().filter_map(|line| line.strip_prefix(PREFIX));
    let raw = reports
        .next()
        .ok_or("Commons test produced no measurement")?;
    if reports.next().is_some() {
        return Err("Commons test produced more than one measurement".into());
    }
    let report: Value = aterm_json::from_str(raw).map_err(|error| error.to_string())?;
    let number = |key: &str| {
        report
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("Commons measurement has no unsigned {key}"))
    };
    if number("schema")? != 1 || number("moved_records")? != 0 {
        return Err("Commons measurement is not a quiet-round schema 1 report".into());
    }
    if report.get("workload").and_then(Value::as_str) != Some(workload)
        || number("tables")? != if workload == "catalog" { 2 } else { 1 }
    {
        return Err("Commons measurement has another workload or table count".into());
    }
    let rounds = number("rounds")?;
    let total = number("process_cpu_ns")?;
    let per_round = number("per_round_cpu_ns")?;
    number("warmup_rounds")?;
    number("clock_read_cpu_ns")?;
    if rounds == 0 || total / rounds != per_round {
        return Err("Commons measurement has inconsistent round counts or CPU time".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Value {
        json!({"schema":1,"warmup_rounds":2,"rounds":8,"process_cpu_ns":800,
            "per_round_cpu_ns":100,"clock_read_cpu_ns":1,"moved_records":0,
            "workload":"explicit","tables":1})
    }

    #[test]
    fn measurement_requires_one_complete_consistent_quiet_report() {
        let valid = format!("{PREFIX}{}", report());
        assert!(parse_report(&valid, "explicit").is_ok());
        assert!(parse_report(&valid, "catalog").is_err());
        assert!(parse_report("running 0 tests", "explicit").is_err());
        assert!(parse_report(&format!("{valid}\n{valid}"), "explicit").is_err());
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
                parse_report(&format!("{PREFIX}{invalid}"), "explicit").is_err(),
                "{key}"
            );
        }
        let mut missing = report();
        missing.as_object_mut().unwrap().remove("clock_read_cpu_ns");
        assert!(parse_report(&format!("{PREFIX}{missing}"), "explicit").is_err());
        let mut catalog = report();
        let fields = catalog.as_object_mut().unwrap();
        fields.insert("workload".into(), json!("catalog"));
        fields.insert("tables".into(), json!(2));
        assert!(parse_report(&format!("{PREFIX}{catalog}"), "catalog").is_ok());
        assert!(parse_report(&format!("{PREFIX}{catalog}"), "explicit").is_err());
    }
}
