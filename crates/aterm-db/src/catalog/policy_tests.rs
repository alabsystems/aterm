// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Policy;

/// A symlinked `aterm.toml` (a dotfiles checkout, home-manager) gives Commons its policy as
/// the window reads the file, so sharing that reads on in the window starts Commons here too.
#[test]
fn a_symlinked_aterm_toml_gives_commons_its_policy() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let target = scratch.path().join("dotfiles.toml");
    aterm_uds::atomic_file::write(&target, b"[commons]\nenabled = false\n", 0o600).unwrap();
    let link = scratch.path().join("aterm.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(!Policy::read(&link).expect("the link is followed").enabled);
    std::fs::write(&target, "[swarm]\nenabled = true\n").unwrap();
    assert!(Policy::read(&link).expect("the link is followed").enabled);
}
