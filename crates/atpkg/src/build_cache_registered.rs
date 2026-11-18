// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit Cargo caches created by aterm, at the caller's chosen location.
//!
//! A name or a cache tag is not ownership. A fresh private stage and durable identity receipt
//! precede atomic target publication. An existing unrecorded target that proves cargo made it
//! as its build directory (cargo's own tag, which cargo writes into a build directory only as
//! it makes it, and the rustc facts beside a profile a compilation or `clean -p` laid there) is
//! recorded where it stands, its
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

#[path = "build_cache_known.rs"]
mod known;
pub use known::KnownCargoCache;

#[path = "build_cache_retain.rs"]
pub(super) mod retention;
pub use retention::CAPABILITY as RETAIN_CAPABILITY;
pub use retention::retain;

#[cfg(unix)]
#[path = "build_cache_gate.rs"]
mod gate;

#[cfg(unix)]
#[path = "build_cache_receipt_stage.rs"]
mod receipt_stage;

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
            #[cfg(unix)]
            if receipt_stage::abandoned(&record) {
                continue;
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

impl Receipt {
    /// Whether this is ordinary settled custody: no keep and no removal's fence, no creation or
    /// retirement under way, and no package policy, as a plain creation or an adoption records
    /// once it is done.
    #[cfg(unix)]
    const fn ordinary_settled(&self) -> bool {
        !self.retained
            && self.removal_fence.is_none()
            && self.creating.is_none()
            && self.pending.is_none()
            && self.package.is_none()
    }
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
    let child = private_child(parent, name, path)?;
    // Synchronize even an existing component: a previous attempt may have stopped after mkdir.
    creation::sync_directory(&child, point)?;
    creation::sync_directory(parent, point)?;
    Some(child)
}

/// Establish a private child without a durability claim. Disposable intermediates need its
/// live descriptor binding; receipt publication additionally synchronizes its hierarchy.
#[cfg(unix)]
fn private_child(
    parent: &aterm_dirfd::OwnedFd,
    name: &std::ffi::OsStr,
    path: &Path,
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
    Some(child)
}

/// WHETHER THIS HOME CAN RECORD NO BUILD'S OWNER: the deepest folder standing on the index's path
/// ([`INDEX`], then its gates) is one this user may not write, so no missing folder, receipt or
/// gate can be made there, as in a home whose `Library/Caches` is another user's or read-only.
/// The shim's block and [`super::decide`] place a build in its own target there (the shared
/// cache sits in the same folders), and that placement owes no receipt
/// ([`observe_with_placement`]). A path that reaches anything but a folder (a link, a file)
/// says nothing of the kind: a build there is refused as before.
fn unrecordable(home: &Path) -> bool {
    let mut standing = home.to_path_buf();
    let gates = std::path::Component::Normal(std::ffi::OsStr::new("locks.noindex"));
    for component in Path::new(INDEX).components().chain([gates]) {
        let next = standing.join(component);
        match std::fs::symlink_metadata(&next) {
            Ok(metadata) if metadata.is_dir() => standing = next,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            _ => return false,
        }
    }
    !crate::platform::dir_writable_by_caller(&standing)
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
fn open_admission(index: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(index.join("locks.noindex/admission.lock"))
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
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

/// Whether `receipt` binds the folder at its root: a receipt no creation is still making, of a
/// canonical folder in scope with the identity it recorded, no source and no cargo home's cache
/// ([`adoption::home_cache`]: what came under custody before it was one, or under a rule that
/// did not refuse it, binds nothing once it is), whose marker binds it ([`marker_binds`]).
fn same(home: &Path, receipt: &Receipt) -> bool {
    receipt.creating.is_none()
        && scope(home, &receipt.root)
        && std::fs::canonicalize(&receipt.root).ok().as_ref() == Some(&receipt.root)
        && identity(&receipt.root).is_some_and(|now| receipt.identity.names(&now))
        && [".git", "Cargo.toml", "Targo.toml"].iter().all(|name| {
            std::fs::symlink_metadata(receipt.root.join(name))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
        && !home_cache(home, &receipt.root)
        && read(&receipt.root.join(MARKER)).is_some_and(|marker| marker_binds(&marker, receipt))
}

/// [`adoption::home_cache`] where adoption is built; elsewhere nothing is.
#[cfg(unix)]
fn home_cache(home: &Path, root: &Path) -> bool {
    adoption::home_cache(home, root)
}

#[cfg(not(unix))]
fn home_cache(_: &Path, _: &Path) -> bool {
    false
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
    if !receipt.ordinary_settled() || !same(home, receipt) || read(path).as_ref() != Some(receipt) {
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
    let Ok(gate) = gate::open(&gate) else {
        return;
    };
    let Ok(_held) = gate::try_exclusive(gate) else {
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
    let _held = gate::try_exclusive(file).ok()?;
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
    let Some(path) = producer_record(home, root) else {
        return false;
    };
    let Some(gate) = gate_path(&path) else {
        return false;
    };
    let Ok(file) = gate::open(&gate) else {
        return false;
    };
    let Ok(_held) = gate::try_shared(file) else {
        return false;
    };
    still() && read(&path).is_some_and(|receipt| producer_matches(home, root, workspace, &receipt))
}

#[cfg(unix)]
fn producer_record(home: &Path, root: &Path) -> Option<PathBuf> {
    let directory = home.join(INDEX);
    let gates = directory.join("locks.noindex");
    if identity(&directory).is_none()
        || identity(&gates).is_none()
        || std::fs::canonicalize(&gates).ok().as_ref() != Some(&gates)
    {
        return None;
    }
    Some(directory.join(format!("{}.json", key(root)?)))
}

#[cfg(unix)]
fn producer_matches(home: &Path, root: &Path, workspace: &Path, receipt: &Receipt) -> bool {
    receipt.root == root
        && receipt.pending.is_none()
        && same(home, receipt)
        && match &receipt.package {
            Some(policy) => {
                !receipt.owners_unknown && package::producer_allows(home, policy, workspace)
            }
            None => receipt.owners_unknown || receipt.owners.iter().any(|owner| owner == workspace),
        }
}

/// Prepare only the producer's derived descendants of an admitted target. The receipt remains
/// rooted at the target, and its shared gate excludes retirement throughout preparation.
#[cfg(unix)]
fn prepare_intermediates(
    home: &Path,
    root: &Path,
    workspace: &Path,
    still: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    prepare_owned_descendants(
        home,
        root,
        workspace,
        &super::target_build_relative(),
        still,
    )
}

/// WHETHER `receipt`, ON RECORD FOR THE FOLDER `path` (A DESCENDANT THAT CARRIES NO MARKER),
/// BINDS NOTHING THERE: ordinary settled custody ([`Receipt::ordinary_settled`]) of `path` whose
/// identity names another folder than the one read there now as `now`, the folder it bound no
/// longer standing there. As [`create_with_policy`] holds, an ordinary receipt of a folder no
/// longer here binds nothing here: no reclaimer acts on it ([`roots`] lists an ordinary settled
/// receipt only where [`same`] binds it) and no marker stands for it. A receipt that names the
/// folder (an adoption puts its receipt on record before its marker binds it), or any other kind
/// (pending, being created, a keep or a package's), binds custody still.
///
/// A folder made again at `path` is another folder only where it got another inode number. APFS,
/// the format of a Mac's own volumes, never hands a freed one out again, and a build prepares a
/// managed target's descendants on macOS alone (each build front that reaches
/// [`prepare_owned_descendants`] returns before it elsewhere, as [`observation_with`] does).
/// Where a filesystem does reuse one (ext4), a record of the folder that went names one made
/// again that took its number back, so that folder stays a boundary its record cannot satisfy,
/// refused as before.
#[cfg(unix)]
fn binds_nothing(path: &Path, receipt: &Receipt, now: &Identity) -> bool {
    receipt.root == path && receipt.ordinary_settled() && !receipt.identity.names(now)
}

/// Extend one admitted root under its shared retirement gate, without adopting existing
/// descendants or following a link. The root's receipt covers the prepared output paths.
///
/// Each descendant that carries a marker or is on record in the index is a custody boundary:
/// its receipt must admit this producer and name the folder held there, its gate held shared
/// until preparation ends, or preparation stops. The one exception is a descendant that carries
/// no marker and whose receipt [`binds_nothing`] there (a folder removed and made again under
/// this root, its old registration left on record): it is no boundary, and the root's receipt
/// covers it as any other descendant; its receipt's gate is held shared too, and preparation
/// stops if, at its end, the receipt changed, the folder carries a marker or that receipt names
/// it. Every writer of a receipt holds its gate exclusively and every publication holds the
/// admission lock exclusively, both held shared here, so no writer that keeps to them changes
/// either kind of receipt meanwhile, and the recheck sees one that does not.
#[cfg(unix)]
fn prepare_owned_descendants(
    home: &Path,
    root: &Path,
    workspace: &Path,
    relative: &Path,
    still: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    let stop = || {
        std::io::Error::other("the managed target's quiet build directory could not be prepared")
    };
    let record = producer_record(home, root).ok_or_else(stop)?;
    // Publication takes this lock exclusively. Hold it shared before any root gate so a
    // new nested custody boundary cannot appear between the walk's checks and preparation.
    let admission = open_admission(record.parent().ok_or_else(stop)?).ok_or_else(stop)?;
    let _held_admission = crate::lock::Flock::try_lock_shared(admission).map_err(|_| stop())?;
    let gate = gate::open(&gate_path(&record).ok_or_else(stop)?)?;
    let _held_gate = gate::try_shared(gate).map_err(|_| stop())?;
    let receipt = read(&record)
        .filter(|receipt| still() && producer_matches(home, root, workspace, receipt))
        .ok_or_else(stop)?;
    let parent = creation::pin(root.parent().ok_or_else(stop)?).ok_or_else(stop)?;
    let name = root.file_name().ok_or_else(stop)?;
    let held = creation::pin(root).ok_or_else(stop)?;
    if !Identity::held(&held).is_some_and(|now| receipt.identity.names(&now))
        || !creation::named(&parent, name, &receipt.identity)
    {
        return Err(stop());
    }
    let mut boundaries = Vec::new();
    let mut unbound = Vec::new();
    prepare_private_descendants(
        root,
        held,
        relative,
        &|| {
            still()
                && retention::private_directory(&parent)
                && creation::named(&parent, name, &receipt.identity)
        },
        &mut |path, held| {
            let marked =
                match aterm_dirfd::fstatat(held, MARKER, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(_) => true,
                    Err(error) if error.is_noent() => false,
                    Err(_) => return Err(stop()),
                };
            let record = producer_record(home, path).ok_or_else(stop)?;
            let indexed = match std::fs::symlink_metadata(&record) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => return Err(stop()),
            };
            if marked || indexed {
                let gate = gate::open(&gate_path(&record).ok_or_else(stop)?)?;
                let gate = gate::try_shared(gate).map_err(|_| stop())?;
                let on_record = read(&record).ok_or_else(stop)?;
                let now = Identity::held(held).ok_or_else(stop)?;
                if !marked && binds_nothing(path, &on_record, &now) {
                    unbound.push((record, on_record, gate));
                } else if producer_matches(home, path, workspace, &on_record)
                    && on_record.identity.names(&now)
                {
                    boundaries.push((record, on_record, gate));
                } else {
                    return Err(stop());
                }
            }
            Ok(())
        },
    )?;
    let unmarked = |path: &Path| {
        std::fs::symlink_metadata(path.join(MARKER))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    };
    if !still()
        || !creation::named(&parent, name, &receipt.identity)
        || !retention::private_directory(&parent)
        || read(&record).as_ref() != Some(&receipt)
        || !producer_matches(home, root, workspace, &receipt)
        || boundaries.iter().any(|(record, receipt, _gate)| {
            read(record).as_ref() != Some(receipt)
                || !producer_matches(home, &receipt.root, workspace, receipt)
        })
        || unbound.iter().any(|(record, receipt, _gate)| {
            read(record).as_ref() != Some(receipt)
                || !unmarked(&receipt.root)
                || !identity(&receipt.root)
                    .is_some_and(|now| binds_nothing(&receipt.root, receipt, &now))
        })
    {
        return Err(stop());
    }
    Ok(())
}

/// Prepare disposable directories while each held parent still has its original name below
/// the pinned canonical anchor. No ownership marker, index entry or durable flush is added.
#[cfg(unix)]
fn prepare_private_descendants(
    root: &Path,
    mut held: aterm_dirfd::OwnedFd,
    relative: &Path,
    still: &dyn Fn() -> bool,
    boundary: &mut dyn FnMut(&Path, &aterm_dirfd::OwnedFd) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let stop = || std::io::Error::other("the target's quiet build directory could not be prepared");
    let root_identity = Identity::held(&held).ok_or_else(stop)?;
    let mut chain: Vec<(aterm_dirfd::OwnedFd, std::ffi::OsString, Identity)> = Vec::new();
    let intact = |chain: &[(aterm_dirfd::OwnedFd, std::ffi::OsString, Identity)],
                  held: &aterm_dirfd::OwnedFd| {
        still()
            && retention::private_directory(held)
            && creation::pin(root)
                .and_then(Identity::held)
                .is_some_and(|now| root_identity.names(&now))
            && chain.iter().all(|(parent, name, identity)| {
                retention::private_directory(parent) && creation::named(parent, name, identity)
            })
    };
    if relative.components().count() > 64 {
        return Err(stop());
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(stop());
        };
        if !intact(&chain, &held) {
            return Err(stop());
        }
        path.push(name);
        let child = private_child(&held, name, &path).ok_or_else(stop)?;
        boundary(&path, &child)?;
        creation::step(creation::Point::Parent).ok_or_else(stop)?;
        let identity = Identity::held(&child).ok_or_else(stop)?;
        chain.push((held, name.to_os_string(), identity));
        held = child;
    }
    if !intact(&chain, &held) {
        return Err(stop());
    }
    let mut path = root.to_path_buf();
    for (at, (_, name, _)) in chain.iter().enumerate() {
        path.push(name);
        let child = chain.get(at + 1).map_or(&held, |next| &next.0);
        boundary(&path, child)?;
    }
    if !intact(&chain, &held) {
        return Err(stop());
    }
    Ok(())
}

/// Prepare the exact producer-selected intermediates without granting cache custody. Native
/// producers use this when their workspace is outside managed registration scope. Existing
/// directories keep their data and modes; only private canonical parents may be extended.
/// Outside macOS this preserves the existing no-preparation policy.
///
/// # Errors
/// The path is ambiguous, a parent or descendant is unsafe, or its binding changes.
pub fn prepare_target_build_dir(target: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let stop =
            || std::io::Error::other("the target's quiet build directory could not be prepared");
        if !target.is_absolute()
            || target
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            return Err(stop());
        }
        let mut anchor = target;
        loop {
            match std::fs::symlink_metadata(anchor) {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    anchor = anchor.parent().ok_or_else(stop)?;
                }
                Err(_) => return Err(stop()),
            }
        }
        let held = creation::pin(anchor).ok_or_else(stop)?;
        let destination = super::target_build_path(target);
        let relative = destination.strip_prefix(anchor).map_err(|_| stop())?;
        prepare_private_descendants(anchor, held, relative, &|| true, &mut |_, _| Ok(()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = target;
        Ok(())
    }
}

/// Prepare only this version's automatic final target below the canonical workspace root.
/// The lexical hierarchy is checked before any caller resolves it physically. Existing
/// contents and modes stay unchanged; this grants no cache custody or cleanup authority.
/// Outside macOS no directory preparation is added.
///
/// # Errors
/// The workspace root or a generated descendant is linked, shared, ambiguous or replaced.
pub fn prepare_default_target(root: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let stop = || std::io::Error::other("the automatic final target could not be prepared");
        if !root.is_absolute() {
            return Err(stop());
        }
        let held = creation::pin(root).ok_or_else(stop)?;
        prepare_private_descendants(
            root,
            held,
            Path::new(aterm_toolchain::build_cache::DEFAULT_TARGET),
            &|| true,
            &mut |_, _| Ok(()),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = root;
        Ok(())
    }
}

/// A producer's hang detector for a gate a running holder on record holds ([`producer_gate`]):
/// it waits while a holder on record does work ([`gate::Watch`]), however long that takes, and
/// is refused once this long passes in which none does any it can read. A reclaim holds a root's
/// gate across its inventories of the root and the removal of its stale parts
/// ([`reclaim_selected_on`]), which a loaded Mac can stretch past any wall-clock bound while the
/// reclaim keeps working. The advisory planner ([`plan_on`]) holds it only while it reads the
/// receipt, and lets it go before its inventory.
#[cfg(unix)]
const GATE_QUIET: std::time::Duration = std::time::Duration::from_secs(60);

/// How often [`producer_gate`] looks at a held gate again.
#[cfg(unix)]
const GATE_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Take the producer's terminal exclusive root gate `gate`, the file at `at`, under the admission
/// lock `admission` of `index`, preserving contention and other I/O kinds (GATE-WAIT). A gate
/// found held is waited for while a holder on record runs and is not this process
/// ([`gate::Holders::awaited`]), until `within` passes in which no running holder on record did
/// any work `work` reads of its pid ([`gate::Watch`]). The wait lets the admission lock, which
/// every producer of the index takes, go: every [`GATE_POLL`] it looks at the gate, which it takes
/// for that look alone where the gate is free ([`gate::free`]), and reads its record and its
/// holders' work, and it takes the admission lock again to try the gate only once the gate is
/// free, its record names no such holder, or `within` has passed with no work done. So no other
/// taker of the admission lock waits behind this one's holder or is refused by its wait. A gate
/// held with no running holder on record (its holder recorded nothing, or has ended) is tried
/// once more after one pause, since a holder of this module is off its record for a moment as it
/// takes the gate and as it lets go ([`gate`]), and refused where that holds again. The
/// `WouldBlock` refusal names the cache, the gate's holders as its last read of the record gives
/// them ([`gate::Holders::words`]), how long it waited wherever it paused and, where it was
/// refused for its holder's want of work, for how long none was seen, the gate file, and the
/// processes with that inode open ([`gate_openers`]): the evidence left where a holder recorded
/// nothing.
///
/// The final boolean says whether the admission lock was released while waiting; an
/// index-wide caller must then repeat its census before relying on its completeness.
///
/// # Errors
/// `WouldBlock`, as above; the gate or the admission lock could not be locked or opened again.
#[cfg(unix)]
fn producer_gate(
    index: &Path,
    mut admission: crate::lock::Flock,
    mut gate: std::fs::File,
    at: &Path,
    root: &Path,
    within: std::time::Duration,
    work: &mut dyn FnMut(i32) -> Option<u64>,
) -> std::io::Result<(crate::lock::Flock, gate::Held, bool)> {
    let began = std::time::Instant::now();
    let mut watch = gate::Watch::new();
    let mut paused = false;
    // Whether the last read found the gate held with no running holder on record.
    let mut unrecorded = false;
    loop {
        // A second descriptor on the gate's own inode, for the refusal's scan of who has it open.
        let probe = aterm_uds::kept_out(|| gate.try_clone());
        match gate::try_exclusive(gate) {
            Ok(held) => return Ok((admission, held, paused)),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!(
                        "the managed build cache {} registration gate could not be locked: {error}",
                        root.display()
                    ),
                ));
            }
        }
        let holders = gate::holders(at);
        watch.look(&holders, work);
        if (unrecorded && !holders.awaited()) || watch.quiet() >= within {
            let waited = match (paused, holders.awaited()) {
                (false, _) => String::new(),
                (true, false) => {
                    format!(", after a wait of {:.1} s", began.elapsed().as_secs_f64())
                }
                (true, true) => format!(
                    ", after a wait of {:.1} s, in the last {:.1} s of which no holder on record did any work this process could read",
                    began.elapsed().as_secs_f64(),
                    watch.quiet().as_secs_f64(),
                ),
            };
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "the managed build cache {} is busy: its registration gate is {}{waited}; gate={at:?}; {}",
                    root.display(),
                    holders.words(),
                    gate_openers(probe),
                ),
            ));
        }
        unrecorded = !holders.awaited();
        drop(admission);
        paused = true;
        loop {
            gate::seam::waiting(at);
            std::thread::sleep(GATE_POLL);
            if unrecorded || gate::free(at) {
                break;
            }
            let now = gate::holders(at);
            if !now.awaited() {
                break;
            }
            watch.look(&now, work);
            if watch.quiet() >= within {
                break;
            }
        }
        let again = open_admission(index).ok_or_else(|| {
            std::io::Error::other("the managed build cache's owner could not be recorded")
        })?;
        admission = crate::lock::Flock::lock(again)?;
        gate = gate::open(at)?;
    }
}

