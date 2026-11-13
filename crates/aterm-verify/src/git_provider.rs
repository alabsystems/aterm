// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The invocation's one Git provider, shared by metadata reads and journal writes.
//! Git on the provided absolute PATH is already the verifier's journal/push trust
//! boundary. Apple's fixed launcher additionally requires an activated Xcode install.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static PROVIDER: OnceLock<Option<PathBuf>> = OnceLock::new();

fn resolve(path: &OsStr) -> Option<PathBuf> {
    let file = aterm_toolchain::discovery::tool_file("git");
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file))
        .find(|candidate| aterm_toolchain::discovery::is_executable_file(candidate))
}

fn selected<'a>(cell: &'a OnceLock<Option<PathBuf>>, path: &OsStr) -> Option<&'a Path> {
    cell.get_or_init(|| resolve(path)).as_deref()
}

/// Pin the first absolute executable Git from this process's PATH before dispatch.
/// A missing provider stays missing for the invocation; no OS launcher or relative
/// path can silently replace it. This selects a trusted local tool, not a proof.
#[must_use]
pub fn pin() -> Option<&'static Path> {
    selected(&PROVIDER, &std::env::var_os("PATH").unwrap_or_default())
}

pub(crate) fn program() -> &'static Path {
    // The process's PATH selects the provider, never a caller's own tool PATH:
    // otherwise whichever caller ran first would decide it for the invocation.
    // The existing command builders return a Command rather than a Result. An
    // empty program cannot dispatch a fallback; its I/O error keeps those callers
    // fail-closed. Checkout-requiring CLI paths diagnose absence before spawning.
    pin().unwrap_or_else(|| Path::new(""))
}

pub(crate) fn command() -> aterm_uds::spawnfd::Command {
    aterm_uds::spawnfd::command(program())
}

#[cfg(test)]
#[path = "git_provider_tests.rs"]
mod tests;
