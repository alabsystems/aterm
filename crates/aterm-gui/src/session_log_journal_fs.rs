// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Filesystem operations for the session journal behind [`JournalFs`].

use std::io;

use aterm_slog::journal::{Inst, Liveness};

/// What the journal knows of one entry, read through an open no-follow
/// descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    /// Bytes.
    pub(crate) size: u64,
    /// Last modified, Unix ms.
    pub(crate) mtime_ms: u64,
    /// Identity: device and inode.
    pub(crate) dev: u64,
    /// See [`Meta::dev`].
    pub(crate) ino: u64,
    /// A regular file with one link, this user's, no group or other bits:
    /// the only kind a journal reads or removes.
    pub(crate) private: bool,
}

/// One listing of the directory ([`list`]).
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Listing {
    /// The names, in directory order.
    pub(crate) names: Vec<String>,
    /// The directory held more names than the listing's bound: some are
    /// missing, and nothing may be concluded from one's absence.
    pub(crate) truncated: bool,
}

/// A lock this process holds; dropping it lets go.
pub(crate) trait Held: Send {
    /// The held file's identity, for removing exactly it.
    #[cfg(unix)]
    fn identity(&self) -> Option<(u64, u64)>;
}

/// A segment this process appends to.
pub(crate) trait Appender: Send {
    /// One `write(2)` of `bytes`: how many were written.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize>;
    /// Flush to the disk.
    fn sync(&mut self) -> io::Result<()>;
}

/// The journal's directory.
pub(crate) trait JournalFs: Send + Sync {
    /// Lock `inst` for its journal's life.
    fn lock_instance(&self, inst: &Inst) -> io::Result<Box<dyn Held>>;
    /// The clean end: remove `inst`'s lock file (exactly the one `held`
    /// holds), then let go.
    fn unlock_instance(&self, inst: &Inst, held: Box<dyn Held>);
    /// Create a new segment `name`.
    fn create_segment(&self, name: &str) -> io::Result<Box<dyn Appender>>;
    /// Visit at most `max` of the directory's names (those that are UTF-8),
    /// in directory order, holding none: `true` when some were left
    /// unvisited.
    fn scan(&self, max: usize, visit: &mut dyn FnMut(&str)) -> io::Result<bool>;
    /// `name`'s metadata (no follow).
    fn meta(&self, name: &str) -> io::Result<Meta>;
    /// At most `max` bytes of `name` from offset `from` (a private file
    /// only), and its metadata.
    #[cfg(unix)]
    fn read(&self, name: &str, from: u64, max: u64) -> io::Result<(Vec<u8>, Meta)>;
    /// Remove `name` iff it is still the private file `expect` names.
    fn remove(&self, name: &str, expect: Meta) -> io::Result<()>;
    /// Whether the lock file `name`'s holder lives.
    fn probe(&self, name: &str) -> Liveness;
    /// The retention pass's lock (`aterm_slog::journal::PRUNE_LOCK`), or
    /// `None` while another holds it.
    fn try_prune_lock(&self) -> io::Result<Option<Box<dyn Held>>>;
    /// Flush the directory.
    fn sync_dir(&self) -> io::Result<()>;
    /// Free bytes on the directory's volume, if known.
    fn free_bytes(&self) -> Option<u64>;
}

/// The directory's names, at most `aterm_slog::journal::LIST_MAX`, and
/// whether that bound cut the listing.
#[cfg(unix)]
pub(crate) fn list(fs: &dyn JournalFs) -> io::Result<Listing> {
    let mut names = Vec::new();
    let truncated = fs.scan(aterm_slog::journal::LIST_MAX, &mut |n| {
        names.push(n.to_string());
    })?;
    Ok(Listing { names, truncated })
}

#[cfg(unix)]
pub(crate) use real::PinnedJournalFs;

#[cfg(unix)]
mod real {
    use std::ffi::OsStr;
    use std::io::{self, Read as _, Seek as _, Write as _};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    use aterm_slog::journal::{Inst, Liveness, PRUNE_LOCK, lock_name, pending_lock_name};

    use super::{Appender, Held, JournalFs, Meta};
    use crate::native_document_host::HeldAdvisoryLock;
    use crate::pinned_dir::PinnedDir;

    /// The journal's directory's name under the logs directory.
    pub(crate) const DIR_NAME: &str = "session-log";

