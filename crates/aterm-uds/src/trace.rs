// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a run read and compared, so that a later look can tell, without running it again,
//! whether running it again could answer otherwise: a thread's trace.
//!
//! A trace is recorded only while a [`scope`] is armed on the calling thread. With none armed,
//! each recorder here makes one thread-local look and records nothing, and each [`fs`] reader
//! makes that look and then std's own call, nothing more. A scope armed inside another is merged
//! into it as it closes, so the outer scope's trace holds everything the inner one recorded;
//! [`unrecorded`] runs code with no scope armed.
//!
//! What a trace holds:
//!
//! - **Reads.** A file's [`Stamp`] as an [`fs`] reader found it, or that nothing was there; a
//!   directory's names and their kinds ([`fs::read_dir`]); an owner's [`standing`], a digest of
//!   only what that file's readers decide by, which the owner computes again on replay; and a
//!   [`Watched`] value's or a [`Generation`]'s generation.
//! - **Clock comparisons.** Each comparison of a [`Now`] narrows the wall interval over which
//!   every comparison the run made answers the same ([`Wall`]); a [`Since`] that has not yet
//!   elapsed records its monotonic deadline.
//! - **What keeps a run from being kept** ([`Trace::unkept`]): an [`effect`] (a write, a spawn,
//!   a fetch, a log line, a note, a watched value changed), a [`retry`], a [`poison`] (among
//!   them [`Now::raw`], and any wall read `aterm_time::raw_reads` counted on the thread while
//!   the scope was armed), and a file read that changed within [`SETTLED`] of the read.
//!
//! A kept trace stands ([`Trace::stands`]) while every read shows the same, the wall clock stays
//! inside its interval and the monotonic clock short of its deadline. [`Trace::horizon`] names
//! when it may stop standing with no read moved, and what rerun a trace not kept owes.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher as _};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{
    Arc, LockResult, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, TryLockResult,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long before a read a file must last have changed for what the read found to be kept
/// while the file's [`Stamp`] stands: two seconds, a FAT clock's tick. A change made after the
/// read then takes a later time on any clock that ticks this coarsely or finer, so a later
/// `lstat` shows it, as git's index treats a file it stamped within its own tick as racily
/// clean. Without it, a rewrite in place at the same length, or a new file given a freed inode,
/// within one tick of a coarse clock would keep the stamp of the file it replaced. The one copy
/// of the rule: aterm-secrets' kept digests judge by [`settled`] too, and `private_file`'s kept
/// parses pin a file only this long after its change (`private_file::SETTLE`).
pub const SETTLED: Duration = Duration::from_secs(2);

/// [`SETTLED`] in nanoseconds.
const SETTLED_NS: i128 = SETTLED.as_nanos() as i128;

/// Whether a file whose contents or inode last changed at `changed_ns` (Unix nanoseconds: the
/// later of its modification and inode-change times) had settled by `now`: it changed
/// [`SETTLED`] or more before it. `false` where `now` is before the Unix epoch. A reader takes
/// `now` before the `lstat` whose times it judges, so a change after that `lstat` is after
/// `now` as well.
#[must_use]
pub fn settled(changed_ns: i128, now: SystemTime) -> bool {
    unix_ns(now).is_some_and(|now| changed_ns <= now - SETTLED_NS)
}

/// `at` in nanoseconds past the Unix epoch; `None` before it.
fn unix_ns(at: SystemTime) -> Option<i128> {
    at.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i128::try_from(since.as_nanos()).ok())
}

/// What kind of thing a path names, as `lstat` shows it (a link is a link, never its target).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Link,
    /// Anything else: a socket, a pipe, a device.
    Other,
}

impl Kind {
    /// The kind `file_type` names.
    #[must_use]
    pub fn of(file_type: std::fs::FileType) -> Self {
        if file_type.is_symlink() {
            Self::Link
        } else if file_type.is_dir() {
            Self::Dir
        } else if file_type.is_file() {
            Self::File
        } else {
            Self::Other
        }
    }
}

/// What one `lstat` or `stat` showed of a path: its kind, device and inode, its length, when
/// its contents and its inode last changed (in Unix nanoseconds), its mode, owner and link
/// count. A file renamed over the path, or one written since, shows another; a `chmod`,
/// `chown` or `link` moves the inode-change time, and the mode, owner and link count show those
/// on a clock too coarse to.
///
/// On Windows, where std's `Metadata` holds none of a file's identity, owner, access list or
/// names, the look also opens the name (as itself, or through a link for its target) and reads
/// the handle (`win::Facts`): the device is the volume's serial number, the inode the 128-bit
/// file id folded to 64 bits, the inode-change time the file's change time (which a write, an
/// attribute change and an access-list change move), the mode its attributes, the owner a
/// 32-bit digest of its owner and access list, and the link count its names. So a change of
/// who may use a file, or a second name given it or taken away, shows another. A file system
/// with no file ids (FAT, exFAT), which keeps no owner, access list or second name either,
/// shows no device or inode there and its creation time for its change time; a file whose
/// descriptor this user may not read shows a zero owner. Elsewhere off Unix, std's stable API
/// holds no device, inode, owner or link count (each is zero), the inode-change time is the
/// creation time, and the mode is the read-only bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Meta {
    /// What it is.
    pub kind: Kind,
    /// The device it is on (on Windows, its volume's serial number).
    pub dev: u64,
    /// Its inode (on Windows, its file id folded to 64 bits).
    pub ino: u64,
    /// Its length in bytes.
    pub len: u64,
    /// When its contents last changed.
    pub mtime_ns: i128,
    /// When its inode last changed (on Windows its change time; elsewhere off Unix, when it was
    /// made).
    pub ctime_ns: i128,
    /// Its mode, type bits included (on Windows, its attributes).
    pub mode: u32,
    /// Its owner (on Windows, a digest of its owner and access list).
    pub uid: u32,
    /// Its link count.
    pub nlink: u64,
}

impl Meta {
    /// What `meta` shows.
    #[cfg(unix)]
    #[must_use]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let ns = |secs: i64, nsec: i64| i128::from(secs) * 1_000_000_000 + i128::from(nsec);
        Self {
            kind: Kind::of(meta.file_type()),
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
            mode: meta.mode(),
            uid: meta.uid(),
            nlink: meta.nlink(),
        }
    }

    /// What `meta`, std's look at a Windows name, shows with `facts`, what the name's handle
    /// shows beyond it (the look's own open of the same name).
    #[cfg(windows)]
    fn of(meta: &std::fs::Metadata, facts: &win::Facts) -> Self {
        let ns = |at: io::Result<SystemTime>| at.ok().and_then(unix_ns).unwrap_or(0);
        Self {
            kind: Kind::of(meta.file_type()),
            dev: facts.dev,
            ino: facts.ino,
            len: meta.len(),
            mtime_ns: ns(meta.modified()),
            ctime_ns: facts.changed_ns.unwrap_or_else(|| ns(meta.created())),
            mode: facts.attributes,
            uid: facts.access,
            nlink: facts.nlink,
        }
    }

    /// What `meta` shows, of what std holds off Unix and Windows.
    #[cfg(not(any(unix, windows)))]
    #[must_use]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        let ns = |at: io::Result<SystemTime>| at.ok().and_then(unix_ns).unwrap_or(0);
        Self {
            kind: Kind::of(meta.file_type()),
            dev: 0,
            ino: 0,
            len: meta.len(),
            mtime_ns: ns(meta.modified()),
            ctime_ns: ns(meta.created()),
            mode: u32::from(meta.permissions().readonly()),
            uid: 0,
            nlink: 0,
        }
    }

    /// The later of its two change times: what [`settled`] judges.
    #[must_use]
    pub fn changed_ns(&self) -> i128 {
        self.mtime_ns.max(self.ctime_ns)
    }
}

