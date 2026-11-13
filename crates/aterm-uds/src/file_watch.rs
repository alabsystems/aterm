// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Path-aware file events shared by documents and headless observers.
//! [`FileEvents`] reports raw invalidations; [`FileWatch`] adds debounce and stamp filtering.
//!
//! [`FileWatch`] runs one thread. It waits on the platform's file events
//! through a [`FileEvents`] arm (kqueue on macOS, inotify on Linux, directory
//! change notifications on Windows, a stat poll elsewhere, and [`InertEvents`]
//! for a test that drives the host by hand), lets a burst of them settle
//! ([`Debounce`]), and tells its listener
//! which watched files really changed: a new [`FileStamp`] since the last one
//! it told. Its first look at a newly watched file always tells, because the
//! host read the file before the watch armed and a change in between is
//! folded into any baseline the watch takes itself (the host's re-read finds
//! no change when there was none). It watches each file's directory as well
//! as the file, so a save that replaces the file by rename (a new inode, as
//! most editors, formatters and `git checkout` save) is heard too, as is a
//! deleted file made again, and after each burst it arms again on whatever
//! file the path names now. Where the directory is missing it watches the
//! nearest one above it that is there, whose every change is a look (each
//! finding no change until the file is made), so a file whose folder does
//! not exist yet, or went with it, is heard once both are made. Every folder
//! above that one, up to the root, listens only for its own rename, end or
//! change of mode, one listener per folder however many files' paths run
//! through it, so a folder of the path renamed (a project folder moved, a
//! checkout swapped in) or made unsearchable is heard as well. On Windows a
//! listener is an open handle on its folder, and no folder above an open
//! handle can be renamed, moved or sent to the Recycle Bin, so that arm
//! listens only inside the account's application-data and temporary
//! folders and every other file is looked at on a clock instead (`Looks`),
//! as is one its host asks that of ([`FileWatch::watch_looked`]). A file no arm
//! took (a folder refused it, descriptors or watches ran out, or its folders
//! kept being made or moved while it armed) is armed again every
//! `REARM_EVERY`, and only while some file is unarmed; each round that still
//! finds it unarmed looks at it, so its change is told with no event to tell
//! of it. A look tells a path missing from one it could not look at, and a
//! link made again from the one it replaced; a file whose look could not see
//! it is looked at in the rounds too, since no event need tell when that
//! ends. A relative path is watched where it names when it is watched.
//!
//! A folder is watched the same way ([`FileWatch::watch_directory`]), on its
//! entries ([`FileEvents::arm_directory`]): each settled burst of them tells,
//! whatever the folder's stamp says, since a folder's times need not tell two
//! entries made inside one tick apart. While it is missing, or where no arm
//! hears a folder's entries (the stat poll), it is watched as a file is, by its
//! stamp, and its making is told.
//!
//! The watch only tells: the host re-reads the file through the document's
//! own capability and decides (reload a clean buffer in place, or offer
//! reload or keep over unsaved edits).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long a file's events must stop before the watch looks at it: one save
/// is often several events (truncate, write, rename, attributes).
pub const QUIET: Duration = Duration::from_millis(80);

/// The longest a file that keeps changing waits to be looked at.
pub const SETTLE_CAP: Duration = Duration::from_millis(600);

/// How often the stat-poll arm looks where no file events exist.
#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
const POLL_EVERY: Duration = Duration::from_millis(500);

/// How often the watch arms again a file no arm took ([`Unarmed`]), while one is left: no event
/// tells it the arm would take now.
const REARM_EVERY: Duration = Duration::from_millis(500);

/// How soon the watch looks again at the files it only looks at ([`Looks`]) after a round that
/// found one changed, or after one more came to be watched.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The longest the watch rests between two rounds of looks: each round that finds nothing
/// changed doubles the rest up to this.
const LOOK_SLOWEST: Duration = Duration::from_secs(2);

/// Wakes a parked [`FileEvents::wait`] from another thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// What tells one version of a file from the next: its device and inode
/// (which a replace by rename changes), length, and modification and change
/// times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    dev: u64,
    ino: u64,
    len: u64,
    modified_ns: i128,
    changed_ns: i128,
}

impl FileStamp {
    /// The stamp of the file `path` names now, or `None` when it names none (or cannot be
    /// looked at).
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        std::fs::metadata(path)
            .ok()
            .map(|meta| Self::from_metadata(&meta))
    }

    fn from_metadata(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Self {
                dev: meta.dev(),
                ino: meta.ino(),
                len: meta.size(),
                modified_ns: i128::from(meta.mtime()) * 1_000_000_000
                    + i128::from(meta.mtime_nsec()),
                changed_ns: i128::from(meta.ctime()) * 1_000_000_000
                    + i128::from(meta.ctime_nsec()),
            }
        }
        #[cfg(not(unix))]
        {
            let modified_ns = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos() as i128);
            Self {
                dev: 0,
                ino: 0,
                len: meta.len(),
                modified_ns,
                changed_ns: 0,
            }
        }
    }
}

/// What one look at a watched path found: the link it is, if it is one, and what it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Look {
    /// The link's own stamp, where the path is one: a link made again to name the same file is
    /// a new one.
    link: Option<FileStamp>,
    found: Found,
}

/// What a path names, as [`Look`] found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    /// Nothing there.
    Missing,
    /// It could not be looked at: a folder on the way this user cannot search, an I/O error, a
    /// loop of links. No event need tell such a refusal's end ([`Unarmed`]).
    Unreadable,
    /// A file or a folder, with this stamp.
    At(FileStamp),
}

impl Look {
    /// Look at `path` now. Only an answer that nothing is there is [`Found::Missing`]; any
    /// other refusal is [`Found::Unreadable`].
    fn of(path: &Path) -> Self {
        let refused = |error: io::Error| {
            if error.kind() == io::ErrorKind::NotFound {
                Found::Missing
            } else {
                Found::Unreadable
            }
        };
        let link = match std::fs::symlink_metadata(path) {
            Ok(entry) => entry
                .file_type()
                .is_symlink()
                .then(|| FileStamp::from_metadata(&entry)),
            Err(error) => {
                return Self {
                    link: None,
                    found: refused(error),
                };
            }
        };
        let found = std::fs::metadata(path)
            .map_or_else(refused, |meta| Found::At(FileStamp::from_metadata(&meta)));
        Self { link, found }
    }
}

/// Lets each watched file's burst of events settle: a file is due once its
/// events stop for [`QUIET`], or [`SETTLE_CAP`] after the first of them.
#[derive(Clone, Debug, Default)]
pub struct Debounce {
    /// Each noisy file's first and latest event.
    pending: BTreeMap<u64, (Instant, Instant)>,
    /// With multiple pending files, one schedulable deadline per key, ordered by
    /// time. A single file uses its entry directly, without a second tree.
    deadlines: BTreeSet<(Instant, u64)>,
}

impl Debounce {
    /// `key`'s file had an event at `now`.
    pub fn note(&mut self, key: u64, now: Instant) {
        let (old, new) = match self.pending.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (first, latest) = *entry.get();
                entry.get_mut().1 = now;
                (Self::at(first, latest), Self::at(first, now))
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((now, now));
                (None, Self::at(now, now))
            }
        };
        if self.pending.len() == 1 {
            return;
        }
        if self.pending.len() == 2 && self.deadlines.is_empty() {
            self.deadlines
                .extend(self.pending.iter().filter_map(|(key, (first, latest))| {
                    Self::at(*first, *latest).map(|at| (at, *key))
                }));
        } else if old != new {
            if let Some(at) = old {
                self.deadlines.remove(&(at, key));
            }
            if let Some(at) = new {
                self.deadlines.insert((at, key));
            }
        }
    }

    /// Forget `key` (it is no longer watched).
    pub fn forget(&mut self, key: u64) {
        if let Some((first, latest)) = self.pending.remove(&key)
            && let Some(at) = Self::at(first, latest)
        {
            self.deadlines.remove(&(at, key));
        }
        if self.pending.len() == 1 {
            self.deadlines.clear();
        }
    }

    /// The files due at `now`, which leave the debounce.
    pub fn due(&mut self, now: Instant) -> Vec<u64> {
        let mut due = Vec::new();
        if self.pending.len() == 1 {
            if self.deadline().is_some_and(|at| at <= now)
                && let Some((key, _)) = self.pending.pop_first()
            {
                due.push(key);
            }
            return due;
        }
        while let Some(&(at, key)) = self.deadlines.first()
            && at <= now
        {
            // A settled whole burst already has key order in `pending`; drain
            // its trees in bulk rather than remove every key from both indexes.
            if due.is_empty()
                && self.deadlines.len() == self.pending.len()
                && self.deadlines.last().is_some_and(|(at, _)| *at <= now)
            {
                self.deadlines.clear();
                return std::mem::take(&mut self.pending).into_keys().collect();
            }
            self.deadlines.pop_first();
            self.pending.remove(&key);
            due.push(key);
        }
        if self.pending.len() == 1 {
            self.deadlines.clear();
        }
        // Preserve the key order of the original key-indexed collector.
        due.sort_unstable();
        due
    }

    /// When the next file falls due, if any is pending.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        if self.pending.len() == 1 {
            self.pending
                .first_key_value()
                .and_then(|(_, (first, latest))| Self::at(*first, *latest))
        } else {
            self.deadlines.first().map(|(at, _)| *at)
        }
    }

    fn at(first: Instant, latest: Instant) -> Option<Instant> {
        match (latest.checked_add(QUIET), first.checked_add(SETTLE_CAP)) {
            (Some(quiet), Some(cap)) => Some(quiet.min(cap)),
            (Some(quiet), None) => Some(quiet),
            (None, Some(cap)) => Some(cap),
            (None, None) => None,
        }
    }
}

/// The platform side of the watch: arms events on files and reports which
/// keys heard one. The watch thread owns it.
pub trait FileEvents: Send {
    /// Hear events on the file `path` names now and on its directory (where
    /// that is missing, on the nearest directory above it that is there, so
    /// the missing one's making is heard), under `key`, in place of whatever
    /// `key` was armed on before.
    ///
    /// # Errors
    /// An arm that did not take, which the watch arms again: the platform's,
    /// when neither the file nor any directory of its can be armed, when
    /// descriptors (or inotify watches) ran out for the file or a folder above
    /// its own, or when the kernel refused a folder's listener; and
    /// [`io::ErrorKind::WouldBlock`] when, after every look, its folders were
    /// still being made or a folder above it kept moving.
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()>;

