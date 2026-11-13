// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-process ABBA comparison of rejected dispatch, never a workload admission.

use std::path::Path;

use aterm_json::{Value, json};

use crate::testrun::admission_bench::{self, Arm, CANDIDATES, PASSES, WORKERS};

use super::{LaneRun, workspace_root};

const PREFIX: &str = "ADMISSION-WAIT ";

pub(super) fn child(args: &[String]) -> i32 {
    let result = (|| {
        let arm = match args {
            [name] if name == "old" => Arm::Old,
            [name] if name == "probe" => Arm::Probe,
            _ => return Err("admission-wait child takes old|probe".to_string()),
        };
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if aterm_uds::test_binary::is_test_binary(&exe) {
            return Err("admission timing requires the dedicated verify CLI".into());
        }
        let report = admission_bench::measure(arm)?;
        validate(&report, arm)?;
        println!("{PREFIX}{report}");
        Ok(())
    })();
    match result {
        Ok(()) => crate::exit::PASS,
        Err(why) => {
            eprintln!("admission-wait: {why}");
            crate::exit::COULD_NOT_RUN
        }
    }
}

fn git(args: &[&str]) -> Result<String, String> {
    let output = crate::exec::check_command(aterm_uds::os_path!("git"))
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("cannot read benchmark source identity".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn validate(report: &Value, arm: Arm) -> Result<(), String> {
    let number = |name| report.get(name).and_then(Value::as_u64);
    if report.get("arm").and_then(Value::as_str) != Some(arm.word())
        || number("candidates") != Some(CANDIDATES as u64)
        || number("workers") != Some(WORKERS as u64)
        || number("passes") != Some(PASSES as u64)
        || number("attempts") != Some((CANDIDATES * WORKERS * PASSES) as u64)
        || number("admitted") != Some(0)
        || report.get("all_queued").and_then(Value::as_bool) != Some(true)
        || report.get("holder_unchanged").and_then(Value::as_bool) != Some(true)
        || number("read_calls").is_none_or(|n| n == 0)
        || number("process_sweeps").is_none_or(|n| n == 0)
        || number("lineage_reads").is_none()
        || number("wait_mark_reads").is_none()
        || number("unavailable_observations") != Some(0)
        || number("process_cpu_ns").is_none_or(|n| n == 0)
        || [
            "logical_write_bytes",
            "storage_read_bytes",
            "storage_write_bytes",
        ]
        .iter()
        .any(|name| number(name).is_none())
        || report
            .get("worker_cpu_ns")
            .and_then(Value::as_array)
            .is_none_or(|values| {
                values.len() != WORKERS || values.iter().any(|v| v.as_u64().is_none_or(|n| n == 0))
            })
    {
        return Err("incomplete or non-rejecting admission measurement".into());
    }
    Ok(())
}

fn measure(json_out: Option<&Path>) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if aterm_uds::test_binary::is_test_binary(&exe) {
        return Err(
            "run admission-wait with the dedicated verify CLI, not a test executable".into(),
        );
    }
    let source = git(&["rev-parse", "HEAD"])?;
    if !git(&["status", "--porcelain"])?.is_empty() {
        return Err("commit benchmark sources before timing".into());
    }
    let hash = super::arena::sha256_file(&exe)?;
    let mut rows = Vec::new();
    for quad in 0..3 {
        for arm in [Arm::Old, Arm::Probe, Arm::Probe, Arm::Old] {
            // Each arm initializes its own ledger and threads, then obtains fresh timing
            // entry inside the child. The parent holds no outer timing reservation.
            let scratch = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut command = crate::exec::Cmd::new(exe.as_os_str()).args([
                "perf",
                "--admission-wait-child",
                arm.word(),
            ]);
            command.hang_after = Some(std::time::Duration::from_secs(60));
            let captured = crate::exec::run_machine(
                &command,
                crate::exec::ExecEnv {
                    events: None,
                    cwd: &workspace_root(),
                    path: &path,
                    scratch: scratch.path(),
                    child_ceiling: None,
                    remove_env: &[],
                    add_env: &[],
                    stage_groups: None,
                    host: &crate::exec::LIVE_HOST,
                    hang_samples: None,
                },
                None,
            );
            let stderr = captured.run.output;
            if !captured.run.ok {
                return Err(format!(
                    "{} child refused {:?}: {stderr}",
                    arm.word(),
                    captured.run.code
                ));
            }
            let stdout = captured.stdout?;
            let mut records = stdout.lines().filter_map(|line| line.strip_prefix(PREFIX));
            let text = records.next().ok_or("child returned no measurement")?;
            if records.next().is_some() {
                return Err("child returned duplicate measurements".into());
            }
            let report: Value = aterm_json::from_str(text).map_err(|e| e.to_string())?;
            validate(&report, arm)?;
            rows.push(
                json!({ "quad":quad, "sample":report, "timing_and_diagnostics":stderr.as_str() }),
            );
        }
    }
    if git(&["rev-parse", "HEAD"])? != source
        || !git(&["status", "--porcelain"])?.is_empty()
        || super::arena::sha256_file(&exe)? != hash
    {
        return Err("benchmark source or executable changed during measurement".into());
    }
    let report = json!({
        "schema":1, "lane":"admission-wait", "source":source, "executable":exe,
        "sha256":hash, "debug_assertions":cfg!(debug_assertions), "quads":3, "order":"ABBA",
        "provenance_scope":"runtime source label and executable digest; acceptance also requires an externally retained matching compiler artifact record",
        "workload":"32 blocked candidates, two workers, four fixed scans each, real 500ms interval",
        "scope":"Live observations and production admission reason construction; only available memory overridden to zero; private ledger; global Timing admission only; excludes Trace transitions and event persistence",
        "samples":rows
    });
    if let Some(path) = json_out {
        std::fs::write(path, format!("{report}\n")).map_err(|e| e.to_string())?;
    }
    println!("{report}");
    Ok("12 fresh-process samples, old admission vs shared rejection probe; CPU and logical/storage writes (no committed floor)".into())
}

pub(super) fn run(json_out: Option<&Path>) -> LaneRun {
    match measure(json_out) {
        Ok(summary) => LaneRun::measured(summary),
        Err(why) => LaneRun::could_not_run(
            why,
            "use the committed dedicated macOS verify CLI with readable process counters and normal Timing admission",
        ),
    }
}

#[cfg(test)]
#[path = "admission_wait_tests.rs"]
mod tests;
