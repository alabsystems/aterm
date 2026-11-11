// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT MOVES A CARGO RUN OFF THE `trustc` BESIDE ITS `targo`, in cargo's own order: `$RUSTC`,
//! `$CARGO_BUILD_RUSTC`, then `build.rustc` in each directory's `.cargo/config` (or
//! `.cargo/config.toml`) from the run's directory up, nearest first, then `$CARGO_HOME`'s. The
//! first one set is the compiler every compile child of the run takes ([`Mover::resolve`] finds it
//! as cargo does). `aterm help rust` lists them; verify and `atpkg-publish adopt` hold the first to
//! a tree's `trust.lock` ([`lock_refusal`]), since a compiler beside the locked `targo` is only
//! the lock's when nothing names another.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::trust_lock::TrustLock;

/// The variables that name a cargo run's compiler, in cargo's precedence.
pub const RUSTC_VARS: [&str; 2] = ["RUSTC", "CARGO_BUILD_RUSTC"];

/// The most of a cargo config file this reads.
const MAX_CONFIG_BYTES: usize = 1 << 20;

/// One setting that names the compiler a cargo run takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mover {
    /// Where it is set: `$RUSTC`, `$CARGO_BUILD_RUSTC`, or `build.rustc in <file>`.
    pub what: String,
    /// Its value, verbatim.
    pub value: String,
    /// What a value with a path separator is relative to: the run's directory for a variable,
    /// the directory that holds a config file's directory for a file (cargo's config-relative
    /// paths).
    pub base: PathBuf,
}

impl Mover {
    /// The compiler it names, found as cargo finds it in a child whose `PATH` is `path_env`: a
    /// bare name on that `PATH`, a value with a separator joined to [`Mover::base`]. `None` when
    /// no executable is there.
    #[must_use]
    pub fn resolve(&self, path_env: &OsStr) -> Option<PathBuf> {
        if self.value.chars().any(std::path::is_separator) {
            let path = self.base.join(&self.value);
            return crate::discovery::is_executable_file(&path).then_some(path);
        }
        std::env::split_paths(path_env).find_map(|dir| {
            [
                dir.join(&self.value),
                dir.join(crate::discovery::tool_file(&self.value)),
            ]
            .into_iter()
            .find(|path| crate::discovery::is_executable_file(path))
        })
    }

    /// What takes it away: the variable to unset, or the file to drop `build.rustc` from.
    #[must_use]
    pub fn fix(&self) -> String {
        match self.what.strip_prefix('$') {
            Some(var) => format!("unset {var}"),
            None => format!("remove {}", self.what.replacen(" in ", " from ", 1)),
        }
    }
}

/// What names the compiler of a cargo run in `cwd` ([`RUSTC_VARS`] through `var`, then the
/// config files up from `cwd`, then `cargo_home`'s), in cargo's precedence: the first is the one
/// the run takes. A directory holding both `config` and `config.toml` is read for `config`, as
/// cargo reads it; each file is read once however it is reached.
#[must_use]
pub fn rustc_movers(
    cwd: Option<&Path>,
    var: &dyn Fn(&str) -> Option<String>,
    cargo_home: Option<&Path>,
) -> Vec<Mover> {
    let mut found = Vec::new();
    for key in RUSTC_VARS {
        if let Some(value) = var(key).filter(|v| !v.is_empty()) {
            found.push(Mover {
                what: format!("${key}"),
                value,
                base: cwd.map(Path::to_path_buf).unwrap_or_default(),
            });
        }
    }
    let mut dirs: Vec<PathBuf> = cwd
        .into_iter()
        .flat_map(Path::ancestors)
        .map(|d| d.join(".cargo"))
        .collect();
    dirs.extend(cargo_home.map(Path::to_path_buf));
    let mut seen = std::collections::BTreeSet::new();
    for dir in dirs {
        let Some(file) = ["config", "config.toml"]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|file| file.is_file())
        else {
            continue;
        };
        if !seen.insert(file.clone()) {
            continue;
        }
        if let Some(value) = crate::discovery::read_bounded_text(&file, MAX_CONFIG_BYTES)
            .as_deref()
            .and_then(build_rustc_in)
        {
            found.push(Mover {
                what: format!("build.rustc in {}", file.display()),
                value,
                base: dir.parent().unwrap_or(&dir).to_path_buf(),
            });
        }
    }
    found
}

/// The `rustc` key of a cargo config's `[build]` table (or a top-level dotted `build.rustc`),
/// verbatim between its quotes; the last one wins.
#[must_use]
pub fn build_rustc_in(text: &str) -> Option<String> {
    let mut table = String::new();
    let mut found = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            table = header
                .split(']')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !((table == "build" && key == "rustc") || (table.is_empty() && key == "build.rustc")) {
            continue;
        }
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|q| *q == '"' || *q == '\'') else {
            continue;
        };
        if let Some(end) = value[1..].find(quote) {
            let inner = &value[1..=end];
            if !inner.is_empty() {
                found = Some(inner.to_string());
            }
        }
    }
    found
}

/// THE COMPILER A CARGO RUN TAKES, HELD TO A LOCK (A16): `None` when nothing names one (the run
/// compiles with the `trustc` beside its `targo`) or when the first of `movers`, resolved on the
/// child's `path_env`, answers `lock`'s `compiler_commit`; otherwise why, naming the setting,
/// what it resolved to and what takes it away.
#[must_use]
pub fn lock_refusal(
    movers: &[Mover],
    path_env: &OsStr,
    lock_path: &Path,
    lock: &TrustLock,
) -> Option<String> {
    let winner = movers.first()?;
    let resolved = winner.resolve(path_env);
    let reported = resolved
        .as_deref()
        .and_then(crate::trust_lock::compiler_commit_of);
    if reported.as_deref() == Some(lock.compiler_commit.as_str()) {
        return None;
    }
    let answers = match (&resolved, &reported) {
        (None, _) => "names no executable".to_string(),
        (Some(path), None) => format!("({}) answers no commit-hash", path.display()),
        (Some(path), Some(commit)) => format!("({}) answers commit {commit}", path.display()),
    };
    Some(format!(
        "{} = {} {answers}, and {} names compiler commit {}: every compile child would build \
         with it. Fix: {}",
        winner.what,
        winner.value,
        lock_path.display(),
        lock.compiler_commit,
        winner.fix()
    ))
}

#[cfg(test)]
#[path = "rustc_movers_tests.rs"]
mod tests;
