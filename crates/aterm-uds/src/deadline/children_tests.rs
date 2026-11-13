// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Write as _;
use std::os::fd::AsFd as _;

use super::writers_gone;

#[test]
fn a_pipe_is_writer_less_once_its_last_writer_closes() {
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    assert!(!writers_gone(reader.as_fd()), "open and empty");
    writer.write_all(b"x").expect("write");
    assert!(!writers_gone(reader.as_fd()), "open with data");
    let second = writer.try_clone().expect("a second writer");
    drop(writer);
    assert!(!writers_gone(reader.as_fd()), "a second writer holds it");
    drop(second);
    let hang = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !writers_gone(reader.as_fd()) {
        assert!(std::time::Instant::now() < hang, "closed with data left");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
