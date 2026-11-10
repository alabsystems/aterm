// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit Cargo caches created by aterm, at the caller's chosen location.
//!
//! A name or a cache tag is not ownership. A fresh private stage and durable identity receipt
//! precede atomic target publication. An existing unrecorded target that proves cargo made it
//! as its build directory (cargo's own tag, which cargo writes into a build directory only as
//! it makes it, and the rustc facts a build keeps there) is recorded where it stands, its
//! contents and identity kept (the `adoption` module); every other one stays unmanaged and
//! keeps its output paths, and a producer requiring managed output refuses it.
//! The index avoids searching the person's folders.

use std::path::{Path, PathBuf};

#[path = "build_cache_package.rs"]
pub mod package;

#[path = "build_cache_create.rs"]
mod creation;

#[path = "build_cache_adopt.rs"]
mod adoption;

#[path = "build_cache_retain.rs"]
pub(super) mod retention;
pub use retention::CAPABILITY as RETAIN_CAPABILITY;
pub use retention::retain;

/// The internal front-door call a managed compiler shim makes before it execs Cargo.
pub const REGISTER_VERB: &str = "__build_cache_register";

/// Capability response for external producers before they create an explicit target.
pub const REGISTER_CAPABILITY: &str = "aterm-build-cache-register-v1";

/// Where the bounded receipts live, relative to the user's home.
pub const INDEX: &str = "Library/Caches/aterm/.ubd/registered-builds.noindex";

/// Presence declares registered-cache custody, even where its schema cannot be read.
/// Ordinary target cleanup must defer to the registered-cache authority.
pub const MARKER: &str = ".aterm-build-cache.json";
/// Either name declares managed custody, regardless of contents or supported schema.
/// Inspect these names before removing a directory's children.
pub const CUSTODY_MARKERS: &[&str] = &[MARKER, package::COORDINATOR];

/// A standing or unreadable custody declaration keeps ordinary cleaners out. Contents
/// deliberately do not matter: an older cleaner must also retain an unknown schema.
#[must_use]
pub fn custody_present(path: &Path) -> bool {
    CUSTODY_MARKERS.iter().any(|marker| {
        !std::fs::symlink_metadata(path.join(marker))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexStamp {
    identity: Identity,
    modified: std::time::SystemTime,
    changed: Option<(i64, i64)>,
}

/// A complete, disjoint snapshot of the existing registration index. This supplies no
/// removal authority itself; ordinary cleaners must still inspect filesystem custody.
#[derive(Debug)]
pub struct OrdinaryClearance {
    home: PathBuf,
    stamp: Option<IndexStamp>,
}

impl OrdinaryClearance {
    /// Supported receipt writers publish atomically in the index directory. A new,
    /// replaced or changed index revokes this snapshot without parsing every receipt again.
    /// This does not exclude arbitrary in-place edits by an unrelated writer.
    #[must_use]
    pub fn current(&self) -> bool {
        registry_stamp(&self.home).is_ok_and(|stamp| stamp == self.stamp)
    }
}

fn registry_stamp(home: &Path) -> Result<Option<IndexStamp>, String> {
    let mut at = home.to_path_buf();
    for component in Path::new(INDEX).components() {
        at.push(component);
        let metadata = match std::fs::symlink_metadata(&at) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("registered-cache index cannot be inspected".into()),
        };
        if !metadata.is_dir() || at.canonicalize().ok().as_ref() != Some(&at) {
            return Err("registered-cache index has an uncertain directory".into());
        }
        #[cfg(unix)]
        if identity(&at).is_none() {
            return Err("registered-cache index ownership is uncertain".into());
        }
    }
    let metadata = std::fs::symlink_metadata(&at).map_err(|_| "registered-cache index changed")?;
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt as _;
        Some((metadata.ctime(), metadata.ctime_nsec()))
    };
    #[cfg(not(unix))]
    let changed = None;
    Ok(Some(IndexStamp {
        identity: identity(&at).ok_or("registered-cache index ownership cannot be established")?,
        modified: metadata
            .modified()
            .map_err(|_| "registered-cache index clock is unknown")?,
        changed,
    }))
}

/// Establish that `path` does not overlap any published root, pending stage or package cache.
/// Unlike [`roots`], this is complete within the existing bound and never skips an unreadable
/// or unsupported receipt. Filesystem markers still require separate inspection.
///
/// # Errors
/// The path is relative, ambiguous or cannot be resolved; the index is uncertain, changes
/// during the read, exceeds its bound, contains an invalid receipt, or any recorded custody
/// overlaps the path.
pub fn ordinary_clearance(home: &Path, path: &Path) -> Result<OrdinaryClearance, String> {
    clearance(home, path, None)
}

fn resolve_clearance_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("ordinary cache assessment needs an absolute unambiguous path".into());
    }
    let mut missing = Vec::new();
    let mut cur = path;
    loop {
        match std::fs::symlink_metadata(cur) {
            Ok(_) => {
                let mut resolved = cur
                    .canonicalize()
                    .map_err(|_| "ordinary cache assessment path cannot be resolved".to_owned())?;
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cur.file_name() else {
                    return Err("ordinary cache assessment path cannot be resolved".into());
                };
                missing.push(name.to_os_string());
                cur = cur.parent().ok_or_else(|| {
                    "ordinary cache assessment path cannot be resolved".to_owned()
                })?;
            }
            Err(_) => {
                return Err("ordinary cache assessment path cannot be inspected".into());
            }
        }
    }
}

/// [`ordinary_clearance`], the index's record named `except` left out of the overlap census:
/// `path`'s own, which an adoption writes before its marker or replaces, and which its caller
/// already holds to name `path`. Every other entry is read.
fn clearance(
    home: &Path,
    path: &Path,
    except: Option<&std::ffi::OsStr>,
) -> Result<OrdinaryClearance, String> {
    let path = resolve_clearance_path(path)?;
    let home = home
        .canonicalize()
        .map_err(|_| "registered-cache home cannot be resolved")?;
    let stamp = registry_stamp(&home)?;
    if stamp.is_some() {
        let entries = std::fs::read_dir(home.join(INDEX))
            .map_err(|_| "registered-cache index cannot be listed")?;
        let mut records = 0;
        for entry in entries {
            let entry = entry.map_err(|_| "registered-cache index entry cannot be read")?;
            let record = entry.path();
            if entry.file_name() == "locks.noindex" && identity(&record).is_some() {
                continue;
            }
            records += 1;
            if records > MAX_ROOTS {
                return Err("registered-cache index exceeds its bound".into());
            }
            if record
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                return Err("registered-cache index contains an unknown entry".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                let metadata = std::fs::symlink_metadata(&record)
                    .map_err(|_| "registered-cache receipt cannot be inspected")?;
                if metadata.uid() != crate::platform::our_uid() || metadata.mode() & 0o022 != 0 {
                    return Err("registered-cache receipt ownership is uncertain".into());
                }
            }
            let receipt = read(&record)
                .ok_or("registered-cache index contains an unreadable or unsupported receipt")?;
            if except.is_some_and(|own| own == entry.file_name()) {
                continue;
            }
            let held = std::iter::once(receipt.root.as_path())
                .chain(
                    receipt
                        .pending
                        .as_ref()
                        .map(|pending| pending.directory.as_path()),
                )
                .chain(
                    receipt
                        .creating
                        .as_ref()
                        .map(|creating| creating.directory.as_path()),
                )
                .chain(receipt.package.as_ref().map(package::Policy::cache_root));
            for held in held {
                if !scope(&home, held) {
                    return Err("registered-cache receipt has uncertain custody paths".into());
                }
                if path.starts_with(held) || held.starts_with(&path) {
                    return Err("registered-cache custody overlaps this path".into());
                }
            }
        }
    }
    let clearance = OrdinaryClearance { home, stamp };
    if !clearance.current() {
        return Err("registered-cache index changed during assessment".into());
    }
    Ok(clearance)
}

const MAX_RECORD: usize = 64 << 10;
const MAX_ROOTS: usize = 4096;
const MAX_OWNERS: usize = 64;

#[path = "build_cache_volume.rs"]
pub(super) mod volume;

/// A folder (or file) as custody binds it: its inode on its volume.
///
/// THE DEVICE NUMBER IS THE MOUNT'S (VOLUME-IDENTITY, 2026-11-10): macOS numbers a volume as it
/// mounts it, so after a restart the same volume, folder and inode read with another device
/// number, and another volume can read with the number a record kept ([`volume`]). A record that
/// keeps its volume's UUID binds the volume by that UUID ([`Identity::names`]); one written before
/// records kept it (no `volume`), or of a volume with none, binds by the device number alone, as
/// every record did, and so binds nothing once its volume is renumbered ([`renumbered`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Identity {
    /// The device number of the volume where this was read, as that mount numbered it.
    pub(super) device: u64,
    pub(super) inode: u64,
    /// The persistent UUID of the volume where this was read (`ATTR_VOL_UUID`, [`volume`]), where
    /// it was read and the volume has one. Absent off macOS, and in every record written before
    /// records kept it; an older reader ignores it and binds by the device number, as it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) volume: Option<volume::Uuid>,
}

impl Identity {
    /// What a read gives with its volume unread: an identity compared only with another of this
    /// boot, by its device number, and never recorded (a record keeps the volume: [`identity`],
    /// [`Identity::held`]).
    #[cfg(unix)]
    pub(super) const fn unrecorded(device: u64, inode: u64) -> Self {
        Self {
            device,
            inode,
            volume: None,
        }
    }

    /// The folder or file held open as `fd`, read now, its volume included. `None` where either
    /// read fails.
    #[cfg(unix)]
    pub(super) fn held(fd: impl std::os::fd::AsFd) -> Option<Self> {
        let stat = aterm_dirfd::fstat(fd.as_fd()).ok()?;
        Some(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            volume: volume::held(fd.as_fd()).ok()?,
        })
    }

    /// The entry `name` of the folder held as `folder`, read now as `stat` (`fstatat`, a link as
    /// itself), its volume read by that name; `None` where that read fails or the name no longer
    /// reads as `stat` once it is done.
    #[cfg(unix)]
    pub(super) fn at(
        folder: aterm_dirfd::BorrowedFd<'_>,
        name: &std::ffi::OsStr,
        stat: &aterm_dirfd::Stat,
    ) -> Option<Self> {
        let volume = volume::at(folder, name).ok()?;
        let again =
            aterm_dirfd::fstatat(folder, name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW).ok()?;
        ((again.st_dev, again.st_ino) == (stat.st_dev, stat.st_ino)).then_some(Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            volume,
        })
    }

    /// WHETHER THIS IDENTITY, AS RECORDED (in another boot, perhaps), NAMES THE FOLDER READ NOW AS
    /// `now`: the same inode on the same volume. Where it keeps the volume's UUID, `now` must be
    /// on the volume of that UUID, whatever its device number now (another boot numbers a volume
    /// anew), so another volume is refused though it reads with the device number recorded, and so
    /// is a folder whose volume reads with no UUID now. Where it keeps none, only the device number
    /// recorded can say, as before records kept the UUID: `now` must read with it.
    pub(super) fn names(&self, now: &Self) -> bool {
        self.inode == now.inode
            && match &self.volume {
                Some(volume) => now.volume.as_ref() == Some(volume),
                None => self.device == now.device,
            }
    }

    /// Whether two records name one folder (a marker and its receipt, a receipt and the copy a
    /// pending stage keeps): the same inode, and one volume, by its UUID where both keep it, by
    /// the device number where either does not (one written before the other was witnessed:
    /// [`Identity::witnessed`] never changes the device number recorded).
    pub(super) fn same_record(&self, other: &Self) -> bool {
        self.inode == other.inode && self.same_volume(other)
    }

    /// Whether two records name folders on one volume: by its UUID where both keep it, by the
    /// device number where either does not.
    pub(super) fn same_volume(&self, other: &Self) -> bool {
        match (&self.volume, &other.volume) {
            (Some(a), Some(b)) => a == b,
            _ => self.device == other.device,
        }
    }

    /// THIS RECORD WITH ITS VOLUME WITNESSED (VOLUME-WITNESS): a record that keeps no UUID but
    /// still names `now` by its device number (this boot has not renumbered its volume) gains
    /// `now`'s volume UUID, its device number and inode kept as recorded, so it goes on naming
    /// the folder once a restart renumbers the volume. `None` where it keeps one already, where
    /// `now` has none, or where it does not name `now`.
    #[cfg(unix)]
    pub(super) fn witnessed(&self, now: &Self) -> Option<Self> {
        (self.volume.is_none() && now.volume.is_some() && self.names(now)).then(|| Self {
            volume: now.volume,
            ..self.clone()
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Receipt {
    schema: u32,
    /// Schema 4 is keep-only authority. Older readers refuse it rather than ignore a
    /// preservation field and apply their normal stale-unit or whole-root policy.
    #[serde(default)]
    retained: bool,
    /// A removal's provisional keep. The private token exempts only that exact live walk;
    /// an explicit keep clears it before acknowledgement. Older schema4 readers retain it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    removal_fence: Option<retention::FenceDeclaration>,
    root: PathBuf,
    identity: Identity,
    nonce: String,
    /// Schema1 tracks all known owners; schema2 retains bounded diagnostic references and
    /// binds each admitted producer to its exact active package attempt instead.
    owners: Vec<PathBuf>,
    /// Unresolved producers or a schema1 roster overflow prevent whole-cache retirement.
    /// Historical schema2 overflows remain unknown too; no existing flag is cleared.
    owners_unknown: bool,
    #[serde(default)]
    pending: Option<Pending>,
    /// Schema5 is an unfinished creation, never an admitted writable cache. Older readers
    /// refuse it rather than treating its final name as an unmanaged existing target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    creating: Option<creation::Creating>,
    /// Package schema2 retains old producer custody; guarded schema3 makes old reclaimers
    /// refuse rather than silently discard the preservation contract.
    #[serde(default)]
    package: Option<package::Policy>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Pending {
    directory: PathBuf,
    identity: Identity,
    /// Set durably only after the staged payload matched the admitted root. A later pass
    /// may then finish a partially removed payload whose marker has already gone.
    #[serde(default)]
    authorized: bool,
}

/// A cache whose creation receipt still names the same private directory and marker.
#[derive(Clone, Debug)]
pub struct Registered {
    pub root: PathBuf,
    pub owners: Vec<PathBuf>,
    pub owners_unknown: bool,
    receipt_path: PathBuf,
    identity: Identity,
    nonce: String,
}

impl Registered {
    fn matches(&self, receipt: &Receipt) -> bool {
        receipt.root == self.root
            && receipt.identity == self.identity
            && receipt.nonce == self.nonce
            && receipt.owners == self.owners
            && receipt.owners_unknown == self.owners_unknown
    }

    /// Whether this still names the same active cache and owners under its creation receipt.
    /// Retired stages and a replaced root are not current, even when [`roots`] retains their
    /// receipts so the reclaimer can finish them. This read does not authorize a removal.
    #[must_use]
    pub fn is_current(&self, home: &Path) -> bool {
        read(&self.receipt_path).is_some_and(|receipt| {
            self.matches(&receipt) && receipt.pending.is_none() && same(home, &receipt)
        })
    }
}

#[cfg(unix)]
fn identity(path: &Path) -> Option<Identity> {
    identity_for(path, euid())
}

/// This process's effective user, the one whose private folders custody binds ([`identity`]).
#[cfg(unix)]
fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and always returns the current effective uid.
    unsafe { libc::geteuid() }
}

/// The private folder at `path` (a link at it read as itself), its volume included: the volume
/// is read by the path as the folder is (`FSOPT_NOFOLLOW`, which asks no more of the path than
/// `lstat` does), and the folder read again after it, so a folder swapped at the path between the
/// two reads is no identity.
#[cfg(unix)]
fn identity_for(path: &Path, uid: u32) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() || !private(metadata.uid(), metadata.mode(), uid) {
        return None;
    }
    let volume = volume::at(aterm_dirfd::CWD, path).ok()?;
    let again = std::fs::symlink_metadata(path).ok()?;
    ((again.dev(), again.ino()) == (metadata.dev(), metadata.ino())).then(|| Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        volume,
    })
}

/// The private folder at `path` as [`identity`] judges it, its volume left unread: the identities
/// a package declaration records (its coordinator folder and its preserved path's folder), which
/// keep binding by the device number alone, as they did (VOLUME-IDENTITY's open item).
#[cfg(unix)]
fn boot_local(path: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    (metadata.is_dir() && private(metadata.uid(), metadata.mode(), euid()))
        .then(|| Identity::unrecorded(metadata.dev(), metadata.ino()))
}

/// Whether a folder owned by `owner`, its mode `mode`, is one custody binds for the user `uid`:
/// that user's own, and writable by no group or other user. A marker in any other folder binds
/// none, read by its path ([`identity`]) or through the folder held
/// ([`retention::allows_at`]).
#[cfg(unix)]
const fn private(owner: u32, mode: u32, uid: u32) -> bool {
    owner == uid && mode & 0o022 == 0
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Option<Identity> {
    None
}

fn scope(home: &Path, path: &Path) -> bool {
    path.is_absolute()
        && path != home
        && path.starts_with(home)
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && super::checkout::never_looked_at(path, Some(home)).is_none()
        && !crate::protected::under_app_data(home, path)
        && !crate::protected::under_file_provider_domain(home, path)
}

/// Establish durable reachability of the fixed index and gates before publishing any target.
/// Existing modes stay unchanged and no directory symlink is followed.
#[cfg(unix)]
fn index(home: &Path) -> Option<PathBuf> {
    let mut at = home.to_path_buf();
    let mut held = creation::pin(home)?;
    for component in Path::new(INDEX).components() {
        at.push(component);
        held = directory_child(&held, component.as_os_str(), &at, creation::Point::Index)?;
    }
    let gates = at.join("locks.noindex");
    directory_child(
        &held,
        std::ffi::OsStr::new("locks.noindex"),
        &gates,
        creation::Point::Index,
    )?;
    Some(at)
}

#[cfg(unix)]
fn directory_child(
    parent: &aterm_dirfd::OwnedFd,
    name: &std::ffi::OsStr,
    path: &Path,
    point: creation::Point,
) -> Option<aterm_dirfd::OwnedFd> {
    match aterm_dirfd::mkdirat(parent, name, aterm_dirfd::Mode::from_bits(0o700)) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == libc::EEXIST => {}
        Err(_) => return None,
    }
    let child = creation::pin(path)?;
    let captured = aterm_dirfd::fstat(&child).ok()?;
    if !creation::named(
        parent,
        name,
        &Identity::unrecorded(captured.st_dev, captured.st_ino),
    ) {
        return None;
    }
    // Synchronize even an existing component: a previous attempt may have stopped after mkdir.
    creation::sync_directory(&child, point)?;
    creation::sync_directory(parent, point)?;
    Some(child)
}

/// The person's caches folder under a home, in which the index and the shared build cache sit:
/// the folders below it that the two share are aterm's own ([`lay_shared`]); it is not.
#[cfg(unix)]
const CACHES: &str = "Library/Caches";

/// LAY THE FOLDERS THE INDEX SHARES WITH THE SHARED BUILD CACHE, PRIVATE, BEFORE A BUILD CAN: a
/// build into the shared cache ([`super::CACHE_UNDER_HOME`]) makes each folder of its path it
/// does not find with the build's umask, so under umask 002 `Library/Caches/aterm` and its `.ubd`
/// would be left writable by the group, and the index, which binds only private folders
/// ([`index`]), could never be laid under them: no explicit target would be registered, and none
/// retired once its owner is gone. Each missing folder of the path the two share is made here at
/// 0o700, never through a link. One that stands is left as it is, as [`index`] leaves it, but
/// for aterm's own below [`CACHES`] that a build made writable by the group or others before
/// this was laid: write is taken from them there ([`seal`]), so a home such a build already
/// reached registers explicit targets again.
#[cfg(unix)]
pub(super) fn lay_shared(home: &Path) {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let caches = home.join(CACHES);
    let mut at = home.to_path_buf();
    let shared = Path::new(INDEX)
        .components()
        .zip(Path::new(super::CACHE_UNDER_HOME).components())
        .take_while(|(index, cache)| index == cache);
    for (component, _) in shared {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Ok(metadata) if metadata.is_dir() => {
                if at.starts_with(&caches)
                    && at != caches
                    && metadata.uid() == euid()
                    && metadata.mode() & 0o022 != 0
                {
                    seal(&at);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::DirBuilder::new().mode(0o700).create(&at).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// TAKE WRITE FROM THE GROUP AND OTHERS on aterm's own folder `at` ([`lay_shared`]), held: opened
/// never through a link, and changed through that descriptor, never by its name, only while the
/// folder held is this user's own; every other bit of its mode stays as it stands.
#[cfg(unix)]
fn seal(at: &Path) {
    use aterm_dirfd::{self as dirfd, Mode, OFlags};
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let Ok(fd) = dirfd::openat(dirfd::CWD, at, flags, Mode::empty()) else {
        return;
    };
    let Ok(held) = dirfd::fstat(&fd) else {
        return;
    };
    let bits = held.st_mode & 0o7777;
    if held.st_uid == euid()
        && bits & 0o022 != 0
        && let Ok(bits) = libc::mode_t::try_from(bits & !0o022)
    {
        let _ = dirfd::fchmod(&fd, Mode::from_bits(bits));
    }
}

#[cfg(not(unix))]
pub(super) const fn lay_shared(_: &Path) {}

fn gate_path(receipt: &Path) -> Option<PathBuf> {
    Some(
        receipt
            .parent()?
            .join("locks.noindex")
            .join(receipt.file_name()?)
            .with_extension("lock"),
    )
}

/// Called under the admission lock. Never unlink a gate another process might already have
/// opened: bound retained gates too, and leave further fresh paths unmanaged at that limit.
#[cfg(unix)]
fn open_gate(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::read_dir(path.parent()?)
                .ok()?
                .take(MAX_ROOTS)
                .count()
                >= MAX_ROOTS
            {
                return None;
            }
        }
        Err(_) => return None,
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    aterm_durable::ordered(&file).ok()?;
    let parent = creation::pin(path.parent()?)?;
    creation::sync_directory(&parent, creation::Point::Gate)?;
    Some(file)
}

#[cfg(unix)]
fn key(path: &Path) -> Option<String> {
    Some(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(path.to_str()?.as_bytes()),
    ))
}

fn read(path: &Path) -> Option<Receipt> {
    let bytes = crate::metadata_io::read_bounded_regular(path, MAX_RECORD).ok()?;
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Option<Receipt> {
    let receipt: Receipt = aterm_json::from_slice(bytes).ok()?;
    (((receipt.schema == 5
        && !receipt.retained
        && receipt.creating.is_some()
        && receipt.pending.is_none())
        || (receipt.schema == 4 && receipt.retained && receipt.creating.is_none())
        || (!receipt.retained
            && receipt.creating.is_none()
            && ((receipt.schema == 1 && receipt.package.is_none())
                || receipt
                    .package
                    .as_ref()
                    .is_some_and(|policy| policy.valid() && receipt.schema == policy.schema()))))
        && receipt.package.as_ref().is_none_or(package::Policy::valid)
        && receipt
            .removal_fence
            .as_ref()
            .is_none_or(|fence| receipt.schema == 4 && receipt.retained && fence.valid())
        && receipt.owners.len() <= MAX_OWNERS
        && receipt.nonce.len() == 32
        && receipt.nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(receipt)
}

fn same(home: &Path, receipt: &Receipt) -> bool {
    receipt.creating.is_none()
        && scope(home, &receipt.root)
        && std::fs::canonicalize(&receipt.root).ok().as_ref() == Some(&receipt.root)
        && identity(&receipt.root).is_some_and(|now| receipt.identity.names(&now))
        && [".git", "Cargo.toml", "Targo.toml"].iter().all(|name| {
            std::fs::symlink_metadata(receipt.root.join(name))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
        && read(&receipt.root.join(MARKER)).is_some_and(|marker| marker_binds(&marker, receipt))
}

/// The marker declares identity and package policy; producer rosters, Pending and evolving
/// package state belong to the index and need not equal the creation-time marker.
fn marker_binds(marker: &Receipt, receipt: &Receipt) -> bool {
    marker.root == receipt.root
        && marker.identity.same_record(&receipt.identity)
        && marker.nonce == receipt.nonce
        && marker.schema == receipt.schema
        && package::same_declaration(marker.package.as_ref(), receipt.package.as_ref())
}

/// RECORD THE VOLUME OF THE FOLDER A RECEIPT WITHOUT ONE BINDS (VOLUME-WITNESS), while this boot
/// still numbers its volume as the receipt recorded it: the folder's marker first, read and
/// rewritten through the folder held, under its publication lock (which a removal's walk takes
/// to arm its fence), then the receipt at `path`, each keeping the device number and inode it
/// recorded and gaining the volume's UUID ([`Identity::witnessed`]), so both go on naming the
/// folder once a restart renumbers its volume. A receipt that keeps a UUID already is left as it
/// is; so is any but an ordinary settled one (pending, being created, a keep or a package's) or
/// one whose marker does not bind it or is a keep or a removal's fence. Called holding the
/// receipt's gate exclusively. Whether `*receipt`, what is on record at `path`, keeps its
/// volume's UUID once done; a marker rewritten before the receipt failed still binds it
/// ([`marker_binds`]: the device numbers they keep are the same).
#[cfg(unix)]
fn witness(home: &Path, path: &Path, receipt: &mut Receipt) -> bool {
    if receipt.identity.volume.is_some() {
        return true;
    }
    if receipt.retained
        || receipt.removal_fence.is_some()
        || receipt.creating.is_some()
        || receipt.pending.is_some()
        || receipt.package.is_some()
        || !same(home, receipt)
        || read(path).as_ref() != Some(receipt)
    {
        return false;
    }
    let Some(held) = creation::pin(&receipt.root) else {
        return false;
    };
    let Some(identity) = Identity::held(&held).and_then(|now| receipt.identity.witnessed(&now))
    else {
        return false;
    };
    let Some(_publication) = retention::publication_lock(&held) else {
        return false;
    };
    let Some(mut marker) = retention::receipt_at(&held) else {
        return false;
    };
    if marker.retained || marker.removal_fence.is_some() || !marker_binds(&marker, receipt) {
        return false;
    }
    if marker.identity.volume.is_none() {
        marker.identity = Identity {
            volume: identity.volume,
            ..marker.identity
        };
        if retention::write_marker(&held, &marker).is_err() {
            return false;
        }
    }
    let witnessed = Receipt {
        identity,
        ..receipt.clone()
    };
    if write(path, &witnessed).is_none() || read(path).as_ref() != Some(&witnessed) {
        return false;
    }
    *receipt = witnessed;
    same(home, receipt)
}

/// [`witness`] the receipt of `root` where it keeps no volume UUID yet, for a producer that found
/// its owner on record already ([`recorded_while`]) and so writes nothing else: only where the
/// admission lock and the root's gate are both free at once, held as [`create_with_policy`] holds
/// them to write, so no producer waits for this or is refused by it (one that finds the gate held
/// waits on the admission lock, then finds it free); skipped otherwise, for a later producer or
/// the reclaim pass ([`reclaim_selected_on`]) to do.
#[cfg(unix)]
fn witness_soon(home: &Path, root: &Path) {
    let directory = home.join(INDEX);
    let Some(key) = key(root) else {
        return;
    };
    let path = directory.join(format!("{key}.json"));
    if read(&path).is_none_or(|receipt| receipt.identity.volume.is_some()) {
        return;
    }
    let (Ok(admission), Some(gate)) = (
        crate::metadata_io::open_regular(&directory.join("locks.noindex/admission.lock")),
        gate_path(&path),
    ) else {
        return;
    };
    let Ok(_admission) = crate::lock::Flock::try_lock(admission) else {
        return;
    };
    let Ok(gate) = crate::metadata_io::open_regular(&gate) else {
        return;
    };
    let Ok(_held) = crate::lock::Flock::try_lock(gate) else {
        return;
    };
    if let Some(mut receipt) = read(&path)
        && receipt.root == root
    {
        witness(home, &path, &mut receipt);
    }
}

/// WHETHER `receipt` NAMES ITS FOLDER IN ALL BUT THE DEVICE NUMBER (VOLUME-IDENTITY): a receipt
/// that keeps no volume UUID (written before records kept one, and not witnessed since), whose
/// folder stands at its canonical path, private, with the inode it recorded and the marker that
/// binds it, but on a volume this boot numbers otherwise, as a restart renumbers one. Nothing it
/// recorded can prove that volume the one it was written on, so it binds nothing ([`same`]) and
/// stays refused; the device number its folder reads with now, for [`renumbered_words`].
#[cfg(unix)]
fn renumbered(home: &Path, receipt: &Receipt) -> Option<u64> {
    if receipt.identity.volume.is_some()
        || receipt.creating.is_some()
        || !scope(home, &receipt.root)
        || std::fs::canonicalize(&receipt.root).ok().as_ref() != Some(&receipt.root)
    {
        return None;
    }
    let now = identity(&receipt.root)?;
    (now.inode == receipt.identity.inode
        && now.device != receipt.identity.device
        && read(&receipt.root.join(MARKER)).is_some_and(|marker| marker_binds(&marker, receipt)))
    .then_some(now.device)
}

/// What a producer is told of a [`renumbered`] receipt: why it is refused, and what to do.
#[cfg(unix)]
fn renumbered_words(receipt: &Receipt, now: u64) -> String {
    let root = receipt.root.display();
    format!(
        "the managed build cache {root} was recorded on device {}, and its volume reads as device \
         {now} now (a restart renumbers a volume); its record predates atpkg's record of a \
         volume's own identity, so nothing proves it the same folder and it stays refused: \
         remove {root} (a build cache atpkg made; the next build makes it again) or build into \
         another target directory",
        receipt.identity.device
    )
}

#[cfg(unix)]
fn write(path: &Path, receipt: &Receipt) -> Option<()> {
    use std::io::Write as _;
    let parent = path.parent()?;
    let stage = aterm_tempfile::Builder::new()
        .prefix("cache-receipt-")
        .tempdir_in(parent)
        .ok()?;
    let pending = stage.path().join("receipt");
    let mut file = crate::platform::open_create_new_write(&pending, 0o600).ok()?;
    let bytes = aterm_json::to_vec(receipt).ok()?;
    if bytes.len() > MAX_RECORD {
        return None;
    }
    file.write_all(&bytes).ok()?;
    aterm_durable::ordered(&file).ok()?;
    std::fs::rename(pending, path).ok()?;
    aterm_durable::ordered_dir(parent).ok()
}

#[cfg(unix)]
fn pending_copy(index: &Path, root_key: &str, receipt: &Receipt) -> Option<()> {
    let pending = receipt.pending.as_ref()?;
    let stage_key = key(&pending.directory)?;
    let path = index.join(format!(
        "{root_key}-{}-{stage_key}.retired.json",
        receipt.nonce
    ));
    if let Some(before) = read(&path) {
        return (before.nonce == receipt.nonce
            && before.identity.same_record(&receipt.identity)
            && before.pending.as_ref().is_some_and(|before| {
                before.directory == pending.directory
                    && before.identity.same_record(&pending.identity)
            }))
        .then_some(());
    }
    if std::fs::symlink_metadata(&path).is_ok() || record_count(index)? >= MAX_ROOTS {
        return None;
    }
    // A separate stage receipt survives publishing a replacement at the original target.
    // Its own gate exists before the receipt can become visible to a reclaimer.
    let file = open_gate(&gate_path(&path)?)?;
    let _held = crate::lock::Flock::try_lock(file).ok()?;
    write(&path, receipt)
}

#[cfg(unix)]
fn record_count(index: &Path) -> Option<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(index).ok()? {
        let entry = entry.ok()?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            count += 1;
            if count >= MAX_ROOTS {
                break;
            }
        }
    }
    Some(count)
}

/// Contained producers need no new write authority once their owner or exact guarded
/// package attempt was published. The shared gate excludes retirement during this read.
#[cfg(unix)]
fn already_recorded(home: &Path, root: &Path, workspace: &Path) -> bool {
    recorded_while(home, root, workspace, &|| true)
}

/// [`already_recorded`], where `still` must also hold under the root's shared gate, as its
/// receipt is read: a target named through a link ([`Linked::still`]) is re-checked there.
#[cfg(unix)]
fn recorded_while(home: &Path, root: &Path, workspace: &Path, still: &dyn Fn() -> bool) -> bool {
    let directory = home.join(INDEX);
    let gates = directory.join("locks.noindex");
    if identity(&directory).is_none()
        || identity(&gates).is_none()
        || std::fs::canonicalize(&gates).ok().as_ref() != Some(&gates)
    {
        return false;
    }
    let Some(key) = key(root) else {
        return false;
    };
    let path = directory.join(format!("{key}.json"));
    let Some(gate) = gate_path(&path) else {
        return false;
    };
    let Ok(file) = crate::metadata_io::open_regular(&gate) else {
        return false;
    };
    let Ok(_held) = crate::lock::Flock::try_lock_shared(file) else {
        return false;
    };
    still()
        && read(&path).is_some_and(|receipt| {
            receipt.root == root
                && receipt.pending.is_none()
                && same(home, &receipt)
                && match &receipt.package {
                    Some(policy) => {
                        !receipt.owners_unknown && package::producer_allows(home, policy, workspace)
                    }
                    None => {
                        receipt.owners_unknown
                            || receipt.owners.iter().any(|owner| owner == workspace)
                    }
                }
        })
}

/// A producer must not continue on a previously owned cache if it cannot be admitted.
/// Ownership begins by a fresh staged creation and atomic publication, or, for an existing
/// target that proves cargo made it, unrecorded or recorded only for a folder no longer there,
/// by recording it where it stands (the `adoption` module); existing output never moves. Any
/// other unmanaged existing path is a successful observation with `false`.
///
/// # Errors
/// The registration gate was busy or an admitted cache's owner could not be published.
#[cfg(unix)]
pub fn create_checked(home: &Path, root: &Path, workspace: &Path) -> std::io::Result<bool> {
    create_with_policy(home, root, workspace, None, true).map(|(recorded, _)| recorded)
}

/// Admit `workspace`'s producer to the cache `root`, creating it when it is absent, and
/// recording a cargo-made existing one where it stands when `adopt` ([`create_checked`]'s
/// producers; a package target is only ever created fresh).
#[cfg(unix)]
fn create_with_policy(
    home: &Path,
    root: &Path,
    workspace: &Path,
    policy: Option<package::Policy>,
    adopt: bool,
) -> std::io::Result<(bool, bool)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut must_record = false;
    let mut created = false;
    let mut refusal = None;
    let mut attempt = || -> Option<()> {
        if !scope(home, root) || !scope(home, workspace) {
            return None;
        }
        // An existing ownership marker makes publication mandatory even when the index
        // itself became unreadable. Unknown ordinary targets have no such declaration.
        let declared = match std::fs::symlink_metadata(root.join(MARKER)) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        };
        must_record = declared;
        let parent = root.parent()?;
        if !canonical_existing_ancestor(home, parent)
            || std::fs::canonicalize(workspace).ok().as_deref() != Some(workspace)
            || !super::checkout::MANIFESTS.iter().any(|name| {
                std::fs::symlink_metadata(workspace.join(name))
                    .is_ok_and(|metadata| metadata.is_file())
            })
        {
            return None;
        }
        if already_recorded(home, root, workspace) {
            if policy.is_some() && read(&root.join(MARKER)).is_some_and(|receipt| receipt.retained)
            {
                return None;
            }
            witness_soon(home, root);
            return Some(());
        }
        let index = index(home)?;
        // Serialize admission and archived-stage publication as well as each root. Counting
        // only JSON records bounds the index even when several independent builds start.
        let admission = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(index.join("locks.noindex/admission.lock"))
            .ok()?;
        if !admission.metadata().ok()?.is_file() {
            return None;
        }
        let _admission = crate::lock::Flock::lock(admission).ok()?;
        let key = key(root)?;
        let path = index.join(format!("{key}.json"));
        let lock_path = gate_path(&path)?;
        let lock = open_gate(&lock_path)?;
        must_record = true;
        // The record may be between fresh creation and publication, so a busy gate is never
        // permission to use the directory without recording this producer's owner.
        let _held = crate::lock::Flock::try_lock(lock).ok()?;
        must_record = declared;
        let mut before = read(&path);
        match std::fs::symlink_metadata(&path) {
            Ok(_) if before.is_none() => {
                must_record = true;
                return None;
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                must_record = true;
                return None;
            }
            _ => {}
        }
        if before.as_ref().is_some_and(|before| before.root != root) {
            must_record = true;
            return None;
        }
        if before
            .as_ref()
            .is_some_and(|before| before.creating.is_some())
        {
            must_record = true;
            let interrupted = before.as_ref()?;
            if !match (&interrupted.package, &policy) {
                (None, None) => true,
                (Some(before), Some(requested)) => before.same_creation(requested),
                _ => false,
            } {
                return None;
            }
            before = Some(creation::finish(home, &path, interrupted)?);
            created = true;
        }
        if let Some(before) = before.as_ref()
            && before.pending.is_some()
        {
            must_record = true;
            if before.package.is_some() {
                return None;
            }
            pending_copy(&index, &key, before)?;
            must_record = declared;
        }
        if std::fs::symlink_metadata(&path).is_err() && record_count(&index)? >= MAX_ROOTS {
            return None;
        }
        let mut receipt = match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                must_record = true;
                created = true;
                creation::begin(home, &path, root, workspace, policy.clone())?
            }
            Ok(_) => {
                let mut receipt = match before.or_else(|| read(&path)) {
                    Some(receipt) if receipt.root != root => return None,
                    Some(mut receipt) if same(home, &receipt) => {
                        witness(home, &path, &mut receipt);
                        receipt
                    }
                    Some(receipt) if renumbered(home, &receipt).is_some() => {
                        must_record = true;
                        refusal =
                            renumbered(home, &receipt).map(|now| renumbered_words(&receipt, now));
                        return None;
                    }
                    // Only an adoption puts its receipt on record before its marker binds it, and
                    // an ordinary receipt of a folder no longer here binds nothing here.
                    Some(receipt) if adopt && !declared => {
                        let proven = adoption::proven(home, root, &path)?;
                        if proven.resumes(&receipt) {
                            must_record = true;
                            adoption::finish(home, &receipt, &proven)?
                        } else if proven.supersedes(&receipt) {
                            must_record = true;
                            adoption::begin(home, &path, workspace, &proven)?
                        } else {
                            return None;
                        }
                    }
                    None if adopt && !declared => {
                        let proven = adoption::proven(home, root, &path)?;
                        must_record = true;
                        adoption::begin(home, &path, workspace, &proven)?
                    }
                    Some(_) | None => return None,
                };
                if receipt.retained && policy.is_some() {
                    return None;
                }
                must_record = true;
                if receipt.package.is_some() && receipt.owners_unknown {
                    return None;
                }
                if policy.is_none()
                    && receipt
                        .package
                        .as_ref()
                        .is_some_and(|policy| !package::producer_allows(home, policy, workspace))
                {
                    return None;
                }
                if receipt.pending.take().is_some() {
                    write(&path, &receipt)?;
                }
                receipt
            }
            Err(_) => return None,
        };
        if receipt.owners.iter().any(|owner| owner == workspace) {
            return Some(());
        } else {
            if receipt.owners.len() < MAX_OWNERS {
                receipt.owners.push(workspace.to_path_buf());
            } else if receipt.package.is_none() {
                receipt.owners_unknown = true;
            }
        }
        write(&path, &receipt)
    };
    if attempt().is_some() {
        Ok((true, created))
    } else if must_record {
        Err(std::io::Error::other(refusal.unwrap_or_else(|| {
            "the managed build cache's owner could not be recorded".to_owned()
        })))
    } else {
        Ok((false, false))
    }
}

#[cfg(not(unix))]
pub fn create_checked(_: &Path, _: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// WHY AN EXPLICIT TARGET NAMED THROUGH A LINK IS NOT OBSERVED AS OWNED ([`through_link`],
/// [`admit_linked`]), each refusal in its own words.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkRefusal {
    /// The link names nothing.
    Dangling,
    /// The folder it names cannot be inspected.
    Unreachable,
    /// It names another link, or reaches its folder through one.
    Chain,
    /// It names something that is no folder.
    NotFolder,
    /// Its folder stands on another volume than the link.
    OtherVolume,
    /// Its folder is no registered root of this home and no build directory cargo made.
    Unowned,
    /// Its folder proves cargo made it, but atpkg never registered it.
    CargoMade,
    /// Its folder declares custody, or the index records it, but no receipt binds it here.
    Foreign,
    /// Its folder's receipt names it in all but the device number, recorded before atpkg recorded
    /// a volume's own identity, on a volume a restart has renumbered since ([`renumbered`]).
    Renumbered,
    /// The link, or the folder it names, changed between its observation and the re-check
    /// under the folder's gate.
    Changed,
}

#[cfg(unix)]
impl LinkRefusal {
    const fn why(self) -> &'static str {
        match self {
            Self::Dangling => "the explicit target is a link to no folder (a dangling link)",
            Self::Unreachable => "the explicit target is a link to a folder atpkg cannot inspect",
            Self::Chain => {
                "the explicit target is a link that reaches its folder through another link (a link chain)"
            }
            Self::NotFolder => "the explicit target is a link to something that is not a folder",
            Self::OtherVolume => "the explicit target is a link to a folder on another volume",
            Self::Unowned => "the explicit target is a link to a folder atpkg does not own",
            Self::CargoMade => {
                "the explicit target is a link to a folder cargo made that atpkg never registered; atpkg adopts a folder only by its own name, never through a link"
            }
            Self::Foreign => {
                "the explicit target is a link to a folder whose custody atpkg cannot confirm"
            }
            Self::Renumbered => {
                "the explicit target is a link to a build cache recorded before atpkg recorded a volume's own identity, on a volume a restart has renumbered since, so nothing proves it the same folder: remove the folder it names (the next build makes it again) or build into another target directory"
            }
            Self::Changed => "the explicit target's link changed while atpkg observed it",
        }
    }
}

