// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

use std::ffi::{CStr, CString, OsStr};
use std::fs::{DirBuilder, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{DIRECTORY_NAME, MAX_RECORD_BYTES, MAX_SLOTS, Owner, Record, SlotState, invalid};

/// Native per-account capacity directory, independent of a scratch `HOME`.
/// The account database is the one authority for the Unix home directory.
pub fn capacity_dir() -> io::Result<PathBuf> {
    // SAFETY: getuid has no arguments and only reads this process's real uid.
    let uid = unsafe { libc::getuid() };
    let mut bytes = vec![0u8; 4096];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::zeroed();
        let mut found = std::ptr::null_mut();
        // SAFETY: entry, found and bytes are writable for their declared sizes;
        // successful pointers are read only while this call's buffer is alive.
        let result = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &raw mut found,
            )
        };
        if result == libc::ERANGE && bytes.len() < 1024 * 1024 {
            bytes.resize(bytes.len() * 2, 0);
            continue;
        }
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
        if found.is_null() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "account home is unavailable"));
        }
        // SAFETY: a successful getpwuid_r initialized entry and its home string
        // points into bytes, which is still alive and has not moved.
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() {
            return Err(invalid("account home is absent"));
        }
        // SAFETY: pw_dir is the NUL-terminated string from getpwuid_r above.
        let home =
            PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes()));
        if !home.is_absolute() {
            return Err(invalid("account home is not absolute"));
        }
        return Ok(if cfg!(target_os = "macos") {
            home.join("Library/Caches/aterm/capacity")
        } else {
            home.join(".cache/aterm/capacity")
        });
    }
}

#[derive(Debug)]
struct Inner {
    directory: File,
    path: PathBuf,
    config: Identity,
    identities: Vec<Identity>,
    slots: NonZeroU32,
    next: AtomicU32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Identity {
    pub(super) dev: u64,
    pub(super) ino: u64,
}

impl Identity {
    pub(super) fn of(file: &File) -> io::Result<Self> {
        let meta = file.metadata()?;
        Ok(Self { dev: meta.dev(), ino: meta.ino() })
    }
}

const CONFIG_MAX_BYTES: usize = 128 + MAX_SLOTS as usize * 64;

/// A shared CPU pool, with one kernel lock per permitted executing unit.
/// Clones share a directory handle and rotate their starting scan position.
#[derive(Clone, Debug)]
pub struct Pool(Arc<Inner>);

impl Pool {
    /// Open the current account's pool. `slots` is the machine-wide capacity,
    /// not this invocation's local `-j` limit. Conflicting capacities refuse.
    pub fn for_current_user(slots: NonZeroU32) -> io::Result<Self> {
        Self::open_in(&capacity_dir()?, slots)
    }

    /// Open a pool under an explicit absolute capacity directory. Production
    /// callers should use `for_current_user`; this also permits isolated tests.
    /// The capacity parent must be owned and not writable by others; the pool
    /// directory and its regular single-link files must also remain private.
    pub fn open_in(capacity: &Path, slots: NonZeroU32) -> io::Result<Self> {
        if !capacity.is_absolute() || slots.get() > MAX_SLOTS {
            return Err(invalid("invalid CPU pool path or slot count"));
        }
        if !capacity.exists() {
            DirBuilder::new().recursive(true).mode(0o700).create(capacity)?;
        }
        let capacity_handle = open_parent(capacity)?;
        let name = CString::new(DIRECTORY_NAME).expect("fixed directory name");
        // SAFETY: valid directory descriptor and fixed NUL-terminated leaf.
        if unsafe { libc::mkdirat(capacity_handle.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        let path = capacity.join(DIRECTORY_NAME);
        let directory =
            open_at(&capacity_handle, DIRECTORY_NAME, libc::O_RDONLY | libc::O_DIRECTORY, false)?;
        check_private(&directory, true)?;
        // Typed, so a reader of the lock sites (aterm's lock-order census) can see this is an OS
        // advisory lock rather than an in-process mutex; nothing fallible runs between taking the
        // lock and the guard that lets it go.
        let initialization: std::fs::File = open_at(&directory, "init.lock", libc::O_RDWR, true)?;
        check_private(&initialization, false)?;
        initialization.lock()?;
        let initialization = Probe(initialization);
        let mut config = match open_at(&directory, "pool", libc::O_RDONLY, false) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut identities = Vec::with_capacity(slots.get() as usize);
                for slot in 0..slots.get() {
                    let file = open_at(&directory, &slot_name(slot), libc::O_RDWR, true)?;
                    check_private(&file, false)?;
                    identities.push(Identity::of(&file)?);
                }
                publish_config(&directory, &encode_config(&identities))?;
                open_at(&directory, "pool", libc::O_RDONLY, false)?
            }
            result => result?,
        };
        check_private(&config, false)?;
        let identities = parse_config(&read_text(&mut config, CONFIG_MAX_BYTES)?)?;
        if identities.len() != slots.get() as usize {
            return Err(invalid("CPU pool capacity differs"));
        }
        let config_identity = Identity::of(&config)?;
        drop(initialization);
        let pool = Self::from_parts(directory, path, config_identity, identities)?;
        pool.validate_path()?;
        Ok(pool)
    }

