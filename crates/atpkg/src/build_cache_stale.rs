// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHAT NO BUILD READS AGAIN inside a build directory that stays.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A stale incremental crate folder, or a unit an old compiler built, goes once nothing wrote it
/// for this long: long enough that a worktree left over a weekend keeps its incremental state,
/// short enough that a week of merges does not pile up beside it.
pub const RECLAIM_STALE: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// The leading bytes of a fingerprint's JSON that name its compiler: cargo writes
/// `{"rustc":<u64>,` first.
const FINGERPRINT_HEAD: usize = 64;

/// The most of `.rustc_info.json` read: a few dozen kilobytes in practice. A longer one is
/// not read, and no compiler is called current.
const RUSTC_INFO_MAX: usize = 1 << 20;

/// What a path is, read without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Link,
    Other,
}

/// A path's kind and its own modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub modified: SystemTime,
    /// Device identity where available; unknown never admits an outside compiler read.
    pub device: Option<u64>,
}

/// What [`survey`] and [`unchanged`] read of the filesystem: nothing follows a link.
pub trait Fs {
    /// `path`'s own kind and modification time.
    fn meta(&self, path: &Path) -> Option<Meta>;
    /// The entries of the real directory `dir`, each with what its own metadata says (`None` where
    /// it could not be read); `None` where `dir` is no readable real directory.
    fn list(&self, dir: &Path) -> Option<Vec<(OsString, Option<Meta>)>>;
    /// Up to `max` leading bytes of the regular file `file`.
    fn head(&self, file: &Path, max: usize) -> Option<Vec<u8>>;
    /// Where the link `link` points, as written.
    fn read_link(&self, link: &Path) -> Option<PathBuf>;
}

/// A path and the newest write seen in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub path: PathBuf,
    pub newest: Option<SystemTime>,
}

/// One cargo unit of a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub fingerprint: Stamp,
    /// The `rustc` every JSON in the fingerprint folder names.
    pub rustc: Option<u64>,
    pub artifacts: Vec<Stamp>,
}

/// One cargo profile (`debug`, `release`, `<triple>/debug`): the folder holding a cargo lock.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    /// Its `incremental/` crate folders.
    pub crates: Vec<Stamp>,
    pub units: Vec<Unit>,
}

/// The store build cargo last built with in a build directory (`.rustc_info.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastBuilt {
    /// It is the build its store's `current` link names.
    pub current: bool,
    /// When that link was made (its own modification time).
    pub since: SystemTime,
}

/// What [`survey`] read of one build directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Survey {
    /// `None` where cargo last built here with a compiler no store holds, or that cannot be read.
    pub last_built: Option<LastBuilt>,
    pub profiles: Vec<Profile>,
}

/// What one doomed thing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// A stale incremental crate folder.
    Incremental,
    /// A unit an old compiler built: its fingerprint folder and its entries.
    Unit,
}

/// Something [`decide`] says goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doomed {
    pub part: Part,
    pub witness: Stamp,
    pub paths: Vec<PathBuf>,
}

/// THE RULE, pure: in each profile, every incremental crate folder nothing wrote for
/// [`RECLAIM_STALE`]; and, when cargo last built here with a store's build
/// ([`Survey::last_built`]), every unit whose fingerprint names a compiler no unit of the
/// profile written since the store's `current` link was made names, nothing having written
/// its fingerprint or any of its entries for [`RECLAIM_STALE`]. Where that build is the
/// current one, a profile with no unit written since keeps every unit: nothing there says
/// which compiler is current.
#[must_use]
pub fn decide(survey: &Survey, now: SystemTime) -> Vec<Doomed> {
    let stale = |t: Option<SystemTime>| {
        t.is_some_and(|t| now.duration_since(t).unwrap_or(Duration::ZERO) >= RECLAIM_STALE)
    };
    let mut doomed = Vec::new();
    for profile in &survey.profiles {
        for folder in profile.crates.iter().filter(|c| stale(c.newest)) {
            doomed.push(Doomed {
                part: Part::Incremental,
                witness: folder.clone(),
                paths: vec![folder.path.clone()],
            });
        }
        let Some(last) = survey.last_built else {
            continue;
        };
        let current: BTreeSet<u64> = profile
            .units
            .iter()
            .filter(|u| u.fingerprint.newest.is_some_and(|t| t >= last.since))
            .filter_map(|u| u.rustc)
            .collect();
        if last.current && current.is_empty() {
            continue;
        }
        for unit in &profile.units {
            let old = unit.rustc.is_some_and(|r| !current.contains(&r));
            if old
                && stale(unit.fingerprint.newest)
                && unit.artifacts.iter().all(|a| stale(a.newest))
            {
                let mut paths = vec![unit.fingerprint.path.clone()];
                paths.extend(unit.artifacts.iter().map(|a| a.path.clone()));
                doomed.push(Doomed {
                    part: Part::Unit,
                    witness: unit.fingerprint.clone(),
                    paths,
                });
            }
        }
    }
    doomed
}

