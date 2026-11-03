// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `deck` floor lane (`docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §6): what a frame of
//! the stats deck in motion costs the main thread, from aterm-gui's `deck_frame_cost` bench (the
//! layout and patch of one needle, grid and new-reading frame, Folded and Full at 2238 device px,
//! the owner's busy scene), as frames per second of that work (BIGGER-IS-BETTER), plus one
//! headless `idle` take of `aterm-lab deck-motion` on a release dev-seams build, which is
//! reported and not judged: a shared desk's scheduler moves it before aterm does.

use std::path::{Path, PathBuf};

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
              take (aterm-lab deck-motion idle, release dev-seams) is reported, not judged. \
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

/// Run `deck_frame_cost` optimized and alone, its table written into `dir`.
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
        .workspace_command("test")
        .args(args)
        .env("ATERM_DECK_COST", &out)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("could not build the bench: {e}"))?;
    if !built.status.success() {
        return Err(format!(
            "deck bench build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        )
        .into());
    }
    let executable = super::bench_build::built_executable(
        &String::from_utf8_lossy(&built.stdout),
        "aterm_gui",
        "lib",
    )?;
    let mut run = aterm_uds::spawnfd::command(&executable);
    run.args([
        "--ignored",
        "deck_frame_cost",
        "--nocapture",
        "--test-threads",
        "1",
    ])
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
    crate::read_regular_text(&out)
        .map_err(|e| super::floors::MeasureError::Failed(format!("the bench wrote no table: {e}")))
}

/// Build aterm release with its dev seams, then take `deck-motion idle` of it; its summary.
fn idle_take(dir: &Path) -> Result<String, String> {
    let driver = super::driver::cargo_driver()?;
    let build = ["--release", "-q", "-p", "aterm", "--features", "dev-seams"];
    eprintln!("  $ {}", driver.display_workspace("build", &build));
    let built = driver
        .workspace_command("build")
        .args(build)
        .current_dir(workspace_root())
        .status()
        .map_err(|e| format!("could not spawn the build: {e}"))?;
    if !built.success() {
        return Err(format!("the release build exited {:?}", built.code()));
    }
    let aterm: PathBuf = super::driver::target_dir()?.join("release/aterm");
    let out = dir.join("takes");
    let json = dir.join("deck.json");
    let (aterm_s, out_s, json_s) = (
        aterm.to_string_lossy().into_owned(),
        out.to_string_lossy().into_owned(),
        json.to_string_lossy().into_owned(),
    );
    let lab = [
        "-q",
        "-p",
        "aterm-lab",
        "--",
        "deck-motion",
        "--aterm",
        aterm_s.as_str(),
        "--json-out",
        json_s.as_str(),
        out_s.as_str(),
        "idle",
    ];
    eprintln!("  $ {}", driver.display_workspace("run", &lab));
    let took = driver
        .workspace_command("run")
        .args(lab)
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
mod tests {
    use super::*;

    #[test]
    fn the_gated_keys_are_the_busy_2238_rows_as_frames_per_second() {
        let mut tsv =
            String::from("scene\tform\tdevice_px\tframe\tp50_us\tp95_us\tpatched_share\n");
        for form in ["Folded", "Full"] {
            for (frame, p50) in [("needle", 200.0), ("grid", 500.0), ("reading", 1250.0)] {
                tsv.push_str(&format!("busy\t{form}\t2238\t{frame}\t{p50}\t1.0\t0.1\n"));
                tsv.push_str(&format!("worst\t{form}\t2238\t{frame}\t9999\t1.0\t0.1\n"));
                tsv.push_str(&format!("busy\t{form}\t700\t{frame}\t9999\t1.0\t0.1\n"));
            }
        }
        let got = report_of(&tsv).expect("every row");
        assert_eq!(got.len(), DECK_KEYS.len());
        assert_eq!(got[0], ("folded_needle_fps", 5000.0));
        assert_eq!(got[2], ("folded_reading_fps", 800.0));
        assert_eq!(got[4], ("full_grid_fps", 2000.0));
        assert!(DECK_KEYS.iter().zip(&got).all(|(k, (g, _))| k == g));
        let short = tsv.replace("busy\tFull\t2238\tgrid", "busy\tFull\t2238\tgrids");
        assert!(report_of(&short).is_err());
    }
}
