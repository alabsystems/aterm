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

pub(crate) fn trend_path() -> std::path::PathBuf {
    workspace_root().join("tools/golden/perf-trend.tsv")
}

// TODO(trend-keying, deferred as >S effort — Wave-0 prescription e): key
// ledger history by HARDWARE UUID instead of hostname. Hostnames rename
// (silently forking a box's history) and collide (silently merging two boxes'
// histories into one bogus reference window). The fix needs a cross-platform
// probe (macOS: `ioreg -d2 -c IOPlatformExpertDevice` IOPlatformUUID; Linux:
// /etc/machine-id; Windows: MachineGuid) plus a migration story for the
// committed ledger's existing hostname-keyed rows — do it as one piece, not a
// macOS-only half.
fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
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

/// The trend sub-gate's pure verdict over one run's samples.
pub(crate) struct TrendJudgment {
    /// Failing `(metric, value, bound, best, inverted)` rows — `bound` is a
    /// floor for bigger-is-better metrics and a CEILING for inverted
    /// (lower-is-better) ones.
    pub breaches: Vec<(String, f64, f64, f64, bool)>,
    /// Metrics with NO same-box history: first run on this box. Reported as
    /// SKIP — never counted into a GREEN claim (Wave-0 prescription e).
    pub skipped: Vec<String>,
}

/// Judge `samples` against the same-box history in the ledger text. Pure
/// (unit-tested).
pub(crate) fn judge_trend(ledger: &str, host: &str, samples: &[TrendSample]) -> TrendJudgment {
    let mut breaches = Vec::new();
    let mut skipped = Vec::new();
    for s in samples {
        let metric_key = format!("{}/{}", s.lane, s.metric);
        let mut history: Vec<f64> = ledger
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .filter_map(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                // date, sha, host, lane/metric, value — older rows have exactly
                // these 5 columns; newer rows append a toolchain column.
                if cols.len() >= 5 && cols[2] == host && cols[3] == metric_key {
                    cols[4].parse::<f64>().ok()
                } else {
                    None
                }
            })
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
    TrendJudgment { breaches, skipped }
}

/// Which toolchain builds a lane's measurement binary — recorded per ledger
/// row (Wave-0 prescription e) so cross-toolchain drift (a Trust-toolchain
/// bump vs an upstream-stable bump) is never misread as a same-box perf
/// change. The wasm lane rides upstream stable (Trust has no wasm32 std, see
/// tools/wasm-bench/run.sh); every native lane is the Trust toolchain.
fn toolchain_lane(lane: &str) -> &'static str {
    if lane == "wasm" { "stable" } else { "trust" }
}

/// Render `samples` as ledger rows (one metric per line, TSV; the trailing
/// toolchain column is absent from pre-2026-07-22 rows, which the parser
/// still accepts).
pub(crate) fn trend_rows(date: &str, sha: &str, host: &str, samples: &[TrendSample]) -> String {
    let mut s = String::new();
    for sample in samples {
        s.push_str(&format!(
            "{date}\t{sha}\t{host}\t{}/{}\t{:.3}\t{}\n",
            sample.lane,
            sample.metric,
            sample.value,
            toolchain_lane(sample.lane)
        ));
    }
    s
}

const TREND_HEADER: &str = "# aterm same-box perf trend ledger (E0, audit 5.6). Appended by every \
GREEN `gate perf` run;\n# each metric must clear TREND_RATIO x the best of this box's last \
TREND_WINDOW entries\n# (xtask/src/perf.rs; *_worst_ms metrics are INVERTED — bounded by \
best-MIN / TREND_RATIO).\n# date\tsha\thost\tlane/metric\tvalue\ttoolchain\n";

