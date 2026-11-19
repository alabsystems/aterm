// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A UNIT SOURCE FOR TARGO (DC-24b): atpkg's share of taking a shared dependency unit from a
//! fleet member instead of compiling it (docs/DESIGN-disk-custody-2026-11-12.md §5).
//!
//! THE FRONT ([`environment`]): Trust's targo (DC-24a) asks a program named in
//! [`UNIT_SOURCE_ENV`] for a shared unit whose check missed under its lock, starting it once per
//! build at its first ask. The build's front names `aterm pkg unit-source` there, through an
//! argv0 link of the running `aterm` ([`LINK`], made in this Mac's state, one folder per
//! executable, since targo starts the program with no arguments), and only where this Mac's
//! fleet access file reads Full Access (`aterm_types::fleet_access`): a member that may start a
//! plain shell here may also hand this Mac's builds a unit, and a Mac below Full Access starts no
//! source and sends no ask. Only the `aterm` image serves the link's name, so a front another
//! image runs (a dev `atpkg`, a lab tool) names none. A release takes nothing: `ship release`
//! runs its compiler's targo by path, never through this front, and targo keeps the variable
//! from every process a build starts (a `run` alias such as `ship` included). A targo that
//! predates DC-24a never reads the variable, so there it changes nothing.
//!
//! THE STORE ([`held`]): a member's listener answers an ask by key only: the unit of targo's
//! store (each of [`super::shared_units::roots`]) whose fingerprint record's SHA-256 is that key,
//! read while its build lock (`<unit>/.lock`) is held shared, as a build reading it holds it. A
//! unit a build is writing (its lock held exclusive) is answered as not held. The keys are kept
//! in an index walked again at most every [`INDEX_FRESH`] on a miss, so an ask costs a lookup and
//! not a walk of the store. Nothing lists what the store holds.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// The variable that names targo's unit source (Trust's `unit_source::UNIT_SOURCE_ENV`). It is
/// targo's own: no process the build starts sees it.
pub const UNIT_SOURCE_ENV: &str = "TRUST_TARGO_UNIT_SOURCE";

/// The argv0 name `aterm` runs `aterm pkg unit-source` under.
pub const LINK: &str = "aterm-unit-source";

/// The file name of the one image that serves [`LINK`]'s name: aterm's own front door.
const SERVING_IMAGE: &str = "aterm";

/// The folder of this Mac's state holding a folder per executable, each with its [`LINK`].
#[cfg(unix)]
const LINK_DIR: &str = "unit-source";

/// The largest fleet access file read: more than its three lines ever hold.
const ACCESS_MAX: usize = 256;

/// The environment entry the front gives a targo it starts, if any: none where the call names
/// its own source (`inherited`), else [`environment`] for this Mac's `state` and the front's own
/// executable.
#[must_use]
pub fn for_front(
    inherited: Option<&std::ffi::OsStr>,
    state: Option<&Path>,
    exe: Option<&Path>,
) -> Option<(OsString, OsString)> {
    if inherited.is_some() {
        return None;
    }
    environment(state?, exe?)
}

/// The environment entry naming the source: [`UNIT_SOURCE_ENV`] naming the [`LINK`] to `exe`
/// (its real path, which must be an `aterm` image) under `state`, where this Mac's fleet access
/// (read from `state`, failing closed) is Full Access; `None` at any other level, for another
/// image, and where the link cannot be made.
#[must_use]
pub fn environment(state: &Path, exe: &Path) -> Option<(OsString, OsString)> {
    if level(state) != aterm_types::fleet_access::FleetAccess::Full {
        return None;
    }
    let exe = std::fs::canonicalize(exe).ok()?;
    if exe.file_name()? != SERVING_IMAGE {
        return None;
    }
    let link = link_at(state, &exe).ok()?;
    Some((UNIT_SOURCE_ENV.into(), link.into_os_string()))
}

