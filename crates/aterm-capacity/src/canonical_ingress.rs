// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit pending-only fencing of canonical legacy admission paths.
//!
//! The whole capacity directory (old memory ledger and CPU pool) and the
//! build-slot directory are preserved. Two exchanges cannot be atomic, so an
//! immutable paired intent precedes either one. This component publishes no
//! readiness, namespace, CPU supply, drain proof or recovery authority. Existing
//! descriptors, adopted work and covered/nested launches can outlive both fences.
//! Nothing invokes this preparation on the account or changes the default route.

use aterm_dirfd::{self as fd, AsFd as _, Mode, OFlags, RenameFlags};
use aterm_json::Value;
use std::{
    fs::File,
    io::{self, Write as _},
    os::unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
    path::{Component, Path, PathBuf},
};

const MAX_RECORD: u64 = 32 * 1024;
const MAX_DEPTH: usize = 128;

fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[derive(Debug, PartialEq, Eq)]
struct Scope {
    host: String,
    uid: u32,
}
impl Scope {
    fn current() -> io::Result<Self> {
        let host = aterm_uds::process::host_id()
            .ok_or_else(|| refused("ingress host identity unavailable"))?;
        // SAFETY: geteuid only observes the effective account selecting this transaction.
        Ok(Self {
            host,
            uid: unsafe { libc::geteuid() },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity(u64, u64);
impl Identity {
    fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self(metadata.dev(), metadata.ino()))
    }
    fn value(&self) -> Value {
        aterm_json::json!([self.0, self.1])
    }
    fn decode(value: &Value) -> io::Result<Self> {
        let pair = value
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| refused("invalid ingress inode"))?;
        Ok(Self(
            pair[0]
                .as_u64()
                .ok_or_else(|| refused("invalid ingress device"))?,
            pair[1]
                .as_u64()
                .ok_or_else(|| refused("invalid ingress inode number"))?,
        ))
    }
}

fn owned_directory(file: &File) -> io::Result<()> {
    let m = file.metadata()?;
    // SAFETY: geteuid only reads this process's account identity.
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 {
        return Err(refused(
            "ingress directory must be owned and not writable by others",
        ));
    }
    Ok(())
}
fn private_file(file: &File) -> io::Result<()> {
    let m = file.metadata()?;
    // SAFETY: geteuid only reads this process's account identity.
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o777 != 0o600
        || m.nlink() != 1
    {
        return Err(refused(
            "ingress record must be private, owned and single-link",
        ));
    }
    Ok(())
}
fn directory(path: &Path) -> io::Result<(File, Vec<Identity>)> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(fd::openat(fd::CWD, "/", flags, Mode::empty())?);
    let mut ancestors = vec![Identity::of(&directory)?];
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) if ancestors.len() < MAX_DEPTH => {
                directory = File::from(fd::openat(&directory, name, flags, Mode::empty())?);
                ancestors.push(Identity::of(&directory)?);
            }
            _ => {
                return Err(refused(
                    "ingress ancestry is not canonical or exceeds its bound",
                ));
            }
        }
    }
    owned_directory(&directory)?;
    Ok((directory, ancestors))
}
fn node(parent: &File, name: &str, writable: bool) -> io::Result<File> {
    let access = if writable {
        OFlags::RDWR
    } else {
        OFlags::RDONLY
    };
    aterm_uds::kept_out(|| {
        fd::openat(
            parent,
            name,
            access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    })
}
fn create(parent: &File, name: &str) -> io::Result<File> {
    let file = crate::cohort_namespace::create(parent, name)?;
    // Normalize only this exclusively created inode, never a legacy directory.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    private_file(&file)?;
    Ok(file)
}
fn read(file: &File) -> io::Result<Vec<u8>> {
    private_file(file)?;
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_RECORD {
        return Err(refused(
            "partial or oversized ingress intent remains unavailable",
        ));
    }
    let mut bytes = vec![0; length as usize];
    file.read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}

/// Path selection only, never admission or migration authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    capacity: PathBuf,
    build_slot: PathBuf,
}
impl Layout {
    /// Select exact existing canonical legacy paths without changing them.
    /// Private fixtures use the same API beneath their own existing parents.
    /// # Errors
    /// Missing/noncanonical paths, wrong leaf names, excessive paths or overlap.
    pub fn new(capacity: &Path, build_slot: &Path) -> io::Result<Self> {
        for (path, leaf) in [(capacity, "capacity"), (build_slot, "build-slot")] {
            if !path.is_absolute()
                || path.file_name().and_then(|name| name.to_str()) != Some(leaf)
                || path
                    .to_str()
                    .is_none_or(|text| text.len() > 4096 || text.chars().any(char::is_control))
                || path.canonicalize()?.as_os_str() != path.as_os_str()
            {
                return Err(refused(
                    "ingress paths must name exact canonical legacy entries",
                ));
            }
        }
        if capacity.starts_with(build_slot) || build_slot.starts_with(capacity) {
            return Err(refused("ingress paths overlap"));
        }
        Ok(Self {
            capacity: capacity.to_owned(),
            build_slot: build_slot.to_owned(),
        })
    }
    /// Resolve the real account's existing paths without changing or preparing them.
    /// # Errors
    /// Account lookup, missing paths, or [`Self::new`]'s refusals.
    pub fn for_current_account() -> io::Result<Self> {
        let capacity = crate::build_cpu::capacity_dir()?;
        let home = crate::account_home().ok_or_else(|| refused("account home unavailable"))?;
        Self::new(&capacity, &home.join(".cache/build-slot"))
    }
    /// The whole legacy memory-ledger and CPU-pool entry.
    #[must_use]
    pub fn capacity(&self) -> &Path {
        &self.capacity
    }
    /// The canonical legacy build-slot entry.
    #[must_use]
    pub fn build_slot(&self) -> &Path {
        &self.build_slot
    }
}