    /// Hear entries created, removed or replaced in the existing directory `path` (the one it
    /// names, where it is a link to one), and the directory or its lookup ancestry moved or
    /// removed, under `key`. This replaces that key's
    /// previous arm. A write into an existing entry need not be heard: callers publishing new
    /// contents replace the entry by rename. The caller reads its snapshot after arming and
    /// arms again after invalidation, since events can refer to a retired directory.
    ///
    /// # Errors
    /// [`io::ErrorKind::Unsupported`] where no directory event backend exists, and the native
    /// backend's errors where the directory is missing, refused or keeps moving while arming.
    fn arm_directory(&mut self, _key: u64, _path: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Stop hearing `key`.
    fn disarm(&mut self, key: u64);

    /// Park for at most `timeout` (`None`: until an event or a wake), then
    /// push the keys that heard an event into `touched`.
    ///
    /// # Errors
    /// The platform's, when the wait itself failed.
    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()>;

    /// What wakes a parked [`Self::wait`].
    fn waker(&self) -> Waker;

    /// Whether the volumes `key` was last armed on (its folder's, and its file's) report every
    /// writer's change to this machine's events. A network volume reports only this machine's
    /// own writes, and a user-space one only those made through its mount here; the watch looks
    /// at such a key in every round, as at one no arm takes.
    fn hears_every_writer(&self, _key: u64) -> bool {
        true
    }

    /// Whether a listener on a folder keeps every folder above it from being renamed or moved
    /// for as long as it listens (Windows, where a listener is an open handle). The watch puts
    /// no listener of such an arm on a file its host asked only to be looked at
    /// ([`FileWatch::watch_looked`]).
    fn pins_folders(&self) -> bool {
        false
    }

    /// Whether this arm may listen for `path` at all. Where it may not (on Windows, a path
    /// outside the account's application-data and temporary folders, where a person renames
    /// folders), the watch never arms the key and looks at it on a clock instead.
    fn listens_for(&self, _path: &Path) -> bool {
        true
    }
}

/// Whether a volume of the type `fstype` (`statfs`'s `f_fstypename`) with `f_flags` reports
/// every writer's change to this Mac's kqueue: a local one does, while a network volume
/// (`MNT_LOCAL` clear, or a network type), a user-space one, or a virtual machine's share
/// (virtiofs, VMware's), whose writes from elsewhere reach no kernel here, does not.
#[cfg(any(target_os = "macos", test))]
fn volume_reports_every_writer(f_flags: u32, fstype: &str) -> bool {
    const MNT_LOCAL: u32 = 0x0000_1000;
    const ELSEWHERE: &[&str] = &[
        "nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp", "macfuse", "osxfuse", "fusefs", "fuse",
        "sshfs", "virtiofs", "vmhgfs",
    ];
    f_flags & MNT_LOCAL != 0 && !ELSEWHERE.contains(&fstype.to_ascii_lowercase().as_str())
}

/// Whether this kernel sets the times of the files on the volume the open `file` is on, as it
/// hears every change of them (`volume_reports_every_writer` on macOS,
/// `volume_type_reports_every_writer` on Linux): a network, user-space or virtual machine's volume's
/// times come from a clock elsewhere. `false` where the volume cannot be read, and on a system
/// neither names.
#[cfg(unix)]
pub(crate) fn volume_times_are_this_kernels(file: &std::fs::File) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::shared_write::filesystem_of(file).is_some_and(volume_type_reports_every_writer)
    }
    #[cfg(target_os = "macos")]
    {
        kqueue::volume_hears_every_writer(file)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        false
    }
}

/// Whether a Linux volume of `statfs`'s `f_type` `magic` reports every writer's change to
/// inotify, which hears only what this kernel's own calls do. One the shared-write rule counts as
/// a network or FUSE one does not ([`crate::shared_write::REMOTE_FILESYSTEMS`]: NFS, SMB 2 and
/// 3, Ceph, both AFS clients, 9p, Lustre, GPFS and FUSE, whose type virtiofs reports too), nor
/// does SMB 1, a virtual machine's share (VirtualBox, Parallels, VMware), a cluster's shared
/// disk (OCFS2, GFS2), Coda or OrangeFS. An overlay does: a change made through it is this
/// kernel's, and its layers changed beneath it are outside what an overlay defines.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn volume_type_reports_every_writer(magic: u32) -> bool {
    const ELSEWHERE: &[u32] = &[
        0x517B,      // SMB 1
        0x786F_4256, // VirtualBox's vboxsf
        0x7C7C_6673, // Parallels' prl_fs
        0xBACB_ACBC, // VMware's vmhgfs
        0x7461_636F, // OCFS2
        0x0116_1970, // GFS2
        0x7375_7245, // Coda
        0x2003_0528, // OrangeFS
    ];
    !crate::shared_write::remote_filesystem(magic) && !ELSEWHERE.contains(&magic)
}

enum Command {
    /// Watch the key's path as this shape; `true` asks that it only be looked at where a
    /// listener would pin its folders ([`FileWatch::watch_looked`]).
    Watch(u64, PathBuf, Shape, bool),
    Unwatch(u64),
}

/// What a key watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// A file, and its folder ([`FileEvents::arm`]).
    File,
    /// A folder's entries ([`FileEvents::arm_directory`]).
    Directory,
}

/// How a key's arm took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Took {
    /// Not at all: the key is armed again in [`Unarmed`]'s rounds.
    No,
    /// On the path as a file and its folder, which hears a folder still missing made.
    File,
    /// On the folder's entries.
    Entries,
}

impl Took {
    /// Arm `key` as `shape` on `path`. Where no arm hears a folder's entries, a folder arms as
    /// a file; so does one not there (or a file where it was), whose own folder hears it made.
    /// A link that names no folder is left to the rounds instead: the folder it would name is
    /// made where no listener of the link's own folder hears it, and a round's look at the
    /// path follows the link. So is a file named through a link its reader cannot read
    /// ([`link_unread`]), whose target no listener can be put on, and any other refusal.
    fn of(events: &mut dyn FileEvents, key: u64, path: &Path, shape: Shape) -> Self {
        let armed = match shape {
            Shape::File if link_unread(path) => return Self::No,
            Shape::File => events.arm(key, path).map(|()| Self::File),
            Shape::Directory => match events.arm_directory(key, path) {
                Ok(()) => Ok(Self::Entries),
                Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                    events.arm(key, path).map(|()| Self::File)
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) && !std::fs::symlink_metadata(path)
                        .is_ok_and(|there| there.file_type().is_symlink()) =>
                {
                    events.arm(key, path).map(|()| Self::File)
                }
                Err(error) => Err(error),
            },
        };
        armed.unwrap_or(Self::No)
    }
}

/// Whether `path` is a link its reader cannot read though a lookup goes through it (on macOS,
/// one made under a strict umask: `readlink` needs read access to the link, a lookup does not).
/// What it names cannot be found to listen on, so a key on it is looked at in the rounds,
/// whose look follows the link. A host that holds each hop ([`link_chain`]) cannot name that
/// target either.
fn link_unread(path: &Path) -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        std::fs::symlink_metadata(path).is_ok_and(|there| there.file_type().is_symlink())
            && link_target(path).is_err()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        false
    }
}

/// `path` made absolute against the working folder now, as a look at it would read it, so its
/// folders above can be walked: a relative one ends in an empty path no listen opens. One the
/// platform cannot make absolute (an empty one) stays as it is.
fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// One watched key.
struct Watched {
    path: PathBuf,
    shape: Shape,
    /// What the watch last told.
    seen: Seen,
    /// Armed on the folder's entries, whose every settled burst tells: a folder's own times
    /// need not tell two entries made inside one of their ticks apart.
    entries: bool,
    /// Never armed: looked at on a clock ([`Looks`]).
    looked: bool,
}

/// The watch thread and the door to it.
pub struct FileWatch {
    commands: mpsc::Sender<Command>,
    wake: Waker,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FileWatch {
    /// Start the watch over this platform's file events on a thread named
    /// `name`, telling `notify` each watched key whose file changed.
    ///
    /// # Errors
    /// When the platform's event source or the thread cannot start.
    pub fn platform(name: &str, notify: Box<dyn FnMut(u64) + Send>) -> io::Result<Self> {
        Self::spawn(name, platform_events()?, notify)
    }

    /// Start the watch over `events` on a thread named `name`.
    ///
    /// # Errors
    /// When the thread cannot start.
    pub fn spawn(
        name: &str,
        events: Box<dyn FileEvents>,
        notify: Box<dyn FnMut(u64) + Send>,
    ) -> io::Result<Self> {
        let (commands, receive) = mpsc::channel();
        let wake = events.waker();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || watch_loop(events, &receive, &stopping, notify))?;
        Ok(Self {
            commands,
            wake,
            stop,
            thread: Some(thread),
        })
    }

    /// Watch the file at `path` as `key` (again, after a save-as). A relative path is the one it
    /// names now ([`std::path::absolute`]).
    pub fn watch(&self, key: u64, path: PathBuf) {
        if self
            .commands
            .send(Command::Watch(key, absolute(path), Shape::File, false))
            .is_ok()
        {
            (self.wake)();
        }
    }

    /// [`Self::watch`] for a file a person chose (a document open in a tab), whose folders must
    /// stay theirs to rename, move and delete. On Windows nothing is held open on them: the
    /// file is looked at every `LOOK_EVERY` while it changes and less and less often, down to
    /// every `LOOK_SLOWEST`, while it rests. A look reads the file's size as it is now, so a
    /// writer that keeps the file open (a log being appended) is followed too. On macOS and
    /// Linux, whose listeners pin nothing, this is [`Self::watch`].
    pub fn watch_looked(&self, key: u64, path: PathBuf) {
        if self
            .commands
            .send(Command::Watch(key, absolute(path), Shape::File, true))
            .is_ok()
        {
            (self.wake)();
        }
    }

    /// Watch the folder at `path` as `key`: told at each settled burst of its entries made,
    /// removed or replaced (a write into one need not be heard, as
    /// [`FileEvents::arm_directory`] says) or of the folder moved or gone, and at its first
    /// look. A folder that is a link to one is watched through the link. While the folder is
    /// missing it is watched as a file is, told once it is made; a link that names no folder is
    /// looked at every `REARM_EVERY` until it names one. A relative path is the one it names
    /// now, as [`Self::watch`]'s is.
    pub fn watch_directory(&self, key: u64, path: PathBuf) {
        if self
            .commands
            .send(Command::Watch(key, absolute(path), Shape::Directory, false))
            .is_ok()
        {
            (self.wake)();
        }
    }

    /// Stop watching `key`.
    pub fn unwatch(&self, key: u64) {
        if self.commands.send(Command::Unwatch(key)).is_ok() {
            (self.wake)();
        }
    }
}

