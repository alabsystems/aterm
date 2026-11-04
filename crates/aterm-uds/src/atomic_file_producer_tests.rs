// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn producer_error_and_panic_leave_the_old_file_and_no_temporary() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    write(&path, b"old", 0o600).unwrap();
    let failed = write_with(&path, 0o600, |writer| {
        writer.write_all(b"incomplete prefix")?;
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "producer failed"))
    });
    assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let panicked = std::panic::catch_unwind(|| {
        let _ = write_with(&path, 0o600, |writer| {
            writer.write_all(b"another prefix")?;
            panic!("producer panic");
        });
    });
    assert!(panicked.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn producer_check_and_directory_failure_keep_the_atomic_boundary() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    write(&path, b"old", 0o600).unwrap();
    let events = RefCell::new(Vec::new());
    let result = write_produced_landing(
        &path,
        0o640,
        |writer| {
            assert_eq!(std::fs::read(&path)?, b"old");
            events.borrow_mut().push("produce");
            writer.write_all(b"new ")?;
            writer.write_all(b"records")
        },
        || {
            assert_eq!(std::fs::read(&path)?, b"old");
            events.borrow_mut().push("check");
            Ok(())
        },
        |_| {
            assert_eq!(std::fs::read(&path)?, b"new records");
            events.borrow_mut().push("directory");
            Err(io::Error::other("directory failed after rename"))
        },
    )
    .unwrap();
    assert!(result.is_err(), "the failure is after, not before, rename");
    assert_eq!(*events.borrow(), ["produce", "check", "directory"]);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let refused = write_produced_landing(
        &path,
        0o600,
        |writer| writer.write_all(b"not published"),
        || Err(io::Error::new(io::ErrorKind::PermissionDenied, "changed")),
        |_| panic!("a refused check cannot reach directory flushing"),
    );
    assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&path).unwrap(), b"new records");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn metadata_capture_observes_the_written_temporary_and_failure_keeps_old_file() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    write(&path, b"old", 0o600).unwrap();
    let old = std::fs::metadata(&path).unwrap();
    let (identity, landed) = write_captured_landing(
        &path,
        0o600,
        |writer| writer.write_all(b"new"),
        |file| {
            let metadata = file.metadata()?;
            assert_ne!((metadata.dev(), metadata.ino()), (old.dev(), old.ino()));
            assert_eq!(metadata.len(), 3);
            assert_eq!(std::fs::read(&path)?, b"old");
            Ok((metadata.dev(), metadata.ino()))
        },
        || Ok(()),
        |_| Ok(()),
    )
    .unwrap();
    landed.unwrap();
    let current = std::fs::metadata(&path).unwrap();
    assert_eq!(identity, (current.dev(), current.ino()));
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    let failed = write_captured_landing(
        &path,
        0o600,
        |writer| writer.write_all(b"lost"),
        |_| {
            Err::<(), _>(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "metadata failed",
            ))
        },
        || panic!("capture failure reached check"),
        |_| panic!("capture failure reached rename"),
    );
    assert_eq!(failed.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