/// The named legacy ingress whose original tree is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingress {
    /// The complete legacy memory ledger and CPU-pool directory.
    Capacity,
    /// The legacy build-slot directory, including queues and adopted records.
    BuildSlot,
}
impl Ingress {
    fn name(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::BuildSlot => "build-slot",
        }
    }
    fn record(self) -> &'static str {
        match self {
            Self::Capacity => ".capacity-ingress.v1.pending",
            Self::BuildSlot => ".build-slot-ingress.v1.pending",
        }
    }
    fn prior(self) -> &'static str {
        match self {
            Self::Capacity => ".capacity-ingress.v1.prior.noindex",
            Self::BuildSlot => ".build-slot-ingress.v1.prior.noindex",
        }
    }
}

/// Physical name observation only; neither variant establishes quiescence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameState {
    /// The original directory still occupies its legacy name.
    LegacyDirectory,
    /// The original regular-file fence occupies that name.
    FileFence,
}
/// Both observations remain pending-only, including after both names are fenced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingStatus {
    /// Current whole-capacity name observation.
    pub capacity: NameState,
    /// Current build-slot name observation.
    pub build_slot: NameState,
}
/// A bounded predecessor inventory observation, never a drain proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contents {
    /// No non-dot entry was visible during this read; old descriptors may still write.
    EmptySnapshot,
    /// At least one original or late entry is present; nothing is deleted or classified stale.
    EntriesPresent,
}
/// Read-only location and identity of an original predecessor tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Predecessor {
    /// Current diagnostic path, not a release or recovery capability.
    pub path: PathBuf,
    /// Original device identity.
    pub device: u64,
    /// Original inode identity.
    pub inode: u64,
    /// One inventory observation, without any readiness implication.
    pub contents: Contents,
}

#[derive(Debug)]
struct Lock {
    file: File,
    identity: Identity,
    owner: u32,
}
impl Lock {
    fn take(parent: &File, kind: Ingress, fresh: bool) -> io::Result<Self> {
        let file = if fresh {
            create(parent, kind.record())?
        } else {
            node(parent, kind.record(), true)?
        };
        private_file(&file)?;
        let identity = Identity::of(&file)?;
        let owner = std::process::id();
        #[expect(
            clippy::disallowed_methods,
            reason = "original physical rendezvous guard unlocks in Drop"
        )]
        let locked = file.try_lock();
        match locked {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        Ok(Self {
            identity,
            file,
            owner,
        })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        // A fork inherits the same flock open-file description. An inherited
        // copy may close its FD, but must not explicitly unlock its parent's.
        if self.owner == std::process::id() {
            let _ = self.file.unlock();
        }
    }
}