impl Drop for FileWatch {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        (self.wake)();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Command producers must yield to event service and due retry batches. A full
// turn polls immediately so queued commands cannot be stranded when the
// coalesced wake drains.
const COMMANDS_PER_TURN: usize = 128;

fn watch_loop(
    mut events: Box<dyn FileEvents>,
    commands: &mpsc::Receiver<Command>,
    stop: &std::sync::atomic::AtomicBool,
    mut notify: Box<dyn FnMut(u64) + Send>,
) {
    // Each watched key, and the look the watch last told, `Seen::Never`
    // until its first look.
    let mut watched: BTreeMap<u64, Watched> = BTreeMap::new();
    let mut unarmed = Unarmed::default();
    let mut debounce = Debounce::default();
    let mut touched = Vec::new();
    loop {
        let mut remaining = COMMANDS_PER_TURN;
        while remaining != 0 {
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            match commands.try_recv() {
                Ok(Command::Watch(key, path, shape, asked_looked)) => {
                    // Armed, then looked at once, and that first look tells:
                    // the host read the file before this, so a change since
                    // its read may already be in any stamp taken here.
                    let now = Instant::now();
                    let looked =
                        (asked_looked && events.pins_folders()) || !events.listens_for(&path);
                    let took = if looked {
                        // Whatever the key was armed on before is let go.
                        events.disarm(key);
                        unarmed.look(key, now);
                        Took::No
                    } else {
                        unarmed.looks.keys.remove(&key);
                        unarmed.arm(events.as_mut(), key, &path, shape, now)
                    };
                    watched.insert(
                        key,
                        Watched {
                            path,
                            shape,
                            seen: Seen::Never,
                            entries: took == Took::Entries,
                            looked,
                        },
                    );
                    debounce.note(key, now);
                }
                Ok(Command::Unwatch(key)) => {
                    events.disarm(key);
                    watched.remove(&key);
                    unarmed.forget(key);
                    debounce.forget(key);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
            remaining -= 1;
        }
        if stop.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let before = Instant::now();
        let due = debounce
            .deadline()
            .map(|deadline| deadline.saturating_duration_since(before));
        let timeout = if remaining == 0 {
            Some(Duration::ZERO)
        } else {
            match (due, unarmed.rest(before)) {
                (Some(due), Some(rearm)) => Some(due.min(rearm)),
                (due, rearm) => due.or(rearm),
            }
        };
        touched.clear();
        if events.wait(timeout, &mut touched).is_err() {
            // A failed wait is not a reason to spin: look again shortly.
            std::thread::park_timeout(QUIET);
        }
        if stop.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        if let Some(keys) = unarmed.looks.due(now) {
            // A look that moved is due at once, and told by the look its burst ends in.
            let mut moved = false;
            for key in keys {
                if let Some(key_watched) = watched.get(&key)
                    && Seen::Looked(Look::of(&key_watched.path)) != key_watched.seen
                {
                    debounce.note(key, now);
                    moved = true;
                }
            }
            unarmed.looks.rest(moved);
        }
        for key in unarmed.due(now) {
            let Some(key_watched) = watched.get_mut(&key) else {
                continue;
            };
            if key_watched.looked || unarmed.armed(key) {
                // Armed, but its last look could not see it, or its volume does not report every
                // writer, so no event need tell of it: the round looks, and a look that moved is
                // due at once, armed again.
                if Seen::Looked(Look::of(&key_watched.path)) != key_watched.seen {
                    debounce.note(key, now);
                }
                continue;
            }
            let took = unarmed.arm(
                events.as_mut(),
                key,
                &key_watched.path,
                key_watched.shape,
                now,
            );
            key_watched.entries = took == Took::Entries;
            if took == Took::No {
                // Still unarmed, so no event can tell of the file: the round
                // looks at it itself.
                let look = Seen::Looked(Look::of(&key_watched.path));
                if look != key_watched.seen {
                    key_watched.seen = look;
                    notify(key);
                }
            } else {
                // An arm that takes now is looked at, as a first look is:
                // the file may have come back with its folder before the arm.
                debounce.note(key, now);
            }
        }
        for key in &touched {
            if watched.contains_key(key) {
                debounce.note(*key, now);
            }
        }
        for key in debounce.due(now) {
            let Some(key_watched) = watched.get_mut(&key) else {
                continue;
            };
            // A burst on a folder's entries tells whatever the folder's stamp says. The tell
            // comes after the arm again, so the host's read then holds whatever the new
            // listener came too late to hear.
            let burst = key_watched.entries;
            if !key_watched.looked {
                let took = unarmed.arm(
                    events.as_mut(),
                    key,
                    &key_watched.path,
                    key_watched.shape,
                    now,
                );
                key_watched.entries = took == Took::Entries;
            }
            let look = Look::of(&key_watched.path);
            unarmed.looked(key, look.found == Found::Unreadable, now);
            let look = Seen::Looked(look);
            if burst || look != key_watched.seen {
                key_watched.seen = look;
                notify(key);
            }
        }
    }
}

/// The watched files no arm took ([`FileEvents::arm`]), which the watch arms
/// again in batches every [`REARM_EVERY`] while one is left, looking at each
/// one a batch still could not arm. New failures do not postpone a pending
/// batch; with none left the watch waits on events alone.
///
/// An armed key whose last look could not see its path ([`Found::Unreadable`]) is in the rounds
/// too, looked at in each: no event need tell the end of an I/O error or a refusal no
/// permission change on the path made (a privacy grant). So is one armed on a volume that does
/// not report every writer's change ([`FileEvents::hears_every_writer`]).
#[derive(Debug)]
struct Unarmed {
    keys: BTreeSet<u64>,
    unreadable: BTreeSet<u64>,
    elsewhere: BTreeSet<u64>,
    round_at: Instant,
    looks: Looks,
}

/// The watched files that are never armed, because a listener would pin their folders
/// ([`FileEvents::pins_folders`], [`FileEvents::listens_for`]): each is looked at in rounds of
/// its own, [`LOOK_EVERY`] apart while a round finds one changed and twice as far apart after
/// each round that finds none, up to [`LOOK_SLOWEST`]. With none the watch waits on events
/// alone.
#[derive(Debug)]
struct Looks {
    keys: BTreeSet<u64>,
    every: Duration,
    round_at: Instant,
}

impl Looks {
    /// `key` is looked at from `now` on; one more to look at brings the rounds close again.
    fn insert(&mut self, key: u64, now: Instant) {
        if self.keys.is_empty() {
            self.round_at = now;
        }
        if self.keys.insert(key) {
            self.every = LOOK_EVERY;
        }
    }

    /// How long from `now` until the next round, if a key is in them.
    fn wait(&self, now: Instant) -> Option<Duration> {
        (!self.keys.is_empty()).then(|| {
            self.every
                .saturating_sub(now.saturating_duration_since(self.round_at))
        })
    }

    /// The keys a round looks at, where one is due at `now`.
    fn due(&mut self, now: Instant) -> Option<Vec<u64>> {
        if self.wait(now) != Some(Duration::ZERO) {
            return None;
        }
        self.round_at = now;
        Some(self.keys.iter().copied().collect())
    }

    /// The round just looked found a file `moved`, or none: how long until the next.
    fn rest(&mut self, moved: bool) {
        self.every = if moved {
            LOOK_EVERY
        } else {
            self.every.saturating_mul(2).min(LOOK_SLOWEST)
        };
    }
}

impl Default for Unarmed {
    fn default() -> Self {
        Self {
            keys: BTreeSet::new(),
            unreadable: BTreeSet::new(),
            elsewhere: BTreeSet::new(),
            round_at: Instant::now(),
            looks: Looks {
                keys: BTreeSet::new(),
                every: LOOK_EVERY,
                round_at: Instant::now(),
            },
        }
    }
}

impl Unarmed {
    /// `key` is never armed and is looked at in [`Looks`]' rounds from `now` on.
    fn look(&mut self, key: u64, now: Instant) {
        self.keys.remove(&key);
        self.elsewhere.remove(&key);
        self.looks.insert(key, now);
    }

    /// Arm `key` as `shape` on `path` at `now`: how it took ([`Took::of`]). One that did not
    /// is armed again later ([`Self::due`]).
    fn arm(
        &mut self,
        events: &mut dyn FileEvents,
        key: u64,
        path: &Path,
        shape: Shape,
        now: Instant,
    ) -> Took {
        let took = Took::of(events, key, path, shape);
        if took == Took::No {
            self.begin(now);
            self.keys.insert(key);
            self.elsewhere.remove(&key);
        } else {
            self.keys.remove(&key);
            if events.hears_every_writer(key) {
                self.elsewhere.remove(&key);
            } else {
                self.begin(now);
                self.elsewhere.insert(key);
            }
        }
        took
    }

    /// `key`'s look at `now` could not see its path, or could.
    fn looked(&mut self, key: u64, unreadable: bool, now: Instant) {
        if unreadable {
            self.begin(now);
            self.unreadable.insert(key);
        } else {
            self.unreadable.remove(&key);
        }
    }

    /// A batch begins at `now` where none is pending.
    fn begin(&mut self, now: Instant) {
        if self.is_empty() {
            self.round_at = now;
        }
    }

    fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.unreadable.is_empty() && self.elsewhere.is_empty()
    }

    /// Whether `key`'s last arm took.
    fn armed(&self, key: u64) -> bool {
        !self.keys.contains(&key)
    }

    /// `key` is no longer watched.
    fn forget(&mut self, key: u64) {
        self.keys.remove(&key);
        self.unreadable.remove(&key);
        self.elsewhere.remove(&key);
        self.looks.keys.remove(&key);
    }

    /// How long from `now` until the next round, if a key is in them.
    fn wait(&self, now: Instant) -> Option<Duration> {
        (!self.is_empty())
            .then(|| REARM_EVERY.saturating_sub(now.saturating_duration_since(self.round_at)))
    }

    /// How long from `now` until the watch owes a round of either kind.
    fn rest(&self, now: Instant) -> Option<Duration> {
        match (self.wait(now), self.looks.wait(now)) {
            (Some(round), Some(looks)) => Some(round.min(looks)),
            (round, looks) => round.or(looks),
        }
    }

    /// The keys a round looks at, or arms again, at `now`: every one once [`REARM_EVERY`]
    /// passed since this batch began, else none.
    fn due(&mut self, now: Instant) -> Vec<u64> {
        if self.wait(now) != Some(Duration::ZERO) {
            return Vec::new();
        }
        self.round_at = now;
        let mut due: BTreeSet<u64> = self.keys.union(&self.unreadable).copied().collect();
        due.extend(&self.elsewhere);
        due.into_iter().collect()
    }
}

/// What the watch last told of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// Nothing yet: its first look tells.
    Never,
    /// This look.
    Looked(Look),
}

/// This platform's file events: kqueue on macOS, inotify on Linux, directory change
/// notifications on Windows, a stat poll elsewhere. On Windows a completion port the kernel
/// refuses leaves the stat poll, said once on stderr: a caller there was never refused a watch.
/// The Windows arm listens only inside the account's application-data and temporary folders
/// ([`FileEvents::listens_for`]); a caller that drives the arm itself gets
/// [`io::ErrorKind::Unsupported`] from an arm on a path anywhere else.
///
/// # Errors
/// When the kernel refuses the event source (macOS and Linux).
pub fn platform_events() -> io::Result<Box<dyn FileEvents>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(kqueue::KqueueEvents::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(inotify::InotifyEvents::new()?))
    }
    #[cfg(windows)]
    {
        match win::WinEvents::new() {
            Ok(events) => Ok(Box::new(events)),
            Err(why) => {
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| {
                    eprintln!(
                        "aterm: no file events ({why}): watched files are looked at every {POLL_EVERY:?}"
                    );
                });
                Ok(Box::new(PollEvents::default()))
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Ok(Box::new(PollEvents::default()))
    }
}

#[cfg(windows)]
#[path = "file_watch_win.rs"]
mod win;

/// The arm a test drives the host with: it arms nothing and hears nothing,
/// and records what it was asked to watch.
#[derive(Clone, Default)]
pub struct InertEvents {
    /// Each armed key and the path it was last armed on.
    pub armed: Arc<std::sync::Mutex<BTreeMap<u64, PathBuf>>>,
    woken: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl FileEvents for InertEvents {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        self.armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, path.to_path_buf());
        Ok(())
    }

    fn disarm(&mut self, key: u64) {
        self.armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
    }

    fn wait(&mut self, timeout: Option<Duration>, _touched: &mut Vec<u64>) -> io::Result<()> {
        let (flag, bell) = &*self.woken;
        let mut woken = flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*woken {
            woken = match timeout {
                Some(timeout) => {
                    bell.wait_timeout(woken, timeout)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0
                }
                None => bell
                    .wait(woken)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            };
        }
        *woken = false;
        Ok(())
    }

    fn waker(&self) -> Waker {
        let woken = Arc::clone(&self.woken);
        Arc::new(move || {
            let (flag, bell) = &*woken;
            *flag
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            bell.notify_all();
        })
    }
}

/// The arm for a platform with no file events: every [`POLL_EVERY`] it
/// stats each watched file and reports the ones whose stamp moved.
#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
#[derive(Default)]
pub struct PollEvents {
    files: BTreeMap<u64, (PathBuf, Look)>,
    inert: InertEvents,
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
impl FileEvents for PollEvents {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        self.files.insert(key, (path.to_path_buf(), Look::of(path)));
        Ok(())
    }

    fn disarm(&mut self, key: u64) {
        self.files.remove(&key);
    }

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        let nap = timeout.map_or(POLL_EVERY, |timeout| timeout.min(POLL_EVERY));
        self.inert.wait(Some(nap), touched)?;
        for (key, (path, seen)) in &mut self.files {
            let look = Look::of(path);
            if look != *seen {
                *seen = look;
                touched.push(*key);
            }
        }
        Ok(())
    }

    fn waker(&self) -> Waker {
        self.inert.waker()
    }
}

