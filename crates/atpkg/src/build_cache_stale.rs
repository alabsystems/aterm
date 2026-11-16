// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHAT NO BUILD READS AGAIN inside a build directory that stays: stale incremental folders,
//! units an old compiler built, and, in a build directory whose check runs or `targo` builds named
//! the units they used ([`USED`], [`Kept`]; [`RECEIPTS`], [`Receipts`]), each unit of the
//! workspace's own packages those runs superseded (none of the kept runs uses it, and one uses
//! another build of the same package and kind, [`Part::Superseded`]). A
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

/// Extra receipt reads across one fingerprint directory, including a byte that must
/// remain unused to establish that each required metadata file was read in full.
const OUTPUT_METADATA_BUDGET: usize = 64 * 1024;

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
    /// The `rustc` every ordinary compiler JSON in the fingerprint folder names;
    /// recognized native output metadata is not another compiler fingerprint.
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
    /// The hashes of its units the build directory's kept runs used ([`USED`], [`Kept::runs`],
    /// and each receipt [`Receipts::read`] takes), together; `None` where the build directory
    /// carries no record (or one unread) and no receipt ([`Survey::began`]).
    pub used: Option<BTreeSet<String>>,
    /// The hashes of its units the run that last ended in the build directory retired
    /// ([`Kept::retired`]): a kept run used each, and none does since that run ended. Empty where
    /// the record names none, or there is no record.
    pub retired: BTreeSet<String>,
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
    /// When the oldest of the build directory's kept runs began ([`USED`], [`Kept::runs`], and its
    /// receipts, [`Receipts`]; [`Used::began_ms`]), each no later than its file was written;
    /// `None` where it carries no record (or one unread, or one that keeps no run) and no receipt.
    pub began: Option<SystemTime>,
    /// The workspace's own packages the kept runs built ([`Used::members`]), together.
    pub members: BTreeSet<String>,
    /// When the run that last ended in the build directory began ([`Kept::retired`], its
    /// [`Used::began_ms`]), no later than its record was written: a unit it retired
    /// ([`Profile::retired`]) that nothing wrote since goes without waiting to be quiet. `None`
    /// where there is no record, or one unread.
    pub retired_before: Option<SystemTime>,
}

/// Ages of recognized survey entries, never a census of the whole cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Ages {
    pub quiet: usize,
    pub not_quiet: usize,
    pub unknown: usize,
}

impl Ages {
    fn add(&mut self, known: bool, quiet: bool) {
        if !known {
            self.unknown += 1;
        } else if quiet {
            self.quiet += 1;
        } else {
            self.not_quiet += 1;
        }
    }
}

/// Evidence already in a survey. No reads, byte estimates or removal authority.
/// Unrecognized/unreadable layouts can be absent from the survey entirely.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Observations {
    pub incremental: Ages,
    pub units: Ages,
    pub last_build_current: Option<bool>,
    pub profiles: usize,
    pub profiles_with_compiler_references: usize,
    pub units_without_compiler: usize,
    /// The build directory's record of the units its check runs used ([`USED`]) was read and
    /// keeps a run, or a receipt of a `targo` build was read ([`Survey::began`]).
    pub used_record: bool,
    pub profiles_naming_used_units: usize,
}

/// Describe recognized observations without inferring current-compiler units from
/// missing old-compiler evidence, or treating an omitted entry as young or unused.
pub(super) fn observations(survey: &Survey, now: SystemTime, after: Duration) -> Observations {
    let mut seen = Observations {
        last_build_current: survey.last_built.map(|last| last.current),
        profiles: survey.profiles.len(),
        used_record: survey.began.is_some(),
        ..Observations::default()
    };
    for profile in &survey.profiles {
        for folder in &profile.crates {
            seen.incremental
                .add(folder.newest.is_some(), stale_at(folder.newest, now, after));
        }
        for unit in &profile.units {
            let known = unit.fingerprint.newest.is_some()
                && unit.artifacts.iter().all(|a| a.newest.is_some());
            seen.units.add(known, quiet_at(unit, now, after));
            seen.units_without_compiler += usize::from(unit.rustc.is_none());
        }
        seen.profiles_with_compiler_references +=
            usize::from(survey.last_built.is_some_and(|last| {
                profile
                    .units
                    .iter()
                    .any(|unit| compiler_reference(unit, last).is_some())
            }));
        seen.profiles_naming_used_units +=
            usize::from(profile.used.as_ref().is_some_and(|used| !used.is_empty()));
    }
    seen
}

fn stale_at(at: Option<SystemTime>, now: SystemTime, after: Duration) -> bool {
    at.is_some_and(|at| now.duration_since(at).unwrap_or(Duration::ZERO) >= after)
}

fn quiet_at(unit: &Unit, now: SystemTime, after: Duration) -> bool {
    stale_at(unit.fingerprint.newest, now, after)
        && unit
            .artifacts
            .iter()
            .all(|a| stale_at(a.newest, now, after))
}

fn compiler_reference(unit: &Unit, last: LastBuilt) -> Option<u64> {
    unit.fingerprint
        .newest
        .filter(|at| *at >= last.since)
        .and(unit.rustc)
}

/// What one doomed thing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// A stale incremental crate folder.
    Incremental,
    /// A unit an old compiler built: its fingerprint folder and its entries.
    Unit,
    /// A unit the build directory's kept runs superseded ([`USED`]): of a package of the
    /// workspace's own ([`Used::members`]), which none of them uses while one uses another build
    /// of its package and kind ([`Unit::kind`]); written before the run that retired it began
    /// ([`Kept::retired`]) or, where no kept run ever named it, before the oldest kept run began
    /// and by nothing for the stale window.
    Superseded,
}

/// THE RECORD OF WHAT THE CHECK RUNS OF A BUILD DIRECTORY USED, at its top (`aterm-verify`
/// writes it as each run whose compile calls completed ends): the newest run of each scope
/// ([`Kept::runs`]; a scope is what a run was asked to check, so a scoped run replaces only its
/// own scope's), at most [`KEPT_RUNS`] of them and none begun more than [`KEPT_FOR`] before the
/// run that ended last, and the units that run retired ([`Kept::retired`]). Of each run it holds
/// when it began, the workspace's own packages it built ([`Used::members`]), and the units of each
/// profile its compile calls reported as their outputs, fresh or built, the bins they copied into
/// the target directory and the build scripts they ran among them ([`Used`]). A unit of one of
/// those packages that no kept run names, of a kind a kept run names another unit of, is a build
/// those runs superseded ([`Part::Superseded`]): every version bump, and every change to the
/// features or dependencies a unit is built with, hashes each unit of the workspace's own
/// packages it reaches anew (their version, or a dependency's), and cargo never takes the old
/// ones, so a build directory grew by every such change's units (an edit to a unit's source alone
/// builds it again in place, under the hash it had). One the run that ended last retired (a kept
/// run named it, and none does now) goes at that run's end, where nothing wrote it since that run
/// began. One no kept run ever named (a plain build's) goes once nothing wrote it since the
/// oldest kept run began nor for the stale window. Kept are every unit a kept run names; every
/// unit of another package (a dependency's, the same build in every candidate, which another
/// compile of a run may use in a build of its own the record does not see, spec-check's under its
/// own features: cargo never rewrites a unit it found fresh, so its age says nothing of its use);
/// every unit of a kind the record names none of; and every unit written since the run it is
/// judged against began, which some compile built (spec-check's build of the models without the
/// checkers). A `targo` build that keeps a receipt ([`RECEIPTS`]) is a kept run too, read beside
/// the record's ([`survey`]), and the record keeps a copy of each receipt it last took, so the
/// next end that takes them ([`Kept::take_receipts`]) retires what a selection's build before used
/// and its build since does not. ACCEPTED: a unit of the workspace's own that a compile no run
/// or receipt reports (a build with no receipt, a doctest's or spec-check's) found fresh goes
/// once a kept run replaced it or it is quiet for the window, though that compile reads it again:
/// that compile builds it once more.
/// The record before this one (schema 1, `.aterm-release-used`, a passing release run's alone) is
/// not read and supersedes nothing; the first run to end in its build directory removes it
/// ([`Kept::keep`]).
pub const USED: &str = ".aterm-used";

