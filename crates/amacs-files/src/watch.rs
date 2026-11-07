// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The file watch: an open document notices when its file changes on disk,
//! whoever changed it (an AI CLI's edit tool, `git checkout`, a formatter).
//!
//! [`FileWatch`] runs one thread. It waits on the platform's file events
//! through a [`FileEvents`] arm (kqueue on macOS, inotify on Linux, a stat
//! poll elsewhere, and [`InertEvents`] for a test that drives the host by
//! hand), lets a burst of them settle ([`Debounce`]), and tells its listener
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
//! above that one, up to the root, listens only for its own rename or end, one
//! listener per folder however many files' paths run through it, so a folder
//! of the path renamed (a project folder moved, a checkout swapped in) is
//! heard as well. A file no arm took (a folder refused it, descriptors or
//! watches ran out, or its folders kept being made or moved while it armed)
//! is armed again every `REARM_EVERY`, and only while some file is unarmed.
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
    /// The stamp of the file `path` names now, or `None` when it names none.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Some(Self {
                dev: meta.dev(),
                ino: meta.ino(),
                len: meta.size(),
                modified_ns: i128::from(meta.mtime()) * 1_000_000_000
                    + i128::from(meta.mtime_nsec()),
                changed_ns: i128::from(meta.ctime()) * 1_000_000_000
                    + i128::from(meta.ctime_nsec()),
            })
        }
        #[cfg(not(unix))]
        {
            let modified_ns = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos() as i128);
            Some(Self {
                dev: 0,
                ino: 0,
                len: meta.len(),
                modified_ns,
                changed_ns: 0,
            })
        }
    }
}

/// Lets each watched file's burst of events settle: a file is due once its
/// events stop for [`QUIET`], or [`SETTLE_CAP`] after the first of them.
#[derive(Clone, Debug, Default)]
pub struct Debounce {
    /// Each noisy file's first and latest event.
    pending: BTreeMap<u64, (Instant, Instant)>,
}

impl Debounce {
    /// `key`'s file had an event at `now`.
    pub fn note(&mut self, key: u64, now: Instant) {
        self.pending
            .entry(key)
            .and_modify(|(_, latest)| *latest = now)
            .or_insert((now, now));
    }

    /// Forget `key` (it is no longer watched).
    pub fn forget(&mut self, key: u64) {
        self.pending.remove(&key);
    }

    /// The files due at `now`, which leave the debounce.
    pub fn due(&mut self, now: Instant) -> Vec<u64> {
        let due: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, (first, latest))| {
                now.saturating_duration_since(*latest) >= QUIET
                    || now.saturating_duration_since(*first) >= SETTLE_CAP
            })
            .map(|(key, _)| *key)
            .collect();
        for key in &due {
            self.pending.remove(key);
        }
        due
    }

    /// When the next file falls due, if any is pending.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .filter_map(|(first, latest)| {
                match (latest.checked_add(QUIET), first.checked_add(SETTLE_CAP)) {
                    (Some(quiet), Some(cap)) => Some(quiet.min(cap)),
                    (Some(quiet), None) => Some(quiet),
                    (None, Some(cap)) => Some(cap),
                    (None, None) => None,
                }
            })
            .min()
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
}

enum Command {
    Watch(u64, PathBuf),
    Unwatch(u64),
}

