// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The isolated git runner (design §6.9): what runs, with which environment, as whom, and
//! what it printed. Unix only (plan §1, rule 12).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::client::Github;
use crate::error::{Error, Result};
use crate::policy::Named;
use crate::slug::Slug;

use super::GIT_BUDGET;

/// What the runner runs. There is no free-form argv.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitOp {
    /// `git clone --quiet [--depth N] -- <url> <dest>`, `<dest>` absent or empty.
    Clone {
        /// Where to.
        dest: PathBuf,
        /// `--depth`.
        depth: Option<u32>,
    },
    /// `git -C <dir> fetch --quiet --tags --force --recurse-submodules=no <url>
    /// +refs/heads/*:refs/remotes/origin/*`: atpkg-publish's `fetch_repo`, by URL, never by
    /// the remote's name. `--recurse-submodules=no` overrides the clone's own
    /// `submodule.recurse` and `fetch.recurseSubmodules`, so no submodule is fetched over a
    /// URL or a transport the runner did not choose while `AGH_GIT_GRANT` is set.
    Fetch {
        /// The clone.
        dir: PathBuf,
    },
    /// `git ls-remote --refs <url> [<ref>]`.
    LsRemote {
        /// One ref, or every one.
        reference: Option<String>,
    },
    /// `git push --porcelain --force-with-lease=<ref>:<expect> <url> :<ref>`, from an empty
    /// scratch repository (`git init --quiet --bare` in the run's private scratch), so no
    /// local ref or hook is near it. It speaks as the owner's writer. The runner refuses it
    /// (Usage, before anything runs) when `reference` fails the reference rule or `expect` is
    /// not 40 lowercase hex, because `GitRun` is public and both go into the argv:
    /// `Refs::delete_expect` checks them first, and the runner checks again.
    PushDelete {
        /// The ref.
        reference: String,
        /// The sha it must point at.
        expect: String,
    },
}

/// One isolated git run on one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GitRun {
    /// The repository.
    pub slug: Slug,
    /// What runs.
    pub op: GitOp,
    /// How the destination was named: a release-tier owner needs `Named::Literal`.
    pub named: Named,
    /// The run's bound (`GIT_BUDGET` by default).
    pub budget: Duration,
}

impl GitRun {
    /// `op` on `slug`, `Named::Default`, bounded by `GIT_BUDGET`.
    fn of(slug: &Slug, op: GitOp) -> GitRun {
        GitRun {
            slug: slug.clone(),
            op,
            named: Named::Default,
            budget: GIT_BUDGET,
        }
    }

    /// A clone of `slug` into `dest`. It is not named `clone`, because `GitRun` derives
    /// `Clone` and `run.clone()` keeps meaning a copy.
    #[must_use]
    pub fn new_clone(slug: &Slug, dest: &Path) -> GitRun {
        GitRun::of(
            slug,
            GitOp::Clone {
                dest: dest.to_path_buf(),
                depth: None,
            },
        )
    }

    /// A fetch of `slug` into the clone `dir`.
    #[must_use]
    pub fn fetch(slug: &Slug, dir: &Path) -> GitRun {
        GitRun::of(
            slug,
            GitOp::Fetch {
                dir: dir.to_path_buf(),
            },
        )
    }

    /// `ls-remote` of `slug`.
    #[must_use]
    pub fn ls_remote(slug: &Slug, reference: Option<&str>) -> GitRun {
        GitRun::of(
            slug,
            GitOp::LsRemote {
                reference: reference.map(str::to_string),
            },
        )
    }

    /// The leased delete of `reference` at `expect`.
    #[must_use]
    pub fn push_delete(slug: &Slug, reference: &str, expect: &str) -> GitRun {
        GitRun::of(
            slug,
            GitOp::PushDelete {
                reference: reference.to_string(),
                expect: expect.to_string(),
            },
        )
    }

    /// The same run, named (a release-tier owner's run needs a literal or a grant).
    #[must_use]
    pub fn with_named(mut self, named: Named) -> GitRun {
        self.named = named;
        self
    }

    /// The same run with another bound.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> GitRun {
        self.budget = budget;
        self
    }

    /// The same clone, shallow. Any other op is unchanged.
    #[must_use]
    pub fn with_depth(mut self, depth: u32) -> GitRun {
        if let GitOp::Clone { depth: shallow, .. } = &mut self.op {
            *shallow = Some(depth);
        }
        self
    }
}

/// What a run printed: stdout and stderr, each redacted and cut to its last 64 KiB, and who it
/// spoke as.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GitOut {
    /// stdout.
    pub stdout: String,
    /// stderr.
    pub stderr: String,
    /// The profile the helper would answer: `None` for a dev-tier read that went anonymous,
    /// and the owner's writer for a `PushDelete`.
    pub as_: Option<String>,
    /// The grant's nonce, for a release-tier run or a `PushDelete`.
    pub grant: Option<String>,
}

/// The command git runs as its credential helper: this executable, and the words before
/// `auth git-credential`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperCommand {
    /// The executable, absolute and canonical.
    pub exe: PathBuf,
    /// The words before `auth git-credential`: `["agh"]` for `aterm` and `atpkg-publish`,
    /// none for an executable whose file stem is `agh`.
    pub args: Vec<String>,
}

impl HelperCommand {
    /// `std::env::current_exe()`, canonicalized, with `args` by its file stem.
    ///
    /// # Errors
    ///
    /// could_not_run, why `the helper's executable cannot be named: {e}`.
    pub fn current() -> Result<HelperCommand> {
        Err(Error::not_yet("the git helper's command", "WP7"))
    }

    /// The `credential.helper` value: `!` and the shell-quoted words, then
    /// `auth git-credential`. Each word is wrapped in `"…"`, with `"`, `\`, `$` and a backtick
    /// escaped by `\`.
    #[must_use]
    pub fn config_value(&self) -> String {
        // WP7 writes the body (plan §11); until then no helper is named.
        String::new()
    }
}

/// The runner (`Github::run_git`): route, refuse a rewriting config, isolate the child's
/// environment and directories, write a grant when one is needed, spawn `git` through
/// `Fx::git`, clean up and map the exit (design §6.9).
///
/// # Errors
///
/// The refusals of each step; off unix `write::unix_only()`.
pub(crate) fn run(gh: &Github, run: &GitRun) -> Result<GitOut> {
    let _ = (gh, run);
    Err(Error::not_yet("the git runner", "WP7"))
}