/// What a read of a path saw of it without reading its contents: the `lstat` of the path, and,
/// where the path is a link whose target is there and the read may follow it (any but
/// [`fs::opening_itself`]'s), the `stat` of what it names. A link swapped for another moves the
/// first; a link left alone whose target is replaced moves the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    /// The path's own `lstat`.
    pub own: Meta,
    /// The `stat` of the link's target; `None` where the path is no link, a link whose target
    /// is missing, or a name read as itself ([`fs::opening_itself`]).
    pub target: Option<Meta>,
}

impl Stamp {
    /// The latest change time either half shows: what [`settled`] judges.
    #[must_use]
    pub fn changed_ns(&self) -> i128 {
        self.target.map_or(self.own.changed_ns(), |target| {
            target.changed_ns().max(self.own.changed_ns())
        })
    }
}

/// Whether `error` says that nothing is at the path: it, or a folder on the way to it, is
/// missing, or a step on the way is no folder.
fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Whether a read that failed with `error`, after a stamp of its path was recorded, could
/// answer otherwise with no stamp moved: an error that no stamp decides (an I/O fault, an
/// interruption), unlike a path that is missing, a folder, refused by its mode or owner, or
/// not text.
fn transient(error: &io::Error) -> bool {
    !missing(error)
        && !matches!(
            error.kind(),
            io::ErrorKind::PermissionDenied
                | io::ErrorKind::IsADirectory
                | io::ErrorKind::InvalidData
        )
}

/// How a look at a path reaches what it names, which its replay reaches the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Reach {
    /// The path's own `lstat`, and for a link the `stat` of what it names: what a reader that
    /// may follow a link decides by.
    Followed,
    /// The path's own `lstat` alone, for a reader that opens the name itself and refuses a link
    /// there ([`fs::opening_itself`]): a link's target is never looked at, and on Windows the
    /// look is made with this thread's cloud placeholders exposed
    /// (`aterm_dirfd::expose_placeholders`), as such a reader's own open is, so a placeholder
    /// shows as the reparse point it is; where they cannot be exposed nothing is looked at.
    Itself,
}

/// One half of a [`Look`]: std's answer, and on Windows what the name's handle shows beyond it.
struct Half {
    /// std's `lstat` or `stat`.
    meta: std::fs::Metadata,
    /// What the name, opened as itself (the `lstat` half) or through a link (the `stat` half),
    /// shows beyond `meta`.
    #[cfg(windows)]
    facts: io::Result<win::Facts>,
}

impl Half {
    /// std's `meta` of `path`, with, on Windows, the facts of the name opened as `follow` says.
    #[cfg_attr(
        not(windows),
        expect(unused_variables, reason = "only Windows opens the name")
    )]
    fn of(meta: std::fs::Metadata, path: &Path, follow: bool) -> Self {
        Self {
            #[cfg(windows)]
            facts: win::Facts::of(path, follow),
            meta,
        }
    }

    /// The [`Meta`] this half shows; on Windows the error where its handle could not be read.
    fn stamp(&self) -> Result<Meta, &io::Error> {
        #[cfg(windows)]
        {
            self.facts.as_ref().map(|facts| Meta::of(&self.meta, facts))
        }
        #[cfg(not(windows))]
        {
            Ok(Meta::of(&self.meta))
        }
    }
}

/// The `lstat` of a path, and for a link reached through ([`Reach::Followed`]) the `stat` of
/// what it names: everything a [`Stamp`] is made of, kept whole so that a reader's answer comes
/// from the very calls its record does.
struct Look {
    /// The `lstat`.
    own: io::Result<Half>,
    /// The `stat`, taken only where the `lstat` showed a link and the look follows one.
    target: Option<io::Result<Half>>,
}

impl Look {
    /// Look at `path` now, reaching what it names as `reach` says.
    fn of(path: &Path, reach: Reach) -> Self {
        #[cfg(windows)]
        let _exposed = match reach {
            Reach::Itself => match aterm_dirfd::expose_placeholders() {
                Ok(exposed) => Some(exposed),
                Err(error) => {
                    return Self {
                        own: Err(io::Error::from(error)),
                        target: None,
                    };
                }
            },
            Reach::Followed => None,
        };
        let own = std::fs::symlink_metadata(path).map(|meta| Half::of(meta, path, false));
        let target = match (&own, reach) {
            (Ok(half), Reach::Followed) if half.meta.file_type().is_symlink() => {
                Some(std::fs::metadata(path).map(|meta| Half::of(meta, path, true)))
            }
            _ => None,
        };
        Self { own, target }
    }

    /// The stamp this shows: `None` where nothing is at the path; the error where the path, or
    /// a link's target, could not be examined.
    fn stamp(&self) -> Result<Option<Stamp>, &io::Error> {
        let own = match &self.own {
            Ok(half) => half.stamp()?,
            Err(error) if missing(error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let target = match &self.target {
            None => None,
            Some(Ok(half)) => Some(half.stamp()?),
            Some(Err(error)) if missing(error) => None,
            Some(Err(error)) => return Err(error),
        };
        Ok(Some(Stamp { own, target }))
    }

    /// What `lstat` answered.
    fn own(self) -> io::Result<std::fs::Metadata> {
        self.own.map(|half| half.meta)
    }

    /// What `stat` would have answered: the link's target where the path is a link.
    fn followed(self) -> io::Result<std::fs::Metadata> {
        self.target.unwrap_or(self.own).map(|half| half.meta)
    }
}

/// The Windows half of a look: what a name's handle shows beyond std's `Metadata`.
#[cfg(windows)]
#[path = "trace_win.rs"]
mod win;

/// The wall time a read here is judged settled as of ([`settled`]): the clock, and in a test
/// (this crate's own, or another crate's in a build with debug assertions) the time this
/// thread's [`read_as_if_later`] or [`read_as_if_at`] puts in its place.
fn read_now() -> SystemTime {
    let now = SystemTime::now();
    #[cfg(any(test, debug_assertions))]
    let now = scripted_read_time(now);
    now
}

/// The time a test thread's reads are judged settled as of, where the clock reads `now`.
#[cfg(any(test, debug_assertions))]
fn scripted_read_time(now: SystemTime) -> SystemTime {
    match READ_CLOCK.get() {
        ReadClock::Clock => now,
        ReadClock::Later(by) => now + by,
        ReadClock::At(at) => at,
    }
}

/// The time a test thread's reads are judged settled as of.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy)]
enum ReadClock {
    /// The clock.
    Clock,
    /// As much later than the clock ([`read_as_if_later`]).
    Later(Duration),
    /// This instant ([`read_as_if_at`]).
    At(SystemTime),
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// The time this thread's reads are judged settled as of.
    static READ_CLOCK: Cell<ReadClock> = const { Cell::new(ReadClock::Clock) };
}

/// Run `f` with this thread's reads judged by `clock`, put back as `f` returns or unwinds.
#[cfg(any(test, debug_assertions))]
fn reading_by<T>(clock: ReadClock, f: impl FnOnce() -> T) -> T {
    struct Restore(ReadClock);
    impl Drop for Restore {
        fn drop(&mut self) {
            READ_CLOCK.set(self.0);
        }
    }
    let _restore = Restore(READ_CLOCK.replace(clock));
    f()
}

/// Run `f` with each read this thread records judged settled ([`settled`]) as of `by` later than
/// the clock: a test's files have stood settled without its waiting for them to.
#[cfg(any(test, debug_assertions))]
pub fn read_as_if_later<T>(by: Duration, f: impl FnOnce() -> T) -> T {
    reading_by(ReadClock::Later(by), f)
}

/// Run `f` with each read this thread records judged settled ([`settled`]) as of `at`, whatever
/// the clock says: a test judges a read at a set distance from its file's change.
#[cfg(any(test, debug_assertions))]
pub fn read_as_if_at<T>(at: SystemTime, f: impl FnOnce() -> T) -> T {
    reading_by(ReadClock::At(at), f)
}

/// A wall interval, in whole Unix seconds and inclusive at both ends, over which every
/// comparison of a [`Now`] a run made answers as it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wall {
    /// The earliest second.
    pub lo: i64,
    /// The latest second.
    pub hi: i64,
}

impl Wall {
    /// Every second: what a run that compared nothing holds.
    pub const ALWAYS: Self = Self {
        lo: i64::MIN,
        hi: i64::MAX,
    };

