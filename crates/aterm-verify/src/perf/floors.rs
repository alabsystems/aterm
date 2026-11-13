// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Release performance floor lanes against committed golden baselines ([`super::trend`]).

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::trend::{PointFigure, utc_date};
use super::workspace_root;
use super::{LaneRun, Status, TrendSample};

/// The pass threshold as a fraction of the recorded baseline.
pub(crate) const PASS_RATIO: f64 = 0.45;

/// Where the committed baselines and the trend ledger live, in the running checkout
/// ([`workspace_root`]).
pub(crate) fn golden_dir() -> PathBuf {
    aterm_tempfile::manifest_dir().join("perf/golden")
}

/// The verdict of comparing one fresh number against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No usable baseline (missing, non-finite or <= 0).
    NoBaseline,
    /// `measured >= baseline * ratio`.
    Pass,
    /// `measured < baseline * ratio`.
    Fail,
}

/// Pure threshold comparison.
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
/// cannot use it.
fn read_baseline<T>(
    path: &Path,
    parse: impl FnOnce(&Value) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let text = match crate::read_regular_text(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} is unreadable: {e}", path.display())),
    };
    let json = object(&text).ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    parse(&json)
        .map(Some)
        .map_err(|why| format!("{} {why}", path.display()))
}

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

/// A [`PerfReport`] from the harness line or a baseline.
pub(crate) fn parse_report(json: &Value) -> Option<PerfReport> {
    let u64_field = |key: &str| {
        let v = number(json, key)?;
        (v >= 0.0 && v <= u64::MAX as f64 && v.fract() == 0.0).then_some(v as u64)
    };
    Some(PerfReport {
        median_mbps: number(json, "median_mbps")?,
        min_mbps: number(json, "min_mbps")?,
        max_mbps: number(json, "max_mbps")?,
        workload_bytes: u64_field("workload_bytes").filter(|&b| b > 0)?,
        n: u64_field("n").filter(|&n| n > 0)?,
        warmup: u64_field("warmup")?,
    })
}