    fn euid() -> u32 {
        // SAFETY: `geteuid` is a side-effect-free libc getter.
        unsafe { libc::geteuid() }
    }

    fn meta_of(md: &std::fs::Metadata, euid: u32) -> Meta {
        let mtime_ms = u64::try_from(md.mtime())
            .unwrap_or(0)
            .saturating_mul(1000)
            .saturating_add(u64::try_from(md.mtime_nsec()).unwrap_or(0) / 1_000_000);
        Meta {
            size: md.size(),
            mtime_ms,
            dev: md.dev(),
            ino: md.ino(),
            private: md.is_file() && md.nlink() == 1 && md.uid() == euid && md.mode() & 0o077 == 0,
        }
    }

    struct Lock(HeldAdvisoryLock);

    impl Held for Lock {
        fn identity(&self) -> Option<(u64, u64)> {
            let md = self.0.file().metadata().ok()?;
            Some((md.dev(), md.ino()))
        }
    }

    struct Segment(std::fs::File);

    impl Appender for Segment {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.write(bytes)
        }

        fn sync(&mut self) -> io::Result<()> {
            aterm_durable::ordered(&self.0)
        }
    }

    fn take_lock(file: std::fs::File) -> io::Result<HeldAdvisoryLock> {
        HeldAdvisoryLock::try_lock(file).map_err(|refusal| match refusal {
            std::fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
            std::fs::TryLockError::Error(error) => error,
        })
    }

    /// The journal's directory, pinned.
    pub(crate) struct PinnedJournalFs {
        dir: PinnedDir,
        euid: u32,
    }

    impl PinnedJournalFs {
        /// `<logs>/session-log`, pinned.
        pub(crate) fn open(logs: &Path, create: bool) -> io::Result<Option<PinnedJournalFs>> {
            let logs = match PinnedDir::open_resolved(logs) {
                Ok(dir) => dir,
                Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                    std::fs::create_dir_all(logs)?;
                    PinnedDir::open_resolved(logs)?
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let name = OsStr::new(DIR_NAME);
            let dir = match logs.child(name) {
                Ok(dir) => dir,
                Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                    logs.create_child(name)?
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let euid = euid();
            let md = dir.leaf_metadata()?;
            if md.uid() != euid {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the session journal's directory belongs to another user",
                ));
            }
            if create && md.mode() & 0o077 != 0 {
                dir.restrict_to_owner()?;
            }
            Ok(Some(PinnedJournalFs { dir, euid }))
        }
    }

    impl JournalFs for PinnedJournalFs {
        fn lock_instance(&self, inst: &Inst) -> io::Result<Box<dyn Held>> {
            let pending = pending_lock_name(inst);
            let pending = OsStr::new(&pending);
            let file = self.dir.create_new_private_rw(pending)?;
            let held = match take_lock(file) {
                Ok(held) => held,
                Err(error) => {
                    let _ = self.dir.remove_file_if_exists(pending);
                    return Err(error);
                }
            };
            if let Err(error) = self
                .dir
                .rename_no_replace(pending, OsStr::new(&lock_name(inst)))
            {
                drop(held);
                let _ = self.dir.remove_file_if_exists(pending);
                return Err(error);
            }
            // Accepted, failed or not: the lock is held whatever its name's flush says, and a
            // name a power cut takes back names a lock no one holds after the reboot anyway.
            let _named = self.dir.sync_ordered();
            Ok(Box::new(Lock(held)))
        }

        fn unlock_instance(&self, inst: &Inst, held: Box<dyn Held>) {
            let name = lock_name(inst);
            if let (Some((dev, ino)), Ok(md)) = (held.identity(), self.meta(&name))
                && md.dev == dev
                && md.ino == ino
            {
                let _ = self.remove(&name, md);
            }
            drop(held);
        }

        fn create_segment(&self, name: &str) -> io::Result<Box<dyn Appender>> {
            Ok(Box::new(Segment(
                self.dir.create_new_append_private(OsStr::new(name))?,
            )))
        }

        fn scan(&self, max: usize, visit: &mut dyn FnMut(&str)) -> io::Result<bool> {
            self.dir.visit_names(max, &mut |name| {
                if let Some(name) = name.to_str() {
                    visit(name);
                }
            })
        }

        fn meta(&self, name: &str) -> io::Result<Meta> {
            let file = self.dir.open_regular_at_retained(OsStr::new(name))?;
            Ok(meta_of(&file.metadata()?, self.euid))
        }

        fn read(&self, name: &str, from: u64, max: u64) -> io::Result<(Vec<u8>, Meta)> {
            let mut file = self.dir.open_regular_at_retained(OsStr::new(name))?;
            let meta = meta_of(&file.metadata()?, self.euid);
            if !meta.private {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "not a private journal file",
                ));
            }
            file.seek(io::SeekFrom::Start(from))?;
            let mut bytes = Vec::with_capacity(usize::try_from(max.min(1 << 20)).unwrap_or(0));
            file.take(max).read_to_end(&mut bytes)?;
            Ok((bytes, meta))
        }

        fn remove(&self, name: &str, expect: Meta) -> io::Result<()> {
            let pinned = self.dir.pin_private_file_at_retained(OsStr::new(name))?;
            let now = meta_of(&pinned.metadata()?, self.euid);
            if !now.private || now.dev != expect.dev || now.ino != expect.ino {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the entry is not the private journal file listed",
                ));
            }
            pinned.remove_exact()
        }

        fn probe(&self, name: &str) -> Liveness {
            let file = match self.dir.open_regular_at_retained(OsStr::new(name)) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Liveness::Dead,
                Err(_) => return Liveness::Unknown,
            };
            if !file.metadata().is_ok_and(|md| md.uid() == self.euid) {
                return Liveness::Unknown;
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "a probe: released by `unlock` below before the close"
            )]
            let taken = file.try_lock_shared();
            match taken {
                Ok(()) => {
                    let _ = file.unlock();
                    Liveness::Dead
                }
                Err(std::fs::TryLockError::WouldBlock) => Liveness::Live,
                Err(std::fs::TryLockError::Error(_)) => Liveness::Unknown,
            }
        }

        fn try_prune_lock(&self) -> io::Result<Option<Box<dyn Held>>> {
            let file = self.dir.open_or_create_private_rw(OsStr::new(PRUNE_LOCK))?;
            let md = file.metadata()?;
            if md.uid() != self.euid || md.nlink() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the retention lock is not this user's own file",
                ));
            }
            match take_lock(file) {
                Ok(held) => Ok(Some(Box::new(Lock(held)))),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(error) => Err(error),
            }
        }

        fn sync_dir(&self) -> io::Result<()> {
            self.dir.sync_ordered()
        }

        fn free_bytes(&self) -> Option<u64> {
            aterm_agent::harness::disk::free_bytes(self.dir.path())
        }
    }
}

