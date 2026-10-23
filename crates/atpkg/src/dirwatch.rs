// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A blocking watch over directories and files, for a thread that sleeps until something it reads may have moved: a kqueue of vnode filters on macOS, an inotify instance on Linux, and none elsewhere ([`Watcher::new`] answers `Unsupported`; nothing names a watch on Windows).

use std::io;
use std::path::Path;
use std::time::Duration;

/// One watch of a [`Watcher`]: what [`Watcher::wait`] names when it fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WatchId(u64);

/// Directory and file watches over one kernel queue (module docs).
pub struct Watcher {
    imp: imp::Watcher,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

impl Watcher {
    /// A watcher with no watches.
    /// # Errors
    /// The kernel refused the queue (`EMFILE`), or this platform has no arm (`Unsupported`).
    pub fn new() -> io::Result<Self> {
        imp::Watcher::new().map(|imp| Self { imp })
    }

    /// Watch the directory `dir`'s entries and the directory itself.
    /// # Errors
    /// `dir` cannot be opened or watched, is a symlink, or is not a directory.
    pub fn add_dir(&self, dir: &Path) -> io::Result<WatchId> {
        self.imp.add(dir, Kind::Dir)
    }

    /// Watch the regular file `file`'s contents and the file itself.
    /// # Errors
    /// `file` cannot be opened or watched, is a symlink, or is not a regular file.
    pub fn add_file(&self, file: &Path) -> io::Result<WatchId> {
        self.imp.add(file, Kind::File)
    }

    /// Stop watch `id`.
    pub fn remove(&self, id: WatchId) {
        self.imp.remove(id);
    }

    /// Whether watch `id` will never fire again: its object was removed (kqueue `NOTE_DELETE` or `NOTE_REVOKE`; inotify `IN_IGNORED`, which follows `IN_DELETE_SELF` and an unmount), the kernel's queue overflowed so such an edge may have been lost, or the id names no watch (never added, or removed).
    #[must_use]
    pub fn is_gone(&self, id: WatchId) -> bool {
        self.imp.is_gone(id)
    }

    /// Block until a watch fires, `timeout` passes (`None`: no timeout), or [`Self::wake`] is called; the watches that fired, each once, in id order.
    /// # Errors
    /// The kernel queue failed; the watcher is then of no further use.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<WatchId>> {
        let mut ids = self.imp.wait(timeout)?;
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// End a [`Self::wait`] now, from any thread; given while no wait runs, it ends the next one at once.
    /// # Errors
    /// The kernel refused the trigger.
    pub fn wake(&self) -> io::Result<()> {
        self.imp.wake()
    }

    /// The inotify descriptor (`/proc/self/fdinfo` lists its watches).
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn inotify_fd(&self) -> std::os::fd::RawFd {
        self.imp.inotify_fd()
    }
}

/// Which kind of object a watch is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashMap;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::{Kind, WatchId};

    /// The `EVFILT_USER` identifier [`Watcher::wake`] triggers.
    const BELL: libc::uintptr_t = 0;

    /// How many events one `kevent` call collects.
    const BATCH: usize = 16;

    pub(super) struct Watcher {
        kq: OwnedFd,
        next: AtomicU64,
        /// Each watch not yet removed.
        open: Mutex<HashMap<WatchId, Open>>,
    }

    /// One watch's open descriptor (closing it removes its knote, pending
    /// events included), and whether a wait read its object's removal.
    struct Open {
        #[expect(dead_code, reason = "held for its close, which removes the knote")]
        fd: OwnedFd,
        gone: bool,
    }

    impl Watcher {
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
            let watcher = Self {
                kq,
                next: AtomicU64::new(1),
                open: Mutex::new(HashMap::new()),
            };
            watcher.change(BELL, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0, 0)?;
            Ok(watcher)
        }

