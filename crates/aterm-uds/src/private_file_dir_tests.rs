// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn already_opened_private_directory_never_reopens_its_replaced_name() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("current");
    let moved = scratch.path().join("original");
    crate::private_file::create_private_dir(&path).unwrap();
    let original = std::fs::File::open(&path).unwrap();
    std::fs::rename(&path, &moved).unwrap();
    crate::private_file::create_private_dir(&path).unwrap();
    let directory = PrivateDirectory::from_file(original).unwrap();
    directory
        .view()
        .unwrap()
        .write("owned", b"same original descriptor")
        .unwrap();
    assert!(!path.join("owned").exists());
    assert_eq!(
        std::fs::read(moved.join("owned")).unwrap(),
        b"same original descriptor"
    );
    assert!(directory.is_at(&moved).unwrap());
    assert!(!directory.is_at(&path).unwrap());
    let ordinary = std::fs::File::create(scratch.path().join("ordinary-file")).unwrap();
    assert!(PrivateDirectory::from_file(ordinary).is_err());
}
use std::os::unix::fs::{PermissionsExt, symlink};

#[cfg(target_os = "macos")]
#[test]
fn a_directory_stream_retains_the_supported_child_inheritance_flags() {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = PrivateDirectory::open(scratch.path()).unwrap();
    let supports_fork_flag = astream_sys::close_on_fork(root.file.as_fd()).unwrap();
    let entries = root.view().unwrap().entries().unwrap();
    // SAFETY: the stream owns this live descriptor throughout the flag observation.
    let flags = unsafe { libc::fcntl(entries.as_fd().as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags, -1);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    if supports_fork_flag {
        // Darwin FD_CLOFORK, also used by the pinned astream-sys implementation.
        assert_ne!(
            flags & 2,
            0,
            "fdopendir must precede the final inheritance mark"
        );
    }
}

#[test]
fn reads_writes_listing_removal_and_locks_stay_on_the_open_directory() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("current");
    let retired = scratch.path().join("retired");
    crate::private_file::create_private_dir(&path).unwrap();
    let root = PrivateDirectory::open(&path).unwrap();
    root.view().unwrap().write("row", b"old").unwrap();
    let keeper = root.lock(".keeper", Duration::ZERO, "busy").unwrap();
    // A writer's lock is independent of the keeper's named lock.
    let writer = root.lock(".lock", Duration::ZERO, "busy").unwrap();
    std::fs::rename(&path, &retired).unwrap();
    crate::private_file::create_private_dir(&path).unwrap();
    let replacement = PrivateDirectory::open(&path).unwrap();
    replacement.view().unwrap().write("row", b"new").unwrap();
    assert!(!root.is_at(&path).unwrap());
    assert!(root.is_at(&retired).unwrap());
    let view = root.view().unwrap();
    assert_eq!(view.read("row", 100).unwrap(), b"old");
    view.write("old-only", b"bound").unwrap();
    let names: Vec<_> = view.names().unwrap().collect::<io::Result<_>>().unwrap();
    assert!(names.iter().any(|name| name == "old-only"));
    assert!(replacement.view().unwrap().read("old-only", 100).is_err());
    view.remove("row").unwrap();
    assert_eq!(
        replacement.view().unwrap().read("row", 100).unwrap(),
        b"new"
    );
    assert!(root.lock(".keeper", Duration::ZERO, "busy").is_err());
    assert!(root.lock(".lock", Duration::ZERO, "busy").is_err());
    let new_keeper = replacement.lock(".keeper", Duration::ZERO, "busy").unwrap();
    let new_writer = replacement.lock(".lock", Duration::ZERO, "busy").unwrap();
    drop((keeper, writer, new_keeper, new_writer));
    assert!(root.lock(".keeper", Duration::ZERO, "busy").is_ok());
}

#[test]
fn publication_and_failed_temporary_cleanup_use_the_original_directory() {
    for fail in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("current");
        let retired = scratch.path().join("retired");
        crate::private_file::create_private_dir(&path).unwrap();
        let root = PrivateDirectory::open(&path).unwrap();
        let view = root.view().unwrap();
        view.write("row", b"before").unwrap();
        let result = view.write_checked("row", b"after", || {
            std::fs::rename(&path, &retired)?;
            crate::private_file::create_private_dir(&path)?;
            crate::private_file::write_private(&path, "row", b"replacement")?;
            if fail {
                Err(io::Error::other("injected pre-publication failure"))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.is_err(), fail);
        assert_eq!(
            view.read("row", 100).unwrap(),
            if fail { &b"before"[..] } else { &b"after"[..] }
        );
        assert_eq!(
            crate::private_file::read_private(&path, "row", 100).unwrap(),
            b"replacement"
        );
        let names: Vec<_> = view.names().unwrap().collect::<io::Result<_>>().unwrap();
        assert!(
            !names
                .iter()
                .any(|name| name.to_string_lossy().contains(".tmp."))
        );
    }
}

#[test]
fn admission_is_rechecked_and_children_refuse_links_and_nonregular_files() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("private");
    crate::private_file::create_private_dir(&path).unwrap();
    let alias = scratch.path().join("alias");
    symlink(&path, &alias).unwrap();
    assert!(PrivateDirectory::open(&alias).is_err());
    let root = PrivateDirectory::open(&path).unwrap();
    let view = root.view().unwrap();
    view.write("row", b"private").unwrap();
    symlink("row", path.join("link")).unwrap();
    std::fs::hard_link(path.join("row"), path.join("second-link")).unwrap();
    assert!(view.read("row", 100).is_err());
    assert!(view.read("link", 100).is_err());
    assert!(root.lock("link", Duration::ZERO, "busy").is_err());
    let fifo = std::ffi::CString::new(path.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a terminated scratch path with no interior NUL, used only for this call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(view.read("fifo", 100).is_err());
    assert!(root.lock("fifo", Duration::ZERO, "busy").is_err());
    for name in ["", ".", "..", "../escape", "nested/row", "nul\0tail"] {
        assert!(view.read(name, 100).is_err());
        assert!(view.write(name, b"bytes").is_err());
        assert!(view.remove(name).is_err());
        assert!(root.lock(name, Duration::ZERO, "busy").is_err());
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(root.view().is_err());
    assert!(root.lock("new-lock", Duration::ZERO, "busy").is_err());
    assert!(PrivateDirectory::open(&path).is_err());
}