/// Descriptor evidence is diagnostic only: a process with the gate open need not hold its
/// lock. The open file keeps the scan on the original inode even when its pathname moves.
/// Preserve incomplete readings; this is never lock or cleanup authority.
#[cfg(unix)]
fn gate_openers(probe: std::io::Result<std::fs::File>) -> String {
    let probe = match probe {
        Ok(probe) => probe,
        Err(why) => return format!("gate descriptor scan unavailable: {why}"),
    };
    match aterm_uds::holders::scan_open_file_holders(&probe, &[]) {
        Ok(scan) => describe_gate_openers(&scan, aterm_uds::holders::exe_of),
        Err(why) => format!("gate descriptor scan unavailable: {why}"),
    }
}

#[cfg(unix)]
fn describe_gate_openers(
    scan: &aterm_uds::holders::HolderScan,
    mut executable: impl FnMut(u32) -> aterm_uds::holders::Exe,
) -> String {
    const SHOWN: usize = 8;
    let mut words = Vec::new();
    for &pid in scan.holders.iter().take(SHOWN) {
        let observer = if pid == std::process::id() {
            "observer; "
        } else {
            ""
        };
        words.push(match executable(pid) {
            aterm_uds::holders::Exe::At(path) => format!("pid {pid} ({observer}{path:?})"),
            aterm_uds::holders::Exe::Unnamed => {
                format!("pid {pid} ({observer}executable unreadable)")
            }
            aterm_uds::holders::Exe::Exited => {
                format!("pid {pid} ({observer}exited during the read)")
            }
        });
    }
    if scan.holders.len() > SHOWN {
        words.push(format!("{} more", scan.holders.len() - SHOWN));
    }
    let open = if words.is_empty() {
        "none observed".into()
    } else {
        words.join(", ")
    };
    format!(
        "processes with the gate open (not proof of lock ownership): {open}; descriptor tables: {} scanned, {} unreadable, {} sealed",
        scan.scanned,
        scan.unreadable.len(),
        scan.sealed.len(),
    )
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
    create_with_policy(home, root, workspace, None, Admission::Adopt).map(|(recorded, _)| recorded)
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Adopt,
    Create,
    Existing,
}

