// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk watch's reserve on Windows.

use std::os::windows::fs::MetadataExt as _;

use super::*;
use crate::harness::disk::tests::Tmp;

const SPARSE: u32 = 0x200;
const REPARSE: u32 = 0x400;
const COMPRESSED: u32 = 0x800;
const NOT_CONTENT_INDEXED: u32 = 0x2000;

fn run(tool: &str, args: &[&str], path: &Path) {
    let out = aterm_uds::spawnfd::command(tool)
        .args(args)
        .arg(path)
        .output()
        .expect("the tool ran");
    assert!(
        out.status.success(),
        "{tool} {args:?} {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn attributes(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("there")
        .file_attributes()
}

fn foreign_reparse(path: &Path) {
    use std::os::windows::fs::OpenOptionsExt as _;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(super::super::win::FILE_FLAG_OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
        .open(path)
        .expect("opened as itself");
    super::super::win::set_foreign_reparse_point(&file).expect("a reparse point set");
    let meta = std::fs::symlink_metadata(path).expect("there");
    assert!(!meta.file_type().is_symlink(), "std reads no link");
    assert_ne!(meta.file_attributes() & REPARSE, 0, "a reparse point");
}

fn read_as_itself(path: &Path) -> Vec<u8> {
    use std::io::Read as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    let mut bytes = Vec::new();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(super::super::win::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .expect("opened as itself")
        .read_to_end(&mut bytes)
        .expect("read");
    bytes
}

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .expect("listed")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_reserve_is_written_once_and_released_through_its_handle() {
    let tmp = Tmp::new("reserve-win");
    let home = tmp.path().join("home");
    let path = path_for(&home);
    assert_eq!(
        path,
        home.join("AppData")
            .join("Local")
            .join("aterm")
            .join("disk-reserve")
            .join("reserve")
    );
    assert_eq!(state(&path), Ok(State::Absent));
    let bytes = 3 * 1024 * 1024 + 5;
    assert_eq!(ensure(&path, bytes), Ok(true));
    let attrs = attributes(&path);
    assert_ne!(attrs & NOT_CONTENT_INDEXED, 0, "{attrs:#x}");
    assert_eq!(attrs & (SPARSE | COMPRESSED | REPARSE), 0, "{attrs:#x}");
    let file = std::fs::File::open(&path).expect("open");
    let held = super::super::win::allocated(&file).expect("allocation");
    drop(file);
    assert!(held >= bytes, "{held} held for {bytes}");
    assert_eq!(state(&path), Ok(State::Present(bytes)));
    assert_eq!(ensure(&path, bytes), Ok(false), "kept as is");
    assert_eq!(release(&path), Ok(Some(bytes)));
    assert!(std::fs::symlink_metadata(&path).is_err(), "gone");
    assert_eq!(release(&path), Ok(None));
    assert!(
        names_in(path.parent().expect("folder")).is_empty(),
        "no temporary left"
    );
}

#[test]
fn a_junction_for_the_reserves_folder_is_never_followed() {
    let tmp = Tmp::new("reserve-junction");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(elsewhere.join("reserve"), b"not the reserve").expect("file");
    let path = path_for(&tmp.path().join("home"));
    let folder = path.parent().expect("folder").to_path_buf();
    std::fs::create_dir_all(folder.parent().expect("aterm")).expect("aterm");
    atpkg::platform::atomic_symlink(&elsewhere, &folder).expect("junction");
    assert_ne!(attributes(&folder) & REPARSE, 0, "a junction");
    assert!(matches!(state(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(release(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(ensure(&path, 1024), Err(ReserveError::NotOurs(_))));
    assert_eq!(
        std::fs::read(elsewhere.join("reserve")).expect("file"),
        b"not the reserve"
    );
    assert_eq!(names_in(&elsewhere), ["reserve"], "nothing written there");
    assert_ne!(attributes(&folder) & REPARSE, 0, "the junction stands");
}

#[test]
fn a_junction_at_the_reserves_name_is_never_removed() {
    let tmp = Tmp::new("reserve-junction-name");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(elsewhere.join("keep"), b"keep me").expect("file");
    let path = path_for(&tmp.path().join("home"));
    std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
    atpkg::platform::atomic_symlink(&elsewhere, &path).expect("junction");
    assert!(matches!(state(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(release(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(ensure(&path, 1024), Err(ReserveError::NotOurs(_))));
    assert_eq!(
        std::fs::read(elsewhere.join("keep")).expect("kept"),
        b"keep me"
    );
    assert_ne!(attributes(&path) & REPARSE, 0, "the junction stands");
}

#[test]
fn releasing_a_hard_link_removes_only_its_name() {
    let tmp = Tmp::new("reserve-hardlink");
    let other = tmp.path().join("other");
    std::fs::write(&other, vec![7u8; 64 * 1024]).expect("other");
    let path = path_for(&tmp.path().join("home"));
    std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
    std::fs::hard_link(&other, &path).expect("hard link");
    assert_eq!(release(&path), Ok(Some(64 * 1024)));
    assert!(
        std::fs::symlink_metadata(&path).is_err(),
        "the name is gone"
    );
    assert_eq!(std::fs::read(&other).expect("kept"), vec![7u8; 64 * 1024]);
}

#[test]
fn a_reparse_point_that_is_no_link_is_never_the_reserve() {
    let tmp = Tmp::new("reserve-foreign-reparse");
    let path = path_for(&tmp.path().join("home"));
    std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
    let body = vec![0xa5_u8; 64 * 1024];
    std::fs::write(&path, &body).expect("a file of the reserve's name");
    foreign_reparse(&path);
    assert!(std::fs::symlink_metadata(&path).expect("there").is_file());
    assert!(matches!(state(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(release(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(
        ensure(&path, 64 * 1024),
        Err(ReserveError::NotOurs(_))
    ));
    assert_ne!(attributes(&path) & REPARSE, 0, "left as it is");
    assert_eq!(read_as_itself(&path), body);

    let path = path_for(&tmp.path().join("home-folder"));
    let folder = path.parent().expect("folder").to_path_buf();
    std::fs::create_dir_all(&folder).expect("folder");
    foreign_reparse(&folder);
    assert!(std::fs::symlink_metadata(&folder).expect("there").is_dir());
    assert!(matches!(state(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(release(&path), Err(ReserveError::NotOurs(_))));
    assert!(matches!(ensure(&path, 1024), Err(ReserveError::NotOurs(_))));
    assert_ne!(attributes(&folder) & REPARSE, 0, "left as it is");
}

#[test]
fn a_compressed_or_sparse_reserve_is_not_counted_and_is_replaced() {
    let tmp = Tmp::new("reserve-thin");
    let path = path_for(&tmp.path().join("home"));
    let folder = path.parent().expect("folder").to_path_buf();
    std::fs::create_dir_all(&folder).expect("folder");
    run("compact", &["/c", "/q"], &folder);
    assert_ne!(attributes(&folder) & COMPRESSED, 0, "a compressing folder");
    let refused = ensure(&path, 1024 * 1024);
    assert!(
        matches!(&refused, Err(ReserveError::NotOurs(why)) if why.contains("compresses")),
        "{refused:?}"
    );
    assert!(
        names_in(&folder).is_empty(),
        "nothing written: {:?}",
        names_in(&folder)
    );

    let mib = 1024 * 1024;
    for (name, tool, args) in [
        ("compressed", "compact", &["/c", "/q"][..]),
        ("sparse", "fsutil", &["sparse", "setflag"][..]),
    ] {
        let path = path_for(&tmp.path().join(name));
        let thin = || {
            run(tool, args, &path);
            assert_ne!(
                attributes(&path) & (COMPRESSED | SPARSE),
                0,
                "{name}: {:#x}",
                attributes(&path)
            );
            assert!(
                matches!(state(&path), Err(ReserveError::Thin(_))),
                "{name}: {:?}",
                state(&path)
            );
        };
        assert_eq!(ensure(&path, mib), Ok(true), "{name}");
        thin();
        assert_eq!(ensure(&path, mib), Ok(true), "{name}: written over");
        assert_eq!(attributes(&path) & (COMPRESSED | SPARSE), 0, "{name}");
        assert_eq!(state(&path), Ok(State::Present(mib)), "{name}");
        thin();
        let released = release(&path);
        assert!(
            matches!(released, Ok(Some(held)) if held <= mib),
            "{name}: {released:?}"
        );
        assert!(std::fs::symlink_metadata(&path).is_err(), "{name}: gone");
        assert_eq!(
            names_in(path.parent().expect("folder")),
            Vec::<String>::new(),
            "{name}: no temporary left"
        );
    }
}

#[test]
fn the_pass_lock_lets_one_pass_run_at_a_time_on_windows() {
    let tmp = Tmp::new("pass-lock-win");
    let home = tmp.path().join("home");
    let path = pass_lock_for(&home);
    assert_eq!(
        path.parent(),
        path_for(&home).parent(),
        "beside the reserve"
    );
    let first = take_pass(&path).expect("made").expect("taken");
    assert!(take_pass(&path).expect("opened").is_none(), "held");
    drop(first);
    let again = take_pass(&path).expect("opened");
    assert!(again.is_some(), "released");
    drop(again);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(elsewhere.join("keep"), b"keep").expect("file");
    std::fs::remove_file(&path).expect("removed");
    atpkg::platform::atomic_symlink(&elsewhere, &path).expect("junction");
    assert!(take_pass(&path).is_err(), "a junction is not followed");
    assert_eq!(
        std::fs::read(elsewhere.join("keep")).expect("kept"),
        b"keep"
    );
    assert_eq!(names_in(&elsewhere), ["keep"]);
}

#[test]
fn a_reparse_point_at_the_pass_locks_name_is_never_the_lock() {
    let tmp = Tmp::new("pass-lock-reparse");
    let path = pass_lock_for(&tmp.path().join("home"));
    std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
    std::fs::write(&path, b"not the lock").expect("a file of the lock's name");
    foreign_reparse(&path);
    let taken = take_pass(&path);
    assert!(taken.is_err(), "{:?}", taken.map(|t| t.is_some()));
    assert_ne!(attributes(&path) & REPARSE, 0, "left as it is");
    assert_eq!(read_as_itself(&path), b"not the lock");
}
