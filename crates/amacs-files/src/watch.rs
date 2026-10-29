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
//! file the path names now.
//!
//! The watch only tells: the host re-reads the file through the document's
//! own capability and decides (reload a clean buffer in place, or offer
//! reload or keep over unsaved edits).

use std::collections::BTreeMap;
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
            .map(|(first, latest)| (*latest + QUIET).min(*first + SETTLE_CAP))
            .min()
    }
}

/// The platform side of the watch: arms events on files and reports which
/// keys heard one. The watch thread owns it.
pub trait FileEvents: Send {
    /// Hear events on the file `path` names now and on its directory, under
    /// `key`, in place of whatever `key` was armed on before.
    ///
    /// # Errors
    /// The platform's, when neither the file nor its directory can be armed.
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
                    let _ = events.arm(key, &path);
                    watched.insert(key, (path, Seen::Never));
                    debounce.note(key, Instant::now());
                }
                Ok(Command::Unwatch(key)) => {
                    events.disarm(key);
                    watched.remove(&key);
                    debounce.forget(key);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let timeout = debounce
            .deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        touched.clear();
        if events.wait(timeout, &mut touched).is_err() {
            // A failed wait is not a reason to spin: look again shortly.
            std::thread::park_timeout(QUIET);
        }
        let now = Instant::now();
        for key in &touched {
            if watched.contains_key(key) {
                debounce.note(*key, now);
            }
        }
        for key in debounce.due(now) {
            let Some((path, seen)) = watched.get_mut(&key) else {
                continue;
            };
            let _ = events.arm(key, path);
            let stamp = Seen::Stamp(FileStamp::of(path));
            if stamp != *seen {
                *seen = stamp;
                notify(key);
            }
        }
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

/// `<sys/fcntl.h>`'s `O_EVTONLY`, which the generated libc does not carry: a
/// descriptor for events only, which never keeps its volume from unmounting
/// (a document open from a USB drive or a network share can still eject).
#[cfg(target_os = "macos")]
const O_EVTONLY: libc::c_int = 0x0000_8000;

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
    //! directory, and a user event that wakes the wait.

    use std::collections::BTreeMap;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{FileEvents, Waker, open_for_events};

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

    pub(super) struct KqueueEvents {
        kq: Arc<OwnedFd>,
        /// Each key's open file and directory (closing one drops its filter).
        armed: BTreeMap<u64, Vec<std::fs::File>>,
        /// Which key each open descriptor hears for.
        keys: BTreeMap<RawFd, u64>,
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
            })
        }

        fn listen(&mut self, key: u64, path: &Path, directory: bool) -> io::Result<()> {
            let file = open_for_events(path, directory)?;
            let fd = file.as_raw_fd();
            let fflags = if directory {
                DIRECTORY_EVENTS
            } else {
                FILE_EVENTS
            };
            change(
                &self.kq,
                libc::kevent {
                    ident: libc::uintptr_t::try_from(fd).unwrap_or(0),
                    filter: libc::EVFILT_VNODE,
                    flags: libc::EV_ADD | libc::EV_CLEAR,
                    fflags,
                    data: 0,
                    udata: std::ptr::null_mut(),
                },
            )?;
            self.keys.insert(fd, key);
            self.armed.entry(key).or_default().push(file);
            Ok(())
        }
    }

    impl FileEvents for KqueueEvents {
        fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
            self.disarm(key);
            let file = self.listen(key, path, false);
            let directory = path
                .parent()
                .map_or(Ok(()), |directory| self.listen(key, directory, true));
            file.or(directory)
        }

        fn disarm(&mut self, key: u64) {
            for file in self.armed.remove(&key).unwrap_or_default() {
                self.keys.remove(&file.as_raw_fd());
            }
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
                if let Some(key) = RawFd::try_from(event.ident)
                    .ok()
                    .and_then(|fd| self.keys.get(&fd))
                {
                    touched.push(*key);
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

/// Push the keys a buffer of `struct inotify_event`s touches: each event
/// whose watch and entry name are a key's (one on the directory itself, with
/// no name, touches every key on it), and every key after a queue overflow,
/// whose look then finds which files really changed.
#[cfg(any(target_os = "linux", test))]
fn inotify_touched(buffer: &[u8], keys: &BTreeMap<u64, (i32, Vec<u8>)>, touched: &mut Vec<u64>) {
    let word = |at: usize| {
        buffer
            .get(at..at + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map_or(0, u32::from_ne_bytes)
    };
    let mut at = 0;
    while at + INOTIFY_HEAD <= buffer.len() {
        let wd = i32::from_ne_bytes(word(at).to_ne_bytes());
        let mask = word(at + 4);
        let len = usize::try_from(word(at + 12)).unwrap_or(0);
        let end = (at + INOTIFY_HEAD + len).min(buffer.len());
        let name = &buffer[at + INOTIFY_HEAD..end];
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        if mask & IN_Q_OVERFLOW != 0 {
            touched.extend(keys.keys());
        } else {
            for (key, (watch, entry)) in keys {
                if *watch == wd && (name.is_empty() || name == entry.as_slice()) {
                    touched.push(*key);
                }
            }
        }
        at = end;
    }
}

#[cfg(target_os = "linux")]
mod inotify {
    //! Linux: one inotify descriptor watching each watched file's directory
    //! (events name the entry, so a replace by rename is heard), and a pipe
    //! that wakes the wait.

    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{FileEvents, Waker, inotify_touched};

    /// `<sys/inotify.h>`'s event bits the generated libc does not carry.
    const IN_MODIFY: u32 = 0x0000_0002;
    const IN_ATTRIB: u32 = 0x0000_0004;
    const IN_CLOSE_WRITE: u32 = 0x0000_0008;

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
            })
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
                inotify_touched(&buffer[..read.min(buffer.len())], &self.keys, touched);
            }
        }
    }

    impl FileEvents for InotifyEvents {
        fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
            let (Some(directory), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            };
            let directory = CString::new(directory.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: a live inotify descriptor and a NUL-terminated path.
            let wd = unsafe {
                libc::inotify_add_watch(self.fd.as_raw_fd(), directory.as_ptr(), DIRECTORY_EVENTS)
            };
            if wd < 0 {
                return Err(io::Error::last_os_error());
            }
            self.keys.insert(key, (wd, name.as_bytes().to_vec()));
            Ok(())
        }

        fn disarm(&mut self, key: u64) {
            let Some((wd, _)) = self.keys.remove(&key) else {
                return;
            };
            if !self.keys.values().any(|(other, _)| *other == wd) {
                // SAFETY: a live inotify descriptor and a watch it returned.
                unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) };
            }
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
            let millis = timeout.map_or(-1, |timeout| {
                libc::c_int::try_from(timeout.as_millis().max(1)).unwrap_or(libc::c_int::MAX)
            });
            // SAFETY: two live pollfd slots.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
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
