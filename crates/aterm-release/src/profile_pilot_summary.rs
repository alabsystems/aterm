// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline LLVM wall and sampled single-core exposure; never executes a measured image.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::Path;

use aterm_json::{Value, json};

type Key = (u64, u64);

#[derive(Clone)]
struct Member {
    key: Key,
    cpu: u64,
    runnable: u64,
}

struct Frame {
    at: f64,
    members: Vec<Member>,
}

#[derive(Clone)]
struct Pass {
    name: String,
    end: f64,
    wall: f64,
}

#[derive(Default)]
struct Exposure {
    covered: f64,
    cpu: f64,
    serial: f64,
}

fn number(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| "missing or invalid nonnegative timing".into())
}

fn exposure(frames: &[Frame], key: Key, pass: &Pass) -> Exposure {
    let mut out = Exposure::default();
    for pair in frames.windows(2) {
        let [a, b] = pair else { continue };
        let dt = b.at - a.at;
        let overlap = (b.at.min(pass.end) - a.at.max(pass.end - pass.wall)).max(0.0);
        if dt <= 0.0 || overlap <= 0.0 {
            continue;
        }
        let Some(before) = a.members.iter().find(|m| m.key == key) else {
            continue;
        };
        let Some(after) = b.members.iter().find(|m| m.key == key) else {
            continue;
        };
        let (Some(cpu), Some(runnable)) = (
            after.cpu.checked_sub(before.cpu),
            after.runnable.checked_sub(before.runnable),
        ) else {
            continue;
        };
        out.covered += overlap;
        out.cpu += cpu as f64 / 1e9 * overlap / dt;
        // Runnable time includes CPU and scheduler delay. An oversubscribed single thread
        // remains one runnable core even when it receives less than one core of CPU.
        if (0.75..=1.25).contains(&(runnable as f64 / 1e9 / dt)) {
            out.serial += overlap;
        }
    }
    out
}

fn samples(path: &Path) -> Result<(Vec<Frame>, Vec<Pass>), String> {
    let file = crate::durable::open_regular_file(path).map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    let mut passes = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let v: Value = aterm_json::from_str(&line.map_err(|e| e.to_string())?)
            .map_err(|e| format!("resource sample: {e}"))?;
        let at = number(&v["elapsed_s"])?;
        let mut members = Vec::new();
        if let Some(list) = v["members"].as_array() {
            for m in list {
                if let (Some(pid), Some(start), Some(cpu), Some(runnable)) = (
                    m["pid"].as_u64(),
                    m["start"].as_u64(),
                    m["cpu_ns"].as_u64(),
                    m["runnable_ns"].as_u64(),
                ) {
                    members.push(Member {
                        key: (pid, start),
                        cpu,
                        runnable,
                    });
                }
            }
        }
        if let Some(list) = v["compiler_passes"].as_array() {
            for pass in list {
                if let Some(name) = pass["pass"].as_str() {
                    passes.push(Pass {
                        name: name.to_owned(),
                        end: at,
                        wall: number(&pass["time"])?,
                    });
                }
            }
        }
        frames.push(Frame { at, members });
    }
    Ok((frames, passes))
}

fn gui_end(path: &Path, wall: f64) -> Result<f64, String> {
    let text = crate::durable::read_text(path).map_err(|e| e.to_string())?;
    let body = text
        .split_once("const UNIT_DATA = ")
        .ok_or("Cargo timing has no unit data")?
        .1
        .split_once("\n];")
        .ok_or("Cargo unit data has no terminator")?
        .0;
    let units: Value =
        aterm_json::from_str(&format!("{body}\n]")).map_err(|e| format!("Cargo unit data: {e}"))?;
    let units = units.as_array().ok_or("Cargo unit data is not an array")?;
    let mut end: f64 = 0.0;
    let mut gui = None;
    for unit in units {
        let start = number(&unit["start"])?;
        end = end.max(start + number(&unit["duration"])?);
        if unit["name"].as_str() == Some("aterm-gui") && unit["target"].as_str() == Some("") {
            for section in unit["sections"]
                .as_array()
                .ok_or("GUI timing has no sections")?
            {
                if section[0].as_str() == Some("codegen") {
                    gui = Some(start + number(&section[1]["end"])?);
                }
            }
        }
    }
    Ok(gui.ok_or("GUI timing has no codegen section")? + wall - end)
}