/// The committed baseline for a [`PerfReport`].
pub(crate) fn baseline_json(r: &PerfReport, ratio: f64, comment: &str) -> String {
    format!(
        "{{\n  \"_comment\": {},\n  \"median_mbps\": {},\n  \"min_mbps\": {},\n  \"max_mbps\": {},\n  \"workload_bytes\": {},\n  \"n\": {},\n  \"warmup\": {},\n  \"pass_ratio\": {}\n}}\n",
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

/// Throughput: the median clears `baseline * PASS_RATIO`.
pub(crate) fn gate_throughput(record: bool) -> LaneRun {
    let report = match measure_example("perf_harness").and_then(|json| {
        parse_report(&json).ok_or_else(|| {
            MeasureError::Failed(format!("perf_harness JSON lacks a field: {json:?}"))
        })
    }) {
        Ok(r) => r,
        Err(e) => return e.verdict(),
    };
    let samples = vec![TrendSample {
        lane: "throughput",
        metric: "median_mbps".to_string(),
        value: report.median_mbps,
        inverted: false,
        at: Vec::new(),
    }];
    let path = baseline_path();
    let mut run = if record {
        record_throughput(&path, &report)
    } else {
        compare_against_baseline(&path, &report)
    };
    run.samples = samples;
    run
}

fn record_throughput(path: &Path, report: &PerfReport) -> LaneRun {
    if !(report.median_mbps.is_finite() && report.median_mbps > 0.0) {
        return LaneRun::fail(format!(
            "REFUSING to record — median_mbps measured {}; a non-positive floor judges nothing. \
             {} left untouched.",
            report.median_mbps,
            path.display()
        ));
    }
    let previous = crate::read_regular_text(path).ok().and_then(|t| object(&t));
    let (comment, _) = carried_comment(previous.as_ref(), THROUGHPUT_COMMENT, &utc_date());
    write_baseline(path, &baseline_json(report, PASS_RATIO, &comment))
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

/// A keyed floor lane.
pub(crate) struct FloorLane {
    /// The lane's name in the trend ledger's `lane/metric` column.
    pub lane: &'static str,
    /// The `aterm-bench` example that measures it.
    pub example: &'static str,
    /// The gated keys (bigger-is-better).
    pub keys: &'static [&'static str],
    /// The baseline's file name under [`golden_dir`].
    pub baseline_file: &'static str,
    /// The `_comment` a first recording writes.
    pub comment: &'static str,
    /// The CLI lane name.
    pub cli: &'static str,
}

/// Per-corpus hostile-input floors.
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

/// Scrollback-scrub read-path floors.
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

/// Crash-restore floor (`crates/aterm-restore`).
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

/// The tail tap's absolute fence.
pub(crate) const TAIL_TAP_RATIO_FENCE: f64 = 0.97;

/// ZSTD-decode floor: decoded mb/s of `aterm_zstd::Decoder`, atpkg's `.tar.zst` reader.
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

/// Effects: what a frame of the rectangle effects costs on scripted scenes.
pub(crate) const EFFECTS_LANE: FloorLane = FloorLane {
    lane: "effects",
    example: "effects_harness",
    keys: EFFECTS_KEYS,
    baseline_file: "perf-baseline-effects.json",
    comment: "aterm EFFECTS baseline (docs/DESIGN-card-engine-2026-10-19.md lane B0): per scene \
              (episode, flowing, settled, typing, scroll), the GPU time as a reference full-repaint \
              frame's p50 over the scene's (_gpu_rel), the reference's own against a blank \
              full repaint (ref_text_gpu_rel), and the CPU work of the producer, the \
              CPU raster and the GPU encode as frames per 10^9 instructions retired \
              (_per_ginsn). BIGGER-IS-BETTER. The absolute figures and the pass-economy \
              ablation are in docs/measured/effects-baseline-2026-10-19.md. Re-record with \
              targo --unverified verify perf --record effects.",
    cli: "effects",
};

/// A lane's gated keys: its GPU keys, its CPU work keys, and both, in that order.
macro_rules! effects_keys {
    (
        $all:ident, $gpu_keys:ident, $cpu_keys:ident;
        gpu: [$($gpu:literal),* $(,)?], cpu: [$($cpu:literal),* $(,)?] $(,)?
    ) => {
        const $gpu_keys: &[&str] = &[$($gpu),*];
        const $cpu_keys: &[&str] = &[$($cpu),*];
        const $all: &[&str] = &[$($gpu,)* $($cpu),*];
    };
}

effects_keys! {
    EFFECTS_KEYS, EFFECTS_GPU_KEYS, EFFECTS_CPU_KEYS;
    gpu: [
        "ref_text_gpu_rel",
        "episode_gpu_rel",
        "flowing_gpu_rel",
        "settled_gpu_rel",
        "typing_gpu_rel",
        "scroll_gpu_rel",
    ],
    cpu: [
        "episode_emit_per_ginsn",
        "episode_raster_per_ginsn",
        "episode_encode_per_ginsn",
        // Half the flowing card's frames are its steps' draws and half the replays of the keys
        // between them, so its median frame is its costliest replay, one frame's outlier: its
        // emit and its raster are rated by all its frames over all their instructions.
        "flowing_emit_mean_per_ginsn",
        "flowing_raster_mean_per_ginsn",
        "flowing_encode_per_ginsn",
        "settled_raster_per_ginsn",
        "settled_encode_per_ginsn",
        "typing_emit_per_ginsn",
        "typing_raster_per_ginsn",
        "typing_encode_per_ginsn",
        "scroll_emit_per_ginsn",
        "scroll_raster_per_ginsn",
        "scroll_encode_per_ginsn",
    ],
}

/// A lane the effects harness measures: the harness run it judges, and the halves of its keys
/// a host with only one of the GPU clock and the instruction counter judges.
pub(crate) struct EffectsLane {
    /// Every gated key.
    pub full: FloorLane,
    /// The GPU keys alone: an arm with a GPU clock (Metal's) on a host with no instruction
    /// counter the harness reads.
    pub gpu: FloorLane,
    /// The CPU work keys alone: an arm with no GPU clock (a device without timestamp queries)
    /// on a host that counts instructions.
    pub cpu: FloorLane,
    /// The harness's arguments: the scenes it runs and the ground it draws them on.
    pub args: &'static [&'static str],
    /// The ground its report must name (`dark` or `light`): a light lane fed a dark frame
    /// would judge the dark trail under the light trail's name.
    pub ground: &'static str,
}

/// [`EFFECTS_LANE`], every scene a bare harness run draws, on the dark ground.
pub(crate) const EFFECTS: EffectsLane = EffectsLane {
    gpu: FloorLane {
        keys: EFFECTS_GPU_KEYS,
        ..EFFECTS_LANE
    },
    cpu: FloorLane {
        keys: EFFECTS_CPU_KEYS,
        ..EFFECTS_LANE
    },
    full: EFFECTS_LANE,
    args: &[],
    ground: "dark",
};

effects_keys! {
    COMPOSER_KEYS, COMPOSER_GPU_KEYS, COMPOSER_CPU_KEYS;
    gpu: ["composer_gpu_rel"],
    cpu: [
        "composer_emit_per_ginsn",
        "composer_raster_per_ginsn",
        "composer_encode_per_ginsn",
    ],
}

/// A lane of the cursor trail on Claude Code's composer (docs/design/PRISM-GLASS.md §15.1
/// P0.12): the effects harness's `composer` scene alone, drawn on `ground` by `args`.
const fn composer_lane(
    cli: &'static str,
    baseline_file: &'static str,
    comment: &'static str,
    args: &'static [&'static str],
    ground: &'static str,
) -> EffectsLane {
    let full = FloorLane {
        lane: cli,
        example: "effects_harness",
        keys: COMPOSER_KEYS,
        baseline_file,
        comment,
        cli,
    };
    EffectsLane {
        gpu: FloorLane {
            keys: COMPOSER_GPU_KEYS,
            ..full
        },
        cpu: FloorLane {
            keys: COMPOSER_CPU_KEYS,
            ..full
        },
        full,
        args,
        ground,
    }
}