/// The trend sub-gate: compare this run's samples against the same-box
/// history, and append them when the run is healthy (`lanes_ok`, so a
/// regressed run cannot write itself into its own future reference window —
/// though the MAX-of-window reference resists that anyway). Never blocks a
/// fresh box or a fresh checkout.
pub(crate) fn gate_trend(samples: &[TrendSample], lanes_ok: bool) -> bool {
    let path = trend_path();
    let host = hostname();
    let ledger = std::fs::read_to_string(&path).unwrap_or_default();
    let TrendJudgment { breaches, skipped } = judge_trend(&ledger, &host, samples);
    let trend_ok = breaches.is_empty();
    // No-history metrics are SKIP, not GREEN: a first run on a box verified
    // nothing, and saying otherwise is how unmeasured regressions get blessed.
    for metric in &skipped {
        eprintln!(
            "  trend: SKIP — {metric}: no same-box history on {host} (first run; this \
             run only SEEDS the reference window, it verifies nothing)."
        );
    }
    let judged = samples.len() - skipped.len();
    if trend_ok && judged == 0 {
        eprintln!(
            "  trend: SKIP — no metric had same-box history on {host}; nothing was \
             judged (seed run, not a pass)."
        );
    } else if trend_ok {
        eprintln!(
            "  trend: GREEN — {judged} metric(s) within same-box trend bounds of this \
             box's ({host}) recent best ({} skipped, no history).",
            skipped.len()
        );
    } else {
        for (metric, value, bound, best, inverted) in &breaches {
            if *inverted {
                eprintln!(
                    "  trend: FAILED — {metric} {value:.1} > same-box ceiling {bound:.1} \
                     (best-of-last-{TREND_WINDOW} {best:.1} / {TREND_RATIO:.2} on {host}; \
                     lower is better). A same-box latency creep the absolute cap would \
                     have let through."
                );
            } else {
                eprintln!(
                    "  trend: FAILED — {metric} {value:.1} < same-box floor {bound:.1} \
                     (best-of-last-{TREND_WINDOW} {best:.1} x {TREND_RATIO:.2} on {host}). A \
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
        text.push_str(&trend_rows(&utc_date(), &head_sha(), &host, samples));
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
            ledger.push_str(&trend_rows(
                "2026-07-22",
                "abc",
                "boxA",
                &[inverted_sample("resize", "resize_tiered_sync_worst_ms", v)],
            ));
        }
        // 30 ms > ceiling: a same-box latency creep trips.
        let breaches = judge_trend(
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
            let ok = judge_trend(
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
        let fresh = judge_trend(
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
        let j = judge_trend("", "boxA", &[sample("throughput", "median_mbps", 100.0)]);
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
        let j = judge_trend(
            legacy,
            "boxA",
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert_eq!(j.breaches.len(), 1, "legacy history still judges");
        assert!(j.skipped.is_empty());
    }

    #[test]
    fn trend_same_box_regression_trips_and_other_boxes_do_not() {
        let ledger = trend_rows(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("throughput", "median_mbps", 1000.0)],
        );
        // 40% of the same-box best: below the 0.70 floor.
        let breaches = judge_trend(
            &ledger,
            "boxA",
            &[sample("throughput", "median_mbps", 400.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        assert!((breaches[0].2 - 700.0).abs() < 1e-9, "floor is best x 0.70");
        // A DIFFERENT box is not held to boxA's history.
        let cross = judge_trend(
            &ledger,
            "boxB",
            &[sample("throughput", "median_mbps", 400.0)],
        );
        assert!(cross.breaches.is_empty());
        // Normal same-box variance passes.
        let ok = judge_trend(
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
            ledger.push_str(&trend_rows(
                "2026-07-22",
                "abc",
                "boxA",
                &[sample("scroll", "scrub_median_rps", v)],
            ));
        }
        let breaches = judge_trend(
            &ledger,
            "boxA",
            &[sample("scroll", "scrub_median_rps", 650.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1);
        // But only the last TREND_WINDOW entries count: bury the 1000 past the
        // window and the reference becomes the recent best.
        let mut long = String::new();
        long.push_str(&trend_rows(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("scroll", "scrub_median_rps", 1000.0)],
        ));
        for _ in 0..TREND_WINDOW {
            long.push_str(&trend_rows(
                "2026-07-22",
                "abc",
                "boxA",
                &[sample("scroll", "scrub_median_rps", 800.0)],
            ));
        }
        let windowed = judge_trend(
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
        let rows = trend_rows(
            "2026-07-22",
            "abc123",
            "boxA",
            &[sample("search", "rotating_build_klps", 202.6)],
        );
        // Comment lines and the header are ignored by the parser.
        let ledger = format!("{TREND_HEADER}{rows}");
        let breaches = judge_trend(
            &ledger,
            "boxA",
            &[sample("search", "rotating_build_klps", 100.0)],
        )
        .breaches;
        assert_eq!(breaches.len(), 1, "100 < 0.70 x 202.6");
        let ok = judge_trend(
            &ledger,
            "boxA",
            &[sample("search", "rotating_build_klps", 200.0)],
        );
        assert!(ok.breaches.is_empty());
        // Rows carry the toolchain lane: native lanes are the Trust toolchain.
        assert!(rows.trim_end().ends_with("\ttrust"), "row: {rows:?}");
    }

    #[test]
    fn trend_rows_record_the_wasm_stable_toolchain_lane() {
        // The wasm lane's modules are built on upstream stable (Trust has no
        // wasm32 std) — its rows must say so, not claim the Trust toolchain.
        let rows = trend_rows(
            "2026-07-22",
            "abc",
            "boxA",
            &[sample("wasm", "wasm_cpu_ingest_mixed_mbps", 343.0)],
        );
        assert!(rows.trim_end().ends_with("\tstable"), "row: {rows:?}");
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