/// The name the record of schema 1 was kept under, which [`Kept::keep`] removes.
const USED_BEFORE: &str = ".aterm-release-used";

/// The most runs a [`USED`] record keeps, one to a scope ([`Kept::end`]). Every kept run holds
/// its units against both rules that take a superseded one, so each more keeps another run's
/// units on disk, and each fewer makes the scopes one checkout alternates between (a `-p` loop,
/// the landing check over its cone, a run of the whole workspace, one of them narrowed) retire
/// each other's units and build them again.
pub const KEPT_RUNS: usize = 4;

/// A kept run goes from a [`USED`] record once another run ends that began this much later
/// ([`Kept::end`]), or once the record takes a receipt of a build that began this much later
/// ([`Kept::take_receipts`]): without it a scope nobody runs again, among [`KEPT_RUNS`] or
/// fewer, stays the oldest kept run for good, and no unit written since it began would be
/// superseded ([`Survey::began`]).
pub const KEPT_FOR: Duration = RECLAIM_STALE;

/// The lock file beside the [`USED`] record that every change to the record holds from its read
/// to its write ([`Kept::update`]): a verify run's end and a plain build's holder may end in one
/// build directory at once, and a change made from a read the other's write then replaced would
/// drop the other's kept run.
pub const USED_LOCK: &str = ".aterm-used.lock";

/// The longest a change to the [`USED`] record waits for another's [`USED_LOCK`]: a change reads,
/// folds and writes one record of at most [`USED_MAX`] bytes, far within it. Past it the change
/// is not made, and the record before stands.
const USED_LOCK_WITHIN: Duration = Duration::from_secs(10);

/// The most of a [`USED`] record read: some thousands of units of a few dozen bytes each, for
/// each kept run and each receipt copy. A longer one is not read, and supersedes nothing.
const USED_MAX: usize = 16 << 20;

/// THE RECEIPTS `targo` KEEPS (Trust's DC-12a, `build_receipt.rs` in targo-core) in a build
/// directory: once a build's compile graph has run and succeeded, one file for its selection,
/// `<16 lowercase hex>.json` in this folder, replaced by the selection's next build and withdrawn
/// by one whose graph succeeded but could keep none. Its `began_ms`, `members` and `profiles`
/// are a [`Used`] in this module's encoding ([`Used::of_receipt`]); `members` names every member
/// the build used. A build run as `cargo`, or by a Trust build without DC-12a, keeps none, and a
/// build directory with no receipt is judged as it was before receipts.
pub const RECEIPTS: &str = ".targo-receipts";

/// The receipt format [`Receipt::of_bytes`] reads (targo's `FORMAT`).
pub const RECEIPT_FORMAT: u64 = 1;

/// The most of one receipt read: a few thousand units and the selection's roots are some hundreds
/// of KiB. A longer one is not read, as a malformed one is not.
const RECEIPT_MAX: usize = 4 << 20;

/// The most bytes of receipts one read of a build directory takes, newest first; a receipt past
/// it is not read.
const RECEIPTS_READ_MAX: usize = 16 << 20;

/// The most receipts one read of a build directory takes, newest first: one per selection built
/// within [`KEPT_FOR`] of the newest. One past it is not read: its units are judged as a build's
/// with no receipt, and the read is not whole ([`Receipts::unread`]), so nothing is retired at
/// once from it.
pub const RECEIPTS_MAX: usize = 64;

/// The units a run used, by profile, the workspace's own packages it built, and when it began
/// ([`USED`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Used {
    /// When the run began, milliseconds since the epoch: a unit written since is one some compile
    /// of it built. `0` (the default) supersedes nothing.
    pub began_ms: u64,
    /// The workspace's own packages it built, as the build directory names them, which alone it
    /// supersedes units of: each it built tests of, and where its writer knew the workspace's
    /// members by name, each of them it built anything of (`aterm-verify` names them for a
    /// cell's calls, whose lint builds no test).
    pub members: BTreeSet<String>,
    /// Each profile, by its path below the build directory (`debug`, `aarch64-apple-darwin/debug`),
    /// and the hashes of its units the run used.
    pub profiles: BTreeMap<String, BTreeSet<String>>,
}

/// Whether `word` is one plain path component: not empty, not `.` or `..`, and holding no
/// separator.
fn plain(word: &str) -> bool {
    !word.is_empty() && word != "." && word != ".." && !word.contains(['/', '\\'])
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
    /// [`Self::add`] does not read either ([`uplifted_units`]).
    pub fn add_uplifted(&mut self, leaf: &Path, profile: &str, package: &str, file: &Path) {
        let hashes = uplifted_units(leaf, profile, package, file);
        if !hashes.is_empty() {
            self.profiles
                .entry(profile.to_owned())
                .or_default()
                .extend(hashes);
        }
    }

    /// The run as a [`USED`] record holds it: when it began, its packages and each profile's
    /// hashes, sorted.
    fn value(&self) -> aterm_json::Value {
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
        aterm_json::Value::Object(o)
    }

    /// The run `v` holds; `None` for a value that is not [`Self::value`]'s: no time it began, no
    /// packages or an empty one among them, a profile whose name is not plain components joined
    /// by `/` (a record's profile names say which folders a trim reads), or a hash that is not
    /// one ([`unit_hash`]'s sixteen hex digits).
    fn of_value(v: &aterm_json::Value) -> Option<Used> {
        let mut used = Used {
            began_ms: v.get("began_ms")?.as_u64()?,
            ..Used::default()
        };
        for member in v.get("members")?.as_array()? {
            used.members
                .insert(member.as_str().filter(|m| !m.is_empty())?.to_owned());
        }
        for (name, hashes) in v.get("profiles")?.as_object()? {
            if !name.split('/').all(plain) {
                return None;
            }
            let set = used.profiles.entry(name.clone()).or_default();
            for h in hashes.as_array()? {
                let h = h.as_str().filter(|h| is_hash(h))?;
                set.insert(h.to_owned());
            }
        }
        Some(used)
    }

    /// The run a `targo` receipt's bytes hold ([`Receipt::of_bytes`]).
    #[must_use]
    pub fn of_receipt(bytes: &[u8]) -> Option<Used> {
        Receipt::of_bytes(bytes).map(|receipt| receipt.run)
    }

    /// Each `(profile, hash)` unit it names.
    fn units(&self) -> impl Iterator<Item = (&str, &str)> {
        self.profiles
            .iter()
            .flat_map(|(p, hashes)| hashes.iter().map(move |h| (p.as_str(), h.as_str())))
    }
}

/// ONE `targo` RECEIPT ([`RECEIPTS`]) as this module reads it: the run it holds, and which of its
/// profiles its build compiled incrementally.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipt {
    /// Its `began_ms`, `members` and `profiles`, a [`USED`] record's run.
    pub run: Used,
    /// The profiles its `incremental` (Trust's DC-12b) names a unit of: those whose units the
    /// build compiled into the profile's `incremental/` folder, which a member's unit of a
    /// profile with incremental compilation on is. `None` for a receipt with no `incremental`
    /// (written before DC-12b), which says nothing of it. A selection's name leaves out what
    /// turns incremental compilation on or off (`CARGO_INCREMENTAL`, which aterm-verify's main
    /// lane sets to `0` in the build directory plain builds share), though a member's unit hash
    /// holds it: two builds of one selection that differ there replace each other's receipt and
    /// use two sets of units, neither of which supersedes the other ([`Self::same_variant`]).
    pub incremental: Option<BTreeSet<String>>,
}

