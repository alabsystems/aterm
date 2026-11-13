// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Live, Stat, System};

#[test]
fn clearing_a_publication_slot_preserves_later_files_links_and_nonempty_directories() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let live = Live::new();
    let fake = Fake::root();
    for (native, sys) in [(true, &live as &dyn System), (false, &fake as &dyn System)] {
        for kind in ["file", "link", "nonempty", "empty", "absent"] {
            let path = dir.path().join(kind);
            // The slot was empty when the publisher admitted it. These objects arrive
            // before rmdir: a generic unlink would silently discard them.
            assert!(sys.stat(&path).is_err());
            match kind {
                "file" => sys.write_file(&path, b"late owner work", 0o600).unwrap(),
                "link" => {
                    if native {
                        std::os::unix::fs::symlink("owner-target", &path).unwrap();
                    } else {
                        fake.link(path.to_str().unwrap(), "owner-target");
                    }
                }
                "nonempty" | "empty" => {
                    sys.create_dir_all(&path, 0o700).unwrap();
                    if kind == "nonempty" {
                        sys.write_file(&path.join("keep"), b"owner files", 0o600)
                            .unwrap();
                    }
                }
                _ => {}
            }
            let result = sys.remove_empty_dir(&path);
            assert_eq!(
                result.is_ok(),
                matches!(kind, "empty" | "absent"),
                "{kind}: {result:?}"
            );
            match kind {
                "file" => assert_eq!(sys.read_file(&path).unwrap(), b"late owner work"),
                "link" => assert!(sys.stat(&path).unwrap().symlink),
                "nonempty" => {
                    assert_eq!(sys.read_file(&path.join("keep")).unwrap(), b"owner files")
                }
                _ => assert!(sys.stat(&path).is_err()),
            }
        }
    }
    let path = Path::new("/synthetic-link");
    fake.stat_of(
        path.to_str().unwrap(),
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o700,
            dir: true,
            symlink: true,
            len: 0,
        },
    );
    assert!(fake.remove_empty_dir(path).is_err());
    assert!(fake.stat(path).unwrap().symlink);
}
