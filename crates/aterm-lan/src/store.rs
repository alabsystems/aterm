// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spotlight's store on each mounted local volume, walked by the broker (SD-94): the one answer
//! the broker gives that is no LAN act. Spotlight's store (`/System/Volumes/Data/.Spotlight-V100`,
//! and each other volume's own) is root's alone, and the broker is the one root process aterm
//! already runs, so `sys why` reads the index's size through it instead of saying
//! `index_folder needs_privilege:root`.
//!
//! The answer is read-only and bounded:
//! - it opens only the store folders the broker names itself, each mounted local volume's
//!   [`STORE_NAME`] ([`Store::system`]); the request carries no field, so no client names a path;
//! - it reads metadata alone, by descriptor: each folder is opened from the folder that listed
//!   it (`openat` with `O_NOFOLLOW`) and only while it is still the folder that listing counted
//!   (its device and inode), and each entry is read with `fstatat` without following it. No
//!   path is resolved again, so a folder swapped for a symbolic link (or for another folder)
//!   after it was listed is not entered; no file's contents are read, no other device is
//!   entered, and nothing is written;
//! - it walks at most once every [`WALK_EVERY`], on a thread of its own, and keeps that walk's
//!   answer between walks; a request never waits on a walk longer than [`MAX_WALK`] and
//!   [`WAIT_PAST_WALK`], so a read stuck on a failing disk holds that one thread, never a
//!   connection;
//! - each walk stops at [`MAX_ENTRIES`] entries or after [`MAX_WALK`], and an answer it cut
//!   short says where ([`StoreBound`]): its figures are then a lower bound, never a whole answer.
//!   A mount table read short is answered as an error, never as a list missing some volumes.
//!
//! Indexing progress is not read: whether root can read it at all is owed a measurement made as
//! root on macOS 27 (`mdutil -s -v`, the store's item counts over time), which no agent of this
//! lab may make. Until that measurement is made, `sys why` keeps `index_progress unsupported`.

use std::collections::HashSet;
use std::ffi::{CString, OsStr};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_dirfd::{AtFlags, Dir, Errno, FileType, Mode, OFlags, Stat};
use aterm_uds::lan::wire::{SpotlightStores, StoreBound, StoreVolume};

use crate::counts::{Count, Counts};
use crate::server::Failure;

/// The store folder's name at a volume's root.
pub(crate) const STORE_NAME: &str = ".Spotlight-V100";

/// The broker walks at most this often and answers the walk it kept in between. The store grows
/// with what Spotlight indexes, over hours; a reading every ten minutes is the history's bucket
/// (`aterm_vitals::history`, 10-minute buckets), so a more frequent walk would add root I/O
/// without a figure anyone keeps.
pub(crate) const WALK_EVERY: Duration = Duration::from_secs(600);

/// The most entries one walk counts, across every volume: Spotlight's store holds hundreds to a
/// few thousand files (its index shards, journals and caches), so this bound is far over any
/// store and stops only a tree that is no store's. Each entry is one `fstatat`, so a walk that
/// reaches it costs well under a second of the broker's CPU once every [`WALK_EVERY`].
pub(crate) const MAX_ENTRIES: u64 = 100_000;

/// The longest one walk runs, across every volume: a hang detector for a slow or failing disk,
/// as a walk of a store's entries takes milliseconds. It is read between entries, so one read
/// that blocks outlasts it; a request therefore waits on a walk for at most this long and
/// [`WAIT_PAST_WALK`], and then answers the walk kept before it, or `ETIMEDOUT`.
pub(crate) const MAX_WALK: Duration = Duration::from_secs(10);

/// How long past [`MAX_WALK`] a request waits for a walk to end: the last entry's read and the
/// walk's handing over its answer.
pub(crate) const WAIT_PAST_WALK: Duration = Duration::from_secs(1);