/// An in-memory journal directory with fault injection for tests.
#[cfg(test)]
#[cfg(unix)]
pub(crate) mod mem {
    use std::collections::BTreeMap;
    use std::io;
    use std::sync::{Arc, Mutex};

    use aterm_slog::journal::{Inst, Liveness, PRUNE_LOCK, lock_name, pending_lock_name};

    use super::{Appender, Held, JournalFs, Meta};

    /// One entry.
    #[derive(Clone, Debug)]
    pub(crate) struct Entry {
        pub(crate) bytes: Vec<u8>,
        pub(crate) ino: u64,
        pub(crate) mtime_ms: u64,
        /// Not a private file of this user (a foreign or hard-linked one).
        pub(crate) foreign: bool,
        /// A symlink: every open of it fails (`ELOOP`).
        pub(crate) symlink: bool,
    }

    /// A fault the next write meets.
    #[derive(Clone, Debug)]
    pub(crate) enum Fault {
        /// Writes after `n` more bytes fail with this errno (ENOSPC, EIO…);
        /// a write straddling the limit is short.
        After(u64, i32),
        /// Every write fails with this errno until cleared.
        Always(i32),
        /// The next `n` writes fail with EINTR.
        Eintr(u32),
        /// Writes block until the barrier is released.
        Block(Arc<(Mutex<bool>, std::sync::Condvar)>),
        /// The next write panics.
        Panic,
    }

