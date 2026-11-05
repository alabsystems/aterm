// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
    write_with(&path, 0o600, |writer| {
        writer.write_all(b"complete replacement")
    })
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"complete replacement");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
