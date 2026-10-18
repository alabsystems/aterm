// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The FLOOR lanes: each runs an `aterm-bench` example in a release
//! subprocess, reads the one JSON line it prints, and holds every gated metric
//! to a floor derived from its committed baseline in `crates/aterm-verify/perf/golden/`.
//!
//! aterm is a NO-CI, MULTI-MACHINE repo: boxes of different speeds share one
//! committed baseline, and a run may land on a throttled laptop. So a floor
//! must catch a CATASTROPHIC regression (an algorithmic blow-up, a debug-build
//! slip, lock contention) while NEVER failing a normal or slower machine:
//!
//! * MEASURE the shipped build: `targo --unverified run --release`, never this
//!   debug-built CLI.
//! * MEDIAN-OF-N after warmup, inside each harness: one scheduler hiccup cannot
//!   move a median.
//! * A GENEROUS RATIO: a metric fails only below `baseline * PASS_RATIO`.
//! * A FRESH CHECKOUT IS NEVER BLOCKED: with no baseline the lane reports the
//!   number and says it judged nothing.
//!
//! The tight same-box comparison is the trend ledger's job ([`super::trend`]).

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::trend::utc_date;
use super::workspace_root;
use super::{LaneRun, Status, TrendSample};

/// The pass threshold as a fraction of the recorded baseline: a metric fails
/// iff `measured < baseline * PASS_RATIO`.
///
/// 0.45 lets a box run at 45% of the baseline's speed (~2.2x slower) and pass:
/// measured cross-box and throttled spreads sit inside ~1.5x, while a debug
/// build or an algorithmic blow-up costs 5x-50x. The deterministic allocation
/// gates (`mem_budget`, `perf_scaling`) are the precise guards; this is the
/// coarse wall-clock floor.
pub(crate) const PASS_RATIO: f64 = 0.45;

/// Where the committed baselines and the trend ledger live: beside this
/// crate's sources, so an exported tree carries its own data.
pub(crate) fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("perf/golden")
}

/// The verdict of comparing one fresh number against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No usable baseline (missing, non-finite or <= 0): judged nothing.
    NoBaseline,
    /// `measured >= baseline * ratio`.
    Pass,
    /// `measured < baseline * ratio`.
    Fail,
}

/// Pure threshold comparison. Returns the floor too, so callers can print it.
/// The boundary is inclusive: a measurement exactly at the floor passes.
pub(crate) fn compare(baseline: f64, measured: f64, ratio: f64) -> (Verdict, f64) {
    if !baseline.is_finite() || baseline <= 0.0 {
        return (Verdict::NoBaseline, 0.0);
    }
    let floor = baseline * ratio;
    if measured >= floor {
        (Verdict::Pass, floor)
    } else {
        (Verdict::Fail, floor)
    }
}

/// A finite number under `key` of a JSON object.
pub(crate) fn number(json: &Value, key: &str) -> Option<f64> {
    json.get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
}

/// A JSON object from text, or `None`.
fn object(text: &str) -> Option<Value> {
    aterm_json::from_str::<Value>(text.trim())
        .ok()
        .filter(Value::is_object)
}

/// The committed baseline at `path`: `Ok(None)` when there is none (a fresh
/// checkout judges nothing and says so), an error when it exists but `parse`
/// cannot use it. An unusable baseline fails rather than reading as absent,
/// so a key added without its recorded value cannot switch a lane off.
fn read_baseline<T>(
    path: &Path,
    parse: impl FnOnce(&Value) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} is unreadable: {e}", path.display())),
    };
    let json = object(&text).ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    parse(&json)
        .map(Some)
        .map_err(|why| format!("{} {why}", path.display()))
}

// ---------------------------------------------------------------------------
// The throughput lane.
// ---------------------------------------------------------------------------

/// The `perf_harness` report, and the shape persisted to its baseline.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PerfReport {
    pub median_mbps: f64,
    pub min_mbps: f64,
    pub max_mbps: f64,
    pub workload_bytes: u64,
    pub n: u64,
    pub warmup: u64,
}

/// A [`PerfReport`] from the harness line or a baseline; `None` if any field
/// is missing.
pub(crate) fn parse_report(json: &Value) -> Option<PerfReport> {
    Some(PerfReport {
        median_mbps: number(json, "median_mbps")?,
        min_mbps: number(json, "min_mbps")?,
        max_mbps: number(json, "max_mbps")?,
        workload_bytes: number(json, "workload_bytes")? as u64,
        n: number(json, "n")? as u64,
        warmup: number(json, "warmup")? as u64,
    })
}

/// The committed baseline for a [`PerfReport`]. `pass_ratio` is recorded for
/// readers; the lane judges with [`PASS_RATIO`].
pub(crate) fn baseline_json(r: &PerfReport, ratio: f64, comment: &str) -> String {
    format!(
        "{{\n  \"_comment\": {},\n  \"median_mbps\": {:.3},\n  \"min_mbps\": {:.3},\n  \"max_mbps\": {:.3},\n  \"workload_bytes\": {},\n  \"n\": {},\n  \"warmup\": {},\n  \"pass_ratio\": {:.3}\n}}\n",
        quoted(comment),
        r.median_mbps,
        r.min_mbps,
        r.max_mbps,
        r.workload_bytes,
        r.n,
        r.warmup,
        ratio,
    )
}

