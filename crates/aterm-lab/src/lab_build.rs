// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build the lab's evaluator judges in, which aos-search's bundles and the M6 learner's
//! stores name. A neutral module (design §13.3) so the learner reaches it without naming
//! `aos_search`.
//!
//! The commit is read when the lab runs, never compiled in: a compiled-in stamp made the lab's
//! build script watch every tracked file, so any edit anywhere rebuilt the lab. `targo lab` builds
//! the checkout before it runs it, so there the checkout's commit names the source the program was
//! built from, and an edit made since marks it `-dirty`; a dirty build names no clean commit, so
//! the gates that need one (`aos-learn-bench`'s G0, a bundle verified against an instance) refuse
//! it. A binary run some other way names the checkout as it is when it runs.

use std::path::Path;
use std::sync::OnceLock;

use crate::Fail;
use crate::paths::{CRATE_DIR, paths};

/// The paths a commit's `-dirty` is of, one rule with every other stamp producer's.
#[path = "../../aterm/stamp_paths.rs"]
pub(crate) mod stamp_paths;

/// The variable that names the commit a run reports instead of the checkout's: a run with no
/// work tree pins it, and `aos-learn-bench` passes its own to the copy of itself it runs.
pub const PIN: &str = aterm_stamp::input::COMMIT;

/// The commit this run names: [`PIN`] when it is set and not blank, else the checkout's
/// ([`commit_of`] of [`paths`]' `repo`). Read once.
pub fn lab_commit() -> &'static str {
    static COMMIT: OnceLock<String> = OnceLock::new();
    COMMIT.get_or_init(|| {
        let pin = std::env::var(PIN).ok();
        commit_from(pin.as_deref(), &paths().repo)
    })
}

/// The build the lab's evaluator judges in and its bundles name: `aterm-lab+g` and
/// [`lab_commit`]. A dev runner carries no product version (no library compiles one in,
/// `aterm_stamp`), and builds pair by their commit.
pub fn lab_build() -> &'static str {
    static BUILD: OnceLock<String> = OnceLock::new();
    BUILD.get_or_init(|| format!("aterm-lab+g{}", lab_commit()))
}

/// [`lab_commit`]'s rule: the pin when it is set and not blank, else [`commit_of`] `repo`.
fn commit_from(pin: Option<&str>, repo: &Path) -> String {
    match pin.map(str::trim).filter(|p| !p.is_empty()) {
        Some(pin) => pin.to_owned(),
        None => commit_of(repo),
    }
}

/// The checkout `repo`'s commit: HEAD's first 12 hex digits, `-dirty` when `git status
/// --porcelain` over [`stamp_paths::PATHSPEC`] prints anything; `unknown` when `repo` holds no
/// lab crate or git cannot read it.
fn commit_of(repo: &Path) -> String {
    let unknown = || "unknown".to_owned();
    if !repo.join(CRATE_DIR).join("Cargo.toml").is_file() {
        return unknown();
    }
    let git = |args: &[&str]| crate::measure::git(repo, args).ok();
    let Some(head) = git(&["rev-parse", "--short=12", "HEAD"]).filter(|h| !h.is_empty()) else {
        return unknown();
    };
    match git(&[&["status", "--porcelain"][..], &stamp_paths::PATHSPEC].concat()) {
        Some(changes) if changes.is_empty() => head,
        Some(_) => format!("{head}-dirty"),
        None => unknown(),
    }
}

/// Refuses a build that names no commit.
/// # Errors
/// Exit 1, naming the build.
pub fn check_build(build: &str) -> Result<(), Fail> {
    let commit = build.rsplit_once("+g").map_or("", |(_, commit)| commit);
    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    if hash.len() >= 12 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(Fail::die(format!(
            "the build `{build}` names no commit (run outside a git checkout; pin one with \
             {PIN}), so what it writes could not name the interpreter that judged it"
        )))
    }
}

#[cfg(test)]
#[path = "lab_build_tests.rs"]
mod tests;