/// AN EXPLICIT TARGET NAMED THROUGH A LINK, resolved one hop ([`through_link`]): the name the call
/// used and the folder it names, each as observed.
#[cfg(unix)]
struct Linked {
    /// The link itself, in a canonical folder of HOME.
    link: PathBuf,
    /// The link's own device and inode (it is not followed).
    node: (u64, u64),
    /// What the link says.
    text: PathBuf,
    /// The folder it names: canonical, a folder (no link) of this user's, on the link's volume.
    real: PathBuf,
    /// That folder's identity.
    identity: Identity,
}

#[cfg(unix)]
impl Linked {
    /// Whether the link still names the folder observed: the same link (its device, inode and
    /// text), which the kernel resolves to the same canonical folder of the same identity.
    fn still(&self) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        link_seam::rechecking(&self.link);
        std::fs::symlink_metadata(&self.link).is_ok_and(|meta| {
            meta.file_type().is_symlink() && (meta.dev(), meta.ino()) == self.node
        }) && std::fs::read_link(&self.link).is_ok_and(|text| text == self.text)
            && std::fs::canonicalize(&self.link).is_ok_and(|real| real == self.real)
            && identity(&self.real).as_ref() == Some(&self.identity)
    }
}

/// THE TESTS' CALL AT EACH RE-CHECK OF A LINKED TARGET ([`Linked::still`]), with the link's path:
/// where a test retargets the link between its observation and the re-check under the gate.
#[cfg(all(test, unix))]
mod link_seam {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static RECHECK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Call `then` with the link's path at each re-check on this thread.
    pub(super) fn on_recheck(then: impl FnMut(&Path) + 'static) {
        RECHECK.with(|slot| *slot.borrow_mut() = Some(Box::new(then)));
    }

    /// Forget this thread's call.
    pub(super) fn clear() {
        RECHECK.with(|slot| *slot.borrow_mut() = None);
    }

    pub(super) fn rechecking(link: &Path) {
        let call = RECHECK.with(|slot| slot.borrow_mut().take());
        if let Some(mut call) = call {
            call(link);
            RECHECK.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(call);
                }
            });
        }
    }
}

#[cfg(all(not(test), unix))]
mod link_seam {
    pub(super) const fn rechecking(_: &std::path::Path) {}
}

/// AN EXPLICIT TARGET NAMED THROUGH A LINK, resolved one hop, read and never written: `None`
/// where `target` is no link, or a link outside [`scope`], which every other rule reads as it
/// reads any target. A link in scope, in a folder reached through no link, resolves only to a
/// folder that stands, no link and reached through none (the kernel's resolution of the link is
/// that folder's canonical path), on the link's own volume, this user's and private as custody
/// binds a folder ([`identity`]); each other case is its [`LinkRefusal`]. Ownership is
/// [`admit_linked`]'s, which re-checks the link under the folder's gate.
#[cfg(unix)]
fn through_link(home: &Path, target: &Path) -> Option<Result<Linked, LinkRefusal>> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(target).ok()?;
    let parent = target.parent()?;
    if !meta.file_type().is_symlink() || !scope(home, target) {
        return None;
    }
    match std::fs::canonicalize(parent) {
        Ok(canonical) if canonical == parent => {}
        Ok(_) => return Some(Err(LinkRefusal::Chain)),
        Err(_) => return Some(Err(LinkRefusal::Unreachable)),
    }
    let volume = std::fs::symlink_metadata(parent).ok()?.dev();
    let text = std::fs::read_link(target).ok()?;
    let resolve = || {
        // Text that climbs past the root names no folder this reading can bind.
        let real =
            normalize_with_cwd(text.clone(), Some(parent)).ok_or(LinkRefusal::Unreachable)?;
        let folder = match std::fs::symlink_metadata(&real) {
            Ok(at) if at.file_type().is_symlink() => return Err(LinkRefusal::Chain),
            Ok(at) if at.is_dir() => at,
            Ok(_) => return Err(LinkRefusal::NotFolder),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Err(LinkRefusal::Dangling);
            }
            Err(_) => return Err(LinkRefusal::Unreachable),
        };
        // The kernel's own resolution of the link must be the folder read from its text: a
        // canonical path, so no link on the way, `..` past one included.
        match std::fs::canonicalize(target) {
            Ok(resolved) if resolved == real => {}
            Ok(_) => return Err(LinkRefusal::Chain),
            Err(_) => return Err(LinkRefusal::Unreachable),
        }
        if folder.dev() != volume {
            return Err(LinkRefusal::OtherVolume);
        }
        let identity = identity(&real).ok_or(LinkRefusal::Unowned)?;
        Ok(Linked {
            link: target.to_path_buf(),
            node: (meta.dev(), meta.ino()),
            text: text.clone(),
            real,
            identity,
        })
    };
    Some(resolve())
}

/// ADMIT `workspace`'s PRODUCER TO THE FOLDER `linked` NAMES, as one atpkg registered and owns:
/// its receipt names that folder and its identity, its marker binds the receipt ([`same`]), and
/// the link, re-checked under the folder's gate, still names it ([`Linked::still`]). The producer
/// joins the receipt's owners as [`create_with_policy`] records one at a folder's own name, an
/// owner already on record needing no write. Nothing is written in the folder or through the
/// link, no receipt is keyed by the link, and a folder atpkg did not make is never adopted
/// through one: the link is only the name the call used. `Ok(Err(..))` says why the folder is
/// not owned ([`LinkRefusal`]), read under the admission lock as an adoption reads it.
///
/// # Errors
/// As [`create_checked`]'s: the folder's gate was busy, or the owner its custody needs could not
/// be recorded.
#[cfg(unix)]
fn admit_linked(
    home: &Path,
    linked: &Linked,
    workspace: &Path,
) -> std::io::Result<Result<(), LinkRefusal>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let stop = || std::io::Error::other("the managed build cache's owner could not be recorded");
    let root = linked.real.as_path();
    if !scope(home, workspace)
        || std::fs::canonicalize(workspace).ok().as_deref() != Some(workspace)
        || !super::checkout::MANIFESTS.iter().any(|name| {
            std::fs::symlink_metadata(workspace.join(name)).is_ok_and(|metadata| metadata.is_file())
        })
    {
        return Err(stop());
    }
    if recorded_while(home, root, workspace, &|| linked.still()) {
        witness_soon(home, root);
        return Ok(Ok(()));
    }
    let index = index(home).ok_or_else(stop)?;
    let admission = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(index.join("locks.noindex/admission.lock"))?;
    if !admission.metadata()?.is_file() {
        return Err(stop());
    }
    let _admission = crate::lock::Flock::lock(admission)?;
    let key = key(root).ok_or_else(stop)?;
    let path = index.join(format!("{key}.json"));
    let gate = open_gate(&gate_path(&path).ok_or_else(stop)?).ok_or_else(stop)?;
    let _held = crate::lock::Flock::try_lock(gate).map_err(|_| stop())?;
    if !linked.still() {
        return Ok(Err(LinkRefusal::Changed));
    }
    let Some(mut receipt) = read(&path).filter(|receipt| {
        receipt.root == root && receipt.identity.names(&linked.identity) && same(home, receipt)
    }) else {
        return Ok(Err(
            if scope(home, root) && adoption::proven(home, root, &path).is_some() {
                LinkRefusal::CargoMade
            } else if read(&path)
                .is_some_and(|receipt| receipt.root == root && renumbered(home, &receipt).is_some())
            {
                LinkRefusal::Renumbered
            } else if custody_present(root) || std::fs::symlink_metadata(&path).is_ok() {
                LinkRefusal::Foreign
            } else {
                LinkRefusal::Unowned
            },
        ));
    };
    if receipt.package.as_ref().is_some_and(|policy| {
        receipt.owners_unknown || !package::producer_allows(home, policy, workspace)
    }) {
        return Err(stop());
    }
    witness(home, &path, &mut receipt);
    let mut changed = false;
    if receipt.pending.is_some() {
        if receipt.package.is_some() {
            return Err(stop());
        }
        pending_copy(&index, &key, &receipt).ok_or_else(stop)?;
        receipt.pending = None;
        changed = true;
    }
    if !receipt.owners.iter().any(|owner| owner == workspace) {
        if receipt.owners.len() < MAX_OWNERS {
            receipt.owners.push(workspace.to_path_buf());
        } else if receipt.package.is_none() {
            receipt.owners_unknown = true;
        }
        changed = true;
    }
    if changed {
        write(&path, &receipt).ok_or_else(stop)?;
    }
    Ok(Ok(()))
}

/// The independently registered cache roots, without walking any workspace or home folder.
#[must_use]
pub fn roots(home: &Path) -> Vec<Registered> {
    let index = home.join(INDEX);
    if identity(&index).is_none() || std::fs::canonicalize(&index).ok().as_ref() != Some(&index) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&index) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .take(MAX_ROOTS)
        .filter_map(|entry| read(&entry.path()).map(|receipt| (entry.path(), receipt)))
        .filter(|(_, receipt)| {
            same(home, receipt) || receipt.pending.is_some() || receipt.creating.is_some()
        })
        .map(|(receipt_path, receipt)| Registered {
            root: receipt.root,
            owners: receipt.owners,
            owners_unknown: receipt.owners_unknown,
            receipt_path,
            identity: receipt.identity,
            nonce: receipt.nonce,
        })
        .collect()
}

#[cfg(unix)]
fn settle(path: &Path, receipt: &mut Receipt) {
    let still_root = identity(&receipt.root).is_some_and(|now| receipt.identity.names(&now));
    if receipt.package.is_none()
        && (!still_root
            || path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".retired.json")))
    {
        let _ = std::fs::remove_file(path);
    } else {
        receipt.pending = None;
        let _ = write(path, receipt);
    }
}

fn pending_owned(home: &Path, receipt: &Receipt, pending: &Pending) -> bool {
    let payload = pending.directory.join(PAYLOAD);
    package::payload_safe(receipt, pending)
        && scope(home, &pending.directory)
        && std::fs::canonicalize(&pending.directory).ok().as_ref() == Some(&pending.directory)
        && identity(&pending.directory).is_some_and(|now| pending.identity.names(&now))
        && (pending.authorized
            || (identity(&payload).is_some_and(|now| receipt.identity.names(&now))
                && read(&payload.join(MARKER)).is_some_and(|marker| {
                    marker.identity.same_record(&receipt.identity) && marker.nonce == receipt.nonce
                })))
}

fn workspace(home: &Path, manifest: &Path) -> Option<PathBuf> {
    let manifest = std::fs::canonicalize(manifest).ok()?;
    if !super::checkout::MANIFESTS
        .iter()
        .any(|name| manifest.file_name() == Some(name.as_ref()))
    {
        return None;
    }
    let package = manifest.parent()?;
    if !scope(home, package) {
        return None;
    }
    let text = crate::metadata_io::read_bounded_regular_utf8(&manifest, 1 << 20).ok()?;
    let parsed = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
    if parsed.get("workspace").is_some() {
        return Some(package.to_path_buf());
    }
    if let Some(explicit) = parsed
        .get("package")
        .and_then(|table| table.get("workspace"))
        .and_then(aterm_toml::Value::as_str)
    {
        let owner = std::fs::canonicalize(package.join(explicit)).ok()?;
        if !scope(home, &owner) {
            return None;
        }
        let text =
            crate::metadata_io::read_bounded_regular_utf8(&owner.join("Cargo.toml"), 1 << 20)
                .ok()?;
        let parsed = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
        return parsed.get("workspace").is_some().then_some(owner);
    }
    for ancestor in package.ancestors().skip(1).take(32) {
        if !scope(home, ancestor) {
            break;
        }
        let path = ancestor.join("Cargo.toml");
        if let Ok(text) = crate::metadata_io::read_bounded_regular_utf8(&path, 1 << 20) {
            let parsed = aterm_toml::from_str::<aterm_toml::Value>(&text).ok()?;
            if parsed.get("workspace").is_some() {
                return Some(ancestor.to_path_buf());
            }
        }
    }
    Some(package.to_path_buf())
}

#[cfg(unix)]
fn canonical_existing_ancestor(home: &Path, path: &Path) -> bool {
    for ancestor in path.ancestors() {
        if !ancestor.starts_with(home) {
            return false;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                return std::fs::canonicalize(ancestor).ok().as_deref() == Some(ancestor);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    false
}

fn raw_target_spec(args: &[String], machine: &super::Machine<'_>) -> Option<PathBuf> {
    let mut target = None;
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--" => break,
            "--target-dir" => target = words.next().map(PathBuf::from),
            "--manifest-path" | "--config" | "--color" | "--explain" | "-Z" | "-C" => {
                let _ = words.next();
            }
            _ => {
                if let Some(path) = word.strip_prefix("--target-dir=") {
                    target = Some(PathBuf::from(path));
                }
            }
        }
    }
    let target = target
        .or_else(|| (machine.env)("CARGO_TARGET_DIR").map(PathBuf::from))
        .or_else(|| (machine.env)("CARGO_BUILD_TARGET_DIR").map(PathBuf::from))?;
    (!target.as_os_str().is_empty()).then_some(target)
}

fn normalize_with_cwd(path: PathBuf, cwd: Option<&Path>) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Some(normalized)
}

fn target_name(args: &[String], machine: &super::Machine<'_>) -> Option<PathBuf> {
    normalize_with_cwd(raw_target_spec(args, machine)?, machine.cwd)
}

#[cfg(unix)]
fn suppress_unresolved(home: &Path, target: &Path) -> std::io::Result<bool> {
    // A target named through a link to a folder stands for that folder, the link re-checked
    // under each gate held; a link that resolves to none is read by its own name, as before.
    let linked = through_link(home, target).and_then(Result::ok);
    let target = linked
        .as_ref()
        .map_or(target, |linked| linked.real.as_path());
    let candidates = [target.to_path_buf(), target.join(super::TARGET_BUILD_LEAF)];
    let mut protected = false;
    for root in candidates {
        if !scope(home, &root)
            || std::fs::symlink_metadata(root.join(MARKER))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            continue;
        }
        let stop =
            || std::io::Error::other("the managed cache's unknown owner could not be recorded");
        let index = index(home).ok_or_else(stop)?;
        let key = key(&root).ok_or_else(stop)?;
        let path = index.join(format!("{key}.json"));
        let lock = crate::metadata_io::open_regular(&gate_path(&path).ok_or_else(stop)?)?;
        let _held = crate::lock::Flock::try_lock(lock).map_err(|_| stop())?;
        if linked.as_ref().is_some_and(|linked| !linked.still()) {
            return Err(std::io::Error::other(LinkRefusal::Changed.why()));
        }
        let mut receipt = read(&path)
            .filter(|receipt| receipt.root == root && same(home, receipt))
            .ok_or_else(stop)?;
        receipt.owners_unknown = true;
        write(&path, &receipt).ok_or_else(stop)?;
        protected = true;
    }
    Ok(protected)
}

