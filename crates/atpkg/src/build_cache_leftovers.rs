// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! BUILD LEFTOVERS OUTSIDE THE SHARED CACHE: the regenerable bulk campaigns, worktree roots and
//! per-campaign caches leave behind, which no workspace's build directory accounts for.
//!
//! The shared cache is keyed by workspace, so [`super::reclaim_watched`] knows when a build
//! directory's owner is gone. A campaign that names its own `CARGO_HOME` and target directory
//! under a scratch root leaves both where nothing reads them again: on one release host 3.6 TB
//! stood in finished scopes while `aterm pkg machine cache` listed 84 KiB.
//!
//! WHERE ([`places`]): the folders such roots are made in, each child of one a *unit* (a
//! campaign scope, a worktree root, one campaign's cache). WHAT GOES: inside a unit, each topmost
//! directory carrying a signed cache directory tag ([`tagged`]: cargo writes one into every
//! target directory and into a cargo home's `registry` and `git`), and nothing else; a unit's
//! receipts, logs, clones and evidence are never candidates. A unit that carries the tag itself
//! (a target directory made directly in a place) goes whole. WHEN ([`wait`], [`quiet`]): a unit
//! that holds a receipt naming it landed or delivered ([`receipt`]) is finished, and its caches
//! go once nothing the look saw in the unit was written for [`FINISHED_QUIET`]: no folder or
//! file outside its caches, and no cache it found. Any other unit's go once nothing was for
//! [`LEFTOVER_IDLE`], or [`FINISHED_QUIET`] below the disk watch's floor. One write anywhere
//! keeps every cache of the unit: a campaign that is alive keeps the target it has not built
//! into this week. HOW: as a build directory of the shared cache goes ([`super::Sweep::take`]):
//! custody read at the cache and every folder above it (`aterm pkg machine cache retain` keeps a
//! unit and all it holds), no process using the unit, every cargo lock of a complete inventory
//! taken and the inventory read again under them, renamed aside under a stage name of its own
//! and removed there.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{KeepReason, Planned, Unused, age, fence, inventory, last_written};

/// The cache directory tag's file name (<https://bford.info/cachedir/>).
pub const TAG: &str = "CACHEDIR.TAG";

/// The tag's fixed first bytes: only a file starting with them marks its directory regenerable.
pub const SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// A finished unit's caches go once nothing seen in the unit was written for this long; below
/// the disk watch's floor it is every unit's wait.
pub const FINISHED_QUIET: Duration = Duration::from_secs(24 * 60 * 60);

/// The caches of a unit with no receipt go once nothing seen in the unit was written for this
/// long while the volume has room: the wait of the retired gate's leftovers
/// ([`super::retired::UNWRITTEN`]).
pub const LEFTOVER_IDLE: Duration = super::retired::UNWRITTEN;

/// What a cache `<name>` is renamed to before its removal: `<name>` with this suffix, then
/// `-<n>` ([`super::stage_name`]). A folder so named in a unit is a stage an earlier pass left.
const STAGE: &str = ".aterm-leftover-removing";

/// How deep below a unit a tagged directory is looked for.
const DEPTH: usize = 8;

/// The directories read in one unit while looking for tagged ones. A unit that holds more is
/// looked into no further this pass; what was found is still judged.
const LOOKS: usize = 20_000;

/// The campaign scopes' folder under `$HOME`: release-campaign sessions make one scope each
/// there by hand, and no tool removes one.
const SCOPES: &str = ".cache/alab-release-scope";

/// A folder whose children are units ([`places`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The folder.
    pub root: PathBuf,
    /// What its units are, as a listing names them.
    pub kind: &'static str,
    /// Children that are no unit of it: another place's root, or a cache another sweep owns.
    skip: Vec<PathBuf>,
}

impl Place {
    /// The units of this place: its child folders, sorted, never a link, a stage or a skipped
    /// child.
    #[must_use]
    pub fn units(&self) -> Vec<PathBuf> {
        let mut units: Vec<PathBuf> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| staged_from(&entry.file_name()).is_none())
            .map(|entry| entry.path())
            .filter(|path| !self.skip.contains(path))
            .collect();
        units.sort();
        units
    }

    /// The stages an earlier pass left directly in this place, each with the unit it was (a
    /// unit that was itself a tagged cache), sorted.
    fn stages(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut stages: Vec<(PathBuf, PathBuf)> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let original = staged_from(&entry.file_name())?;
                Some((entry.path(), self.root.join(original)))
            })
            .collect();
        stages.sort();
        stages
    }
}

