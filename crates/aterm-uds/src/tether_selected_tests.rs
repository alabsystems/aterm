// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{deadline, spawnfd};
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::time::Duration;

#[test]
fn selected_native_copy_executes_and_same_byte_source_replacement_refuses() {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("cat");
    deadline::ad_hoc_copy(Path::new("/bin/cat"), &source);
    let until = Instant::now() + Duration::from_secs(60);
    let image = SelectedExecutable::retain(&source, until).unwrap();
    let (read, mut write) = std::io::pipe().unwrap();
    write.write_all(b"ran\n").unwrap();
    drop(write);
    let input: OwnedFd = read.into();
    let mut command = spawnfd::command(image.path(until).unwrap());
    let output = deadline::output_within_from(
        &mut command,
        spawnfd::Stdio::from(input),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"ran\n");
    // Only the persistent retained path was executed. These source files
    // were read as inputs, and their temporary directory may now disappear.
    let old = root.path().join("original-cat");
    std::fs::rename(&source, &old).unwrap();
    std::fs::copy(&old, &source).unwrap();
    assert!(
        image.path(until).is_err(),
        "equal bytes do not replace the selected inode"
    );
}

#[test]
fn non_native_selected_file_is_not_executed() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let file = root.path().join("unqualified");
    std::fs::write(&file, b"not a native executable").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (result, spawned) = spawnfd::spawns_during(|| {
        SelectedExecutable::retain(&file, Instant::now() + Duration::from_secs(60))
    });
    assert!(result.is_err());
    assert_eq!(spawned, 0);
}