/// The watch thread and the door to it.
pub struct FileWatch {
    commands: mpsc::Sender<Command>,
    wake: Waker,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FileWatch {
    /// Start the watch over this platform's file events, telling `notify`
    /// each watched key whose file changed.
    ///
    /// # Errors
    /// When the platform's event source or the thread cannot start.
    pub fn platform(notify: Box<dyn FnMut(u64) + Send>) -> io::Result<Self> {
        Self::spawn(platform_events()?, notify)
    }

    /// Start the watch over `events`.
    ///
    /// # Errors
    /// When the thread cannot start.
    pub fn spawn(
        events: Box<dyn FileEvents>,
        notify: Box<dyn FnMut(u64) + Send>,
    ) -> io::Result<Self> {
        let (commands, receive) = mpsc::channel();
        let wake = events.waker();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("aterm-document-watch".to_string())
            .spawn(move || watch_loop(events, &receive, &stopping, notify))?;
        Ok(Self {
            commands,
            wake,
            stop,
            thread: Some(thread),
        })
    }

    /// Watch the file at `path` as `key` (again, after a save-as).
    pub fn watch(&self, key: u64, path: PathBuf) {
        if self.commands.send(Command::Watch(key, path)).is_ok() {
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

fn watch_loop(
    mut events: Box<dyn FileEvents>,
    commands: &mpsc::Receiver<Command>,
    stop: &std::sync::atomic::AtomicBool,
    mut notify: Box<dyn FnMut(u64) + Send>,
) {
    // Each watched file and the stamp the watch last told, `Seen::Never`
    // until its first look.
    let mut watched: BTreeMap<u64, (PathBuf, Seen)> = BTreeMap::new();
    let mut unarmed = Unarmed::default();
    let mut debounce = Debounce::default();
    let mut touched = Vec::new();
    loop {
        if stop.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        loop {
            match commands.try_recv() {
                Ok(Command::Watch(key, path)) => {
                    // Armed, then looked at once, and that first look tells:
                    // the host read the file before this, so a change since
                    // its read may already be in any stamp taken here.
                    let now = Instant::now();
                    unarmed.arm(events.as_mut(), key, &path, now);
                    watched.insert(key, (path, Seen::Never));
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
        }
        let before = Instant::now();
        let due = debounce
            .deadline()
            .map(|deadline| deadline.saturating_duration_since(before));
        let timeout = match (due, unarmed.wait(before)) {
            (Some(due), Some(rearm)) => Some(due.min(rearm)),
            (due, rearm) => due.or(rearm),
        };
        touched.clear();
        if events.wait(timeout, &mut touched).is_err() {
            // A failed wait is not a reason to spin: look again shortly.
            std::thread::park_timeout(QUIET);
        }
        let now = Instant::now();
        for key in unarmed.due(now) {
            // An arm that takes now is looked at, as a first look is: the
            // file may have come back with its folder before the arm.
            if let Some((path, _)) = watched.get(&key)
                && unarmed.arm(events.as_mut(), key, path, now)
            {
                debounce.note(key, now);
            }
        }
        for key in &touched {
            if watched.contains_key(key) {
                debounce.note(*key, now);
            }
        }
        for key in debounce.due(now) {
            let Some((path, seen)) = watched.get_mut(&key) else {
                continue;
            };
            unarmed.arm(events.as_mut(), key, path, now);
            let stamp = Seen::Stamp(FileStamp::of(path));
            if stamp != *seen {
                *seen = stamp;
                notify(key);
            }
        }
    }
}

/// The watched files no arm took ([`FileEvents::arm`]), which the watch arms
/// again every [`REARM_EVERY`] after the last arm that failed, while one is
/// left; with none left the watch waits on events alone.
#[derive(Debug)]
struct Unarmed {
    keys: BTreeSet<u64>,
    last_failed: Instant,
}

impl Default for Unarmed {
    fn default() -> Self {
        Self {
            keys: BTreeSet::new(),
            last_failed: Instant::now(),
        }
    }
}

impl Unarmed {
    /// Arm `key` on `path` at `now`: whether it took. One that did not is
    /// armed again later ([`Self::due`]).
    fn arm(&mut self, events: &mut dyn FileEvents, key: u64, path: &Path, now: Instant) -> bool {
        let armed = events.arm(key, path).is_ok();
        if armed {
            self.keys.remove(&key);
        } else {
            self.keys.insert(key);
            self.last_failed = now;
        }
        armed
    }

    /// `key` is no longer watched.
    fn forget(&mut self, key: u64) {
        self.keys.remove(&key);
    }

    /// How long from `now` until the next arm again, if a file is unarmed.
    fn wait(&self, now: Instant) -> Option<Duration> {
        (!self.keys.is_empty())
            .then(|| REARM_EVERY.saturating_sub(now.saturating_duration_since(self.last_failed)))
    }

    /// The unarmed files to arm again at `now`: every one once
    /// [`REARM_EVERY`] passed since the last failed arm, else none.
    fn due(&mut self, now: Instant) -> Vec<u64> {
        if self.wait(now) != Some(Duration::ZERO) {
            return Vec::new();
        }
        self.last_failed = now;
        self.keys.iter().copied().collect()
    }
}

/// What the watch last told of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// Nothing yet: its first look tells.
    Never,
    /// This stamp (`None`: no file at the path).
    Stamp(Option<FileStamp>),
}

/// This platform's file events: kqueue on macOS, inotify on Linux, a stat
/// poll elsewhere.
///
/// # Errors
/// When the kernel refuses the event source.
pub fn platform_events() -> io::Result<Box<dyn FileEvents>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(kqueue::KqueueEvents::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(inotify::InotifyEvents::new()?))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(Box::new(PollEvents::default()))
    }
}

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
    files: BTreeMap<u64, (PathBuf, Option<FileStamp>)>,
    inert: InertEvents,
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "linux"))))]
impl FileEvents for PollEvents {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        let stamp = FileStamp::of(path);
        self.files.insert(key, (path.to_path_buf(), stamp));
        Ok(())
    }

    fn disarm(&mut self, key: u64) {
        self.files.remove(&key);
    }

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        let nap = timeout.map_or(POLL_EVERY, |timeout| timeout.min(POLL_EVERY));
        self.inert.wait(Some(nap), touched)?;
        for (key, (path, seen)) in &mut self.files {
            let stamp = FileStamp::of(path);
            if stamp != *seen {
                *seen = stamp;
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
const ARM_AGAIN: usize = 8;

/// The folders a watch of `path` can listen on, nearest first, each with the
/// entry below it on the way to `path`: its own folder (with `path`), then
/// each one above (with the folder below it). Where `path`'s folder is
/// missing, the nearest one that is there hears it made, and the look that
/// event brings arms again lower.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn folders_up(path: &Path) -> impl Iterator<Item = (&Path, &Path)> {
    std::iter::successors(Some(path), |below| below.parent())
        .filter_map(|below| Some((below.parent()?, below)))
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
    //! folder above that, shared by every file whose path runs through it, and
    //! a user event that wakes the wait.

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
    /// renamed (a replace by rename writes the directory).
    const DIRECTORY_EVENTS: u32 =
        libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;

    /// The events that mean a folder above a file's own one was renamed or went, which moves the
    /// file's path off what its own listeners hold. A write into one is not among them, so a busy
    /// folder up the path (a home folder) wakes nothing.
    const ABOVE_EVENTS: u32 = libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;

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
        /// Listen on the nearest folder of `path`'s that is there ([`super::folders_up`]): that
        /// folder, and the folder just below it that was missing, or `None` where that nearest
        /// one is `path`'s own.
        fn listen_folder<'p>(
            &mut self,
            key: u64,
            path: &'p Path,
            step: &mut impl FnMut(Listen, &Path) -> io::Result<()>,
        ) -> io::Result<(&'p Path, Option<PathBuf>)> {
            for (directory, below) in super::folders_up(path) {
                let listened = self.listen(key, directory, true, step);
                step(Listen::Answered, directory)?;
                match listened {
                    Ok(()) => return Ok((directory, (below != path).then(|| below.to_path_buf()))),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Err(io::Error::from(io::ErrorKind::NotFound))
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
        /// ([`super::passed_through`]) and its own listeners do not hold, each for its own rename
        /// or end ([`Self::listen_one`]): any of them renamed, gone or replaced moves the path off
        /// what those listeners hold. A link is listened on itself, which hears it replaced (made
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
        /// file's own folder refused its listen (a link, or one this user cannot read) and the
        /// file's listener took: that is all the key holds there.
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
                    // nothing inside it: arm again, lower.
                    Ok((_, Some(made))) if made.exists() => {}
                    Ok((directory, _)) => match self.listen_above(key, directory, directory) {
                        // A folder on the way moved as the walk listened: arm again.
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
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

        /// How many folders above the keys' own have a listener, and which keys share each.
        #[cfg(test)]
        pub(super) fn above(&self) -> Vec<((u64, u64), Vec<u64>)> {
            self.above
                .iter()
                .map(|(id, above)| (*id, above.keys.iter().copied().collect()))
                .collect()
        }
    }

    /// Where a listen has got to, for [`KqueueEvents::arm_around`].
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Listen {
        /// The path opened, and its filter is not on yet.
        Opened,
        /// The listen has answered, either way.
        Answered,
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

/// Push the keys a buffer of `struct inotify_event`s touches: each event
/// whose watch and entry name are a key's (one on the directory itself, with
/// no name, touches every key on it), each one on a folder above a key's own
/// (`above`, which hears only that folder renamed or gone) touches that key,
/// and every key after a queue overflow, whose look then finds which files
/// really changed.
#[cfg(any(target_os = "linux", test))]
fn inotify_touched(
    buffer: &[u8],
    keys: &BTreeMap<u64, (i32, Vec<u8>)>,
    above: &BTreeMap<u64, Vec<i32>>,
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
        } else {
            for (key, (watch, entry)) in keys {
                if *watch == wd && (name.is_empty() || name == entry.as_slice()) {
                    touched.push(*key);
                }
            }
            // A watch another key holds as its own folder also hears that folder's entries,
            // which name one: only the folder's own rename or end touches a key above it.
            if name.is_empty() && mask & INOTIFY_SELF != 0 {
                for (key, watches) in above {
                    if watches.contains(&wd) {
                        touched.push(*key);
                    }
                }
            }
        }
        at = end;
    }
}

#[cfg(target_os = "linux")]
mod inotify {
    //! Linux: one inotify descriptor watching each watched file's directory, or
    //! the nearest one above it that is there (events name the entry, so a
    //! replace by rename is heard, as is the making of the missing folder), each
    //! folder above that for its own rename or end, and a pipe that wakes the
    //! wait. The kernel keeps one watch per folder, so a watch two keys hold
    //! stays until neither does.

    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{FileEvents, INOTIFY_SELF, Waker, inotify_touched};

    /// `<sys/inotify.h>`'s event bits the generated libc does not carry.
    const IN_MODIFY: u32 = 0x0000_0002;
    const IN_ATTRIB: u32 = 0x0000_0004;
    const IN_CLOSE_WRITE: u32 = 0x0000_0008;
    /// `<sys/inotify.h>`'s flag that adds a watch's mask to the one its folder has.
    const IN_MASK_ADD: u32 = 0x2000_0000;

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
        /// Each key's directory watch and entry name.
        keys: BTreeMap<u64, (i32, Vec<u8>)>,
        /// Each key's watches on the folders above its directory.
        above: BTreeMap<u64, Vec<i32>>,
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
            let mut pipe = [0; 2];
            // SAFETY: pipe2 writes two fresh descriptors into the two-slot array.
            if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both descriptors are fresh and exclusively ours.
            let (wake_read, wake_write) =
                unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
            Ok(Self {
                fd,
                wake_read,
                wake_write: Arc::new(wake_write),
                keys: BTreeMap::new(),
                above: BTreeMap::new(),
            })
        }

        /// Whether any key still holds `wd`, as its directory or as a folder above it.
        fn held(&self, wd: i32) -> bool {
            self.keys.values().any(|(other, _)| *other == wd)
                || self.above.values().any(|watches| watches.contains(&wd))
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
        /// nothing renames, for its own rename or end; a mask added to whatever another key
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
                            INOTIFY_SELF | IN_MASK_ADD | only | libc::IN_DONT_FOLLOW,
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
                libc::inotify_add_watch(self.fd.as_raw_fd(), directory.as_ptr(), DIRECTORY_EVENTS)
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
                if let Some((old_wd, _)) = self.keys.insert(key, (wd, name.as_bytes().to_vec()))
                    && old_wd != wd
                {
                    self.release([old_wd]);
                }
                // A folder made between its failed watch and the watch above it, which
                // hears nothing inside it: arm again, lower.
                if below != path && below.exists() {
                    continue;
                }
                let folder = below
                    .parent()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                between(folder);
                match self.watch_above(key, folder) {
                    // A folder on the way moved as the walk watched: arm again.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    above => return above,
                }
            }
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        fn drain_wakes(&self) {
            let mut sink = [0u8; 64];
            // SAFETY: a live non-blocking descriptor and a buffer of the length passed.
            while unsafe {
                libc::read(
                    self.wake_read.as_raw_fd(),
                    sink.as_mut_ptr().cast(),
                    sink.len(),
                )
            } > 0
            {}
        }

        fn read_events(&self, touched: &mut Vec<u64>) {
            let mut buffer = [0u8; 4096];
            loop {
                // SAFETY: a live non-blocking descriptor and a buffer of the length passed.
                let read = unsafe {
                    libc::read(
                        self.fd.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                    )
                };
                let Ok(read) = usize::try_from(read) else {
                    return;
                };
                if read == 0 {
                    return;
                }
                inotify_touched(
                    &buffer[..read.min(buffer.len())],
                    &self.keys,
                    &self.above,
                    touched,
                );
            }
        }
    }

    impl FileEvents for InotifyEvents {
        fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
            self.arm_then(key, path, |_| {})
        }

        fn disarm(&mut self, key: u64) {
            let own = self.keys.remove(&key).map(|(wd, _)| wd);
            let above = self.above.remove(&key).unwrap_or_default();
            self.release(own.into_iter().chain(above));
        }

        fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
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
            ];
            // SAFETY: two live pollfd slots.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, super::poll_millis(timeout)) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::Interrupted {
                    Ok(())
                } else {
                    Err(error)
                };
            }
            if fds[1].revents != 0 {
                self.drain_wakes();
            }
            if fds[0].revents != 0 {
                self.read_events(touched);
            }
            Ok(())
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
#[path = "watch_tests.rs"]
mod tests;