#[cfg(not(unix))]
fn suppress_unresolved(_: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// An unresolvable directory config might select any currently registered root. Poison
/// whole-retirement authority conservatively, never claim its fresh target or change paths.
#[cfg(unix)]
fn suppress_configured(home: &Path) -> std::io::Result<()> {
    let directory = home.join(INDEX);
    if std::fs::symlink_metadata(&directory)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    let stop = || std::io::Error::other("configured build cache owners could not be recorded");
    let directory = index(home).ok_or_else(stop)?;
    let admission =
        crate::metadata_io::open_regular(&directory.join("locks.noindex/admission.lock"))?;
    let _admission = crate::lock::Flock::lock(admission)?;
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            continue;
        }
        let lock = crate::metadata_io::open_regular(&gate_path(&path).ok_or_else(stop)?)?;
        let _held = crate::lock::Flock::try_lock(lock).map_err(|_| stop())?;
        let mut receipt = read(&path).ok_or_else(stop)?;
        receipt.owners_unknown = true;
        write(&path, &receipt).ok_or_else(stop)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn suppress_configured(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Cargo's short version flag together with any verbose flags, such as `-vV`.
fn short_version_query(word: &str) -> bool {
    word.strip_prefix('-').is_some_and(|flags| {
        flags.contains('V') && flags.bytes().all(|flag| matches!(flag, b'v' | b'V'))
    })
}

/// THE BUILTIN VERBS THAT ONLY READ, judged by what Targo's own code for each writes (trust
/// `src/tools/targo` at 73409c7360, read 2026-11-06; `docs/DECISION-atpkg-output-free-verbs-2026-11-06.md`
/// cites each path), never by name: `pkgid`, `verify-project`, `read-manifest`, `locate-project`
/// and `version` write nothing under a target or build directory, whatever `--target-dir`, `-Z`
/// option or `--config` the call carries.
const READING_VERBS: &[&str] = &[
    "pkgid",
    "verify-project",
    "read-manifest",
    "locate-project",
    "version",
];

/// THE BUILTIN VERBS THAT RESOLVE A WORKSPACE AND WRITE NO BUILD OUTPUT, judged as
/// [`READING_VERBS`] are, unless the call can put their lockfile under a target
/// ([`lockfile_may_land_in_a_target`]): `fetch` fills the registry under `CARGO_HOME`, and each
/// may write the workspace's own `Cargo.lock` where the workspace stands. Each also resolves
/// target platforms, so the compiler's probe cache (`.rustc_info.json`) may be rewritten in a
/// build directory that already stands, as every build there rewrites it: compiler facts, no
/// artifact and no directory made (`paths::write` creates no parent), nothing a custody record
/// keeps. Which verbs run without a lane is [`crate::reroute::LANE_FREE_QUERIES`], a separate
/// rule. No alias or external `cargo-<verb>` can take a builtin's name: Targo and cargo run the
/// builtin.
const RESOLVING_VERBS: &[&str] = &["fetch", "tree", "metadata", "generate-lockfile"];

/// WHETHER A RESOLVING VERB'S LOCKFILE MAY LAND UNDER A TARGET OR BUILD DIRECTORY, which Targo then
/// makes, `CACHEDIR.TAG` and all (`ops/lockfile.rs`): an embedded manifest, whose lock root is in
/// the build directory, or a `resolver.lockfile-path`, wherever the call can name one. Targo
/// reads a manifest path (`--manifest-path` or `-m`) as embedded when it ends in `.rs` or has no
/// extension (`workspace/parser/mod.rs`'s `is_embedded`). A lockfile path is set by a `--config`
/// value, or by the config file it names when something stands at that path, as Targo reads it,
/// by `CARGO_RESOLVER_LOCKFILE_PATH` or its `TARGO_` twin, by the configs of a `-C` directory, by
/// the deprecated `--lockfile-path`, or by a config Targo reads ([`configs_name_a_lock_root`]).
/// An `include` may set it too ([`names_lock_root`]). Every word is scanned, so an option value
/// that looks like one of these counts, as does a short-option cluster that holds `m`. Such a
/// call is observed as a producer is.
fn lockfile_may_land_in_a_target(args: &[String], machine: &super::Machine<'_>) -> bool {
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        let (flag, inline) = if word.starts_with("--") {
            word.split_once('=')
                .map_or((word, None), |(flag, value)| (flag, Some(value)))
        } else if let Some(rest) = word.strip_prefix("-m") {
            let rest = rest.strip_prefix('=').unwrap_or(rest);
            (
                "--manifest-path",
                Some(rest).filter(|rest| !rest.is_empty()),
            )
        } else {
            (word, None)
        };
        let value = match flag {
            "--manifest-path" | "--config" => inline.or_else(|| words.next()),
            "--lockfile-path" => return true,
            _ if flag.starts_with("-C") => return true,
            _ if flag.starts_with('-')
                && !flag.starts_with("--")
                && !flag.starts_with("-Z")
                && flag.contains('m') =>
            {
                return true;
            }
            _ => continue,
        };
        let Some(value) = value else {
            return true;
        };
        let placed = if flag == "--manifest-path" {
            Path::new(value)
                .extension()
                .is_none_or(|extension| extension == "rs")
        } else {
            // Targo reads the value as a config file when something stands at that path, and as
            // a dotted key otherwise (`context/mod.rs`'s `load_cli_args`).
            match machine
                .cwd
                .map(|cwd| config_text(machine, &cwd.join(value)))
            {
                None | Some(Err(())) => true,
                Some(Ok(Some(text))) => names_lock_root(&text),
                Some(Ok(None)) => names_lock_root(value),
            }
        };
        if placed {
            return true;
        }
    }
    [
        "CARGO_RESOLVER_LOCKFILE_PATH",
        "TARGO_RESOLVER_LOCKFILE_PATH",
    ]
    .iter()
    .any(|name| (machine.env)(name).is_some())
        || configs_name_a_lock_root(machine)
}

/// Whether config text, a file's or a `--config` value's, sets `resolver.lockfile-path` or an
/// `include`. It is read as TOML, so a quoted key's escapes spell what Targo reads. Text this
/// TOML 1.0 reader cannot parse (Targo's reads TOML 1.1) may still set either unless it holds
/// neither word and no backslash, through which an escape could spell one.
fn names_lock_root(text: &str) -> bool {
    match aterm_toml::from_str::<aterm_toml::Value>(text) {
        Ok(config) => {
            config.get("include").is_some()
                || config
                    .get("resolver")
                    .and_then(|resolver| resolver.get("lockfile-path"))
                    .is_some()
        }
        Err(_) => text.contains("lockfile") || text.contains("include") || text.contains('\\'),
    }
}

/// The text of the config file at `path` as Targo reads it, following links as it does: atpkg's
/// reader refuses a link, so one is read at its canonical path (`.cargo/config -> config.toml`,
/// the spelling cargo once advised). `Ok(None)` where nothing stands; `Err` where it cannot be
/// read.
fn config_text(machine: &super::Machine<'_>, path: &Path) -> Result<Option<String>, ()> {
    (machine.read)(path).or_else(|()| match std::fs::canonicalize(path) {
        Ok(real) if real != path => (machine.read)(&real),
        _ => Err(()),
    })
}

/// Whether a config Targo reads for this call sets a lock root ([`names_lock_root`]) or cannot be
/// read: each `.cargo/config.toml` and `.cargo/config` from the working directory up, then the
/// package home's. That home is `CARGO_HOME`, or its `TARGO_HOME` twin when `CARGO_HOME` is
/// unset, against the working directory, or `~/.cargo` where the one chosen is empty
/// (`context/mod.rs`'s `homedir`, `context/environment.rs`).
fn configs_name_a_lock_root(machine: &super::Machine<'_>) -> bool {
    let Some(cwd) = machine.cwd else {
        return true;
    };
    let home = (machine.env)("CARGO_HOME")
        .or_else(|| (machine.env)("TARGO_HOME"))
        .filter(|home| !home.is_empty())
        .map(|home| cwd.join(home))
        .or_else(|| machine.home.map(|home| home.join(".cargo")));
    cwd.ancestors()
        .map(|dir| dir.join(".cargo"))
        .chain(home)
        .flat_map(|dir| ["config.toml", "config"].map(|name| dir.join(name)))
        .any(|file| match config_text(machine, &file) {
            Err(()) => true,
            Ok(None) => false,
            Ok(Some(text)) => names_lock_root(&text),
        })
}

/// Known builtin commands that produce no build output need no owner observation: the
/// [`READING_VERBS`], the [`RESOLVING_VERBS`] whose lockfile stays out of every target, help,
/// version queries and clean. Clean must reach Cargo even for an unregistered target, without
/// adopting it. A command name used as an option value, an external alias or lint argument
/// proves none of this; a free verb still runs through every other rule of the shim's front.
fn non_producing_command(args: &[String], tool: super::Tool, machine: &super::Machine<'_>) -> bool {
    // These exact native build-tree operations never compile. Their own
    // manager checks tree custody and Cargo locks; asking about that custody
    // must not first adopt or reject the workspace's build output directory.
    // `exec` and unknown operations remain possible producers here.
    if tool == super::Tool::Targo
        && let [trust, repo, lock, rest @ ..] = args
        && trust == "trust"
        && repo == "repo"
        && lock == "lock"
        && matches!(
            rest.first().map(String::as_str),
            None | Some(
                "--protocol"
                    | "--help"
                    | "-h"
                    | "status"
                    | "acquire"
                    | "release"
                    | "break"
                    | "reclaim-incremental"
            )
        )
    {
        return true;
    }
    // A native scheduler capability probe has no command or build output.
    // Recognize only its exact shape; aliases and mixed commands still owe
    // normal cache ownership and build admission.
    if tool == super::Tool::Targo && matches!(args, [flag] if flag == "--resource-capabilities") {
        return true;
    }
    if tool == super::Tool::Tippy {
        return false;
    }
    let mut words = args.iter().map(String::as_str).peekable();
    if tool == super::Tool::Cargo && words.peek().is_some_and(|word| word.starts_with('+')) {
        let _ = words.next();
    }
    while let Some(word) = words.next() {
        match word {
            "--" => return false,
            "--config" | "--color" | "--manifest-path" | "--target-dir" | "--explain" | "-Z"
            | "-C" => {
                if words.next().is_none() {
                    return false;
                }
            }
            "--unverified" | "--locked" | "--offline" | "--frozen" | "--verbose" | "-v" | "-vv"
            | "--quiet" | "-q" => {}
            _ if short_version_query(word) || word == "--version" => return true,
            _ if word.starts_with("--config=")
                || word.starts_with("--color=")
                || word.starts_with("--manifest-path=")
                || word.starts_with("--target-dir=")
                || word.starts_with("--explain=")
                || word.starts_with("-Z")
                || word.starts_with("-C") => {}
            _ => {
                return matches!(word, "help" | "clean")
                    || READING_VERBS.contains(&word)
                    || (RESOLVING_VERBS.contains(&word)
                        && !lockfile_may_land_in_a_target(args, machine));
            }
        }
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observation {
    Unmanaged,
    Protected,
    Owned,
    /// Unmanaged: an explicit target named through a link that does not name a folder atpkg
    /// owns, for this reason; a producer requiring managed output refuses it in these words.
    #[cfg(unix)]
    Refused(LinkRefusal),
}

fn observe(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> std::io::Result<Observation> {
    if non_producing_command(args, tool, machine)
        || args
            .iter()
            .take_while(|word| word.as_str() != "--")
            .any(|word| matches!(word.as_str(), "-h" | "--help" | "-V" | "--version"))
    {
        return Ok(Observation::Unmanaged);
    }
    // The shim's call is observed before its build runs, so the folders the index shares with
    // the shared cache are made private here, never by the build with its umask.
    lay_shared(home);
    if let Some(typed) = equivalent_placement_args(args, tool, machine) {
        let env = |name: &str| {
            if name == super::BUILD_DIR_ENV {
                None
            } else {
                (machine.env)(name)
            }
        };
        return observe(
            &typed,
            tool,
            home,
            &super::Machine {
                env: &env,
                ..*machine
            },
        );
    }
    let Some(chosen) = super::explicit_cache(args, tool, machine) else {
        let protected = match target_name(args, machine) {
            Some(target) => suppress_unresolved(home, &target)?,
            None => false,
        };
        if super::unresolved_directory(args, machine) {
            suppress_configured(home)?;
        }
        return Ok(if protected {
            Observation::Protected
        } else {
            Observation::Unmanaged
        });
    };
    let Some(owner) = workspace(home, &chosen.manifest) else {
        return suppress_unresolved(home, &chosen.target).map(|protected| {
            if protected {
                Observation::Protected
            } else {
                Observation::Unmanaged
            }
        });
    };
    // A target named through a link is the folder it names when atpkg owns that folder, and
    // never adopted or created through it.
    #[cfg(unix)]
    match through_link(home, &chosen.target) {
        Some(Ok(linked)) => {
            return Ok(match admit_linked(home, &linked, &owner)? {
                Ok(()) => Observation::Owned,
                Err(refusal) => Observation::Refused(refusal),
            });
        }
        Some(Err(refusal)) => return Ok(Observation::Refused(refusal)),
        None => {}
    }
    let owned = create_checked(home, &chosen.target, &owner)?;
    if !owned && let Some(super::BuildDir::Target(placement)) = super::decide(args, tool, machine) {
        let child = chosen.target.join(super::TARGET_BUILD_LEAF);
        if placement == format!("build.build-dir='{}'", child.display()) {
            return create_checked(home, &child, &owner).map(|owned| {
                if owned {
                    Observation::Owned
                } else {
                    Observation::Unmanaged
                }
            });
        }
    }
    Ok(if owned {
        Observation::Owned
    } else {
        Observation::Unmanaged
    })
}

/// Existing observer controls also exercise conservative unknown-owner protection.
#[cfg(all(test, unix))]
fn observe_on(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> std::io::Result<bool> {
    observe(args, tool, home, machine)
        .map(|observed| matches!(observed, Observation::Protected | Observation::Owned))
}

/// An inherited directory can be resolved without granting its environment provenance:
/// recompute this call's existing placement with only that variable hidden, stripping only
/// a matching leading config value our producer can emit. Anything other than exact equality
/// keeps the conservative unknown-owner path. An existing root is admitted only as [`observe`]
/// admits one typed with no inherited directory.
fn equivalent_placement_args<'a>(
    args: &'a [String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> Option<std::borrow::Cow<'a, [String]>> {
    let inherited = (machine.env)(super::BUILD_DIR_ENV)?;
    let inherited = inherited.to_str()?;
    let typed = if super::directory_from_placed_args(args) == Some(inherited) {
        let at = usize::from(
            tool == super::Tool::Cargo && args.first().is_some_and(|arg| arg.starts_with('+')),
        );
        let mut typed = args[..at].to_vec();
        typed.extend_from_slice(&args[at + 2..]);
        std::borrow::Cow::Owned(typed)
    } else {
        std::borrow::Cow::Borrowed(args)
    };
    let env = |name: &str| {
        if name == super::BUILD_DIR_ENV {
            None
        } else {
            (machine.env)(name)
        }
    };
    let placement = super::decide(
        &typed,
        tool,
        &super::Machine {
            env: &env,
            ..*machine
        },
    )?;
    let directory = match &placement {
        super::BuildDir::Cache(value) | super::BuildDir::Target(value) => {
            super::directory_from_config(value)?
        }
        super::BuildDir::TrustEnv(directory) => directory,
    };
    (directory == inherited).then_some(typed)
}

/// Observe a real managed compiler invocation. This adds no flag and moves no output. The
/// last explicit target overrides the environment; an explicit manifest owns the cache even
/// when a launcher runs from another directory. An existing cache cargo did not make remains
/// unmanaged (the `adoption` module).
pub fn observe_command(
    args: &[String],
    tool: super::Tool,
    cwd: Option<&Path>,
) -> std::io::Result<()> {
    command_observation(args, tool, cwd, true).map(|_| ())
}

fn command_observation(
    args: &[String],
    tool: super::Tool,
    cwd: Option<&Path>,
    require_explicit: bool,
) -> std::io::Result<bool> {
    observation_with(
        args,
        tool,
        cwd,
        &|name| std::env::var_os(name),
        require_explicit,
    )
}

/// OBSERVE A SHIM'S CALL FROM ITS FRONT (BUILD-ADMIT's hop, `atpkg __build_admit --shim`), as the
/// shim's own registration call observed it before its block placed the build: `args` are the
/// words the front was handed and `placed` what the block did to them (`config`: it put
/// `--config build.build-dir=…` first, and exported the same directory for a call that
/// [`loses_config`](super::loses_config) (tippy, clippy); the words and the variable, which the
/// call never had, are hidden here; `env`: it exported `CARGO_BUILD_BUILD_DIR` alone (the trust
/// lane), which the call never had, hidden here; `-`: nothing), so the call is read as typed.
///
/// # Errors
/// As [`observe_command`]'s, `InvalidInput` for a `placed` the block never says, or an explicit
/// target whose managed ownership cannot be established.
pub fn observe_shim_call(
    args: &[String],
    tool: super::Tool,
    placed: &str,
    cwd: Option<&Path>,
) -> std::io::Result<()> {
    let (args, hidden) = typed_call(args, placed)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    observation_with(
        args,
        tool,
        cwd,
        &|name| {
            if Some(name) == hidden {
                None
            } else {
                std::env::var_os(name)
            }
        },
        true,
    )
    .map(|_| ())
}

/// The call as typed, from the words a shim's block `placed` handed its front
/// ([`observe_shim_call`]): its words, and the variable the block exported that the call never
/// had. `None` for a `placed` the block never says.
pub(crate) fn typed_call<'a>(
    args: &'a [String],
    placed: &str,
) -> Option<(&'a [String], Option<&'static str>)> {
    match placed {
        "-" => Some((args, None)),
        "config" => Some(match args {
            [flag, value, rest @ ..]
                if flag == "--config" && value.starts_with("build.build-dir=") =>
            {
                (rest, Some(super::BUILD_DIR_ENV))
            }
            _ => (args, None),
        }),
        "env" => Some((args, Some(super::BUILD_DIR_ENV))),
        _ => None,
    }
}

fn observation_with(
    args: &[String],
    tool: super::Tool,
    cwd: Option<&Path>,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    require_explicit: bool,
) -> std::io::Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Ok(false);
    };
    let machine = super::Machine {
        home: Some(&home),
        root: None,
        cwd,
        env,
        read: &|path| match crate::metadata_io::read_bounded_regular_utf8(path, 1 << 20) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        },
        writable: &super::writable,
    };
    observe_for_producer(args, tool, &home, &machine, require_explicit)
}

fn under_shared_cache(home: &Path, path: &Path, machine: &super::Machine<'_>) -> bool {
    let root = machine
        .root
        .map_or_else(|| super::cache_root(home), Path::to_path_buf);
    path.starts_with(&root)
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && path
            .strip_prefix(&root)
            .ok()
            .is_some_and(|rest| rest.components().any(|c| c.as_os_str() == super::LEAF))
}

/// Whether the package whose manifest is `manifest` (a path to a `Cargo.toml` or `Targo.toml`,
/// read through its links as [`workspace`] reads it) lies where atpkg never manages a workspace:
/// its folder outside [`scope`], as HOME itself, a path outside HOME, a folder of HOME atpkg
/// never looks into, app data and a file provider's domain are. A build of it owes no managed
/// custody, wherever its target is.
fn never_managed(home: &Path, manifest: &Path) -> bool {
    let resolved = std::fs::canonicalize(manifest).unwrap_or_else(|_| manifest.to_path_buf());
    resolved
        .parent()
        .is_none_or(|package| !scope(home, package))
}

/// Whether a producer call owes managed custody of its output ([`observe_for_producer`] refuses
/// it when none can be established): it names a target or build directory under HOME outside
/// the shared cache, or one atpkg cannot resolve, for a package atpkg could manage. A call whose
/// manifest lies outside HOME, or that names none and runs outside HOME or in no package there,
/// owes none, as does one whose package [`never_managed`] names.
fn requires_managed_explicit(args: &[String], home: &Path, machine: &super::Machine<'_>) -> bool {
    let mut manifest_arg = None;
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--" => break,
            "--manifest-path" => manifest_arg = words.next(),
            "--target-dir" | "--config" | "--color" | "--explain" | "-Z" | "-C" => {
                let _ = words.next();
            }
            _ => {
                if let Some(path) = word.strip_prefix("--manifest-path=") {
                    manifest_arg = Some(path);
                }
            }
        }
    }
    let scan_cwd = if let Some(raw) = manifest_arg {
        if !raw.is_empty() {
            if let Some(manifest) = normalize_with_cwd(PathBuf::from(raw), machine.cwd) {
                if !manifest.starts_with(home) || never_managed(home, &manifest) {
                    return false;
                }
                manifest
                    .parent()
                    .filter(|p| p.starts_with(home))
                    .unwrap_or(home)
                    .to_path_buf()
            } else {
                home.to_path_buf()
            }
        } else {
            home.to_path_buf()
        }
    } else {
        let Some(cwd) = machine.cwd.filter(|c| c.starts_with(home) && *c != home) else {
            return false;
        };
        let found = cwd
            .ancestors()
            .take_while(|p| p.starts_with(home) && *p != home)
            .take(32)
            .find_map(|dir| {
                super::checkout::MANIFESTS
                    .iter()
                    .map(|name| dir.join(name))
                    .find(|path| !matches!((machine.read)(path), Ok(None)) || path.is_file())
            });
        if found.is_none_or(|manifest| never_managed(home, &manifest)) {
            return false;
        }
        cwd.to_path_buf()
    };

    if let Some(raw) = raw_target_spec(args, machine) {
        let Some(target) = normalize_with_cwd(raw, machine.cwd) else {
            return true;
        };
        if target.starts_with(home) && !under_shared_cache(home, &target, machine) {
            return true;
        }
    }

    if let Some(raw) = (machine.env)(super::BUILD_DIR_ENV) {
        let root = machine
            .root
            .map_or_else(|| super::cache_root(home), Path::to_path_buf);
        let shared_template = super::config_value(&root)
            .and_then(|cfg| super::directory_from_config(&cfg).map(str::to_owned));
        match raw.to_str() {
            Some(s) if shared_template.as_deref() == Some(s) => {}
            Some(s) if !s.is_empty() && !s.contains('{') && !s.contains('}') => {
                let Some(dir) = normalize_with_cwd(PathBuf::from(s), machine.cwd) else {
                    return true;
                };
                if dir.starts_with(home) && !under_shared_cache(home, &dir, machine) {
                    return true;
                }
            }
            _ => return true,
        }
    }

    let env_without_build_dir = |name: &str| {
        if name == super::BUILD_DIR_ENV {
            None
        } else {
            (machine.env)(name)
        }
    };
    super::unresolved_directory(
        args,
        &super::Machine {
            home: Some(home),
            cwd: Some(&scan_cwd),
            env: &env_without_build_dir,
            ..*machine
        },
    )
}

fn observe_for_producer(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    require_explicit: bool,
) -> std::io::Result<bool> {
    if non_producing_command(args, tool, machine) {
        return Ok(false);
    }
    // A nested compiler can inherit the exact directory our shim placed. Classify the
    // original call, not that derived environment, before deciding ownership is required.
    if let Some(typed) = equivalent_placement_args(args, tool, machine) {
        let env = |name: &str| {
            if name == super::BUILD_DIR_ENV {
                None
            } else {
                (machine.env)(name)
            }
        };
        return observe_for_producer(
            &typed,
            tool,
            home,
            &super::Machine {
                env: &env,
                ..*machine
            },
            require_explicit,
        );
    }
    let query = args
        .iter()
        .take_while(|word| word.as_str() != "--")
        .any(|word| matches!(word.as_str(), "-h" | "--help" | "-V" | "--version"));
    let explicit = requires_managed_explicit(args, home, machine);
    let refusal = || {
        std::io::Error::other(
            "explicit build target is unmanaged or unsupported; choose a fresh target for managed cache ownership",
        )
    };
    if require_explicit
        && explicit
        && !query
        && super::explicit_cache(args, tool, machine)
            .and_then(|chosen| workspace(home, &chosen.manifest))
            .is_none()
    {
        return Err(refusal());
    }
    let observed = observe(args, tool, home, machine)?;
    let owned = observed == Observation::Owned;
    if require_explicit && explicit && !query && !owned {
        #[cfg(unix)]
        if let Observation::Refused(refusal) = observed {
            return Err(std::io::Error::other(refusal.why()));
        }
        return Err(refusal());
    }
    Ok(owned)
}

/// THE SHIM'S HOP (BUILD-ADMIT), rendered after its placement block: `helper`'s front,
/// `<helper> __build_admit --shim <tool> <placed> <program> "$@"`, which registers the call's
/// build cache as the shim's registration call did before the block, claims the build's memory
/// and becomes `<program>` in the shim's pid. `<program>` is `target` (quoted), or `route`'s root
/// (quoted) when its tests hold (the shim's own route guard's, each `[ … ] && `, which the guard
/// ahead of the shim's `exec` line keeps); `<placed>` says what the block did
/// ([`observe_shim_call`]). With no helper
/// there, no build starts (exit 75, said), as when the registration call had none. The helper is
/// stamped beside that shim at installation, never selected from the process environment. No
/// line of it starts `exec '`, so every reader of the shim's target still reads its last line.
#[must_use]
#[cfg(any(unix, test))]
pub fn shim_hop(helper: &Path, tool: &str, target: &str, route: Option<(&str, &str)>) -> String {
    if !matches!(tool, "targo" | "targo-tippy") {
        return String::new();
    }
    let quoted = super::super::platform::sh_quote_str(&helper.to_string_lossy());
    let marker = aterm_json::to_string(helper.to_string_lossy().as_ref()).unwrap_or_default();
    let routed = route.map_or_else(String::new, |(tests, root)| {
        format!("{tests}__atb_p={root}\n")
    });
    let verb = crate::build_admit::VERB;
    format!(
        "# atpkg cache helper {marker}\n\
         # aterm: the build registers its cache and claims its memory, then runs in this pid (BUILD-ADMIT)\n\
         __atb_p={target}\n\
         {routed}\
         __atb_h=-\n\
         if [ -n \"$__atb\" ]; then if [ -n \"$__atb_D\" ] && [ -n \"$__atb_e\" ]; then __atb_h=env; else __atb_h=config; fi; fi\n\
         [ -x {quoted} ] && exec {quoted} {verb} --shim {tool} \"$__atb_h\" \"$__atb_p\" \"$@\"\n\
         printf '%s\\n' 'atpkg: the cache registrar is unavailable; run aterm pkg repair; no build started' >&2\n\
         exit 75\n"
    )
}

/// The generated shim's optional Rust call, before its existing placement block: the form laid for
/// a helper that does not speak the hop ([`helper_speaks_hop`]: an older atpkg standing, a bundle
/// rolled back), which every atpkg that registers caches serves. The helper is stamped beside that
/// shim at installation, never selected from the process environment.
#[must_use]
#[cfg(any(unix, test))]
pub fn shim_call(helper: &Path, tool: &str) -> String {
    if !matches!(tool, "targo" | "targo-tippy") {
        return String::new();
    }
    let quoted = super::super::platform::sh_quote_str(&helper.to_string_lossy());
    let marker = aterm_json::to_string(helper.to_string_lossy().as_ref()).unwrap_or_default();
    format!(
        "# atpkg cache helper {marker}\n\
         # aterm: register only newly created compiler caches at their existing output paths\n\
         if [ -x {quoted} ]; then\n  \
         {quoted} {REGISTER_VERB} --require-owned-if-explicit {tool} \"$@\" >/dev/null\n  \
         __atpkg_cache_status=$?\n  \
         if [ \"$__atpkg_cache_status\" -ne 0 ]; then\n    \
         printf '%s\\n' 'atpkg: a build cache record is busy or unavailable; no build started' >&2\n    \
         exit 75\n  \
         fi\n\
         else\n  \
         printf '%s\\n' 'atpkg: the cache registrar is unavailable; run aterm pkg repair; no build started' >&2\n  \
         exit 75\n\
         fi\n"
    )
}

/// WHETHER `helper` SPEAKS THE HOP ([`shim_hop`]): it is this process's own image, or it answers
/// `<helper> __build_admit --capabilities` with [`crate::build_admit::HOP_CAPABILITY`] alone,
/// exit 0, within ten seconds, from the same file before and after (its path's target, device,
/// inode, size and modification time). An atpkg from before the hop answers otherwise (an unknown
/// verb, or a front that takes `--capabilities` for its target), so a shim naming it keeps the
/// registration call's form ([`shim_call`]). Each helper's answer is asked once per process.
#[cfg(unix)]
#[must_use]
pub fn helper_speaks_hop(helper: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    static ASKED: std::sync::Mutex<Vec<(PathBuf, [u64; 5], bool)>> =
        std::sync::Mutex::new(Vec::new());
    let stamp = |path: &Path| {
        let physical = path.canonicalize().ok()?;
        let m = std::fs::metadata(&physical).ok()?;
        let at = u64::try_from(m.mtime()).unwrap_or(0);
        let ns = u64::try_from(m.mtime_nsec()).unwrap_or(0);
        Some((physical, [m.dev(), m.ino(), m.len(), at, ns]))
    };
    let Some((physical, before)) = stamp(helper) else {
        return false;
    };
    if std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .is_ok_and(|me| me == physical)
    {
        return true;
    }
    let mut asked = ASKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, _, speaks)) = asked
        .iter()
        .find(|(path, at, _)| *path == physical && *at == before)
    {
        return *speaks;
    }
    let mut command = aterm_uds::spawnfd::command(helper);
    command
        .setsid(true)
        .args([crate::build_admit::VERB, "--capabilities"]);
    let answered =
        aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(10));
    let speaks = answered.is_ok_and(|out| {
        out.status.success()
            && out.stdout == format!("{}\n", crate::build_admit::HOP_CAPABILITY).as_bytes()
    }) && stamp(helper)
        .is_some_and(|(path, after)| path == physical && after == before);
    asked.push((physical, before, speaks));
    speaks
}

/// The bounded backend hint a previous managed compiler shim recorded. Keep a standing hint
/// even if it has disappeared: silently dropping its guard would lose cache-owner observations.
#[must_use]
pub fn shim_helper(text: &str) -> Option<PathBuf> {
    let named = text
        .lines()
        .find_map(|line| line.strip_prefix("# atpkg cache helper "))?;
    let path = PathBuf::from(aterm_json::from_str::<String>(named).ok()?);
    path.is_absolute().then_some(path)
}

