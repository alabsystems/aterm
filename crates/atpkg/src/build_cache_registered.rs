// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit Cargo caches created by aterm, at the caller's chosen location.
//!
//! A name or a Cargo tag is not ownership. A fresh private stage and durable identity receipt
//! precede atomic target publication. Existing unrecorded targets stay unmanaged and keep
//! their output paths. A producer requiring managed output refuses them. The index avoids
//! searching the person's folders.

use std::path::{Path, PathBuf};

#[path = "build_cache_package.rs"]
pub mod package;

#[path = "build_cache_create.rs"]
mod creation;

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
/// The index is uncertain, changes during the read, exceeds its bound, contains an invalid
/// receipt, or any recorded custody overlaps the path.
pub fn ordinary_clearance(home: &Path, path: &Path) -> Result<OrdinaryClearance, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("ordinary cache assessment needs an absolute unambiguous path".into());
    }
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
                if path.starts_with(held) || held.starts_with(path) {
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Identity {
    pub(super) device: u64,
    pub(super) inode: u64,
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

#[cfg(unix)]
fn identity_for(path: &Path, uid: u32) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    (metadata.is_dir() && private(metadata.uid(), metadata.mode(), uid)).then(|| Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
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
        &Identity {
            device: captured.st_dev,
            inode: captured.st_ino,
        },
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
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
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
        && identity(&receipt.root).as_ref() == Some(&receipt.identity)
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
        && marker.identity == receipt.identity
        && marker.nonce == receipt.nonce
        && marker.schema == receipt.schema
        && package::same_declaration(marker.package.as_ref(), receipt.package.as_ref())
}

#[cfg(unix)]
fn write(path: &Path, receipt: &Receipt) -> Option<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let parent = path.parent()?;
    let stage = aterm_tempfile::Builder::new()
        .prefix("cache-receipt-")
        .tempdir_in(parent)
        .ok()?;
    let pending = stage.path().join("receipt");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)
        .ok()?;
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
            && before.identity == receipt.identity
            && before.pending.as_ref().is_some_and(|before| {
                before.directory == pending.directory && before.identity == pending.identity
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
    read(&path).is_some_and(|receipt| {
        receipt.root == root
            && receipt.pending.is_none()
            && same(home, &receipt)
            && match &receipt.package {
                Some(policy) => {
                    !receipt.owners_unknown && package::producer_allows(home, policy, workspace)
                }
                None => {
                    receipt.owners_unknown || receipt.owners.iter().any(|owner| owner == workspace)
                }
            }
    })
}

/// A producer must not continue on a previously owned cache if it cannot be admitted.
/// Unmanaged existing paths are a successful observation with `false`, never an adoption.
/// A fresh staged creation and atomic publication are the only way to begin ownership;
/// existing output never moves.
///
/// # Errors
/// The registration gate was busy or an admitted cache's owner could not be published.
#[cfg(unix)]
pub fn create_checked(home: &Path, root: &Path, workspace: &Path) -> std::io::Result<bool> {
    create_with_policy(home, root, workspace, None).map(|(recorded, _)| recorded)
}

#[cfg(unix)]
fn create_with_policy(
    home: &Path,
    root: &Path,
    workspace: &Path,
    policy: Option<package::Policy>,
) -> std::io::Result<(bool, bool)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut must_record = false;
    let mut created = false;
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
            .custom_flags(libc::O_NOFOLLOW)
            .open(index.join("locks.noindex/admission.lock"))
            .ok()?;
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
                let mut receipt = before.or_else(|| read(&path))?;
                if receipt.root != root || !same(home, &receipt) {
                    return None;
                }
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
        Err(std::io::Error::other(
            "the managed build cache's owner could not be recorded",
        ))
    } else {
        Ok((false, false))
    }
}