    /// Whether `secs` is inside.
    #[must_use]
    pub const fn holds(self, secs: i64) -> bool {
        self.lo <= secs && secs <= self.hi
    }

    /// The seconds both hold: an empty interval (`lo > hi`) where they share none.
    fn meet(self, lo: i64, hi: i64) -> Self {
        Self {
            lo: self.lo.max(lo),
            hi: self.hi.min(hi),
        }
    }
}

impl Default for Wall {
    fn default() -> Self {
        Self::ALWAYS
    }
}

/// What a run did that keeps its trace from being kept: running it again may do it again, or
/// may find what it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    /// A file written.
    Write,
    /// A process or a thread started.
    Spawn,
    /// A fetch over the network.
    Fetch,
    /// A log line written.
    Log,
    /// A note handed to whoever tells a person.
    Note,
    /// A [`Watched`] value or a [`Generation`] changed.
    Memory,
}

/// Why a trace is not kept ([`Trace::unkept`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unkept {
    /// The run used something the trace cannot replay ([`poison`]).
    Poison(Cow<'static, str>),
    /// The run did not finish its work ([`retry`]).
    Retry(Cow<'static, str>),
    /// The run did something ([`effect`]).
    Effect(Effect),
    /// The run read a file that changed within [`SETTLED`] of the read; every such file has
    /// settled at `until_ns` (Unix nanoseconds).
    Unsettled {
        /// When the last of them settles.
        until_ns: i128,
    },
}

/// What [`Trace::stands`] found moved, the first of them in the order it looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Moved {
    /// The trace was never kept, so it stands for nothing.
    Unkept(Unkept),
    /// The wall clock is outside the trace's interval.
    Wall,
    /// The monotonic clock reached the trace's deadline.
    Mono,
    /// A watched generation moved (its id).
    Memory(&'static str),
    /// A file read shows another stamp, or nothing, or something where nothing was.
    File(PathBuf),
    /// A directory listed holds other names or kinds, or is gone, or came.
    Listing(PathBuf),
    /// An owner's standing of a file computes to another digest.
    Standing(PathBuf),
}

/// How a trace not kept for an effect, a retry or a poison owes its rerun ([`Horizon::rerun`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rerun {
    /// After [`backoff`] of the runs not kept in a row.
    Backoff,
    /// After [`RERUN_CAP`]: a poisoned run keeps the cadence of one a minute.
    Cap,
}

impl Rerun {
    /// How long after the run's end the rerun is owed, where this run is the `unkept_in_a_row`th
    /// not kept in a row.
    #[must_use]
    pub fn after(self, unkept_in_a_row: u32) -> Duration {
        match self {
            Self::Backoff => backoff(unkept_in_a_row),
            Self::Cap => RERUN_CAP,
        }
    }
}

/// The first rerun a run not kept owes ([`backoff`]).
pub const RERUN_FIRST: Duration = Duration::from_secs(10);

/// The longest a run not kept waits for its rerun ([`backoff`]), and a poisoned run's wait.
pub const RERUN_CAP: Duration = Duration::from_secs(60);

/// The rerun the `unkept_in_a_row`th run not kept in a row owes: [`RERUN_FIRST`], doubled for
/// each run not kept before it in the row, capped at [`RERUN_CAP`]: 10 s, 20 s, 40 s, then
/// 60 s for every one after. A zeroth is treated as the first.
#[must_use]
pub fn backoff(unkept_in_a_row: u32) -> Duration {
    let doublings = unkept_in_a_row.saturating_sub(1).min(3);
    RERUN_FIRST.saturating_mul(1 << doublings).min(RERUN_CAP)
}

/// When a trace may stop standing with no read moved, and what rerun it owes where it is not
/// kept ([`Trace::horizon`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Horizon {
    /// The earliest wall instant, in Unix nanoseconds, that may answer otherwise: the first
    /// second past the trace's wall interval, or, for a trace not kept for a read that had not
    /// settled, the moment the last such read settles; the earlier of the two. `None` for
    /// neither.
    pub wall_ns: Option<i128>,
    /// The earliest monotonic deadline a [`Since`] compared against and had not reached.
    pub mono: Option<Instant>,
    /// The rerun a trace not kept for an effect, a retry or a poison owes.
    pub rerun: Option<Rerun>,
}

/// A digest an owner computes of a file it reads, of only what that file's readers decide by
/// ([`standing`]), and the owner's way to compute it again.
#[derive(Clone, Debug)]
struct Standing {
    /// The file.
    path: PathBuf,
    /// The digest; `None` where the owner found none to take (the file missing or unreadable).
    value: Option<[u8; 32]>,
    /// The owner's digest of `path` now.
    replay: fn(&Path) -> Option<[u8; 32]>,
}

impl Standing {
    /// Whether `other` records the same standing.
    fn same(&self, other: &Self) -> bool {
        self.path == other.path
            && self.value == other.value
            && std::ptr::fn_addr_eq(self.replay, other.replay)
    }
}

/// Where a recorded generation is read again on replay: the counter of a [`Watched`] value or
/// of a [`Generation`], shared, so a trace can outlive the value it read.
#[derive(Clone)]
struct Current(Arc<AtomicU64>);

impl Current {
    /// The generation now.
    fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Whether both read the same counter.
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Current {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Current({})", self.get())
    }
}

/// A generation a run read ([`Watched::lock`], [`Generation::record`]).
#[derive(Clone, Debug)]
struct Memory {
    /// What it counts.
    id: &'static str,
    /// The generation the run read.
    generation: u64,
    /// Where it is read again.
    current: Current,
}

/// What a run read and compared while a [`scope`] was armed, and what keeps it from being kept.
#[derive(Clone, Debug, Default)]
pub struct Trace {
    /// Each file read, by how its looks reached it, with every stamp the run saw of it (more
    /// than one where it changed between two reads); `None` for nothing there.
    files: BTreeMap<(PathBuf, Reach), Vec<Option<Stamp>>>,
    /// Each directory listed, with every digest of its names and kinds the run saw; `None` for
    /// no directory there.
    listings: BTreeMap<PathBuf, Vec<Option<[u8; 32]>>>,
    /// Each owner's standing recorded.
    standings: Vec<Standing>,
    /// Each generation read.
    memory: Vec<Memory>,
    /// The interval every comparison of a [`Now`] answers the same over.
    wall: Wall,
    /// The earliest monotonic deadline a [`Since`] had not reached.
    mono: Option<Instant>,
    /// When the last read that had not settled settles (Unix nanoseconds).
    unsettled_until_ns: Option<i128>,
    /// What the run did.
    effects: BTreeSet<Effect>,
    /// Why the run did not finish its work.
    retries: Vec<Cow<'static, str>>,
    /// What the run used that cannot be replayed.
    poisons: Vec<Cow<'static, str>>,
}

