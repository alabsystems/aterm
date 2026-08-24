// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Wall-clock THROUGHPUT baseline for `gate perf` (PERF-WALLCLOCK-BASELINE lane).
//!
//! aterm is a NO-CI, MULTI-MACHINE repo: m3 and m7 are different-speed boxes that
//! share one committed `tools/golden/perf-baseline.json`, and a gate run may land
//! on a throttled laptop. The #1 requirement is therefore NON-FLAKINESS — the gate
//! must catch a CATASTROPHIC throughput regression (an algorithmic blow-up, a
//! debug-build slip, lock contention) while NEVER spuriously failing on a normal
//! or slower-than-baseline machine.
//!
//! How it stays robust:
//!
//! MEASURE in a release subprocess. The `aterm-bench` `perf_harness` example feeds
//! a deterministic ~32 MiB representative VT workload (plain text + SGR + CSI +
//! scrolling) through `Terminal::process` and prints a single JSON line of
//! throughput. We spawn it with `cargo run --release` so timing is the shipped
//! build, never this debug-built xtask.
//!
//! MEDIAN-OF-N + WARMUP. The harness discards warmup iters, then takes the MEDIAN
//! of N>=5 timed iters — one scheduler hiccup cannot move the median.
//!
//! GENEROUS RATIO. The gate FAILS only when `median < baseline * RATIO`.
//! [`PASS_RATIO`] is 0.45: a machine running at 45% of the baseline box still
//! passes, tolerating ~2.2x slowdown from a slower core or thermal throttle. That
//! is far wider than real machine variance yet still trips on the kind of 10x+
//! collapse a debug build or O(n^2) parser would cause. See [`PASS_RATIO`].
//!
//! NEVER BLOCK A FRESH CHECKOUT. With no baseline file present the gate REPORTS the
//! measured throughput and PASSES. The strict comparison is engaged only when a
//! committed baseline exists.

use std::path::Path;
use std::process::Command;

use crate::workspace_root;

/// The pass threshold as a fraction of the recorded baseline median. The gate
/// fails iff `measured_median < baseline_median * PASS_RATIO`.
///
/// WHY 0.45: the baseline is recorded on one machine (m3) and checked on others
/// (m7, throttled laptops). A factor of 0.45 means a box can run at 45% of the
/// baseline's speed — i.e. be ~2.2x slower — and still pass. Measured m3-vs-m7
/// and throttled-vs-cool spreads sit comfortably inside ~1.5x, so 0.45 has a wide
/// margin against false positives while still catching a CATASTROPHIC regression:
/// a debug-build slip or an algorithmic blow-up costs 5x-50x, dropping the ratio
/// far below 0.45. The deterministic allocation gates (mem_budget, perf_scaling)
/// remain the precise, zero-flake guards; this is the coarse wall-clock floor.
pub(crate) const PASS_RATIO: f64 = 0.45;

/// Parsed throughput report emitted (as one JSON line on stdout) by the
/// `aterm-bench` `perf_harness` example, and the shape persisted to the golden
/// baseline. Field-for-field identical so a recorded baseline round-trips.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PerfReport {
    pub median_mbps: f64,
    pub min_mbps: f64,
    pub max_mbps: f64,
    pub workload_bytes: u64,
    pub n: u64,
    pub warmup: u64,
}

/// The verdict of comparing a fresh measurement against a baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No baseline on disk — report the number, never block (fresh checkout).
    NoBaseline,
    /// `measured >= baseline * PASS_RATIO`.
    Pass,
    /// `measured < baseline * PASS_RATIO` — a catastrophic regression.
    Fail,
}

/// Pure threshold comparison (unit-tested). `baseline` is the recorded median MB/s;
/// `measured` the fresh median MB/s. Returns the minimum MB/s that would pass too,
/// so callers can print the floor. A non-finite or non-positive baseline is treated
/// as "no usable baseline" -> [`Verdict::NoBaseline`] (never blocks).
pub(crate) fn compare(baseline: f64, measured: f64, ratio: f64) -> (Verdict, f64) {
    if !baseline.is_finite() || baseline <= 0.0 {
        return (Verdict::NoBaseline, 0.0);
    }
    let floor = baseline * ratio;
    // `>=` so a measurement EXACTLY at the floor passes (boundary is inclusive).
    if measured >= floor {
        (Verdict::Pass, floor)
    } else {
        (Verdict::Fail, floor)
    }
}

/// Extract a numeric field from the harness's flat JSON object. The harness emits
/// a known, flat shape (`{"k":v,...}`) so a dependency-free scan suffices: find
/// `"key"`, skip to the `:`, then parse the run of number characters. Returns
/// `None` if the key is absent or the value isn't a finite number.
fn json_number(src: &str, key: &str) -> Option<f64> {
    let needle = format!("\"{key}\"");
    let start = src.find(&needle)? + needle.len();
    let after_colon = src[start..].find(':')? + start + 1;
    let tail = src[after_colon..].trim_start();
    let end = tail
        .find(|c: char| {
            !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')
        })
        .unwrap_or(tail.len());
    let num: f64 = tail[..end].parse().ok()?;
    num.is_finite().then_some(num)
}

/// Parse the harness's JSON line into a [`PerfReport`]. Pure (unit-tested).
pub(crate) fn parse_report(json: &str) -> Option<PerfReport> {
    Some(PerfReport {
        median_mbps: json_number(json, "median_mbps")?,
        min_mbps: json_number(json, "min_mbps")?,
        max_mbps: json_number(json, "max_mbps")?,
        workload_bytes: json_number(json, "workload_bytes")? as u64,
        n: json_number(json, "n")? as u64,
        warmup: json_number(json, "warmup")? as u64,
    })
}

/// Render a [`PerfReport`] as the committed baseline JSON (pretty, with metadata).
/// Hand-rolled so xtask gains no serde dependency. `ratio` is recorded for humans;
/// the live gate uses [`PASS_RATIO`] (the source of truth), not this echoed copy.
pub(crate) fn baseline_json(r: &PerfReport, ratio: f64) -> String {
    format!(
        "{{\n  \"_comment\": \"aterm wall-clock throughput baseline (PERF-WALLCLOCK-BASELINE). Median-of-N MB/s of Terminal::process over a deterministic ~32 MiB mixed VT workload. Re-record with `ATERM_PERF_RECORD=1 cargo run -p xtask -- gate perf` (or `gate perf --record`). The gate fails only if measured median < median_mbps * pass_ratio; pass_ratio is generous to tolerate multi-machine/throttle variance.\",\n  \"median_mbps\": {:.3},\n  \"min_mbps\": {:.3},\n  \"max_mbps\": {:.3},\n  \"workload_bytes\": {},\n  \"n\": {},\n  \"warmup\": {},\n  \"pass_ratio\": {:.3}\n}}\n",
        r.median_mbps, r.min_mbps, r.max_mbps, r.workload_bytes, r.n, r.warmup, ratio,
    )
}

/// Path to the committed golden baseline.
pub(crate) fn baseline_path() -> std::path::PathBuf {
    workspace_root().join("tools/golden/perf-baseline.json")
}

