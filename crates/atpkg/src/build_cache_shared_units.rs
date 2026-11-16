// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SHARED DEPENDENCY STORE'S SHARE OF THE RECLAIM (DC-17g): the per-user store Trust's targo
//! fills under `build.shared-dependency-cache` (`<cargo home>/trust/native-dependencies-v1`,
//! Trust's `shared_dependencies::storage_root`), which targo itself never evicts (`targo clean`
//! stays scoped to project output). aterm is its one evictor (docs/DESIGN-disk-custody-2026-11-12.md
//! §5): one content-keyed store per Mac, evicted by last use.
//!
//! A UNIT is a folder of the store's `build/` holding targo's per-unit `.lock`
//! (`compilation_files.rs`: `shared_unit_dir`, `build_unit_lock`): `build/<name>/<hash>`, or
//! `build/git/<commit>/<name>/<hash>` for a Git source. Each build takes that lock, shared to read
//! a unit and exclusive to write it, through `std::fs::File::lock_shared`/`lock` (`flock(2)` on
//! macOS, `util/flock.rs`), so a pass takes the same kind of lock, exclusive and without waiting
//! ([`crate::lock::Flock::try_lock`]): a unit any build holds stays, and while aterm holds it no
//! build reads or writes it. Held, the unit is renamed aside in its own folder and removed there;
//! a build arriving meanwhile finds no unit at its path and builds a fresh one. The lock is the
//! store's whole custody contract (targo: "coordinates readers and writers through a per-unit
//! lock"), so no process scan is made per unit: a store holds thousands of them.
//!
//! LAST USE IS RECORDED BY THE PASSES, since a build records none: a fresh build only reads a
//! unit (its fingerprint, then the compiler maps its `.rlib`) and writes no time into it unless
//! `-Zmtime-on-use` is on, which nothing sets. A read moves a file's access time only while that
//! time is older than the file's write time (macOS's APFS, measured on macOS 27.2 on the Data
//! volume: a file read whose access time equalled or passed its write time kept it; Linux's
//! `relatime` updates the same case), so a file targo wrote and builds then only read shows no
//! read at all. Each pass therefore, holding a unit's lock, carries the unit's newest use into
//! the times of its `.lock` (which targo opens only to `flock`: it never reads, writes or times
//! it) and ARMS each of its other files, an access time a second below its write time, so the
//! next read moves it ([`keep`]). A unit a build holds while a pass reads it is in use at that
//! moment, and its `.lock` says so ([`record_use`]). A pass that finds a unit idle long enough
//! reads its use again once it holds the lock, so a build that used it since stops the removal.
//!
//! THE STORES: Targo names the store from the build's environment (`CARGO_HOME`, else
//! `TARGO_HOME`, else `~/.cargo`), but the passes run from launchd (the window's disk watch, the
//! daily pass), which never reads the login shell's profile where a person sets either. So the
//! build's front door notes a store other than the home's `.cargo` one ([`note`]), and a pass
//! reads its own store and each noted one ([`roots`]).

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Planned, RECLAIM_IDLE, ReclaimAfter, Unused};

/// The store under a package home (Trust's `storage_root`).
const STORE: [&str; 2] = ["trust", "native-dependencies-v1"];

/// The store's folder of units.
const BUILD: &str = "build";

/// The lock targo takes on each unit, in the unit's folder.
const LOCK: &str = ".lock";

/// The deepest a unit lies under `build/`: a Git source's `git/<commit>/<name>/<hash>`.
const MAX_DEPTH: usize = 4;

/// How deep in a unit its use is read ([`super::tooling::last_used`]) and its files are armed
/// ([`arm`]): the unit's own entries (`.lock`, `out/`, `fingerprint/`) and what those folders hold.
const USE_DEPTH: usize = 2;

/// How far below its write time a pass sets an armed file's access time ([`arm`]).
const ARMED: Duration = Duration::from_secs(1);

/// The stage names a unit is renamed to in its own folder before its removal
/// ([`super::stage_aside`]): `.aterm-reclaim`, then `.aterm-reclaim-<n>`. A package name never
/// starts with a dot, nor a unit hash, so no unit of targo's takes one.
const STAGE: &str = ".aterm-reclaim";