/// The places under `home` whose units are judged: the campaign scopes, the user cache folder,
/// the worktree roots' folder and aterm's own `.ubd` cache folder, less the shared and the
/// registered caches, which their own sweeps bound.
#[must_use]
pub fn places(home: &Path) -> Vec<Place> {
    let scopes = home.join(SCOPES);
    let own = home.join("Library/Caches/aterm/.ubd");
    vec![
        Place {
            root: scopes.clone(),
            kind: "campaign scope",
            skip: Vec::new(),
        },
        Place {
            root: home.join(".cache"),
            kind: "user cache",
            skip: vec![scopes],
        },
        Place {
            root: home.join(".ubd"),
            kind: "worktree root",
            skip: Vec::new(),
        },
        Place {
            skip: vec![super::cache_root(home), home.join(super::registered::INDEX)],
            root: own,
            kind: "campaign cache",
        },
    ]
}

/// Whether `dir` carries a signed cache directory tag: a regular file [`TAG`] in it, never a
/// link, starting with [`SIGNATURE`].
#[must_use]
pub fn tagged(dir: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = crate::metadata_io::open_regular(&dir.join(TAG)) else {
        return false;
    };
    let mut head = [0u8; SIGNATURE.len()];
    let mut file = file;
    file.read_exact(&mut head).is_ok() && head == SIGNATURE
}

/// The receipt that says the unit `unit` is finished: a regular `.json` file directly in it
/// whose name holds the word `landed`, `delivered` or `delivery` (any case, between characters
/// that are no letter or digit) and whose text is a JSON object; the newest such file's
/// modification time. A campaign writes one when its change is on main or its package is
/// published, as its last record.
#[must_use]
pub fn receipt(unit: &Path) -> Option<SystemTime> {
    let mut newest = None;
    for entry in std::fs::read_dir(unit).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name
            .strip_suffix(".json")
            .or_else(|| name.strip_suffix(".JSON"))
        else {
            continue;
        };
        let says = stem
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| {
                ["landed", "delivered", "delivery"]
                    .iter()
                    .any(|w| word.eq_ignore_ascii_case(w))
            });
        if !says {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
            continue;
        }
        let object = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| aterm_json::from_str::<aterm_json::Value>(&text).ok())
            .is_some_and(
                |value| matches!(value, aterm_json::Value::Object(map) if !map.is_empty()),
            );
        if let (true, Ok(at)) = (object, meta.modified()) {
            newest = Some(newest.map_or(at, |n: SystemTime| n.max(at)));
        }
    }
    newest
}

/// How long a unit and a cache of it stay unwritten before the cache goes: [`FINISHED_QUIET`]
/// when the unit is `finished` (it holds a receipt) or `pressed` (the volume is below the disk
/// watch's floor), else [`LEFTOVER_IDLE`].
#[must_use]
pub const fn wait(finished: bool, pressed: bool) -> Duration {
    if finished || pressed {
        FINISHED_QUIET
    } else {
        LEFTOVER_IDLE
    }
}

/// The stage base of the cache named `name`.
fn stage_base(name: &OsStr) -> OsString {
    let mut base = name.to_os_string();
    base.push(STAGE);
    base
}

/// The cache a stage named `name` was, when `name` is a stage name ([`stage_base`], then
/// [`super::stage_name`]'s ordinals).
fn staged_from(name: &OsStr) -> Option<OsString> {
    let text = name.to_str()?;
    let at = text.rfind(STAGE)?;
    let (original, rest) = text.split_at(at);
    (!original.is_empty() && super::is_stage(name, &stage_base(OsStr::new(original)))).then(|| {
        debug_assert!(rest.starts_with(STAGE));
        OsString::from(original)
    })
}

/// What a unit holds that a pass judges.
#[derive(Debug, Default, PartialEq, Eq)]
struct Found {
    /// Its topmost tagged directories, sorted.
    caches: Vec<PathBuf>,
    /// The stages an earlier pass left in it, each with the cache it was, sorted.
    stages: Vec<(PathBuf, PathBuf)>,
    /// The last write the look saw outside the caches: the unit's own folder and every entry
    /// of each folder it read. `None` when the unit itself could not be read.
    newest: Option<SystemTime>,
}

/// Whether another authority's custody marker stands directly in `dir`
/// ([`super::registered::CUSTODY_MARKERS`]): a registered or package cache, which its own sweep
/// bounds and this one neither takes nor looks into.
fn managed(dir: &Path) -> bool {
    super::registered::CUSTODY_MARKERS
        .iter()
        .any(|marker| std::fs::symlink_metadata(dir.join(marker)).is_ok())
}