/// Run the release `perf_harness` and parse its throughput report. The harness is
/// built+run via `cargo run --release` so the engine is the optimized build (a
/// debug build would itself read as a "regression" — which is, deliberately, what
/// we want the gate to catch if someone ships one).
pub(crate) fn measure() -> Result<PerfReport, String> {
    eprintln!("  $ cargo run --release -q -p aterm-bench --example perf_harness");
    let out = Command::new("cargo")
        .args([
            "run",
            "--release",
            "-q",
            "-p",
            "aterm-bench",
            "--example",
            "perf_harness",
        ])
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not spawn perf_harness: {e}"))?;
    // Surface the harness's human line (stderr) for the gate log.
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if line.contains("perf_harness:") {
            eprintln!("  {line}");
        }
    }
    if !out.status.success() {
        return Err(format!(
            "perf_harness exited {:?}\n{stderr}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.contains("median_mbps"))
        .ok_or_else(|| format!("perf_harness produced no JSON report:\n{stdout}"))?;
    parse_report(line).ok_or_else(|| format!("could not parse perf_harness JSON: {line}"))
}

// ---------------------------------------------------------------------------
// PATHOLOGICAL-BENCH lane (EXCEED plan Cluster G, closed by
// FASTER_THAN_GHOSTTY_PLAN §4): per-corpus hostile-input floors. Same
// philosophy as the mixed lane — deterministic corpora, release subprocess,
// median-of-N, the generous [`PASS_RATIO`] so it trips only on catastrophe —
// but ONE floor PER CORPUS, so an SGR-interning regression (style_churn) can't
// hide behind a healthy ASCII number.
// ---------------------------------------------------------------------------

/// The corpora the pathological harness emits, in emission order. MUST mirror
/// the name list in `aterm-bench/examples/pathological_harness.rs` — the gate
/// reads `<name>_median_mbps` for each and fails LOUDLY on a missing key, so a
/// one-sided rename cannot silently drop a floor.
pub(crate) const PATHOLOGICAL_CORPORA: [&str; 5] = [
    "yes_flood",
    "escape_storm",
    "style_churn",
    "long_escapes",
    "wide_unicode",
];

/// Path to the committed pathological baseline (kept separate from the mixed
/// baseline so re-recording one lane never perturbs the other).
pub(crate) fn pathological_baseline_path() -> std::path::PathBuf {
    workspace_root().join("tools/golden/perf-baseline-pathological.json")
}

/// Parse the harness/baseline flat JSON into per-corpus medians. `None` if ANY
/// corpus key is missing — a partial report must read as unparseable, never as
/// "the missing corpus passed".
pub(crate) fn parse_pathological(json: &str) -> Option<Vec<(&'static str, f64)>> {
    PATHOLOGICAL_CORPORA
        .iter()
        .map(|name| json_number(json, &format!("{name}_median_mbps")).map(|v| (*name, v)))
        .collect()
}

/// Render the committed pathological baseline JSON (hand-rolled, no serde).
pub(crate) fn pathological_baseline_json(medians: &[(&str, f64)], ratio: f64) -> String {
    let mut s = String::from(
        "{\n  \"_comment\": \"aterm PATHOLOGICAL-BENCH baseline: per-corpus median MB/s of \
         Terminal::process under hostile input (yes-flood / escape-storm / style-churn / \
         long-escapes / wide-unicode). Re-record with ATERM_PERF_RECORD=1 cargo run -p xtask -- \
         gate perf. Each corpus fails independently iff measured < recorded * pass_ratio.\",\n",
    );
    for (name, med) in medians {
        s.push_str(&format!("  \"{name}_median_mbps\": {med:.3},\n"));
    }
    s.push_str(&format!("  \"pass_ratio\": {ratio:.3}\n}}\n"));
    s
}

/// Run the release pathological harness and parse its per-corpus report.
fn measure_pathological() -> Result<Vec<(&'static str, f64)>, String> {
    eprintln!("  $ cargo run --release -q -p aterm-bench --example pathological_harness");
    let out = Command::new("cargo")
        .args([
            "run",
            "--release",
            "-q",
            "-p",
            "aterm-bench",
            "--example",
            "pathological_harness",
        ])
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not spawn pathological_harness: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if line.contains("pathological_harness:") {
            eprintln!("  {line}");
        }
    }
    if !out.status.success() {
        return Err(format!(
            "pathological_harness exited {:?}\n{stderr}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.contains("_median_mbps"))
        .ok_or_else(|| format!("pathological_harness produced no JSON report:\n{stdout}"))?;
    parse_pathological(line)
        .ok_or_else(|| format!("could not parse pathological_harness JSON: {line}"))
}

/// The pathological-floors sub-gate: every corpus median must clear its own
/// recorded floor. Mirrors [`gate_throughput`]'s contract exactly — records on
/// request, report-only PASS with no/malformed baseline, fails loudly naming
/// the offending corpus otherwise.
pub(crate) fn gate_pathological(trend: &mut Vec<TrendSample>) -> bool {
    let medians = match measure_pathological() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("  pathological: FAILED to measure — {e}");
            return false;
        }
    };
    for (name, med) in &medians {
        trend.push(TrendSample {
            lane: "pathological",
            metric: format!("{name}_median_mbps"),
            value: *med,
            inverted: false,
        });
    }
    let path = pathological_baseline_path();

    if record_requested() {
        let json = pathological_baseline_json(&medians, PASS_RATIO);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &json) {
            Ok(()) => {
                eprintln!("  pathological: RECORDED baseline -> {}", path.display());
                return true;
            }
            Err(e) => {
                eprintln!(
                    "  pathological: FAILED to write baseline {}: {e}",
                    path.display()
                );
                return false;
            }
        }
    }

    compare_pathological_against_baseline(&path, &medians)
}

/// Read + compare per corpus (split out so the decision is unit-testable
/// without spawning cargo, mirroring [`compare_against_baseline`]).
fn compare_pathological_against_baseline(path: &Path, medians: &[(&'static str, f64)]) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!(
            "  pathological: no baseline at {} — REPORT-ONLY, PASS (record with \
             ATERM_PERF_RECORD=1).",
            path.display()
        );
        for (name, med) in medians {
            eprintln!("    {name}: {med:.1} MB/s");
        }
        return true;
    };
    let Some(base) = parse_pathological(&text) else {
        eprintln!(
            "  pathological: baseline {} is unparseable — REPORT-ONLY, PASS.",
            path.display()
        );
        return true;
    };
    let mut ok = true;
    for ((name, med), (bname, bmed)) in medians.iter().zip(base.iter()) {
        debug_assert_eq!(name, bname, "corpus order is the shared const");
        let (verdict, floor) = compare(*bmed, *med, PASS_RATIO);
        match verdict {
            Verdict::NoBaseline | Verdict::Pass => {
                eprintln!("    {name}: GREEN — {med:.1} MB/s >= floor {floor:.1} MB/s");
            }
            Verdict::Fail => {
                eprintln!(
                    "    {name}: FAILED — {med:.1} MB/s < floor {floor:.1} MB/s \
                     (baseline {bmed:.1} x {PASS_RATIO:.2}); hostile-input regression in \
                     the {name} workload class."
                );
                ok = false;
            }
        }
    }
    ok
}

// ---------------------------------------------------------------------------
// ARENA-SCROLL lane (FASTER_THAN_GHOSTTY_PLAN §2 harness table / §4 SCROLL-1):
// scrollback-scrub read-path floors. Our tiered compressed scrollback pays tier
// decode on the interactive scrub path where ghostty's all-RAM PageList pays
// only pointer math — the dimension we are structurally most at risk of losing,
// and the one THRU-5's async-compression change must not regress. Engine-level
// and headless-capable (frame pacing is windowed-only, so the WINDOWED half of
// ARENA-SCROLL lives in tools/perf-arena/scroll.sh; this is the gate-enforced
// floor). Same non-flake contract as the other lanes; one floor PER PHASE.
// ---------------------------------------------------------------------------

/// The scrub phases and their exact JSON keys, in emission order. MUST mirror
/// the keys emitted by `aterm-bench/examples/scroll_scrub_harness.rs`. Every
/// metric is BIGGER-IS-BETTER (jump-to-top is reported as jumps/sec, not ms) so
/// the shared [`compare`] throughput contract applies unchanged. `parse_scroll`
/// fails LOUDLY on any missing key, so a one-sided rename cannot drop a floor.
pub(crate) const SCROLL_PHASES: [(&str, &str); 3] = [
    ("scrub", "scrub_median_rps"),
    ("pageup", "pageup_median_rps"),
    ("jump_top", "jump_top_median_jps"),
];

/// Path to the committed scroll baseline (separate file per lane).
pub(crate) fn scroll_baseline_path() -> std::path::PathBuf {
    workspace_root().join("tools/golden/perf-baseline-scroll.json")
}

/// Parse the harness/baseline flat JSON into per-phase medians. `None` if ANY
/// phase key is missing — a partial report must read as unparseable, never as
/// "the missing phase passed".
pub(crate) fn parse_scroll(json: &str) -> Option<Vec<(&'static str, f64)>> {
    SCROLL_PHASES
        .iter()
        .map(|(name, key)| json_number(json, key).map(|v| (*name, v)))
        .collect()
}

/// Render the committed scroll baseline JSON (hand-rolled, no serde).
pub(crate) fn scroll_baseline_json(medians: &[(&str, f64)], ratio: f64) -> String {
    let mut s = String::from(
        "{\n  \"_comment\": \"aterm ARENA-SCROLL baseline: scrollback-scrub read-path rates \
         (rows materialized/sec for wheel-scrub + page-sweep, jumps/sec for jump-to-top) over a \
         100k+-line tiered-scrollback fill. All BIGGER-IS-BETTER. Re-record with \
         ATERM_PERF_RECORD=1 cargo run -p xtask -- gate perf. Each phase fails independently iff \
         measured < recorded * pass_ratio.\",\n",
    );
    for (name, key) in SCROLL_PHASES {
        let med = medians
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0.0, |(_, v)| *v);
        s.push_str(&format!("  \"{key}\": {med:.3},\n"));
    }
    s.push_str(&format!("  \"pass_ratio\": {ratio:.3}\n}}\n"));
    s
}

/// Run the release scroll-scrub harness and parse its per-phase report.
fn measure_scroll() -> Result<Vec<(&'static str, f64)>, String> {
    eprintln!("  $ cargo run --release -q -p aterm-bench --example scroll_scrub_harness");
    let out = Command::new("cargo")
        .args([
            "run",
            "--release",
            "-q",
            "-p",
            "aterm-bench",
            "--example",
            "scroll_scrub_harness",
        ])
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not spawn scroll_scrub_harness: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if line.contains("scroll_scrub_harness:") {
            eprintln!("  {line}");
        }
    }
    if !out.status.success() {
        return Err(format!(
            "scroll_scrub_harness exited {:?}\n{stderr}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.contains("_median_"))
        .ok_or_else(|| format!("scroll_scrub_harness produced no JSON report:\n{stdout}"))?;
    parse_scroll(line).ok_or_else(|| format!("could not parse scroll_scrub_harness JSON: {line}"))
}

/// The scroll-scrub floors sub-gate: every phase rate must clear its own recorded
/// floor. Mirrors [`gate_pathological`]'s contract exactly — records on request,
/// report-only PASS with no/malformed baseline, fails loudly naming the phase.
pub(crate) fn gate_scroll_scrub(trend: &mut Vec<TrendSample>) -> bool {
    let medians = match measure_scroll() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("  scroll-scrub: FAILED to measure — {e}");
            return false;
        }
    };
    for ((name, med), (bname, key)) in medians.iter().zip(SCROLL_PHASES.iter()) {
        debug_assert_eq!(name, bname, "phase order is the shared const");
        trend.push(TrendSample {
            lane: "scroll",
            metric: (*key).to_string(),
            value: *med,
            inverted: false,
        });
    }
    let path = scroll_baseline_path();

    if record_requested() {
        let json = scroll_baseline_json(&medians, PASS_RATIO);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &json) {
            Ok(()) => {
                eprintln!("  scroll-scrub: RECORDED baseline -> {}", path.display());
                return true;
            }
            Err(e) => {
                eprintln!(
                    "  scroll-scrub: FAILED to write baseline {}: {e}",
                    path.display()
                );
                return false;
            }
        }
    }

    compare_scroll_against_baseline(&path, &medians)
}

/// Read + compare per phase (split out so the decision is unit-testable without
/// spawning cargo, mirroring [`compare_pathological_against_baseline`]).
fn compare_scroll_against_baseline(path: &Path, medians: &[(&'static str, f64)]) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!(
            "  scroll-scrub: no baseline at {} — REPORT-ONLY, PASS (record with \
             ATERM_PERF_RECORD=1).",
            path.display()
        );
        for (name, med) in medians {
            eprintln!("    {name}: {med:.0}/s");
        }
        return true;
    };
    let Some(base) = parse_scroll(&text) else {
        eprintln!(
            "  scroll-scrub: baseline {} is unparseable — REPORT-ONLY, PASS.",
            path.display()
        );
        return true;
    };
    let mut ok = true;
    for ((name, med), (bname, bmed)) in medians.iter().zip(base.iter()) {
        debug_assert_eq!(name, bname, "phase order is the shared const");
        let (verdict, floor) = compare(*bmed, *med, PASS_RATIO);
        match verdict {
            Verdict::NoBaseline | Verdict::Pass => {
                eprintln!("    {name}: GREEN — {med:.0}/s >= floor {floor:.0}/s");
            }
            Verdict::Fail => {
                eprintln!(
                    "    {name}: FAILED — {med:.0}/s < floor {floor:.0}/s \
                     (baseline {bmed:.0} x {PASS_RATIO:.2}); scrollback-scrub read-path \
                     regression in the {name} phase."
                );
                ok = false;
            }
        }
    }
    ok
}

/// Should the gate (re)write the baseline this run? Either `ATERM_PERF_RECORD` is
/// set to a truthy value, or `--record` appears anywhere on the argv.
pub(crate) fn record_requested() -> bool {
    let env_truthy = std::env::var("ATERM_PERF_RECORD")
        .map(|v| {
            let v = v.trim();
            !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
        })
        .unwrap_or(false);
    env_truthy || std::env::args().any(|a| a == "--record")
}

/// The wall-clock throughput sub-gate. Returns `true` (PASS) on success, including
/// the "no baseline / record / fresh checkout" cases that must NEVER block.
/// Measured medians are pushed into `trend` for the same-box ledger.
pub(crate) fn gate_throughput(trend: &mut Vec<TrendSample>) -> bool {
    let report = match measure() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  throughput: FAILED to measure — {e}");
            return false;
        }
    };
    trend.push(TrendSample {
        lane: "throughput",
        metric: "median_mbps".to_string(),
        value: report.median_mbps,
        inverted: false,
    });

    let path = baseline_path();

    if record_requested() {
        let json = baseline_json(&report, PASS_RATIO);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &json) {
            Ok(()) => {
                eprintln!(
                    "  throughput: RECORDED baseline {:.1} MB/s -> {}",
                    report.median_mbps,
                    path.display()
                );
                return true;
            }
            Err(e) => {
                eprintln!(
                    "  throughput: FAILED to write baseline {}: {e}",
                    path.display()
                );
                return false;
            }
        }
    }

    compare_against_baseline(&path, &report)
}

/// Read the baseline at `path` (if any) and apply the threshold. Split out so the
/// pure decision (read -> parse -> [`compare`]) is exercised without spawning cargo.
fn compare_against_baseline(path: &Path, report: &PerfReport) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!(
            "  throughput: no baseline at {} — REPORT-ONLY: {:.1} MB/s (median of {}). \
             PASS (a fresh checkout is never blocked; record with ATERM_PERF_RECORD=1).",
            path.display(),
            report.median_mbps,
            report.n,
        );
        return true;
    };
    let Some(base) = parse_report(&text) else {
        // A malformed baseline must not silently block; report and pass.
        eprintln!(
            "  throughput: baseline {} is unparseable — REPORT-ONLY: {:.1} MB/s. PASS.",
            path.display(),
            report.median_mbps,
        );
        return true;
    };

    let (verdict, floor) = compare(base.median_mbps, report.median_mbps, PASS_RATIO);
    match verdict {
        Verdict::NoBaseline => {
            eprintln!(
                "  throughput: baseline median non-positive — REPORT-ONLY: {:.1} MB/s. PASS.",
                report.median_mbps
            );
            true
        }
        Verdict::Pass => {
            eprintln!(
                "  throughput: GREEN — {:.1} MB/s >= floor {:.1} MB/s (baseline {:.1} MB/s x {:.2}).",
                report.median_mbps, floor, base.median_mbps, PASS_RATIO,
            );
            true
        }
        Verdict::Fail => {
            eprintln!(
                "  throughput: FAILED — {:.1} MB/s < floor {:.1} MB/s (baseline {:.1} MB/s x {:.2}). \
                 A catastrophic throughput regression (debug build? algorithmic blow-up? lock contention?).",
                report.median_mbps, floor, base.median_mbps, PASS_RATIO,
            );
            false
        }
    }
}

