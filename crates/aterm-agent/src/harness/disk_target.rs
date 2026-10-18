// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A cargo build directory, measured, and the one thing in it the harness reclaims: an idle
//! profile's `incremental/`. Incremental state is the largest share of a debug build directory
//! and losing it costs only speed.
//!
//! `incremental/` is the only unit, by design. `deps/`, `build/` and `.fingerprint/` are
//! `targo clean`'s: cargo releases its locks before it runs tests, doctests or a runner, and
//! those read `deps/` and `build/`, so no lock proves them unused. Nothing is reached through
//! a symlink.
//!
//! One exception: below the floor, a build directory whose owner is gone goes whole
//! ([`reclaim_whole`]), where what is running stands in for the missing lock proof
//! (`super::discover::unused`).
//!
//! # Profiles and idleness
//!
//! A profile is a directory holding one of [`CARGO_LOCKS`] and cargo's layout, found at any
//! depth by one bounded walk that never follows a symlink or crosses devices; a truncated walk
//! disqualifies the whole directory. Nested surveyed build directories decide their own
//! profiles ([`reclaim_leaving`]).
//!
//! Last used is the newest mtime of [`CLOCK_DIRS`], never a lock's (a lock keeps its creation
//! time). A profile is idle after `disk.target_stale_days` whole days, and never within
//! [`PRESSURE_MIN_IDLE_S`]. A failing compile writes nothing into `deps/`, so a checkout whose
//! builds keep failing reads as last used at its last success; that costs speed only.
//!
//! Below the floor (`disk.auto_free_gib`) an idle window alone frees nothing when agents build
//! in every checkout daily, so [`reclaim_lru`] takes profiles inside the window too, one at a
//! time, least recently used first, while the caller re-measures (`super::apply_auto`). Such a
//! profile must, in the survey and again under its locks, be written no later than the survey
//! saw and not within [`PRESSURE_MIN_IDLE_S`].
//!
//! # Reclaiming one profile ([`reclaim`])
//!
//! Every step is relative to a directory handle: the build directory is opened component by
//! component from `/` with `O_NOFOLLOW`, and everything below is reached from that handle,
//! never leaving its device.
//!
//! 1. Take cargo's build locks exclusively without waiting (`flock` via `try_lock`): every
//!    existing lock first (one held means a build is running: the profile is skipped, nothing
//!    created), then each missing one, created only now. A lock that cannot be created skips
//!    the profile. A lock file is never deleted.
//! 2. Under the locks, re-derive: still a profile, still idle (under pressure: not written
//!    since the survey nor within [`PRESSURE_MIN_IDLE_S`]), `incremental/` still a real
//!    directory on the same device.
//! 3. Delete `incremental/` holding every lock, never following a symlink (a link is unlinked,
//!    its target kept) or entering another device. An entry that cannot be deleted is
//!    reported; the rest still go.
//! 4. Release the locks by `LOCK_UN` ([`HeldLock`]), never by close alone: a child being
//!    spawned holds copies of every descriptor until its exec.
//!
//! Freed bytes are the blocks of each deleted file that was its last name; a file that cannot
//! be opened to measure is deleted but not counted. An APFS clone's shared blocks count too, so
//! there the figure is an upper bound.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::discover::{Origin, Owned};
use super::{
    CACHEDIR_SIGNATURE, MAX_WALK_ENTRIES, RUSTC_INFO, Removed, Witness, age_days,
    cachedir_tag_signed, mtime_of, walk_bytes,
};
use held::HeldLock;

/// The build locks cargo takes in each profile: `.cargo-lock` (every cargo),
/// and since 1.97 `.cargo-build-lock` and `.cargo-artifact-lock`, each
/// `flock`-held for a whole compile. None is written at a build directory's
/// root.
pub const CARGO_LOCKS: &[&str] = &[".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// The one cache ever reclaimed: rustc's incremental-compilation state.
pub const INCREMENTAL: &str = "incremental";

/// A profile's cache directories: the walk never enters them.
pub const CACHE_DIRS: &[&str] = &[".fingerprint", "deps", "build", "incremental"];

/// What dates a profile: the directories a compile writes, and `examples/`.
/// Not `incremental/`, which the reclaim's own unlinks write.
pub const CLOCK_DIRS: &[&str] = &[".fingerprint", "deps", "build", "examples"];

/// Root entries that mark source. A build directory holding any of them at its root is not a
/// candidate.
pub const SOURCE_MARKERS: &[&str] = &[
    ".git",
    ".hg",
    ".jj",
    ".svn",
    "Cargo.toml",
    "Cargo.lock",
    "src",
    "build.rs",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// The deepest directory the profile walk enters, below the build
/// directory's root.
pub const MAX_PROFILE_DEPTH: usize = 12;

/// The profile walk's wall-clock budget.
pub const PROFILE_WALK_WALL: Duration = Duration::from_secs(10);

/// A profile a compile wrote into within this many seconds (ten minutes) is
/// never reclaimed, by either pass: long enough to cover the pause between an
/// agent's cargo runs, short enough to reach a checkout left alone briefly.
pub const PRESSURE_MIN_IDLE_S: i64 = 10 * 60;

/// The deepest tree the deletion descends; deeper is kept and reported,
/// rather than overflowing a stack.
#[cfg(unix)]
const MAX_DELETE_DEPTH: usize = 256;

/// The profile walk's bounds.
#[derive(Debug, Clone, Copy)]
pub struct WalkBudget {
    /// Directory entries it may read.
    pub entries: usize,
    /// Directory levels below the root it may enter.
    pub depth: usize,
    /// Wall clock it may spend.
    pub wall: Duration,
}

impl WalkBudget {
    /// The shipped bounds.
    pub const SHIPPED: WalkBudget = WalkBudget {
        entries: MAX_WALK_ENTRIES,
        depth: MAX_PROFILE_DEPTH,
        wall: PROFILE_WALK_WALL,
    };
}

/// One cargo profile inside a build directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Its path.
    pub path: PathBuf,
    /// Last used: the newest mtime of its [`CLOCK_DIRS`]. Never a lock's.
    pub last_used: Option<i64>,
    /// Does it hold a real (not linked) [`INCREMENTAL`] directory with
    /// anything in it? An empty one (a release profile's, a
    /// `CARGO_INCREMENTAL=0` build's) is nothing to reclaim.
    pub incremental: bool,
    /// Why it is in use right now, or `None` — a build lock someone holds.
    /// Probed on the report's candidates only; the reclaim decides again
    /// under the locks.
    pub in_use: Option<String>,
    /// Bytes deleting its `incremental/` would release: the blocks of each
    /// file there that is its last name, on the profile's device. Sized on
    /// the report's candidates only.
    pub bytes: u64,
    /// `true` when that walk stopped at its bound: the figure is a floor.
    pub bytes_partial: bool,
}

/// One build directory, with the facts that decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDir {
    /// Its canonical path: the reclaim reopens it component by component
    /// without following symlinks, so the report names the real one.
    pub path: PathBuf,
    /// The marker that proves a build tool laid it, when one was found.
    pub marker: Option<String>,
    /// The first [`SOURCE_MARKERS`] entry at its root, when there is one.
    pub holds_source: Option<String>,
    /// Why the profile walk did not read all of it, when it did not.
    pub walk_cut_short: Option<String>,
    /// Every cargo profile in it, at any depth.
    pub profiles: Vec<Profile>,
    /// The device it lives on ([`dev_of`]), when that could be read: the
    /// pressure pass takes only from the volume whose free space it measured.
    pub device: Option<u64>,
    /// Were its recent profiles also probed for locks and sized
    /// ([`assess_under`] under pressure)? Otherwise only idle ones are, and the
    /// pressure pass makes no row from the rest.
    pub probed_recent: bool,
    /// How it was found ([`Origin::Named`]: named by the caller).
    pub origin: Origin,
    /// Whether its owner is gone ([`super::discover::Owners`]).
    pub owned: Owned,
    /// Its whole size ([`total_bytes`]) and whether that is a floor, when it
    /// was sized: what the report's largest-hogs list reads.
    pub total: Option<(u64, bool)>,
}

