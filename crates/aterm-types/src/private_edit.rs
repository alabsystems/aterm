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
mod tests {
    use super::*;

    #[test]
    fn a_file_in_a_marked_folder_is_private_and_nothing_else_is() {
        assert!(is_private(Path::new(
            "/private/var/folders/x/T/.ubd/att-edit-1_ab.private.noindex/address.tsv"
        )));
        assert!(is_private(Path::new("att-edit-x.private.noindex/a/b.tsv")));
        // The negative controls: an ordinary scratch folder, a file named like the marker, and
        // the bare suffix.
        assert!(!is_private(Path::new(
            "/tmp/.ubd/att-edit-1_ab.noindex/address.tsv"
        )));
        assert!(!is_private(Path::new("/tmp/x/notes.private.noindex.tsv")));
        assert!(!is_private(Path::new("/tmp/.private.noindex/a.tsv")));
    }

    #[test]
    fn a_scratch_folder_names_its_maker() {
        assert_eq!(
            scratch_maker("att-edit-4242_0123abcd_1.private.noindex"),
            Some(4242)
        );
        // The negative controls: no pid, pid 1, another suffix, another prefix, a bare marker.
        assert_eq!(scratch_maker("att-edit-x_1.private.noindex"), None);
        assert_eq!(scratch_maker("att-edit-1_ab.private.noindex"), None);
        assert_eq!(scratch_maker("att-edit-4242_ab.noindex"), None);
        assert_eq!(scratch_maker("other-4242_ab.private.noindex"), None);
        assert_eq!(scratch_maker("att-edit-.private.noindex"), None);
    }

    /// A dead maker's folder goes, with its file; a live maker's, `keep`, another name and a
    /// plain file stay; and where no process can be read as running (this one included),
    /// nothing goes.
    /// NEGATIVE CONTROL: with `sweep_with` removing regardless of `runs`, the live maker's folder
    /// goes too: `` assertion `left == right` failed left: 2 right: 1 ``.
    #[test]
    fn only_a_dead_makers_scratch_folder_is_swept() {
        let root = aterm_tempfile::tempdir().unwrap();
        let folder = |name: &str| {
            let path = root.path().join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("address.tsv"), "pk\tpassword\n").unwrap();
            path
        };
        let own = i32::try_from(std::process::id()).unwrap();
        let dead = folder("att-edit-900001_aa_1.private.noindex");
        let live = folder(&format!("att-edit-{own}_bb_1.private.noindex"));
        let kept = folder("att-edit-900002_cc_1.private.noindex");
        let other = folder("att-edit-900003_dd_1.noindex");
        std::fs::write(root.path().join("att-edit-900004_ee.private.noindex"), "x").unwrap();
        let runs = |pid: i32| pid == own;
        assert_eq!(sweep_with(root.path(), Some(&kept), &|pid| runs(pid)), 1);
        assert!(!dead.exists(), "dead maker's folder swept");
        assert!(live.exists(), "live maker's folder kept");
        assert!(kept.exists() && other.exists());
        assert!(
            root.path()
                .join("att-edit-900004_ee.private.noindex")
                .is_file()
        );
        let blind = folder("att-edit-900005_ff_1.private.noindex");
        assert_eq!(sweep_with(root.path(), None, &|_| false), 0);
        assert!(blind.exists(), "nothing goes where liveness is unknown");
    }
}
