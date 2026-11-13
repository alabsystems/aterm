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
            let (_, md) = self.dir.open_regular_at_retained(OsStr::new(name))?;
            Ok(meta_of(&md, self.euid))
        }

        fn read(&self, name: &str, from: u64, max: u64) -> io::Result<(Vec<u8>, Meta)> {
            let (mut file, md) = self.dir.open_regular_at_retained(OsStr::new(name))?;
            let meta = meta_of(&md, self.euid);
            if !meta.private {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "not a private journal file",
                ));
            }
            file.seek(io::SeekFrom::Start(from))?;
            let avail = meta.size.saturating_sub(from).min(max);
            let cap = usize::try_from(avail.min(1 << 20).saturating_add(1)).unwrap_or(0);
            let mut bytes = Vec::with_capacity(cap);
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
            let (file, md) = match self.dir.open_regular_at_retained(OsStr::new(name)) {
                Ok(opened) => opened,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Liveness::Dead,
                Err(_) => return Liveness::Unknown,
            };
            if md.uid() != self.euid {
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
#[path = "session_log_journal_fs_mem_tests.rs"]
pub(crate) mod mem;
