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
/// When git fails or prints something that is not a count.
pub fn rev_count(dir: &Path, rev: &str) -> Result<u64> {
    let n = run(dir, &["rev-list", "--count", rev])?;
    n.parse().map_err(|_| {
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

/// Whether `dir` is a work tree with no change to a tracked or untracked file.
/// # Errors
/// When `dir` is not a work tree.
pub fn is_clean(dir: &Path) -> Result<bool> {
    run(dir, &["rev-parse", "--is-inside-work-tree"])?;
    Ok(run(dir, &["status", "--porcelain"])?.is_empty())
}