impl TargetDir {
    /// The profiles a reclaim would take: idle past `threshold_days` at
    /// `now`, holding an `incremental/`, and not in use.
    #[must_use]
    pub fn candidates(&self, now: i64, threshold_days: i64) -> Vec<&Profile> {
        self.profiles
            .iter()
            .filter(|p| {
                p.incremental && p.in_use.is_none() && idle(p.last_used, now, threshold_days)
            })
            .collect()
    }

    /// Bytes a reclaim would release — the candidates' `incremental/` — and
    /// whether the figure is a floor.
    #[must_use]
    pub fn reclaimable(&self, now: i64, threshold_days: i64) -> (u64, bool) {
        self.candidates(now, threshold_days)
            .iter()
            .fold((0u64, false), |(b, partial), p| {
                (b.saturating_add(p.bytes), partial || p.bytes_partial)
            })
    }

    /// The profiles the pressure pass may take ([`reclaim_lru`]), and each
    /// other recent one with why it is kept. A candidate holds a non-empty
    /// `incremental/`, is inside the idle window (idle ones belong to the idle
    /// pass), has no build holding its locks, and was last written at least
    /// [`PRESSURE_MIN_IDLE_S`] before `now`. None when the directory fails its
    /// [`Self::fences`].
    #[must_use]
    pub fn pressure_candidates(
        &self,
        now: i64,
        threshold_days: i64,
    ) -> (Vec<&Profile>, Vec<(&Profile, String)>) {
        let mut picks = Vec::new();
        let mut kept = Vec::new();
        if self.fences().is_err() {
            return (picks, kept);
        }
        for p in &self.profiles {
            if !p.incremental || idle(p.last_used, now, threshold_days) {
                continue;
            }
            let Some(last) = p.last_used else {
                continue;
            };
            if let Some(why) = &p.in_use {
                kept.push((p, why.clone()));
                continue;
            }
            let age = now.saturating_sub(last);
            if age < PRESSURE_MIN_IDLE_S {
                kept.push((
                    p,
                    format!(
                        "written into {} ago; no reclaim takes a profile built in the last {}",
                        human_age(age),
                        human_age(PRESSURE_MIN_IDLE_S)
                    ),
                ));
                continue;
            }
            picks.push(p);
        }
        (picks, kept)
    }

    /// The fences a build directory must clear before anything in it is
    /// removed: a marker, no source at its root, every profile known.
    ///
    /// # Errors
    ///
    /// The first fence it fails, as the reason.
    pub fn fences(&self) -> Result<String, String> {
        if let Some(src) = &self.holds_source {
            return Err(format!("it holds {src} — source, never build output"));
        }
        let Some(marker) = self.marker.clone() else {
            return Err(format!(
                "no build-tool marker ({CACHEDIR_SIGNATURE} in CACHEDIR.TAG, or {RUSTC_INFO}) — the harness cannot prove it laid this"
            ));
        };
        if let Some(why) = &self.walk_cut_short {
            return Err(format!("its profiles are not all known: {why}"));
        }
        Ok(marker)
    }
}

