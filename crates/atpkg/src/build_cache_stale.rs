// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHAT NO BUILD READS AGAIN inside a build directory that stays: stale incremental folders,
//! units an old compiler built, and, in a build directory whose last passing release run named
//! the units it used ([`USED`], [`Used`]), each unit of the workspace's own packages that run
//! superseded (another build of the same package and kind it did use, [`Part::Superseded`]). A
//! unit whose folder, fingerprint or outputs cannot be listed is not read, so nothing is decided
//! of it. A profile's units are read in both of
//! cargo's layouts: `.fingerprint/<pkg>-<hash>` beside `deps/`, `build/` and `examples/` entries
//! named with the hash, and the build directory's own, each unit one folder `build/<pkg>/<hash>`
//! holding its `fingerprint/` and its outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A stale incremental crate folder, or a unit an old compiler built, goes once nothing wrote it
/// for this long: long enough that a worktree left over a weekend keeps its incremental state,
/// short enough that a week of merges does not pile up beside it.
pub const RECLAIM_STALE: Duration = Duration::from_secs(3 * 24 * 60 * 60);

/// Below the disk watch's floor, [`RECLAIM_STALE`] is this: a day of merges in one worktree
/// leaves more stale folders than the volume has room for, and the worst a wrong call costs is
/// one crate's cold compile.
pub const RECLAIM_STALE_UNDER_PRESSURE: Duration = Duration::from_secs(60 * 60);

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
    /// Its hash, cargo's name for it within the profile ([`unit_hash`]).
    pub hash: String,
    /// Its package, as the build directory names it.
    pub package: String,
    /// What it is a build of: its package, the kinds its fingerprint names (`lib-aterm_json`,
    /// `test-lib-aterm_json`, `run-build-script-build-script-build`) and whether it wrote a
    /// library's `.rlib` or `.rmeta` (a check writes only the second). Two units of one kind are
    /// two builds of the same thing (another version, other features or flags).
    pub kind: String,
    /// The folder that is the whole unit, in the layout that keeps one per unit
    /// (`build/<pkg>/<hash>`); `None` in the one that names its entries with its hash.
    pub folder: Option<PathBuf>,
}

impl Unit {
    /// What removing it removes: its folder, or its fingerprint folder and every entry named
    /// with its hash.
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        self.folder.clone().map_or_else(
            || {
                std::iter::once(self.fingerprint.path.clone())
                    .chain(self.artifacts.iter().map(|a| a.path.clone()))
                    .collect()
            },
            |folder| vec![folder],
        )
    }
}

/// One cargo profile (`debug`, `release`, `<triple>/debug`): the folder holding a cargo lock.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    /// Its `incremental/` crate folders.
    pub crates: Vec<Stamp>,
    pub units: Vec<Unit>,
    /// The hashes of its units the build directory's last passing release run used ([`USED`]);
    /// `None` where no such run named any of the build directory's ([`Survey::began`]).
    pub used: Option<BTreeSet<String>>,
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
    /// When the build directory's last passing release run began ([`USED`], [`Used::began_ms`]),
    /// no later than its record was written; `None` where it carries no record, or one unread.
    pub began: Option<SystemTime>,
    /// The packages that run built tests of ([`Used::members`]): the workspace's own.
    pub members: BTreeSet<String>,
}

/// What one doomed thing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// A stale incremental crate folder.
    Incremental,
    /// A unit an old compiler built: its fingerprint folder and its entries.
    Unit,
    /// A unit the build directory's last passing release run superseded ([`USED`]): of a package
    /// of the workspace's own ([`Used::members`]), written before that run began, which did not
    /// use it and did use another build of its package and kind ([`Unit::kind`]).
    Superseded,
}