impl Receipt {
    /// The receipt a `targo` receipt's bytes hold: its `began_ms`, `members` and `profiles`
    /// read as a [`USED`] record's run is ([`Used::of_value`]), and the profiles its
    /// `incremental` names a unit of. `None` for bytes that are no receipt of format
    /// [`RECEIPT_FORMAT`] with a `selection` object, that carry a `schema` (a record of this
    /// module's, never a receipt), whose run is none, or whose `incremental` is there and is no
    /// object of profiles, each named as a run's are, to objects.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Option<Receipt> {
        let v: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if v.get("receipt").and_then(aterm_json::Value::as_u64) != Some(RECEIPT_FORMAT)
            || !v.get("selection").is_some_and(aterm_json::Value::is_object)
            || v.get("schema").is_some()
        {
            return None;
        }
        let incremental = match v.get("incremental") {
            None => None,
            Some(by_profile) => {
                let mut named = BTreeSet::new();
                for (profile, units) in by_profile.as_object()? {
                    if !profile.split('/').all(plain) {
                        return None;
                    }
                    if !units.as_object()?.is_empty() {
                        named.insert(profile.clone());
                    }
                }
                Some(named)
            }
        };
        Some(Receipt {
            run: Used::of_value(&v)?,
            incremental,
        })
    }

    /// The copy a [`USED`] record keeps ([`Kept::receipts`]): the run's value, with its
    /// incremental profiles as an array (`incremental`) where the receipt said them.
    fn value(&self) -> aterm_json::Value {
        let mut v = self.run.value();
        if let (Some(o), Some(profiles)) = (v.as_object_mut(), &self.incremental) {
            o.insert(
                "incremental".to_owned(),
                aterm_json::Value::Array(
                    profiles
                        .iter()
                        .map(|p| aterm_json::Value::from(p.as_str()))
                        .collect(),
                ),
            );
        }
        v
    }

    /// The copy `v` holds ([`Self::value`]); `None` where its run is none, or its `incremental`
    /// is there and is no array of profiles named as a run's are.
    fn of_value(v: &aterm_json::Value) -> Option<Receipt> {
        let incremental = match v.get("incremental") {
            None => None,
            Some(profiles) => Some(
                profiles
                    .as_array()?
                    .iter()
                    .map(|p| {
                        p.as_str()
                            .filter(|p| p.split('/').all(plain))
                            .map(str::to_owned)
                    })
                    .collect::<Option<BTreeSet<String>>>()?,
            ),
        };
        Some(Receipt {
            run: Used::of_value(v)?,
            incremental,
        })
    }

    /// Whether the build behind this receipt and the one behind `since`, a later receipt of the
    /// same selection, built `profile` in one variant, so what this one used there and `since`
    /// did not is superseded: both say which profiles they compiled incrementally
    /// ([`Self::incremental`]), and agree on `profile`. Builds that differ there (a plain build
    /// and verify's main lane, `CARGO_INCREMENTAL=0`) each keep their units for the other.
    #[must_use]
    pub fn same_variant(&self, since: &Receipt, profile: &str) -> bool {
        match (&self.incremental, &since.incremental) {
            (Some(was), Some(now)) => was.contains(profile) == now.contains(profile),
            _ => false,
        }
    }
}

/// THE RECEIPTS OF A BUILD DIRECTORY ([`RECEIPTS`]) one read takes ([`Self::read`]), each by its
/// file's name without `.json` (its selection's name), and how many it left unread.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipts {
    pub runs: BTreeMap<String, Receipt>,
    /// How many files named as receipts, written within [`KEPT_FOR`] of the newest, the read did
    /// not take: past a bound, unreadable, or none ([`Receipt::of_bytes`]). Such a receipt may
    /// name a unit nothing read names, so a read that left one is not whole
    /// ([`Self::whole`]), and nothing is retired at once from it.
    pub unread: usize,
}

impl Receipts {
    /// The receipts in the build directory `leaf`, as `fs` reads them: the regular files of the
    /// real folder [`RECEIPTS`] named sixteen lowercase hex digits and `.json` (a write cut short
    /// leaves a `.receipt-*.tmp` there), newest first, each written within [`KEPT_FOR`] of the
    /// newest (an older one is a selection nobody built since, as [`Kept::end`] ages a run), at
    /// most [`RECEIPTS_MAX`] of them, [`RECEIPT_MAX`] bytes each and [`RECEIPTS_READ_MAX`]
    /// together; each run as beginning no later than its file was written. A receipt past a
    /// bound, or that is none ([`Receipt::of_bytes`]), is not taken and is counted
    /// ([`Self::unread`]). Empty where there is no such folder or it is a link; one that cannot
    /// be listed is read as one receipt unread.
    #[must_use]
    pub fn read_on(fs: &dyn Fs, leaf: &Path) -> Receipts {
        let dir = leaf.join(RECEIPTS);
        let mut out = Receipts::default();
        if fs.meta(&dir).map(|m| m.kind) != Some(Kind::Dir) {
            return out;
        }
        let Some(entries) = fs.list(&dir) else {
            out.unread = 1;
            return out;
        };
        let mut files: Vec<(SystemTime, String)> = entries
            .into_iter()
            .filter_map(|(name, meta)| {
                let stem = name
                    .to_str()?
                    .strip_suffix(".json")
                    .filter(|s| is_hash(s))?;
                let meta = meta.filter(|m| m.kind == Kind::File)?;
                Some((meta.modified, stem.to_owned()))
            })
            .collect();
        let Some(newest) = files.iter().map(|(at, _)| *at).max() else {
            return out;
        };
        files.retain(|(at, _)| newest.duration_since(*at).unwrap_or(Duration::ZERO) <= KEPT_FOR);
        files.sort_by(|a, b| b.cmp(a));
        out.unread = files.len().saturating_sub(RECEIPTS_MAX);
        files.truncate(RECEIPTS_MAX);
        let mut budget = RECEIPTS_READ_MAX;
        for (written, stem) in files {
            let taken = fs
                .head(&dir.join(format!("{stem}.json")), RECEIPT_MAX + 1)
                .filter(|bytes| bytes.len() <= RECEIPT_MAX && bytes.len() <= budget)
                .and_then(|bytes| {
                    budget -= bytes.len();
                    Receipt::of_bytes(&bytes)
                });
            let Some(mut receipt) = taken else {
                out.unread += 1;
                continue;
            };
            let written_ms = written
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            receipt.run.began_ms = receipt.run.began_ms.min(written_ms);
            out.runs.insert(stem, receipt);
        }
        out
    }

    /// Whether the read took every receipt written within [`KEPT_FOR`] of the newest
    /// ([`Self::unread`]): only then does no receipt name a unit the read does not see.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.unread == 0
    }

    /// Each receipt's run.
    fn used(&self) -> impl Iterator<Item = &Used> {
        self.runs.values().map(|receipt| &receipt.run)
    }

    /// [`Self::read_on`] on the real filesystem.
    #[must_use]
    pub fn read(leaf: &Path) -> Receipts {
        Self::read_on(&Disk, leaf)
    }

    /// Whether the read is whole ([`Self::whole`]) and the receipts of builds begun no earlier
    /// than `run` began together name every unit `run` names, and it names one: the builds
    /// behind them used what `run` used, so they speak for it ([`Kept::take_receipts`]).
    #[must_use]
    pub fn name_all(&self, run: &Used) -> bool {
        let since: Vec<&Used> = self.used().filter(|r| r.began_ms >= run.began_ms).collect();
        let mut units = run.units().peekable();
        self.whole()
            && units.peek().is_some()
            && units.all(|(profile, hash)| {
                since
                    .iter()
                    .any(|r| r.profiles.get(profile).is_some_and(|h| h.contains(hash)))
            })
    }
}