/// The composer's trail on Default, the dark ground.
pub(crate) const TRAIL_DARK_COMPOSER: EffectsLane = composer_lane(
    "trail-dark-composer",
    "perf-baseline-trail-dark-composer.json",
    "aterm TRAIL-DARK-COMPOSER baseline (docs/design/PRISM-GLASS.md §15.1 P0.12): the effects \
     harness's composer scene, Rainbow Kitty typed at 10 keys a second to a 117-cell line in \
     Claude Code's composer (its prompt between two rules) on Default's dark ground: the GPU \
     time as a reference full-repaint frame's p50 over the scene's (composer_gpu_rel), and the \
     CPU work of the producer, the CPU raster and the GPU encode as frames per 10^9 \
     instructions retired (_per_ginsn). BIGGER-IS-BETTER. Re-record with targo --unverified \
     verify perf --record trail-dark-composer.",
    &["--only", "composer"],
    "dark",
);

/// The composer's trail on Default Light: main's light (paper) arm.
pub(crate) const TRAIL_LIGHT_COMPOSER: EffectsLane = composer_lane(
    "trail-light-composer",
    "perf-baseline-trail-light-composer.json",
    "aterm TRAIL-LIGHT-COMPOSER baseline (docs/design/PRISM-GLASS.md §15.1 P0.12): \
     trail-dark-composer's scene on Default Light (effects_harness --light), the glow's \
     page and light grounded as a window grounds them (TrailGround::ground on the terminal's \
     configured pair), so the trail draws the light arm a window draws at the live level \
     (prism_glass::LIVE): the GPU time as a reference full-repaint frame's p50 over the \
     scene's (composer_gpu_rel), and the CPU work of the producer, the CPU raster and the GPU \
     encode as frames per 10^9 instructions retired (_per_ginsn). BIGGER-IS-BETTER. Re-record \
     with targo --unverified verify perf --record trail-light-composer.",
    &["--only", "composer", "--light"],
    "light",
);

