// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A cargo build directory, measured, and the one thing in it the harness reclaims: an idle profile's `incremental/`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::discover::{Origin, Owned};
use super::{
    CACHEDIR_SIGNATURE, MAX_WALK_ENTRIES, RUSTC_INFO, Removed, Witness, age_days,
    cachedir_tag_signed,
};
#[cfg(not(windows))]
use super::{walk_bytes, walk_bytes_until};
use held::HeldLock;

/// The build locks cargo takes in each profile.
pub const CARGO_LOCKS: &[&str] = &[".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// What every refusal of a place in a cloud sync root says of it, after the path it names.
pub(crate) const IN_SYNC_ROOT: &str = "lies in a cloud sync root (or behind another reparse point that is no link): nothing in it is searched or removed, since a delete there reaches the cloud and every synced device";

/// The one cache ever reclaimed: rustc's incremental-compilation state.
pub const INCREMENTAL: &str = "incremental";

/// A profile's cache directories: the walk never enters them.
pub const CACHE_DIRS: &[&str] = &[".fingerprint", "deps", "build", "incremental"];

/// What dates a legacy profile: the directories a compile writes, and `examples/`.
/// Build-unit layouts also date each unit's output/fingerprint/run directories and invocation stamps.
pub const CLOCK_DIRS: &[&str] = &[".fingerprint", "deps", "build", "examples"];

/// A profile's directories cargo writes and rebuilds.
pub const PROFILE_OUTPUTS: &[&str] = &[".fingerprint", "build", "deps", "incremental", "examples"];

/// Is the `deps/` entry `dep` the hashed twin of `name`, the artifact cargo uplifts into the profile.
#[must_use]
pub fn uplift_twin(name: &str, dep: &str) -> bool {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    if stem.is_empty() || (ext.is_empty() && dep.contains('.')) {
        return false;
    }
    let Some(rest) = dep.strip_suffix(ext) else {
        return false;
    };
    let Some((twin, hash)) = rest.rsplit_once('-') else {
        return false;
    };
    hash.len() >= 8
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && twin.replace('-', "_") == stem.replace('-', "_")
}

/// Is the `deps/` entry `dep` the UNHASHED twin of `name`, the artifact cargo uplifts into the profile where it leaves the hash out of the file name.
#[must_use]
pub fn unhashed_twin(name: &str, dep: &str) -> bool {
    let has_ext = name.rfind('.').is_some_and(|i| i > 0 && i + 1 < name.len());
    has_ext && name.len() == dep.len() && name.replace('-', "_") == dep.replace('-', "_")
}

/// Root entries that mark source.
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

/// The deepest directory the profile walk enters, below the build directory's root.
pub const MAX_PROFILE_DEPTH: usize = 12;

/// The profile walk's wall-clock budget.
pub const PROFILE_WALK_WALL: Duration = Duration::from_secs(10);

/// A profile a compile wrote into within this many seconds (ten minutes) is never reclaimed, by either pass.
pub const PRESSURE_MIN_IDLE_S: i64 = 10 * 60;

/// The least idle time a profile's incremental cache needs under a projection that is not also a
/// measured short (`docs/DESIGN-disk-free-ask-2026-10-24.md` §8.2): an hour, the span of the
/// projection's own hour rate, so a session paused between builds keeps its cache and its rebuild
/// never reads as a fill aterm made.
pub const PROJECTED_MIN_IDLE_S: i64 = 60 * 60;

#[cfg(any(unix, windows))]
const MAX_DELETE_DEPTH: usize = 256;
const MAX_CUSTODY_DEPTH: usize = 256;

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
    /// Last used: its legacy directory clocks and bounded build-unit invocation/output clocks.
    pub last_used: Option<i64>,
    /// Does it hold a real (not linked) [`INCREMENTAL`] directory with anything in it? An empty one (a release profile's, a `CARGO_INCREMENTAL=0` build's) is nothing to reclaim.
    pub incremental: bool,
    /// Why it is in use right now, or `None` — a build lock someone holds.
    pub in_use: Option<String>,
    /// File allocation in its `incremental/` whose last name would be removed, on the profile's device.
    pub bytes: u64,
    /// `true` when that walk stopped at its bound: the figure is a floor.
    pub bytes_partial: bool,
    /// What a build that died left in it ([`died_output`]), read only under pressure.
    pub died: Died,
}

impl Profile {
    /// The unix second this profile's `incremental/` may first go under pressure (tier 4), where
    /// it is kept now only for its age: a build wrote into it within `min_idle_s`
    /// ([`PRESSURE_MIN_IDLE_S`] at least). `None` where it may go now or by tier 2 (idle past
    /// `threshold_days`), holds nothing to take, has no clock, or a build holds it.
    #[must_use]
    pub fn ripens_at(&self, now: i64, threshold_days: i64, min_idle_s: i64) -> Option<i64> {
        let last = self.last_used?;
        if !self.incremental || self.in_use.is_some() || idle(self.last_used, now, threshold_days) {
            return None;
        }
        let at = last.saturating_add(min_idle_s.max(PRESSURE_MIN_IDLE_S));
        (now < at).then_some(at)
    }
}

/// A died build's unfinished incremental session folder younger than this, by its own
/// modification time, is left. rustc's own collection of unfinished sessions waits ten seconds
/// by the time in the session's name (`is_old_enough_to_be_collected`, rustc_incremental
/// persist/fs.rs at Trust 0e47bc0b33); a folder's modification time is never earlier than its
/// creation, so this is at least as strict. The session's lock is the real fence: rustc takes
/// it before it makes the folder and holds it until the session ends.
pub const DIED_SESSION_MIN_AGE_S: i64 = 10;

/// A metadata temporary folder ([`metadata_temp`]) younger than this is left. No lock covers
/// one, so its age and the process fence (no process with an executable, mapping, working
/// directory or descriptor inside it) are what keep a live compile's.
pub const DIED_METADATA_MIN_AGE_S: i64 = 60;

/// The lock file's name of rustc's incremental session folder `name` while the session is
/// unfinished: `s-{timestamp}-{random}-working`, each part base 36 (digits and lowercase
/// letters), has its lock at `s-{timestamp}-{random}.lock` beside it. `None` for any other name,
/// a finished session's (`s-{timestamp}-{random}-{svh}`) included. The names are the Trust
/// compiler's own (rustc_incremental persist/fs.rs at Trust 0e47bc0b33): a change there has to
/// reach this reader.
#[must_use]
pub fn unfinished_session_lock(name: &str) -> Option<String> {
    let middle = name.strip_prefix("s-")?.strip_suffix("-working")?;
    let (stamp, random) = middle.split_once('-')?;
    let base36 = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    };
    (base36(stamp) && base36(random)).then(|| format!("s-{stamp}-{random}.lock"))
}

/// Whether `name` is a temporary folder rustc's metadata writer makes beside a crate's outputs
/// while it writes them: `rmeta` and six random ASCII letters or digits (rustc_metadata fs.rs's
/// `TempFileBuilder` prefix and the default random length, at Trust 0e47bc0b33). A compile that
/// ends removes its own; one that died leaves it.
#[must_use]
pub fn metadata_temp(name: &str) -> bool {
    name.len() == 11
        && name
            .strip_prefix("rmeta")
            .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// One piece of a died build's partial output in a profile ([`died_output`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partial {
    /// Its path: an unfinished incremental session's folder, or a metadata temporary folder.
    pub path: PathBuf,
    /// A session's lock file ([`unfinished_session_lock`]); `None` for a metadata folder.
    pub session_lock: Option<PathBuf>,
    /// Its folder's modification time, unix seconds.
    pub modified: i64,
    /// What removing it would release ([`cache_bytes`]).
    pub bytes: u64,
    /// `true` when that walk stopped at its bound: the figure is a floor.
    pub bytes_partial: bool,
}

/// A lock a build held over what a pass would take: the pass waits for it to be let go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// A cargo profile whose build locks ([`CARGO_LOCKS`]) a build holds.
    Profile(PathBuf),
    /// An unfinished incremental session's lock file a rustc holds.
    Session(PathBuf),
}

impl Held {
    /// Whether the lock is let go now: a probe that takes each lock a moment and releases it,
    /// creating none (a lock file that is gone is let go).
    #[must_use]
    pub fn released(&self) -> bool {
        match self {
            Held::Profile(profile) => locks_held(profile).is_none(),
            Held::Session(lock) => lock_held(lock, &mut |_| {}).is_none(),
        }
    }
}

/// What a build that died left in one profile ([`died_output`]), and what keeps the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Died {
    /// The pieces a pass may take.
    pub partials: Vec<Partial>,
    /// The earliest unix second a piece kept for its age may go.
    pub ripens: Option<i64>,
    /// The session locks a rustc holds over unfinished sessions (at most
    /// [`super::cadence::HELD_PROBES`]).
    pub held: Vec<Held>,
    /// The listing stopped at [`MAX_WALK_ENTRIES`] names: more may be there.
    pub cut: bool,
}

impl Died {
    fn ripen(&mut self, at: i64) {
        self.ripens = Some(self.ripens.map_or(at, |was| was.min(at)));
    }

    fn hold(&mut self, held: Held) {
        if self.held.len() < super::cadence::HELD_PROBES {
            self.held.push(held);
        }
    }
}

/// The names in one profile's folders [`died_output`] reads, counted against one bound.
struct Listing {
    left: usize,
    cut: bool,
}

impl Listing {
    /// The names in the folder at `dir` on device `dev` (none where it is no folder there, a
    /// link included), each counted; past the bound the listing stops and is cut. Only a name
    /// that matches a pattern is read further, so a large `deps/` costs its names alone.
    fn names(&mut self, dir: &Path, dev: u64) -> Vec<std::ffi::OsString> {
        if folder_modified(dir, dev).is_none() {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries {
            if self.left == 0 {
                self.cut = true;
                break;
            }
            self.left -= 1;
            if let Ok(entry) = entry {
                out.push(entry.file_name());
            }
        }
        out
    }
}

/// The modification time (unix seconds) of the folder at `path` on device `dev`, read without
/// following a link; `None` where it is no folder there.
fn folder_modified(path: &Path, dev: u64) -> Option<i64> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.dev() != dev {
            return None;
        }
    }
    #[cfg(not(unix))]
    if aterm_sysprobe::volumes::file_id(path, false).map(|id| id.dev) != Some(dev) {
        return None;
    }
    let modified = meta.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0),
    )
}

/// WHAT A BUILD THAT DIED LEFT in the profile at `profile` (on device `dev`), read at `now` by
/// the Trust compiler's own names: each unfinished incremental session folder
/// (`incremental/<crate>/s-…-working`, [`unfinished_session_lock`]) whose lock file is there and
/// no process holds, [`DIED_SESSION_MIN_AGE_S`] old; and each metadata temporary folder
/// ([`metadata_temp`]) in `deps/`, `examples/`, `build/<unit>/`, a build script's output folder
/// (`build/<unit>/out/`, or `build/<unit>/<run>/out/` where the units are laid out by package),
/// [`DIED_METADATA_MIN_AGE_S`] old. A younger piece sets [`Died::ripens`], a session whose lock
/// a rustc holds is [`Died::held`]. A reading: it follows no link, leaves `dev`, takes a lock only
/// a moment (and creates none), and lists at most [`MAX_WALK_ENTRIES`] names. The process fence
/// of a metadata folder is the remover's, under the profile's locks ([`reclaim_died`]).
pub(crate) fn died_output(profile: &Path, dev: u64, now: i64) -> Died {
    let mut died = Died::default();
    let mut listing = Listing {
        left: MAX_WALK_ENTRIES,
        cut: false,
    };
    let incremental = profile.join(INCREMENTAL);
    for krate in listing.names(&incremental, dev) {
        let krate = incremental.join(krate);
        for name in listing.names(&krate, dev) {
            let Some(lock) = name.to_str().and_then(unfinished_session_lock) else {
                continue;
            };
            let path = krate.join(&name);
            let Some(modified) = folder_modified(&path, dev) else {
                continue;
            };
            // A session with no lock file beside it is no session rustc's rule judges: left.
            let lock = krate.join(lock);
            if !std::fs::symlink_metadata(&lock).is_ok_and(|m| m.is_file()) {
                continue;
            }
            if now.saturating_sub(modified) < DIED_SESSION_MIN_AGE_S {
                died.ripen(modified.saturating_add(DIED_SESSION_MIN_AGE_S));
                continue;
            }
            if lock_held(&lock, &mut |_| {}).is_some() {
                died.hold(Held::Session(lock));
                continue;
            }
            let (bytes, bytes_partial) = cache_bytes(&path, Some(dev));
            died.partials.push(Partial {
                path,
                session_lock: Some(lock),
                modified,
                bytes,
                bytes_partial,
            });
        }
    }
    let mut outputs = vec![profile.join("deps"), profile.join("examples")];
    let build = profile.join("build");
    for unit in listing.names(&build, dev) {
        let unit = build.join(unit);
        for run in listing.names(&unit, dev) {
            outputs.push(unit.join(run).join("out"));
        }
        outputs.push(unit.join("out"));
        outputs.push(unit);
    }
    for folder in outputs {
        for name in listing.names(&folder, dev) {
            if !name.to_str().is_some_and(metadata_temp) {
                continue;
            }
            let path = folder.join(&name);
            let Some(modified) = folder_modified(&path, dev) else {
                continue;
            };
            if now.saturating_sub(modified) < DIED_METADATA_MIN_AGE_S {
                died.ripen(modified.saturating_add(DIED_METADATA_MIN_AGE_S));
                continue;
            }
            let (bytes, bytes_partial) = cache_bytes(&path, Some(dev));
            died.partials.push(Partial {
                path,
                session_lock: None,
                modified,
                bytes,
                bytes_partial,
            });
        }
    }
    died.cut = listing.cut;
    died
}

