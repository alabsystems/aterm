// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn record_publication_respects_a_restrictive_umask() {
    if !crate::testdir::isolated_lifecycle(
        "metadata_io::tests::record_publication_respects_a_restrictive_umask",
    ) {
        return;
    }
    struct RestoreMask(libc::mode_t);
    impl Drop for RestoreMask {
        fn drop(&mut self) {
            // SAFETY: this exact-test child owns its process-wide mask for this scope.
            unsafe { libc::umask(self.0) };
        }
    }
    // SAFETY: the lifecycle helper runs only this case in its owned child process.
    let _restore = RestoreMask(unsafe { libc::umask(0o077) });
    use std::os::unix::fs::PermissionsExt as _;
    let root = fixture("restricted-mask");
    let private = root.join("private");
    let buffered = root.join("buffered");
    let durable = root.join("durable");
    write_private_atomic(&private, b"private bytes").unwrap();
    write_record_atomic(&buffered, b"buffered bytes").unwrap();
    write_record_durably(&durable, b"durable bytes").unwrap();
    for path in [private, buffered, durable] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn publication_policy_preserves_permissions_and_flush_boundary() {
    for (policy, expected_flushes, expected_mode) in [
        (Publication::Private, 0, 0o600),
        (Publication::Record, 0, 0o644),
        (Publication::DurableRecord, 1, 0o644),
    ] {
        let root = fixture("flush-boundary");
        let path = root.join("record");
        std::fs::write(&path, b"previous bytes").unwrap();
        let flushes = std::cell::Cell::new(0);
        let pending = PendingMetadata::prepare_with(&path, b"complete new bytes", policy, |file| {
            assert_eq!(file.metadata()?.len(), b"complete new bytes".len() as u64);
            assert_eq!(
                std::fs::read(&path)?,
                b"previous bytes",
                "flush precedes publication"
            );
            flushes.set(flushes.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(flushes.get(), expected_flushes);
        assert_eq!(std::fs::read(&path).unwrap(), b"previous bytes");
        pending.commit().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"complete new bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            let expected_mode = if matches!(policy, Publication::Private) {
                expected_mode
            } else {
                let control = root.join("prior-creation-policy");
                drop(
                    std::fs::OpenOptions::new()
                        .mode(expected_mode)
                        .create_new(true)
                        .write(true)
                        .open(&control)
                        .unwrap(),
                );
                std::fs::metadata(control).unwrap().permissions().mode() & 0o777
            };
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                expected_mode
            );
        }
        #[cfg(not(unix))]
        let _ = expected_mode;
    }
}

#[test]
fn failed_content_flush_keeps_old_record_and_removes_staging() {
    let root = fixture("flush-failure");
    let path = root.join("record");
    std::fs::write(&path, b"original").unwrap();
    let result =
        PendingMetadata::prepare_with(&path, b"replacement", Publication::DurableRecord, |_| {
            Err(io::Error::other("injected content flush failure"))
        });
    assert!(
        result.is_err(),
        "a required flush failure refuses publication"
    );
    assert_eq!(std::fs::read(path).unwrap(), b"original");
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "failed staging is cleaned"
    );
}

#[test]
fn overlapping_metadata_writes_publish_complete_bytes_and_keep_independent_staging() {
    let root = fixture("overlap");
    let path = root.join("metadata");
    std::fs::write(&path, b"original").unwrap();
    let first =
        PendingMetadata::prepare(&path, b"first complete version", Publication::Private).unwrap();
    let second =
        PendingMetadata::prepare(&path, b"second complete version", Publication::Private).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    first.commit().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first complete version");
    second.commit().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second complete version");
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "staging is cleaned"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn abandoned_and_failed_metadata_writes_preserve_destination_and_remove_staging() {
    let root = fixture("abandon");
    let path = root.join("metadata");
    std::fs::write(&path, b"original").unwrap();
    drop(PendingMetadata::prepare(&path, b"abandoned", Publication::Private).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    let blocked = root.join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    assert!(write_private_atomic(&blocked, b"cannot replace a directory").is_err());
    assert!(blocked.is_dir());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
}

fn fixture(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-metadata-{label}-"))
}

#[test]
fn exact_boundary_is_admitted_and_one_byte_over_is_rejected() {
    let root = fixture("boundary");
    let exact = root.join("exact");
    let over = root.join("over");
    std::fs::write(&exact, b"12345678").unwrap();
    std::fs::write(&over, b"123456789").unwrap();
    assert_eq!(read_bounded_regular(&exact, 8).unwrap(), b"12345678");
    let error = read_bounded_regular(&over, 8).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A DIRECTORY WHERE METADATA BELONGS IS NOT REGULAR, on every platform: the reader opens it
/// and refuses it by its handle, never with an access error a caller would keep as unreadable
/// (a ready marker so kept reads as complete). Windows opens a directory only when asked to.
#[test]
fn a_directory_at_a_metadata_path_is_refused_as_not_regular() {
    let root = fixture("directory");
    let path = root.join("18.ready");
    std::fs::create_dir_all(path.join("occupied")).unwrap();
    assert_eq!(
        read_bounded_regular(&path, 4096).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn line_reduction_bounds_records_and_detects_changes_after_partial_observation() {
    let root = fixture("line-reduction");
    let path = root.join("records");
    std::fs::write(&path, b"one\ntwo\n").unwrap();
    let mut names = Vec::new();
    read_regular_lines(&path, 4, |line| {
        names.push(line.trim().to_string());
        Ok(true)
    })
    .unwrap();
    assert_eq!(names, ["one", "two"]);
    assert!(read_regular_lines(&path, 3, |_| Ok(true)).is_err());
    std::fs::write(&path, [b'o', b'k', b'\n', 0xff]).unwrap();
    assert!(read_regular_lines(&path, 4, |_| Ok(true)).is_err());
    std::fs::write(&path, b"one\ntwo\n").unwrap();
    assert!(
        read_regular_lines(&path, 4, |_| {
            std::fs::write(&path, b"replacement is larger\n")?;
            Ok(false)
        })
        .is_err(),
        "even a completed prefix is refused if its admitted file changes"
    );
}

#[cfg(unix)]
#[test]
fn writerless_fifo_and_final_symlink_return_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = fixture("special");
    let fifo = root.join("fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    assert_eq!(
        read_bounded_regular(&fifo, 64).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    let target = root.join("target");
    let link = root.join("link");
    std::fs::write(&target, b"status").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(read_bounded_regular(&link, 64).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn explicit_follow_reader_supports_and_safely_retargets_config_symlinks() {
    let root = fixture("follow-link");
    let first = root.join("first.toml");
    let second = root.join("second.toml");
    let logical = root.join("aterm.toml");
    std::fs::write(&first, "[packages]\nchannel = \"first\"\n").unwrap();
    std::fs::write(&second, "[packages]\nchannel = \"second\"\n").unwrap();
    std::os::unix::fs::symlink(&first, &logical).unwrap();
    assert!(
        read_bounded_regular_utf8_follow_final_links(&logical, 128)
            .unwrap()
            .contains("first")
    );
    std::fs::remove_file(&logical).unwrap();
    std::os::unix::fs::symlink(&second, &logical).unwrap();
    assert!(
        read_bounded_regular_utf8_follow_final_links(&logical, 128)
            .unwrap()
            .contains("second")
    );
    std::fs::remove_dir_all(&root).unwrap();
}