/// This Mac's fleet access level as stored under `state`, Read Only unless its private file
/// names another.
#[must_use]
pub fn level(state: &Path) -> aterm_types::fleet_access::FleetAccess {
    #[cfg(unix)]
    let bytes =
        aterm_uds::private_file::read_private(state, aterm_types::fleet_access::FILE, ACCESS_MAX)
            .ok();
    #[cfg(not(unix))]
    let bytes: Option<Vec<u8>> = {
        let _ = (state, ACCESS_MAX);
        None
    };
    aterm_types::fleet_access::FleetAccess::of_file(bytes.as_deref())
}

/// The [`LINK`] to `exe` in its own folder of `state`'s [`LINK_DIR`] (named by the SHA-256 of
/// `exe`'s path, so two `aterm`s each keep their own), made where it is not there or names
/// another.
///
/// # Errors
/// The folders or the link could not be made.
#[cfg(unix)]
pub fn link_at(state: &Path, exe: &Path) -> std::io::Result<PathBuf> {
    let dir = state.join(LINK_DIR);
    crate::platform::ensure_private_dir(&dir)?;
    let digest = sha256_hex(exe.as_os_str().as_encoded_bytes());
    let dir = dir.join(&digest[..16]);
    crate::platform::ensure_private_dir(&dir)?;
    let link = dir.join(LINK);
    if std::fs::read_link(&link).is_ok_and(|to| to == exe) {
        return Ok(link);
    }
    let fresh = dir.join(format!(".{LINK}.{}", std::process::id()));
    let _ = std::fs::remove_file(&fresh);
    std::os::unix::fs::symlink(exe, &fresh)?;
    std::fs::rename(&fresh, &link).inspect_err(|_| {
        let _ = std::fs::remove_file(&fresh);
    })?;
    Ok(link)
}

/// Off Unix no targo shares, so no link is made.
///
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn link_at(_: &Path, _: &Path) -> std::io::Result<PathBuf> {
    Err(std::io::Error::other("no unit source off Unix"))
}

/// A unit [`held`] found, its build lock held shared until this drops.
pub struct Held {
    /// The unit's directory relative to its store: `build/<name>/<hash>`, or
    /// `build/git/<commit>/<name>/<hash>`.
    pub dir: String,
    /// Each plain file of its `fingerprint/` and `out/` folders: its path relative to the unit
    /// and where it is.
    pub files: Vec<(String, PathBuf)>,
    /// The unit's build lock, held shared.
    pub lock: UnitLock,
}