const THROUGHPUT_COMMENT: &str = "aterm wall-clock throughput baseline (PERF-WALLCLOCK-BASELINE). \
    Median-of-N MB/s of Terminal::process over a deterministic ~32 MiB mixed VT workload. \
    Re-record with `targo --unverified verify perf --record throughput`. The lane \
    fails only if measured median < median_mbps * pass_ratio.";

pub(crate) fn baseline_path() -> PathBuf {
    golden_dir().join("perf-baseline.json")
}

/// A JSON string literal for `s`, escaped by the one JSON writer.
fn quoted(s: &str) -> String {
    aterm_json::to_string(&Value::String(s.to_string())).unwrap_or_else(|_| "\"\"".to_string())
}

/// THROUGHPUT: the median clears `baseline * PASS_RATIO`.
pub(crate) fn gate_throughput(record: bool) -> LaneRun {
    let report = match measure_example("perf_harness").and_then(|json| {
        parse_report(&json).ok_or_else(|| format!("perf_harness JSON lacks a field: {json:?}"))
    }) {
        Ok(r) => r,
        Err(e) => return LaneRun::fail(format!("could not measure: {e}")),
    };
    let samples = vec![TrendSample {
        lane: "throughput",
        metric: "median_mbps".to_string(),
        value: report.median_mbps,
        inverted: false,
    }];
    let path = baseline_path();
    let mut run = if record {
        let previous = std::fs::read_to_string(&path).ok().and_then(|t| object(&t));
        let (comment, _) = carried_comment(previous.as_ref(), THROUGHPUT_COMMENT, &utc_date());
        write_baseline(&path, &baseline_json(&report, PASS_RATIO, &comment))
    } else {
        compare_against_baseline(&path, &report)
    };
    run.samples = samples;
    run
}

/// Read the baseline at `path` (if any) and apply the floor.
fn compare_against_baseline(path: &Path, report: &PerfReport) -> LaneRun {
    let base = match read_baseline(path, |v| {
        parse_report(v).ok_or_else(|| "lacks a perf_harness field".to_string())
    }) {
        Ok(Some(base)) => base,
        Ok(None) => {
            eprintln!(
                "  throughput: no baseline at {} — {:.1} MB/s (median of {}) judged against \
                 nothing (record with --record throughput).",
                path.display(),
                report.median_mbps,
                report.n,
            );
            return LaneRun::not_judged("no baseline");
        }
        Err(why) => {
            return LaneRun::fail(format!(
                "unusable baseline: {why}; re-record with --record throughput"
            ));
        }
    };
    match compare(base.median_mbps, report.median_mbps, PASS_RATIO) {
        (Verdict::NoBaseline, _) => {
            eprintln!(
                "  throughput: baseline median is non-positive — {:.1} MB/s judged against nothing.",
                report.median_mbps
            );
            LaneRun::not_judged("non-positive baseline")
        }
        (Verdict::Pass, floor) => {
            eprintln!(
                "  throughput: GREEN — {:.1} MB/s >= floor {floor:.1} MB/s (baseline {:.1} x {PASS_RATIO:.2}).",
                report.median_mbps, base.median_mbps,
            );
            LaneRun::pass()
        }
        (Verdict::Fail, floor) => LaneRun::fail(format!(
            "throughput {:.1} MB/s < floor {floor:.1} MB/s (baseline {:.1} x {PASS_RATIO:.2}): a \
             catastrophic regression (debug build? algorithmic blow-up? lock contention?)",
            report.median_mbps, base.median_mbps,
        )),
    }
}

// ---------------------------------------------------------------------------
// KEYED lanes: the harness emits flat JSON and every listed key gets its own
// floor. Keys are bigger-is-better; a missing key makes the whole report
// unparseable (never "the missing metric passed").
// ---------------------------------------------------------------------------

/// A keyed floor lane.
pub(crate) struct FloorLane {
    /// The lane's name in the trend ledger's `lane/metric` column.
    pub lane: &'static str,
    /// The `aterm-bench` example that measures it.
    pub example: &'static str,
    /// The gated keys (bigger-is-better); other keys in the report are
    /// informational.
    pub keys: &'static [&'static str],
    /// The baseline's file name under [`golden_dir`].
    pub baseline_file: &'static str,
    /// The `_comment` a first recording writes.
    pub comment: &'static str,
    /// The CLI lane name, which every message names it by (`lane` can differ
    /// for trend continuity: `scroll-scrub` trends as `scroll`).
    pub cli: &'static str,
}

/// Per-corpus hostile-input floors: one floor PER CORPUS, so an SGR-interning
/// regression (style_churn) cannot hide behind a healthy ASCII number. The
/// keys mirror `aterm-bench/examples/pathological_harness.rs`.
pub(crate) const PATHOLOGICAL_LANE: FloorLane = FloorLane {
    lane: "pathological",
    example: "pathological_harness",
    keys: &[
        "yes_flood_median_mbps",
        "escape_storm_median_mbps",
        "style_churn_median_mbps",
        "long_escapes_median_mbps",
        "wide_unicode_median_mbps",
    ],
    baseline_file: "perf-baseline-pathological.json",
    comment: "aterm PATHOLOGICAL-BENCH baseline: per-corpus median MB/s of Terminal::process \
              under hostile input. Re-record with targo --unverified verify perf \
              --record pathological. Each corpus fails independently iff measured < recorded * \
              pass_ratio.",
    cli: "pathological",
};

