// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC MEASUREMENT.

#[cfg(unix)]
#[path = "../tests/fabric_world/harness.rs"]
mod harness;

#[cfg(unix)]
#[path = "../tests/fabric_bench/census.rs"]
mod census;
#[cfg(unix)]
#[path = "../tests/fabric_bench/clock.rs"]
mod clock;
#[cfg(unix)]
#[path = "../tests/fabric_bench/profile.rs"]
#[expect(
    dead_code,
    reason = "the verify perf driver also uses this shared profile table's build and CLI methods"
)]
mod profile;
#[cfg(unix)]
#[path = "../tests/fabric_bench/run.rs"]
mod run;
#[cfg(unix)]
#[path = "../tests/fabric_bench/spec.rs"]
mod spec;
#[cfg(unix)]
#[path = "../tests/fabric_bench/stats.rs"]
mod stats;

/// The measurement. See the module note.
#[cfg(unix)]
fn main() -> Result<(), String> {
    let spec = spec::from_env()?;
    std::fs::create_dir_all(&spec.out)
        .map_err(|e| format!("create the report dir {}: {e}", spec.out.display()))?;
    let report = run::measure(&spec);
    let json = aterm_json::to_string(&report.json).map_err(|e| format!("serialize report: {e}"))?;
    let json_path = spec.out.join("fabric.json");
    let md_path = spec.out.join("fabric.md");
    std::fs::write(&json_path, json).map_err(|e| format!("write {}: {e}", json_path.display()))?;
    std::fs::write(&md_path, &report.markdown)
        .map_err(|e| format!("write {}: {e}", md_path.display()))?;
    println!("{}", report.markdown);
    eprintln!(
        "fabric bench: wrote {} and {}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() -> Result<(), String> {
    Err("fabric measurement requires Unix".to_string())
}