/// Admit a producer under the root's gate. `Existing` extends custody already recorded;
/// `Create` also creates fresh roots, and `Adopt` may claim a proven cargo-made target.
#[cfg(unix)]
fn create_with_policy(
    home: &Path,
    root: &Path,
    workspace: &Path,
    policy: Option<package::Policy>,
    mode: Admission,
) -> std::io::Result<(bool, bool)> {
    let adopt = mode == Admission::Adopt;
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
        if !canonical_existing_ancestor(home, parent) || !workspace_is_owner(home, workspace) {
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
        let admission = open_admission(&index)?;
        let admission = crate::lock::Flock::lock(admission).ok()?;
        let key = key(root)?;
        let path = index.join(format!("{key}.json"));
        let lock_path = gate_path(&path)?;
        let lock = open_gate(&lock_path)?;
        must_record = true;
        // The record may be between fresh creation and publication, so a busy gate is never
        // permission to use the directory without recording this producer's owner.
        let (_admission, _held, _) = match producer_gate(
            &index,
            admission,
            lock,
            &lock_path,
            root,
            GATE_QUIET,
            &mut gate::work,
        ) {
            Ok(held) => held,
            Err(error) => {
                refusal = Some(error);
                return None;
            }
        };
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
        if mode == Admission::Existing && before.is_none() {
            return None;
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
            // A cargo home's cache, or a folder in one, is the cache's, never a build's to own.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && home_cache(home, root) =>
            {
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if mode == Admission::Existing {
                    return None;
                }
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
                        refusal = renumbered(home, &receipt)
                            .map(|now| std::io::Error::other(renumbered_words(&receipt, now)));
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
        Err(refusal.unwrap_or_else(|| {
            std::io::Error::other("the managed build cache's owner could not be recorded")
        }))
    } else {
        Ok((false, false))
    }
}

/// Add a known default-target producer without creating or adopting an ordinary target.
#[cfg(unix)]
fn observe_existing(home: &Path, target: &Path, owner: &Path) -> std::io::Result<bool> {
    if let Some(linked) = through_link(home, target) {
        let admitted = match linked {
            Ok(linked) => admit_linked(home, &linked, owner)?,
            Err(refusal) => Err(refusal),
        };
        return match admitted {
            Ok(()) => Ok(true),
            Err(LinkRefusal::Unowned | LinkRefusal::CargoMade | LinkRefusal::Dangling) => Ok(false),
            Err(refusal) => Err(std::io::Error::other(refusal.why())),
        };
    }
    create_with_policy(home, target, owner, None, Admission::Existing).map(|(recorded, _)| recorded)
}

#[cfg(not(unix))]
fn observe_existing(_: &Path, _: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
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
#[path = "build_cache_registered_link_seam_tests.rs"]
mod link_seam;

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
    if !workspace_is_owner(home, workspace) {
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
    let admission = crate::lock::Flock::lock(admission)?;
    let key = key(root).ok_or_else(stop)?;
    let path = index.join(format!("{key}.json"));
    let gate_at = gate_path(&path).ok_or_else(stop)?;
    let gate = open_gate(&gate_at).ok_or_else(stop)?;
    let (_admission, _held, _) = producer_gate(
        &index,
        admission,
        gate,
        &gate_at,
        root,
        GATE_QUIET,
        &mut gate::work,
    )?;
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

/// Resolve the workspace on every supported driver's reading of its package home. Search
/// boundaries use Targo's logical manifest spelling; custody records canonical directories.
///
/// The call `args` is read from the directory Targo works in ([`from_targo_cwd`]): Targo takes
/// its package home, and with it the stop of its root walk, against that directory (`cli.rs`
/// changes into a `-C` and reloads `homedir` there), so a relative `CARGO_HOME` (or
/// `TARGO_HOME`) is that directory's, never the launcher's. The root is the one Targo finds for
/// `manifest` on each package home ([`super::workspace_root`], the walk a local fallback's
/// `target` is read from, which stops where `find_root_iter` stops), each manifest on the way
/// read by the front's own reader ([`live_read`], whatever reader `machine` holds for its
/// configs) as Targo reads it, through a final link ([`super::config_file`]). The walk inspects
/// actual ancestors outside managed scope to detect an external workspace, but a manifest
/// redirected through a link to a folder outside that ancestry and outside `home` grants no
/// permission to read it.
///
/// `Ok(None)` for a package or a root outside `home`, where atpkg keeps no custody. `Err`
/// where the owner cannot be told ([`workspace_at`]), where the directory Targo works in cannot
/// be told, where a manifest on the way is one Targo refuses, this reader cannot read or is
/// redirected outside that ancestry, and where the root does not hold the package as a member,
/// which Targo refuses too.
fn workspace(
    home: &Path,
    manifest: &Path,
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> Result<Option<PathBuf>, ()> {
    let package = manifest
        .parent()
        .and_then(|package| std::fs::canonicalize(package).ok());
    let read = |path: &Path| -> Result<Option<String>, ()> {
        match std::fs::canonicalize(path) {
            Ok(real) => {
                let directory = real.parent().ok_or(())?;
                if !scope(home, directory)
                    && !package
                        .as_deref()
                        .is_some_and(|package| package.starts_with(directory))
                {
                    return Err(());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(()),
        }
        live_read(path)
    };
    from_targo_cwd(args, tool, home, machine, &|_, moved| {
        let moved = super::Machine {
            read: &read,
            ..*moved
        };
        Some(workspace_at(
            home,
            manifest,
            super::workspace_root(manifest, tool, &moved).as_deref(),
        ))
    })
    .unwrap_or(Err(()))
}

/// Every supported initial-manifest reading must resolve to the same canonical owner.
/// `Ok(None)` only where every reading's owner lies where atpkg keeps no custody; `Err` where
/// any reading's owner cannot be told ([`workspace`]) or two readings disagree.
fn workspace_choices(
    home: &Path,
    manifests: &[PathBuf],
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> Result<Option<PathBuf>, ()> {
    let mut selected = None;
    for manifest in manifests {
        let owner = workspace(home, manifest, args, tool, machine)?;
        if selected.as_ref().is_some_and(|prior| prior != &owner) {
            return Err(());
        }
        selected = Some(owner);
    }
    selected.ok_or(())
}

/// The owner [`workspace`] records for the package whose manifest is `manifest`, given the root
/// Targo finds for it, `root` (`None` where it cannot be told): that root spelled canonically,
/// `Ok(None)` where the package or the root lies outside `home`, `Err` for a file not named as
/// a manifest, a package or root that cannot be resolved, and a root in `home` no owner may be
/// ([`workspace_is_owner`]).
fn workspace_at(home: &Path, manifest: &Path, root: Option<&Path>) -> Result<Option<PathBuf>, ()> {
    let name = manifest.file_name().ok_or(())?;
    if !super::checkout::MANIFESTS
        .iter()
        .any(|manifest| name == std::ffi::OsStr::new(manifest))
    {
        return Err(());
    }
    let package_owner = std::fs::canonicalize(manifest.parent().ok_or(())?).map_err(|_| ())?;
    if !scope(home, &package_owner) {
        return Ok(None);
    }
    let owner = |root: &Path| {
        let root = std::fs::canonicalize(root).map_err(|_| ())?;
        if !scope(home, &root) {
            return Ok(None);
        }
        // Workspace members are loaded as Cargo.toml and checked by manifest identity.
        // Native initial discovery does not make Targo.toml a member of an ancestor's
        // Cargo workspace, even when both files describe the same package directory.
        if root != package_owner && manifest.file_name() != Some("Cargo.toml".as_ref()) {
            return Err(());
        }
        if !workspace_is_owner(home, &root) {
            return Err(());
        }
        Ok(Some(root))
    };
    owner(root.ok_or(())?)
}

/// The same canonical directory and manifest eligibility is required before selecting an
/// owner, creating a root or admitting a producer through a linked root: a folder in `home`,
/// spelled canonically, that holds a manifest ([`holds_manifest`]).
fn workspace_is_owner(home: &Path, workspace: &Path) -> bool {
    scope(home, workspace)
        && std::fs::canonicalize(workspace).ok().as_deref() == Some(workspace)
        && holds_manifest(workspace)
}

/// Whether the folder `workspace` holds a manifest, a regular file at its name or through a
/// final link, as Targo reads one ([`super::config_file`]): a root Targo finds there is one its
/// owner may be. A link stands as the checkout's while it stands ([`super::checkout::gone`]
/// reads the name, not what it names).
fn holds_manifest(workspace: &Path) -> bool {
    super::checkout::MANIFESTS.iter().any(|name| {
        std::fs::metadata(workspace.join(name)).is_ok_and(|metadata| metadata.is_file())
    })
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

/// The target directory `args` name, as `tool` reads them: the last `--target-dir` among
/// Targo's own words ([`super::typed_target_dir`]: `run serve --target-dir out` hands it to
/// `serve`), else `CARGO_TARGET_DIR`, else `CARGO_BUILD_TARGET_DIR`, each spelled `TARGO_` too
/// where `tool` reads twins ([`super::cargo_var`]); `None` for none, or an empty variable, which
/// Targo refuses. An empty typed one is the directory Targo works in (`value_of_path` joins it
/// to it, and builds there).
fn raw_target_spec(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> Option<PathBuf> {
    if let Some(typed) = super::typed_target_dir(args, tool) {
        return Some(PathBuf::from(if typed.is_empty() { "." } else { typed }));
    }
    let target = super::cargo_var("CARGO_TARGET_DIR", tool, machine)
        .or_else(|| super::cargo_var("CARGO_BUILD_TARGET_DIR", tool, machine))
        .map(PathBuf::from)?;
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

/// `path` against `cwd` as the kernel resolves a directory Targo creates there: each `..` the
/// parent of the folder before it as it stands ([`standing`]), through a link (`/v/link/../x` is
/// `x` beside the link's target, as is `/v/link/missing/../x`, the folder Targo makes inside
/// the link's target). A path with no `..` is read as written, its links left to
/// [`through_link`] and [`under_home`]. `None` for a relative path with no `cwd`, or a `..`
/// above the root.
fn physical(path: PathBuf, cwd: Option<&Path>) -> Option<PathBuf> {
    if !path
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return normalize_with_cwd(path, cwd);
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        cwd?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved = standing(&resolved);
                if !resolved.pop() {
                    return None;
                }
            }
            component => resolved.push(component.as_os_str()),
        }
    }
    Some(resolved)
}

/// `path` (absolute, with no `..`) with its deepest folder that stands resolved through its
/// links, the rest appended as written: where the kernel puts it once Targo makes the folders
/// that do not stand yet, each a plain folder inside the one before it.
fn standing(path: &Path) -> PathBuf {
    let mut base = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(mut real) = std::fs::canonicalize(base) {
            real.extend(rest.iter().rev());
            return real;
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `path` (absolute) spelled under `home` where it lies there as the kernel resolves it
/// ([`standing`]): as written, through a link from outside HOME, or through another name of
/// HOME's folder itself (`/System/Volumes/Data/Users/<me>` is the folder `/Users/<me>` is, by
/// device and inode, which also finds HOME's canonical spelling; a platform with neither
/// compares canonical spellings). The custody rules compare spellings under `home`. `None` for a
/// path elsewhere.
fn under_home(path: &Path, home: &Path) -> Option<PathBuf> {
    if path.starts_with(home) {
        return Some(path.to_path_buf());
    }
    let real = standing(path);
    #[cfg(not(unix))]
    {
        let real_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        if let Ok(rest) = real.strip_prefix(&real_home) {
            return Some(home.join(rest));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let id = |path: &Path| std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
        let home_id = id(home)?;
        for ancestor in real.ancestors() {
            if id(ancestor) == Some(home_id) {
                return Some(home.join(real.strip_prefix(ancestor).ok()?));
            }
        }
    }
    None
}

fn target_name(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> Option<PathBuf> {
    physical(raw_target_spec(args, tool, machine)?, machine.cwd)
}

#[cfg(unix)]
fn suppress_unresolved(home: &Path, target: &Path) -> std::io::Result<bool> {
    // A target named through a link to a folder stands for that folder, the link re-checked
    // under each gate held; a link that resolves to none is read by its own name, as before.
    let linked = through_link(home, target).and_then(Result::ok);
    let target = linked
        .as_ref()
        .map_or(target, |linked| linked.real.as_path());
    let candidates = [target.to_path_buf(), super::target_build_path(target)];
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
        let admission = crate::lock::Flock::lock(open_admission(&index).ok_or_else(stop)?)?;
        let at = gate_path(&path).ok_or_else(stop)?;
        let lock = gate::open(&at)?;
        let (_admission, _held, _) = producer_gate(
            &index,
            admission,
            lock,
            &at,
            &root,
            GATE_QUIET,
            &mut gate::work,
        )?;
        if linked.as_ref().is_some_and(|linked| !linked.still()) {
            return Err(std::io::Error::other(LinkRefusal::Changed.why()));
        }
        let mut receipt = read(&path)
            .filter(|receipt| receipt.root == root && same(home, receipt))
            .ok_or_else(stop)?;
        if !receipt.owners_unknown {
            receipt.owners_unknown = true;
            write(&path, &receipt).ok_or_else(stop)?;
        }
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
    let mut admission = crate::lock::Flock::lock(admission)?;
    loop {
        // Snapshot under admission. If a gate wait releases it, the next pass includes
        // registrations or replacements made during that wait.
        let entries = std::fs::read_dir(&directory)?
            .take(MAX_ROOTS + 1)
            .collect::<std::io::Result<Vec<_>>>()?;
        if entries.len() > MAX_ROOTS {
            return Err(stop());
        }
        let mut released = false;
        for entry in entries {
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                continue;
            }
            let root = match read(&path) {
                Some(receipt) => receipt.root,
                None if std::fs::symlink_metadata(&path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                None => return Err(stop()),
            };
            let at = gate_path(&path).ok_or_else(stop)?;
            let lock = gate::open(&at)?;
            let (again, _held, waited) = producer_gate(
                &directory,
                admission,
                lock,
                &at,
                &root,
                GATE_QUIET,
                &mut gate::work,
            )?;
            admission = again;
            released |= waited;
            let mut receipt = read(&path).ok_or_else(stop)?;
            if !receipt.owners_unknown {
                receipt.owners_unknown = true;
                write(&path, &receipt).ok_or_else(stop)?;
            }
        }
        if !released {
            return Ok(());
        }
    }
}

#[cfg(not(unix))]
fn suppress_configured(_: &Path) -> std::io::Result<()> {
    Ok(())
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
/// keeps. Adoption never counts those facts alone: it also needs, beside them, a profile a
/// compilation prepared with a unit's invocation fingerprint, or one holding the build lock
/// `Layout::new` lays, with nothing a build prepares or every folder a compilation's `prepare`
/// laid, and no unit's folder in any profile, as a `clean -p` leaves it, before any build or after
/// one (`adoption::built`); no such verb lays that lock or a profile. Which verbs run without a
/// lane is [`crate::reroute::LANE_FREE_QUERIES`], a separate rule. No alias or external
/// `cargo-<verb>` can take a builtin's name: Targo and cargo run the builtin.
const RESOLVING_VERBS: &[&str] = &["fetch", "tree", "metadata", "generate-lockfile", "update"];

/// WHETHER A RESOLVING VERB'S LOCKFILE MAY LAND UNDER A TARGET OR BUILD DIRECTORY, which Targo then
/// makes, `CACHEDIR.TAG` and all (`ops/lockfile.rs`): an embedded manifest, whose lock root is in
/// the build directory, or a `resolver.lockfile-path`, wherever the call can name one. Targo
/// reads a manifest path (`--manifest-path` or `-m`) as embedded when it ends in `.rs` or has no
/// extension (`workspace/parser/mod.rs`'s `is_embedded`). A lockfile path is set by a `--config`
/// value, or by the config file it names when something stands at that path, as Targo reads it,
/// by `CARGO_RESOLVER_LOCKFILE_PATH` or its `TARGO_` twin, by the configs of a `-C` directory (in
/// a cluster too: `-qC dir`), by
/// the deprecated `--lockfile-path`, or by a config Targo reads ([`configs_name`]).
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
            _ if flag.starts_with("-C")
                || super::shorts(flag).is_some_and(|cluster| cluster.value_of('C').is_some()) =>
            {
                return true;
            }
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
            cli_config_names(machine, value, &names_lock_root)
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
        || configs_name(machine, &names_lock_root)
}

/// Whether config text, a file's or a `--config` value's, sets `resolver.lockfile-path` or an
/// `include`. It is read as TOML 1.1, as Targo parses it ([`super::config_toml`]), so a quoted
/// key's escapes spell what Targo reads. Text this reader cannot parse (Targo refuses it too)
/// may still set either unless it holds neither word and no escape that could spell one
/// ([`super::spells_by_escape`]).
fn names_lock_root(text: &str) -> bool {
    match super::config_toml(text) {
        Some(config) => {
            config.get("include").is_some()
                || config
                    .get("resolver")
                    .and_then(|resolver| resolver.get("lockfile-path"))
                    .is_some()
        }
        None => {
            text.contains("lockfile") || text.contains("include") || super::spells_by_escape(text)
        }
    }
}

/// Whether the `--config` value `value` holds what `names` finds in config text: the config file
/// it names when something stands at that path, as Targo reads it, and the value as a dotted key
/// otherwise (`context/mod.rs`'s `load_cli_args`); one that cannot be read may.
fn cli_config_names(
    machine: &super::Machine<'_>,
    value: &str,
    names: &dyn Fn(&str) -> bool,
) -> bool {
    match machine
        .cwd
        .map(|cwd| super::config_file(&cwd.join(value), machine))
    {
        None | Some(Err(())) => true,
        Some(Ok(Some(text))) => names(&text),
        Some(Ok(None)) => names(value),
    }
}

/// Whether a config Targo reads for this call holds what `names` finds in config text (a lock
/// root, [`names_lock_root`]) or cannot be read: each
/// `.cargo/config.toml` and `.cargo/config` from the working directory up as far as Targo walks
/// ([`super::config_walk`]), then each package
/// home's ([`super::package_homes`]): `CARGO_HOME` against the working directory, or `~/.cargo`
/// where it is unset or empty, and a `TARGO_HOME` where `CARGO_HOME` is unset, since Targo
/// 16387 takes its home from the first and Trust's main from the second. They are read as
/// Targo's, whatever the tool: reroute observes Trust's own cargo as an upstream one, and a
/// config read in excess only reads the call more warily.
fn configs_name(machine: &super::Machine<'_>, names: &dyn Fn(&str) -> bool) -> bool {
    let Some(cwd) = machine.cwd else {
        return true;
    };
    super::config_walk(cwd, machine)
        .into_iter()
        .chain(super::package_homes(super::Tool::Targo, machine))
        .flat_map(|dir| ["config.toml", "config"].map(|name| dir.join(name)))
        .any(|file| match super::config_file(&file, machine) {
            Err(()) => true,
            Ok(None) => false,
            Ok(Some(text)) => names(&text),
        })
}

/// The outputs and manifests `args` selected ([`super::selected_outputs`]), read from the
/// directory Targo works in ([`targo_cwd`]). Placement still reads the original words: this
/// observation never rewrites the compiler's argv or invents a build-directory plan, and unknown
/// directories and configs grant no ownership. With a `-C` before the verb, Targo changes into
/// its directory before it reads a config, an alias, a relative path or a relative package home
/// (`cli.rs`), then runs the same call without it: so this reads the words without the `-C`
/// ([`super::without_directory`]) on `machine` moved there, spelled under `home` where it lies
/// there ([`under_home`]), as [`requires_managed_explicit`] judges what the call owes. That
/// spelling is Targo's own config walk: [`targo_cwd`] reads the directory back as Targo's
/// `getcwd` does, HOME's spelling for a folder in it; and under a HOME that is not its own
/// canonical spelling nothing is granted ([`workspace`]'s scope,
/// `create_with_policy`'s canonical ancestor). `None` for a `-C` this reader cannot resolve, a
/// second one included, which Targo refuses, and for one Targo refuses for want of its gate
/// ([`super::directory_refused`]: no `-Zunstable-options` clap keeps, or a channel that allows
/// no nightly feature), where it builds nothing; the lint's words are its own, so a `-C` among
/// them grants nothing.
fn selected_call_outputs(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> Option<super::SelectedOutputs> {
    from_targo_cwd(args, tool, home, machine, &|args, machine| {
        super::selected_outputs(args, tool, machine, &|verb| {
            user_alias(verb, args, tool, machine)
        })
    })
}

/// The `target` Targo builds `args` into where nothing names another
/// ([`super::workspace_target`]), read from the directory Targo works in as [`fallback_chosen`]
/// reads the fallback.
fn workspace_target_chosen(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> Option<super::WorkspaceTarget> {
    from_targo_cwd(args, tool, home, machine, &|args, machine| {
        super::workspace_target(args, tool, machine, &|verb| {
            user_alias(verb, args, tool, machine)
        })
    })
}

/// The manifests of the local fallback `args` chose ([`super::local_fallback_manifests`]) where
/// `selected`, the placement the call was handed, is that fallback, read from the directory
/// Targo works in as [`selected_call_outputs`] reads its outputs ([`from_targo_cwd`]): with a
/// `-C` before the verb, the words without it on `machine` moved there, its inherited `$PWD`
/// (the launcher's) hidden, as [`super::decide`] judged the placement from that directory.
fn fallback_chosen(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    selected: &super::BuildDir,
) -> Option<Vec<PathBuf>> {
    from_targo_cwd(args, tool, home, machine, &|args, machine| {
        super::local_fallback_manifests(args, tool, machine, selected, &|verb| {
            user_alias(verb, args, tool, machine)
        })
    })
}

/// `read` given the call `args` as Targo runs it from the directory it works in: `args` on
/// `machine` where no `-C` stands before the verb (or the lint's words are read, which are its
/// own), else the words without it ([`super::without_directory`]) on `machine` moved to that
/// directory ([`targo_cwd`], spelled under `home` where it lies there, [`under_home`]) with its
/// `$PWD`, the launcher's, hidden. `None` for a `-C` this reader cannot resolve and for one
/// Targo refuses for want of its gate ([`super::directory_refused`]), where it builds nothing.
/// An upstream cargo whose channel cannot be told is read from that directory: it refuses the
/// `-C` before it reads anything, or runs there.
fn from_targo_cwd<R>(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    read: &dyn Fn(&[String], &super::Machine<'_>) -> Option<R>,
) -> Option<R> {
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
    let typed = &args[at..];
    if tool == super::Tool::Tippy || super::directory_arg(typed).is_none() {
        return read(args, machine);
    }
    if super::directory_refused(args, tool, machine) {
        return None;
    }
    let cwd = targo_cwd(typed, machine).ok().flatten()?;
    let cwd = under_home(&cwd, home).unwrap_or(cwd);
    let mut moved_args = args[..at].to_vec();
    moved_args.extend(super::without_directory(typed));
    let env = |name: &str| {
        if name == "PWD" {
            None
        } else {
            (machine.env)(name)
        }
    };
    let moved = super::Machine {
        cwd: Some(&cwd),
        env: &env,
        ..*machine
    };
    read(&moved_args, &moved)
}

/// Whether this reader cannot tell which package home the call `args` reads: one by the cargo
/// name (an upstream cargo's or Trust's Cargo-compatible name's, [`super::Tool::is_cargo`]) that
/// may honour a `-C` (one [`super::directory_refused`] does not refuse), with a relative `CARGO_HOME` that names one
/// home against the launcher's folder and another against that directory ([`super::package_homes`]
/// on each). Targo run directly takes the `-C` directory's (`reload_cwd` recomputes `homedir`
/// there); a rustup proxy, which a call by the cargo name may go through, exports to the cargo it
/// runs the `CARGO_HOME` it resolved against the launcher's folder, absolute, which Targo then
/// takes as it stands. Nothing in the call says which runs it, and each home's config, and the
/// stop of the workspace's root walk, may name another target. A call by Targo's name, or the
/// lint's, is read as Targo runs it.
fn home_unsettled(args: &[String], tool: super::Tool, machine: &super::Machine<'_>) -> bool {
    if !tool.is_cargo() {
        return false;
    }
    let at = usize::from(args.first().is_some_and(|arg| arg.starts_with('+')));
    let typed = &args[at..];
    let relative = (machine.env)("CARGO_HOME")
        .is_some_and(|value| !value.is_empty() && Path::new(&value).is_relative());
    if !relative
        || super::directory_arg(typed).is_none()
        || super::directory_refused(args, tool, machine)
    {
        return false;
    }
    let Ok(Some(moved)) = targo_cwd(typed, machine) else {
        return false;
    };
    let homes = |cwd: &Path| {
        super::package_homes(
            tool,
            &super::Machine {
                cwd: Some(cwd),
                ..*machine
            },
        )
    };
    machine
        .cwd
        .is_some_and(|launcher| homes(launcher) != homes(&moved))
}

/// The directory Targo works in for `args`, the words typed: the one a `-C` before the verb
/// names ([`super::directory_arg`]), read back as Targo reads it once it has changed into it
/// from the launcher's ([`super::changed_into`], `cli.rs`: a link's `..` is its target's parent,
/// a firmlinked folder its root spelling), or else the launcher's. `Err` for a `-C` whose
/// directory Targo cannot change into.
fn targo_cwd(args: &[String], machine: &super::Machine<'_>) -> Result<Option<PathBuf>, ()> {
    match super::directory_arg(args) {
        None => Ok(machine.cwd.map(Path::to_path_buf)),
        Some(Some(dir)) if !dir.is_empty() => {
            let dir = Path::new(dir);
            let dir = if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                machine.cwd.ok_or(())?.join(dir)
            };
            super::changed_into(&dir).map(Some).ok_or(())
        }
        Some(_) => Err(()),
    }
}

/// `f` given what `cargo-miri` reads for `args`, the words typed, besides its own words
/// ([`super::Miri`]): `machine`'s variables, and `cargo metadata`'s target
/// ([`super::miri_metadata_target`]) as Targo runs it, from the directory Targo works in
/// ([`targo_cwd`], spelled under HOME where it lies there, as Targo's `getcwd` reads it back);
/// none where that directory cannot be resolved, which Targo cannot change into either.
fn with_miri<R>(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
    f: impl FnOnce(&super::Miri<'_>) -> R,
) -> R {
    let cwd = targo_cwd(args, machine).ok().flatten().map(|dir| {
        machine
            .home
            .and_then(|home| under_home(&dir, home))
            .unwrap_or(dir)
    });
    let metadata_target = |words: &[String]| {
        let cwd = cwd.as_deref()?;
        super::miri_metadata_target(
            words,
            tool,
            &super::Machine {
                cwd: Some(cwd),
                ..*machine
            },
        )
    };
    f(&super::Miri {
        env: machine.env,
        metadata_target: &metadata_target,
    })
}

/// Whether `cargo-miri`'s sysroot setup ([`super::MiriCall::setup`]) for `args`, the words typed,
/// writes nothing atpkg could record or must protect. Setup (`setup.rs`) runs a Targo of its own
/// (`-Zjson-target-spec`, `RUSTC` its own) on the standard library's sources through
/// `rustc-build-sysroot` (`build_from_source`, read at 0.5.14, the version `cargo-miri`'s lock
/// pins): in a fresh workspace `tempfile` makes in the temporary folder (`Builder::tempdir`;
/// `TMPDIR`, an empty or relative one against the directory Targo works in, as tempfile 3.27's
/// `create_helper` joins it), with `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` both that
/// workspace's `target`, which outrank every inherited variable, its `TARGO_` twin and every
/// config, so no target or build directory the call names moves it. Beside the package home,
/// into which that Targo fetches the standard library's registry dependencies (no target and no
/// intermediates), its writes outside that workspace are in the sysroot folder (`MIRI_SYSROOT`,
/// read the same way, else miri's cache folder, `~/Library/Caches/org.rust-lang.miri`): before
/// that build it makes the folder's `lib/rustlib` and moves an earlier `lib/rustlib/<target>`
/// aside into a temporary folder in it, deleted once the setup ends; after the build it stages
/// the result in another temporary folder in it and renames that to `lib/rustlib/<target>`. A
/// link at `lib` or `lib/rustlib` moves those writes to the folder it names, so the sysroot's
/// `lib/rustlib` is judged as the sysroot is, each folder resolved as the kernel resolves it
/// ([`standing`], then spelled under HOME, [`under_home`]), whatever its spelling, one under a
/// folder of HOME atpkg never looks into included; a link at `lib/rustlib/<target>` is never written through (each rename that meets it
/// fails). That Targo works where the call's does and is handed no `--config`. So setup is
/// clear only where each of those folders lies outside every folder atpkg manages ([`scope`],
/// HOME itself included, every spelling through [`under_home`]) and outside the shared cache's
/// `~/Library/Caches/aterm`; where no variable names an artifact directory
/// ([`super::ARTIFACT_DIR_ENV`], or its twin), into which that build would copy its output; and
/// where no config Targo reads walking up from where it works names one or cannot be read
/// ([`super::configs_name_artifacts`]). An unset `TMPDIR` is the platform's temporary folder
/// (std's `temp_dir`: on macOS the per-user one `confstr` names), outside HOME. A `-C` this
/// reader cannot resolve leaves it unclear.
fn miri_setup_clear(
    home: &Path,
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> bool {
    let Ok(Some(cwd)) = targo_cwd(args, machine) else {
        return false;
    };
    let cwd = under_home(&cwd, home).unwrap_or(cwd);
    if super::cargo_var(super::ARTIFACT_DIR_ENV, tool, machine).is_some() {
        return false;
    }
    let at = super::Machine {
        cwd: Some(&cwd),
        ..*machine
    };
    let base = |name: &str| (machine.env)(name).map(|value| cwd.join(value));
    let sysroot =
        base("MIRI_SYSROOT").unwrap_or_else(|| home.join("Library/Caches/org.rust-lang.miri"));
    let shared = home.join("Library/Caches/aterm");
    let rustlib = sysroot.join("lib/rustlib");
    for folder in base("TMPDIR").into_iter().chain([sysroot, rustlib]) {
        // As the kernel resolves it, whatever its spelling: one under a folder of HOME atpkg
        // never looks into may still be, or hold, a link into one it manages.
        let Some(resolved) = physical(folder, Some(&cwd)).map(|folder| standing(&folder)) else {
            return false;
        };
        let spelled = under_home(&resolved, home).unwrap_or(resolved);
        if spelled == home || scope(home, &spelled) || spelled.starts_with(&shared) {
            return false;
        }
    }
    !super::configs_name_artifacts(&cwd, tool, &at)
}

/// Whether the call `args`, the words typed, builds `cargo-miri`'s sysroot
/// ([`super::miri_call`]'s `setup`: a `miri setup`, a verb it hands back with no `MIRI_SYSROOT`,
/// or an alias the reader cannot tell where the call runs in the lane) somewhere
/// [`miri_setup_clear`] does not clear.
fn miri_setup_unclear(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> bool {
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
    let alias = |verb: &str| user_alias(verb, args, tool, machine);
    with_miri(&args[at..], tool, machine, |miri| {
        super::miri_call(&args[at..], tool, &alias, miri).setup
    }) && !miri_setup_clear(home, &args[at..], tool, machine)
}

/// One definition of an alias in a config file ([`user_alias`]): a string's words, an array's,
/// or words this reader cannot tell.
#[derive(Clone)]
enum AliasDefinition {
    Words(Vec<String>),
    List(Vec<String>),
    Unknown,
}

/// The definitions of the alias at the key path `parts` (under `alias`) that the config file
/// `file` gives, highest priority first, as Targo loads it (`context/mod.rs`'s `_load_file` and
/// `load_includes`): its own value, then each file its `include` names, the last one first (a
/// later include's value replaces an earlier one's, the file's own replaces all of them), each
/// read the same way. The text is read as TOML 1.1, as Targo parses it ([`super::config_toml`]),
/// and the includes by the walk's own reader ([`super::config_includes`]): a string or a
/// `{ path, optional }` table (any other key ignored, as Targo ignores it), the path relative to
/// `file`'s folder, folded as written ([`super::include_file`]); an optional one where nothing
/// stands is skipped.
/// `None` where nothing stands at `file`. A definition this reader cannot tell
/// ([`AliasDefinition::Unknown`]) where Targo refuses the config (a file it cannot read or whose
/// `include` is malformed: no list, an entry of another type, a path that is no `.toml`, holds a
/// glob, a brace or a control character, names nothing though not optional, or that `seen`
/// already loaded, its cycle), or in text this reader cannot parse (Targo refuses it too) that
/// may spell one (holding `alias`, `include` or an escape that could spell either,
/// [`super::spells_by_escape`]).
/// `seen` collects every file this file's walk loads, as Targo's does for each config file it
/// finds (`load_file` starts a fresh set), and so bounds the walk as it bounds Targo's: each
/// file loads once, and paths that differ only by looping through a link end at the kernel's
/// link limit, a read that fails. Where `follow` is false (an upstream
/// cargo, which reads `include` only from 1.93, before that under `-Zconfig-include` alone), a
/// file with an `include` is one this reader cannot tell.
fn alias_definitions(
    machine: &super::Machine<'_>,
    file: &Path,
    parts: &[&str],
    follow: bool,
    seen: &mut Vec<PathBuf>,
) -> Option<Vec<AliasDefinition>> {
    use AliasDefinition::Unknown;
    let text = match super::config_file(file, machine) {
        Err(()) => return Some(vec![Unknown]),
        Ok(None) => return None,
        Ok(Some(text)) => text,
    };
    if seen.iter().any(|loaded| loaded == file) {
        return Some(vec![Unknown]);
    }
    seen.push(file.to_path_buf());
    let config = match super::config_toml(&text) {
        Some(config) => config,
        None => {
            let may = text.contains("alias")
                || text.contains("include")
                || super::spells_by_escape(&text);
            return Some(if may { vec![Unknown] } else { Vec::new() });
        }
    };
    let mut found: Vec<AliasDefinition> = alias_definition(&config, parts).into_iter().collect();
    if config.get("include").is_none() {
        return Some(found);
    }
    if !follow {
        return Some(vec![Unknown]);
    }
    let Ok(includes) = super::config_includes(&config) else {
        return Some(vec![Unknown]);
    };
    let base = file.parent().unwrap_or(Path::new("/"));
    let mut included = Vec::new();
    for include in &includes {
        let full = super::include_file(base, include);
        match alias_definitions(machine, &full, parts, follow, seen) {
            Some(definitions) => included.push(definitions),
            None if include.optional => {}
            None => return Some(vec![Unknown]),
        }
    }
    found.extend(included.into_iter().rev().flatten());
    Some(found)
}

/// The alias at the key path `parts` (under `alias`) that one parsed config defines itself:
/// a string's words, split at whitespace, or an array's strings; [`AliasDefinition::Unknown`]
/// for a key whose path meets a value that is no table, or a value of another type.
fn alias_definition(config: &aterm_toml::Value, parts: &[&str]) -> Option<AliasDefinition> {
    let mut value = config.get("alias")?;
    for part in parts {
        let Some(table) = value.as_table() else {
            return Some(AliasDefinition::Unknown);
        };
        value = table.get(part)?;
    }
    Some(match value {
        aterm_toml::Value::String(words) => {
            AliasDefinition::Words(words.split_whitespace().map(str::to_owned).collect())
        }
        aterm_toml::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .map_or(AliasDefinition::Unknown, AliasDefinition::List),
        _ => AliasDefinition::Unknown,
    })
}

/// The user alias Targo expands the name `verb` into for this call, `args` the words typed
/// ([`super::UserAlias`]; [`super::targo_reading`] asks only of a name no builtin command has),
/// as `main.rs`'s `aliased_command` reads the key `alias.<verb>`, split at each `.` as Targo
/// splits a key (`context/key.rs`): the variable `CARGO_ALIAS_<PARTS>` (each part upper case, `-`
/// as `_`, joined by `_`) or its `TARGO_` twin, whose UTF-8 text is words split at whitespace
/// whatever they spell (Targo reads it as a string: `CARGO_ALIAS_X=1` runs `1`); else that key's
/// definitions in the configs Targo reads ([`alias_definitions`], their includes with them, each
/// config file found with a cycle check of its own, an upstream cargo's includes not followed), a
/// string's words split the same way or an array's strings. Those configs are each
/// `.cargo/config` (or, where none stands, `.cargo/config.toml`, as Targo picks) from the
/// directory Targo works in ([`targo_cwd`]: `cli.rs` changes into a `-C`'s before it expands an
/// alias) up as far as Targo walks ([`super::config_walk`]), then a package home's ([`super::package_homes`], resolved against that directory,
/// as Targo's `reload_cwd` resolves it, unless it is an ancestor's, compared through links as
/// Targo's `walk_tree` compares them). Targo expands an alias before it merges a `--config`, so
/// none defines one. The variable wins over a config's string and the nearest config's string
/// over a farther one's, as in Targo. A variable that is no UTF-8 text is one Targo does not see
/// (`get_str`): a config's string is read then. Where this reader cannot tell, the alias is
/// [`super::UserAlias::Unknown`]: no working directory, or a `-C` it cannot resolve; a variable
/// that is no UTF-8 text with no config string to fall back on, or a twin read by a tool
/// observed as an upstream cargo (Trust's own cargo reads it); a definition
/// [`alias_definitions`] cannot tell; an array beside any other definition (Targo joins them);
/// package homes whose readings differ (Targo 16387 reads `CARGO_HOME`'s, Trust's main
/// `TARGO_HOME`'s); and an alias for a name Targo may run as a manifest instead (a path, or a
/// `.rs` file: `run.rs`'s `is_manifest_command`, which `-Zscript` keeps unexpanded).
fn user_alias(
    verb: &str,
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> super::UserAlias {
    use super::UserAlias;
    use AliasDefinition as Defined;
    /// The alias's variable, as Targo reads it.
    enum Variable {
        /// None is set.
        Unset,
        /// Its text's words.
        Words(Vec<String>),
        /// One is set that is no UTF-8 text, which Targo does not see.
        NoText,
        /// A twin a tool reads that this reader observes as one that does not.
        Unknown,
    }
    let parts: Vec<&str> = verb.split('.').collect();
    let key = parts
        .iter()
        .map(|part| part.replace('-', "_").to_uppercase())
        .collect::<Vec<_>>()
        .join("_");
    let text = |value: std::ffi::OsString| match value.into_string() {
        Ok(text) => Variable::Words(text.split_whitespace().map(str::to_owned).collect()),
        Err(_) => Variable::NoText,
    };
    let variable = match (machine.env)(&format!("CARGO_ALIAS_{key}")) {
        Some(value) => text(value),
        None => match (machine.env)(&format!("TARGO_ALIAS_{key}")) {
            Some(value) if super::reads_twins(tool) => text(value),
            Some(_) => Variable::Unknown,
            None => Variable::Unset,
        },
    };
    let Ok(Some(cwd)) = targo_cwd(args, machine) else {
        return UserAlias::Unknown;
    };
    let follow = tool != super::Tool::Cargo;
    let dir_definitions = |dir: &Path| {
        alias_definitions(
            machine,
            &dir.join("config"),
            &parts,
            follow,
            &mut Vec::new(),
        )
        .or_else(|| {
            let file = dir.join("config.toml");
            alias_definitions(machine, &file, &parts, follow, &mut Vec::new())
        })
        .unwrap_or_default()
    };
    let ancestors = super::config_walk(&cwd, machine);
    let canonical = |dir: &Path| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let near: Vec<Defined> = ancestors
        .iter()
        .flat_map(|dir| dir_definitions(dir))
        .collect();
    let reading = |home: Option<&PathBuf>| {
        let far = home
            .filter(|home| {
                let home = canonical(home);
                !ancestors.iter().any(|dir| canonical(dir) == home)
            })
            .map(|home| dir_definitions(home))
            .unwrap_or_default();
        let found: Vec<&Defined> = near.iter().chain(&far).collect();
        let strings = found.iter().all(|found| matches!(found, Defined::Words(_)));
        match (&variable, found.as_slice()) {
            (Variable::Unset, []) => UserAlias::None,
            (Variable::Words(words), _) if strings => UserAlias::Words(words.clone()),
            (Variable::Unset | Variable::NoText, [Defined::Words(words), ..]) if strings => {
                UserAlias::Words(words.clone())
            }
            (Variable::Unset, [Defined::List(words)]) => UserAlias::Words(words.clone()),
            _ => UserAlias::Unknown,
        }
    };
    let homes = super::package_homes(
        super::Tool::Targo,
        &super::Machine {
            cwd: Some(&cwd),
            ..*machine
        },
    );
    let readings: Vec<UserAlias> = if homes.len() > 1 {
        homes.iter().map(|home| reading(Some(home))).collect()
    } else {
        vec![reading(homes.first())]
    };
    let first = readings[0].clone();
    let path = Path::new(verb);
    let manifest_like = path.components().count() > 1
        || path.extension().is_some_and(|extension| extension == "rs");
    if readings.iter().any(|reading| *reading != first)
        || (manifest_like && first != UserAlias::None)
    {
        UserAlias::Unknown
    } else {
        first
    }
}

/// Known builtin commands that produce no build output need no owner observation: the
/// [`READING_VERBS`], the [`RESOLVING_VERBS`] whose lockfile stays out of every target, help and
/// clean, as the verb of the line Targo runs ([`super::targo_line`]: `h = "clean"` cleans),
/// Targo's `miri clean`, which removes and builds nothing, and its `miri setup` where it builds
/// nothing or builds where nothing atpkg records or protects lies ([`miri_setup_unclear`]; with
/// no HOME, only where Targo refuses it for want of its lane). Clean must reach Cargo even for an
/// unregistered target, without adopting it. A command name used as
/// an option value, an external alias or lint argument proves none of this, nor does a line whose
/// alias cannot be told; a free verb still runs through every other rule of the shim's front. A
/// version query is [`targo_query`]'s.
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
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
    let alias = |verb: &str| user_alias(verb, args, tool, machine);
    let Some(line) = with_miri(&args[at..], tool, machine, |miri| {
        super::targo_line(&args[at..], tool, &alias, miri)
    }) else {
        return false;
    };
    let mut words = line.iter().map(String::as_str);
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
            _ if word.starts_with("--config=")
                || word.starts_with("--color=")
                || word.starts_with("--manifest-path=")
                || word.starts_with("--target-dir=")
                || word.starts_with("--explain=")
                || word.starts_with("-Z")
                || word.starts_with("-C") => {}
            // A cluster of flags (`-qv`), or one ending in a `-C` or `-Z` (`-qC dir`, `-vZx`),
            // whose value is skipped as the spaced spelling's is.
            _ if super::shorts(word)
                .is_some_and(|cluster| matches!(cluster.valued, None | Some(('C' | 'Z', _)))) =>
            {
                let detached = super::shorts(word)
                    .is_some_and(|cluster| matches!(cluster.valued, Some((_, None))));
                if detached && words.next().is_none() {
                    return false;
                }
            }
            // Targo's `miri` is its own `targo-miri`, whose subcommand is the word after its name
            // (`phase_cargo_miri`): `clean` runs `targo metadata --no-deps` and removes what it
            // names (`clean_target_dir`, `clean_sysroot`), and builds nothing, as `clean` does.
            "miri" if tool == super::Tool::Targo => {
                return match words.next() {
                    Some("clean") => true,
                    // `setup` builds only miri's sysroot, with a Targo of its own: free where it
                    // builds nothing (help, or no lane, which Targo refuses), or where that
                    // build writes nothing atpkg could record or must protect.
                    Some("setup") => machine.home.map_or_else(
                        || !super::in_lane(&args[at..], tool, machine.env),
                        |home| !miri_setup_unclear(args, tool, home, machine),
                    ),
                    _ => false,
                };
            }
            _ => {
                return matches!(word, "help" | "clean")
                    || READING_VERBS.contains(&word)
                    || (RESOLVING_VERBS.contains(&word)
                        // Older Cargo versions' update --breaking/-b rewrites workspace
                        // manifests. Only ordinary lock updates take this path, even
                        // when a current driver no longer accepts that older flag.
                        && !(word == "update"
                            && line.iter().any(|arg| {
                                arg == "--breaking"
                                    || arg.starts_with("--breaking=")
                                    || super::shorts(arg)
                                        .is_some_and(|cluster| cluster.flags.contains('b'))
                            }))
                        && !lockfile_may_land_in_a_target(&line, machine));
            }
        }
    }
    false
}

/// Whether Targo answers `args` with help or its version and builds nothing, as it reads the
/// line it runs ([`super::targo_reading`]): `-V b` builds, as does `xtask --help` where `xtask
/// = "run -p xtask --"`. A line whose alias cannot be told is no query.
fn targo_query(args: &[String], tool: super::Tool, machine: &super::Machine<'_>) -> bool {
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
    let alias = |verb: &str| user_alias(verb, args, tool, machine);
    with_miri(&args[at..], tool, machine, |miri| {
        super::targo_reading(&args[at..], tool, &alias, miri)
    })
    .is_some_and(|reading| reading.query)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observation {
    Unmanaged,
    Protected,
    Owned,
    /// Only the quiet intermediate directory is owned. Final artifacts in the explicit
    /// target remain unmanaged, so this cannot acknowledge a managed producer's output.
    IntermediatesOwned,
    /// A managed package selected its workspace's default final output, but complete
    /// output custody was not established. The producer must not forget that final output
    /// merely because its separately selected intermediate lies outside managed scope.
    DefaultOutputsUnowned,
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
    observe_with_placement(args, tool, home, machine, None)
}

fn observe_with_placement(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    selected: Option<&super::BuildDir>,
) -> std::io::Result<Observation> {
    if non_producing_command(args, tool, machine) || targo_query(args, tool, machine) {
        return Ok(Observation::Unmanaged);
    }
    // The shim's call is observed before its build runs, so the folders the index shares with
    // the shared cache are made private here, never by the build with its umask.
    lay_shared(home);
    if let Some(typed) = equivalent_placement_args(args, tool, machine) {
        let env = |name: &str| without_build_dir(machine, name);
        return observe_with_placement(
            &typed,
            tool,
            home,
            &super::Machine {
                env: &env,
                ..*machine
            },
            selected,
        );
    }
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
    // A `+toolchain` is no word `-C` is read from ([`super::directory_arg`]).
    let alias = |verb: &str| user_alias(verb, args, tool, machine);
    // The placement this call was handed, else the one it would get. A handed placement is never
    // decided again: admission makes the cache's parents, after which a fresh decision may name
    // the cache instead of the workspace fallback chosen in a home that had none.
    let placement = selected
        .cloned()
        .or_else(|| super::decide(args, tool, machine));
    let chosen = selected_call_outputs(args, tool, home, machine)
        .map(|chosen| Ok((chosen, false)))
        .or_else(|| {
            // Under the root Targo finds, where atpkg manages the package's workspace; a fallback
            // there whose owner cannot be told is refused, its intermediates never prepared. A
            // root atpkg never manages (HOME's) leaves its `target` held in custody below, as
            // any call naming no target holds its root's.
            let manifest = fallback_chosen(args, tool, home, machine, placement.as_ref()?)?;
            let owner = match workspace_choices(home, &manifest, args, tool, machine) {
                Ok(owner) => owner?,
                Err(()) => {
                    return Some(Err(std::io::Error::other(
                        "the workspace that owns the local fallback could not be read; no build started",
                    )));
                }
            };
            Some(Ok((
                super::SelectedOutputs {
                    target: super::FinalTarget::Explicit(owner.join("target")),
                    manifests: manifest,
                    within: None,
                    intermediates: None,
                },
                true,
            )))
        })
        .transpose()?;
    let Some((chosen, local_fallback)) = chosen else {
        // The line Targo runs names the target and the directories; where its alias cannot be
        // told, the words typed name the target, and any registered root may be the one chosen.
        let typed = &args[at..];
        // A line `miri` hands back whose target this reader cannot tell may build into any of them.
        let (line, unread, base) = with_miri(typed, tool, machine, |miri| {
            let call = super::miri_call(typed, tool, &alias, miri);
            (
                super::targo_line(typed, tool, &alias, miri),
                call.target_unread,
                call.base,
            )
        });
        let named = line.as_deref().unwrap_or(typed);
        // Judged from the directory Targo works in, as [`requires_managed_explicit`] judges it:
        // a `-C` this reader resolves is that directory, its words dropped; one it cannot stays
        // in the line, which leaves every directory unresolved.
        let (named, cwd) = match targo_cwd(typed, machine) {
            Ok(cwd) => (
                super::without_directory(named),
                cwd.map(|dir| under_home(&dir, home).unwrap_or(dir)),
            ),
            Err(()) => (named.to_vec(), machine.cwd.map(Path::to_path_buf)),
        };
        let moved = super::Machine {
            cwd: cwd.as_deref(),
            ..*machine
        };
        // A target is HOME's where the kernel reaches it there, whatever its spelling
        // ([`under_home`]: through HOME's other name or a link from outside it).
        // With none named, a managed workspace joins its recorded default target's owners
        // without claiming an ordinary target. A root atpkg never manages
        // (HOME's) records no owner there, so a root recorded at that folder for another
        // workspace is held in custody, as one this call named explicitly would be, never
        // retired once that workspace goes while this one still builds there.
        // A `miri` line builds inside the folder whose `miri` its target is (`base`): a root
        // recorded at that folder is held too, as the same call without `miri` would hold it.
        let mut owned_default = false;
        let protected = match target_name(&named, tool, &moved) {
            Some(target) => {
                let within = target
                    .parent()
                    .filter(|_| base.is_some() && target.file_name() == Some("miri".as_ref()))
                    .map(Path::to_path_buf);
                let mut protected = false;
                for output in std::iter::once(target).chain(within) {
                    let output = under_home(&output, home).unwrap_or(output);
                    protected |= suppress_unresolved(home, &output)?;
                }
                protected
            }
            None => match workspace_target_chosen(args, tool, home, machine) {
                Some(default) => {
                    let target = under_home(&default.target, home).unwrap_or(default.target);
                    match workspace_choices(home, &default.manifests, args, tool, machine) {
                        Ok(Some(owner)) => {
                            owned_default = observe_existing(home, &target, &owner)?;
                            owned_default
                        }
                        Ok(None) | Err(()) => suppress_unresolved(home, &target)?,
                    }
                }
                None => false,
            },
        };
        let unresolved = match cwd
            .as_deref()
            .and_then(|cwd| install_roots(&named, tool, cwd, &moved))
        {
            Some(roots) => {
                super::unresolved_words(&named, tool, &moved)
                    || roots.iter().any(|root| {
                        super::configs_unresolved(
                            root,
                            tool,
                            &moved,
                            super::reads_artifacts(&named),
                        )
                    })
            }
            None => super::unresolved_directory(&named, tool, &moved),
        };
        if line.is_none() || unresolved || unread {
            suppress_configured(home)?;
        }
        return Ok(if owned_default {
            Observation::Owned
        } else if protected {
            Observation::Protected
        } else {
            Observation::Unmanaged
        });
    };
    let managed_default = matches!(&chosen.target, super::FinalTarget::WorkspaceDefault)
        && chosen
            .manifests
            .iter()
            .any(|manifest| !never_managed(home, manifest));
    // An owner this reader cannot tell, or one where atpkg keeps no custody (a package outside
    // HOME, a root at HOME), holds every output the call writes in custody: its target and the
    // build directory it names apart ([`super::SelectedOutputs::intermediates`]), each spelled
    // where the kernel reaches it in HOME.
    let Ok(Some(owner)) = workspace_choices(home, &chosen.manifests, args, tool, machine) else {
        let target = match &chosen.target {
            super::FinalTarget::Explicit(target) => Some(target.clone()),
            // No canonical workspace means no default target. Keep unresolved configured
            // roots protected, as the unsupported-output path does, without guessing one.
            super::FinalTarget::WorkspaceDefault if managed_default => {
                suppress_configured(home)?;
                return Ok(Observation::DefaultOutputsUnowned);
            }
            // A package atpkg never manages builds into the `target` of the root Targo finds
            // for it, as a call that selects no output does (above).
            super::FinalTarget::WorkspaceDefault => {
                workspace_target_chosen(args, tool, home, machine).map(|default| default.target)
            }
        };
        // A `miri` target is held with the folder it lies inside of ([`super::SelectedOutputs::within`]).
        let mut protected = false;
        for output in target
            .iter()
            .chain(chosen.within.as_ref())
            .chain(chosen.intermediates.as_ref())
        {
            let output = under_home(output, home).unwrap_or_else(|| output.clone());
            protected |= suppress_unresolved(home, &output)?;
        }
        return Ok(if protected {
            Observation::Protected
        } else {
            Observation::Unmanaged
        });
    };
    let target = match chosen.target {
        super::FinalTarget::Explicit(target) => target,
        super::FinalTarget::WorkspaceDefault => owner.join("target"),
    };
    // A `miri` line's target beside a build directory named apart (miri's line reads one as any
    // line does, and its intermediates go there, not into the target) is held through the
    // folder the target lies only inside of, as below: that folder and the build directory are
    // admitted together, neither adopted nor admitted through a link, as no paired output is.
    // Only where that folder neither became owned nor holds a marker (one whose absence cannot
    // be read counts) is the line's own target admitted in its place, so no root is made
    // inside a folder atpkg declares its own.
    if let Some(intermediates) = &chosen.intermediates {
        if let Some(within) = &chosen.within {
            let observed = observe_configured_outputs(home, &owner, within, intermediates)?;
            if observed != Observation::Unmanaged
                || !std::fs::symlink_metadata(within.join(MARKER))
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(observed);
            }
        }
        let observed = observe_configured_outputs(home, &owner, &target, intermediates)?;
        return Ok(if managed_default && observed != Observation::Owned {
            Observation::DefaultOutputsUnowned
        } else {
            observed
        });
    }
    let quiet = matches!(
        placement.as_ref(),
        Some(super::BuildDir::Target(_) | super::BuildDir::TrustEnv(_))
    );
    // A target named through a link is the folder it names when atpkg owns that folder, and
    // never adopted or created through it.
    #[cfg(unix)]
    match through_link(home, &target) {
        Some(Ok(linked)) => {
            return Ok(match admit_linked(home, &linked, &owner)? {
                Ok(()) => {
                    if quiet {
                        prepare_intermediates(home, &linked.real, &owner, &|| linked.still())?;
                    }
                    Observation::Owned
                }
                Err(refusal) => Observation::Refused(refusal),
            });
        }
        Some(Err(refusal)) => return Ok(Observation::Refused(refusal)),
        None => {}
    }
    // A `miri` line's target with no build directory named apart (one with it is admitted
    // above) is owned through the folder the build writes only inside of, as
    // that folder was owned when its `miri` was read as the target itself: through a link to a
    // folder atpkg owns, as any target named through one is, and else that folder, recorded,
    // created or adopted, so a later build that names it finds it owned (a root made inside a
    // folder that has none would split its custody, leaving it one no build can take). Only
    // where that folder cannot be owned is the line's own target owned in its place.
    let declared = chosen
        .within
        .as_deref()
        .filter(|within| std::fs::symlink_metadata(within.join(MARKER)).is_ok());
    let mut owned = false;
    if let Some(within) = &chosen.within {
        #[cfg(unix)]
        match through_link(home, within) {
            Some(Ok(linked)) => {
                return Ok(match admit_linked(home, &linked, &owner)? {
                    Ok(()) => Observation::Owned,
                    Err(refusal) => Observation::Refused(refusal),
                });
            }
            Some(Err(refusal)) => return Ok(Observation::Refused(refusal)),
            None => {}
        }
        owned = create_checked(home, within, &owner)?;
    }
    let through_within = owned;
    if !owned && declared.is_none() {
        owned = create_checked(home, &target, &owner)?;
    }
    // A target owned through the folder it lies in is a line `miri` hands back, run by an
    // external command, which Targo hands no global `--config`: no build writes a placed folder
    // there, so none is made (nor could one be under the target's record, which it has none of).
    let quiet = quiet && !through_within;
    // The quiet intermediates are prepared under the target's own record.
    #[cfg(unix)]
    if owned && quiet {
        prepare_intermediates(home, &target, &owner, &|| true)?;
    }
    if !owned && quiet {
        let child = super::target_build_path(&target);
        let directory = match placement.as_ref() {
            Some(super::BuildDir::Target(value)) => super::directory_from_config(value),
            Some(super::BuildDir::TrustEnv(directory)) => Some(directory.as_str()),
            _ => None,
        };
        // Never a root inside a folder atpkg already declares its own, nor any folder in a cargo
        // home's cache, its leaf's parent included. A local fallback's target is the `target` of
        // the workspace root Targo finds, a folder holding a manifest, reached through no link
        // (one returned above): that names no cargo home's cache (a folder in a package is its
        // build's, [`adoption::home_cache`]), and what it keeps at its top (an xtask's
        // `target/cache`) is no cache's, so its contents are not read as one.
        #[cfg(unix)]
        let cache = !local_fallback && adoption::home_cache(home, &target);
        #[cfg(not(unix))]
        let cache = false;
        if declared.is_none()
            && !cache
            && (local_fallback || directory.map(Path::new) == Some(child.as_path()))
        {
            #[cfg(unix)]
            if scope(home, &child) {
                let parent = child
                    .parent()
                    .ok_or_else(|| std::io::Error::other("quiet build directory has no parent"))?;
                creation::ensure_parent(home, parent).ok_or_else(|| {
                    std::io::Error::other("the target's quiet build parent could not be prepared")
                })?;
            }
            if create_checked(home, &child, &owner)? {
                return Ok(Observation::IntermediatesOwned);
            }
        }
        // The producer's own fallback in a home that can record nothing owes no receipt: its
        // caller prepares the same directory receiptless, by the same private-parent checks.
        if scope(home, &target) && !(local_fallback && unrecordable(home)) {
            return Err(std::io::Error::other(
                "the target's quiet build directory could not be owned; no build started",
            ));
        }
    }
    Ok(if owned {
        Observation::Owned
    } else {
        Observation::Unmanaged
    })
}

/// Admit the smallest roots covering both outputs. No existing unowned root is adopted:
/// a failed second admission may leave a fresh empty owned root, but never starts a producer
/// or claims that an existing caller directory is disposable. Nested outputs share one
/// receipt, and every descendant is prepared while that receipt's retirement gate is held.
fn observe_configured_outputs(
    home: &Path,
    owner: &Path,
    target: &Path,
    intermediates: &Path,
) -> std::io::Result<Observation> {
    #[cfg(unix)]
    {
        let mut roots = vec![target, intermediates];
        roots.sort_by_key(|root| root.components().count());
        if roots[1].starts_with(roots[0]) {
            roots.pop();
        }
        if roots.iter().any(|root| !scope(home, root)) {
            return Ok(Observation::Unmanaged);
        }
        for root in &roots {
            if !create_with_policy(home, root, owner, None, Admission::Create)?.0 {
                return Ok(Observation::Unmanaged);
            }
        }
        for output in [target, intermediates] {
            let root = roots
                .iter()
                .find(|root| output.starts_with(root))
                .ok_or_else(|| std::io::Error::other("configured output has no custody root"))?;
            let relative = output.strip_prefix(root).map_err(std::io::Error::other)?;
            prepare_owned_descendants(home, root, owner, relative, &|| true)?;
        }
        Ok(Observation::Owned)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, owner, target, intermediates);
        Ok(Observation::Unmanaged)
    }
}

/// Existing observer controls also exercise conservative unknown-owner protection.
#[cfg(all(test, unix))]
fn observe_on(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> std::io::Result<bool> {
    observe(args, tool, home, machine).map(|observed| {
        matches!(
            observed,
            Observation::Protected | Observation::Owned | Observation::IntermediatesOwned
        )
    })
}

/// `machine`'s variable `name`, with the build directory hidden in both spellings
/// ([`super::BUILD_DIR_ENV`], [`super::BUILD_DIR_TWIN`]): a call classified as typed without the
/// one it inherited reads neither.
fn without_build_dir(machine: &super::Machine<'_>, name: &str) -> Option<std::ffi::OsString> {
    if name == super::BUILD_DIR_ENV || name == super::BUILD_DIR_TWIN {
        None
    } else {
        (machine.env)(name)
    }
}

/// An inherited directory can be resolved without granting its environment provenance:
/// recompute this call's existing placement with only that variable hidden (in both spellings,
/// [`without_build_dir`]; the one read is the one `tool` reads, [`super::cargo_var`]), stripping only
/// a matching leading config value our producer can emit. Anything other than exact equality
/// keeps the conservative unknown-owner path. An existing root is admitted only as [`observe`]
/// admits one typed with no inherited directory.
fn equivalent_placement_args<'a>(
    args: &'a [String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> Option<std::borrow::Cow<'a, [String]>> {
    let inherited = super::cargo_var(super::BUILD_DIR_ENV, tool, machine)?;
    let inherited = inherited.to_str()?;
    let typed = if super::directory_from_placed_args(args) == Some(inherited) {
        let at =
            usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
        let mut typed = args[..at].to_vec();
        typed.extend_from_slice(&args[at + 2..]);
        std::borrow::Cow::Owned(typed)
    } else {
        std::borrow::Cow::Borrowed(args)
    };
    let env = |name: &str| without_build_dir(machine, name);
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

/// Observe a native producer using its planned environment rather than this launcher's.
/// The machine describes the call before producer-selected placement, as [`observe_command`]
/// sees a call before the shim adds its directory. Explicit caller layouts retain that status.
/// Outside macOS no managed ownership is claimed.
///
/// # Errors
/// As [`observe_command`]; without a home, a call that may build ([`homeless`]).
pub fn observe_command_with(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> std::io::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let Some(home) = machine.home else {
        return homeless(args, tool, machine);
    };
    observe_for_producer(args, tool, home, machine, true).map(|_| ())
}

/// The call `args` where `machine` has no home (HOME unset or no absolute path): this reader
/// cannot tell which folders atpkg manages, while Targo still builds into them (its home is the
/// account's, `getpwuid`'s, and a target a word, a variable or a config names may lie in it),
/// so only a call that builds nothing goes on ([`non_producing_command`], [`targo_query`]).
///
/// # Errors
/// Any other call, refused before it builds.
fn homeless(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> std::io::Result<()> {
    if non_producing_command(args, tool, machine) || targo_query(args, tool, machine) {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "HOME is unset or not an absolute path, so atpkg cannot tell which folders it manages; no build started",
        ))
    }
}

/// Validate the complete argv a native reroute actually selected, then observe ownership
/// and prepare its generated intermediates. Caller layouts and unsupported upstream versions
/// use [`observe_command_with`] without selecting a placement. Outside macOS no managed
/// ownership is claimed, as there.
///
/// # Errors
/// The selected arguments no longer match placement policy, or directory safety or existing
/// managed custody cannot be established before the compiler starts.
pub(crate) fn observe_placed_command_with(
    args: &[String],
    placed: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
) -> std::io::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let stop = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the selected compiler build-directory plan changed before admission; no build started",
        )
    };
    let home = machine.home.ok_or_else(stop)?;
    let (selected, expected) = super::placed_build_dir(args, tool, machine).ok_or_else(stop)?;
    if expected != placed {
        return Err(stop());
    }
    observe_selected_placement(args, tool, home, machine, &selected, true).map(|_| ())
}

/// Use one environment snapshot for actual placement selection and its admission. The
/// original-argument observer and shim share this live machine's bounded config reader.
pub(crate) fn with_live_machine<T>(
    cwd: Option<&Path>,
    run: impl FnOnce(&super::Machine<'_>) -> T,
) -> T {
    let environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    with_machine(
        cwd,
        &|name| environment.get(std::ffi::OsStr::new(name)).cloned(),
        run,
    )
}

fn with_machine<T>(
    cwd: Option<&Path>,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    run: impl FnOnce(&super::Machine<'_>) -> T,
) -> T {
    let home = aterm_types::dirs::home_dir();
    run(&super::Machine {
        home: home.as_deref(),
        root: None,
        cwd,
        env,
        read: &live_read,
        read_manifest: &|path| match crate::metadata_io::read_bounded_regular_utf8(path, 1 << 20) {
            Ok(text) => Ok(Some(text)),
            // The metadata reader also calls redirected/non-regular inputs NotFound.
            // Only actual absence may advance initial discovery; existence grants no owner.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match path.try_exists() {
                Ok(false) => Ok(None),
                Ok(true) | Err(_) => Err(()),
            },
            Err(_) => Err(()),
        },
        writable: &super::writable,
    })
}

/// The front's own reader ([`with_machine`]'s): the regular UTF-8 file at `path`, of at most
/// 1 MiB, its final link not followed; `Ok(None)` where nothing stands there, `Err` for anything
/// else.
fn live_read(path: &Path) -> Result<Option<String>, ()> {
    match crate::metadata_io::read_bounded_regular_utf8(path, 1 << 20) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
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
        None,
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
/// With `build`, the answer is what an observed call that produces build output builds
/// ([`ShimBuild`]), for the front's start line ([`crate::build_start`]), read by a second reader
/// ([`shim_build`]): the registration resolves no owner for a build in the shared cache, the
/// common one. `None` without `build` (the front asks only for a call started in a tab whose
/// record it can write, so no other call pays for the read), for a call that produces none (a
/// reading or resolving verb, help, clean, a version) or whose workspace root this reader does
/// not resolve, and wherever calls are not observed (off macOS, or with no home).
///
/// # Errors
/// As [`observe_command`]'s, or `InvalidInput` for an
/// unknown or mismatched placement claim.
pub fn observe_shim_call(
    args: &[String],
    tool: super::Tool,
    placed: &str,
    cwd: Option<&Path>,
    build: bool,
) -> std::io::Result<Option<ShimBuild>> {
    observation_with(
        args,
        tool,
        cwd,
        &|name| std::env::var_os(name),
        true,
        Some(placed),
    )?;
    if !build || !cfg!(target_os = "macos") {
        return Ok(None);
    }
    Ok(with_machine(
        cwd,
        &|name| std::env::var_os(name),
        |machine| shim_build(args, tool, placed, machine.home?, machine),
    ))
}

/// What a shim's call builds, as its front read it ([`observe_shim_call`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShimBuild {
    /// The workspace root the call compiles, canonical: Cargo's workspace root of the manifest
    /// the call names or finds, as this reader resolves it ([`workspace`], inside home).
    pub workspace: PathBuf,
    /// Where the call's build output goes, where the placement names it: the directory the
    /// shim's block placed (or, for a call it left as typed, the one `CARGO_BUILD_BUILD_DIR`
    /// names), with the workspace root in place of `{workspace-root}`. A placement under a
    /// target directory (`<target>/.ubd/build.noindex`) names that target directory. `None`
    /// where none is named, and where it is relative or holds another template.
    pub build_dir: Option<PathBuf>,
}

/// What the shim's call `args` builds, read as [`observe_shim_on`] reads it (`placed` what the
/// block did to the words, the variable it exported hidden). It reads manifests and configs
/// and writes nothing: no receipt, no directory. `None` for a `placed` the block never says, a
/// call that produces no build output ([`non_producing_command`], [`targo_query`]), a `-C` this
/// reader cannot resolve or Targo refuses for want of its gate ([`from_targo_cwd`]), a call that names no manifest and finds none (`targo install x` in a
/// folder with no `Cargo.toml` builds no workspace there), and a manifest whose workspace root
/// this reader does not resolve (outside home, or read two ways): a line names the workspace a
/// call compiles or nothing, never the folder it was typed in, which may hold many.
fn shim_build(
    args: &[String],
    tool: super::Tool,
    placed: &str,
    home: &Path,
    machine: &super::Machine<'_>,
) -> Option<ShimBuild> {
    let (typed, hidden) = typed_call(args, placed)?;
    let env = |name: &str| {
        if Some(name) == hidden {
            None
        } else {
            (machine.env)(name)
        }
    };
    let original = &super::Machine {
        env: &env,
        ..*machine
    };
    if non_producing_command(typed, tool, original) || targo_query(typed, tool, original) {
        return None;
    }
    // A `+toolchain` is no word `-C` is read from, nor one of the line Targo runs.
    let at = usize::from(tool.is_cargo() && typed.first().is_some_and(|arg| arg.starts_with('+')));
    let words = &typed[at..];
    let cwd = targo_cwd(words, original).ok()??;
    let cwd = under_home(&cwd, home).unwrap_or(cwd);
    let alias = |verb: &str| user_alias(verb, typed, tool, original);
    let line = with_miri(words, tool, original, |miri| {
        super::targo_line(words, tool, &alias, miri)
    });
    let named = super::without_directory(line.as_deref().unwrap_or(words));
    let moved = &super::Machine {
        cwd: Some(&cwd),
        ..*original
    };
    let manifest = match named_manifest(&named, tool) {
        Some(raw) if raw.is_empty() => None,
        Some(raw) => normalize_with_cwd(PathBuf::from(raw), Some(&cwd)),
        None => super::initial_manifest_readings(&named, tool, moved)
            .into_iter()
            .find_map(|reading| match reading {
                super::InitialManifest::Readable(path) => Some(path),
                super::InitialManifest::Absent | super::InitialManifest::Unreadable(_) => None,
            }),
    };
    let workspace = workspace(home, &manifest?, typed, tool, original).ok()??;
    let directory = match placed {
        "config" => args
            .get(1)
            .and_then(|value| super::directory_from_config(value))
            .map(str::to_owned),
        "env" => (machine.env)(super::BUILD_DIR_ENV).and_then(|value| value.into_string().ok()),
        _ => super::cargo_var(super::BUILD_DIR_ENV, tool, machine)
            .and_then(|value| value.into_string().ok()),
    };
    let build_dir = directory.and_then(|directory| {
        const ROOT: &str = "{workspace-root}";
        if directory.replace(ROOT, "").contains(['{', '}']) {
            return None;
        }
        let directory = if directory.contains(ROOT) {
            directory.replace(ROOT, workspace.to_str()?)
        } else {
            directory
        };
        let mut directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            return None;
        }
        let quiet = super::target_build_relative();
        if directory.ends_with(&quiet) {
            for _ in quiet.components() {
                directory.pop();
            }
        }
        Some(directory)
    });
    Some(ShimBuild {
        workspace,
        build_dir,
    })
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
        "config" => match args {
            [flag, value, rest @ ..]
                if flag == "--config" && super::directory_from_config(value).is_some() =>
            {
                Some((rest, Some(super::BUILD_DIR_ENV)))
            }
            _ => None,
        },
        "env" => Some((args, Some(super::BUILD_DIR_ENV))),
        _ => None,
    }
}

fn invalid_shim_claim() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "the compiler shim's build-directory claim does not match current placement; run aterm pkg repair; no build started",
    )
}

fn observe_shim_on(
    args: &[String],
    tool: super::Tool,
    placed: &str,
    home: &Path,
    machine: &super::Machine<'_>,
    require_explicit: bool,
) -> std::io::Result<bool> {
    let stop = invalid_shim_claim;
    let (typed, hidden) = typed_call(args, placed).ok_or_else(stop)?;
    if placed == "-" {
        if !non_producing_command(typed, tool, machine)
            && super::decide(typed, tool, machine).is_some()
        {
            return Err(stop());
        }
        return observe_for_producer(typed, tool, home, machine, require_explicit);
    }
    let env = |name: &str| {
        if Some(name) == hidden {
            None
        } else {
            (machine.env)(name)
        }
    };
    let original = super::Machine {
        env: &env,
        ..*machine
    };
    let expected = super::decide(typed, tool, &original).ok_or_else(stop)?;
    let matches = match &expected {
        super::BuildDir::Cache(value) | super::BuildDir::Target(value) => {
            placed == "config"
                && args.get(1) == Some(value)
                && if super::loses_config(typed, tool) {
                    super::directory_from_config(value).is_some_and(|directory| {
                        (machine.env)(super::BUILD_DIR_ENV).as_deref()
                            == Some(std::ffi::OsStr::new(directory))
                    })
                } else {
                    (machine.env)(super::BUILD_DIR_ENV).is_none()
                }
        }
        super::BuildDir::TrustEnv(directory) => {
            placed == "env"
                && (machine.env)(super::BUILD_DIR_ENV).as_deref()
                    == Some(std::ffi::OsStr::new(directory))
        }
    };
    if !matches {
        return Err(stop());
    }
    observe_selected_placement(typed, tool, home, &original, &expected, require_explicit)
}

fn observe_selected_placement(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    selected: &super::BuildDir,
    require_explicit: bool,
) -> std::io::Result<bool> {
    // Bind the actual selected call before admission mutates its own cache
    // parents. The maintained still_selected keeps that location afterward;
    // configuration, environment and physical ownership remain live inputs.
    if super::decide(args, tool, machine).as_ref() != Some(selected) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the selected compiler build-directory plan changed before admission; no build started",
        ));
    }
    let observed = observe_for_producer_with_placement(
        args,
        tool,
        home,
        machine,
        require_explicit,
        Some(selected),
    )?;
    // A call that builds nothing, a query Targo answers among them, writes no placed folder.
    if !matches!(
        observed,
        Observation::Owned | Observation::IntermediatesOwned
    ) && !non_producing_command(args, tool, machine)
        && !targo_query(args, tool, machine)
    {
        // Judge the plan before writing the placement's directory, so a refused admission
        // leaves no build directory behind; the writability answer is held, so the cache
        // parents admission just laid do not flip it.
        still_placed(args, tool, machine, selected)?;
        prepare_unowned_placement(args, tool, home, machine, selected)?;
    }
    still_placed(args, tool, machine, selected)?;
    Ok(observed == Observation::Owned)
}

fn still_placed(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
    selected: &super::BuildDir,
) -> std::io::Result<()> {
    if super::still_selected(args, tool, machine, selected) {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "the selected compiler build-directory plan changed during admission; no build started",
    ))
}

/// Scope limits cache custody, not the privacy of intermediates the shim selected. Observe
/// first so unknown-owner protection and custody refusals remain authoritative; this step
/// only prepares the exact generated target and never writes an ownership receipt. The target is
/// read from the directory Targo works in ([`selected_call_outputs`], [`fallback_chosen`]), where a
/// `-C` before the verb moves it.
fn prepare_unowned_placement(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    selected: &super::BuildDir,
) -> std::io::Result<()> {
    let directory = match selected {
        super::BuildDir::Target(value) => super::directory_from_config(value),
        super::BuildDir::TrustEnv(directory) => Some(directory.as_str()),
        super::BuildDir::Cache(_) => return Ok(()),
    };
    let stop = || {
        std::io::Error::other(
            "the selected quiet build target could not be resolved safely; no build started",
        )
    };
    let target = if let Some(chosen) = selected_call_outputs(args, tool, home, machine) {
        let target = match chosen.target {
            super::FinalTarget::Explicit(target) => target,
            super::FinalTarget::WorkspaceDefault => {
                workspace_choices(home, &chosen.manifests, args, tool, machine)
                    .ok()
                    .flatten()
                    .ok_or_else(stop)?
                    .join("target")
            }
        };
        if directory.map(Path::new) != Some(super::target_build_path(&target).as_path()) {
            return Err(stop());
        }
        target
    } else {
        fallback_chosen(args, tool, home, machine, selected).ok_or_else(stop)?;
        // The root Targo finds, wherever the package lies: scope limits custody, not this.
        workspace_target_chosen(args, tool, home, machine)
            .ok_or_else(stop)?
            .target
    };
    prepare_target_build_dir(&target)
}

fn observation_with(
    args: &[String],
    tool: super::Tool,
    cwd: Option<&Path>,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    require_explicit: bool,
    placed: Option<&str>,
) -> std::io::Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    with_machine(cwd, env, |machine| {
        observation_on(args, tool, machine, require_explicit, placed)
    })
}

/// [`observation_with`]'s judgement on `machine`: a shim's call as `placed` says the block
/// placed it ([`observe_shim_on`]), else the producer's call as typed; with no home, a claim of
/// a placement the block never makes without one is refused, and the call goes to [`homeless`].
fn observation_on(
    args: &[String],
    tool: super::Tool,
    machine: &super::Machine<'_>,
    require_explicit: bool,
    placed: Option<&str>,
) -> std::io::Result<bool> {
    let Some(home) = machine.home else {
        if placed.is_some_and(|claim| claim != "-") {
            return Err(invalid_shim_claim());
        }
        return homeless(args, tool, machine).map(|()| false);
    };
    match placed {
        Some(placed) => observe_shim_on(args, tool, placed, home, machine, require_explicit),
        None => observe_for_producer(args, tool, home, machine, require_explicit),
    }
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

/// Whether the package whose manifest is `manifest` (a path to a `Cargo.toml` or `Targo.toml`)
/// lies where atpkg never manages a workspace: its folder, the one the manifest is named in and
/// resolved through its links as [`workspace`] resolves it (Targo's package root is that folder,
/// wherever a final link names), outside [`scope`], as HOME itself, a path outside HOME, a
/// folder of HOME atpkg never looks into, app data and a file provider's domain are. A build of
/// it owes no managed custody, wherever its target is.
fn never_managed(home: &Path, manifest: &Path) -> bool {
    // A manifest identifies its package by the folder it is named in, never through its links.
    // An existing non-file, or a link to a file elsewhere, selected in a managed directory must
    // refuse, not borrow its target's parent (HOME itself or foreign scope) to pass as an
    // unmanaged package.
    let resolved = manifest
        .parent()
        .map(|package| std::fs::canonicalize(package).unwrap_or_else(|_| package.to_path_buf()));
    resolved.is_none_or(|package| !scope(home, &package))
}

/// Whether a producer call owes managed custody of its output ([`observe_for_producer`] refuses
/// it when none can be established): it names a target or build directory under HOME outside
/// the shared cache, or one atpkg cannot resolve, for a package atpkg could manage. A call whose
/// manifest lies outside HOME, or that names none and runs outside HOME or in no package there,
/// owes none, as does one whose package [`never_managed`] names. A package atpkg could manage
/// whose root lies where atpkg keeps no custody ([`workspace_at`]'s `Ok(None)`: HOME itself, or
/// outside HOME) still owes custody of an output it names, and the producer refuses it: no
/// owner can be recorded, and none is invented. Each is judged on the line Targo
/// runs ([`super::targo_line`]: a user alias's words with the words after its name), from the
/// directory Targo works in ([`targo_cwd`]), which accounts for every `-C`
/// ([`super::without_directory`]); a `-C` this reader cannot resolve owes custody. A line whose
/// alias this reader cannot tell is judged on every line it may run ([`super::untold_lines`]):
/// the words after its name up to a `--` Targo's, each prefix of them, so a `--target-dir` among
/// them is read whatever the alias makes of the words after it; it owes custody where any of
/// them, the environment or a config names a target under HOME, or where a config's line that is
/// no comment spells a directory's key, as the alias's definition there may
/// ([`super::configs_mention_dirs`]). What the alias's own words name is unread there, and
/// [`observe`] poisons every registered root's owners. A config's text is read for what its
/// `build` table names ([`super::configs_unresolved`]): an alias's words there (`build-dev =
/// "run --target-dir target/tools …"`) name no target for a call that does not run it, and a line
/// that runs it holds them. A line `miri` hands back whose target this reader cannot tell
/// ([`super::MiriCall::target_unread`]) owes custody wherever its package is one atpkg could
/// manage. A call Targo refuses ([`super::targo_line`] reads no line for it) builds nothing and
/// owes none. A `-C` it refuses for want of its gate ([`super::directory_refused`]) is judged as
/// the same call honoured, so one that names an output there is refused, naming the gate
/// ([`observe_for_producer`]): a gate this reader misread would otherwise let a build Targo honours
/// through unrecorded.
fn requires_managed_explicit(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> bool {
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
    let typed = &args[at..];
    let Ok(cwd) = targo_cwd(typed, machine) else {
        return true;
    };
    // Spelled as Targo reads its directory back (`getcwd`), under HOME where it lies there: a
    // `-C` resolved through HOME's other name (`/System/Volumes/Data/Users/<me>`) is HOME's.
    let cwd = cwd.map(|dir| under_home(&dir, home).unwrap_or(dir));
    let alias = |verb: &str| user_alias(verb, typed, tool, machine);
    with_miri(typed, tool, machine, |miri| {
        let unread = super::miri_call(typed, tool, &alias, miri).target_unread;
        let owes = |line: &[String], untold: bool| {
            let line = super::without_directory(line);
            let manifest = named_manifest(&line, tool);
            owes_custody(
                manifest.as_deref(),
                &line,
                tool,
                home,
                cwd.as_deref(),
                machine,
                Unread {
                    target: unread,
                    alias: untold,
                },
            )
        };
        if let Some(line) = super::targo_line(typed, tool, &alias, miri) {
            return owes(&line, false);
        }
        super::untold_lines(typed, tool, &alias, miri)
            .is_some_and(|lines| lines.iter().any(|line| owes(line, true)))
    })
}

/// The last manifest `line` names among Targo's own words ([`super::own_words`]):
/// `--manifest-path` in either spelling, a cluster's `-m` (`-m`, `-mX`, `-m=X`, `-qm X`) where it
/// is Targo's manifest ([`super::short_manifest_scope`]), as the placement reads it, and
/// `install`'s `--path` in either spelling, the folder whose `Cargo.toml` it builds and whose
/// configs it reads (`install.rs`'s `reload_rooted_at`).
fn named_manifest(line: &[String], tool: super::Tool) -> Option<String> {
    let scope = super::short_manifest_scope(line, tool);
    let own = super::own_words(line, tool);
    let install = super::subcommand_at(line).is_some_and(|at| line[at] == "install");
    // An empty `--path` is the working directory, as Targo joins it; `/` stays the root.
    let package = |dir: &str| {
        if dir.is_empty() {
            "Cargo.toml".to_owned()
        } else {
            format!("{}/Cargo.toml", dir.trim_end_matches('/'))
        }
    };
    let mut manifest = None;
    // A short option's value needs no skip here: this loop acts only on words that start with
    // `-`, which no value but `-j`'s is, and the scope reads none it skips as a manifest.
    let mut words = line[..own].iter().map(String::as_str).zip(scope);
    while let Some((word, reads)) = words.next() {
        match word {
            "--manifest-path" => manifest = words.next().map(|(value, _)| value.to_owned()),
            "--path" if install => manifest = words.next().map(|(value, _)| package(value)),
            "--target-dir" | "--config" | "--color" | "--explain" => {
                let _ = words.next();
            }
            _ => {
                if let Some(path) = word.strip_prefix("--manifest-path=") {
                    manifest = Some(path.to_owned());
                } else if let Some(dir) = word.strip_prefix("--path=").filter(|_| install) {
                    manifest = Some(package(dir));
                } else if let Some(value) = super::shorts(word)
                    .filter(|_| reads)
                    .and_then(|c| c.value_of('m'))
                {
                    manifest = value
                        .or_else(|| words.next().map(|(value, _)| value))
                        .map(str::to_owned);
                }
            }
        }
    }
    manifest
}

/// What [`owes_custody`] cannot read of the line it judges.
#[derive(Clone, Copy)]
struct Unread {
    /// The line builds into a target this reader cannot tell ([`super::MiriCall::target_unread`]).
    target: bool,
    /// The line is one of those an alias this reader cannot tell may run
    /// ([`super::untold_lines`]): the alias's own words, which a config's text may hold, are
    /// unread.
    alias: bool,
}

/// [`requires_managed_explicit`]'s judgement of a call whose manifest is `manifest_arg`, `args`
/// the line Targo runs, Targo working in `cwd`, `unread` what of that line this reader cannot
/// tell. A config the walk reads owes custody where its `build` table names a target or build
/// directory, or one where the line reads it ([`super::configs_unresolved`]); for a line an
/// untold alias may run, also where any of its lines that is no comment spells a directory's key
/// ([`super::configs_mention_dirs`]), as the alias's own words may. An alias this reader reads
/// has its words in the line, judged with the call's own.
fn owes_custody(
    manifest_arg: Option<&str>,
    args: &[String],
    tool: super::Tool,
    home: &Path,
    cwd: Option<&Path>,
    machine: &super::Machine<'_>,
    unread: Unread,
) -> bool {
    // A package home is Targo's against the directory it works in, wherever the scan stands;
    // the build directory is judged apart, in both spellings.
    let env = |name: &str| match name {
        super::BUILD_DIR_ENV | super::BUILD_DIR_TWIN => None,
        "CARGO_HOME" | "TARGO_HOME" => (machine.env)(name).map(|value| match cwd {
            Some(cwd) if !value.is_empty() => cwd.join(value).into_os_string(),
            _ => value,
        }),
        _ => (machine.env)(name),
    };
    let at = super::Machine {
        home: Some(home),
        cwd,
        env: &env,
        ..*machine
    };
    let scan_cwd = if let Some(raw) = manifest_arg {
        if !raw.is_empty() {
            if let Some(manifest) = normalize_with_cwd(PathBuf::from(raw), cwd) {
                let Some(manifest) = under_home(&manifest, home) else {
                    return false;
                };
                if never_managed(home, &manifest) {
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
        let Some(cwd) = cwd.filter(|c| c.starts_with(home) && *c != home) else {
            return false;
        };
        // A failed read still identifies a package whose outputs may require custody.
        // Use the command's supported readings: known native discovery must retain
        // foreign-source pass-through; uncertain compatibility discovery must agree.
        // Filesystem presence may require refusal when either supplied reader is opaque;
        // it never authorizes an owner or changes configuration discovery semantics.
        let evidence = |observation: Result<Option<String>, ()>, path: &Path| match observation {
            Ok(None) if path.is_file() => Err(()),
            observation => observation,
        };
        let read = |path: &Path| evidence((machine.read)(path), path);
        let read_manifest = |path: &Path| evidence((machine.read_manifest)(path), path);
        let moved = super::Machine {
            cwd: Some(cwd),
            read: &read,
            read_manifest: &read_manifest,
            ..*machine
        };
        let managed = super::initial_manifest_readings(args, tool, &moved)
            .into_iter()
            .any(|observation| match observation {
                super::InitialManifest::Absent => false,
                super::InitialManifest::Readable(path)
                | super::InitialManifest::Unreadable(path) => !never_managed(home, &path),
            });
        if !managed {
            return false;
        }
        cwd.to_path_buf()
    };

    // Its package is one atpkg could manage, and its target may be anywhere.
    if unread.target {
        return true;
    }

    if let Some(raw) = raw_target_spec(args, tool, machine) {
        let Some(target) = physical(raw, cwd) else {
            return true;
        };
        if under_home(&target, home)
            .is_some_and(|target| !under_shared_cache(home, &target, machine))
        {
            return true;
        }
    }

    // The build directory `tool` reads from the environment: `CARGO_BUILD_BUILD_DIR`, or its
    // `TARGO_` twin where that is unset.
    if let Some(raw) = super::cargo_var(super::BUILD_DIR_ENV, tool, machine) {
        let root = machine
            .root
            .map_or_else(|| super::cache_root(home), Path::to_path_buf);
        let shared_template = super::config_value(&root)
            .and_then(|cfg| super::directory_from_config(&cfg).map(str::to_owned));
        match raw.to_str() {
            Some(s) if shared_template.as_deref() == Some(s) => {}
            Some(s) if !s.is_empty() && !s.contains('{') && !s.contains('}') => {
                let Some(dir) = physical(PathBuf::from(s), cwd) else {
                    return true;
                };
                if under_home(&dir, home)
                    .is_some_and(|dir| !under_shared_cache(home, &dir, machine))
                {
                    return true;
                }
            }
            _ => return true,
        }
    }

    let configs = |root: &Path| {
        super::configs_unresolved(root, tool, &at, super::reads_artifacts(args))
            || (unread.alias && super::configs_mention_dirs(root, tool, &at))
    };
    // A `--config` file is read against the directory Targo works in, whatever its configs'.
    if super::unresolved_words(args, tool, &at) {
        return true;
    }
    match cwd.and_then(|cwd| install_roots(args, tool, cwd, &at)) {
        Some(roots) => roots.iter().any(|root| configs(root)),
        // Targo reads the working directory's configs whatever manifest the call names, and the
        // manifest's folder is scanned too, as it was before.
        None => {
            configs(&scan_cwd)
                || cwd
                    .filter(|cwd| cwd.starts_with(home) && *cwd != scan_cwd)
                    .is_some_and(configs)
        }
    }
}

/// The folders from which `install` walks up to read its configs ([`super::configs_unresolved`]),
/// Targo working in `cwd`: it reloads them (`install.rs`'s `reload_rooted_at`) from its `--path
/// DIR` joined to `cwd` as typed, so `notes/../pkg` reads `notes`' too (`value_of_path`; only
/// the package's path is folded), or, with none, from the package home (`gctx.home()`), each
/// that Targo may take ([`super::package_homes`]). `None` for any other call, which reads
/// `cwd`'s.
fn install_roots(
    line: &[String],
    tool: super::Tool,
    cwd: &Path,
    machine: &super::Machine<'_>,
) -> Option<Vec<PathBuf>> {
    if !super::subcommand_at(line).is_some_and(|at| line[at] == "install") {
        return None;
    }
    Some(match named_manifest(line, tool) {
        Some(manifest) => vec![cwd.join(Path::new(&manifest).parent().unwrap_or(Path::new("")))],
        None => super::package_homes(
            tool,
            &super::Machine {
                cwd: Some(cwd),
                ..*machine
            },
        ),
    })
}

fn observe_for_producer(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    require_explicit: bool,
) -> std::io::Result<bool> {
    observe_for_producer_with_placement(args, tool, home, machine, require_explicit, None)
        .map(|observed| observed == Observation::Owned)
}

fn observe_for_producer_with_placement(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
    require_explicit: bool,
    selected: Option<&super::BuildDir>,
) -> std::io::Result<Observation> {
    // Where the package home cannot be told ([`home_unsettled`]), the call is read in each home
    // it may be: the `-C` directory's, as Targo run directly reads it, and the launcher's, which
    // a rustup proxy exports absolute. A user alias is that home's, so only a call that builds
    // nothing, or is a query, in both is one.
    let unsettled = require_explicit && home_unsettled(args, tool, machine);
    // Read only where unsettled: a call that names no relative home reads no environment here.
    let launcher_home = machine
        .cwd
        .filter(|_| unsettled)
        .and_then(|cwd| Some(cwd.join((machine.env)("CARGO_HOME")?).into_os_string()));
    let launcher_env = |name: &str| {
        if name == "CARGO_HOME" {
            launcher_home.clone()
        } else {
            (machine.env)(name)
        }
    };
    let launcher = super::Machine {
        env: &launcher_env,
        ..*machine
    };
    let in_every_home = |reads: &dyn Fn(&super::Machine<'_>) -> bool| {
        reads(machine) && (!unsettled || reads(&launcher))
    };
    if in_every_home(&|machine| non_producing_command(args, tool, machine)) {
        return Ok(Observation::Unmanaged);
    }
    // A nested compiler can inherit the exact directory our shim placed. Classify the
    // original call, not that derived environment, before deciding ownership is required.
    if let Some(typed) = equivalent_placement_args(args, tool, machine) {
        let env = |name: &str| without_build_dir(machine, name);
        return observe_for_producer_with_placement(
            &typed,
            tool,
            home,
            &super::Machine {
                env: &env,
                ..*machine
            },
            require_explicit,
            selected,
        );
    }
    if require_explicit && miri_setup_unclear(args, tool, home, machine) {
        return Err(std::io::Error::other(
            "miri's sysroot setup may build where atpkg manages builds, or where it cannot read; no build started",
        ));
    }
    let query = in_every_home(&|machine| targo_query(args, tool, machine));
    if unsettled && !query {
        return Err(std::io::Error::other(
            "a relative CARGO_HOME with -C names one package home against the launcher's folder (a rustup proxy's) and another against that directory (Targo's), and atpkg cannot tell which this cargo reads; set CARGO_HOME to an absolute path; no build started",
        ));
    }
    let explicit = requires_managed_explicit(args, tool, home, machine);
    let refusal = || {
        let at =
            usize::from(tool.is_cargo() && args.first().is_some_and(|arg| arg.starts_with('+')));
        if super::directory_arg(&args[at..]).is_some()
            && super::directory_refused(args, tool, machine)
        {
            return std::io::Error::other(
                "this call names a -C without the gate Targo's -C needs (-Z unstable-options before the verb, on a nightly channel), which Targo refuses; add the gate, or run the call from the tree it builds (cd there); no build started",
            );
        }
        std::io::Error::other(
            "build outputs are unmanaged or unsupported; choose fresh output directories for managed cache ownership",
        )
    };
    if require_explicit
        && explicit
        && !query
        && selected_call_outputs(args, tool, home, machine)
            .and_then(|chosen| {
                workspace_choices(home, &chosen.manifests, args, tool, machine)
                    .ok()
                    .flatten()
            })
            .is_none()
    {
        return Err(refusal());
    }
    let observed = match selected {
        Some(selected) => observe_with_placement(args, tool, home, machine, Some(selected)),
        None => observe(args, tool, home, machine),
    }?;
    let owned = observed == Observation::Owned;
    if require_explicit
        && (explicit || observed == Observation::DefaultOutputsUnowned)
        && !query
        && !owned
    {
        #[cfg(unix)]
        if let Observation::Refused(refusal) = observed {
            return Err(std::io::Error::other(refusal.why()));
        }
        return Err(refusal());
    }
    Ok(observed)
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
/// registration call's form ([`shim_call`]). Only completed, stable answers are cached. Queries
/// run outside the cache lock, so one unresponsive helper cannot hold up unrelated helpers.
///
/// # Errors
/// The helper cannot be inspected or queried, exits abnormally, or changes while its answer is obtained.
#[cfg(unix)]
pub fn helper_speaks_hop(helper: &Path) -> std::io::Result<bool> {
    helper_speaks_hop_with(helper, |physical| {
        let mut command = aterm_uds::spawnfd::command(physical);
        command
            .setsid(true)
            .args([crate::build_admit::VERB, "--capabilities"]);
        let out =
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(10))
                .map_err(|error| match error {
                    aterm_uds::deadline::RunError::Spawn(error)
                    | aterm_uds::deadline::RunError::Capture(error)
                    | aterm_uds::deadline::RunError::Wait(error) => error,
                    aterm_uds::deadline::RunError::TimedOut => std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "build admission helper capability query did not finish",
                    ),
                })?;
        helper_hop_answer(out)
    })
}

#[cfg(unix)]
fn helper_hop_answer(out: std::process::Output) -> std::io::Result<bool> {
    if out.status.code().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            format!(
                "build admission helper capability query did not exit normally: {}",
                out.status
            ),
        ));
    }
    Ok(out.status.success()
        && out.stdout == format!("{}\n", crate::build_admit::HOP_CAPABILITY).as_bytes())
}

#[cfg(unix)]
fn helper_speaks_hop_with(
    helper: &Path,
    probe: impl FnOnce(&Path) -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    static ASKED: std::sync::Mutex<Vec<(PathBuf, [u64; 5], bool)>> =
        std::sync::Mutex::new(Vec::new());
    let stamp = |path: &Path| {
        let physical = path.canonicalize()?;
        let m = std::fs::metadata(&physical)?;
        let at = u64::try_from(m.mtime()).unwrap_or(0);
        let ns = u64::try_from(m.mtime_nsec()).unwrap_or(0);
        Ok::<_, std::io::Error>((physical, [m.dev(), m.ino(), m.len(), at, ns]))
    };
    let (physical, before) = stamp(helper)?;
    {
        let asked = ASKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, _, speaks)) = asked
            .iter()
            .find(|(path, at, _)| *path == physical && *at == before)
        {
            return Ok(*speaks);
        }
    }
    let speaks = if crate::stub::is_running_image(&physical) {
        true
    } else {
        probe(&physical)?
    };
    if stamp(helper)? != (physical.clone(), before) {
        return Err(std::io::Error::other(
            "build admission helper changed during capability query",
        ));
    }
    let mut asked = ASKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    asked.retain(|(path, _, _)| *path != physical);
    asked.push((physical, before, speaks));
    Ok(speaks)
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
    let reporting = fence.reporting_pass();
    let removed = reclaim_selected_on(
        home,
        std::time::SystemTime::now(),
        super::ReclaimAfter::CALM,
        None,
        record,
        &reporting,
        Selection::Requested(&cache),
    );
    let retained = reporting.retained();
    for check in &retained {
        fence.note(&check.path, check.reason.clone());
    }
    if removed.is_empty() {
        if !retained.is_empty() {
            return Err(retained
                .iter()
                .map(|check| check.words())
                .collect::<Vec<_>>()
                .join("; "));
        }
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
        #[cfg(unix)]
        let held = gate::open(&lock_path)
            .ok()
            .and_then(|lock| gate::try_exclusive(lock).ok());
        #[cfg(not(unix))]
        let held = crate::metadata_io::open_regular(&lock_path)
            .ok()
            .and_then(|lock| crate::lock::Flock::try_lock(lock).ok());
        let Some(_held_record) = held else {
            continue;
        };
        let Some(receipt) = read(&cache.receipt_path) else {
            continue;
        };
        if receipt.retained {
            continue;
        }
        let advisory = match coordination {
            Some(guard) => match guard.into_advisory(home, &receipt) {
                Some(snapshot) => Some(snapshot),
                None => continue,
            },
            None if receipt.package.is_none() => None,
            None => continue,
        };
        // This snapshot supplies advisory candidates, never removal authority. Inventory can
        // be expensive; let producers update custody while it runs. Reclaim rechecks its gates.
        drop(_held_record);
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
        if let Some(snapshot) = advisory {
            if let Some(why) = snapshot.reason() {
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
        let Ok(lock) = gate::open(&lock_path) else {
            fence.note(&cache.root, super::KeepReason::CoordinationUnavailable);
            continue;
        };
        let Ok(_held_record) = gate::try_exclusive(lock) else {
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
            let Some(parent) = cache.root.parent() else {
                fence.note(&cache.root, super::KeepReason::CannotStage);
                continue;
            };
            // Its root moves into a stage made beside it: whether its name can be taken out of
            // its folder is read before the intent, as a leaf's is, so a root flagged before the
            // pass is kept with no intent on record, rather than asked for, refused at its rename
            // and asked for again every other pass, as before.
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
            ) || !fence.admit(
                &cache.root,
                super::KeepReason::CannotStage,
                super::movable(&cache.root, parent),
            ) || !fence.clear(&cache.root, &cache.root)
                || !intents.ask(&cache.root, why)
                || !fence.clear(&cache.root, &cache.root)
            {
                continue;
            }
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
                let _ = sweep.prune(&cache.root, &cache.root, None);
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
#[path = "build_cache_registered_tests.rs"]
mod tests;