/// Scrollback-scrub read-path floors: tiered compressed scrollback pays tier
/// decode on the scrub path where an all-RAM scrollback pays pointer math.
/// Jump-to-top is jumps/sec so every key is bigger-is-better. The windowed half
/// is the `scroll` arena lane. The keys mirror
/// `aterm-bench/examples/scroll_scrub_harness.rs`.
pub(crate) const SCROLL_LANE: FloorLane = FloorLane {
    lane: "scroll",
    example: "scroll_scrub_harness",
    keys: &[
        "scrub_median_rps",
        "pageup_median_rps",
        "jump_top_median_jps",
    ],
    baseline_file: "perf-baseline-scroll.json",
    comment: "aterm ARENA-SCROLL baseline: scrollback-scrub read-path rates over a 100k+-line \
              tiered-scrollback fill. All BIGGER-IS-BETTER. Re-record with targo --unverified \
              verify perf --record scroll-scrub.",
    cli: "scroll-scrub",
};

pub(crate) const SEARCH_LANE: FloorLane = FloorLane {
    lane: "search",
    example: "search_harness",
    keys: &[
        "rotating_build_klps",
        "rotating_query_qps",
        "rotating_lines_per_mib",
        "replog_build_klps",
        "replog_query_qps",
        "replog_lines_per_mib",
        "linkheavy_build_klps",
        "linkheavy_query_qps",
        "linkheavy_lines_per_mib",
        "index_line_klps",
    ],
    baseline_file: "perf-baseline-search.json",
    comment: "aterm SEARCH-BENCH baseline: full-rebuild klines/s, cached-query q/s and \
              retained-index lines-per-MiB on three corpora, plus the incremental \
              index_scrollback_line primitive. All BIGGER-IS-BETTER. Re-record with targo \
              --unverified verify perf --record search.",
    cli: "search",
};

pub(crate) const RESTORE_LANE: FloorLane = FloorLane {
    lane: "restore",
    example: "restore_harness",
    keys: &["restore_median_hz"],
    baseline_file: "perf-baseline-restore.json",
    comment: "aterm RESTORE-BENCH baseline: serialize->fresh-engine replay rate over a 10k-line \
              SGR-mixed snapshot. BIGGER-IS-BETTER. Re-record with targo --unverified verify \
              perf --record restore.",
    cli: "restore",
};

pub(crate) const RESIZE_LANE: FloorLane = FloorLane {
    lane: "resize",
    example: "resize_rewrap_harness",
    keys: &["resize_ring_median_rps", "resize_tiered_median_rps"],
    baseline_file: "perf-baseline-resize.json",
    comment: "aterm RESIZE/REWRAP baseline: synchronous full-ring resizes/s at the 50k cap and \
              offload+pump+reattach cycles/s over a 110k-line tiered fill. BIGGER-IS-BETTER; the \
              absolute fences live in the lane code. Re-record with targo --unverified verify \
              perf --record resize.",
    cli: "resize",
};

/// CRASH-RESTORE floor (the crash-restore plan §6 and §7.3; `crates/aterm-restore`): what
/// the journal costs the parse thread and what a restore costs the next window. The keys
/// mirror `aterm-bench/examples/crash_restore_harness.rs`: the parse thread's throughput
/// with the tail tap on, keyframes per second (capture, encode and copy) for a shell, a
/// full-screen program over a saved primary and a shell with a 256- and a 512-line bridge,
/// and restores per second of a keyframe plus a 256 KiB tail. The tap's overhead ratio is an
/// absolute fence ([`TAIL_TAP_RATIO_FENCE`]), not a ratio floor: it is already a ratio. Named
/// `crash-restore` because `restore` is the serialize-and-replay lane.
pub(crate) const CRASH_RESTORE_LANE: FloorLane = FloorLane {
    lane: "crash-restore",
    example: "crash_restore_harness",
    keys: &[
        "tail_tap_mbps",
        "kf_shell_hz",
        "kf_tui_hz",
        "kf_bridge256_hz",
        "kf_bridge512_hz",
        "recover_hz",
    ],
    baseline_file: "perf-baseline-crash-restore.json",
    comment: "aterm CRASH-RESTORE baseline: parse-thread MB/s with the journal's tail tap, \
              keyframes/s (shell, TUI over a saved primary, +256 and +512 bridge) and \
              restores/s of a keyframe plus a 256 KiB tail. BIGGER-IS-BETTER; the tap \
              overhead fence lives in the lane code. Re-record with targo --unverified verify \
              perf --record crash-restore.",
    cli: "crash-restore",
};

/// The tail tap's ABSOLUTE fence: with the tap on, the parse thread keeps at least this
/// fraction of its throughput on the throughput corpus (the plan's floor). The tap copies
/// each PTY byte once into the region after `process`; measured 0.993-0.995 on an M5 Max
/// (a word-at-a-time copy measured 0.75 and a double copy 0.93, which this fence catches).
/// It holds with no baseline, like the resize fences.
pub(crate) const TAIL_TAP_RATIO_FENCE: f64 = 0.97;

