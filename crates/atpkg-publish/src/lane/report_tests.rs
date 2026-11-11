// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// NEGATIVE CONTROL: `trim_tail` aligns on a whole line (or UTF-8 codepoint boundary when a
/// single line spans the kept window) instead of splitting multi-byte UTF-8 characters like
/// `—` (`0xE2 0x80 0x94`) into invalid UTF-8 or leaving a torn leading fragment.
#[test]
fn trim_tail_preserves_whole_lines_and_utf8_boundaries() {
    let log = "lane: WARNING — first line\nlane: WARNING — second line\n";
    let trimmed = trim_tail(log.as_bytes(), 36);
    assert_eq!(
        std::str::from_utf8(trimmed).expect("valid UTF-8"),
        "lane: WARNING — second line\n"
    );

    let single_line = "——————end\n";
    // Cut 1 byte into the multi-byte `—` sequence with only a trailing newline in `tail`.
    let kept = single_line.len() - 1;
    let single_trimmed = trim_tail(single_line.as_bytes(), kept);
    assert_eq!(
        std::str::from_utf8(single_trimmed).expect("valid UTF-8 without mid-codepoint tear"),
        "—————end\n"
    );
}

#[cfg(unix)]
#[test]
fn write_atomic_bytes_replaces_symlink_target_path_without_following() {
    let root = aterm_tempfile::Builder::new()
        .prefix("atpkg-report-atomic-")
        .tempdir()
        .expect("scratch dir");
    let outside = root.path().join("outside.txt");
    std::fs::write(&outside, b"keep-me").expect("write outside");
    let dest = root.path().join("state.txt");
    std::os::unix::fs::symlink(&outside, &dest).expect("symlink dest");

    write_atomic(&dest, "fresh\n").expect("atomic write");
    assert!(
        std::fs::symlink_metadata(&dest)
            .expect("dest meta")
            .file_type()
            .is_file()
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "fresh\n");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep-me");
}
