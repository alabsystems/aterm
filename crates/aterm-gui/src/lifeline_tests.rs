// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::os::unix::fs::MetadataExt as _;

/// The node `fd` names, read from the descriptor itself, never by its `/dev/fd` path:
/// on Darwin that lookup reads an open descriptor as closed while anything else looks
/// the number up.
fn node(fd: i32) -> Option<(u64, u64)> {
    aterm_uds::spawnfd::descriptor_metadata(fd)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

fn close_on_exec(fd: i32) -> bool {
    // SAFETY: `F_GETFD` only reads one descriptor's flags.
    unsafe { libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0 }
}

/// The copy kept for the cold lane is the lifeline's own FIFO, survives the adoption's sweep,
/// and is close-on-exec except inside [`super::inherit_across_exec`].
#[test]
fn the_relaunch_copy_is_the_lifeline_and_crosses_only_an_exec() {
    let dir = std::env::temp_dir().join(format!("aterm-lifeline-r8-28-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (held, far) = aterm_uds::lifeline::Lifeline::open(&dir).unwrap();
    let _held = std::mem::ManuallyDrop::new(held);
    let number = far.into_raw_fd();
    let adopted = super::adopt_or_exit(number);
    let copy = super::relaunch_descriptor().expect("a relaunch copy is kept");
    assert_ne!(copy, adopted.fd.as_raw_fd());
    assert!(
        node(copy).is_some() && node(copy) == node(adopted.fd.as_raw_fd()),
        "the copy is open, and the lifeline's own FIFO"
    );
    assert!(close_on_exec(copy), "no spawn carries it");
    let inherited = super::inherit_across_exec().expect("inheritable for an exec");
    assert!(!close_on_exec(copy), "the exec carries it");
    drop(inherited);
    assert!(
        close_on_exec(copy),
        "an exec that returned leaves it close-on-exec"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