#[derive(Debug)]
struct Location {
    path: PathBuf,
    parent: File,
    ancestors: Vec<Identity>,
    kind: Ingress,
}
impl Location {
    fn open(path: &Path, kind: Ingress) -> io::Result<Self> {
        let (parent, ancestors) = directory(
            path.parent()
                .ok_or_else(|| refused("ingress has no parent"))?,
        )?;
        Ok(Self {
            path: path.to_owned(),
            parent,
            ancestors,
            kind,
        })
    }
    fn validate(&self) -> io::Result<()> {
        let (_, ancestors) = directory(
            self.path
                .parent()
                .ok_or_else(|| refused("ingress has no parent"))?,
        )?;
        if ancestors != self.ancestors {
            return Err(refused("original ingress ancestry changed"));
        }
        owned_directory(&self.parent)
    }
}
#[derive(Debug)]
struct Description {
    path: PathBuf,
    ancestors: Vec<Identity>,
    record: Identity,
    original: Identity,
    fence: Identity,
}
impl Description {
    fn value(&self) -> Value {
        let ancestors: Vec<_> = self.ancestors.iter().map(Identity::value).collect();
        aterm_json::json!({"path":self.path,"ancestors":ancestors,"record":self.record.value(),
            "original":self.original.value(),"fence":self.fence.value()})
    }
    fn decode(value: &Value) -> io::Result<Self> {
        fields(value, &["path", "ancestors", "record", "original", "fence"])?;
        let ancestors = value
            .get("ancestors")
            .and_then(Value::as_array)
            .filter(|values| !values.is_empty() && values.len() <= MAX_DEPTH)
            .ok_or_else(|| refused("invalid ingress ancestry"))?
            .iter()
            .map(Identity::decode)
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            path: PathBuf::from(
                value
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| refused("invalid ingress path"))?,
            ),
            ancestors,
            record: Identity::decode(
                value
                    .get("record")
                    .ok_or_else(|| refused("missing ingress record"))?,
            )?,
            original: Identity::decode(
                value
                    .get("original")
                    .ok_or_else(|| refused("missing ingress original"))?,
            )?,
            fence: Identity::decode(
                value
                    .get("fence")
                    .ok_or_else(|| refused("missing ingress fence"))?,
            )?,
        })
    }
}
fn fields(value: &Value, names: &[&str]) -> io::Result<()> {
    if !value.as_object().is_some_and(|object| {
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
    }) {
        return Err(refused("unexpected ingress intent fields"));
    }
    Ok(())
}

#[derive(Debug)]
struct Entry {
    location: Location,
    lock: Lock,
    original: File,
    fence: File,
    description: Description,
}
impl Entry {
    fn fresh(location: Location, original: File) -> io::Result<Self> {
        owned_directory(&original)?;
        let lock = Lock::take(&location.parent, location.kind, true)?;
        let fence = create(&location.parent, location.kind.prior())?;
        let description = Description {
            path: location.path.clone(),
            ancestors: location.ancestors.clone(),
            record: lock.identity.clone(),
            original: Identity::of(&original)?,
            fence: Identity::of(&fence)?,
        };
        Ok(Self {
            location,
            lock,
            original,
            fence,
            description,
        })
    }
    fn recover(location: Location, lock: Lock, description: Description) -> io::Result<Self> {
        if description.path != location.path
            || description.ancestors != location.ancestors
            || description.record != lock.identity
            || description.original == description.fence
        {
            return Err(refused(
                "ingress intent differs from its actual selected scope",
            ));
        }
        let old = node(&location.parent, location.kind.name(), false)?;
        let prior = node(&location.parent, location.kind.prior(), false)?;
        let (original, fence) = if Identity::of(&old)? == description.original
            && Identity::of(&prior)? == description.fence
        {
            (old, prior)
        } else if Identity::of(&old)? == description.fence
            && Identity::of(&prior)? == description.original
        {
            (prior, old)
        } else {
            return Err(refused("original ingress pair is missing or replaced"));
        };
        owned_directory(&original)?;
        private_file(&fence)?;
        Ok(Self {
            location,
            lock,
            original,
            fence,
            description,
        })
    }
    fn validate(&self, bytes: &[u8]) -> io::Result<NameState> {
        self.location.validate()?;
        owned_directory(&self.original)?;
        private_file(&self.fence)?;
        let record = node(&self.location.parent, self.location.kind.record(), false)?;
        if Identity::of(&record)? != self.lock.identity
            || read(&record)? != bytes
            || read(&self.lock.file)? != bytes
            || read(&self.fence)? != bytes
            || Identity::of(&self.original)? != self.description.original
            || Identity::of(&self.fence)? != self.description.fence
        {
            return Err(refused("original ingress intent or held objects changed"));
        }
        let old = node(&self.location.parent, self.location.kind.name(), false)?;
        let prior = node(&self.location.parent, self.location.kind.prior(), false)?;
        if Identity::of(&old)? == self.description.original
            && Identity::of(&prior)? == self.description.fence
        {
            owned_directory(&old)?;
            private_file(&prior)?;
            Ok(NameState::LegacyDirectory)
        } else if Identity::of(&old)? == self.description.fence
            && Identity::of(&prior)? == self.description.original
        {
            private_file(&old)?;
            owned_directory(&prior)?;
            Ok(NameState::FileFence)
        } else {
            Err(refused(
                "original ingress names changed; no rollback is permitted",
            ))
        }
    }
    fn sync(&self) -> io::Result<()> {
        aterm_durable::durable(&self.lock.file)?;
        aterm_durable::durable(&self.fence)?;
        aterm_durable::durable(&self.location.parent)
    }
}