/// Where a build notes the store it fills when that is not its home's `.cargo` one ([`note`]),
/// under the home: one file per store, named by [`noted_name`], holding the store's path.
pub const NOTED: &str = "Library/Caches/aterm/.ubd/shared-dependency-stores.noindex";

/// The longest store path a note holds.
#[cfg(unix)]
const NOTE_MAX: u64 = 4096;

/// What a pass over the rebuildable budget waits on ([`reclaim_lru`]): a unit unused this long
/// goes least recently used first, the same hour a stale part of a shared-cache build directory
/// no live session keeps waits over the budget ([`ReclaimAfter::OVER_BUDGET`]).
pub const OVER_BUDGET: Duration = ReclaimAfter::OVER_BUDGET.stale;

/// The store of the package home Targo selects from `env` working in `cwd`: `CARGO_HOME`, else
/// `TARGO_HOME` (which Trust's frontend imports as `CARGO_HOME` where that is unset), a set but
/// empty value falling back as Targo's `homedir` does, else `home`'s `.cargo`; a relative value
/// joined to `cwd`, and `.` and `..` folded as written (`normalize_path`), never through a link.
#[must_use]
pub fn root_with(
    home: &Path,
    cwd: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> PathBuf {
    let package_home = env("CARGO_HOME")
        .or_else(|| env("TARGO_HOME"))
        .filter(|value| !value.is_empty())
        .map_or_else(
            || home.join(".cargo"),
            |value| cwd.map_or_else(|| PathBuf::from(&value), |cwd| cwd.join(&value)),
        );
    STORE
        .iter()
        .fold(lexical(&package_home), |path, name| path.join(name))
}

/// The store at `home`'s `.cargo`, whatever an environment names.
fn home_root(home: &Path) -> PathBuf {
    root_with(home, None, &|_| None)
}

/// The store of `home` as this process names it: by this process's environment where `home` is
/// its own ([`aterm_types::dirs::home_dir`]), since `CARGO_HOME` and `TARGO_HOME` name that
/// login's package home; another home (a scratch one, a fixture's) is read at its own `.cargo`.
#[must_use]
pub fn root(home: &Path) -> PathBuf {
    root_for(home, aterm_types::dirs::home_dir().as_deref(), &|name| {
        std::env::var_os(name)
    })
}

/// [`root`] for a process whose own home is `own` and whose environment `env` reads.
fn root_for(home: &Path, own: Option<&Path>, env: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    if own == Some(home) {
        let cwd = std::env::current_dir().ok();
        root_with(home, cwd.as_deref(), env)
    } else {
        home_root(home)
    }
}

/// `path` with `.` dropped and `..` taking the folder before it, as written.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(part);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The name of `store`'s note: its path's 64-bit FNV-1a, in hex.
#[cfg(unix)]
fn noted_name(store: &Path) -> String {
    let hash = store
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{hash:016x}")
}

/// NOTE THE STORE THIS PROCESS'S BUILD FILLS ([`root`]) under `home` ([`NOTED`]), for the passes
/// that run without the build's environment ([`roots`]). The build's front door calls it before
/// the build starts (`build_admit`'s shim hop); a store at the home's `.cargo` is not noted, and
/// one noted before is only looked up (one `lstat`), so a build pays one stat. Best effort: a
/// note that cannot be written leaves the build as it is, and is tried again by the next build.
pub fn note(home: &Path) {
    let _ = note_store(home, &root(home));
}

#[cfg(unix)]
fn note_store(home: &Path, store: &Path) -> std::io::Result<()> {
    if !store.is_absolute() || store == home_root(home) {
        return Ok(());
    }
    let dir = home.join(NOTED);
    let entry = dir.join(noted_name(store));
    if std::fs::symlink_metadata(&entry).is_ok() {
        return Ok(());
    }
    crate::platform::ensure_private_dir(&dir)?;
    crate::metadata_io::write_private_atomic(&entry, store.as_os_str().as_encoded_bytes())
}

#[cfg(not(unix))]
fn note_store(_: &Path, _: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Each note under `home` with the store it names: `None` where the note is no regular file of
/// at most [`NOTE_MAX`] bytes naming an absolute path. In name order; nothing off Unix.
#[cfg(unix)]
fn notes(home: &Path) -> Vec<(PathBuf, Option<PathBuf>)> {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStringExt as _;

    let Ok(entries) = std::fs::read_dir(home.join(NOTED)) else {
        return Vec::new();
    };
    let mut notes: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    notes.sort();
    notes
        .into_iter()
        .map(|note| {
            let store = crate::metadata_io::open_regular_with_meta(&note)
                .ok()
                .filter(|(_, meta)| meta.len() <= NOTE_MAX)
                .and_then(|(file, _)| {
                    let mut bytes = Vec::new();
                    file.take(NOTE_MAX)
                        .read_to_end(&mut bytes)
                        .ok()
                        .map(|_| PathBuf::from(OsString::from_vec(bytes)))
                })
                .filter(|store| store.is_absolute());
            (note, store)
        })
        .collect()
}

#[cfg(not(unix))]
fn notes(_: &Path) -> Vec<(PathBuf, Option<PathBuf>)> {
    Vec::new()
}

/// Whether a pass may read the noted `store` of `home`: it lies in no folder a pass never looks
/// into ([`super::checkout::never_looked_at`], looked at before anything there is read) and is a
/// folder, not a link, this user owns.
#[cfg(unix)]
fn readable(home: &Path, store: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    super::checkout::never_looked_at(store, Some(home)).is_none()
        && std::fs::symlink_metadata(store)
            .is_ok_and(|meta| meta.is_dir() && meta.uid() == crate::platform::our_uid())
}

#[cfg(not(unix))]
fn readable(_: &Path, _: &Path) -> bool {
    false
}

/// THE STORES A PASS READS under `home`: its own ([`root`]), then each a build noted ([`note`])
/// that a pass may read ([`readable`]), each once.
#[must_use]
pub fn roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root(home)];
    for store in notes(home).into_iter().filter_map(|(_, store)| store) {
        if !roots.contains(&store) && readable(home, &store) {
            roots.push(store);
        }
    }
    roots
}