/// A span of seconds a person can read: minutes under an hour, hours under a
/// day, whole days after. A negative span (a clock that moved) reads `0 min`.
#[must_use]
pub fn human_age(secs: i64) -> String {
    let s = secs.max(0);
    if s < 3_600 {
        format!("{} min", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3_600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// Is a profile last used at `last_used` idle at `now`: `threshold_days` whole
/// days old and not written within [`PRESSURE_MIN_IDLE_S`]? An unknown clock
/// is never idle.
fn idle(last_used: Option<i64>, now: i64, threshold_days: i64) -> bool {
    last_used.is_some_and(|m| {
        age_days(now, m) >= threshold_days && now.saturating_sub(m) >= PRESSURE_MIN_IDLE_S
    })
}

/// The witness for one build directory, or the reason it has none: the first refusal it
/// fails.
pub(super) fn target_witness(
    t: &TargetDir,
    now: i64,
    threshold_days: i64,
) -> Result<Witness, String> {
    let marker = t.fences()?;
    if t.profiles.is_empty() {
        return Err(
            "no cargo profile (a build lock beside deps/ or .fingerprint/) — cargo never built here"
                .to_owned(),
        );
    }
    let idle_ones: Vec<&Profile> = t
        .profiles
        .iter()
        .filter(|p| idle(p.last_used, now, threshold_days))
        .collect();
    if idle_ones.is_empty() {
        let newest = t.profiles.iter().filter_map(|p| p.last_used).max();
        if newest.is_some_and(|m| age_days(now, m) >= threshold_days) {
            return Err(format!(
                "every profile was written into within the last {}; no reclaim takes one that recent",
                human_age(PRESSURE_MIN_IDLE_S)
            ));
        }
        let newest = newest.map_or(0, |m| age_days(now, m));
        return Err(format!(
            "every profile was written into {newest}d ago or since, under the {threshold_days}d threshold"
        ));
    }
    if !idle_ones.iter().any(|p| p.incremental) {
        return Err("no idle profile holds an incremental/ cache — nothing to reclaim".to_owned());
    }
    let candidates = t.candidates(now, threshold_days);
    if candidates.is_empty() {
        return Err(idle_ones
            .iter()
            .find_map(|p| p.in_use.clone())
            .unwrap_or_else(|| "no idle profile can be reclaimed".to_owned()));
    }
    let idle_days = candidates
        .iter()
        .filter_map(|p| p.last_used)
        .map(|m| age_days(now, m))
        .min()
        .unwrap_or(threshold_days);
    Ok(Witness::StaleBuildDir {
        marker,
        profiles: candidates.len(),
        idle_days,
        threshold_days,
        leaves: Vec::new(),
    })
}

/// The build directory at `path`, assessed for the report: canonicalized,
/// its facts and its profiles (one bounded walk); then, only when it clears
/// its fences, the idle profiles' locks probed (never created) and their
/// `incremental/` sized. `None` when `path` is not a directory.
#[must_use]
pub fn assess(path: &Path, now: i64, threshold_days: i64, budget: WalkBudget) -> Option<TargetDir> {
    assess_under(path, now, threshold_days, false, budget)
}

/// [`assess`], and under `pressure` (free space below the floor) every profile's locks probed
/// and every `incremental/` sized, recent ones too.
#[must_use]
pub fn assess_under(
    path: &Path,
    now: i64,
    threshold_days: i64,
    pressure: bool,
    budget: WalkBudget,
) -> Option<TargetDir> {
    let path = std::fs::canonicalize(path).ok()?;
    let mut td = facts(&path, budget)?;
    if td.fences().is_ok() {
        td.probed_recent = pressure;
        for p in &mut td.profiles {
            let idle_cache = p.incremental && idle(p.last_used, now, threshold_days);
            // Under pressure every profile's locks are probed, one without an
            // `incremental/` too: a gone owner's build directory goes whole, and only
            // if no build holds any of them.
            if pressure || idle_cache {
                p.in_use = locks_held(&p.path);
            }
            if p.incremental && (pressure || idle_cache) {
                let dir = p.path.join(INCREMENTAL);
                let (bytes, partial) = cache_bytes(&dir, dev_of(&p.path));
                p.bytes = bytes;
                p.bytes_partial = partial;
            }
        }
    }
    Some(td)
}

/// The whole size of the build directory at `dir` — each file's allocated
/// blocks, shared evenly among its hard links (rustc links an uplifted
/// binary to its `deps/` twin, both inside), never through a symlink and
/// never onto another device — and whether the walk stopped at
/// [`MAX_WALK_ENTRIES`]. An estimate: what removing it whole would release,
/// and what the report's largest-hogs list says it holds.
#[must_use]
pub fn total_bytes(dir: &Path) -> (u64, bool) {
    let dev = dev_of(dir);
    walk_bytes(dir, |meta| same_dev(meta, dev), shared_blocks)
}

/// The blocks of the file `meta` describes, divided among its links.
fn shared_blocks(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        meta.blocks().saturating_mul(512) / meta.nlink().max(1)
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// The device the directory at `path` lives on, a link followed as the free-space figure
/// (statvfs) and the tick's volume filter follow it, when it can be read.
#[must_use]
pub fn dev_of(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// The facts of the build directory at the canonical `path` — marker,
/// source, profiles at any depth (bounded by `budget`) — or `None` when it is
/// not a directory. No size walk and no lock probe.
fn facts(path: &Path, budget: WalkBudget) -> Option<TargetDir> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let (marker, holds_source) = root_facts(path);
    let (found, cut) = fd::find(path, budget);
    let profiles = found
        .into_iter()
        .map(|p| Profile {
            last_used: profile_clock(&p),
            incremental: holds_cache(&p.join(INCREMENTAL)),
            path: p,
            in_use: None,
            bytes: 0,
            bytes_partial: false,
        })
        .collect();
    Some(TargetDir {
        path: path.to_path_buf(),
        marker,
        holds_source,
        walk_cut_short: cut,
        profiles,
        device: dev_of(path),
        probed_recent: false,
        origin: Origin::Named,
        owned: Owned::NotOwned,
        total: None,
    })
}

/// The build directory's root facts: the marker that proves a build tool
/// laid it (a signed `CACHEDIR.TAG`, else a plain `.rustc_info.json`), and
/// the first [`SOURCE_MARKERS`] entry at its root.
fn root_facts(path: &Path) -> (Option<String>, Option<String>) {
    let marker = if cachedir_tag_signed(&path.join("CACHEDIR.TAG")) {
        Some("CACHEDIR.TAG".to_owned())
    } else if std::fs::symlink_metadata(path.join(RUSTC_INFO)).is_ok_and(|m| m.is_file()) {
        Some(RUSTC_INFO.to_owned())
    } else {
        None
    };
    let holds_source = SOURCE_MARKERS
        .iter()
        .find(|m| std::fs::symlink_metadata(path.join(m)).is_ok())
        .map(|m| (*m).to_owned());
    (marker, holds_source)
}

/// Is `dir` a real (not linked) directory holding at least one entry?
fn holds_cache(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && std::fs::read_dir(dir).is_ok_and(|mut rd| rd.next().is_some())
}

/// A profile's last-used clock: the newest mtime of its [`CLOCK_DIRS`].
fn profile_clock(profile: &Path) -> Option<i64> {
    CLOCK_DIRS
        .iter()
        .filter_map(|c| mtime_of(&profile.join(c)))
        .max()
}

/// The one way this module takes a lock: [`HeldLock`]'s field is private to `held`, so
/// [`HeldLock::try_take`] is the module's only `try_lock` and the only way to hold what it
/// takes. No lock taken here can be released by close alone.
mod held {
    use std::io;

    /// One of cargo's build locks held by this process, released by `LOCK_UN`
    /// on drop. An `flock(2)` lives on the open file description, which a child
    /// mid-spawn keeps until its exec, so releasing by close alone would leave
    /// it held there.
    pub(super) struct HeldLock(std::fs::File);

    impl HeldLock {
        /// Take `file`'s `flock` exclusively without waiting. `Ok(None)`:
        /// someone holds it.
        pub(super) fn try_take(file: std::fs::File) -> io::Result<Option<HeldLock>> {
            // The ascription is the lock-order census's File evidence
            // (aterm-census `is_file_binding_rhs`), keeping this site
            // categorized as a cross-process advisory lock.
            let file: std::fs::File = file;
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => Ok(Some(HeldLock(file))),
                Err(std::fs::TryLockError::WouldBlock) => Ok(None),
                Err(std::fs::TryLockError::Error(e)) => Err(e),
            }
        }
    }

    impl Drop for HeldLock {
        fn drop(&mut self) {
            let _ = self.0.unlock();
        }
    }
}

/// A test's build: one of cargo's locks held as a build holds it, released by `LOCK_UN` on
/// drop (unwind included), so no sibling test's spawned child keeps it held. Every test that
/// simulates a build takes its lock through this or [`HeldLock`].
#[cfg(test)]
pub(crate) struct Build {
    _lock: HeldLock,
}

#[cfg(test)]
impl Build {
    /// A build takes the lock at `lock` (which exists). Panics when it
    /// cannot: someone else holds it.
    pub(crate) fn holds(lock: &Path) -> Build {
        let file = std::fs::File::open(lock).expect("the lock exists");
        let lock = HeldLock::try_take(file)
            .expect("the lock can be taken")
            .expect("the test's build takes the lock");
        Build { _lock: lock }
    }
}

/// Why a build holds one of `profile`'s locks right now, or `None`. Each
/// existing lock is taken exclusively without waiting and released at once by
/// `LOCK_UN` ([`HeldLock`]); a missing one is not created (a report writes
/// nothing), and a lock that is a symlink or cannot be probed reads as held.
fn locks_held(profile: &Path) -> Option<String> {
    locks_held_with(profile, &mut |_| {})
}

/// [`locks_held`] with a hook told each lock's path while the probe holds it
/// (a test's).
fn locks_held_with(profile: &Path, hook: &mut dyn FnMut(&Path)) -> Option<String> {
    for lock in CARGO_LOCKS {
        let path = profile.join(lock);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return Some(format!("{} is not a plain file", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Some(format!("{} could not be probed: {e}", path.display())),
        }
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => return Some(format!("{} could not be probed: {e}", path.display())),
        };
        match HeldLock::try_take(file) {
            // Taken: released at once, by `LOCK_UN`.
            Ok(Some(held)) => {
                hook(&path);
                drop(held);
            }
            Ok(None) => return Some(format!("a build holds {}", path.display())),
            Err(e) => return Some(format!("{} could not be probed: {e}", path.display())),
        }
    }
    None
}

/// Bytes deleting the tree at `dir` would release: the blocks of each file
/// that is its last name, never following a symlink, never entering another
/// device than `dev` — and whether the walk stopped at [`MAX_WALK_ENTRIES`].
/// A report's estimate; the reclaim counts what it actually unlinked.
#[must_use]
pub fn cache_bytes(dir: &Path, dev: Option<u64>) -> (u64, bool) {
    walk_bytes(dir, |meta| same_dev(meta, dev), released)
}

/// Is `meta` on the device `dev` (any, when `dev` is unknown)?
fn same_dev(meta: &std::fs::Metadata, dev: Option<u64>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        dev.is_none_or(|d| meta.dev() == d)
    }
    #[cfg(not(unix))]
    {
        let _ = (meta, dev);
        true
    }
}

/// What unlinking the file `meta` describes releases: its allocated blocks
/// when this is its last name, else nothing.
fn released(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() == 1 {
            meta.blocks().saturating_mul(512)
        } else {
            0
        }
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

// ---------------------------------------------------------------------------
// Reclaim
// ---------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    static FAULT_CREATE_LOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's fault: on this thread, creating a missing lock fails as a full
/// disk makes it fail (ENOSPC).
#[cfg(test)]
pub(crate) fn inject_create_lock_fault(on: bool) {
    FAULT_CREATE_LOCK.with(|f| f.set(on));
}

#[cfg(all(test, unix))]
fn fault_create_lock() -> bool {
    FAULT_CREATE_LOCK.with(std::cell::Cell::get)
}

/// What a reclaim judges each profile by, under that profile's locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Judge {
    /// Unix seconds.
    pub now: i64,
    /// The stale window, in days.
    pub threshold_days: i64,
    /// The device whose free space the caller measured: a build directory on
    /// any other is refused, since reclaiming it frees nothing there. `None`:
    /// any (the hand-run verb, which names its directories).
    pub device: Option<u64>,
}

/// Where a reclaim of one profile stands, for a test's hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Every lock just taken; nothing re-derived under them yet.
    Locked,
    /// Under the locks, re-derived; the delete about to start.
    Deleting,
    /// The delete done and the locks released.
    Released,
}

/// Reclaim one build directory's idle `incremental/` caches: the one removal
/// primitive of [`super::Class::CargoTargets`], for the host's tick and the
/// hand-run verb ([`super::remover`]). The module docs give the steps.
///
/// # Errors
///
/// The path is not absolute, a component is a symlink, or it is not a
/// directory; it is on another device than `judge.device`; it no longer clears
/// its fences; or not one byte was reclaimed (each profile's reason is in the
/// message).
pub fn reclaim(path: &Path, judge: &Judge) -> io::Result<Removed> {
    reclaim_with(path, judge, &mut |_, _| {})
}

/// [`reclaim`], leaving every profile inside one of `leave`: build directories
/// named inside this one, which decide their own profiles. Such a profile is
/// passed over like a non-idle one, its locks never taken.
///
/// # Errors
///
/// As [`reclaim`].
pub fn reclaim_leaving(path: &Path, judge: &Judge, leave: &[PathBuf]) -> io::Result<Removed> {
    fd::reclaim(path, judge, Select::Idle { leave }, &mut |_, _| {})
}

/// [`reclaim`] with a hook told each [`Stage`] of each profile (a test's
/// probe).
///
/// # Errors
///
/// As [`reclaim`].
pub fn reclaim_with(
    path: &Path,
    judge: &Judge,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim(path, judge, Select::Idle { leave: &[] }, hook)
}

/// Under pressure, reclaim one profile's `incremental/`: the profile at
/// `profile` in the build directory at `path`, idle or not, as the pressure
/// pass picks them. The same steps as [`reclaim`]; under the locks it is
/// re-derived as still one of this build directory's profiles, written no
/// later than `seen` (the survey's last used) and not within
/// [`PRESSURE_MIN_IDLE_S`] of `judge.now`.
///
/// # Errors
///
/// As [`reclaim`]; and the profile is no longer one of the build directory's,
/// or it was not reclaimed (the reason is in the message).
pub fn reclaim_lru(path: &Path, profile: &Path, judge: &Judge, seen: i64) -> io::Result<Removed> {
    reclaim_lru_with(path, profile, judge, seen, &mut |_, _| {})
}

/// [`reclaim_lru`] with a hook told each [`Stage`] (a test's probe).
///
/// # Errors
///
/// As [`reclaim_lru`].
pub fn reclaim_lru_with(
    path: &Path,
    profile: &Path,
    judge: &Judge,
    seen: i64,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim(path, judge, Select::Lru { profile, seen }, hook)
}

/// Below the floor, remove a build directory whose owner is gone, whole
/// (`super::Witness::OwnerGone`). The same discipline as [`reclaim`], and more: a signed
/// `CACHEDIR.TAG`; at least one profile, every lock of every one taken (a held one refuses
/// the whole directory); under them, the profiles found again (one made since holds a lock
/// this reclaim never took), each written no later than `seen` (the survey's newest last
/// used) and not within `quiet_s` of `judge.now`, and the caller's `check` asked
/// (`super::discover::recheck`). The contents are deleted through the handle pinned at the
/// start, cargo's lock files last in each directory so a cargo starting meanwhile waits on
/// them; the emptied directory goes by name only if the name still names it. The bytes are
/// what the unlinks released.
///
/// # Errors
///
/// As [`reclaim`]; the marker is not a signed `CACHEDIR.TAG`; it holds no
/// profile; a build holds a lock; a profile appeared since the first walk;
/// a profile was written into since the survey or inside the quiet window;
/// `check` refused; or nothing was removed.
pub fn reclaim_whole(
    path: &Path,
    judge: &Judge,
    seen: i64,
    quiet_s: i64,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> io::Result<Removed> {
    reclaim_whole_with(path, judge, seen, quiet_s, check, &mut |_, _| {})
}

/// [`reclaim_whole`] with a hook told each [`Stage`] (a test's probe; the
/// path is the build directory's).
///
/// # Errors
///
/// As [`reclaim_whole`].
pub fn reclaim_whole_with(
    path: &Path,
    judge: &Judge,
    seen: i64,
    quiet_s: i64,
    check: &mut dyn FnMut() -> Result<(), String>,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim_whole(path, judge, seen, quiet_s, check, hook)
}

/// Which profiles one reclaim of a build directory takes.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix reclaim reads a selection")
)]
enum Select<'a> {
    /// Every idle one — past `judge.threshold_days` — but those inside one
    /// of `leave`, the build directories named inside this one.
    Idle {
        /// The build directories whose profiles are left to their own rows.
        leave: &'a [PathBuf],
    },
    /// Under pressure, the one at `profile`, last written at `seen` when the
    /// survey read it.
    Lru {
        /// The profile.
        profile: &'a Path,
        /// Its last used as the survey read it.
        seen: i64,
    },
}

