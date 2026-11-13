// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `update` decides before it does anything: whether origin/main needs a
//! build, whether the staged build is installed, and which dist folders go.

use crate::pointer::{Pointer, Smoke, is_commit12};

/// What `update` does about origin/main's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Build, package, smoke and stage it.
    Build,
    /// The pointer already names it: nothing to build. Carries the verdict the
    /// smoke gave that build.
    Staged(Smoke),
    /// The staged build is newer than origin/main's head (main was moved
    /// back): nothing is built, so no run downgrades the staged build.
    Older {
        /// The staged build's number.
        staged: u64,
        /// The head's number (its committer time).
        head: u64,
    },
}

/// What origin/main looks like to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head<'a> {
    /// origin/main's commit, forty hex digits.
    pub commit_full: &'a str,
    /// Its committer time, the number a build of it prints as `build:`.
    pub build_number: u64,
}

/// Decides [`Plan`] from the pointer on disk (`None`: none yet), whether the
/// folder it names still holds a build, and origin/main's head. `force`
/// builds whatever the pointer says.
#[must_use]
pub fn plan(pointer: Option<&Pointer>, dist_present: bool, head: Head<'_>, force: bool) -> Plan {
    let Some(p) = pointer.filter(|_| !force) else {
        return Plan::Build;
    };
    if p.commit_full == head.commit_full {
        // The same commit builds the same images: a failed smoke is not built
        // again, a smoke that could not run and a folder that is gone are.
        return match p.smoke {
            Smoke::Pass | Smoke::Fail if dist_present => Plan::Staged(p.smoke),
            _ => Plan::Build,
        };
    }
    if p.build_number > head.build_number {
        return Plan::Older {
            staged: p.build_number,
            head: head.build_number,
        };
    }
    Plan::Build
}

/// What `update` does with the staged build and the installed copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// Hand the staged build to its installer.
    Yes,
    /// The staged build's smoke did not pass: it is never installed.
    Unproven,
    /// The installed copy is the staged commit.
    Same,
    /// The installed copy is a newer build than the staged one (a build of an
    /// unlanded branch a person installed): no run downgrades it.
    Newer {
        /// The installed build's number.
        installed: u64,
        /// The staged build's number.
        staged: u64,
    },
}

/// Decides [`Install`] from the staged pointer and the installed copy's
/// commit, build number and changed-tree mark (`None`: nothing installed, or
/// an install older than `BUILD.json`). Only a build whose smoke passed is
/// installed, only over another commit or a copy built from a changed tree
/// (which carries its commit's number and is not that commit), and over a
/// newer build only with `force`; equal numbers on different commits install,
/// as [`plan`] builds them.
#[must_use]
pub fn install_needed(
    staged: &Pointer,
    installed: Option<(&str, u64, bool)>,
    force: bool,
) -> Install {
    if staged.smoke != Smoke::Pass {
        return Install::Unproven;
    }
    match installed {
        None => Install::Yes,
        Some((commit_full, _, false)) if commit_full == staged.commit_full => Install::Same,
        Some((_, number, _)) if number > staged.build_number && !force => Install::Newer {
            installed: number,
            staged: staged.build_number,
        },
        Some(_) => Install::Yes,
    }
}

/// The dist folders to delete: every entry of `existing` that is a dist folder
/// by name (twelve hex digits) and is not in `keep` (the build just staged, the
/// one staged before it, the installed one). A name of any other shape is
/// never chosen, so the pointer, a log and a folder a person made stay.
#[must_use]
pub fn retention<'a>(existing: &'a [String], keep: &[&str]) -> Vec<&'a str> {
    existing
        .iter()
        .map(String::as_str)
        .filter(|name| is_commit12(name) && !keep.contains(name))
        .collect()
}

/// What the name of a dist that is still being assembled ends in.
pub const BUILDING: &str = ".building";
/// What the name of a dist that a rebuild replaced ends in, until it is deleted.
pub const REPLACED: &str = ".old";

/// The folders an interrupted run left: every entry of `existing` that is a
/// dist's name followed by [`BUILDING`] or [`REPLACED`]. Neither is ever the
/// folder a pointer names, and a name of any other shape is never chosen.
#[must_use]
pub fn leftovers(existing: &[String]) -> Vec<&str> {
    existing
        .iter()
        .map(String::as_str)
        .filter(|name| {
            [BUILDING, REPLACED]
                .iter()
                .any(|end| name.strip_suffix(end).is_some_and(is_commit12))
        })
        .collect()
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