fn measured(frames: &[Frame], pass: &Pass, gui: bool) -> Result<Value, String> {
    let mut lives: BTreeMap<Key, (f64, f64)> = BTreeMap::new();
    for frame in frames {
        for member in &frame.members {
            lives
                .entry(member.key)
                .and_modify(|life| life.1 = frame.at)
                .or_insert((frame.at, frame.at));
        }
    }
    let key = lives
        .iter()
        .filter(|(_, (first, last))| {
            *first <= pass.end - pass.wall + 2.0 && *last >= pass.end - 2.0
        })
        .min_by(|(a, (_, end_a)), (b, (_, end_b))| {
            if gui {
                (end_a - pass.end)
                    .abs()
                    .total_cmp(&(end_b - pass.end).abs())
            } else {
                exposure(frames, **b, pass)
                    .cpu
                    .total_cmp(&exposure(frames, **a, pass).cpu)
            }
        })
        .map(|(key, _)| *key)
        .ok_or("no sampled compiler covers the LLVM pass")?;
    let e = exposure(frames, key, pass);
    if e.covered < pass.wall * 0.9 {
        return Err(format!(
            "insufficient LLVM sample coverage: {} of {}s",
            e.covered, pass.wall
        ));
    }
    Ok(
        json!({"pass":pass.name,"wall_s":pass.wall,"end_s":pass.end,"pid":key.0,
        "covered_s":e.covered,"cpu_s":e.cpu,"mean_cpu_cores":e.cpu/e.covered,
        "single_runnable_core_s":e.serial}),
    )
}

pub(super) fn run(root: &Path) -> Result<(), String> {
    let report: Value = aterm_json::from_str(
        &crate::durable::read_text(&root.join("report.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for row in report["rows"].as_array().ok_or("pilot has no rows")? {
        let image = Path::new(row["binary"].as_str().ok_or("pilot row has no image")?);
        let dir = image.parent().ok_or("pilot image has no parent")?;
        if !std::fs::canonicalize(dir)
            .map_err(|e| e.to_string())?
            .starts_with(root)
        {
            return Err("pilot take is outside the retained root".into());
        }
        let (frames, passes) = samples(&dir.join("build-resources.jsonl"))?;
        let end = gui_end(
            &dir.join("cargo-timings/cargo-timing.html"),
            number(&row["wall_s"])?,
        )?;
        let gui = passes
            .iter()
            .filter(|p| p.name == "LLVM_passes")
            .min_by(|a, b| (a.end - end).abs().total_cmp(&(b.end - end).abs()))
            .ok_or("no GUI LLVM pass")?;
        let gap = frames
            .windows(2)
            .map(|p| p[1].at - p[0].at)
            .fold(0.0, f64::max);
        if (gui.end - end).abs() > gap + 2.0 {
            return Err("GUI pass and Cargo section do not align".into());
        }
        let lto = passes
            .iter()
            .rev()
            .find(|p| matches!(p.name.as_str(), "LLVM_fatlto" | "LLVM_thinlto"))
            .ok_or("no executable LTO pass")?;
        let gui = measured(&frames, gui, true)?;
        let lto = measured(&frames, lto, false)?;
        rows.push(json!({"arm":row["arm"],"slice":row["slice"],"binary":row["binary"],
            "gui":gui,"lto":lto,"max_sample_gap_s":gap,
            "single_runnable_core_s": number(&gui["single_runnable_core_s"])?+number(&lto["single_runnable_core_s"])?}));
    }
    let out = root.join("llvm-summary.json");
    crate::durable::write_fresh(&out,json!({"schema":1,"method":"one-second per-process runnable deltas; 0.75..=1.25 runnable cores; LLVM windows end at diagnostic observation; Cargo identifies GUI",
        "commit":report["commit"],"rows":rows}).to_string()).map_err(|e|e.to_string())?;
    println!("profile-pilot LLVM summary: {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_work_and_reused_pids_do_not_count_as_serial_exposure() {
        let frames: Vec<_> = [(0., 0), (10., 10), (20., 20), (30., 60), (40., 70)]
            .into_iter()
            .map(|(at, secs)| Frame {
                at,
                members: vec![Member {
                    key: (1, 1),
                    cpu: secs * 1_000_000_000,
                    runnable: secs * 1_000_000_000,
                }],
            })
            .collect();
        let pass = Pass {
            name: "LLVM".into(),
            end: 30.,
            wall: 20.,
        };
        let out = exposure(&frames, (1, 1), &pass);
        assert_eq!(out.covered, 20.);
        assert_eq!(out.serial, 10.);
        assert_eq!(out.cpu, 50.);
        assert_eq!(exposure(&frames, (1, 2), &pass).covered, 0.);
        let serial: Vec<_> = frames
            .into_iter()
            .map(|mut f| {
                f.members[0].runnable = (f.at * 1e9) as u64;
                f
            })
            .collect();
        assert_eq!(exposure(&serial, (1, 1), &pass).serial, 20.);
    }
}
