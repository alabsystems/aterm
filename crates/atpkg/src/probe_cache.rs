// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Readings a doctor keeps between runs: each answer a probe spawned a program for, kept under
//! a key that names everything the answer depends on (a file's identity, a link's target, a
//! receipt's time), so a run whose key matches reads it instead of spawning again and a change
//! to any part of the key retires it. A cache that cannot be read or written is no error: the
//! probe runs as it would without one.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The most keys one file keeps; the oldest goes first.
const ENTRIES: usize = 64;

/// Separates the parts of a key ([`key`]).
const UNIT: char = '\u{1f}';

/// Separates a key from its value on a line.
const RECORD: char = '\u{1e}';

/// Serializes this process's rewrites, so two probes on two threads keep both answers.
static WRITES: Mutex<()> = Mutex::new(());

/// The file under the state root of the account this process runs as that keeps `name`'s
/// readings ([`aterm_types::dirs::account_state_dir`]: a `sudo aterm doctor` that keeps the
/// person's `HOME` writes root's own, never a root-owned file into the person's state);
/// `None` with no state root.
#[must_use]
pub fn file(name: &str) -> Option<PathBuf> {
    aterm_types::dirs::account_state_dir().map(|dir| dir.join("doctor").join(name))
}

/// Days since the epoch, UTC: a key part for a reading kept for the day.
#[must_use]
pub fn today() -> Option<u64> {
    aterm_time::since_epoch().map(|d| d.as_secs() / 86_400)
}

/// The parts of a key joined into one; `None` when a part could not be read (no key is
/// better than one that leaves out what the answer depends on) or holds a line break.
#[must_use]
pub fn key<I, S>(parts: I) -> Option<String>
where
    I: IntoIterator<Item = Option<S>>,
    S: AsRef<str>,
{
    let mut joined = String::new();
    for part in parts {
        let part = part?;
        let part = part.as_ref();
        if part.contains(['\n', '\r', UNIT, RECORD]) {
            return None;
        }
        if !joined.is_empty() {
            joined.push(UNIT);
        }
        joined.push_str(part);
    }
    Some(joined)
}

/// What a file is now, followed through links: its path as resolved, device, inode, length and
/// modification time to the nanosecond. A rewrite, a replacement or a removal changes it.
#[must_use]
pub fn identity(path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&real).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    #[cfg(unix)]
    let node = {
        use std::os::unix::fs::MetadataExt as _;
        format!("{}:{}", meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let node = String::new();
    Some(format!(
        "{} {node} {} {modified}",
        real.display(),
        meta.len()
    ))
}

/// The reading `file` keeps under exactly `key`.
#[must_use]
pub fn get(file: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(RECORD)?;
        (k == key).then(|| v.to_owned())
    })
}

/// Keep `value` under `key` in `file`, replacing what it kept under that key, newest last and
/// at most [`ENTRIES`] keys; written whole beside it and renamed over it, so a reader never
/// sees half a file. A key or value with a line break or a record separator is not kept.
pub fn put(file: &Path, key: &str, value: &str) {
    if value.contains(['\n', '\r', RECORD]) || key.contains(['\n', '\r', RECORD]) {
        return;
    }
    let _serial = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let old = std::fs::read_to_string(file).unwrap_or_default();
    let mut lines: Vec<&str> = old
        .lines()
        .filter(|line| line.split_once(RECORD).is_some_and(|(k, _)| k != key))
        .collect();
    let line = format!("{key}{RECORD}{value}");
    lines.push(&line);
    let keep = lines.len().saturating_sub(ENTRIES);
    let mut text = lines[keep..].join("\n");
    text.push('\n');
    let Some(dir) = file.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos());
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.{}.{nanos}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut out| std::io::Write::write_all(&mut out, text.as_bytes()));
    if written.and_then(|()| std::fs::rename(&temp, file)).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_is_found_only_under_its_whole_key_and_the_newest_stay() {
        let dir = crate::testdir::dir("atpkg-probe-cache-");
        let file = dir.join("doctor").join("readings");
        assert_eq!(get(&file, "a"), None, "nothing kept yet");
        put(&file, "a", "1");
        put(&file, "b", "2");
        put(&file, "a", "3");
        assert_eq!(get(&file, "a").as_deref(), Some("3"));
        assert_eq!(get(&file, "b").as_deref(), Some("2"));
        assert_eq!(get(&file, "ab"), None);
        for n in 0..ENTRIES {
            put(&file, &format!("k{n}"), "v");
        }
        assert_eq!(get(&file, "a"), None, "the oldest key went first");
        assert_eq!(
            get(&file, &format!("k{}", ENTRIES - 1)).as_deref(),
            Some("v")
        );
        put(&file, "c", "two\nlines");
        assert_eq!(
            get(&file, "c"),
            None,
            "a value with a line break is not kept"
        );
        assert_eq!(key([Some("x"), None]), None, "an unread part leaves no key");
        let joined = key([Some("x"), Some("y")]).expect("two read parts");
        assert_eq!(joined, "x\u{1f}y");
        put(&file, &joined, "joined");
        assert_eq!(
            get(&file, &joined).as_deref(),
            Some("joined"),
            "a joined key is kept"
        );
        assert_eq!(get(&file, "x"), None, "a key's first part is not the key");
    }

    #[test]
    fn a_rewritten_file_has_another_identity() {
        let dir = crate::testdir::dir("atpkg-probe-cache-id-");
        let file = dir.join("bin");
        std::fs::write(&file, "one").unwrap();
        let first = identity(&file).expect("a file has an identity");
        assert_eq!(identity(&file).as_deref(), Some(first.as_str()));
        std::fs::write(&file, "three").unwrap();
        assert_ne!(identity(&file).as_deref(), Some(first.as_str()));
        assert_eq!(identity(&dir.join("gone")), None);
    }
}