/// THE RECORD OF WHAT A PASSING RELEASE RUN USED, at the top of its build directory
/// (`aterm-verify` writes it for a release run over the whole workspace that passed): when the
/// run began, the packages it built tests of (the workspace's own), and the units of each profile
/// its test compile reported as its outputs, fresh or built, the bins it copied into the target
/// directory and the build scripts it ran among them ([`Used`]). A unit of one of those packages
/// that it does not name, of a kind it names another unit of, written by nothing since the run
/// began nor for the stale window, is a build that run superseded: every version bump hashes each
/// unit of the workspace's own packages anew (their version, or a dependency's), and cargo never
/// takes the old ones, so a release clone's build directory grew by every candidate's units
/// ([`Part::Superseded`]). Kept are every unit of another package (a dependency's, the same build
/// in every candidate, which another compile of the run may use in a build of its own the record
/// does not see, spec-check's under its own features: cargo never rewrites a unit it found fresh,
/// so its age says nothing of its use); every unit of a kind the record names none of; and every
/// unit written since the run began, which some compile of it built (spec-check's build of the
/// models without the checkers). ACCEPTED: a unit of the workspace's own that another compile of
/// the run found fresh from an earlier run of the same version (a release cut again at one
/// version) goes once quiet for the window, though that compile reads it again: the next version
/// bump hashes it anew anyway, and a cut again past the window builds those crates once more.
pub const USED: &str = ".aterm-release-used";

/// The most of a [`USED`] record read: some thousands of units of a few dozen bytes each. A
/// longer one is not read, and supersedes nothing.
const USED_MAX: usize = 16 << 20;

/// The units a run used, by profile, the packages it built tests of, and when it began ([`USED`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Used {
    /// When the run began, milliseconds since the epoch: a unit written since is one some compile
    /// of it built. `0` (the default) supersedes nothing.
    pub began_ms: u64,
    /// The packages it built tests of, as the build directory names them: the workspace's own,
    /// which alone it supersedes units of.
    pub members: BTreeSet<String>,
    /// Each profile, by its path below the build directory (`debug`, `aarch64-apple-darwin/debug`),
    /// and the hashes of its units the run used.
    pub profiles: BTreeMap<String, BTreeSet<String>>,
}

impl Used {
    /// Add the unit the output `file` of a compile is in the build directory `leaf` (the path a
    /// compile message names it by): `<profile>/build/<pkg>/<hash>/…`, or `<profile>/deps/…`,
    /// `<profile>/build/…` and `<profile>/examples/…` named with the hash. A file outside `leaf`
    /// (an output cargo copied into the target directory) or named with no hash names none.
    pub fn add(&mut self, leaf: &Path, file: &Path) {
        let Ok(rel) = file.strip_prefix(leaf) else {
            return;
        };
        let parts: Vec<&str> = rel
            .components()
            .map(|c| match c {
                std::path::Component::Normal(n) => n.to_str(),
                _ => None,
            })
            .collect::<Option<_>>()
            .unwrap_or_default();
        let Some(at) = parts
            .iter()
            .position(|p| matches!(*p, "build" | "deps" | "examples"))
            .filter(|at| *at > 0)
        else {
            return;
        };
        let next = parts.get(at + 1).copied();
        let hash = match (parts[at], next) {
            ("build", Some(pkg)) if unit_hash(pkg).is_none() => {
                parts.get(at + 2).copied().filter(|h| is_hash(h))
            }
            (_, Some(name)) => unit_hash(name),
            _ => None,
        };
        if let Some(hash) = hash {
            self.profiles
                .entry(parts[..at].join("/"))
                .or_default()
                .insert(hash.to_owned());
        }
    }

