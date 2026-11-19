// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `files-idle` floor lane: what an idle hour of the shared files' materializer costs
//! (docs/DESIGN-shared-files-2026-11-11.md §4), as exact counts, at the load the lane finds.
//!
//! It builds aterm-db's library tests in the `test` profile and runs one ignored test alone,
//! `alab::upkeep::tests::an_idle_hour_reports_its_counts` ([`TEST`]): on a two-Mac lab whose
//! registry is in its second generation, carrying a facts row of each Mac from a membership
//! epoch that is no longer current, and whose owner declares `lab/tracker` with no entry, the
//! owner's materializer schedule (on the test's clock) makes its first pass over the ten
//! shared files, then 60 regular ones with nothing arriving and nothing changing. Thread counters, which no release build compiles, count each pass's
//! work, and the test prints one `FILES-IDLE {json}` line: what the first pass counted
//! (`first_<counter>`) and what the 60 after it counted together (`idle_<counter>`), for each of
//! [`COUNTERS`]. The test runs on one thread and counts only that thread's work, and before its
//! first pass it waits out the time a vault file or a table journal must stand unchanged for a
//! read to keep what it read, so every pass reads what an idle Mac's reads however long the
//! setup took: the counts do not move with the load or the clock.
//!
//! Each count is judged SMALLER-OR-EQUAL its value in the committed baseline
//! (`perf-baseline-files-idle.json`): one above fails, one below passes and says a recording
//! would ratchet it. A key the report or the baseline lacks fails the run, so no count goes
//! unjudged by a report that stopped carrying it. There is no law: the baseline alone judges,
//! and the drive's own assertions hold it to being the drive these counts are of (every pass
//! `live`, the carried facts shown as seats, every counter but the fences, the kept key asks and
//! the folder modes moved by the first pass). No time and no ratio is judged, and the lane keeps
//! no trend.
//!
//! NEGATIVE CONTROL (2026-11-15): with `file::materialize::opened` taking no pass for one whose
//! key it already recorded (`let unchanged = false && …`), every regular pass projecting
//! afresh, `targo --unverified verify perf files-idle` against the baseline recorded once a pass
//! that changes nothing writes nothing fails: `` idle_projections 60 > its baseline 0;
//! idle_machine_claim_projects 60 > its baseline 0; idle_machine_claim_contexts 120 > its
//! baseline 0 `` (a projecting pass makes one claim projection of MACHINE, and two claim
//! contexts: that projection's and the one the view reads its claim cells with), its puts and
//! chmods still 0 (each projection is what the files already hold). Run again on 2026-11-15
//! against the baseline recorded over the seven files, once main's journal ring files joined
//! the pass: the same three counts failed, at the same values.

use std::collections::BTreeMap;
use std::path::Path;

use aterm_json::Value;

use super::arena::{self, Halt};
use super::bench_build::{self, Targo, load_average};
use super::driver::{built_executable, cargo_driver, compiler_output};
use super::{LaneRun, Status, workspace_root};

/// The lane's name.
const LANE: &str = "files-idle";

/// The committed baseline, in `crates/aterm-verify/perf/golden/`.
const BASELINE_FILE: &str = "perf-baseline-files-idle.json";

/// The ignored drive in aterm-db's library tests.
const TEST: &str = "alab::upkeep::tests::an_idle_hour_reports_its_counts";

/// What starts the drive's one report line.
const PREFIX: &str = "FILES-IDLE ";

/// The drive's counters, by the names its report gives them: the table views a pass reads to
/// project from, MACHINE's claim projections and claim contexts, the durable writes and folder
/// modes the recordings make, the carried records handed to authentication, the store read
/// batches' fences and the asks of the kept historical keys.
pub(crate) const COUNTERS: [&str; 8] = [
    "projections",
    "machine_claim_projects",
    "machine_claim_contexts",
    "puts",
    "chmods",
    "carried_authenticated",
    "fences",
    "kept_key_asks",
];

/// The drive's two spans: its first pass, and the regular passes after it.
const SPANS: [&str; 2] = ["first", "idle"];

/// What the drive must say it ran, or its counts are not this lane's: the passes, the files,
/// the registry generation and the carried facts rows.
const SHAPE: [(&str, u64); 4] = [
    ("passes", 61),
    ("files", 10),
    ("generation", 2),
    ("carried", 2),
];

