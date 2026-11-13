// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-pin-{label}-"))
}

#[test]
fn an_unreadable_pin_file_cannot_be_replaced_by_a_pin_edit() {
    let l = layout("unreadable-edit");
    let original = b"ay\n\xff";
    fs::write(pins_path(&l), original).unwrap();
    assert!(pinned_set(&l).is_err());
    assert!(is_pinned(&l, "ay").is_err());
    assert!(holds_update(&l, "ay"));
    assert!(set_pinned(&l, "trust", true).is_err());
    assert_eq!(fs::read(pins_path(&l)).unwrap(), original);
    assert!(set_pinned(&l, "ay", false).is_err());
    assert_eq!(fs::read(pins_path(&l)).unwrap(), original);
}

#[test]
fn set_is_pinned_roundtrip_and_0600() {
    let l = layout("roundtrip");
    assert!(!is_pinned(&l, "ay").unwrap());
    assert!(set_pinned(&l, "ay", true).unwrap(), "first pin is a change");
    assert!(is_pinned(&l, "ay").unwrap());
    #[cfg(unix)]
    {
        let mode = fs::metadata(pins_path(&l)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "pin file is 0600");
    }
    assert!(
        !set_pinned(&l, "ay", true).unwrap(),
        "re-pinning is a no-op change"
    );
    assert!(set_pinned(&l, "ay", false).unwrap(), "unpin is a change");
    assert!(!is_pinned(&l, "ay").unwrap());
    let _ = fs::remove_dir_all(&l.prefix);
}

#[test]
fn rejects_unsafe_program_names() {
    let l = layout("unsafe");
    for bad in [
        "", ".", "..", "a/b", "a\\b", "x\ny", "x\ry", "x\0y", " ay", "ay ", "\tay", "   ",
    ] {
        let err = set_pinned(&l, bad, true).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidInput,
            "{bad:?} must be rejected"
        );
    }
    assert!(!pins_path(&l).exists(), "a rejected name writes no file");
    let _ = fs::remove_dir_all(&l.prefix);
}

#[test]
fn pinned_set_reads_all() {
    let l = layout("reads-all");
    set_pinned(&l, "ay", true).unwrap();
    set_pinned(&l, "trust", true).unwrap();
    assert_eq!(
        pinned_set(&l).unwrap(),
        BTreeSet::from(["ay".to_string(), "trust".to_string()])
    );
    let _ = fs::remove_dir_all(&l.prefix);
}

#[test]
fn invalid_stored_names_preserve_the_entire_pin_file() {
    let l = layout("invalid-stored");
    let original = "ay\n../trust\n";
    fs::write(pins_path(&l), original).unwrap();
    assert_eq!(
        pinned_set(&l).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(holds_update(&l, "ay"));
    assert!(set_pinned(&l, "ay", false).is_err());
    assert_eq!(fs::read_to_string(pins_path(&l)).unwrap(), original);
}

#[test]
fn oversized_sparse_and_overcount_pin_files_are_errors() {
    let l = layout("bounded");
    let path = pins_path(&l);
    let file = fs::File::create(&path).unwrap();
    file.set_len((MAX_PIN_FILE_BYTES + 1) as u64).unwrap();
    assert!(pinned_set(&l).is_err());
    assert!(holds_update(&l, "ay"));
    assert!(set_pinned(&l, "ay", true).is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        (MAX_PIN_FILE_BYTES + 1) as u64
    );

    let exact_with_dup: String = (0..MAX_PINNED_PROGRAMS)
        .map(|index| format!("p{index}\n"))
        .chain(std::iter::once("p0\n".to_string()))
        .collect();
    fs::write(&path, exact_with_dup).unwrap();
    assert_eq!(
        pinned_set(&l).unwrap().len(),
        MAX_PINNED_PROGRAMS,
        "duplicate lines do not push a 2048-member set over the cap"
    );
    let err = set_pinned(&l, "p2048", true).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        pinned_set(&l).unwrap().len(),
        MAX_PINNED_PROGRAMS,
        "refusing the 2049th pin preserves all 2048 existing pins"
    );

    let body: String = (0..=MAX_PINNED_PROGRAMS)
        .map(|index| format!("p{index}\n"))
        .collect();
    fs::write(&path, &body).unwrap();
    assert!(pinned_set(&l).is_err(), "over-count is never partial");
    assert!(set_pinned(&l, "ay", true).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
    let _ = fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn fifo_and_symlink_pin_files_return_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let l = layout("special");
    let path = pins_path(&l);
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert!(pinned_set(&l).is_err());
    assert!(holds_update(&l, "ay"));
    assert!(set_pinned(&l, "ay", true).is_err());
    fs::remove_file(&path).unwrap();
    let target = l.prefix.join("target-pins");
    fs::write(&target, "ay\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(pinned_set(&l).is_err(), "pin links are not followed");
    assert!(set_pinned(&l, "trust", true).is_err());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "ay\n");
    let _ = fs::remove_dir_all(&l.prefix);
}
