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
/// A test runs it through [`kept_git_command`]; where a test hands only its path on (to the
/// code under test, or into a script another Git runs), the command is built there.
pub(crate) fn git() -> PathBuf {
    let resolved = aterm_uds::fixture_git::git().get_program().to_owned();
    kept(Path::new(&resolved))
}

/// [`git`] as the command every test that runs the kept Git builds on: told where the developer
/// Git it copies keeps its scripted commands (`GIT_EXEC_PATH`, [`git_exec_path`]) and its init
/// templates (`GIT_TEMPLATE_DIR`, [`git_template_dir`]) where those directories exist, so a
/// `git submodule` runs and an `init` lays `hooks/` from the copy as from the original.
pub(crate) fn kept_git_command() -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command(git());
    if let Some(exec_path) = git_exec_path() {
        cmd.env("GIT_EXEC_PATH", exec_path);
    }
    if let Some(template_dir) = git_template_dir() {
        cmd.env("GIT_TEMPLATE_DIR", template_dir);
    }
    cmd
}

/// Where the developer Git that [`git`] copies keeps its scripted commands (`git submodule` and
/// the rest of `libexec/git-core`), as that Git answers `git --exec-path`
/// ([`aterm_uds::fixture_git::exec_path`]): a Git finds that directory relative to its own
/// executable, so its kept copy, elsewhere, finds none. `None` off Apple, where [`git`] is the
/// original, and where that Git answers none.
fn git_exec_path() -> Option<PathBuf> {
    if !cfg!(target_vendor = "apple") {
        return None;
    }
    let dir = aterm_uds::fixture_git::exec_path()?;
    dir.is_dir().then(|| dir.to_path_buf())
}

/// Where the developer Git that [`git`] copies keeps its init templates (`hooks/`, `info/`
/// and the rest of `share/git-core/templates`, beside the `libexec/git-core` that
/// [`git_exec_path`] names): a Git finds them relative to its own executable, which its kept
/// copy does not move to, so an `init` from the copy lays no `hooks/` unless
/// `GIT_TEMPLATE_DIR` names this. `None` on the same terms as [`git_exec_path`].
fn git_template_dir() -> Option<PathBuf> {
    let dir = git_exec_path()?
        .parent()?
        .parent()?
        .join("share/git-core/templates");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
#[path = "test_program_tests.rs"]
mod tests;