    /// Add the unit of `package` whose output cargo copied out of the build directory `leaf` to
    /// `file` under the target directory's `profile` (`debug`, `aarch64-apple-darwin/debug`): a bin
    /// a test needs, which a compile message names by that copy alone. It is the unit
    /// `<profile>/build/<package>/<hash>` whose `out/` holds the file's twin under its crate name
    /// (each `-` an `_`): the same file (device and inode, a hard link) or its copy (the same
    /// length and modification time to the nanosecond, which cargo's copy keeps). None where no
    /// unit holds a twin; and none in the layout that names its entries with the hash, whose copies
    /// [`Self::add`] does not read either.
    pub fn add_uplifted(&mut self, leaf: &Path, profile: &str, package: &str, file: &Path) {
        let plain = |word: &str| {
            !word.is_empty() && word != "." && word != ".." && !word.contains(['/', '\\'])
        };
        if !plain(package) || !profile.split('/').all(plain) {
            return;
        }
        let Some(stem) = file
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| plain(n))
            .map(|n| n.replace('-', "_"))
        else {
            return;
        };
        let Ok(copy) = std::fs::symlink_metadata(file) else {
            return;
        };
        let dir = leaf.join(profile).join("build").join(package);
        let Ok(units) = std::fs::read_dir(&dir) else {
            return;
        };
        for unit in units.flatten() {
            let Some(hash) = unit
                .file_name()
                .to_str()
                .filter(|h| is_hash(h))
                .map(str::to_owned)
            else {
                continue;
            };
            let twin = std::fs::symlink_metadata(dir.join(&hash).join("out").join(&stem));
            if twin.is_ok_and(|twin| twins(&copy, &twin)) {
                self.profiles
                    .entry(profile.to_owned())
                    .or_default()
                    .insert(hash);
            }
        }
    }

    /// The record as [`USED`] holds it: schema 1, when the run began, its packages and each
    /// profile's hashes sorted.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut profiles = aterm_json::Map::new();
        for (name, hashes) in &self.profiles {
            profiles.insert(
                name.clone(),
                aterm_json::Value::Array(
                    hashes
                        .iter()
                        .map(|h| aterm_json::Value::from(h.as_str()))
                        .collect(),
                ),
            );
        }
        let mut o = aterm_json::Map::new();
        o.insert("schema".to_owned(), aterm_json::Value::from(1u64));
        o.insert(
            "began_ms".to_owned(),
            aterm_json::Value::from(self.began_ms),
        );
        o.insert(
            "members".to_owned(),
            aterm_json::Value::Array(
                self.members
                    .iter()
                    .map(|m| aterm_json::Value::from(m.as_str()))
                    .collect(),
            ),
        );
        o.insert("profiles".to_owned(), aterm_json::Value::Object(profiles));
        aterm_json::to_string(&aterm_json::Value::Object(o)).unwrap_or_default()
    }

    /// The record `bytes` holds; `None` for bytes that are not [`Self::encode`]'s: no time it
    /// began, no packages, or a hash that is not one ([`unit_hash`]'s sixteen hex digits) among
    /// them.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Used> {
        let v: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if v.get("schema").and_then(aterm_json::Value::as_u64) != Some(1) {
            return None;
        }
        let mut used = Used {
            began_ms: v.get("began_ms")?.as_u64()?,
            ..Used::default()
        };
        for member in v.get("members")?.as_array()? {
            used.members
                .insert(member.as_str().filter(|m| !m.is_empty())?.to_owned());
        }
        for (name, hashes) in v.get("profiles")?.as_object()? {
            let set = used.profiles.entry(name.clone()).or_default();
            for h in hashes.as_array()? {
                let h = h.as_str().filter(|h| is_hash(h))?;
                set.insert(h.to_owned());
            }
        }
        Some(used)
    }

    /// Keep the record at the top of the build directory `leaf` ([`USED`]): staged in a private
    /// folder there, then renamed over the one before.
    ///
    /// # Errors
    /// The build directory is not there, or the write failed: the record before stands.
    pub fn keep(&self, leaf: &Path) -> std::io::Result<()> {
        use std::io::Write as _;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".aterm-release-used-")
            .tempdir_in(leaf)?;
        let staged = stage.path().join("data");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(self.encode().as_bytes())?;
        drop(file);
        std::fs::rename(&staged, leaf.join(USED))
    }
}

