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

/// Resolves the underlying `git` executable once per process, bypassing `/usr/bin/git` on macOS.
fn path() -> &'static Path {
    static GIT: OnceLock<PathBuf> = OnceLock::new();
    GIT.get_or_init(|| {
        let on_path = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("git"))
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
    })
}