/// ZSTD-DECODE floor (docs/AUDIT-third-party-2026-10-15.md ZC.4): decoded MB/s
/// of `aterm_zstd::Decoder`, atpkg's `.tar.zst` reader, over five frames the
/// zstd 1.5.7 CLI wrote (aterm-zstd's recorded answers, so no encoder runs and
/// the input never drifts). The keys mirror
/// `aterm-bench/examples/zstd_decode_harness.rs`.
pub(crate) const ZSTD_LANE: FloorLane = FloorLane {
    lane: "zstd",
    example: "zstd_decode_harness",
    keys: &["zstd_decode_median_mbps"],
    baseline_file: "perf-baseline-zstd.json",
    comment: "aterm ZSTD-DECODE baseline (ZC.4): median decoded MB/s of aterm_zstd::Decoder \
              over five recorded zstd 1.5.7 CLI frames (-19, --ultra -22 --long=27, --fast=5, \
              a 1 KiB window, a single-segment file frame). BIGGER-IS-BETTER. Re-record with \
              targo --unverified verify perf --record zstd.",
    cli: "zstd",
};

/// The 42s-freeze-class ABSOLUTE fences. Unlike the ratio floors these hold on
/// a fresh checkout with no baseline: a synchronous O(history) rewrap costs
/// seconds, far past either cap, while the measured worsts (73 ms ring, 18.6 ms
/// tiered-sync on an M5 Max; a slow box ~2-3x) sit far below.
///
/// The tiered cap is ~5x the measured worst on purpose: the offloading
/// resize's synchronous phase must stay VIEWPORT-bounded, and O(history) work
/// leaking back costs hundreds of ms at the harness's 110k-line depth.
pub(crate) const RESIZE_RING_WORST_CAP_MS: f64 = 5_000.0;
pub(crate) const RESIZE_TIERED_SYNC_WORST_CAP_MS: f64 = 100.0;

/// The gated values of `json`, in key order; `None` if ANY key is missing.
pub(crate) fn parse_keyed(lane: &FloorLane, json: &Value) -> Option<Vec<(&'static str, f64)>> {
    lane.keys
        .iter()
        .map(|key| number(json, key).map(|v| (*key, v)))
        .collect()
}

/// A keyed lane's committed baseline (hand-rendered: one key per line, so a
/// re-record diffs line by line).
pub(crate) fn keyed_baseline_json(values: &[(&str, f64)], ratio: f64, comment: &str) -> String {
    let mut s = format!("{{\n  \"_comment\": {},\n", quoted(comment));
    for (key, v) in values {
        s.push_str(&format!("  \"{key}\": {v:.3},\n"));
    }
    s.push_str(&format!("  \"pass_ratio\": {ratio:.3}\n}}\n"));
    s
}

/// The `_comment` a re-record writes: the committed note, grown by a dated
/// marker, or `default` when there is none. `true` when a note was carried.
///
/// The committed note is the only record of how a floor was earned (which box,
/// which date, why), so a re-record keeps it rather than replacing it.
pub(crate) fn carried_comment(
    previous: Option<&Value>,
    default: &str,
    date: &str,
) -> (String, bool) {
    let Some(prev) = previous
        .and_then(|p| p.get("_comment"))
        .and_then(Value::as_str)
        .map(str::trim_end)
    else {
        return (default.to_string(), false);
    };
    let marker = format!(" Re-recorded {date}.");
    if prev.ends_with(&marker) {
        (prev.to_string(), true)
    } else {
        (format!("{prev}{marker}"), true)
    }
}

fn keyed_baseline_path(lane: &FloorLane) -> PathBuf {
    golden_dir().join(lane.baseline_file)
}

fn write_baseline(path: &Path, text: &str) -> LaneRun {
    match std::fs::write(path, text) {
        Ok(()) => {
            eprintln!("  RECORDED baseline -> {}", path.display());
            LaneRun {
                status: Status::Recorded(path.to_path_buf()),
                ..LaneRun::pass()
            }
        }
        Err(e) => LaneRun::fail(format!("could not write {}: {e}", path.display())),
    }
}

/// Run an `aterm-bench` example in release and return its JSON report (the
/// last stdout line that starts with `{`).
pub(crate) fn measure_example(example: &str) -> Result<Value, String> {
    let driver = super::driver::cargo_driver()?;
    let args = ["--release", "-q", "-p", "aterm-bench", "--example", example];
    eprintln!("  $ {}", driver.display("run", &args));
    let out = driver
        .command("run")
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not spawn {example}: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if line.contains(&format!("{example}:")) {
            eprintln!("  {line}");
        }
    }
    if !out.status.success() {
        return Err(format!(
            "{example} exited {:?}\n{stderr}",
            out.status.code()
        ));
    }
    last_json_line(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("{example} printed no JSON report"))
}

/// The last line of `stdout` that parses as a JSON object.
pub(crate) fn last_json_line(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .filter(|l| l.trim_start().starts_with('{'))
        .find_map(object)
}

