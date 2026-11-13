// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn original_output_descriptor_is_borrowed_and_refuses_replaced_namespace() {
    use std::os::unix::fs::MetadataExt as _;
    let temporary = aterm_tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    let path = directory.join("output");
    std::fs::create_dir(&path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let output =
        trust_resource_provenance::program::produced::HeldOutputRoot::open(&path, deadline)
            .unwrap();
    let borrowed = output.descriptor(deadline).unwrap();
    assert_eq!(
        output.descriptor(deadline).unwrap().as_raw_fd(),
        borrowed.as_raw_fd()
    );
    let original =
        std::fs::File::from(aterm_uds::kept_out(|| borrowed.try_clone_to_owned()).unwrap());
    let before = original.metadata().unwrap();
    std::fs::rename(&path, directory.join("original")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(output.descriptor(deadline).is_err());
    let retained = original.metadata().unwrap();
    let replacement = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (retained.dev(), retained.ino()),
        (before.dev(), before.ino())
    );
    assert_ne!(
        (replacement.dev(), replacement.ino()),
        (before.dev(), before.ino())
    );
}

#[test]
fn original_gnu_has_zero_supplement_and_refuses_foreign_or_swapped_rights() {
    let original = RootGnu::new().unwrap();
    let copied = duplicate_pipe(&original.pipe).unwrap();
    for (source, copy) in original.pipe.iter().zip(&copied) {
        // SAFETY: both descriptions remain owned throughout these read-only flag queries.
        let flags = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        // SAFETY: the still-owned duplicate is read without changing its flags or OFD.
        assert_eq!(
            unsafe { libc::fcntl(copy.as_raw_fd(), libc::F_GETFD) },
            flags
        );
    }
    assert!(original.matches(&copied).unwrap());
    let [read, write] = duplicate_pipe(&copied).unwrap();
    assert!(!original.matches(&[write, read]).unwrap());
    let foreign = RootGnu::new().unwrap();
    assert!(!original.matches(&foreign.pipe).unwrap());
    let mut queued: libc::c_int = -1;
    // SAFETY: FIONREAD only observes the held original GNU read end.
    assert_eq!(
        unsafe { libc::ioctl(original.pipe[0].as_raw_fd(), libc::FIONREAD, &mut queued) },
        0
    );
    assert_eq!(
        queued, 0,
        "one original implicit CPU never creates a supplemental GNU token"
    );
}
