// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const RECORD: &str = "host-record-test";

#[test]
fn a_record_is_durable_one_line_per_mac_and_bounded() {
    use aterm_durable::{Flush, seam::Recording};
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let config = scratch.path().join("aterm.toml");
    let path = scratch.path().join(RECORD);
    let named = |host: &str| lists(&config, RECORD, host).unwrap();
    assert!(!named("MAC-A"), "a genuine absence");
    let recording = Recording::start();
    note(&config, RECORD, "MAC-A", 100).unwrap();
    let durable: &[Flush] = if cfg!(unix) {
        &[Flush::Durable, Flush::Durable]
    } else {
        &[Flush::Durable]
    };
    assert_eq!(
        recording.take(),
        durable,
        "written durably: the record, then (unix) its folder's entry"
    );
    drop(recording);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "MAC-A 100\n");
    assert!(named("MAC-A"));
    assert!(!named("MAC-B"), "another Mac on a synced config");
    note(&config, RECORD, "MAC-A", 200).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "MAC-A 100\n",
        "one line per Mac"
    );
    for (n, host) in ["B", "C", "D", "E", "F", "G", "H"].iter().enumerate() {
        note(&config, RECORD, &format!("MAC-{host}"), 300 + n as u64).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().lines().count(),
        MAX_LINES
    );
    note(&config, RECORD, "MAC-I", 400).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), MAX_LINES);
    assert!(text.starts_with("MAC-B 300\n") && text.ends_with("MAC-I 400\n"));
    assert!(!named("MAC-A"), "the oldest Mac went");
    let long = |i: u8| format!("{}{}", "F".repeat(MAX_HOST_BYTES - 1), char::from(b'0' + i));
    for i in 0..8 {
        note(&config, RECORD, &long(i), u64::MAX).unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() as usize <= MAX_BYTES);
    assert!(named(&long(7)));
    let before = std::fs::read(&path).unwrap();
    let too_long = "F".repeat(MAX_HOST_BYTES + 1);
    for bad in [
        "",
        "MAC A",
        "MAC-A\n",
        "MAC-A 1",
        "mac/a",
        too_long.as_str(),
    ] {
        assert_eq!(
            lists(&config, RECORD, bad).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "{bad:?}"
        );
        assert_eq!(
            note(&config, RECORD, bad, 1).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::InvalidInput),
            "{bad:?}"
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    forget(&config, RECORD, &long(7)).unwrap();
    assert!(!named(&long(7)), "forgotten");
}

#[cfg(unix)]
#[test]
fn an_unreadable_record_is_an_error_and_is_left_as_found() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let config = scratch.path().join("aterm.toml");
    let path = scratch.path().join(RECORD);

    let target = scratch.path().join("elsewhere");
    std::fs::write(&target, "MAC-B 1\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(
        lists(&config, RECORD, "MAC-B").is_err(),
        "a link is never read through"
    );
    assert!(note(&config, RECORD, "MAC-A", 2).is_err());
    assert!(forget(&config, RECORD, "MAC-B").is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "MAC-B 1\n");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_file(&path).unwrap();

    std::fs::create_dir(&path).unwrap();
    assert!(lists(&config, RECORD, "MAC-A").is_err(), "a folder");
    assert!(note(&config, RECORD, "MAC-A", 2).is_err());
    assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
    std::fs::remove_dir(&path).unwrap();

    let nine = (0..=MAX_LINES)
        .map(|i| format!("MAC-{i} 1\n"))
        .collect::<String>();
    let oversized = format!("MAC-B 1\n{}\n", "x".repeat(MAX_BYTES));
    for bytes in [
        oversized.as_bytes(),
        nine.as_bytes(),
        &b""[..],
        &b"\n"[..],
        &b"MAC-B\n"[..],
        &b"MAC-B 1"[..],
        &b"MAC-B x\n"[..],
        &b"MAC-B -1\n"[..],
        &b"MAC-B +1\n"[..],
        &b"MAC-B 1 2\n"[..],
        &b"MAC-B  1\n"[..],
        &b"MAC/B 1\n"[..],
        &b"MAC-B 1\n\n"[..],
        &b"MAC-B 99999999999999999999999\n"[..],
        &b"\xff 1\n"[..],
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(
            lists(&config, RECORD, "MAC-A").is_err(),
            "{:?} is unreadable",
            String::from_utf8_lossy(bytes)
        );
        assert!(note(&config, RECORD, "MAC-A", 2).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "left as found");
    }
    std::fs::write(&path, "MAC-B 1\nMAC-C 2\n").unwrap();
    assert!(!lists(&config, RECORD, "MAC-A").unwrap());
}
