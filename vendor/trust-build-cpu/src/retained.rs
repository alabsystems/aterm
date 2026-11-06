// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

//! Private retained debt. This deliberately does not use v1's stale-record rules.

use std::ffi::CString;
use std::fs::File;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use std::io::Read;
use std::io::{self, Seek, SeekFrom, Write};
use std::num::NonZeroU32;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::unix::{Identity, Probe, check_private, open_at, open_parent, read_text, slot_name};
use crate::{DIRECTORY_NAME, MAX_RECORD_BYTES, MAX_SLOTS, Owner, invalid};

/// Backend directory inside a newly created retained private capacity domain.
pub const RETAINED_DIRECTORY_NAME: &str = "build-cpu-retained-v1";
const VERSION: &str = "trust-build-cpu-retained-v1";
const RECOVERABLE_VERSION: &str = "trust-build-cpu-retained-v2";
const MANIFEST_MAX: usize = 512 + MAX_SLOTS as usize * 64;
const FENCE_MAX: usize = 512;

/// A retained-domain observation. Both held and retained slots deny new work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedSlotState {
    /// An explicit free record was read while a shared kernel lock was held.
    Available {
        /// Slot index.
        slot: u32,
    },
    /// The kernel lock is free but durable controller debt remains.
    Retained {
        /// Slot index.
        slot: u32,
        /// Diagnostic reported owner; this is not recovery authority.
        reported_owner: Owner,
    },
    /// A controller holds the kernel lock. A concurrent write can hide metadata.
    Held {
        /// Slot index.
        slot: u32,
        /// Diagnostic reported owner, if a complete record was observed.
        reported_owner: Option<Owner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Seal {
    root: Identity,
    fence: Identity,
    backend: Identity,
}

#[derive(Debug)]
struct Inner {
    parent: File,
    root: File,
    directory: File,
    path: PathBuf,
    seal: Seal,
    manifest: Identity,
    identities: Vec<Identity>,
    slots: NonZeroU32,
    next: AtomicU32,
}

/// A one-use generated CPU nonce bundle for a custodian's pre-grant journal.
/// Nonce bytes are observations, never constructors or settlement authority.
/// This type intentionally cannot be cloned or rebuilt from observed bytes.
pub struct PreparedReservation {
    width: NonZeroU32,
    nonces: Option<Vec<[u8; 16]>>,
}
impl PreparedReservation {
    /// Generate the complete bounded bundle before any slot record is written.
    pub fn new(width: NonZeroU32) -> io::Result<Self> {
        if width.get() > MAX_SLOTS {
            return Err(invalid("prepared CPU reservation is too large"));
        }
        let mut nonces = Vec::with_capacity(width.get() as usize);
        let mut distinct = std::collections::BTreeSet::new();
        for _ in 0..width.get() {
            let nonce = new_nonce()?;
            if !distinct.insert(nonce) {
                return Err(invalid("prepared CPU nonce collision"));
            }
            nonces.push(nonce);
        }
        Ok(Self { width, nonces: Some(nonces) })
    }
    /// Original requested width, not an acquired grant.
    pub const fn width(&self) -> NonZeroU32 {
        self.width
    }
    /// Persist these identities together with the exact memory cohort reference
    /// BEFORE calling reserve. Observing them cannot mint another prepared plan.
    pub fn nonces(&self) -> io::Result<&[[u8; 16]]> {
        self.nonces
            .as_deref()
            .ok_or_else(|| invalid("prepared CPU reservation was already consumed"))
    }
    pub(crate) fn take(&mut self) -> io::Result<Vec<[u8; 16]>> {
        self.nonces.take().ok_or_else(|| invalid("prepared CPU reservation was already consumed"))
    }
}

/// A private crash-retained pool. It is never an automatic alternate global pool.
/// Clones share observation state but cannot clone a settlement capability.
#[derive(Clone, Debug)]
pub struct RetainedPool(Arc<Inner>);

impl RetainedPool {
    /// Exclusively create a fresh private domain at an absent absolute path.
    /// The existing parent must be owned and not writable by others. A regular
    /// file at the legacy v1 name fences old clients before readiness is written.
    /// Failure leaves the partial domain unavailable; it is never repaired or
    /// adopted automatically. This API does not migrate a machine's global pool.
    pub fn create_private(path: &Path, slots: NonZeroU32) -> io::Result<Self> {
        if !path.is_absolute() || slots.get() > MAX_SLOTS {
            return Err(invalid("invalid retained CPU pool path or slot count"));
        }
        let parent_path =
            path.parent().ok_or_else(|| invalid("retained CPU pool has no parent"))?;
        let leaf = path
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .ok_or_else(|| invalid("invalid retained CPU pool leaf"))?;
        let parent = open_parent(parent_path)?;
        mkdir_exclusive(&parent, leaf)?;
        let root = open_at(&parent, leaf, libc::O_RDONLY | libc::O_DIRECTORY, false)?;
        check_private(&root, true)?;
        // This MUST precede backend initialization. A racing legacy mkdir can
        // win, but then O_EXCL fails and this constructor never publishes.
        let mut fence = open_at(&root, DIRECTORY_NAME, libc::O_RDWR | libc::O_EXCL, true)?;
        check_private(&fence, false)?;
        fence.sync_all()?;
        root.sync_all()?;
        mkdir_exclusive(&root, RETAINED_DIRECTORY_NAME)?;
        let directory =
            open_at(&root, RETAINED_DIRECTORY_NAME, libc::O_RDONLY | libc::O_DIRECTORY, false)?;
        check_private(&directory, true)?;
        let seal = Seal {
            root: Identity::of(&root)?,
            fence: Identity::of(&fence)?,
            backend: Identity::of(&directory)?,
        };
        let mut identities = Vec::with_capacity(slots.get() as usize);
        for slot in 0..slots.get() {
            let mut file =
                open_at(&directory, &slot_name(slot), libc::O_RDWR | libc::O_EXCL, true)?;
            check_private(&file, false)?;
            write_record(&mut file, &State::Free.encode(slot))?;
            identities.push(Identity::of(&file)?);
        }
        let mut manifest = open_at(&directory, "pool", libc::O_RDWR | libc::O_EXCL, true)?;
        check_private(&manifest, false)?;
        manifest.write_all(encode_manifest(seal, &identities).as_bytes())?;
        manifest.sync_all()?;
        let manifest_identity = Identity::of(&manifest)?;
        directory.sync_all()?;
        // Commit both directory links before another opener can see a complete
        // readiness seal. An incomplete fence after a crash remains unavailable.
        root.sync_all()?;
        parent.sync_all()?;
        // Readiness is the complete fence, which pins the manifest inode. An
        // empty/partial fence cannot be opened as a usable retained pool.
        fence.write_all(encode_fence(seal, manifest_identity).as_bytes())?;
        fence.sync_all()?;
        let pool = Self::from_parts(
            parent,
            root,
            directory,
            path.to_owned(),
            seal,
            manifest_identity,
            identities,
        )?;
        pool.validate()?;
        Ok(pool)
    }

    /// Open only a ready retained domain, without creating or repairing files.
    /// `None` means the root does not exist. A partial or changed domain refuses.
    pub fn open_existing(path: &Path) -> io::Result<Option<Self>> {
        if !path.is_absolute() {
            return Err(invalid("retained CPU pool path is not absolute"));
        }
        let parent_path =
            path.parent().ok_or_else(|| invalid("retained CPU pool has no parent"))?;
        let leaf = path
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .ok_or_else(|| invalid("invalid retained CPU pool leaf"))?;
        let parent = match open_parent(parent_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        let root = match open_at(&parent, leaf, libc::O_RDONLY | libc::O_DIRECTORY, false) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        check_private(&root, true)?;
        let mut fence = open_at(&root, DIRECTORY_NAME, libc::O_RDONLY, false)?;
        check_private(&fence, false)?;
        let (seal, manifest_identity) = parse_fence(&read_text(&mut fence, FENCE_MAX)?)?;
        if Identity::of(&root)? != seal.root || Identity::of(&fence)? != seal.fence {
            return Err(invalid("retained CPU root or fence identity changed"));
        }
        let directory =
            open_at(&root, RETAINED_DIRECTORY_NAME, libc::O_RDONLY | libc::O_DIRECTORY, false)?;
        check_private(&directory, true)?;
        if Identity::of(&directory)? != seal.backend {
            return Err(invalid("retained CPU backend identity changed"));
        }
        let mut manifest = open_at(&directory, "pool", libc::O_RDONLY, false)?;
        check_private(&manifest, false)?;
        if Identity::of(&manifest)? != manifest_identity {
            return Err(invalid("retained CPU manifest identity changed"));
        }
        let identities = parse_manifest(&read_text(&mut manifest, MANIFEST_MAX)?, seal)?;
        let pool = Self::from_parts(
            parent,
            root,
            directory,
            path.to_owned(),
            seal,
            manifest_identity,
            identities,
        )?;
        pool.validate()?;
        // Ready-open validates every unlocked record. A live holder may be in
        // the middle of a write, so snapshot keeps that unit unavailable
        // without treating partial held metadata as free or as corruption.
        pool.snapshot()?;
        Ok(Some(pool))
    }

    fn from_parts(
        parent: File,
        root: File,
        directory: File,
        path: PathBuf,
        seal: Seal,
        manifest: Identity,
        identities: Vec<Identity>,
    ) -> io::Result<Self> {
        let slots = NonZeroU32::new(identities.len() as u32)
            .ok_or_else(|| invalid("retained CPU pool has no slots"))?;
        let pool = Self(Arc::new(Inner {
            parent,
            root,
            directory,
            path,
            seal,
            manifest,
            identities,
            slots,
            next: AtomicU32::new(0),
        }));
        for slot in 0..slots.get() {
            pool.open_slot(slot, false)?;
        }
        Ok(pool)
    }

    /// Private domain location for diagnostics. Paths confer no release authority.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// Capacity of this explicit private domain.
    #[must_use]
    pub fn slots(&self) -> NonZeroU32 {
        self.0.slots
    }

    /// Acquire one free slot, durably retaining its debt before returning.
    /// A stale retained record remains unavailable even without a kernel holder.
    /// Unknown or damaged state is an error, never silently free capacity.
    pub fn try_acquire(&self, owner: Owner) -> io::Result<Option<RetainedLease>> {
        self.try_acquire_prepared(owner, new_nonce()?)
    }

    pub(crate) fn try_acquire_prepared(
        &self,
        owner: Owner,
        nonce: [u8; 16],
    ) -> io::Result<Option<RetainedLease>> {
        self.acquire_with(owner, nonce, |_| None)
    }

    fn acquire_with(
        &self,
        owner: Owner,
        nonce: [u8; 16],
        commitment: impl Fn(u32) -> Option<[u8; 32]>,
    ) -> io::Result<Option<RetainedLease>> {
        self.validate()?;
        if owner.pid == 0 {
            return Err(invalid("CPU owner pid is zero"));
        }
        let width = self.0.slots.get();
        let start = self.0.next.fetch_add(1, Ordering::Relaxed) % width;
        for offset in 0..width {
            let slot = (start + offset) % width;
            let mut file = self.open_slot(slot, true)?;
            match file.try_lock() {
                Ok(()) => match read_record(&mut file, slot)? {
                    State::Free => {
                        let recovery = commitment(slot);
                        write_record(
                            &mut file,
                            &State::Retained { owner, nonce, recovery }.encode(slot),
                        )?;
                        self.validate()?;
                        return Ok(Some(RetainedLease {
                            pool: self.clone(),
                            slot,
                            owner,
                            nonce,
                            recovery,
                            file,
                        }));
                    }
                    State::Retained { .. } => {
                        // Temporary acquisition must unlock explicitly: a fork
                        // may have transiently copied this otherwise-CLOEXEC fd.
                        drop(Probe(file));
                    }
                },
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
        Ok(None)
    }

    /// Observe free, held, and unlocked retained debt without waiting or repair.
    pub fn snapshot(&self) -> io::Result<Vec<RetainedSlotState>> {
        self.validate()?;
        let mut states = Vec::with_capacity(self.0.slots.get() as usize);
        for slot in 0..self.0.slots.get() {
            let mut probe = Probe(self.open_slot(slot, false)?);
            let state = match probe.0.try_lock_shared() {
                Ok(()) => match read_record(&mut probe.0, slot)? {
                    State::Free => RetainedSlotState::Available { slot },
                    State::Retained { owner, .. } => {
                        RetainedSlotState::Retained { slot, reported_owner: owner }
                    }
                },
                Err(std::fs::TryLockError::WouldBlock) => RetainedSlotState::Held {
                    slot,
                    reported_owner: read_record(&mut probe.0, slot).ok().and_then(|state| {
                        match state {
                            State::Retained { owner, .. } => Some(owner),
                            State::Free => None,
                        }
                    }),
                },
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            };
            states.push(state);
        }
        Ok(states)
    }

    fn validate(&self) -> io::Result<()> {
        let parent_path =
            self.0.path.parent().ok_or_else(|| invalid("retained CPU pool has no parent"))?;
        let parent = open_parent(parent_path)?;
        if Identity::of(&parent)? != Identity::of(&self.0.parent)? {
            return Err(invalid("retained CPU parent identity changed"));
        }
        let path = std::fs::symlink_metadata(&self.0.path)?;
        if !path.is_dir() || (Identity { dev: path.dev(), ino: path.ino() }) != self.0.seal.root {
            return Err(invalid("retained CPU root identity changed"));
        }
        check_private(&self.0.root, true)?;
        let mut fence = open_at(&self.0.root, DIRECTORY_NAME, libc::O_RDONLY, false)?;
        check_private(&fence, false)?;
        if Identity::of(&fence)? != self.0.seal.fence
            || parse_fence(&read_text(&mut fence, FENCE_MAX)?)? != (self.0.seal, self.0.manifest)
        {
            return Err(invalid("retained CPU fence changed"));
        }
        let backend = open_at(
            &self.0.root,
            RETAINED_DIRECTORY_NAME,
            libc::O_RDONLY | libc::O_DIRECTORY,
            false,
        )?;
        check_private(&backend, true)?;
        check_private(&self.0.directory, true)?;
        if Identity::of(&backend)? != self.0.seal.backend {
            return Err(invalid("retained CPU backend identity changed"));
        }
        let mut manifest = open_at(&self.0.directory, "pool", libc::O_RDONLY, false)?;
        check_private(&manifest, false)?;
        if Identity::of(&manifest)? != self.0.manifest
            || parse_manifest(&read_text(&mut manifest, MANIFEST_MAX)?, self.0.seal)?
                != self.0.identities
        {
            return Err(invalid("retained CPU manifest changed"));
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
            return Err(invalid("retained CPU slot identity changed"));
        }
        Ok(file)
    }
}

/// The API's settlement capability for one durable retained claim.
/// It cannot be cloned, decoded from a record, or reopened from a path. Dropping
/// it closes the lock but leaves debt; process death has the same behavior.
#[derive(Debug)]
pub struct RetainedLease {
    pool: RetainedPool,
    slot: u32,
    owner: Owner,
    nonce: [u8; 16],
    recovery: Option<[u8; 32]>,
    file: File,
}

/// Persistable identity of one original retained debt generation.
///
/// These fields are observations, not a settlement or recovery capability.
/// Reconstructing them cannot prove cohort closure, reclaim a lock, or release
/// debt. A recovery transaction must validate its original durable binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedLeaseIdentity {
    /// Device and inode of the original retained pool manifest.
    pub manifest: (u64, u64),
    /// Slot index within that exact pool.
    pub slot: u32,
    /// Device and inode of the original slot file.
    pub file: (u64, u64),
    /// One-use debt generation, distinct even when the same slot is reused.
    pub nonce: [u8; 16],
    /// Original diagnostic owner; PID/birth alone authorizes no recovery.
    pub owner: Owner,
}

impl RetainedLease {
    /// Capture identity while this original capability is still owned.
    /// This neither creates a recovery grant nor closes the reserve-to-bind
    /// crash gap: unbound debt must remain conservative after that gap.
    pub fn identity(&self) -> io::Result<RetainedLeaseIdentity> {
        self.pool.validate()?;
        check_private(&self.file, false)?;
        let identity = Identity::of(&self.file)?;
        let mut current = self.pool.open_slot(self.slot, false)?;
        if identity != Identity::of(&current)? {
            return Err(invalid("retained CPU identity no longer names the original slot"));
        }
        match read_record(&mut current, self.slot)? {
            State::Retained { owner, nonce, recovery }
                if owner == self.owner && nonce == self.nonce && recovery == self.recovery => {}
            _ => {
                return Err(invalid("retained CPU identity no longer matches its debt generation"));
            }
        }
        Ok(RetainedLeaseIdentity {
            manifest: (self.pool.0.manifest.dev as u64, self.pool.0.manifest.ino as u64),
            slot: self.slot,
            file: (identity.dev as u64, identity.ino as u64),
            nonce: self.nonce,
            owner: self.owner,
        })
    }

    /// Slot whose debt this capability controls.
    #[must_use]
    pub fn slot(&self) -> u32 {
        self.slot
    }

    /// Diagnostic owner reported when acquiring the claim.
    #[must_use]
    pub fn owner(&self) -> Owner {
        self.owner
    }

    /// Settle after the caller conclusively establishes that all work using
    /// this credit has ended. This leaf cannot establish descendant closure.
    /// Possession of this capability alone is not evidence of work completion.
    ///
    /// The original lock remains held while identities and nonce are checked
    /// and the explicit free record is written and synced. Success commits
    /// settlement. An I/O error after the free write has begun is uncertain:
    /// the slot can already be free, or remain debt/damaged state. The caller
    /// must therefore establish closure BEFORE calling, including on errors.
    pub fn settle(mut self) -> io::Result<()> {
        self.pool.validate()?;
        let current = self.pool.open_slot(self.slot, false)?;
        if Identity::of(&self.file)? != Identity::of(&current)? {
            return Err(invalid("retained CPU capability identity changed"));
        }
        check_private(&self.file, false)?;
        match read_record(&mut self.file, self.slot)? {
            State::Retained { owner, nonce, recovery }
                if owner == self.owner && nonce == self.nonce && recovery == self.recovery => {}
            _ => return Err(invalid("retained CPU claim does not match settlement capability")),
        }
        write_record(&mut self.file, &State::Free.encode(self.slot))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Free,
    Retained { owner: Owner, nonce: [u8; 16], recovery: Option<[u8; 32]> },
}

impl State {
    fn encode(self, slot: u32) -> String {
        match self {
            Self::Free => format!("{VERSION} slot={slot} free=1\n"),
            Self::Retained { owner, nonce, recovery } => {
                let birth = owner.birth.map_or_else(|| "-".into(), |value| value.to_string());
                let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
                match recovery {
                    None => format!(
                        "{VERSION} slot={slot} pid={} birth={birth} nonce={nonce} retained=1\n",
                        owner.pid
                    ),
                    Some(commitment) => {
                        let commitment: String =
                            commitment.iter().map(|byte| format!("{byte:02x}")).collect();
                        format!(
                            "{RECOVERABLE_VERSION} slot={slot} pid={} birth={birth} nonce={nonce} commitment={commitment} retained=1\n",
                            owner.pid
                        )
                    }
                }
            }
        }
    }
}

fn read_record(file: &mut File, slot: u32) -> io::Result<State> {
    file.seek(SeekFrom::Start(0))?;
    let text = read_text(file, MAX_RECORD_BYTES)?;
    if text == State::Free.encode(slot) {
        return Ok(State::Free);
    }
    let words: Vec<_> = text
        .strip_suffix('\n')
        .ok_or_else(|| invalid("unterminated retained CPU record"))?
        .split(' ')
        .collect();
    let recoverable = words.first() == Some(&RECOVERABLE_VERSION);
    if words.len() != if recoverable { 7 } else { 6 }
        || words[0] != if recoverable { RECOVERABLE_VERSION } else { VERSION }
        || words[1] != format!("slot={slot}")
        || words.last() != Some(&"retained=1")
    {
        return Err(invalid("invalid retained CPU record"));
    }
    let pid = u32::try_from(number(words[2], "pid=")?)
        .map_err(|_| invalid("retained CPU pid overflow"))?;
    if pid == 0 {
        return Err(invalid("retained CPU pid is zero"));
    }
    let birth = if words[3] == "birth=-" { None } else { Some(number(words[3], "birth=")?) };
    let hex = words[4]
        .strip_prefix("nonce=")
        .filter(|hex| hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("invalid retained CPU nonce"))?;
    let mut nonce = [0; 16];
    for (index, byte) in nonce.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
            .map_err(|_| invalid("invalid retained CPU nonce"))?;
    }
    let recovery = if recoverable {
        let text = words[5]
            .strip_prefix("commitment=")
            .filter(|text| text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| invalid("invalid recovery commitment"))?;
        let mut commitment = [0; 32];
        for (index, byte) in commitment.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * index..2 * index + 2], 16)
                .map_err(|_| invalid("invalid recovery commitment"))?;
        }
        Some(commitment)
    } else {
        None
    };
    let state = State::Retained { owner: Owner { pid, birth }, nonce, recovery };
    if state.encode(slot) != text {
        return Err(invalid("non-canonical retained CPU record"));
    }
    Ok(state)
}