impl Trace {
    /// Why this trace may not be kept: the first poison, else the first retry, else the first
    /// effect, else a read that had not settled; `None` for a trace that may be kept.
    #[must_use]
    pub fn unkept(&self) -> Option<Unkept> {
        if let Some(why) = self.poisons.first() {
            return Some(Unkept::Poison(why.clone()));
        }
        if let Some(why) = self.retries.first() {
            return Some(Unkept::Retry(why.clone()));
        }
        if let Some(effect) = self.effects.first() {
            return Some(Unkept::Effect(*effect));
        }
        self.unsettled_until_ns
            .map(|until_ns| Unkept::Unsettled { until_ns })
    }

    /// Whether this trace stands at wall time `now` and monotonic time `mono`, both taken no
    /// earlier than the run: it may be kept ([`Self::unkept`]), `now` is inside its wall
    /// interval, `mono` is short of its deadline, each generation it read is the same, and one
    /// `lstat` each (and a `stat` for a link a read may follow), one listing each and one owner's
    /// digest each show what the run saw. It looks under [`unrecorded`], so a scope armed around it records none
    /// of its looks.
    ///
    /// # Errors
    /// The first thing found moved, in that order.
    pub fn stands(&self, now: Now, mono: Instant) -> Result<(), Moved> {
        if let Some(why) = self.unkept() {
            return Err(Moved::Unkept(why));
        }
        self.moved(now, mono)
    }

    /// [`Self::stands`] for what the run read and compared alone, whether or not it may be kept:
    /// `now` is inside its wall interval, `mono` short of its deadline, each generation it read
    /// the same, and each file, listing and owner's digest shows what the run saw. An owner that
    /// keeps a run not kept (one that did something, or must run again) asks this to tell
    /// whether an input of it moved since, apart from the rerun it owes ([`Self::horizon`]).
    ///
    /// # Errors
    /// The first thing found moved, in [`Self::stands`]'s order.
    pub fn moved(&self, now: Now, mono: Instant) -> Result<(), Moved> {
        if !self.wall.holds(now.0) {
            return Err(Moved::Wall);
        }
        if self.mono.is_some_and(|deadline| mono >= deadline) {
            return Err(Moved::Mono);
        }
        if let Some(memory) = self
            .memory
            .iter()
            .find(|memory| memory.current.get() != memory.generation)
        {
            return Err(Moved::Memory(memory.id));
        }
        unrecorded(|| {
            for ((path, reach), seen) in &self.files {
                match (seen.as_slice(), Look::of(path, *reach).stamp()) {
                    ([seen], Ok(now)) if *seen == now => {}
                    _ => return Err(Moved::File(path.clone())),
                }
            }
            for (dir, seen) in &self.listings {
                match (seen.as_slice(), names_now(dir)) {
                    ([seen], Ok(now)) if *seen == now => {}
                    _ => return Err(Moved::Listing(dir.clone())),
                }
            }
            for standing in &self.standings {
                if (standing.replay)(&standing.path) != standing.value {
                    return Err(Moved::Standing(standing.path.clone()));
                }
            }
            Ok(())
        })
    }

    /// When this trace may stop standing with no read moved, and the rerun it owes where it is
    /// not kept for an effect, a retry or a poison.
    #[must_use]
    pub fn horizon(&self) -> Horizon {
        let past_wall =
            (self.wall.hi < i64::MAX).then(|| (i128::from(self.wall.hi) + 1) * 1_000_000_000);
        let wall_ns = match (past_wall, self.unsettled_until_ns) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let rerun = if !self.poisons.is_empty() {
            Some(Rerun::Cap)
        } else if !self.retries.is_empty() || !self.effects.is_empty() {
            Some(Rerun::Backoff)
        } else {
            None
        };
        Horizon {
            wall_ns,
            mono: self.mono,
            rerun,
        }
    }

    /// Each file read, with the stamp it showed (`None` for nothing there), once per stamp seen.
    pub fn files(&self) -> impl Iterator<Item = (&Path, Option<&Stamp>)> {
        self.files.iter().flat_map(|((path, _reach), seen)| {
            seen.iter()
                .map(move |stamp| (path.as_path(), stamp.as_ref()))
        })
    }

    /// Each directory listed.
    pub fn listings(&self) -> impl Iterator<Item = &Path> {
        self.listings.keys().map(PathBuf::as_path)
    }

    /// Each file an owner's standing was recorded of.
    pub fn standings(&self) -> impl Iterator<Item = &Path> {
        self.standings
            .iter()
            .map(|standing| standing.path.as_path())
    }

    /// Each generation read: what it counts, and the generation.
    pub fn memory(&self) -> impl Iterator<Item = (&'static str, u64)> {
        self.memory
            .iter()
            .map(|memory| (memory.id, memory.generation))
    }

    /// The interval every comparison of a [`Now`] answers the same over.
    #[must_use]
    pub fn wall(&self) -> Wall {
        self.wall
    }

    /// The earliest monotonic deadline a [`Since`] had not reached.
    #[must_use]
    pub fn mono_deadline(&self) -> Option<Instant> {
        self.mono
    }

    /// What the run did.
    pub fn effects(&self) -> impl Iterator<Item = Effect> {
        self.effects.iter().copied()
    }

    /// Add what `other` recorded, as if this run had recorded it too.
    fn absorb(&mut self, other: &Self) {
        for ((path, reach), seen) in &other.files {
            for stamp in seen {
                self.add_file(path, *reach, *stamp);
            }
        }
        for (dir, seen) in &other.listings {
            for names in seen {
                self.add_listing(dir, *names);
            }
        }
        for standing in &other.standings {
            self.add_standing(standing.clone());
        }
        for memory in &other.memory {
            self.add_memory(memory.clone());
        }
        self.wall = self.wall.meet(other.wall.lo, other.wall.hi);
        self.mono = earlier(self.mono, other.mono);
        self.unsettled_until_ns = match (self.unsettled_until_ns, other.unsettled_until_ns) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.effects.extend(other.effects.iter().copied());
        for why in &other.retries {
            add_reason(&mut self.retries, why.clone());
        }
        for why in &other.poisons {
            add_reason(&mut self.poisons, why.clone());
        }
    }

    /// Record that `path`, reached as `reach` says, showed `stamp`.
    fn add_file(&mut self, path: &Path, reach: Reach, stamp: Option<Stamp>) {
        let seen = self.files.entry((path.to_path_buf(), reach)).or_default();
        if !seen.contains(&stamp) {
            seen.push(stamp);
        }
    }

    /// Record that `dir` listed as `names`.
    fn add_listing(&mut self, dir: &Path, names: Option<[u8; 32]>) {
        let seen = self.listings.entry(dir.to_path_buf()).or_default();
        if !seen.contains(&names) {
            seen.push(names);
        }
    }

    /// Record `standing`.
    fn add_standing(&mut self, standing: Standing) {
        if !self.standings.iter().any(|kept| kept.same(&standing)) {
            self.standings.push(standing);
        }
    }

    /// Record `memory`.
    fn add_memory(&mut self, memory: Memory) {
        if !self.memory.iter().any(|kept| {
            kept.id == memory.id
                && kept.generation == memory.generation
                && kept.current.same(&memory.current)
        }) {
            self.memory.push(memory);
        }
    }

    /// Record that a file not settled at its read settles at `until_ns`.
    fn add_unsettled(&mut self, until_ns: i128) {
        self.unsettled_until_ns = Some(
            self.unsettled_until_ns
                .map_or(until_ns, |was| was.max(until_ns)),
        );
    }
}