/// One build directory, with the facts that decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDir {
    /// Its canonical path: the reclaim reopens it component by component without following symlinks, so the report names the real one.
    pub path: PathBuf,
    /// The marker that proves a build tool laid it, when one was found.
    pub marker: Option<String>,
    /// The first [`SOURCE_MARKERS`] entry at its root, when there is one.
    pub holds_source: Option<String>,
    /// Why the profile walk did not read all of it, when it did not.
    pub walk_cut_short: Option<String>,
    /// Every cargo profile in it, at any depth.
    pub profiles: Vec<Profile>,
    /// The device it lives on ([`dev_of`]), when that could be read.
    pub device: Option<u64>,
    /// Why its volume is not one a reclaim may touch, when it is not.
    pub volume: Option<String>,
    /// Were its recent profiles also probed for locks and sized ([`assess_under`] under pressure)? Otherwise only idle ones are, and the pressure pass makes no row from the rest.
    pub probed_recent: bool,
    /// How it was found ([`Origin::Named`]: named by the caller).
    pub origin: Origin,
    /// Whether its owner is gone ([`super::discover::Owners`]).
    pub owned: Owned,
    /// Its whole size ([`total_bytes`]) and whether that is a floor, when it was sized: what the report's largest-hogs list reads.
    pub total: Option<(u64, bool)>,
    /// Why it holds an entry of another user, when the sizing walk met one ([`total_bytes_owned`]).
    pub foreign: Option<String>,
    /// Tier 3's verdict on it ([`super::idle::assess`]): its facts as a whole idle build directory, or why it is kept whole.
    pub idle: Option<Result<super::idle::IdleFacts, String>>,
}

impl TargetDir {
    /// The profiles a reclaim would take: idle past `threshold_days` at `now`, holding an `incremental/`, and not in use.
    #[must_use]
    pub fn candidates(&self, now: i64, threshold_days: i64) -> Vec<&Profile> {
        self.candidate_profiles(now, threshold_days).collect()
    }

    /// Counted file allocation in the candidates' `incremental/` caches, and whether it is a floor.
    #[must_use]
    pub fn reclaimable(&self, now: i64, threshold_days: i64) -> (u64, bool) {
        self.candidate_profiles(now, threshold_days)
            .fold((0u64, false), |(b, partial), p| {
                let (bytes, overflow) = b.overflowing_add(p.bytes);
                (
                    if overflow { u64::MAX } else { bytes },
                    partial || p.bytes_partial || overflow,
                )
            })
    }

    fn candidate_profiles(&self, now: i64, threshold_days: i64) -> impl Iterator<Item = &Profile> {
        self.profiles.iter().filter(move |p| {
            p.incremental && p.in_use.is_none() && idle(p.last_used, now, threshold_days)
        })
    }

    /// The profiles the pressure pass may take ([`reclaim_lru`]), each unwritten for at least
    /// `min_idle_s` ([`PRESSURE_MIN_IDLE_S`], or [`PROJECTED_MIN_IDLE_S`] under a projection above
    /// the floor), and each other recent one with why it is kept.
    #[must_use]
    pub fn pressure_candidates(
        &self,
        now: i64,
        threshold_days: i64,
        min_idle_s: i64,
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
            if age < min_idle_s.max(PRESSURE_MIN_IDLE_S) {
                kept.push((
                    p,
                    format!(
                        "written into {} ago; no reclaim takes a profile built in the last {}",
                        human_age(age),
                        human_age(min_idle_s.max(PRESSURE_MIN_IDLE_S))
                    ),
                ));
                continue;
            }
            picks.push(p);
        }
        (picks, kept)
    }

    /// The fences a build directory must clear before anything in it is removed.
    pub fn fences(&self) -> Result<String, String> {
        ordinary_custody(&self.path)?;
        if let Some(why) = &self.volume {
            return Err(why.clone());
        }
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

/// A span of seconds a person can read: minutes under an hour, hours under a day, whole days after.
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

fn idle(last_used: Option<i64>, now: i64, threshold_days: i64) -> bool {
    last_used.is_some_and(|m| {
        age_days(now, m) >= threshold_days && now.saturating_sub(m) >= PRESSURE_MIN_IDLE_S
    })
}

/// The witness for one build directory, or the reason it has none: the first refusal it fails.
pub(super) fn target_witness(
    t: &TargetDir,
    now: i64,
    threshold_days: i64,
) -> Result<Witness, String> {
    let marker = t.fences()?;
    if t.profiles.is_empty() {
        return Err(
            "no recognized Cargo/Targo profile (a build lock beside deps/, .fingerprint/, or build/ with incremental/)"
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

/// The build directory at `path`, assessed for the report: canonicalized, its facts and its profiles (one bounded walk).
#[must_use]
pub fn assess(path: &Path, now: i64, threshold_days: i64, budget: WalkBudget) -> Option<TargetDir> {
    assess_under(path, now, threshold_days, false, budget)
}

/// [`assess`], and under `pressure` (free space below the floor) every profile's locks probed and every `incremental/` sized, recent ones too.
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

/// The whole size of the build directory at `dir`.
#[must_use]
pub fn total_bytes(dir: &Path) -> (u64, bool) {
    #[cfg(windows)]
    {
        let t = fd::tally(dir, &fd::Walk::sizes(fd::Count::Shared, None));
        (t.bytes, t.partial)
    }
    #[cfg(not(windows))]
    {
        let dev = dev_of(dir);
        walk_bytes(dir, |meta| same_dev(meta, dev), shared_blocks)
    }
}

/// [`total_bytes`] that also stops once `deadline` passes, cut short: a survey with a wall-clock budget sizes no folder past it.
#[must_use]
pub fn total_bytes_until(dir: &Path, deadline: std::time::Instant) -> (u64, bool) {
    #[cfg(windows)]
    {
        let wall = deadline.saturating_duration_since(std::time::Instant::now());
        if wall.is_zero() {
            return (0, true);
        }
        let t = fd::tally(
            dir,
            &fd::Walk {
                wall: Some(wall),
                ..fd::Walk::sizes(fd::Count::Shared, None)
            },
        );
        (t.bytes, t.partial)
    }
    #[cfg(not(windows))]
    {
        let dev = dev_of(dir);
        walk_bytes_until(
            dir,
            |meta| same_dev(meta, dev),
            shared_blocks,
            Some(deadline),
        )
    }
}

/// [`total_bytes`], and the first owner other than this process's user met on the way (the directory itself included), said as why a whole removal does not go.
#[must_use]
pub fn total_bytes_owned(dir: &Path) -> ((u64, bool), Option<String>) {
    #[cfg(windows)]
    {
        let t = fd::tally(
            dir,
            &fd::Walk {
                owners: true,
                ..fd::Walk::sizes(fd::Count::Shared, None)
            },
        );
        ((t.bytes, t.partial), t.foreign)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let dev = dev_of(dir);
        let me = super::discover::own_uid();
        let foreign = std::cell::Cell::new(
            std::fs::symlink_metadata(dir)
                .ok()
                .map(|m| m.uid())
                .filter(|uid| *uid != me),
        );
        let seen = |meta: &std::fs::Metadata| {
            if foreign.get().is_none() && meta.uid() != me {
                foreign.set(Some(meta.uid()));
            }
        };
        let total = walk_bytes(
            dir,
            |meta| {
                seen(meta);
                same_dev(meta, dev)
            },
            |meta| {
                seen(meta);
                shared_blocks(meta)
            },
        );
        let why = foreign.get().map(|uid| {
            format!(
                "it holds an entry of another user (uid {uid}), which a removal could not delete"
            )
        });
        (total, why)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let dev = dev_of(dir);
        (
            walk_bytes(dir, |meta| same_dev(meta, dev), shared_blocks),
            None,
        )
    }
}

/// The most entries [`foreign_beyond`] reads.
pub const FOREIGN_WALK_ENTRIES: usize = 5_000_000;

/// [`foreign_beyond`]'s wall-clock bound.
pub const FOREIGN_WALK_WALL: Duration = Duration::from_secs(60);

/// THE OTHER-USER FENCE OF A WHOLE REMOVAL, past a sizing walk that stopped short ([`total_bytes_owned`]: a large build directory holds more than [`MAX_WALK_ENTRIES`], or a folder could not be read).
#[must_use]
pub fn foreign_beyond(dir: &Path) -> Option<String> {
    foreign_walk(dir, FOREIGN_WALK_ENTRIES, FOREIGN_WALK_WALL)
}

/// [`foreign_beyond`] within `max` entries and `wall`.
#[must_use]
pub fn foreign_walk(dir: &Path, max: usize, wall: Duration) -> Option<String> {
    #[cfg(windows)]
    {
        let t = fd::tally(
            dir,
            &fd::Walk {
                count: fd::Count::Nothing,
                owners: true,
                max,
                wall: Some(wall),
                dev: None,
            },
        );
        t.foreign.or_else(|| {
            t.cut
                .map(|why| format!("{why}, so nothing of another user's in it is ruled out"))
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let me = super::discover::own_uid();
        let dev = dev_of(dir);
        let started = std::time::Instant::now();
        let mut seen = 0usize;
        let mut stack = vec![dir.to_path_buf()];
        let unruled = |what: String| {
            Some(format!(
                "{what}, so nothing of another user's in it is ruled out"
            ))
        };
        while let Some(d) = stack.pop() {
            if started.elapsed() >= wall {
                return unruled("its ownership walk ran out of time".to_owned());
            }
            let rd = match std::fs::read_dir(&d) {
                Ok(rd) => rd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return unruled(format!("{} could not be read ({e})", d.display())),
            };
            for entry in rd {
                let Ok(entry) = entry else {
                    return unruled(format!("{} could not be listed", d.display()));
                };
                seen += 1;
                aterm_uds::progress::tick();
                if seen > max {
                    return unruled(format!("it holds more than {max} entries"));
                }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        return unruled(format!(
                            "{} could not be read ({e})",
                            entry.path().display()
                        ));
                    }
                };
                if meta.uid() != me {
                    return Some(format!(
                        "it holds an entry of another user (uid {}), which a removal could not delete",
                        meta.uid()
                    ));
                }
                if meta.is_dir() && same_dev(&meta, dev) {
                    stack.push(entry.path());
                }
            }
        }
        None
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (dir, max, wall);
        Some("no owner can be read here, so nothing of another user's is ruled out".to_owned())
    }
}

#[cfg(not(windows))]
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

/// The device the directory at `path` lives on, a link followed as the free-space figure (statvfs) and the tick's volume filter follow it, when it can be read.
#[must_use]
pub fn dev_of(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        aterm_sysprobe::volumes::file_id(path, true).map(|id| id.dev)
    }
}

/// A Windows volume's own name: the GUID the mount manager names it by (`\\?\Volume{6140fc3b-…}\`), which two volumes never share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeGuid(u128);

impl VolumeGuid {
    /// The GUID spelled `{6140fc3b-044f-43d7-b060-cb98170c7a63}`, in any case; `None` for any other shape.
    #[must_use]
    pub fn parse(guid: &str) -> Option<Self> {
        let inner = guid.strip_prefix('{')?.strip_suffix('}')?;
        let groups: Vec<&str> = inner.split('-').collect();
        let shaped = groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
            && groups
                .iter()
                .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()));
        if !shaped {
            return None;
        }
        u128::from_str_radix(&groups.concat(), 16).ok().map(Self)
    }

    /// The volume a reading or a mount is keyed by, when that key is a Windows volume's GUID ([`aterm_sysprobe::volumes::VolumeKey::Volume`]).
    #[must_use]
    pub fn of_key(key: &aterm_sysprobe::volumes::VolumeKey) -> Option<Self> {
        match key {
            aterm_sysprobe::volumes::VolumeKey::Volume(guid) => Self::parse(guid),
            _ => None,
        }
    }

    /// The volume holding `path` in `table`, placed as a free-space reading places it (`super::volume_of`).
    #[must_use]
    pub fn of_path(path: &Path, table: &aterm_sysprobe::volumes::Table) -> Option<Self> {
        if !cfg!(windows) {
            return None;
        }
        let placed = path
            .ancestors()
            .find_map(|a| std::fs::canonicalize(a).ok())?;
        Self::of_key(&aterm_sysprobe::volumes::mount_of(&placed, table)?.key()?)
    }

    /// [`Self::of_path`] through the mount table read now — read on Windows only; `None` elsewhere, reading nothing.
    #[must_use]
    pub fn of_path_now(path: &Path) -> Option<Self> {
        if !cfg!(windows) {
            return None;
        }
        Self::of_path(path, &aterm_sysprobe::volumes::table())
    }
}

impl std::fmt::Display for VolumeGuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let g = self.0;
        write!(
            f,
            "{{{:08x}-{:04x}-{:04x}-{:04x}-{:012x}}}",
            g >> 96,
            (g >> 80) & 0xFFFF,
            (g >> 64) & 0xFFFF,
            (g >> 48) & 0xFFFF,
            g & 0xFFFF_FFFF_FFFF
        )
    }
}

/// The volume fence's second reading (Windows; [`Judge::volume`]).
#[cfg(any(windows, test))]
pub(crate) fn same_volume(
    measured: Option<VolumeGuid>,
    here: Option<VolumeGuid>,
) -> Result<(), &'static str> {
    match (measured, here) {
        (None, _) => Err("the measured volume's own name (its GUID) was not read"),
        (Some(_), None) => Err("its volume's own name (its GUID) could not be read"),
        (Some(m), Some(h)) if m != h => Err(
            "it is on another volume than the one measured (the volumes' GUIDs differ, whatever their serials)",
        ),
        (Some(_), Some(_)) => Ok(()),
    }
}

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
            died: Died::default(),
        })
        .collect();
    Some(TargetDir {
        path: path.to_path_buf(),
        marker,
        holds_source,
        walk_cut_short: cut,
        profiles,
        device: dev_of(path),
        volume: volume_refusal(path),
        probed_recent: false,
        origin: Origin::Named,
        owned: Owned::NotOwned,
        total: None,
        foreign: None,
        idle: None,
    })
}

#[cfg(all(test, windows))]
thread_local! {
    static DRIVE_KIND: std::cell::Cell<Option<Option<bool>>> = const { std::cell::Cell::new(None) };
}