fn write_record(file: &mut File, text: &str) -> io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

fn new_nonce() -> io::Result<[u8; 16]> {
    let mut nonce = [0; 16];
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        // SAFETY: the live buffer is writable for 16 bytes, below getentropy's
        // 256-byte maximum. Failure refuses issuance, with no weak fallback.
        if unsafe { libc::getentropy(nonce.as_mut_ptr().cast(), nonce.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Unix portability fallback. No path or PID-derived entropy is used.
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    }
    Ok(nonce)
}

fn mkdir_exclusive(parent: &File, leaf: &str) -> io::Result<()> {
    if leaf.is_empty() || leaf.contains('/') || leaf == "." || leaf == ".." {
        return Err(invalid("invalid retained CPU directory leaf"));
    }
    let leaf = CString::new(leaf).map_err(|_| invalid("retained CPU directory contains NUL"))?;
    // SAFETY: live directory fd and checked single NUL-terminated leaf.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn number(word: &str, prefix: &str) -> io::Result<u64> {
    let digits = word
        .strip_prefix(prefix)
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| invalid("invalid retained CPU number"))?;
    digits.parse().map_err(|_| invalid("retained CPU number overflow"))
}

fn encode_fence(seal: Seal, manifest: Identity) -> String {
    format!(
        "{VERSION} ready=1 root-dev={} root-ino={} fence-dev={} fence-ino={} backend-dev={} backend-ino={} manifest-dev={} manifest-ino={}\n",
        seal.root.dev,
        seal.root.ino,
        seal.fence.dev,
        seal.fence.ino,
        seal.backend.dev,
        seal.backend.ino,
        manifest.dev,
        manifest.ino
    )
}