// ---------------------------------------------------------------------------
// E0 KEYED-FLOOR lanes (search / restore / resize / wasm): one shared engine
// for "harness emits flat JSON, every listed key gets its own floor" — the
// same contract as the pathological/scroll lanes, generalized so the four new
// lanes don't quadruplicate the record/compare plumbing. Keys are all
// BIGGER-IS-BETTER ([`compare`] applies unchanged); a missing key makes the
// whole report unparseable (never "the missing metric passed").
// ---------------------------------------------------------------------------

/// A keyed floor lane: which harness to run, which JSON keys are gated, where
/// the committed baseline lives.
pub(crate) struct FloorLane {
    /// Lane name for gate output + the trend ledger.
    pub lane: &'static str,
    /// The `aterm-bench` example to `cargo run --release` (measurement source).
    pub example: &'static str,
    /// Gated flat-JSON keys (bigger-is-better). Extra keys in the harness
    /// output are informational and ignored here.
    pub keys: &'static [&'static str],
    /// Baseline filename under `tools/golden/`.
    pub baseline_file: &'static str,
    /// `_comment` written into the recorded baseline.
    pub comment: &'static str,
}

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
    comment: "aterm SEARCH-BENCH baseline (E0): full-rebuild klines/s, cached-query q/s, and \
              retained-index lines-per-MiB on the trigram-diverse (rotating), repetitive-log \
              (replog), and hyperlink-heavy (linkheavy, Wave-4A P7) corpora, plus the \
              incremental index_scrollback_line primitive. All BIGGER-IS-BETTER. Re-record \
              with ATERM_PERF_RECORD=1 cargo run -p xtask -- gate perf.",
};

pub(crate) const RESTORE_LANE: FloorLane = FloorLane {
    lane: "restore",
    example: "restore_harness",
    keys: &["restore_median_hz"],
    baseline_file: "perf-baseline-restore.json",
    comment: "aterm RESTORE-BENCH baseline (E0): serialize->fresh-engine replay rate over a \
              10k-line SGR-mixed snapshot (the product's cold-restore path). BIGGER-IS-BETTER. \
              Re-record with ATERM_PERF_RECORD=1 cargo run -p xtask -- gate perf.",
};

pub(crate) const RESIZE_LANE: FloorLane = FloorLane {
    lane: "resize",
    example: "resize_rewrap_harness",
    keys: &["resize_ring_median_rps", "resize_tiered_median_rps"],
    baseline_file: "perf-baseline-resize.json",
    comment: "aterm RESIZE/REWRAP baseline (E0, audit 5.3): synchronous full-ring resizes/s at \
              the 50k cap and offload+pump+reattach cycles/s over a 110k-line tiered fill. \
              BIGGER-IS-BETTER; the 42s-freeze-class ABSOLUTE fences live in the gate code \
              (RESIZE_RING_WORST_CAP_MS / RESIZE_TIERED_SYNC_WORST_CAP_MS), not this file. \
              Re-record with ATERM_PERF_RECORD=1 cargo run -p xtask -- gate perf.",
};

pub(crate) const WASM_LANE: FloorLane = FloorLane {
    lane: "wasm",
    example: "", // measured via tools/wasm-bench/run.sh, not an aterm-bench example
    keys: &[
        "wasm_cpu_ingest_mixed_mbps",
        "wasm_cpu_ingest_replog_mbps",
        "wasm_cpu_scroll_present_fps",
        "wasm_cpu_typing_present_fps",
        "wasm_cpu_search_build_klps",
        "wasm_cpu_search_query_qps",
        "wasm_cpu_restore_hz",
        "wasm_gpu_ingest_mixed_mbps",
        "wasm_gpu_frame_build_fps",
        "wasm_gpu_scroll_frame_build_fps",
    ],
    baseline_file: "perf-baseline-wasm.json",
    comment: "aterm WASM-BENCH baseline (E0): the SHIPPED wasm modules (CPU aterm-wasm + GPU \
              aterm-gpu-web) driven under node by tools/wasm-bench — ingest, scroll/typing \
              present, search build/query, restore, GPU wasm-side frame build. All \
              BIGGER-IS-BETTER. Re-record with ATERM_PERF_RECORD=1 cargo run -p xtask -- gate \
              perf (needs node + a wasm32-capable stable toolchain).",
};

/// The 42s-freeze-class ABSOLUTE fences (audit §5.3). Unlike the ratio floors
/// these hold on a FRESH CHECKOUT with no baseline: a synchronous O(history)
/// rewrap regression costs seconds-to-minutes, far past either cap, while the
/// measured normal costs (73 ms ring / 18.6 ms tiered-sync worst on an M5
/// Max; a slow box is ~2-3x) sit far below — catastrophic-only, never flaky.
///
/// The tiered cap is deliberately TIGHT (~5x the measured worst, not ~50x):
/// the invariant it pins is that the offloading resize's synchronous phase is
/// VIEWPORT-BOUNDED — O(rows x cols), never O(history). Any O(history) work
/// leaking back into the sync phase at the harness's 110k-line depth costs
/// hundreds of ms before it costs seconds, and a 100 ms cap catches it at the
/// first step while staying ~5x above a slow box's honest viewport cost.
pub(crate) const RESIZE_RING_WORST_CAP_MS: f64 = 5_000.0;
pub(crate) const RESIZE_TIERED_SYNC_WORST_CAP_MS: f64 = 100.0;

/// Parse a lane's flat JSON into `(key, value)` pairs. `None` if ANY gated key
/// is missing — a partial report must read as unparseable.
pub(crate) fn parse_keyed(lane: &FloorLane, json: &str) -> Option<Vec<(&'static str, f64)>> {
    lane.keys
        .iter()
        .map(|key| json_number(json, key).map(|v| (*key, v)))
        .collect()
}

/// Render a keyed lane's committed baseline JSON (hand-rolled, no serde).
pub(crate) fn keyed_baseline_json(lane: &FloorLane, values: &[(&str, f64)], ratio: f64) -> String {
    let mut s = format!("{{\n  \"_comment\": \"{}\",\n", lane.comment);
    for (key, v) in values {
        s.push_str(&format!("  \"{key}\": {v:.3},\n"));
    }
    s.push_str(&format!("  \"pass_ratio\": {ratio:.3}\n}}\n"));
    s
}

fn keyed_baseline_path(lane: &FloorLane) -> std::path::PathBuf {
    workspace_root()
        .join("tools/golden")
        .join(lane.baseline_file)
}

/// Run a lane's `aterm-bench` example and return its raw JSON line.
fn measure_example_json(example: &str) -> Result<String, String> {
    eprintln!("  $ cargo run --release -q -p aterm-bench --example {example}");
    let out = Command::new("cargo")
        .args([
            "run",
            "--release",
            "-q",
            "-p",
            "aterm-bench",
            "--example",
            example,
        ])
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .map(str::to_string)
        .ok_or_else(|| format!("{example} produced no JSON report:\n{stdout}"))
}

/// Record-or-compare a keyed lane, given its measured raw JSON. Shared by all
/// four lanes; pushes gated metrics into `trend`. Same contract as the older
/// lanes: record on request, report-only PASS with no/malformed baseline,
/// fail loudly naming the metric otherwise.
fn judge_keyed(lane: &FloorLane, json: &str, trend: &mut Vec<TrendSample>) -> bool {
    let Some(values) = parse_keyed(lane, json) else {
        eprintln!(
            "  {}: harness JSON is missing a gated key — refusing to judge: {json}",
            lane.lane
        );
        return false;
    };
    for (key, v) in &values {
        trend.push(TrendSample {
            lane: lane.lane,
            metric: (*key).to_string(),
            value: *v,
            inverted: false,
        });
    }
    let path = keyed_baseline_path(lane);
    if record_requested() {
        let jsonout = keyed_baseline_json(lane, &values, PASS_RATIO);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &jsonout) {
            Ok(()) => {
                eprintln!("  {}: RECORDED baseline -> {}", lane.lane, path.display());
                return true;
            }
            Err(e) => {
                eprintln!(
                    "  {}: FAILED to write baseline {}: {e}",
                    lane.lane,
                    path.display()
                );
                return false;
            }
        }
    }
    compare_keyed_against_baseline(lane, &path, &values)
}

/// Read + compare per key (split out so the decision is unit-testable without
/// spawning cargo, mirroring the older lanes).
fn compare_keyed_against_baseline(
    lane: &FloorLane,
    path: &Path,
    values: &[(&'static str, f64)],
) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!(
            "  {}: no baseline at {} — REPORT-ONLY, PASS (record with ATERM_PERF_RECORD=1).",
            lane.lane,
            path.display()
        );
        for (key, v) in values {
            eprintln!("    {key}: {v:.1}");
        }
        return true;
    };
    let Some(base) = parse_keyed(lane, &text) else {
        eprintln!(
            "  {}: baseline {} is unparseable — REPORT-ONLY, PASS.",
            lane.lane,
            path.display()
        );
        return true;
    };
    let mut ok = true;
    for ((key, v), (bkey, bv)) in values.iter().zip(base.iter()) {
        debug_assert_eq!(key, bkey, "key order is the lane const");
        let (verdict, floor) = compare(*bv, *v, PASS_RATIO);
        match verdict {
            Verdict::NoBaseline | Verdict::Pass => {
                eprintln!("    {key}: GREEN — {v:.1} >= floor {floor:.1}");
            }
            Verdict::Fail => {
                eprintln!(
                    "    {key}: FAILED — {v:.1} < floor {floor:.1} (baseline {bv:.1} x \
                     {PASS_RATIO:.2}); {} lane regression.",
                    lane.lane
                );
                ok = false;
            }
        }
    }
    ok
}

/// SEARCH floors (E0): build/query/memory on both corpus shapes + the
/// incremental primitive.
pub(crate) fn gate_search(trend: &mut Vec<TrendSample>) -> bool {
    match measure_example_json(SEARCH_LANE.example) {
        Ok(json) => judge_keyed(&SEARCH_LANE, &json, trend),
        Err(e) => {
            eprintln!("  search: FAILED to measure — {e}");
            false
        }
    }
}

/// RESTORE floor (E0): serialize->replay rate.
pub(crate) fn gate_restore(trend: &mut Vec<TrendSample>) -> bool {
    match measure_example_json(RESTORE_LANE.example) {
        Ok(json) => judge_keyed(&RESTORE_LANE, &json, trend),
        Err(e) => {
            eprintln!("  restore: FAILED to measure — {e}");
            false
        }
    }
}

/// RESIZE/REWRAP floors + the 42s-freeze-class ABSOLUTE fences (E0, audit
/// §5.3). The fences apply even with NO baseline — that is their point.
pub(crate) fn gate_resize(trend: &mut Vec<TrendSample>) -> bool {
    let json = match measure_example_json(RESIZE_LANE.example) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("  resize: FAILED to measure — {e}");
            return false;
        }
    };
    let mut ok = judge_keyed(&RESIZE_LANE, &json, trend);
    // The worst-ms fence values also feed the same-box trend ledger, INVERTED
    // (lower is better): the absolute caps are catastrophic-only, so a real
    // same-box latency creep (18 ms -> 60 ms, still under the cap) would
    // otherwise ship silently. Missing keys are already a fence FAIL below.
    for key in ["resize_ring_worst_ms", "resize_tiered_sync_worst_ms"] {
        if let Some(v) = json_number(&json, key) {
            trend.push(TrendSample {
                lane: RESIZE_LANE.lane,
                metric: key.to_string(),
                value: v,
                inverted: true,
            });
        }
    }
    ok &= resize_fences(&json);
    ok
}