/// The deepest folder a walk enters, below the store's own: a store is a few levels deep, and
/// each level entered holds one descriptor open until its folders are walked, so this bounds the
/// descriptors a walk holds. A folder deeper than this is counted, not entered, and the answer is
/// marked [`StoreBound::Unreadable`]: a folder it did not read.
pub(crate) const MAX_DEPTH: usize = 32;

/// The most volumes an answer lists: 64 mount points of at most 1024 bytes each, with their
/// figures, fit the wire's frame ([`aterm_uds::lan::wire::MAX_FRAME`]). More volumes with a store
/// than that is answered as an error, never as a list missing some.
pub(crate) const MAX_VOLUMES: usize = 64;

/// What one walk may spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    /// The most entries it counts.
    pub(crate) entries: u64,
    /// The longest it runs.
    pub(crate) time: Duration,
}

impl Bounds {
    /// The broker's own: [`MAX_ENTRIES`] and [`MAX_WALK`].
    pub(crate) const BROKER: Self = Self {
        entries: MAX_ENTRIES,
        time: MAX_WALK,
    };
}

/// The volumes to walk, each its mount point as the label and the folder that holds its store,
/// from the broker's own reading of the mount table, never from a request; `None` where that
/// reading came up short.
pub(crate) type Roots = Box<dyn Fn() -> Option<Vec<(String, PathBuf)>> + Send + Sync>;

/// The clock the walk cadence is read on.
pub(crate) type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

/// Who answers a [`aterm_uds::lan::wire::Request::SpotlightStore`].
pub(crate) enum Store {
    /// No one: the direct path ([`crate::direct`]) serves a process to itself, as its own user,
    /// who cannot read a store that is root's alone.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the direct path serves macOS only")
    )]
    Unserved,
    /// The broker's walker.
    Walked(Arc<Walker>),
}

/// The broker's walker: its roots, its bounds, its cadence, whether a walk runs and the walk it
/// keeps. A walk runs on a thread of its own ([`Walker::walk_and_keep`]); requests wait on
/// [`Walker::ended`] for a bounded time.
pub(crate) struct Walker {
    roots: Roots,
    bounds: Bounds,
    every: Duration,
    clock: Clock,
    state: Mutex<Walks>,
    ended: Condvar,
}

/// Whether a walk runs, and the last one's answer.
#[derive(Default)]
struct Walks {
    walking: bool,
    kept: Option<Kept>,
}

/// One walk's answer, kept until the next.
#[derive(Clone)]
struct Kept {
    ended: Instant,
    walk_ms: u64,
    /// Each store, or why the walk could not list them (a mount table read short).
    volumes: Result<Vec<StoreVolume>, Failure>,
}

impl Store {
    /// The broker's: each mounted local volume's store, [`Bounds::BROKER`], once every
    /// [`WALK_EVERY`].
    pub(crate) fn system() -> Self {
        Self::walked(
            Box::new(local_stores),
            Bounds::BROKER,
            Box::new(Instant::now),
        )
    }

    /// A walker of `roots` within `bounds`, its cadence read on `clock`.
    pub(crate) fn walked(roots: Roots, bounds: Bounds, clock: Clock) -> Self {
        Self::Walked(Arc::new(Walker {
            roots,
            bounds,
            every: WALK_EVERY,
            clock,
            state: Mutex::new(Walks::default()),
            ended: Condvar::new(),
        }))
    }

