// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `deck` floor lane (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §6): what a frame of
//! the stats deck in motion costs the main thread, from aterm-gui's `deck_frame_cost` bench (the
//! layout and patch of one needle, grid and new-reading frame, Folded and Full at 2238 device px,
//! the owner's busy scene), as frames per second of that work (BIGGER-IS-BETTER), plus one
//! headless `idle` take of `aterm-lab deck-motion` on an attested debug build, which is
//! reported and not judged: a shared desk's scheduler moves it before aterm does.

use std::path::Path;

use aterm_json::Value;

use super::floors::{FloorLane, judge_keyed};
use super::{LaneRun, workspace_root};

/// The gated keys: frames per second of each frame's layout and patch, from its p50.
const DECK_KEYS: &[&str] = &[
    "folded_needle_fps",
    "folded_grid_fps",
    "folded_reading_fps",
    "full_needle_fps",
    "full_grid_fps",
    "full_reading_fps",
];

pub(crate) const DECK_LANE: FloorLane = FloorLane {
    lane: "deck",
    example: "deck_frame_cost",
    keys: DECK_KEYS,
    baseline_file: "perf-baseline-deck.json",
    comment: "aterm DECK baseline (docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md §6): the stats \
              deck's frame cost from aterm-gui's deck_frame_cost bench (opt-test, the busy scene, \
              2238 device px at 2x): for the Folded and Full forms, a needle frame, a grid frame \
              and a new reading's whole card, each as 10^6 / its p50 in us of layout plus patch \
              (frames per second of main-thread work). BIGGER-IS-BETTER. The lane's headless idle \
              take (aterm-lab deck-motion idle, debug lab instance) is reported, not judged. \
              Re-record with targo --unverified verify perf --record deck.",
    cli: "deck",
};

/// The bench's `(form, frame)` rows each gated key reads, at the busy scene's 2238 px.
const ROWS: [(&str, &str, &str); 6] = [
    ("folded_needle_fps", "Folded", "needle"),
    ("folded_grid_fps", "Folded", "grid"),
    ("folded_reading_fps", "Folded", "reading"),
    ("full_needle_fps", "Full", "needle"),
    ("full_grid_fps", "Full", "grid"),
    ("full_reading_fps", "Full", "reading"),
];