/// The absolute worst-case-latency fences, split out for unit-testing.
pub(crate) fn resize_fences(json: &str) -> bool {
    let mut ok = true;
    for (key, cap) in [
        ("resize_ring_worst_ms", RESIZE_RING_WORST_CAP_MS),
        (
            "resize_tiered_sync_worst_ms",
            RESIZE_TIERED_SYNC_WORST_CAP_MS,
        ),
    ] {
        match json_number(json, key) {
            Some(v) if v <= cap => {
                eprintln!("    {key}: GREEN — {v:.1} ms <= absolute cap {cap:.0} ms");
            }
            Some(v) => {
                eprintln!(
                    "    {key}: FAILED — {v:.1} ms > ABSOLUTE cap {cap:.0} ms; the \
                     42s-freeze class (synchronous O(history) rewrap) is back."
                );
                ok = false;
            }
            None => {
                eprintln!(
                    "    {key}: FAILED — missing from the harness report (fence unverified)."
                );
                ok = false;
            }
        }
    }
    ok
}

/// WASM floors (E0): the shipped wasm modules under node. SKIP-with-notice
/// (pass) when the box lacks node or a wasm32-capable stable toolchain — a
/// notice is printed, never a silent false "ok" — but a harness FAILURE on an
/// equipped box fails the gate.
pub(crate) fn gate_wasm(trend: &mut Vec<TrendSample>) -> bool {
    let have = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !have("node", &["--version"]) {
        eprintln!(
            "  wasm: SKIP — node not found; the wasm lane needs node (notice, not a pass of the floors)."
        );
        return true;
    }
    let wasm_target = Command::new("rustup")
        .args(["target", "list", "--installed", "--toolchain", "stable"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown"))
        .unwrap_or(false);
    if !wasm_target {
        eprintln!(
            "  wasm: SKIP — no stable wasm32-unknown-unknown target (rustup target add \
             wasm32-unknown-unknown --toolchain stable); floors not evaluated on this box."
        );
        return true;
    }
    // The bench pipeline mirrors the SHIPPING wasm-opt -O3 pass (bench
    // honesty: pre-opt numbers are not the product's) and run.sh refuses to
    // proceed without it — so an unequipped box is a SKIP here, never a
    // harness failure and never a silent pre-opt measurement.
    if !have("wasm-opt", &["--version"]) {
        eprintln!(
            "  wasm: SKIP — wasm-opt (binaryen) not found; the wasm bench applies the \
             shipping wasm-opt -O3 pass and refuses pre-opt numbers (brew/apt install \
             binaryen); floors not evaluated on this box."
        );
        return true;
    }
    eprintln!("  $ tools/wasm-bench/run.sh");
    let script = workspace_root().join("tools/wasm-bench/run.sh");
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(workspace_root())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            eprintln!("  wasm: FAILED to spawn {}: {e}", script.display());
            return false;
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if line.contains("wasm-bench:") {
            eprintln!("  {line}");
        }
    }
    if !out.status.success() {
        eprintln!("  wasm: harness exited {:?}\n{stderr}", out.status.code());
        return false;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(json) = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
    else {
        eprintln!("  wasm: harness produced no JSON report:\n{stdout}");
        return false;
    };
    judge_keyed(&WASM_LANE, json, trend)
}

// ---------------------------------------------------------------------------
// SAME-BOX TREND LEDGER (E0, audit §5.6): the multi-machine floors must stay
// generous (0.45), so a genuine same-box 2x regression passes them. This
// ledger closes that: every green `gate perf` run appends its medians (keyed
// by hostname) to a committed TSV, and each metric must clear
// [`TREND_RATIO`] x the BEST of that box's last [`TREND_WINDOW`] entries —
// tight enough to catch a real 2x, wide enough for same-box run-to-run
// variance, and comparing against the recent BEST so slow drift cannot
// ratchet the floor down with it.
// ---------------------------------------------------------------------------

/// Same-box floor ratio. Engine-lane same-box variance measures well inside
/// 15%; 0.70 leaves 2x margin over that while a genuine 2x regression (0.50)
/// still trips.
pub(crate) const TREND_RATIO: f64 = 0.70;

/// How many most-recent same-box entries per metric form the reference (their
/// MAX is the floor's base).
pub(crate) const TREND_WINDOW: usize = 10;

/// One measured median headed for the ledger.
#[derive(Debug, Clone)]
pub(crate) struct TrendSample {
    pub lane: &'static str,
    pub metric: String,
    pub value: f64,
    /// LOWER is better (worst-latency metrics, e.g. the resize `*_worst_ms`
    /// fences): the same-box bound is best(MIN)/TREND_RATIO — a ceiling —
    /// instead of best(MAX)*TREND_RATIO — a floor.
    pub inverted: bool,
}

// ---------------------------------------------------------------------------
// MACHINE IDENTITY (W-1). The ledger is a SAME-BOX comparison, so every row has
// to say which box measured it. It used to say `hostname`, and that silently
// killed the only guard in this tree that can catch a same-box 2x regression:
// the machine was renamed m15 -> m21, the live hostname stopped matching any
// committed row, and `judge_trend` filtered 249 rows down to nothing. The gate
// went on printing SKIP lines that read as "first run on this box" while every
// win of a whole campaign landed unprotected.
//
// A hostname is a bad machine identity in BOTH directions, and BOTH have
// already happened to this ledger:
//   * it RENAMES — m15 -> m21 forked one box's history away from itself; and
//   * it is SPELLED more than one way on one box — the committed rows carry
//     `m15-Macbook-M5-Max-128GB-8TB.local` (29 rows) AND
//     `m15Macb128GB8TB.localdomain` (220 rows), which are one machine under
//     `.local` and `.localdomain`.
//
// WHAT REPLACES IT, and why not the two obvious alternatives:
//
//   REJECTED — CHIP + CORE COUNT (`Apple M5 Max` + 18). Stable across renames,
//   readable, no probe to write, and it SILENTLY MERGES two machines of the
//   same model. That is strictly worse than the dead gate it would replace: a
//   dead trend prints SKIP and everyone can see nothing was judged, a merged
//   trend prints GREEN while holding this box's number against a DIFFERENT
//   box's best. Refused on that ground alone.
//
//   REJECTED — THE RAW PLATFORM UUID as the key. Unique and rename-proof, but
//   it would land verbatim in a committed file in a published repo, and a
//   machine UUID is not ours to publish. It is also unreadable in a diff.
//
//   CHOSEN — A HARDWARE FINGERPRINT RESOLVED THROUGH AN EXPLICIT COMMITTED
//   ALIAS TABLE. The fingerprint is a DIGEST of the platform UUID (macOS
//   IOPlatformUUID, Linux /etc/machine-id, Windows MachineGuid): unique per
//   machine, untouched by a rename, and non-identifying once digested.
//   [`boxes_path`] is where a HUMAN writes down "these identities are that
//   box", so every merge of two identities is an explicit, reviewable claim in
//   a diff — never an accident of two machines looking alike. An unclaimed
//   machine still works: it keys itself `box-<digest>`, which collides with
//   nothing, and the gate prints the exact alias row to paste to give it a
//   name.
//
//   AND THE CLAIM ITSELF IS GUARDED. Every new row records the box's hardware
//   SHAPE (chip + logical cores). If rows that resolve to one box key disagree
//   about the CHIP, the alias table is claiming two different machines are one
//   machine, and the sub-gate REFUSES — it drops the disagreeing rows and goes
//   red — rather than averaging across them. A dead trend is bad; a trend that
//   looks alive while comparing two boxes is worse, and this is the line
//   between them.
// ---------------------------------------------------------------------------

pub(crate) fn trend_path() -> std::path::PathBuf {
    workspace_root().join("tools/golden/perf-trend.tsv")
}

/// The committed alias table: which raw machine identities are which box.
pub(crate) fn boxes_path() -> std::path::PathBuf {
    workspace_root().join("tools/golden/perf-boxes.tsv")
}

/// Who this machine is, as the ledger keys it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineId {
    /// The ledger's box column: a short name from the alias table, or a
    /// deterministic `box-<digest>` when nothing claims this machine.
    pub key: String,
    /// Chip + logical cores (`Apple-M5-Max-18c`). Recorded per row; its CHIP
    /// half is compared, and a disagreement is what turns a WRONG alias claim
    /// into a red gate instead of a silent cross-machine merge.
    pub shape: String,
    /// The raw tokens this machine answers to, strongest first: `fp:<digest>`
    /// when a platform UUID could be read, then `host:<hostname>`.
    pub identities: Vec<String>,
    /// An alias row explicitly claimed one of [`Self::identities`].
    pub claimed: bool,
}

/// FNV-1a 64. Dependency-free and deterministic, which is the whole
/// requirement: the digest only has to be STABLE for one machine and
/// non-reversible enough that a committed alias row does not publish the
/// platform UUID it came from. It is not a security primitive and nothing
/// downstream treats it as one — a forged fingerprint buys an attacker one
/// wrong perf-trend comparison on a machine they already own.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Alphanumerics kept, runs of anything else collapsed to one `-`, so a chip
/// string is safe in a TSV column and readable in a diff.
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.push(c);
        } else {
            pending_sep = true;
        }
    }
    out
}

