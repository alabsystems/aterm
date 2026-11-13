// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn scratch(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-clone-{label}-"))
}

#[test]
fn a_clone_keeps_length_mode_and_time_and_takes_its_own_inode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = scratch("keeps");
    let src = dir.join("trustc");
    std::fs::write(&src, b"the Trust compiler").unwrap();
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&src)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let dst = dir.join("rustc");
    clone_file(&src, &dst).unwrap();
    let (s, d) = (
        std::fs::metadata(&src).unwrap(),
        std::fs::metadata(&dst).unwrap(),
    );
    assert_eq!(std::fs::read(&dst).unwrap(), b"the Trust compiler");
    assert_eq!(d.permissions().mode() & 0o7777, 0o755);
    assert_eq!(d.modified().unwrap(), old, "the time is the source's");
    assert_ne!(s.ino(), d.ino(), "a clone is its own inode");
    assert_eq!(s.nlink(), 1, "the source gained no link");
    assert!(is_clone_of(&src, &dst));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hard_link_a_symlink_and_a_changed_file_are_not_clones() {
    let dir = scratch("not");
    let src = dir.join("tippy");
    std::fs::write(&src, b"tippy").unwrap();
    let built = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&src)
        .unwrap()
        .set_modified(built)
        .unwrap();
    let link = dir.join("hard");
    std::fs::hard_link(&src, &link).unwrap();
    assert!(
        !is_clone_of(&src, &link),
        "a hard link is the store's own inode, which is exactly what a view must not be"
    );
    let sym = dir.join("sym");
    std::os::unix::fs::symlink(&src, &sym).unwrap();
    assert!(!is_clone_of(&src, &sym));
    let clone = dir.join("clone");
    clone_file(&src, &clone).unwrap();
    assert!(is_clone_of(&src, &clone));
    std::fs::write(&clone, b"tipsy").unwrap();
    assert!(
        !is_clone_of(&src, &clone),
        "a rewritten clone no longer is one"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn same_bytes_sees_a_signature_only_difference_that_the_attributes_cannot() {
    let dir = scratch("bytes");
    let trustc = dir.join("trustc");
    let rustc = dir.join("rustc");
    let mut image = vec![7u8; 200_000];
    std::fs::write(&trustc, &image).unwrap();
    image[150_000] = 8;
    std::fs::write(&rustc, &image).unwrap();
    let t = std::fs::metadata(&trustc).unwrap().modified().unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&rustc)
        .unwrap()
        .set_modified(t)
        .unwrap();
    assert!(
        is_clone_of(&trustc, &rustc),
        "length, mode and time cannot tell them apart, so neither can the clone identity"
    );
    assert!(!same_bytes(&trustc, &rustc).unwrap(), "the bytes can");
    let clone = dir.join("clone");
    clone_file(&trustc, &clone).unwrap();
    assert!(same_bytes(&trustc, &clone).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clone_refuses_an_occupied_name_and_a_non_file() {
    let dir = scratch("refuse");
    let src = dir.join("targo");
    std::fs::write(&src, b"targo").unwrap();
    let dst = dir.join("cargo");
    std::fs::write(&dst, b"a copy the bundle shipped").unwrap();
    assert_eq!(
        clone_file(&src, &dst).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&dst).unwrap(), b"a copy the bundle shipped");
    assert_eq!(
        clone_file(&dir, &dir.join("d")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let _ = std::fs::remove_dir_all(&dir);
}
