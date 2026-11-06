// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Git queries on a checkout through [`aterm_uds::spawnfd::command`].

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use aterm_uds::deadline::{CompleteOutputError, MAX_COMPLETE_STDOUT_BYTES, RunError};

use crate::error::{Error, Result};

/// A caller-owned, frozen repository admitted by Agh's native local-read guard.
/// No credentials or network route is loaded. Admission does not freeze the caller's
/// directory against concurrent replacement; publication owns that source lifetime.
pub struct SourceRepo {
    root: std::path::PathBuf,
}

impl SourceRepo {
    /// Admit a local repository without changing its configuration, refs or objects.
    /// # Errors
    /// Native source/discovery/config/physical-path admission refusal.
    pub fn open(root: &Path) -> Result<Self> {
        let root =
            std::fs::canonicalize(root).map_err(|e| Error::io("resolve source repository", &e))?;
        let gh = agh::GithubBuilder::new(agh::policy::Router::empty())
            .fx(std::sync::Arc::new(agh::Fx::live()))
            .build();
        let slug = agh::Slug::new("alab", "local").map_err(|e| Error::refused(e.to_string()))?;
        gh.run_git(&agh::git::GitRun::check_local(&slug, &root))
            .map_err(|e| Error::refused(format!("source read admission: {e}")))?;
        Ok(Self { root })
    }

    /// The canonical repository admitted for these object reads.
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Read one immutable object query with no ambient Git routing/replacement/lazy fetch.
    /// # Errors
    /// Missing objects, invalid output, spawn or bounded collection failure.
    pub(crate) fn query(&self, args: &[&str]) -> Result<String> {
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            cmd.env_remove(key);
        }
        cmd.args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(&self.root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
        let out = output_within(&mut cmd, LOCAL_GIT_BOUND)
            .map_err(|e| Error::cannot_run(format!("read immutable source object: {e}")))?;
        if !out.status.success() {
            return Err(Error::refused("immutable source objects could not be read"));
        }
        String::from_utf8(out.stdout).map_err(|_| Error::refused("source metadata is not UTF-8"))
    }
}

/// How long a local git query or checkout ([`run`]) may take before it is taken for hung and
/// killed: ten minutes, where a checkout of a compiler's tree on a loaded machine takes one or two.
pub const LOCAL_GIT_BOUND: Duration = Duration::from_mins(10);

/// Runs a git `cmd` to its end within `within`, its stdout whole: more than
/// [`MAX_COMPLETE_STDOUT_BYTES`] is refused, never a kept tail read as the whole (a first-parent
/// `rev-list` prints its newest commits first, which a tail drops), and a git that has not ended
/// by then is killed with its process group.
/// # Errors
/// A spawn refusal ([`RunError::Spawn`]), the deadline, or a stdout that is not whole.
pub(crate) fn output_within(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: Duration,
) -> std::result::Result<Output, CompleteOutputError> {
    aterm_uds::deadline::output_until_complete_stdout_with_limit(
        cmd,
        Instant::now() + within,
        MAX_COMPLETE_STDOUT_BYTES,
    )
}

/// `git -C <dir> <args…>`'s trimmed stdout, within [`LOCAL_GIT_BOUND`] ([`output_within`]).
/// # Errors
/// A could-not-run when git cannot be started, does not end in time or prints more than a whole
/// stdout can hold, or a refusal when git fails.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    let out = output_within(&mut cmd, LOCAL_GIT_BOUND).map_err(|e| match e {
        CompleteOutputError::Run(RunError::Spawn(e)) => Error::io("spawn git", &e),
        other => Error::cannot_run(format!(
            "`git -C {} {}`: {other}",
            dir.display(),
            args.join(" ")
        )),
    })?;
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
