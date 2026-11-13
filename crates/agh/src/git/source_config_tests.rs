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

/// A refusal of `SourceConfig::read`, by its why and next.
fn refusal(root: &Path) -> (String, String) {
    match SourceConfig::read(root, 1024) {
        Err(ReadError::Refused(Refusal { why, next })) => (why, next),
        Err(ReadError::Missing) => panic!("{}: no common config", root.display()),
        Ok(_) => panic!("{}: read", root.display()),
    }
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// What git leaves under umask 002: each directory a read looks in 0775, each file 0664.
/// Every read refuses the group write bit by the path that carries it, its mode and its
/// cause, and names the chmod that clears it, with the file the directory holds when that
/// carries one too; following each remedy lifts the refusals one directory at a time.
#[test]
fn a_group_write_bit_is_refused_by_path_and_mode_with_the_chmod_that_clears_it() {
    let root = root();
    // Refusals name real paths: the temporary root's own, and the commondir's `../..`
    // resolved.
    let base = std::fs::canonicalize(root.path()).unwrap();
    let common = base.join("common");
    let admin = common.join("worktrees/linked");
    let checkout = base.join("checkout");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir(&checkout).unwrap();
    let (gitfile, commondir, config, worktree) = (
        checkout.join(".git"),
        admin.join("commondir"),
        common.join("config"),
        admin.join("config.worktree"),
    );
    // The pointers as `git worktree add` writes them.
    std::fs::write(&config, b"[user]\nname = common\n").unwrap();
    std::fs::write(&commondir, b"../..\n").unwrap();
    std::fs::write(&gitfile, format!("gitdir: {}\n", admin.display())).unwrap();
    std::fs::write(&worktree, b"[user]\nname = linked\n").unwrap();
    for dir in [&checkout, &common, &admin] {
        chmod(dir, 0o775);
    }
    for file in [&gitfile, &commondir, &config, &worktree] {
        chmod(file, 0o664);
    }
    for (what, dir, file) in [
        ("gitfile", &checkout, &gitfile),
        ("commondir", &admin, &commondir),
        ("common config", &common, &config),
    ] {
        let (d, f) = (dir.display(), file.display());
        let (why, next) = refusal(&checkout);
        assert!(
            why.starts_with(&format!("source {what} {f}: "))
                && why.contains(&format!("{d} is group-writable (mode 0775)")),
            "{what}: {why}"
        );
        assert_eq!(
            next,
            format!("chmod go-w {d} {f} (inverse: chmod g+w {d} && chmod g+w {f})"),
            "{what}"
        );
        chmod(dir, 0o755);
        chmod(file, 0o644);
    }
    let source = SourceConfig::read(&checkout, 1024).unwrap();
    let w = worktree.display();
    let Refusal { why, next } = source.worktree().unwrap_err();
    assert!(
        why.starts_with(&format!(
            "source worktree config {w} is group-writable (mode 0664)"
        )),
        "{why}"
    );
    assert_eq!(next, format!("chmod go-w {w} (inverse: chmod g+w {w})"));
    // A directory whose file is already right names the directory alone, and an other
    // write bit is said as that.
    chmod(&common, 0o757);
    let c = common.display();
    assert_eq!(
        refusal(&checkout).1,
        format!("chmod go-w {c} (inverse: chmod o+w {c})")
    );
    assert!(
        refusal(&checkout)
            .0
            .contains(&format!("{c} is world-writable (mode 0757)")),
        "{:?}",
        refusal(&checkout)
    );
}

/// A refusal for anything but a write bit names the path it judged and its own cause, never
/// another one's, and points at repairing it, never at a chmod.
#[test]
fn other_refusals_name_their_path_and_cause() {
    let root = root();
    let admin = root.path().join("admin");
    std::fs::create_dir(&admin).unwrap();
    chmod(&admin, 0o755);
    let config = admin.join("config");
    std::fs::write(&config, vec![b'x'; 2048]).unwrap();
    chmod(&config, 0o644);
    assert_eq!(
        refusal(&admin),
        (
            format!(
                "source common config {}: longer than such a file can be",
                config.display()
            ),
            REPAIR.to_owned()
        )
    );
    std::fs::write(&config, b"[user]\nname = valid\n").unwrap();
    std::os::unix::fs::symlink(&config, admin.join("commondir")).unwrap();
    assert_eq!(
        refusal(&admin),
        (
            format!(
                "source commondir {}: it or its directory is a symlink",
                admin.join("commondir").display()
            ),
            REPAIR.to_owned()
        )
    );
    std::fs::remove_file(admin.join("commondir")).unwrap();
    std::fs::write(admin.join("commondir"), b" ../other\n").unwrap();
    chmod(&admin.join("commondir"), 0o644);
    assert_eq!(
        refusal(&admin).0,
        format!(
            "source commondir {}: the pointer has unsupported empty, whitespace or control \
             framing",
            admin.join("commondir").display()
        )
    );
}