#[cfg(not(unix))]
pub fn create_checked(_: &Path, _: &Path, _: &Path) -> std::io::Result<bool> {
    Ok(false)
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
    let still_root = identity(&receipt.root).as_ref() == Some(&receipt.identity);
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
        && identity(&pending.directory).as_ref() == Some(&pending.identity)
        && (pending.authorized
            || (identity(&payload).as_ref() == Some(&receipt.identity)
                && read(&payload.join(MARKER)).is_some_and(|marker| {
                    marker.identity == receipt.identity && marker.nonce == receipt.nonce
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

/// Known builtin commands that produce no build output need no owner observation: queries
/// and clean. Clean must reach Cargo even for an unregistered target, without adopting it.
/// A command name used as an option value, an external alias or lint argument proves none of this.
fn non_producing_command(args: &[String], tool: super::Tool) -> bool {
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
            _ if short_version_query(word) => return true,
            _ if word.starts_with("--config=")
                || word.starts_with("--color=")
                || word.starts_with("--manifest-path=")
                || word.starts_with("--target-dir=")
                || word.starts_with("--explain=")
                || word.starts_with("-Z")
                || word.starts_with("-C") => {}
            _ => {
                return matches!(word, "help" | "clean")
                    || crate::reroute::LANE_FREE_QUERIES.contains(&word);
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
}

fn observe(
    args: &[String],
    tool: super::Tool,
    home: &Path,
    machine: &super::Machine<'_>,
) -> std::io::Result<Observation> {
    if non_producing_command(args, tool)
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
    observe(args, tool, home, machine).map(|observed| observed != Observation::Unmanaged)
}

/// An inherited directory can be resolved without granting its environment provenance:
/// recompute this call's existing placement with only that variable hidden, stripping only
/// a matching leading config value our producer can emit. Anything other than exact equality
/// keeps the conservative unknown-owner path. Existing roots are still never adopted.
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
/// when a launcher runs from another directory. Existing unknown caches remain unmanaged.
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
                if !manifest.starts_with(home) {
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
        let has_manifest = cwd
            .ancestors()
            .take_while(|p| p.starts_with(home) && *p != home)
            .take(32)
            .any(|dir| {
                super::checkout::MANIFESTS.iter().any(|name| {
                    let path = dir.join(name);
                    !matches!((machine.read)(&path), Ok(None)) || path.is_file()
                })
            });
        if !has_manifest {
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
    if non_producing_command(args, tool) {
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
    let owned = observe(args, tool, home, machine)? == Observation::Owned;
    if require_explicit && explicit && !query && !owned {
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
            if identity(&pending.directory).as_ref() != Some(&pending.identity)
                || pending.identity.device != receipt.identity.device
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
                || identity(&pending.directory).as_ref() != Some(&pending.identity)
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
        super::RECLAIM_GONE,
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
        if idle >= super::RECLAIM_GONE && owners_gone(home, &receipt, None) {
            // An unheld whole-root retirement already contains these stages.
            if !held {
                planned.truncate(stage_start);
            }
            planned.push(super::Planned {
                path: cache.root.clone(),
                paths: vec![cache.root],
                why: super::Unused::Gone {
                    idle,
                    after: super::RECLAIM_GONE,
                },
                held,
                stays: None,
            });
            continue;
        }
        let store = super::compiler_store(Some(home));
        let doomed = super::stale::decide(
            &super::stale::survey(
                &super::stale::Disk,
                &cache.root,
                &super::profiles_of(&seen.locks),
                store.as_deref(),
            ),
            now,
        );
        if !doomed.is_empty() {
            let incremental = doomed
                .iter()
                .filter(|part| part.part == super::stale::Part::Incremental)
                .count();
            let units = doomed.len() - incremental;
            planned.push(super::Planned {
                path: cache.root,
                paths: doomed.into_iter().flat_map(|part| part.paths).collect(),
                why: super::Unused::Stale {
                    incremental,
                    units,
                    after: super::RECLAIM_STALE,
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
    gone_after: std::time::Duration,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_on(
        home,
        now,
        gone_after,
        placing,
        record,
        &super::fence::Fence::default(),
    )
}

#[cfg(unix)]
pub(super) fn reclaim_on(
    home: &Path,
    now: std::time::SystemTime,
    gone_after: std::time::Duration,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    reclaim_selected_on(
        home,
        now,
        gone_after,
        placing,
        record,
        fence,
        Selection::Automatic,
    )
}

#[cfg(unix)]
enum Selection<'a> {
    Automatic,
    Package(package::Context<'a>),
    Requested(&'a Registered),
}

#[cfg(unix)]
fn reclaim_selected_on(
    home: &Path,
    now: std::time::SystemTime,
    gone_after: std::time::Duration,
    placing: Option<&super::checkout::Placing>,
    record: &mut dyn FnMut(&Path, super::Unused) -> bool,
    fence: &super::fence::Fence<'_>,
    selection: Selection<'_>,
) -> Vec<(PathBuf, super::Unused)> {
    let (selected_cache, requested, kernel) = match selection {
        Selection::Automatic => (None, None, &package::Live as &dyn package::Kernel),
        Selection::Package(context) => (context.cache, None, context.kernel),
        Selection::Requested(root) => (None, Some(root), &package::Live as &dyn package::Kernel),
    };
    let mut intents = super::Intents::new(record);
    let candidates = requested.map_or_else(|| roots(home), |wanted| vec![wanted.clone()]);
    for cache in candidates {
        let Some(preliminary) = read(&cache.receipt_path) else {
            continue;
        };
        if preliminary.retained || preliminary.creating.is_some() {
            continue;
        }
        if selected_cache.is_some_and(|selected| !package::belongs(&preliminary, selected)) {
            continue;
        }
        // Take a package's whole-build coordinator BEFORE its receipt gate, including the
        // resume path. Keep it alive until the last staged entry and receipt are settled.
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
        let Some(mut receipt) = read(&cache.receipt_path) else {
            continue;
        };
        if receipt.retained
            || receipt.creating.is_some()
            || requested.is_some_and(|wanted| !wanted.matches(&receipt))
            || !package::same_declaration(preliminary.package.as_ref(), receipt.package.as_ref())
        {
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
            continue;
        }
        if let Some(mut pending) = receipt.pending.clone() {
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
                    Err(_) => continue,
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => continue,
            };
            let Some(_held_pending) = super::hold(&pending_locks) else {
                continue;
            };
            if fence.clear(&cache.root, &pending.directory)
                && read(&cache.receipt_path).as_ref() == Some(&receipt)
                && coordination
                    .as_ref()
                    .is_none_or(|guard| guard.current(home, &receipt))
                && intents.ask_staged(&pending.directory, super::Unused::Unfinished)
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
            continue;
        }
        let Some(seen) = super::look(&cache.root) else {
            continue;
        };
        // A missing or deeper lock is uncertainty, not an idle-cache witness.
        if seen.locks.is_empty()
            || super::locks_under(&cache.root).ok().as_ref() != Some(&seen.locks)
        {
            continue;
        }
        let gone = owners_gone(home, &receipt, placing);
        let budget = coordination
            .as_ref()
            .and_then(|guard| guard.reason_on(home, &receipt, kernel));
        let whole = requested
            .map(|_| super::unused(&cache.root, &|_| Some(super::Unused::Requested)))
            .or_else(|| {
                budget
                    .map(|why| super::unused(&cache.root, &|_| Some(why)))
                    .or_else(|| {
                        gone.then(|| {
                            super::unused(&cache.root, &|seen| {
                                let idle = super::age(now, seen.newest);
                                (idle >= gone_after).then_some(super::Unused::Gone {
                                    idle,
                                    after: gone_after,
                                })
                            })
                        })
                    })
            });
        if let Some(Some((held_cargo, why))) = whole {
            if !same(home, &receipt)
                || read(&cache.receipt_path).as_ref() != Some(&receipt)
                || coordination
                    .as_ref()
                    .is_some_and(|guard| !guard.current(home, &receipt))
                || !fence.clear(&cache.root, &cache.root)
                || !intents.ask(&cache.root, why)
                || !fence.clear(&cache.root, &cache.root)
            {
                continue;
            }
            let Some(parent) = cache.root.parent() else {
                continue;
            };
            let Ok(stage) = aterm_tempfile::Builder::new()
                .prefix("atpkg-cache-retiring-")
                .tempdir_in(parent)
            else {
                continue;
            };
            let directory = stage.keep();
            let Some(stage_id) = identity(&directory) else {
                continue;
            };
            receipt.pending = Some(Pending {
                directory: directory.clone(),
                identity: stage_id,
                authorized: false,
            });
            // Keep the private stage and its receipt on every failure, including a crash.
            if write(&cache.receipt_path, &receipt).is_none() {
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
                gone_after,
                placing,
                moved: false,
                compiler_store: super::compiler_store(Some(home)),
                intents: &mut intents,
                fence,
                permitted: &permitted,
            };
            // `prune` makes a fresh stage; only `walk` finishes older stages now. A registered
            // target is not a mirror tree, so finish just its own stages under this receipt gate.
            for stage in super::stages_in(&cache.root, std::ffi::OsStr::new(super::DOOMED)) {
                if !same(home, &receipt) {
                    break;
                }
                if sweep.stage_recorded(&stage, &cache.root) && same(home, &receipt) {
                    sweep.finish_stage(&stage, &cache.root);
                }
            }
            if same(home, &receipt) {
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
    _: std::time::Duration,
    _: Option<&super::checkout::Placing>,
    _: &mut dyn FnMut(&Path, super::Unused) -> bool,
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

    struct World {
        _directory: aterm_tempfile::TempDir,
        home: PathBuf,
        owner: PathBuf,
        target: PathBuf,
    }

    impl World {
        fn new() -> Self {
            let directory = aterm_tempfile::tempdir().unwrap();
            Self::in_directory(directory)
        }

        fn in_directory(directory: aterm_tempfile::TempDir) -> Self {
            let home = directory.path().canonicalize().unwrap().join("home");
            std::fs::create_dir(&home).unwrap();
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
                Duration::ZERO,
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
        let clear = |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default());
        let held = crate::lock::Flock::try_lock(
            std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap(),
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
        assert!(
            retire_on(
                &world.home,
                &world.target,
                &mut |_, _| true,
                &super::super::fence::Fence::new(&clear)
            )
            .is_err()
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
                Duration::ZERO,
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
    fn fresh_creation_preserves_output_locations_and_never_adopts_an_existing_target() {
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
        std::fs::create_dir_all(protected.parent().unwrap()).unwrap();
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
        std::fs::create_dir(&target).unwrap();
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
                super::super::RECLAIM_GONE,
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
                super::super::RECLAIM_GONE,
                None,
                &mut |_, _| true
            )
            .is_empty()
        );
        drop(held);
        let removed = reclaim(
            &world.home,
            world.later(),
            super::super::RECLAIM_GONE,
            None,
            &mut |_, _| true,
        );
        assert_eq!(removed.len(), 1);
        assert!(!world.target.exists());
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
            super::super::RECLAIM_GONE,
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
                super::super::RECLAIM_GONE,
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
                super::super::RECLAIM_GONE,
                None,
                &mut |p, _| p != stage
            )
            .is_empty()
        );
        assert!(stage.is_dir(), "a refused intent keeps the stage");
        let retried = reclaim(
            &world.home,
            now,
            super::super::RECLAIM_GONE,
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
                super::super::RECLAIM_GONE,
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
            super::super::RECLAIM_GONE,
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
            super::super::RECLAIM_GONE,
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
                super::super::RECLAIM_GONE,
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
                super::super::RECLAIM_GONE,
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
            super::super::RECLAIM_GONE,
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
                    "elsewhere.toml".into(),
                    "-Z".into(),
                    "unstable-options".into(),
                    "-C".into(),
                    "another-directory".into(),
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
            .current_dir(&world.owner)
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
        aterm_uds::deadline::ends_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
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
        // standard tag, outside managed admission. Its tag grants no ownership, and the
        // historical receipt must not acknowledge this unregistered replacement.
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
        let refused = through_clean_front(&world, Path::new("/bin/echo"), &build);
        assert_eq!(refused.status.code(), Some(75), "{refused:?}");
        assert!(!world.target.join(MARKER).exists());
        assert_eq!(
            std::fs::read(world.target.join("CACHEDIR.TAG")).unwrap(),
            foreign_tag
        );
        assert_eq!(
            std::fs::read(world.target.join("keep")).unwrap(),
            b"unknown replacement"
        );
        assert_eq!(std::fs::read(world.path()).unwrap(), recorded);
        let out = through_clean_front(&world, driver, &["clean".into(), target]);
        assert!(out.status.success(), "unregistered cleanup: {out:?}");
        assert!(!world.target.exists());
        assert_eq!(std::fs::read(world.path()).unwrap(), recorded);

        let out = through_clean_front(&world, driver, &build);
        assert!(out.status.success(), "{out:?}");
        let after = read(&world.path()).unwrap();
        assert_ne!(
            after.nonce, before.nonce,
            "a new creation, not the historical receipt"
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
        assert!(
            world.target.join("debug/deps").is_dir(),
            "the actual compiler wrote output"
        );
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
                super::super::RECLAIM_GONE,
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
                    super::super::RECLAIM_GONE,
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
}