/// How many more times one arm looks, where a missing folder of the file's was
/// made, or a folder above it moved, while it armed: each look starts again at
/// the file's own folder. A path still being made or moved when these run out
/// fails the arm, which the watch arms again later, so a folder made and taken
/// away (or renamed back and forth) again and again cannot keep the watch
/// thread arming.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
const ARM_AGAIN: usize = 8;

/// The folders a watch of `path` can listen on, nearest first, each with the
/// entry below it on the way to `path`: its own folder (with `path`), then
/// each one above (with the folder below it). Where `path`'s folder is
/// missing, the nearest one that is there hears it made, and the look that
/// event brings arms again lower.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn folders_up(path: &Path) -> impl Iterator<Item = (&Path, &Path)> {
    std::iter::successors(Some(path), |below| below.parent())
        .filter_map(|below| Some((below.parent()?, below)))
}

/// Whether a listen on the folder `directory` answered `error` because a regular file stands
/// there (a path named under a file, as a records layout's data file is while the records entry
/// is still a plain file): no folder is there, as where nothing is, and the folder that holds
/// the file hears it replaced by one. A link there is not this: it may name a folder.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn under_a_file(error: &io::Error, directory: &Path) -> bool {
    error.raw_os_error() == Some(libc::ENOTDIR)
        && std::fs::symlink_metadata(directory).is_ok_and(|there| there.file_type().is_file())
}

/// A folder or a link that looking a path up passes through, named by a path with no link on it,
/// so a listener opened by that path is on the folder or on the link itself.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug)]
struct Passed {
    path: PathBuf,
    /// A link, whose target the lookup goes on through, rather than a folder.
    link: bool,
    /// The folder holds the root of another volume that the lookup goes on into.
    #[cfg(target_os = "macos")]
    holds_volume: bool,
}

/// The most links one lookup reads: XNU's `MAXSYMLINKS` (32), Linux's (40).
#[cfg(any(target_os = "macos", target_os = "linux"))]
const LOOKUP_LINKS: usize = if cfg!(target_os = "linux") { 40 } else { 32 };

/// The paths a lookup of `path` names in turn: `path` (made absolute), then, while the last is a
/// link, what it names (a relative target from the link's folder), ending at the first that is
/// no link: a file, a folder, or a path not there. It follows as many links as the kernel does
/// and ends at a link it cannot read. A caller that must hear each of these change watches each
/// one: a watch on `path` hears its own entry replaced, and links in the folders on the way
/// ([`FileWatch::watch`]), but not the file a final link names changed in another folder.
#[must_use]
pub fn link_chain(path: &Path) -> Vec<PathBuf> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let (most, read) = (LOOKUP_LINKS, link_target);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let (most, read) = (40, |link: &Path| std::fs::read_link(link));
    let mut chain = vec![absolute(path.to_path_buf())];
    while chain.len() <= most
        && let Some(last) = chain.last()
        && std::fs::symlink_metadata(last).is_ok_and(|there| there.file_type().is_symlink())
        && let Ok(target) = read(last)
    {
        let next = last
            .parent()
            .map_or_else(|| target.clone(), |folder| folder.join(&target));
        chain.push(next);
    }
    chain
}

/// Every folder and link that looking the folder `path` up passes through, from the root down to
/// `path` itself, as the kernel walks it: a link's target is looked up in turn, from the root
/// where it is absolute and from the link's folder otherwise, and `..` goes back to the folder
/// the lookup came from (the target's, after a link). Behind a link its user cannot read (macOS),
/// the walk goes on from the folder its target names, by that folder's own path, so the links the
/// target itself goes through are not passed.
///
/// The walk ends early, with what it passed, in two places it cannot follow the kernel: where
/// the link-free name of a folder runs past the longest path the platform names (a link that
/// shortens the path can take the lookup there), and where an entry is missing while `path`
/// still looks up to the folder it did (a `/proc` link, which the kernel follows to a place its
/// text does not name).
///
/// # Errors
/// [`io::ErrorKind::NotFound`] where `path` does not look up to a folder, or an entry on the way
/// is missing or neither a folder nor a link and `path` no longer looks up to where it did: the
/// path moved as it walked. The platform's `ELOOP` past [`LOOKUP_LINKS`] links, and its error
/// where an entry cannot be read.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn passed_through(path: &Path) -> io::Result<Vec<Passed>> {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::Component;
    enum Step {
        Root,
        Up,
        Name(OsString),
    }
    fn steps(path: &Path) -> impl DoubleEndedIterator<Item = Step> + '_ {
        path.components().filter_map(|component| match component {
            Component::RootDir => Some(Step::Root),
            Component::ParentDir => Some(Step::Up),
            Component::Normal(name) => Some(Step::Name(name.to_os_string())),
            Component::CurDir | Component::Prefix(_) => None,
        })
    }
    use std::os::unix::fs::MetadataExt as _;
    let path = std::path::absolute(path)?;
    let looked_up = |path: &Path| std::fs::metadata(path).map(|there| (there.dev(), there.ino()));
    let start = looked_up(&path)?;
    let mut passed = vec![Passed {
        path: PathBuf::from("/"),
        link: false,
        #[cfg(target_os = "macos")]
        holds_volume: false,
    }];
    // The folders from the root down to where the lookup is, by their place in `passed`.
    let mut chain = vec![0];
    let mut rest: VecDeque<Step> = steps(&path).collect();
    let mut links = 0;
    while let Some(step) = rest.pop_front() {
        let name = match step {
            Step::Root => {
                chain.truncate(1);
                continue;
            }
            Step::Up => {
                if chain.len() > 1 {
                    chain.pop();
                }
                continue;
            }
            Step::Name(name) => name,
        };
        let holder = chain.last().copied().unwrap_or(0);
        let next = passed[holder].path.join(name);
        let there = match std::fs::symlink_metadata(&next) {
            Ok(there) => there,
            Err(error) if error.raw_os_error() == Some(libc::ENAMETOOLONG) => break,
            // Still missing, and the path still looks up where it did: not a move.
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&next).is_err()
                    && looked_up(&path).is_ok_and(|now| now == start) =>
            {
                break;
            }
            Err(error) => return Err(error),
        };
        if there.file_type().is_symlink() {
            links += 1;
            if links > LOOKUP_LINKS {
                return Err(io::Error::from_raw_os_error(libc::ELOOP));
            }
            let target = link_target(&next);
            passed.push(Passed {
                path: next,
                link: true,
                #[cfg(target_os = "macos")]
                holds_volume: false,
            });
            match target {
                Ok(target) => {
                    for step in steps(&target).rev() {
                        rest.push_front(step);
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::ENAMETOOLONG) => break,
                Err(error) => return Err(error),
            }
        } else if there.is_dir() {
            #[cfg(target_os = "macos")]
            {
                if std::fs::symlink_metadata(&passed[holder].path)
                    .is_ok_and(|folder| folder.dev() != there.dev())
                {
                    passed[holder].holds_volume = true;
                }
            }
            chain.push(passed.len());
            passed.push(Passed {
                path: next,
                link: false,
                #[cfg(target_os = "macos")]
                holds_volume: false,
            });
        } else {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
    }
    Ok(passed)
}

/// What the link `link` names. On macOS a link made under a strict umask can refuse its reader
/// (`readlink` needs read access to it) while a lookup still goes through it; there the folder it
/// names is opened through it for search alone (`O_SEARCH`, which the lookup's own permission
/// covers) and asked for its own path (`F_GETPATH`), which has no link on it.
///
/// # Errors
/// The platform's where the link cannot be read (or, refused, the folder it names cannot be
/// opened), and `ENAMETOOLONG` where that folder's own path is longer than the platform names.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn link_target(link: &Path) -> io::Result<PathBuf> {
    match std::fs::read_link(link) {
        #[cfg(target_os = "macos")]
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::ffi::OsStrExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let folder = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(O_EVTONLY | libc::O_NONBLOCK | O_SEARCH)
                .open(link)?;
            let mut named = vec![0u8; usize::try_from(libc::PATH_MAX).unwrap_or(1024)];
            // SAFETY: a live descriptor and a buffer of `PATH_MAX` bytes (`MAXPATHLEN`), which
            // `F_GETPATH` fills with a NUL-terminated path.
            if unsafe { libc::fcntl(folder.as_raw_fd(), libc::F_GETPATH, named.as_mut_ptr()) } < 0 {
                let error = io::Error::last_os_error();
                // XNU's answer for a path that does not fit `MAXPATHLEN`.
                if error.raw_os_error() == Some(libc::ENOSPC) {
                    return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
                }
                return Err(error);
            }
            let end = named.iter().position(|&b| b == 0).unwrap_or(named.len());
            Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&named[..end])))
        }
        read => read,
    }
}

/// `poll(2)`'s timeout for a [`FileEvents::wait`] of at most `timeout`: `-1`
/// (until an event or a wake) for none, `0` for a wait already due, which only
/// takes what is ready, and otherwise its whole milliseconds, a rest under one
/// rounded up to one so it never spins, and at most `i32::MAX`.
#[cfg(any(test, target_os = "linux"))]
fn poll_millis(timeout: Option<Duration>) -> i32 {
    match timeout {
        None => -1,
        Some(timeout) if timeout.is_zero() => 0,
        Some(timeout) => i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX),
    }
}

#[cfg(any(test, target_os = "linux"))]
const READS_PER_TURN: usize = 16;

/// Read a bounded batch of nonblocking input, forwarding each initialized prefix.
/// Inotify and its wake pipe use level-triggered poll: unread bytes keep the
/// descriptor ready for the next scheduler turn without another producer wake.
#[cfg(any(test, target_os = "linux"))]
fn read_batch<const BYTES: usize>(
    mut read: impl FnMut(&mut [u8]) -> Option<usize>,
    mut consume: impl FnMut(&[u8]),
) {
    let mut buffer = [0u8; BYTES];
    for _ in 0..READS_PER_TURN {
        let Some(count) = read(&mut buffer) else {
            return;
        };
        if count == 0 {
            return;
        }
        consume(&buffer[..count.min(buffer.len())]);
    }
}

/// `<sys/fcntl.h>`'s `O_EVTONLY`, which the generated libc does not carry: a
/// descriptor for events only, which never keeps its volume from unmounting
/// (a document open from a USB drive or a network share can still eject).
#[cfg(target_os = "macos")]
const O_EVTONLY: libc::c_int = 0x0000_8000;

/// `<sys/fcntl.h>`'s `O_SYMLINK`, which the generated libc does not carry: open
/// a link itself rather than what it names.
#[cfg(target_os = "macos")]
const O_SYMLINK: libc::c_int = 0x0020_0000;

/// `<sys/fcntl.h>`'s `O_SEARCH` (`O_EXEC | O_DIRECTORY`), which the generated libc
/// does not carry: open a folder for search alone, which needs no permission to
/// list it.
#[cfg(target_os = "macos")]
const O_SEARCH: libc::c_int = 0x4000_0000 | libc::O_DIRECTORY;

/// Opens the link `path` itself to hear its events only.
#[cfg(target_os = "macos")]
fn open_link_for_events(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_EVTONLY | libc::O_NONBLOCK | O_SYMLINK)
        .open(path)
}

/// Opens `path` to hear its events only ([`O_EVTONLY`]), never blocking (a
/// FIFO swapped in) and never following a link.
#[cfg(target_os = "macos")]
fn open_for_events(path: &Path, directory: bool) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let flags = O_EVTONLY
        | libc::O_NONBLOCK
        | libc::O_NOFOLLOW
        | if directory { libc::O_DIRECTORY } else { 0 };
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
}