    #[derive(Default)]
    pub(crate) struct State {
        pub(crate) files: BTreeMap<String, Entry>,
        pub(crate) locks: BTreeMap<String, usize>,
        pub(crate) next_ino: u64,
        pub(crate) clock_ms: u64,
        pub(crate) fault: Option<Fault>,
        pub(crate) writes: u64,
        /// Writes begun (counted before a barrier blocks one).
        pub(crate) entered: u64,
        pub(crate) free: Option<u64>,
        pub(crate) no_flock: bool,
        pub(crate) creates_refused: Option<i32>,
        /// Every flush (a segment's, the directory's) fails with this errno until cleared.
        pub(crate) flush_fault: Option<i32>,
        /// Names a scan visits after every other (a real directory's order
        /// is its own: a test puts a name past a listing's bound).
        pub(crate) listed_last: std::collections::BTreeSet<String>,
        /// Segments created.
        pub(crate) created: u64,
    }

    /// The in-memory directory, shareable across "processes" (two writers
    /// and a reader in one test).
    #[derive(Clone, Default)]
    pub(crate) struct MemFs(pub(crate) Arc<Mutex<State>>);

    fn err(errno: i32) -> io::Error {
        io::Error::from_raw_os_error(errno)
    }

    impl MemFs {
        pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap_or_else(|p| p.into_inner())
        }

        pub(crate) fn names(&self) -> Vec<String> {
            self.state().files.keys().cloned().collect()
        }

        pub(crate) fn bytes(&self, name: &str) -> Vec<u8> {
            self.state()
                .files
                .get(name)
                .map(|e| e.bytes.clone())
                .unwrap_or_default()
        }

        /// Every byte of every file: what a leak test greps.
        pub(crate) fn all_bytes(&self) -> Vec<u8> {
            self.state()
                .files
                .values()
                .flat_map(|e| e.bytes.clone())
                .collect()
        }

        pub(crate) fn put(&self, name: &str, bytes: &[u8], foreign: bool, symlink: bool) {
            let mut s = self.state();
            s.next_ino += 1;
            let (ino, mtime_ms) = (s.next_ino, s.clock_ms);
            s.files.insert(
                name.to_string(),
                Entry {
                    bytes: bytes.to_vec(),
                    ino,
                    mtime_ms,
                    foreign,
                    symlink,
                },
            );
        }

        pub(crate) fn fault(&self, f: Option<Fault>) {
            self.state().fault = f;
        }

