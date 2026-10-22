// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (test and debug builds, never release ones): the `git` a test fixture
//! runs, the developer directory's own, isolated from the machine the tests run on.

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

/// Isolate `cmd` (a fixture's git, or a child that runs one) from this machine: no
/// inherited `GIT_*` but a trace (a hook's `GIT_DIR`, an alias's `GIT_CONFIG_PARAMETERS`),
/// no system or global config, no global ignore or attributes file (git reads those under
/// `$XDG_CONFIG_HOME` whatever `GIT_CONFIG_GLOBAL` says), and a fixed `EMAIL` for an
/// identity no fixture set. What is set on `cmd` after this still applies.
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

/// The executable a `git` from `PATH` ends up running, resolved once per process: on macOS,
/// what xcode-select's `/usr/bin/git` trampoline would exec (`xcrun --find git`), so no
/// fixture git pays a second exec; elsewhere, or when the lookup fails, that `git`.
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