/// What a [`USED`] record holds: the newest run of each scope, and what the run that ended last
/// retired.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Kept {
    /// The newest run of each scope, by the scope's name: what its run was asked to check, spelled
    /// the same each time (`aterm-verify` names its checkout, its checks and how they are
    /// narrowed, and its packages).
    pub runs: BTreeMap<String, Used>,
    /// What the run that ended last retired ([`Self::end`]): by profile, the units a run it
    /// replaced or evicted used and no kept run uses; and when that run began, which a retired
    /// unit must be older than to go ([`decide`]). Its packages are not read. Each end computes
    /// it again and carries nothing over: a unit an earlier end retired that is still there may
    /// since have been built again in place by a compile no run reports, so it waits the stale
    /// window like any unit no kept run names.
    pub retired: Used,
    /// A copy of each receipt the record last took ([`Self::take_receipts`]), by its name: what
    /// the selection's build before used, which `targo` replaces on disk with what its build since
    /// used. It is no kept run: [`survey`] reads the receipts on disk.
    pub receipts: BTreeMap<String, Receipt>,
}

impl Kept {
    /// THE RUN `run` OF `scope` ENDED: it is kept in place of the run of its scope before; every
    /// other kept run that began more than [`KEPT_FOR`] before it goes; and while more than
    /// [`KEPT_RUNS`] are kept, the one that began first goes (by its scope's name between two
    /// that began together), never `run` itself. [`Self::retired`] becomes what those runs used
    /// that no kept run uses, as of when `run` began. Returns how many units that is.
    pub fn end(&mut self, scope: &str, run: Used) -> usize {
        let began_ms = run.began_ms;
        let mut dropped: Vec<Used> = self
            .runs
            .insert(scope.to_owned(), run)
            .into_iter()
            .collect();
        let window = u64::try_from(KEPT_FOR.as_millis()).unwrap_or(u64::MAX);
        let aged: Vec<String> = self
            .runs
            .iter()
            .filter(|(name, kept)| {
                name.as_str() != scope && kept.began_ms.saturating_add(window) < began_ms
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in aged {
            dropped.extend(self.runs.remove(&name));
        }
        while self.runs.len() > KEPT_RUNS {
            let Some(first) = self
                .runs
                .iter()
                .filter(|(name, _)| name.as_str() != scope)
                .min_by(|a, b| (a.1.began_ms, a.0).cmp(&(b.1.began_ms, b.0)))
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            dropped.extend(self.runs.remove(&first));
        }
        let mut retired = Used {
            began_ms,
            ..Used::default()
        };
        for run in dropped {
            for (profile, hashes) in run.profiles {
                retired.profiles.entry(profile).or_default().extend(hashes);
            }
        }
        for kept in self.runs.values() {
            for (profile, hashes) in &kept.profiles {
                if let Some(gone) = retired.profiles.get_mut(profile) {
                    gone.retain(|hash| !hashes.contains(hash));
                }
            }
        }
        retired.profiles.retain(|_, hashes| !hashes.is_empty());
        self.retired = retired;
        self.retired.profiles.values().map(BTreeSet::len).sum()
    }

    /// THE BUILDS BEHIND `receipts` ENDED (a `targo` build's holder at its end, or a run whose
    /// builds' receipts speak for it, `covered`): the record takes a copy of them in place of the
    /// copies it held ([`Self::receipts`]), and [`Self::retired`] becomes, as of the earliest of
    /// the builds that retired something began, what the copies of the receipts they replaced
    /// used (a copy of a selection whose receipt now began later), in each profile they built in
    /// one variant ([`Receipt::same_variant`]); where `covered` names a scope and when its run
    /// began, what that scope's kept run used, which goes from the record; and, as of when the
    /// newest of them began, what each kept run that began more than [`KEPT_FOR`] before it used,
    /// which goes from the record as [`Self::end`] ages a run; each less every unit a kept run or
    /// any of `receipts` names. A receipt that is new, or that `targo` withdrew (its selection's
    /// last build used units nobody knows), retires nothing. Returns how many units that is:
    /// where nothing was retired and only the copies changed, 0, and what the last end or take
    /// retired stands. `None` where the record would not change (the copies were these receipts
    /// already and nothing was retired), and where the read of `receipts` is not whole
    /// ([`Receipts::whole`]): a receipt it left may name a unit this would retire, so the record
    /// before, and what it retired, stand.
    pub fn take_receipts(
        &mut self,
        receipts: &Receipts,
        covered: Option<(&str, u64)>,
    ) -> Option<usize> {
        if !receipts.whole() {
            return None;
        }
        let mut dropped: Vec<Used> = Vec::new();
        let mut since: Option<u64> = None;
        let as_of = |since: &mut Option<u64>, ms: u64| {
            *since = Some(since.map_or(ms, |s: u64| s.min(ms)));
        };
        for (name, now) in &receipts.runs {
            let Some(was) = self
                .receipts
                .get(name)
                .filter(|was| was.run.began_ms < now.run.began_ms)
            else {
                continue;
            };
            let superseded = Used {
                profiles: was
                    .run
                    .profiles
                    .iter()
                    .filter(|(profile, _)| was.same_variant(now, profile))
                    .map(|(profile, hashes)| (profile.clone(), hashes.clone()))
                    .collect(),
                ..Used::default()
            };
            if !superseded.profiles.is_empty() {
                dropped.push(superseded);
                as_of(&mut since, now.run.began_ms);
            }
        }
        if let Some((scope, began_ms)) = covered
            && let Some(run) = self.runs.remove(scope)
        {
            dropped.push(run);
            as_of(&mut since, began_ms);
        }
        let newest = receipts
            .used()
            .map(|run| run.began_ms)
            .chain(covered.map(|(_, began_ms)| began_ms))
            .max();
        if let Some(newest) = newest {
            let window = u64::try_from(KEPT_FOR.as_millis()).unwrap_or(u64::MAX);
            let aged: Vec<String> = self
                .runs
                .iter()
                .filter(|(_, kept)| kept.began_ms.saturating_add(window) < newest)
                .map(|(name, _)| name.clone())
                .collect();
            for name in aged {
                dropped.extend(self.runs.remove(&name));
                as_of(&mut since, newest);
            }
        }
        if dropped.is_empty() {
            if self.receipts == receipts.runs {
                return None;
            }
            self.receipts.clone_from(&receipts.runs);
            return Some(0);
        }
        self.receipts.clone_from(&receipts.runs);
        let mut retired = Used {
            began_ms: since.unwrap_or(0),
            ..Used::default()
        };
        for run in dropped {
            for (profile, hashes) in run.profiles {
                retired.profiles.entry(profile).or_default().extend(hashes);
            }
        }
        for kept in self.runs.values().chain(receipts.used()) {
            for (profile, hashes) in &kept.profiles {
                if let Some(gone) = retired.profiles.get_mut(profile) {
                    gone.retain(|hash| !hashes.contains(hash));
                }
            }
        }
        retired.profiles.retain(|_, hashes| !hashes.is_empty());
        self.retired = retired;
        Some(self.retired.profiles.values().map(BTreeSet::len).sum())
    }

    /// CHANGE THE RECORD at the top of the build directory `leaf` ([`USED`]) by `change`, holding
    /// its lock ([`USED_LOCK`]) from the read to the write: the record as it stands (an empty one
    /// where there is none or it cannot be read, [`Self::read`]) is changed, and kept
    /// ([`Self::keep`]) where `change` answers. `None` where it did not, and the record stands.
    ///
    /// # Errors
    /// The lock was not taken within [`USED_LOCK_WITHIN`] (or could not be: `leaf` is no real
    /// directory), or the write failed: the record before stands.
    pub fn update<R>(
        leaf: &Path,
        change: impl FnOnce(&mut Kept) -> Option<R>,
    ) -> std::io::Result<Option<R>> {
        Self::update_within(leaf, USED_LOCK_WITHIN, change)
    }

    /// [`Self::update`], waiting at most `within` for the lock.
    pub(crate) fn update_within<R>(
        leaf: &Path,
        within: Duration,
        change: impl FnOnce(&mut Kept) -> Option<R>,
    ) -> std::io::Result<Option<R>> {
        let _held = crate::lock::lock_file_within(&leaf.join(USED_LOCK), within)?;
        let mut kept = Kept::read(leaf);
        let Some(changed) = change(&mut kept) else {
            return Ok(None);
        };
        kept.keep(leaf)?;
        Ok(Some(changed))
    }

    /// Every profile the record names, a kept run's or a retired unit's.
    #[must_use]
    pub fn profiles(&self) -> BTreeSet<String> {
        self.runs
            .values()
            .chain(std::iter::once(&self.retired))
            .flat_map(|run| run.profiles.keys().cloned())
            .collect()
    }

    /// The record as [`USED`] holds it: schema 2, each kept run by its scope, what the last
    /// retired, and, where it took any, its copies of the receipts (`receipts`, by name).
    #[must_use]
    pub fn encode(&self) -> String {
        let mut runs = aterm_json::Map::new();
        for (scope, run) in &self.runs {
            runs.insert(scope.clone(), run.value());
        }
        let mut o = aterm_json::Map::new();
        o.insert("schema".to_owned(), aterm_json::Value::from(2u64));
        o.insert("runs".to_owned(), aterm_json::Value::Object(runs));
        o.insert("retired".to_owned(), self.retired.value());
        if !self.receipts.is_empty() {
            let mut receipts = aterm_json::Map::new();
            for (name, receipt) in &self.receipts {
                receipts.insert(name.clone(), receipt.value());
            }
            o.insert("receipts".to_owned(), aterm_json::Value::Object(receipts));
        }
        aterm_json::to_string(&aterm_json::Value::Object(o)).unwrap_or_default()
    }

    /// The record `bytes` holds; `None` for bytes that are not [`Self::encode`]'s: another schema
    /// (the record before this one, schema 1, among them), no runs, more runs than
    /// [`Self::end`] ever leaves ([`KEPT_RUNS`]: what a run's end does with a record is bounded
    /// by the runs it holds, so one that holds more is not read at all), nothing retired, more
    /// receipt copies than a read takes ([`RECEIPTS_MAX`]) or one not named as a receipt is, or a
    /// run or copy that is none ([`Used::of_value`], [`Receipt::of_value`]). A record with no
    /// `receipts` holds no copy.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Kept> {
        let v: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if v.get("schema").and_then(aterm_json::Value::as_u64) != Some(2) {
            return None;
        }
        let runs = v.get("runs")?.as_object()?;
        if runs.len() > KEPT_RUNS {
            return None;
        }
        let mut kept = Kept {
            runs: BTreeMap::new(),
            retired: Used::of_value(v.get("retired")?)?,
            receipts: BTreeMap::new(),
        };
        for (scope, run) in runs {
            kept.runs.insert(scope.clone(), Used::of_value(run)?);
        }
        if let Some(receipts) = v.get("receipts") {
            let receipts = receipts.as_object()?;
            if receipts.len() > RECEIPTS_MAX {
                return None;
            }
            for (name, run) in receipts {
                if !is_hash(name) {
                    return None;
                }
                kept.receipts.insert(name.clone(), Receipt::of_value(run)?);
            }
        }
        Some(kept)
    }

    /// The record at the top of the build directory `leaf` and when it was written, as `fs` reads
    /// them: `None` where there is none, it is no regular file, it is longer than [`USED_MAX`],
    /// or it does not decode.
    fn read_on(fs: &dyn Fs, leaf: &Path) -> Option<(SystemTime, Kept)> {
        let record = leaf.join(USED);
        let written = fs.meta(&record).filter(|m| m.kind == Kind::File)?.modified;
        let bytes = fs.head(&record, USED_MAX + 1)?;
        let kept = (bytes.len() <= USED_MAX)
            .then(|| Kept::decode(&bytes))
            .flatten()?;
        Some((written, kept))
    }

    /// The record at the top of the build directory `leaf`; an empty one where there is none or
    /// it cannot be read ([`Self::read_on`]), so the run that ends next starts it again.
    #[must_use]
    pub fn read(leaf: &Path) -> Kept {
        Self::read_on(&Disk, leaf).map_or_else(Kept::default, |(_, kept)| kept)
    }

    /// Keep the record at the top of the build directory `leaf` ([`USED`]): staged in a private
    /// folder there, then renamed over the one before. The record of schema 1, where one is left
    /// under its own name, is then removed; one that stays is not read.
    ///
    /// # Errors
    /// The build directory is not there, or the write failed: the record before stands.
    pub fn keep(&self, leaf: &Path) -> std::io::Result<()> {
        use std::io::Write as _;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".aterm-used-")
            .tempdir_in(leaf)?;
        let staged = stage.path().join("data");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(self.encode().as_bytes())?;
        drop(file);
        std::fs::rename(&staged, leaf.join(USED))?;
        let _ = std::fs::remove_file(leaf.join(USED_BEFORE));
        Ok(())
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
/// nothing there says which compiler is current. And where the build directory's check runs
/// named the units they used ([`USED`], [`Survey::began`]), every unit of the profile of a
/// package of the workspace's own a kept run built ([`Survey::members`]) that no kept run uses,
/// of a package and kind ([`Unit::kind`]) a kept run uses another unit of
/// ([`Part::Superseded`]): one the run that ended last retired ([`Profile::retired`]) where
/// nothing wrote it since that run began ([`Survey::retired_before`]), whatever `after`; any
/// other where nothing wrote it since the oldest kept run began nor for `after`.
#[must_use]
pub fn decide(survey: &Survey, now: SystemTime, after: Duration) -> Vec<Doomed> {
    let stale = |at| stale_at(at, now, after);
    let quiet = |unit: &Unit| quiet_at(unit, now, after);
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
        for (unit, old) in profile.units.iter().zip(&old) {
            // A unit the last run retired is judged against that run's beginning and need not
            // be quiet: a kept run reported it, so its use is known. Any other is judged
            // against the oldest kept run's and waits the window.
            let retired = survey
                .retired_before
                .filter(|_| profile.retired.contains(&unit.hash));
            let since = retired.unwrap_or(began);
            let before = |t: Option<SystemTime>| t.is_some_and(|t| t < since);
            if *old
                || used.contains(&unit.hash)
                || !ours(unit)
                || !kinds.contains(unit.kind.as_str())
                || !(retired.is_some() || quiet(unit))
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
        .filter_map(|u| compiler_reference(u, last))
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
/// units its kept runs used and the last of them retired ([`USED`]), the oldest kept run's
/// beginning and the retiring run's each read no later than the record was written. Each receipt
/// `targo` kept there ([`Receipts::read_on`]) is a kept run beside the record's, read no later
/// than its file was written; where there is no record and no receipt nothing is superseded.
/// Where the read of the receipts is not whole ([`Receipts::whole`]), nothing the record retired
/// goes at once: each unit waits the stale window as one no kept run names.
pub fn survey(fs: &dyn Fs, leaf: &Path, profiles: &[PathBuf], store: Option<&Path>) -> Survey {
    let record = Kept::read_on(fs, leaf);
    let receipts = Receipts::read_on(fs, leaf);
    let at = |ms: u64, written: SystemTime| {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_millis(ms))
            .map(|began| began.min(written))
    };
    let recorded = record.as_ref().and_then(|(written, kept)| {
        let oldest = kept.runs.values().map(|run| run.began_ms).min()?;
        at(oldest, *written)
    });
    let receipted = receipts
        .used()
        .map(|run| run.began_ms)
        .min()
        .and_then(|ms| SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(ms)));
    let began = recorded.into_iter().chain(receipted).min();
    // What the record retired goes at once only while the receipts read whole: one left unread
    // may name a retired unit that a build since used.
    let retiring = record.as_ref().filter(|_| receipts.whole());
    let retired_before = retiring.and_then(|(written, kept)| at(kept.retired.began_ms, *written));
    let named = |run: &Used, name: &str| run.profiles.get(name).cloned().unwrap_or_default();
    let runs: Vec<&Used> = record
        .iter()
        .flat_map(|(_, kept)| kept.runs.values())
        .chain(receipts.used())
        .collect();
    let judged = record.is_some() || !receipts.runs.is_empty();
    Survey {
        last_built: last_built(fs, leaf, store),
        profiles: profiles
            .iter()
            .map(|p| {
                let mut read = profile(fs, p);
                if judged {
                    let name = p
                        .strip_prefix(leaf)
                        .ok()
                        .and_then(Path::to_str)
                        .map(|n| n.replace(std::path::MAIN_SEPARATOR, "/"))
                        .unwrap_or_default();
                    read.used = Some(runs.iter().flat_map(|run| named(run, &name)).collect());
                    if let Some((_, kept)) = retiring {
                        read.retired = named(&kept.retired, &name);
                    }
                }
                read
            })
            .collect(),
        members: runs
            .iter()
            .flat_map(|run| run.members.iter().cloned())
            .collect(),
        began,
        retired_before,
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
        retired: BTreeSet::new(),
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
        units.extend(package_units(fs, &package, &name));
    }
    units
}

/// The units of the package `name` whose folder is `package` (`build/<pkg>`), as
/// [`folder_units`] reads them.
fn package_units(fs: &dyn Fs, package: &Path, name: &str) -> Vec<Unit> {
    let mut units = Vec::new();
    for folder in subdirs(fs, package) {
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
        let Some(kind) = kind_of(fs, name, &fingerprint, &outputs) else {
            continue;
        };
        units.push(Unit {
            rustc: compiler(fs, &fingerprint),
            kind,
            fingerprint: stamp(fs, &fingerprint),
            artifacts: entries.iter().map(|p| stamp(fs, p)).collect(),
            hash,
            package: name.to_owned(),
            folder: Some(folder),
        });
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOutputMetadataV1 {
    schema: u32,
    #[serde(rename = "fingerprint")]
    _fingerprint: u64,
    outputs: Vec<NativeOutputRecord>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOutputMetadataV2 {
    schema: u32,
    #[serde(rename = "fingerprint")]
    _fingerprint: u64,
    compiler: NativeOutputCompiler,
    outputs: Vec<NativeOutputRecord>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOutputMetadataV3 {
    schema: u32,
    #[serde(rename = "fingerprint")]
    _fingerprint: u64,
    compiler: NativeOutputCompiler,
    outputs: Vec<NativeOutputRecord>,
    compiler_outputs: Vec<NativeCompilerOutput>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCompilerOutput {
    path_sha256: String,
    #[serde(rename = "emit")]
    _emit: NativeCompilerEmit,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NativeCompilerEmit {
    Link,
    Metadata,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOutputRecord {
    path_sha256: String,
    size: u64,
    sha256: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeOutputCompiler {
    rustc_key: u64,
    #[serde(rename = "content_blake3")]
    _content_blake3: [u8; 32],
    #[serde(rename = "workspace")]
    _workspace: bool,
    #[serde(rename = "mode")]
    _mode: NativeOutputMode,
    #[serde(rename = "build_script")]
    _build_script: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NativeOutputMode {
    Build,
    Check,
    CheckTest,
    Test,
}

/// Recognize the producer's bounded metadata shape, without verifying output bytes,
/// trusting declared paths, authenticating execution or granting cleanup authority.
/// Legacy schemas carry no compiler-output coverage. Schema 3 only checks the
/// declared coverage shape; the producer's verifier binds it to its current plan.
fn native_output_metadata(bytes: &[u8], rustc: u64) -> bool {
    let digest = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    let outputs_valid = |outputs: &[NativeOutputRecord]| {
        !outputs.is_empty()
            && outputs.len() <= 4096
            && outputs.iter().all(|output| {
                output.size <= 8 * 1024 * 1024 * 1024
                    && digest(&output.path_sha256)
                    && digest(&output.sha256)
            })
    };
    if let Ok(metadata) = aterm_json::from_slice::<NativeOutputMetadataV1>(bytes) {
        return metadata.schema == 1 && outputs_valid(&metadata.outputs);
    }
    if let Ok(metadata) = aterm_json::from_slice::<NativeOutputMetadataV2>(bytes) {
        return metadata.schema == 2
            && metadata.compiler.rustc_key == rustc
            && outputs_valid(&metadata.outputs);
    }
    let Ok(metadata) = aterm_json::from_slice::<NativeOutputMetadataV3>(bytes) else {
        return false;
    };
    if metadata.schema != 3
        || metadata.compiler.rustc_key != rustc
        || !outputs_valid(&metadata.outputs)
        || metadata.compiler_outputs.is_empty()
        || metadata.compiler_outputs.len() > 4096
    {
        return false;
    }
    let inventory: BTreeSet<_> = metadata
        .outputs
        .iter()
        .map(|output| output.path_sha256.as_str())
        .collect();
    let mut covered = BTreeSet::new();
    metadata.compiler_outputs.iter().all(|output| {
        digest(&output.path_sha256)
            && inventory.contains(output.path_sha256.as_str())
            && covered.insert(output.path_sha256.as_str())
    })
}

/// The compiler every ordinary JSON in `dir` names (its leading `{"rustc":N`).
/// Native output metadata must have a readable ordinary companion and supported
/// shape; every other unknown or unreadable JSON still makes the compiler unknown.
fn compiler(fs: &dyn Fs, dir: &Path) -> Option<u64> {
    aterm_uds::progress::tick();
    let entries = fs.list(dir)?;
    let mut seen = None;
    let mut ordinary = BTreeMap::new();
    for (name, meta) in &entries {
        if !name.to_str().is_some_and(|n| n.ends_with(".json")) {
            continue;
        }
        if meta.as_ref()?.kind != Kind::File {
            return None;
        }
        if name.to_str()?.starts_with("outputs-") {
            continue;
        }
        let head = fs.head(&dir.join(name), FINGERPRINT_HEAD)?;
        let digits = head.strip_prefix(b"{\"rustc\":")?;
        let end = digits.iter().position(|c| !c.is_ascii_digit())?;
        let rustc: u64 = std::str::from_utf8(&digits[..end]).ok()?.parse().ok()?;
        if seen.is_some_and(|s| s != rustc) {
            return None;
        }
        seen = Some(rustc);
        ordinary.insert(name.clone(), rustc);
    }
    let mut remaining = OUTPUT_METADATA_BUDGET;
    for (name, _) in &entries {
        let Some(companion) = name
            .to_str()
            .filter(|name| name.ends_with(".json"))
            .and_then(|name| name.strip_prefix("outputs-"))
        else {
            continue;
        };
        let rustc = *ordinary.get(std::ffi::OsStr::new(companion))?;
        let bytes = fs.head(&dir.join(name), remaining)?;
        // A full-budget read cannot distinguish EOF from a longer file. Keep
        // this unit unknown, including at the exact conservative boundary.
        if bytes.len() >= remaining || !native_output_metadata(&bytes, rustc) {
            return None;
        }
        remaining -= bytes.len();
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

/// The one absolute path among eligible successful probe outputs in `.rustc_info.json`:
/// conventionally the `--print sysroot` line (the other lines are file names, cfgs and
/// `-vV`'s). `None` for none, or several. Failed stdout is cached too, but names no compiler.
/// This selects records with the producer's successful-output shape; it neither validates
/// all cache data nor authenticates the opaque process-hash key or the recorded stdout.
fn sysroot_named(info: &[u8]) -> Option<PathBuf> {
    let value: aterm_json::Value = aterm_json::from_slice(info).ok()?;
    let mut found = BTreeSet::new();
    for output in value.get("outputs")?.as_object()?.values() {
        if output.get("success").and_then(aterm_json::Value::as_bool) != Some(true)
            || output.get("code").and_then(aterm_json::Value::as_i64) != Some(0)
            || output.get("status").and_then(aterm_json::Value::as_str) != Some("")
            || output
                .get("stderr")
                .and_then(aterm_json::Value::as_str)
                .is_none()
        {
            continue;
        }
        let Some(stdout) = output.get("stdout").and_then(aterm_json::Value::as_str) else {
            continue;
        };
        #[cfg(unix)]
        let lines = {
            // Unix paths can contain CR: lines() would silently turn a reported
            // `/sdk\r` into a different `/sdk`. Ambiguous eligible output keeps
            // the whole inference unknown, even alongside a clean record.
            if stdout
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\n')
            {
                return None;
            }
            stdout.split('\n')
        };
        #[cfg(not(unix))]
        let lines = stdout.lines();
        for line in lines {
            if Path::new(line).is_absolute() {
                found.insert(PathBuf::from(line));
            }
        }
    }
    let mut found = found.into_iter();
    let sysroot = found.next()?;
    found.next().is_none().then_some(sysroot)
}

/// The hash of each unit of `package` whose output cargo copied out of the build directory `leaf`
/// to `file` under the target directory's `profile` ([`Used::add_uplifted`]): the unit
/// `<profile>/build/<package>/<hash>` whose `out/` holds the file's twin under its crate name
/// (each `-` an `_`), the same file or its copy ([`twins`]). None where no unit holds a twin.
#[must_use]
pub fn uplifted_units(leaf: &Path, profile: &str, package: &str, file: &Path) -> Vec<String> {
    if !plain(package) || !profile.split('/').all(plain) {
        return Vec::new();
    }
    let Some(stem) = file
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| plain(n))
        .map(|n| n.replace('-', "_"))
    else {
        return Vec::new();
    };
    let Ok(copy) = std::fs::symlink_metadata(file) else {
        return Vec::new();
    };
    let dir = leaf.join(profile).join("build").join(package);
    let Ok(units) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut hashes = Vec::new();
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
            hashes.push(hash);
        }
    }
    hashes
}

/// The most of one fingerprint's JSON [`SecondHashes`] reads:a unit's `deps` hold one short
/// entry for each dependency, some kilobytes for the widest crate. A longer one is not read.
const FINGERPRINT_MAX: usize = 1 << 20;

/// The most of a unit's hash file read ([`SecondHashes`]): sixteen hex digits.
const HASH_FILE_MAX: usize = 64;

/// The deepest chain of `deps` [`SecondHashes`] follows: a dependency graph's depth, with room.
const DEPTH_MAX: usize = 64;

/// The inputs of a fingerprint's JSON (targo's `Fingerprint`, `compiler/fingerprint/mod.rs`),
/// in the order [`SecondHashes`] compares them; any other field but `deps` and `local` after
/// them, by name, and `deps` last, as targo's own `compare` does: a unit's own input that moved
/// is its cause before a dependency's.
const INPUTS: [&str; 9] = [
    "rustc",
    "features",
    "declared_features",
    "target",
    "profile",
    "path",
    "rustflags",
    "config",
    "compile_kind",
];

/// WHAT MOVED between two builds of one unit ([`SecondHashes::second_hash`]): the package whose
/// own input differs (the unit's, or the dependency's the walk down `deps` reached) and which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The package whose input moved, as the build directory names it (`aterm-serde`).
    pub package: String,
    /// The input: a fingerprint field (`features`, `profile`, …, one targo adds by its name);
    /// `deps` where the edge itself moved (a dependency added or removed, another package under
    /// its name, its `public` flag, or a dependency's unit that is gone); `folder` where no
    /// input but the unit's own folder differs (`local`, which names it, alone); `unread` where
    /// a fingerprint could not be read.
    pub input: String,
    /// What it moved from and to, or which edge, in a few words.
    pub detail: String,
}

/// A UNIT A COMPILE BUILT BESIDE ANOTHER OF ITS PACKAGE, KIND ([`Unit::kind`]) AND SOURCE (its
/// fingerprint's `path`): the second hash, the unit it is compared with (the other of its kind
/// and source written last) and what moved between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondHash {
    pub package: String,
    pub kind: String,
    /// The unit the compile built.
    pub hash: String,
    /// The other unit of its package and kind it is compared with.
    pub other: String,
    pub moved: Moved,
}

/// The unit of the layout that keeps one folder per unit the output `file` of a compile into the
/// build directory `leaf` is in: `<profile>/build/<pkg>/<hash>/…` read as its profile (`debug`,
/// `aarch64-apple-darwin/debug`), package and hash; `None` for a file of the other layout or
/// outside `leaf`.
#[must_use]
pub fn folder_unit_of(leaf: &Path, file: &Path) -> Option<(String, String, String)> {
    let rel = file.strip_prefix(leaf).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| match c {
            std::path::Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let at = parts
        .iter()
        .position(|p| *p == "build")
        .filter(|at| *at > 0)?;
    let package = *parts.get(at + 1)?;
    let hash = *parts.get(at + 2)?;
    (plain(package) && unit_hash(package).is_none() && is_hash(hash) && parts.len() > at + 3)
        .then(|| (parts[..at].join("/"), package.to_owned(), hash.to_owned()))
}

/// THE SECOND-HASH READER of one profile's folder (`<leaf>/debug`): for a unit a compile built,
/// the other unit of its package, kind and source and the first input that moved between them
/// ([`Self::second_hash`]). It lists the unit's `build/<package>` folder, reads the
/// fingerprints' JSON, and to follow a `deps` entry down reads the hash file of each unit of the
/// profile, and of the host profile its host dependencies lie in ([`Self::with_host`]), once
/// ([`Self::by_hash`]): an entry holds the name the dependent uses (`serde` for the package
/// `aterm-serde`) and the hash the dependency's hash file holds, never its folder. It walks
/// nothing below a unit's fingerprint and removes nothing.
pub struct SecondHashes<'a> {
    fs: &'a dyn Fs,
    profile: PathBuf,
    /// The host profile's folder where `profile` is a cross target's (`<leaf>/debug` beside
    /// `<leaf>/<triple>/debug`): its proc-macros and build scripts are built there.
    host: Option<PathBuf>,
    /// Each unit's hash, as its hash file holds it, to its package and fingerprint JSON.
    index: std::cell::OnceCell<BTreeMap<String, (String, PathBuf)>>,
}

impl<'a> SecondHashes<'a> {
    #[must_use]
    pub fn new(fs: &'a dyn Fs, profile: &Path) -> Self {
        Self {
            fs,
            profile: profile.to_path_buf(),
            host: None,
            index: std::cell::OnceCell::new(),
        }
    }

    /// The reader of a cross target's profile whose host dependencies (proc-macros, build
    /// scripts) lie in the host profile's folder `host`: a `deps` entry is found in either.
    #[must_use]
    pub fn with_host(mut self, host: &Path) -> Self {
        if host != self.profile {
            self.host = Some(host.to_path_buf());
        }
        self
    }

    /// THE UNIT `hash` OF `package` IS A SECOND HASH where the profile holds another unit of its
    /// package and kind built from the same source (the same fingerprint `path`: another version
    /// of a registry crate is another package, not a second hash of it): compared with the one
    /// of those written last (by its fingerprint), the first input that moved, followed down
    /// `deps` to the dependency whose own input moved. `None` where it is the first unit of its
    /// kind and source, or its folder cannot be read; a unit whose fingerprint JSON cannot be
    /// read is compared with the other of its kind written last, its input `unread`, and another
    /// whose JSON cannot be read is passed over, its source unknown.
    #[must_use]
    pub fn second_hash(&self, package: &str, hash: &str) -> Option<SecondHash> {
        if !plain(package) || !is_hash(hash) {
            return None;
        }
        let units = package_units(self.fs, &self.profile.join("build").join(package), package);
        let built = units.iter().find(|u| u.hash == hash)?;
        let mut others: Vec<&Unit> = units
            .iter()
            .filter(|u| u.kind == built.kind && u.hash != built.hash)
            .collect();
        others
            .sort_by(|a, b| (b.fingerprint.newest, &b.hash).cmp(&(a.fingerprint.newest, &a.hash)));
        let (other, moved) = match self.json_of(&built.fingerprint.path) {
            Some(new) => others.into_iter().find_map(|other| {
                let old = self.json_of(&other.fingerprint.path)?;
                (old.get("path") == new.get("path"))
                    .then(|| (other, self.moved(package, &new, &old, 0)))
            })?,
            None => (
                *others.first()?,
                Moved {
                    package: package.to_owned(),
                    input: "unread".to_owned(),
                    detail: "a fingerprint's JSON could not be read".to_owned(),
                },
            ),
        };
        Some(SecondHash {
            package: package.to_owned(),
            kind: built.kind.clone(),
            hash: built.hash.clone(),
            other: other.hash.clone(),
            moved,
        })
    }

    /// The fingerprint JSON of the unit whose fingerprint folder is `dir`: its first ordinary
    /// `.json` by name (not a native output record, `outputs-…`).
    fn json_of(&self, dir: &Path) -> Option<aterm_json::Value> {
        let mut names: Vec<String> = self
            .fs
            .list(dir)?
            .into_iter()
            .filter(|(_, m)| m.is_some_and(|m| m.kind == Kind::File))
            .filter_map(|(n, _)| n.to_str().map(str::to_owned))
            .filter(|n| n.ends_with(".json") && !n.starts_with("outputs-"))
            .collect();
        names.sort();
        self.read_json(&dir.join(names.first()?))
    }

    /// The JSON object at `file`, read whole up to [`FINGERPRINT_MAX`].
    fn read_json(&self, file: &Path) -> Option<aterm_json::Value> {
        aterm_uds::progress::tick();
        let bytes = self.fs.head(file, FINGERPRINT_MAX + 1)?;
        (bytes.len() <= FINGERPRINT_MAX)
            .then(|| aterm_json::from_slice::<aterm_json::Value>(&bytes).ok())
            .flatten()
            .filter(aterm_json::Value::is_object)
    }

    /// Each unit of the profile and of its host profile ([`Self::with_host`]) by the hash its hash
    /// file holds (the hex of the fingerprint's hash, `lib-aterm_json` beside
    /// `lib-aterm_json.json`), read once: its package and its fingerprint JSON.
    fn by_hash(&self) -> &BTreeMap<String, (String, PathBuf)> {
        self.index.get_or_init(|| {
            let mut index = BTreeMap::new();
            let packages: Vec<PathBuf> = std::iter::once(&self.profile)
                .chain(self.host.as_ref())
                .flat_map(|profile| subdirs(self.fs, &profile.join("build")).collect::<Vec<_>>())
                .collect();
            for package in packages {
                let Some(name) = package.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                for folder in subdirs(self.fs, &package) {
                    let fingerprint = folder.join("fingerprint");
                    for (file, meta) in self.fs.list(&fingerprint).unwrap_or_default() {
                        let Some(file) = file.to_str() else {
                            continue;
                        };
                        if meta.is_none_or(|m| m.kind != Kind::File)
                            || file == "invoked.timestamp"
                            || file.starts_with("dep-")
                            || file.starts_with("output-")
                            || file.ends_with(".json")
                        {
                            continue;
                        }
                        let Some(head) = self.fs.head(&fingerprint.join(file), HASH_FILE_MAX)
                        else {
                            continue;
                        };
                        let Some(hex) = std::str::from_utf8(&head)
                            .ok()
                            .map(str::trim)
                            .filter(|h| is_hash(h))
                        else {
                            continue;
                        };
                        index.insert(
                            hex.to_owned(),
                            (name.to_owned(), fingerprint.join(format!("{file}.json"))),
                        );
                    }
                }
            }
            index
        })
    }

    /// The first input of `package` that moved from `old` to `new`, each a fingerprint's JSON:
    /// its own [`INPUTS`], then any other field but `deps` and `local` (the unit's own dep-info
    /// path, which names its folder and so differs in every pair), then the first `deps` entry
    /// that differs, followed down to the dependency whose own input moved.
    fn moved(
        &self,
        package: &str,
        new: &aterm_json::Value,
        old: &aterm_json::Value,
        depth: usize,
    ) -> Moved {
        let at = |input: &str, detail: String| Moved {
            package: package.to_owned(),
            input: input.to_owned(),
            detail,
        };
        let fields = |v: &aterm_json::Value| -> BTreeSet<String> {
            v.as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default()
        };
        let mut others: Vec<String> = fields(new)
            .union(&fields(old))
            .filter(|f| !INPUTS.contains(&f.as_str()) && *f != "deps" && *f != "local")
            .cloned()
            .collect();
        others.sort();
        for field in INPUTS.iter().map(|f| (*f).to_owned()).chain(others) {
            let (was, is) = (old.get(&field), new.get(&field));
            if was != is {
                return at(&field, format!("{} -> {}", shown(was), shown(is)));
            }
        }
        let new_deps = deps_of(new);
        let old_deps = deps_of(old);
        let mut matched = vec![false; old_deps.len()];
        for dep in &new_deps {
            let Some(i) =
                (0..old_deps.len()).find(|i| !matched[*i] && old_deps[*i].name == dep.name)
            else {
                return at("deps", format!("{} added", dep.name));
            };
            matched[i] = true;
            let was = &old_deps[i];
            if was.pkg_id != dep.pkg_id {
                return at("deps", format!("{}: another package", dep.name));
            }
            if was.public != dep.public {
                return at(
                    "deps",
                    format!("{}: public {} -> {}", dep.name, was.public, dep.public),
                );
            }
            if was.hash == dep.hash {
                continue;
            }
            if depth >= DEPTH_MAX {
                return at(
                    "deps",
                    format!("{}: deeper than {DEPTH_MAX} dependencies", dep.name),
                );
            }
            let index = self.by_hash();
            let (Some((to, new_json)), Some((_, old_json))) = (
                index.get(&hash_hex(dep.hash)),
                index.get(&hash_hex(was.hash)),
            ) else {
                return at("deps", format!("{}: a build of it is gone", dep.name));
            };
            let (Some(n), Some(o)) = (self.read_json(new_json), self.read_json(old_json)) else {
                return Moved {
                    package: to.clone(),
                    input: "unread".to_owned(),
                    detail: "a fingerprint's JSON could not be read".to_owned(),
                };
            };
            return self.moved(to, &n, &o, depth + 1);
        }
        if let Some(gone) = old_deps
            .iter()
            .zip(&matched)
            .find(|(_, m)| !**m)
            .map(|(d, _)| d)
        {
            return at("deps", format!("{} removed", gone.name));
        }
        at(
            "folder",
            "no input but the unit's own folder (`local`) differs".to_owned(),
        )
    }
}

/// One entry of a fingerprint's `deps` (targo writes `[pkg_id, name, public, hash]`).
struct Dep {
    pkg_id: Option<aterm_json::Value>,
    name: String,
    public: bool,
    hash: Option<u64>,
}

/// The `deps` of a fingerprint's JSON, in order; an entry of another shape is read as far as it
/// goes (no name is the empty one, no hash none), so it still compares.
fn deps_of(v: &aterm_json::Value) -> Vec<Dep> {
    v.get("deps")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|d| Dep {
            pkg_id: d.get_index(0).cloned(),
            name: d
                .get_index(1)
                .and_then(aterm_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            public: d
                .get_index(2)
                .and_then(aterm_json::Value::as_bool)
                .unwrap_or(false),
            hash: d.get_index(3).and_then(aterm_json::Value::as_u64),
        })
        .collect()
}

/// The hex a unit's hash file holds for the fingerprint hash `hash` (targo's `to_hex`: its
/// little-endian bytes); none for no hash.
fn hash_hex(hash: Option<u64>) -> String {
    hash.map(|h| h.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect())
        .unwrap_or_default()
}

/// A field's value in a few words: its JSON, cut at 120 characters; `absent` for none.
fn shown(v: Option<&aterm_json::Value>) -> String {
    let Some(v) = v else {
        return "absent".to_owned();
    };
    let text = aterm_json::to_string(v).unwrap_or_default();
    match text.char_indices().nth(120) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
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
