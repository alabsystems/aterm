// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code `versions/` directory survey ([`super::Class::ClaudeStaleVersions`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::VersionDir;

/// The digest cache's file, under the harness state directory.
pub const HASH_FILE: &str = "claude-version-hashes.json";

/// The most digests the cache keeps.
pub const MAX_HASHES: usize = 32;

/// The most bytes of cache file read back.
const MAX_HASH_FILE_BYTES: u64 = 64 * 1024;

/// What one survey read of the versions folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Versions {
    /// Its entries, each strictly inside it.
    pub entries: Vec<VersionDir>,
    /// The live entry, when the launcher named one.
    pub live: Option<PathBuf>,
    /// The launcher the live entry was named from BY CONTENT (Windows' `claude.exe` copy).
    pub copy_of: Option<PathBuf>,
    /// Why no live entry was named.
    pub unnamed: Option<String>,
}

/// Read the versions folder `roots` names ([`list`], [`live_of`], [`mark_kept`]).
#[must_use]
pub fn read(roots: &super::Roots, processes: super::discover::Processes<'_>) -> Versions {
    let Some(root) = roots
        .versions_dir
        .as_ref()
        .and_then(|d| std::fs::canonicalize(d).ok())
    else {
        return Versions::default();
    };
    let mut out = Versions {
        entries: list(&root),
        ..Versions::default()
    };
    let mut hashes = Hashes::load(roots.version_hashes.as_deref());
    match roots.live_link.as_deref() {
        Some(launcher) => match live_of(launcher, &root, &out.entries, &mut hashes) {
            Ok((live, copy_of)) => {
                out.live = Some(live);
                out.copy_of = copy_of;
            }
            Err(why) => out.unnamed = Some(why),
        },
        None => out.unnamed = Some("no live launcher is named".to_owned()),
    }
    hashes.save();
    if let Some(live) = &out.live {
        mark_kept(
            &mut out.entries,
            live,
            roots.version_locks.as_deref(),
            processes,
            roots.mounts.as_ref(),
        );
    }
    out
}

/// Why each of `entries` but the `live` one is KEPT ([`kept`]).
pub fn mark_kept(
    entries: &mut [VersionDir],
    live: &Path,
    locks: Option<&Path>,
    processes: super::discover::Processes<'_>,
    table: Option<&aterm_sysprobe::volumes::Table>,
) {
    let mut open: Vec<usize> = Vec::new();
    for (i, v) in entries.iter_mut().enumerate() {
        if v.path == live {
            continue;
        }
        v.kept = kept_by_name(v, live, locks);
        if v.kept.is_none() {
            open.push(i);
        }
    }
    if open.is_empty() {
        return;
    }
    let dirs: Vec<PathBuf> = open.iter().map(|&i| entries[i].path.clone()).collect();
    let scan = processes(&dirs, &[std::process::id()]);
    for i in open {
        let v = &mut entries[i];
        let named = |u: &aterm_uds::holders::PathUse| {
            super::idle::under(&u.path, &v.path, table) != Some(false)
        };
        let unnamed_here = scan
            .as_ref()
            .is_ok_and(|s| !s.users.is_empty() && !s.users.iter().any(named));
        v.kept = if unnamed_here {
            super::discover::unused_under(std::slice::from_ref(&v.path), processes).err()
        } else {
            super::discover::scan_says_unused(&scan, named).err()
        };
    }
}

/// The entries of the canonical versions folder `root` ([`super::dir_bytes`]).
#[must_use]
pub fn list(root: &Path) -> Vec<VersionDir> {
    let mut out: Vec<VersionDir> = Vec::new();
    for child in super::read_children(root) {
        if out.len() >= super::MAX_ROWS_PER_CLASS {
            break;
        }
        let Ok(path) = std::fs::canonicalize(&child) else {
            continue;
        };
        if path.parent() != Some(root) || out.iter().any(|v| v.path == path) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let (bytes, bytes_partial) = if meta.is_file() {
            (meta.len(), false)
        } else if meta.is_dir() {
            super::dir_bytes(&path)
        } else {
            continue;
        };
        out.push(VersionDir {
            path,
            bytes,
            bytes_partial,
            kept: None,
        });
    }
    out
}

/// The live entry of the canonical versions folder `root`, named by `launcher`, and — when it was
/// named by content — the launcher it is the original of.
/// # Errors
/// Why it names no entry.
pub fn live_of(
    launcher: &Path,
    root: &Path,
    entries: &[VersionDir],
    hashes: &mut Hashes,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    if cfg!(windows) {
        match std::fs::symlink_metadata(launcher) {
            Ok(m) if m.is_file() => {
                return copy_of(launcher, entries, hashes)
                    .map(|v| (v, Some(launcher.to_path_buf())));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("there is no live launcher {}", launcher.display()));
            }
            _ => {}
        }
    }
    let resolved = std::fs::canonicalize(launcher)
        .map_err(|_| "the live symlink did not resolve".to_owned())?;
    super::version_dir_of(&resolved, Some(root))
        .map(|v| (v, None))
        .ok_or_else(|| {
            format!(
                "the live launcher resolves to {}, which is not inside {}",
                resolved.display(),
                root.display()
            )
        })
}