    /// Open an existing pool without creating or repairing any file.
    /// `None` means its capacity directory or pool does not exist. A pending
    /// initialization is an error, never a claim that every slot is free.
    pub fn open_existing(capacity: &Path) -> io::Result<Option<Self>> {
        if !capacity.is_absolute() {
            return Err(invalid("CPU pool path is not absolute"));
        }
        let parent = match open_parent(capacity) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        let directory =
            match open_at(&parent, DIRECTORY_NAME, libc::O_RDONLY | libc::O_DIRECTORY, false) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                result => result?,
            };
        check_private(&directory, true)?;
        let _initialization = observe_initialization(&directory)?;
        let mut config = open_at(&directory, "pool", libc::O_RDONLY, false)?;
        check_private(&config, false)?;
        let identities = parse_config(&read_text(&mut config, CONFIG_MAX_BYTES)?)?;
        let identity = Identity::of(&config)?;
        let pool =
            Self::from_parts(directory, capacity.join(DIRECTORY_NAME), identity, identities)?;
        pool.validate_path()?;
        Ok(Some(pool))
    }

    fn from_parts(
        directory: File,
        path: PathBuf,
        config: Identity,
        identities: Vec<Identity>,
    ) -> io::Result<Self> {
        let slots = NonZeroU32::new(
            u32::try_from(identities.len()).map_err(|_| invalid("CPU pool too large"))?,
        )
        .ok_or_else(|| invalid("CPU pool has no slots"))?;
        let pool = Self(Arc::new(Inner {
            directory,
            path,
            config,
            identities,
            slots,
            next: AtomicU32::new(0),
        }));
        for slot in 0..slots.get() {
            pool.open_slot(slot, false)?;
        }
        Ok(pool)
    }

    /// Pool location for diagnostics. Never remove or replace it while used.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// Machine-wide slot count, independent of a scheduler's local parallelism.
    #[must_use]
    pub fn slots(&self) -> NonZeroU32 {
        self.0.slots
    }