const COMMENT: &str = "aterm FILES-IDLE baseline: exact counts of the shared files' \
    materializer over an idle hour, from aterm-db's ignored drive \
    alab::upkeep::tests::an_idle_hour_reports_its_counts in the test profile: a two-Writer lab \
    whose registry is in its second generation, carrying each Mac's facts row from an old \
    membership epoch, and whose owner declares lab/tracker with no entry; the owner's schedule \
    makes its first pass over the ten shared files \
    (first_*) and then 60 regular passes with nothing changing (idle_*, summed). Counted on the \
    drive's thread: table views read to project (projections), MACHINE's claim projections and \
    claim contexts, durable writes by the recordings (puts), folder modes set by the looks, the \
    recordings and the outbox changes (chmods), carried records handed to authentication, store \
    read batch fences, and kept historical key asks. SMALLER-OR-EQUAL: a count above its value here fails. No time and no ratio is judged; \
    the lane runs at the load it finds and reports it. Re-record with targo --unverified verify \
    perf --record files-idle.";

/// One count the drive reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Count {
    pub key: String,
    pub value: u64,
}

/// The counts of the one `FILES-IDLE {json}` line in `text`, the drive's output, keyed as the
/// baseline keys them.
///
/// # Errors
/// No such line, more than one, one that does not parse, a drive of another shape than
/// [`SHAPE`], or a span that lacks a counter of [`COUNTERS`] or holds one this lane does not
/// know.
pub(crate) fn parse_report(text: &str) -> Result<Vec<Count>, String> {
    // libtest prints `test <name> ... ` before a test's own output on the same line.
    let mut reports = text
        .lines()
        .filter_map(|line| Some(&line[line.find(PREFIX)? + PREFIX.len()..]));
    let raw = reports
        .next()
        .ok_or("the drive printed no FILES-IDLE line")?;
    if reports.next().is_some() {
        return Err("the drive printed more than one FILES-IDLE line".into());
    }
    let report: Value = aterm_json::from_str(raw.trim())
        .map_err(|error| format!("the FILES-IDLE line does not parse: {error}"))?;
    if report.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err("the FILES-IDLE line is not a schema 1 report".into());
    }
    for (key, want) in SHAPE {
        let got = report.get(key).and_then(Value::as_u64);
        if got != Some(want) {
            return Err(format!(
                "the drive reports {key} {got:?}, not {want}: these are not the lane's counts"
            ));
        }
    }
    let mut counts = Vec::new();
    for span in SPANS {
        let counted = report
            .get(span)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("the FILES-IDLE line has no `{span}` counts"))?;
        if let Some(unknown) = counted.keys().find(|key| !COUNTERS.contains(&key.as_str())) {
            return Err(format!(
                "the `{span}` counts hold `{unknown}`, which this lane does not judge: add it \
                 to COUNTERS"
            ));
        }
        for counter in COUNTERS {
            let value = counted
                .get(counter)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("the `{span}` counts have no `{counter}`"))?;
            counts.push(Count {
                key: format!("{span}_{counter}"),
                value,
            });
        }
    }
    Ok(counts)
}

/// Judge `counts` against `baseline` (the committed file's JSON): a count above its baseline
/// fails, one below passes and says a recording would ratchet it, and a count the baseline does
/// not hold fails, as does a baseline count no longer reported. With no baseline at all nothing
/// is judged.
pub(crate) fn judge(counts: &[Count], baseline: Option<&Value>) -> LaneRun {
    let Some(baseline) = baseline else {
        for c in counts {
            eprintln!("    {}: {} (no baseline)", c.key, c.value);
        }
        return LaneRun::not_judged(format!(
            "no {LANE} baseline recorded (--record {LANE} writes one)"
        ));
    };
    let mut run = LaneRun::pass();
    for c in counts {
        match baseline.get(&c.key).and_then(Value::as_u64) {
            Some(base) if c.value > base => {
                eprintln!("    {}: FAILED — {} > baseline {base}", c.key, c.value);
                run.add_failure(format!("{} {} > its baseline {base}", c.key, c.value));
            }
            Some(base) if c.value < base => eprintln!(
                "    {}: GREEN — {} < baseline {base} (--record {LANE} ratchets it)",
                c.key, c.value
            ),
            Some(base) => eprintln!("    {}: GREEN — {} == baseline {base}", c.key, c.value),
            None => {
                eprintln!("    {}: FAILED — {} has no baseline", c.key, c.value);
                run.add_failure(format!(
                    "{} {} has no baseline value (--record {LANE} writes one)",
                    c.key, c.value
                ));
            }
        }
    }
    let stale = baseline
        .as_object()
        .into_iter()
        .flat_map(|held| held.keys())
        .filter(|key| *key != "_comment" && !counts.iter().any(|c| &c.key == *key));
    for key in stale {
        eprintln!("    {key}: FAILED — in the baseline, not reported");
        run.add_failure(format!(
            "the baseline holds {key}, which the drive no longer reports (--record {LANE} \
             rewrites it)"
        ));
    }
    run
}