        pub(crate) fn tick(&self, ms: u64) {
            self.state().clock_ms += ms;
        }
    }

    struct MemLock {
        fs: MemFs,
        name: String,
        ino: u64,
    }

    impl Held for MemLock {
        fn identity(&self) -> Option<(u64, u64)> {
            Some((1, self.ino))
        }
    }

    impl Drop for MemLock {
        fn drop(&mut self) {
            let mut s = self.fs.state();
            if let Some(n) = s.locks.get_mut(&self.name) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    s.locks.remove(&self.name);
                }
            }
        }
    }

    struct MemSeg {
        fs: MemFs,
        name: String,
    }

    impl Appender for MemSeg {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let fault = {
                let mut s = self.fs.state();
                s.entered += 1;
                s.fault.clone()
            };
            if let Some(Fault::Block(barrier)) = &fault {
                let (lock, cv) = &**barrier;
                let mut open = lock.lock().unwrap_or_else(|p| p.into_inner());
                while !*open {
                    open = cv.wait(open).unwrap_or_else(|p| p.into_inner());
                }
            }
            let mut s = self.fs.state();
            s.writes += 1;
            let n = match s.fault.clone() {
                Some(Fault::Panic) => {
                    s.fault = None;
                    drop(s);
                    panic!("an injected panic in the journal's write");
                }
                Some(Fault::Always(e)) => return Err(err(e)),
                Some(Fault::Eintr(k)) => {
                    s.fault = if k > 1 {
                        Some(Fault::Eintr(k - 1))
                    } else {
                        None
                    };
                    return Err(err(libc::EINTR));
                }
                Some(Fault::After(room, e)) => {
                    if room == 0 {
                        return Err(err(e));
                    }
                    let n = usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len());
                    s.fault = Some(Fault::After(room - n as u64, e));
                    n
                }
                Some(Fault::Block(_)) | None => bytes.len(),
            };
            let clock = s.clock_ms;
            let e = s
                .files
                .get_mut(&self.name)
                .ok_or_else(|| err(libc::ENOENT))?;
            e.bytes.extend_from_slice(&bytes[..n]);
            e.mtime_ms = clock;
            Ok(n)
        }

        fn sync(&mut self) -> io::Result<()> {
            self.fs.state().flush_fault.map_or(Ok(()), |e| Err(err(e)))
        }
    }

    impl JournalFs for MemFs {
        fn lock_instance(&self, inst: &Inst) -> io::Result<Box<dyn Held>> {
            if self.state().no_flock {
                return Err(err(libc::ENOTSUP));
            }
            let name = lock_name(inst);
            let _ = pending_lock_name(inst);
            if self.state().files.contains_key(&name) {
                return Err(err(libc::EEXIST));
            }
            self.put(&name, b"", false, false);
            let ino = self.state().files.get(&name).map_or(0, |e| e.ino);
            *self.state().locks.entry(name.clone()).or_insert(0) += 1;
            Ok(Box::new(MemLock {
                fs: self.clone(),
                name,
                ino,
            }))
        }

        fn unlock_instance(&self, inst: &Inst, held: Box<dyn Held>) {
            self.state().files.remove(&lock_name(inst));
            drop(held);
        }

        fn create_segment(&self, name: &str) -> io::Result<Box<dyn Appender>> {
            if let Some(e) = self.state().creates_refused {
                return Err(err(e));
            }
            if self.state().files.contains_key(name) {
                return Err(err(libc::EEXIST));
            }
            self.put(name, b"", false, false);
            self.state().created += 1;
            Ok(Box::new(MemSeg {
                fs: self.clone(),
                name: name.to_string(),
            }))
        }

        fn scan(&self, max: usize, visit: &mut dyn FnMut(&str)) -> io::Result<bool> {
            let (mut names, last): (Vec<String>, Vec<String>) = {
                let s = self.state();
                s.files
                    .keys()
                    .cloned()
                    .partition(|n| !s.listed_last.contains(n))
            };
            names.extend(last);
            for (i, name) in names.iter().enumerate() {
                if i == max {
                    return Ok(true);
                }
                visit(name);
            }
            Ok(false)
        }

        fn meta(&self, name: &str) -> io::Result<Meta> {
            let s = self.state();
            let e = s.files.get(name).ok_or_else(|| err(libc::ENOENT))?;
            if e.symlink {
                return Err(err(libc::ELOOP));
            }
            Ok(Meta {
                size: e.bytes.len() as u64,
                mtime_ms: e.mtime_ms,
                dev: 1,
                ino: e.ino,
                private: !e.foreign,
            })
        }

        fn read(&self, name: &str, from: u64, max: u64) -> io::Result<(Vec<u8>, Meta)> {
            let meta = self.meta(name)?;
            if !meta.private {
                return Err(err(libc::EACCES));
            }
            let s = self.state();
            let e = s.files.get(name).ok_or_else(|| err(libc::ENOENT))?;
            let from = usize::try_from(from)
                .unwrap_or(usize::MAX)
                .min(e.bytes.len());
            let to = from
                .saturating_add(usize::try_from(max).unwrap_or(usize::MAX))
                .min(e.bytes.len());
            Ok((e.bytes[from..to].to_vec(), meta))
        }

        fn remove(&self, name: &str, expect: Meta) -> io::Result<()> {
            let now = self.meta(name)?;
            if !now.private || now.ino != expect.ino {
                return Err(err(libc::EACCES));
            }
            self.state().files.remove(name);
            Ok(())
        }

        fn probe(&self, name: &str) -> Liveness {
            let s = self.state();
            match s.files.get(name) {
                None => Liveness::Dead,
                Some(e) if e.symlink || e.foreign => Liveness::Unknown,
                Some(_) if s.locks.contains_key(name) => Liveness::Live,
                Some(_) => Liveness::Dead,
            }
        }

        fn try_prune_lock(&self) -> io::Result<Option<Box<dyn Held>>> {
            let mut s = self.state();
            if s.locks.contains_key(PRUNE_LOCK) {
                return Ok(None);
            }
            s.locks.insert(PRUNE_LOCK.to_string(), 1);
            drop(s);
            Ok(Some(Box::new(MemLock {
                fs: self.clone(),
                name: PRUNE_LOCK.to_string(),
                ino: 0,
            })))
        }

        fn sync_dir(&self) -> io::Result<()> {
            self.state().flush_fault.map_or(Ok(()), |e| Err(err(e)))
        }

        fn free_bytes(&self) -> Option<u64> {
            self.state().free
        }
    }
}
