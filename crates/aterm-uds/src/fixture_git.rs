// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hermetic `git` command builder for test fixtures.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::spawnfd::{Command, Stdio, command};

/// `git` for a fixture's setup and reads: [`path`]'s, made [`hermetic`].
#[must_use]
pub fn git() -> Command {
    let mut git = command(path());
    hermetic(&mut git);
    git
}

/// Isolates `cmd` from the host machine's Git environment and global configuration.
pub fn hermetic(cmd: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("GIT_") && !key_text.starts_with("GIT_TRACE") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("XDG_CONFIG_HOME", "/dev/null")
        .env("EMAIL", "fixture@example.invalid")
}

/// The directory where the `git` [`git`] runs keeps its helper programs (`git --exec-path`), on
/// macOS where that Git answers it; `None` elsewhere.
#[must_use]
pub fn exec_path() -> Option<&'static Path> {
    resolved().exec_path.as_deref()
}

/// The `git` executable [`git`] runs ([`resolved`]).
fn path() -> &'static Path {
    &resolved().program
}

/// The `git` a fixture runs, and where it keeps its helpers.
struct Resolved {
    program: PathBuf,
    exec_path: Option<PathBuf>,
}

/// Resolves the underlying `git` executable once per process: the first `git` on `PATH`, and on
/// macOS, past `/usr/bin/git` (Apple's dispatcher, through `xcrun`) and past any `#!` wrapper in
/// front of Git, to Git's own executable ([`underlying`]).
fn resolved() -> &'static Resolved {
    static GIT: OnceLock<Resolved> = OnceLock::new();
    GIT.get_or_init(|| resolve(on_path()))
}

#[cfg(target_os = "macos")]
fn resolve(found: PathBuf) -> Resolved {
    underlying(found)
}

#[cfg(not(target_os = "macos"))]
fn resolve(found: PathBuf) -> Resolved {
    Resolved {
        program: found,
        exec_path: None,
    }
}

/// The first `git` on `PATH`, where on macOS `/usr/bin/git`, the dispatcher, is the developer
/// Git `xcrun` names.
fn on_path() -> PathBuf {
    let on_path = std::env::var_os("PATH")
        .and_then(|path| {
            // Windows names the program `git.exe`; a bare `git` there is
            // no file, and a caller that resolves the path would miss it.
            std::env::split_paths(&path)
                .map(|dir| dir.join(if cfg!(windows) { "git.exe" } else { "git" }))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("git"));
    if !cfg!(target_os = "macos") || on_path != Path::new("/usr/bin/git") {
        return on_path;
    }
    command("/usr/bin/xcrun")
        .args(["--find", "git"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        .filter(|found| found.is_file())
        .unwrap_or(on_path)
}

/// The Git that `found` starts, and where it keeps its helpers: `found` itself, or where `found`
/// is a wrapper (atpkg's `PATH` entry, a package's `bin/git` script), the executable Git keeps
/// beside its helpers ([`own_executable`]). On macOS 26.6 each `#!` file the kernel starts leaks
/// a kernel element (ATERM-KI-1116-1024), so a fixture that runs Git thousands of times runs Git
/// itself.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn underlying(found: PathBuf) -> Resolved {
    let exec_path = exec_path_of(&found);
    Resolved {
        program: exec_path
            .as_deref()
            .and_then(own_executable)
            .unwrap_or(found),
        exec_path,
    }
}

/// Where `git` keeps its helper programs, as it answers `git --exec-path` with no inherited Git
/// environment ([`hermetic`]): an absolute directory, or `None`.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn exec_path_of(git: &Path) -> Option<PathBuf> {
    let mut query = command(git);
    hermetic(&mut query);
    let out = query
        .arg("--exec-path")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let text = String::from_utf8(out.stdout).ok()?;
    let dir = PathBuf::from(text.strip_suffix('\n').unwrap_or(&text));
    (dir.is_absolute() && dir.is_dir()).then_some(dir)
}

/// `exec_path`'s `git`, at its real path: a regular file with an execute bit that is no `#!`
/// script, or `None`.
#[cfg(all(unix, any(target_os = "macos", test)))]
fn own_executable(exec_path: &Path) -> Option<PathBuf> {
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;

    let git = exec_path.join("git").canonicalize().ok()?;
    let meta = std::fs::metadata(&git).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return None;
    }
    let mut head = [0; 2];
    std::fs::File::open(&git).ok()?.read_exact(&mut head).ok()?;
    (head != *b"#!").then_some(git)
}

#[cfg(all(test, unix))]
#[path = "fixture_git_tests.rs"]
mod tests;
