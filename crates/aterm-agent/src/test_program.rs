// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test executable paths: kept signed copies on Apple hosts, original paths elsewhere.

use std::path::{Path, PathBuf};

/// Use the kept signed program on Apple hosts and the original path elsewhere.
pub(crate) fn kept(from: impl AsRef<Path>) -> PathBuf {
    aterm_uds::deadline::stand_in(from.as_ref())
}

/// Resolve developer Git before copying it: Apple's dispatcher depends on its image name.
/// The existing fixture resolver's xcrun capture remains its inherited setup boundary.
pub(crate) fn git() -> PathBuf {
    let resolved = aterm_uds::fixture_git::git().get_program().to_owned();
    kept(Path::new(&resolved))
}

/// Where the developer Git that [`git`] copies keeps its scripted commands (`git submodule` and
/// the rest of `libexec/git-core`): Apple's Git finds that directory beside its own executable,
/// so its kept copy, elsewhere, finds none. `None` where no such directory sits beside it (a Git
/// that keeps the path it was built with, or one that runs in place).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn git_exec_path() -> Option<PathBuf> {
    if !cfg!(target_vendor = "apple") {
        return None;
    }
    let resolved = PathBuf::from(aterm_uds::fixture_git::git().get_program());
    let dir = resolved.parent()?.parent()?.join("libexec/git-core");
    dir.is_dir().then_some(dir)
}

/// Where the developer Git that [`git`] copies keeps its init templates (`hooks/`, `info/`
/// and the rest of `share/git-core/templates`): Apple's Git reads them from a path compiled
/// into it, which its kept copy does not move to, so an `init` from the copy lays no
/// `hooks/` unless `GIT_TEMPLATE_DIR` names this. `None` on the same terms as
/// [`git_exec_path`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn git_template_dir() -> Option<PathBuf> {
    if !cfg!(target_vendor = "apple") {
        return None;
    }
    let resolved = PathBuf::from(aterm_uds::fixture_git::git().get_program());
    let dir = resolved
        .parent()?
        .parent()?
        .join("share/git-core/templates");
    dir.is_dir().then_some(dir)
}