/// Internal compiler-shim entry, with no store/profile/credential discovery. Explicit producer
/// targets require managed ownership even for older shim argv; `--require-owned-if-explicit`
/// names that contract. A producer's `--require-owned` additionally requires ownership for every call.
pub fn run_hidden(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    if args.len() == 1 && args[0] == "--capabilities" {
        println!("{REGISTER_CAPABILITY}");
        return std::process::ExitCode::SUCCESS;
    }
    let require_owned = args.first().is_some_and(|word| word == "--require-owned");
    let require_explicit = args
        .first()
        .is_some_and(|word| word == "--require-owned-if-explicit");
    let args = if require_owned || require_explicit {
        &args[1..]
    } else {
        args
    };
    let Some(tool) = args
        .first()
        .and_then(|word| word.to_str())
        .and_then(|word| match word {
            "targo" => Some(super::Tool::Targo),
            "targo-tippy" => Some(super::Tool::Tippy),
            _ => None,
        })
    else {
        return std::process::ExitCode::from(2);
    };
    let Some(words) = args[1..]
        .iter()
        .map(|word| word.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
    else {
        return std::process::ExitCode::from(2);
    };
    match command_observation(&words, tool, std::env::current_dir().ok().as_deref(), true) {
        Ok(owned) if owned || !require_owned => std::process::ExitCode::SUCCESS,
        Ok(_) => std::process::ExitCode::from(75),
        Err(error) => {
            eprintln!("atpkg: build cache ownership refused: {error}; no build started");
            std::process::ExitCode::from(75)
        }
    }
}

fn owners_gone(home: &Path, receipt: &Receipt, placing: Option<&super::checkout::Placing>) -> bool {
    // A package roster is diagnostic, not complete. Only its coordinated disposable policy
    // can authorize retirement; missing retained references never establish `Gone`.
    receipt.package.is_none()
        && !receipt.owners_unknown
        && !receipt.owners.is_empty()
        && receipt.owners.iter().all(|owner| {
            scope(home, owner)
                && std::fs::symlink_metadata(owner)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                && super::checkout::gone_placed(owner, Some(home), placing).is_ok()
        })
}

/// Recover a retirement that stopped before moving the original cache. Called under its
/// receipt gate: only the same still-current root and an unchanged owner receipt authorize
/// clearing an empty, identity-bound private stage, or its already-missing name.
#[cfg(unix)]
fn recover_unstarted(
    home: &Path,
    path: &Path,
    receipt: &mut Receipt,
    fence: &super::fence::Fence<'_>,
    coordination: Option<&package::Coordination>,
) -> bool {
    let Some(pending) = receipt.pending.clone() else {
        return false;
    };
    let current = |receipt: &Receipt| {
        same(home, receipt)
            && read(path).as_ref() == Some(receipt)
            && coordination.is_none_or(package::Coordination::removal_allowed)
    };
    if !scope(home, &pending.directory) || !current(receipt) {
        return false;
    }
    match std::fs::symlink_metadata(&pending.directory) {
        Ok(_) => {
            if !identity(&pending.directory).is_some_and(|now| pending.identity.names(&now))
                || !pending.identity.same_volume(&receipt.identity)
                || std::fs::canonicalize(&pending.directory).ok().as_ref()
                    != Some(&pending.directory)
            {
                return false;
            }
            let Ok(mut entries) = std::fs::read_dir(&pending.directory) else {
                return false;
            };
            if entries.next().is_some() {
                return false;
            }
            drop(entries);
            if !fence.clear(&pending.directory, &pending.directory)
                || !current(receipt)
                || !identity(&pending.directory).is_some_and(|now| pending.identity.names(&now))
                || std::fs::remove_dir(&pending.directory).is_err()
            {
                return false;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    if !current(receipt) {
        return false;
    }
    let mut cleared = receipt.clone();
    cleared.pending = None;
    if write(path, &cleared).is_none() {
        return false;
    }
    *receipt = cleared;
    true
}

/// Retire exactly one explicitly requested, genuinely registered ordinary cache. This is
/// separate from automatic age/owner policy: an empty legacy owner roster grants no
/// automatic deletion, but does not prevent a caller naming its own disposable cache.
/// Retention, package custody, unknown ownership, changed identities, Cargo locks and
/// process-use uncertainty still refuse. No tag or owner record is manufactured.
///
/// # Errors
/// An unregistered or unsupported root, retained/package/unknown custody, or a pass that
/// cannot admit any retirement returns a refusal. Returned partial outcomes are not success.
#[cfg(unix)]
pub fn retire(
    home: &Path,
    root: &Path,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Result<Vec<(PathBuf, super::Unused)>, String> {
    retire_on(home, root, record, &super::fence::Fence::default())
}

#[cfg(unix)]
fn retire_on(
    home: &Path,
    root: &Path,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Result<Vec<(PathBuf, super::Unused)>, String> {
    if root.is_absolute()
        && [
            super::cache_root(home),
            super::moved_from(home),
            super::tooling::root(home),
        ]
        .iter()
        .any(|fixed| root.starts_with(fixed))
    {
        return Err(
            "this is a shared or tooling cache; use `aterm pkg machine cache clean`, \
                    which rechecks its safety gates; exact retirement accepts only registered \
                    ordinary cache roots"
                .into(),
        );
    }
    if !root.is_absolute() || !scope(home, root) {
        return Err("retirement needs one exact absolute cache root under this home".into());
    }
    let cache = roots(home)
        .into_iter()
        .find(|cache| cache.root == root)
        .ok_or("cache has no current genuine registration")?;
    let receipt = read(&cache.receipt_path).ok_or("cache registration cannot be read")?;
    if receipt.retained || receipt.creating.is_some() {
        return Err("cache is retained or its creation is incomplete".into());
    }
    if receipt.package.is_some() || receipt.owners_unknown {
        return Err("package or unknown owner custody cannot be retired by this request".into());
    }
    let removed = reclaim_selected_on(
        home,
        std::time::SystemTime::now(),
        super::ReclaimAfter::CALM,
        None,
        record,
        fence,
        Selection::Requested(&cache),
    );
    if removed.is_empty() {
        return Err(
            "cache is active, changed, or could not admit retirement; nothing removed".into(),
        );
    }
    Ok(removed)
}

/// Unsupported hosts cannot retire a registered cache.
///
/// # Errors
/// Always returns an unsupported-host refusal.
#[cfg(not(unix))]
pub fn retire(
    _: &Path,
    _: &Path,
    _: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Result<Vec<(PathBuf, super::Unused)>, String> {
    Err("registered cache retirement is unavailable on this host".into())
}

/// The eligible registered-cache parts, including held locks, for the existing read-only
/// machine-cache planner. These are advisory candidates: cleanup rechecks active use, which
/// this planner does not probe. Current-compiler dependencies stay while any owner still exists.
#[must_use]
pub fn plan(home: &Path, now: std::time::SystemTime) -> Vec<super::Planned> {
    plan_on(home, now, super::ReclaimAfter::CALM.into())
}

pub(super) fn plan_on(
    home: &Path,
    now: std::time::SystemTime,
    policy: super::ReclaimPolicy<'_>,
) -> Vec<super::Planned> {
    let mut planned = Vec::new();
    for cache in roots(home) {
        let Some(preliminary) = read(&cache.receipt_path) else {
            continue;
        };
        if preliminary.retained || preliminary.creating.is_some() {
            continue;
        }
        let Ok(coordination) = package::coordinate(home, &preliminary) else {
            continue;
        };
        let Some(lock_path) = gate_path(&cache.receipt_path) else {
            continue;
        };
        let Ok(lock) = crate::metadata_io::open_regular(&lock_path) else {
            continue;
        };
        let Ok(_held_record) = crate::lock::Flock::try_lock(lock) else {
            continue;
        };
        let Some(receipt) = read(&cache.receipt_path) else {
            continue;
        };
        if receipt.retained {
            continue;
        }
        if coordination
            .as_ref()
            .is_some_and(|guard| !guard.current(home, &receipt))
        {
            continue;
        }
        if let Some(pending) = &receipt.pending {
            if pending_owned(home, &receipt, pending) {
                planned.push(super::Planned::staged(
                    pending.directory.clone(),
                    forecast_pending(&receipt, pending),
                ));
            }
            continue;
        }
        if !same(home, &receipt) {
            continue;
        }
        let Some(seen) = super::look(&cache.root) else {
            continue;
        };
        if seen.locks.is_empty()
            || super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks)
        {
            continue;
        }
        let stage_start = planned.len();
        planned.extend(
            super::stages_in(&cache.root, std::ffi::OsStr::new(super::DOOMED))
                .into_iter()
                .map(super::Planned::stage),
        );
        let held = super::hold(&seen.locks).is_none();
        if let Some(coordinator) = coordination {
            if let Some(why) = coordinator.reason(home, &receipt) {
                planned.push(super::Planned {
                    path: cache.root.clone(),
                    paths: vec![cache.root],
                    why,
                    held,
                    stays: None,
                });
            }
            continue;
        }
        let idle = super::age(now, seen.newest);
        if owners_gone(home, &receipt, None) {
            // An unheld whole-root retirement already contains these stages.
            if !held {
                planned.truncate(stage_start);
            }
            planned.push(super::Planned {
                path: cache.root.clone(),
                paths: vec![cache.root],
                why: super::Unused::Gone { idle },
                held,
                stays: None,
            });
            continue;
        }
        let store = super::compiler_store(Some(home));
        let after = policy.leaf(&cache.root);
        let doomed = super::stale::decide(
            &super::stale::survey(
                &super::stale::Disk,
                &cache.root,
                &super::profiles_of(&seen.locks),
                store.as_deref(),
            ),
            now,
            after.stale,
        );
        if !doomed.is_empty() {
            let count = |which| doomed.iter().filter(|part| part.part == which).count();
            let incremental = count(super::stale::Part::Incremental);
            let units = count(super::stale::Part::Unit);
            let superseded = count(super::stale::Part::Superseded);
            planned.push(super::Planned {
                path: cache.root,
                paths: doomed.into_iter().flat_map(|part| part.paths).collect(),
                why: super::Unused::Stale {
                    incremental,
                    units,
                    superseded,
                    after: after.stale,
                },
                held,
                stays: None,
            });
        }
    }
    planned
}

/// A registered root's partial cleanup uses exactly the normal cache's stale-unit rule.
/// Schema1 whole retirement requires every recorded workspace to have disappeared; package
/// roots instead require their coordinator's budget or superseded policy authority.
/// The record lock, complete Cargo-lock scan and identity are checked again before acting.
/// Each intent `record` puts on record closes with an outcome: what went, what stayed of its
/// stage, or, when nothing of it went (its rename into the private stage refused, or a check
/// made again once the intent was on record refusing it), [`super::Unused::Refused`].
#[cfg(unix)]
pub fn reclaim(
    home: &Path,
    now: std::time::SystemTime,
    after: super::ReclaimAfter,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_on(
        home,
        now,
        after,
        placing,
        record,
        &super::fence::Fence::default(),
    )
}

#[cfg(unix)]
pub(super) fn reclaim_on(
    home: &Path,
    now: std::time::SystemTime,
    after: super::ReclaimAfter,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_selected_on(
        home,
        now,
        after,
        placing,
        record,
        fence,
        Selection::Automatic,
    )
}

/// [`reclaim_on`] short of a live cache's stale parts and of every package target: what costs
/// nobody a compile, which a pass takes in every root before any live part
/// ([`super::reclaim_roots`]).
#[cfg(unix)]
pub(super) fn reclaim_dead_on(
    home: &Path,
    now: std::time::SystemTime,
    after: super::ReclaimPolicy<'_>,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_selected_on(home, now, after, placing, record, fence, Selection::Dead)
}

/// What [`reclaim_dead_on`] left to the end of a pass: a live cache's stale parts, and each package
/// target whole. Retiring one costs a cold build, and only here has a low-disk reason read what
/// the rest of the pass freed. What that run left of the rest is its to leave.
#[cfg(unix)]
pub(super) fn reclaim_live_on(
    home: &Path,
    now: std::time::SystemTime,
    after: super::ReclaimPolicy<'_>,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_selected_on(home, now, after, placing, record, fence, Selection::Live)
}

#[cfg(unix)]
enum Selection<'a> {
    Automatic,
    Dead,
    Live,
    Package(package::Context<'a>),
    Requested(&'a Registered),
}

#[cfg(unix)]
fn reclaim_selected_on<'policy>(
    home: &Path,
    now: std::time::SystemTime,
    after: impl Into<super::ReclaimPolicy<'policy>>,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
    selection: Selection<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    let after = after.into();
    let dead_only = matches!(selection, Selection::Dead);
    let live_only = matches!(selection, Selection::Live);
    let (selected_cache, requested, kernel) = match selection {
        Selection::Automatic | Selection::Dead | Selection::Live => {
            (None, None, &package::Live as &dyn package::Kernel)
        }
        Selection::Package(context) => (context.cache, None, context.kernel),
        Selection::Requested(root) => (None, Some(root), &package::Live as &dyn package::Kernel),
    };
    let mut intents = super::Intents::new(record);
    let candidates = requested.map_or_else(|| roots(home), |wanted| vec![wanted.clone()]);
    for cache in candidates {
        let Some(preliminary) = read(&cache.receipt_path) else {
            fence.note(&cache.root, super::KeepReason::RegistrationChanged);
            continue;
        };
        if preliminary.retained || preliminary.creating.is_some() {
            fence.note(&cache.root, super::KeepReason::Custody);
            continue;
        }
        if selected_cache.is_some_and(|selected| !package::belongs(&preliminary, selected)) {
            continue;
        }
        if dead_only && preliminary.package.is_some() {
            continue;
        }
        let takes = !live_only || preliminary.package.is_some();
        // Take a package's whole-build coordinator BEFORE its receipt gate, including the
        // resume path. Keep it alive until the last staged entry and receipt are settled.
        let Ok(coordination) = package::coordinate(home, &preliminary) else {
            fence.note(&cache.root, super::KeepReason::CoordinationUnavailable);
            continue;
        };
        let Some(lock_path) = gate_path(&cache.receipt_path) else {
            fence.note(&cache.root, super::KeepReason::RegistrationChanged);
            continue;
        };
        let Ok(lock) = crate::metadata_io::open_regular(&lock_path) else {
            fence.note(&cache.root, super::KeepReason::CoordinationUnavailable);
            continue;
        };
        let Ok(_held_record) = crate::lock::Flock::try_lock(lock) else {
            fence.note(&cache.root, super::KeepReason::CoordinationUnavailable);
            continue;
        };
        let Some(mut receipt) = read(&cache.receipt_path) else {
            fence.note(&cache.root, super::KeepReason::RegistrationChanged);
            continue;
        };
        if receipt.retained
            || receipt.creating.is_some()
            || requested.is_some_and(|wanted| !wanted.matches(&receipt))
            || !package::same_declaration(preliminary.package.as_ref(), receipt.package.as_ref())
        {
            fence.note(&cache.root, super::KeepReason::RegistrationChanged);
            continue;
        }
        if receipt.root != cache.root || !scope(home, &receipt.root) {
            continue;
        }
        if requested.is_some() && (receipt.package.is_some() || receipt.owners_unknown) {
            continue;
        }
        if let Some(coordinator) = &coordination
            && (!coordinator.recover_on(home, &cache.receipt_path, &mut receipt, fence, kernel)
                || !coordinator.current(home, &receipt))
        {
            fence.note(&cache.root, super::KeepReason::PackageChanged);
            continue;
        }
        // Under the gate held, a settled receipt with no volume UUID gains its folder's while this
        // boot still numbers the volume as it was recorded (VOLUME-WITNESS), so a cache no build
        // touched before a restart still binds after it ([`witness`]).
        witness(home, &cache.receipt_path, &mut receipt);
        if let Some(mut pending) = receipt.pending.clone() {
            if !takes {
                continue;
            }
            // The remover may have finished before the final receipt write reached disk.
            // A policy's coordinator still excludes another generation: only two genuinely
            // absent names and unchanged authorization permit this metadata-only settlement.
            let absent = |path: &Path| {
                std::fs::symlink_metadata(path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            };
            if let Some(coordinator) = &coordination
                && pending.authorized
                && scope(home, &pending.directory)
                && absent(&receipt.root)
                && absent(&pending.directory)
                && fence.clear(&receipt.root, &pending.directory)
                && read(&cache.receipt_path).as_ref() == Some(&receipt)
                && coordinator.current(home, &receipt)
                && absent(&receipt.root)
                && absent(&pending.directory)
            {
                settle(&cache.receipt_path, &mut receipt);
                continue;
            }
            if !pending_owned(home, &receipt, &pending) {
                let _ = recover_unstarted(
                    home,
                    &cache.receipt_path,
                    &mut receipt,
                    fence,
                    coordination.as_ref(),
                );
                continue;
            }
            let payload = pending.directory.join(PAYLOAD);
            let pending_locks = match std::fs::symlink_metadata(&payload) {
                Ok(_) => match super::locks_under(&payload) {
                    Ok(locks) => locks,
                    Err(_) => {
                        fence.note(&pending.directory, super::KeepReason::InventoryUnavailable);
                        continue;
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => {
                    fence.note(&pending.directory, super::KeepReason::InventoryUnavailable);
                    continue;
                }
            };
            let Some(_held_pending) = super::hold(&pending_locks) else {
                fence.note(&pending.directory, super::KeepReason::CargoLocks);
                continue;
            };
            if fence.clear(&cache.root, &pending.directory)
                && fence.admit(
                    &pending.directory,
                    super::KeepReason::RegistrationChanged,
                    read(&cache.receipt_path).as_ref() == Some(&receipt),
                )
                && fence.admit(
                    &pending.directory,
                    super::KeepReason::PackageChanged,
                    coordination
                        .as_ref()
                        .is_none_or(|guard| guard.current(home, &receipt)),
                )
                && fence.admit(
                    &pending.directory,
                    super::KeepReason::IntentDeclined,
                    intents.ask_staged(&pending.directory, super::Unused::Unfinished),
                )
            {
                pending.authorized = true;
                receipt.pending = Some(pending.clone());
                if write(&cache.receipt_path, &receipt).is_none() {
                    continue;
                }
                if !fence.clear(&cache.root, &pending.directory)
                    || read(&cache.receipt_path).as_ref() != Some(&receipt)
                    || !pending_owned(home, &receipt, &pending)
                    || coordination
                        .as_ref()
                        .is_some_and(|guard| !guard.current(home, &receipt))
                {
                    continue;
                }
                let left = match remove_pending(
                    &cache.receipt_path,
                    &receipt,
                    &pending,
                    coordination.as_ref(),
                ) {
                    Ok(left) => left,
                    Err(revoked) => {
                        intents.outcome(
                            pending.directory.clone(),
                            super::Unused::Revoked {
                                freed: revoked.freed,
                            },
                        );
                        continue;
                    }
                };
                if left.whole() {
                    intents.outcome(pending.directory.clone(), super::Unused::Unfinished);
                    if coordination
                        .as_ref()
                        .is_none_or(package::Coordination::removal_allowed)
                    {
                        settle(&cache.receipt_path, &mut receipt);
                    }
                } else {
                    intents.outcome(
                        pending.directory.clone(),
                        super::Unused::Stayed {
                            stayed: left.stayed,
                            freed: left.freed,
                        },
                    );
                }
            }
            continue;
        }
        if !same(home, &receipt) {
            fence.note(&cache.root, super::KeepReason::RegistrationChanged);
            continue;
        }
        let Some(seen) = super::look(&cache.root) else {
            fence.note(&cache.root, super::KeepReason::InventoryUnavailable);
            continue;
        };
        // A complete inventory without Cargo locks cannot witness an idle cache.
        if seen.locks.is_empty() {
            fence.note(&cache.root, super::KeepReason::NoCargoLocks);
            continue;
        }
        if super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks) {
            fence.note(&cache.root, super::KeepReason::InventoryChanged);
            continue;
        }
        let gone = owners_gone(home, &receipt, placing);
        let budget = coordination
            .as_ref()
            .and_then(|guard| guard.reason_on(home, &receipt, kernel));
        let whole = requested
            .map(|_| super::unused(&cache.root, &|_| Some(super::Unused::Requested), fence))
            .or_else(|| {
                budget
                    .map(|why| super::unused(&cache.root, &|_| Some(why), fence))
                    .or_else(|| {
                        gone.then(|| {
                            super::unused(
                                &cache.root,
                                &|seen| {
                                    Some(super::Unused::Gone {
                                        idle: super::age(now, seen.newest),
                                    })
                                },
                                fence,
                            )
                        })
                    })
            });
        if let Some(Some((held_cargo, why))) = whole {
            if !takes {
                continue;
            }
            if !fence.admit(
                &cache.root,
                super::KeepReason::RegistrationChanged,
                same(home, &receipt),
            ) || !fence.admit(
                &cache.root,
                super::KeepReason::RegistrationChanged,
                read(&cache.receipt_path).as_ref() == Some(&receipt),
            ) || !fence.admit(
                &cache.root,
                super::KeepReason::PackageChanged,
                !coordination
                    .as_ref()
                    .is_some_and(|guard| !guard.current(home, &receipt)),
            ) || !fence.clear(&cache.root, &cache.root)
                || !intents.ask(&cache.root, why)
                || !fence.clear(&cache.root, &cache.root)
            {
                continue;
            }
            let Some(parent) = cache.root.parent() else {
                fence.note(&cache.root, super::KeepReason::CannotStage);
                continue;
            };
            let Ok(stage) = aterm_tempfile::Builder::new()
                .prefix("atpkg-cache-retiring-")
                .tempdir_in(parent)
            else {
                fence.note(&cache.root, super::KeepReason::CannotStage);
                continue;
            };
            let directory = stage.keep();
            let Some(stage_id) = identity(&directory) else {
                fence.note(&cache.root, super::KeepReason::RegistrationChanged);
                continue;
            };
            receipt.pending = Some(Pending {
                directory: directory.clone(),
                identity: stage_id,
                authorized: false,
            });
            // Keep the private stage and its receipt on every failure, including a crash.
            if write(&cache.receipt_path, &receipt).is_none() {
                fence.note(&cache.root, super::KeepReason::RegistrationChanged);
                continue;
            }
            let staged = directory.join(PAYLOAD);
            if !same(home, &receipt)
                || (coordination.is_none()
                    && requested.is_none()
                    && !owners_gone(home, &receipt, placing))
                || coordination
                    .as_ref()
                    .is_some_and(|guard| !guard.current(home, &receipt))
                || read(&cache.receipt_path).as_ref() != Some(&receipt)
                || super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks)
                || !fence.clear(&cache.root, &cache.root)
                || coordination
                    .as_ref()
                    .is_some_and(|guard| !guard.current(home, &receipt))
            {
                continue;
            }
            if let Err(e) = std::fs::rename(&cache.root, &staged) {
                intents.refused(super::Refusal::Rename(e.kind()));
                continue;
            }
            // From here a stop keeps it in its pending stage, which the next pass finishes.
            intents.moved(&directory);
            // A pathname could have been replaced between the last check and rename. Such
            // bytes are kept staged, never passed to the remover as this cache's payload.
            if !receipt
                .pending
                .as_ref()
                .is_some_and(|pending| pending_owned(home, &receipt, pending))
            {
                continue;
            }
            if !fence.clear(&cache.root, &directory)
                || coordination
                    .as_ref()
                    .is_some_and(|guard| !guard.current(home, &receipt))
                || read(&cache.receipt_path).as_ref() != Some(&receipt)
            {
                continue;
            }
            if let Some(pending) = receipt.pending.as_mut() {
                pending.authorized = true;
            }
            if write(&cache.receipt_path, &receipt).is_none() {
                continue;
            }
            drop(held_cargo);
            if !fence.clear(&cache.root, &directory)
                || coordination
                    .as_ref()
                    .is_some_and(|guard| !guard.current(home, &receipt))
                || read(&cache.receipt_path).as_ref() != Some(&receipt)
                || !receipt
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending_owned(home, &receipt, pending))
            {
                continue;
            }
            let Some(pending) = receipt.pending.as_ref() else {
                continue;
            };
            let left = match remove_pending(
                &cache.receipt_path,
                &receipt,
                pending,
                coordination.as_ref(),
            ) {
                Ok(left) => left,
                Err(revoked) => {
                    intents.outcome(
                        directory,
                        super::Unused::Revoked {
                            freed: revoked.freed,
                        },
                    );
                    continue;
                }
            };
            if left.whole() {
                intents.outcome(cache.root, why);
                if coordination
                    .as_ref()
                    .is_none_or(package::Coordination::removal_allowed)
                {
                    settle(&cache.receipt_path, &mut receipt);
                }
            } else {
                intents.outcome(
                    directory,
                    super::Unused::Stayed {
                        stayed: left.stayed,
                        freed: left.freed,
                    },
                );
            }
        } else {
            // An opted-in live package is bounded only by whole-generation retirement.
            // It never uses the ordinary stale-unit/partial current-deps pruning path.
            if coordination.is_some() || requested.is_some() {
                continue;
            }
            let permitted = || retention::allows(&cache.receipt_path, &receipt);
            let mut sweep = super::Sweep {
                root: &cache.root,
                home: Some(home),
                now,
                after,
                placing,
                moved: false,
                compiler_store: super::compiler_store(Some(home)),
                intents: &mut intents,
                fence,
                permitted: &permitted,
                pruning: Vec::new(),
                declined: false,
            };
            // `prune` makes a fresh stage; only `walk` finishes older stages now. A registered
            // target is not a mirror tree, so finish just its own stages under this receipt gate.
            if takes {
                for stage in super::stages_in(&cache.root, std::ffi::OsStr::new(super::DOOMED)) {
                    if !same(home, &receipt) {
                        break;
                    }
                    if sweep.stage_recorded(&stage, &cache.root) && same(home, &receipt) {
                        sweep.finish_stage(&stage, &cache.root);
                    }
                }
            }
            if !dead_only && same(home, &receipt) {
                let _ = sweep.prune(&cache.root, &cache.root);
            }
        }
    }
    intents.finish()
}

/// The name a pending stage holds its payload under: the cache root renamed into it.
const PAYLOAD: &str = "cache.noindex";

/// The names whose standing in a package payload's root stops its removal: a workspace there is
/// someone's.
#[cfg(unix)]
fn protected(receipt: &Receipt) -> &'static [&'static str] {
    if receipt.package.is_some() {
        &[".git", "Cargo.toml", "Targo.toml"]
    } else {
        &[]
    }
}

/// REMOVE A PENDING STAGE by the identified rules: its payload ([`PAYLOAD`]) as the very folder
/// the receipt names, in the very stage it names ([`super::remove::tree_identified`]), and then
/// the stage, only once nothing else stands in it ([`super::remove::empty_identified`]). What
/// stayed is the payload's when it did not go whole, named under the stage the pass returns it
/// beside ([`super::remove::named_as`]), else the stage's own.
#[cfg(unix)]
fn remove_pending(
    path: &Path,
    receipt: &Receipt,
    pending: &Pending,
    coordination: Option<&package::Coordination>,
) -> Result<super::remove::Left, super::remove::Revoked> {
    let payload = pending.directory.join(PAYLOAD);
    // Custody is read by path above the payload, its stage's own included; the payload's own is
    // the walk's to read, through the folder it holds ([`retention::allows_walk`]).
    let permitted = || {
        retention::allows(path, receipt)
            && retention::allows_walk(&payload)
            && coordination.is_none_or(package::Coordination::removal_allowed)
    };
    let left = super::remove::tree_identified(
        &payload,
        &pending.identity,
        &receipt.identity,
        protected(receipt),
        &permitted,
    )?;
    if !left.whole() {
        super::remove::named_as(&payload, &pending.directory);
        return Ok(left);
    }
    match super::remove::empty_identified(&pending.directory, &pending.identity, &permitted) {
        Ok(stage) => Ok(super::remove::Left {
            freed: left.freed.saturating_add(stage.freed),
            stayed: stage.stayed,
        }),
        Err(revoked) => Err(super::remove::Revoked {
            freed: left.freed.saturating_add(revoked.freed),
        }),
    }
}

/// WHAT OF A PENDING STAGE ITS REMOVAL WILL LEAVE ([`remove_pending`]), read and never acted on,
/// by that removal's own rules: the payload forecast as [`super::remove::tree_identified`]
/// removes it ([`super::remove::forecast_tree_identified`]: another inode at its name, or a
/// protected name in it, stops it), each entry named under the stage; and, once it would go
/// whole, the stage as [`super::remove::empty_identified`] removes it after the payload went
/// ([`super::remove::forecast_empty_identified`]: it stays while it holds anything else).
#[cfg(unix)]
fn forecast_pending(receipt: &Receipt, pending: &Pending) -> super::remove::Forecast {
    let payload = pending.directory.join(PAYLOAD);
    let mut forecast = super::remove::forecast_tree_identified(
        &payload,
        &pending.identity,
        &receipt.identity,
        protected(receipt),
        super::remove::NAMED,
    );
    for (rel, _) in &mut forecast.first {
        *rel = if rel.as_os_str().is_empty() {
            PathBuf::from(PAYLOAD)
        } else {
            Path::new(PAYLOAD).join(&*rel)
        };
    }
    if forecast.count > 0 || !forecast.whole || forecast.stops.is_some() {
        return forecast;
    }
    let mut stage = super::remove::forecast_empty_identified(
        &pending.directory,
        &pending.identity,
        &[PAYLOAD],
        super::remove::NAMED,
    );
    stage.walked = stage.walked.saturating_add(forecast.walked);
    stage
}

/// Off Unix no pending stage is removed ([`reclaim`]): the plain forecast.
#[cfg(not(unix))]
fn forecast_pending(_: &Receipt, pending: &Pending) -> super::remove::Forecast {
    super::remove::forecast(&pending.directory, super::remove::NAMED)
}

#[cfg(not(unix))]
pub fn reclaim(
    _: &Path,
    _: std::time::SystemTime,
    _: super::ReclaimAfter,
    _: Option<&super::checkout::Placing>,
    _: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Vec<(PathBuf, super::Unused)> {
    Vec::new()
}

#[cfg(not(unix))]
pub(super) fn reclaim_dead_on(
    _: &Path,
    _: std::time::SystemTime,
    _: super::ReclaimPolicy<'_>,
    _: Option<&super::checkout::Placing>,
    _: &mut dyn FnMut(&Path, super::Unused) -> bool,
    _: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    Vec::new()
}

#[cfg(not(unix))]
pub(super) fn reclaim_live_on(
    _: &Path,
    _: std::time::SystemTime,
    _: super::ReclaimPolicy<'_>,
    _: Option<&super::checkout::Placing>,
    _: &mut dyn FnMut(&Path, super::Unused) -> bool,
    _: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    Vec::new()
}

/// An interrupted, identity-bound retirement fixture, after its image proved execution.
#[cfg(all(test, unix))]
pub(super) fn stage_for_process_test(home: &Path, root: &Path, stage: &Path) {
    let path = home
        .join(INDEX)
        .join(format!("{}.json", key(root).unwrap()));
    let mut receipt = read(&path).unwrap();
    // Private whatever the umask, as a retirement makes its stage (`aterm_tempfile`, 0o700).
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(stage)
            .unwrap();
    }
    receipt.pending = Some(Pending {
        directory: stage.to_path_buf(),
        identity: identity(stage).unwrap(),
        authorized: false,
    });
    write(&path, &receipt).unwrap();
    std::fs::rename(root, stage.join("cache.noindex")).unwrap();
    assert!(pending_owned(
        home,
        &receipt,
        receipt.pending.as_ref().unwrap()
    ));
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::time::{Duration, SystemTime};

    #[test]
    fn resource_capabilities_query_reaches_native_without_observing_cache_ownership() {
        let args = vec!["--resource-capabilities".to_owned()];
        let home = Path::new("/unreadable-home");
        let machine = super::super::Machine {
            home: Some(home),
            root: None,
            cwd: Some(home),
            env: &|_| panic!("a capability query must not inspect inherited cache settings"),
            read: &|_| panic!("a capability query must not read or register caches"),
            writable: &|_| panic!("a capability query must not require cache writes"),
        };
        assert!(
            !observe_for_producer(&args, super::super::Tool::Targo, home, &machine, true).unwrap()
        );
        assert!(crate::build_admit::classify(&args, super::super::Tool::Targo).is_none());
        for tool in [super::super::Tool::Cargo, super::super::Tool::Tippy] {
            assert!(!non_producing_command(&args, tool, &machine));
        }
        for args in [
            vec!["--resource-capabilities", "build"],
            vec!["--resource-capabilities", "--target-dir=elsewhere"],
            vec!["--unverified", "--resource-capabilities"],
            vec!["alias", "--resource-capabilities"],
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(!non_producing_command(
                &args,
                super::super::Tool::Targo,
                &machine
            ));
        }
    }

    struct World {
        _directory: aterm_tempfile::TempDir,
        home: PathBuf,
        owner: PathBuf,
        target: PathBuf,
    }

    #[test]
    fn native_build_tree_administration_does_not_read_or_adopt_build_caches() {
        let directory = aterm_tempfile::tempdir().unwrap();
        let home = directory.path();
        let machine = super::super::Machine {
            home: Some(home),
            root: None,
            cwd: Some(home),
            env: &|_| panic!("native tree administration must not inspect build-cache settings"),
            read: &|_| panic!("native tree administration must not read build-cache config"),
            writable: &|_| panic!("native tree administration must not register build output"),
        };
        for operation in [
            None,
            Some("--protocol"),
            Some("--help"),
            Some("-h"),
            Some("status"),
            Some("acquire"),
            Some("release"),
            Some("break"),
            Some("reclaim-incremental"),
        ] {
            let mut args = vec!["trust".into(), "repo".into(), "lock".into()];
            args.extend(operation.map(str::to_owned));
            assert!(
                !observe_for_producer(&args, super::super::Tool::Targo, home, &machine, true)
                    .unwrap(),
                "{args:?}"
            );
            assert!(crate::build_admit::classify(&args, super::super::Tool::Targo).is_none());
            for tool in [super::super::Tool::Cargo, super::super::Tool::Tippy] {
                assert!(!non_producing_command(&args, tool, &machine));
            }
        }
        assert_eq!(std::fs::read_dir(home).unwrap().count(), 0);
        for words in [
            vec!["trust", "repo", "lock", "exec", "--", "build"],
            vec!["trust", "repo", "lock", "unknown"],
            vec!["trust", "repo", "locker", "status"],
            vec!["--config", "trust", "repo", "lock", "status"],
            vec!["alias", "trust", "repo", "lock", "status"],
            vec![
                "--unverified",
                "run",
                "--",
                "trust",
                "repo",
                "lock",
                "status",
            ],
            vec!["--unverified", "build", "trust", "repo", "lock", "status"],
        ] {
            let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(
                !non_producing_command(&args, super::super::Tool::Targo, &machine),
                "{args:?}"
            );
        }
    }

    impl World {
        fn new() -> Self {
            let directory = aterm_tempfile::tempdir().unwrap();
            Self::in_directory(directory)
        }

        fn in_directory(directory: aterm_tempfile::TempDir) -> Self {
            let home = directory.path().canonicalize().unwrap().join("home");
            // Writable by its owner alone whatever the umask, as a person's home is: the index
            // binds only such folders, so a home umask 002 left group-writable registers nothing.
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&home)
                .unwrap();
            let owner = home.join("source");
            std::fs::create_dir(&owner).unwrap();
            std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
            Self {
                _directory: directory,
                target: home.join("outputs.noindex"),
                home,
                owner,
            }
        }

        fn another_owner(&self) -> PathBuf {
            let owner = self.home.join("another-source");
            std::fs::create_dir(&owner).unwrap();
            std::fs::write(owner.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
            owner
        }

        fn build(&self) {
            assert!(create_checked(&self.home, &self.target, &self.owner).unwrap());
            std::fs::create_dir_all(self.target.join("debug/deps")).unwrap();
            std::fs::write(self.target.join("debug/deps/keep"), b"reusable output").unwrap();
            std::fs::write(self.target.join("debug/.cargo-lock"), b"").unwrap();
        }

        fn path(&self) -> PathBuf {
            self.home
                .join(INDEX)
                .join(format!("{}.json", key(&self.target).unwrap()))
        }

        fn later(&self) -> SystemTime {
            SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60)
        }
    }

    #[test]
    fn explicit_retirement_handles_a_tagless_legacy_roster_without_granting_automatic_deletion() {
        let world = World::new();
        world.build();
        let other = world.home.join("other-output.noindex");
        assert!(create_checked(&world.home, &other, &world.owner).unwrap());
        std::fs::create_dir_all(other.join("debug/deps")).unwrap();
        std::fs::write(other.join("debug/.cargo-lock"), b"").unwrap();
        std::fs::write(other.join("debug/deps/keep"), b"another cache").unwrap();
        let path = world.path();
        let mut receipt = read(&path).unwrap();
        receipt.owners.clear();
        write(&path, &receipt).unwrap();
        let tag = world.target.join("CACHEDIR.TAG");
        if tag.exists() {
            std::fs::remove_file(&tag).unwrap();
        }
        let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let fence = super::super::fence::Fence::new(&processes);
        assert!(
            reclaim_on(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true,
                &fence
            )
            .is_empty()
        );
        assert!(world.target.join("debug/deps/keep").is_file());
        let removed = retire_on(&world.home, &world.target, &mut |_, _| true, &fence).unwrap();
        assert!(
            matches!(removed.as_slice(), [(root, super::super::Unused::Requested)] if root == &world.target)
        );
        assert!(!world.target.exists());
        assert_eq!(
            std::fs::read(other.join("debug/deps/keep")).unwrap(),
            b"another cache"
        );
        assert!(world.owner.join("Cargo.toml").is_file());
    }

    #[test]
    fn explicit_retirement_refuses_unregistered_retained_and_unknown_custody() {
        for kind in ["unregistered", "retained", "unknown"] {
            let world = World::new();
            if kind == "unregistered" {
                std::fs::create_dir(&world.target).unwrap();
                std::fs::write(world.target.join("keep"), b"unclaimed data").unwrap();
            } else {
                world.build();
                if kind == "retained" {
                    retain(&world.home, &world.target).unwrap();
                } else {
                    let path = world.path();
                    let mut receipt = read(&path).unwrap();
                    receipt.owners_unknown = true;
                    write(&path, &receipt).unwrap();
                }
            }
            let processes = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
            assert!(
                retire_on(
                    &world.home,
                    &world.target,
                    &mut |_, _| true,
                    &super::super::fence::Fence::new(&processes)
                )
                .is_err(),
                "{kind}"
            );
            assert!(world.target.is_dir(), "{kind}");
        }
    }

    #[test]
    fn explicit_retirement_keeps_cargo_locks_and_unknown_or_live_processes_authoritative() {
        let world = World::new();
        world.build();
        let deep = world
            .target
            .join("verify/.ubd/cells.noindex/linux/triple/debug");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(".cargo-build-lock"), b"").unwrap();
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let held = crate::lock::Flock::try_lock(
            std::fs::File::open(deep.join(".cargo-build-lock")).unwrap(),
        )
        .unwrap();
        assert!(
            retire_on(
                &world.home,
                &world.target,
                &mut |_, _| true,
                &super::super::fence::Fence::new(&clear)
            )
            .is_err()
        );
        drop(held);
        for unknown in [false, true] {
            let processes = |_: &[PathBuf], _: &[u32]| {
                if unknown {
                    return Err(std::io::Error::other("process census unavailable"));
                }
                Ok(aterm_uds::holders::PathScan {
                    users: vec![aterm_uds::holders::PathUse {
                        pid: 42,
                        how: "exe",
                        path: world.target.join("debug/deps/keep"),
                    }],
                    ..Default::default()
                })
            };
            assert!(
                retire_on(
                    &world.home,
                    &world.target,
                    &mut |_, _| true,
                    &super::super::fence::Fence::new(&processes)
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
                b"reusable output"
            );
        }
        let removed = retire_on(
            &world.home,
            &world.target,
            &mut |_, _| true,
            &super::super::fence::Fence::new(&clear),
        )
        .unwrap();
        assert!(
            matches!(removed.as_slice(), [(root, super::super::Unused::Requested)] if root == &world.target)
        );
        assert!(
            !world.target.exists(),
            "the same deep inventory is reclaimable once clear"
        );
    }

    #[test]
    fn explicit_retirement_keeps_a_late_holder_staged_and_resumes_only_the_identified_payload() {
        let world = World::new();
        world.build();
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            let moved = dirs.iter().any(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("atpkg-cache-retiring-"))
                    && p.is_dir()
            });
            Ok(aterm_uds::holders::PathScan {
                users: moved
                    .then(|| aterm_uds::holders::PathUse {
                        pid: 42,
                        how: "exe",
                        path: world.target.join("debug/deps/keep"),
                    })
                    .into_iter()
                    .collect(),
                ..Default::default()
            })
        };
        let _ = retire_on(
            &world.home,
            &world.target,
            &mut |_, _| true,
            &super::super::fence::Fence::new(&processes),
        );
        let pending = read(&world.path()).unwrap().pending.unwrap();
        assert_eq!(
            std::fs::read(pending.directory.join(PAYLOAD).join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let held = crate::lock::Flock::try_lock(
            std::fs::File::open(pending.directory.join(PAYLOAD).join("debug/.cargo-lock")).unwrap(),
        )
        .unwrap();
        let reporting = super::super::fence::Fence::new(&clear).with_reporting();
        assert!(retire_on(&world.home, &world.target, &mut |_, _| true, &reporting).is_err());
        assert!(
            reporting.retained().contains(&super::super::RetainedCheck {
                path: pending.directory.clone(),
                reason: super::super::KeepReason::CargoLocks,
            }),
            "a resumed-stage lock refusal must reach the cleanup diagnostic"
        );
        assert!(
            pending
                .directory
                .join(PAYLOAD)
                .join("debug/deps/keep")
                .is_file()
        );
        drop(held);
        let removed = retire_on(
            &world.home,
            &world.target,
            &mut |_, _| true,
            &super::super::fence::Fence::new(&clear),
        )
        .unwrap();
        assert!(matches!(
            removed.as_slice(),
            [(_, super::super::Unused::Unfinished)]
        ));
        assert!(!pending.directory.exists());
    }

    #[test]
    fn explicit_retirement_does_not_take_a_replacement_published_after_its_intent() {
        let world = World::new();
        world.build();
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let old = world.home.join("preserved.noindex");
        let mut record = |_: &Path, _: super::super::Unused| {
            std::fs::rename(&world.target, &old).unwrap();
            std::fs::create_dir(&world.target).unwrap();
            std::fs::write(world.target.join("keep"), b"replacement data").unwrap();
            true
        };
        let outcomes = retire_on(
            &world.home,
            &world.target,
            &mut record,
            &super::super::fence::Fence::new(&clear),
        )
        .unwrap();
        assert!(matches!(
            outcomes.as_slice(),
            [(_, super::super::Unused::Refused { .. })]
        ));
        assert_eq!(
            std::fs::read(world.target.join("keep")).unwrap(),
            b"replacement data"
        );
        assert_eq!(
            std::fs::read(old.join("debug/deps/keep")).unwrap(),
            b"reusable output"
        );
    }

    #[test]
    fn explicit_retirement_rechecks_its_captured_snapshot_against_the_locked_fresh_receipt() {
        for replaced in [false, true] {
            let world = World::new();
            world.build();
            let wanted = roots(&world.home).pop().unwrap();
            let saved = world.home.join("saved-output.noindex");
            if replaced {
                std::fs::rename(&world.target, &saved).unwrap();
                world.build();
            } else {
                let other = world.another_owner();
                assert!(create_checked(&world.home, &world.target, &other).unwrap());
            }
            let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
            let removed = reclaim_selected_on(
                &world.home,
                SystemTime::now(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true,
                &super::super::fence::Fence::new(&clear),
                Selection::Requested(&wanted),
            );
            assert!(removed.is_empty(), "replaced={replaced}");
            assert_eq!(
                std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
                b"reusable output"
            );
            if replaced {
                assert_eq!(
                    std::fs::read(saved.join("debug/deps/keep")).unwrap(),
                    b"reusable output"
                );
            }
        }
    }

    #[test]
    fn ordinary_clearance_includes_partial_stages_and_refuses_unknown_index_state() {
        let world = World::new();
        world.build();
        let stage = world.home.join("retiring.noindex");
        stage_for_process_test(&world.home, &world.target, &stage);
        let payload = stage.join("cache.noindex");
        std::fs::remove_file(payload.join(MARKER)).unwrap();
        for held in [
            &stage,
            &payload,
            &payload.join("debug/deps/keep"),
            &world.target,
        ] {
            assert!(ordinary_clearance(&world.home, held).is_err());
        }
        let unrelated = world.home.join("unrelated.noindex");
        let clearance = ordinary_clearance(&world.home, &unrelated).unwrap();
        assert!(clearance.current());
        let index = world.home.join(INDEX);
        std::fs::rename(&index, world.home.join("former-index")).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&index)
            .unwrap();
        assert!(!clearance.current());
        std::fs::write(index.join("unknown.json"), br#"{"schema":999}"#).unwrap();
        assert!(ordinary_clearance(&world.home, &unrelated).is_err());
    }

    /// SYMLINKED OR ALIASED ANCESTORS CANNOT BYPASS `ordinary_clearance`: resolving the deepest
    /// existing ancestor of `path` catches a symlinked parent directory, a symlink naming the
    /// registered target, a non-existent child reached through that symlink, and a dangling
    /// symlink. NEGATIVE CONTROL: a real sibling directory outside the registered root still clears.
    #[test]
    fn ordinary_clearance_refuses_symlinked_ancestors_and_aliases_into_registered_custody() {
        let world = World::new();
        world.build();
        let real_sibling = world.home.join("real-sibling.noindex");
        assert!(ordinary_clearance(&world.home, &real_sibling).is_ok());

        let parent_link = world.home.join("alias-home");
        std::os::unix::fs::symlink(&world.home, &parent_link).unwrap();
        let target_name = world.target.file_name().unwrap();
        let aliased_target = parent_link.join(target_name);
        let aliased_child = aliased_target.join("debug/deps/future-unit");
        assert!(
            ordinary_clearance(&world.home, &aliased_target).is_err(),
            "registered target named through a symlinked ancestor must be refused"
        );
        assert!(
            ordinary_clearance(&world.home, &aliased_child).is_err(),
            "non-existent descendant reached through a symlinked ancestor must be refused"
        );
        assert!(
            ordinary_clearance(&world.home, &parent_link).is_err(),
            "symlinked directory whose target contains a registered root must be refused"
        );

        let dangling = world.home.join("dangling.noindex");
        std::os::unix::fs::symlink(world.home.join("missing-target"), &dangling).unwrap();
        assert!(
            ordinary_clearance(&world.home, &dangling).is_err(),
            "dangling symlink cannot receive ordinary clearance"
        );
    }

    #[test]
    fn an_unstarted_retirement_recovers_only_its_unchanged_empty_or_missing_stage() {
        #[derive(Clone, Copy, Debug)]
        enum Stage {
            Empty,
            Missing,
            Nonempty,
            HeldEmpty,
            OriginalInUse,
            Replaced,
            ChangedReceipt,
            ChangedRoot,
        }
        for state in [
            Stage::Empty,
            Stage::Missing,
            Stage::Nonempty,
            Stage::HeldEmpty,
            Stage::OriginalInUse,
            Stage::Replaced,
            Stage::ChangedReceipt,
            Stage::ChangedRoot,
        ] {
            let world = World::new();
            world.build();
            let path = world.path();
            let lock = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
            let _held = crate::lock::Flock::try_lock(lock).unwrap();
            let mut receipt = read(&path).unwrap();
            let stage = aterm_tempfile::Builder::new()
                .prefix("atpkg-cache-retiring-")
                .tempdir_in(&world.home)
                .unwrap()
                .keep();
            receipt.pending = Some(Pending {
                directory: stage.clone(),
                identity: identity(&stage).unwrap(),
                authorized: false,
            });
            write(&path, &receipt).unwrap();
            match state {
                Stage::Empty | Stage::HeldEmpty | Stage::OriginalInUse => {}
                Stage::Missing => std::fs::remove_dir(&stage).unwrap(),
                Stage::Nonempty => std::fs::write(stage.join("keep"), b"owner data").unwrap(),
                Stage::Replaced => {
                    std::fs::rename(&stage, stage.with_extension("original")).unwrap();
                    std::fs::create_dir(&stage).unwrap();
                }
                Stage::ChangedReceipt => {
                    let mut changed = receipt.clone();
                    changed.owners_unknown = true;
                    write(&path, &changed).unwrap();
                }
                Stage::ChangedRoot => {
                    std::fs::rename(&world.target, world.home.join("original.noindex")).unwrap();
                    std::fs::create_dir(&world.target).unwrap();
                }
            }
            let before = std::fs::read(&path).unwrap();
            let recover = matches!(state, Stage::Empty | Stage::Missing | Stage::OriginalInUse);
            let processes = |dirs: &[PathBuf], _: &[u32]| {
                assert!(dirs.contains(&stage));
                let reported = match state {
                    Stage::HeldEmpty => Some(("cwd", stage.clone())),
                    Stage::OriginalInUse => Some(("exe", world.target.join("debug/deps/keep"))),
                    _ => None,
                };
                Ok(aterm_uds::holders::PathScan {
                    users: reported
                        .filter(|(_, path)| dirs.iter().any(|dir| path.starts_with(dir)))
                        .map(|(how, path)| aterm_uds::holders::PathUse { pid: 34, how, path })
                        .into_iter()
                        .collect(),
                    ..Default::default()
                })
            };
            assert_eq!(
                recover_unstarted(
                    &world.home,
                    &path,
                    &mut receipt,
                    &super::super::fence::Fence::new(&processes),
                    None,
                ),
                recover,
                "{state:?}"
            );
            if recover {
                assert!(receipt.pending.is_none() && !stage.exists(), "{state:?}");
                assert!(roots(&world.home)[0].is_current(&world.home), "{state:?}");
            } else {
                assert_eq!(std::fs::read(&path).unwrap(), before, "{state:?}");
                assert!(receipt.pending.is_some() && stage.is_dir(), "{state:?}");
                if matches!(state, Stage::Nonempty) {
                    assert_eq!(std::fs::read(stage.join("keep")).unwrap(), b"owner data");
                }
            }
            let original = if matches!(state, Stage::ChangedRoot) {
                world.home.join("original.noindex")
            } else {
                world.target.clone()
            };
            assert_eq!(
                std::fs::read(original.join("debug/deps/keep")).unwrap(),
                b"reusable output"
            );
        }
    }

    #[test]
    fn fresh_creation_preserves_output_locations_and_never_adopts_a_folder_cargo_did_not_make() {
        let world = World::new();
        world.build();
        assert!(world.target.join("debug/deps/keep").is_file());
        assert!(!world.target.join("build.noindex").exists());
        let unknown = world.home.join("existing.noindex");
        std::fs::create_dir(&unknown).unwrap();
        std::fs::write(unknown.join("keep"), b"existing data").unwrap();
        assert!(!create_checked(&world.home, &unknown, &world.owner).unwrap());
        assert_eq!(
            std::fs::read(unknown.join("keep")).unwrap(),
            b"existing data"
        );
        assert!(!unknown.join(MARKER).exists());
        assert_eq!(roots(&world.home).len(), 1);
    }

    #[test]
    fn package_registers_its_fresh_target_without_adopting_existing_output() {
        for no_verify in [false, true] {
            let world = World::new();
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &|_| None,
                read: &|path| {
                    if path.starts_with(&world.home) {
                        super::super::read_config(path)
                    } else {
                        Ok(None)
                    }
                },
                writable: &|_| Some(true),
            };
            let mut args = vec![
                "--unverified".into(),
                "package".into(),
                format!("--target-dir={}", world.target.display()),
            ];
            if no_verify {
                args.push("--no-verify".into());
            }
            assert!(
                observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine,
                    true
                )
                .unwrap()
            );
            let receipt = read(&world.path()).unwrap();
            assert_eq!(receipt.root, world.target);
            assert_eq!(receipt.owners, vec![world.owner.clone()]);
            assert!(world.target.join(MARKER).is_file());

            let existing = world.home.join("unmanaged-package.noindex");
            std::fs::create_dir(&existing).unwrap();
            std::fs::write(existing.join("keep"), b"owner artifact").unwrap();
            args[2] = format!("--target-dir={}", existing.display());
            assert!(
                observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine,
                    true
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(existing.join("keep")).unwrap(),
                b"owner artifact"
            );
            assert!(!existing.join(MARKER).exists());
        }
    }

    #[test]
    fn observations_distinguish_unmanaged_outputs_from_registered_roots() {
        let world = World::new();
        world.build();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = |manifest: &Path, target: &Path| {
            vec![
                "--unverified".into(),
                "build".into(),
                format!("--manifest-path={}", manifest.display()),
                format!("--target-dir={}", target.display()),
            ]
        };
        let original = std::fs::read(world.path()).unwrap();
        let other = world.home.join("other-managed.noindex");
        assert!(create_checked(&world.home, &other, &world.owner).unwrap());
        let other_receipt = world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&other).unwrap()));
        std::fs::copy(other_receipt, world.path()).unwrap();
        assert!(
            observe_on(
                &[
                    "--unverified".into(),
                    "unknown-alias".into(),
                    format!("--target-dir={}", world.target.display())
                ],
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .is_err(),
            "another root's receipt cannot acknowledge the requested target"
        );
        std::fs::write(world.path(), original).unwrap();
        assert!(
            observe_on(
                &args(&world.owner.join("Cargo.toml"), &world.target),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .unwrap()
        );
        let legacy = world.home.join("legacy.noindex");
        std::fs::create_dir(&legacy).unwrap();
        assert!(
            !observe_on(
                &args(&world.owner.join("Cargo.toml"), &legacy),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .unwrap()
        );
        assert!(!legacy.join(MARKER).exists());

        // A target can be managed under one HOME, yet out of scope beneath a nested HOME's
        // protected roots. Exit zero for an unmanaged observation must not acknowledge it.
        let nested = world.home.join("nested-home");
        let protected = nested.join("Library/Containers/cache.noindex");
        // Private whatever the umask: a target is laid only through such folders.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(protected.parent().unwrap())
            .unwrap();
        assert!(create_checked(&world.home, &protected, &world.owner).unwrap());
        let project = nested.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let nested_machine = super::super::Machine {
            home: Some(&nested),
            cwd: Some(&project),
            ..machine
        };
        assert!(
            !observe_on(
                &args(&project.join("Cargo.toml"), &protected),
                super::super::Tool::Targo,
                &nested,
                &nested_machine,
            )
            .unwrap()
        );
        assert!(already_recorded(&world.home, &protected, &world.owner));
    }

    /// THE INDEX'S FOLDERS ARE LAID PRIVATE BEFORE A SHARED-CACHE BUILD CAN MAKE THEM: a build
    /// into the shared cache makes each folder of its path it does not find with its umask, and
    /// under umask 002 that left `Library/Caches/aterm` and its `.ubd` writable by the group, so
    /// no explicit target could be registered after it. The shim's call is observed before its
    /// build runs: for a plain build in a home whose `Library/Caches` (private, as macOS makes
    /// it) holds no `aterm`, the folders the index shares with the cache are made at 0o700, the
    /// folders the build then makes (here as umask 002 makes them) stand below them, and an
    /// explicit target is registered with its marker. NEGATIVE CONTROL: with the build's folders
    /// made as umask 002 makes them before any call is observed, no target is registered.
    #[test]
    fn the_index_folders_are_laid_private_before_a_shared_cache_build() {
        let caches = |home: &Path| {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(home.join("Library/Caches"))
                .unwrap();
        };
        let build_as_umask_002 = |home: &Path| {
            let mut at = home.to_path_buf();
            for component in Path::new(super::super::CACHE_UNDER_HOME).components() {
                at.push(component);
                if !at.exists() {
                    std::fs::create_dir(&at).unwrap();
                    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o775)).unwrap();
                }
            }
        };
        let control = World::new();
        caches(&control.home);
        build_as_umask_002(&control.home);
        assert!(!create_checked(&control.home, &control.target, &control.owner).unwrap());
        assert!(!control.target.join(MARKER).exists());

        let world = World::new();
        caches(&world.home);
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let plain = ["--unverified".to_owned(), "build".to_owned()];
        observe_on(&plain, super::super::Tool::Targo, &world.home, &machine).unwrap();
        for laid in ["Library/Caches/aterm", "Library/Caches/aterm/.ubd"] {
            let mode = std::fs::symlink_metadata(world.home.join(laid))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o7777, 0o700, "{laid}");
        }
        build_as_umask_002(&world.home);
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(world.target.join(MARKER).is_file());
    }

    /// A HOME A BUILD ALREADY LEFT GROUP-WRITABLE IS MADE PRIVATE BY THE NEXT OBSERVED CALL: a build
    /// before the fix above made `Library/Caches/aterm` and its `.ubd` at 0o775 (umask 002), so
    /// the index could never be laid there and no explicit target registered on any later call.
    /// The next observed shim call takes write from the group and others on each of those
    /// folders that is this user's own (through the folder held, never through a link), and
    /// leaves every other bit, and `Library/Caches` itself (the person's folder, not aterm's), as
    /// it stands: an explicit target is registered with its marker. A link standing at
    /// `Library/Caches/aterm` is never followed: the folder it names keeps its mode. NEGATIVE
    /// CONTROL: before the call is observed, the same home registers no target.
    #[test]
    fn a_home_a_build_left_group_writable_is_made_private_by_the_next_observed_call() {
        let lay = |caches: u32| {
            let world = World::new();
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(world.home.join("Library/Caches"))
                .unwrap();
            let mut at = world.home.clone();
            for component in Path::new(super::super::CACHE_UNDER_HOME).components() {
                at.push(component);
                if !at.exists() {
                    std::fs::create_dir(&at).unwrap();
                    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o775)).unwrap();
                }
            }
            let library_caches = world.home.join("Library/Caches");
            std::fs::set_permissions(&library_caches, std::fs::Permissions::from_mode(caches))
                .unwrap();
            world
        };
        let mode_of =
            |at: &Path| std::fs::symlink_metadata(at).unwrap().permissions().mode() & 0o7777;
        let observe = |world: &World| {
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &|_| None,
                read: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            let plain = ["--unverified".to_owned(), "build".to_owned()];
            observe_on(&plain, super::super::Tool::Targo, &world.home, &machine).unwrap();
        };

        let world = lay(0o700);
        assert!(!create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(!world.target.join(MARKER).exists());
        observe(&world);
        for laid in ["Library/Caches/aterm", "Library/Caches/aterm/.ubd"] {
            assert_eq!(mode_of(&world.home.join(laid)), 0o755, "{laid}");
        }
        assert_eq!(
            mode_of(
                &world
                    .home
                    .join("Library/Caches/aterm/.ubd/cargo-build.noindex")
            ),
            0o775
        );
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(world.target.join(MARKER).is_file());

        let shared = lay(0o770);
        observe(&shared);
        assert_eq!(mode_of(&shared.home.join("Library/Caches")), 0o770);
        assert_eq!(mode_of(&shared.home.join("Library/Caches/aterm")), 0o755);

        let linked = lay(0o700);
        let aterm = linked.home.join("Library/Caches/aterm");
        let elsewhere = linked.home.join("elsewhere");
        std::fs::rename(&aterm, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &aterm).unwrap();
        observe(&linked);
        assert_eq!(mode_of(&elsewhere), 0o775);
        assert_eq!(mode_of(&elsewhere.join(".ubd")), 0o775);
    }

    #[test]
    fn equivalent_inherited_placement_preserves_owners_through_nested_and_rust_calls() {
        // An ordinary target must be outside the test runner's own .noindex parent.
        let directory = aterm_tempfile::Builder::new()
            .prefix("cache-equivalent-")
            .tempdir_in("/tmp")
            .unwrap();
        let world = World::in_directory(directory);
        world.build();
        let manifest = world.owner.join("Cargo.toml");
        let manifest_text = std::fs::read_to_string(&manifest).unwrap();
        let read_manifest =
            |path: &Path| Ok((path == manifest.as_path()).then(|| manifest_text.clone()));
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &|_| None,
            read: &read_manifest,
            writable: &|_| Some(true),
        };
        let typed = vec!["--unverified".into(), "tippy".into()];
        let cache = super::super::config_value(&super::super::cache_root(&world.home)).unwrap();
        let directory = super::super::directory_from_config(&cache).unwrap();
        let environment =
            |name: &str| (name == super::super::BUILD_DIR_ENV).then(|| directory.into());
        let inherited = super::super::Machine {
            env: &environment,
            ..machine
        };
        let before = std::fs::read(world.path()).unwrap();
        for args in [
            typed.clone(),
            [vec!["--config".into(), cache.clone()], typed.clone()].concat(),
        ] {
            assert!(
                !observe_on(&args, super::super::Tool::Targo, &world.home, &inherited).unwrap()
            );
            assert_eq!(
                std::fs::read(world.path()).unwrap(),
                before,
                "equivalent default placement must not poison any existing receipt"
            );
        }
        let target = world.home.join("ordinary-target");
        // Private whatever the umask: a build folder is laid only through such folders.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&target)
            .unwrap();
        std::fs::write(target.join("keep"), b"unmanaged target output").unwrap();
        let child = target.join(super::super::TARGET_BUILD_LEAF);
        let typed = vec![
            "--unverified".into(),
            "build".into(),
            format!("--target-dir={}", target.display()),
        ];
        assert_eq!(
            super::super::explicit_cache(&typed, super::super::Tool::Targo, &machine)
                .expect("the fixture exposes its explicit target and owning manifest")
                .manifest,
            manifest
        );
        let value = format!("build.build-dir='{}'", child.display());
        let environment = |name: &str| {
            (name == super::super::BUILD_DIR_ENV).then(|| child.clone().into_os_string())
        };
        let inherited = super::super::Machine {
            env: &environment,
            ..machine
        };
        for args in [
            typed.clone(),
            [vec!["--config".into(), value], typed].concat(),
        ] {
            assert!(observe_on(&args, super::super::Tool::Targo, &world.home, &inherited).unwrap());
            assert_eq!(
                std::fs::read(world.path()).unwrap(),
                before,
                "a resolved target must not poison unrelated receipts"
            );
        }
        let receipt = world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&child).unwrap()));
        assert_eq!(
            read(&receipt).unwrap().owners.as_slice(),
            std::slice::from_ref(&world.owner)
        );
        assert!(!read(&receipt).unwrap().owners_unknown);
        assert!(
            !target.join(MARKER).exists(),
            "the unmarked parent was never adopted"
        );
        assert_eq!(
            std::fs::read(target.join("keep")).unwrap(),
            b"unmanaged target output"
        );
        let mut unknown = read(&receipt).unwrap();
        unknown.owners_unknown = true;
        write(&receipt, &unknown).unwrap();
        observe_on(
            &[
                "--unverified".into(),
                "build".into(),
                format!("--target-dir={}", target.display()),
            ],
            super::super::Tool::Targo,
            &world.home,
            &inherited,
        )
        .unwrap();
        assert!(
            read(&receipt).unwrap().owners_unknown,
            "equivalence never repairs old suppression"
        );
    }

    #[test]
    fn custom_or_overridden_inherited_placement_keeps_conservative_custody() {
        for choice in [
            "mismatch",
            "opaque",
            "override",
            "file",
            "unknown-template",
            "unmarked",
        ] {
            let directory = aterm_tempfile::Builder::new()
                .prefix("cache-explicit-")
                .tempdir_in("/tmp")
                .unwrap();
            let world = World::in_directory(directory);
            world.build();
            let target = world.home.join("ordinary-target");
            let child = target.join(super::super::TARGET_BUILD_LEAF);
            let cache = super::super::config_value(&super::super::cache_root(&world.home)).unwrap();
            let mut directory = super::super::directory_from_config(&cache)
                .unwrap()
                .to_owned();
            let mut args = vec!["--unverified".into(), "build".into()];
            match choice {
                "mismatch" => directory = "/some-other-directory".into(),
                "opaque" => args.extend(["--config".into(), "choice.toml".into()]),
                "override" => args.extend(["--config".into(), "build.build-dir='user'".into()]),
                "file" => {
                    std::fs::create_dir(world.owner.join(".cargo")).unwrap();
                    std::fs::write(
                        world.owner.join(".cargo/config.toml"),
                        "[build]\nbuild-dir='user'\n",
                    )
                    .unwrap();
                }
                "unknown-template" => directory = "{unknown-template}/build.noindex".into(),
                "unmarked" => {
                    std::fs::create_dir_all(&child).unwrap();
                    std::fs::write(child.join("keep"), b"existing unmarked cache").unwrap();
                    directory = child.display().to_string();
                    args.push(format!("--target-dir={}", target.display()));
                }
                _ => unreachable!(),
            }
            let environment = |name: &str| {
                (name == super::super::BUILD_DIR_ENV).then(|| directory.clone().into())
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &environment,
                read: &super::super::read_config,
                writable: &|_| Some(true),
            };
            observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
            assert_eq!(
                read(&world.path()).unwrap().owners_unknown,
                choice != "unmarked",
                "{choice}"
            );
            if choice == "unmarked" {
                assert!(!child.join(MARKER).exists());
                assert_eq!(
                    std::fs::read(child.join("keep")).unwrap(),
                    b"existing unmarked cache"
                );
            }
            assert_eq!(
                std::fs::read(world.target.join("debug/deps/keep")).unwrap(),
                b"reusable output"
            );
        }
    }

    #[test]
    fn a_registered_cache_without_cargo_locks_is_kept_until_they_exist() {
        let world = World::new();
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        let marker = std::fs::read(world.target.join(MARKER)).unwrap();
        let receipt = std::fs::read(world.path()).unwrap();
        let original = identity(&world.target).unwrap();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let reporting = super::super::fence::Fence::new(&clear).with_reporting();
        let now = world.later();
        let removed = reclaim_on(
            &world.home,
            now,
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| panic!("an unbuilt cache grants no removal intent"),
            &reporting,
        );
        assert!(removed.is_empty());
        assert_eq!(identity(&world.target).as_ref(), Some(&original));
        assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
        assert_eq!(std::fs::read(world.path()).unwrap(), receipt);
        assert_eq!(
            reporting.retained(),
            [super::super::RetainedCheck {
                path: world.target.clone(),
                reason: super::super::KeepReason::NoCargoLocks,
            }]
        );
        assert!(plan(&world.home, now).is_empty());

        // The same registration and gone owner are eligible once Cargo left its lock.
        std::fs::create_dir_all(world.target.join("debug/deps")).unwrap();
        std::fs::write(world.target.join("debug/deps/output"), b"reusable output").unwrap();
        std::fs::write(world.target.join("debug/.cargo-lock"), b"").unwrap();
        let mut asked = Vec::new();
        let removed = reclaim_on(
            &world.home,
            now,
            super::super::ReclaimAfter::CALM,
            None,
            &mut |path, why| {
                asked.push((path.to_path_buf(), why));
                true
            },
            &super::super::fence::Fence::new(&clear),
        );
        assert_eq!(asked.len(), 1);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].0, world.target);
        assert!(!world.target.exists());
    }

    #[test]
    fn every_shared_owner_and_live_cargo_lock_retains_current_dependencies() {
        let world = World::new();
        world.build();
        let other = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &other).unwrap());
        std::fs::remove_dir_all(&world.owner).unwrap();
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        assert!(world.target.join("debug/deps/keep").is_file());
        std::fs::remove_dir_all(&other).unwrap();
        let file = std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap();
        let held = crate::lock::Flock::try_lock(file).unwrap();
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        drop(held);
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        assert_eq!(removed.len(), 1);
        assert!(!world.target.exists());
    }

    /// A live worktree of the shared cache, its crate folder as old as the fixture's clock says.
    #[cfg(target_os = "macos")]
    fn live_in_shared_cache(world: &World) -> PathBuf {
        let live = world.home.join("live");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("Cargo.toml"), "[workspace]\n").unwrap();
        let root = super::super::cache_root(&world.home);
        let profile = super::super::build_dir(&root, &live).join("debug");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join(".cargo-build-lock"), b"").unwrap();
        let folder = profile.join("incremental/ws-1k116trr0a0xf");
        std::fs::create_dir_all(folder.join("s-hneouhc0uf-1tn8o9j-2awpw")).unwrap();
        folder
    }

    /// DEAD FIRST HOLDS ACROSS ROOTS: with room for two removals, a pass takes the root the cache
    /// moved from and a registered cache whose owners are gone, and the shared cache's live
    /// worktree keeps its warm folder. NEGATIVE CONTROL: the third ask is that folder's.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pass_takes_every_roots_dead_before_any_live_part() {
        let world = World::new();
        world.build();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let folder = live_in_shared_cache(&world);
        let moved = super::super::moved_from(&world.home).join("Users/gone");
        let moved_leaf = moved.join(super::super::LEAF);
        std::fs::create_dir_all(moved_leaf.join("debug/deps")).unwrap();

        let mut asked = Vec::new();
        super::super::reclaim_beside_sessions(
            &world.home,
            world.later(),
            true,
            None,
            &mut |_, why| {
                asked.push(why);
                asked.len() <= 2
            },
        );

        assert!(
            matches!(
                asked.as_slice(),
                [
                    super::super::Unused::Moved { .. },
                    super::super::Unused::Gone { .. },
                    super::super::Unused::Stale { .. },
                ]
            ),
            "{asked:?}"
        );
        assert!(!moved_leaf.exists() && !world.target.exists());
        assert!(folder.is_dir());
    }

    /// THE SHARED CACHE'S DECLINE SPEAKS FOR A REGISTERED CACHE'S PRUNE: a record that declines the
    /// first is not asked of the second, whose parts would only move aside and back. A record
    /// that takes them gets both, the shared cache's first.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_decline_of_a_shared_prune_spares_a_registered_cache_the_move() {
        let world = World::new();
        world.build();
        let unit = world.target.join("debug/incremental/old-unit");
        std::fs::create_dir_all(&unit).unwrap();
        let folder = live_in_shared_cache(&world);
        let pass = |record: &mut dyn FnMut(&Path, super::super::Unused) -> bool| {
            super::super::reclaim_beside_sessions(&world.home, world.later(), true, None, record)
        };

        let mut asked = Vec::new();
        pass(&mut |path, _| {
            asked.push(path.to_path_buf());
            false
        });
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(unit.is_dir() && folder.is_dir());

        let mut asked = Vec::new();
        pass(&mut |path, _| {
            asked.push(path.to_path_buf());
            true
        });
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(!asked[0].starts_with(&world.target), "{asked:?}");
        assert!(asked[1].starts_with(&world.target), "{asked:?}");
        assert!(!unit.exists() && !folder.exists());
        assert!(world.target.join("debug/deps/keep").is_file());
    }

    /// WHAT A PASS LEFT OF THE DEAD IT DOES NOT ASK ABOUT AGAIN: its prunes come after, and only
    /// prune. NEGATIVE CONTROL: the shared cache's prune went, so the prunes ran.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pass_asks_of_a_dead_cache_once() {
        let world = World::new();
        world.build();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let folder = live_in_shared_cache(&world);
        let mut gone = 0;
        super::super::reclaim_beside_sessions(
            &world.home,
            world.later(),
            true,
            None,
            &mut |_, why| {
                let dead = matches!(why, super::super::Unused::Gone { .. });
                gone += usize::from(dead);
                !dead
            },
        );
        assert_eq!(gone, 1);
        assert!(world.target.is_dir() && !folder.exists());
    }

    #[test]
    fn an_interrupted_stale_stage_is_planned_and_retried_under_its_receipt_gate() {
        use std::os::unix::fs::MetadataExt as _;
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                super::super::remove::seams::clear();
            }
        }
        let world = World::new();
        world.build();
        let incremental = world.target.join("debug/incremental/old-unit");
        std::fs::create_dir_all(&incremental).unwrap();
        let held = incremental.join("held");
        std::fs::write(&held, b"unfinished output").unwrap();
        let _clear = Clear;
        super::super::remove::seams::other_user(std::fs::symlink_metadata(&held).unwrap().ino());
        let now = SystemTime::now() + super::super::RECLAIM_STALE + Duration::from_secs(60);
        let went = reclaim(
            &world.home,
            now,
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        assert!(
            went.iter()
                .any(|(_, why)| matches!(why, super::super::Unused::Stayed { .. })),
            "{went:?}"
        );
        let stages =
            super::super::stages_in(&world.target, std::ffi::OsStr::new(super::super::DOOMED));
        assert_eq!(stages.len(), 1, "the real stale prune left one stage");
        let stage = &stages[0];
        assert!(!incremental.exists());
        assert!(world.target.join("debug/deps/keep").is_file());
        super::super::remove::seams::clear();
        assert!(
            plan(&world.home, now)
                .iter()
                .any(|p| p.path == *stage && p.why == super::super::Unused::Unfinished)
        );

        let file = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
        let gate = crate::lock::Flock::try_lock(file).unwrap();
        assert!(
            plan(&world.home, now).is_empty(),
            "a busy receipt gate admits no stage plan"
        );
        assert!(
            reclaim(
                &world.home,
                now,
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        drop(gate);
        assert!(
            reclaim(
                &world.home,
                now,
                super::super::ReclaimAfter::CALM,
                None,
                &mut |p, _| p != stage
            )
            .is_empty()
        );
        assert!(stage.is_dir(), "a refused intent keeps the stage");
        let retried = reclaim(
            &world.home,
            now,
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        assert_eq!(retried, [(stage.clone(), super::super::Unused::Unfinished)]);
        assert!(!stage.exists());
        assert!(
            world.target.join("debug/deps/keep").is_file(),
            "the live owner's current dependencies stay"
        );
        assert!(plan(&world.home, now).is_empty());
    }

    #[test]
    fn a_marked_cache_with_missing_or_corrupt_owner_records_refuses_another_producer() {
        let world = World::new();
        world.build();
        let other = world.another_owner();
        let before = std::fs::read(world.path()).unwrap();
        for record in [None, Some(b"{".as_slice())] {
            match record {
                None => std::fs::remove_file(world.path()).unwrap(),
                Some(bytes) => std::fs::write(world.path(), bytes).unwrap(),
            }
            assert!(create_checked(&world.home, &world.target, &other).is_err());
            assert!(world.target.join("debug/deps/keep").is_file());
            std::fs::write(world.path(), &before).unwrap();
        }
        let gate = crate::metadata_io::open_regular(&gate_path(&world.path()).unwrap()).unwrap();
        let _held = crate::lock::Flock::try_lock(gate).unwrap();
        assert!(create_checked(&world.home, &world.target, &other).is_err());
    }

    /// A PENDING STAGE IS FORECAST, AND WHAT STAYED OF IT NAMED, BY ITS OWN REMOVAL'S RULES: a
    /// pending stage holding, beside its payload, a folder its removal was never given (sorting
    /// before the payload) is planned with the forecast that the stage itself will stay for what
    /// it holds, its payload going; the pass then leaves just that, returned as what stayed, and
    /// its readers name the stage with that reason, never the folder's entry. With an entry of
    /// the payload read as another user's (the remover's seam), the forecast and the pass's
    /// readers name that entry under the stage instead, the stage never tried. NEGATIVE CONTROL:
    /// the plain forecast of the same stage says nothing stays, and a walk of what stands names
    /// the folder's entry first.
    #[test]
    fn a_pending_stage_is_forecast_and_named_by_its_own_removal() {
        use std::os::unix::fs::MetadataExt as _;
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                super::super::remove::seams::clear();
            }
        }
        use super::super::remove::{self as remove, Why};
        for kept in [false, true] {
            let world = World::new();
            world.build();
            let mut receipt = read(&world.path()).unwrap();
            let stage = world.home.join("interrupted-retirement.noindex");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&stage)
                .unwrap();
            receipt.pending = Some(Pending {
                directory: stage.clone(),
                identity: identity(&stage).unwrap(),
                authorized: false,
            });
            write(&world.path(), &receipt).unwrap();
            std::fs::rename(&world.target, stage.join(PAYLOAD)).unwrap();
            std::fs::create_dir_all(stage.join("a-foreign/sub")).unwrap();
            std::fs::write(stage.join("a-foreign/sub/y"), b"never given").unwrap();
            let _clear = Clear;
            let theirs = stage.join(PAYLOAD).join("debug/deps/keep");
            if kept {
                remove::seams::other_user(std::fs::symlink_metadata(&theirs).unwrap().ino());
            }
            let (rel, why) = if kept {
                (
                    PathBuf::from("cache.noindex/debug/deps/keep"),
                    Why::AnotherUser,
                )
            } else {
                (PathBuf::new(), Why::HoldsOthers)
            };

            let planned = plan(&world.home, world.later());
            let one = planned
                .iter()
                .find(|p| p.path == stage)
                .unwrap_or_else(|| panic!("{planned:?}"));
            let stays = one.stays.as_ref().unwrap();
            assert_eq!((stays.count, &stays.first), (1, &vec![(rel.clone(), why)]));
            let line = crate::cli::cache_plan_line(one);
            assert!(line.contains(&why.words(&stage.join(&rel))), "{line}");
            assert_eq!(
                super::super::Planned::stage(stage.clone())
                    .stays
                    .unwrap()
                    .count,
                usize::from(kept),
                "the plain forecast"
            );

            let removed = reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true,
            );
            let Some((_, super::super::Unused::Stayed { stayed, .. })) =
                removed.iter().find(|(p, _)| *p == stage)
            else {
                panic!("{removed:?}");
            };
            let named = remove::named(&stage, *stayed);
            let at = if kept { rel.clone() } else { stage.clone() };
            assert_eq!(named.first, [(at, why)]);
            let words = remove::stayed_words(&stage, *stayed);
            assert!(!words.contains("a-foreign"), "{words}");
            assert_eq!(
                std::fs::read(stage.join("a-foreign/sub/y")).unwrap(),
                b"never given"
            );
            assert_eq!(stage.join(PAYLOAD).exists(), kept);
            assert_eq!(
                remove::stayed(&stage, 1),
                [stage.join("a-foreign/sub/y")],
                "a walk of what stands names the folder's entry first"
            );
        }
    }

    #[test]
    fn pending_stage_survives_fresh_replacement_and_finishes_without_touching_new_outputs() {
        let world = World::new();
        world.build();
        let mut before = read(&world.path()).unwrap();
        let stage = world.home.join("interrupted-retirement.noindex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&stage)
            .unwrap();
        before.pending = Some(Pending {
            directory: stage.clone(),
            identity: identity(&stage).unwrap(),
            authorized: false,
        });
        write(&world.path(), &before).unwrap();
        std::fs::rename(&world.target, stage.join("cache.noindex")).unwrap();
        let owner = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &owner).unwrap());
        std::fs::write(world.target.join("new-output"), b"keep the new generation").unwrap();
        assert_eq!(
            roots(&world.home).len(),
            2,
            "the old stage has independent custody"
        );
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        assert_eq!(removed.len(), 1);
        assert!(!stage.exists());
        assert_eq!(
            std::fs::read(world.target.join("new-output")).unwrap(),
            b"keep the new generation"
        );
        assert_eq!(roots(&world.home).len(), 1);
    }

    /// A REGISTERED CACHE WHOSE RENAME IS REFUSED ONCE ITS INTENT IS ON RECORD CLOSES THAT
    /// INTENT: a gone owner's cache passes its checks and its intent is put on record; then its
    /// root is flagged `uchg` (its owner may set it), so its rename into the private stage fails.
    /// The pass returns it refused by the rename's error kind, nothing of it gone, its outputs
    /// whole; with the flag cleared, the next pass recovers the empty stage the refused move left
    /// and the one after it retires the cache. NEGATIVE CONTROL: without the outcome, the pass
    /// returns nothing for the intent it put on record.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_refused_rename_after_the_intent_closes_it_with_nothing_gone() {
        struct Unflag(PathBuf);
        impl Drop for Unflag {
            fn drop(&mut self) {
                let _ = crate::platform::set_file_flags(&self.0, 0);
            }
        }
        let world = World::new();
        world.build();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let unflag = Unflag(world.target.clone());
        let mut asked = Vec::new();
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |path, _| {
                asked.push(path.to_path_buf());
                crate::platform::set_file_flags(path, libc::UF_IMMUTABLE).unwrap();
                true
            },
        );
        assert_eq!(asked, std::slice::from_ref(&world.target));
        assert_eq!(
            removed,
            [(
                world.target.clone(),
                super::super::Unused::Refused {
                    by: super::super::Refusal::Rename(std::io::ErrorKind::PermissionDenied)
                }
            )]
        );
        assert!(world.target.join("debug/deps/keep").is_file());
        drop(unflag);
        let pass = || {
            reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true,
            )
        };
        assert!(
            pass().is_empty(),
            "the empty stage recovered, nothing removed"
        );
        assert!(world.target.join("debug/deps/keep").is_file());
        assert!(
            matches!(pass().as_slice(), [(path, super::super::Unused::Gone { .. })] if path == &world.target)
        );
        assert!(!world.target.exists());
    }

    /// A REGISTERED CACHE STOPPED ONCE STAGED CLOSES AS ITS PENDING STAGE: a gone owner's cache
    /// renamed into its private stage, then found in use there (the fence reports a process in a
    /// stage that holds the payload, from the moment an intent is on record), is returned as that
    /// pending stage with nothing of it gone ([`super::super::Refusal::Staged`]), never as its
    /// root, which no longer holds it; the stage holds it all. The next pass, still held, resumes
    /// that stage and is stopped the same way, `Staged`; the pass after, the process gone,
    /// finishes it ([`super::super::Unused::Unfinished`]). NEGATIVE CONTROL: the move not marked
    /// on the open intent, the pass returns the gone root as `Stopped`.
    #[test]
    fn a_cache_stopped_once_staged_closes_as_its_pending_stage() {
        let world = World::new();
        world.build();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let (held, asked) = (std::cell::Cell::new(true), std::cell::Cell::new(false));
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            let users = dirs
                .iter()
                .filter(|d| held.get() && asked.get() && d.join(PAYLOAD).is_dir())
                .map(|stage| aterm_uds::holders::PathUse {
                    pid: 36,
                    how: "cwd",
                    path: stage.join(PAYLOAD),
                })
                .collect();
            Ok(aterm_uds::holders::PathScan {
                users,
                ..aterm_uds::holders::PathScan::default()
            })
        };
        let fence = super::super::fence::Fence::new(&processes);
        let pass = || {
            asked.set(false);
            reclaim_on(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| {
                    asked.set(true);
                    true
                },
                &fence,
            )
        };
        let staged = super::super::Unused::Refused {
            by: super::super::Refusal::Staged,
        };
        let went = pass();
        let pending = read(&world.path())
            .and_then(|receipt| receipt.pending)
            .map(|pending| pending.directory)
            .expect("its pending stage");
        assert_eq!(
            went,
            [(pending.clone(), staged)],
            "its stage, nothing of it gone"
        );
        assert!(!world.target.exists(), "the root no longer holds it");
        assert!(
            pending.join(PAYLOAD).join("debug/deps/keep").is_file(),
            "the stage holds it all"
        );
        assert_eq!(
            pass(),
            [(pending.clone(), staged)],
            "resumed and stopped after its intent, the stage again"
        );
        held.set(false);
        assert_eq!(
            pass(),
            [(pending.clone(), super::super::Unused::Unfinished)],
            "the next pass finishes it"
        );
        assert!(!pending.exists() && !world.target.exists());
    }

    #[test]
    fn replacement_and_symlink_races_never_authorize_unrecorded_bytes() {
        let world = World::new();
        world.build();
        std::fs::remove_dir_all(&world.owner).unwrap();
        let old = world.home.join("old-output.noindex");
        let mut swapped = false;
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |path, _| {
                if path == world.target && !swapped {
                    std::fs::rename(&world.target, &old).unwrap();
                    std::fs::create_dir(&world.target).unwrap();
                    std::fs::write(world.target.join("unowned"), b"preserve").unwrap();
                    swapped = true;
                }
                true
            },
        );
        assert!(swapped);
        assert_eq!(
            removed,
            [(
                world.target.clone(),
                super::super::Unused::Refused {
                    by: super::super::Refusal::Stopped
                }
            )],
            "its intent closed, nothing of it gone"
        );
        assert_eq!(
            std::fs::read(world.target.join("unowned")).unwrap(),
            b"preserve"
        );
        assert!(old.join("debug/deps/keep").exists());
        let alias = world.home.join("alias.noindex");
        std::os::unix::fs::symlink(&world.target, &alias).unwrap();
        let owner = world.another_owner();
        assert!(!create_checked(&world.home, &alias, &owner).unwrap());
    }

    #[test]
    fn outside_home_is_not_an_explicit_output_directory() {
        let world = World::new();
        let outside = world.home.parent().unwrap().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&outside),
            env: &|_| None,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = ["--unverified".into(), "build".into()];
        assert!(super::super::unresolved_directory(&args, &machine));
        assert!(
            !observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true
            )
            .unwrap()
        );
        let scoped = super::super::Machine {
            cwd: Some(&world.owner),
            ..machine
        };
        for option in ["--config=build.build-dir='output'", "--config=other.toml"] {
            let args = ["--unverified".into(), "build".into(), option.into()];
            assert!(
                observe_for_producer(&args, super::super::Tool::Targo, &world.home, &scoped, true)
                    .is_err(),
                "an explicit unsupported output must still be refused: {option}"
            );
        }
        assert!(!outside.join("output").exists());
        assert!(!world.owner.join("output").exists());
    }

    #[test]
    fn explicit_manifest_and_target_precedence_bind_the_real_workspace() {
        let world = World::new();
        let launcher = world.home.join("launcher");
        std::fs::create_dir(&launcher).unwrap();
        let target = world.home.join("chosen.noindex");
        let env_target = world.home.join("not-chosen.noindex");
        let env =
            |name: &str| (name == "CARGO_TARGET_DIR").then(|| env_target.as_os_str().to_owned());
        let read = |path: &Path| match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&launcher),
            env: &env,
            read: &read,
            writable: &|_| Some(true),
        };
        let args = vec![
            "--unverified".into(),
            "test".into(),
            format!(
                "--manifest-path={}",
                world.owner.join("Cargo.toml").display()
            ),
            format!("--target-dir={}", target.display()),
        ];
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(!env_target.exists());
        let recorded = roots(&world.home);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].root, target);
        assert_eq!(recorded[0].owners, [world.owner]);
    }

    #[test]
    fn builtin_queries_leave_registered_receipts_unchanged_even_when_writes_are_refused() {
        struct RestorePermissions<'a>(&'a Path, std::fs::Permissions);
        impl Drop for RestorePermissions<'_> {
            fn drop(&mut self) {
                std::fs::set_permissions(self.0, self.1.clone()).unwrap();
            }
        }

        let world = World::new();
        world.build();
        let path = world.path();
        let before = std::fs::read(&path).unwrap();
        let marker = std::fs::read(world.target.join(MARKER)).unwrap();
        let fresh = world.home.join("query-only.noindex");
        let config = world.owner.join(".cargo/config.toml");
        let env =
            |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
        let read_config = |path: &Path| {
            Ok((path == config)
                .then(|| format!("[build]\ntarget-dir = '{}'\n", world.target.display())))
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &read_config,
            writable: &|_| Some(false),
        };
        let index = path.parent().unwrap();
        let permissions =
            RestorePermissions(index, std::fs::metadata(index).unwrap().permissions());
        std::fs::set_permissions(index, std::fs::Permissions::from_mode(0o500)).unwrap();
        // The held gate also makes a mistaken owner observation fail before writing, even
        // when a test runner has privileges that let it ignore the directory's mode.
        let gate = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
        let held = crate::lock::Flock::try_lock(gate).unwrap();
        for (tool, args) in [
            (super::super::Tool::Targo, vec!["metadata".into()]),
            (super::super::Tool::Targo, vec!["-vV".into()]),
            (super::super::Tool::Targo, vec!["-Vv".into()]),
            (super::super::Tool::Targo, vec!["-vvV".into()]),
            (
                super::super::Tool::Targo,
                vec!["--version".into(), "--verbose".into()],
            ),
            (
                super::super::Tool::Cargo,
                vec!["+nightly".into(), "-vV".into()],
            ),
            (
                super::super::Tool::Targo,
                vec![
                    "--unverified".into(),
                    "metadata".into(),
                    "--format-version=1".into(),
                    format!("--target-dir={}", fresh.display()),
                ],
            ),
            (
                super::super::Tool::Cargo,
                vec![
                    "+nightly".into(),
                    "--config".into(),
                    "net.offline=true".into(),
                    "-Z".into(),
                    "unstable-options".into(),
                    "metadata".into(),
                ],
            ),
            (
                super::super::Tool::Targo,
                vec!["locate-project".into(), "--workspace".into()],
            ),
            (super::super::Tool::Cargo, vec!["version".into()]),
            (
                super::super::Tool::Targo,
                vec!["help".into(), "build".into()],
            ),
        ] {
            assert!(
                !observe_on(&args, tool, &world.home, &machine).unwrap(),
                "{tool:?} {args:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
            assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
            assert!(!fresh.exists());
        }
        for (tool, words) in [
            (
                super::super::Tool::Targo,
                vec!["--unverified", "metadata-alias"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--config", "metadata", "build"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unknown-option", "metadata"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "run", "--", "metadata"],
            ),
            (super::super::Tool::Tippy, vec!["metadata"]),
            // A config file, or another directory's configs, may place the lockfile in a target.
            (
                super::super::Tool::Cargo,
                vec![
                    "+nightly",
                    "--config",
                    "elsewhere.toml",
                    "-Z",
                    "unstable-options",
                    "-C",
                    "another-directory",
                    "metadata",
                ],
            ),
            (super::super::Tool::Targo, vec!["-vV-not-a-query"]),
            (super::super::Tool::Targo, vec!["--color", "-vV", "build"]),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "an-unknown-alias", "-vV"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "run", "--", "-vV"],
            ),
            (super::super::Tool::Targo, vec!["tippy", "-vV"]),
            (super::super::Tool::Tippy, vec!["-vV"]),
            (
                super::super::Tool::Targo,
                vec!["--config", "clean", "--unverified", "build"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "clean-alias"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "run", "--", "clean"],
            ),
            (super::super::Tool::Tippy, vec!["clean"]),
        ] {
            let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(
                observe_on(&args, tool, &world.home, &machine).is_err(),
                "a possible producer must still observe its owners: {tool:?} {args:?}"
            );
        }
        drop(held);
        drop(permissions);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!read(&path).unwrap().owners_unknown);
    }

    /// THE VERBS THAT WRITE NO BUILD OUTPUT OWE NO OWNERSHIP, in every spelling of a target: each
    /// of [`READING_VERBS`] and [`RESOLVING_VERBS`], named literally here, with a registered
    /// target in the environment, a fresh one by `--target-dir` (both forms), a `-Z` option,
    /// `--config build.target-dir=…` (both forms) or `build.build-dir`, upstream cargo's
    /// `+toolchain`, is observed as nothing: its receipt and marker unchanged and no target made,
    /// under a gate held and an index that refuses writes, so a mistaken observation fails.
    /// NEGATIVE CONTROLS: the same verb as an option's value, past `--`, after a building verb,
    /// behind `trust`, as a lint's argument, or a name that only begins like it, is still
    /// observed; so is a resolving verb whose lockfile may land in a target, by an embedded
    /// manifest or a `resolver.lockfile-path` in each place a call can name one, while a reading
    /// verb with the same words stays free.
    #[test]
    fn output_free_verbs_owe_no_ownership_in_every_target_spelling() {
        struct RestorePermissions<'a>(&'a Path, std::fs::Permissions);
        impl Drop for RestorePermissions<'_> {
            fn drop(&mut self) {
                std::fs::set_permissions(self.0, self.1.clone()).unwrap();
            }
        }

        let world = World::new();
        world.build();
        let path = world.path();
        let before = std::fs::read(&path).unwrap();
        let marker = std::fs::read(world.target.join(MARKER)).unwrap();
        let fresh = world.home.join("fetch-only.noindex");
        let env =
            |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &|_| Ok(None),
            writable: &|_| Some(false),
        };
        let index = path.parent().unwrap();
        let permissions =
            RestorePermissions(index, std::fs::metadata(index).unwrap().permissions());
        std::fs::set_permissions(index, std::fs::Permissions::from_mode(0o500)).unwrap();
        let gate = crate::metadata_io::open_regular(&gate_path(&path).unwrap()).unwrap();
        let held = crate::lock::Flock::try_lock(gate).unwrap();
        let fresh_at = fresh.display().to_string();
        let config = format!("build.target-dir='{fresh_at}'");
        for verb in [
            "fetch",
            "tree",
            "metadata",
            "generate-lockfile",
            "pkgid",
            "verify-project",
            "read-manifest",
            "locate-project",
            "version",
        ] {
            let spellings: [(super::super::Tool, Vec<String>); 10] = [
                (
                    super::super::Tool::Targo,
                    vec![verb.into(), "--locked".into()],
                ),
                (
                    super::super::Tool::Targo,
                    vec![
                        "--unverified".into(),
                        verb.into(),
                        "--target-dir".into(),
                        fresh_at.clone(),
                    ],
                ),
                (
                    super::super::Tool::Targo,
                    vec![verb.into(), format!("--target-dir={fresh_at}")],
                ),
                (
                    super::super::Tool::Targo,
                    vec![
                        "-Z".into(),
                        "unstable-options".into(),
                        verb.into(),
                        "--target-dir".into(),
                        fresh_at.clone(),
                    ],
                ),
                (
                    super::super::Tool::Targo,
                    vec![
                        "-Zbuild-std".into(),
                        "--offline".into(),
                        verb.into(),
                        "-Zgc".into(),
                    ],
                ),
                (
                    super::super::Tool::Targo,
                    vec!["--config".into(), config.clone(), verb.into()],
                ),
                (
                    super::super::Tool::Targo,
                    vec![
                        format!("--config={config}"),
                        "--unverified".into(),
                        verb.into(),
                    ],
                ),
                (
                    super::super::Tool::Targo,
                    vec![
                        "--config".into(),
                        format!("build.build-dir='{fresh_at}'"),
                        verb.into(),
                        "--config".into(),
                        config.clone(),
                    ],
                ),
                (
                    super::super::Tool::Cargo,
                    vec![
                        "+nightly".into(),
                        verb.into(),
                        format!("--target-dir={fresh_at}"),
                    ],
                ),
                (
                    super::super::Tool::Cargo,
                    vec![
                        "-v".into(),
                        "--frozen".into(),
                        verb.into(),
                        "--target-dir".into(),
                        fresh_at.clone(),
                    ],
                ),
            ];
            for (tool, args) in spellings {
                assert!(
                    non_producing_command(&args, tool, &machine),
                    "{tool:?} {args:?}"
                );
                assert!(
                    !observe_for_producer(&args, tool, &world.home, &machine, true).unwrap(),
                    "{tool:?} {args:?}"
                );
                assert!(
                    !observe_on(&args, tool, &world.home, &machine).unwrap(),
                    "{args:?}"
                );
                assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
                assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
                assert!(!fresh.exists(), "{args:?}");
            }
        }
        for (tool, words) in [
            (
                super::super::Tool::Targo,
                vec!["--config", "fetch", "build"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "run", "--", "fetch"],
            ),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "build", "fetch"],
            ),
            (super::super::Tool::Targo, vec!["trust", "fetch"]),
            (
                super::super::Tool::Targo,
                vec!["--unverified", "fetch-alias"],
            ),
            (super::super::Tool::Targo, vec!["--unverified", "treehouse"]),
            (super::super::Tool::Tippy, vec!["fetch"]),
            (super::super::Tool::Tippy, vec!["tree"]),
        ] {
            let args = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(
                !non_producing_command(&args, tool, &machine),
                "{tool:?} {args:?}"
            );
            assert!(
                observe_on(&args, tool, &world.home, &machine).is_err(),
                "a possible producer must still observe its owners: {tool:?} {args:?}"
            );
        }
        let lockfile = fresh.join("Cargo.lock").display().to_string();
        let config_file = world.owner.join(".cargo").join("config.toml");
        let named_file = world.owner.join("net.retry=2");
        let benign_file = world.owner.join("ci.toml");
        let words = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
        // Words before the verb, words after it, `CARGO_RESOLVER_LOCKFILE_PATH`, and what the
        // workspace's own config reads as.
        type Case = (
            Vec<String>,
            Vec<String>,
            Option<String>,
            Result<Option<String>, ()>,
        );
        let cases: Vec<Case> = vec![
            (
                vec![],
                words(&["--manifest-path", "script.rs"]),
                None,
                Ok(None),
            ),
            (
                vec![],
                words(&["--manifest-path=tools/helper"]),
                None,
                Ok(None),
            ),
            (
                words(&["--manifest-path", "/elsewhere/build.rs"]),
                vec![],
                None,
                Ok(None),
            ),
            (
                vec![],
                vec![
                    "--config".into(),
                    format!("resolver.lockfile-path='{lockfile}'"),
                ],
                None,
                Ok(None),
            ),
            (
                vec![format!(
                    "--config=resolver.\"\\u006cockfile-path\"='{lockfile}'"
                )],
                vec![],
                None,
                Ok(None),
            ),
            (
                vec![],
                words(&["--config", "include='more.toml'"]),
                None,
                Ok(None),
            ),
            (vec![], words(&["--config", "net.retry=2"]), None, Ok(None)),
            (vec![], words(&["--config"]), None, Ok(None)),
            (
                words(&["-Z", "unstable-options", "-C", "elsewhere"]),
                vec![],
                None,
                Ok(None),
            ),
            (words(&["-Celsewhere"]), vec![], None, Ok(None)),
            (
                vec![],
                words(&["--lockfile-path", "x/Cargo.lock"]),
                None,
                Ok(None),
            ),
            (vec![], vec![], Some(lockfile.clone()), Ok(None)),
            (
                vec![],
                vec![],
                None,
                Ok(Some(format!("[resolver]\nlockfile-path = '{lockfile}'\n"))),
            ),
            (
                vec![],
                vec![],
                None,
                Ok(Some("include = ['more.toml']\n".into())),
            ),
            (
                vec![],
                vec![],
                None,
                Ok(Some("[resolver]\n\"\\u006cockfile-path\" = 'x'\n".into())),
            ),
            (vec![], vec![], None, Err(())),
            (vec![], words(&["-m", "script.rs"]), None, Ok(None)),
            (vec![], words(&["-mtools/helper"]), None, Ok(None)),
            (vec![], words(&["-m=helper.rs"]), None, Ok(None)),
            (vec![], words(&["-qm", "sub/Cargo.toml"]), None, Ok(None)),
            (
                vec![],
                vec![],
                None,
                Ok(Some("[resolver\nlockfile-path = 'x'\n".into())),
            ),
        ];
        // Calls whose words or configs only look like those: an ordinary manifest by either
        // spelling and either name, a long flag or a `-Z` option holding an `m`, another resolver
        // key, a config file that sets neither, a value naming nothing that stands and no lock
        // root, a backslash in a value (as aterm's own `.cargo/config.toml` has one), a key named
        // only in a comment, and TOML 1.1 that names neither.
        let free: Vec<Case> = vec![
            (vec![], words(&["-m", "sub/Cargo.toml"]), None, Ok(None)),
            (vec![], words(&["-m", "sub/Targo.toml"]), None, Ok(None)),
            (vec![], words(&["--manifest-path", "sub/other.toml"]), None, Ok(None)),
            (words(&["-Zminimal-versions"]), vec![], None, Ok(None)),
            (words(&["--config", "ci.toml"]), vec![], None, Ok(None)),
            (words(&["--config", "extra.toml"]), vec![], None, Ok(None)),
            (vec![], vec![], None, Ok(Some("net = { retry = 2, }\n".into()))),
            (
                vec![],
                words(&["--manifest-path=sub/Cargo.toml", "--format-version=1"]),
                None,
                Ok(None),
            ),
            (
                vec![],
                words(&["--config", "resolver.feature-unification=\"workspace\""]),
                None,
                Ok(None),
            ),
            (
                vec![],
                vec![],
                None,
                Ok(Some(
                    "[alias]\nverify = \"run --config resolver.feature-unification=\\\"workspace\\\" -p aterm-verify --\"\n"
                        .into(),
                )),
            ),
            (
                vec![],
                vec![],
                None,
                Ok(Some(
                    "[resolver]\nfeature-unification = \"workspace\" # lockfile-path = 'x'\n".into(),
                )),
            ),
        ];
        let cases = cases
            .into_iter()
            .map(|case| (case, true))
            .chain(free.into_iter().map(|case| (case, false)));
        for ((before_verb, after_verb, lock_env, config), placed) in cases {
            let env = |name: &str| match name {
                "CARGO_TARGET_DIR" => Some(world.target.as_os_str().to_owned()),
                "CARGO_RESOLVER_LOCKFILE_PATH" => lock_env.clone().map(Into::into),
                _ => None,
            };
            let read = |path: &Path| -> Result<Option<String>, ()> {
                if path == config_file {
                    config.clone()
                } else if path == named_file {
                    Ok(Some(format!("[resolver]\nlockfile-path = '{lockfile}'\n")))
                } else {
                    Ok((path == benign_file).then(|| "[net]\nretry = 2\n".into()))
                }
            };
            let placing = super::super::Machine {
                env: &env,
                read: &read,
                ..machine
            };
            for verb in READING_VERBS.iter().chain(RESOLVING_VERBS) {
                let mut args = before_verb.clone();
                args.push((*verb).to_owned());
                args.extend(after_verb.iter().cloned());
                let observed_as_producer = placed && RESOLVING_VERBS.contains(verb);
                assert_eq!(
                    non_producing_command(&args, super::super::Tool::Targo, &placing),
                    !observed_as_producer,
                    "{args:?} {lock_env:?} {config:?}"
                );
                let observed = observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &placing,
                    true,
                );
                assert_eq!(
                    observed.is_err(),
                    observed_as_producer,
                    "{args:?} {observed:?}"
                );
                assert!(observed.is_err() || !observed.unwrap(), "{args:?}");
                assert_eq!(std::fs::read(&path).unwrap(), before, "{args:?}");
                assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
                assert!(!fresh.exists(), "{args:?}");
            }
        }
        // The package home's config counts too, the home chosen as Targo chooses it: `CARGO_HOME`
        // (against the working directory), its `TARGO_HOME` twin where `CARGO_HOME` is unset, or
        // `~/.cargo` where the chosen one is empty or none is set.
        let package_home = world.home.join("package-home");
        let package_home_at = package_home.as_os_str().to_owned();
        // Read from outside the home, so no ancestor's config stands in for the package home's.
        let elsewhere = world.home.parent().unwrap().join("elsewhere");
        let dot_cargo = world.home.join(".cargo").join("config");
        for (cargo_home, targo_home, config) in [
            (
                Some(package_home_at.clone()),
                None,
                package_home.join("config.toml"),
            ),
            (
                Some("../home/package-home".into()),
                None,
                package_home.join("config.toml"),
            ),
            (
                None,
                Some(package_home_at.clone()),
                package_home.join("config"),
            ),
            (
                Some("".into()),
                Some(package_home_at.clone()),
                dot_cargo.clone(),
            ),
            (None, None, dot_cargo.clone()),
        ] {
            let env = |name: &str| match name {
                "CARGO_HOME" => cargo_home.clone(),
                "TARGO_HOME" => targo_home.clone(),
                _ => None,
            };
            // Paths compare as Targo would read them: the relative home joined to the cwd.
            let read = |path: &Path| {
                let path = normalize_with_cwd(path.to_path_buf(), None).unwrap();
                Ok((path == config).then(|| "[resolver]\nlockfile-path = 'x/Cargo.lock'\n".into()))
            };
            let homed = super::super::Machine {
                cwd: Some(&elsewhere),
                env: &env,
                read: &read,
                ..machine
            };
            assert!(
                lockfile_may_land_in_a_target(&words(&["fetch"]), &homed),
                "{cargo_home:?} {targo_home:?}"
            );
            let read = |_: &Path| Ok(None);
            let plain = super::super::Machine {
                read: &read,
                ..homed
            };
            assert!(!lockfile_may_land_in_a_target(&words(&["fetch"]), &plain));
        }
        // An explicit `CARGO_HOME` hides its `TARGO_HOME` twin, as in Targo.
        let env = |name: &str| match name {
            "CARGO_HOME" => Some(world.home.join("chosen").into_os_string()),
            "TARGO_HOME" => Some(package_home_at.clone()),
            _ => None,
        };
        let read = |path: &Path| {
            Ok(path
                .starts_with(&package_home)
                .then(|| "include = ['x.toml']\n".into()))
        };
        let twinned = super::super::Machine {
            cwd: Some(&elsewhere),
            env: &env,
            read: &read,
            ..machine
        };
        assert!(!lockfile_may_land_in_a_target(&words(&["fetch"]), &twinned));
        // A lockfile path in the `TARGO_` twin of its variable, and a call with no working
        // directory to read configs from, may place the lockfile too.
        let env =
            |name: &str| (name == "TARGO_RESOLVER_LOCKFILE_PATH").then(|| "x/Cargo.lock".into());
        let read = |_: &Path| Ok(None);
        let twin = super::super::Machine {
            env: &env,
            read: &read,
            ..machine
        };
        assert!(lockfile_may_land_in_a_target(&words(&["fetch"]), &twin));
        let env = |_: &str| None;
        let nowhere = super::super::Machine {
            cwd: None,
            env: &env,
            read: &read,
            ..machine
        };
        assert!(lockfile_may_land_in_a_target(&words(&["fetch"]), &nowhere));
        // A config that is a link (`.cargo/config -> config.toml`), which atpkg's own reader
        // refuses, is read at its canonical path, as Targo reads it; a dangling one cannot be
        // read at all.
        let linked = world.home.join("linked-config");
        std::fs::create_dir_all(linked.join(".cargo")).unwrap();
        std::os::unix::fs::symlink("config.toml", linked.join(".cargo").join("config")).unwrap();
        let no_follow = |path: &Path| match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => Err(()),
            Ok(_) => Ok(Some(std::fs::read_to_string(path).unwrap())),
            Err(_) => Ok(None),
        };
        let env = |_: &str| None;
        let through = super::super::Machine {
            cwd: Some(&linked),
            env: &env,
            read: &no_follow,
            ..machine
        };
        for (text, placed) in [
            ("[resolver]\nfeature-unification = 'workspace'\n", false),
            ("[resolver]\nlockfile-path = 'x/Cargo.lock'\n", true),
        ] {
            std::fs::write(linked.join(".cargo").join("config.toml"), text).unwrap();
            assert_eq!(
                lockfile_may_land_in_a_target(&words(&["fetch"]), &through),
                placed,
                "{text}"
            );
        }
        std::fs::remove_file(linked.join(".cargo").join("config.toml")).unwrap();
        assert!(lockfile_may_land_in_a_target(&words(&["fetch"]), &through));
        drop(held);
        drop(permissions);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// The `*.json` receipts of `home`'s index, by name, with their bytes.
    fn receipts(home: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
        let mut found = std::fs::read_dir(home.join(INDEX))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| {
                (
                    path.file_name().unwrap().to_owned(),
                    std::fs::read(&path).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        found.sort();
        found
    }

    /// The producer call of `owner`'s workspace building into `target`.
    fn build_into(owner: &Path, target: &Path) -> Vec<String> {
        vec![
            "--unverified".into(),
            "build".into(),
            format!("--manifest-path={}", owner.join("Cargo.toml").display()),
            format!("--target-dir={}", target.display()),
        ]
    }

    /// A TARGET NAMED THROUGH A LINK TO A FOLDER ATPKG OWNS IS THAT FOLDER: a build of another
    /// workspace through a link (its text relative, as Macs set up before 2026-09-10 have
    /// `crates/target -> target.noindex`, or absolute) is observed as owned, its workspace joins
    /// the folder's own receipt, and nothing else changes: the marker, the link, the count of
    /// registered roots, no receipt keyed by the link and none for a build leaf under it. A second
    /// build reads its owner on record and writes nothing. An unresolved producer through the link
    /// protects the folder's receipt as its own name would. Each fails without link resolution:
    /// the link's name has no receipt of its own while the folder's marker shows through it.
    #[test]
    fn a_target_named_through_a_link_to_an_owned_folder_is_that_folder() {
        for text in [PathBuf::from("outputs.noindex"), PathBuf::new()] {
            let world = World::new();
            world.build();
            let text = if text.as_os_str().is_empty() {
                world.target.clone()
            } else {
                text
            };
            let link = world.home.join("target");
            std::os::unix::fs::symlink(&text, &link).unwrap();
            let marker = std::fs::read(world.target.join(MARKER)).unwrap();
            let other = world.another_owner();
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&other),
                env: &|_| None,
                read: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            let args = build_into(&other, &link);
            assert_eq!(
                observe(&args, super::super::Tool::Targo, &world.home, &machine).unwrap(),
                Observation::Owned,
                "{text:?}"
            );
            let receipt = read(&world.path()).unwrap();
            assert_eq!(receipt.root, world.target);
            assert_eq!(receipt.owners, [world.owner.clone(), other.clone()]);
            assert!(!receipt.owners_unknown);
            assert_eq!(std::fs::read(world.target.join(MARKER)).unwrap(), marker);
            assert_eq!(std::fs::read_link(&link).unwrap(), text);
            assert!(!link.join(super::super::TARGET_BUILD_LEAF).exists());
            let registered = roots(&world.home);
            assert_eq!(registered.len(), 1);
            assert_eq!(registered[0].root, world.target);
            let keyed_by_link = world
                .home
                .join(INDEX)
                .join(format!("{}.json", key(&link).unwrap()));
            assert!(std::fs::symlink_metadata(keyed_by_link).is_err());

            let recorded = receipts(&world.home);
            assert!(
                observe_for_producer(
                    &args,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine,
                    true
                )
                .unwrap()
            );
            assert_eq!(
                receipts(&world.home),
                recorded,
                "an owner on record writes nothing"
            );

            let unresolved = [
                "--unverified".into(),
                "unknown-alias".into(),
                format!("--target-dir={}", link.display()),
            ];
            assert_eq!(
                observe(
                    &unresolved,
                    super::super::Tool::Targo,
                    &world.home,
                    &machine
                )
                .unwrap(),
                Observation::Protected
            );
            assert!(read(&world.path()).unwrap().owners_unknown);
            assert_eq!(roots(&world.home).len(), 1);
        }
    }

    /// A LINK TO A FOLDER ATPKG DOES NOT OWN IS REFUSED, EACH CASE IN ITS OWN WORDS, and nothing is
    /// written: no marker in the folder, no receipt in the index, the link as it was. A folder
    /// with no custody; one cargo made that atpkg never registered (never adopted through the
    /// link, though at its own name it would be); a link to nothing; a link to a link to an owned
    /// folder, or to an owned folder through a linked folder on the way, or in a folder reached
    /// through a link; text that climbs past the root; a folder on another volume (`/dev`); a folder whose marker no receipt binds; a
    /// file. NEGATIVE CONTROL: the cargo-made folder named by its own path is adopted.
    #[test]
    fn links_to_folders_atpkg_does_not_own_refuse_each_in_its_own_words() {
        use std::os::unix::fs::MetadataExt as _;
        let world = World::new();
        world.build();
        let other = world.another_owner();
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&other),
            env: &|_| None,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let folder = |name: &str| {
            let at = world.home.join(name);
            std::fs::DirBuilder::new().mode(0o700).create(&at).unwrap();
            at
        };
        let plain = folder("plain.noindex");
        let cargo_made = folder("cargo-made.noindex");
        let tag = cargo_made.join("CACHEDIR.TAG");
        std::fs::write(
            &tag,
            "Signature: 8a477f597d28d172789f06886806bc55\n\
             # This file is a cache directory tag created by cargo.\n\
             # For information about cache directory tags see https://bford.info/cachedir/\n",
        )
        .unwrap();
        std::fs::set_permissions(&tag, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(cargo_made.join(".rustc_info.json"), b"{}").unwrap();
        let foreign = folder("foreign.noindex");
        std::fs::copy(world.target.join(MARKER), foreign.join(MARKER)).unwrap();
        let file = world.home.join("a-file");
        std::fs::write(&file, b"not a folder").unwrap();
        let hop = world.home.join("hop");
        std::os::unix::fs::symlink("outputs.noindex", &hop).unwrap();
        std::os::unix::fs::symlink(".", world.home.join("via")).unwrap();
        assert_ne!(
            std::fs::metadata("/dev").unwrap().dev(),
            std::fs::metadata(&world.home).unwrap().dev(),
            "the other volume must be one"
        );
        let index = receipts(&world.home);
        for (name, text, refusal) in [
            (
                "unowned",
                PathBuf::from("plain.noindex"),
                LinkRefusal::Unowned,
            ),
            (
                "cargo",
                PathBuf::from("cargo-made.noindex"),
                LinkRefusal::CargoMade,
            ),
            (
                "dangling",
                PathBuf::from("nothing.noindex"),
                LinkRefusal::Dangling,
            ),
            ("chain", PathBuf::from("hop"), LinkRefusal::Chain),
            (
                "detour",
                PathBuf::from("via/outputs.noindex"),
                LinkRefusal::Chain,
            ),
            ("volume", PathBuf::from("/dev"), LinkRefusal::OtherVolume),
            (
                "foreign",
                PathBuf::from("foreign.noindex"),
                LinkRefusal::Foreign,
            ),
            ("file", PathBuf::from("a-file"), LinkRefusal::NotFolder),
            // Named as `via/link-parent`: a link to an owned folder, in a folder reached
            // through a link.
            (
                "parent",
                PathBuf::from("outputs.noindex"),
                LinkRefusal::Chain,
            ),
            (
                "above-root",
                PathBuf::from(format!("{}dev", "../".repeat(64))),
                LinkRefusal::Unreachable,
            ),
        ] {
            let link = world.home.join(format!("link-{name}"));
            std::os::unix::fs::symlink(&text, &link).unwrap();
            let named = if name == "parent" {
                world.home.join("via").join(format!("link-{name}"))
            } else {
                link.clone()
            };
            let args = build_into(&other, &named);
            assert_eq!(
                observe(&args, super::super::Tool::Targo, &world.home, &machine).unwrap(),
                Observation::Refused(refusal),
                "{name}"
            );
            let refused = observe_for_producer(
                &args,
                super::super::Tool::Targo,
                &world.home,
                &machine,
                true,
            )
            .unwrap_err();
            assert_eq!(refused.to_string(), refusal.why(), "{name}");
            assert_eq!(receipts(&world.home), index, "{name}");
            assert_eq!(std::fs::read_link(&link).unwrap(), text, "{name}");
        }
        let messages = [
            LinkRefusal::Unowned,
            LinkRefusal::CargoMade,
            LinkRefusal::Dangling,
            LinkRefusal::Chain,
            LinkRefusal::OtherVolume,
            LinkRefusal::Foreign,
            LinkRefusal::Renumbered,
            LinkRefusal::NotFolder,
            LinkRefusal::Unreachable,
            LinkRefusal::Changed,
        ]
        .map(LinkRefusal::why);
        assert_eq!(
            messages
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            messages.len(),
            "each refusal has its own words"
        );
        for untouched in [&plain, &cargo_made] {
            assert!(!untouched.join(MARKER).exists());
        }
        assert!(!world.home.join("nothing.noindex").exists());

        assert!(create_checked(&world.home, &cargo_made, &other).unwrap());
        assert!(cargo_made.join(MARKER).is_file());
    }

    /// A LINK RETARGETED BETWEEN ITS OBSERVATION AND THE RE-CHECK UNDER THE FOLDER'S GATE IS
    /// REFUSED: a link to one owned folder, moved to another owned folder at the first re-check
    /// (the shared gate's, for an owner already on record) or the second (the exclusive gate's,
    /// for a new one), or with the link left as it was and another folder renamed into its
    /// folder's path, is refused as changed, and neither folder's receipt gains the producer. An
    /// unresolved call through the link, moved at its re-check under the folder's gate, is refused
    /// as changed too, and the first folder's receipt is left as it was. NEGATIVE CONTROL: with no
    /// move, the same calls are owned, and the unresolved one suppresses the folder's retirement.
    #[test]
    fn a_link_retargeted_between_observation_and_the_gate_is_refused() {
        for (moved_at, recorded, swapped) in [
            (Some(1), true, false),
            (Some(1), false, false),
            (Some(2), false, false),
            (Some(1), true, true),
            (Some(2), false, true),
            (None, false, false),
        ] {
            let world = World::new();
            world.build();
            let second = world.home.join("second.noindex");
            assert!(create_checked(&world.home, &second, &world.owner).unwrap());
            let producer = if recorded {
                world.owner.clone()
            } else {
                world.another_owner()
            };
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&producer),
                env: &|_| None,
                read: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            let link = world.home.join("target");
            std::os::unix::fs::symlink("outputs.noindex", &link).unwrap();
            let index = receipts(&world.home);
            let rechecks = std::rc::Rc::new(std::cell::Cell::new(0));
            let seen = rechecks.clone();
            let (first, aside) = (world.target.clone(), world.home.join("aside.noindex"));
            let swap = second.clone();
            link_seam::on_recheck(move |link| {
                seen.set(seen.get() + 1);
                if Some(seen.get()) != moved_at {
                } else if swapped {
                    // The link as it was, its folder's path now another folder's.
                    std::fs::rename(&first, &aside).unwrap();
                    std::fs::rename(&swap, &first).unwrap();
                } else {
                    std::fs::remove_file(link).unwrap();
                    std::os::unix::fs::symlink("second.noindex", link).unwrap();
                }
            });
            let observed = observe(
                &build_into(&producer, &link),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            );
            link_seam::clear();
            let label =
                format!("moved at {moved_at:?}, owner on record {recorded}, swapped {swapped}");
            if moved_at.is_some() {
                assert_eq!(
                    observed.unwrap(),
                    Observation::Refused(LinkRefusal::Changed),
                    "{label}"
                );
                assert_eq!(receipts(&world.home), index, "{label}");
            } else {
                assert_eq!(observed.unwrap(), Observation::Owned, "{label}");
                assert!(rechecks.get() >= 1, "{label}");
                assert!(read(&world.path()).unwrap().owners.contains(&producer));
            }
        }
        for moved in [true, false] {
            let world = World::new();
            world.build();
            let second = world.home.join("second.noindex");
            assert!(create_checked(&world.home, &second, &world.owner).unwrap());
            let link = world.home.join("target");
            std::os::unix::fs::symlink("outputs.noindex", &link).unwrap();
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&world.owner),
                env: &|_| None,
                read: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            let index = receipts(&world.home);
            if moved {
                link_seam::on_recheck(|link| {
                    std::fs::remove_file(link).unwrap();
                    std::os::unix::fs::symlink("second.noindex", link).unwrap();
                });
            }
            let unresolved = [
                "--unverified".into(),
                "unknown-alias".into(),
                format!("--target-dir={}", link.display()),
            ];
            let observed = observe(
                &unresolved,
                super::super::Tool::Targo,
                &world.home,
                &machine,
            );
            link_seam::clear();
            if moved {
                assert_eq!(
                    observed.unwrap_err().to_string(),
                    LinkRefusal::Changed.why()
                );
                assert_eq!(receipts(&world.home), index);
            } else {
                assert_eq!(observed.unwrap(), Observation::Protected);
                assert!(read(&world.path()).unwrap().owners_unknown);
            }
        }
        // Shapes only one part of the re-check sees: the link replaced by a new link of the same
        // text (its own inode alone differs), and the link's folder moved deeper with a link left
        // in its place (the same link, text and folder, the kernel resolving it elsewhere).
        for shape in ["relinked", "parent moved"] {
            let world = World::new();
            world.build();
            let producer = world.another_owner();
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&producer),
                env: &|_| None,
                read: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            let links = world.home.join("links");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&links)
                .unwrap();
            let link = links.join("target");
            std::os::unix::fs::symlink("../outputs.noindex", &link).unwrap();
            let index = receipts(&world.home);
            let home = world.home.clone();
            let mut moved = false;
            link_seam::on_recheck(move |link| {
                if std::mem::replace(&mut moved, true) {
                    return;
                }
                if shape == "relinked" {
                    // A new link renamed over the old, so the replacement is a node of its own:
                    // one made after the old is removed can take the freed inode number back
                    // (Linux's ext4 reuses it at once), and no stat tells that from the original.
                    let fresh = link.with_extension("fresh");
                    std::os::unix::fs::symlink("../outputs.noindex", &fresh).unwrap();
                    std::fs::rename(&fresh, link).unwrap();
                } else {
                    let deeper = home.join("deeper");
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&deeper)
                        .unwrap();
                    std::fs::rename(home.join("links"), deeper.join("links")).unwrap();
                    std::os::unix::fs::symlink("deeper/links", home.join("links")).unwrap();
                }
            });
            let observed = observe(
                &build_into(&producer, &link),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            );
            link_seam::clear();
            assert_eq!(
                observed.unwrap(),
                Observation::Refused(LinkRefusal::Changed),
                "{shape}"
            );
            assert_eq!(receipts(&world.home), index, "{shape}");
        }
    }

    #[cfg(target_os = "macos")]
    const CLEAN_FRONT_HOME: &str = "ATPKG_TEST_CLEAN_FRONT_HOME";
    #[cfg(target_os = "macos")]
    const CLEAN_FRONT_TOOL: &str = "ATPKG_TEST_CLEAN_FRONT_TOOL";
    #[cfg(target_os = "macos")]
    const CLEAN_FRONT_WORDS: &str = "ATPKG_TEST_CLEAN_FRONT_WORDS";

    /// This original test image enters the real shim front, which execs the requested
    /// underlying tool. A refusal returns before that tool can print or remove anything.
    #[cfg(target_os = "macos")]
    #[test]
    fn clean_front_child_if_asked() {
        if std::env::var_os(CLEAN_FRONT_HOME).is_none() {
            return;
        }
        let words: Vec<String> =
            aterm_json::from_str(&std::env::var(CLEAN_FRONT_WORDS).unwrap()).unwrap();
        let mut args: Vec<std::ffi::OsString> = vec![
            "--shim".into(),
            "targo".into(),
            "-".into(),
            std::env::var_os(CLEAN_FRONT_TOOL).unwrap(),
        ];
        args.extend(words.into_iter().map(std::ffi::OsString::from));
        let code = crate::build_admit::run_hidden(&args);
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            75
        });
    }

    #[cfg(target_os = "macos")]
    fn through_clean_front(world: &World, tool: &Path, words: &[String]) -> std::process::Output {
        through_clean_front_in(world, &world.owner, tool, words, None)
    }

    /// [`through_clean_front`] run in `cwd`, with `CARGO_TARGET_DIR` set to `target_env` when one
    /// is given.
    #[cfg(target_os = "macos")]
    fn through_clean_front_in(
        world: &World,
        cwd: &Path,
        tool: &Path,
        words: &[String],
        target_env: Option<&Path>,
    ) -> std::process::Output {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        // Terse progress leaves the exec'd tool's first output line unprefixed.
        command
            .args([
                "--exact",
                "build_cache::registered::tests::clean_front_child_if_asked",
                "--quiet",
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &world.home)
            .env(CLEAN_FRONT_HOME, &world.home)
            .env(CLEAN_FRONT_TOOL, tool)
            .env(CLEAN_FRONT_WORDS, aterm_json::to_string(words).unwrap())
            .env(
                crate::build_admit::LEDGER_ENV,
                world.home.join("ledger.noindex"),
            );
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        if let Some(target) = target_env {
            command.env("CARGO_TARGET_DIR", target);
        }
        aterm_uds::deadline::ends_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
    }

    /// `verify provision`'s fish build through the real shim front, its words as the shim hands
    /// them on (`-`: its block leaves a `*.noindex` target unplaced): the source and its
    /// `--target-dir`, named again in `CARGO_TARGET_DIR`, in one scratch under
    /// `~/Library/Caches`, run from that scratch. HOME's `Library` is a folder atpkg never manages
    /// a workspace in, as it manages none outside HOME, so the front does not register its cache.
    /// An explicit one-job request keeps the resource bound independent of the host's load.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_front_hands_on_a_build_of_a_workspace_atpkg_never_manages() {
        let world = World::new();
        let scratch = world
            .home
            .join("Library/Caches/aterm/tooling.noindex/.provision-fish-1_0_0");
        let src = scratch.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("Cargo.toml"),
            "[package]\nname = 'fish'\nversion = '4.9.3'\n",
        )
        .unwrap();
        let target = scratch.join("target.noindex");
        let manifest = src.join("Cargo.toml").display().to_string();
        let target_dir = target.display().to_string();
        let words: Vec<String> = [
            "--unverified",
            "build",
            "--release",
            "--locked",
            "--no-default-features",
            "--jobs=1",
            "--manifest-path",
            manifest.as_str(),
            "--target-dir",
            target_dir.as_str(),
        ]
        .map(String::from)
        .into();
        // Echo is an in-place tool: its line proves the front handed it these words, not 75.
        let out = through_clean_front_in(
            &world,
            &scratch,
            Path::new("/bin/echo"),
            &words,
            Some(&target),
        );
        assert!(out.status.success(), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line == words.join(" ")),
            "{out:?}"
        );
        assert!(!target.exists(), "no managed cache was created for it");
        assert!(
            !world
                .home
                .join(INDEX)
                .join(format!("{}.json", key(&target).unwrap()))
                .exists()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clean_reaches_the_tool_without_adopting_unknown_or_changing_registered_output() {
        for registered in [false, true] {
            let world = World::new();
            if registered {
                world.build();
            } else {
                std::fs::create_dir(&world.target).unwrap();
            }
            let sentinel = world.target.join("keep");
            std::fs::write(&sentinel, b"clean itself owns removal").unwrap();
            let before = std::fs::read(world.path()).ok();
            let marker = std::fs::read(world.target.join(MARKER)).ok();
            let target = format!("--target-dir={}", world.target.display());
            for words in [
                vec!["clean".into(), target.clone()],
                vec![
                    "--config".into(),
                    "build.build-dir='custom-output'".into(),
                    "clean".into(),
                    target.clone(),
                ],
                vec!["metadata".into(), target.clone()],
            ] {
                // Echo is an in-place tool, not a copied image or a script. Its output
                // proves the actual front handed it these words and did not return 75.
                let out = through_clean_front(&world, Path::new("/bin/echo"), &words);
                assert!(out.status.success(), "{registered}: {out:?}");
                assert!(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .any(|line| line == words.join(" ")),
                    "{registered}: words={words:?}, output={out:?}"
                );
                assert_eq!(std::fs::read(world.path()).ok(), before);
                assert_eq!(std::fs::read(world.target.join(MARKER)).ok(), marker);
                assert_eq!(
                    std::fs::read(&sentinel).unwrap(),
                    b"clean itself owns removal"
                );
            }
            let words = vec![
                "--config".into(),
                "build.build-dir='custom-output'".into(),
                "--unverified".into(),
                "check".into(),
                target.clone(),
            ];
            let out = through_clean_front(&world, Path::new("/bin/echo"), &words);
            assert_eq!(out.status.code(), Some(75), "{out:?}");
            assert!(
                !String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|line| line == words.join(" "))
            );
            assert_eq!(std::fs::read(world.path()).ok(), before);
            assert_eq!(std::fs::read(world.target.join(MARKER)).ok(), marker);
            if !registered {
                let words = vec!["--unverified".into(), "check".into(), target];
                let out = through_clean_front(&world, Path::new("/bin/echo"), &words);
                assert_eq!(out.status.code(), Some(75), "{out:?}");
                assert!(
                    !String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .any(|line| line == words.join(" "))
                );
                assert!(!world.path().exists() && !world.target.join(MARKER).exists());
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_real_clean_and_rebuild_publish_fresh_owned_output_over_only_a_missing_root() {
        let world = World::new();
        std::fs::write(
            world.owner.join("Cargo.toml"),
            "[package]\nname = 'cache-clean-probe'\nversion = '0.0.0'\nedition = '2024'\n[workspace]\n",
        ).unwrap();
        std::fs::create_dir(world.owner.join("src")).unwrap();
        std::fs::write(
            world.owner.join("src/lib.rs"),
            "pub const BUILT: usize = 1;\n",
        )
        .unwrap();
        let target = format!("--target-dir={}", world.target.display());
        let build = vec![
            "--unverified".into(),
            "check".into(),
            "--offline".into(),
            "--message-format=json".into(),
            "-j1".into(),
            target.clone(),
        ];
        // Cargo supplies the actual driver used to compile this image. Managed creation,
        // both clean calls and the final rebuild enter the production shim front.
        let driver = Path::new(env!("CARGO"));
        let out = through_clean_front(&world, driver, &build);
        assert!(out.status.success(), "initial managed compilation: {out:?}");
        let before = read(&world.path()).unwrap();
        let recorded = std::fs::read(world.path()).unwrap();
        let tag = std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap();
        assert_eq!(
            tag,
            format!("{}\n", aterm_toolchain::lasting::CACHEDIR_TAG_SIGNATURE).as_bytes()
        );
        assert!(same(&world.home, &before));
        let out = through_clean_front(&world, driver, &["clean".into(), target.clone()]);
        assert!(out.status.success(), "registered cleanup: {out:?}");
        assert!(!world.target.exists(), "Cargo removed this private target");
        assert_eq!(std::fs::read(world.path()).unwrap(), recorded);

        // Model another producer by having Cargo itself recreate generated output and its
        // standard tag, outside managed admission. The historical receipt binds nothing in
        // this replacement; the next managed build adopts the folder Cargo made where it
        // stands, as it adopts one never recorded, and keeps everything in it.
        let mut foreign = aterm_uds::spawnfd::command(driver);
        foreign
            .args(&build)
            .current_dir(&world.owner)
            .env_clear()
            .env("HOME", &world.home);
        if let Some(path) = std::env::var_os("PATH") {
            foreign.env("PATH", path);
        }
        let out = aterm_uds::deadline::ends_within(&mut foreign, aterm_spec::wait::MUST_HAPPEN);
        assert!(
            out.status.success(),
            "unregistered compiler output: {out:?}"
        );
        let foreign_tag = std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap();
        assert!(
            foreign_tag.starts_with(aterm_toolchain::lasting::CACHEDIR_TAG_SIGNATURE.as_bytes())
        );
        assert!(!world.target.join(MARKER).exists());
        assert_eq!(std::fs::read(world.path()).unwrap(), recorded);
        std::fs::write(world.target.join("keep"), b"unknown replacement").unwrap();
        let replacement = identity(&world.target).unwrap();
        let adopted = through_clean_front(&world, Path::new("/bin/echo"), &build);
        assert!(adopted.status.success(), "{adopted:?}");
        let receipt = read(&world.path()).unwrap();
        assert_eq!(receipt.identity, replacement);
        assert!(same(&world.home, &receipt));
        assert_eq!(
            std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap(),
            foreign_tag
        );
        assert_eq!(
            std::fs::read(world.target.join("keep")).unwrap(),
            b"unknown replacement"
        );
        let recorded = std::fs::read(world.path()).unwrap();
        let out = through_clean_front(&world, driver, &["clean".into(), target]);
        assert!(out.status.success(), "adopted cleanup: {out:?}");
        assert!(!world.target.exists());
        assert_eq!(std::fs::read(world.path()).unwrap(), recorded);

        let out = through_clean_front(&world, driver, &build);
        assert!(out.status.success(), "{out:?}");
        let after = read(&world.path()).unwrap();
        assert!(
            after.nonce != before.nonce && after.nonce != receipt.nonce,
            "a new creation, not a historical receipt"
        );
        assert_eq!(after.identity, identity(&world.target).unwrap());
        assert_eq!(after.owners, [world.owner]);
        assert!(!after.owners_unknown && after.creating.is_none() && after.pending.is_none());
        assert!(world.target.join(MARKER).is_file());
        assert_eq!(
            std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap(),
            tag
        );
        assert!(same(&world.home, &after));
        let mut artifacts = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Ok(message) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            if message.get("reason").and_then(aterm_json::Value::as_str)
                == Some("compiler-artifact")
            {
                for artifact in message
                    .get("filenames")
                    .and_then(aterm_json::Value::as_array)
                    .expect("compiler artifact paths")
                {
                    artifacts.push(PathBuf::from(artifact.as_str().expect("artifact path")));
                }
            }
        }
        assert!(
            !artifacts.is_empty(),
            "the actual compiler reported its output: {out:?}"
        );
        let target = world.target.canonicalize().unwrap();
        for artifact in artifacts {
            assert!(
                artifact.is_file() && artifact.canonicalize().unwrap().starts_with(&target),
                "actual compiler output in the managed target: {}",
                artifact.display()
            );
        }
    }

    #[test]
    fn unknown_invocations_suppress_retirement_but_never_claim_fresh_targets() {
        let world = World::new();
        world.build();
        let env =
            |name: &str| (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned());
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&world.owner),
            env: &env,
            read: &|_| Ok(None),
            writable: &|_| Some(true),
        };
        let args = ["--unverified".into(), "an-unknown-alias".into()];
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
        let unknown = std::fs::read(world.path()).unwrap();
        assert!(
            !observe_on(
                &["metadata".into()],
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .unwrap()
        );
        assert_eq!(std::fs::read(world.path()).unwrap(), unknown);
        let fresh = world.home.join("unknown-fresh.noindex");
        let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| fresh.as_os_str().to_owned());
        let machine = super::super::Machine {
            env: &env,
            ..machine
        };
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(!fresh.exists());
    }

    #[test]
    fn configured_targets_preserve_an_unobserved_shared_owner() {
        let world = World::new();
        world.build();
        let owner = world.another_owner();
        let config = owner.join(".cargo/config.toml");
        let read_config = |path: &Path| {
            Ok((path == config)
                .then(|| format!("[build]\ntarget-dir = '{}'\n", world.target.display())))
        };
        let machine = super::super::Machine {
            home: Some(&world.home),
            root: None,
            cwd: Some(&owner),
            env: &|_| None,
            read: &read_config,
            writable: &|_| Some(true),
        };
        observe_on(
            &["--unverified".into(), "build".into()],
            super::super::Tool::Targo,
            &world.home,
            &machine,
        )
        .unwrap();
        std::fs::remove_dir_all(&world.owner).unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
        assert!(
            reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        assert!(world.target.join("debug/deps/keep").exists());
        let args = [
            "--unverified".into(),
            "build".into(),
            "--config".into(),
            format!("build.target-dir='{}'", world.target.display()),
        ];
        observe_on(&args, super::super::Tool::Targo, &world.home, &machine).unwrap();
        assert!(!owner.join("target").exists());
        let mut receipt = read(&world.path()).unwrap();
        receipt.owners_unknown = false;
        write(&world.path(), &receipt).unwrap();
        let args = [
            "-C".into(),
            "another-launcher".into(),
            "test".into(),
            "--target-dir".into(),
            "../outputs.noindex".into(),
        ];
        observe_on(&args, super::super::Tool::Cargo, &world.home, &machine).unwrap();
        assert!(read(&world.path()).unwrap().owners_unknown);
    }

    #[test]
    fn interrupted_staging_never_authorizes_an_unrelated_payload() {
        let world = World::new();
        world.build();
        // Two interrupted attempts can belong to the same root generation. Each stage must
        // retain its own custody without making the next producer permanently unavailable.
        for name in ["interrupted-a.noindex", "interrupted-b.noindex"] {
            let mut receipt = read(&world.path()).unwrap();
            let stage = world.home.join(name);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&stage)
                .unwrap();
            let payload = stage.join("cache.noindex");
            std::fs::create_dir(&payload).unwrap();
            std::fs::write(payload.join("unowned"), b"preserve").unwrap();
            receipt.pending = Some(Pending {
                directory: stage.clone(),
                identity: identity(&stage).unwrap(),
                authorized: false,
            });
            write(&world.path(), &receipt).unwrap();
            assert!(
                reclaim(
                    &world.home,
                    world.later(),
                    super::super::ReclaimAfter::CALM,
                    None,
                    &mut |_, _| true
                )
                .is_empty()
            );
            assert_eq!(std::fs::read(payload.join("unowned")).unwrap(), b"preserve");
            assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
            assert!(read(&world.path()).unwrap().pending.is_none());
            assert!(payload.exists());
        }
        assert_eq!(roots(&world.home).len(), 3);
        assert!(world.target.join("debug/deps/keep").exists());
    }

    /// THE HOP HANDS ITS FRONT THE CALL, AND NO BUILD STARTS WITHOUT IT: the front gets
    /// `__build_admit --shim targo <placed> <program>` and the words, and the shim reaches no line
    /// after it (the front became the build); with no helper there the shim refuses (exit 75,
    /// said) and starts nothing. NEGATIVE CONTROL: the same lines with the helper's `exec` taken
    /// off run on past the helper into the refusal, so the front's end is not the shim's.
    #[test]
    fn the_hop_hands_its_front_the_call_and_nothing_runs_without_it() {
        let world = World::new();
        let helper = world.home.join("atpkg");
        let shim = world.home.join("targo");
        let log = world.home.join("ran");
        std::fs::write(
            &helper,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$ATPKG_TEST_CACHE_LOG\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hop = shim_hop(&helper, "targo", "'/store/targo'", None);
        let run = |hop: &str| {
            let _ = std::fs::remove_file(&log);
            std::fs::write(
                &shim,
                format!(
                    "#!/bin/sh\n__atb=\n{hop}printf 'compiler\\n' >> \"$ATPKG_TEST_CACHE_LOG\"\n"
                ),
            )
            .unwrap();
            let mut command = aterm_uds::spawnfd::command("/bin/sh");
            command
                .arg(&shim)
                .args(["--unverified", "check"])
                .env_clear()
                .env("ATPKG_TEST_CACHE_LOG", &log);
            let result =
                aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
            (result, std::fs::read_to_string(&log).unwrap_or_default())
        };
        let (result, ran) = run(&hop);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            ran,
            "__build_admit\n--shim\ntargo\n-\n/store/targo\n--unverified\ncheck\n"
        );
        let (result, ran) = run(&hop.replacen(" && exec ", " && ", 1));
        assert_eq!(result.status.code(), Some(75), "the control runs on: {ran}");
        assert!(ran.starts_with("__build_admit\n"), "{ran}");
        std::fs::remove_file(&helper).unwrap();
        let (result, ran) = run(&hop);
        assert_eq!(result.status.code(), Some(75));
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("the cache registrar is unavailable"),
            "{result:?}"
        );
        assert_eq!(ran, "", "no build started");
    }

    /// The front reads the call as typed: a `config` placement's two leading words and derived
    /// environment come off, an `env` placement hides its variable, and `-` changes nothing.
    /// NEGATIVE CONTROL: a `config` placement whose leading words are not the block's keeps them
    /// (a typed `--config` is the call's own), and a placement the block never says reads none.
    #[test]
    fn the_front_reads_the_call_as_typed() {
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let placed = words(&[
            "--config",
            "build.build-dir='/c/ws/.cargo-build'",
            "test",
            "-q",
        ]);
        assert_eq!(
            typed_call(&placed, "config"),
            Some((&placed[2..], Some(super::super::BUILD_DIR_ENV)))
        );
        assert_eq!(
            typed_call(&placed[2..], "env"),
            Some((&placed[2..], Some(super::super::BUILD_DIR_ENV)))
        );
        assert_eq!(typed_call(&placed, "-"), Some((&placed[..], None)));
        let own = words(&["--config", "net.offline=true", "test"]);
        assert_eq!(typed_call(&own, "config"), Some((&own[..], None)));
        assert_eq!(typed_call(&own, "both"), None);
    }

    #[test]
    fn helper_failure_or_interruption_starts_no_compiler() {
        let world = World::new();
        let helper = world.home.join("registrar");
        let shim = world.home.join("targo");
        let log = world.home.join("ran");
        for exit in [75, 130, 2] {
            std::fs::write(&helper, format!("#!/bin/sh\nexit {exit}\n")).unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(
                &shim,
                format!(
                    "#!/bin/sh\n{}\nprintf 'compiler\\n' >> \"$ATPKG_TEST_CACHE_LOG\"\n",
                    shim_call(&helper, "targo")
                ),
            )
            .unwrap();
            let mut command = aterm_uds::spawnfd::command("/bin/sh");
            command
                .arg(&shim)
                .env_clear()
                .env("ATPKG_TEST_CACHE_LOG", &log);
            let result =
                aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
            assert_eq!(result.status.code(), Some(75));
            assert!(!log.exists());
        }
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .arg(&shim)
            .env_clear()
            .env("ATPKG_TEST_CACHE_LOG", &log);
        let result =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
        assert!(result.status.success());
        assert_eq!(std::fs::read_to_string(log).unwrap(), "compiler\n");
    }

    #[test]
    fn a_contained_recorded_owner_needs_no_index_write_authority() {
        let world = World::new();
        world.build();
        let original = std::fs::read(world.path()).unwrap();
        let other_root = world.home.join("another-root.noindex");
        assert!(create_checked(&world.home, &other_root, &world.owner).unwrap());
        let other_path = world
            .home
            .join(INDEX)
            .join(format!("{}.json", key(&other_root).unwrap()));
        std::fs::copy(other_path, world.path()).unwrap();
        assert!(create_checked(&world.home, &world.target, &world.owner).is_err());
        std::fs::write(world.path(), original).unwrap();
        let directory = world.home.join(INDEX);
        let gates = directory.join("locks.noindex");
        let gate = gate_path(&world.path()).unwrap();
        for path in [&directory, &gates] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        for path in [&world.path(), &gate] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        let other = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &other).is_err());
        for path in [&directory, &gates] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// THE RECORD OF `world`'s TARGET, ITS RECEIPT AND ITS MARKER, AS KEEPING `identity`: as an
    /// earlier boot, an older atpkg or another volume recorded it (VOLUME-IDENTITY). The marker is
    /// written through the folder held, as atpkg writes one.
    fn recorded_as(world: &World, identity: &Identity) {
        let path = world.path();
        let mut receipt = read(&path).unwrap();
        receipt.identity = identity.clone();
        write(&path, &receipt).unwrap();
        let held = creation::pin(&world.target).unwrap();
        let mut marker = retention::receipt_at(&held).unwrap();
        marker.identity = identity.clone();
        retention::write_marker(&held, &marker).unwrap();
    }

    /// The identity an earlier boot recorded of the folder read now as `now`: the same volume and
    /// inode, the volume numbered otherwise, as a restart renumbers one. Only the tests that read
    /// a volume's UUID (macOS) ask for one.
    #[cfg(target_os = "macos")]
    fn an_earlier_boots(now: &Identity) -> Identity {
        Identity {
            device: now.device.wrapping_add(1),
            ..now.clone()
        }
    }

    /// WHETHER A RECORD NAMES A FOLDER, AS [`Identity::names`] DECIDES IT (VOLUME-IDENTITY): a
    /// record that keeps the volume's UUID names the same inode on the volume of that UUID
    /// whatever its device number now (a), and nothing on another volume, its device number and
    /// inode those recorded (b), nor on a volume that reads with no UUID now; nothing of another
    /// inode (c); a record that keeps none names by its device number alone (d), and is witnessed
    /// (gains the UUID, its device number and inode kept) only while that number still names it.
    /// Two records name one folder by their UUIDs where both keep one, else by device number.
    #[test]
    fn a_record_names_a_folder_by_its_volumes_uuid_else_by_its_device_number() {
        let (one, another) = ([7; 16], [8; 16]);
        let at = |device, inode, volume| Identity {
            device,
            inode,
            volume,
        };
        let recorded = at(16_777_233, 42, Some(one));
        assert!(
            recorded.names(&at(16_777_232, 42, Some(one))),
            "(a) renumbered"
        );
        assert!(recorded.names(&at(16_777_233, 42, Some(one))));
        assert!(!recorded.names(&at(16_777_233, 42, Some(another))), "(b)");
        assert!(!recorded.names(&at(16_777_233, 42, None)), "no UUID now");
        assert!(!recorded.names(&at(16_777_233, 43, Some(one))), "(c)");
        let legacy = at(16_777_233, 42, None);
        assert!(
            legacy.names(&at(16_777_233, 42, Some(one))),
            "(d) as before"
        );
        assert!(
            !legacy.names(&at(16_777_232, 42, Some(one))),
            "(d) renumbered"
        );
        assert!(!legacy.names(&at(16_777_233, 43, Some(one))));
        assert_eq!(
            legacy.witnessed(&at(16_777_233, 42, Some(one))),
            Some(at(16_777_233, 42, Some(one)))
        );
        assert_eq!(legacy.witnessed(&at(16_777_232, 42, Some(one))), None);
        assert_eq!(legacy.witnessed(&at(16_777_233, 42, None)), None);
        assert_eq!(recorded.witnessed(&at(16_777_233, 42, Some(one))), None);
        assert!(
            legacy.same_record(&recorded),
            "witnessed beside unwitnessed"
        );
        assert!(recorded.same_record(&at(16_777_232, 42, Some(one))));
        assert!(!recorded.same_record(&at(16_777_233, 42, Some(another))));
        assert!(!legacy.same_record(&at(16_777_232, 42, None)));
        assert!(!legacy.same_record(&at(16_777_233, 43, None)));
        // A record that keeps no UUID is written as an older atpkg wrote one, and reads back.
        let bytes = aterm_json::to_vec(&legacy).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("volume"));
        assert_eq!(aterm_json::from_slice::<Identity>(&bytes).unwrap(), legacy);
        let bytes = aterm_json::to_vec(&recorded).unwrap();
        assert_eq!(
            aterm_json::from_slice::<Identity>(&bytes).unwrap(),
            recorded
        );
    }

    /// (a) A RECEIPT AN EARLIER BOOT RECORDED BINDS ITS FOLDER BY ITS VOLUME'S UUID: a build
    /// records the volume's UUID beside the device number; recorded with the volume numbered
    /// otherwise (as before a restart: the same UUID and inode, another device number), the cache
    /// is still the registered root it was, its owner admitted with nothing written, another
    /// owner recorded, the device number kept as recorded, and its marker binds it, read by its
    /// path and through the folder held. Before receipts kept the UUID, this was the restart's
    /// `the managed build cache's owner could not be recorded`, exit 75.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_receipt_an_earlier_boot_recorded_binds_its_folder_by_its_volumes_uuid() {
        let world = World::new();
        world.build();
        let now = identity(&world.target).unwrap();
        assert!(now.volume.is_some(), "the scratch volume has a UUID");
        assert_eq!(read(&world.path()).unwrap().identity, now);
        let earlier = an_earlier_boots(&now);
        recorded_as(&world, &earlier);
        assert!(same(&world.home, &read(&world.path()).unwrap()));
        let index = receipts(&world.home);
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert_eq!(
            receipts(&world.home),
            index,
            "an owner on record writes nothing"
        );
        let other = world.another_owner();
        assert!(create_checked(&world.home, &world.target, &other).unwrap());
        let receipt = read(&world.path()).unwrap();
        assert_eq!(
            receipt.identity, earlier,
            "the device number stays as recorded"
        );
        assert_eq!(receipt.owners, [world.owner.clone(), other]);
        assert!(
            roots(&world.home)
                .iter()
                .any(|cache| cache.root == world.target)
        );
        assert_eq!(retention::kept_by(&world.target), None);
        assert!(retention::allows_at(creation::pin(&world.target).unwrap()));
    }

    /// (b) A RECEIPT OF ANOTHER VOLUME BINDS NOTHING, THOUGH ITS DEVICE NUMBER AND INODE MATCH: a
    /// record whose UUID is another volume's, with the device number and inode the folder reads
    /// with now (another volume reusing that number, as a restart can leave it), names nothing
    /// here: the producer is refused and nothing is written, it is no registered root, and its
    /// marker binds nothing, so the ordinary cleaners keep the folder. MUTANT: with the UUID
    /// dropped from [`Identity::names`] (the device number alone deciding), the producer is
    /// admitted and this fails.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_receipt_of_another_volume_binds_nothing_though_its_device_number_and_inode_match() {
        let world = World::new();
        world.build();
        let now = identity(&world.target).unwrap();
        let mut volume = now.volume.unwrap();
        volume[0] ^= 0xff;
        let elsewhere = Identity {
            volume: Some(volume),
            ..now.clone()
        };
        assert_eq!((elsewhere.device, elsewhere.inode), (now.device, now.inode));
        recorded_as(&world, &elsewhere);
        assert!(!same(&world.home, &read(&world.path()).unwrap()));
        let index = receipts(&world.home);
        let refused = create_checked(&world.home, &world.target, &world.another_owner())
            .expect_err("another volume's receipt admits no producer");
        assert_eq!(
            refused.to_string(),
            "the managed build cache's owner could not be recorded"
        );
        assert_eq!(receipts(&world.home), index, "nothing is written");
        assert!(
            !roots(&world.home)
                .iter()
                .any(|cache| cache.root == world.target)
        );
        assert_eq!(
            retention::kept_by(&world.target),
            Some(world.target.as_path())
        );
        assert!(!retention::allows_at(creation::pin(&world.target).unwrap()));
    }

    /// (c) A FOLDER SWAPPED IN AT A RECEIPT'S PATH BINDS NOTHING: the cache moved aside and
    /// another private folder made at its path, holding a copy of its marker: the same volume and
    /// device number, another inode. The producer is refused, nothing is written, and the copied
    /// marker binds nothing there, so the ordinary cleaners keep it; the cache moved aside binds
    /// nothing either (its receipt names its path).
    #[test]
    fn a_folder_swapped_in_at_a_receipts_path_binds_nothing() {
        let world = World::new();
        world.build();
        let receipt = read(&world.path()).unwrap();
        let aside = world.home.join("aside.noindex");
        std::fs::rename(&world.target, &aside).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&world.target)
            .unwrap();
        for name in [MARKER, "CACHEDIR.TAG"] {
            std::fs::copy(aside.join(name), world.target.join(name)).unwrap();
        }
        let now = identity(&world.target).unwrap();
        assert_eq!(
            (now.device, &now.volume),
            (receipt.identity.device, &receipt.identity.volume)
        );
        assert_ne!(now.inode, receipt.identity.inode);
        assert!(!same(&world.home, &receipt));
        let index = receipts(&world.home);
        assert!(
            create_checked(&world.home, &world.target, &world.another_owner()).is_err(),
            "a swapped folder admits no producer"
        );
        assert_eq!(receipts(&world.home), index, "nothing is written");
        assert_eq!(
            retention::kept_by(&world.target),
            Some(world.target.as_path())
        );
        assert!(
            !roots(&world.home)
                .iter()
                .any(|cache| cache.root == world.target)
        );
    }

    /// (d) A RECEIPT AN OLDER ATPKG WROTE (NO VOLUME UUID) IS WITNESSED WHILE THIS BOOT NUMBERS ITS
    /// VOLUME AS IT WAS RECORDED: it reads back as written; its owner's next build is admitted,
    /// and the receipt and its marker gain the volume's UUID, the device number and inode they
    /// recorded kept; so once a restart renumbers the volume the cache still binds, and the next
    /// build is admitted. NEGATIVE CONTROL: without the witness, the renumbered receipt is refused
    /// as [`renumbered`].
    #[cfg(target_os = "macos")]
    #[test]
    fn a_receipt_an_older_atpkg_wrote_is_witnessed_while_its_volume_is_numbered_alike() {
        let world = World::new();
        world.build();
        let now = identity(&world.target).unwrap();
        let legacy = Identity::unrecorded(now.device, now.inode);
        recorded_as(&world, &legacy);
        assert!(!String::from_utf8_lossy(&std::fs::read(world.path()).unwrap()).contains("volume"));
        assert!(same(&world.home, &read(&world.path()).unwrap()));
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert_eq!(read(&world.path()).unwrap().identity, now, "the receipt");
        let marker = retention::receipt_at(creation::pin(&world.target).unwrap()).unwrap();
        assert_eq!(marker.identity, now, "the marker");
        recorded_as(&world, &an_earlier_boots(&now));
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(create_checked(&world.home, &world.target, &world.another_owner()).unwrap());
    }

    /// (d) A RECEIPT AN OLDER ATPKG WROTE ON A VOLUME A RESTART HAS RENUMBERED SINCE STAYS REFUSED,
    /// AND SAYS WHAT TO DO: its folder stands at its path with the inode it recorded and the
    /// marker that binds it, but nothing it recorded proves the volume the one it was written on,
    /// so it is never witnessed: the producer is refused naming the cache, both device numbers
    /// and the remedy; nothing is written, and the ordinary cleaners keep the folder. Once the
    /// person removes it, as the words say, the next build makes it again.
    #[test]
    fn a_receipt_an_older_atpkg_wrote_on_a_renumbered_volume_stays_refused_and_says_what_to_do() {
        let world = World::new();
        world.build();
        let now = identity(&world.target).unwrap();
        let legacy = Identity::unrecorded(now.device.wrapping_add(1), now.inode);
        recorded_as(&world, &legacy);
        let receipt = read(&world.path()).unwrap();
        assert_eq!(renumbered(&world.home, &receipt), Some(now.device));
        let index = receipts(&world.home);
        for owner in [world.owner.clone(), world.another_owner()] {
            let refused = create_checked(&world.home, &world.target, &owner)
                .expect_err("a renumbered receipt that keeps no UUID admits no producer");
            let words = refused.to_string();
            assert_eq!(words, renumbered_words(&receipt, now.device));
            for part in [
                format!(
                    "{} was recorded on device {}",
                    world.target.display(),
                    legacy.device
                ),
                format!("reads as device {} now", now.device),
                format!("remove {}", world.target.display()),
                "or build into another target directory".to_owned(),
            ] {
                assert!(words.contains(&part), "{words}: {part}");
            }
        }
        assert_eq!(
            receipts(&world.home),
            index,
            "nothing is written or witnessed"
        );
        assert_eq!(
            retention::kept_by(&world.target),
            Some(world.target.as_path())
        );
        std::fs::remove_dir_all(&world.target).unwrap();
        assert!(create_checked(&world.home, &world.target, &world.owner).unwrap());
        assert!(same(&world.home, &read(&world.path()).unwrap()));
    }

    /// THE RECLAIM PASS WITNESSES A RECEIPT NO BUILD TOUCHED: a receipt an older atpkg wrote, its
    /// volume numbered as recorded, gains its UUID in a pass that removes nothing (its owner
    /// stands and it is fresh), so the cache binds after a restart though no build ran before it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_reclaim_pass_witnesses_a_receipt_no_build_touched() {
        let world = World::new();
        world.build();
        let now = identity(&world.target).unwrap();
        recorded_as(&world, &Identity::unrecorded(now.device, now.inode));
        let removed = reclaim(
            &world.home,
            SystemTime::now(),
            super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
        );
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(read(&world.path()).unwrap().identity, now);
        recorded_as(&world, &an_earlier_boots(&now));
        assert!(create_checked(&world.home, &world.target, &world.another_owner()).unwrap());
    }

    /// A RETIREMENT STAGED IN AN EARLIER BOOT IS FINISHED, AND ONE WHOSE STAGE IS ANOTHER VOLUME'S
    /// STAYS: the receipt, its marker and its pending stage recorded with the volume numbered
    /// otherwise (the same UUIDs and inodes), the next pass removes the staged cache and its
    /// stage by their recorded identities, read through the folders held. NEGATIVE CONTROL: the
    /// stage recorded with another volume's UUID is no stage of this receipt's, and stays whole.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_retirement_staged_in_an_earlier_boot_finishes_and_another_volumes_stays() {
        for elsewhere in [false, true] {
            let world = World::new();
            world.build();
            let root = an_earlier_boots(&identity(&world.target).unwrap());
            recorded_as(&world, &root);
            let stage = world.home.join("interrupted-retirement.noindex");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&stage)
                .unwrap();
            let mut staged = an_earlier_boots(&identity(&stage).unwrap());
            if elsewhere {
                staged.volume.as_mut().unwrap()[0] ^= 0xff;
            }
            let mut receipt = read(&world.path()).unwrap();
            receipt.pending = Some(Pending {
                directory: stage.clone(),
                identity: staged,
                authorized: false,
            });
            write(&world.path(), &receipt).unwrap();
            std::fs::rename(&world.target, stage.join(PAYLOAD)).unwrap();
            assert_eq!(
                pending_owned(&world.home, &receipt, receipt.pending.as_ref().unwrap()),
                !elsewhere
            );
            let removed = reclaim(
                &world.home,
                world.later(),
                super::super::ReclaimAfter::CALM,
                None,
                &mut |_, _| true,
            );
            if elsewhere {
                assert!(removed.is_empty(), "{removed:?}");
                assert!(stage.join(PAYLOAD).join("debug/deps/keep").is_file());
                assert!(read(&world.path()).unwrap().pending.is_some());
            } else {
                assert_eq!(removed.len(), 1, "{removed:?}");
                assert!(!stage.exists());
            }
        }
    }

    /// A LINK TO A CACHE AN OLDER ATPKG RECORDED ON A VOLUME RENUMBERED SINCE IS REFUSED IN ITS
    /// OWN WORDS ([`LinkRefusal::Renumbered`]), with nothing written; NEGATIVE CONTROL: with the
    /// device number its volume has now, the same link is owned.
    #[test]
    fn a_link_to_a_cache_recorded_on_a_renumbered_volume_says_so() {
        for renumbered in [true, false] {
            let world = World::new();
            world.build();
            let now = identity(&world.target).unwrap();
            let device = now.device.wrapping_add(u64::from(renumbered));
            recorded_as(&world, &Identity::unrecorded(device, now.inode));
            let link = world.home.join("target");
            std::os::unix::fs::symlink("outputs.noindex", &link).unwrap();
            let other = world.another_owner();
            let machine = super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: Some(&other),
                env: &|_| None,
                read: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            let index = receipts(&world.home);
            let observed = observe(
                &build_into(&other, &link),
                super::super::Tool::Targo,
                &world.home,
                &machine,
            )
            .unwrap();
            if renumbered {
                assert_eq!(observed, Observation::Refused(LinkRefusal::Renumbered));
                assert_eq!(receipts(&world.home), index);
            } else {
                assert_eq!(observed, Observation::Owned);
            }
        }
    }
}
