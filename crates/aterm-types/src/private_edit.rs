// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The folder a private edit lives in: one whose document holds plaintext the person asked to
//! see and change (a registry grid with its `Secret` column revealed, `aterm db emails --edit`),
//! so the editor keeps no copy of it and serves no read of it (one-mesh design §4.8, §11 A7).
//!
//! The writer names its scratch folder with [`DIR_SUFFIX`]; amacs reads the name: a document
//! whose path has a component ending in it gets no draft journal (and any journal an earlier
//! open left is purged), keeps no closed-buffer recovery, and `aterm ctl doc read` refuses it.
//! The marker is a name, so anyone may mark a folder of their own private; doing so only gives up
//! the editor's crash recovery for it.
//!
//! A grid's scratch folder is made in [`scratch_root`] and named for the process that made it
//! (`att-edit-<pid>_<random>_<n>.private.noindex`), which removes it when the edit ends. One its
//! maker could not remove (killed mid-edit, its file still holding what it revealed) is swept by
//! [`sweep_dead`]: at the next grid edit, when aterm starts, and when amacs lets go of a private
//! document.

use std::path::{Path, PathBuf};

/// The suffix of a private edit's folder name (`att-edit-<random>.private.noindex`). It ends in
/// `.noindex`, so Spotlight leaves the folder out as it does every scratch folder.
pub const DIR_SUFFIX: &str = ".private.noindex";

/// Whether `path` lies in a private edit's folder: some component of it ends in [`DIR_SUFFIX`]
/// and is longer than it. A `file://` URI's path is given without the scheme.
#[must_use]
pub fn is_private(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| name.len() > DIR_SUFFIX.len() && name.ends_with(DIR_SUFFIX))
    })
}

/// The prefix of a grid scratch folder's name: its maker's pid follows it, then `_`.
pub const SCRATCH_PREFIX: &str = "att-edit-";

/// The folder grid scratch folders are made in: on macOS the system temporary root's quiet folder
/// (`aterm_tempfile::quiet_in`, whose file events fseventsd drops), elsewhere the root itself.
#[must_use]
pub fn scratch_root() -> PathBuf {
    let temp = std::env::temp_dir();
    if cfg!(target_os = "macos") {
        aterm_tempfile::quiet_in(&temp)
    } else {
        temp
    }
}

/// The pid that made the grid scratch folder named `name` (`att-edit-<pid>_…private.noindex`):
/// `None` for any other name.
#[must_use]
pub fn scratch_maker(name: &str) -> Option<i32> {
    name.strip_prefix(SCRATCH_PREFIX)
        .filter(|_| name.len() > SCRATCH_PREFIX.len() + DIR_SUFFIX.len())
        .filter(|_| name.ends_with(DIR_SUFFIX))
        .and_then(|rest| rest.split_once('_'))
        .and_then(|(pid, _)| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 1)
}

/// Remove every grid scratch folder directly in `parent` whose maker no longer runs, except
/// `keep`; the number removed. A folder whose maker runs (or whose pid another process now
/// holds) stays, and so does every other entry. Where this host cannot tell whether a process
/// runs (it cannot read its own), nothing is removed.
#[cfg(not(target_family = "wasm"))]
pub fn sweep_dead(parent: &Path, keep: Option<&Path>) -> usize {
    sweep_with(parent, keep, &|pid| {
        aterm_uds::process::read_lineage(pid).is_some()
    })
}

/// [`sweep_dead`] with `runs` saying whether a pid runs.
#[cfg(not(target_family = "wasm"))]
fn sweep_with(parent: &Path, keep: Option<&Path>, runs: &dyn Fn(i32) -> bool) -> usize {
    let own = i32::try_from(std::process::id()).unwrap_or(0);
    if !runs(own) {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(parent) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if Some(path.as_path()) == keep {
            continue;
        }
        let Some(maker) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(scratch_maker)
        else {
            continue;
        };
        if !runs(maker)
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && std::fs::remove_dir_all(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
#[path = "private_edit_tests.rs"]
mod tests;