        /// Submit one change and collect nothing.
        fn change(
            &self,
            ident: libc::uintptr_t,
            filter: i16,
            flags: u16,
            fflags: u32,
            udata: u64,
        ) -> io::Result<()> {
            let change = libc::kevent {
                ident,
                filter,
                flags,
                fflags,
                data: 0,
                udata: std::ptr::without_provenance_mut(
                    usize::try_from(udata).unwrap_or(usize::MAX),
                ),
            };
            // SAFETY: one initialized change entry, and no output slot.
            let rc = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    &change,
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

        pub(super) fn add(&self, path: &Path, kind: Kind) -> io::Result<WatchId> {
            use std::os::unix::fs::OpenOptionsExt as _;

            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(path)?;
            let meta = file.metadata()?;
            let fflags = match kind {
                Kind::Dir if meta.is_dir() => {
                    libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE
                }
                Kind::File if meta.is_file() => {
                    libc::NOTE_WRITE
                        | libc::NOTE_EXTEND
                        | libc::NOTE_DELETE
                        | libc::NOTE_RENAME
                        | libc::NOTE_REVOKE
                }
                Kind::Dir => return Err(io::Error::from(io::ErrorKind::NotADirectory)),
                Kind::File => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
            };
            let fd = OwnedFd::from(file);
            let id = WatchId(self.next.fetch_add(1, Ordering::Relaxed));
            let mut open = self.open.lock().unwrap_or_else(|p| p.into_inner());
            self.change(
                fd.as_raw_fd() as libc::uintptr_t,
                libc::EVFILT_VNODE,
                libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
                fflags,
                id.0,
            )?;
            open.insert(id, Open { fd, gone: false });
            Ok(id)
        }

        pub(super) fn is_gone(&self, id: WatchId) -> bool {
            self.open
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&id)
                .is_none_or(|open| open.gone)
        }

        pub(super) fn remove(&self, id: WatchId) {
            let open = self
                .open
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            drop(open);
        }

