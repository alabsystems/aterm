// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::headless_boot::{LOG_TAIL_BYTES, log_tail};

#[test]
fn a_log_tail_bounds_long_lines_and_keeps_diagnostics_after_invalid_bytes() {
    use std::io::{Seek as _, SeekFrom, Write as _};
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("instance.log");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"old-marker\n").unwrap();
    file.set_len(LOG_TAIL_BYTES * 4).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(b"latest-marker\n").unwrap();
    let tail = log_tail(&path);
    assert!(tail.ends_with("latest-marker"));
    assert!(!tail.contains("old-marker"));
    assert!(tail.len() <= usize::try_from(LOG_TAIL_BYTES).unwrap() * 3);

    file.write_all(b"\xfffinal-marker\n").unwrap();
    let tail = log_tail(&path);
    assert!(tail.ends_with("final-marker"));
    assert!(tail.len() <= usize::try_from(LOG_TAIL_BYTES).unwrap() * 3);
}
