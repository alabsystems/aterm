// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Store layout paths for [`crate::lease`] and [`crate::discovery`].
//!
//! THE STORE'S SPOTLIGHT LAYOUT (macOS). Spotlight skips a folder named `*.noindex`, so a
//! build that ships a Rust sysroot ([`ships_rust_sysroot`]: thousands of crate files under
//! `lib/rustlib`) lives in `store/<program>/<build>.noindex`, and `store/<program>/<build>`
//! is a symbolic link whose target is exactly `<build>.noindex` ([`is_linked_aside`]).
//! Every path through `<build>` (the `current` link, the shims, the rustup view laid from it,
//! a lease) reaches the build unchanged; a compiler that canonicalizes its sysroot reports
//! the `.noindex` folder, which every reader of a store path takes for the build.
//! A real directory at `store/<program>/<build>` that ships a sysroot is a build not moved
//! yet ([`spotlight_exposed_builds`]): atpkg installs every new one aside, and moves an older
//! one once no `current` link or shim names it and nothing runs from it.
//!
//! The rustup view (`rustup/<name>`, a copy of the current build that rustup's
//! `toolchains/<name>` links to) follows the same contract: it lives in
//! `rustup/<name>.noindex` with `rustup/<name>` its link. A plain view
//! ([`spotlight_exposed_views`]) is laid anew aside only where atpkg lays the view anyway
//! (the next update, or a repair of a view that no longer matches its build), never renamed
//! while it may be running.
//!
//! A reader that cannot link this crate reads the same contract from the directories alone:
//! a plain directory that holds `lib/rustlib` is not moved yet, a symbolic link is moved.

use std::path::{Path, PathBuf};

/// `<prefix>/store` — the versioned, immutable builds, `store/<program>/<build>`.
pub const STORE_DIR: &str = "store";

/// `<prefix>/compat` — the store's exec roots, one per build.
pub const COMPAT_DIR: &str = "compat";

/// `<prefix>/rustup` — the seam's views, one per rustup toolchain name.
pub const VIEWS_DIR: &str = "rustup";

/// The suffix of the marker beside a store build that says its install completed:
/// `<prefix>/store/<program>/<build>.ready`. A build without it is not installed, whatever its
/// tree holds (a stage cut short, or a build its resolve check refused).
pub const READY_SUFFIX: &str = ".ready";

/// `<prefix>/store/<program>/<build>`.
#[inline]
#[must_use]
pub fn build_dir(prefix: &Path, program: &str, build: u64) -> PathBuf {
    prefix.join(STORE_DIR).join(program).join(build.to_string())
}

/// `<prefix>/rustup/<name>` — the view the seam `name` targets.
#[inline]
#[must_use]
pub fn view_dir(prefix: &Path, name: &str) -> PathBuf {
    prefix.join(VIEWS_DIR).join(name)
}

/// `<name>.noindex` beside the store build `build_dir`: where its tree lives once laid aside.
#[must_use]
pub fn aside_of(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(crate::build_cache::noindex(name)))
}

/// Whether `build_dir` is a store build laid aside: a symbolic link (by `lstat`) whose target
/// is exactly its own `<name>.noindex`, relative ([`aside_of`]).
#[must_use]
pub fn is_linked_aside(build_dir: &Path) -> bool {
    let Some(name) = build_dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    std::fs::read_link(build_dir)
        .is_ok_and(|target| target == Path::new(&crate::build_cache::noindex(name)))
}

/// Whether the build tree at `tree` ships a Rust sysroot: `lib/` and `lib/rustlib` are real
/// directories (`tree` itself may be reached through a link).
#[must_use]
pub fn ships_rust_sysroot(tree: &Path) -> bool {
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    real_dir(&tree.join("lib")) && real_dir(&tree.join("lib").join("rustlib"))
}

/// A store build Spotlight still indexes: a real directory at `store/<program>/<build>` that
/// ships a Rust sysroot ([`spotlight_exposed_builds`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposedBuild {
    pub program: String,
    pub build: u64,
    /// `<prefix>/store/<program>/<build>`.
    pub dir: PathBuf,
    /// Whether `store/<program>/current` names it: the build in use, which stays where it is
    /// until an update makes another current.
    pub active: bool,
}

/// Every store build under `prefix` not yet laid aside ([`ExposedBuild`]), by program then
/// build; empty when the store cannot be read.
#[must_use]
pub fn spotlight_exposed_builds(prefix: &Path) -> Vec<ExposedBuild> {
    let mut out = Vec::new();
    let Ok(programs) = std::fs::read_dir(prefix.join(STORE_DIR)) else {
        return out;
    };
    for program_dir in programs.flatten() {
        if !program_dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(program) = program_dir.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // A current build laid aside canonicalizes to `<build>.noindex`, no plain build name,
        // so it marks no exposed build active.
        let current = std::fs::canonicalize(program_dir.path().join("current"))
            .ok()
            .and_then(|target| {
                target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(plain_build_name)
            });
        let Ok(entries) = std::fs::read_dir(program_dir.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(build) = entry.file_name().to_str().and_then(plain_build_name) else {
                continue;
            };
            let dir = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) && ships_rust_sysroot(&dir) {
                out.push(ExposedBuild {
                    program: program.clone(),
                    build,
                    dir,
                    active: current == Some(build),
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.program, a.build).cmp(&(&b.program, b.build)));
    out
}

/// A rustup view Spotlight still indexes: a real directory at `rustup/<name>` that ships a
/// Rust sysroot ([`spotlight_exposed_views`]). It is always the view in use, and atpkg lays it
/// anew aside the next time it lays the view at all, so it asks nothing of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposedView {
    pub name: String,
    /// `<prefix>/rustup/<name>`.
    pub dir: PathBuf,
}

/// Every rustup view under `prefix` not yet laid aside ([`ExposedView`]), by name; empty when
/// there is none or the views cannot be read. A view whose `lib/` links into a dev checkout
/// holds no sysroot of its own and is not one.
#[must_use]
pub fn spotlight_exposed_views(prefix: &Path) -> Vec<ExposedView> {
    let Ok(entries) = std::fs::read_dir(prefix.join(VIEWS_DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<ExposedView> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let named = !name.starts_with('.') && !name.ends_with(crate::build_cache::NOINDEX);
            (named && ships_rust_sysroot(&entry.path())).then(|| ExposedView {
                name,
                dir: entry.path(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// A build directory's name as the store writes one: digits, no leading zero, not zero.
fn plain_build_name(name: &str) -> Option<u64> {
    let digits = !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit());
    let build = (digits && !(name.len() > 1 && name.starts_with('0')))
        .then(|| name.parse::<u64>().ok())
        .flatten()?;
    is_valid_build_id(build).then_some(build)
}

/// Return `true` when `build` is a valid non-zero store build identifier.
#[inline]
#[must_use]
pub const fn is_valid_build_id(build: u64) -> bool {
    build != 0
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