/// Remove each note under `home` that names no store or a store that is gone; a store in a
/// folder a pass never looks into is not looked up, and its note stays.
fn forget_gone(home: &Path) {
    for (note, store) in notes(home) {
        let gone = store.is_none_or(|store| {
            super::checkout::never_looked_at(&store, Some(home)).is_none()
                && std::fs::symlink_metadata(&store)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        });
        if gone {
            let _ = std::fs::remove_file(note);
        }
    }
}

/// What one walk of the stores found: their units and the stages an earlier pass left.
#[derive(Default)]
struct Found {
    units: Vec<PathBuf>,
    stages: Vec<PathBuf>,
}

fn is_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn walk(dir: &Path, depth: usize, found: &mut Found) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<(OsString, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| (entry.file_name(), entry.path()))
        .collect();
    children.sort();
    for (name, child) in children {
        if super::is_stage(&name, OsStr::new(STAGE)) {
            found.stages.push(child);
        } else if std::fs::symlink_metadata(child.join(LOCK)).is_ok_and(|meta| meta.is_file()) {
            found.units.push(child);
        } else if depth < MAX_DEPTH {
            walk(&child, depth + 1, found);
        }
    }
}

/// The units and stages under each of `roots`, each root's in name order; nothing of a root that
/// or whose `build/` is no folder, and never through a link.
fn found(roots: &[PathBuf]) -> Found {
    let mut found = Found::default();
    for root in roots {
        let build = root.join(BUILD);
        if is_dir(root) && is_dir(&build) {
            walk(&build, 1, &mut found);
        }
    }
    found
}

/// The newest use of `unit` ([`super::tooling::last_used`]): its `.lock`'s times (the use a pass
/// carried there, [`keep`]), the newest write of it or of what lies in its folders, and the newest
/// read of a file there that a pass armed since.
fn last_used(unit: &Path) -> Option<SystemTime> {
    super::tooling::last_used(unit, USE_DEPTH)
}

/// How long nothing used a unit last used `used`, at `now`.
fn idle(used: SystemTime, now: SystemTime) -> Duration {
    now.duration_since(used).unwrap_or(Duration::ZERO)
}

