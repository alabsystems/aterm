// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh's git (design §6.9): the isolated runner every git child of agh or its callers runs
//! under, the credential helper git calls back, and the grants that let the helper answer a
//! release-tier owner or a push's writer. This module runs the `git` program. It is not
//! `agh::github::git`, M1's REST git-data API (commits, refs, trees).

pub mod credential;
mod fetch;
pub mod grant;
#[cfg(test)]
mod grant_conformance_tests;
mod run;
#[cfg(unix)]
mod source_config;

pub use fetch::{FetchBatch, FetchRow};
pub use run::{GitOp, GitOut, GitRun, HelperCommand};

/// One validation phase of an incremental native atomic ref admission.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativePushPhase {
    /// Build the complete commit/tree/tag metadata pack for BEFORE and AFTER.
    MetadataPack,
    /// Strictly verify that metadata pack and its original object IDs.
    MetadataVerify,
    /// Walk AFTER's connectivity, including referenced blob presence.
    Connectivity,
}
// `run` stays a private module; its runner is re-exported to the crate alone, so
// `Github::run_git` (client.rs) calls it as `crate::git::run(self, run)`. The function and
// the module share the name in different namespaces.
#[cfg(unix)]
pub(crate) use run::project_config_names;
pub(crate) use run::run;

/// The username the helper answers: GitHub ignores it for a token, and a fixed one keeps
/// the Basic credential's shape known to the Redactor.
pub const GIT_USERNAME: &str = "x-access-token";
/// The most a git run may take, unless `GitRun::with_budget` says otherwise: 10 minutes.
pub const GIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
/// The longest a grant lives: 15 minutes.
pub const GRANT_MAX: std::time::Duration = std::time::Duration::from_secs(900);
/// The variable that carries a grant's nonce from the runner to the helper (and to
/// `whoami --git`): one spelling on both sides of the handshake.
pub(crate) const GRANT_VAR: &str = "AGH_GIT_GRANT";
/// The variable that tells the helper its profile's token file may be loose (`1`), as the
/// runner's router read it (`legacy_loose`).
pub(crate) const LOOSE_VAR: &str = "AGH_GIT_LOOSE";
/// The variables that carry the runner's `Dirs` to the helper: its config, state and home.
pub(crate) const CONFIG_DIR_VAR: &str = "AGH_GIT_CONFIG_DIR";
/// See [`CONFIG_DIR_VAR`].
pub(crate) const STATE_DIR_VAR: &str = "AGH_GIT_STATE_DIR";
/// See [`CONFIG_DIR_VAR`].
pub(crate) const HOME_VAR: &str = "AGH_GIT_HOME";
/// The variable that carries a loopback run's `127.0.0.1:<port>` host from the runner to the
/// helper, set and read only in a test or debug build.
#[cfg(any(test, debug_assertions))]
pub(crate) const LOOPBACK_VAR: &str = "AGH_GIT_LOOPBACK";
/// How far up the process tree the helper looks for the grant's spawner.
pub const ANCESTRY_DEPTH: usize = 8;
