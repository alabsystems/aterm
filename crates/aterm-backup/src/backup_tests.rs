// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch_file() -> (aterm_tempfile::TempDir, std::path::PathBuf, File) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("kept");
    let file = File::create(&path).expect("create");
    (dir, path, file)
}

/// The inert arm writes nothing, whatever it answers: a control that the attribute below is
/// the system arm's own doing.
#[test]
#[cfg(target_os = "macos")]
fn the_system_arm_marks_a_file_and_the_inert_arm_leaves_it_alone() {
    let (_dir, path, file) = scratch_file();
    Backups::INERT.exclude(&file).expect("inert");
    assert!(!Backups::SYSTEM.excluded(&file), "the inert arm wrote it");
    assert!(Backups::INERT.excluded(&file));
    Backups::SYSTEM.exclude(&file).expect("marked");
    assert!(Backups::SYSTEM.excluded(&file));
    Backups::SYSTEM.exclude(&file).expect("marked twice");
    let again = File::open(&path).expect("reopen");
    assert!(Backups::SYSTEM.excluded(&again), "it is on the file");
}

/// By path reaches the same mark, and a folder takes it.
#[test]
#[cfg(target_os = "macos")]
fn a_folder_is_marked_by_its_path() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("unmarked");
    std::fs::create_dir(&path).expect("folder");
    let folder = File::open(&path).expect("open");
    assert!(!Backups::SYSTEM.excluded(&folder));
    Backups::SYSTEM.exclude_path(&path).expect("marked");
    assert!(Backups::SYSTEM.excluded(&folder));
}

/// The value `tmutil addexclusion` wrote (`xattr -px` on a file it excluded, macOS 27.0) reads
/// as excluded, and marking over it leaves it: compared by bytes, a file `tmutil` excluded
/// read as unmarked, and the next mark overwrote its value.
#[test]
#[cfg(target_os = "macos")]
fn the_value_tmutil_writes_counts_and_is_left_alone() {
    const TMUTIL: &[u8] = &[
        0x62, 0x70, 0x6C, 0x69, 0x73, 0x74, 0x30, 0x30, 0x5F, 0x10, 0x11, 0x63, 0x6F, 0x6D, 0x2E,
        0x61, 0x70, 0x70, 0x6C, 0x65, 0x2E, 0x62, 0x61, 0x63, 0x6B, 0x75, 0x70, 0x64, 0x08, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x1C,
    ];
    let (_dir, _path, file) = scratch_file();
    platform::set(&file, TMUTIL).expect("the attribute");
    assert!(Backups::SYSTEM.excluded(&file), "tmutil's own value");
    Backups::SYSTEM.exclude(&file).expect("marked");
    assert_eq!(platform::value(&file), TMUTIL, "and it is left as it was");
}

/// A system with no mark says so at every step: an `Ok` here is what let a caller report a key
/// left out of backups that a backup copies.
#[test]
fn a_system_with_no_mark_says_so() {
    let (_dir, path, file) = scratch_file();
    assert_eq!(
        Backups::NO_MARK.exclude(&file).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        Backups::NO_MARK.exclude_path(&path).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(!Backups::NO_MARK.excluded(&file));
}

/// This system's own answer agrees with [`Backups::system_has_a_mark`], so the arm above stands
/// for the real one wherever there is no mark.
#[test]
fn the_system_arm_answers_as_the_system_does() {
    let (_dir, _path, file) = scratch_file();
    let marked = Backups::SYSTEM.exclude(&file);
    assert_eq!(marked.is_ok(), Backups::system_has_a_mark(), "{marked:?}");
    assert_eq!(
        Backups::SYSTEM.excluded(&file),
        Backups::system_has_a_mark()
    );
}

/// A path that names nothing is an error, not a silent success: the mark is what a caller
/// relies on.
#[test]
fn a_missing_path_says_so() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let err = Backups::SYSTEM
        .exclude_path(&dir.path().join("none"))
        .expect_err("no such file");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}

#[test]
fn build_folders_report_absent_marks_and_propagate_creation_errors() {
    let root = aterm_tempfile::tempdir().unwrap();
    let build = root.path().join("build.noindex");
    validate_build_dir(&build).unwrap();
    assert!(
        !build.exists(),
        "a custody preflight must not create the directory"
    );
    assert!(!Backups::NO_MARK.build_dir(&build).unwrap());
    assert!(build.is_dir());
    assert!(Backups::INERT.build_dir(&build).unwrap());
    let file = root.path().join("file.noindex");
    std::fs::write(&file, b"kept").unwrap();
    assert!(Backups::INERT.build_dir(&file).is_err());
    assert_eq!(std::fs::read(file).unwrap(), b"kept");
}

#[test]
fn an_indexed_build_folder_is_refused_before_creation() {
    let root = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    let path = root.path().join("build");
    assert_eq!(
        validate_build_dir(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        Backups::INERT.build_dir(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!path.exists());
}

#[test]
fn a_noindex_symlink_cannot_hide_an_indexed_destination() {
    let root = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = root.path().join("alias.noindex");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    assert_eq!(
        Backups::INERT
            .build_dir(&alias.join("not-created"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!real.join("not-created").exists());
}

#[test]
fn a_noindex_parent_cannot_be_escaped_with_dot_dot() {
    let root = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    let named = root.path().join("child.noindex");
    std::fs::create_dir(&named).unwrap();
    let escaped = named.join("../not-created");
    assert_eq!(
        Backups::INERT.build_dir(&escaped).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!root.path().join("not-created").exists());
}

#[test]
fn a_symlink_and_dot_dot_cannot_escape_a_noindex_parent() {
    let root = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
    let named = root.path().join("named.noindex");
    let indexed = root.path().join("indexed/deep");
    std::fs::create_dir(&named).unwrap();
    std::fs::create_dir_all(&indexed).unwrap();
    std::os::unix::fs::symlink(&indexed, named.join("jump")).unwrap();
    let path = named.join("jump/../not-created");
    assert_eq!(
        Backups::INERT.build_dir(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!root.path().join("indexed/not-created").exists());
}
