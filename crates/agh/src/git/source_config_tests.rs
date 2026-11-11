// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

fn root() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("agh-config-source-")
        .tempdir()
        .unwrap()
}

#[test]
fn ordinary_bare_and_linked_metadata_select_the_same_common_config() {
    let root = root();
    let common = root.path().join("common");
    let admin = common.join("worktrees/linked");
    let checkout = root.path().join("checkout");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir(&checkout).unwrap();
    std::fs::write(common.join("config"), b"[user]\nname = common\n").unwrap();
    std::fs::write(admin.join("commondir"), b"../..\n").unwrap();
    std::fs::write(
        checkout.join(".git"),
        b"gitdir: ../common/worktrees/linked\n",
    )
    .unwrap();
    let bare = SourceConfig::read(&common, 1024).unwrap();
    let linked = SourceConfig::read(&checkout, 1024).unwrap();
    assert_eq!(bare.common, linked.common);
    assert!(linked.worktree().unwrap().is_none());
    std::fs::write(admin.join("config.worktree"), b"[user]\nname = linked\n").unwrap();
    assert_eq!(
        linked.worktree().unwrap().unwrap(),
        b"[user]\nname = linked\n"
    );
}

#[test]
fn malformed_oversized_and_symlinked_metadata_are_refused() {
    let root = root();
    let checkout = root.path().join("checkout");
    std::fs::create_dir(&checkout).unwrap();
    for bytes in [
        b"not a gitfile\n".as_slice(),
        b"gitdir: ../common\r\n",
        b"gitdir: ../common\nextra\n",
        b"gitdir: \n",
        &[0xff],
    ] {
        std::fs::write(checkout.join(".git"), bytes).unwrap();
        assert!(SourceConfig::read(&checkout, 64).is_err());
    }
    std::fs::write(checkout.join(".git"), vec![b'x'; 65]).unwrap();
    assert!(SourceConfig::read(&checkout, 64).is_err());
    std::fs::remove_file(checkout.join(".git")).unwrap();
    let common = root.path().join("common");
    std::fs::create_dir(&common).unwrap();
    std::fs::write(common.join("config"), b"[user]\nname = test\n").unwrap();
    std::os::unix::fs::symlink(&common, checkout.join(".git")).unwrap();
    assert!(SourceConfig::read(&checkout, 1024).is_err());
    std::fs::remove_file(checkout.join(".git")).unwrap();
    std::fs::create_dir(checkout.join(".git")).unwrap();
    std::os::unix::fs::symlink(common.join("config"), checkout.join(".git/config")).unwrap();
    assert!(SourceConfig::read(&checkout, 1024).is_err());
}

#[test]
fn common_pointer_config_and_worktree_file_keep_owned_reader_refusals() {
    let root = root();
    let admin = root.path().join("admin");
    std::fs::create_dir(&admin).unwrap();
    std::fs::write(admin.join("config"), b"[user]\nname = valid\n").unwrap();
    for bytes in [
        b"\n".as_slice(),
        b"../other\nextra\n",
        b" ../other\n",
        b"../other\r\n",
    ] {
        std::fs::write(admin.join("commondir"), bytes).unwrap();
        assert!(SourceConfig::read(&admin, 64).is_err());
    }
    std::fs::remove_file(admin.join("commondir")).unwrap();
    std::os::unix::fs::symlink(admin.join("config"), admin.join("commondir")).unwrap();
    assert!(SourceConfig::read(&admin, 64).is_err());
    std::fs::remove_file(admin.join("commondir")).unwrap();
    std::fs::write(admin.join("config"), vec![b'x'; 65]).unwrap();
    assert!(SourceConfig::read(&admin, 64).is_err());
    std::fs::write(admin.join("config"), b"[user]\nname = valid\n").unwrap();
    let source = SourceConfig::read(&admin, 64).unwrap();
    std::os::unix::fs::symlink(admin.join("config"), admin.join("config.worktree")).unwrap();
    assert!(source.worktree().is_err());
}
