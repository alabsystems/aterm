// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The channel's scratch directories: created new and private (a predictable or
//! pre-created directory in a shared `/tmp` is one another local user controls), one per
//! call, removed on drop.

use atpkg_publish::channel::Scratch;

#[test]
fn scratch_is_new_private_per_call_and_removed_on_drop() {
    let a = Scratch::new("cas").expect("scratch");
    let b = Scratch::new("cas").expect("scratch");
    assert_ne!(a.path(), b.path(), "one per call");
    assert!(a.path().is_dir());
    let name = a.path().file_name().expect("name").to_string_lossy();
    assert!(name.starts_with("atpkg-publish-cas-"), "{name}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(a.path())
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }
    let path = a.path().to_path_buf();
    std::fs::write(path.join("index.toml"), "x").expect("write");
    drop(a);
    assert!(!path.exists(), "removed with its contents");
}