/// Reads the build directory `leaf`.
pub fn survey(fs: &dyn Fs, leaf: &Path, profiles: &[PathBuf], store: Option<&Path>) -> Survey {
    Survey {
        last_built: last_built(fs, leaf, store),
        profiles: profiles.iter().map(|p| profile(fs, p)).collect(),
    }
}

/// Whether `doomed` still stands as [`survey`] saw it: its witness written no later.
pub fn unchanged(fs: &dyn Fs, doomed: &Doomed) -> bool {
    doomed.witness.newest.is_some() && stamp(fs, &doomed.witness.path) == doomed.witness
}

/// The newest write in `path`.
pub fn stamp(fs: &dyn Fs, path: &Path) -> Stamp {
    let newest = fs.meta(path).and_then(|meta| match meta.kind {
        Kind::Dir => fs
            .list(path)?
            .into_iter()
            .try_fold(meta.modified, |n, (_, m)| m.map(|m| n.max(m.modified))),
        _ => Some(meta.modified),
    });
    Stamp {
        path: path.to_path_buf(),
        newest,
    }
}

/// One profile's incremental crate folders and units.
fn profile(fs: &dyn Fs, dir: &Path) -> Profile {
    let crates = subdirs(fs, &dir.join("incremental"))
        .map(|p| stamp(fs, &p))
        .collect();
    let mut named: Vec<(String, PathBuf)> = Vec::new();
    for sub in ["deps", "build", "examples"] {
        let sub = dir.join(sub);
        for (name, meta) in fs.list(&sub).unwrap_or_default() {
            let Some(hash) = name.to_str().and_then(unit_hash) else {
                continue;
            };
            if meta.is_some_and(|m| matches!(m.kind, Kind::Dir | Kind::File | Kind::Link)) {
                named.push((hash.to_owned(), sub.join(&name)));
            }
        }
    }
    named.sort();
    let units = subdirs(fs, &dir.join(".fingerprint"))
        .filter_map(|fingerprint| {
            let hash = fingerprint
                .file_name()?
                .to_str()
                .and_then(unit_hash)?
                .to_owned();
            let from = named.partition_point(|(h, _)| *h < hash);
            let artifacts = named[from..]
                .iter()
                .take_while(|(h, _)| *h == hash)
                .map(|(_, p)| stamp(fs, p))
                .collect();
            Some(Unit {
                rustc: compiler(fs, &fingerprint),
                fingerprint: stamp(fs, &fingerprint),
                artifacts,
            })
        })
        .collect();
    Profile { crates, units }
}

/// The real directories directly in `dir`.
fn subdirs<'a>(fs: &dyn Fs, dir: &'a Path) -> impl Iterator<Item = PathBuf> + 'a {
    fs.list(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, m)| m.is_some_and(|m| m.kind == Kind::Dir))
        .map(move |(name, _)| dir.join(name))
}

/// The unit hash a cargo output's name carries.
#[must_use]
pub fn unit_hash(name: &str) -> Option<&str> {
    let bytes = name.as_bytes();
    bytes.iter().enumerate().find_map(|(i, &b)| {
        let hash = bytes.get(i + 1..i + 17)?;
        let after = bytes.get(i + 17);
        (b == b'-'
            && hash.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
            && after.is_none_or(|&c| c == b'.'))
        .then(|| &name[i + 1..i + 17])
    })
}

/// The compiler every JSON of the fingerprint folder `dir` names (its leading `{"rustc":N`).
fn compiler(fs: &dyn Fs, dir: &Path) -> Option<u64> {
    let mut seen = None;
    for (name, meta) in fs.list(dir)? {
        if !name.to_str().is_some_and(|n| n.ends_with(".json")) {
            continue;
        }
        if meta?.kind != Kind::File {
            return None;
        }
        let head = fs.head(&dir.join(&name), FINGERPRINT_HEAD)?;
        let digits = head.strip_prefix(b"{\"rustc\":")?;
        let end = digits.iter().position(|c| !c.is_ascii_digit())?;
        let rustc: u64 = std::str::from_utf8(&digits[..end]).ok()?.parse().ok()?;
        if seen.is_some_and(|s| s != rustc) {
            return None;
        }
        seen = Some(rustc);
    }
    seen
}