/// Look through `unit` for its topmost tagged directories and the stages an earlier pass left:
/// no link is followed, nothing off the unit's volume is entered, a tagged directory is not
/// looked into, a `.git` folder holds none, a folder under another authority's custody
/// ([`managed`]) is passed by, and at most [`LOOKS`] directories are read. Every entry of a
/// folder it reads counts toward [`Found::newest`], whatever it is. A unit that is itself
/// tagged is its own one cache, and nothing in it is looked for.
fn find(unit: &Path) -> Found {
    fn walk(dir: &Path, depth: usize, left: &mut usize, dev: Option<u64>, found: &mut Found) {
        let Some(next) = left.checked_sub(1) else {
            return;
        };
        *left = next;
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            aterm_uds::progress::tick();
            if let Ok(at) = entry.metadata().and_then(|meta| meta.modified()) {
                found.newest = found.newest.map(|newest| newest.max(at));
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let path = entry.path();
            if let Some(original) = staged_from(&name) {
                found.stages.push((path, dir.join(original)));
                continue;
            }
            if name == ".git" || device(&path) != dev || managed(&path) {
                continue;
            }
            if tagged(&path) {
                found.caches.push(path);
            } else if depth < DEPTH {
                walk(&path, depth + 1, left, dev, found);
            }
        }
    }
    let mut found = Found {
        newest: last_written(unit),
        ..Found::default()
    };
    if tagged(unit) {
        found.caches.push(unit.to_path_buf());
        return found;
    }
    let mut left = LOOKS;
    walk(unit, 1, &mut left, device(unit), &mut found);
    found.caches.sort();
    found.stages.sort();
    found
}

/// The device `path` is on, not followed.
#[cfg(unix)]
fn device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).ok().map(|meta| meta.dev())
}

/// Off Unix every entry is taken to be on its folder's volume.
#[cfg(not(unix))]
fn device(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok().map(|_| 0)
}

/// Whether the unit `unit` is quiet enough at `now` for its caches to be judged, and by what
/// rule: its own folder unwritten for the wait, its receipt (when that is the rule) as old, and
/// custody allowing it. `None` keeps every cache of it without a look inside.
fn unit_rule(unit: &Path, now: SystemTime, pressed: bool) -> Option<(Duration, bool)> {
    let receipt = receipt(unit);
    let finished = receipt.is_some();
    let after = wait(finished, pressed);
    let unwritten = last_written(unit).is_some_and(|at| age(now, at) >= after);
    let settled = receipt.is_none_or(|at| age(now, at) >= after);
    (unwritten && settled && !managed(unit) && super::registered::retention::allows_tree(unit))
        .then_some((after, finished))
}

/// The caches `found` in a unit, each with its complete inventory (`None` where none could be
/// read), when the unit is quiet at `now`: nothing the look saw outside the caches
/// ([`Found::newest`]) and no cache it found was written within `after`. `None` when anything
/// was, which keeps every cache of the unit.
fn quiet(
    found: &Found,
    now: SystemTime,
    after: Duration,
) -> Option<Vec<(PathBuf, Option<super::Seen>)>> {
    if found.newest.is_none_or(|at| age(now, at) < after) {
        return None;
    }
    let mut caches = Vec::with_capacity(found.caches.len());
    for cache in &found.caches {
        let seen = inventory::retiring(cache).ok();
        if seen
            .as_ref()
            .is_some_and(|seen| age(now, seen.newest) < after)
        {
            return None;
        }
        caches.push((cache.clone(), seen));
    }
    Some(caches)
}

/// Advisory candidates among the leftovers under `home` at `now`: each tagged cache of a quiet
/// unit ([`quiet`]) whose complete inventory could be read, and each stage an earlier pass
/// left. Process use and cargo locks are checked by [`reclaim_on`], not by
/// this read; nothing is moved or removed.
#[must_use]
pub fn plan(home: &Path, now: SystemTime, pressed: bool) -> Vec<Planned> {
    let mut planned = Vec::new();
    for place in places(home) {
        for (stage, _) in place.stages() {
            planned.push(Planned::stage(stage));
        }
        for unit in place.units() {
            let Some((after, finished)) = unit_rule(&unit, now, pressed) else {
                continue;
            };
            let found = find(&unit);
            for (stage, _) in &found.stages {
                planned.push(Planned::stage(stage.clone()));
            }
            for (cache, seen) in quiet(&found, now, after).unwrap_or_default() {
                let Some(seen) = seen else { continue };
                let Some(holder) = cache.parent() else {
                    continue;
                };
                let Some(name) = cache.file_name() else {
                    continue;
                };
                if !super::registered::retention::allows_tree(&cache)
                    || super::stage_for(&cache, holder, &stage_base(name)).is_none()
                {
                    continue;
                }
                let idle = age(now, seen.newest);
                planned.push(Planned {
                    path: cache.clone(),
                    why: Unused::Leftover {
                        idle,
                        after,
                        finished,
                    },
                    paths: vec![cache],
                    held: super::hold(&seen.locks).is_none(),
                    stays: None,
                });
            }
        }
    }
    planned
}