/// Report suffixes for the calibration of the run supplying a GPU ratio's median.
/// Pooled reference medians include other scenes and runs. The ledger uses distinct
/// ratio-local names so those older rows cannot calibrate these observations. CPU keys
/// count instructions and carry no GPU point.
const EFFECTS_GPU_POINT: &[&str] = &["ref_gpu_p50_us", "ref_blank_gpu_p50_us"];

/// The features the effects harness is built with: its pass-economy ablation is a dev seam.
const EFFECTS_FEATURES: &[&str] = &["dev-seams"];

/// The 42s-freeze-class absolute fences.
pub(crate) const RESIZE_RING_WORST_CAP_MS: f64 = 5_000.0;
pub(crate) const RESIZE_TIERED_SYNC_WORST_CAP_MS: f64 = 100.0;
pub(crate) const RESIZE_DEEP_STEP_WORST_CAP_MS: f64 = 100.0;

/// The gated values of `json`, in key order.
pub(crate) fn parse_keyed(lane: &FloorLane, json: &Value) -> Option<Vec<(&'static str, f64)>> {
    lane.keys
        .iter()
        .map(|key| number(json, key).map(|v| (*key, v)))
        .collect()
}

/// A keyed lane's committed baseline.
pub(crate) fn keyed_baseline_json(values: &[(&str, f64)], ratio: f64, comment: &str) -> String {
    let mut s = format!("{{\n  \"_comment\": {},\n", quoted(comment));
    for (key, v) in values {
        s.push_str(&format!("  \"{key}\": {v},\n"));
    }
    s.push_str(&format!("  \"pass_ratio\": {ratio}\n}}\n"));
    s
}

/// The `_comment` a re-record writes.
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
    write_baseline_using(path, text, super::publication::publish)
}

fn write_baseline_using(
    path: &Path,
    text: &str,
    publish: super::publication::Publisher,
) -> LaneRun {
    match publish(path, text) {
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

/// Run an `aterm-bench` example in release and return its JSON report.
pub(crate) fn measure_example(example: &str) -> Result<Value, MeasureError> {
    measure_example_with(example, &[], &[])
}

/// [`measure_example`] with `aterm-bench` features on (a harness that needs a dev seam), handed
/// `harness_args` (the scenes a harness runs, and on what).
pub(crate) fn measure_example_with(
    example: &str,
    features: &[&str],
    harness_args: &[&str],
) -> Result<Value, MeasureError> {
    measure_example_for(example, features, harness_args, true)
}

/// The performance-core-cycle lane deliberately judges cycles at the load it finds.
pub(crate) fn measure_cycle_example_with(
    example: &str,
    features: &[&str],
    harness_args: &[&str],
) -> Result<Value, String> {
    measure_example_for(example, features, harness_args, false).map_err(|e| e.to_string())
}

#[derive(Debug)]
pub(crate) enum MeasureError {
    Failed(String),
    Timing(String),
}
impl std::fmt::Display for MeasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(s) | Self::Timing(s) => f.write_str(s),
        }
    }
}
impl From<String> for MeasureError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}
impl From<&str> for MeasureError {
    fn from(s: &str) -> Self {
        Self::Failed(s.to_owned())
    }
}
impl MeasureError {
    pub(crate) fn verdict(self) -> LaneRun {
        match self {
            Self::Failed(why) => LaneRun::fail(format!("could not measure: {why}")),
            Self::Timing(why) => LaneRun::could_not_run(
                why,
                "run with measured timing admission at the postcompile entry",
            ),
        }
    }
}