/// The store build cargo last built with in `leaf`: `.rustc_info.json` names one sysroot,
/// `…/store/<program>/<build>`, and `…/store/<program>/current` is a link (to it, or to
/// another build).
fn last_built(fs: &dyn Fs, leaf: &Path, store: Option<&Path>) -> Option<LastBuilt> {
    let info = fs.head(&leaf.join(".rustc_info.json"), RUSTC_INFO_MAX + 1)?;
    if info.len() > RUSTC_INFO_MAX {
        return None;
    }
    let sysroot = sysroot_named(&info)?;
    let store = store?;
    let mut relative = sysroot.strip_prefix(store).ok()?.components();
    let std::path::Component::Normal(name) = relative.next()? else {
        return None;
    };
    let std::path::Component::Normal(raw_sysroot_build) = relative.next()? else {
        return None;
    };
    let sysroot_build = raw_sysroot_build
        .to_str()?
        .strip_suffix(aterm_toolchain::build_cache::NOINDEX)
        .unwrap_or(raw_sysroot_build.to_str()?);
    if relative.next().is_some() || sysroot_build.parse::<u64>().is_err() {
        return None;
    }
    let program = store.join(name);
    let root = fs.meta(store)?;
    let meta = fs.meta(&program)?;
    if root.kind != Kind::Dir
        || meta.kind != Kind::Dir
        || root.device.is_none()
        || meta.device != root.device
    {
        return None;
    }
    let link = program.join("current");
    let meta = fs.meta(&link)?;
    if meta.kind != Kind::Link {
        return None;
    }
    let target = program.join(fs.read_link(&link)?);
    let mut relative = target.strip_prefix(&program).ok()?.components();
    let std::path::Component::Normal(raw_target_build) = relative.next()? else {
        return None;
    };
    let target_build = raw_target_build
        .to_str()?
        .strip_suffix(aterm_toolchain::build_cache::NOINDEX)
        .unwrap_or(raw_target_build.to_str()?);
    if relative.next().is_some() || target_build.parse::<u64>().is_err() {
        return None;
    }
    Some(LastBuilt {
        current: target_build == sysroot_build,
        since: meta.modified,
    })
}

/// The one absolute path among the outputs `.rustc_info.json` keeps: the `--print sysroot`
/// line (the other lines are file names, cfgs and `-vV`'s). `None` for none, or several.
fn sysroot_named(info: &[u8]) -> Option<PathBuf> {
    let value: aterm_json::Value = aterm_json::from_slice(info).ok()?;
    let mut found = BTreeSet::new();
    for output in value.get("outputs")?.as_object()?.values() {
        let stdout = output.get("stdout").and_then(aterm_json::Value::as_str);
        for line in stdout.unwrap_or_default().lines() {
            if Path::new(line).is_absolute() {
                found.insert(PathBuf::from(line));
            }
        }
    }
    let mut found = found.into_iter();
    let sysroot = found.next()?;
    found.next().is_none().then_some(sysroot)
}

/// [`Fs`] on the real filesystem.
pub struct Disk;

impl Fs for Disk {
    fn meta(&self, path: &Path) -> Option<Meta> {
        std::fs::symlink_metadata(path)
            .ok()
            .and_then(|m| meta_of(&m))
    }

    fn list(&self, dir: &Path) -> Option<Vec<(OsString, Option<Meta>)>> {
        if !std::fs::symlink_metadata(dir).ok()?.is_dir() {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok().and_then(|m| meta_of(&m));
            out.push((entry.file_name(), meta));
        }
        Some(out)
    }

    fn head(&self, file: &Path, max: usize) -> Option<Vec<u8>> {
        crate::metadata_io::read_head(file, max).ok()
    }

    fn read_link(&self, link: &Path) -> Option<PathBuf> {
        std::fs::read_link(link).ok()
    }
}

fn meta_of(m: &std::fs::Metadata) -> Option<Meta> {
    let t = m.file_type();
    let kind = if t.is_symlink() {
        Kind::Link
    } else if t.is_dir() {
        Kind::Dir
    } else if t.is_file() {
        Kind::File
    } else {
        Kind::Other
    };
    #[cfg(unix)]
    let device = {
        use std::os::unix::fs::MetadataExt as _;
        Some(m.dev())
    };
    #[cfg(not(unix))]
    let device = None;
    Some(Meta {
        kind,
        modified: m.modified().ok()?,
        device,
    })
}

#[cfg(test)]
#[path = "build_cache_stale_tests.rs"]
mod tests;