/// The gated keys from the bench's TSV (`scene form device_px frame p50_us p95_us share`).
fn report_of(tsv: &str) -> Result<Vec<(&'static str, f64)>, String> {
    ROWS.iter()
        .map(|&(key, form, frame)| {
            let p50 = tsv
                .lines()
                .map(|l| l.split('\t').collect::<Vec<_>>())
                .find(|c| {
                    c.len() >= 5
                        && c[0] == "busy"
                        && c[1] == form
                        && c[2] == "2238"
                        && c[3] == frame
                })
                .and_then(|c| c[4].parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or_else(|| format!("the bench's table has no busy {form} 2238 {frame} row"))?;
            Ok((key, 1e6 / p50))
        })
        .collect()
}

/// `deck`: the bench judged against the baseline, then the idle take reported.
pub(crate) fn gate_deck(record: bool) -> LaneRun {
    let scratch = match aterm_tempfile::Builder::new()
        .prefix("aterm-deck-")
        .suffix(".noindex")
        .tempdir()
    {
        Ok(dir) => dir,
        Err(e) => return LaneRun::fail(format!("could not make a scratch directory: {e}")),
    };
    let tsv = match bench(scratch.path()) {
        Ok(tsv) => tsv,
        Err(e) => return e.verdict(),
    };
    let values = match report_of(&tsv) {
        Ok(v) => v,
        Err(e) => return LaneRun::fail(e),
    };
    let mut obj = aterm_json::Map::new();
    for (key, v) in &values {
        obj.insert(
            (*key).to_string(),
            Value::Number(aterm_json::Number::from(*v)),
        );
        eprintln!("    {key} = {v:.1}");
    }
    let mut run = judge_keyed(&DECK_LANE, &Value::Object(obj), record);
    match idle_take(scratch.path()) {
        Ok(summary) => eprintln!("    UNGATED — the headless idle take, reported:\n{summary}"),
        Err(why) => {
            eprintln!("    UNGATED — the headless idle take did not run: {why}");
            run.unjudged.push(format!("deck idle take ({why})"));
        }
    }
    run
}

/// The bench's test, run by its exact path: an ordinary test (four live frames a case) that
/// takes 400 samples a case and writes its table only when `ATERM_DECK_COST` names a file.
const BENCH: &str = "stats_deck::live_paint_tests::deck_frame_cost";

/// Run [`BENCH`] optimized and alone, its table written into `dir`.
fn bench(dir: &Path) -> Result<String, super::floors::MeasureError> {
    let driver = super::driver::cargo_driver()?;
    let out = dir.join("deck_frame_cost.tsv");
    let args = [
        "-q",
        "-p",
        "aterm-gui",
        "--profile",
        "opt-test",
        "--lib",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ];
    eprintln!(
        "  $ ATERM_DECK_COST={} {}",
        out.display(),
        driver.display_workspace("test", &args)
    );
    let built = driver
        .workspace_command("test")?
        .args(args)
        .env("ATERM_DECK_COST", &out)
        .current_dir(workspace_root())
        .stderr(aterm_uds::spawnfd::Stdio::inherit())
        .output()
        .map_err(|e| format!("could not build the bench: {e}"))?;
    if !built.status.success() {
        return Err(format!("deck bench build exited {}", built.status).into());
    }
    let executable = super::bench_build::built_executable(
        &String::from_utf8_lossy(&built.stdout),
        "aterm_gui",
        "lib",
    )?;
    let mut run = crate::exec::check_command(&executable);
    run.args([BENCH, "--exact", "--nocapture", "--test-threads", "1"])
        .env("ATERM_DECK_COST", &out)
        .current_dir(workspace_root());
    let _entry = super::measurement_entry().map_err(super::floors::MeasureError::Timing)?;
    let ran = run
        .output()
        .map_err(|e| format!("could not execute deck bench: {e}"))?;
    _entry
        .timing_admission()
        .map_err(|why| super::floors::MeasureError::Timing(why.to_string()))?;
    if !ran.status.success() {
        return Err(format!(
            "the bench exited {:?}\n{}",
            ran.status.code(),
            String::from_utf8_lossy(&ran.stderr)
        )
        .into());
    }
    crate::read_regular_text(&out).map_err(|e| {
        let said = String::from_utf8_lossy(&ran.stdout);
        let said = said
            .lines()
            .rfind(|line| line.starts_with("test result:"))
            .unwrap_or("no test result line");
        super::floors::MeasureError::Failed(format!(
            "the bench wrote no table ({e}); {BENCH} said `{said}`"
        ))
    })
}

/// Build an attested debug app and lab, then take `deck-motion idle`; its diagnostic summary.
fn idle_take(dir: &Path) -> Result<String, String> {
    use super::bench_build::{self, Targo};

    let driver = super::driver::cargo_driver()?;
    let targo = Targo::new(driver);
    let build = [
        "-q",
        "--profile",
        "test",
        "-p",
        "aterm",
        "--bin",
        "aterm",
        "-p",
        "aterm-lab",
        "--bin",
        "aterm-lab",
        "--message-format=json-render-diagnostics",
    ];
    let flags = bench_build::config_rustflags(&targo)
        .map_err(|why| format!("debug deck configuration: {why:?}"))?;
    if let Some(why) = bench_build::flags::refusal(&flags) {
        return Err(why);
    }
    let plan = bench_build::audit(&targo, "build", &build)
        .map_err(|why| format!("debug deck plan: {why:?}"))?;
    let promise = bench_build::units::Promise::of(bench_build::bench_profile::Profile::Dev);
    if let Some(why) = plan.refusal(promise) {
        return Err(why);
    }
    eprintln!("  debug deck fixture plan: {}", plan.summary());
    eprintln!("  $ {}", targo.display("build", &build));
    let built = targo
        .command("build")?
        .args(build)
        .stderr(aterm_uds::spawnfd::Stdio::inherit())
        .output()
        .map_err(|e| format!("could not spawn the build: {e}"))?;
    if !built.status.success() {
        return Err(format!("the debug fixture build exited {}", built.status));
    }
    let messages = String::from_utf8_lossy(&built.stdout);
    let aterm = bench_build::built_debug_app(&messages, "aterm")?;
    let lab = bench_build::built_debug_app(&messages, "aterm-lab")?;
    let out = dir.join("takes");
    let json = dir.join("deck.json");
    let (aterm_s, out_s, json_s) = (
        aterm.to_string_lossy().into_owned(),
        out.to_string_lossy().into_owned(),
        json.to_string_lossy().into_owned(),
    );
    let args = [
        "deck-motion",
        "--aterm",
        aterm_s.as_str(),
        "--json-out",
        json_s.as_str(),
        out_s.as_str(),
        "idle",
    ];
    eprintln!(
        "  debug deck idle diagnostic: {} {}",
        lab.display(),
        args.join(" ")
    );
    let took = crate::exec::check_command(&lab)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not spawn aterm-lab: {e}"))?;
    if !took.status.success() {
        return Err(format!(
            "deck-motion exited {:?}: {}",
            took.status.code(),
            String::from_utf8_lossy(&took.stderr)
                .lines()
                .last()
                .unwrap_or("")
        ));
    }
    crate::read_regular_text(out.join("deck-motion.txt"))
        .map_err(|e| format!("deck-motion wrote no summary: {e}"))
}

#[cfg(test)]
#[path = "deck_tests.rs"]
mod tests;