/// The earlier of two optional deadlines.
fn earlier(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Add `why` to `reasons` unless it is there.
fn add_reason(reasons: &mut Vec<Cow<'static, str>>, why: Cow<'static, str>) {
    if !reasons.contains(&why) {
        reasons.push(why);
    }
}

/// One armed scope: what it has recorded so far, and the thread's raw wall reads when it was
/// armed.
struct Frame {
    /// What the scope has recorded.
    trace: Trace,
    /// `aterm_time::raw_reads` when the scope was armed.
    raw_reads_at: u64,
}

std::thread_local! {
    /// This thread's armed scopes, innermost last.
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    /// Whether this thread is within [`keeper`].
    static KEEPER: Cell<bool> = const { Cell::new(false) };
}

/// Whether a scope is armed on this thread, so the recorders record. Never on a thread whose
/// scopes have been torn down as it exits: a read or a write made from another thread-local's
/// destructor after that records nothing, and does not panic, though every shared reader and
/// writer reaches this thread's scopes.
#[must_use]
pub fn armed() -> bool {
    FRAMES
        .try_with(|frames| !frames.borrow().is_empty())
        .unwrap_or(false)
}

/// Record into the innermost scope armed on this thread, where one is; nothing on a thread
/// whose scopes have been torn down as it exits ([`armed`]).
fn record(add: impl FnOnce(&mut Trace)) {
    let _ = FRAMES.try_with(|frames| {
        if let Some(frame) = frames.borrow_mut().last_mut() {
            add(&mut frame.trace);
        }
    });
}

/// Run `f` with a scope armed on this thread, and answer what it returned and the trace it
/// recorded. A scope armed within `f` is merged into this one as it closes. The trace is
/// poisoned where `aterm_time::raw_reads` moved on this thread during `f` (a raw read of the
/// wall clock anywhere in its extent, within [`unrecorded`] too), and where `f` unwound, in
/// which case the trace merges into the scope around this one, if any, as the unwinding leaves.
pub fn scope<R>(f: impl FnOnce() -> R) -> (R, Trace) {
    /// Closes the scope, as `f` returns or unwinds.
    struct Closing {
        /// Whether the scope was closed after `f` returned.
        closed: bool,
    }
    impl Closing {
        /// Close the scope after `f` returned.
        fn close(mut self) -> Trace {
            self.closed = true;
            close_frame(false)
        }
    }
    impl Drop for Closing {
        fn drop(&mut self) {
            if !self.closed {
                close_frame(true);
            }
        }
    }
    FRAMES.with_borrow_mut(|frames| {
        frames.push(Frame {
            trace: Trace::default(),
            raw_reads_at: aterm_time::raw_reads(),
        });
    });
    let closing = Closing { closed: false };
    let out = f();
    (out, closing.close())
}

/// Close the innermost scope: poison it for raw wall reads, and where `unwound`, for that; merge
/// it into the scope around it; answer it.
fn close_frame(unwound: bool) -> Trace {
    let Some(frame) = FRAMES.with_borrow_mut(Vec::pop) else {
        return Trace::default();
    };
    let mut trace = frame.trace;
    let raw = aterm_time::raw_reads().wrapping_sub(frame.raw_reads_at);
    if raw != 0 {
        add_reason(
            &mut trace.poisons,
            Cow::Owned(format!("the wall clock was read raw {raw} times")),
        );
    }
    if unwound {
        add_reason(&mut trace.poisons, Cow::Borrowed("the scope unwound"));
    }
    record(|parent| parent.absorb(&trace));
    trace
}

/// Run `f` with no scope armed on this thread: nothing `f` reads or compares is recorded, and a
/// scope `f` arms records apart, merged into no scope around this call. A raw wall read within
/// `f` still poisons each scope armed around it, since that count is the thread's.
pub fn unrecorded<R>(f: impl FnOnce() -> R) -> R {
    /// Puts the scopes back, as `f` returns or unwinds.
    struct Restore(Vec<Frame>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let frames = std::mem::take(&mut self.0);
            FRAMES.with_borrow_mut(|now| *now = frames);
        }
    }
    let _restore = Restore(FRAMES.with_borrow_mut(std::mem::take));
    f()
}

/// Run `f`, whose readings of the wall clock the caller sets aside: it compares through none of
/// them and puts a reading of its own in their place (a [`Now`] it holds). A raw read within `f`
/// (`aterm_time::raw_reads` moving) poisons no scope armed around this call, as it otherwise
/// would; everything else `f` reads, compares and does is recorded as ever, and a scope armed
/// within `f` still counts the raw reads made in it, its poison reaching the scopes around it as
/// it closes. A caller that compares through what `f` read raw leaves its trace standing past
/// a change of the clock: set aside only what is replaced.
pub fn clock_set_aside<R>(f: impl FnOnce() -> R) -> R {
    let before = aterm_time::raw_reads();
    let out = f();
    let read = aterm_time::raw_reads().wrapping_sub(before);
    if read != 0 {
        let _ = FRAMES.try_with(|frames| {
            for frame in frames.borrow_mut().iter_mut() {
                frame.raw_reads_at = frame.raw_reads_at.wrapping_add(read);
            }
        });
    }
    out
}

/// Add `trace` to the innermost scope armed on this thread, as if the run had recorded it: a
/// run skipped because a kept trace of it stands records what that trace did.
pub fn absorb(trace: &Trace) {
    record(|top| top.absorb(trace));
}

/// Run `f` as the keeper's thread ([`in_keeper`]).
pub fn keeper<R>(f: impl FnOnce() -> R) -> R {
    /// Restores the thread's mark, as `f` returns or unwinds.
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            KEEPER.set(self.0);
        }
    }
    let _restore = Restore(KEEPER.replace(true));
    f()
}

/// Whether this thread is running within [`keeper`].
#[must_use]
pub fn in_keeper() -> bool {
    KEEPER.get()
}

/// Record `effect` in the armed scope.
pub fn effect(effect: Effect) {
    record(|top| {
        top.effects.insert(effect);
    });
}

/// Record that the run did not finish its work, for `why`: it is not kept, and owes a rerun
/// after [`backoff`].
pub fn retry(why: impl Into<Cow<'static, str>>) {
    if armed() {
        let why = why.into();
        record(|top| add_reason(&mut top.retries, why));
    }
}

/// Record that the run used something the trace cannot replay, for `why`: it is not kept, and
/// owes a rerun after [`RERUN_CAP`].
pub fn poison(why: impl Into<Cow<'static, str>>) {
    if armed() {
        let why = why.into();
        record(|top| add_reason(&mut top.poisons, why));
    }
}

/// Record that the run read `path`, which showed `look` reached as `reach` says, at `read_at`
/// (taken before the look).
fn record_look(path: &Path, reach: Reach, look: &Look, read_at: SystemTime) {
    match look.stamp() {
        Ok(stamp) => {
            let unsettled = stamp
                .filter(|stamp| !settled(stamp.changed_ns(), read_at))
                .map(|stamp| stamp.changed_ns() + SETTLED_NS);
            record(|top| {
                top.add_file(path, reach, stamp);
                if let Some(until_ns) = unsettled {
                    top.add_unsettled(until_ns);
                }
            });
        }
        Err(error) => {
            let why = format!("{}: {error}", path.display());
            record(|top| {
                top.add_file(path, reach, None);
                add_reason(&mut top.retries, Cow::Owned(why));
            });
        }
    }
}

/// Record a read of a file that failed with `error` after its stamp was recorded, where no
/// stamp decides that error ([`transient`]).
fn record_failed_read(path: &Path, error: &io::Error) {
    if transient(error) {
        retry(format!("{}: {error}", path.display()));
    }
}

/// Record that the run read `path` and found `value`, an owner's digest of only what that
/// file's readers decide by, which `replay` computes again: the trace stands while
/// `replay(path)` answers `value`, so a rewrite that leaves it the same moves nothing. The
/// owner reads the file, and computes `value`, under [`unrecorded`], so no stamp of it is
/// recorded beside; `replay` is called under [`unrecorded`] too.
pub fn standing(path: &Path, value: Option<[u8; 32]>, replay: fn(&Path) -> Option<[u8; 32]>) {
    if armed() {
        let standing = Standing {
            path: path.to_path_buf(),
            value,
            replay,
        };
        record(|top| top.add_standing(standing));
    }
}

