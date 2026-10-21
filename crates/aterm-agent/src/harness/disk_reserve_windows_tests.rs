// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk watch's reserve on Windows: real files in a scratch
//! directory on NTFS, links made as junctions (a file symlink needs a
//! privilege a plain account lacks), compression and sparseness set by the
//! system's own `compact` and `fsutil`.

use std::os::windows::fs::MetadataExt as _;

use super::*;
use crate::harness::disk::tests::Tmp;

/// `FILE_ATTRIBUTE_*` read back in these tests.
const SPARSE: u32 = 0x200;
const REPARSE: u32 = 0x400;
const COMPRESSED: u32 = 0x800;
const NOT_CONTENT_INDEXED: u32 = 0x2000;

/// Run one of the system's own tools on `path`, which must succeed.
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

/// The attributes of what is at `path`, not following a link.
fn attributes(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("there")
        .file_attributes()
}

/// Give the file (or EMPTY folder) at `path` a reparse point that is no link
/// ([`super::super::win::set_foreign_reparse_point`]): std then reads it as a
/// plain file or folder — `is_symlink` false — as it reads a cloud
/// placeholder, which a test cannot make.
fn foreign_reparse(path: &Path) {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_FLAG_BACKUP_SEMANTICS`: what opens a folder.
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

/// The bytes of the file at `path` that carries a reparse point no filter
/// owns, read through the point itself (an ordinary open is refused).
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

/// The files in `folder`, by name.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .expect("listed")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// THE RESERVE IS WRITTEN, KEPT AND RELEASED ON WINDOWS: under the profile's
/// `AppData\Local\aterm\disk-reserve`, written once `NOT_CONTENT_INDEXED`,
/// neither sparse nor compressed nor a reparse point, holding at least its
/// length in clusters; then left as it is, and released by a delete through
/// its handle that says what it held; a second release finds nothing.
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

/// The reserve's folder a JUNCTION to a folder holding a file of the
/// reserve's name is refused by `state`, `ensure` and `release`; that file
/// survives byte for byte, and so does the junction. A regression guard: std
/// reads a junction as a symlink too, so this does not tell the any-reparse-
/// point check from `is_symlink` alone —
/// [`a_reparse_point_that_is_no_link_is_never_the_reserve`] does.
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

/// A junction (a folder link) AT the reserve's name is not a regular file:
/// refused by all three, and what it leads to is untouched. A regression
/// guard, as above: std reads it as a symlink, and as no file either.
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

/// A HARD LINK AT THE RESERVE'S NAME is a regular file holding its blocks,
/// so it is the reserve; its release removes that NAME only, and the other
/// name keeps the bytes (as an unlink does on Unix).
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

/// A NON-SURROGATE REPARSE POINT — what std reads as a plain file or folder,
/// a cloud placeholder's kind — at the reserve's name or as its folder is not
/// the reserve: `state`, `ensure` and `release` refuse it as not ours and
/// leave it as it is, its bytes and its reparse point. NEGATIVE CONTROL for
/// the any-reparse-point check: `is_symlink` is false for both, so a check
/// that asked only that would take the file for the reserve.
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

/// A RESERVE MUST HOLD ITS BLOCKS. NEGATIVE CONTROL: in a folder that
/// compresses what is written into it, `ensure` writes nothing (no reserve, no
/// temporary). A reserve that `compact` compressed, or `fsutil` made sparse,
/// in place is not counted as the reserve (`state` says it is thin), yet it is
/// still aterm's own file, never left for good: `ensure` writes a whole one
/// over it, and at an emergency `release` deletes a thin one for what it
/// holds.
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

/// ONE PASS AT A TIME ON WINDOWS: the pass lock is taken without waiting, a
/// second taker (another open, as another instance makes) finds it held, and
/// once the first lets go the next takes it. A junction at the lock's name is
/// refused and what it leads to is untouched — by Windows itself, which will
/// not open a folder as the lock's file, before any check of ours runs; the
/// check after the open has its own control,
/// [`a_reparse_point_at_the_pass_locks_name_is_never_the_lock`].
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

/// THE PASS LOCK IS NEVER TAKEN THROUGH A REPARSE POINT: a file at the lock's
/// name carrying one that is no link (std reads it as a plain file, so the
/// open as itself succeeds) is refused by the check after the open, and is
/// left as it is. NEGATIVE CONTROL: without that check the lock would be
/// taken on it (a junction never reaches the check: see above).
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