/// Whether `name` is a unit's hash: sixteen lowercase hex digits.
fn is_hash(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `copy` is `unit`'s output under another name ([`Used::add_uplifted`]): the same file,
/// or a regular file of its length written at its nanosecond.
fn twins(copy: &std::fs::Metadata, unit: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if copy.dev() == unit.dev() && copy.ino() == unit.ino() {
            return true;
        }
    }
    copy.is_file()
        && unit.is_file()
        && copy.len() == unit.len()
        && copy
            .modified()
            .ok()
            .is_some_and(|t| unit.modified().ok() == Some(t))
}

/// Something [`decide`] says goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doomed {
    pub part: Part,
    pub witness: Stamp,
    pub paths: Vec<PathBuf>,
}

/// THE RULE, pure: in each profile, every incremental crate folder nothing wrote for `after`
/// ([`RECLAIM_STALE`], or [`RECLAIM_STALE_UNDER_PRESSURE`] below the disk watch's floor); and,
/// when cargo last built here with a store's build ([`Survey::last_built`]), every unit whose
/// fingerprint names a compiler no unit of the profile written since the store's `current` link
/// was made names, nothing having written its fingerprint or any of its entries for `after`.
/// Where that build is the current one, a profile with no unit written since keeps every unit:
/// nothing there says which compiler is current. And where the build directory's last passing
/// release run named the units it used ([`USED`], [`Survey::began`]), every unit of the profile
/// of a package that run built tests of ([`Survey::members`]) that it did not use, of a package
/// and kind ([`Unit::kind`]) it used another unit of, nothing having written it since that run
/// began nor for `after` ([`Part::Superseded`]).
#[must_use]
pub fn decide(survey: &Survey, now: SystemTime, after: Duration) -> Vec<Doomed> {
    let stale = |t: Option<SystemTime>| {
        t.is_some_and(|t| now.duration_since(t).unwrap_or(Duration::ZERO) >= after)
    };
    let quiet = |unit: &Unit| {
        stale(unit.fingerprint.newest) && unit.artifacts.iter().all(|a| stale(a.newest))
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
        let old = old_compilers(survey, profile);
        for (unit, _) in profile.units.iter().zip(&old).filter(|(_, old)| **old) {
            if quiet(unit) {
                doomed.push(Doomed {
                    part: Part::Unit,
                    witness: unit.fingerprint.clone(),
                    paths: unit.paths(),
                });
            }
        }
        let (Some(used), Some(began)) = (profile.used.as_ref(), survey.began) else {
            continue;
        };
        let ours = |u: &Unit| survey.members.contains(&u.package);
        let kinds: BTreeSet<&str> = profile
            .units
            .iter()
            .filter(|u| ours(u) && used.contains(&u.hash))
            .map(|u| u.kind.as_str())
            .collect();
        let before = |t: Option<SystemTime>| t.is_some_and(|t| t < began);
        for (unit, old) in profile.units.iter().zip(&old) {
            if *old
                || used.contains(&unit.hash)
                || !ours(unit)
                || !kinds.contains(unit.kind.as_str())
                || !quiet(unit)
                || !before(unit.fingerprint.newest)
                || !unit.artifacts.iter().all(|a| before(a.newest))
            {
                continue;
            }
            doomed.push(Doomed {
                part: Part::Superseded,
                witness: unit.fingerprint.clone(),
                paths: unit.paths(),
            });
        }
    }
    doomed
}

/// Which of `profile`'s units, in order, an old compiler built ([`decide`]'s second rule),
/// whatever their age: one whose fingerprint names a compiler no unit of the profile written
/// since the store's `current` link was made names, where cargo last built here with a store's
/// build, and, where that is the current one, some unit was written since.
fn old_compilers(survey: &Survey, profile: &Profile) -> Vec<bool> {
    let none = vec![false; profile.units.len()];
    let Some(last) = survey.last_built else {
        return none;
    };
    let current: BTreeSet<u64> = profile
        .units
        .iter()
        .filter(|u| u.fingerprint.newest.is_some_and(|t| t >= last.since))
        .filter_map(|u| u.rustc)
        .collect();
    if last.current && current.is_empty() {
        return none;
    }
    profile
        .units
        .iter()
        .map(|u| u.rustc.is_some_and(|r| !current.contains(&r)))
        .collect()
}

