// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One journal note's latency: the spool's single append, as p50, p95 and p99 microseconds over
//! 20,000 notes in an optimized build (MEASURED; docs/DESIGN-alab-lab-model-2026-11-01.md §8,
//! §10 row 19: the test holds the hook structurally, this lane reports its p99). It runs the
//! ignored measurement in `crates/aterm-db/tests/journal_note.rs`.

use std::path::Path;

use aterm_json::{Value, json};

use super::arena::{Halt, chip, outcome};
use super::bench_build::{self, Targo};
use super::driver::cargo_driver;
use super::{LaneRun, workspace_root};

const TEST: &str = "note_latency_reports_its_percentiles";
const PREFIX: &str = "JOURNAL-NOTE ";

pub(crate) fn run(json_out: Option<&Path>) -> LaneRun {
    outcome(measure(json_out))
}

fn measure(json_out: Option<&Path>) -> Result<String, Halt> {
    let driver = cargo_driver()
        .map_err(|why| Halt::skip(why, "run it with the pinned toolchain's targo"))?;
    let targo = Targo::new(driver);
    let flags = bench_build::config_rustflags(&targo)?;
    if let Some(why) = bench_build::flags::refusal(&flags) {
        return Err(Halt::Fail(why));
    }
    let plan = bench_build::audit(
        &targo,
        "test",
        &[
            "--profile",
            "opt-test",
            "-p",
            "aterm-db",
            "--test",
            "journal_note",
        ],
    )?;
    if let Some(why) = plan.refusal(bench_build::units::Promise::of(
        bench_build::bench_profile::Profile::Opt,
    )) {
        return Err(Halt::Fail(why));
    }
    let args = [
        "--profile",
        "opt-test",
        "-p",
        "aterm-db",
        "--test",
        "journal_note",
        TEST,
        "--",
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ];
    let build = targo.display("test", &args);
    eprintln!("  {}", plan.summary());
    eprintln!("  $ {build}");
    let output = targo
        .command("test")
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|error| format!("could not run the journal measurement: {error}"))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        let lines: Vec<_> = text.lines().rev().take(30).collect();
        return Err(Halt::Fail(format!(
            "journal measurement exited {:?}\n{}",
            output.status.code(),
            lines.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    let measurement = parse_report(&text)?;
    let summary = format!(
        "journal note p50 {} us, p95 {} us, p99 {} us over {} notes (opt-test)",
        measurement["p50_us"], measurement["p95_us"], measurement["p99_us"], measurement["n"]
    );
    if let Some(path) = json_out {
        let report = json!({
            "schema": 1,
            "lane": "journal-note",
            "profile": "opt-test",
            "build": build,
            "build_plan": plan.summary(),
            "machine": chip(),
            "measurement": measurement,
        });
        super::publication::publish_named(path, format!("{report}\n"))
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok(format!("{summary}; report in {}", path.display()))
    } else {
        Ok(format!("{summary}; --json-out PATH keeps provenance"))
    }
}

/// The one `JOURNAL-NOTE n= p50_us= p95_us= p99_us=` line, its percentiles in order.
fn parse_report(text: &str) -> Result<Value, String> {
    // libtest prints `test <name> ... ` before a test's own output on the same line.
    let mut reports = text
        .lines()
        .filter_map(|line| Some(&line[line.find(PREFIX)? + PREFIX.len()..]));
    let raw = reports
        .next()
        .ok_or("the journal test produced no measurement")?;
    if reports.next().is_some() {
        return Err("the journal test produced more than one measurement".into());
    }
    let mut report = aterm_json::Map::new();
    for field in raw.split_whitespace() {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("`{field}` is not key=value"))?;
        let value: u64 = value
            .parse()
            .map_err(|_| format!("`{key}` is not a count"))?;
        report.insert(key.to_owned(), json!(value));
    }
    let number = |key: &str| {
        report
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("the journal measurement has no {key}"))
    };
    let (n, p50, p95, p99) = (
        number("n")?,
        number("p50_us")?,
        number("p95_us")?,
        number("p99_us")?,
    );
    if n == 0 || p50 > p95 || p95 > p99 {
        return Err("the journal measurement's percentiles are out of order".into());
    }
    Ok(Value::Object(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_requires_one_ordered_report() {
        let valid = "JOURNAL-NOTE n=20000 p50_us=3 p95_us=5 p99_us=9";
        assert_eq!(parse_report(valid).unwrap()["p99_us"], json!(9));
        let after_name = format!("test note_latency_reports_its_percentiles ... {valid}");
        assert_eq!(parse_report(&after_name).unwrap()["p50_us"], json!(3));
        assert!(parse_report("running 0 tests").is_err());
        assert!(parse_report(&format!("{valid}\n{valid}")).is_err());
        assert!(parse_report("JOURNAL-NOTE n=20000 p50_us=9 p95_us=5 p99_us=9").is_err());
        assert!(parse_report("JOURNAL-NOTE n=0 p50_us=1 p95_us=1 p99_us=1").is_err());
        assert!(parse_report("JOURNAL-NOTE n=20000 p50_us=1 p95_us=1").is_err());
    }
}
