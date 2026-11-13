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

/// Maximum size of a single probe-cache file read into memory.
const MAX_PROBE_CACHE_BYTES: usize = 256 * 1024;

/// Monotonic per-process sequence counter disambiguating temp-file names.
static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The file under the state root of the account this process runs as that keeps `name`'s
/// readings ([`aterm_types::dirs::account_state_dir`]: a `sudo aterm doctor` that keeps the
/// person's `HOME` writes root's own, never a root-owned file into the person's state);
/// `None` with no state root or an invalid `name`.
#[must_use]
pub fn file(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return None;
    }
    aterm_types::dirs::account_state_dir().map(|dir| dir.join("doctor").join(name))
}

/// Days since the epoch, UTC: a key part for a reading kept for the day.
#[must_use]
pub fn today() -> Option<u64> {
    aterm_time::since_epoch().map(|d| d.as_secs() / 86_400)
}

/// The parts of a key framed into one, preserving empty fields and their positions.
/// `None` when a part is unavailable or contains a line break or reserved separator:
/// an unread dependency cannot leave a usable partial key.
#[must_use]
pub fn key<I, S>(parts: I) -> Option<String>
where
    I: IntoIterator<Item = Option<S>>,
    S: AsRef<str>,
{
    // The old encoding never began with UNIT. Framing also retires its ambiguous records.
    let mut joined = UNIT.to_string();
    for part in parts {
        let part = part?;
        let part = part.as_ref();
        if part.contains(['\n', '\r', UNIT, RECORD]) {
            return None;
        }
        joined.push_str(part);
        joined.push(UNIT);
    }
    Some(joined)
}

/// What a file is now, followed through links: its path as resolved, device, inode, length and
/// modification time to the nanosecond, plus Unix mode, ownership and change timestamp.
/// The resolved path's native bytes are encoded without lossy conversion. This is metadata
/// identity, not a content digest: a mutation retaining every observed field can retain it.
#[must_use]
pub fn identity(path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&real).ok()?;
    metadata_identity(&real, &meta)
}

/// Describe metadata captured from a held file, without reopening its path.
pub(crate) fn metadata_identity(path: &Path, meta: &std::fs::Metadata) -> Option<String> {
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    #[cfg(unix)]
    let node = {
        use std::os::unix::fs::MetadataExt as _;
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.mode(),
            meta.uid(),
            meta.gid(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    };
    #[cfg(not(unix))]
    let node = String::new();
    Some(format!(
        "native-path-v2:{} {node} {} {modified}",
        aterm_codec::hex::encode_lower(path.as_os_str().as_encoded_bytes()),
        meta.len()
    ))
}

#[cfg(unix)]
fn current_euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and is always safe to call.
    unsafe { libc::geteuid() }
}

fn admitted_dir(dir: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != current_euid() || (meta.mode() & 0o002) != 0 {
            return false;
        }
    }
    true
}

fn read_admitted(file: &Path) -> Option<String> {
    use std::io::Read as _;

    let dir = file.parent()?;
    if !admitted_dir(dir) {
        return None;
    }
    let (handle, meta) = crate::metadata_io::open_regular_with_meta(file).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != current_euid() || (meta.mode() & 0o022) != 0 {
            return None;
        }
    }
    if meta.len() > MAX_PROBE_CACHE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    handle
        .take(MAX_PROBE_CACHE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_PROBE_CACHE_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let mut keys = std::collections::BTreeSet::new();
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n')?;
        let (key, value) = line.split_once(RECORD)?;
        if key.is_empty()
            || line.contains('\r')
            || value.contains(RECORD)
            || !keys.insert(key)
            || keys.len() > ENTRIES
        {
            return None;
        }
    }
    Some(text)
}

/// The reading `file` keeps under exactly `key`. An incomplete, malformed, duplicate-key or
/// over-capacity file is a miss; its records are not carried into the next rewrite.
#[must_use]
pub fn get(file: &Path, key: &str) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let text = read_admitted(file)?;
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(RECORD)?;
        (k == key).then(|| v.to_owned())
    })
}

/// Keep `value` under `key` in `file`, replacing what it kept under that key, newest last and
/// at most [`ENTRIES`] keys within the reader's byte limit; written whole beside it and renamed
/// over it, so a reader never sees half a file. A key or value with a line break or record
/// separator, or a record larger than that limit, is not kept.
pub fn put(file: &Path, key: &str, value: &str) {
    let record_bytes = key
        .len()
        .saturating_add(value.len())
        .saturating_add(RECORD.len_utf8() + 1);
    if key.is_empty()
        || record_bytes > MAX_PROBE_CACHE_BYTES
        || value.contains(['\n', '\r', RECORD])
        || key.contains(['\n', '\r', RECORD])
    {
        return;
    }
    let Some(dir) = file.parent() else {
        return;
    };
    let _serial = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match std::fs::symlink_metadata(dir) {
        Ok(_) if !admitted_dir(dir) => return,
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if crate::platform::make_dirs_born(dir, 0o700).is_err() || !admitted_dir(dir) {
                return;
            }
        }
        Err(_) => return,
    }
    let old = match std::fs::symlink_metadata(file) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                return;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if meta.uid() != current_euid() {
                    return;
                }
            }
            read_admitted(file).unwrap_or_default()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return,
    };
    let mut lines: Vec<&str> = old
        .lines()
        .filter(|line| line.split_once(RECORD).is_some_and(|(k, _)| k != key))
        .collect();
    let line = format!("{key}{RECORD}{value}");
    lines.push(&line);
    let mut keep = lines.len().saturating_sub(ENTRIES);
    let mut bytes: usize = lines[keep..].iter().map(|line| line.len() + 1).sum();
    while bytes > MAX_PROBE_CACHE_BYTES {
        bytes -= lines[keep].len() + 1;
        keep += 1;
    }
    let mut text = lines[keep..].join("\n");
    text.push('\n');
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos());
    let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(".{name}.{}.{nanos}.{seq}", std::process::id()));
    let written = crate::platform::open_create_new_write(&temp, 0o600)
        .and_then(|mut out| std::io::Write::write_all(&mut out, text.as_bytes()));
    if written.and_then(|()| std::fs::rename(&temp, file)).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

#[cfg(test)]
#[path = "probe_cache_tests.rs"]
mod tests;