#[cfg(unix)]
impl Select<'_> {
    /// Why a profile last written at `clock` is not taken under this
    /// selection at `judge.now`, or `None` when it is.
    fn refuses(self, clock: Option<i64>, judge: &Judge) -> Option<String> {
        match self {
            Select::Idle { .. } => {
                if idle(clock, judge.now, judge.threshold_days) {
                    return None;
                }
                Some(match clock {
                    None => "its caches' clock could not be read".to_owned(),
                    Some(m) if age_days(judge.now, m) >= judge.threshold_days => format!(
                        "no longer idle: written into {} ago; no reclaim takes a profile built in the last {}",
                        human_age(judge.now.saturating_sub(m)),
                        human_age(PRESSURE_MIN_IDLE_S)
                    ),
                    Some(m) => format!(
                        "no longer idle: written into {}d ago, under the {}d threshold",
                        age_days(judge.now, m),
                        judge.threshold_days
                    ),
                })
            }
            Select::Lru { seen, .. } => {
                let Some(m) = clock else {
                    return Some("its caches' clock could not be read".to_owned());
                };
                if m > seen {
                    return Some(
                        "written into since the survey: a build is using it, so it is no longer the least recently used"
                            .to_owned(),
                    );
                }
                let age = judge.now.saturating_sub(m);
                (age < PRESSURE_MIN_IDLE_S).then(|| {
                    format!(
                        "written into {} ago; no reclaim takes a profile built in the last {}",
                        human_age(age),
                        human_age(PRESSURE_MIN_IDLE_S)
                    )
                })
            }
        }
    }
}