    /// Acquire one available unit lease without waiting. `None` means every
    /// slot was held during the scan. It is not a permanent reservation.
    pub fn try_acquire(&self, owner: Owner) -> io::Result<Option<Lease>> {
        self.validate_path()?;
        if owner.pid == 0 {
            return Err(invalid("CPU owner pid is zero"));
        }
        let width = self.0.slots.get();
        let start = self.0.next.fetch_add(1, Ordering::Relaxed) % width;
        for offset in 0..width {
            let slot = (start + offset) % width;
            let mut file = self.open_slot(slot, true)?;
            match file.try_lock() {
                Ok(()) => {
                    let record = Record { slot, owner };
                    file.set_len(0)?;
                    file.seek(SeekFrom::Start(0))?;
                    file.write_all(record.encode().as_bytes())?;
                    file.flush()?;
                    return Ok(Some(Lease { slot, owner, file }));
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(None)
    }

    /// Read current slot states without modifying records or waiting on a
    /// holder. An unlocked stale record is always reported as available.
    pub fn snapshot(&self) -> io::Result<Vec<SlotState>> {
        self.validate_path()?;
        let mut states = Vec::with_capacity(self.0.slots.get() as usize);
        for slot in 0..self.0.slots.get() {
            let mut probe = Probe(self.open_slot(slot, false)?);
            states.push(match probe.0.try_lock_shared() {
                Ok(()) => SlotState::Available { slot },
                Err(std::fs::TryLockError::WouldBlock) => SlotState::Held {
                    slot,
                    reported_owner: bounded_text(&mut probe.0)
                        .ok()
                        .and_then(|text| Record::parse(&text).ok())
                        .filter(|record| record.slot == slot)
                        .map(|record| record.owner),
                },
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            });
        }
        Ok(states)
    }

    fn validate_path(&self) -> io::Result<()> {
        let path = std::fs::symlink_metadata(&self.0.path)?;
        let open = self.0.directory.metadata()?;
        if !path.is_dir() || path.dev() != open.dev() || path.ino() != open.ino() {
            return Err(invalid("CPU pool directory identity changed"));
        }
        check_private(&self.0.directory, true)?;
        let _initialization = observe_initialization(&self.0.directory)?;
        let mut config = open_at(&self.0.directory, "pool", libc::O_RDONLY, false)?;
        check_private(&config, false)?;
        if Identity::of(&config)? != self.0.config {
            return Err(invalid("CPU pool manifest identity changed"));
        }
        if parse_config(&read_text(&mut config, CONFIG_MAX_BYTES)?)? != self.0.identities {
            return Err(invalid("CPU pool manifest contents changed"));
        }
        Ok(())
    }

    fn open_slot(&self, slot: u32, write: bool) -> io::Result<File> {
        let file = open_at(
            &self.0.directory,
            &slot_name(slot),
            if write { libc::O_RDWR } else { libc::O_RDONLY },
            false,
        )?;
        check_private(&file, false)?;
        if Identity::of(&file)? != self.0.identities[slot as usize] {
            return Err(invalid("CPU slot file identity changed"));
        }
        Ok(file)
    }
}

/// A lock taken only to order initialization or to observe the pool: the initializer's, an
/// observer's, a snapshot's. Unlike a lease, no child inherits one on purpose, so it lets go
/// explicitly at drop. A host that forks to spawn (a `pre_exec` hook does) copies every
/// descriptor into the child until its exec, and a lock released only by its final close stays
/// held that long: readers saw a pending initialization, and claimants a busy slot.
pub(super) struct Probe(pub(super) File);

impl Drop for Probe {
    fn drop(&mut self) {
        // A failed unlock leaves the release to the final close.
        let _ = self.0.unlock();
    }
}

/// One executing unit's lease. There is deliberately no explicit unlock in
/// `Drop`: the final close of all duplicated/inherited descriptors releases it.
#[derive(Debug)]
pub struct Lease {
    slot: u32,
    owner: Owner,
    file: File,
}

impl Lease {
    /// Slot held by this lease.
    #[must_use]
    pub fn slot(&self) -> u32 {
        self.slot
    }
    /// Reported unit owner.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }
    /// Duplicate ownership of this same kernel lease. Releasing either copy
    /// alone cannot make the slot available.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { slot: self.slot, owner: self.owner, file: self.file.try_clone()? })
    }
    /// Transfer the descriptor to an existing checked child-launch mechanism.
    /// It stays close-on-exec until that mechanism explicitly inherits it.
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }
}

impl AsFd for Lease {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl AsRawFd for Lease {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }
}

pub(super) fn slot_name(slot: u32) -> String {
    format!("slot-{slot:08x}")
}

fn bounded_text(file: &mut File) -> io::Result<String> {
    read_text(file, MAX_RECORD_BYTES)
}

pub(super) fn read_text(file: &mut File, max: usize) -> io::Result<String> {
    let mut text = String::new();
    file.take((max + 1) as u64).read_to_string(&mut text)?;
    if text.len() > max {
        return Err(invalid("CPU pool record exceeds its size bound"));
    }
    Ok(text)
}

pub(super) fn open_parent(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    // SAFETY: geteuid only reads this process's effective uid.
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "CPU capacity parent must be owned and not writable by others",
        ));
    }
    Ok(file)
}

