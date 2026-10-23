// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify perf aos` (MEASURED).

use std::path::Path;

use aterm_json::Value;
use aterm_uds::spawnfd::command;

use super::arena::{Halt, outcome, stdout_of, thermal};
use super::driver::{cargo_driver, target_dir};
use super::floors::last_json_line;
use super::{LaneRun, workspace_root};

/// The harness's cold ops, each OP of `aos_harness --cold OP` (the harness refuses any other).
pub(crate) const COLD_OPS: [&str; 7] = [
    "direct",
    "host",
    "attached",
    "checkpoint",
    "log",
    "replay",
    "repair",
];

/// Fresh processes per cold op.
pub(crate) const COLD_RUNS: usize = 5;

const EXAMPLE: &str = "aos_harness";

/// How many lines of a failed build's or run's stderr a failure quotes.
const TAIL: usize = 30;

/// The trace file the export run writes and the import run reads, under the target
/// directory.
const TRACE_FILE: &str = "aos_harness.trace";

/// One build of the harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Build {
    /// `--release`: the shipped profile (fat LTO, one codegen unit).
    Release,
    /// `--profile opt-test`: release optimization, debug assertions on, no LTO.
    OptTest,
}

impl Build {
    /// The `targo build` arguments after the verb.
    pub(crate) fn args(self) -> Vec<&'static str> {
        let mut args = match self {
            Build::Release => vec!["--release"],
            Build::OptTest => vec!["--profile", "opt-test"],
        };
        args.extend(["-q", "-p", "aterm-bench", "--example", EXAMPLE]);
        args
    }

    /// Where under the target directory the profile's binaries land.
    pub(crate) fn dir(self) -> &'static str {
        match self {
            Build::Release => "release",
            Build::OptTest => "opt-test",
        }
    }
}

pub(crate) fn run(json_out: Option<&Path>) -> LaneRun {
    outcome(measure(json_out))
}

