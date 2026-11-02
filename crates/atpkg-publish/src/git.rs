// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Git queries on a checkout through [`aterm_uds::spawnfd::command`].

use std::path::Path;

use crate::error::{Error, Result};

/// `git -C <dir> <args…>`'s trimmed stdout.
/// # Errors
/// A could-not-run when git cannot be started, or a refusal when git fails.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| Error::io("spawn git", &e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(Error::refused(format!(
            "`git -C {} {}` failed: {}",
            dir.display(),
            args.join(" "),
            stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(no message)")
        )))
    }
}

/// The build number of `rev` in `dir`.
/// # Errors
/// When the repository is shallow, git fails, or its output is not recognized.
pub fn rev_count(dir: &Path, rev: &str) -> Result<u64> {
    match run(dir, &["rev-parse", "--is-shallow-repository"])?.as_str() {
        "false" => {}
        "true" => {
            return Err(Error::refused(format!(
                "{} is shallow; publication build numbers require complete history. \
                 Use a full clone or fetch --unshallow before publishing",
                dir.display()
            )));
        }
        other => {
            return Err(Error::refused(format!(
                "`git rev-parse --is-shallow-repository` in {} printed `{other}`",
                dir.display()
            )));
        }
    }
    let n = run(dir, &["rev-list", "--count", rev])?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
        .ok_or_else(|| {
            Error::refused(format!(
                "`git rev-list --count {rev}` in {} printed `{n}`",
                dir.display()
            ))
        })
}

/// The full sha `HEAD` names in `dir`.
/// # Errors
/// When git fails.
pub fn head(dir: &Path) -> Result<String> {
    run(dir, &["rev-parse", "HEAD"])
}

/// `git status --porcelain` with every untracked file and submodule change listed whatever
/// the repository's or the user's config says (`status.showUntrackedFiles=no` and
/// `diff.ignoreSubmodules` would otherwise hide them), plus `extra`.
const STATUS: [&str; 4] = [
    "status",
    "--porcelain",
    "--untracked-files=all",
    "--ignore-submodules=none",
];

/// Whether `dir` is a work tree with no change to a tracked or untracked file, or to a
/// submodule, whatever the config says.
/// # Errors
/// When `dir` is not a work tree.
pub fn is_clean(dir: &Path) -> Result<bool> {
    run(dir, &["rev-parse", "--is-inside-work-tree"])?;
    Ok(run(dir, &STATUS)?.is_empty())
}

/// The first path that keeps `dir` from being a fresh clone's tree, or `None`: a change to a
/// tracked or untracked file or a submodule ([`is_clean`]), or an ignored file anywhere but the
/// top-level `target/` (an ignored `.cargo/config.toml`, `build.rs` or source file is read by
/// a build exactly as a tracked one is, and an ignore rule can come from the user's own
/// `core.excludesFile`). Index hints that can hide tracked changes also prevent publication;
/// this query never clears them in the caller's checkout.
/// # Errors
/// When `dir` is not a work tree.
pub fn is_pristine(dir: &Path) -> Result<Option<String>> {
    run(dir, &["rev-parse", "--is-inside-work-tree"])?;
    // `-v` lowercases assume-unchanged entries; `S` names skip-worktree entries.
    // A clean status is not evidence about the contents Git was told not to inspect.
    for args in [
        &["ls-files", "-v", "-z"][..],
        &["ls-files", "-v", "-z", "--recurse-submodules"][..],
    ] {
        let entries = run(dir, args)?;
        if let Some(entry) = entries.split('\0').find(|entry| {
            entry
                .as_bytes()
                .first()
                .is_some_and(|flag| flag.is_ascii_lowercase() || *flag == b'S')
        }) {
            return Ok(Some(format!("index hint hides tracked input: {entry:?}")));
        }
    }
    let mut args = STATUS.to_vec();
    args.push("--ignored=matching");
    let status = run(dir, &args)?;
    Ok(status
        .lines()
        .find(|line| *line != "!! target/")
        .map(str::to_string))
}
