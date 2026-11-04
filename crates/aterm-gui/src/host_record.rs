// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Once-per-Mac records beside `aterm.toml`: one `<host-uuid> <secs>` line per Mac, at most
//! [`MAX_LINES`] of them, the oldest dropped first, so a config folder synced between Macs keeps
//! each Mac's line apart. A record that is not a bounded regular UTF-8 file in that form (a
//! link, a folder, a malformed line) is never read through and is left as found.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The Macs a record keeps.
pub(crate) const MAX_LINES: usize = 8;

const MAX_HOST_BYTES: usize = 64;

const MAX_BYTES: usize = MAX_LINES * (MAX_HOST_BYTES + 1 + 20 + 1);

/// Serializes this process's read-modify-writes of every record.
static RECORDS: Mutex<()> = Mutex::new(());

fn host_key_ok(host: &str) -> bool {
    (1..=MAX_HOST_BYTES).contains(&host.len())
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn bad_key() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "not a host key the record can hold",
    )
}

fn parse(text: &str) -> Option<Vec<(&str, u64)>> {
    let body = text.strip_suffix('\n')?;
    let mut lines = Vec::new();
    for line in body.split('\n') {
        let (host, secs) = line.split_once(' ')?;
        if !host_key_ok(host) || secs.is_empty() || !secs.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        lines.push((host, secs.parse().ok()?));
    }
    (lines.len() <= MAX_LINES).then_some(lines)
}

fn record_path(config_path: &Path, record: &str) -> std::io::Result<PathBuf> {
    config_path
        .parent()
        .map(|dir| dir.join(record))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config directory to record in",
            )
        })
}

fn read_text(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(_) => crate::config_marker::read_marker(path, MAX_BYTES)
            .map(Some)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the record is not a bounded regular UTF-8 record",
                )
            }),
    }
}

fn malformed() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "the record is malformed and is left as found",
    )
}

/// Whether the record `record` beside `config_path` names `host`; an error for a bad key or a
/// record that cannot be read whole.
pub(crate) fn lists(config_path: &Path, record: &str, host: &str) -> std::io::Result<bool> {
    if !host_key_ok(host) {
        return Err(bad_key());
    }
    let Some(text) = read_text(&record_path(config_path, record)?)? else {
        return Ok(false);
    };
    parse(&text)
        .map(|lines| lines.iter().any(|&(named, _)| named == host))
        .ok_or_else(malformed)
}

/// Write `host` into the record `record` beside `config_path`, durably; a Mac already named
/// keeps its line.
pub(crate) fn note(config_path: &Path, record: &str, host: &str, now: u64) -> std::io::Result<()> {
    use std::fmt::Write as _;
    if !host_key_ok(host) {
        return Err(bad_key());
    }
    let path = record_path(config_path, record)?;
    let _serial = RECORDS.lock().unwrap_or_else(|p| p.into_inner());
    let text = read_text(&path)?;
    let mut lines = match text.as_deref() {
        None => Vec::new(),
        Some(text) => parse(text).ok_or_else(malformed)?,
    };
    if lines.iter().any(|&(named, _)| named == host) {
        return Ok(());
    }
    lines.push((host, now));
    let keep = lines.len().saturating_sub(MAX_LINES);
    let mut written = String::new();
    for (named, secs) in &lines[keep..] {
        let _ = writeln!(written, "{named} {secs}");
    }
    crate::config_marker::write_marker(&path, &written, MAX_BYTES, crate::durable::Flush::Durable)
}

/// Take `host` out of the record `record` beside `config_path`, written as [`note`] writes it;
/// a record left naming no Mac is removed.
pub(crate) fn forget(config_path: &Path, record: &str, host: &str) -> std::io::Result<()> {
    use std::fmt::Write as _;
    let path = record_path(config_path, record)?;
    let _serial = RECORDS.lock().unwrap_or_else(|p| p.into_inner());
    let Some(text) = read_text(&path)? else {
        return Ok(());
    };
    let lines = parse(&text).ok_or_else(malformed)?;
    if !lines.iter().any(|&(named, _)| named == host) {
        return Ok(());
    }
    let mut kept = String::new();
    for (named, secs) in lines.iter().filter(|&&(named, _)| named != host) {
        let _ = writeln!(kept, "{named} {secs}");
    }
    if kept.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    crate::config_marker::write_marker(&path, &kept, MAX_BYTES, crate::durable::Flush::Durable)
}

#[cfg(test)]
mod tests {
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
}