#[cfg(target_os = "macos")]
mod kqueue {
    //! macOS: one kqueue, a vnode filter on each watched file and on its
    //! directory (or the nearest one above it that is there), one on each
    //! folder above that, shared by every file whose path runs through it, a
    //! file system filter that hears every mount and unmount, and a user event
    //! that wakes the wait.

    use std::collections::{BTreeMap, BTreeSet};
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use super::{FileEvents, Waker, open_for_events, open_link_for_events};

    /// The events that mean a file's bytes, name or attributes moved.
    const FILE_EVENTS: u32 = libc::NOTE_WRITE
        | libc::NOTE_EXTEND
        | libc::NOTE_ATTRIB
        | libc::NOTE_DELETE
        | libc::NOTE_RENAME
        | libc::NOTE_REVOKE
        | libc::NOTE_LINK;

    /// The events that mean an entry of a directory came, went or was
    /// renamed (a replace by rename writes the directory), or the directory's
    /// own attributes changed (a `chmod` that lets the path below be read, or
    /// stops it).
    const DIRECTORY_EVENTS: u32 = libc::NOTE_WRITE
        | libc::NOTE_ATTRIB
        | libc::NOTE_DELETE
        | libc::NOTE_RENAME
        | libc::NOTE_REVOKE;

    /// The events that mean a folder above a file's own one was renamed or went, which moves the
    /// file's path off what its own listeners hold, or that its own attributes changed, which
    /// can let the lookup through it or stop it. A write into one is not among them, and nor is
    /// an entry's change, which tells no folder's attributes (measured on macOS 27.0: an entry
    /// made, replaced or removed is `NOTE_WRITE` alone), so a busy folder up the path (a home
    /// folder) wakes nothing.
    const ABOVE_EVENTS: u32 =
        libc::NOTE_ATTRIB | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;

    /// `<sys/event.h>`'s file system filter, and `<sys/mount.h>`'s events for a volume mounted
    /// and one unmounted, which the generated libc does not carry.
    const EVFILT_FS: i16 = -9;
    const VQ_MOUNT: u32 = 0x0008;
    const VQ_UNMOUNT: u32 = 0x0010;

    /// Whether a file system event's `fflags` say a volume was mounted or unmounted: what every
    /// path below the mount point names then changes, with no event on any vnode a listener
    /// holds (the folder a volume covers hears nothing; measured on macOS 27.0 with a disk
    /// image attached over a watched empty folder). The others (a low disk, a server not
    /// answering) move no path.
    pub(super) const fn remounted(fflags: u32) -> bool {
        fflags & (VQ_MOUNT | VQ_UNMOUNT) != 0
    }

    /// The listener on one folder above some keys' own folders, which every key whose path runs
    /// through that folder shares (closing it drops its filter).
    struct Above {
        file: std::fs::File,
        /// What its filter hears.
        fflags: u32,
        keys: BTreeSet<u64>,
    }

    pub(super) struct KqueueEvents {
        kq: Arc<OwnedFd>,
        /// Each key's open file and directory (closing one drops its filter), the directory
        /// first.
        armed: BTreeMap<u64, Vec<std::fs::File>>,
        /// Which key each open descriptor hears for.
        keys: BTreeMap<RawFd, u64>,
        /// The folders above the keys' own, by identity (device and inode): one listener each,
        /// however many keys' paths run through it.
        above: BTreeMap<(u64, u64), Above>,
        /// Which folder above each of those listeners' descriptors is.
        above_at: BTreeMap<RawFd, (u64, u64)>,
    }