/// Set `unit`'s `.lock` times to `at` where they are older: the use a pass records for it.
fn record_use(unit: &Path, at: SystemTime) -> std::io::Result<()> {
    let (file, meta) = crate::metadata_io::open_regular_with_meta(&unit.join(LOCK))?;
    if meta.modified().is_ok_and(|was| was >= at) {
        return Ok(());
    }
    file.set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
}

/// ARM `path` where it is a regular file whose access time is not below its write time: its
/// access time becomes [`ARMED`] below its write time, which stays, so the next read moves it.
fn arm_file(path: &Path) {
    let Ok((file, meta)) = crate::metadata_io::open_regular_with_meta(path) else {
        return;
    };
    let (Ok(written), Ok(read)) = (meta.modified(), meta.accessed()) else {
        return;
    };
    if read < written {
        return;
    }
    if let Some(below) = written.checked_sub(ARMED) {
        let _ = file.set_times(std::fs::FileTimes::new().set_accessed(below));
    }
}

/// [`arm_file`] each file of `dir` other than its `.lock`, and with `depth` left each file of
/// its folders; never through a link.
fn arm(dir: &Path, depth: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() && entry.file_name() != LOCK {
            arm_file(&entry.path());
        } else if kind.is_dir() && depth > 1 {
            arm(&entry.path(), depth - 1);
        }
    }
}

/// KEEP A RECORD OF `unit`'S USE, with its lock held so no build reads it meanwhile: its newest
/// use ([`last_used`]) goes into its `.lock`'s times ([`record_use`]) and its files are armed
/// again ([`arm`]). Returns that use.
fn keep(unit: &Path) -> Option<SystemTime> {
    let used = last_used(unit)?;
    let _ = record_use(unit, used);
    arm(unit, USE_DEPTH);
    Some(used)
}