/// A unit's build lock, held shared while it lives.
pub struct UnitLock(#[expect(dead_code, reason = "held for its Drop")] crate::lock::Flock);

/// How long the index answers a miss before the stores are walked again: a unit a member built
/// this long ago at most is not yet served, and is compiled by the Mac that asked.
pub const INDEX_FRESH: Duration = Duration::from_secs(5);

/// What a record's key was when it was last read: by its write time and size.
type Seen = (SystemTime, u64, String);

/// The keys of the stores' units: where each key's record is, and what each record read as.
struct Index {
    roots: Vec<PathBuf>,
    walked: Instant,
    /// Each key: its store root, its unit and its record.
    by_key: HashMap<String, (PathBuf, PathBuf, PathBuf)>,
    /// Each record read, so a walk hashes only what changed.
    seen: HashMap<PathBuf, Seen>,
}

static INDEX: Mutex<Option<Index>> = Mutex::new(None);

/// The most records the index keeps before it starts again.
const KEYS_MAX: usize = 1 << 17;

/// The unit of the stores at `roots` whose fingerprint record's SHA-256 is `key` (a lowercase hex
/// digest), with its build lock held shared and its record read again under the lock; `None`
/// where no unit holds the key, or a build holds its lock exclusive (it is being written).
#[must_use]
pub fn held(roots: &[PathBuf], key: &str) -> Option<Held> {
    held_within(roots, key, INDEX_FRESH)
}

/// [`held`], the index walked again on a miss where it is older than `fresh`.
#[must_use]
pub fn held_within(roots: &[PathBuf], key: &str, fresh: Duration) -> Option<Held> {
    let (root, unit, record) = {
        let mut index = INDEX.lock().unwrap_or_else(PoisonError::into_inner);
        let stale = index
            .as_ref()
            .is_none_or(|index| index.roots != roots || index.seen.len() > KEYS_MAX);
        if stale {
            *index = Some(Index {
                roots: roots.to_vec(),
                walked: Instant::now(),
                by_key: HashMap::new(),
                seen: HashMap::new(),
            });
            if let Some(index) = index.as_mut() {
                walk(index);
            }
        }
        let index = index.as_mut()?;
        if !index.by_key.contains_key(key) && index.walked.elapsed() >= fresh && !stale {
            walk(index);
        }
        index.by_key.get(key)?.clone()
    };
    let lock = crate::metadata_io::open_regular(&unit.join(".lock")).ok()?;
    let lock = crate::lock::Flock::try_lock_shared(lock).ok()?;
    // Read again under the lock: a build may have written the unit since the walk.
    if std::fs::read(&record).ok().map(|bytes| sha256_hex(&bytes)) != Some(key.to_owned()) {
        return None;
    }
    let dir = unit.strip_prefix(&root).ok()?.to_str()?.to_owned();
    Some(Held {
        dir,
        files: files_of(&unit),
        lock: UnitLock(lock),
    })
}

/// Walk every store of `index` again: each unit's records, hashing only those whose write time
/// or size changed since they were last read.
fn walk(index: &mut Index) {
    let mut by_key = HashMap::new();
    let mut seen = HashMap::new();
    for root in &index.roots {
        for unit in units(&root.join("build")) {
            let Ok(entries) = std::fs::read_dir(unit.join("fingerprint")) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if !name.ends_with(".json") || name.starts_with("outputs-") {
                    continue;
                }
                let path = entry.path();
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                let (true, Ok(at), len) = (meta.is_file(), meta.modified(), meta.len()) else {
                    continue;
                };
                let known = index
                    .seen
                    .get(&path)
                    .filter(|(seen_at, seen_len, _)| *seen_at == at && *seen_len == len)
                    .map(|(_, _, key)| key.clone());
                let key = match known {
                    Some(key) => key,
                    None => {
                        let Ok(bytes) = std::fs::read(&path) else {
                            continue;
                        };
                        sha256_hex(&bytes)
                    }
                };
                seen.insert(path.clone(), (at, len, key.clone()));
                by_key.insert(key, (root.clone(), unit.clone(), path));
            }
        }
    }
    index.by_key = by_key;
    index.seen = seen;
    index.walked = Instant::now();
}

/// Each unit folder under a store's `build/`: `<name>/<hash>`, and `git/<commit>/<name>/<hash>`
/// for a Git source. A folder under `build/git/` that holds a `fingerprint/` is the unit of a
/// registry crate named `git`, not a commit.
fn units(build: &Path) -> Vec<PathBuf> {
    let dirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    let is_unit = |dir: &Path| {
        std::fs::symlink_metadata(dir.join("fingerprint")).is_ok_and(|meta| meta.is_dir())
    };
    let mut units = Vec::new();
    for name in dirs(build) {
        if name.file_name().is_some_and(|name| name == "git") {
            for below in dirs(&name) {
                if is_unit(&below) {
                    units.push(below);
                } else {
                    for named in dirs(&below) {
                        units.extend(dirs(&named));
                    }
                }
            }
        } else {
            units.extend(dirs(&name));
        }
    }
    units
}

/// Each plain file directly in `unit`'s `fingerprint/` and `out/`, by name.
fn files_of(unit: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for folder in ["fingerprint", "out"] {
        let Ok(entries) = std::fs::read_dir(unit.join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                files.push((format!("{folder}/{name}"), entry.path()));
            }
        }
    }
    files.sort();
    files
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    aterm_digest::Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[cfg(test)]
#[path = "build_cache_unit_source_tests.rs"]
mod tests;