        pub(super) fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<WatchId>> {
            let deadline = timeout.map(|t| Instant::now() + t);
            // SAFETY: kevent initializes the slots it reports before they are read.
            let mut events: [libc::kevent; BATCH] = unsafe { std::mem::zeroed() };
            let got = loop {
                let ts = deadline.map(|at| {
                    let left = at.saturating_duration_since(Instant::now());
                    libc::timespec {
                        tv_sec: libc::time_t::try_from(left.as_secs()).unwrap_or(libc::time_t::MAX),
                        tv_nsec: libc::c_long::from(left.subsec_nanos()),
                    }
                });
                // SAFETY: the timeout (if any) and the output slots live for this call; BATCH slots are writable.
                let rc = unsafe {
                    libc::kevent(
                        self.kq.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        events.as_mut_ptr(),
                        BATCH as libc::c_int,
                        ts.as_ref().map_or(std::ptr::null(), std::ptr::from_ref),
                    )
                };
                if rc >= 0 {
                    break usize::try_from(rc).unwrap_or(0);
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            };
            let mut ids = Vec::with_capacity(got);
            for event in &events[..got] {
                if event.flags & libc::EV_ERROR != 0 {
                    return Err(io::Error::from_raw_os_error(event.data as i32));
                }
                if event.filter != libc::EVFILT_VNODE {
                    continue;
                }
                let id = WatchId(event.udata.addr() as u64);
                if event.fflags & (libc::NOTE_DELETE | libc::NOTE_REVOKE) != 0
                    && let Some(open) = self
                        .open
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get_mut(&id)
                {
                    // again; a new object at the path is another vnode.
                    open.gone = true;
                }
                ids.push(id);
            }
            Ok(ids)
        }

        pub(super) fn wake(&self) -> io::Result<()> {
            self.change(BELL, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, 0)
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::{HashMap, HashSet};
    use std::io::{self, Read as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::{Kind, WatchId};

    const IN_MODIFY: u32 = 0x0000_0002;
    const IN_CLOSE_WRITE: u32 = 0x0000_0008;
    const IN_Q_OVERFLOW: u32 = 0x0000_4000;
    const IN_IGNORED: u32 = 0x0000_8000;

    /// The directory edges the macOS arm's `NOTE_WRITE`, `NOTE_DELETE` and
    /// `NOTE_RENAME` report.
    const DIR_MASK: u32 = libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_ONLYDIR
        | libc::IN_DONT_FOLLOW;

    /// The file edges the macOS arm's `NOTE_WRITE`, `NOTE_EXTEND`,
    /// `NOTE_DELETE` and `NOTE_RENAME` report.
    const FILE_MASK: u32 = IN_MODIFY
        | IN_CLOSE_WRITE
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_DONT_FOLLOW;

    /// The fixed head of a `struct inotify_event`: `wd`, `mask`, `cookie`,
    /// `len`, then `len` bytes of name.
    const HEAD: usize = 16;

    pub(super) struct Watcher {
        inotify: std::fs::File,
        /// The eventfd [`Watcher::wake`] writes.
        bell: std::fs::File,
        next: AtomicU64,
        wds: Mutex<Wds>,
    }

    /// The kernel's watch descriptors against ours: one inode watched twice
    /// shares one descriptor, taken off when its last watch goes.
    #[derive(Default)]
    struct Wds {
        of_id: HashMap<WatchId, libc::c_int>,
        ids: HashMap<libc::c_int, Vec<WatchId>>,
        /// The watches the kernel took off (`IN_IGNORED`), or may have (an
        /// overflow); each leaves with its removal.
        gone: HashSet<WatchId>,
    }

    impl Watcher {
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: inotify_init1 creates a new descriptor owned by this process.
            let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is fresh and exclusively ours.
            let inotify = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            // SAFETY: eventfd creates a new descriptor owned by this process.
            let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is fresh and exclusively ours.
            let bell = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            Ok(Self {
                inotify,
                bell,
                next: AtomicU64::new(1),
                wds: Mutex::new(Wds::default()),
            })
        }

        #[cfg(test)]
        pub(super) fn inotify_fd(&self) -> std::os::fd::RawFd {
            self.inotify.as_raw_fd()
        }

        pub(super) fn add(&self, path: &Path, kind: Kind) -> io::Result<WatchId> {
            use std::os::unix::ffi::OsStrExt as _;

            let mask = match kind {
                Kind::Dir => DIR_MASK,
                Kind::File => {
                    // than refuse it; the kind is checked on the link.
                    if !std::fs::symlink_metadata(path)?.is_file() {
                        return Err(io::Error::from(io::ErrorKind::InvalidInput));
                    }
                    FILE_MASK
                }
            };
            let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            let mut wds = self.wds.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: `cpath` is NUL-terminated and outlives the call.
            let wd =
                unsafe { libc::inotify_add_watch(self.inotify.as_raw_fd(), cpath.as_ptr(), mask) };
            if wd < 0 {
                return Err(io::Error::last_os_error());
            }
            let id = WatchId(self.next.fetch_add(1, Ordering::Relaxed));
            wds.of_id.insert(id, wd);
            wds.ids.entry(wd).or_default().push(id);
            Ok(id)
        }

        pub(super) fn is_gone(&self, id: WatchId) -> bool {
            let wds = self.wds.lock().unwrap_or_else(|p| p.into_inner());
            !wds.of_id.contains_key(&id) || wds.gone.contains(&id)
        }

        pub(super) fn remove(&self, id: WatchId) {
            let mut wds = self.wds.lock().unwrap_or_else(|p| p.into_inner());
            wds.gone.remove(&id);
            let Some(wd) = wds.of_id.remove(&id) else {
                return;
            };
            let Some(ids) = wds.ids.get_mut(&wd) else {
                // The kernel already dropped it (its object removed).
                return;
            };
            ids.retain(|other| *other != id);
            if ids.is_empty() {
                wds.ids.remove(&wd);
                // SAFETY: `wd` was returned for this descriptor, which is open.
                unsafe { libc::inotify_rm_watch(self.inotify.as_raw_fd(), wd) };
            }
        }

        pub(super) fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<WatchId>> {
            let deadline = timeout.map(|t| Instant::now() + t);
            let mut fds = [
                libc::pollfd {
                    fd: self.inotify.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.bell.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            loop {
                let ms = deadline.map_or(-1, |at| {
                    let left = at.saturating_duration_since(Instant::now());
                    libc::c_int::try_from(left.as_nanos().div_ceil(1_000_000))
                        .unwrap_or(libc::c_int::MAX)
                });
                // SAFETY: two initialized pollfds that live for this call.
                let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
                if rc >= 0 {
                    break;
                }
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
            if fds[1].revents & libc::POLLIN != 0 {
                let mut count = [0u8; 8];
                let _ = (&self.bell).read(&mut count);
            }
            let mut ids = Vec::new();
            if fds[0].revents != 0 {
                if fds[0].revents & libc::POLLIN == 0 {
                    return Err(io::Error::other("the inotify descriptor failed"));
                }
                self.drain(&mut ids)?;
            }
            Ok(ids)
        }

        /// Read every queued event, naming the watches they are for.
        fn drain(&self, ids: &mut Vec<WatchId>) -> io::Result<()> {
            let mut buf = [0u8; 4096];
            loop {
                let got = match (&self.inotify).read(&mut buf) {
                    Ok(got) => got,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };
                if got == 0 {
                    return Ok(());
                }
                let mut wds = self.wds.lock().unwrap_or_else(|p| p.into_inner());
                let mut at = 0;
                while at + HEAD <= got {
                    let word = |o: usize| {
                        let mut b = [0u8; 4];
                        b.copy_from_slice(&buf[at + o..at + o + 4]);
                        b
                    };
                    let wd = libc::c_int::from_ne_bytes(word(0));
                    let mask = u32::from_ne_bytes(word(4));
                    let len = usize::try_from(u32::from_ne_bytes(word(12))).unwrap_or(usize::MAX);
                    at = at.saturating_add(HEAD).saturating_add(len);
                    if mask & IN_Q_OVERFLOW != 0 {
                        let all: Vec<WatchId> = wds.of_id.keys().copied().collect();
                        ids.extend(&all);
                        wds.gone.extend(all);
                        continue;
                    }
                    if let Some(of_wd) = wds.ids.get(&wd) {
                        ids.extend(of_wd.iter().copied());
                    }
                    if mask & IN_IGNORED != 0 {
                        // descriptor's number.
                        if let Some(of_wd) = wds.ids.remove(&wd) {
                            wds.gone.extend(of_wd);
                        }
                    }
                }
            }
        }

        pub(super) fn wake(&self) -> io::Result<()> {
            use std::io::Write as _;

            match (&self.bell).write(&1u64.to_ne_bytes()) {
                Ok(_) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
                Err(e) => Err(e),
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use super::{Kind, WatchId};

    /// No arm: no watcher can be made.
    pub(super) struct Watcher(std::convert::Infallible);

    impl Watcher {
        pub(super) fn new() -> io::Result<Self> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }

        pub(super) fn add(&self, _path: &Path, _kind: Kind) -> io::Result<WatchId> {
            match self.0 {}
        }

        pub(super) fn remove(&self, _id: WatchId) {
            match self.0 {}
        }

        pub(super) fn is_gone(&self, _id: WatchId) -> bool {
            match self.0 {}
        }

        pub(super) fn wait(&self, _timeout: Option<Duration>) -> io::Result<Vec<WatchId>> {
            match self.0 {}
        }

        pub(super) fn wake(&self) -> io::Result<()> {
            match self.0 {}
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    /// No wait: whatever the kernel queued, now.
    const NOW: Option<Duration> = Some(Duration::ZERO);

    /// An entry renamed into one watched directory names that directory's
    /// watch and not another's; a removal and the directory's own removal
    /// name it too.
    #[test]
    fn a_directory_entry_names_its_watch_alone() {
        let root = crate::testdir::dir("atpkg-dirwatch-dir-");
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let watcher = Watcher::new().unwrap();
        let wa = watcher.add_dir(&a).unwrap();
        let wb = watcher.add_dir(&b).unwrap();
        assert_ne!(wa, wb);
        assert!(
            watcher.wait(NOW).unwrap().is_empty(),
            "CONTROL: nothing yet"
        );
        std::fs::write(root.join("staged"), b"x").unwrap();
        std::fs::rename(root.join("staged"), a.join("row")).unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [wa]);
        std::fs::remove_file(a.join("row")).unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [wa]);
        std::fs::remove_dir(&b).unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [wb], "the directory itself");
    }

    /// A file written in place names the file's watch and NOT its directory's:
    /// the edge a directory watch alone misses (Claude Code creates its
    /// registry row, then writes it).
    #[test]
    fn a_file_written_in_place_names_the_file_and_not_its_directory() {
        use std::io::Write as _;

        let root = crate::testdir::dir("atpkg-dirwatch-file-");
        let row = root.join("4242.json");
        std::fs::write(&row, b"").unwrap();
        let watcher = Watcher::new().unwrap();
        let dir = watcher.add_dir(&root).unwrap();
        let file = watcher.add_file(&row).unwrap();
        let mut out = std::fs::OpenOptions::new().append(true).open(&row).unwrap();
        out.write_all(b"{\"pid\":4242}").unwrap();
        drop(out);
        assert_eq!(watcher.wait(NOW).unwrap(), [file], "not {dir:?}");
        let mut out = std::fs::OpenOptions::new().append(true).open(&row).unwrap();
        out.write_all(b"\n").unwrap();
        drop(out);
        assert_eq!(watcher.wait(NOW).unwrap(), [file]);
    }

    /// The edge after the registration is kept for a wait that starts later;
    /// the one before it is not (register first, then read).
    #[test]
    fn an_edge_after_registration_is_kept_for_a_later_wait() {
        let root = crate::testdir::dir("atpkg-dirwatch-kept-");
        std::fs::write(root.join("before"), b"").unwrap();
        let watcher = Watcher::new().unwrap();
        let id = watcher.add_dir(&root).unwrap();
        assert!(
            watcher.wait(NOW).unwrap().is_empty(),
            "before: not reported"
        );
        std::fs::write(root.join("after"), b"").unwrap();
        assert_eq!(
            watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
            [id]
        );
    }

    /// A removed watch names nothing, the watcher's others still do, and its
    /// id is never handed out again.
    #[test]
    fn a_removed_watch_names_nothing() {
        let root = crate::testdir::dir("atpkg-dirwatch-removed-");
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let watcher = Watcher::new().unwrap();
        let wa = watcher.add_dir(&a).unwrap();
        let wb = watcher.add_dir(&b).unwrap();
        watcher.remove(wa);
        watcher.remove(wa);
        std::fs::write(a.join("x"), b"").unwrap();
        std::fs::write(b.join("x"), b"").unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [wb]);
        let again = watcher.add_dir(&a).unwrap();
        assert!(again != wa && again != wb, "ids are never reused");
        std::fs::write(a.join("y"), b"").unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [again], "CONTROL: kept");
    }

    /// A watch whose object is removed fires once and is then gone, and
    /// stays gone when the path is made again (another object, which on Linux
    /// may carry the old inode number); a removed watch, and an id never
    /// handed out, are gone too.
    #[test]
    fn a_watch_whose_object_is_removed_is_gone() {
        let root = crate::testdir::dir("atpkg-dirwatch-gone-");
        let (dir, kept, row) = (root.join("dir"), root.join("kept"), root.join("row"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&kept).unwrap();
        std::fs::write(&row, b"").unwrap();
        let watcher = Watcher::new().unwrap();
        let wd = watcher.add_dir(&dir).unwrap();
        let wk = watcher.add_dir(&kept).unwrap();
        let wf = watcher.add_file(&row).unwrap();
        assert!(!watcher.is_gone(wd) && !watcher.is_gone(wk) && !watcher.is_gone(wf));
        std::fs::remove_dir(&dir).unwrap();
        std::fs::write(kept.join("x"), b"").unwrap();
        std::fs::remove_file(&row).unwrap();
        assert_eq!(
            watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
            [wd, wk, wf]
        );
        assert!(watcher.is_gone(wd), "the removed directory");
        assert!(watcher.is_gone(wf), "the removed file");
        assert!(
            !watcher.is_gone(wk),
            "CONTROL: an entry changed, not the object"
        );
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("y"), b"").unwrap();
        assert!(
            watcher.wait(NOW).unwrap().is_empty(),
            "the new directory is not watched"
        );
        assert!(watcher.is_gone(wd));
        watcher.remove(wk);
        assert!(watcher.is_gone(wk), "a removed watch");
        assert!(watcher.is_gone(WatchId(u64::MAX)), "an id never handed out");
    }

    /// One directory watched twice is two watches: removing one leaves the
    /// other live (inotify gives both one kernel descriptor).
    #[test]
    fn one_directory_watched_twice_keeps_the_other_watch() {
        let root = crate::testdir::dir("atpkg-dirwatch-twice-");
        let watcher = Watcher::new().unwrap();
        let first = watcher.add_dir(&root).unwrap();
        let second = watcher.add_dir(&root).unwrap();
        std::fs::write(root.join("x"), b"").unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [first, second]);
        watcher.remove(first);
        std::fs::write(root.join("y"), b"").unwrap();
        assert_eq!(watcher.wait(NOW).unwrap(), [second]);
    }

    /// A wake ends a wait another thread is blocked in, and one given while
    /// no wait runs ends the next at once; neither names a watch.
    #[test]
    fn a_wake_ends_the_wait() {
        let watcher = std::sync::Arc::new(Watcher::new().unwrap());
        let waiting = || {
            let (tx, rx) = std::sync::mpsc::channel();
            let watcher = std::sync::Arc::clone(&watcher);
            std::thread::spawn(move || tx.send(watcher.wait(None).unwrap()));
            rx
        };
        watcher.wake().unwrap();
        let kept = waiting()
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("a wake given before the wait ends it");
        assert!(kept.is_empty());
        let rx = waiting();
        watcher.wake().unwrap();
        let woke = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the woken wait returns");
        assert!(woke.is_empty());
    }

    /// A watch is on what the caller named: a symlink is refused (its target
    /// is not watched through it), and each kind refuses the other.
    #[test]
    fn a_symlink_or_the_wrong_kind_is_refused() {
        let root = crate::testdir::dir("atpkg-dirwatch-kind-");
        let (dir, file) = (root.join("dir"), root.join("file"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(&file, b"").unwrap();
        std::os::unix::fs::symlink(&dir, root.join("dir-link")).unwrap();
        std::os::unix::fs::symlink(&file, root.join("file-link")).unwrap();
        let watcher = Watcher::new().unwrap();
        assert!(watcher.add_dir(&root.join("dir-link")).is_err());
        assert!(watcher.add_file(&root.join("file-link")).is_err());
        assert!(watcher.add_dir(&file).is_err());
        assert!(watcher.add_file(&dir).is_err());
        assert!(watcher.add_dir(&root.join("missing")).is_err());
        assert!(watcher.add_dir(&dir).is_ok() && watcher.add_file(&file).is_ok());
    }
}