/// Of `units`, each with its last use, those unused `after` or more at `now`, least recently used
/// first, each with how long it was unused; one that cannot be staged ([`super::stage_for`]) is
/// left out.
fn candidates(
    units: Vec<(PathBuf, SystemTime)>,
    now: SystemTime,
    after: Duration,
) -> Vec<(PathBuf, Duration)> {
    let mut out: Vec<(PathBuf, Duration)> = units
        .into_iter()
        .filter_map(|(unit, used)| {
            let idle = idle(used, now);
            let holder = unit.parent()?;
            (idle >= after && super::stage_for(&unit, holder, OsStr::new(STAGE)).is_some())
                .then_some((unit, idle))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// `unit`'s lock taken exclusive without waiting; `None` where a build holds it, or where it is
/// no regular file (a link, or gone).
fn hold(unit: &Path) -> Option<crate::lock::Flock> {
    let file = crate::metadata_io::open_regular(&unit.join(LOCK)).ok()?;
    crate::lock::Flock::try_lock(file).ok()
}

/// Advisory cleanup candidates of the store `root` at `now`: one row for the store
/// ([`Unused::SharedUnits`]) whose paths are the units a calm pass would take (each unused
/// [`RECLAIM_IDLE`] or more by the use passes recorded and builds read since, no build holding
/// its lock when read), `held` where a build held one that was old enough, and each stage an
/// earlier pass left. Nothing is moved, removed or timed.
#[must_use]
pub fn plan(root: &Path, now: SystemTime) -> Vec<Planned> {
    let found = found(std::slice::from_ref(&root.to_path_buf()));
    let mut planned: Vec<Planned> = found.stages.into_iter().map(Planned::stage).collect();
    let units = found
        .units
        .into_iter()
        .filter_map(|unit| last_used(&unit).map(|used| (unit, used)))
        .collect();
    let mut paths = Vec::new();
    let mut held = false;
    let mut newest = None::<Duration>;
    for (unit, idle) in candidates(units, now, RECLAIM_IDLE) {
        if hold(&unit).is_none() {
            held = true;
            continue;
        }
        newest = Some(newest.map_or(idle, |was: Duration| was.min(idle)));
        paths.push(unit);
    }
    if let Some(idle) = newest {
        planned.push(Planned {
            path: root.to_path_buf(),
            why: Unused::SharedUnits {
                units: paths.len(),
                idle,
                after: RECLAIM_IDLE,
            },
            paths,
            held,
            stays: None,
        });
    }
    planned
}

/// The calm pass over `home`'s stores ([`roots`]) at `now` ([`reclaim_on`] at [`RECLAIM_IDLE`]):
/// each unit unused a month goes, as a tooling entry does, and each note of a store that is gone
/// is dropped.
pub fn reclaim(
    home: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    let went = reclaim_on(&roots(home), now, RECLAIM_IDLE, record);
    forget_gone(home);
    went
}

/// The pass over the rebuildable budget across `home`'s stores ([`roots`], [`reclaim_on`] at
/// [`OVER_BUDGET`]): each unit unused an hour, least recently used first, while `record` asks for
/// more.
pub fn reclaim_lru(
    home: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    reclaim_on(&roots(home), now, OVER_BUDGET, record)
}

/// Remove from the stores `roots` at `now` each stage an earlier pass left, then each unit unused
/// `after` or more, least recently used first across them all. First every unit's use is kept:
/// one whose lock this pass takes without waiting has its record kept and its files armed
/// ([`keep`]), and one a build holds is recorded as used `now` ([`record_use`]). `record` is
/// asked before each removal, the stage's intent too ([`Unused::Unfinished`]); `false` keeps that
/// one and ends the pass, so a budget that is met stops it at the next unit. A unit goes only
/// while this pass holds its lock without waiting ([`hold`]), and only where its use read again
/// under that lock is still `after` old (a build that used it since the walk keeps it, its use
/// kept), from before its intent until it is gone: renamed aside in its own folder under the
/// lowest free stage name ([`STAGE`]) and removed whole there ([`super::remove::tree`]). Each is
/// returned with why it went ([`Unused::SharedUnits`], one unit), a stage some entry of which
/// stayed with what stayed ([`Unused::Stayed`]), and a unit whose rename was refused after its
/// intent with [`Unused::Refused`]. The folders a unit sat in stay: a build makes its unit's
/// folder and then opens its lock, so a folder removed between the two would fail that build.
pub(super) fn reclaim_on(
    roots: &[PathBuf],
    now: SystemTime,
    after: Duration,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    let found = found(roots);
    let mut intents = super::Intents::new(record);
    for stage in found.stages {
        if !intents.ask_staged(&stage, Unused::Unfinished) {
            return intents.finish();
        }
        let left = super::remove::tree(&stage);
        let why = if left.whole() {
            Unused::Unfinished
        } else {
            Unused::Stayed {
                stayed: left.stayed,
                freed: left.freed,
            }
        };
        intents.outcome(stage, why);
    }
    let mut units = Vec::new();
    for unit in found.units {
        if let Some(lock) = hold(&unit) {
            if let Some(used) = keep(&unit) {
                units.push((unit, used));
            }
            drop(lock);
        } else {
            let _ = record_use(&unit, now);
        }
    }
    for (unit, _) in candidates(units, now, after) {
        let Some(holder) = unit.parent().map(Path::to_path_buf) else {
            continue;
        };
        let Some(lock) = hold(&unit) else {
            continue;
        };
        let Some(used) = last_used(&unit) else {
            continue;
        };
        let idle = idle(used, now);
        if idle < after {
            keep(&unit);
            continue;
        }
        let why = Unused::SharedUnits {
            units: 1,
            idle,
            after,
        };
        if !intents.ask(&unit, why) {
            break;
        }
        super::seams::staging(&unit);
        let stage = match super::stage_aside(&unit, &holder, OsStr::new(STAGE)) {
            Ok(stage) => {
                intents.moved(&stage);
                stage
            }
            Err(e) => {
                intents.refused(super::Refusal::Rename(e.kind()));
                continue;
            }
        };
        let left = super::remove::tree(&stage);
        drop(lock);
        if left.whole() {
            intents.outcome(unit, why);
        } else {
            intents.outcome(
                stage,
                Unused::Stayed {
                    stayed: left.stayed,
                    freed: left.freed,
                },
            );
        }
    }
    intents.finish()
}

#[cfg(test)]
#[path = "build_cache_shared_units_tests.rs"]
mod tests;