    impl KqueueEvents {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: kqueue creates a new descriptor owned by this process.
            let raw = unsafe { libc::kqueue() };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is fresh and exclusively ours.
            let kq = unsafe { OwnedFd::from_raw_fd(raw) };
            // SAFETY: F_SETFD changes only this descriptor's close-on-exec flag.
            unsafe { libc::fcntl(kq.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
            change(&kq, user_event(0, libc::EV_ADD | libc::EV_CLEAR))?;
            change(&kq, fs_event())?;
            Ok(Self {
                kq: Arc::new(kq),
                armed: BTreeMap::new(),
                keys: BTreeMap::new(),
                above: BTreeMap::new(),
                above_at: BTreeMap::new(),
            })
        }

        fn listen(
            &mut self,
            key: u64,
            path: &Path,
            directory: bool,
            step: &mut impl FnMut(Listen, &Path) -> io::Result<()>,
        ) -> io::Result<()> {
            let file = open_for_events(path, directory)?;
            step(Listen::Opened, path)?;
            let fd = file.as_raw_fd();
            let fflags = if directory {
                DIRECTORY_EVENTS
            } else {
                FILE_EVENTS
            };
            change(&self.kq, vnode_event(fd, fflags))?;
            // Gone or replaced before the filter took, whose event then never comes.
            if !still_at(&file, path)? {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            self.keys.insert(fd, key);
            self.armed.entry(key).or_default().push(file);
            Ok(())
        }
    }

    impl KqueueEvents {
        /// Listen on the nearest folder of `path`'s that is there ([`super::folders_up`]): where
        /// the folders above it are walked from, the folder its listener holds, and the folder
        /// just below it that was missing, or `None` where that nearest one is `path`'s own.
        /// A file standing where a folder of `path`'s would be ([`super::under_a_file`]) is a
        /// folder missing: the folder that holds the file hears it replaced by one.
        ///
        /// Where that nearest one is a link to a folder (a dotfiles `~/.config`), which a listen
        /// refuses, since it opens no link it is not asked to, the listener is on the folder the
        /// link names, and the walk above starts at the link: its own listener there hears it
        /// made to name another folder, and the folders behind it are walked too. A link made to
        /// name another one before that listener took is caught by [`Self::arm_around`]'s
        /// [`Self::first_named_by`].
        fn listen_folder(
            &mut self,
            key: u64,
            path: &Path,
            step: &mut impl FnMut(Listen, &Path) -> io::Result<()>,
        ) -> io::Result<Folder> {
            for (directory, below) in super::folders_up(path) {
                let listened = self.listen(key, directory, true, step);
                step(Listen::Answered, directory)?;
                let made = (below != path).then(|| below.to_path_buf());
                match listened {
                    Ok(()) => {
                        return Ok(Folder {
                            from: directory.to_path_buf(),
                            held: directory.to_path_buf(),
                            made,
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    // A file where a folder of the path's would be: no folder is there.
                    Err(error) if super::under_a_file(&error, directory) => {}
                    Err(error) if refused_as_link(&error, directory) => {
                        let held = self.listen_named(key, directory, error, step)?;
                        return Ok(Folder {
                            from: directory.to_path_buf(),
                            held,
                            made,
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::from(io::ErrorKind::NotFound))
        }

        /// Listen on the folder the link `link` names, whose own listen answered `refused`
        /// ([`refused_as_link`]): the folder listened on. A link to no folder answers `refused`,
        /// since that is all it is.
        fn listen_named(
            &mut self,
            key: u64,
            link: &Path,
            refused: io::Error,
            step: &mut impl FnMut(Listen, &Path) -> io::Result<()>,
        ) -> io::Result<PathBuf> {
            let named = std::fs::canonicalize(link).map_err(|_| refused)?;
            let listened = self.listen(key, &named, true, step);
            step(Listen::Answered, &named)?;
            listened.map(|()| named)
        }

        /// Listen on the folder `path`, or the link itself where `link`, for `fflags` under `key`,
        /// sharing a listener another key already holds on it, widened to `fflags` where it hears
        /// less (it is never narrowed while it lives: a key that needed more and left costs the
        /// others a look when it fires). Nothing where `path` cannot be opened for events (a
        /// folder or a link this user cannot read).
        ///
        /// # Errors
        /// [`io::ErrorKind::NotFound`] when the folder or link moved or went as it was listened
        /// on (a link or a file is where the folder was); the platform's when descriptors ran out
        /// or the kqueue refused a filter.
        pub(super) fn listen_one(
            &mut self,
            key: u64,
            path: &Path,
            link: bool,
            fflags: u32,
        ) -> io::Result<()> {
            use std::os::unix::fs::MetadataExt as _;
            let opened = if link {
                open_link_for_events(path)
            } else {
                open_for_events(path, true)
            };
            let file = match opened {
                Ok(file) => file,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        || matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) =>
                {
                    return Err(error);
                }
                // A link or a file where the lookup passed a folder: it moved as the walk ran.
                Err(error)
                    if !link
                        && matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) =>
                {
                    return Err(io::Error::from(io::ErrorKind::NotFound));
                }
                Err(_) => return Ok(()),
            };
            let held = file.metadata()?;
            let id = (held.dev(), held.ino());
            if let Some(shared) = self.above.get_mut(&id) {
                if shared.fflags & fflags != fflags {
                    // Adding a filter that is there changes what it hears.
                    change(
                        &self.kq,
                        vnode_event(shared.file.as_raw_fd(), shared.fflags | fflags),
                    )?;
                    shared.fflags |= fflags;
                }
                shared.keys.insert(key);
                return Ok(());
            }
            let fd = file.as_raw_fd();
            change(&self.kq, vnode_event(fd, fflags))?;
            // Moved before the filter took, whose event then never comes.
            if !still_at(&file, path)? {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            self.above_at.insert(fd, id);
            self.above.insert(
                id,
                Above {
                    file,
                    fflags,
                    keys: BTreeSet::from([key]),
                },
            );
            Ok(())
        }

        /// Listen for `key` on every folder and link that looking `from` up passes through
        /// ([`super::passed_through`]) and its own listeners do not hold, each for its own rename,
        /// end or change of mode ([`Self::listen_one`]): any of them renamed, gone or replaced
        /// moves the path off what those listeners hold, and a mode can stop the lookup through
        /// it or let it through. A link is listened on itself, which hears it replaced (made
        /// to name another folder), and so is each folder the lookup goes on through behind it.
        /// One that cannot be opened for events is passed over, and the rest are still listened
        /// on. The root, which nothing renames, is left out unless it holds a volume's root.
        ///
        /// A volume's root, on another device than the folder that holds it, hears nothing when
        /// the volume is renamed in place (Finder or `diskutil rename`): XNU renames the folder it
        /// is mounted on instead, which writes only the folder that holds that one, so a folder
        /// holding a volume's root listens for writes as well.
        ///
        /// Then `held` must still name what `key`'s first listener is on (its own folder's, or its
        /// file's where the folder refused its listen): a folder above renamed before its listen
        /// took moved it away unheard.
        ///
        /// # Errors
        /// [`io::ErrorKind::NotFound`] when a folder or link on the way moved or went as the walk
        /// listened on it, so the arm looks again; the platform's when descriptors ran out, the
        /// kqueue refused a filter, or the lookup failed ([`super::passed_through`]).
        fn listen_above(&mut self, key: u64, from: &Path, held: &Path) -> io::Result<()> {
            use std::os::unix::fs::MetadataExt as _;
            let own: Vec<(u64, u64)> = self
                .armed
                .get(&key)
                .into_iter()
                .flatten()
                .filter_map(|file| file.metadata().ok())
                .map(|there| (there.dev(), there.ino()))
                .collect();
            for passed in super::passed_through(from)? {
                if passed.path.parent().is_none() && !passed.holds_volume {
                    continue;
                }
                if std::fs::symlink_metadata(&passed.path)
                    .is_ok_and(|there| own.contains(&(there.dev(), there.ino())))
                {
                    continue;
                }
                let fflags = if passed.holds_volume {
                    ABOVE_EVENTS | libc::NOTE_WRITE
                } else {
                    ABOVE_EVENTS
                };
                self.listen_one(key, &passed.path, passed.link, fflags)?;
            }
            // The first listener a key holds is its own folder's, or its file's where the folder
            // refused ([`Self::arm_around`]).
            let first = self.armed.get(&key).and_then(|files| files.first());
            match first {
                Some(first) if still_at(first, held)? => Ok(()),
                _ => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }

        /// Listen on `path`'s folder, then on `path`, then on each folder above that one,
        /// telling `step` how the first two go.
        ///
        /// The folder goes first because it hears a file made but no write into one. Where
        /// `path`'s own folder is there, a file made after its listen is heard there and one made
        /// before it opens below. In the other order a file made between the two has no listener
        /// of its own, and every later write into it goes unheard. The folders above hear the
        /// path renamed out from under both ([`Self::listen_above`]), and still do where the
        /// file's own folder refused its listen (one this user cannot read, or a link that names
        /// no folder) and the file's listener took: that is all the key holds there. A folder
        /// that is a link to one is listened on through the link ([`Self::listen_folder`]).
        pub(super) fn arm_around(
            &mut self,
            key: u64,
            path: &Path,
            mut step: impl FnMut(Listen, &Path) -> io::Result<()>,
        ) -> io::Result<()> {
            for _ in 0..=super::ARM_AGAIN {
                self.disarm(key);
                let folder = self.listen_folder(key, path, &mut step);
                let file = self.listen(key, path, false, &mut step);
                step(Listen::Answered, path)?;
                match folder {
                    // Made between its failed open and the listen above it, which hears
                    // nothing inside it: arm again, lower. A file there is no folder made.
                    Ok(Folder {
                        made: Some(made), ..
                    }) if made.is_dir() => {}
                    Ok(Folder { from, held, .. }) => match self.listen_above(key, &from, &held) {
                        // A folder on the way moved as the walk listened: arm again.
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        // The link made to name another folder before its own listener took.
                        Ok(()) if from != held && !self.first_named_by(key, &from)? => {}
                        above => return above.and(file_answer(file)),
                    },
                    Err(error) => match (file, path.parent()) {
                        (Ok(()), Some(own)) => match self.listen_above(key, own, path) {
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            above => return above,
                        },
                        (file, _) => return file.or(Err(error)),
                    },
                }
            }
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        /// A directory arm, running `between` after its own listener takes and before its lookup
        /// ancestry is listened on. The ancestry walk rechecks that the listener still holds
        /// this path's directory; a replacement in between requires another arm. Where `path` is
        /// a link to a folder, the listener is on the folder it names and the walk starts at the
        /// link, as [`Self::listen_folder`]'s does, and then the link must still name that
        /// folder ([`Self::first_named_by`]).
        pub(super) fn arm_directory_then(
            &mut self,
            key: u64,
            path: &Path,
            mut between: impl FnMut(&Path),
        ) -> io::Result<()> {
            let step = &mut |_, _: &Path| Ok(());
            for _ in 0..=super::ARM_AGAIN {
                self.disarm(key);
                let held = match self.listen(key, path, true, step) {
                    Ok(()) => path.to_path_buf(),
                    Err(error) if refused_as_link(&error, path) => {
                        self.listen_named(key, path, error, step)?
                    }
                    Err(error) => return Err(error),
                };
                between(path);
                match self.listen_above(key, path, &held) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    // The link made to name another folder before its own listener took.
                    Ok(()) if held.as_path() != path && !self.first_named_by(key, path)? => {}
                    answer => return answer,
                }
            }
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        /// Whether `link` still names the folder `key`'s first listener is on. A walk from a link
        /// ([`Self::listen_above`]) listens on the link it finds, so a link made to name another
        /// folder before then is heard by nothing; after then, by that listener.
        ///
        /// # Errors
        /// The platform's, when the listener or the lookup could not be read.
        fn first_named_by(&self, key: u64, link: &Path) -> io::Result<bool> {
            use std::os::unix::fs::MetadataExt as _;
            let Some(first) = self.armed.get(&key).and_then(|files| files.first()) else {
                return Ok(false);
            };
            let held = first.metadata()?;
            match std::fs::metadata(link) {
                Ok(there) => Ok((held.dev(), held.ino()) == (there.dev(), there.ino())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        }

        /// How many folders above the keys' own have a listener, and which keys share each.
        #[cfg(test)]
        pub(super) fn above(&self) -> Vec<((u64, u64), Vec<u64>)> {
            self.above
                .iter()
                .map(|(id, above)| (*id, above.keys.iter().copied().collect()))
                .collect()
        }
    }

    /// The folder [`KqueueEvents::listen_folder`] listened on for a key.
    struct Folder {
        /// Where the folders above are walked from: the nearest folder there, or the link that
        /// stands for it.
        from: PathBuf,
        /// What the listener is on: `from`, or the folder that link names.
        held: PathBuf,
        /// The folder just below `from` that was missing, where `from` is not the path's own.
        made: Option<PathBuf>,
    }

    /// Where a listen has got to, for [`KqueueEvents::arm_around`].
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Listen {
        /// The path opened, and its filter is not on yet.
        Opened,
        /// The listen has answered, either way.
        Answered,
    }

    /// Whether a listen on the folder `path` answered `error` because `path` is a link, which a
    /// listen does not open unless it is asked to.
    fn refused_as_link(error: &io::Error, path: &Path) -> bool {
        matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP))
            && std::fs::symlink_metadata(path).is_ok_and(|there| there.file_type().is_symlink())
    }

    /// Whether the volume `file` is on reports every writer's change to this kqueue
    /// ([`super::volume_reports_every_writer`]); `false` where `fstatfs` refuses.
    pub(super) fn volume_hears_every_writer(file: &std::fs::File) -> bool {
        // SAFETY: `statfs` is integers and arrays: all-zero is a value.
        let mut volume: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor and a writable `statfs`.
        if unsafe { libc::fstatfs(file.as_raw_fd(), &raw mut volume) } != 0 {
            return false;
        }
        let fstype: Vec<u8> = volume
            .f_fstypename
            .iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte.to_ne_bytes()[0])
            .collect();
        super::volume_reports_every_writer(volume.f_flags, &String::from_utf8_lossy(&fstype))
    }

    /// Whether `file` is still what `path` names.
    fn still_at(file: &std::fs::File, path: &Path) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt as _;
        let held = file.metadata()?;
        match std::fs::symlink_metadata(path) {
            Ok(there) => Ok((held.dev(), held.ino()) == (there.dev(), there.ino())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// What an arm answers once its folder listens, given how the file's own listen went. A file
    /// out of descriptors is armed again, since its folder hears no write into it. Any other
    /// refusal would answer the same at every look, so the folder is what that file gets.
    fn file_answer(file: io::Result<()>) -> io::Result<()> {
        match file {
            Err(error) if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) => {
                Err(error)
            }
            _ => Ok(()),
        }
    }

    impl FileEvents for KqueueEvents {
        fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
            self.arm_around(key, path, |_, _| Ok(()))
        }

        /// Read from the volume of each of the key's own listeners (its folder and its file); a
        /// key one of whose volumes cannot be read is looked at in rounds.
        fn hears_every_writer(&self, key: u64) -> bool {
            self.armed
                .get(&key)
                .is_none_or(|files| files.iter().all(volume_hears_every_writer))
        }

        fn arm_directory(&mut self, key: u64, path: &Path) -> io::Result<()> {
            self.arm_directory_then(key, path, |_| {})
        }

        fn disarm(&mut self, key: u64) {
            for file in self.armed.remove(&key).unwrap_or_default() {
                self.keys.remove(&file.as_raw_fd());
            }
            let above_at = &mut self.above_at;
            self.above.retain(|_, above| {
                above.keys.remove(&key);
                let shared = !above.keys.is_empty();
                if !shared {
                    above_at.remove(&above.file.as_raw_fd());
                }
                shared
            });
        }

        fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
            let timespec = timeout.map(|timeout| libc::timespec {
                tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
                tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
            });
            // SAFETY: `kevent` initializes the slots it reports before they are read.
            let mut events: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
            // SAFETY: sixteen live event slots, and a timeout that lives for the call or none.
            let count = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    16,
                    timespec
                        .as_ref()
                        .map_or(std::ptr::null(), |timespec| timespec as *const _),
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::Interrupted {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            for event in events.iter().take(usize::try_from(count).unwrap_or(0)) {
                if event.filter == EVFILT_FS {
                    // A mount moves what any path names: every key looks again, armed again.
                    if remounted(event.fflags) {
                        touched.extend(self.armed.keys().copied());
                    }
                    continue;
                }
                if event.filter != libc::EVFILT_VNODE {
                    continue;
                }
                let Ok(fd) = RawFd::try_from(event.ident) else {
                    continue;
                };
                if let Some(key) = self.keys.get(&fd) {
                    touched.push(*key);
                } else if let Some(above) = self.above_at.get(&fd).and_then(|id| self.above.get(id))
                {
                    touched.extend(above.keys.iter().copied());
                }
            }
            Ok(())
        }

        fn waker(&self) -> Waker {
            let kq = Arc::clone(&self.kq);
            Arc::new(move || {
                let _ = change(&kq, user_event(libc::NOTE_TRIGGER, 0));
            })
        }
    }

    /// The change entry that adds a vnode filter for `fflags` on `fd`.
    fn vnode_event(fd: RawFd, fflags: u32) -> libc::kevent {
        libc::kevent {
            ident: libc::uintptr_t::try_from(fd).unwrap_or(0),
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags,
            data: 0,
            udata: std::ptr::null_mut(),
        }
    }

    /// The change entry that hears every volume's file system events ([`EVFILT_FS`], ident 0).
    const fn fs_event() -> libc::kevent {
        libc::kevent {
            ident: 0,
            filter: EVFILT_FS,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }
    }

    /// The wake's `EVFILT_USER` change entry (ident 0).
    const fn user_event(fflags: u32, flags: u16) -> libc::kevent {
        libc::kevent {
            ident: 0,
            filter: libc::EVFILT_USER,
            flags,
            fflags,
            data: 0,
            udata: std::ptr::null_mut(),
        }
    }

    fn change(kq: &OwnedFd, entry: libc::kevent) -> io::Result<()> {
        // SAFETY: one initialized change entry on a live kqueue, and no output slot.
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &entry,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// `<sys/inotify.h>`'s event head: wd, mask, cookie, len.
#[cfg(any(target_os = "linux", test))]
const INOTIFY_HEAD: usize = 16;

/// `<sys/inotify.h>`'s queue-overflow bit, which the generated libc does not
/// carry: the kernel dropped events, so any watched file may have changed.
#[cfg(any(target_os = "linux", test))]
const IN_Q_OVERFLOW: u32 = 0x0000_4000;

/// `<sys/inotify.h>`'s `IN_MOVE_SELF | IN_DELETE_SELF`: the watched folder
/// itself was renamed or went.
#[cfg(any(target_os = "linux", test))]
const INOTIFY_SELF: u32 = 0x0000_0800 | 0x0000_0400;

/// What a folder above a key's own one listens for: [`INOTIFY_SELF`], and `IN_ATTRIB`, its own
/// attributes changed (a `chmod` that lets the lookup through it or stops it). inotify has no
/// way to ask for a folder's own `IN_ATTRIB` alone: each entry's comes too (a `chmod`, `touch`
/// or an atomic write's `fchmod` in the folder), with the entry's name. It touches no key above,
/// and the wait it comes in goes on (the inotify events' `wait`).
#[cfg(any(target_os = "linux", test))]
const INOTIFY_ABOVE: u32 = INOTIFY_SELF | 0x0000_0004;

/// A named file arm filters sibling entries; a directory arm hears all of them.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
enum InotifyEntry {
    Named(Vec<u8>),
    Directory,
}

#[cfg(any(target_os = "linux", test))]
impl InotifyEntry {
    fn hears(&self, name: &[u8]) -> bool {
        match self {
            Self::Named(entry) => name.is_empty() || name == entry.as_slice(),
            Self::Directory => true,
        }
    }
}

/// Reverse registrations: named events visit just their entry's keys; folder
/// self events visit its owners and ancestor listeners. Empty routes disappear
/// immediately, so membership also answers whether the kernel watch is held.
#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct InotifyRoutes {
    own: BTreeMap<i32, BTreeMap<Vec<u8>, BTreeSet<u64>>>,
    directories: BTreeMap<i32, BTreeSet<u64>>,
    above: BTreeMap<i32, BTreeSet<u64>>,
    /// The file keys each watch on a file itself is for.
    files: BTreeMap<i32, BTreeSet<u64>>,
}

#[cfg(any(target_os = "linux", test))]
impl InotifyRoutes {
    fn add_own(&mut self, key: u64, wd: i32, entry: &InotifyEntry) {
        let InotifyEntry::Named(name) = entry else {
            self.directories.entry(wd).or_default().insert(key);
            return;
        };
        let entries = self.own.entry(wd).or_default();
        if let Some(keys) = entries.get_mut(name) {
            keys.insert(key);
        } else {
            entries.insert(name.to_vec(), BTreeSet::from([key]));
        }
    }

    fn remove_own(&mut self, key: u64, wd: i32, entry: &InotifyEntry) {
        let InotifyEntry::Named(name) = entry else {
            if let Some(keys) = self.directories.get_mut(&wd) {
                keys.remove(&key);
                if keys.is_empty() {
                    self.directories.remove(&wd);
                }
            }
            return;
        };
        if let Some(entries) = self.own.get_mut(&wd) {
            if let Some(keys) = entries.get_mut(name) {
                keys.remove(&key);
                if keys.is_empty() {
                    entries.remove(name);
                }
            }
            if entries.is_empty() {
                self.own.remove(&wd);
            }
        }
    }

    fn replace_above(&mut self, key: u64, old: &[i32], new: &[i32]) {
        if old == new {
            return;
        }
        for wd in old {
            if let Some(keys) = self.above.get_mut(wd) {
                keys.remove(&key);
                if keys.is_empty() {
                    self.above.remove(wd);
                }
            }
        }
        for wd in new {
            self.above.entry(*wd).or_default().insert(key);
        }
    }

    fn replace_file(&mut self, key: u64, old: Option<i32>, new: Option<i32>) {
        if let Some(wd) = old
            && let Some(keys) = self.files.get_mut(&wd)
        {
            keys.remove(&key);
            if keys.is_empty() {
                self.files.remove(&wd);
            }
        }
        if let Some(wd) = new {
            self.files.entry(wd).or_default().insert(key);
        }
    }

    fn held(&self, wd: i32) -> bool {
        self.own.contains_key(&wd)
            || self.directories.contains_key(&wd)
            || self.above.contains_key(&wd)
            || self.files.contains_key(&wd)
    }

    fn touch(&self, wd: i32, mask: u32, name: &[u8], touched: &mut Vec<u64>) {
        let start = touched.len();
        if let Some(keys) = self.directories.get(&wd) {
            touched.extend(keys);
        }
        if let Some(entries) = self.own.get(&wd) {
            if name.is_empty() {
                for keys in entries.values() {
                    touched.extend(keys);
                }
            } else if let Some(keys) = entries.get(name) {
                touched.extend(keys);
            }
        }
        // A file's own watch: a write or attribute change through any of its names.
        if name.is_empty()
            && let Some(keys) = self.files.get(&wd)
        {
            touched.extend(keys);
        }
        if name.is_empty() || self.directories.contains_key(&wd) {
            touched[start..].sort_unstable();
        }
        // Entry events from a shared watch never invalidate ancestor listeners.
        if name.is_empty()
            && mask & INOTIFY_ABOVE != 0
            && let Some(keys) = self.above.get(&wd)
        {
            touched.extend(keys);
        }
    }
}

/// Push the keys a buffer of `struct inotify_event`s touches: each event
/// whose watch and entry name are a key's (one on the directory itself, with
/// no name, touches every key on it), each one of a folder above a key's own
/// renamed, gone or changed in mode (no name: [`INOTIFY_ABOVE`]) touches that
/// key, while an entry's own event there touches none,
/// and every key after a queue overflow, whose look then finds which files
/// really changed.
#[cfg(any(target_os = "linux", test))]
fn inotify_touched(
    buffer: &[u8],
    keys: &BTreeMap<u64, (i32, InotifyEntry)>,
    routes: &InotifyRoutes,
    touched: &mut Vec<u64>,
) {
    let word = |at: usize| {
        buffer
            .get(at..at + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map_or(0, u32::from_ne_bytes)
    };
    let mut at = 0usize;
    while let Some(payload_at) = at.checked_add(INOTIFY_HEAD)
        && payload_at <= buffer.len()
    {
        let wd = i32::from_ne_bytes(word(at).to_ne_bytes());
        let mask = word(at + 4);
        let len = usize::try_from(word(at + 12)).unwrap_or(usize::MAX);
        let end = payload_at.saturating_add(len).min(buffer.len());
        let name = &buffer[payload_at..end];
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        if mask & IN_Q_OVERFLOW != 0 {
            touched.extend(keys.keys());
        } else if keys.len() == 1 && !name.is_empty() {
            // The common one-document case needs no reverse-tree lookups.
            if let Some((key, (watch, entry))) = keys.first_key_value()
                && *watch == wd
                && entry.hears(name)
            {
                touched.push(*key);
            }
        } else {
            routes.touch(wd, mask, name, touched);
        }
        at = end;
    }
}

#[cfg(target_os = "linux")]
mod inotify {
    //! Linux: one inotify descriptor watching each watched file's directory, or
    //! the nearest one above it that is there (events name the entry, so a
    //! replace by rename is heard, as is the making of the missing folder), each
    //! folder above that for its own rename, end or change of mode, the regular
    //! file a file key names (which hears a write through another of its names,
    //! a hard link, that its folder's watch does not name), and a pipe that
    //! wakes the wait. The kernel keeps one watch per folder, so a watch
    //! two keys hold stays until neither does. A folder's `IN_ATTRIB` cannot be
    //! asked for without its entries' (inotify(7)): those reach the wait, touch
    //! no key, and end no wait ([`InotifyEvents::wait`]). The wait also polls
    //! this namespace's mount table, which a mount or unmount marks
    //! ([`mount_table`]).

    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{FileEvents, INOTIFY_ABOVE, InotifyEntry, InotifyRoutes, Waker, inotify_touched};

    /// `<sys/inotify.h>`'s event bits the generated libc does not carry.
    const IN_MODIFY: u32 = 0x0000_0002;
    const IN_ATTRIB: u32 = 0x0000_0004;
    const IN_CLOSE_WRITE: u32 = 0x0000_0008;
    /// `<sys/inotify.h>`'s flag that adds a watch's mask to the one its folder has.
    const IN_MASK_ADD: u32 = 0x2000_0000;

    /// What a file's own watch hears through any of its names: a write, a write's close, and a
    /// change of mode, owner, times or link count.
    const FILE_EVENTS: u32 = IN_MODIFY | IN_ATTRIB | IN_CLOSE_WRITE;

    const DIRECTORY_EVENTS: u32 = IN_MODIFY
        | IN_ATTRIB
        | IN_CLOSE_WRITE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF;

    pub(super) struct InotifyEvents {
        fd: OwnedFd,
        wake_read: OwnedFd,
        wake_write: Arc<OwnedFd>,
        /// Each key's directory watch and entry filter.
        keys: BTreeMap<u64, (i32, InotifyEntry)>,
        /// Each key's watches on the folders above its directory.
        above: BTreeMap<u64, Vec<i32>>,
        /// Each file key's watch on the file itself, which hears a write made through another
        /// name (a hard link) that its folder's watch, routed by name, does not.
        files: BTreeMap<u64, i32>,
        /// The keys last armed on a volume that does not report every writer
        /// ([`super::volume_type_reports_every_writer`]).
        elsewhere: BTreeSet<u64>,
        routes: InotifyRoutes,
        /// This namespace's mount table ([`mount_table`]), where it opened.
        mounts: Option<OwnedFd>,
    }

    /// `/proc/self/mountinfo`, which `poll` marks `POLLPRI` (and `POLLERR`) once each time a
    /// mount or unmount changes this namespace's table (proc(5)): what every path below a mount
    /// point names then changes, with no inotify event on the folder it covers. `None` where it
    /// does not open (no `/proc`), where no mount is heard.
    fn mount_table() -> Option<OwnedFd> {
        // SAFETY: a NUL-terminated path; `open` returns a fresh descriptor or -1.
        let raw = unsafe {
            libc::open(
                c"/proc/self/mountinfo".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        // SAFETY: `raw` is fresh and exclusively ours where it is not -1.
        (raw >= 0).then(|| unsafe { OwnedFd::from_raw_fd(raw) })
    }

    impl InotifyEvents {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: inotify_init1 creates a new descriptor owned by this process.
            let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is fresh and exclusively ours.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            let (wake_read, wake_write) = crate::pipe()?;
            let (wake_read, wake_write) = (OwnedFd::from(wake_read), OwnedFd::from(wake_write));
            for end in [&wake_read, &wake_write] {
                // SAFETY: F_GETFL reads the status flags of a descriptor this function owns.
                let flags = unsafe { libc::fcntl(end.as_raw_fd(), libc::F_GETFL) };
                // SAFETY: F_SETFL takes an integer flag word for the same owned descriptor.
                if flags < 0
                    || unsafe {
                        libc::fcntl(end.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                    } < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(Self {
                fd,
                wake_read,
                wake_write: Arc::new(wake_write),
                keys: BTreeMap::new(),
                above: BTreeMap::new(),
                files: BTreeMap::new(),
                elsewhere: BTreeSet::new(),
                routes: InotifyRoutes::default(),
                mounts: mount_table(),
            })
        }

        /// Whether any key still holds `wd`, as its directory, a folder above it or its file.
        fn held(&self, wd: i32) -> bool {
            self.routes.held(wd)
        }

        /// Stop watching each of `watches` no key holds any more.
        fn release(&self, watches: impl IntoIterator<Item = i32>) {
            for wd in watches {
                if !self.held(wd) {
                    // SAFETY: a live inotify descriptor and a watch it returned.
                    unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) };
                }
            }
        }

        /// Watch every folder and link that looking `folder` up passes through
        /// ([`super::passed_through`]), but `folder`, `key`'s own watch, and the root, which
        /// nothing renames, for its own rename, end or change of mode ([`INOTIFY_ABOVE`]); a mask
        /// added to whatever another key
        /// watches there. A link is watched itself, which hears it replaced (made to name another
        /// folder), and so is each folder the lookup goes on through behind it. A folder on
        /// another device is watched too: a btrfs subvolume can be renamed, and so can the folder
        /// holding a mount (the mount itself cannot be), which takes the mount with it. One the
        /// kernel refuses to watch is passed over, and the rest are still watched.
        ///
        /// Then `folder` must still be the folder the key's own watch is on: a folder above it
        /// renamed before its watch took moved it away unheard. The kernel keeps one watch per
        /// folder, so watching `folder` again answers that watch while the path names it.
        ///
        /// # Errors
        /// [`io::ErrorKind::NotFound`] when a folder or link on the way moved or went as the walk
        /// watched it, so the arm looks again; the kernel's when it is out of watches, and the
        /// lookup's where it failed ([`super::passed_through`]).
        fn watch_above(&mut self, key: u64, folder: &Path) -> io::Result<()> {
            let own = self.keys.get(&key).map(|(wd, _)| *wd);
            let mut watches = Vec::new();
            let walked = (|| {
                for passed in super::passed_through(folder)? {
                    if passed.path.parent().is_none() {
                        continue;
                    }
                    let path = CString::new(passed.path.as_os_str().as_bytes())
                        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
                    let only = if passed.link { 0 } else { libc::IN_ONLYDIR };
                    // SAFETY: a live inotify descriptor and a NUL-terminated path.
                    let wd = unsafe {
                        libc::inotify_add_watch(
                            self.fd.as_raw_fd(),
                            path.as_ptr(),
                            INOTIFY_ABOVE | IN_MASK_ADD | only | libc::IN_DONT_FOLLOW,
                        )
                    };
                    if wd >= 0 {
                        if Some(wd) != own {
                            watches.push(wd);
                        }
                        continue;
                    }
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ENOSPC)
                    {
                        return Err(error);
                    }
                    // A link or a file where the lookup passed a folder: it moved as the walk ran.
                    if !passed.link && error.raw_os_error() == Some(libc::ENOTDIR) {
                        return Err(io::Error::from(io::ErrorKind::NotFound));
                    }
                }
                Ok(())
            })();
            let old = self.above.insert(key, watches).unwrap_or_default();
            self.routes.replace_above(key, &old, &self.above[&key]);
            self.release(old);
            walked?;
            match self.add_folder_watch(folder) {
                Ok(wd) if Some(wd) == own => Ok(()),
                Ok(wd) => {
                    self.release([wd]);
                    Err(io::Error::from(io::ErrorKind::NotFound))
                }
                Err(_) => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }

        /// Watch `directory` for its entries' events and its own, as a key's own folder is.
        fn add_folder_watch(&self, directory: &Path) -> io::Result<i32> {
            let directory = CString::new(directory.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: a live inotify descriptor and a NUL-terminated path.
            let wd = unsafe {
                libc::inotify_add_watch(
                    self.fd.as_raw_fd(),
                    directory.as_ptr(),
                    DIRECTORY_EVENTS | libc::IN_ONLYDIR,
                )
            };
            if wd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(wd)
        }

        /// Watch the nearest folder of `path`'s that is there ([`super::folders_up`]): its watch,
        /// and the entry below it on the way to `path` (`path` itself, where its own folder is
        /// there).
        ///
        /// # Errors
        /// The kernel's where it refuses a folder's watch, or where a link on the way names
        /// nothing.
        fn watch_folder<'a>(&self, path: &'a Path) -> io::Result<(i32, &'a Path)> {
            for (directory, below) in super::folders_up(path) {
                match self.add_folder_watch(directory) {
                    Ok(wd) => return Ok((wd, below)),
                    // A link naming nothing, whose folder went or is not made yet: where it comes
                    // back is no folder of this path's, so no watch above the link hears it. The
                    // arm fails, as a link refuses it on macOS, and the watch arms it again.
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            && !std::fs::symlink_metadata(directory)
                                .is_ok_and(|there| there.file_type().is_symlink()) => {}
                    // A file where a folder of the path's would be: no folder is there, and
                    // the folder that holds the file hears it replaced by one.
                    Err(error) if super::under_a_file(&error, directory) => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::from(io::ErrorKind::InvalidInput))
        }

        /// [`FileEvents::arm`], running `between` with the folder the key's own watch is on once
        /// that watch is on and before the folders above it are watched.
        pub(super) fn arm_then(
            &mut self,
            key: u64,
            path: &Path,
            mut between: impl FnMut(&Path),
        ) -> io::Result<()> {
            for _ in 0..=super::ARM_AGAIN {
                let (wd, below) = self.watch_folder(path)?;
                let name = below
                    .file_name()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                self.set_own(key, wd, InotifyEntry::Named(name.as_bytes().to_vec()));
                // A folder made between its failed watch and the watch above it, which
                // hears nothing inside it: arm again, lower. A file there is no folder made.
                if below != path && below.is_dir() {
                    continue;
                }
                let folder = below
                    .parent()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                between(folder);
                match self.watch_above(key, folder) {
                    // A folder on the way moved as the walk watched: arm again.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Ok(()) => {
                        let file = (below == path).then_some(path);
                        self.watch_file(key, file)?;
                        self.note_volume(
                            key,
                            folder,
                            self.files.contains_key(&key).then_some(path),
                        );
                        return Ok(());
                    }
                    above => return above,
                }
            }
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        /// Watch the regular file `path` names, where it is one, for writes and attribute changes
        /// made through any of its names, in place of `key`'s file watch before; none where
        /// there is no such file (it is missing, or a link, a folder or a device).
        ///
        /// # Errors
        /// The kernel's when it is out of watches, so the arm is made again; any other refusal
        /// (a file this user cannot read) leaves the key its folder's watch.
        fn watch_file(&mut self, key: u64, path: Option<&Path>) -> io::Result<()> {
            let file = path.filter(|path| {
                std::fs::symlink_metadata(path).is_ok_and(|there| there.file_type().is_file())
            });
            let watched = match file {
                None => None,
                Some(path) => {
                    let path = CString::new(path.as_os_str().as_bytes())
                        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
                    // SAFETY: a live inotify descriptor and a NUL-terminated path.
                    let wd = unsafe {
                        libc::inotify_add_watch(
                            self.fd.as_raw_fd(),
                            path.as_ptr(),
                            FILE_EVENTS | IN_MASK_ADD | libc::IN_DONT_FOLLOW,
                        )
                    };
                    if wd < 0 {
                        let error = io::Error::last_os_error();
                        if matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::ENOMEM)) {
                            self.drop_file(key);
                            return Err(error);
                        }
                        None
                    } else {
                        Some(wd)
                    }
                }
            };
            let old = match watched {
                Some(wd) => self.files.insert(key, wd),
                None => self.files.remove(&key),
            };
            if old != watched {
                self.routes.replace_file(key, old, watched);
                self.release(old);
            }
            Ok(())
        }

        /// `key` has no file watch now.
        fn drop_file(&mut self, key: u64) {
            if let Some(old) = self.files.remove(&key) {
                self.routes.replace_file(key, Some(old), None);
                self.release([old]);
            }
        }

        /// Note whether `key`'s volumes, the one `folder` is on and the one its watched `file`
        /// is on, report every writer; one that cannot be read is taken as not, and the key is
        /// looked at in rounds.
        fn note_volume(&mut self, key: u64, folder: &Path, file: Option<&Path>) {
            let reports = std::iter::once(folder).chain(file).all(|on| {
                crate::shared_write::filesystem_at(on)
                    .is_some_and(super::volume_type_reports_every_writer)
            });
            if reports {
                self.elsewhere.remove(&key);
            } else {
                self.elsewhere.insert(key);
            }
        }

        /// A directory arm with the same registration boundary as [`Self::arm_then`]: its own
        /// kernel watch is installed before `between`, and the ancestry walk must then confirm
        /// that this path still names that watch's directory.
        pub(super) fn arm_directory_then(
            &mut self,
            key: u64,
            path: &Path,
            mut between: impl FnMut(&Path),
        ) -> io::Result<()> {
            self.drop_file(key);
            for _ in 0..=super::ARM_AGAIN {
                let wd = self.add_folder_watch(path)?;
                self.set_own(key, wd, InotifyEntry::Directory);
                between(path);
                match self.watch_above(key, path) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    answer => {
                        if answer.is_ok() {
                            self.note_volume(key, path, None);
                        }
                        return answer;
                    }
                }
            }
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        fn set_own(&mut self, key: u64, wd: i32, entry: InotifyEntry) {
            let old = self.keys.insert(key, (wd, entry));
            if old.as_ref() != self.keys.get(&key) {
                if let Some((old_wd, old_entry)) = &old {
                    self.routes.remove_own(key, *old_wd, old_entry);
                }
                self.routes.add_own(key, wd, &self.keys[&key].1);
            }
            if let Some((old_wd, _)) = old
                && old_wd != wd
            {
                self.release([old_wd]);
            }
        }

        fn drain_wakes(&self) {
            super::read_batch::<64>(|buffer| read_nonblocking(&self.wake_read, buffer), |_| {});
        }

        fn read_events(&self, touched: &mut Vec<u64>) {
            super::read_batch::<4096>(
                |buffer| read_nonblocking(&self.fd, buffer),
                |buffer| inotify_touched(buffer, &self.keys, &self.routes, touched),
            );
        }
    }

    /// Both descriptors are created nonblocking and stay owned throughout this borrow.
    fn read_nonblocking(fd: &OwnedFd, buffer: &mut [u8]) -> Option<usize> {
        // SAFETY: fd is live and buffer supplies the passed number of writable bytes.
        let read = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        usize::try_from(read).ok()
    }

    impl FileEvents for InotifyEvents {
        fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
            self.arm_then(key, path, |_| {})
        }

        fn arm_directory(&mut self, key: u64, path: &Path) -> io::Result<()> {
            self.arm_directory_then(key, path, |_| {})
        }

        fn disarm(&mut self, key: u64) {
            let own = self.keys.remove(&key);
            if let Some((wd, name)) = &own {
                self.routes.remove_own(key, *wd, name);
            }
            let above = self.above.remove(&key).unwrap_or_default();
            self.routes.replace_above(key, &above, &[]);
            self.release(own.into_iter().map(|(wd, _)| wd).chain(above));
            self.drop_file(key);
            self.elsewhere.remove(&key);
        }

        fn hears_every_writer(&self, key: u64) -> bool {
            !self.elsewhere.contains(&key)
        }

        /// Events that touch no key end no wait: an entry's `IN_ATTRIB` in a folder above a
        /// key's own ([`INOTIFY_ABOVE`]), which the kernel sends with the folder's own, or one
        /// for a watch just let go. The wait goes on for what is left of `timeout`, so a caller
        /// woken only by a key's event, a wake or the time (the board keeper's rest) is not woken
        /// by a write it does not watch.
        fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
            let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
            let before = touched.len();
            loop {
                let left = match (timeout, deadline) {
                    (Some(_), Some(deadline)) => {
                        Some(deadline.saturating_duration_since(Instant::now()))
                    }
                    (timeout, _) => timeout,
                };
                let mut fds = [
                    libc::pollfd {
                        fd: self.fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: self.wake_read.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    // A negative descriptor, where the table did not open, `poll` passes over.
                    libc::pollfd {
                        fd: self.mounts.as_ref().map_or(-1, |table| table.as_raw_fd()),
                        events: libc::POLLPRI,
                        revents: 0,
                    },
                ];
                // SAFETY: three live pollfd slots.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 3, super::poll_millis(left)) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    return if error.kind() == io::ErrorKind::Interrupted {
                        Ok(())
                    } else {
                        Err(error)
                    };
                }
                let woken = fds[1].revents != 0;
                if woken {
                    self.drain_wakes();
                }
                if fds[0].revents != 0 {
                    self.read_events(touched);
                }
                if fds[2].revents & (libc::POLLPRI | libc::POLLERR) != 0 {
                    // A mount moves what any path names: every key looks again, armed again.
                    touched.extend(self.keys.keys().copied());
                }
                if woken || ready == 0 || touched.len() > before || left == Some(Duration::ZERO) {
                    return Ok(());
                }
            }
        }

        fn waker(&self) -> Waker {
            let write = Arc::clone(&self.wake_write);
            Arc::new(move || {
                let byte = 1u8;
                // SAFETY: a live non-blocking descriptor and one byte; a full pipe already wakes.
                unsafe { libc::write(write.as_raw_fd(), (&raw const byte).cast(), 1) };
            })
        }
    }
}

#[cfg(test)]
#[path = "file_watch_tests.rs"]
mod tests;
