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
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_same_size_rewrite_with_restored_mtime_retires_a_reading() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = crate::testdir::dir("atpkg-probe-cache-rewrite-");
        let bin = dir.join("compiler");
        std::fs::write(&bin, b"old!").unwrap();
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let restore = || {
            std::fs::File::options()
                .write(true)
                .open(&bin)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        };
        restore();
        let before = std::fs::metadata(&bin).unwrap();
        let old = identity(&bin).unwrap();
        let cache = dir.join("doctor/readings");
        put(&cache, &old, "old answer");
        std::fs::write(&bin, b"new!").unwrap();
        restore();
        let after = std::fs::metadata(&bin).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.uid(),
                before.gid(),
                before.len(),
                before.modified().unwrap()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.uid(),
                after.gid(),
                after.len(),
                after.modified().unwrap()
            )
        );
        assert_ne!(
            (before.ctime(), before.ctime_nsec()),
            (after.ctime(), after.ctime_nsec()),
            "the filesystem must report this fixture's mutation"
        );
        let current = identity(&bin).unwrap();
        assert_eq!(
            get(&cache, &current),
            None,
            "served the answer from before the rewrite"
        );
        put(&cache, &current, "new answer");
        assert_eq!(get(&cache, &current).as_deref(), Some("new answer"));
        assert_eq!(identity(&bin).unwrap(), current);
    }

    #[test]
    fn malformed_cache_records_are_misses_and_a_new_answer_repairs_them() {
        let dir = crate::testdir::dir("atpkg-probe-cache-records-");
        let file = dir.join("doctor/readings");
        put(&file, "k", "old");
        let good = format!("k{RECORD}old\n");
        let too_many = (0..=ENTRIES)
            .map(|n| format!("k{n}{RECORD}v\n"))
            .collect::<String>();
        for bad in [
            format!("k{RECORD}partial"),
            format!("{good}k{RECORD}different\n"),
            format!("{good}{good}"),
            format!("{good}{RECORD}empty-key\n"),
            format!("{good}other{RECORD}two{RECORD}values\n"),
            format!("k{RECORD}old\r\n"),
            format!("{good}unfinished"),
            format!("{good}{too_many}"),
        ] {
            std::fs::write(&file, &bad).unwrap();
            assert_eq!(get(&file, "k"), None, "accepted malformed cache {bad:?}");
            assert_eq!(std::fs::read_to_string(&file).unwrap(), bad);
            put(&file, "fresh", "answer");
            assert_eq!(get(&file, "fresh").as_deref(), Some("answer"));
            assert_eq!(get(&file, "k"), None, "retained a malformed old file");
        }
        put(&file, "empty", "");
        assert_eq!(get(&file, "empty"), Some(String::new()));
        assert_eq!(get(&file, "fresh").as_deref(), Some("answer"));
    }

    #[test]
    fn cache_writes_fit_the_read_limit_without_partial_records() {
        let dir = crate::testdir::dir("atpkg-probe-cache-bytes-");
        let file = dir.join("doctor/readings");
        let large = "é".repeat(MAX_PROBE_CACHE_BYTES / 4);
        put(&file, "oldest", &large);
        put(&file, "newest", &large);
        assert!(
            file.metadata().unwrap().len() <= MAX_PROBE_CACHE_BYTES as u64,
            "the writer exceeded the reader's byte limit"
        );
        assert!(get(&file, "newest").is_some_and(|value| value == large));
        assert_eq!(get(&file, "oldest"), None);
        let before = std::fs::read(&file).unwrap();
        let oversized = "é".repeat(MAX_PROBE_CACHE_BYTES / 2);
        for (key, value) in [("too-big", oversized.as_str()), (oversized.as_str(), "x")] {
            put(&file, key, value);
            assert!(
                std::fs::read(&file).unwrap() == before,
                "an oversized record replaced readable cache entries"
            );
        }
        let exact_key = "exact";
        let exact = "y".repeat(MAX_PROBE_CACHE_BYTES - exact_key.len() - RECORD.len_utf8() - 1);
        put(&file, exact_key, &exact);
        assert_eq!(file.metadata().unwrap().len(), MAX_PROBE_CACHE_BYTES as u64);
        assert!(get(&file, exact_key).is_some_and(|value| value == exact));
        assert_eq!(get(&file, "newest"), None);
        put(&file, "tail", "small");
        assert_eq!(get(&file, "tail").as_deref(), Some("small"));
        assert_eq!(get(&file, exact_key), None);
    }

    #[test]
    fn empty_key_fields_keep_their_positions_and_cannot_reuse_old_records() {
        let shapes: &[&[&str]] = &[
            &["x"],
            &["", "x"],
            &["x", ""],
            &["", "x", ""],
            &[],
            &[""],
            &["", ""],
            &["x", "y"],
            &["x", "", "y"],
        ];
        let mut keys = Vec::new();
        for parts in shapes {
            let encoded = key(parts.iter().copied().map(Some)).unwrap();
            assert!(!keys.contains(&encoded), "key fields collapsed: {parts:?}");
            keys.push(encoded);
        }
        let dir = crate::testdir::dir("atpkg-probe-key-fields-");
        let file = dir.join("doctor/readings");
        put(&file, "x", "old ambiguous encoding");
        assert_eq!(get(&file, &keys[0]), None);
        for (index, encoded) in keys.iter().enumerate() {
            put(&file, encoded, &index.to_string());
        }
        for (index, encoded) in keys.iter().enumerate() {
            assert_eq!(get(&file, encoded), Some(index.to_string()));
        }
    }

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
        put(&file, "x\u{1f}y", "old unframed encoding");
        let joined = key([Some("x"), Some("y")]).expect("two read parts");
        assert_eq!(get(&file, &joined), None);
        put(&file, &joined, "joined");
        assert_eq!(
            get(&file, &joined).as_deref(),
            Some("joined"),
            "a joined key is kept"
        );
        assert_eq!(get(&file, "x"), None, "a key's first part is not the key");
        assert_eq!(super::file(""), None);
        assert_eq!(super::file("."), None);
        assert_eq!(super::file(".."), None);
        assert_eq!(super::file("a/b"), None);
        assert_eq!(super::file("a\\b"), None);
    }

    #[test]
    #[cfg(unix)]
    fn native_paths_remain_distinct_for_the_same_file_metadata() {
        use std::os::unix::ffi::OsStringExt as _;

        let dir = crate::testdir::dir("atpkg-probe-cache-native-id-");
        let file = dir.join("compiler");
        std::fs::write(&file, "compiler fixture").unwrap();
        let metadata = file.metadata().unwrap();
        let first = dir.join(std::ffi::OsString::from_vec(b"compiler-\xff".to_vec()));
        let second = dir.join(std::ffi::OsString::from_vec(b"compiler-\xfe".to_vec()));
        assert_eq!(first.display().to_string(), second.display().to_string());
        let first_id = metadata_identity(&first, &metadata).unwrap();
        assert_eq!(metadata_identity(&first, &metadata).unwrap(), first_id);
        assert_ne!(first_id, metadata_identity(&second, &metadata).unwrap());
        let alias = dir.join("alias");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        assert_eq!(identity(&alias).unwrap(), identity(&file).unwrap());
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

    #[cfg(unix)]
    /// NEGATIVE CONTROL: `probe_cache` must refuse symlinked directories, symlinked cache files,
    /// and group/world-writable cache files so a cached compiler or doctor verdict cannot be
    /// poisoned or redirected.
    #[test]
    fn get_and_put_refuse_symlinked_dirs_symlinked_files_and_writable_modes() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = crate::testdir::dir("atpkg-probe-cache-admit-");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("readings");
        std::fs::write(&victim, format!("k{RECORD}poison\n")).unwrap();

        let link_dir = root.join("doctor-link");
        std::os::unix::fs::symlink(&outside, &link_dir).unwrap();
        let via_link_dir = link_dir.join("readings");
        assert_eq!(get(&via_link_dir, "k"), None);
        put(&via_link_dir, "k", "safe");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            format!("k{RECORD}poison\n")
        );

        let clean_dir = root.join("doctor");
        let file = clean_dir.join("readings");
        put(&file, "k", "clean");
        assert_eq!(get(&file, "k").as_deref(), Some("clean"));

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            get(&file, "k"),
            None,
            "world-writable cache file must not be trusted"
        );
        put(&file, "k2", "fresh");
        assert_eq!(get(&file, "k"), None, "untrusted old entries are discarded");
        assert_eq!(get(&file, "k2").as_deref(), Some("fresh"));

        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&victim, &file).unwrap();
        assert_eq!(get(&file, "k"), None);
        put(&file, "k", "overwrite");
        assert!(
            std::fs::symlink_metadata(&file)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            format!("k{RECORD}poison\n")
        );
    }
}