fn measure_example_for(
    example: &str,
    features: &[&str],
    harness_args: &[&str],
    timing: bool,
) -> Result<Value, MeasureError> {
    let driver = super::driver::cargo_driver()?;
    let features = features.join(",");
    let mut args = vec!["--release", "-q", "-p", "aterm-bench"];
    if !features.is_empty() {
        args.extend(["--features", features.as_str()]);
    }
    args.extend([
        "--example",
        example,
        "--message-format=json-render-diagnostics",
    ]);
    eprintln!("  $ {}", driver.display_workspace("build", &args));
    let built = driver
        .workspace_command("build")?
        .args(&args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not build {example}: {e}"))?;
    if !built.status.success() {
        return Err(MeasureError::Failed(format!(
            "{example} build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        )));
    }
    let executable = super::bench_build::built_executable(
        &String::from_utf8_lossy(&built.stdout),
        example,
        "example",
    )?;
    let mut run = crate::exec::check_command(&executable);
    run.args(harness_args).current_dir(workspace_root());
    // Compilation and command preparation are complete. No targo run/bench prefix can build
    // after this fresh observation and before the measurement executable starts.
    let _entry = if timing {
        Some(super::measurement_entry().map_err(MeasureError::Timing)?)
    } else {
        None
    };
    let out = run
        .output()
        .map_err(|e| format!("could not execute {}: {e}", executable.display()))?;
    if let Some(entry) = &_entry {
        entry
            .timing_admission()
            .map_err(|why| MeasureError::Timing(why.to_string()))?;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if line.contains(&format!("{example}:")) {
            eprintln!("  {line}");
        }
    }
    if !out.status.success() {
        return Err(MeasureError::Failed(format!(
            "{example} exited {:?}\n{stderr}",
            out.status.code()
        )));
    }
    last_json_line(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| MeasureError::Failed(format!("{example} printed no JSON report")))
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
    judge_keyed_at(lane, json, record, &keyed_baseline_path(lane))
}

fn judge_keyed_at(lane: &FloorLane, json: &Value, record: bool, path: &Path) -> LaneRun {
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
            at: Vec::new(),
        })
        .collect();
    let mut run = if record {
        record_keyed(lane, path, &values)
    } else {
        compare_keyed_against_baseline(lane, path, &values)
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
    let previous = crate::read_regular_text(path).ok().and_then(|t| object(&t));
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
        Err(e) => e.verdict(),
    }
}

/// Crash-restore: the ratio floors, the tail tap's absolute fence (which holds with no baseline).
pub(crate) fn gate_crash_restore(record: bool) -> LaneRun {
    match measure_example(CRASH_RESTORE_LANE.example) {
        Ok(json) => judge_crash_restore(&json, record),
        Err(e) => e.verdict(),
    }
}

fn judge_crash_restore(json: &Value, record: bool) -> LaneRun {
    judge_crash_restore_at(json, record, &keyed_baseline_path(&CRASH_RESTORE_LANE))
}

fn judge_crash_restore_at(json: &Value, record: bool, path: &Path) -> LaneRun {
    let fence = tail_tap_fence(json);
    let mut run = judge_keyed_at(&CRASH_RESTORE_LANE, json, record && fence.is_ok(), path);
    match fence {
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

/// An effects harness lane: the calibrated floors, judged on the keys the harness's GPU clock
/// allows.
pub(crate) fn gate_effects(lane: &EffectsLane, record: bool) -> LaneRun {
    match measure_example_with(lane.full.example, EFFECTS_FEATURES, lane.args) {
        Ok(json) => judge_effects(lane, &json, record),
        Err(e) => e.verdict(),
    }
}

fn judge_effects(lane: &EffectsLane, json: &Value, record: bool) -> LaneRun {
    let (judged, missing) = match effects_lane(lane, json, record) {
        Ok(judged) => judged,
        Err(run) => return run,
    };
    let mut run = judge_keyed(judged, json, record);
    run.unjudged.extend(missing);
    at_gpu_point(lane, &mut run, json);
    let reported = json.as_object().map_or(0, |o| o.len());
    eprintln!(
        "    UNGATED — {} more keys reported, not judged (the record: absolute costs, spreads, \
         time against the reference, the ablation)",
        reported.saturating_sub(judged.keys.len())
    );
    run
}

/// Pin each GPU trend sample to the GPU state the report measured it in, or keep it out of the
/// trend when the report does not say: held against another state's rows, a state change reads
/// as a regression.
fn at_gpu_point(lane: &EffectsLane, run: &mut LaneRun, json: &Value) {
    let before = run.samples.len();
    run.samples.retain_mut(|s| {
        if !lane.gpu.keys.contains(&s.metric.as_str()) {
            return true;
        }
        let at: Option<Vec<PointFigure>> = EFFECTS_GPU_POINT
            .iter()
            .zip(["ratio_ref_gpu_p50_us", "ratio_ref_blank_gpu_p50_us"])
            .map(|(&suffix, name)| {
                number(json, &format!("{}_{suffix}", s.metric))
                    .filter(|v| *v > 0.0)
                    .map(|value| PointFigure { name, value })
            })
            .collect();
        if let Some(at) = at {
            s.at = at;
            true
        } else {
            false
        }
    });
    let dropped = before - run.samples.len();
    if dropped > 0 {
        let figures = EFFECTS_GPU_POINT.join(", ");
        eprintln!(
            "    NO TREND SAMPLE for {dropped} GPU key(s) — the report lacks a positive \
             ratio-local {figures}, so it does not say which GPU state they were measured in."
        );
        run.unjudged.push(format!(
            "{} GPU trend ({dropped} key(s); the report lacks {figures})",
            lane.full.cli
        ));
    }
}

/// The keys that judge an effects report, and what this host could not measure for them; or
/// the run that ends it before any baseline is read.
fn effects_lane<'a>(
    lane: &'a EffectsLane,
    json: &Value,
    record: bool,
) -> Result<(&'a FloorLane, Option<String>), LaneRun> {
    if let Some(why) = json.get("skip").and_then(Value::as_str) {
        return Err(LaneRun::skip(
            format!("the effects harness could not draw here: {why}"),
            "run it on a host with a GPU",
        ));
    }
    let named = |key: &str| json.get(key).and_then(Value::as_str);
    if named("ground") != Some(lane.ground) {
        return Err(LaneRun::fail(format!(
            "the harness report names the {} ground, not the {} ground {} measures; refusing \
             to judge it",
            named("ground").unwrap_or("no"),
            lane.ground,
            lane.full.cli
        )));
    }
    let (Some(clock), Some(counter)) = (named("gpu_clock"), named("cpu_counter")) else {
        return Err(LaneRun::fail(
            "the harness report does not name its gpu_clock and cpu_counter; refusing to judge it",
        ));
    };
    if !matches!(clock, "metal" | "none") || !matches!(counter, "instructions" | "none") {
        return Err(LaneRun::fail(format!(
            "the harness report names an unsupported clock or counter ({clock}, {counter}); \
             refusing to judge it"
        )));
    }
    let cli = lane.full.cli;
    let (judged, missing) = match (clock != "none", counter != "none") {
        (true, true) => (&lane.full, None),
        (true, false) => (
            &lane.gpu,
            Some(format!(
                "{cli} CPU work (no instruction counter on this host)"
            )),
        ),
        (false, true) => (
            &lane.cpu,
            Some(format!("{cli} GPU time (this arm has no GPU clock)")),
        ),
        (false, false) => {
            return Err(LaneRun::skip(
                "this host has neither a GPU clock nor an instruction counter the effects \
                 harness reads, so nothing it measured can be judged",
                "run it on macOS (Metal's command-buffer clock and the cores' instruction counter)",
            ));
        }
    };
    if let Some(what) = missing.as_ref().filter(|_| record) {
        return Err(LaneRun::fail(format!(
            "REFUSING to record — the baseline would lose the floors this host cannot judge \
             ({what}); record on a host that has both (macOS)"
        )));
    }
    Ok((judged, missing))
}

/// The tail tap's fence: a missing ratio is a breach (an unmeasured cost is not a small one).
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
/// that has history. A synchronous resize's worst is the median of its
/// passes' worsts, not the run's one worst, which one stall lifts past a
/// ceiling of its best / 0.70 (the 1.1 ms tiered sync read 10.7 ms at a load
/// of 42); a resize made slower is slower in every pass. A pump step's worst
/// is fenced, not trended ([`RESIZE_FENCES`]): the worst of the hundreds of
/// steps a pass takes is the machine's tail, and even its median over five
/// passes read 0.88 to 1.29 ms over ten runs of unchanged code at a load of
/// 17 to 45, past the 1 / 0.70 the trend allows.
const RESIZE_INVERTED_TREND_KEYS: [&str; 5] = [
    "resize_ring_worst_p50_ms",
    "resize_tiered_sync_worst_p50_ms",
    "deep_mixed_pump_total_ms",
    "deep_mixed_sync_worst_p50_ms",
    "deep_wrapall_pump_total_ms",
];

/// Each absolute fence: the key it holds, its cap, and what a breach is. A
/// synchronous resize is held by the run's one worst; a pump step by the
/// median of its passes' worsts, since one stall reached 132.8 ms in a run
/// whose median read 1.29 ms (load 42).
const RESIZE_FENCES: [(&str, f64, &str); 4] = [
    (
        "resize_ring_worst_ms",
        RESIZE_RING_WORST_CAP_MS,
        SYNC_FREEZE,
    ),
    (
        "resize_tiered_sync_worst_ms",
        RESIZE_TIERED_SYNC_WORST_CAP_MS,
        SYNC_FREEZE,
    ),
    (
        "deep_mixed_step_worst_p50_ms",
        RESIZE_DEEP_STEP_WORST_CAP_MS,
        STEP_FREEZE,
    ),
    (
        "deep_wrapall_step_worst_p50_ms",
        RESIZE_DEEP_STEP_WORST_CAP_MS,
        STEP_FREEZE,
    ),
];

const SYNC_FREEZE: &str = "the 42s-freeze class (synchronous O(history) rewrap) is back";
const STEP_FREEZE: &str = "a bounded pump step costs O(history) again, in every pass";

/// Resize/rewrap: the ratio floors, the absolute fences (which hold with no baseline at all).
pub(crate) fn gate_resize(record: bool) -> LaneRun {
    let json = match measure_example(RESIZE_LANE.example) {
        Ok(j) => j,
        Err(e) => return e.verdict(),
    };
    judge_resize(&json, record)
}

fn judge_resize(json: &Value, record: bool) -> LaneRun {
    judge_resize_at(json, record, &keyed_baseline_path(&RESIZE_LANE))
}

fn judge_resize_at(json: &Value, record: bool, path: &Path) -> LaneRun {
    let breached = resize_fences(json);
    let mut run = judge_keyed_at(&RESIZE_LANE, json, record && breached.is_empty(), path);
    for key in RESIZE_INVERTED_TREND_KEYS {
        match number(json, key) {
            Some(v) => run.samples.push(TrendSample {
                lane: RESIZE_LANE.lane,
                metric: key.to_string(),
                value: v,
                inverted: true,
                at: Vec::new(),
            }),
            None => eprintln!("    {key}: NO TREND SAMPLE — absent from the harness report."),
        }
    }
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
        .filter(|k| {
            !RESIZE_LANE.keys.contains(k)
                && !RESIZE_INVERTED_TREND_KEYS.contains(k)
                && !RESIZE_FENCES.iter().any(|(fenced, ..)| fenced == k)
        })
        .collect();
    if !ungated.is_empty() {
        // Several are the run's shape (ring_lines, n, warmup); converge_stale_width
        // is a correctness outcome nothing here refuses. Which deserve a floor is
        // an owner call; the log says they were not judged.
        eprintln!("    UNGATED — reported, not judged: {}", ungated.join(", "));
    }
    run
}

/// The absolute worst-case fences.
pub(crate) fn resize_fences(json: &Value) -> Vec<String> {
    let mut findings = Vec::new();
    for (key, cap, breach) in RESIZE_FENCES {
        match number(json, key) {
            Some(v) if v <= cap => {
                eprintln!("    {key}: GREEN — {v:.1} ms <= absolute cap {cap:.0} ms");
            }
            Some(v) => findings.push(format!(
                "{key} {v:.1} ms > absolute cap {cap:.0} ms: {breach}"
            )),
            None => findings.push(format!("{key} missing from the harness report")),
        }
    }
    findings
}

#[cfg(test)]
#[path = "floors_tests.rs"]
mod tests;
