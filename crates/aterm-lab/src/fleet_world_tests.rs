// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::open_fds;

fn ran(image: &std::path::Path) {
    let mut child = aterm_uds::spawnfd::command(image)
        .arg("image-ran")
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("start the copied image");
    aterm_uds::deadline::prints(&mut child, "image-ran", super::HANG);
    assert!(child.wait().unwrap().success());
}

/// A build may rewrite its output in place as well as rename over it. The world's image
/// still executes afterwards; a symlink or hard link would observe the replacement.
#[test]
fn the_private_image_outlives_a_rewrite_of_the_build_output() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let mut scratch =
        aterm_uds::deadline::KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
    let source = scratch.path().join("source");
    let image = scratch.path().join("aterm");
    aterm_uds::deadline::ad_hoc_copy(std::path::Path::new("/bin/echo"), &source);
    super::copy_image(&source, &image).unwrap();
    assert_ne!(
        std::fs::metadata(&source).unwrap().ino(),
        std::fs::metadata(&image).unwrap().ino()
    );
    assert_eq!(
        std::fs::metadata(&image).unwrap().permissions().mode() & 0o222,
        0
    );
    ran(&source);
    ran(&image);
    std::fs::write(&source, b"not an executable").unwrap();
    ran(&image);
    scratch.ran();
}

/// A copied program that does not produce the app's version cannot authorize cleanup.
#[test]
fn a_failed_version_probe_keeps_the_copied_image() {
    let mut scratch =
        aterm_uds::deadline::KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
    let image = scratch.path().join("aterm");
    aterm_uds::deadline::ad_hoc_copy(std::path::Path::new("/bin/echo"), &image);
    let mut world = super::World {
        base: scratch.path().to_path_buf(),
        bin: scratch.path().to_path_buf(),
        aterm: image.clone(),
        insts: Vec::new(),
        keep: false,
        extra: std::collections::BTreeMap::new(),
    };
    assert!(world.prove_image().is_err());
    assert!(world.keep);
    drop(world);
    assert!(image.is_file());
    // Explicit stdout proof makes removing this test's intentionally retained image safe.
    ran(&image);
    scratch.ran();
}

/// A member's descriptors are counted from its table; a reaped one is left out.
#[test]
fn a_members_descriptors_are_counted_and_a_gone_one_left_out() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn a member");
    let pid = i32::try_from(child.id()).unwrap();
    let counted = open_fds(&[pid]);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(counted.get(&pid), Some(&3), "stdin, stdout and stderr");
    assert!(open_fds(&[pid]).is_empty(), "reaped");
}
