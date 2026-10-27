// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh's git (design §6.9): the isolated runner every git child of agh or its callers runs
//! under, the credential helper git calls back, and the grants that let the helper answer a
//! release-tier owner or a push's writer. This module runs the `git` program. It is not
//! `agh::github::git`, M1's REST git-data API (commits, refs, trees).

pub mod credential;
pub mod grant;
#[cfg(test)]
mod grant_conformance_tests;
mod run;

pub use run::{GitOp, GitOut, GitRun, HelperCommand};
// `run` stays a private module; its runner is re-exported to the crate alone, so
// `Github::run_git` (client.rs) calls it as `crate::git::run(self, run)`. The function and
// the module share the name in different namespaces.
pub(crate) use run::run;

/// The username the helper answers: GitHub ignores it for a token, and a fixed one keeps
/// the Basic credential's shape known to the Redactor.
pub const GIT_USERNAME: &str = "x-access-token";
/// The most a git run may take, unless `GitRun::with_budget` says otherwise: 10 minutes.
pub const GIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);
/// The longest a grant lives: 15 minutes.
pub const GRANT_MAX: std::time::Duration = std::time::Duration::from_secs(900);
/// How far up the process tree the helper looks for the grant's spawner.
pub const ANCESTRY_DEPTH: usize = 8;
