// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The grants (design §6.9): `<state>/agh/grants/<nonce>.json`, 0600, written by the agh
//! process that spawns a git child for a release-tier repository, read by the helper,
//! removed when the child exits.
//!
//! A grant's file name is its nonce, so a process of the same user that lists `grants/`
//! learns the live nonces. A hidden name would close nothing: the nonce is also in the git
//! child's environment (`AGH_GIT_GRANT`), which a process of the same user can read, and the
//! token files are that user's to read anyway. What bounds a grant is the helper's check,
//! never the nonce's secrecy: the spawner's `(pid, start)` must be an ancestor of the asking
//! helper within `ANCESTRY_DEPTH`, the slug must be the grant's, and it must not have expired
//! (at most `GRANT_MAX`). For a long-lived lane process the ancestry includes its build
//! children, for at most the run's bound.

#[cfg(any(test, debug_assertions))]
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::Dirs;
use crate::error::{Error, Result};
use crate::slug::Slug;

/// One grant file's contents:
/// `{"expires_at":N,"pid":N,"profile":"…","slug":"…","start":N,"v":1}`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grant {
    /// The nonce: 32 lowercase hex from 16 random bytes. It is the file's stem.
    pub nonce: String,
    /// The repository.
    pub slug: Slug,
    /// The profile it lets the helper answer: a release-tier owner's writer for a release-tier
    /// run, or the owner's writer for a `PushDelete`.
    pub profile: String,
    /// The spawner's pid (`std::process::id()`).
    pub pid: u32,
    /// The spawner's start: `boot.0` of `aterm_uds::process::read_process_start`, which takes
    /// an `i32` pid (`i32::try_from(pid)`) and answers an `Option`. It tells the spawner from a
    /// later process with the same pid.
    pub start: u64,
    /// `Fx::wall_secs` + at most `GRANT_MAX`.
    pub expires_at: u64,
}

/// What the helper reads grants through. The live view reads `<state>/agh/grants` and the
/// process table. A test's view is a map with no file and no process.
pub trait GrantView {
    /// The grant `nonce` names, when it is live for `slug`: not expired at `now`, its
    /// spawner `(pid, start)` an ancestor of this process within `ANCESTRY_DEPTH`, and its
    /// slug `slug` (ASCII case-insensitive).
    fn find(&self, nonce: &str, slug: &Slug, now: u64) -> Option<Grant>;
}

/// The live view: the grant files under `dirs.grants()` and the ancestry walk
/// (`aterm_uds::process::read_process_parent` and `read_process_start`).
pub struct LiveGrants {
    /// `dirs.grants()`.
    dir: PathBuf,
}

impl LiveGrants {
    /// The view over `dirs.grants()`.
    #[must_use]
    pub fn new(dirs: &Dirs) -> LiveGrants {
        LiveGrants { dir: dirs.grants() }
    }
}

impl GrantView for LiveGrants {
    fn find(&self, nonce: &str, slug: &Slug, now: u64) -> Option<Grant> {
        // WP7 writes the body (plan §11); until then no grant is live.
        let _ = (&self.dir, nonce, slug, now);
        None
    }
}

/// A test's grants: fixed answers by nonce, with the ancestry taken as given.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, Default)]
pub struct MapGrants {
    /// Each grant by its nonce, and whether its spawner is an ancestor.
    grants: BTreeMap<String, (Grant, bool)>,
}

#[cfg(any(test, debug_assertions))]
impl MapGrants {
    /// No grant: `MapGrants::default()`.
    #[must_use]
    pub fn new() -> MapGrants {
        MapGrants::default()
    }

    /// `grant`, its spawner an ancestor when `in_tree`.
    #[must_use]
    pub fn with(mut self, grant: Grant, in_tree: bool) -> MapGrants {
        self.grants.insert(grant.nonce.clone(), (grant, in_tree));
        self
    }
}

#[cfg(any(test, debug_assertions))]
impl GrantView for MapGrants {
    fn find(&self, nonce: &str, slug: &Slug, now: u64) -> Option<Grant> {
        // WP7 writes the body (plan §11); until then no grant is live.
        let _ = (&self.grants, nonce, slug, now);
        None
    }
}

/// Write a grant for `slug` and `profile`, spawned by this process, for `ttl` (at most
/// `GRANT_MAX`), in `dirs.grants()`, made with its parents by
/// `config::make_state_subdir(dirs, &dirs.grants())`, mode 0600, `create_new`. So the first
/// grant on a state directory nothing has made yet is written.
///
/// # Errors
///
/// could_not_run for the directory or the file, or why `this process's start cannot be read`
/// when `read_process_start` answers `None`; off unix `write::unix_only()`.
pub(crate) fn write(
    dirs: &Dirs,
    slug: &Slug,
    profile: &str,
    now: u64,
    ttl: std::time::Duration,
) -> Result<Grant> {
    let _ = (dirs, slug, profile, now, ttl);
    Err(Error::not_yet("the git grant's write", "WP7"))
}

/// Remove the grant's file. A file already gone is fine.
pub(crate) fn remove(dirs: &Dirs, grant: &Grant) {
    // WP7 writes the body (plan §11).
    let _ = (dirs, grant);
}

/// Remove every grant file whose `expires_at` has passed: the spawner killed before it
/// removed its own (the model's `Expire`). Each `GitRun` calls it before writing its grant.
pub(crate) fn sweep(dirs: &Dirs, now: u64) {
    // WP7 writes the body (plan §11).
    let _ = (dirs, now);
}

/// The model's `grant` for the grant `nonce` names, read from `dirs.grants()` at `now`: 0 when
/// its file is absent, 1 when it is there and unexpired, 2 when it is there and expired (a
/// spawner killed before it removed its own). `Spawn` (`write`), `Consume` (`remove`) and
/// `Expire` (`sweep`) are each judged by it after they run.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_grant_file(dirs: &Dirs, nonce: &str, now: u64) -> i64 {
    // WP7 writes the body (plan §11).
    let _ = (dirs, nonce, now);
    0
}

/// The host of `AghGitGrant`'s waivers: the world's toggles, the spawner's death, and the
/// negative controls. Empty; WP0b writes it bare, and WP7 puts the waivers on it.
#[doc(hidden)]
pub fn git_grant_environment() {}