/// The machine's platform UUID, or `None` where none can be read.
///
/// Runtime `cfg!` branches rather than `#[cfg]`-gated functions ON PURPOSE:
/// this file is inside `crates/**`, which the source censuses walk, and a
/// platform-gated CALL whose CALLEE is gated differently is exactly the
/// mismatch that reddens main. One function, one body, every probe compiled
/// everywhere; the ones that cannot apply simply fail to spawn.
fn platform_uuid() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let key = "\"IOPlatformUUID\"";
        let at = text.find(key)? + key.len();
        let tail = &text[at..];
        let open = tail.find('"')? + 1;
        let rest = &tail[open..];
        let close = rest.find('"')?;
        return Some(rest[..close].to_string());
    }
    if cfg!(target_os = "windows") {
        let out = Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        return text
            .lines()
            .find(|l| l.contains("MachineGuid"))
            .and_then(|l| l.split_whitespace().last())
            .map(str::to_string);
    }
    // Linux and the BSDs: the systemd/dbus machine id, whichever exists.
    for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(s) = std::fs::read_to_string(p) {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// The stable, non-identifying machine fingerprint — `None` when no platform
/// UUID is readable, in which case the hostname is all we have.
fn platform_fingerprint() -> Option<String> {
    let raw = platform_uuid()?;
    let t = raw.trim();
    (!t.is_empty()).then(|| format!("{:016x}", fnv1a64(t.as_bytes())))
}

fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// Chip + logical cores, e.g. `Apple-M5-Max-18c`. Recorded per row so a wrong
/// alias claim is DETECTABLE; see [`chip_of`] for what is actually compared.
fn machine_shape() -> String {
    let chip = if cfg!(target_os = "macos") {
        Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
    };
    let chip = chip.unwrap_or_else(|| std::env::consts::ARCH.to_string());
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
    format!("{}-{cores}c", slug(&chip))
}

/// The CHIP half of a shape — the shape with a trailing `-<n>c` core count
/// removed.
///
/// WHY ONLY THE CHIP IS COMPARED. The core count is the discriminating half but
/// it is also the FLAKY half: `available_parallelism` answers with the cgroup /
/// affinity view, so a run inside a constrained container would report a
/// different number on the same machine and a strict compare would go red for a
/// reason that has nothing to do with performance. The chip string does not
/// move. The core count is still RECORDED — it is in the ledger for a human
/// reading a surprising row — it just is not what fails the gate.
pub(crate) fn chip_of(shape: &str) -> &str {
    let Some(stripped) = shape.strip_suffix('c') else {
        return shape;
    };
    match stripped.rfind('-') {
        Some(dash)
            if dash + 1 < stripped.len()
                && stripped[dash + 1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            &shape[..dash]
        }
        _ => shape,
    }
}

/// The box an alias table gives `identity` (`fp:<digest>` / `host:<name>`), or
/// `None` when no row claims it. Pure (unit-tested).
pub(crate) fn alias_lookup(aliases: &str, identity: &str) -> Option<String> {
    aliases
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .find_map(|l| {
            let mut cols = l.split('\t');
            let key = cols.next()?.trim();
            let ident = cols.next()?.trim();
            (ident == identity && !key.is_empty()).then(|| key.to_string())
        })
}

/// Every box name the alias table declares. Pure (unit-tested).
pub(crate) fn alias_boxes(aliases: &str) -> Vec<String> {
    let mut v: Vec<String> = aliases
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let key = l.split('\t').next()?.trim();
            (!key.is_empty()).then(|| key.to_string())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// What a ledger row's box column means, resolved through the alias table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BoxRef {
    /// An alias row claims this token (as `host:<token>`/`fp:<token>`) or
    /// declares it as a box name outright.
    Claimed(String),
    /// Nothing in the alias table mentions it. It stands alone — it is NEVER
    /// merged into a neighbouring box on a guess.
    Unclaimed(String),
}

impl BoxRef {
    pub(crate) fn key(&self) -> &str {
        match self {
            BoxRef::Claimed(k) | BoxRef::Unclaimed(k) => k,
        }
    }
    pub(crate) fn is_claimed(&self) -> bool {
        matches!(self, BoxRef::Claimed(_))
    }
}

/// Resolve one ledger row's box column. Pure (unit-tested).
///
/// The column holds either a BOX NAME (rows written since W-1) or a RAW
/// HOSTNAME (every row written before it). Both resolve here, and an unknown
/// token resolves to ITSELF — never to whatever box looks closest — so the one
/// thing this function cannot do is quietly merge two machines.
pub(crate) fn resolve_ledger_box(aliases: &str, token: &str) -> BoxRef {
    if let Some(key) = alias_lookup(aliases, &format!("host:{token}")) {
        return BoxRef::Claimed(key);
    }
    if let Some(key) = alias_lookup(aliases, &format!("fp:{token}")) {
        return BoxRef::Claimed(key);
    }
    // `box-<digest>` is, BY CONSTRUCTION, the auto-key of `fp:<digest>` (see
    // [`default_box_key`]). Resolving it through the fingerprint is what lets a
    // machine be NAMED after it has already written rows: the operator adds one
    // `fp:` row and the auto-keyed history it already produced comes with it,
    // instead of being stranded under a name nobody will ever type again —
    // which is the same orphaning W-1 exists to end.
    if let Some(digest) = token.strip_prefix("box-")
        && let Some(key) = alias_lookup(aliases, &format!("fp:{digest}"))
    {
        return BoxRef::Claimed(key);
    }
    if alias_boxes(aliases).iter().any(|b| b == token) {
        return BoxRef::Claimed(token.to_string());
    }
    BoxRef::Unclaimed(token.to_string())
}

/// The key an UNCLAIMED machine gets. Deterministic and collision-free where a
/// fingerprint exists; falls back to the hostname (with its rename hazard
/// intact, which is the honest state of a box with no readable platform UUID)
/// where one does not. Pure (unit-tested).
pub(crate) fn default_box_key(identities: &[String]) -> String {
    for i in identities {
        if let Some(fp) = i.strip_prefix("fp:") {
            return format!("box-{fp}");
        }
    }
    for i in identities {
        if let Some(h) = i.strip_prefix("host:") {
            return format!("host-{}", slug(h));
        }
    }
    "unknown-box".to_string()
}

/// This machine, resolved against the committed alias table.
pub(crate) fn machine_id() -> MachineId {
    let aliases = std::fs::read_to_string(boxes_path()).unwrap_or_default();
    let mut identities = Vec::new();
    if let Some(fp) = platform_fingerprint() {
        identities.push(format!("fp:{fp}"));
    }
    identities.push(format!("host:{}", hostname()));
    let claimed = identities.iter().find_map(|i| alias_lookup(&aliases, i));
    let key = claimed
        .clone()
        .unwrap_or_else(|| default_box_key(&identities));
    MachineId {
        key,
        shape: machine_shape(),
        identities,
        claimed: claimed.is_some(),
    }
}

fn head_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(workspace_root())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// UTC date (YYYY-MM-DD) without a chrono dependency: civil-from-days on the
/// Unix epoch offset (Howard Hinnant's algorithm, exact for the Gregorian
/// calendar).
fn utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// One parsed ledger row. Rows have grown a column twice and the parser
/// accepts all three widths: 5 (pre-2026-07-22), 6 (+ toolchain), 7 (+ the
/// box's hardware shape, since W-1).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrendRow<'a> {
    pub token: &'a str,
    pub metric: &'a str,
    pub value: f64,
    /// `""` on every row written before the shape column existed.
    pub shape: &'a str,
}

/// Parse the ledger, skipping comments, blanks and unparseable rows. Pure
/// (unit-tested).
pub(crate) fn trend_ledger_rows(ledger: &str) -> Vec<TrendRow<'_>> {
    ledger
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            if cols.len() < 5 {
                return None;
            }
            Some(TrendRow {
                token: cols[2],
                metric: cols[3],
                value: cols[4].parse::<f64>().ok()?,
                shape: cols.get(6).copied().unwrap_or("").trim(),
            })
        })
        .collect()
}

/// The trend sub-gate's pure verdict over one run's samples.
pub(crate) struct TrendJudgment {
    /// Failing `(metric, value, bound, best, inverted)` rows — `bound` is a
    /// floor for bigger-is-better metrics and a CEILING for inverted
    /// (lower-is-better) ones.
    pub breaches: Vec<(String, f64, f64, f64, bool)>,
    /// Metrics with NO same-box history: first run on this box. Reported as
    /// SKIP — never counted into a GREEN claim (Wave-0 prescription e).
    pub skipped: Vec<String>,
    /// `(token, chip)` pairs that resolve to THIS box but were measured on a
    /// different CHIP. The alias table is claiming two machines are one; these
    /// rows are DROPPED from the reference window and the sub-gate goes red.
    /// Silence here would be the one outcome worse than the dead gate W-1
    /// fixed: a trend that looks alive while comparing across machines.
    pub conflicts: Vec<(String, String)>,
}

/// Judge `samples` against the same-box history in the ledger text, with
/// `aliases` deciding which rows are THIS box. Pure (unit-tested).
pub(crate) fn judge_trend(
    ledger: &str,
    aliases: &str,
    me: &MachineId,
    samples: &[TrendSample],
) -> TrendJudgment {
    let rows = trend_ledger_rows(ledger);
    let my_chip = chip_of(&me.shape);

    // MINE, and only mine. A row is this box's iff its token resolves to this
    // box's key; of those, a row whose recorded chip disagrees is a CONFLICT
    // and is excluded rather than compared against.
    let mut conflicts: Vec<(String, String)> = Vec::new();
    let mine: Vec<&TrendRow<'_>> = rows
        .iter()
        .filter(|r| resolve_ledger_box(aliases, r.token).key() == me.key)
        .filter(|r| {
            if r.shape.is_empty() || chip_of(r.shape) == my_chip {
                return true;
            }
            let entry = (r.token.to_string(), chip_of(r.shape).to_string());
            if !conflicts.contains(&entry) {
                conflicts.push(entry);
            }
            false
        })
        .collect();

    let mut breaches = Vec::new();
    let mut skipped = Vec::new();
    for s in samples {
        let metric_key = format!("{}/{}", s.lane, s.metric);
        let mut history: Vec<f64> = mine
            .iter()
            .filter(|r| r.metric == metric_key)
            .map(|r| r.value)
            .filter(|v| *v > 0.0)
            .collect();
        let recent = history.split_off(history.len().saturating_sub(TREND_WINDOW));
        if recent.is_empty() {
            // First run on this box for this metric: nothing to hold it to —
            // surfaced as SKIP, never silently folded into GREEN.
            skipped.push(metric_key);
            continue;
        }
        if s.inverted {
            // Lower is better: hold the value under best(MIN)/TREND_RATIO.
            let best = recent.iter().copied().fold(f64::INFINITY, f64::min);
            let ceiling = best / TREND_RATIO;
            if s.value > ceiling {
                breaches.push((metric_key, s.value, ceiling, best, true));
            }
        } else {
            let best = recent.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let floor = best * TREND_RATIO;
            if s.value < floor {
                breaches.push((metric_key, s.value, floor, best, false));
            }
        }
    }
    TrendJudgment {
        breaches,
        skipped,
        conflicts,
    }
}

/// Which toolchain builds a lane's measurement binary — recorded per ledger
/// row (Wave-0 prescription e) so cross-toolchain drift (a Trust-toolchain
/// bump vs an upstream-stable bump) is never misread as a same-box perf
/// change. The wasm lane rides upstream stable (Trust has no wasm32 std, see
/// tools/wasm-bench/run.sh); every native lane is the Trust toolchain.
fn toolchain_lane(lane: &str) -> &'static str {
    if lane == "wasm" { "stable" } else { "trust" }
}

/// Render `samples` as ledger rows (one metric per line, TSV). The trailing
/// toolchain column is absent from pre-2026-07-22 rows and the SHAPE column
/// from every row before W-1; the parser still accepts both.
pub(crate) fn trend_rows(
    date: &str,
    sha: &str,
    me: &MachineId,
    samples: &[TrendSample],
) -> String {
    let mut s = String::new();
    for sample in samples {
        s.push_str(&format!(
            "{date}\t{sha}\t{}\t{}/{}\t{:.3}\t{}\t{}\n",
            me.key,
            sample.lane,
            sample.metric,
            sample.value,
            toolchain_lane(sample.lane),
            me.shape
        ));
    }
    s
}

const TREND_HEADER: &str = "# aterm same-box perf trend ledger (E0, audit 5.6). Appended by every \
GREEN `gate perf` run;\n# each metric must clear TREND_RATIO x the best of this box's last \
TREND_WINDOW entries\n# (xtask/src/perf.rs; *_worst_ms metrics are INVERTED — bounded by \
best-MIN / TREND_RATIO).\n# The BOX column is a name from tools/golden/perf-boxes.tsv, which is \
also where a\n# pre-W-1 hostname is claimed as the box it was measured on. NEVER edit a box name \
here\n# to merge two machines — add the claim to perf-boxes.tsv, where the chip column keeps \
it\n# honest.\n# date\tsha\tbox\tlane/metric\tvalue\ttoolchain\tshape\n";