fn measure(json_out: Option<&Path>) -> Result<String, Halt> {
    let driver = cargo_driver().map_err(|why| {
        Halt::skip(
            format!("no targo to build the harness with: {why}"),
            "run it with the pinned toolchain's targo",
        )
    })?;
    let load_before = load_average();
    let targo = stdout_of(command(&driver.program).args(["--unverified", "--version"]))
        .unwrap_or_else(|| "unavailable".into());
    let (commit, dirty) = commit();
    let target = target_dir()?;
    let mut bins = Vec::new();
    for build in [Build::Release, Build::OptTest] {
        let args = build.args();
        eprintln!("  $ {}", driver.display_workspace("build", &args));
        let out = driver
            .workspace_command("build")
            .args(&args)
            .current_dir(workspace_root())
            .output()
            .map_err(|e| format!("could not run targo build: {e}"))?;
        if !out.status.success() {
            return Err(Halt::Fail(format!(
                "`{}` exited {:?}\n{}",
                driver.display_workspace("build", &args),
                out.status.code(),
                tail(&String::from_utf8_lossy(&out.stderr))
            )));
        }
        let bin = target.join(build.dir()).join("examples").join(EXAMPLE);
        if !bin.is_file() {
            return Err(Halt::Fail(format!(
                "the {} build left no harness at {}",
                build.dir(),
                bin.display()
            )));
        }
        bins.push(bin);
    }
    let (release, opt_test) = (&bins[0], &bins[1]);
    let load_after_build = load_average();
    let mut cold: Vec<(&str, Value, String)> = Vec::new();
    for run in 0..COLD_RUNS {
        for op in COLD_OPS {
            eprintln!("  cold {op} ({} of {COLD_RUNS})", run + 1);
            let (report, raw) = harness(release, &["--cold", op])?;
            cold.push((op, report, raw));
        }
    }
    eprintln!("  warm");
    let (warm, warm_raw) = harness(release, &[])?;
    eprintln!("  stranded (opt-test)");
    let (stranded, stranded_raw) = harness(opt_test, &["--stranded"])?;
    let trace_path = target.join(TRACE_FILE);
    let trace_arg = trace_path.to_string_lossy().into_owned();
    eprintln!("  export, then import in another process");
    let (export, export_raw) = harness(release, &["--export", &trace_arg])?;
    let (import, import_raw) = harness(release, &["--import", &trace_arg])?;
    let load_after = load_average();
    let mut reports: Vec<&Value> = vec![&warm, &stranded, &export, &import];
    reports.extend(cold.iter().map(|(_, v, _)| v));
    let story = one_story(&reports)?;
    let head = cross_process(&export, &import, &cold).map_err(Halt::Fail)?;
    let rows = cold_rows(&cold);
    for line in summary(&warm, &stranded, &rows) {
        eprintln!("  {line}");
    }
    eprintln!(
        "  cross-process: export, import and {} cold replay/checkpoint runs reached state {} chain {}",
        head.agreeing, head.state, head.chain
    );
    let Some(path) = json_out else {
        return Ok("summary above; --json-out PATH keeps every report".into());
    };
    let mut cold_json = String::new();
    for op in COLD_OPS {
        let runs: Vec<&str> = cold
            .iter()
            .filter(|(o, _, _)| *o == op)
            .map(|(_, _, raw)| raw.as_str())
            .collect();
        if !cold_json.is_empty() {
            cold_json.push(',');
        }
        cold_json.push_str(&format!("{}:[{}]", quote(op), runs.join(",")));
    }
    let summary_json: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{{\"op\":{},\"key\":{},\"median\":{},\"min\":{},\"max\":{},\"runs\":{}}}",
                quote(&r.op),
                quote(&r.key),
                r.median,
                r.min,
                r.max,
                r.runs
            )
        })
        .collect();
    let text = format!(
        "{{\"lane\":\"aos\",\"story\":{},\"targo\":{},\"commit\":{},\"dirty_paths\":{dirty},\
         \"load_before_build\":{},\"load_after_build\":{},\"load_after_run\":{},\"thermal\":{},\
         \"builds\":{{\"release\":{},\"opt_test\":{}}},\"cold_runs\":{COLD_RUNS},\
         \"warm\":{warm_raw},\"stranded\":{stranded_raw},\"cold\":{{{cold_json}}},\
         \"cold_summary\":[{}],\"cross_process\":{{\"export\":{export_raw},\"import\":{import_raw},\
         \"state\":{},\"chain\":{}}}}}\n",
        quote(&story),
        quote(&targo),
        quote(&commit),
        quote(&load_before),
        quote(&load_after_build),
        quote(&load_after),
        quote(&thermal()),
        quote(&Build::Release.args().join(" ")),
        quote(&Build::OptTest.args().join(" ")),
        summary_json.join(","),
        quote(&head.state),
        quote(&head.chain)
    );
    aterm_json::from_str::<Value>(&text)
        .map_err(|e| format!("the lane's own JSON does not parse: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(format!("summary above; every report in {}", path.display()))
}

/// One harness run: its report (the last JSON line on stdout, which must say `ok`) and that
/// line as printed. Its stderr's progress lines are echoed.
fn harness(bin: &Path, args: &[&str]) -> Result<(Value, String), Halt> {
    let out = command(bin)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines().filter(|l| l.starts_with("aos_harness:")) {
        eprintln!("    {line}");
    }
    if !out.status.success() {
        return Err(Halt::Fail(format!(
            "`{} {}` exited {:?}\n{}",
            bin.display(),
            args.join(" "),
            out.status.code(),
            tail(&stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report = last_json_line(&stdout).ok_or_else(|| {
        format!(
            "`{} {}` printed no JSON report",
            bin.display(),
            args.join(" ")
        )
    })?;
    if report.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(Halt::Fail(format!(
            "`{} {}` reported no `ok`",
            bin.display(),
            args.join(" ")
        )));
    }
    let raw = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok((report, raw))
}

/// The one story every report measured (its digest), or which report measured another.
/// Pure (unit-tested).
pub(crate) fn one_story(reports: &[&Value]) -> Result<String, String> {
    let digest = |r: &Value| {
        r.pointer("/story/digest")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let first = reports
        .first()
        .and_then(|r| digest(r))
        .ok_or("a report names no story digest")?;
    for (i, r) in reports.iter().enumerate() {
        match digest(r) {
            Some(d) if d == first => {}
            Some(d) => {
                return Err(format!(
                    "report {i} measured story {d}, the first report story {first}: the runs are not matched"
                ));
            }
            None => return Err(format!("report {i} names no story digest")),
        }
    }
    Ok(first)
}

/// The head every process reached ([`cross_process`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    /// The state digest, full hex.
    pub state: String,
    /// The observation chain, full hex.
    pub chain: String,
    /// How many cold runs agreed with it.
    pub agreeing: usize,
}

/// That `found`, `who`'s `what` digest, is the export's `want`.
fn agree(who: &str, what: &str, found: Option<&str>, want: &str) -> Result<(), String> {
    match found {
        None => Err(format!("{who} reports no {what} digest")),
        Some(f) if f != want => Err(format!(
            "{who} reports {what} {f}, not the exported head's {want}: the processes disagree"
        )),
        Some(_) => Ok(()),
    }
}

/// The head the export run's attached host reached (its state digest and observation chain),
/// checked against every other process that reached it: the head the import read from the
/// file, the import's own replay on its own epoch, each cold `replay` run (state and chain)
/// and each cold `checkpoint` run (state). The first disagreement, or a report that names no
/// digest, is refused; so is a lane with no cold run of either op. Pure (unit-tested).
pub(crate) fn cross_process(
    export: &Value,
    import: &Value,
    cold: &[(&str, Value, String)],
) -> Result<Head, String> {
    let text = |v: &Value, p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
    let state = text(export, "/export/state").ok_or("the export reports no state digest")?;
    let chain = text(export, "/export/chain").ok_or("the export reports no chain digest")?;
    agree(
        "the file the import read",
        "state",
        text(import, "/import/recorded_state").as_deref(),
        &state,
    )?;
    agree(
        "the file the import read",
        "chain",
        text(import, "/import/recorded_chain").as_deref(),
        &chain,
    )?;
    agree(
        "the import's replay",
        "state",
        text(import, "/import/state").as_deref(),
        &state,
    )?;
    agree(
        "the import's replay",
        "chain",
        text(import, "/import/chain").as_deref(),
        &chain,
    )?;
    let mut agreeing = 0;
    for op in ["replay", "checkpoint"] {
        let runs: Vec<&Value> = cold
            .iter()
            .filter(|(o, _, _)| *o == op)
            .map(|(_, v, _)| v)
            .collect();
        if runs.is_empty() {
            return Err(format!("no cold {op} run reports a digest"));
        }
        for (i, run) in runs.iter().enumerate() {
            let who = format!("cold {op} run {}", i + 1);
            agree(
                &who,
                "state",
                text(run, "/cold/digests/state").as_deref(),
                &state,
            )?;
            if op == "replay" {
                agree(
                    &who,
                    "chain",
                    text(run, "/cold/digests/chain").as_deref(),
                    &chain,
                )?;
            }
            agreeing += 1;
        }
    }
    Ok(Head {
        state,
        chain,
        agreeing,
    })
}

/// One cold measurement across its runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColdRow {
    pub op: String,
    pub key: String,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub runs: usize,
}

/// Every number under each cold report's `cold.ns` (and its `ghz`), by op and key in the order
/// first seen: the median (the lower middle for an even count), min and max over the runs
/// that report it. Pure (unit-tested).
pub(crate) fn cold_rows(cold: &[(&str, Value, String)]) -> Vec<ColdRow> {
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut values: Vec<Vec<f64>> = Vec::new();
    for (op, report, _) in cold {
        let mut found: Vec<(String, f64)> = report
            .pointer("/cold/ns")
            .and_then(Value::as_object)
            .map(|ns| {
                ns.iter()
                    .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(ghz) = report.pointer("/cold/ghz").and_then(Value::as_f64) {
            found.push(("ghz".into(), ghz));
        }
        for (key, v) in found {
            let id = (op.to_string(), key);
            match keys.iter().position(|k| *k == id) {
                Some(i) => values[i].push(v),
                None => {
                    keys.push(id);
                    values.push(vec![v]);
                }
            }
        }
    }
    keys.into_iter()
        .zip(values)
        .map(|((op, key), mut v)| {
            v.sort_by(f64::total_cmp);
            ColdRow {
                op,
                key,
                median: v[(v.len() - 1) / 2],
                min: v[0],
                max: v[v.len() - 1],
                runs: v.len(),
            }
        })
        .collect()
}

/// A number from a report by JSON pointer, printed plainly, or `?`.
fn num(v: &Value, pointer: &str) -> String {
    match v.pointer(pointer).and_then(Value::as_f64) {
        Some(n) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{n:.0}"),
        Some(n) => format!("{n:.1}"),
        None => "?".into(),
    }
}

fn text(v: &Value, pointer: &str) -> String {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

/// The summary lines: per-call latencies, throughput, checkpoint, log, replay, repair, cold.
fn summary(warm: &Value, stranded: &Value, rows: &[ColdRow]) -> Vec<String> {
    let mut lines = vec![
        format!(
            "story: {} events ({} keys, {} ticks, {} queries, {} reset), digest {}",
            num(warm, "/story/events"),
            num(warm, "/story/keys"),
            num(warm, "/story/ticks"),
            num(warm, "/story/queries"),
            num(warm, "/story/resets"),
            text(warm, "/story/digest")
        ),
        format!(
            "machine: {} {} macOS {}; load {} at the warm run's start",
            text(warm, "/machine/hw.model"),
            text(warm, "/machine/cpu"),
            text(warm, "/machine/macos"),
            text(warm, "/load_start")
        ),
    ];
    for path in ["direct", "host", "attached"] {
        let p = |rest: &str| num(warm, &format!("/warm/paths/{path}/{rest}"));
        lines.push(format!(
            "{path:>8}: tick p50/p99 {}/{} ns, key p50 {} ns, query p50 {} ns; {} events/s, {} instructions/event, {} allocations/event",
            p("per_call_ns/tick/p50"),
            p("per_call_ns/tick/p99"),
            p("per_call_ns/key/p50"),
            p("per_call_ns/query/p50"),
            p("events_per_s"),
            p("per_event/instructions"),
            p("per_event/allocs"),
        ));
    }
    lines.push(format!(
        "attach p50 {} ns, detach p50 {} ns",
        num(warm, "/warm/attach_ns/p50"),
        num(warm, "/warm/detach_ns/p50")
    ));
    lines.push(format!(
        "checkpoint p50 capture/encode/decode/restore {}/{}/{}/{} ns; state {} B (engine snapshot {} B), checkpoint {} B",
        num(warm, "/checkpoint/capture_ns/p50"),
        num(warm, "/checkpoint/encode_ns/p50"),
        num(warm, "/checkpoint/decode_ns/p50"),
        num(warm, "/checkpoint/restore_ns/p50"),
        num(warm, "/checkpoint/state_bytes_max"),
        num(warm, "/checkpoint/engine_snapshot_bytes"),
        num(warm, "/checkpoint/checkpoint_bytes_max"),
    ));
    lines.push(format!(
        "log: {} B/event; p50 encode/decode/trace digest {}/{}/{} ns",
        num(warm, "/log/bytes_per_event"),
        num(warm, "/log/encode_ns/p50"),
        num(warm, "/log/decode_ns/p50"),
        num(warm, "/log/trace_digest_ns/p50"),
    ));
    lines.push(format!(
        "replay: {} events/s at p50 ({} ns/event; the observation fold alone {} ns/event)",
        num(warm, "/replay/events_per_s_at_p50"),
        num(warm, "/replay/ns_per_event_at_p50"),
        num(warm, "/replay/observation_fold_ns_per_event"),
    ));
    lines.push(format!(
        "repair Shipped->Shipped (release): pause + repair p50 {} ns over {} inputs",
        num(warm, "/repair/pause_plus_repair_ns/p50"),
        num(warm, "/repair/replayed_inputs"),
    ));
    lines.push(format!(
        "repair DemoStrandedSettle->Shipped (opt-test): p50 {} ns, live {}, candidate {}; Shipped->Shipped there p50 {} ns",
        num(stranded, "/stranded_repair/pause_plus_repair_ns/p50"),
        text(stranded, "/stranded_repair/live_verdict"),
        text(stranded, "/stranded_repair/candidate_verdict"),
        num(stranded, "/shipped_repair/pause_plus_repair_ns/p50"),
    ));
    for op in COLD_OPS {
        let cells: Vec<String> = rows
            .iter()
            .filter(|r| r.op == op)
            .map(|r| format!("{}={}", r.key, r.median))
            .collect();
        lines.push(format!(
            "cold {op} (median of {COLD_RUNS}): {}",
            cells.join(" ")
        ));
    }
    lines
}

/// `sysctl -n vm.loadavg`'s three averages, or `unavailable`.
fn load_average() -> String {
    stdout_of(command("sysctl").args(["-n", "vm.loadavg"]))
        .map(|s| {
            s.trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unavailable".into())
}

/// The checkout's HEAD and how many paths `git status` lists.
fn commit() -> (String, usize) {
    let root = workspace_root();
    let head = stdout_of(
        command("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root),
    )
    .unwrap_or_else(|| "unknown".into());
    let dirty = stdout_of(
        command("git")
            .args(["status", "--porcelain"])
            .current_dir(&root),
    )
    .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count());
    (head, dirty)
}

fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().rev().take(TAIL).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

fn quote(s: &str) -> String {
    aterm_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(json: &str) -> Value {
        aterm_json::from_str(json).expect("test JSON parses")
    }

    #[test]
    fn the_release_build_and_the_opt_test_build_name_their_profiles_and_the_harness() {
        assert_eq!(
            Build::Release.args(),
            [
                "--release",
                "-q",
                "-p",
                "aterm-bench",
                "--example",
                "aos_harness"
            ]
        );
        assert_eq!(
            Build::OptTest.args(),
            [
                "--profile",
                "opt-test",
                "-q",
                "-p",
                "aterm-bench",
                "--example",
                "aos_harness"
            ]
        );
        assert_eq!(Build::Release.dir(), "release");
        assert_eq!(Build::OptTest.dir(), "opt-test");
    }

    #[test]
    fn reports_of_one_story_name_its_digest_and_another_story_is_refused() {
        let a = report(r#"{"story":{"digest":"00f0"}}"#);
        let b = report(r#"{"story":{"digest":"00f0"}}"#);
        assert_eq!(one_story(&[&a, &b]), Ok("00f0".to_string()));
        let other = report(r#"{"story":{"digest":"beef"}}"#);
        let why = one_story(&[&a, &other]).unwrap_err();
        assert!(why.contains("beef") && why.contains("not matched"), "{why}");
        let none = report(r#"{"story":{}}"#);
        assert_eq!(
            one_story(&[&a, &none]),
            Err("report 1 names no story digest".to_string())
        );
        assert_eq!(
            one_story(&[]),
            Err("a report names no story digest".to_string())
        );
    }

    #[test]
    fn each_cold_number_is_summarized_by_its_median_min_and_max_over_the_runs() {
        let run = |ns: &str, ghz: f64| {
            (
                "replay",
                report(&format!(r#"{{"cold":{{"ns":{ns},"ghz":{ghz}}}}}"#)),
                String::new(),
            )
        };
        let cold = vec![
            run(r#"{"replay":30,"first":null}"#, 2.0),
            run(r#"{"replay":10}"#, 3.0),
            run(r#"{"replay":20}"#, 4.0),
            run(r#"{"replay":40}"#, 1.0),
        ];
        let rows = cold_rows(&cold);
        assert_eq!(
            rows,
            vec![
                ColdRow {
                    op: "replay".into(),
                    key: "replay".into(),
                    median: 20.0,
                    min: 10.0,
                    max: 40.0,
                    runs: 4,
                },
                ColdRow {
                    op: "replay".into(),
                    key: "ghz".into(),
                    median: 2.0,
                    min: 1.0,
                    max: 4.0,
                    runs: 4,
                },
            ]
        );
    }

    /// Every process that reached the head agrees with the export, and the first one that
    /// does not, or names no digest, is refused by name.
    #[test]
    fn every_process_must_reach_the_exported_head_and_the_first_that_does_not_is_named() {
        let export = report(r#"{"export":{"state":"aa","chain":"cc"}}"#);
        let import = |rs: &str, rc: &str, s: &str, c: &str| {
            report(&format!(
                r#"{{"import":{{"recorded_state":"{rs}","recorded_chain":"{rc}","state":"{s}","chain":"{c}"}}}}"#
            ))
        };
        let replay = |s: &str, c: &str| {
            (
                "replay",
                report(&format!(
                    r#"{{"cold":{{"digests":{{"state":"{s}","chain":"{c}"}}}}}}"#
                )),
                String::new(),
            )
        };
        let checkpoint = |s: &str| {
            (
                "checkpoint",
                report(&format!(
                    r#"{{"cold":{{"digests":{{"state":"{s}","checkpoint":"01"}}}}}}"#
                )),
                String::new(),
            )
        };
        let good = import("aa", "cc", "aa", "cc");
        let cold = vec![
            replay("aa", "cc"),
            checkpoint("aa"),
            replay("aa", "cc"),
            checkpoint("aa"),
        ];
        assert_eq!(
            cross_process(&export, &good, &cold),
            Ok(Head {
                state: "aa".into(),
                chain: "cc".into(),
                agreeing: 4,
            })
        );
        assert_eq!(
            cross_process(&export, &import("aa", "cc", "ab", "cc"), &cold),
            Err(
                "the import's replay reports state ab, not the exported head's aa: the processes disagree"
                    .to_string()
            )
        );
        assert_eq!(
            cross_process(&export, &import("aa", "cd", "aa", "cc"), &cold),
            Err(
                "the file the import read reports chain cd, not the exported head's cc: the processes disagree"
                    .to_string()
            )
        );
        let bent = vec![replay("aa", "cc"), checkpoint("aa"), replay("aa", "c0")];
        assert_eq!(
            cross_process(&export, &good, &bent),
            Err(
                "cold replay run 2 reports chain c0, not the exported head's cc: the processes disagree"
                    .to_string()
            )
        );
        let bent = vec![replay("aa", "cc"), checkpoint("a0")];
        assert_eq!(
            cross_process(&export, &good, &bent),
            Err(
                "cold checkpoint run 1 reports state a0, not the exported head's aa: the processes disagree"
                    .to_string()
            )
        );
        assert_eq!(
            cross_process(&export, &good, &[replay("aa", "cc")]),
            Err("no cold checkpoint run reports a digest".to_string())
        );
        assert_eq!(
            cross_process(&export, &report(r#"{"import":{}}"#), &cold),
            Err("the file the import read reports no state digest".to_string())
        );
        assert_eq!(
            cross_process(&report(r#"{"export":{"state":"aa"}}"#), &good, &cold),
            Err("the export reports no chain digest".to_string())
        );
        let no_digests = vec![(
            "replay",
            report(r#"{"cold":{"ns":{"replay":3}}}"#),
            String::new(),
        )];
        assert_eq!(
            cross_process(&export, &good, &no_digests),
            Err("cold replay run 1 reports no state digest".to_string())
        );
    }

    #[test]
    fn a_missing_number_reads_as_a_question_mark_and_a_whole_one_has_no_decimals() {
        let v = report(r#"{"a":{"b":3,"c":2.375}}"#);
        assert_eq!(num(&v, "/a/b"), "3");
        assert_eq!(num(&v, "/a/c"), "2.4");
        assert_eq!(num(&v, "/a/d"), "?");
        assert_eq!(text(&v, "/a/b"), "?");
    }
}
