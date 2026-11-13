// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A dev binary of the selected checkout that a step hands a child it starts: the source
//! signer's helper (`atpkg-keys`) and the publication engine's GitHub client (`agh`).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// One such binary: the `[[bin]]` of its package's own name, the variable a caller names one of
/// its own with, and what a refusal calls it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bin {
    pub(crate) package: &'static str,
    pub(crate) env: &'static str,
    pub(crate) what: &'static str,
}

/// Build `bin` through the selected checkout's Cargo graph: `build` runs `targo` with the
/// arguments it is handed and answers its stdout, cargo's JSON messages. An explicit binary
/// (`explicit`, the caller's `bin.env`) remains authoritative and is never replaced.
/// `env` describes the captured Cargo layout; the callback applies its supplied plan to the
/// compiler command so later process-environment changes cannot choose a different layout.
pub(crate) fn prepare(
    repo: &Path,
    bin: &Bin,
    explicit: Option<&OsStr>,
    env: &aterm_verify::EnvSnapshot,
    build: impl FnOnce(&[OsString], &aterm_verify::NativeBuildPlan) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    prepare_with_jobs(repo, bin, explicit, env, None, build)
}

/// Prepare a checkout helper within an existing caller's CPU allocation.
pub(crate) fn prepare_with_jobs(
    repo: &Path,
    bin: &Bin,
    explicit: Option<&OsStr>,
    env: &aterm_verify::EnvSnapshot,
    jobs: Option<u32>,
    build: impl FnOnce(&[OsString], &aterm_verify::NativeBuildPlan) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
    if jobs == Some(0) {
        return Err("a checkout helper needs a positive CPU allocation".into());
    }
    if let Some(path) = explicit.filter(|value| !value.is_empty()) {
        return ready(bin, absolute(path, bin.env)?);
    }
    let mut args: Vec<OsString> = [
        "--unverified",
        "build",
        "--locked",
        "--profile",
        "gate",
        "-p",
        bin.package,
        "--bin",
        bin.package,
        "--message-format=json",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if let Some(jobs) = jobs {
        args.extend([OsString::from("--jobs"), jobs.to_string().into()]);
    }
    // The selected driver may be the compiler's own targo, rather than the
    // atpkg shim. Give it the same quiet shared cache layout as verification.
    let plan = aterm_verify::NativeBuildPlan::capture(env, repo)
        .map_err(|e| format!("select {} build directory: {e}", bin.what))?;
    if let Some(layout) = plan.build_dir() {
        args.extend([OsString::from("--config"), layout.into()]);
    }
    // Final helper artifacts must stay quiet even when the selected checkout has no target
    // configuration of its own. An inherited caller-selected target still wins above.
    args.extend([
        OsString::from("--target-dir"),
        plan.target().as_os_str().to_os_string(),
    ]);
    args.push("--manifest-path".into());
    args.push(repo.join("Cargo.toml").into_os_string());
    plan.prepare()
        .map_err(|e| format!("prepare {} build directory: {e}", bin.what))?;
    ready(bin, built(repo, bin, &build(&args, &plan)?)?)
}

fn ready(bin: &Bin, path: PathBuf) -> Result<PathBuf, String> {
    if aterm_toolchain::is_executable_file(&path) {
        Ok(path)
    } else {
        Err(format!(
            "{} {} is not an executable file",
            bin.what,
            path.display()
        ))
    }
}

/// The one executable of `bin` that `output`, cargo's JSON messages, reports this checkout's
/// package built.
pub(crate) fn built(repo: &Path, bin: &Bin, output: &[u8]) -> Result<PathBuf, String> {
    let manifest = repo.join("crates").join(bin.package).join("Cargo.toml");
    let mut found = std::collections::BTreeSet::new();
    for line in output.split(|byte| *byte == b'\n') {
        if !line
            .windows(b"compiler-artifact".len())
            .any(|w| w == b"compiler-artifact")
            || !line
                .windows(bin.package.len())
                .any(|w| w == bin.package.as_bytes())
        {
            continue;
        }
        let Ok(row) = aterm_json::from_slice::<aterm_json::Value>(line) else {
            continue;
        };
        if row["reason"].as_str() == Some("compiler-artifact")
            && row["target"]["name"].as_str() == Some(bin.package)
            && row["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
            && row["manifest_path"].as_str().map(Path::new) == Some(manifest.as_path())
            && let Some(exe) = row["executable"]
                .as_str()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        {
            found.insert(exe);
        }
    }
    if found.len() != 1 {
        return Err(format!(
            "the {} build did not identify exactly one {} from this checkout",
            bin.package, bin.what
        ));
    }
    found
        .pop_first()
        .ok_or_else(|| format!("the {} build reported no {}", bin.package, bin.what))
}

/// `value` as a path, when it is absolute; else `name` must name one.
pub(crate) fn absolute(value: &OsStr, name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{name} must name an absolute executable path"))
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "checkout_bin_tests.rs"]
mod tests;