    /// The stores as last walked: the kept walk while it is younger than [`WALK_EVERY`]; else a
    /// new walk on the walker's thread ([`Count::StoreWalks`]), unless one already runs, waited
    /// on for at most [`MAX_WALK`] and [`WAIT_PAST_WALK`]. A walk still running then is answered
    /// by the walk kept before it, its age saying how old.
    ///
    /// # Errors
    /// `ENOTSUP` where no walker serves; `ETIMEDOUT` for a walk still running with none kept
    /// before it; `EIO` for a mount table read short; `E2BIG` for more than [`MAX_VOLUMES`]
    /// volumes with a store; or the `errno` of a walker thread that could not start.
    pub(crate) fn answer(&self, counts: &Counts) -> Result<SpotlightStores, Failure> {
        let Self::Walked(walker) = self else {
            return Err((
                libc::ENOTSUP,
                "only the root LAN broker reads Spotlight's stores, which are root's alone"
                    .to_owned(),
            ));
        };
        let mut state = walker.lock();
        let asked = (walker.clock)();
        let fresh = state
            .kept
            .as_ref()
            .is_some_and(|k| asked.saturating_duration_since(k.ended) < walker.every);
        if !fresh {
            if !state.walking {
                counts.add(Count::StoreWalks);
                state.walking = true;
                let on = Arc::clone(walker);
                let started = std::thread::Builder::new()
                    .name("lan-store-walk".into())
                    .spawn(move || on.walk_and_keep());
                if let Err(error) = started {
                    state.walking = false;
                    return Err((
                        error.raw_os_error().unwrap_or(libc::EAGAIN),
                        format!("no thread to walk Spotlight's stores on: {error}"),
                    ));
                }
            }
            state = walker
                .ended
                .wait_timeout_while(state, walker.bounds.time + WAIT_PAST_WALK, |s| s.walking)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(state, _)| state);
        }
        let Some(walk) = state.kept.clone() else {
            return Err((
                libc::ETIMEDOUT,
                format!(
                    "the walk of Spotlight's stores still runs past its {} s, and no walk was \
                     kept before it",
                    (walker.bounds.time + WAIT_PAST_WALK).as_secs()
                ),
            ));
        };
        drop(state);
        let volumes = walk.volumes?;
        if volumes.len() > MAX_VOLUMES {
            return Err((
                libc::E2BIG,
                format!(
                    "{} volumes have a Spotlight store, over the {MAX_VOLUMES} one answer lists",
                    volumes.len()
                ),
            ));
        }
        Ok(SpotlightStores {
            age_ms: millis((walker.clock)().saturating_duration_since(walk.ended)),
            walk_ms: walk.walk_ms,
            volumes,
        })
    }
}