/// The ONE entry of `entries` that `launcher` is a byte-for-byte copy of: every regular-file entry
/// of the launcher's length is digested ([`Hashes::digest`]).
/// # Errors
/// The launcher is a copy of none of them, or of more than one (which is the original is then
/// unknown), or a file could not be read.
pub fn copy_of(
    launcher: &Path,
    entries: &[VersionDir],
    hashes: &mut Hashes,
) -> Result<PathBuf, String> {
    let (len, digest) = hashes.digest(launcher)?;
    let mut same: Vec<&Path> = Vec::new();
    for v in entries {
        let meta = std::fs::symlink_metadata(&v.path)
            .map_err(|e| format!("{} could not be read: {e}", v.path.display()))?;
        if !meta.is_file() || meta.len() != len {
            continue;
        }
        if hashes.digest(&v.path)?.1 == digest {
            same.push(&v.path);
        }
    }
    match same.as_slice() {
        [one] => Ok(one.to_path_buf()),
        [] => Err(format!(
            "{} is a copy of no version here, so which one is live is unknown",
            launcher.display()
        )),
        more => Err(format!(
            "{} is a copy of {} versions here, so which one is live is unknown",
            launcher.display(),
            more.len()
        )),
    }
}

/// Why the entry `v` (not the live one, `live`) is KEPT, or `None` when nothing holds it.
#[must_use]
pub fn kept(
    v: &VersionDir,
    live: &Path,
    locks: Option<&Path>,
    processes: super::discover::Processes<'_>,
) -> Option<String> {
    kept_by_name(v, live, locks)
        .or_else(|| super::discover::unused_under(std::slice::from_ref(&v.path), processes).err())
}

/// [`kept`] but for the process scan.
fn kept_by_name(v: &VersionDir, live: &Path, locks: Option<&Path>) -> Option<String> {
    let Some(name) = v.path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Some(format!("{} has no name", v.path.display()));
    };
    if let Some(locks) = locks {
        let lock = locks.join(format!("{name}.lock"));
        match std::fs::symlink_metadata(&lock) {
            Ok(_) => {
                return Some(format!("Claude Code's lock {} holds it", lock.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Some(format!(
                    "its lock {} could not be read: {e}",
                    lock.display()
                ));
            }
        }
    }
    if cfg!(windows) {
        return newer_than(&name, live);
    }
    None
}

/// Why the entry named `name` is not older than the live entry `live`.
#[must_use]
pub fn newer_than(name: &str, live: &Path) -> Option<String> {
    let live_name = live
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match (version_number(name), version_number(&live_name)) {
        (Some(v), Some(l)) if v > l => Some(format!(
            "it is newer than the live {live_name}: the update a running claude.exe keeps \
             from being swapped in"
        )),
        (Some(_), Some(_)) => None,
        _ => Some(format!(
            "{name} or the live {live_name} is no version number, so which is newer is unknown"
        )),
    }
}

/// A dotted version number's parts (`2.1.286` → `[2, 1, 286]`), compared part by part.
#[must_use]
pub fn version_number(name: &str) -> Option<Vec<u64>> {
    name.split('.')
        .map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        })
        .collect()
}

/// The digest cache's schema ([`Stamp::id`]).
const HASH_SCHEMA: u64 = 2;

/// What a file's own handle says of it: a digest stands while none of it moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    written_ns: Option<u64>,
    created_ns: Option<u64>,
    /// The file's volume and number ([`aterm_sysprobe::volumes::FileId`],
    /// [`aterm_sysprobe::volumes::file_id_of_handle`]).
    id: Option<(u64, u128)>,
}

impl Stamp {
    fn of(file: &std::fs::File, meta: &std::fs::Metadata) -> Stamp {
        let ns = |t: std::io::Result<std::time::SystemTime>| {
            t.ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| u64::try_from(d.as_nanos()).ok())
        };
        Stamp {
            len: meta.len(),
            written_ns: ns(meta.modified()),
            created_ns: ns(meta.created()),
            id: file_id(file, meta).map(|id| (id.dev, id.ino)),
        }
    }
}

/// The identity of the file open as `file` ([`Stamp::id`]).
fn file_id(
    file: &std::fs::File,
    meta: &std::fs::Metadata,
) -> Option<aterm_sysprobe::volumes::FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = file;
        Some(aterm_sysprobe::volumes::FileId {
            dev: meta.dev(),
            ino: u128::from(meta.ino()),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle as _;
        let _ = meta;
        aterm_sysprobe::volumes::file_id_of_handle(file.as_handle())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, meta);
        None
    }
}