/// Remove the leftovers under `home` that [`plan`] would list, `record` asked before each
/// (`false` keeps it), every outcome returned as [`super::reclaim_watched`] returns its own.
pub(super) fn reclaim_on(
    home: &Path,
    now: SystemTime,
    pressed: bool,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
    fence: &fence::Fence<'_>,
) -> Vec<(PathBuf, Unused)> {
    let mut intents = super::Intents::new(record);
    for place in places(home) {
        let mut sweep = super::Sweep {
            root: &place.root,
            home: Some(home),
            now,
            after: super::ReclaimAfter::by_floor(pressed).into(),
            placing: None,
            moved: false,
            compiler_store: None,
            intents: &mut intents,
            fence,
            permitted: &|| true,
            pruning: Vec::new(),
            declined: false,
        };
        for (stage, original) in place.stages() {
            if sweep.stage_recorded(&stage, &original) {
                sweep.finish_stage(&stage, &original);
            }
        }
        for unit in place.units() {
            aterm_uds::progress::tick();
            let Some((after, finished)) = unit_rule(&unit, now, pressed) else {
                continue;
            };
            let found = find(&unit);
            if found.caches.is_empty() && found.stages.is_empty() {
                continue;
            }
            // A process anywhere in the unit keeps all of it: a campaign between two builds
            // holds its scope, not its target directory.
            if !fence.clear(&unit, &unit) {
                continue;
            }
            for (stage, original) in &found.stages {
                if sweep.stage_recorded(stage, original) {
                    sweep.finish_stage(stage, original);
                }
            }
            for (cache, seen) in quiet(&found, now, after).unwrap_or_default() {
                match seen {
                    Some(seen) => {
                        take(&mut sweep, &cache, &seen, after, finished);
                    }
                    None => fence.note(&cache, KeepReason::InventoryUnavailable),
                }
            }
        }
    }
    intents.finish()
}

/// Take the tagged cache `cache` of a quiet unit whole, its complete inventory `seen` showing
/// it unwritten for `after`: every cargo lock of that inventory held and the inventory read
/// again under them, custody and process use checked before its intent and again after, then
/// renamed aside under a stage name of its own and removed there ([`super::Sweep::take`]).
fn take(
    sweep: &mut super::Sweep<'_, '_>,
    cache: &Path,
    seen: &super::Seen,
    after: Duration,
    finished: bool,
) -> bool {
    let (Some(holder), Some(name)) = (cache.parent(), cache.file_name()) else {
        return false;
    };
    let base = stage_base(name);
    let fence = sweep.fence;
    if !fence.admit(
        cache,
        KeepReason::Custody,
        super::registered::retention::allows_tree(cache),
    ) {
        return false;
    }
    let idle = age(sweep.now, seen.newest);
    if idle < after {
        return false;
    }
    let Some(held) = super::hold(&seen.locks) else {
        fence.note(cache, KeepReason::CargoLocks);
        return false;
    };
    let unchanged = inventory::retiring(cache)
        .is_ok_and(|again| again.newest == seen.newest && again.locks == seen.locks);
    let why = Unused::Leftover {
        idle,
        after,
        finished,
    };
    if !fence.admit(cache, KeepReason::InventoryChanged, unchanged)
        || !fence.admit(cache, KeepReason::Custody, tagged(cache))
        || !fence.admit(
            cache,
            KeepReason::CannotStage,
            super::stage_for(cache, holder, &base).is_some(),
        )
        || !fence.clear(cache, cache)
        || !fence.admit(
            cache,
            KeepReason::IntentDeclined,
            sweep.intents.ask(cache, why),
        )
    {
        return false;
    }
    if !fence.admit(
        cache,
        KeepReason::Custody,
        super::registered::retention::allows_tree(cache),
    ) || !fence.clear(cache, cache)
    {
        sweep.intents.stop();
        return false;
    }
    super::seams::staging(cache);
    let staged = super::stage_unkept(cache, holder, &base);
    drop(held);
    match staged {
        Ok(Some(stage)) => {
            sweep.intents.moved(&stage);
            sweep.take(&stage, cache.to_path_buf(), why, cache)
        }
        Ok(None) => {
            sweep.intents.stop();
            false
        }
        Err(error) => {
            sweep.intents.refused(super::Refusal::Rename(error.kind()));
            false
        }
    }
}

#[cfg(all(test, unix))]
#[path = "build_cache_leftovers_tests.rs"]
mod tests;
