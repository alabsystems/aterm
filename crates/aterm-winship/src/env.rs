// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The isolated environment a smoked build runs in: an allowlist of machine
//! variables, and every per-user root pointed into one scratch directory.
//!
//! A release image has no state-directory seam (`ATERM_STATE_HOME` is a dev
//! seam), so it is kept away from the owner's instance by what it reads: its
//! control-socket directory comes from `TMP`, `TEMP`, then `LOCALAPPDATA`, and
//! its state and configuration from `USERPROFILE`, `APPDATA` and
//! `LOCALAPPDATA`. Redirect those five and pass an explicit `--control-sock`,
//! and nothing of the owner's is opened.

use std::path::{Path, PathBuf};

/// The variables of the caller's environment a smoked build keeps. Everything
/// else is dropped: a session id, a control socket or an agent's variables of
/// the terminal the smoke runs in must not reach the instance under test.
pub const KEPT: [&str; 16] = [
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATH",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "OS",
];

/// The variables without which Windows resolves `%SystemDrive%` and
/// `%SystemRoot%` to nothing and writes a folder literally named so.
pub const REQUIRED: [&str; 2] = ["SystemRoot", "SystemDrive"];

/// The per-user roots of one smoke, all inside `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    /// The scratch root the smoke deletes when it ends.
    pub root: PathBuf,
    /// `TMP` and `TEMP`.
    pub tmp: PathBuf,
    /// `LOCALAPPDATA`.
    pub local: PathBuf,
    /// `APPDATA`.
    pub roaming: PathBuf,
    /// `USERPROFILE`, and the working directory of the instance and its shell.
    pub home: PathBuf,
}

impl Roots {
    /// The roots under `root`.
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            tmp: root.join("tmp"),
            local: root.join("local"),
            roaming: root.join("roaming"),
            home: root.join("home"),
        }
    }

    /// Every directory to create before the instance starts.
    #[must_use]
    pub fn dirs(&self) -> [&Path; 4] {
        [&self.tmp, &self.local, &self.roaming, &self.home]
    }
}

/// The value of `name` in `env`, by Windows' rule: names differ only in case.
#[must_use]
pub fn lookup<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// The environment of a smoked build: the [`KEPT`] variables of `parent`, then
/// the five per-user roots of `roots`.
///
/// # Errors
/// `parent` lacks one of [`REQUIRED`]: the smoke could not run without writing
/// a literal `%SystemDrive%` folder.
pub fn isolated(
    parent: &[(String, String)],
    roots: &Roots,
) -> Result<Vec<(String, String)>, String> {
    for name in REQUIRED {
        if lookup(parent, name).is_none_or(str::is_empty) {
            return Err(format!(
                "the environment has no {name}: a build started without it writes a literal %{name}% folder"
            ));
        }
    }
    let mut env: Vec<(String, String)> = KEPT
        .iter()
        .filter_map(|name| lookup(parent, name).map(|v| ((*name).to_string(), v.to_string())))
        .collect();
    let text = |p: &Path| p.to_string_lossy().into_owned();
    for (name, dir) in [
        ("TMP", &roots.tmp),
        ("TEMP", &roots.tmp),
        ("LOCALAPPDATA", &roots.local),
        ("APPDATA", &roots.roaming),
        ("USERPROFILE", &roots.home),
    ] {
        env.push((name.to_string(), text(dir)));
    }
    Ok(env)
}

/// The longest control-socket path this tool passes, in bytes. `AF_UNIX`'s
/// `sun_path` holds 108 bytes with its terminator; aterm-gui refuses a longer
/// path and serves no control socket.
pub const SOCK_PATH_MAX: usize = 100;

/// Where the control socket of a smoke goes: inside `root` when that path is
/// short enough to bind, else in a directory named `tag` under `short` (the
/// system temporary directory). The second is `None` when it lies in `root`;
/// otherwise it is the extra directory the smoke creates and deletes.
///
/// # Errors
/// Neither path fits [`SOCK_PATH_MAX`].
pub fn sock_path(
    root: &Path,
    short: &Path,
    tag: &str,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let fits = |p: &Path| p.as_os_str().len() <= SOCK_PATH_MAX;
    let inside = root.join("s").join("c.sock");
    if fits(&inside) {
        return Ok((inside, None));
    }
    let dir = short.join(tag);
    let outside = dir.join("c.sock");
    if fits(&outside) {
        return Ok((outside, Some(dir)));
    }
    Err(format!(
        "no control-socket path of at most {SOCK_PATH_MAX} bytes under {} or {}: pass a shorter --scratch",
        root.display(),
        short.display()
    ))
}

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
