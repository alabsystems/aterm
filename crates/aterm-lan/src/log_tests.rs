// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs::OpenOptions;
use std::os::unix::fs::MetadataExt as _;

use super::*;

/// Every write repairs an oversized inherited file and keeps the same inode at the cap.
/// NEGATIVE CONTROL: a raw append to that same descriptor grows past the cap; the next
/// bounded write repairs it. Both append and ordinary inherited offsets are covered.
#[test]
fn the_existing_log_inode_is_bounded_even_after_an_oversized_predecessor() {
    let root = aterm_tempfile::tempdir().unwrap();
    for append in [false, true] {
        let path = root.path().join(format!("log-{append}"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .append(append)
            .open(&path)
            .unwrap();
        let inode = file.metadata().unwrap().ino();
        file.set_len(129).unwrap();
        let mut sink = Bounded { file, cap: 64 };
        for _ in 0..20 {
            sink.write(b"a state changed\n").unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            assert!(meta.len() <= sink.cap);
            assert_eq!(
                meta.ino(),
                inode,
                "launchd must keep writing the bounded inode"
            );
            assert!(
                !std::fs::read(&path).unwrap().contains(&0),
                "no hole at the old offset"
            );
        }
        sink.file.write_all(&[b'x'; 65]).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > sink.cap);
        sink.write(b"repaired\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired\n");
        assert!(sink.write(&[b'x'; 65]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"repaired\n");
    }
}
