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
pub(crate) fn prepare(
    repo: &Path,
    bin: &Bin,
    explicit: Option<&OsStr>,
    build: impl FnOnce(&[OsString]) -> Result<Vec<u8>, String>,
) -> Result<PathBuf, String> {
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
    // The selected driver may be the compiler's own targo, rather than the
    // atpkg shim. Give it the same quiet shared cache layout as verification.
    let env = aterm_verify::EnvSnapshot::capture();
    let target = env
        .cargo_target_dir
        .clone()
        .unwrap_or_else(|| repo.join("target"));
    if let Some(layout) = aterm_verify::main_build_dir(&env, repo)
        .or_else(|| aterm_verify::fallback_build_dir(&env, repo, &target))
    {
        args.extend([OsString::from("--config"), layout.into()]);
    }
    args.push("--manifest-path".into());
    args.push(repo.join("Cargo.toml").into_os_string());
    ready(bin, built(repo, bin, &build(&args)?)?)
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