fn parse_fence(text: &str) -> io::Result<(Seal, Identity)> {
    let words: Vec<_> = text
        .strip_suffix('\n')
        .ok_or_else(|| invalid("incomplete retained CPU fence"))?
        .split(' ')
        .collect();
    if words.len() != 10 || words[0] != VERSION || words[1] != "ready=1" {
        return Err(invalid("invalid retained CPU fence"));
    }
    let seal = Seal {
        root: Identity { dev: number(words[2], "root-dev=")?, ino: number(words[3], "root-ino=")? },
        fence: Identity {
            dev: number(words[4], "fence-dev=")?,
            ino: number(words[5], "fence-ino=")?,
        },
        backend: Identity {
            dev: number(words[6], "backend-dev=")?,
            ino: number(words[7], "backend-ino=")?,
        },
    };
    let manifest = Identity {
        dev: number(words[8], "manifest-dev=")?,
        ino: number(words[9], "manifest-ino=")?,
    };
    if encode_fence(seal, manifest) != text {
        return Err(invalid("non-canonical retained CPU fence"));
    }
    Ok((seal, manifest))
}

fn encode_manifest(seal: Seal, identities: &[Identity]) -> String {
    use std::fmt::Write as _;
    let mut text = format!(
        "{VERSION} slots={} root-dev={} root-ino={} fence-dev={} fence-ino={} backend-dev={} backend-ino={}\n",
        identities.len(),
        seal.root.dev,
        seal.root.ino,
        seal.fence.dev,
        seal.fence.ino,
        seal.backend.dev,
        seal.backend.ino
    );
    for (slot, identity) in identities.iter().enumerate() {
        writeln!(text, "slot={slot} dev={} ino={}", identity.dev, identity.ino)
            .expect("writing to String");
    }
    text
}