/// The trend sub-gate: compare this run's samples against the same-box
/// history, and append them when the run is healthy (`lanes_ok`, so a
/// regressed run cannot write itself into its own future reference window —
/// though the MAX-of-window reference resists that anyway). Never blocks a
/// fresh box or a fresh checkout.
pub(crate) fn gate_trend(samples: &[TrendSample], lanes_ok: bool) -> bool {
    let path = trend_path();
    let me = machine_id();
    let aliases = std::fs::read_to_string(boxes_path()).unwrap_or_default();
    let ledger = std::fs::read_to_string(&path).unwrap_or_default();
    let TrendJudgment {
        breaches,
        skipped,
        conflicts,
    } = judge_trend(&ledger, &aliases, &me, samples);
    let trend_ok = breaches.is_empty() && conflicts.is_empty();
    let bx = &me.key;

    // AN UNCLAIMED BOX STILL WORKS — it just has no name, and the operator is
    // told exactly how to give it one. Printing the paste-ready row is the
    // point: the alias table only stays honest if adding to it is easier than
    // hand-editing the box column of the ledger.
    if !me.claimed {
        eprintln!(
            "  trend: NOTE — this machine is not named in {}; its rows are keyed `{bx}`.\n         \
             Add:  {bx}\t{}\t<what this box is>",
            boxes_path().display(),
            me.identities.first().map_or("host:?", String::as_str)
        );
    }

    // ORPHANS IN THE COMMITTED LEDGER, surfaced where the operator already is.
    // The merge contract checks this too (see the tripwires in this file's
    // tests), but the gate is what APPENDS rows, so it is also where a wrong
    // key is cheapest to notice.
    let mut orphans: Vec<&str> = trend_ledger_rows(&ledger)
        .iter()
        .map(|r| r.token)
        .filter(|t| !resolve_ledger_box(&aliases, t).is_claimed())
        .collect();
    orphans.sort_unstable();
    orphans.dedup();
    for token in &orphans {
        eprintln!(
            "  trend: NOTE — ledger rows keyed `{token}` are claimed by no row of {}; \
             they anchor nothing until some box declares them.",
            boxes_path().display()
        );
    }

    // A CONFLICT IS A WRONG CLAIM, NOT A SLOW MACHINE. Loud, and red: an alias
    // row is asserting that two different machines are one box, and comparing
    // across them would make the trend look alive while it lied.
    for (token, chip) in &conflicts {
        eprintln!(
            "  trend: FAILED — ledger rows keyed `{token}` resolve to box `{bx}` but were \
             measured on chip `{chip}`, not `{}`. {} claims two different machines are one \
             box; those rows were DROPPED from the reference window rather than compared \
             against. Fix the claim — do not widen it.",
            chip_of(&me.shape),
            boxes_path().display()
        );
    }

    // No-history metrics are SKIP, not GREEN: a first run on a box verified
    // nothing, and saying otherwise is how unmeasured regressions get blessed.
    for metric in &skipped {
        eprintln!(
            "  trend: SKIP — {metric}: no same-box history on {bx} (first run; this \
             run only SEEDS the reference window, it verifies nothing)."
        );
    }
    let judged = samples.len() - skipped.len();
    if trend_ok && judged == 0 {
        eprintln!(
            "  trend: SKIP — no metric had same-box history on {bx}; nothing was \
             judged (seed run, not a pass)."
        );
    } else if trend_ok {
        eprintln!(
            "  trend: GREEN — {judged} metric(s) within same-box trend bounds of this \
             box's ({bx}) recent best ({} skipped, no history).",
            skipped.len()
        );
    } else {
        for (metric, value, bound, best, inverted) in &breaches {
            if *inverted {
                eprintln!(
                    "  trend: FAILED — {metric} {value:.1} > same-box ceiling {bound:.1} \
                     (best-of-last-{TREND_WINDOW} {best:.1} / {TREND_RATIO:.2} on {bx}; \
                     lower is better). A same-box latency creep the absolute cap would \
                     have let through."
                );
            } else {
                eprintln!(
                    "  trend: FAILED — {metric} {value:.1} < same-box floor {bound:.1} \
                     (best-of-last-{TREND_WINDOW} {best:.1} x {TREND_RATIO:.2} on {bx}). A \
                     same-box regression the generous multi-machine floor would have let through."
                );
            }
        }
    }
    // Append only healthy runs (or explicit record runs) so the committed
    // ledger stays a record of good states.
    if (lanes_ok && trend_ok) || record_requested() {
        let mut text = if ledger.is_empty() {
            TREND_HEADER.to_string()
        } else {
            ledger
        };
        text.push_str(&trend_rows(&utc_date(), &head_sha(), &me, samples));
        if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("  trend: could not append ledger {}: {e}", path.display());
        } else {
            eprintln!(
                "  trend: appended {} row(s) -> {}",
                samples.len(),
                path.display()
            );
        }
    }
    trend_ok
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn compare_passes_above_floor() {
        let (v, floor) = compare(1000.0, 500.0, 0.45);
        assert_eq!(v, Verdict::Pass);
        assert!((floor - 450.0).abs() < 1e-9);
    }

    #[test]
    fn compare_fails_below_floor() {
        let (v, floor) = compare(1000.0, 449.0, 0.45);
        assert_eq!(v, Verdict::Fail);
        assert!((floor - 450.0).abs() < 1e-9);
    }

    #[test]
    fn compare_boundary_is_inclusive_pass() {
        // EXACTLY at the floor must PASS (>= floor), never flake at the edge.
        let (v, _) = compare(1000.0, 450.0, 0.45);
        assert_eq!(v, Verdict::Pass);
        // A hair below fails.
        let (v2, _) = compare(1000.0, 449.999, 0.45);
        assert_eq!(v2, Verdict::Fail);
    }

    #[test]
    fn compare_catastrophic_regression_fails() {
        // A 10x collapse (debug build / O(n^2)) is far below any generous floor.
        let (v, _) = compare(3000.0, 300.0, 0.45);
        assert_eq!(v, Verdict::Fail);
    }

    #[test]
    fn compare_faster_machine_passes() {
        // A faster box (2x baseline) trivially passes.
        let (v, _) = compare(1000.0, 2000.0, 0.45);
        assert_eq!(v, Verdict::Pass);
    }

    #[test]
    fn compare_nonpositive_baseline_is_no_baseline() {
        assert_eq!(compare(0.0, 1000.0, 0.45).0, Verdict::NoBaseline);
        assert_eq!(compare(-5.0, 1000.0, 0.45).0, Verdict::NoBaseline);
        assert_eq!(compare(f64::NAN, 1000.0, 0.45).0, Verdict::NoBaseline);
    }

    #[test]
    fn parse_report_round_trips_through_baseline_json() {
        let r = report(1234.5);
        let json = baseline_json(&r, PASS_RATIO);
        let back = parse_report(&json).expect("parse the json we just wrote");
        assert!((back.median_mbps - r.median_mbps).abs() < 1e-3);
        assert!((back.min_mbps - r.min_mbps).abs() < 1e-3);
        assert!((back.max_mbps - r.max_mbps).abs() < 1e-3);
        assert_eq!(back.workload_bytes, r.workload_bytes);
        assert_eq!(back.n, r.n);
        assert_eq!(back.warmup, r.warmup);
    }

    #[test]
    fn parse_report_reads_harness_line_shape() {
        // The exact one-line shape the harness prints on stdout.
        let line = "{\"median_mbps\":3142.000,\"min_mbps\":3000.500,\"max_mbps\":3300.250,\"workload_bytes\":33554432,\"n\":7,\"warmup\":2}";
        let r = parse_report(line).expect("parse harness stdout");
        assert!((r.median_mbps - 3142.0).abs() < 1e-3);
        assert!((r.min_mbps - 3000.5).abs() < 1e-3);
        assert!((r.max_mbps - 3300.25).abs() < 1e-3);
        assert_eq!(r.workload_bytes, 33_554_432);
        assert_eq!(r.n, 7);
        assert_eq!(r.warmup, 2);
    }

    #[test]
    fn json_number_handles_negative_and_missing() {
        assert_eq!(json_number("{\"a\":-1.5}", "a"), Some(-1.5));
        assert_eq!(json_number("{\"a\":1.0}", "b"), None);
        assert_eq!(json_number("{\"a\":}", "a"), None);
    }

    #[test]
    fn parse_report_rejects_incomplete_json() {
        // Missing fields -> None (won't be mistaken for a valid baseline).
        assert!(parse_report("{\"median_mbps\":100.0}").is_none());
    }

    #[test]
    fn compare_against_missing_baseline_passes_report_only() {
        // A path that does not exist => report-only PASS (fresh checkout).
        let missing = workspace_root().join("tools/golden/__no_such_perf_baseline__.json");
        assert!(super::compare_against_baseline(&missing, &report(1234.5)));
    }

    fn pathological_line(scale: f64) -> String {
        let mut s = String::from("{");
        for name in PATHOLOGICAL_CORPORA {
            s.push_str(&format!("\"{name}_median_mbps\":{:.3},", 100.0 * scale));
        }
        s.push_str("\"corpus_bytes\":16777216,\"n\":7,\"warmup\":2}");
        s
    }

    #[test]
    fn parse_pathological_reads_harness_line_and_round_trips_baseline() {
        let parsed = parse_pathological(&pathological_line(1.0)).expect("full line parses");
        assert_eq!(parsed.len(), PATHOLOGICAL_CORPORA.len());
        assert!(parsed.iter().all(|(_, v)| (*v - 100.0).abs() < 1e-6));
        // The baseline writer's output parses back to the same medians.
        let json = pathological_baseline_json(&parsed, PASS_RATIO);
        let back = parse_pathological(&json).expect("baseline round-trips");
        assert_eq!(back, parsed);
    }

    #[test]
    fn parse_pathological_rejects_partial_reports() {
        // Any missing corpus => None: a dropped corpus must never read as a pass.
        let full = pathological_line(1.0);
        let missing_one = full.replace("\"style_churn_median_mbps\"", "\"renamed\"");
        assert!(parse_pathological(&missing_one).is_none());
    }

    #[test]
    fn pathological_compare_fails_only_the_collapsed_corpus() {
        // Baseline at 100 MB/s each; measurement fine except one 10x collapse.
        let dir = std::env::temp_dir().join(format!(
            "aterm-xtask-pathological-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("baseline.json");
        let base: Vec<(&'static str, f64)> =
            PATHOLOGICAL_CORPORA.iter().map(|n| (*n, 100.0)).collect();
        std::fs::write(&path, pathological_baseline_json(&base, PASS_RATIO)).unwrap();

        let healthy: Vec<(&'static str, f64)> =
            PATHOLOGICAL_CORPORA.iter().map(|n| (*n, 90.0)).collect();
        assert!(super::compare_pathological_against_baseline(
            &path, &healthy
        ));

        let mut collapsed = healthy.clone();
        collapsed[2].1 = 10.0; // style_churn at 10% of baseline: below the 0.45 floor
        assert!(!super::compare_pathological_against_baseline(
            &path, &collapsed
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pathological_missing_baseline_passes_report_only() {
        let missing = workspace_root().join("tools/golden/__no_such_pathological__.json");
        let m: Vec<(&'static str, f64)> = PATHOLOGICAL_CORPORA.iter().map(|n| (*n, 50.0)).collect();
        assert!(super::compare_pathological_against_baseline(&missing, &m));
    }

    fn scroll_line(scale: f64) -> String {
        let mut s = String::from("{");
        for (_, key) in SCROLL_PHASES {
            s.push_str(&format!("\"{key}\":{:.3},", 1000.0 * scale));
        }
        s.push_str("\"fill_lines\":120000,\"depth\":110000,\"n\":7,\"warmup\":2}");
        s
    }

    #[test]
    fn parse_scroll_reads_harness_line_and_round_trips_baseline() {
        let parsed = parse_scroll(&scroll_line(1.0)).expect("full line parses");
        assert_eq!(parsed.len(), SCROLL_PHASES.len());
        assert!(parsed.iter().all(|(_, v)| (*v - 1000.0).abs() < 1e-6));
        // The baseline writer's output parses back to the same medians.
        let json = scroll_baseline_json(&parsed, PASS_RATIO);
        let back = parse_scroll(&json).expect("baseline round-trips");
        assert_eq!(back, parsed);
    }

    #[test]
    fn parse_scroll_rejects_partial_reports() {
        // Any missing phase => None: a dropped phase must never read as a pass.
        let full = scroll_line(1.0);
        let missing_one = full.replace("\"pageup_median_rps\"", "\"renamed\"");
        assert!(parse_scroll(&missing_one).is_none());
    }

    #[test]
    fn scroll_compare_fails_only_the_collapsed_phase() {
        let dir =
            std::env::temp_dir().join(format!("aterm-xtask-scroll-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("baseline.json");
        let base: Vec<(&'static str, f64)> =
            SCROLL_PHASES.iter().map(|(n, _)| (*n, 1000.0)).collect();
        std::fs::write(&path, scroll_baseline_json(&base, PASS_RATIO)).unwrap();

        let healthy: Vec<(&'static str, f64)> =
            SCROLL_PHASES.iter().map(|(n, _)| (*n, 900.0)).collect();
        assert!(super::compare_scroll_against_baseline(&path, &healthy));

        let mut collapsed = healthy.clone();
        collapsed[1].1 = 100.0; // pageup at 10% of baseline: below the 0.45 floor
        assert!(!super::compare_scroll_against_baseline(&path, &collapsed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scroll_missing_baseline_passes_report_only() {
        let missing = workspace_root().join("tools/golden/__no_such_scroll__.json");
        let m: Vec<(&'static str, f64)> = SCROLL_PHASES.iter().map(|(n, _)| (*n, 500.0)).collect();
        assert!(super::compare_scroll_against_baseline(&missing, &m));
    }

    // --- E0 keyed lanes ----------------------------------------------------

    fn keyed_line(lane: &FloorLane, scale: f64) -> String {
        let mut s = String::from("{");
        for key in lane.keys {
            s.push_str(&format!("\"{key}\":{:.3},", 100.0 * scale));
        }
        s.push_str("\"n\":5,\"warmup\":1}");
        s
    }

    #[test]
    fn parse_keyed_reads_harness_line_and_round_trips_baseline() {
        for lane in [&SEARCH_LANE, &RESTORE_LANE, &RESIZE_LANE, &WASM_LANE] {
            let parsed = parse_keyed(lane, &keyed_line(lane, 1.0)).expect("full line parses");
            assert_eq!(parsed.len(), lane.keys.len());
            let json = keyed_baseline_json(lane, &parsed, PASS_RATIO);
            let back = parse_keyed(lane, &json).expect("baseline round-trips");
            assert_eq!(back, parsed);
        }
    }

    #[test]
    fn parse_keyed_rejects_partial_reports() {
        // Any missing gated key => None: a dropped metric must never pass.
        let full = keyed_line(&SEARCH_LANE, 1.0);
        let missing = full.replace("\"replog_query_qps\"", "\"renamed\"");
        assert!(parse_keyed(&SEARCH_LANE, &missing).is_none());
    }

    #[test]
    fn keyed_compare_fails_only_the_collapsed_metric() {
        let dir =
            std::env::temp_dir().join(format!("aterm-xtask-keyed-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("baseline.json");
        let base: Vec<(&'static str, f64)> = SEARCH_LANE.keys.iter().map(|k| (*k, 100.0)).collect();
        std::fs::write(&path, keyed_baseline_json(&SEARCH_LANE, &base, PASS_RATIO)).unwrap();

        let healthy: Vec<(&'static str, f64)> =
            SEARCH_LANE.keys.iter().map(|k| (*k, 90.0)).collect();
        assert!(super::compare_keyed_against_baseline(
            &SEARCH_LANE,
            &path,
            &healthy
        ));

        let mut collapsed = healthy.clone();
        collapsed[3].1 = 10.0; // one metric at 10% of baseline: below the 0.45 floor
        assert!(!super::compare_keyed_against_baseline(
            &SEARCH_LANE,
            &path,
            &collapsed
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyed_missing_baseline_passes_report_only() {
        let missing = workspace_root().join("tools/golden/__no_such_keyed__.json");
        let m: Vec<(&'static str, f64)> = RESTORE_LANE.keys.iter().map(|k| (*k, 50.0)).collect();
        assert!(super::compare_keyed_against_baseline(
            &RESTORE_LANE,
            &missing,
            &m
        ));
    }

    #[test]
    fn resize_fences_hold_absolutely() {
        // Under both caps: pass.
        assert!(resize_fences(
            "{\"resize_ring_worst_ms\":73.3,\"resize_tiered_sync_worst_ms\":15.0}"
        ));
        // The 42s class trips the ring cap even with no baseline anywhere.
        assert!(!resize_fences(
            "{\"resize_ring_worst_ms\":42000.0,\"resize_tiered_sync_worst_ms\":15.0}"
        ));
        // A synchronous tiered rewrap trips the sync cap.
        assert!(!resize_fences(
            "{\"resize_ring_worst_ms\":73.3,\"resize_tiered_sync_worst_ms\":2500.0}"
        ));
        // The TIGHTENED sync cap (100 ms, ~5x the 18.6 ms measured worst):
        // O(history) creep that the old 1 s cap waved through now trips.
        assert!(!resize_fences(
            "{\"resize_ring_worst_ms\":73.3,\"resize_tiered_sync_worst_ms\":150.0}"
        ));
        // A missing fence value is a FAIL, never an implicit pass.
        assert!(!resize_fences("{\"resize_ring_worst_ms\":73.3}"));
    }

    // --- same-box trend ledger ---------------------------------------------

    /// A synthetic box for the pure-judgment tests: a name, and one fixed chip
    /// so a row this helper writes and a row it judges agree.
    fn box_id(key: &str) -> MachineId {
        MachineId {
            key: key.to_string(),
            shape: "Test-Chip-8c".to_string(),
            identities: vec![format!("host:{key}")],
            claimed: true,
        }
    }

    /// [`judge_trend`] with no alias table — the shape every pre-W-1 trend test
    /// wants, where the ledger's box column IS the box.
    fn judged(ledger: &str, key: &str, samples: &[TrendSample]) -> TrendJudgment {
        judge_trend(ledger, "", &box_id(key), samples)
    }

    /// [`trend_rows`] for a synthetic box.
    fn rows_for(date: &str, sha: &str, key: &str, samples: &[TrendSample]) -> String {
        trend_rows(date, sha, &box_id(key), samples)
    }


    fn sample(lane: &'static str, metric: &str, value: f64) -> TrendSample {
        TrendSample {
            lane,
            metric: metric.to_string(),
            value,
            inverted: false,
        }
    }

    fn inverted_sample(lane: &'static str, metric: &str, value: f64) -> TrendSample {
        TrendSample {
            inverted: true,
            ..sample(lane, metric, value)
        }
    }

    #[test]
    fn trend_inverted_metric_uses_min_best_and_a_ceiling() {
        // Latency history 20, 18, 25 ms: the reference best is the MIN (18),
        // the bound a CEILING at 18/0.70 ≈ 25.7 ms.
        let mut ledger = String::new();
        for v in [20.0, 18.0, 25.0] {
            ledger.push_str(&rows_for(
                "2026-07-22",
                "abc",
                "boxA",
                &[inverted_sample("resize", "resize_tiered_sync_worst_ms", v)],
            ));
        }
        // 30 ms > ceiling: a same-box latency creep trips.
        let breaches = judged(
            &ledger,
            "boxA",
            &[inverted_sample(
                "resize",
                "resize_tiered_sync_worst_ms",
                30.0,
            )],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        assert!((breaches[0].2 - 18.0 / TREND_RATIO).abs() < 1e-9);
        assert!(breaches[0].4, "breach is flagged inverted");
        // 24 ms is inside the ceiling; getting FASTER (5 ms) never trips.
        for ok_v in [24.0, 5.0] {
            let ok = judged(
                &ledger,
                "boxA",
                &[inverted_sample(
                    "resize",
                    "resize_tiered_sync_worst_ms",
                    ok_v,
                )],
            );
            assert!(ok.breaches.is_empty(), "{ok_v} ms must pass");
        }
        // First run on a box: nothing to hold it to — SKIP, not a judged pass.
        let fresh = judged(
            &ledger,
            "boxB",
            &[inverted_sample(
                "resize",
                "resize_tiered_sync_worst_ms",
                30.0,
            )],
        );
        assert!(fresh.breaches.is_empty());
        assert_eq!(fresh.skipped, vec!["resize/resize_tiered_sync_worst_ms"]);
    }

    #[test]
    fn trend_first_run_on_a_box_never_blocks_and_reports_skip() {
        let j = judged("", "boxA", &[sample("throughput", "median_mbps", 100.0)]);
        assert!(j.breaches.is_empty());
        // …but it is a SKIP, not a judged metric (Wave-0 e: no history means
        // nothing was verified — the gate must not print GREEN for it).
        assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
    }

    #[test]
    fn trend_legacy_five_column_rows_still_count_as_history() {
        // Pre-2026-07-22 ledger rows have no trailing toolchain column; they
        // must keep anchoring the same-box window after the format change.
        let legacy = "2026-07-20\tabc\tboxA\tthroughput/median_mbps\t1000.000\n";
        let j = judged(
            legacy,
            "boxA",
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(j.breaches.len(), 1, "legacy history still judges");
        assert!(j.skipped.is_empty());
    }

    #[test]
    fn trend_same_box_regression_trips_and_other_boxes_do_not() {
        let ledger = rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("throughput", "median_mbps", 1000.0)],
        );
        // 40% of the same-box best: below the 0.70 floor.
        let breaches = judged(
            &ledger,
            "boxA",
            &[sample("throughput", "median_mbps", 400.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        assert!((breaches[0].2 - 700.0).abs() < 1e-9, "floor is best x 0.70");
        // A DIFFERENT box is not held to boxA's history.
        let cross = judged(
            &ledger,
            "boxB",
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(cross.breaches.is_empty());
        // Normal same-box variance passes.
        let ok = judged(
            &ledger,
            "boxA",
            &[sample("throughput", "median_mbps", 850.0)],
        );
        assert!(ok.breaches.is_empty());
    }

    #[test]
    fn trend_floor_is_best_of_window_so_drift_cannot_ratchet_it_down() {
        // History drifting downward: 1000, 900, 800 — the floor tracks the BEST
        // (1000), so a further sag to 650 (>0.70x of 800 but <0.70x of 1000) trips.
        let mut ledger = String::new();
        for v in [1000.0, 900.0, 800.0] {
            ledger.push_str(&rows_for(
                "2026-07-22",
                "abc",
                "boxA",
                &[sample("scroll", "scrub_median_rps", v)],
            ));
        }
        let breaches = judged(
            &ledger,
            "boxA",
            &[sample("scroll", "scrub_median_rps", 650.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        // But only the last TREND_WINDOW entries count: bury the 1000 past the
        // window and the reference becomes the recent best.
        let mut long = String::new();
        long.push_str(&rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("scroll", "scrub_median_rps", 1000.0)],
        ));
        for _ in 0..TREND_WINDOW {
            long.push_str(&rows_for(
                "2026-07-22",
                "abc",
                "boxA",
                &[sample("scroll", "scrub_median_rps", 800.0)],
            ));
        }
        let windowed = judged(
            &long,
            "boxA",
            &[sample("scroll", "scrub_median_rps", 650.0)],
        );
        assert!(
            windowed.breaches.is_empty(),
            "650 >= 0.70 x 800 (recent best), old 1000 aged out"
        );
    }

    #[test]
    fn trend_rows_round_trip_through_breach_parser() {
        let rows = rows_for(
            "2026-07-22",
            "abc123",
            "boxA",
            &[sample("search", "rotating_build_klps", 202.6)],
        );
        // Comment lines and the header are ignored by the parser.
        let ledger = format!("{TREND_HEADER}{rows}");
        let breaches = judged(
            &ledger,
            "boxA",
            &[sample("search", "rotating_build_klps", 100.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1, "100 < 0.70 x 202.6");
        let ok = judged(
            &ledger,
            "boxA",
            &[sample("search", "rotating_build_klps", 200.0)],
        );
        assert!(ok.breaches.is_empty());
        // Rows carry the toolchain lane: native lanes are the Trust toolchain.
        assert_eq!(
            rows.trim_end().split('\t').nth(5),
            Some("trust"),
            "row: {rows:?}"
        );
    }

    #[test]
    fn trend_rows_record_the_wasm_stable_toolchain_lane() {
        // The wasm lane's modules are built on upstream stable (Trust has no
        // wasm32 std) — its rows must say so, not claim the Trust toolchain.
        let rows = rows_for(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("wasm", "wasm_cpu_ingest_mixed_mbps", 343.0)],
        );
        assert_eq!(
            rows.trim_end().split('\t').nth(5),
            Some("stable"),
            "row: {rows:?}"
        );
    }


    // --- machine identity (W-1) --------------------------------------------

    const ALIASES: &str = "# box\tidentity\tnote\n\
         m21\tfp:deadbeefdeadbeef\tthe box\n\
         m21\thost:m21.local\tafter the rename\n\
         m21\thost:m15.local\tbefore the rename — SAME machine\n\
         m7\tfp:0123456789abcdef\ta different box\n";

    /// THE W-1 BUG, as a test. A rename used to fork a box's history away from
    /// itself; both spellings must now land on one key, and a machine the table
    /// never mentions must land on its own.
    #[test]
    fn a_rename_no_longer_forks_a_box_from_its_own_history() {
        assert_eq!(resolve_ledger_box(ALIASES, "m15.local").key(), "m21");
        assert_eq!(resolve_ledger_box(ALIASES, "m21.local").key(), "m21");
        // A box NAME in the column (every row written since W-1) resolves too.
        assert_eq!(resolve_ledger_box(ALIASES, "m21").key(), "m21");
        assert!(resolve_ledger_box(ALIASES, "m21").is_claimed());
        // …and the two hostnames really do reach the same history.
        let mut ledger = String::new();
        ledger.push_str("2026-07-22\tabc\tm15.local\tthroughput/median_mbps\t1000.000\n");
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let j = judge_trend(
            &ledger,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(
            j.breaches.len(),
            1,
            "the pre-rename rows must judge the post-rename run"
        );
        assert!(j.skipped.is_empty(), "history was found, so nothing skipped");
    }

    /// THE OTHER HALF, and the one that matters more: an identity nobody
    /// claimed is NEVER folded into a box that merely looks similar. A merged
    /// trend is worse than a dead one — it prints GREEN while comparing across
    /// machines.
    #[test]
    fn an_unclaimed_identity_stands_alone_and_is_never_merged() {
        let r = resolve_ledger_box(ALIASES, "some-other-mac.local");
        assert_eq!(r.key(), "some-other-mac.local");
        assert!(!r.is_claimed());
        // Its rows do not become m21's history.
        let ledger = "2026-07-22\tabc\tsome-other-mac.local\tthroughput/median_mbps\t1000.000\n";
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let j = judge_trend(
            ledger,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(j.breaches.is_empty());
        assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
    }

    /// A box that has already written AUTO-KEYED rows keeps them when a human
    /// finally names it — `box-<digest>` resolves through the `fp:` row.
    #[test]
    fn naming_a_box_adopts_the_rows_it_wrote_before_it_had_a_name() {
        assert_eq!(
            resolve_ledger_box(ALIASES, "box-deadbeefdeadbeef").key(),
            "m21"
        );
        assert!(resolve_ledger_box(ALIASES, "box-deadbeefdeadbeef").is_claimed());
        // An auto key for a fingerprint nobody claimed still stands alone.
        assert!(!resolve_ledger_box(ALIASES, "box-1111111111111111").is_claimed());
    }

    /// THE GUARD ON THE CLAIM. If rows under one box name disagree about the
    /// CHIP, the alias table is claiming two machines are one: those rows are
    /// dropped from the reference window AND the sub-gate goes red. It must
    /// never quietly average across them.
    #[test]
    fn a_chip_conflict_drops_the_rows_instead_of_comparing_across_machines() {
        let ledger = "2026-07-22\tabc\tm21\tthroughput/median_mbps\t9000.000\ttrust\tIntel-Core-i9-8c\n";
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let j = judge_trend(
            ledger,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(
            j.conflicts,
            vec![("m21".to_string(), "Intel-Core-i9".to_string())]
        );
        // The foreign row is NOT the reference — 400 is not judged against 9000.
        assert!(j.breaches.is_empty());
        assert_eq!(j.skipped, vec!["throughput/median_mbps"]);
        // A row on the SAME chip with a different core count is fine: the core
        // count is recorded, not enforced (cgroup/affinity views move it).
        let same = "2026-07-22\tabc\tm21\tthroughput/median_mbps\t1000.000\ttrust\tApple-M5-Max-10c\n";
        let ok = judge_trend(
            same,
            ALIASES,
            &me,
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(ok.conflicts.is_empty());
        assert_eq!(ok.breaches.len(), 1, "same chip ⇒ still same-box history");
    }

    #[test]
    fn chip_of_strips_only_a_trailing_core_count() {
        assert_eq!(chip_of("Apple-M5-Max-18c"), "Apple-M5-Max");
        assert_eq!(chip_of("Apple-M5-Max-1c"), "Apple-M5-Max");
        // Nothing that is not `-<digits>c` is stripped.
        assert_eq!(chip_of("Apple-M5-Max"), "Apple-M5-Max");
        assert_eq!(chip_of("weird-c"), "weird-c");
        assert_eq!(chip_of("weird-xc"), "weird-xc");
        assert_eq!(chip_of(""), "");
        assert_eq!(chip_of("c"), "c");
    }

    #[test]
    fn default_box_key_prefers_the_fingerprint() {
        assert_eq!(
            default_box_key(&["fp:abc".to_string(), "host:x.local".to_string()]),
            "box-abc"
        );
        // No fingerprint: the hostname, slugged — the rename hazard is still
        // there and that IS the honest state of a box with no platform UUID.
        assert_eq!(
            default_box_key(&["host:m21.local".to_string()]),
            "host-m21-local"
        );
        assert_eq!(default_box_key(&[]), "unknown-box");
    }

    #[test]
    fn a_seven_column_row_round_trips_with_its_shape() {
        let me = MachineId {
            key: "m21".to_string(),
            shape: "Apple-M5-Max-18c".to_string(),
            identities: vec!["fp:deadbeefdeadbeef".to_string()],
            claimed: true,
        };
        let rows = trend_rows(
            "2026-08-23",
            "abc123",
            &me,
            &[sample("throughput", "median_mbps", 500.0)],
        );
        let parsed = trend_ledger_rows(&rows);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].token, "m21");
        assert_eq!(parsed[0].shape, "Apple-M5-Max-18c");
        assert_eq!(parsed[0].metric, "throughput/median_mbps");
        // And the legacy widths still parse, with an EMPTY shape rather than a
        // guessed one.
        let five = "2026-07-20\tabc\tboxA\tthroughput/median_mbps\t1000.000\n";
        assert_eq!(trend_ledger_rows(five)[0].shape, "");
    }

    #[test]
    fn the_digest_is_stable_and_separates_two_uuids() {
        // Pinned: a fingerprint that moved would orphan the ledger exactly as
        // the rename did, and the committed alias rows would stop resolving.
        assert_eq!(
            format!("{:016x}", fnv1a64(b"4F2C6182-55E3-51B1-B505-42FD89F13166")),
            "4d119fc8de0a3b9e"
        );
        assert_ne!(fnv1a64(b"a"), fnv1a64(b"b"));
    }

    // --- THE COMMITTED LEDGER, checked in the merge contract (W-2) ----------
    //
    // These three read the REAL tools/golden files. They are pure string work
    // over two small committed TSVs — microseconds — so they ride `cargo test`
    // and therefore `tools/verify.sh --fast` at no marginal cost, which is the
    // only way anything about the perf ledger can be in the merge contract at
    // all (the MEASURING half of `gate perf` cannot: see the module docs on
    // `gate_trend`, and the notes in docs/PERF-REGRESSION-DEFENCE.md).
    //
    // Between them they would have caught W-1 the day it happened.

    fn committed_ledger() -> String {
        std::fs::read_to_string(trend_path()).unwrap_or_default()
    }

    fn committed_aliases() -> String {
        std::fs::read_to_string(boxes_path()).unwrap_or_default()
    }

    /// TRIPWIRE 1 — NO ORPHANED IDENTITY. A token in the ledger that no alias
    /// row claims, whose CHIP is one an already-claimed box uses, is the exact
    /// signature of a machine that renamed itself and started a second,
    /// disconnected history. Red, with the fix named.
    ///
    /// A genuinely NEW machine (a chip no claimed box has) is NOT red: it may
    /// contribute rows and be named later, which is the flow `gate_trend`
    /// prints. This is the difference between a guard and an obstacle.
    #[test]
    fn no_committed_ledger_identity_is_an_orphaned_rename() {
        let ledger = committed_ledger();
        let aliases = committed_aliases();
        let rows = trend_ledger_rows(&ledger);
        let claimed_chips: Vec<&str> = rows
            .iter()
            .filter(|r| resolve_ledger_box(&aliases, r.token).is_claimed())
            .map(|r| chip_of(r.shape))
            .filter(|c| !c.is_empty())
            .collect();
        for r in &rows {
            let bx = resolve_ledger_box(&aliases, r.token);
            if bx.is_claimed() {
                continue;
            }
            assert!(
                !r.shape.is_empty(),
                "ledger identity `{}` is claimed by no row of {} and carries no chip \
                 either, so nothing can tell whether it is a new machine or this one \
                 under a new name. Add it to the alias table.",
                r.token,
                boxes_path().display()
            );
            assert!(
                !claimed_chips.contains(&chip_of(r.shape)),
                "ledger identity `{}` is unclaimed but was measured on `{}` — the same \
                 chip as a box the alias table already knows. That is what a RENAME looks \
                 like, and it is how the trend ledger went dead for a month. Either add \
                 `<box>\\t{}\\t<why>` to {}, or say in that file why it is a different \
                 machine.",
                r.token,
                chip_of(r.shape),
                r.token,
                boxes_path().display()
            );
        }
    }

    /// TRIPWIRE 2 — NO CLAIM MERGES TWO MACHINES. One box name, one chip. This
    /// is the guard on the fix itself: the alias table's whole power is to
    /// declare two identities equal, and this is what stops that power being
    /// used to make a number look better.
    #[test]
    fn no_committed_box_name_covers_two_different_chips() {
        let ledger = committed_ledger();
        let aliases = committed_aliases();
        let mut seen: Vec<(String, String)> = Vec::new();
        for r in trend_ledger_rows(&ledger) {
            if r.shape.is_empty() {
                continue;
            }
            let key = resolve_ledger_box(&aliases, r.token).key().to_string();
            let chip = chip_of(r.shape).to_string();
            if let Some((_, prior)) = seen.iter().find(|(k, _)| *k == key) {
                assert_eq!(
                    prior,
                    &chip,
                    "box `{key}` carries rows from two different chips ({prior} and \
                     {chip}). {} is claiming two machines are one; a trend that compares \
                     across them looks alive and lies.",
                    boxes_path().display()
                );
            } else {
                seen.push((key, chip));
            }
        }
    }

    /// TRIPWIRE 3 — THIS BOX IS NOT ORPHANED FROM ITS OWN HISTORY. If the alias
    /// table claims THIS machine, the committed ledger must contain rows that
    /// resolve to it. This is the one that fires on the machine where the
    /// damage happens, and it is exactly what was silently false for a month.
    ///
    /// SELF-SKIPPING BY CONSTRUCTION: a machine no alias row claims has no
    /// history to be orphaned from, so it returns early rather than reddening
    /// somebody else's push. That is not a hole — the claim only exists because
    /// a human wrote it, and writing it is the moment this becomes checkable.
    ///
    /// AND A BOX THAT CLAIMS ONLY ITS OWN CURRENT IDENTITIES IS SIMPLY NEW.
    /// Orphaning means a box asserted a HISTORICAL name — a rename — and that
    /// assertion then failed to reach any row. A machine whose only claims are
    /// its fingerprint and the hostname it answers to right now has asserted no
    /// history, so zero rows is the honest state of a machine that has not run
    /// yet, not a broken claim. Reddening there would push the next author
    /// toward inventing an alias to silence it — which is exactly how two
    /// physical machines got merged under one box name here (see the
    /// `WHAT ACTUALLY HAPPENED HERE` note in perf-boxes.tsv).
    #[test]
    fn this_box_is_not_orphaned_from_its_own_committed_history() {
        let me = machine_id();
        if !me.claimed {
            return;
        }
        let aliases_text = committed_aliases();
        let current: std::collections::BTreeSet<&str> = me
            .identities
            .iter()
            .map(std::string::String::as_str)
            .collect();
        let claims_a_historical_name = aliases_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some((f.next()?.trim(), f.next()?.trim()))
            })
            .any(|(b, ident)| b == me.key && !current.contains(ident));
        if !claims_a_historical_name {
            return;
        }
        let ledger = committed_ledger();
        let aliases = committed_aliases();
        let mine = trend_ledger_rows(&ledger)
            .iter()
            .filter(|r| resolve_ledger_box(&aliases, r.token).key() == me.key)
            .count();
        assert!(
            mine > 0,
            "{} names this machine `{}` ({}), but not one of the {} committed ledger rows \
             resolves to it — the trend guard is DEAD on this box and every `gate perf` \
             run will print SKIP as though it were a fresh machine. That is precisely the \
             state a hostname rename left this ledger in.",
            boxes_path().display(),
            me.key,
            me.identities.join(" "),
            trend_ledger_rows(&ledger).len(),
        );
    }

    #[test]
    fn utc_date_is_iso_shaped() {
        let d = utc_date();
        assert_eq!(d.len(), 10);
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[7], b'-');
        assert!(d.starts_with("20"), "sane century: {d}");
    }
}