/// The digest of what [`fs::read_dir`] decides a listing by: each name and the kind it names,
/// in name order.
fn names_digest(entries: &[std::fs::DirEntry]) -> io::Result<[u8; 32]> {
    let mut named = entries
        .iter()
        .map(|entry| Ok((entry.file_name(), Kind::of(entry.file_type()?))))
        .collect::<io::Result<Vec<_>>>()?;
    named.sort();
    let mut digest = aterm_digest::Sha256::new();
    for (name, kind) in &named {
        let bytes = name.as_encoded_bytes();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
        digest.update([*kind as u8]);
    }
    Ok(digest.finalize())
}

/// The entries of `dir`, read whole.
fn list(dir: &Path) -> io::Result<Vec<std::fs::DirEntry>> {
    std::fs::read_dir(dir)?.collect()
}

/// What a listing of `dir` decides by now: `None` where there is no directory there.
fn names_now(dir: &Path) -> Result<Option<[u8; 32]>, ()> {
    match list(dir) {
        Ok(entries) => names_digest(&entries).map(Some).map_err(|_| ()),
        Err(error) if missing(&error) => Ok(None),
        Err(_) => Err(()),
    }
}

/// The wall clock's reading a run compares against, in whole Unix seconds (floored). Its
/// comparisons answer as `i64` arithmetic would, exactly (no saturation), and each narrows the
/// armed scope's wall interval to the seconds over which it answers the same. [`Self::raw`]
/// hands the number out and poisons the scope.
#[derive(Clone, Copy, Debug)]
pub struct Now(i64);

impl Now {
    /// The reading `secs`.
    #[must_use]
    pub const fn at(secs: i64) -> Self {
        Self(secs)
    }

    /// The wall clock now. It does not move `aterm_time::raw_reads`, so a run may take its
    /// reading here and compare through it.
    #[must_use]
    pub fn at_now() -> Self {
        let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            Err(before) => {
                let before = before.duration();
                let whole = before.as_secs() + u64::from(before.subsec_nanos() > 0);
                i64::try_from(whole).map_or(i64::MIN, |whole| -whole)
            }
        };
        Self(secs)
    }

    /// The reading as a number, for a use no comparison here states: the armed scope is
    /// poisoned.
    #[must_use]
    pub fn raw(self) -> i64 {
        poison("a raw read of now");
        self.0
    }

    /// Whether `at` has come: `now >= at`.
    #[must_use]
    pub fn passed(self, at: i64) -> bool {
        self.at_least(i128::from(at))
    }

    /// Whether `at` is still to come: `now < at`.
    #[must_use]
    pub fn before(self, at: i64) -> bool {
        !self.at_least(i128::from(at))
    }

    /// Whether `span` has gone by since `since`: `now - since >= span`.
    #[must_use]
    pub fn aged(self, since: i64, span: i64) -> bool {
        self.at_least(i128::from(since) + i128::from(span))
    }

    /// Whether `now` is `at` or less than `span` past it: `0 <= now - at < span`. Never, for a
    /// `span` of zero or less, which narrows nothing.
    #[must_use]
    pub fn within(self, at: i64, span: i64) -> bool {
        let start = i128::from(at);
        let end = start + i128::from(span);
        let now = i128::from(self.0);
        if end <= start {
            false
        } else if now < start {
            narrow(i128::MIN, start - 1);
            false
        } else if now >= end {
            narrow(end, i128::MAX);
            false
        } else {
            narrow(start, end - 1);
            true
        }
    }

    /// Whether `now >= bound`, narrowing the wall to the seconds that answer the same.
    fn at_least(self, bound: i128) -> bool {
        if i128::from(self.0) >= bound {
            narrow(bound, i128::MAX);
            true
        } else {
            narrow(i128::MIN, bound - 1);
            false
        }
    }
}

/// The reading `secs` ([`Now::at`]): a function that compares through a [`Now`] takes
/// `impl Into<Now>`, so a caller that holds a number of seconds hands it in as it is, while a run
/// a scope traces hands in its own reading and never reads it raw.
impl From<i64> for Now {
    fn from(secs: i64) -> Self {
        Self(secs)
    }
}

/// Narrow the armed scope's wall interval to `lo..=hi`, clamped to `i64`.
fn narrow(lo: i128, hi: i128) {
    let clamp =
        |secs: i128| i64::try_from(secs).unwrap_or(if secs < 0 { i64::MIN } else { i64::MAX });
    let (lo, hi) = (clamp(lo), clamp(hi));
    record(|top| top.wall = top.wall.meet(lo, hi));
}

/// A monotonic start a run compares the time since against ([`Self::elapsed_at_least`]).
#[derive(Clone, Copy, Debug)]
pub struct Since(Instant);

impl Since {
    /// Time since `start`.
    #[must_use]
    pub const fn new(start: Instant) -> Self {
        Self(start)
    }

    /// Whether `span` has gone by since the start, as of `now`: `now >= start + span`. Where it
    /// has not, the armed scope records `start + span` as a deadline; where it has, it records
    /// nothing, since a later instant answers the same. Never, for a `start + span` past what
    /// an `Instant` holds.
    #[must_use]
    pub fn elapsed_at_least(self, span: Duration, now: Instant) -> bool {
        match self.0.checked_add(span) {
            Some(deadline) if now >= deadline => true,
            Some(deadline) => {
                record(|top| top.mono = earlier(top.mono, Some(deadline)));
                false
            }
            None => false,
        }
    }
}

/// Record, in the armed scope, that the run read generation `generation` of `id`, read again
/// from `current`.
fn record_memory(id: &'static str, generation: u64, current: impl FnOnce() -> Current) {
    if armed() {
        let memory = Memory {
            id,
            generation,
            current: current(),
        };
        record(|top| top.add_memory(memory));
    }
}

/// The hash a [`Watched`] value's generation moves by.
fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A value in memory that runs read and change, behind a [`Mutex`], with a generation that moves
/// whenever a change leaves it otherwise: a guard ([`Self::lock`]) hashes the value as it is
/// taken and again as it drops, and only a changed hash moves the generation, records
/// [`Effect::Memory`] in the scope armed on the dropping thread and rings the [`bell`]. A
/// change undone before the guard drops moves nothing. Taking a guard records the generation
/// it found in the scope armed on the taking thread. A static or any other value may be one,
/// shared behind an `Arc` or borrowed; a trace that read it keeps its counter, not the value.
#[derive(Debug)]
pub struct Watched<T> {
    /// What it holds, for a trace's record.
    id: &'static str,
    /// The value.
    value: Mutex<T>,
    /// How many changes have left it otherwise: made by the first armed look or the first
    /// change, so a lock with no scope armed that changes nothing allocates nothing.
    count: OnceLock<Arc<AtomicU64>>,
}

impl<T> Watched<T> {
    /// `value`, at generation zero, known to traces as `id`.
    pub const fn new(id: &'static str, value: T) -> Self {
        Self {
            id,
            value: Mutex::new(value),
            count: OnceLock::new(),
        }
    }

    /// The generation now.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.count
            .get()
            .map_or(0, |count| count.load(Ordering::Acquire))
    }

    /// What it holds, for a trace's record.
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// The value, taken out as [`Mutex::into_inner`] takes it. Nothing can lock it after, so
    /// the generation stays where it was.
    ///
    /// # Errors
    /// The lock is poisoned ([`Mutex::into_inner`]); the error's
    /// [`PoisonError::into_inner`] is the value as the panic left it.
    pub fn into_inner(self) -> LockResult<T> {
        self.value.into_inner()
    }

    /// The generation's counter, made at its first use.
    fn counter(&self) -> &Arc<AtomicU64> {
        self.count.get_or_init(|| Arc::new(AtomicU64::new(0)))
    }
}