fn observe_initialization(directory: &File) -> io::Result<Probe> {
    let probe = Probe(open_at(directory, "init.lock", libc::O_RDONLY, false)?);
    check_private(&probe.0, false)?;
    probe.0.try_lock_shared().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(probe)
}

fn publish_config(directory: &File, text: &str) -> io::Result<()> {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let name = format!(
        ".pool-init-{}-{tick}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let mut staged = open_at(directory, &name, libc::O_RDWR | libc::O_EXCL, true)?;
    check_private(&staged, false)?;
    let source = CString::new(name).expect("numeric staged name");
    let destination = c"pool";
    let result = (|| {
        staged.write_all(text.as_bytes())?;
        staged.sync_all()?;
        // SAFETY: both leaves are NUL-terminated, relative to the same live
        // directory descriptor. The exclusive init lock orders publishers.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        directory.sync_all()?;
        Ok(())
    })();
    // SAFETY: this leaf was created exclusively by this invocation. After a
    // successful rename it is already absent; failure cleanup touches no peer.
    unsafe { libc::unlinkat(directory.as_raw_fd(), source.as_ptr(), 0) };
    result
}

fn encode_config(identities: &[Identity]) -> String {
    let mut text = format!("trust-build-cpu-v1 slots={}\n", identities.len());
    for (slot, identity) in identities.iter().enumerate() {
        use std::fmt::Write as _;
        writeln!(text, "slot={slot} dev={} ino={}", identity.dev, identity.ino)
            .expect("writing to String");
    }
    text
}

fn parse_config(text: &str) -> io::Result<Vec<Identity>> {
    if text.len() > CONFIG_MAX_BYTES {
        return Err(invalid("CPU pool manifest too large"));
    }
    let mut lines = text.lines();
    let count = lines
        .next()
        .and_then(|line| line.strip_prefix("trust-build-cpu-v1 slots="))
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|count| (1..=MAX_SLOTS).contains(count))
        .ok_or_else(|| invalid("invalid CPU pool manifest header"))?;
    let mut identities = Vec::with_capacity(count as usize);
    for (slot, line) in lines.enumerate() {
        let fields: Vec<_> = line.split(' ').collect();
        if fields.len() != 3 || fields[0] != format!("slot={slot}") || slot >= count as usize {
            return Err(invalid("invalid CPU pool manifest slot"));
        }
        let read = |field: &str, prefix: &str| -> io::Result<u64> {
            field
                .strip_prefix(prefix)
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid("invalid CPU pool file identity"))
        };
        identities.push(Identity { dev: read(fields[1], "dev=")?, ino: read(fields[2], "ino=")? });
    }
    if identities.len() != count as usize || encode_config(&identities) != text {
        return Err(invalid("incomplete or non-canonical CPU pool manifest"));
    }
    Ok(identities)
}

pub(super) fn open_at(directory: &File, name: &str, flags: i32, create: bool) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid("CPU pool leaf contains NUL"))?;
    let flags = flags
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if create { libc::O_CREAT } else { 0 };
    // SAFETY: directory is owned and open; name is a NUL-terminated relative
    // leaf; on success ownership of the fresh descriptor transfers once.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned this descriptor's sole owner.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn check_private(file: &File, directory: bool) -> io::Result<()> {
    let meta = file.metadata()?;
    // SAFETY: geteuid only reads the effective uid of this process.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid
        || meta.mode() & 0o077 != 0
        || (directory && !meta.is_dir())
        || (!directory && (!meta.is_file() || meta.nlink() != 1))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "CPU pool requires private owner-only directories and regular single-link files",
        ));
    }
    Ok(())
}