/// Reads the build directory `leaf`: its profiles, the compiler that last built there, and the
/// units its last passing release run used ([`USED`]), that run's beginning read no later than
/// its record was written.
pub fn survey(fs: &dyn Fs, leaf: &Path, profiles: &[PathBuf], store: Option<&Path>) -> Survey {
    let record = leaf.join(USED);
    let used = fs
        .meta(&record)
        .filter(|m| m.kind == Kind::File)
        .and_then(|m| {
            let bytes = fs.head(&record, USED_MAX + 1)?;
            let used = (bytes.len() <= USED_MAX)
                .then(|| Used::decode(&bytes))
                .flatten()?;
            let began = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(used.began_ms))?;
            Some((began.min(m.modified), used))
        });
    Survey {
        last_built: last_built(fs, leaf, store),
        profiles: profiles
            .iter()
            .map(|p| {
                let mut read = profile(fs, p);
                read.used = used.as_ref().map(|(_, u)| {
                    let name = p
                        .strip_prefix(leaf)
                        .ok()
                        .and_then(Path::to_str)
                        .map(|n| n.replace(std::path::MAIN_SEPARATOR, "/"))
                        .unwrap_or_default();
                    u.profiles.get(&name).cloned().unwrap_or_default()
                });
                read
            })
            .collect(),
        members: used
            .as_ref()
            .map(|(_, u)| u.members.clone())
            .unwrap_or_default(),
        began: used.map(|(at, _)| at),
    }
}

/// Whether `doomed` still stands as [`survey`] saw it: its witness written no later.
pub fn unchanged(fs: &dyn Fs, doomed: &Doomed) -> bool {
    doomed.witness.newest.is_some() && stamp(fs, &doomed.witness.path) == doomed.witness
}