fn parse_manifest(text: &str, seal: Seal) -> io::Result<Vec<Identity>> {
    let mut lines = text.lines();
    let words: Vec<_> =
        lines.next().ok_or_else(|| invalid("empty retained CPU manifest"))?.split(' ').collect();
    if words.len() != 8 || words[0] != VERSION {
        return Err(invalid("invalid retained CPU manifest"));
    }
    let count = number(words[1], "slots=")?;
    if count == 0 || count > u64::from(MAX_SLOTS) {
        return Err(invalid("invalid retained CPU capacity"));
    }
    let mut identities = Vec::with_capacity(count as usize);
    for (slot, line) in lines.enumerate() {
        let fields: Vec<_> = line.split(' ').collect();
        if slot >= count as usize || fields.len() != 3 || fields[0] != format!("slot={slot}") {
            return Err(invalid("invalid retained CPU manifest slot"));
        }
        identities
            .push(Identity { dev: number(fields[1], "dev=")?, ino: number(fields[2], "ino=")? });
    }
    if identities.len() != count as usize || encode_manifest(seal, &identities) != text {
        return Err(invalid("incomplete or changed retained CPU manifest"));
    }
    Ok(identities)
}

#[path = "retained_recovery.rs"]
mod recovery;
pub use recovery::RecoveredReservation;