/// A test's drive (this machine has fixed disks only).
#[cfg(all(test, windows))]
pub(crate) fn pretend_drive_kind(kind: Option<Option<bool>>) {
    DRIVE_KIND.with(|d| d.set(kind));
}

fn volume_refusal(path: &Path) -> Option<String> {
    off_fixed_disk(path)
        .map(|why| format!("{why} — only a fixed disk's build caches are reclaimed"))
}

fn off_fixed_disk(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use aterm_sysprobe::volumes;
        let table = volumes::table();
        let kind = |m: &volumes::Mount| {
            #[cfg(test)]
            if let Some(kind) = DRIVE_KIND.with(std::cell::Cell::get) {
                return kind;
            }
            volumes::is_fixed(m)
        };
        fixed_verdict(volumes::mount_of(path, &table).map(|m| (m.local, kind(m)))).err()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

fn managed_cache(path: &Path) -> bool {
    atpkg::build_cache::registered::custody_present(path)
}

const MANAGED_CACHE: &str =
    "its registered build-cache policy owns retirement, not ordinary target cleanup";

fn ordinary_custody(path: &Path) -> Result<(), String> {
    for (depth, ancestor) in path.ancestors().enumerate() {
        if depth >= MAX_CUSTODY_DEPTH {
            return Err("its build-cache ancestry is too deep to establish custody".into());
        }
        if managed_cache(ancestor) {
            return Err(MANAGED_CACHE.into());
        }
    }
    Ok(())
}

fn root_facts(path: &Path) -> (Option<String>, Option<String>) {
    let marker = if managed_cache(path) {
        None
    } else if cachedir_tag_signed(&path.join("CACHEDIR.TAG")) {
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

fn holds_cache(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && std::fs::read_dir(dir).is_ok_and(|mut rd| rd.next().is_some())
}

fn profile_clock(profile: &Path) -> Option<i64> {
    fd::clock(profile, WalkBudget::SHIPPED)
}

mod held {
    use std::io;

    /// One of cargo's build locks held by this process, released by `LOCK_UN` on drop.
    pub(super) struct HeldLock(std::fs::File);

    impl HeldLock {
        /// Take `file`'s `flock` (Windows: `LockFileEx`) exclusively without waiting.
        pub(super) fn try_take(file: std::fs::File) -> io::Result<Option<HeldLock>> {
            let file: std::fs::File = file;
            #[cfg(unix)]
            aterm_uds::keep_out_of_children(&file)?;
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

/// A test's build: one of cargo's locks held as a build holds it, released by `LOCK_UN` on drop (unwind included), so no sibling test's spawned child keeps it held.
#[cfg(test)]
pub(crate) struct Build {
    _lock: HeldLock,
}

#[cfg(test)]
impl Build {
    /// A build takes the lock at `lock` (which exists).
    pub(crate) fn holds(lock: &Path) -> Build {
        let file = std::fs::File::open(lock).expect("the lock exists");
        let lock = HeldLock::try_take(file)
            .expect("the lock can be taken")
            .expect("the test's build takes the lock");
        Build { _lock: lock }
    }
}

fn locks_held(profile: &Path) -> Option<String> {
    locks_held_with(profile, &mut |_| {})
}

fn locks_held_with(profile: &Path, hook: &mut dyn FnMut(&Path)) -> Option<String> {
    for lock in CARGO_LOCKS {
        if let Some(why) = lock_held(&profile.join(lock), hook) {
            return Some(why);
        }
    }
    None
}

/// Whether a process holds the lock file at `path`: why, where one does or it could not be
/// probed; `None` where it is free or not there. The probe opens it read-only (never creating
/// it, never through a link) and takes its lock a moment (`flock`; `LockFileEx` on Windows),
/// telling `hook` while it holds it. cargo's build locks are `flock`s, as rustc's session
/// locks are on Linux; on macOS rustc takes a session's lock with `fcntl`, which the BSD
/// kernel keeps on the same advisory list of the file as a `flock`, so the probe meets it
/// there too (rustc_data_structures flock/unix.rs at Trust 0e47bc0b33).
fn lock_held(path: &Path, hook: &mut dyn FnMut(&Path)) -> Option<String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Some(format!("{} is not a plain file", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("{} could not be probed: {e}", path.display())),
    }
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = match aterm_uds::kept_out(|| open.open(path)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("{} could not be probed: {e}", path.display())),
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Some(format!("{} is not a plain file", path.display()));
    }
    match HeldLock::try_take(file) {
        Ok(Some(held)) => {
            hook(path);
            drop(held);
            None
        }
        Ok(None) => Some(format!("a build holds {}", path.display())),
        Err(e) => Some(format!("{} could not be probed: {e}", path.display())),
    }
}

/// Bytes deleting the tree at `dir` would release.
#[must_use]
pub fn cache_bytes(dir: &Path, dev: Option<u64>) -> (u64, bool) {
    #[cfg(windows)]
    {
        let t = fd::tally(dir, &fd::Walk::sizes(fd::Count::Released, dev));
        (t.bytes, t.partial)
    }
    #[cfg(not(windows))]
    {
        walk_bytes(dir, |meta| same_dev(meta, dev), released)
    }
}

#[cfg(not(windows))]
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

#[cfg(not(windows))]
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

#[cfg(test)]
thread_local! {
    static FAULT_CREATE_LOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's fault: on this thread, creating a missing lock fails as a full disk makes it fail (ENOSPC).
#[cfg(test)]
pub(crate) fn inject_create_lock_fault(on: bool) {
    FAULT_CREATE_LOCK.with(|f| f.set(on));
}

#[cfg(test)]
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
    /// The device whose free space the caller measured.
    pub device: Option<u64>,
    /// On Windows, that measured volume's own name ([`VolumeGuid::of_key`] of the reading's key).
    pub volume: Option<VolumeGuid>,
}

/// Where a reclaim of one profile stands, for a test's hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Locked,
    Deleting,
    Released,
    #[cfg(test)]
    Removing,
}

/// Reclaim one build directory's idle `incremental/` caches.
pub fn reclaim(path: &Path, judge: &Judge) -> io::Result<Removed> {
    reclaim_with(path, judge, &mut |_, _| {})
}

/// [`reclaim`], leaving every profile inside one of `leave`.
pub fn reclaim_leaving(path: &Path, judge: &Judge, leave: &[PathBuf]) -> io::Result<Removed> {
    fd::reclaim(path, judge, Select::Idle { leave }, &mut |_, _| {})
}

/// [`reclaim`] with a hook told each [`Stage`] of each profile (a test's probe).
pub fn reclaim_with(
    path: &Path,
    judge: &Judge,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim(path, judge, Select::Idle { leave: &[] }, hook)
}

/// Remove an independently admitted cache entry through pinned directory descriptors.
/// The caller supplies its surveyed identity and a cheap, live policy permission check.
/// Managed build custody and a refusal from `permit` stop the whole pass; any bytes already
/// released and the reason for a partial removal are retained in [`Removed`].
/// # Errors
/// The entry cannot be pinned on a fixed local volume, is a link or special file, its identity
/// changed, or custody or the caller's permission cannot be established before deletion starts.
pub fn remove_unmanaged_tree(
    path: &Path,
    expected: aterm_sysprobe::volumes::FileId,
    permit: &mut dyn FnMut() -> Result<(), String>,
) -> io::Result<Removed> {
    fd::remove_unmanaged_tree(path, expected, permit)
}

/// Establish that an entry's pinned ancestors and reachable subtree carry no managed custody.
/// Directory links are skipped, never followed. This read grants no removal authority and
/// does not replace the remover's live checks.
/// # Errors
/// The path cannot be pinned, a custody marker is present or unreadable, a directory is on
/// another device or unreadable, the entry is not a regular file or directory, or a bound ends
/// the scan before it is complete.
pub fn unmanaged_custody(path: &Path, budget: WalkBudget) -> Result<(), String> {
    fd::unmanaged_custody(path, budget)
}

/// Under pressure, reclaim one profile's `incremental/`.
pub fn reclaim_lru(path: &Path, profile: &Path, judge: &Judge, seen: i64) -> io::Result<Removed> {
    reclaim_lru_with(path, profile, judge, seen, &mut |_, _| {})
}

/// [`reclaim_lru`] with a hook told each [`Stage`] (a test's probe).
pub fn reclaim_lru_with(
    path: &Path,
    profile: &Path,
    judge: &Judge,
    seen: i64,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim(path, judge, Select::Lru { profile, seen }, hook)
}

/// TIER 0's removal of one piece of what a died build left ([`died_output`],
/// `super::Witness::DiedBuild`): the unfinished incremental session folder or metadata temporary
/// folder `entry`, in the profile `profile` of the build directory `path`, last written at `seen`
/// by the survey. Under the profile's build locks it takes the session's own lock (a rustc that
/// holds it keeps its session), reads the entry's name again by the compiler's rules and its age
/// again, has `check` judge it (a metadata folder's process fence), removes it on the measured
/// device and puts its folder's access and modification times back, so the profile's clock
/// ([`Profile::last_used`]) reads after the removal what it read before. The session's lock file
/// stays: rustc's own collection removes a lock no session folder has.
/// # Errors
/// Nothing was reclaimed: the entry or its profile is no longer a candidate, a lock is held,
/// `check` refused, or nothing could be removed.
pub fn reclaim_died(
    path: &Path,
    profile: &Path,
    entry: &Path,
    judge: &Judge,
    seen: i64,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> io::Result<Removed> {
    reclaim_died_with(path, profile, entry, judge, seen, check, &mut |_, _| {})
}

/// [`reclaim_died`] with a hook told each [`Stage`] (a test's probe; the path is the entry's).
pub fn reclaim_died_with(
    path: &Path,
    profile: &Path,
    entry: &Path,
    judge: &Judge,
    seen: i64,
    check: &mut dyn FnMut() -> Result<(), String>,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim_died(path, (profile, entry), judge, seen, check, hook)
}

/// Below the floor, remove a build directory whose owner is gone, whole (`super::Witness::OwnerGone`).
pub fn reclaim_whole(
    path: &Path,
    judge: &Judge,
    seen: i64,
    quiet_s: i64,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> io::Result<Removed> {
    reclaim_whole_with(path, judge, seen, quiet_s, check, &mut |_, _| {})
}

/// [`reclaim_whole`] with a hook told each [`Stage`] (a test's probe; the path is the build directory's).
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

/// TIER 3's removal of a live checkout's whole idle build directory (`super::Witness::IdleWhole`).
pub fn reclaim_profiles(
    path: &Path,
    judge: &Judge,
    seen: i64,
    window_s: i64,
    mounts: &mut dyn FnMut() -> Result<(), String>,
    between: &mut dyn FnMut() -> Result<(), String>,
) -> io::Result<Removed> {
    reclaim_profiles_with(path, judge, seen, window_s, mounts, between, &mut |_, _| {})
}

/// [`reclaim_profiles`] with a hook told each [`Stage`] of each profile (a test's probe; the path is the profile's).
pub fn reclaim_profiles_with(
    path: &Path,
    judge: &Judge,
    seen: i64,
    window_s: i64,
    mounts: &mut dyn FnMut() -> Result<(), String>,
    between: &mut dyn FnMut() -> Result<(), String>,
    hook: &mut dyn FnMut(Stage, &Path),
) -> io::Result<Removed> {
    fd::reclaim_profiles(path, judge, (seen, window_s), mounts, between, hook)
}

/// Remove the one entry at the absolute `path` on Windows.
#[cfg(windows)]
pub(crate) fn remove_entry(path: &Path) -> io::Result<u64> {
    fd::remove_entry(path)
}

/// The Windows volume fence of every reclaim, judged from what the mount table says of the build directory's volume.
#[cfg(any(windows, test))]
pub(crate) fn fixed_verdict(volume: Option<(bool, Option<bool>)>) -> Result<(), String> {
    match volume {
        None => Err("no volume of the mount table holds it".to_owned()),
        Some((false, _)) => Err(
            "it is not on a local disk volume (a network share, or a device outside the local allowlist)"
                .to_owned(),
        ),
        Some((true, Some(true))) => Ok(()),
        Some((true, Some(false))) => {
            Err("it is on a removable, optical or RAM drive, not a fixed disk".to_owned())
        }
        Some((true, None)) => Err("its volume's drive type could not be read".to_owned()),
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    not(any(unix, windows)),
    expect(
        dead_code,
        reason = "only the unix and Windows reclaims read a selection"
    )
)]
enum Select<'a> {
    Idle { leave: &'a [PathBuf] },
    Lru { profile: &'a Path, seen: i64 },
}

#[cfg(any(unix, windows))]
impl Select<'_> {
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

#[cfg(any(unix, windows))]
mod fd {
    use super::{
        CACHE_DIRS, CARGO_LOCKS, CLOCK_DIRS, DIED_METADATA_MIN_AGE_S, DIED_SESSION_MIN_AGE_S,
        HeldLock, INCREMENTAL, Judge, MAX_CUSTODY_DEPTH, MAX_DELETE_DEPTH, Path, PathBuf, Removed,
        Select, Stage, TargetDir, WalkBudget, io, root_facts,
    };
    use aterm_dirfd::{self as dirfd, AtFlags, Dir, Errno, Mode, OFlags, OwnedFd};
    #[cfg(windows)]
    use aterm_dirfd::{FileId, FileType};
    use std::ffi::{OsStr, OsString};
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt as _;
    use std::time::Instant;

    #[cfg(unix)]
    mod code {
        /// Gone: the name is not there.
        pub(super) const NOENT: i32 = libc::ENOENT;
        /// What a directory open refuses without entering: not a directory, or a symlink (`ELOOP` under `O_NOFOLLOW`).
        pub(super) const NOT_ENTERED: &[i32] = &[libc::ENOTDIR, libc::ELOOP];
        /// A link refused by `O_NOFOLLOW`.
        pub(super) const LOOP: i32 = libc::ELOOP;
        /// A full disk.
        #[cfg(test)]
        pub(super) const NOSPC: i32 = libc::ENOSPC;
    }

    #[cfg(windows)]
    mod code {
        use aterm_dirfd::Errno;
        /// Gone: `ERROR_FILE_NOT_FOUND` — and only it (`aterm-dirfd`'s "Errors": a missing path or volume is not "gone").
        pub(super) const NOENT: i32 = Errno::NOENT.0;
        /// What a directory open refuses without entering.
        pub(super) const NOT_ENTERED: &[i32] =
            &[Errno::NOTDIR.0, Errno::LOOP.0, Errno::FOREIGN_REPARSE.0];
        /// `ERROR_DISK_FULL`.
        #[cfg(test)]
        pub(super) const NOSPC: i32 = 112;
    }

    fn dir_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
    }

    fn marker_absent_at(root: &OwnedFd) -> Result<(), String> {
        for marker in atpkg::build_cache::registered::CUSTODY_MARKERS {
            match dirfd::openat(
                root,
                *marker,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            ) {
                Err(error) if error.0 == code::NOENT => {}
                _ => return Err(super::MANAGED_CACHE.into()),
            }
        }
        Ok(())
    }

    /// Follow the pinned directory's actual parents, including after a rename.
    #[cfg(unix)]
    fn ordinary_custody_at(root: &OwnedFd, _: &Path) -> Result<(), String> {
        let mut parent = None;
        for _ in 0..MAX_CUSTODY_DEPTH {
            let current = parent.as_ref().unwrap_or(root);
            marker_absent_at(current)?;
            let next = dirfd::openat(current, "..", dir_flags(), Mode::empty())
                .map_err(|e| format!("its build-cache ancestry could not be opened: {e}"))?;
            let identity = |fd: &OwnedFd| dirfd::fstat(fd).map(|s| (s.st_dev, s.st_ino));
            if identity(current).map_err(|e| e.to_string())?
                == identity(&next).map_err(|e| e.to_string())?
            {
                return Ok(());
            }
            parent = Some(next);
        }
        Err("its build-cache ancestry is too deep to establish custody".into())
    }

    /// Windows does not admit `..` relative opens; refuse a changed name and pin each ancestor.
    #[cfg(windows)]
    fn ordinary_custody_at(root: &OwnedFd, path: &Path) -> Result<(), String> {
        marker_absent_at(root)?;
        let named = pin(path).map_err(|e| e.to_string())?;
        if dirfd::file_id(&named).map_err(|e| e.to_string())?
            != dirfd::file_id(root).map_err(|e| e.to_string())?
        {
            return Err("its name now names another directory".into());
        }
        for (depth, ancestor) in path.ancestors().enumerate() {
            if depth >= MAX_CUSTODY_DEPTH {
                return Err("its build-cache ancestry is too deep to establish custody".into());
            }
            marker_absent_at(&pin(ancestor).map_err(|e| e.to_string())?)?;
        }
        Ok(())
    }

    #[cfg(test)]
    thread_local! {
        static FOREIGN: std::cell::RefCell<Vec<u128>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Mark the directory at `path` as on another device, on this thread (a test's).
    #[cfg(test)]
    pub(crate) fn mark_foreign(path: &Path) {
        let id = aterm_sysprobe::volumes::file_id(path, false).expect("marked");
        FOREIGN.with(|f| f.borrow_mut().push(id.ino));
    }

    #[cfg(unix)]
    fn dev(fd: &OwnedFd) -> io::Result<u64> {
        let st = dirfd::fstat(fd)?;
        #[cfg(test)]
        if FOREIGN.with(|f| f.borrow().contains(&u128::from(st.st_ino))) {
            return Ok(st.st_dev.wrapping_add(1));
        }
        Ok(st.st_dev)
    }

    #[cfg(windows)]
    fn dev(fd: &OwnedFd) -> io::Result<u64> {
        let id = dirfd::file_id(fd)?;
        #[cfg(test)]
        if FOREIGN.with(|f| f.borrow().contains(&id.ino)) {
            return Ok(id.dev.wrapping_add(1));
        }
        Ok(id.dev)
    }

    #[cfg(all(test, windows))]
    thread_local! {
        static CLONED: std::cell::RefCell<Vec<u128>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Mark the directory at `path` as on a CLONE of its volume, on this thread (a test's).
    #[cfg(all(test, windows))]
    pub(crate) fn mark_cloned(path: &Path) {
        let id = aterm_sysprobe::volumes::file_id(path, false).expect("marked");
        CLONED.with(|c| c.borrow_mut().push(id.ino));
    }

    #[cfg(windows)]
    fn volume(fd: &OwnedFd) -> Option<super::VolumeGuid> {
        let guid = super::VolumeGuid::parse(&dirfd::volume_guid(fd).ok()?)?;
        #[cfg(test)]
        if dirfd::file_id(fd).is_ok_and(|id| CLONED.with(|c| c.borrow().contains(&id.ino))) {
            return Some(super::VolumeGuid(guid.0 ^ 1));
        }
        Some(guid)
    }

    fn on_measured_volume(root: &OwnedFd, judge: &Judge) -> io::Result<u64> {
        let root_dev = dev(root)?;
        let Some(device) = judge.device else {
            return Ok(root_dev);
        };
        let refuse = |why: &str| io::Error::other(format!("no longer a candidate: {why}"));
        if device != root_dev {
            return Err(refuse("it is on another volume than the one measured"));
        }
        #[cfg(windows)]
        super::same_volume(judge.volume, volume(root)).map_err(refuse)?;
        Ok(root_dev)
    }

    /// Open the directory at the absolute `path` from its volume's root (`aterm_dirfd::pin`: a LOCAL volume's own root, never a share or a `subst` letter), never through a link or any other reparse point.
    #[cfg(windows)]
    pub(super) fn pin(path: &Path) -> io::Result<OwnedFd> {
        if !path.is_absolute() {
            return Err(io::Error::other("not an absolute path"));
        }
        dirfd::pin(path).map_err(|e| {
            if e == Errno::FOREIGN_REPARSE {
                io::Error::other(format!(
                    "refusing {}: it {}",
                    path.display(),
                    super::IN_SYNC_ROOT
                ))
            } else if code::NOT_ENTERED.contains(&e.0) {
                io::Error::other(format!(
                    "refusing {}: a component is a symlink, junction or other reparse point, or not a directory (nothing is reached through a link)",
                    path.display()
                ))
            } else {
                e.into()
            }
        })
    }

    /// Open the directory at the absolute `path` component by component from `/`, never following a symlink.
    #[cfg(unix)]
    pub(super) fn pin(path: &Path) -> io::Result<OwnedFd> {
        if !path.is_absolute() {
            return Err(io::Error::other("not an absolute path"));
        }
        let mut fd = dirfd::openat(dirfd::CWD, "/", dir_flags(), Mode::empty())?;
        for part in path.components() {
            match part {
                std::path::Component::RootDir => {}
                std::path::Component::Normal(name) => {
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

    fn fixed_volume(path: &Path) -> io::Result<()> {
        match super::volume_refusal(path) {
            None => Ok(()),
            Some(why) => Err(io::Error::other(format!(
                "refusing {}: {why}",
                path.display()
            ))),
        }
    }

    fn open_dir(parent: &OwnedFd, name: &OsStr) -> io::Result<Option<OwnedFd>> {
        match dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
            Ok(fd) => Ok(Some(fd)),
            Err(e) if code::NOT_ENTERED.contains(&e.0) || e.0 == code::NOENT => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn names(fd: &OwnedFd) -> io::Result<Vec<OsString>> {
        Ok(children(fd)?.into_iter().map(|c| c.name).collect())
    }

    struct Child {
        name: OsString,
        #[cfg(windows)]
        id: FileId,
        #[cfg(windows)]
        kind: FileType,
        #[cfg(windows)]
        allocation: u64,
    }

    impl Child {
        fn may_be_dir(&self) -> bool {
            #[cfg(unix)]
            {
                true
            }
            #[cfg(windows)]
            {
                self.kind.is_dir()
            }
        }

        fn of(fd: &OwnedFd, name: &OsStr) -> io::Result<Child> {
            #[cfg(unix)]
            {
                let _ = fd;
                Ok(Child {
                    name: name.to_os_string(),
                })
            }
            #[cfg(windows)]
            {
                let st = dirfd::fstat(fd)?;
                Ok(Child {
                    name: name.to_os_string(),
                    id: st.id(),
                    kind: st.file_type(),
                    allocation: st.allocation,
                })
            }
        }
    }

    #[cfg(unix)]
    fn children(fd: &OwnedFd) -> io::Result<Vec<Child>> {
        let mut out = Vec::new();
        for entry in Dir::read_from(fd)? {
            let entry = entry?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name != "." && name != ".." {
                out.push(Child {
                    name: name.to_os_string(),
                });
            }
        }
        Ok(out)
    }

    #[cfg(windows)]
    fn children(fd: &OwnedFd) -> io::Result<Vec<Child>> {
        let dev = dirfd::file_id(fd)?.dev;
        let mut out = Vec::new();
        for entry in Dir::read_from(fd)? {
            let entry = entry?;
            out.push(Child {
                name: entry.file_name().to_os_string(),
                id: FileId {
                    dev,
                    ino: entry.ino(),
                },
                kind: entry.file_type(),
                allocation: entry.allocation(),
            });
        }
        Ok(out)
    }

    fn open_file(parent: &OwnedFd, name: &OsStr) -> Option<std::fs::File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let file = std::fs::File::from(dirfd::openat(parent, name, flags, Mode::empty()).ok()?);
        file.metadata().ok()?.is_file().then_some(file)
    }

    fn holds_lock(fd: &OwnedFd, names: &[OsString]) -> bool {
        CARGO_LOCKS
            .iter()
            .any(|l| names.iter().any(|n| n == l) && open_file(fd, OsStr::new(l)).is_some())
    }

    fn has_layout(fd: &OwnedFd, names: &[OsString]) -> bool {
        let holds_dir = |d: &str| {
            names.iter().any(|n| n == d) && matches!(open_dir(fd, OsStr::new(d)), Ok(Some(_)))
        };
        ["deps", ".fingerprint"].iter().any(|d| holds_dir(d))
            // Targo's build-unit layout prepares these two directories instead
            // of top-level deps/.fingerprint; the SAME profile locks guard it.
            || (holds_dir("build") && holds_dir(INCREMENTAL))
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
            let entries = match children(fd) {
                Ok(n) => n,
                Err(e) => {
                    self.cut = Some(format!("{} could not be read: {e}", path.display()));
                    return;
                }
            };
            let names: Vec<OsString> = entries.iter().map(|c| c.name.clone()).collect();
            self.entries = self.entries.saturating_add(names.len());
            aterm_uds::progress::tick();
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
            for entry in &entries {
                let name = &entry.name;
                if (profile && CACHE_DIRS.iter().any(|c| name == c)) || !entry.may_be_dir() {
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

    fn take_lock(dir: &OwnedFd, name: &str, create: bool) -> io::Result<Option<HeldLock>> {
        let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if create {
            flags |= OFlags::CREATE;
            #[cfg(test)]
            if super::fault_create_lock() {
                return Err(io::Error::from_raw_os_error(code::NOSPC));
            }
        }
        let mode = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH;
        let file = std::fs::File::from(dirfd::openat(dir, name, flags, mode)?);
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("not a plain file"));
        }
        HeldLock::try_take(file)
    }

    fn take_all(dir: &OwnedFd, path: &Path) -> Result<Vec<HeldLock>, String> {
        let mut held = Vec::with_capacity(CARGO_LOCKS.len());
        let mut missing = Vec::new();
        for lock in CARGO_LOCKS {
            match take_lock(dir, lock, false) {
                Ok(Some(file)) => held.push(file),
                Ok(None) => return Err(format!("a build holds {}", path.join(lock).display())),
                Err(e) if e.raw_os_error() == Some(code::NOENT) => missing.push(*lock),
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

    fn holds_cache_at(profile: &OwnedFd) -> bool {
        matches!(open_dir(profile, OsStr::new(INCREMENTAL)), Ok(Some(inc)) if names(&inc).is_ok_and(|n| !n.is_empty()))
    }

    fn modified(file: std::fs::File) -> Option<i64> {
        file.metadata()
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
    }

    /// Read only the producer's bounded unit skeleton, never its artifact tree.
    /// An unreadable/truncated clock is unknown, not evidence that a cache is old.
    /// The same reader runs during the survey and again under all Cargo locks.
    struct Clock {
        budget: WalkBudget,
        started: Instant,
        entries: usize,
        device: u64,
        newest: Option<i64>,
    }

    impl Clock {
        fn names(&mut self, fd: &OwnedFd) -> Option<Vec<OsString>> {
            let names = names(fd).ok()?;
            self.entries = self.entries.checked_add(names.len())?;
            aterm_uds::progress::tick();
            (self.entries <= self.budget.entries && self.started.elapsed() < self.budget.wall)
                .then_some(names)
        }

        fn directory(&mut self, parent: &OwnedFd, name: &OsStr) -> Option<Option<OwnedFd>> {
            self.directory_entry(parent, name, false)
        }

        fn directory_entry(
            &mut self,
            parent: &OwnedFd,
            name: &OsStr,
            skip_plain_file: bool,
        ) -> Option<Option<OwnedFd>> {
            if self.started.elapsed() >= self.budget.wall {
                return None;
            }
            let directory = match dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
                Ok(fd) => Some(fd),
                Err(e) if e.0 == code::NOENT => None,
                Err(e) if code::NOT_ENTERED.contains(&e.0) => {
                    // Enumerated sibling files are not unit directories. A
                    // fixed clock name must be a directory when present.
                    if !skip_plain_file {
                        return None;
                    }
                    open_file(parent, name)?;
                    None
                }
                Err(_) => return None,
            };
            if let Some(fd) = directory.as_ref() {
                if dev(fd).ok()? != self.device {
                    return None;
                }
                // Reopen '.' to read metadata without consuming the pinned handle.
                let meta = open_dir(fd, OsStr::new(".")).ok()??;
                self.newest = self.newest.max(Some(modified(std::fs::File::from(meta))?));
            }
            Some(directory)
        }

        fn stamps(&mut self, directory: &OwnedFd) -> Option<()> {
            for name in self.names(directory)? {
                if self.started.elapsed() >= self.budget.wall {
                    return None;
                }
                let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
                let fd = dirfd::openat(directory, &name, flags, Mode::empty()).ok()?;
                if dev(&fd).ok()? != self.device {
                    return None;
                }
                let file = std::fs::File::from(fd);
                if !file.metadata().ok()?.is_file() {
                    return None;
                }
                self.newest = self.newest.max(Some(modified(file)?));
            }
            Some(())
        }
    }

    fn clock_at_bounded(profile: &OwnedFd, budget: WalkBudget) -> Option<i64> {
        // profile/build/package/unit/fingerprint is the deepest directory.
        if budget.depth < 4 {
            return None;
        }
        let mut clock = Clock {
            budget,
            started: Instant::now(),
            entries: 0,
            device: dev(profile).ok()?,
            newest: None,
        };
        for name in CLOCK_DIRS {
            clock.directory(profile, OsStr::new(name))?;
        }
        if let Some(build) = clock.directory(profile, OsStr::new("build"))? {
            for name in clock.names(&build)? {
                let Some(package) = clock.directory_entry(&build, &name, true)? else {
                    continue;
                };
                for name in clock.names(&package)? {
                    let Some(unit) = clock.directory_entry(&package, &name, true)? else {
                        continue;
                    };
                    // Legacy build scripts use build/<package-hash>/out. A
                    // new unit has fingerprint/out (and optionally run) at
                    // build/<package>/<unit>/. Inspect only that skeleton.
                    if let Some(fingerprint) = clock.directory(&unit, OsStr::new("fingerprint"))? {
                        // invoked.timestamp dates START; existing hash/JSON
                        // files are overwritten on successful completion, so
                        // their mtimes matter even if the directory stayed old.
                        clock.stamps(&fingerprint)?;
                        clock.directory(&unit, OsStr::new("out"))?;
                        if let Some(run) = clock.directory(&unit, OsStr::new("run"))? {
                            clock.stamps(&run)?;
                        }
                    }
                }
            }
        }
        clock.newest
    }

    fn clock_at(profile: &OwnedFd) -> Option<i64> {
        clock_at_bounded(profile, WalkBudget::SHIPPED)
    }

    pub(super) fn clock(profile: &Path, budget: WalkBudget) -> Option<i64> {
        clock_at_bounded(&pin(profile).ok()?, budget)
    }

    #[derive(Default)]
    struct Deleted {
        bytes: u64,
        kept: Vec<String>,
    }

    struct Removal<'a> {
        on: u64,
        custody: Option<(&'a OwnedFd, &'a Path)>,
        revoked: bool,
        _hook: &'a mut dyn FnMut(Stage, &Path),
        permit: Option<&'a mut dyn FnMut() -> Result<(), String>>,
    }

    impl Removal<'_> {
        fn permit(&mut self, at: (&OwnedFd, &Path), here: &Path, out: &mut Deleted) -> bool {
            if self.revoked {
                return false;
            }
            #[cfg(test)]
            (self._hook)(Stage::Removing, here);
            if let Some(permit) = &mut self.permit
                && let Err(why) = permit()
            {
                self.revoked = true;
                out.kept.push(format!("{}: {why}", here.display()));
                return false;
            }
            if let Some((root, path)) = self.custody
                && let Err(why) = ordinary_custody_at(root, path).and_then(|()| {
                    if std::ptr::eq(root, at.0) {
                        Ok(())
                    } else {
                        ordinary_custody_at(at.0, at.1)
                    }
                })
            {
                self.revoked = true;
                out.kept.push(format!("{}: {why}", here.display()));
                return false;
            }
            true
        }
    }

    #[cfg(unix)]
    fn stat_entry(
        parent: &OwnedFd,
        name: &OsStr,
        here: &Path,
        out: &mut Deleted,
    ) -> Option<dirfd::Stat> {
        match dirfd::fstatat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => Some(st),
            Err(Errno::NOENT) => None,
            Err(e) => {
                out.kept
                    .push(format!("{}: {}", here.display(), io::Error::from(e)));
                None
            }
        }
    }

    #[cfg(unix)]
    fn remove_tree_at(
        parent: &OwnedFd,
        child: &Child,
        at: &Path,
        depth: usize,
        out: &mut Deleted,
        pass: &mut Removal<'_>,
    ) {
        let name = child.name.as_os_str();
        let here = at.join(name);
        if !pass.permit((parent, at), &here, out) {
            return;
        }
        let Some(st) = stat_entry(parent, name, &here, out) else {
            return;
        };
        let kind = dirfd::FileType::from_raw_mode(st.st_mode);
        aterm_uds::progress::tick();
        if !kind.is_dir() {
            if !pass.permit((parent, at), &here, out) {
                return;
            }
            // Policy checks may perform I/O. Read the name again afterward and keep replacements.
            let Some(current) = stat_entry(parent, name, &here, out) else {
                return;
            };
            let current_kind = dirfd::FileType::from_raw_mode(current.st_mode);
            if !current_kind.is_file() && !current_kind.is_symlink() {
                pass.revoked = true;
                out.kept.push(format!(
                    "{} is not a regular file or symbolic link and was kept",
                    here.display()
                ));
                return;
            }
            if current.st_dev != st.st_dev || current.st_ino != st.st_ino {
                pass.revoked = true;
                out.kept.push(format!(
                    "{} changed while its removal was permitted and was kept",
                    here.display()
                ));
                return;
            }
            let freed = if current_kind.is_file() && current.st_nlink == 1 {
                current.st_blocks.saturating_mul(512)
            } else {
                0
            };
            match dirfd::unlinkat(parent, name, AtFlags::empty()) {
                Ok(()) => out.bytes = out.bytes.saturating_add(freed),
                Err(Errno::NOENT) => {}
                Err(e) => out
                    .kept
                    .push(format!("{}: {}", here.display(), io::Error::from(e))),
            }
            return;
        }
        let fd = match dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return,
            Err(e) => {
                return out
                    .kept
                    .push(format!("{}: {}", here.display(), io::Error::from(e)));
            }
        };
        if !pass.permit((&fd, &here), &here, out) {
            return;
        }
        if dev(&fd).ok() != Some(pass.on) {
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
        match children(&fd) {
            Ok(mut children) => {
                children.sort_by_key(|c| CARGO_LOCKS.iter().any(|l| c.name == *l));
                for child in children {
                    remove_tree_at(&fd, &child, &here, depth + 1, out, pass);
                    if pass.revoked {
                        return;
                    }
                }
            }
            Err(e) => out.kept.push(format!("{}: {e}", here.display())),
        }
        if !pass.permit((parent, at), &here, out) {
            return;
        }
        match dirfd::unlinkat(parent, name, AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(e) if (e.0 == libc::ENOTEMPTY || e.0 == libc::EEXIST) && out.kept.len() > kept => {}
            Err(e) => out
                .kept
                .push(format!("{}: {}", here.display(), io::Error::from(e))),
        }
    }

    #[cfg(windows)]
    fn remove_tree_at(
        parent: &OwnedFd,
        child: &Child,
        at: &Path,
        depth: usize,
        out: &mut Deleted,
        pass: &mut Removal<'_>,
    ) {
        let name = child.name.as_os_str();
        let here = at.join(name);
        if !pass.permit((parent, at), &here, out) {
            return;
        }
        let keep = |out: &mut Deleted, e: Errno| {
            out.kept
                .push(format!("{}: {}", here.display(), io::Error::from(e)));
        };
        if child.kind.is_foreign_reparse() {
            return keep(out, Errno::FOREIGN_REPARSE);
        }
        aterm_uds::progress::tick();
        if !child.kind.is_dir() {
            let freed = if child.kind.is_link() {
                0
            } else {
                released_at(parent, child)
            };
            if !pass.permit((parent, at), &here, out) {
                return;
            }
            match dirfd::unlinkat(parent, name, AtFlags::empty(), child.id) {
                Ok(()) => out.bytes = out.bytes.saturating_add(freed),
                Err(Errno::NOENT | Errno::DELETE_PENDING) => {}
                Err(Errno::MOUNT_POINT) => out.kept.push(format!(
                    "{} is a volume's mount point, on another device, and was not entered or removed",
                    here.display()
                )),
                Err(e) => keep(out, e),
            }
            return;
        }
        let fd = match dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::DELETE_PENDING) => return,
            Err(e) if code::NOT_ENTERED.contains(&e.0) => return keep(out, Errno::CHANGED),
            Err(e) => return keep(out, e),
        };
        match dirfd::file_id(&fd) {
            Ok(id) if id == child.id => {}
            Ok(_) => return keep(out, Errno::CHANGED),
            Err(e) => return keep(out, e),
        }
        if !pass.permit((&fd, &here), &here, out) {
            return;
        }
        if dev(&fd).ok() != Some(pass.on) {
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
        match children(&fd) {
            Ok(mut children) => {
                children.sort_by_key(|c| CARGO_LOCKS.iter().any(|l| c.name == *l));
                for grandchild in children {
                    remove_tree_at(&fd, &grandchild, &here, depth + 1, out, pass);
                    if pass.revoked {
                        return;
                    }
                }
            }
            Err(e) => out.kept.push(format!("{}: {e}", here.display())),
        }
        if !pass.permit((parent, at), &here, out) {
            return;
        }
        match dirfd::unlinkat(parent, name, AtFlags::REMOVEDIR, child.id) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(Errno::NOTEMPTY) if out.kept.len() > kept => {}
            Err(e) => keep(out, e),
        }
    }

    #[cfg(windows)]
    fn released_at(parent: &OwnedFd, child: &Child) -> u64 {
        if child.allocation == 0 {
            return 0;
        }
        let Ok(fd) = dirfd::openat(
            parent,
            &child.name,
            OFlags::PATH | OFlags::NOFOLLOW,
            Mode::empty(),
        ) else {
            return 0;
        };
        match dirfd::fstat(&fd) {
            Ok(st) if st.id() == child.id && st.st_nlink == 1 => st.allocation,
            _ => 0,
        }
    }

    /// Remove the one entry at the absolute `path`.
    #[cfg(windows)]
    pub(super) fn remove_entry(path: &Path) -> io::Result<u64> {
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::other(format!(
                "refusing to remove {}: not a plain absolute path",
                path.display()
            )));
        };
        let refused = |kind: io::ErrorKind, why: String| {
            io::Error::new(
                kind,
                format!("refusing to remove {}: {why}", path.display()),
            )
        };
        if let Some(why) = super::off_fixed_disk(folder) {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                format!("{why} — only a fixed disk's Claude Code versions are removed"),
            ));
        }
        let parent = dirfd::pin(folder).map_err(|e| match e {
            Errno::FOREIGN_REPARSE => refused(
                io::ErrorKind::InvalidInput,
                format!("it {}", super::IN_SYNC_ROOT),
            ),
            e => refused(
                io::Error::from(e).kind(),
                format!(
                    "its folder could not be opened from its volume's root without passing a link or another reparse point ({e})"
                ),
            ),
        })?;
        let on = dev(&parent)?;
        let Some(child) = children(&parent)?.into_iter().find(|c| c.name == name) else {
            return Err(refused(
                io::ErrorKind::NotFound,
                "it is gone (its folder no longer lists it)".to_owned(),
            ));
        };
        if child.kind.is_link() {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                "it is a symlink or junction (nothing is removed through a link)".to_owned(),
            ));
        }
        if child.kind.is_foreign_reparse() {
            return Err(refused(
                io::ErrorKind::InvalidInput,
                format!("it {}", super::IN_SYNC_ROOT),
            ));
        }
        let mut deleted = Deleted::default();
        let mut hook = |_, _: &Path| {};
        let mut pass = Removal {
            on,
            custody: None,
            revoked: false,
            _hook: &mut hook,
            permit: None,
        };
        remove_tree_at(&parent, &child, folder, 0, &mut deleted, &mut pass);
        if deleted.kept.is_empty() {
            Ok(deleted.bytes)
        } else {
            Err(io::Error::other(format!(
                "{} was removed only in part: {}",
                path.display(),
                deleted.kept.join("; ")
            )))
        }
    }

    fn identity(fd: &OwnedFd) -> io::Result<aterm_sysprobe::volumes::FileId> {
        let st = dirfd::fstat(fd)?;
        Ok(aterm_sysprobe::volumes::FileId {
            dev: st.st_dev,
            #[cfg(unix)]
            ino: u128::from(st.st_ino),
            #[cfg(windows)]
            ino: st.st_ino,
        })
    }

    struct CustodyWalk {
        budget: WalkBudget,
        started: Instant,
        entries: usize,
        on: u64,
    }

    impl CustodyWalk {
        fn timely(&self) -> Result<(), String> {
            if self.started.elapsed() >= self.budget.wall {
                Err("its managed-custody scan ran out of time".into())
            } else {
                Ok(())
            }
        }

        fn visit(&mut self, fd: &OwnedFd, path: &Path, depth: usize) -> Result<(), String> {
            self.timely()?;
            marker_absent_at(fd)?;
            let entries = Dir::read_from(fd).map_err(|e| e.to_string())?;
            for entry in entries {
                self.timely()?;
                let entry = entry.map_err(|e| e.to_string())?;
                #[cfg(unix)]
                let name = OsStr::from_bytes(entry.file_name().to_bytes());
                #[cfg(windows)]
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                self.entries = self.entries.saturating_add(1);
                if self.entries > self.budget.entries {
                    return Err("its managed-custody scan reached the entry bound".into());
                }
                #[cfg(unix)]
                {
                    let st = dirfd::fstatat(fd, name, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|e| format!("{}: {e}", path.join(name).display()))?;
                    let kind = dirfd::FileType::from_raw_mode(st.st_mode);
                    if kind.is_file() || kind.is_symlink() {
                        continue;
                    }
                    if !kind.is_dir() {
                        return Err(format!(
                            "{} is a special filesystem entry, so its cache is kept",
                            path.join(name).display()
                        ));
                    }
                }
                let child = match dirfd::openat(fd, name, dir_flags(), Mode::empty()) {
                    Ok(child) => child,
                    Err(error) => {
                        #[cfg(unix)]
                        let leaf = code::NOT_ENTERED.contains(&error.0);
                        #[cfg(windows)]
                        let leaf = matches!(error, Errno::NOTDIR | Errno::LOOP);
                        if leaf {
                            continue;
                        }
                        return Err(format!(
                            "{} could not be pinned: {error}",
                            path.join(name).display()
                        ));
                    }
                };
                if dev(&child).map_err(|e| e.to_string())? != self.on {
                    return Err("its managed-custody scan reached another device".into());
                }
                if depth >= self.budget.depth.min(MAX_CUSTODY_DEPTH) {
                    return Err("its managed-custody scan reached the depth bound".into());
                }
                self.visit(&child, &path.join(name), depth + 1)?;
            }
            self.timely()
        }
    }

    pub(super) fn unmanaged_custody(path: &Path, budget: WalkBudget) -> Result<(), String> {
        let started = Instant::now();
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return Err("not a plain absolute cache entry".into());
        };
        fixed_volume(path).map_err(|e| e.to_string())?;
        let parent = pin(folder).map_err(|e| e.to_string())?;
        let root = dirfd::openat(
            &parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|e| e.to_string())?;
        let stat = dirfd::fstat(&root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let kind = dirfd::FileType::from_raw_mode(stat.st_mode);
        #[cfg(windows)]
        let kind = stat.file_type();
        let mut walk = CustodyWalk {
            budget,
            started,
            entries: 0,
            on: stat.st_dev,
        };
        if kind.is_dir() {
            ordinary_custody_at(&root, path)?;
            walk.visit(&root, path, 0)
        } else if kind.is_file() {
            ordinary_custody_at(&parent, folder)?;
            walk.timely()
        } else {
            Err("not a regular file or directory".into())
        }
    }

    pub(super) fn remove_unmanaged_tree(
        path: &Path,
        expected: aterm_sysprobe::volumes::FileId,
        permit: &mut dyn FnMut() -> Result<(), String>,
    ) -> io::Result<Removed> {
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(io::Error::other("not a plain absolute cache entry"));
        };
        fixed_volume(path)?;
        let parent = pin(folder)?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let root = dirfd::openat(&parent, name, flags, Mode::empty())?;
        if identity(&root)? != expected {
            return Err(io::Error::other(
                "its identity changed after its fences were read",
            ));
        }
        let st = dirfd::fstat(&root)?;
        #[cfg(unix)]
        let kind = dirfd::FileType::from_raw_mode(st.st_mode);
        #[cfg(windows)]
        let kind = st.file_type();
        if !kind.is_dir() && !kind.is_file() {
            return Err(io::Error::other("not a regular file or directory"));
        }
        let custody = if kind.is_dir() {
            (&root, path)
        } else {
            (&parent, folder)
        };
        ordinary_custody_at(custody.0, custody.1).map_err(io::Error::other)?;
        permit().map_err(io::Error::other)?;
        let mut hook = |_, _: &Path| {};
        let mut pass = Removal {
            on: expected.dev,
            custody: Some(custody),
            revoked: false,
            _hook: &mut hook,
            permit: Some(permit),
        };
        let mut deleted = Deleted::default();
        if kind.is_dir() {
            for child in children(&root)? {
                remove_tree_at(&root, &child, path, 1, &mut deleted, &mut pass);
                if pass.revoked {
                    break;
                }
            }
        }
        let mut gone = false;
        if deleted.kept.is_empty() && pass.permit(custody, path, &mut deleted) {
            let named = dirfd::openat(&parent, name, flags, Mode::empty())
                .map_err(io::Error::from)
                .and_then(|named| identity(&named));
            match named {
                Ok(id) if id == expected => {
                    let removed: io::Result<u64> = if kind.is_dir() {
                        rmdir_named(&parent, name, &root)
                            .map(|()| 0)
                            .map_err(Into::into)
                    } else {
                        dirfd::fstat(&root)
                            .map_err(io::Error::from)
                            .and_then(|current| {
                                #[cfg(unix)]
                                let freed = if current.st_nlink == 1 {
                                    current.st_blocks.saturating_mul(512)
                                } else {
                                    0
                                };
                                #[cfg(windows)]
                                let freed = if current.st_nlink == 1 {
                                    current.allocation
                                } else {
                                    0
                                };
                                #[cfg(unix)]
                                let removed = dirfd::unlinkat(&parent, name, AtFlags::empty());
                                #[cfg(windows)]
                                let removed =
                                    dirfd::unlinkat(&parent, name, AtFlags::empty(), current.id());
                                removed.map(|()| freed).map_err(Into::into)
                            })
                    };
                    match removed {
                        Ok(freed) => {
                            gone = true;
                            deleted.bytes = deleted.bytes.saturating_add(freed);
                        }
                        Err(error) => deleted.kept.push(format!("{}: {error}", path.display())),
                    }
                }
                Ok(_) => deleted
                    .kept
                    .push("its name now names another entry, which is left".into()),
                Err(error) => deleted.kept.push(format!("{}: {error}", path.display())),
            }
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

    fn rmdir_named(parent: &OwnedFd, name: &OsStr, dir: &OwnedFd) -> Result<(), Errno> {
        #[cfg(unix)]
        {
            let _ = dir;
            dirfd::unlinkat(parent, name, AtFlags::REMOVEDIR)
        }
        #[cfg(windows)]
        {
            dirfd::unlinkat(parent, name, AtFlags::REMOVEDIR, dirfd::file_id(dir)?)
        }
    }

    struct Profiled {
        bytes: u64,
        gone: bool,
        kept: Vec<String>,
        revoked: bool,
    }

    fn reclaim_profile(
        profile: &OwnedFd,
        custody: (&OwnedFd, &Path),
        path: &Path,
        on: u64,
        judge: &Judge,
        select: Select<'_>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> Result<Profiled, String> {
        let held = take_all(profile, path)?;
        hook(Stage::Locked, path);
        let names = names(profile).map_err(|e| format!("it could not be read: {e}"))?;
        if !has_layout(profile, &names) {
            return Err("no longer a cargo profile".to_owned());
        }
        if let Some(why) = select.refuses(clock_at(profile), judge) {
            return Err(why);
        }
        let inc = OsStr::new(INCREMENTAL);
        let target = match open_dir(profile, inc) {
            Ok(Some(fd)) if dev(&fd).ok() == Some(on) => {
                if self::names(&fd).is_ok_and(|n| n.is_empty()) {
                    return Err("its incremental/ is empty — nothing to reclaim".to_owned());
                }
                Child::of(&fd, inc)
                    .map_err(|e| format!("its incremental/ could not be read: {e}"))?
            }
            Ok(Some(_)) => return Err("its incremental/ is on another device".to_owned()),
            Ok(None) => return Err("it no longer holds a real incremental/".to_owned()),
            Err(e) => return Err(format!("its incremental/ could not be opened: {e}")),
        };
        hook(Stage::Deleting, path);
        if let Err(why) = ordinary_custody_at(custody.0, custody.1) {
            drop(held);
            hook(Stage::Released, path);
            return Ok(Profiled {
                bytes: 0,
                gone: false,
                kept: vec![format!("{}: {why}", path.display())],
                revoked: true,
            });
        }
        let mut deleted = Deleted::default();
        let revoked = {
            let mut pass = Removal {
                on,
                custody: Some(custody),
                revoked: false,
                _hook: hook,
                permit: None,
            };
            remove_tree_at(profile, &target, path, 0, &mut deleted, &mut pass);
            pass.revoked
        };
        let gone = matches!(open_dir(profile, inc), Ok(None));
        drop(held);
        hook(Stage::Released, path);
        Ok(Profiled {
            bytes: deleted.bytes,
            gone,
            kept: deleted.kept,
            revoked,
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
        fixed_volume(path)?;
        let parent_fd = pin(parent).map_err(gone_since)?;
        let root = match dirfd::openat(&parent_fd, name, dir_flags(), Mode::empty()) {
            Ok(fd) => fd,
            Err(e) if e.0 == code::NOENT => return Err(gone_since(e.into())),
            Err(e) if code::NOT_ENTERED.contains(&e.0) => {
                return Err(io::Error::other(
                    "no longer a candidate: not a directory (nothing is reached through a link)",
                ));
            }
            Err(e) => return Err(e.into()),
        };
        let root_dev = on_measured_volume(&root, judge)?;
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
            volume: None,
            probed_recent: false,
            origin: super::Origin::Named,
            owned: super::Owned::NotOwned,
            total: None,
            foreign: None,
            idle: None,
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
        let mut held = Vec::new();
        for profile in &profiles {
            let fd = reopen(&root, path, profile, root_dev).map_err(|e| refuse(e.to_string()))?;
            held.extend(take_all(&fd, profile).map_err(refuse)?);
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
        check().map_err(refuse)?;
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
        ordinary_custody_at(&root, path).map_err(refuse)?;
        let mut deleted = Deleted::default();
        let mut pass = Removal {
            on: root_dev,
            custody: Some((&root, path)),
            revoked: false,
            _hook: hook,
            permit: None,
        };
        match children(&root) {
            Ok(mut children) => {
                children.sort_by_key(|c| CARGO_LOCKS.iter().any(|l| c.name == *l));
                for child in children {
                    remove_tree_at(&root, &child, path, 1, &mut deleted, &mut pass);
                    if pass.revoked {
                        break;
                    }
                }
            }
            Err(e) => deleted.kept.push(format!("{}: {e}", path.display())),
        }
        let mut gone = false;
        if deleted.kept.is_empty() && pass.permit((&root, path), path, &mut deleted) {
            match named(&parent_fd) {
                Ok(true) => match rmdir_named(&parent_fd, name, &root) {
                    Ok(()) => gone = true,
                    Err(e) => {
                        deleted
                            .kept
                            .push(format!("{}: {}", path.display(), io::Error::from(e)))
                    }
                },
                Ok(false) => deleted.kept.push(format!(
                    "{}: its name now names another directory, which is left",
                    path.display()
                )),
                Err(e) => deleted.kept.push(format!(
                    "{}: its final name could not be inspected: {e}",
                    path.display()
                )),
            }
        }

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

    /// What `parts`, a path inside a profile, names by the compiler's rules: `Some(Some(lock))`
    /// an unfinished incremental session (`incremental/<crate>/<session>`) and its lock file's
    /// name; `Some(None)` a metadata temporary folder in `deps/`, `examples/`, `build/<unit>/`,
    /// `build/<unit>/out/` or `build/<unit>/<run>/out/`; `None` anything else
    /// ([`super::died_output`] reads the same).
    fn died_kind(parts: &[&OsStr]) -> Option<Option<String>> {
        let text: Vec<&str> = parts.iter().map(|p| p.to_str()).collect::<Option<_>>()?;
        match text.as_slice() {
            [inc, _, session] if *inc == INCREMENTAL => {
                super::unfinished_session_lock(session).map(Some)
            }
            ["deps" | "examples", name]
            | ["build", _, name]
            | ["build", _, "out", name]
            | ["build", _, _, "out", name]
                if super::metadata_temp(name) =>
            {
                Some(None)
            }
            _ => None,
        }
    }

    /// The modification time (unix seconds) of the folder `fd` holds open.
    fn modified_at(fd: &OwnedFd) -> io::Result<i64> {
        let file = std::fs::File::from(dirfd::openat(fd, ".", dir_flags(), Mode::empty())?);
        let at = file.metadata()?.modified()?;
        Ok(at
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0))
    }

    /// The folder `dir` (at `path`) opened to have its times put back, with the access and
    /// modification times it has now.
    #[cfg(unix)]
    fn times_of(dir: &OwnedFd, _: &Path) -> io::Result<(std::fs::File, std::fs::FileTimes)> {
        let file = std::fs::File::from(dirfd::openat(dir, ".", dir_flags(), Mode::empty())?);
        let meta = file.metadata()?;
        let times = std::fs::FileTimes::new()
            .set_accessed(meta.accessed()?)
            .set_modified(meta.modified()?);
        Ok((file, times))
    }

    /// [`times_of`] on Windows: a folder's handle opened by a pinned descriptor may not write
    /// attributes, so the folder is opened by `path` (as itself, never through a reparse point)
    /// and held to `dir`'s identity.
    #[cfg(windows)]
    fn times_of(dir: &OwnedFd, path: &Path) -> io::Result<(std::fs::File, std::fs::FileTimes)> {
        use crate::harness::disk::win::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES};
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsHandle as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let mut open = std::fs::OpenOptions::new();
        open.access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
        let file = aterm_uds::kept_out(|| open.open(path))?;
        let want = dirfd::file_id(dir)?;
        let opened = aterm_sysprobe::volumes::file_id_of_handle(file.as_handle());
        if opened.map(|id| (id.dev, id.ino)) != Some((want.dev, want.ino)) {
            return Err(io::Error::other("its name now names another folder"));
        }
        let meta = file.metadata()?;
        let times = std::fs::FileTimes::new()
            .set_accessed(meta.accessed()?)
            .set_modified(meta.modified()?);
        Ok((file, times))
    }

    /// Remove one piece of a died build's output ([`super::reclaim_died`]).
    pub(super) fn reclaim_died(
        path: &Path,
        (profile, entry): (&Path, &Path),
        judge: &Judge,
        seen: i64,
        check: &mut dyn FnMut() -> Result<(), String>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        let refuse = |why: String| io::Error::other(format!("no longer a candidate: {why}"));
        let mut parts = Vec::new();
        for part in entry
            .strip_prefix(profile)
            .map_err(|_| refuse("it is not inside its profile".to_owned()))?
            .components()
        {
            let std::path::Component::Normal(name) = part else {
                return Err(refuse("not a plain path".to_owned()));
            };
            parts.push(name);
        }
        let Some(lock_name) = died_kind(&parts) else {
            return Err(refuse("its name is not one a died build leaves".to_owned()));
        };
        let Some((name, folders)) = parts.split_last() else {
            return Err(refuse("it is its profile".to_owned()));
        };
        fixed_volume(path)?;
        let root = pin(path).map_err(gone_since)?;
        let root_dev = on_measured_volume(&root, judge)?;
        let (marker, holds_source) = root_facts(path);
        let (profiles, cut) = find_at(&root, path, WalkBudget::SHIPPED);
        TargetDir {
            path: path.to_path_buf(),
            marker,
            holds_source,
            walk_cut_short: cut,
            profiles: Vec::new(),
            device: Some(root_dev),
            volume: None,
            probed_recent: false,
            origin: super::Origin::Named,
            owned: super::Owned::NotOwned,
            total: None,
            foreign: None,
            idle: None,
        }
        .fences()
        .map_err(refuse)?;
        if !profiles.iter().any(|p| same_path(p, profile)) {
            return Err(refuse(
                "not one of this build directory's cargo profiles".to_owned(),
            ));
        }
        let profile_fd =
            reopen(&root, path, profile, root_dev).map_err(|e| refuse(e.to_string()))?;
        let held = take_all(&profile_fd, profile).map_err(refuse)?;
        let listed =
            names(&profile_fd).map_err(|e| refuse(format!("it could not be read: {e}")))?;
        if !has_layout(&profile_fd, &listed) {
            return Err(refuse("no longer a cargo profile".to_owned()));
        }
        let mut parent = dirfd::openat(&profile_fd, ".", dir_flags(), Mode::empty())?;
        let mut parent_path = profile.to_path_buf();
        for folder in folders {
            parent_path.push(folder);
            parent = match open_dir(&parent, folder)? {
                Some(fd) if dev(&fd)? == root_dev => fd,
                Some(_) => {
                    return Err(refuse(format!(
                        "{} is on another device",
                        parent_path.display()
                    )));
                }
                None => {
                    return Err(refuse(format!(
                        "{} is no longer a folder",
                        parent_path.display()
                    )));
                }
            };
        }
        let child = match open_dir(&parent, name)? {
            Some(fd) if dev(&fd)? == root_dev => fd,
            Some(_) => return Err(refuse("it is on another device".to_owned())),
            None => return Err(gone_since(io::ErrorKind::NotFound.into())),
        };
        // A session's own lock: a rustc that still holds it is still writing the session.
        let session = match &lock_name {
            Some(lock) => match take_lock(&parent, lock, false) {
                Ok(Some(taken)) => Some(taken),
                Ok(None) => return Err(refuse(format!("a rustc holds its session lock {lock}"))),
                Err(e) if e.raw_os_error() == Some(code::NOENT) => {
                    return Err(refuse("its session lock is gone".to_owned()));
                }
                Err(e) => {
                    return Err(refuse(format!("its session lock could not be taken: {e}")));
                }
            },
            None => None,
        };
        hook(Stage::Locked, entry);
        let modified =
            modified_at(&child).map_err(|e| refuse(format!("its age could not be read: {e}")))?;
        if modified > seen {
            return Err(refuse("it was written into since the survey".to_owned()));
        }
        let min_age = if lock_name.is_some() {
            DIED_SESSION_MIN_AGE_S
        } else {
            DIED_METADATA_MIN_AGE_S
        };
        let age = judge.now.saturating_sub(modified);
        if age < min_age {
            return Err(refuse(format!(
                "it was written into {} ago",
                super::human_age(age)
            )));
        }
        check().map_err(refuse)?;
        let child_entry = Child::of(&child, name).map_err(|e| refuse(e.to_string()))?;
        let times = times_of(&parent, &parent_path);
        hook(Stage::Deleting, entry);
        if let Err(why) = ordinary_custody_at(&root, path) {
            drop(session);
            drop(held);
            hook(Stage::Released, entry);
            return Err(refuse(why));
        }
        let mut deleted = Deleted::default();
        {
            let mut pass = Removal {
                on: root_dev,
                custody: Some((&root, path)),
                revoked: false,
                _hook: hook,
                permit: None,
            };
            remove_tree_at(
                &parent,
                &child_entry,
                &parent_path,
                0,
                &mut deleted,
                &mut pass,
            );
        }
        let gone = matches!(open_dir(&parent, name), Ok(None));
        match times.and_then(|(folder, times)| folder.set_times(times)) {
            Ok(()) => {}
            Err(e) => deleted.kept.push(format!(
                "{}: its times could not be put back, so its profile reads as written now: {e}",
                parent_path.display()
            )),
        }
        drop(session);
        drop(held);
        hook(Stage::Released, entry);
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
                vec![entry.to_path_buf()]
            } else {
                Vec::new()
            },
            skipped: Vec::new(),
            trouble: deleted.kept,
        })
    }

    fn content_rank(name: &OsStr) -> u8 {
        match name.to_str() {
            Some(".fingerprint") => 0,
            Some("build") => 1,
            Some("deps") => 2,
            Some("incremental") => 3,
            Some("examples") => 4,
            _ => 5,
        }
    }

    #[cfg(windows)]
    fn same_artifact(left: &std::fs::File, right: &std::fs::File) -> bool {
        use std::os::windows::fs::FileExt as _;

        let (Ok(a), Ok(b)) = (dirfd::fstat(left), dirfd::fstat(right)) else {
            return false;
        };
        if a.size != b.size {
            return false;
        }
        let read_exact_at = |file: &std::fs::File, buf: &mut [u8], mut at: u64| {
            let mut filled = 0;
            while filled < buf.len() {
                match file.seek_read(&mut buf[filled..], at) {
                    Ok(0) | Err(_) => return false,
                    Ok(n) => {
                        filled += n;
                        at += n as u64;
                    }
                }
            }
            true
        };
        if a.id() != b.id() {
            let mut at = 0;
            let mut left_bytes = [0u8; 8192];
            let mut right_bytes = [0u8; 8192];
            while at < a.size {
                aterm_uds::progress::tick();
                let count = (a.size - at).min(left_bytes.len() as u64) as usize;
                if !read_exact_at(left, &mut left_bytes[..count], at)
                    || !read_exact_at(right, &mut right_bytes[..count], at)
                    || left_bytes[..count] != right_bytes[..count]
                {
                    return false;
                }
                at += count as u64;
            }
        }
        let stamp = |s: &dirfd::Stat| (s.id(), s.size, s.last_write, s.change_time);
        dirfd::fstat(left).is_ok_and(|after| stamp(&a) == stamp(&after))
            && dirfd::fstat(right).is_ok_and(|after| stamp(&b) == stamp(&after))
    }

    #[cfg(unix)]
    fn same_artifact(left: &std::fs::File, right: &std::fs::File) -> bool {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _};

        let (Ok(a), Ok(b)) = (left.metadata(), right.metadata()) else {
            return false;
        };
        if a.len() != b.len() {
            return false;
        }
        if a.dev() != b.dev() || a.ino() != b.ino() {
            let mut at = 0;
            let mut left_bytes = [0u8; 8192];
            let mut right_bytes = [0u8; 8192];
            while at < a.len() {
                aterm_uds::progress::tick();
                let count = (a.len() - at).min(left_bytes.len() as u64) as usize;
                if left.read_exact_at(&mut left_bytes[..count], at).is_err()
                    || right.read_exact_at(&mut right_bytes[..count], at).is_err()
                    || left_bytes[..count] != right_bytes[..count]
                {
                    return false;
                }
                at += count as u64;
            }
        }
        let stamp = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        left.metadata()
            .is_ok_and(|after| stamp(&a) == stamp(&after))
            && right
                .metadata()
                .is_ok_and(|after| stamp(&b) == stamp(&after))
    }

    fn twin_named(name: &str, dep: &str) -> bool {
        #[cfg(unix)]
        {
            super::uplift_twin(name, dep)
        }
        #[cfg(windows)]
        {
            super::uplift_twin(name, dep) || super::unhashed_twin(name, dep)
        }
    }

    fn cargo_output(profile: &OwnedFd, entry: &Child, deps: &[OsString]) -> bool {
        let name = entry.name.as_os_str();
        if super::PROFILE_OUTPUTS.iter().any(|o| name == *o) {
            return true;
        }
        let Some(text) = name.to_str() else {
            return false;
        };
        let plain = open_file(profile, name).filter(|f| listed(f, entry));
        if text.len() > 2 && text.ends_with(".d") && plain.is_some() {
            return true;
        }
        let twins: Vec<&OsString> = deps
            .iter()
            .filter(|d| d.to_str().is_some_and(|d| twin_named(text, d)))
            .collect();
        if twins.is_empty() {
            return false;
        }
        match plain {
            Some(file) => {
                let Ok(Some(deps_fd)) = open_dir(profile, OsStr::new("deps")) else {
                    return false;
                };
                twins.iter().any(|twin| {
                    open_file(&deps_fd, twin).is_some_and(|twin| same_artifact(&file, &twin))
                })
            }
            None => is_link(profile, entry),
        }
    }

    fn listed(file: &std::fs::File, entry: &Child) -> bool {
        #[cfg(unix)]
        {
            let _ = (file, entry);
            true
        }
        #[cfg(windows)]
        {
            dirfd::fstat(file).is_ok_and(|st| st.id() == entry.id)
        }
    }

    fn is_link(profile: &OwnedFd, entry: &Child) -> bool {
        #[cfg(unix)]
        {
            matches!(
                dirfd::openat(
                    profile,
                    &entry.name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty()
                ),
                Err(e) if e.0 == code::LOOP
            )
        }
        #[cfg(windows)]
        {
            dirfd::openat(profile, &entry.name, OFlags::PATH, Mode::empty())
                .and_then(|fd| dirfd::fstat(&fd))
                .is_ok_and(|st| st.id() == entry.id && st.file_type().is_link())
        }
    }

    /// Empty the profiles of the build directory at `path` ([`super::reclaim_profiles`]).
    pub(super) fn reclaim_profiles(
        path: &Path,
        judge: &Judge,
        (seen, window_s): (i64, i64),
        mounts: &mut dyn FnMut() -> Result<(), String>,
        between: &mut dyn FnMut() -> Result<(), String>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        fixed_volume(path)?;
        let root = pin(path).map_err(gone_since)?;
        let root_dev = on_measured_volume(&root, judge)?;
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
            volume: None,
            probed_recent: false,
            origin: super::Origin::Named,
            owned: super::Owned::NotOwned,
            total: None,
            foreign: None,
            idle: None,
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
        let mut done = Removed::default();
        let mut stop: Option<(usize, String)> = None;
        for (i, profile) in profiles.iter().enumerate() {
            let fd = match reopen(&root, path, profile, root_dev) {
                Ok(fd) => fd,
                Err(e) => {
                    stop = Some((i, e.to_string()));
                    break;
                }
            };
            let held = match take_all(&fd, profile) {
                Ok(held) => held,
                Err(why) => {
                    stop = Some((i, why));
                    break;
                }
            };
            hook(Stage::Locked, profile);
            let entries = match children(&fd) {
                Ok(n) => n,
                Err(e) => {
                    stop = Some((i, format!("it could not be read: {e}")));
                    break;
                }
            };
            let names: Vec<OsString> = entries.iter().map(|c| c.name.clone()).collect();
            let refused = if !has_layout(&fd, &names) {
                Some("no longer a cargo profile".to_owned())
            } else {
                match clock_at(&fd) {
                    None => Some("its caches' clock could not be read".to_owned()),
                    Some(m) if m > seen => Some("written into since the survey".to_owned()),
                    Some(m) if judge.now.saturating_sub(m) < window_s => Some(format!(
                        "written into {} ago, inside the {} window",
                        super::human_age(judge.now.saturating_sub(m)),
                        super::human_age(window_s)
                    )),
                    Some(_) => mounts().err().or_else(|| between().err()),
                }
            };
            if let Some(why) = refused {
                stop = Some((i, why));
                break;
            }
            hook(Stage::Deleting, profile);
            if let Err(why) = ordinary_custody_at(&root, path) {
                stop = Some((i, why));
                break;
            }
            let deps = open_dir(&fd, OsStr::new("deps"))
                .ok()
                .flatten()
                .and_then(|d| self::names(&d).ok())
                .unwrap_or_default();
            let mut order: Vec<Child> = Vec::new();
            for entry in entries {
                aterm_uds::progress::tick();
                if CARGO_LOCKS.iter().any(|l| entry.name == *l) {
                    continue;
                }
                if cargo_output(&fd, &entry, &deps) {
                    order.push(entry);
                } else {
                    done.skipped.push((
                        profile.join(&entry.name),
                        "kept: not one of cargo's outputs (tier 3 empties only its caches, \
                         examples/, dep-info files and uplifted artifacts)"
                            .to_owned(),
                    ));
                }
            }
            order.sort_by_key(|c| content_rank(&c.name));
            let mut deleted = Deleted::default();
            let revoked = {
                let mut pass = Removal {
                    on: root_dev,
                    custody: Some((&root, path)),
                    revoked: false,
                    _hook: hook,
                    permit: None,
                };
                for entry in order {
                    remove_tree_at(&fd, &entry, profile, 1, &mut deleted, &mut pass);
                    if pass.revoked {
                        break;
                    }
                }
                pass.revoked
            };
            drop(held);
            hook(Stage::Released, profile);
            done.bytes = done.bytes.saturating_add(deleted.bytes);
            if deleted.kept.is_empty() {
                done.units.push(profile.clone());
            }
            done.trouble.extend(deleted.kept);
            if revoked {
                stop = Some((
                    i + 1,
                    "cleanup stopped because build-cache custody could no longer be established"
                        .into(),
                ));
                break;
            }
        }
        if let Some((from, why)) = stop {
            for p in &profiles[from..] {
                done.skipped.push((p.clone(), why.clone()));
            }
        }
        if done.units.is_empty() && done.bytes == 0 {
            let mut why: Vec<String> = done
                .skipped
                .iter()
                .map(|(p, w)| format!("{}: {w}", p.display()))
                .collect();
            why.extend(done.trouble.iter().cloned());
            return Err(io::Error::other(format!(
                "nothing reclaimed — {}",
                why.join("; ")
            )));
        }
        Ok(done)
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        #[cfg(unix)]
        {
            a == b
        }
        #[cfg(windows)]
        {
            use aterm_sysprobe::paths::windows_key;
            windows_key(a).is_some_and(|k| windows_key(b).is_some_and(|l| k == l))
        }
    }

    /// Reclaim the build directory at `path`: the profiles `select` names.
    pub(super) fn reclaim(
        path: &Path,
        judge: &Judge,
        select: Select<'_>,
        hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        fixed_volume(path)?;
        let root = pin(path).map_err(gone_since)?;
        let root_dev = on_measured_volume(&root, judge)?;
        let (marker, holds_source) = root_facts(path);
        let (profiles, cut) = find_at(&root, path, WalkBudget::SHIPPED);
        TargetDir {
            path: path.to_path_buf(),
            marker,
            holds_source,
            walk_cut_short: cut,
            profiles: Vec::new(),
            device: Some(root_dev),
            volume: None,
            probed_recent: false,
            origin: super::Origin::Named,
            owned: super::Owned::NotOwned,
            total: None,
            foreign: None,
            idle: None,
        }
        .fences()
        .map_err(|why| io::Error::other(format!("no longer a candidate: {why}")))?;
        let (wanted, leave) = match select {
            Select::Idle { leave } => (None, leave),
            Select::Lru { profile, .. } => (Some(profile), &[][..]),
        };
        if wanted.is_some_and(|w| !profiles.iter().any(|p| same_path(p, w))) {
            return Err(io::Error::other(
                "no longer a candidate: not one of this build directory's cargo profiles",
            ));
        }
        let mut done = Removed::default();
        for profile in &profiles {
            // Not the one asked for, or a profile of a build directory named
            // inside this one, which decides its own (one that cannot be
            // placed is left too): passed over, no lock taken.
            if wanted.is_some_and(|w| !same_path(profile, w))
                || leave
                    .iter()
                    .any(|l| aterm_sysprobe::paths::is_under(profile, l) != Some(false))
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
            match reclaim_profile(&fd, (&root, path), profile, root_dev, judge, select, hook) {
                Ok(p) => {
                    done.bytes = done.bytes.saturating_add(p.bytes);
                    if p.gone {
                        done.units.push(profile.join(INCREMENTAL));
                    }
                    done.trouble.extend(p.kept);
                    if p.revoked {
                        break;
                    }
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

    /// What a report walk counts of each file ([`tally`]).
    #[cfg(windows)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Count {
        Shared,
        Released,
        Nothing,
    }

    /// One report walk's asks and bounds ([`tally`]).
    #[cfg(windows)]
    pub(super) struct Walk {
        /// What each file counts.
        pub(super) count: Count,
        /// Read every entry's owner (the root's too).
        pub(super) owners: bool,
        /// Entries it may read.
        pub(super) max: usize,
        /// Wall clock it may spend, when bounded.
        pub(super) wall: Option<std::time::Duration>,
        /// The device it never leaves; `None`: the root's own.
        pub(super) dev: Option<u64>,
    }

    #[cfg(windows)]
    impl Walk {
        /// A sizing walk (no owners) within [`super::MAX_WALK_ENTRIES`] and no wall: the bounds of the unix sizing walk.
        pub(super) fn sizes(count: Count, dev: Option<u64>) -> Walk {
            Walk {
                count,
                owners: false,
                max: super::MAX_WALK_ENTRIES,
                wall: None,
                dev,
            }
        }
    }

    /// What a report walk read.
    #[cfg(windows)]
    #[derive(Debug, Default)]
    pub(super) struct Tally {
        /// What its files count ([`Walk::count`]).
        pub(super) bytes: u64,
        /// It stopped short or could not read a folder: `bytes` is a floor.
        pub(super) partial: bool,
        /// Why it stopped short, when it did.
        pub(super) cut: Option<String>,
        /// When [`Walk::owners`].
        pub(super) foreign: Option<String>,
    }

    #[cfg(all(test, windows))]
    thread_local! {
        static THEIRS: std::cell::RefCell<Vec<u128>> = const { std::cell::RefCell::new(Vec::new()) };
        static PASSES: std::cell::RefCell<Option<Vec<dirfd::Sid>>> = const { std::cell::RefCell::new(None) };
    }

    /// On this thread, read the owner of the entry at `path` as `NT AUTHORITY\SYSTEM` (`S-1-5-18`), another user's.
    #[cfg(all(test, windows))]
    pub(crate) fn mark_theirs(path: &Path) {
        let id = aterm_sysprobe::volumes::file_id(path, false).expect("marked");
        THEIRS.with(|t| t.borrow_mut().push(id.ino));
    }

    /// On this thread, the owners a walk lets pass are exactly `passes`, in place of this process's user and `BUILTIN\Administrators`.
    #[cfg(all(test, windows))]
    pub(crate) fn only_owners_pass(passes: Option<Vec<dirfd::Sid>>) {
        PASSES.with(|p| *p.borrow_mut() = passes);
    }

    /// One report walk of the tree at `dir` (Windows).
    #[cfg(windows)]
    pub(super) fn tally(dir: &Path, walk: &Walk) -> Tally {
        let mut t = Tally::default();
        let unread = |t: &mut Tally, what: String| {
            t.partial = true;
            if walk.owners && t.foreign.is_none() {
                t.foreign = Some(format!(
                    "{what}, so nothing of another user's in it is ruled out"
                ));
            }
        };
        let pinned = match pin(dir) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return t,
            Err(e) => {
                unread(&mut t, format!("{} could not be read ({e})", dir.display()));
                return t;
            }
        };
        let root = match dirfd::openat(
            &pinned,
            ".",
            OFlags::PATH | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map_err(io::Error::from)
        .and_then(|fd| Ok((dev(&fd)?, fd)))
        {
            Ok(root) => root,
            Err(e) => {
                unread(&mut t, format!("{} could not be read ({e})", dir.display()));
                return t;
            }
        };
        let passes = if walk.owners {
            match dirfd::Sid::current_user() {
                Ok(me) => vec![me, dirfd::Sid::administrators()],
                Err(e) => {
                    unread(
                        &mut t,
                        format!("this process's user could not be read ({e})"),
                    );
                    return t;
                }
            }
        } else {
            Vec::new()
        };
        #[cfg(test)]
        let passes = PASSES.with(|p| p.borrow().clone()).unwrap_or(passes);
        let mut w = Tallier {
            walk,
            dev: walk.dev.unwrap_or(root.0),
            passes,
            started: Instant::now(),
            seen: 0,
            t,
        };
        if walk.owners {
            match dirfd::fstat(&root.1) {
                Ok(st) => w.judge_owner(&root.1, st.id(), dir),
                Err(e) => w.unread(format!("{} could not be read ({e})", dir.display())),
            }
        }
        w.visit(&root.1, dir, 0);
        w.t
    }

    #[cfg(windows)]
    struct Tallier<'a> {
        walk: &'a Walk,
        dev: u64,
        passes: Vec<dirfd::Sid>,
        started: Instant,
        seen: usize,
        t: Tally,
    }

    #[cfg(windows)]
    impl Tallier<'_> {
        fn done(&self) -> bool {
            self.t.cut.is_some() || (self.walk.count == Count::Nothing && self.t.foreign.is_some())
        }

        fn cut(&mut self, why: String) {
            self.t.partial = true;
            self.t.cut.get_or_insert(why);
        }

        fn unread(&mut self, what: String) {
            self.t.partial = true;
            if self.walk.owners && self.t.foreign.is_none() {
                self.t.foreign = Some(format!(
                    "{what}, so nothing of another user's in it is ruled out"
                ));
            }
        }

        fn judge_owner(&mut self, fd: &OwnedFd, id: FileId, at: &Path) {
            let owner = dirfd::owner(fd);
            #[cfg(test)]
            let owner = owner.map(|o| {
                if THEIRS.with(|t| t.borrow().contains(&id.ino)) {
                    dirfd::Sid::local_system()
                } else {
                    o
                }
            });
            let _ = id;
            match owner {
                Ok(o) if self.passes.contains(&o) => {}
                Ok(o) => {
                    self.t.foreign.get_or_insert_with(|| {
                        format!(
                            "it holds an entry of another user (owner {o}), which a removal could not delete"
                        )
                    });
                }
                Err(e) => self.unread(format!(
                    "{}: its owner could not be read ({})",
                    at.display(),
                    io::Error::from(e)
                )),
            }
        }

        fn visit(&mut self, fd: &OwnedFd, path: &Path, depth: usize) {
            let entries = match children(fd) {
                Ok(entries) => entries,
                Err(e) => {
                    return self.unread(format!("{} could not be read ({e})", path.display()));
                }
            };
            for c in entries {
                if self.done() {
                    return;
                }
                self.seen += 1;
                aterm_uds::progress::tick();
                if self.seen > self.walk.max {
                    return self.cut(format!("it holds more than {} entries", self.walk.max));
                }
                if self
                    .walk
                    .wall
                    .is_some_and(|wall| self.started.elapsed() >= wall)
                {
                    return self.cut("its walk ran out of time".to_owned());
                }
                let owner = self.walk.owners && self.t.foreign.is_none();
                let size =
                    self.walk.count != Count::Nothing && c.kind.is_file() && c.allocation > 0;
                let enter = c.kind.is_dir();
                if !(owner || size || enter) {
                    continue;
                }
                let here = path.join(&c.name);
                let flags = if enter {
                    OFlags::PATH | OFlags::DIRECTORY
                } else {
                    OFlags::PATH
                };
                let opened = dirfd::openat(fd, &c.name, flags, Mode::empty())
                    .and_then(|h| Ok((dirfd::fstat(&h)?, h)));
                let (st, h) = match opened {
                    Err(Errno::NOENT | Errno::DELETE_PENDING) => continue,
                    Err(e) => {
                        if owner || enter {
                            self.unread(format!(
                                "{} could not be read ({})",
                                here.display(),
                                io::Error::from(e)
                            ));
                        }
                        continue;
                    }
                    Ok((st, _)) if st.id() != c.id => {
                        if owner || enter {
                            self.unread(format!("{} changed while it was read", here.display()));
                        }
                        continue;
                    }
                    Ok(opened) => opened,
                };
                if owner {
                    self.judge_owner(&h, c.id, &here);
                }
                if size {
                    let bytes = match self.walk.count {
                        Count::Shared => st.allocation / st.st_nlink.max(1),
                        Count::Released if st.st_nlink == 1 => st.allocation,
                        Count::Released | Count::Nothing => 0,
                    };
                    self.t.bytes = self.t.bytes.saturating_add(bytes);
                }
                if enter {
                    // Never onto another device: nothing on it is this tree's.
                    if dev(&h).ok() != Some(self.dev) {
                        continue;
                    }
                    if depth + 1 >= super::MAX_DELETE_DEPTH {
                        return self.cut(format!(
                            "it nests deeper than {} levels",
                            super::MAX_DELETE_DEPTH
                        ));
                    }
                    self.visit(&h, &here, depth + 1);
                }
            }
        }
    }
}

#[cfg(all(test, any(unix, windows)))]
pub(crate) use fd::mark_foreign;

#[cfg(all(test, windows))]
pub(crate) use fd::{mark_cloned, mark_theirs, only_owners_pass};

#[cfg(not(any(unix, windows)))]
mod fd {
    use super::{Judge, Path, PathBuf, Removed, Select, Stage, WalkBudget, io};
    pub(super) fn unmanaged_custody(_path: &Path, _budget: WalkBudget) -> Result<(), String> {
        Err("reading cache custody needs unix or Windows".into())
    }
    pub(super) fn remove_unmanaged_tree(
        _path: &Path,
        _expected: aterm_sysprobe::volumes::FileId,
        _permit: &mut dyn FnMut() -> Result<(), String>,
    ) -> io::Result<Removed> {
        Err(io::Error::other("removing caches needs unix or Windows"))
    }
    pub(super) fn clock(_path: &Path, _budget: WalkBudget) -> Option<i64> {
        None
    }
    pub(super) fn find(_path: &Path, _budget: WalkBudget) -> (Vec<PathBuf>, Option<String>) {
        (
            Vec::new(),
            Some("reclaiming build caches needs unix or Windows".to_owned()),
        )
    }
    pub(super) fn reclaim(
        _path: &Path,
        _judge: &Judge,
        _select: Select<'_>,
        _hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        Err(io::Error::other(
            "reclaiming build caches needs unix or Windows",
        ))
    }
    pub(super) fn reclaim_died(
        _path: &Path,
        _at: (&Path, &Path),
        _judge: &Judge,
        _seen: i64,
        _check: &mut dyn FnMut() -> Result<(), String>,
        _hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        Err(io::Error::other(
            "reclaiming build caches needs unix or Windows",
        ))
    }
    pub(super) fn reclaim_whole(
        _path: &Path,
        _judge: &Judge,
        _seen: i64,
        _quiet_s: i64,
        _check: &mut dyn FnMut() -> Result<(), String>,
        _hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        Err(io::Error::other(
            "reclaiming build caches needs unix or Windows",
        ))
    }
    pub(super) fn reclaim_profiles(
        _path: &Path,
        _judge: &Judge,
        _window: (i64, i64),
        _mounts: &mut dyn FnMut() -> Result<(), String>,
        _between: &mut dyn FnMut() -> Result<(), String>,
        _hook: &mut dyn FnMut(Stage, &Path),
    ) -> io::Result<Removed> {
        Err(io::Error::other(
            "reclaiming build caches needs unix or Windows",
        ))
    }
}

#[path = "disk_target_tests.rs"]
#[cfg(test)]
mod tests;