/// Record or judge a keyed lane from its measured report.
pub(crate) fn judge_keyed(lane: &FloorLane, json: &Value, record: bool) -> LaneRun {
    let Some(values) = parse_keyed(lane, json) else {
        return LaneRun::fail(format!(
            "the harness report lacks a gated key ({}); refusing to judge it",
            lane.keys.join(", ")
        ));
    };
    let samples: Vec<TrendSample> = values
        .iter()
        .map(|(key, v)| TrendSample {
            lane: lane.lane,
            metric: (*key).to_string(),
            value: *v,
            inverted: false,
        })
        .collect();
    let path = keyed_baseline_path(lane);
    let mut run = if record {
        record_keyed(lane, &path, &values)
    } else {
        compare_keyed_against_baseline(lane, &path, &values)
    };
    run.samples = samples;
    run
}

/// Write a keyed lane's baseline from `values`, refusing a non-positive one:
/// a floor of zero judges nothing, so recording it would RETIRE the metric.
fn record_keyed(lane: &FloorLane, path: &Path, values: &[(&'static str, f64)]) -> LaneRun {
    if let Some((key, v)) = values.iter().find(|(_, v)| !(v.is_finite() && *v > 0.0)) {
        return LaneRun::fail(format!(
            "REFUSING to record — {key} measured {v}; a non-positive floor judges nothing. \
             {} left untouched.",
            path.display()
        ));
    }
    let previous = std::fs::read_to_string(path).ok().and_then(|t| object(&t));
    let (comment, carried) = carried_comment(previous.as_ref(), lane.comment, &utc_date());
    if carried {
        eprintln!(
            "  {}: carried the committed `_comment` forward (the VALUES under it are this \
             run's) — amend the note if it no longer describes them.",
            lane.cli
        );
    }
    write_baseline(path, &keyed_baseline_json(values, PASS_RATIO, &comment))
}

/// Judge `values` against the baseline at `path`, key by key.
fn compare_keyed_against_baseline(
    lane: &FloorLane,
    path: &Path,
    values: &[(&'static str, f64)],
) -> LaneRun {
    let base = match read_baseline(path, |v| {
        parse_keyed(lane, v).ok_or_else(|| {
            let missing: Vec<&str> = lane
                .keys
                .iter()
                .copied()
                .filter(|k| number(v, k).is_none())
                .collect();
            format!("lacks {}", missing.join(", "))
        })
    }) {
        Ok(Some(base)) => base,
        Ok(None) => {
            eprintln!(
                "  {}: no baseline at {} — judged against nothing (record with --record {}).",
                lane.cli,
                path.display(),
                lane.cli
            );
            for (key, v) in values {
                eprintln!("    {key}: {v:.1}");
            }
            return LaneRun::not_judged("no baseline");
        }
        Err(why) => {
            return LaneRun::fail(format!(
                "unusable baseline: {why}; re-record with --record {}",
                lane.cli
            ));
        }
    };
    let mut run = LaneRun::pass();
    for ((key, v), (_, bv)) in values.iter().zip(base.iter()) {
        match compare(*bv, *v, PASS_RATIO) {
            (Verdict::NoBaseline, _) => {
                eprintln!("    {key}: NOT JUDGED — recorded baseline is non-positive ({bv}).");
                run.unjudged
                    .push(format!("{}/{key} (non-positive baseline)", lane.cli));
            }
            (Verdict::Pass, floor) => eprintln!("    {key}: GREEN — {v:.1} >= floor {floor:.1}"),
            (Verdict::Fail, floor) => {
                eprintln!("    {key}: FAILED — {v:.1} < floor {floor:.1}");
                run.add_failure(format!(
                    "{key} {v:.1} < floor {floor:.1} (baseline {bv:.1} x {PASS_RATIO:.2})"
                ));
            }
        }
    }
    if run.status == Status::Pass && run.unjudged.len() == values.len() {
        return LaneRun::not_judged("every baseline value is non-positive");
    }
    run
}

/// Measure and judge a keyed lane whose harness is an `aterm-bench` example.
pub(crate) fn gate_keyed(lane: &FloorLane, record: bool) -> LaneRun {
    match measure_example(lane.example) {
        Ok(json) => judge_keyed(lane, &json, record),
        Err(e) => LaneRun::fail(format!("could not measure: {e}")),
    }
}

/// CRASH-RESTORE: the ratio floors, the tail tap's absolute fence (which holds with no
/// baseline), and a line naming every key the harness reported that nothing judges.
pub(crate) fn gate_crash_restore(record: bool) -> LaneRun {
    match measure_example(CRASH_RESTORE_LANE.example) {
        Ok(json) => judge_crash_restore(&json, record),
        Err(e) => LaneRun::fail(format!("could not measure: {e}")),
    }
}

fn judge_crash_restore(json: &Value, record: bool) -> LaneRun {
    let mut run = judge_keyed(&CRASH_RESTORE_LANE, json, record);
    match tail_tap_fence(json) {
        Ok(()) => run.fence_held(CRASH_RESTORE_LANE.cli),
        Err(finding) => run.add_failure(finding),
    }
    let ungated: Vec<&str> = json
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|k| !CRASH_RESTORE_LANE.keys.contains(k) && *k != "tail_tap_ratio")
        .collect();
    if !ungated.is_empty() {
        eprintln!("    UNGATED — reported, not judged: {}", ungated.join(", "));
    }
    run
}

/// The tail tap's fence: a missing ratio is a breach (an unmeasured cost is not a small
/// one).
pub(crate) fn tail_tap_fence(json: &Value) -> Result<(), String> {
    match number(json, "tail_tap_ratio") {
        Some(v) if v >= TAIL_TAP_RATIO_FENCE => {
            eprintln!(
                "    tail_tap_ratio: GREEN — {v:.3} >= absolute fence {TAIL_TAP_RATIO_FENCE}"
            );
            Ok(())
        }
        Some(v) => Err(format!(
            "tail_tap_ratio {v:.3} < absolute fence {TAIL_TAP_RATIO_FENCE}: the journal's tail tap \
             costs the parse thread more than the plan allows"
        )),
        None => Err("tail_tap_ratio missing from the harness report".to_string()),
    }
}

/// The resize report's LOWER-is-better latencies: same-box trend samples,
/// INVERTED, so a creep under the absolute caps is still caught on the box
/// that has history.
const RESIZE_INVERTED_TREND_KEYS: [&str; 7] = [
    "resize_ring_worst_ms",
    "resize_tiered_sync_worst_ms",
    "deep_mixed_pump_total_ms",
    "deep_mixed_step_worst_ms",
    "deep_mixed_sync_worst_ms",
    "deep_wrapall_pump_total_ms",
    "deep_wrapall_step_worst_ms",
];

/// RESIZE/REWRAP: the ratio floors, the absolute fences (which hold with no
/// baseline at all), the inverted trend samples, and a line naming every key
/// the harness reported that nothing here judges.
pub(crate) fn gate_resize(record: bool) -> LaneRun {
    let json = match measure_example(RESIZE_LANE.example) {
        Ok(j) => j,
        Err(e) => return LaneRun::fail(format!("could not measure: {e}")),
    };
    judge_resize(&json, record)
}

fn judge_resize(json: &Value, record: bool) -> LaneRun {
    let mut run = judge_keyed(&RESIZE_LANE, json, record);
    for key in RESIZE_INVERTED_TREND_KEYS {
        match number(json, key) {
            Some(v) => run.samples.push(TrendSample {
                lane: RESIZE_LANE.lane,
                metric: key.to_string(),
                value: v,
                inverted: true,
            }),
            None => eprintln!("    {key}: NO TREND SAMPLE — absent from the harness report."),
        }
    }
    let breached = resize_fences(json);
    if breached.is_empty() {
        run.fence_held(RESIZE_LANE.cli);
    }
    for finding in breached {
        run.add_failure(finding);
    }
    let ungated: Vec<&str> = json
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|k| !RESIZE_LANE.keys.contains(k) && !RESIZE_INVERTED_TREND_KEYS.contains(k))
        .collect();
    if !ungated.is_empty() {
        // Several are the run's shape (ring_lines, n, warmup); converge_stale_width
        // is a correctness outcome nothing here refuses. Which deserve a floor is
        // an owner call; the log says they were not judged.
        eprintln!("    UNGATED — reported, not judged: {}", ungated.join(", "));
    }
    run
}

/// The absolute worst-case fences; each finding is one breach. A missing key
/// is a breach: an unmeasured latency is not a small one.
pub(crate) fn resize_fences(json: &Value) -> Vec<String> {
    let mut findings = Vec::new();
    for (key, cap) in [
        ("resize_ring_worst_ms", RESIZE_RING_WORST_CAP_MS),
        (
            "resize_tiered_sync_worst_ms",
            RESIZE_TIERED_SYNC_WORST_CAP_MS,
        ),
    ] {
        match number(json, key) {
            Some(v) if v <= cap => {
                eprintln!("    {key}: GREEN — {v:.1} ms <= absolute cap {cap:.0} ms");
            }
            Some(v) => findings.push(format!(
                "{key} {v:.1} ms > absolute cap {cap:.0} ms: the 42s-freeze class \
                 (synchronous O(history) rewrap) is back"
            )),
            None => findings.push(format!("{key} missing from the harness report")),
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        aterm_json::from_str(text).expect("test JSON parses")
    }

    fn report(median: f64) -> PerfReport {
        PerfReport {
            median_mbps: median,
            min_mbps: median * 0.9,
            max_mbps: median * 1.1,
            workload_bytes: 32 << 20,
            n: 7,
            warmup: 2,
        }
    }

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("atv-perf-floors-")
            .tempdir()
            .expect("scratch dir")
    }

    #[test]
    fn compare_boundary_is_inclusive_pass() {
        let (v, floor) = compare(1000.0, 450.0, 0.45);
        assert_eq!(v, Verdict::Pass);
        assert!((floor - 450.0).abs() < 1e-9, "floor {floor}");
        assert_eq!(compare(1000.0, 449.999, 0.45).0, Verdict::Fail);
    }

    #[test]
    fn compare_nonpositive_baseline_is_no_baseline() {
        assert_eq!(compare(0.0, 1000.0, 0.45).0, Verdict::NoBaseline);
        assert_eq!(compare(-5.0, 1000.0, 0.45).0, Verdict::NoBaseline);
        assert_eq!(compare(f64::NAN, 1000.0, 0.45).0, Verdict::NoBaseline);
    }

    #[test]
    fn the_throughput_baseline_round_trips_and_reads_the_harness_line() {
        let r = report(1234.5);
        let back = parse_report(&json(&baseline_json(&r, PASS_RATIO, "a \"quoted\" note")))
            .expect("parse the baseline we wrote");
        assert!((back.median_mbps - r.median_mbps).abs() < 1e-3);
        assert_eq!(back.workload_bytes, r.workload_bytes);
        let line = "{\"median_mbps\":3142.000,\"min_mbps\":3000.500,\"max_mbps\":3300.250,\
                    \"workload_bytes\":33554432,\"n\":7,\"warmup\":2}";
        let r = parse_report(&json(line)).expect("harness line");
        assert_eq!((r.workload_bytes, r.n, r.warmup), (33_554_432, 7, 2));
        assert!(parse_report(&json("{\"median_mbps\":100.0}")).is_none());
    }

    #[test]
    fn a_missing_baseline_judges_nothing_and_an_unusable_one_fails() {
        let dir = scratch();
        let path = dir.path().join("perf-baseline.json");
        let run = compare_against_baseline(&path, &report(1234.5));
        assert_eq!(run.status, Status::NotJudged("no baseline".into()));
        std::fs::write(&path, "{\"median_mbps\": 1000.0}").expect("write");
        let run = compare_against_baseline(&path, &report(1234.5));
        assert!(run.failed(), "{run:?}");
        std::fs::write(&path, "not json").expect("write");
        assert!(compare_against_baseline(&path, &report(1234.5)).failed());
    }

    const KEYED_LANES: [&FloorLane; 7] = [
        &PATHOLOGICAL_LANE,
        &SCROLL_LANE,
        &SEARCH_LANE,
        &RESTORE_LANE,
        &RESIZE_LANE,
        &ZSTD_LANE,
        &CRASH_RESTORE_LANE,
    ];

    fn keyed_line(lane: &FloorLane, scale: f64) -> Value {
        let mut s = String::from("{");
        for key in lane.keys {
            s.push_str(&format!("\"{key}\":{:.3},", 100.0 * scale));
        }
        s.push_str("\"n\":5,\"warmup\":1}");
        json(&s)
    }

    #[test]
    fn keyed_reports_round_trip_through_their_baselines() {
        for lane in KEYED_LANES
            .iter()
            .copied()
            .chain([&super::super::wasm::WASM_LANE])
        {
            let parsed = parse_keyed(lane, &keyed_line(lane, 1.0)).expect("full line parses");
            assert_eq!(parsed.len(), lane.keys.len());
            let back = parse_keyed(lane, &json(&keyed_baseline_json(&parsed, PASS_RATIO, "c")))
                .expect("baseline round-trips");
            assert_eq!(back, parsed);
        }
    }

    #[test]
    fn a_partial_report_is_refused_not_judged() {
        let mut full = keyed_line(&SEARCH_LANE, 1.0);
        if let Some(o) = full.as_object_mut() {
            o.remove("replog_query_qps");
        }
        assert!(parse_keyed(&SEARCH_LANE, &full).is_none());
        let run = judge_keyed(&SEARCH_LANE, &full, false);
        assert!(run.failed(), "{run:?}");
    }

    #[test]
    fn only_the_collapsed_metric_fails() {
        let dir = scratch();
        let path = dir.path().join("baseline.json");
        let base: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
        std::fs::write(&path, keyed_baseline_json(&base, PASS_RATIO, "c")).expect("write");
        let healthy: Vec<(&'static str, f64)> =
            SEARCH_LANE.keys.iter().map(|k| (*k, 90.0)).collect();
        assert_eq!(
            compare_keyed_against_baseline(&SEARCH_LANE, &path, &healthy).status,
            Status::Pass
        );
        let mut collapsed = healthy.clone();
        collapsed[3].1 = 10.0;
        match compare_keyed_against_baseline(&SEARCH_LANE, &path, &collapsed).status {
            Status::Fail(f) => {
                assert_eq!(f.len(), 1, "{f:?}");
                assert!(f[0].starts_with("replog_build_klps"), "{f:?}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_non_positive_baseline_metric_is_named_unjudged() {
        let dir = scratch();
        let path = dir.path().join("baseline.json");
        let mut base: Vec<(&'static str, f64)> =
            SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
        base[0].1 = 0.0;
        std::fs::write(&path, keyed_baseline_json(&base, PASS_RATIO, "c")).expect("write");
        let healthy: Vec<(&'static str, f64)> =
            SEARCH_LANE.keys.iter().map(|k| (*k, 90.0)).collect();
        let run = compare_keyed_against_baseline(&SEARCH_LANE, &path, &healthy);
        assert_eq!(run.status, Status::Pass);
        assert_eq!(
            run.unjudged,
            vec![format!(
                "search/{} (non-positive baseline)",
                SEARCH_LANE.keys[0]
            )]
        );
        // With every value non-positive the lane judged nothing at all.
        std::fs::write(
            &path,
            keyed_baseline_json(&[("restore_median_hz", 0.0)], PASS_RATIO, "c"),
        )
        .expect("write");
        let run =
            compare_keyed_against_baseline(&RESTORE_LANE, &path, &[("restore_median_hz", 5.0)]);
        assert!(matches!(run.status, Status::NotJudged(_)), "{run:?}");
    }

    /// A baseline missing a key fails, naming it: read as absent it would
    /// switch the whole lane off.
    #[test]
    fn an_incomplete_baseline_fails_and_names_the_missing_key() {
        let dir = scratch();
        let path = dir.path().join("baseline.json");
        let values: Vec<(&'static str, f64)> =
            SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
        std::fs::write(&path, keyed_baseline_json(&values[1..], PASS_RATIO, "c")).expect("write");
        match compare_keyed_against_baseline(&SEARCH_LANE, &path, &values).status {
            Status::Fail(f) => {
                assert!(f[0].contains(SEARCH_LANE.keys[0]), "{f:?}");
                assert!(f[0].contains("--record search"), "{f:?}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        let none = compare_keyed_against_baseline(&SEARCH_LANE, &dir.path().join("x"), &values);
        assert_eq!(none.status, Status::NotJudged("no baseline".into()));
    }

    #[test]
    fn recording_refuses_a_floor_that_judges_nothing() {
        let dir = scratch();
        let path = dir.path().join("baseline.json");
        let run = record_keyed(&RESTORE_LANE, &path, &[("restore_median_hz", 0.0)]);
        assert!(run.failed());
        assert!(!path.exists(), "a refused record writes nothing");
        let run = record_keyed(&RESTORE_LANE, &path, &[("restore_median_hz", 42.0)]);
        assert_eq!(run.status, Status::Recorded(path.clone()));
        let text = std::fs::read_to_string(&path).expect("written");
        assert!(text.contains("\"restore_median_hz\": 42.000"), "{text}");
    }

    #[test]
    fn a_re_record_keeps_the_committed_note_and_grows_it_once_a_day() {
        let prev = json("{\"_comment\": \"Recorded on m21 by hand; see \\\"why\\\".\"}");
        let (c, carried) = carried_comment(Some(&prev), "default", "2026-10-15");
        assert!(carried);
        assert_eq!(
            c,
            "Recorded on m21 by hand; see \"why\". Re-recorded 2026-10-15."
        );
        let again = json(&keyed_baseline_json(&[("k", 1.0)], PASS_RATIO, &c));
        let (twice, _) = carried_comment(Some(&again), "default", "2026-10-15");
        assert_eq!(twice, c, "a same-day re-record does not stutter");
        assert_eq!(
            carried_comment(None, "default", "d"),
            ("default".to_string(), false)
        );
    }

    #[test]
    fn resize_fences_hold_absolutely() {
        let fences = |s: &str| resize_fences(&json(s));
        assert!(
            fences("{\"resize_ring_worst_ms\":73.3,\"resize_tiered_sync_worst_ms\":15.0}")
                .is_empty()
        );
        assert_eq!(
            fences("{\"resize_ring_worst_ms\":42000.0,\"resize_tiered_sync_worst_ms\":15.0}").len(),
            1
        );
        assert_eq!(
            fences("{\"resize_ring_worst_ms\":73.3,\"resize_tiered_sync_worst_ms\":150.0}").len(),
            1
        );
        assert_eq!(
            fences("{\"resize_ring_worst_ms\":73.3}").len(),
            1,
            "missing is a breach"
        );
    }

    #[test]
    fn the_tail_tap_fence_holds_absolutely() {
        assert_eq!(tail_tap_fence(&json("{\"tail_tap_ratio\":0.994}")), Ok(()));
        assert_eq!(
            tail_tap_fence(&json("{\"tail_tap_ratio\":0.97}")),
            Ok(()),
            "inclusive"
        );
        assert!(tail_tap_fence(&json("{\"tail_tap_ratio\":0.93}")).is_err());
        assert!(
            tail_tap_fence(&json("{\"tail_tap_mbps\":500.0}")).is_err(),
            "missing is a breach"
        );
        // The fence holds with no baseline: a fresh checkout judges it and nothing else.
        let mut s = String::from("{\"tail_tap_ratio\":0.5,");
        for key in CRASH_RESTORE_LANE.keys {
            s.push_str(&format!("\"{key}\":10.0,"));
        }
        s.push_str("\"n\":9}");
        let run = judge_crash_restore(&json(&s), false);
        assert!(run.failed(), "{run:?}");
    }

    #[test]
    fn resize_samples_its_latencies_inverted() {
        let mut s = String::from("{");
        for key in RESIZE_LANE.keys {
            s.push_str(&format!("\"{key}\":10.0,"));
        }
        s.push_str("\"resize_ring_worst_ms\":70.0,\"resize_tiered_sync_worst_ms\":15.0,\"n\":5}");
        let run = judge_resize(&json(&s), false);
        let inverted: Vec<&str> = run
            .samples
            .iter()
            .filter(|t| t.inverted)
            .map(|t| t.metric.as_str())
            .collect();
        assert_eq!(
            inverted,
            vec!["resize_ring_worst_ms", "resize_tiered_sync_worst_ms"]
        );
    }
}