/// Original process-affine administrative preparation. No method grants replacement supply.
/// Drop leaves all intents, fences and predecessor trees intact, even after partial failure.
#[derive(Debug)]
pub struct Preparation {
    entries: [Entry; 2],
    bytes: Vec<u8>,
    scope: Scope,
    owner: u32,
}
impl Preparation {
    /// Exclusively record one pending paired intent, before changing either legacy name.
    /// Parents must already exist, be owned and not writable by others. Mode 0755
    /// legacy directories are preserved without claiming private cohort readiness.
    /// An occupied, blank or partial rendezvous is never reset or treated as virgin.
    /// # Errors
    /// Unsafe/overlapping scope, occupied intent/prior, unavailable lock or any IO failure.
    /// Failure preserves partial preparation; it does not remove or replace a legacy tree.
    pub fn begin(layout: &Layout) -> io::Result<Self> {
        let scope = Scope::current()?;
        let checked = Layout::new(&layout.capacity, &layout.build_slot)?;
        let capacity = Location::open(&checked.capacity, Ingress::Capacity)?;
        let slots = Location::open(&checked.build_slot, Ingress::BuildSlot)?;
        // Check both original directory scopes before creating any record.
        let old_capacity = node(&capacity.parent, "capacity", false)?;
        let old_slots = node(&slots.parent, "build-slot", false)?;
        owned_directory(&old_capacity)?;
        owned_directory(&old_slots)?;
        let capacity_id = Identity::of(&old_capacity)?;
        let slots_id = Identity::of(&old_slots)?;
        if capacity_id == slots_id
            || capacity.ancestors.contains(&slots_id)
            || slots.ancestors.contains(&capacity_id)
        {
            return Err(refused("ingress scopes physically overlap"));
        }
        let mut entries = [
            Entry::fresh(capacity, old_capacity)?,
            Entry::fresh(slots, old_slots)?,
        ];
        let value = aterm_json::json!({"schema":1,"state":"pending-only","host":scope.host,"uid":scope.uid,
            "capacity":entries[0].description.value(),"build_slot":entries[1].description.value()});
        let bytes = aterm_json::to_vec(&value).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(refused("ingress intent exceeded bound"));
        }
        for entry in &mut entries {
            entry.lock.file.write_all(&bytes)?;
            entry.fence.write_all(&bytes)?;
        }
        for entry in &entries {
            entry.sync()?;
        }
        let prepared = Self {
            entries,
            bytes,
            scope,
            owner: std::process::id(),
        };
        prepared.observe()?;
        Ok(prepared)
    }

    /// Reopen only an exact complete pending intent and its original physical pairs.
    /// Both deterministic rendezvous locks remain held throughout subsequent operations.
    /// This restores only continue-fencing access, never lost process or CPU authority.
    /// # Errors
    /// Missing/partial/foreign intent, changed identities/ancestry, held lock or IO failure.
    pub fn reopen(layout: &Layout) -> io::Result<Self> {
        let scope = Scope::current()?;
        let checked = Layout::new(&layout.capacity, &layout.build_slot)?;
        let capacity = Location::open(&checked.capacity, Ingress::Capacity)?;
        let slots = Location::open(&checked.build_slot, Ingress::BuildSlot)?;
        let capacity_lock = Lock::take(&capacity.parent, Ingress::Capacity, false)?;
        let slots_lock = Lock::take(&slots.parent, Ingress::BuildSlot, false)?;
        let bytes = read(&capacity_lock.file)?;
        if read(&slots_lock.file)? != bytes {
            return Err(refused("paired ingress intents disagree"));
        }
        let value: Value = aterm_json::from_slice(&bytes).map_err(io::Error::other)?;
        fields(
            &value,
            &["schema", "state", "host", "uid", "capacity", "build_slot"],
        )?;
        if value.get("schema").and_then(Value::as_u64) != Some(1)
            || value.get("state").and_then(Value::as_str) != Some("pending-only")
            || aterm_json::to_vec(&value).map_err(io::Error::other)? != bytes
        {
            return Err(refused("unknown or noncanonical pending ingress intent"));
        }
        if value.get("host").and_then(Value::as_str) != Some(scope.host.as_str())
            || value.get("uid").and_then(Value::as_u64) != Some(u64::from(scope.uid))
        {
            return Err(refused(
                "pending ingress belongs to another host or effective account",
            ));
        }
        let capacity_description = Description::decode(
            value
                .get("capacity")
                .ok_or_else(|| refused("missing capacity intent"))?,
        )?;
        let slots_description = Description::decode(
            value
                .get("build_slot")
                .ok_or_else(|| refused("missing build-slot intent"))?,
        )?;
        if capacity_description.original == slots_description.original
            || capacity.ancestors.contains(&slots_description.original)
            || slots.ancestors.contains(&capacity_description.original)
        {
            return Err(refused("pending ingress scopes physically overlap"));
        }
        let prepared = Self {
            entries: [
                Entry::recover(capacity, capacity_lock, capacity_description)?,
                Entry::recover(slots, slots_lock, slots_description)?,
            ],
            bytes,
            scope,
            owner: std::process::id(),
        };
        prepared.observe()?;
        Ok(prepared)
    }

    /// Read exact current name pairs. Even two file fences mean pending, not drained.
    /// # Errors
    /// A foreign process holds this handle, or an original object, intent, path
    /// or ancestry changed or cannot be read.
    pub fn observe(&self) -> io::Result<PendingStatus> {
        // A fork's copied handle shares the original locks, so &mut self alone
        // cannot serialize its EXCHANGE against the still-owning parent.
        if self.owner != std::process::id() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if Scope::current()? != self.scope {
            return Err(refused("original ingress host/account scope changed"));
        }
        Ok(PendingStatus {
            capacity: self.entries[0].validate(&self.bytes)?,
            build_slot: self.entries[1].validate(&self.bytes)?,
        })
    }

    /// Fence at most one legacy path under both original rendezvous locks.
    /// Previously observed metadata/parents are synced before another exchange,
    /// including a retry after uncertain exchange durability. There is no rollback.
    /// # Errors
    /// Original validation, exchange or durability failed. This value keeps its held objects.
    pub fn fence_next(&mut self) -> io::Result<PendingStatus> {
        self.observe()?;
        for entry in &self.entries {
            entry.sync()?;
        }
        let status = self.observe()?;
        let index = if status.capacity == NameState::LegacyDirectory {
            Some(0)
        } else if status.build_slot == NameState::LegacyDirectory {
            Some(1)
        } else {
            None
        };
        if let Some(index) = index {
            let entry = &self.entries[index];
            fd::renameat_with(
                &entry.location.parent,
                entry.location.kind.name(),
                &entry.location.parent,
                entry.location.kind.prior(),
                RenameFlags::EXCHANGE,
            )?;
            aterm_durable::durable(&entry.location.parent)?;
        }
        self.observe()
    }

    /// Observe the preserved original directory, including writes through old descriptors.
    /// An empty snapshot is never predecessor drain, and this operation removes nothing.
    /// # Errors
    /// Original validation or bounded directory observation fails.
    pub fn predecessor(&self, kind: Ingress) -> io::Result<Predecessor> {
        let status = self.observe()?;
        let (index, state) = match kind {
            Ingress::Capacity => (0, status.capacity),
            Ingress::BuildSlot => (1, status.build_slot),
        };
        let entry = &self.entries[index];
        let mut contents = Contents::EmptySnapshot;
        for item in fd::Dir::read_from(entry.original.as_fd())? {
            let item = item?;
            if !matches!(item.file_name().to_bytes(), b"." | b"..") {
                contents = Contents::EntriesPresent;
                break;
            }
        }
        let path = if state == NameState::FileFence {
            entry.location.path.with_file_name(kind.prior())
        } else {
            entry.location.path.clone()
        };
        Ok(Predecessor {
            path,
            device: entry.description.original.0,
            inode: entry.description.original.1,
            contents,
        })
    }
}

#[cfg(test)]
#[path = "canonical_ingress_tests.rs"]
mod tests;
