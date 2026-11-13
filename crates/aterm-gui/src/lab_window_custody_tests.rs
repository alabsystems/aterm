// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn failed_exec_and_partial_setup_restore_inheritance_flags() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = std::fs::File::create(dir.path().join("guard")).unwrap();
    let fd = file.as_raw_fd();
    // SAFETY: file owns this descriptor throughout this test.
    let flags = || unsafe { libc::fcntl(fd, libc::F_GETFD) };
    let original = flags();
    assert_ne!(original & libc::FD_CLOEXEC, 0);
    {
        let _inherit = inherit_descriptors(vec![fd]).unwrap();
        assert_eq!(flags() & libc::FD_CLOEXEC, 0);
    }
    assert_eq!(flags(), original);
    assert!(inherit_descriptors(vec![fd, -1]).is_err());
    assert_eq!(flags(), original);
}

#[test]
fn successor_replaces_consumed_numbers_and_preserves_payload() {
    let args = [
        "--window",
        "--lab-window-guard",
        "7",
        "--lab-window-memory-guard",
        "8",
        "old.claim",
        "-e",
        "tool",
        "--lab-window-guard",
        "99",
    ]
    .map(OsString::from)
    .to_vec();
    let offer = aterm_types::lab_window_guard::GuardBundle {
        lane: 31,
        memory: Some(aterm_types::lab_window_guard::MemoryRef {
            descriptor: 32,
            claim: "original.claim".into(),
        }),
    };
    let mut replaced = replace_offer(args.clone(), Some(offer.clone()));
    assert_eq!(
        replace_offer(replaced.clone(), Some(offer.clone())),
        replaced
    );
    assert_eq!(
        aterm_types::lab_window_guard::take(&mut replaced).unwrap(),
        Some(offer)
    );
    assert_eq!(
        replaced,
        ["--window", "-e", "tool", "--lab-window-guard", "99"].map(OsString::from)
    );
    assert_eq!(replace_offer(args, None), replaced);
}