impl<T: Hash> Watched<T> {
    /// The value, locked as [`Mutex::lock`] locks it, with its generation recorded in the armed
    /// scope. As there, a guard a panic dropped poisons the lock: the next lock answers `Err`,
    /// whose [`PoisonError::into_inner`] is the guard over the value as the panic left it, and
    /// that guard watches the value as any other does.
    ///
    /// # Errors
    /// The lock is poisoned ([`Mutex::lock`]).
    pub fn lock(&self) -> LockResult<WatchedGuard<'_, T>> {
        match self.value.lock() {
            Ok(guard) => Ok(self.watch(guard)),
            Err(poisoned) => Err(PoisonError::new(self.watch(poisoned.into_inner()))),
        }
    }

    /// The value, where its lock is free now, as [`Mutex::try_lock`] takes it; else as
    /// [`Self::lock`].
    ///
    /// # Errors
    /// The lock is held ([`TryLockError::WouldBlock`]), which records nothing, or poisoned
    /// ([`TryLockError::Poisoned`]).
    pub fn try_lock(&self) -> TryLockResult<WatchedGuard<'_, T>> {
        match self.value.try_lock() {
            Ok(guard) => Ok(self.watch(guard)),
            Err(TryLockError::Poisoned(poisoned)) => Err(TryLockError::Poisoned(PoisonError::new(
                self.watch(poisoned.into_inner()),
            ))),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// Watch the value `guard` holds: record its generation in the armed scope and hash it.
    fn watch<'a>(&'a self, guard: MutexGuard<'a, T>) -> WatchedGuard<'a, T> {
        if armed() {
            let count = self.counter();
            record_memory(self.id, count.load(Ordering::Acquire), || {
                Current(Arc::clone(count))
            });
        }
        let hash = hash_of(&*guard);
        WatchedGuard {
            owner: self,
            guard,
            hash,
        }
    }
}

/// A [`Watched`] value, locked ([`Watched::lock`]).
#[derive(Debug)]
pub struct WatchedGuard<'a, T: Hash> {
    /// Whose value it is.
    owner: &'a Watched<T>,
    /// The lock.
    guard: MutexGuard<'a, T>,
    /// The value's hash when the guard was taken.
    hash: u64,
}

impl<T: Hash> std::ops::Deref for WatchedGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: Hash> std::ops::DerefMut for WatchedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T: Hash> Drop for WatchedGuard<'_, T> {
    fn drop(&mut self) {
        if hash_of(&*self.guard) != self.hash {
            self.owner.counter().fetch_add(1, Ordering::AcqRel);
            effect(Effect::Memory);
            bell();
        }
    }
}

/// A generation an owner keeps of something in memory a [`Watched`] does not hold whole (a part
/// of a structure its readers decide by, such as which rows a table of sign-ins carries): the
/// owner moves it ([`Self::bump`]) whenever what it counts changes, and a run that reads what it
/// counts records it ([`Self::record`]). A clone counts the same.
#[derive(Clone, Debug)]
pub struct Generation {
    /// What it counts, for a trace's record.
    id: &'static str,
    /// The count.
    count: Arc<AtomicU64>,
}

impl Generation {
    /// Generation zero of `id`.
    #[must_use]
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The generation now.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// Record, in the armed scope, that the run read what this counts, at the generation now.
    pub fn record(&self) {
        record_memory(self.id, self.get(), || Current(Arc::clone(&self.count)));
    }

    /// Count a change: the generation moves, [`Effect::Memory`] is recorded in the armed scope,
    /// and the [`bell`] rings.
    pub fn bump(&self) {
        self.count.fetch_add(1, Ordering::AcqRel);
        effect(Effect::Memory);
        bell();
    }
}

/// A listener [`bell`] calls.
type Listener = Arc<dyn Fn() + Send + Sync>;

/// The process's listeners, each under the number [`listen`] gave it.
static LISTENERS: Mutex<Vec<(u64, Listener)>> = Mutex::new(Vec::new());

/// The number the next [`listen`] gives.
static NEXT_LISTENER: AtomicU64 = AtomicU64::new(0);

/// How many times the bell has rung in this process.
static RINGS: AtomicU64 = AtomicU64::new(0);

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// How many times this thread rang the bell ([`rung_here`]).
    static RUNG_HERE: Cell<u64> = const { Cell::new(0) };
}

/// Ring the process's bell: something in memory a trace may have read has changed. Each
/// listener ([`listen`]) is called on the ringing thread, after the listeners' lock is let go,
/// so it may itself listen or stop. A listener must not wait: it is called wherever a
/// [`Watched`] guard drops, its lock still held.
pub fn bell() {
    RINGS.fetch_add(1, Ordering::AcqRel);
    #[cfg(any(test, debug_assertions))]
    RUNG_HERE.set(RUNG_HERE.get().wrapping_add(1));
    let listeners: Vec<Listener> = LISTENERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(_, listener)| Arc::clone(listener))
        .collect();
    for listener in listeners {
        listener();
    }
}

/// How many times the bell has rung in this process, wrapping at `u64::MAX`.
#[must_use]
pub fn rings() -> u64 {
    RINGS.load(Ordering::Acquire)
}

/// How many times this thread has rung the bell: a test's count, which no other thread's
/// rings move.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn rung_here() -> u64 {
    RUNG_HERE.get()
}

/// Call `ring` at each [`bell`] until the answer drops.
#[must_use = "the listener stops when this drops"]
pub fn listen(ring: impl Fn() + Send + Sync + 'static) -> Listening {
    let id = NEXT_LISTENER.fetch_add(1, Ordering::AcqRel);
    LISTENERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((id, Arc::new(ring)));
    Listening(id)
}

/// A listener the [`bell`] calls ([`listen`]) until this drops.
#[derive(Debug)]
pub struct Listening(u64);

impl Drop for Listening {
    fn drop(&mut self) {
        LISTENERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _)| *id != self.0);
    }
}

/// The readers a traced run reads the file system through: each answers as std's call of the
/// same name does, and, with a scope armed, records what it read first. A path's record is its
/// [`Stamp`] (or that nothing was there), taken before the read, so a change after it moves the
/// stamp a replay looks at; a directory's is its names and kinds; a resolved path's
/// ([`fs::canonicalize`]) is a standing of where it resolved to. A look that fails for any
/// reason but a missing path records a [`retry`], as does a read that fails for a reason no
/// stamp decides. A reader that opens a file its own way records the same through
/// [`fs::opening`] and [`fs::failed`]: this crate's `atomic_file` and `regular_file` readers
/// do; one that opens the name itself and refuses a link there records through
/// [`fs::opening_itself`], as `private_file`'s readers do.
pub mod fs {
    use super::{
        Look, Reach, armed, list, missing, names_digest, read_now, record, record_failed_read,
    };
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// How many files a traced read opened ([`opened`]).
    static OPENED: AtomicU64 = AtomicU64::new(0);

    /// How many times, in this process, a reader here (or one that reads its own way and says so
    /// through [`opening`]) opened a file to read it with a scope armed: what traced runs cost
    /// in reads of contents, beside the stamps a replay takes, which open nothing. It only
    /// rises, wrapping at `u64::MAX`; a reader subtracts two readings.
    #[must_use]
    pub fn opened() -> u64 {
        OPENED.load(Ordering::Relaxed)
    }