/// THE DIGEST CACHE ([`HASH_FILE`]) ([`Stamp`]).
#[derive(Debug, Default)]
pub struct Hashes {
    /// Where it is kept.
    file: Option<PathBuf>,
    /// Path → (stamp, lowercase hex SHA-256).
    known: BTreeMap<String, (Stamp, String)>,
    /// The paths this survey asked for.
    used: BTreeSet<String>,
    /// A digest was read anew.
    changed: bool,
}

impl Hashes {
    /// The cache in `file` (none when it is not there or does not decode).
    #[must_use]
    pub fn load(file: Option<&Path>) -> Hashes {
        let mut out = Hashes {
            file: file.map(Path::to_path_buf),
            ..Hashes::default()
        };
        let Some(text) = file.and_then(|f| {
            let mut text = String::new();
            std::fs::File::open(f)
                .ok()?
                .take(MAX_HASH_FILE_BYTES)
                .read_to_string(&mut text)
                .ok()?;
            Some(text)
        }) else {
            return out;
        };
        let Ok(v) = aterm_json::from_str::<Value>(&text) else {
            return out;
        };
        if v.get("schema").and_then(Value::as_u64) != Some(HASH_SCHEMA) {
            return out;
        }
        let ns = |e: &Value, k: &str| e.get(k).and_then(Value::as_u64);
        let ino = |e: &Value| {
            e.get("ino")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u128>().ok())
        };
        for (path, e) in v
            .get("files")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .take(MAX_HASHES)
        {
            let (Some(len), Some(sha)) = (
                e.get("len").and_then(Value::as_u64),
                e.get("sha256").and_then(Value::as_str),
            ) else {
                continue;
            };
            let stamp = Stamp {
                len,
                written_ns: ns(e, "written_ns"),
                created_ns: ns(e, "created_ns"),
                id: ns(e, "dev").zip(ino(e)),
            };
            out.known.insert(path.clone(), (stamp, sha.to_owned()));
        }
        out
    }

    /// The length and lowercase hex SHA-256 of the regular file at `path` ([`Stamp`]).
    /// # Errors
    /// It could not be opened or read, is not a regular file, or moved while it was read.
    pub fn digest(&mut self, path: &Path) -> Result<(u64, String), String> {
        let err = |e: std::io::Error| format!("{} could not be read: {e}", path.display());
        let mut file = std::fs::File::open(path).map_err(err)?;
        let before = file.metadata().map_err(err)?;
        if !before.is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        let stamp = Stamp::of(&file, &before);
        let key = path.to_string_lossy().into_owned();
        self.used.insert(key.clone());
        if let Some((kept, sha)) = self.known.get(&key)
            && *kept == stamp
        {
            return Ok((stamp.len, sha.clone()));
        }
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; 1 << 20];
        let mut read: u64 = 0;
        loop {
            let n = file.read(&mut buf).map_err(err)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            read = read.saturating_add(n as u64);
            super::beat::tick(1);
        }
        let after = file.metadata().map_err(err)?;
        if Stamp::of(&file, &after) != stamp || read != stamp.len {
            return Err(format!("{} changed while it was read", path.display()));
        }
        let sha = aterm_codec::hex::encode_lower(hash.finish().as_ref());
        self.known.insert(key, (stamp, sha.clone()));
        self.changed = true;
        Ok((stamp.len, sha))
    }

    /// The cache as its file holds it.
    fn encode(&self) -> String {
        let mut files = Map::new();
        for (path, (stamp, sha)) in self
            .known
            .iter()
            .filter(|(p, _)| self.used.contains(*p))
            .take(MAX_HASHES)
        {
            let mut e = Map::new();
            e.insert("len".to_owned(), Value::from(stamp.len));
            let opt = |n: Option<u64>| n.map_or(Value::Null, Value::from);
            e.insert("written_ns".to_owned(), opt(stamp.written_ns));
            e.insert("created_ns".to_owned(), opt(stamp.created_ns));
            e.insert("dev".to_owned(), opt(stamp.id.map(|(dev, _)| dev)));
            e.insert(
                "ino".to_owned(),
                stamp
                    .id
                    .map_or(Value::Null, |(_, ino)| Value::from(ino.to_string())),
            );
            e.insert("sha256".to_owned(), Value::from(sha.clone()));
            files.insert(path.clone(), Value::Object(e));
        }
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(HASH_SCHEMA));
        o.insert("files".to_owned(), Value::Object(files));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Write the cache back when a digest was read anew or one it held went unused.
    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let dropped = self.known.keys().any(|p| !self.used.contains(p));
        if !(self.changed || dropped) {
            return;
        }
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = super::super::upgrade_models::write_atomic_ordered(file, &self.encode());
    }
}

#[cfg(test)]
#[path = "disk_versions_tests.rs"]
mod tests;