/// The fd-relative half: the profile walk, the locks, the deletion.
#[cfg(unix)]
mod fd {
    use super::{
        CACHE_DIRS, CARGO_LOCKS, CLOCK_DIRS, HeldLock, INCREMENTAL, Judge, MAX_DELETE_DEPTH, Path,
        PathBuf, Removed, Select, Stage, TargetDir, WalkBudget, io, root_facts,
    };
    use aterm_dirfd::{self as dirfd, AtFlags, Dir, Errno, Mode, OFlags, OwnedFd};
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt as _;
    use std::time::Instant;

    fn dir_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
    }

    // -- the device seam: a test marks a directory "on another device" ------

    #[cfg(test)]
    thread_local! {
        static FOREIGN: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Mark the directory at `path` as on another device, on this thread (a
    /// test's).
    #[cfg(test)]
    pub(crate) fn mark_foreign(path: &Path) {
        use std::os::unix::fs::MetadataExt as _;
        let ino = std::fs::symlink_metadata(path).expect("marked").ino();
        FOREIGN.with(|f| f.borrow_mut().push(ino));
    }

    /// The device of the open `fd`.
    fn dev(fd: &OwnedFd) -> io::Result<u64> {
        let st = dirfd::fstat(fd)?;
        #[cfg(test)]
        if FOREIGN.with(|f| f.borrow().contains(&st.st_ino)) {
            return Ok(st.st_dev.wrapping_add(1));
        }
        Ok(st.st_dev)
    }

    /// Open the directory at the absolute `path` component by component from
    /// `/`, never following a symlink: a component that is (or became) a link
    /// refuses the whole path.
    pub(super) fn pin(path: &Path) -> io::Result<OwnedFd> {
        if !path.is_absolute() {
            return Err(io::Error::other("not an absolute path"));
        }
        let mut fd = dirfd::openat(dirfd::CWD, "/", dir_flags(), Mode::empty())?;
        for part in path.components() {
            match part {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(name) => {
                    // A link answers ELOOP, or ENOTDIR where `O_DIRECTORY`
                    // is checked first (macOS): either way it is refused.
                    fd = dirfd::openat(&fd, name, dir_flags(), Mode::empty()).map_err(|e| {
                        if e.0 == libc::ELOOP || e.0 == libc::ENOTDIR {
                            io::Error::other(format!(
                                "refusing {}: a symlink or not a directory (nothing is reached through a link)",
                                name.to_string_lossy()
                            ))
                        } else {
                            e.into()
                        }
                    })?;
                }
                _ => return Err(io::Error::other("not a plain absolute path")),
            }
        }
        Ok(fd)
    }

    /// A build directory the survey measured that is no longer there —
    /// something else removed it, or the folder holding it, since — said as
    /// that (`NotFound`); any other error as it is.
    fn gone_since(e: io::Error) -> io::Error {
        if e.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no longer a candidate: it is gone (something else removed it since the survey)",
            )
        } else {
            e
        }
    }

    /// `name` under `parent` as a directory handle, `None` when it is not a
    /// directory (a file, a symlink — never followed) or is gone.
    fn open_dir(parent: &OwnedFd, name: &OsStr) -> io::Result<Option<OwnedFd>> {
        match dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
            Ok(fd) => Ok(Some(fd)),
            Err(e) if [libc::ENOTDIR, libc::ELOOP, libc::ENOENT].contains(&e.0) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The names in a directory, `.` and `..` left out.
    fn names(fd: &OwnedFd) -> io::Result<Vec<OsString>> {
        let mut out = Vec::new();
        for entry in Dir::read_from(fd)? {
            let entry = entry?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name != "." && name != ".." {
                out.push(name.to_os_string());
            }
        }
        Ok(out)
    }

    /// `name` under `parent`, opened without following it and without
    /// blocking — a plain file's handle, or `None` for a symlink, a
    /// directory, a socket or anything else that is not one.
    fn open_file(parent: &OwnedFd, name: &OsStr) -> Option<std::fs::File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let file = std::fs::File::from(dirfd::openat(parent, name, flags, Mode::empty()).ok()?);
        file.metadata().ok()?.is_file().then_some(file)
    }

    /// Does this directory hold one of cargo's build locks, as a plain file?
    fn holds_lock(fd: &OwnedFd, names: &[OsString]) -> bool {
        CARGO_LOCKS
            .iter()
            .any(|l| names.iter().any(|n| n == l) && open_file(fd, OsStr::new(l)).is_some())
    }

    /// Does it hold cargo's layout?
    fn has_layout(fd: &OwnedFd, names: &[OsString]) -> bool {
        ["deps", ".fingerprint"].iter().any(|d| {
            names.iter().any(|n| n == d) && matches!(open_dir(fd, OsStr::new(d)), Ok(Some(_)))
        })
    }

    struct Walker {
        budget: WalkBudget,
        started: Instant,
        entries: usize,
        dev: u64,
        cut: Option<String>,
        found: Vec<PathBuf>,
    }

    impl Walker {
        fn visit(&mut self, fd: &OwnedFd, path: &Path, depth: usize) {
            if self.cut.is_some() {
                return;
            }
            if self.started.elapsed() >= self.budget.wall {
                self.cut = Some("its walk ran out of time".to_owned());
                return;
            }
            let names = match names(fd) {
                Ok(n) => n,
                Err(e) => {
                    self.cut = Some(format!("{} could not be read: {e}", path.display()));
                    return;
                }
            };
            self.entries = self.entries.saturating_add(names.len());
            if self.entries > self.budget.entries {
                self.cut = Some(format!(
                    "it holds more than {} entries outside its caches",
                    self.budget.entries
                ));
                return;
            }
            let profile = holds_lock(fd, &names) && has_layout(fd, &names);
            if profile {
                self.found.push(path.to_path_buf());
            }
            for name in &names {
                if profile && CACHE_DIRS.iter().any(|c| name == c) {
                    continue;
                }
                let child = match open_dir(fd, name) {
                    Ok(Some(c)) => c,
                    Ok(None) => continue,
                    Err(e) => {
                        self.cut = Some(format!(
                            "{} could not be opened: {e}",
                            path.join(name).display()
                        ));
                        return;
                    }
                };
                // Never onto another device: nothing on it is this build
                // directory's.
                if dev(&child).ok() != Some(self.dev) {
                    continue;
                }
                if depth + 1 > self.budget.depth {
                    self.cut = Some(format!("it nests deeper than {} levels", self.budget.depth));
                    return;
                }
                self.visit(&child, &path.join(name), depth + 1);
                if self.cut.is_some() {
                    return;
                }
            }
        }
    }

    /// Walk the build directory at the canonical `path` for its profiles.
    pub(super) fn find(path: &Path, budget: WalkBudget) -> (Vec<PathBuf>, Option<String>) {
        let root = match pin(path) {
            Ok(fd) => fd,
            Err(e) => return (Vec::new(), Some(format!("it could not be opened: {e}"))),
        };
        find_at(&root, path, budget)
    }

    fn find_at(root: &OwnedFd, path: &Path, budget: WalkBudget) -> (Vec<PathBuf>, Option<String>) {
        let Ok(root_dev) = dev(root) else {
            return (Vec::new(), Some("its device could not be read".to_owned()));
        };
        let mut w = Walker {
            budget,
            started: Instant::now(),
            entries: 0,
            dev: root_dev,
            cut: None,
            found: Vec::new(),
        };
        w.visit(root, path, 0);
        (w.found, w.cut)
    }

    /// Re-open a profile the walk found, component by component from the
    /// build directory's own handle, never following a symlink and never
    /// leaving `on`.
    fn reopen(root: &OwnedFd, root_path: &Path, path: &Path, on: u64) -> io::Result<OwnedFd> {
        let rel = path
            .strip_prefix(root_path)
            .map_err(|_| io::Error::other("outside the build directory"))?;
        let mut fd = dirfd::openat(root, ".", dir_flags(), Mode::empty())?;
        for part in rel.components() {
            let std::path::Component::Normal(name) = part else {
                return Err(io::Error::other("not a plain path"));
            };
            fd = open_dir(&fd, name)?.ok_or_else(|| io::Error::other("no longer a directory"))?;
            if dev(&fd)? != on {
                return Err(io::Error::other("on another device"));
            }
        }
        Ok(fd)
    }

    /// Take the lock `name` under `dir` exclusively without waiting; `create`
    /// makes it when absent, `0666` under the umask as cargo does (a private
    /// `0600` would lock group members out of a shared build directory).
    /// `Ok(None)`: someone holds it.
    fn take_lock(dir: &OwnedFd, name: &str, create: bool) -> io::Result<Option<HeldLock>> {
        let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if create {
            flags |= OFlags::CREATE;
            #[cfg(test)]
            if super::fault_create_lock() {
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }
        }
        let mode = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH;
        let file = std::fs::File::from(dirfd::openat(dir, name, flags, mode)?);
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("not a plain file"));
        }
        HeldLock::try_take(file)
    }

    /// Every one of cargo's locks in the profile `dir` (at `path`), held —
    /// the ones that exist first, then the missing ones created. `Err` is
    /// why the profile is skipped; nothing is created before every existing
    /// lock is taken.
    fn take_all(dir: &OwnedFd, path: &Path) -> Result<Vec<HeldLock>, String> {
        let mut held = Vec::with_capacity(CARGO_LOCKS.len());
        let mut missing = Vec::new();
        for lock in CARGO_LOCKS {
            match take_lock(dir, lock, false) {
                Ok(Some(file)) => held.push(file),
                Ok(None) => return Err(format!("a build holds {}", path.join(lock).display())),
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => missing.push(*lock),
                Err(e) => {
                    return Err(format!(
                        "{} could not be taken: {e}",
                        path.join(lock).display()
                    ));
                }
            }
        }
        for lock in missing {
            match take_lock(dir, lock, true) {
                Ok(Some(file)) => held.push(file),
                Ok(None) => return Err(format!("a build holds {}", path.join(lock).display())),
                Err(e) => {
                    return Err(format!(
                        "{} could not be created: {e} — nothing is removed without all of cargo's locks",
                        path.join(lock).display()
                    ));
                }
            }
        }
        Ok(held)
    }

    /// Does the profile hold a real `incremental/` with anything in it?
    fn holds_cache_at(profile: &OwnedFd) -> bool {
        matches!(open_dir(profile, OsStr::new(INCREMENTAL)), Ok(Some(inc)) if names(&inc).is_ok_and(|n| !n.is_empty()))
    }

    /// The profile's clock read through its handle.
    fn clock_at(profile: &OwnedFd) -> Option<i64> {
        let mtime = |fd: OwnedFd| {
            std::fs::File::from(fd)
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok())
        };
        CLOCK_DIRS
            .iter()
            .filter_map(|c| open_dir(profile, OsStr::new(c)).ok().flatten())
            .filter_map(mtime)
            .max()
    }

    /// What a delete released, and every reason part of it stayed.
    #[derive(Default)]
    struct Deleted {
        bytes: u64,
        kept: Vec<String>,
    }

    /// The one deletion: `name` under `parent`. A directory is emptied and
    /// removed, never followed or entered on another device than `on`; anything
    /// else is unlinked, its blocks counted only after the unlink succeeded and
    /// only when it was the file's last name. A failure is recorded and the
    /// rest still tried; a directory that kept anything is not removed.
    fn remove_tree_at(
        parent: &OwnedFd,
        name: &OsStr,
        at: &Path,
        on: u64,
        depth: usize,
        out: &mut Deleted,
    ) {
        let here = at.join(name);
        let dir = match dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
            Ok(fd) => Some(fd),
            Err(Errno::NOENT) => return,
            // A symlink (never followed), a file, or anything that is not a
            // directory: it is unlinked below.
            Err(e) if [libc::ENOTDIR, libc::ELOOP].contains(&e.0) => None,
            Err(e) => {
                return out
                    .kept
                    .push(format!("{}: {}", here.display(), io::Error::from(e)));
            }
        };
        let Some(fd) = dir else {
            let freed = open_file(parent, name)
                .and_then(|f| f.metadata().ok())
                .map_or(0, |m| super::released(&m));
            match dirfd::unlinkat(parent, name, AtFlags::empty()) {
                Ok(()) => out.bytes = out.bytes.saturating_add(freed),
                Err(Errno::NOENT) => {}
                Err(e) => out
                    .kept
                    .push(format!("{}: {}", here.display(), io::Error::from(e))),
            }
            return;
        };
        if dev(&fd).ok() != Some(on) {
            return out.kept.push(format!(
                "{} is on another device and was not entered",
                here.display()
            ));
        }
        if depth >= MAX_DELETE_DEPTH {
            return out
                .kept
                .push(format!("{} is too deep to delete here", here.display()));
        }
        let kept = out.kept.len();
        match names(&fd) {
            Ok(mut children) => {
                // cargo's lock files last: a cargo starting meanwhile waits on a lock
                // still held, rather than on a fresh file it made.
                children.sort_by_key(|c| CARGO_LOCKS.iter().any(|l| c == l));
                for child in children {
                    remove_tree_at(&fd, &child, &here, on, depth + 1, out);
                }
            }
            Err(e) => out.kept.push(format!("{}: {e}", here.display())),
        }
        match dirfd::unlinkat(parent, name, AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => {}
            // Not empty because a child was kept: that child's reason is the
            // one on record.
            Err(e) if (e.0 == libc::ENOTEMPTY || e.0 == libc::EEXIST) && out.kept.len() > kept => {}
            Err(e) => out
                .kept
                .push(format!("{}: {}", here.display(), io::Error::from(e))),
        }
    }

    /// What reclaiming one profile did: the bytes released, whether its
    /// `incremental/` is gone, and what stayed.
    struct Profiled {
        bytes: u64,
        gone: bool,
        kept: Vec<String>,
    }

    /// Reclaim one profile, reached by its handle: the module docs' steps.
    fn reclaim_profile(
        profile: &OwnedFd,
        path: &Path,
        on: u64,
        judge: &Judge,
        select: Select<'_>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> Result<Profiled, String> {
        // 1. Every lock, exclusively, without waiting.
        let held = take_all(profile, path)?;
        hook(Stage::Locked, path);
        // 2. Re-derive under the locks.
        let names = names(profile).map_err(|e| format!("it could not be read: {e}"))?;
        if !has_layout(profile, &names) {
            return Err("no longer a cargo profile".to_owned());
        }
        if let Some(why) = select.refuses(clock_at(profile), judge) {
            return Err(why);
        }
        let inc = OsStr::new(INCREMENTAL);
        match open_dir(profile, inc) {
            Ok(Some(fd)) if dev(&fd).ok() == Some(on) => {
                if self::names(&fd).is_ok_and(|n| n.is_empty()) {
                    return Err("its incremental/ is empty — nothing to reclaim".to_owned());
                }
            }
            Ok(Some(_)) => return Err("its incremental/ is on another device".to_owned()),
            Ok(None) => return Err("it no longer holds a real incremental/".to_owned()),
            Err(e) => return Err(format!("its incremental/ could not be opened: {e}")),
        }
        hook(Stage::Deleting, path);
        // 3. Delete it, every lock still held.
        let mut deleted = Deleted::default();
        remove_tree_at(profile, inc, path, on, 0, &mut deleted);
        let gone = matches!(open_dir(profile, inc), Ok(None));
        // 4. Release the locks: `LOCK_UN`, each ([`HeldLock`]).
        drop(held);
        hook(Stage::Released, path);
        Ok(Profiled {
            bytes: deleted.bytes,
            gone,
            kept: deleted.kept,
        })
    }

    /// Remove the build directory at `path` whole ([`super::reclaim_whole`]).
    pub(super) fn reclaim_whole(
        path: &Path,
        judge: &Judge,
        seen: i64,
        quiet_s: i64,
        check: &mut dyn FnMut() -> Result<(), String>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::other("not a plain absolute path"));
        };
        let parent_fd = pin(parent).map_err(gone_since)?;
        let root = match dirfd::openat(&parent_fd, name, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(e) if e.0 == libc::ENOENT => return Err(gone_since(e.into())),
            Err(e) if e.0 == libc::ENOTDIR || e.0 == libc::ELOOP => {
                return Err(io::Error::other(
                    "no longer a candidate: not a directory (nothing is reached through a link)",
                ));
            }
            Err(e) => return Err(e.into()),
        };
        let root_dev = dev(&root)?;
        if judge.device.is_some_and(|d| d != root_dev) {
            return Err(io::Error::other(
                "no longer a candidate: it is on another volume than the one measured",
            ));
        }
        let refuse = |why: String| io::Error::other(format!("no longer a candidate: {why}"));
        let (marker, holds_source) = root_facts(path);
        let (profiles, cut) = find_at(&root, path, WalkBudget::SHIPPED);
        let marker = TargetDir {
            path: path.to_path_buf(),
            marker,
            holds_source,
            walk_cut_short: cut,
            profiles: Vec::new(),
            device: Some(root_dev),
            probed_recent: false,
            origin: super::Origin::Named,
            owned: super::Owned::NotOwned,
            total: None,
        }
        .fences()
        .map_err(refuse)?;
        if marker != "CACHEDIR.TAG" {
            return Err(refuse(format!(
                "only a signed CACHEDIR.TAG lets a whole build directory go, and it carries {marker}"
            )));
        }
        if profiles.is_empty() {
            return Err(refuse("it holds no cargo profile".to_owned()));
        }
        // 1. Every lock of every profile, exclusively, without waiting.
        let mut held = Vec::new();
        for profile in &profiles {
            let fd = reopen(&root, path, profile, root_dev).map_err(|e| refuse(e.to_string()))?;
            held.extend(take_all(&fd, profile).map_err(refuse)?);
            // 2. Re-dated under its locks.
            let Some(clock) = clock_at(&fd) else {
                return Err(refuse(format!(
                    "{}: its caches' clock could not be read",
                    profile.display()
                )));
            };
            if clock > seen {
                return Err(refuse(format!(
                    "{} was written into since the survey",
                    profile.display()
                )));
            }
            let age = judge.now.saturating_sub(clock);
            if age < quiet_s {
                return Err(refuse(format!(
                    "{} was written into {} ago",
                    profile.display(),
                    super::human_age(age)
                )));
            }
        }
        hook(Stage::Locked, path);
        // Under the locks: the profiles found again — one made since the
        // first walk holds a lock this reclaim never took.
        let (mut again, cut) = find_at(&root, path, WalkBudget::SHIPPED);
        let mut before = profiles.clone();
        again.sort();
        before.sort();
        if cut.is_some() || again != before {
            return Err(refuse(
                "its profiles changed since they were locked (a build started a new one)"
                    .to_owned(),
            ));
        }
        // The caller's own checks, under the locks: the owner still gone,
        // nothing running from it.
        check().map_err(refuse)?;
        // The name still names the directory pinned.
        let same = |fd: &OwnedFd| dirfd::fstat(fd).map(|st| (st.st_dev, st.st_ino));
        let named = |parent: &OwnedFd| -> io::Result<bool> {
            Ok(match open_dir(parent, name)? {
                Some(fd) => same(&fd)? == same(&root)?,
                None => false,
            })
        };
        if !named(&parent_fd)? {
            return Err(refuse("its name now names another directory".to_owned()));
        }
        hook(Stage::Deleting, path);
        // 3. Delete it, every lock still held: its contents through the handle pinned above
        // (a directory swapped in under its name since is never entered), cargo's lock files
        // last; then the emptied directory, by name, only while the name still names it.
        let mut deleted = Deleted::default();
        match names(&root) {
            Ok(mut children) => {
                children.sort_by_key(|c| CARGO_LOCKS.iter().any(|l| c == l));
                for child in children {
                    remove_tree_at(&root, &child, path, root_dev, 1, &mut deleted);
                }
            }
            Err(e) => deleted.kept.push(format!("{}: {e}", path.display())),
        }
        let mut gone = false;
        if deleted.kept.is_empty() {
            if named(&parent_fd)? {
                match dirfd::unlinkat(&parent_fd, name, AtFlags::REMOVEDIR) {
                    Ok(()) => gone = true,
                    Err(e) => {
                        deleted
                            .kept
                            .push(format!("{}: {}", path.display(), io::Error::from(e)))
                    }
                }
            } else {
                deleted.kept.push(format!(
                    "{}: its name now names another directory, which is left",
                    path.display()
                ));
            }
        }

        // 4. Release the locks: `LOCK_UN`, each ([`HeldLock`]).
        drop(held);
        hook(Stage::Released, path);
        if !gone && deleted.bytes == 0 {
            let mut why = deleted.kept;
            if why.is_empty() {
                why.push("nothing could be removed".to_owned());
            }
            return Err(io::Error::other(format!(
                "nothing reclaimed — {}",
                why.join("; ")
            )));
        }
        Ok(Removed {
            bytes: deleted.bytes,
            units: if gone {
                vec![path.to_path_buf()]
            } else {
                Vec::new()
            },
            skipped: Vec::new(),
            trouble: deleted.kept,
        })
    }

    /// Reclaim the build directory at `path`: the profiles `select` names.
    pub(super) fn reclaim(
        path: &Path,
        judge: &Judge,
        select: Select<'_>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        let root = pin(path).map_err(gone_since)?;
        let root_dev = dev(&root)?;
        if judge.device.is_some_and(|d| d != root_dev) {
            return Err(io::Error::other(
                "no longer a candidate: it is on another volume than the one measured",
            ));
        }
        // The fences again, the profiles from the walk of this handle.
        let (marker, holds_source) = root_facts(path);
        let (profiles, cut) = find_at(&root, path, WalkBudget::SHIPPED);
        TargetDir {
            path: path.to_path_buf(),
            marker,
            holds_source,
            walk_cut_short: cut,
            profiles: Vec::new(),
            device: Some(root_dev),
            probed_recent: false,
            origin: super::Origin::Named,
            owned: super::Owned::NotOwned,
            total: None,
        }
        .fences()
        .map_err(|why| io::Error::other(format!("no longer a candidate: {why}")))?;
        let (wanted, leave) = match select {
            Select::Idle { leave } => (None, leave),
            Select::Lru { profile, .. } => (Some(profile), &[][..]),
        };
        if wanted.is_some_and(|w| !profiles.iter().any(|p| p == w)) {
            return Err(io::Error::other(
                "no longer a candidate: not one of this build directory's cargo profiles",
            ));
        }
        let mut done = Removed::default();
        for profile in &profiles {
            // Not the one asked for, or a profile of a build directory named
            // inside this one, which decides its own: passed over, no lock
            // taken.
            if wanted.is_some_and(|w| w != profile) || leave.iter().any(|l| profile.starts_with(l))
            {
                continue;
            }
            let fd = match reopen(&root, path, profile, root_dev) {
                Ok(fd) => fd,
                Err(e) => {
                    done.skipped.push((profile.clone(), e.to_string()));
                    continue;
                }
            };
            // Not a candidate (no or empty incremental/, or not selected: for
            // the idle pass, written inside the window): no lock taken, nothing
            // created. Only the profile the pressure pass named is reported.
            let refused = if holds_cache_at(&fd) {
                select.refuses(clock_at(&fd), judge)
            } else {
                Some("it holds no non-empty incremental/".to_owned())
            };
            if let Some(why) = refused {
                if wanted.is_some() {
                    done.skipped.push((profile.clone(), why));
                }
                continue;
            }
            match reclaim_profile(&fd, profile, root_dev, judge, select, hook) {
                Ok(p) => {
                    done.bytes = done.bytes.saturating_add(p.bytes);
                    if p.gone {
                        done.units.push(profile.join(INCREMENTAL));
                    }
                    done.trouble.extend(p.kept);
                }
                Err(why) => done.skipped.push((profile.clone(), why)),
            }
        }
        if done.units.is_empty() && done.bytes == 0 {
            let mut why: Vec<String> = done
                .skipped
                .iter()
                .map(|(p, w)| format!("{}: {w}", p.display()))
                .collect();
            why.extend(done.trouble.iter().cloned());
            if why.is_empty() {
                why.push("no idle profile holds an incremental/ cache".to_owned());
            }
            return Err(io::Error::other(format!(
                "nothing reclaimed — {}",
                why.join("; ")
            )));
        }
        Ok(done)
    }
}

#[cfg(all(test, unix))]
pub(crate) use fd::mark_foreign;

/// Where there is no fd-relative half to run it (not unix): the walk reads
/// nothing and says so, so no build directory is ever a candidate, and a
/// reclaim refuses.
#[cfg(not(unix))]
mod fd {
    use super::{Judge, Path, PathBuf, Removed, Select, Stage, WalkBudget, io};
    pub(super) fn find(_path: &Path, _budget: WalkBudget) -> (Vec<PathBuf>, Option<String>) {
        (
            Vec::new(),
            Some("reclaiming build caches is unix-only".to_owned()),
        )
    }
    pub(super) fn reclaim(
        _path: &Path,
        _judge: &Judge,
        _select: Select<'_>,
        _hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        Err(io::Error::other("reclaiming build caches is unix-only"))
    }
    pub(super) fn reclaim_whole(
        _path: &Path,
        _judge: &Judge,
        _seen: i64,
        _quiet_s: i64,
        _check: &mut dyn FnMut() -> Result<(), String>,
        _hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        Err(io::Error::other("reclaiming build caches is unix-only"))
    }
}

#[path = "disk_target_tests.rs"]
#[cfg(all(test, unix))]
mod tests;