impl Walker {
    fn lock(&self) -> MutexGuard<'_, Walks> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One walk, on the walker's own thread: its answer is kept, and the walk is marked ended
    /// (and its waiters woken) however it ends, a panic included.
    fn walk_and_keep(&self) {
        struct Ending<'a>(&'a Walker);
        impl Drop for Ending<'_> {
            fn drop(&mut self) {
                self.0.lock().walking = false;
                self.0.ended.notify_all();
            }
        }
        let ending = Ending(self);
        let started = Instant::now();
        let volumes = (self.roots)().map(|roots| walk(roots, self.bounds)).ok_or((
            libc::EIO,
            "the mount table was read short, so a volume's store could be missing from the \
             answer"
                .to_owned(),
        ));
        let kept = Kept {
            ended: (self.clock)(),
            walk_ms: millis(started.elapsed()),
            volumes,
        };
        self.lock().kept = Some(kept);
        drop(ending);
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Each mounted local volume's mount point, by the mount table (`getfsstat`): the volumes
/// macOS mounted itself (not an automount), in its order; `None` where the table was read short
/// (an error, or more mounts than it holds). Spotlight is macOS's alone.
fn local_stores() -> Option<Vec<(String, PathBuf)>> {
    #[cfg(target_os = "macos")]
    {
        let table = aterm_sysprobe::volumes::table();
        table.complete.then(|| {
            table
                .mounts
                .into_iter()
                .filter(|m| m.local && !m.automounted)
                .map(|m| (m.on.display().to_string(), m.on))
                .collect()
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(Vec::new())
    }
}

/// What one walk may still spend.
struct Budget {
    entries_left: u64,
    deadline: Instant,
}

impl Budget {
    /// Spend one entry: `Err` with the bound that stops the walk first.
    fn take(&mut self) -> Result<(), StoreBound> {
        if Instant::now() >= self.deadline {
            return Err(StoreBound::Time);
        }
        if self.entries_left == 0 {
            return Err(StoreBound::Entries);
        }
        self.entries_left -= 1;
        Ok(())
    }
}

/// Walk the store of each of `roots` within `bounds`, shared across them. A volume with no store
/// folder is not listed, and a store reached by two mount points is counted once. Once a bound
/// stops the walk, each store folder not yet reached is listed with nothing counted and that
/// bound: one open of each remaining store (at most the mount table's), so no store is left out
/// unmarked.
pub(crate) fn walk(roots: Vec<(String, PathBuf)>, bounds: Bounds) -> Vec<StoreVolume> {
    walk_between(roots, bounds, &mut || {})
}

/// [`walk`], with `listed` run after each folder is listed and before any folder it queued is
/// opened: a test's seam to change the store between the two.
pub(crate) fn walk_between(
    roots: Vec<(String, PathBuf)>,
    bounds: Bounds,
    listed: &mut dyn FnMut(),
) -> Vec<StoreVolume> {
    let mut budget = Budget {
        entries_left: bounds.entries,
        deadline: Instant::now() + bounds.time,
    };
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut stopped: Option<StoreBound> = None;
    let mut out = Vec::new();
    for (volume, mount) in roots {
        if let Some(bound) = stopped {
            if matches!(open_store(&mount), Opened::Absent) {
                continue;
            }
            out.push(StoreVolume {
                volume,
                bytes: 0,
                entries: 0,
                partial: Some(bound),
            });
            continue;
        }
        let Some(counted) = walk_one(volume, &mount, &mut budget, &mut seen, listed) else {
            continue;
        };
        if matches!(
            counted.partial,
            Some(StoreBound::Entries | StoreBound::Time)
        ) {
            stopped = counted.partial;
        }
        out.push(counted);
    }
    out
}

/// A volume's store folder, opened.
enum Opened {
    /// No store there: no such volume folder, or no folder (a link included) at [`STORE_NAME`].
    Absent,
    /// One the broker could not open.
    Unreadable,
    /// The store, and its own `fstat`.
    Store(OwnedFd, Stat),
}

/// What [`walk`] opens folders with: for listing, a folder, never through a link.
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// An open that found nothing to walk: no entry, no folder, or a link where a folder was asked.
fn absent(error: Errno) -> bool {
    matches!(
        error.raw_os_error(),
        libc::ENOENT | libc::ENOTDIR | libc::ELOOP
    )
}

/// The store under the volume folder `mount`: the folder itself opened by its path (the mount
/// table's), and the store opened from it by name, never through a link.
fn open_store(mount: &Path) -> Opened {
    let at = |result: aterm_dirfd::Result<OwnedFd>| match result {
        Ok(fd) => Ok(fd),
        Err(error) if absent(error) => Err(Opened::Absent),
        Err(_) => Err(Opened::Unreadable),
    };
    let opened = at(aterm_dirfd::openat(
        aterm_dirfd::CWD,
        mount,
        dir_flags(),
        Mode::empty(),
    ))
    .and_then(|mount| {
        at(aterm_dirfd::openat(
            &mount,
            STORE_NAME,
            dir_flags(),
            Mode::empty(),
        ))
    });
    match opened {
        Ok(store) => match aterm_dirfd::fstat(&store) {
            Ok(meta) => Opened::Store(store, meta),
            Err(_) => Opened::Unreadable,
        },
        Err(opened) => opened,
    }
}

/// A folder listed and counted, still to be walked: the listed folder it is in, its name there,
/// how deep it is below the store, and the device and inode it was counted with.
struct Queued {
    parent: Rc<Dir>,
    name: CString,
    depth: usize,
    id: (u64, u64),
}

/// A queued folder, opened for listing.
enum Listing {
    Dir(Dir),
    /// Spotlight removed it, or something else took its name, since it was counted: nothing more
    /// of it is read.
    Gone,
    Unreadable,
}

impl Queued {
    /// Open it from the folder that listed it, by name and without following a link, and only
    /// while it is still the folder counted.
    fn open(&self) -> Listing {
        let fd = match aterm_dirfd::openat(
            &*self.parent,
            OsStr::from_bytes(self.name.to_bytes()),
            dir_flags(),
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(error) if absent(error) => return Listing::Gone,
            Err(_) => return Listing::Unreadable,
        };
        match aterm_dirfd::fstat(&fd) {
            Ok(meta) if (meta.st_dev, meta.st_ino) == self.id => {}
            Ok(_) => return Listing::Gone,
            Err(_) => return Listing::Unreadable,
        }
        Dir::new(fd).map_or(Listing::Unreadable, Listing::Dir)
    }
}

/// One store: `None` where `mount` holds no store folder or one already counted.
fn walk_one(
    volume: String,
    mount: &Path,
    budget: &mut Budget,
    seen: &mut HashSet<(u64, u64)>,
    listed: &mut dyn FnMut(),
) -> Option<StoreVolume> {
    let mut counted = StoreVolume {
        volume,
        bytes: 0,
        entries: 0,
        partial: None,
    };
    let (store, meta) = match open_store(mount) {
        Opened::Absent => return None,
        Opened::Unreadable => {
            counted.partial = Some(StoreBound::Unreadable);
            return Some(counted);
        }
        Opened::Store(store, meta) => (store, meta),
    };
    if !seen.insert((meta.st_dev, meta.st_ino)) {
        return None;
    }
    let dev = meta.st_dev;
    if let Err(bound) = budget.take() {
        counted.partial = Some(bound);
        return Some(counted);
    }
    counted.bytes = allocated(&meta);
    counted.entries = 1;
    let mut next = match Dir::new(store) {
        Ok(dir) => Some((dir, 0)),
        Err(_) => {
            counted.partial = Some(StoreBound::Unreadable);
            return Some(counted);
        }
    };
    let mut queue: Vec<Queued> = Vec::new();
    loop {
        let (mut dir, depth) = match next.take() {
            Some(listing) => listing,
            None => {
                let Some(queued) = queue.pop() else {
                    break;
                };
                match queued.open() {
                    Listing::Dir(dir) => (dir, queued.depth),
                    Listing::Gone => continue,
                    Listing::Unreadable => {
                        counted.partial.get_or_insert(StoreBound::Unreadable);
                        continue;
                    }
                }
            }
        };
        let mut folders = Vec::new();
        while let Some(entry) = dir.next() {
            let Ok(entry) = entry else {
                counted.partial.get_or_insert(StoreBound::Unreadable);
                break;
            };
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            if let Err(bound) = budget.take() {
                counted.partial = Some(bound);
                return Some(counted);
            }
            // Read in the listed folder by name, a link as itself.
            let meta = match aterm_dirfd::fstatat(
                &dir,
                OsStr::from_bytes(name.to_bytes()),
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(meta) => meta,
                // Spotlight removed it while the walk ran: it is gone, not unread.
                Err(error) if error.is_noent() => continue,
                Err(_) => {
                    counted.partial.get_or_insert(StoreBound::Unreadable);
                    continue;
                }
            };
            counted.bytes = counted.bytes.saturating_add(allocated(&meta));
            counted.entries += 1;
            if FileType::from_raw_mode(meta.st_mode).is_dir() && meta.st_dev == dev {
                if depth < MAX_DEPTH {
                    folders.push((name.to_owned(), (meta.st_dev, meta.st_ino)));
                } else {
                    counted.partial.get_or_insert(StoreBound::Unreadable);
                }
            }
        }
        if !folders.is_empty() {
            let parent = Rc::new(dir);
            queue.extend(folders.into_iter().map(|(name, id)| Queued {
                parent: Rc::clone(&parent),
                name,
                depth: depth + 1,
                id,
            }));
        }
        listed();
    }
    Some(counted)
}

/// An entry's size on disk: its allocated 512-byte blocks.
fn allocated(meta: &Stat) -> u64 {
    meta.st_blocks.saturating_mul(512)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