/// The baseline file's text: every count by key.
fn baseline_json(counts: &[Count]) -> String {
    let by_key: BTreeMap<&str, u64> = counts.iter().map(|c| (c.key.as_str(), c.value)).collect();
    let mut s = format!(
        "{{\n  \"_comment\": {}",
        aterm_json::to_string(&Value::String(COMMENT.to_string()))
            .unwrap_or_else(|_| "\"\"".to_string())
    );
    for (key, value) in by_key {
        s.push_str(&format!(",\n  \"{key}\": {value}"));
    }
    s.push_str("\n}\n");
    s
}

/// The committed baseline at `path`: `Ok(None)` when there is none.
fn read_baseline(path: &Path) -> Result<Option<Value>, String> {
    let text = match crate::read_regular_text(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} is unreadable: {e}", path.display())),
    };
    aterm_json::from_str::<Value>(text.trim())
        .ok()
        .filter(Value::is_object)
        .map(Some)
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))
}

/// Record `counts` as the baseline at `path`, or judge them against it.
fn conclude_using(
    counts: &[Count],
    record: bool,
    path: &Path,
    publish: super::publication::Publisher,
) -> LaneRun {
    if record {
        return match publish(path, &baseline_json(counts)) {
            Ok(()) => {
                eprintln!("  RECORDED baseline -> {}", path.display());
                LaneRun {
                    status: Status::Recorded(path.to_path_buf()),
                    ..LaneRun::pass()
                }
            }
            Err(e) => LaneRun::fail(format!("could not write {}: {e}", path.display())),
        };
    }
    match read_baseline(path) {
        Ok(baseline) => judge(counts, baseline.as_ref()),
        Err(why) => LaneRun::fail(why),
    }
}

/// The lane: build aterm-db's library tests, run the drive, then judge (or record) its counts.
pub(crate) fn run(record: bool) -> LaneRun {
    match measure() {
        Ok(counts) => conclude_using(
            &counts,
            record,
            &super::floors::golden_dir().join(BASELINE_FILE),
            super::publication::publish,
        ),
        Err(halt) => arena::outcome(Err(halt)),
    }
}

fn measure() -> Result<Vec<Count>, Halt> {
    let driver = cargo_driver()
        .map_err(|why| Halt::skip(why, "run it with the pinned toolchain's targo"))?;
    let targo = Targo::new(driver);
    let args = [
        "-p",
        "aterm-db",
        "--lib",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ];
    eprintln!("  $ {}", targo.display("test", &args));
    eprintln!("  load (1, 5, 15 min) before the build: {}", load_average());
    let mut build = targo.command("test")?;
    build.args(args).current_dir(workspace_root());
    let built = compiler_output(&mut build)
        .map_err(|error| format!("could not build aterm-db's library tests: {error}"))?;
    if !built.status.success() {
        let err = String::from_utf8_lossy(&built.stderr);
        let tail: Vec<&str> = err.lines().rev().take(bench_build::TAIL).collect();
        return Err(Halt::Fail(format!(
            "aterm-db's library tests did not build\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    let executable = built_executable(&String::from_utf8_lossy(&built.stdout), "aterm_db", "lib")?;
    let mut drive = crate::exec::check_command(&executable);
    targo.environment(&mut drive);
    // Every measurement is ignored in a plain run; `--exact` keeps this to the one named.
    drive.current_dir(workspace_root()).args([
        TEST,
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    eprintln!(
        "  $ {} {TEST} --exact --ignored --nocapture --test-threads=1",
        executable.display()
    );
    let before = load_average();
    let output = drive
        .output()
        .map_err(|error| format!("could not run the idle-hour drive: {error}"))?;
    eprintln!(
        "  load (1, 5, 15 min): {before} before the drive, {} after",
        load_average()
    );
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        let lines: Vec<_> = text.lines().rev().take(bench_build::TAIL).collect();
        return Err(Halt::Fail(format!(
            "the idle-hour drive exited {:?}\n{}",
            output.status.code(),
            lines.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    parse_report(&text).map_err(Halt::Fail)
}

#[cfg(test)]
#[path = "files_idle_tests.rs"]
mod tests;