/// The newest write in `path`.
pub fn stamp(fs: &dyn Fs, path: &Path) -> Stamp {
    aterm_uds::progress::tick();
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

/// One profile's incremental crate folders and units, in either layout.
fn profile(fs: &dyn Fs, dir: &Path) -> Profile {
    let crates = subdirs(fs, &dir.join("incremental"))
        .map(|p| stamp(fs, &p))
        .collect();
    let mut units = named_units(fs, dir);
    units.extend(folder_units(fs, dir));
    Profile {
        crates,
        units,
        used: None,
    }
}

/// The units of the layout that names a unit's entries with its hash: each `.fingerprint/`
/// folder, with the `deps/`, `build/` and `examples/` entries named with its hash; none where one
/// of those folders is there but cannot be listed (what a unit holds would go unread).
fn named_units(fs: &dyn Fs, dir: &Path) -> Vec<Unit> {
    let mut named: Vec<(String, PathBuf)> = Vec::new();
    for sub in ["deps", "build", "examples"] {
        let sub = dir.join(sub);
        let Some(listed) = fs.list(&sub) else {
            if fs.meta(&sub).is_some() {
                return Vec::new();
            }
            continue;
        };
        for (name, meta) in listed {
            let Some(hash) = name.to_str().and_then(unit_hash) else {
                continue;
            };
            if meta.is_some_and(|m| matches!(m.kind, Kind::Dir | Kind::File | Kind::Link)) {
                named.push((hash.to_owned(), sub.join(&name)));
            }
        }
    }
    named.sort();
    subdirs(fs, &dir.join(".fingerprint"))
        .filter_map(|fingerprint| {
            let name = fingerprint.file_name()?.to_str()?.to_owned();
            let hash = unit_hash(&name)?.to_owned();
            let package = name.strip_suffix(&format!("-{hash}"))?.to_owned();
            let from = named.partition_point(|(h, _)| *h < hash);
            let mine: Vec<&PathBuf> = named[from..]
                .iter()
                .take_while(|(h, _)| *h == hash)
                .map(|(_, p)| p)
                .collect();
            let outputs: Vec<String> = mine
                .iter()
                .filter_map(|p| p.file_name()?.to_str().map(str::to_owned))
                .collect();
            Some(Unit {
                rustc: compiler(fs, &fingerprint),
                kind: kind_of(fs, &package, &fingerprint, &outputs)?,
                fingerprint: stamp(fs, &fingerprint),
                artifacts: mine.into_iter().map(|p| stamp(fs, p)).collect(),
                hash,
                package,
                folder: None,
            })
        })
        .collect()
}

/// The units of the layout that keeps each in a folder of its own, `build/<pkg>/<hash>/`, its
/// fingerprint in `fingerprint/` and its outputs in `out/` (a build script's run in `run/` too);
/// not one whose folder or `out/` cannot be listed (what it holds would go unread).
fn folder_units(fs: &dyn Fs, dir: &Path) -> Vec<Unit> {
    let mut units = Vec::new();
    for package in subdirs(fs, &dir.join("build")) {
        let Some(name) = package
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        if unit_hash(&name).is_some() {
            continue;
        }
        for folder in subdirs(fs, &package) {
            let Some(hash) = folder
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|h| is_hash(h))
                .map(str::to_owned)
            else {
                continue;
            };
            let fingerprint = folder.join("fingerprint");
            if fs.meta(&fingerprint).is_none_or(|m| m.kind != Kind::Dir) {
                continue;
            }
            let Some(listed) = fs.list(&folder) else {
                continue;
            };
            let entries: Vec<PathBuf> = listed
                .into_iter()
                .filter(|(n, _)| n != "fingerprint")
                .map(|(n, _)| folder.join(n))
                .collect();
            let out = folder.join("out");
            let outputs: Vec<String> = if entries.contains(&out) {
                let Some(listed) = fs.list(&out) else {
                    continue;
                };
                listed
                    .into_iter()
                    .filter_map(|(n, _)| n.to_str().map(str::to_owned))
                    .collect()
            } else {
                Vec::new()
            };
            let Some(kind) = kind_of(fs, &name, &fingerprint, &outputs) else {
                continue;
            };
            units.push(Unit {
                rustc: compiler(fs, &fingerprint),
                kind,
                fingerprint: stamp(fs, &fingerprint),
                artifacts: entries.iter().map(|p| stamp(fs, p)).collect(),
                hash,
                package: name.clone(),
                folder: Some(folder),
            });
        }
    }
    units
}

/// What a unit of `package` is a build of ([`Unit::kind`]): the kinds its fingerprint folder
/// names (each file but its timestamp, its dep-info, its saved output and its JSON), and whether
/// its `outputs` hold a library's `.rlib` or `.rmeta`; `None` where that folder cannot be listed.
fn kind_of(fs: &dyn Fs, package: &str, fingerprint: &Path, outputs: &[String]) -> Option<String> {
    let mut kinds: Vec<String> = fs
        .list(fingerprint)?
        .into_iter()
        .filter(|(_, m)| m.is_some_and(|m| m.kind == Kind::File))
        .filter_map(|(n, _)| n.to_str().map(str::to_owned))
        .filter(|n| {
            n != "invoked.timestamp"
                && !n.starts_with("dep-")
                && !n.starts_with("output-")
                && !n.ends_with(".json")
        })
        .collect();
    kinds.sort();
    let has = |ext: &str| outputs.iter().any(|o| o.ends_with(ext));
    Some(format!(
        "{package} {} {}{}",
        kinds.join(","),
        if has(".rlib") { "rlib" } else { "-" },
        if has(".rmeta") { "+rmeta" } else { "" }
    ))
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
    aterm_uds::progress::tick();
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
    if relative.next().is_some()
        || sysroot_build.is_empty()
        || !sysroot_build.bytes().all(|b| b.is_ascii_digit())
        || sysroot_build.parse::<u64>().is_err()
    {
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
    if relative.next().is_some()
        || target_build.is_empty()
        || !target_build.bytes().all(|b| b.is_ascii_digit())
        || target_build.parse::<u64>().is_err()
    {
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
