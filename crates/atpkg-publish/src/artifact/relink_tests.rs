// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inert payload preservation through the actual hard-link relink callers.

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

pub(super) enum Collision {
    Primary,
    Payload,
}

fn inode(path: &Path) -> (u64, u64) {
    let meta = std::fs::symlink_metadata(path).expect("the inert file");
    assert!(meta.is_file());
    (meta.dev(), meta.ino())
}

fn names(dir: &Path) -> BTreeSet<std::ffi::OsString> {
    std::fs::read_dir(dir)
        .expect("the private stage")
        .map(|entry| entry.expect("an entry").file_name())
        .collect()
}

pub(super) fn preserves_payload(
    collision: Collision,
    relink: impl Fn(&Path, &Path, &[PathBuf]) -> Result<(), String>,
) {
    let root = aterm_tempfile::Builder::new()
        .prefix("atpkg-relink-payload-")
        .tempdir()
        .expect("private inert stage");
    for ordinary in [true, false] {
        let stage = root
            .path()
            .join(if ordinary { "ordinary" } else { "collision" });
        let bin = stage.join("bin");
        std::fs::create_dir_all(&bin).expect("stage bin");
        let primary = bin.join(if !ordinary && matches!(collision, Collision::Primary) {
            ".alias.atpkg-relink"
        } else {
            "primary"
        });
        let alias = bin.join("alias");
        std::fs::write(&primary, b"inert original").expect("primary payload");
        std::fs::hard_link(&primary, &alias).expect("actual hard-link group");
        assert_eq!(inode(&primary), inode(&alias));
        // A byte-changing backend may replace the primary inode. Recreate that effect with
        // an inert rename, without native-object parsing, execution or any signing backend.
        let replacement = bin.join("replacement");
        std::fs::write(&replacement, b"inert rewritten").expect("replacement payload");
        std::fs::rename(&replacement, &primary).expect("replace primary inode");
        assert_ne!(inode(&primary), inode(&alias));
        let occupied = (!ordinary && matches!(collision, Collision::Payload))
            .then(|| bin.join(".alias.atpkg-relink"));
        if let Some(path) = &occupied {
            std::fs::write(path, b"unrelated admitted payload").expect("occupied legacy name");
        }
        let before = names(&bin);
        let result = relink(&stage, &primary, std::slice::from_ref(&alias));
        assert_eq!(
            std::fs::read(&primary).ok().as_deref(),
            Some(b"inert rewritten".as_slice()),
            "the primary is admitted payload, never a scratch reservation: {result:?}"
        );
        if let Some(path) = &occupied {
            assert_eq!(
                std::fs::read(path).ok().as_deref(),
                Some(b"unrelated admitted payload".as_slice()),
                "an occupied legacy name is payload, never disposable scratch"
            );
        }
        result.expect("relink without consuming admitted payload");
        assert_eq!(std::fs::read(&alias).unwrap(), b"inert rewritten");
        assert_eq!(inode(&primary), inode(&alias));
        assert_eq!(
            names(&bin),
            before,
            "only the original payload names remain"
        );
        relink(&stage, &primary, std::slice::from_ref(&alias)).expect("same-inode no-op");
        assert_eq!(names(&bin), before);
    }
}
