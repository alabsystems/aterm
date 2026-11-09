// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fixture's tree again at a new name: what a test lays once and starts each run from.

use std::path::Path;

/// `src` (a file, a link, or a directory and all it holds) again at the new name `dst`, each
/// directory and file with its mode and each link a link to what it names: one clone where the
/// volume clones a tree (APFS), a copy ([`copy`]) elsewhere.
pub(crate) fn clone_or_copy(src: &Path, dst: &Path) {
    if aterm_tempfile::clone_tree(src, dst).is_err() {
        copy(src, dst);
    }
}

/// [`clone_or_copy`]'s copy: a link made again naming what it names, each directory made and given
/// its mode once what it holds is in it, and each file copied with its mode.
fn copy(src: &Path, dst: &Path) {
    let meta = std::fs::symlink_metadata(src)
        .unwrap_or_else(|e| panic!("{}: no entry to copy: {e}", src.display()));
    if meta.file_type().is_symlink() {
        let names = std::fs::read_link(src).expect("the link");
        std::os::unix::fs::symlink(names, dst).expect("the link again");
    } else if meta.is_dir() {
        std::fs::create_dir(dst).expect("the directory again");
        for entry in std::fs::read_dir(src).expect("the directory") {
            let entry = entry.expect("an entry");
            copy(&entry.path(), &dst.join(entry.file_name()));
        }
        std::fs::set_permissions(dst, meta.permissions()).expect("the directory's mode");
    } else {
        std::fs::copy(src, dst).expect("the file again");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// Each entry under `root`, by its path there: its mode and what it holds, or what a link
    /// names.
    fn listing(root: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
        let mut found = Vec::new();
        let mut open = vec![PathBuf::new()];
        while let Some(at) = open.pop() {
            let path = root.join(&at);
            let meta = std::fs::symlink_metadata(&path).expect("an entry");
            let mode = meta.permissions().mode();
            let held = if meta.file_type().is_symlink() {
                std::fs::read_link(&path)
                    .expect("a link")
                    .into_os_string()
                    .into_encoded_bytes()
            } else if meta.is_dir() {
                for entry in std::fs::read_dir(&path).expect("a directory") {
                    open.push(at.join(entry.expect("an entry").file_name()));
                }
                Vec::new()
            } else {
                std::fs::read(&path).expect("a file")
            };
            found.push((at, mode, held));
        }
        found.sort();
        found
    }

    /// A TREE COPIED KEEPS EVERY MODE AND EVERY LINK, and a clone of it is the same tree: an
    /// owner-only directory and file, a shared one, a read-only file and a link each come back as
    /// they were, whichever way the volume serves [`clone_or_copy`].
    ///
    /// NEGATIVE CONTROL (2026-11-08): with [`copy`] leaving a directory the mode it was made with,
    /// or following a link, this fails at the copy's listing.
    #[test]
    fn a_tree_copied_keeps_every_mode_and_every_link() {
        let tmp = aterm_tempfile::tempdir().expect("a scratch directory");
        let tree = tmp.path().join("tree");
        let sub = tree.join("sub");
        std::fs::create_dir_all(&sub).expect("the tree");
        for (path, bytes, mode) in [
            (tree.join("private"), &b"owner only"[..], 0o600),
            (tree.join("shared"), &b"anyone reads"[..], 0o644),
            (sub.join("key"), &b"read only"[..], 0o400),
        ] {
            std::fs::write(&path, bytes).expect("a file");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("a mode");
        }
        std::os::unix::fs::symlink("shared", tree.join("link")).expect("a link");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o750)).expect("a mode");
        std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o700)).expect("a mode");
        let laid = listing(&tree);
        assert_eq!(laid.len(), 6, "{laid:?}");
        let copied = tmp.path().join("copied");
        copy(&tree, &copied);
        assert_eq!(listing(&copied), laid, "the copy");
        let cloned = tmp.path().join("cloned");
        clone_or_copy(&tree, &cloned);
        assert_eq!(listing(&cloned), laid, "the clone");
    }
}
