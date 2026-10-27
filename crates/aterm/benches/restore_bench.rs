// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESTORE MEASUREMENT. `verify perf restore-e2e` runs it with the words of one run in
//! `RESTORE_BENCH` (`spec::parse`): `out=` (required), `sessions=`, `rounds=`, `lines=`, `arm=`
//! (clean, copy or lookalike) and the look-alike arm's `lookalike_kib=`. Each round SIGKILLs a
//! headless instance holding the sessions and times its successor's recovery of them, from the
//! successor's spawn to its tabs back (`restore_bench/run.rs`). It measures, and never asserts a
//! floor. macOS only, as the crash-restore live lane is.

#[cfg(target_os = "macos")]
#[path = "../tests/support/child_output.rs"]
mod child_output;
#[cfg(target_os = "macos")]
#[path = "../tests/support/ctl_accept.rs"]
mod ctl_accept;
#[cfg(target_os = "macos")]
#[path = "../tests/support/headless_boot.rs"]
mod headless_boot;
#[cfg(target_os = "macos")]
#[path = "../tests/support/keeper_world.rs"]
mod keeper_world;
#[cfg(target_os = "macos")]
#[path = "../tests/support/launch_isolation.rs"]
mod launch_isolation;
#[cfg(target_os = "macos")]
#[path = "../tests/support/live_ctl.rs"]
mod live_ctl;
#[cfg(target_os = "macos")]
#[path = "../tests/fabric_bench/profile.rs"]
#[expect(
    dead_code,
    reason = "the verify perf driver also uses this shared profile table's build and CLI methods"
)]
mod profile;
#[cfg(target_os = "macos")]
#[path = "../tests/restore_bench/run.rs"]
mod run;
#[cfg(target_os = "macos")]
#[path = "../tests/restore_bench/spec.rs"]
mod spec;
#[cfg(target_os = "macos")]
#[path = "../tests/fabric_bench/stats.rs"]
#[expect(
    dead_code,
    reason = "the fabric bench's table row; this bench writes its own tables"
)]
mod stats;

/// The measurement.
#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    let spec = spec::from_env()?;
    std::fs::create_dir_all(&spec.out)
        .map_err(|e| format!("create the report dir {}: {e}", spec.out.display()))?;
    let report = run::measure(&spec);
    let json = aterm_json::to_string(&report.json).map_err(|e| format!("serialize report: {e}"))?;
    let json_path = spec.out.join("restore.json");
    let md_path = spec.out.join("restore.md");
    std::fs::write(&json_path, json).map_err(|e| format!("write {}: {e}", json_path.display()))?;
    std::fs::write(&md_path, &report.markdown)
        .map_err(|e| format!("write {}: {e}", md_path.display()))?;
    println!("{}", report.markdown);
    Ok(())
}

/// Elsewhere there is no keeper's crash-restore journal to measure.
#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), String> {
    Err("the restore measurement runs on macOS, where the crash-restore live lane runs".into())
}