    /// Look at `path` with a scope armed, reaching what it names as `reach` says: record its
    /// stamp, and answer the look.
    fn looked(path: &Path, reach: Reach) -> Look {
        let read_at = read_now();
        let look = Look::of(path, reach);
        super::record_look(path, reach, &look, read_at);
        look
    }

    #[cfg(test)]
    std::thread_local! {
        /// How many of [`opened`]'s opens this thread made ([`opened_here`]).
        static OPENED_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// [`opened`]'s opens made on this thread: a test's count, which no other thread moves.
    #[cfg(test)]
    pub(crate) fn opened_here() -> u64 {
        OPENED_HERE.get()
    }

    /// [`looked`], for a read that then opens `path`: counted ([`opened`]).
    fn looked_to_open(path: &Path, reach: Reach) -> Look {
        OPENED.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        OPENED_HERE.set(OPENED_HERE.get().wrapping_add(1));
        looked(path, reach)
    }

    /// Record, with a scope armed, what `path` shows now, for a reader that opens and reads it
    /// its own way (one that refuses a link, judges the file's owner and mode, or bounds its
    /// length) and calls this before its open, as each reader here takes its stamp before its
    /// read: a change after the call moves the stamp a replay looks at. The open is counted
    /// ([`opened`]). With none armed it does nothing, and looks at nothing.
    pub fn opening(path: &Path) {
        if armed() {
            looked_to_open(path, Reach::Followed);
        }
    }

    /// [`opening`], for a reader that opens the name `path` itself and refuses a link there
    /// (`private_file`'s, which never follows one): what is recorded is the name's own `lstat`
    /// alone, never a link's target, and its replay looks the same way, so a link there whose
    /// target changes moves nothing, while the link replaced or removed moves it. On Windows the
    /// look is made with this thread's cloud placeholders exposed, as such a reader's own open
    /// is; where they cannot be exposed nothing is looked at and a retry is recorded. Counted
    /// ([`opened`]); with no scope armed it does nothing, and looks at nothing.
    pub fn opening_itself(path: &Path) {
        if armed() {
            looked_to_open(path, Reach::Itself);
        }
    }

    /// Record, with a scope armed, that a read of `path` whose stamp [`opening`] recorded failed
    /// with `error`: a [`super::retry`] where no stamp decides that error, as this module's own
    /// readers record one. An error of a kind a stamp decides (a missing path, a folder, a
    /// refusal by mode or owner, invalid data) records nothing more.
    pub fn failed(path: &Path, error: &io::Error) {
        if armed() {
            record_failed_read(path, error);
        }
    }

    /// `std::fs::symlink_metadata`: what `lstat` shows of `path`.
    ///
    /// # Errors
    /// As `std::fs::symlink_metadata`.
    pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<std::fs::Metadata> {
        let path = path.as_ref();
        if !armed() {
            return std::fs::symlink_metadata(path);
        }
        looked(path, Reach::Followed).own()
    }

    /// `std::fs::metadata`: what `stat` shows of `path`, a link followed.
    ///
    /// # Errors
    /// As `std::fs::metadata`.
    pub fn metadata(path: impl AsRef<Path>) -> io::Result<std::fs::Metadata> {
        let path = path.as_ref();
        if !armed() {
            return std::fs::metadata(path);
        }
        looked(path, Reach::Followed).followed()
    }

    /// `Path::is_file`: whether `path`, a link followed, is a regular file.
    #[must_use]
    pub fn is_file(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if !armed() {
            return path.is_file();
        }
        looked(path, Reach::Followed)
            .followed()
            .is_ok_and(|meta| meta.is_file())
    }

    /// `Path::is_dir`: whether `path`, a link followed, is a directory.
    #[must_use]
    pub fn is_dir(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if !armed() {
            return path.is_dir();
        }
        looked(path, Reach::Followed)
            .followed()
            .is_ok_and(|meta| meta.is_dir())
    }

    /// `Path::exists`: whether `path`, a link followed, names anything.
    #[must_use]
    pub fn exists(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if !armed() {
            return path.exists();
        }
        looked(path, Reach::Followed).followed().is_ok()
    }

    /// `std::fs::read`: the contents of `path`.
    ///
    /// # Errors
    /// As `std::fs::read`.
    pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        let path = path.as_ref();
        if !armed() {
            return std::fs::read(path);
        }
        looked_to_open(path, Reach::Followed);
        std::fs::read(path).inspect_err(|error| record_failed_read(path, error))
    }

    /// `std::fs::read_to_string`: the contents of `path`, as text.
    ///
    /// # Errors
    /// As `std::fs::read_to_string`.
    pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
        let path = path.as_ref();
        if !armed() {
            return std::fs::read_to_string(path);
        }
        looked_to_open(path, Reach::Followed);
        std::fs::read_to_string(path).inspect_err(|error| record_failed_read(path, error))
    }

    /// `std::fs::File::open`: `path`, opened for reading.
    ///
    /// # Errors
    /// As `std::fs::File::open`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<std::fs::File> {
        let path = path.as_ref();
        if !armed() {
            return std::fs::File::open(path);
        }
        looked_to_open(path, Reach::Followed);
        std::fs::File::open(path).inspect_err(|error| record_failed_read(path, error))
    }

    /// `std::fs::read_dir`, read whole: the entries of `dir`, in the order the system lists
    /// them. With a scope armed it records the names and the kinds they name ([`super::Kind`]),
    /// so a file rewritten in place, or renamed over one of the same name, moves nothing, while
    /// a name that comes, goes or names another kind does.
    ///
    /// # Errors
    /// As `std::fs::read_dir`, or the first entry that could not be read.
    pub fn read_dir(dir: impl AsRef<Path>) -> io::Result<Vec<std::fs::DirEntry>> {
        let dir = dir.as_ref();
        if !armed() {
            return list(dir);
        }
        let listed = list(dir);
        let names = match &listed {
            Ok(entries) => names_digest(entries)
                .map(Some)
                .map_err(|error| error.to_string()),
            Err(error) if missing(error) => Ok(None),
            Err(error) => Err(error.to_string()),
        };
        match names {
            Ok(names) => record(|top| top.add_listing(dir, names)),
            Err(error) => {
                let why = format!("{}: {error}", dir.display());
                record(|top| {
                    top.add_listing(dir, None);
                    super::add_reason(&mut top.retries, why.into());
                });
            }
        }
        listed
    }

    /// `std::fs::canonicalize`: `path` with each link along it followed and each `.` and `..`
    /// resolved. With a scope armed it records a [`super::standing`] of what it answered,
    /// replayed by resolving `path` again, so a link along it swapped for another, or a step
    /// that comes or goes, moves the trace while a rewrite of what it names does not; a failure
    /// for any reason but a missing path records a [`super::retry`] beside.
    ///
    /// # Errors
    /// As `std::fs::canonicalize`.
    pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<std::path::PathBuf> {
        let path = path.as_ref();
        if !armed() {
            return std::fs::canonicalize(path);
        }
        let resolved = std::fs::canonicalize(path);
        super::standing(path, resolution(&resolved), resolved_now);
        if let Err(error) = &resolved
            && !missing(error)
        {
            super::retry(format!("{}: {error}", path.display()));
        }
        resolved
    }

    /// The standing a [`canonicalize`] records of its answer: the digest of the path it resolved
    /// to; `None` where it resolved to nothing.
    fn resolution(resolved: &io::Result<std::path::PathBuf>) -> Option<[u8; 32]> {
        let resolved = resolved.as_ref().ok()?;
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm-uds/trace/canonicalize/v1\0");
        digest.update(resolved.as_os_str().as_encoded_bytes());
        Some(digest.finalize())
    }

    /// What a [`canonicalize`] of `path` would record now: its replay.
    fn resolved_now(path: &Path) -> Option<[u8; 32]> {
        resolution(&std::fs::canonicalize(path))
    }
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
