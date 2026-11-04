// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit keep-only custody for an existing cache. This never makes output disposable.

use super::MARKER;
#[cfg(unix)]
use super::Receipt;
use std::path::{Path, PathBuf};

/// A durable keep a live removal alone may temporarily exempt. This is not deletion
/// authority: other participating walks and schema4-aware readers retain it after a crash.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct FenceDeclaration {
    token: String,
    original_schema: Option<u32>,
}

impl FenceDeclaration {
    pub(super) fn valid(&self) -> bool {
        self.token.len() == 32
            && self.token.bytes().all(|byte| byte.is_ascii_hexdigit())
            && self
                .original_schema
                .is_none_or(|schema| matches!(schema, 1..=3))
    }
}

/// Read-only discovery for the keep-only API, not proof that another host's cleaner is new.
pub const CAPABILITY: &str = "aterm-cache-retain-v1";

/// Keep one existing, canonical private cache root. There is deliberately no release or
/// remove operation: this API supplies no deletion authority or disposable-package policy.
///
/// Schema 4 makes older registered-cache readers refuse the root. Ordinary cleaners must
/// also honor custody markers; aterm 0.147.0 did not, so that host must be updated before
/// these declarations can protect against its ordinary disk cleanup.
///
/// # Errors
/// The root is absent, ambiguous, not private, contains source, belongs to a legacy cleaner
/// class, or its registration, locks, identity or durable publication cannot be established.
#[cfg(unix)]
pub fn retain(home: &Path, root: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let home = home
        .canonicalize()
        .map_err(|_| "cache home is unavailable")?;
    if !super::scope(&home, root) || root.canonicalize().ok().as_deref() != Some(root) {
        return Err("retention needs an exact canonical cache path under this home".into());
    }
    compatible_placement(&home, root)?;
    let captured = super::identity(root).ok_or("cache root is not a private owned directory")?;
    let pinned = aterm_dirfd::openat(
        aterm_dirfd::CWD,
        root,
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::DIRECTORY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::empty(),
    )
    .map_err(|_| "cache root cannot be pinned")?;
    let pinned_identity = aterm_dirfd::fstat(&pinned).map_err(|_| "cache root cannot be proved")?;
    if pinned_identity.st_dev != captured.device || pinned_identity.st_ino != captured.inode {
        return Err("cache root changed before it was pinned".into());
    }
    if !private_directory(&pinned) {
        return Err("cache root is not privately writable through its held directory".into());
    }
    if [".git", "Cargo.toml", "Targo.toml"].iter().any(|name| {
        !std::fs::symlink_metadata(root.join(name))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    }) {
        return Err("retention does not reclassify a source directory as a cache".into());
    }
    let index = super::index(&home).ok_or("cache index is unavailable")?;
    let admission = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(index.join("locks.noindex/admission.lock"))
        .map_err(|_| "cache admission lock is unavailable")?;
    let _admission =
        crate::lock::Flock::try_lock(admission).map_err(|_| "cache admission is busy")?;
    // A staged payload keeps the original registration and gate. Taking a gate at the
    // payload's new pathname would not exclude the transaction already retiring it.
    let marker = match std::fs::symlink_metadata(root.join(MARKER)) {
        Ok(_) => Some(
            super::read(&root.join(MARKER))
                .ok_or("cache custody marker is unreadable or unsupported")?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("cache custody marker cannot be inspected".into()),
    };
    let registered_root = marker
        .as_ref()
        .map_or(root, |receipt| receipt.root.as_path());
    if !super::scope(&home, registered_root) {
        return Err("cache custody names an invalid original root".into());
    }
    let key = super::key(registered_root).ok_or("cache path cannot be addressed")?;
    let path = index.join(format!("{key}.json"));
    let gate = super::open_gate(&super::gate_path(&path).ok_or("cache gate is unavailable")?)
        .ok_or("cache gate is unavailable")?;
    let _held = crate::lock::Flock::try_lock(gate).map_err(|_| "cache root is busy")?;
    // Serialize marker CAS with a deep walk's provisional keep, on the actual directory
    // inode. No pathname lock or replaceable marker inode can stand in for this lock.
    let _publication = publication_lock(&pinned).ok_or("cache custody publication is busy")?;
    let held_marker = read_marker(&pinned).map_err(|()| "cache custody marker changed")?;
    if held_marker.as_ref().map(|value| &value.receipt) != marker.as_ref() {
        return Err("cache custody marker changed before publication".into());
    }
    let mut receipt = match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let existing =
                super::read(&path).ok_or("cache receipt is unreadable or unsupported")?;
            if !bound(&home, root, &captured, &existing) {
                return Err("cache registration or marker no longer binds this directory".into());
            }
            existing
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Do not independently retain a descendant an existing registered cleaner can
            // stage by renaming an ancestor. Exact roots and Pending payloads use their
            // original gate above; unknown index state is a refusal, never empty custody.
            super::ordinary_clearance(&home, root)?;
            if super::custody_present(root)
                && !held_marker.as_ref().is_some_and(|marker| {
                    marker.receipt.removal_fence.as_ref().is_some_and(|fence| {
                        fence.original_schema.is_none()
                            && marker.receipt.root == root
                            && marker.receipt.identity == captured
                    })
                })
            {
                return Err("existing custody has no supported matching registration".into());
            }
            if super::record_count(&index).is_none_or(|count| count >= super::MAX_ROOTS) {
                return Err("cache index is full or unreadable".into());
            }
            Receipt {
                schema: 4,
                retained: true,
                removal_fence: None,
                root: root.to_path_buf(),
                identity: captured.clone(),
                nonce: aterm_uds::rand::hex_token::<16>()
                    .map_err(|_| "cache retention token is unavailable")?,
                owners: Vec::new(),
                owners_unknown: true,
                pending: None,
                creating: None,
                package: None,
            }
        }
        Err(_) => return Err("cache receipt cannot be inspected".into()),
    };
    if receipt.identity != captured || super::identity(root).as_ref() != Some(&captured) {
        return Err("cache root was replaced during retention".into());
    }
    receipt.schema = 4;
    receipt.retained = true;
    // An explicit keep must revoke the live remover's exemption even if it was already
    // looking at schema4 retained=true. Never acknowledge that remover's private token.
    receipt.removal_fence = None;
    // Publish the refusal at the actual root before the index. A failed second write is
    // conservative: old and new readers then see an unsupported or mismatched declaration.
    write_marker(&pinned, &receipt)?;
    if super::identity(root).as_ref() != Some(&captured) {
        return Err("cache root changed before retention publication".into());
    }
    super::write(&path, &receipt).ok_or("cache retention index was not durable")?;
    if !bound(&home, root, &captured, &receipt) || super::read(&path).as_ref() != Some(&receipt) {
        return Err("cache retention changed before acknowledgement".into());
    }
    Ok(root.to_path_buf())
}

#[cfg(unix)]
fn bound(home: &Path, actual: &Path, captured: &super::Identity, receipt: &Receipt) -> bool {
    if &receipt.identity != captured {
        return false;
    }
    if receipt.root == actual && super::same(home, receipt) {
        return true;
    }
    let marker = super::read(&actual.join(MARKER));
    let original = marker.clone().and_then(|mut marker| {
        let fence = marker.removal_fence.take()?;
        if fence.original_schema != Some(receipt.schema) || receipt.retained {
            return None;
        }
        marker.schema = receipt.schema;
        marker.retained = false;
        Some(marker)
    });
    let provisional = original
        .as_ref()
        .is_some_and(|marker| super::marker_binds(marker, receipt));
    if provisional
        && receipt.root == actual
        && super::scope(home, actual)
        && actual.canonicalize().ok().as_deref() == Some(actual)
        && super::identity(actual).as_ref() == Some(captured)
    {
        return true;
    }
    receipt.pending.as_ref().is_some_and(|pending| {
        pending.directory.join(super::PAYLOAD) == actual
            && super::pending_owned(home, receipt, pending)
            && super::identity(actual).as_ref() == Some(captured)
            && marker.is_some_and(|marker| {
                if marker.removal_fence.is_some() {
                    // The creation marker's roster and package state can predate the index,
                    // and Pending is index-only. Keep its established declaration binding
                    // once the exact stage is proved; publish the full index receipt below.
                    provisional
                } else {
                    super::marker_binds(&marker, receipt)
                }
            })
    })
}

#[cfg(not(unix))]
pub fn retain(_: &Path, _: &Path) -> Result<PathBuf, String> {
    Err("cache retention is not supported on this platform".into())
}

/// Older fixed-root cleaners predate custody-marker checks. Do not promise protection
/// there, or inside a retired verify snapshot/cells tree that one of them can take whole.
#[cfg(unix)]
fn compatible_placement(home: &Path, root: &Path) -> Result<(), String> {
    let fixed = [
        super::super::cache_root(home),
        super::super::moved_from(home),
        super::super::tooling::root(home),
        home.join("Library/Caches/aterm-verify"),
    ];
    let legacy = root.ancestors().any(|at| {
        at.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.ends_with("-verify.noindex") || name.ends_with("-verify-cells.noindex")
            })
    });
    if legacy
        || fixed
            .iter()
            .any(|at| root.starts_with(at) || at.starts_with(root))
    {
        return Err("an older fixed-root cleaner cannot honor retention at this placement".into());
    }
    Ok(())
}

/// A registered mutation remains permitted only while its exact declaration is unchanged
/// and not retained. Unknown state revokes an in-progress descriptor walk too.
#[cfg(unix)]
pub(super) fn allows(path: &Path, receipt: &Receipt) -> bool {
    !receipt.retained && super::read(path).as_ref() == Some(receipt)
}

/// Check the actual path and its ancestors at staging and deletion boundaries. Unknown
/// marker contents retain, while an ordinary schema1 marker remains under its own gate. A path
/// more than 64 folders deep is never allowed.
pub(in crate::build_cache) fn allows_tree(path: &Path) -> bool {
    path.ancestors().nth(64).is_none() && kept_by(path).is_none()
}

/// The nearest of `path` and the folders above it (the first 64) whose custody, read by its
/// path as [`allows_tree`] reads it, keeps it: a keep, a marker that cannot be read, or an
/// ordinary one that binds another folder or one that is not private. `None` where every one
/// allows.
pub(in crate::build_cache) fn kept_by(path: &Path) -> Option<&Path> {
    path.ancestors().take(64).find(|at| {
        let marker = at.join(MARKER);
        match std::fs::symlink_metadata(&marker) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Ok(_) => !super::read(&marker).is_some_and(|receipt| {
                !receipt.retained && super::identity(at).as_ref() == Some(&receipt.identity)
            }),
            Err(_) => true,
        }
    })
}

/// [`allows_tree`] as a removal walk of `top` is gated
/// ([`crate::build_cache::remove::tree_while`]): on Unix for the folders above `top` alone,
/// since that walk pins `top` and reads its own custody through the held folder ([`allows_at`])
/// before changing it. To repair denied search, the walk first proves by a complete name scan
/// that no custody marker is hidden there; a hidden marker keeps the folder unchanged. A path
/// check of `top` here would reject even a marker-free folder before the walk can establish
/// that absence. Off Unix, where the walk is `std::fs::remove_dir_all`, this checks `top` too.
pub(in crate::build_cache) fn allows_walk(top: &Path) -> bool {
    if cfg!(unix) {
        top.parent().is_none_or(allows_tree)
    } else {
        allows_tree(top)
    }
}

/// Read through a pinned directory, including after a rename. A live directory stack in
/// the remover holds these identities until it leaves each directory; path replacement
/// cannot shed an ancestor's newly published keep or unreadable declaration. An ordinary
/// marker binds the folder held only as [`allows_tree`] binds one by its path: the very folder
/// it names, private to this user ([`super::private`]); in any other it retains.
#[cfg(unix)]
pub(in crate::build_cache) fn allows_at(fd: impl std::os::fd::AsFd) -> bool {
    let root = fd.as_fd();
    let Ok(identity) = aterm_dirfd::fstat(root) else {
        return false;
    };
    let receipt = match read_marker(root) {
        Ok(None) => return true,
        Ok(Some(marker)) => marker.receipt,
        Err(()) => return false,
    };
    !receipt.retained
        && receipt.identity.device == identity.st_dev
        && receipt.identity.inode == identity.st_ino
        && super::private(identity.st_uid, identity.st_mode, super::euid())
}

#[cfg(unix)]
struct HeldMarker {
    identity: super::Identity,
    bytes: Vec<u8>,
    receipt: Receipt,
}

/// The same bounded, private, stable marker read used by retention, from the pinned directory.
#[cfg(unix)]
pub(super) fn receipt_at(root: impl std::os::fd::AsFd) -> Option<Receipt> {
    read_marker(root).ok()?.map(|marker| marker.receipt)
}

/// The held directory admits no other writer. Read/search mode bits are compatible;
/// an ACL grant, wrong kind/owner or unavailable judgement never is.
#[cfg(unix)]
pub(super) fn private_directory(root: impl std::os::fd::AsFd) -> bool {
    let Ok(copy) = root.as_fd().try_clone_to_owned() else {
        return false;
    };
    match astream_sys::owner_private_dir(&std::fs::File::from(copy)) {
        Ok(Ok(_)) => true,
        Ok(Err(astream_sys::NotPrivate::Mode(mode))) => mode & 0o022 == 0,
        _ => false,
    }
}

#[cfg(unix)]
pub(super) fn private_metadata(file: &std::fs::File) -> bool {
    match astream_sys::owner_private(file) {
        Ok(Ok(_)) => true,
        Ok(Err(astream_sys::NotPrivate::Mode(mode))) => mode & 0o022 == 0,
        _ => false,
    }
}

#[cfg(unix)]
fn read_marker(root: impl std::os::fd::AsFd) -> Result<Option<HeldMarker>, ()> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let opened = aterm_dirfd::openat(
        root.as_fd(),
        MARKER,
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC
            | aterm_dirfd::OFlags::NONBLOCK,
        aterm_dirfd::Mode::empty(),
    );
    let mut file = match opened {
        Err(error) if error.is_noent() => return Ok(None),
        Ok(fd) => std::fs::File::from(fd),
        Err(_) => return Err(()),
    };
    let metadata = file.metadata().map_err(|_| ())?;
    if !metadata.is_file()
        || metadata.len() > super::MAX_RECORD as u64
        || metadata.uid() != crate::platform::our_uid()
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || !private_metadata(&file)
    {
        return Err(());
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len().saturating_add(1)).unwrap_or(0));
    if std::io::Read::by_ref(&mut file)
        .take(super::MAX_RECORD as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > super::MAX_RECORD
    {
        return Err(());
    }
    let after = file.metadata().map_err(|_| ())?;
    if after.len() != bytes.len() as u64
        || after.nlink() != 1
        || after.modified().ok() != metadata.modified().ok()
        || !private_metadata(&file)
    {
        return Err(());
    }
    let receipt = super::decode(&bytes).ok_or(())?;
    Ok(Some(HeldMarker {
        identity: super::Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        bytes,
        receipt,
    }))
}

/// A nonblocking inode lock shared by explicit keep publication, provisional keep CAS and
/// custody-marker deletion. Unsupported directory locking refuses before any move.
#[cfg(unix)]
pub(in crate::build_cache) fn publication_lock(
    root: impl std::os::fd::AsFd,
) -> Option<crate::lock::Flock> {
    use aterm_dirfd::{Mode, OFlags};
    let fd = aterm_dirfd::openat(
        root.as_fd(),
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    crate::lock::Flock::try_lock(std::fs::File::from(fd)).ok()
}

/// A non-cloneable, pass-local exemption from one exact durable provisional keep. Dropping
/// it does not remove the keep: a stopped/crashed walk has no future deletion authority.
#[cfg(unix)]
pub(in crate::build_cache) struct WalkFence {
    root: super::Identity,
    previous: Option<Receipt>,
    marker: HeldMarker,
}

#[cfg(unix)]
impl WalkFence {
    /// Arm the top before any deep rename can escape an ancestor. Preserve every ordinary
    /// receipt field; publishing schema4 never converts its index into deletion authority.
    pub(in crate::build_cache) fn arm(
        root: &aterm_dirfd::OwnedFd,
        path: &Path,
        prepare_unmarked: impl FnOnce() -> bool,
    ) -> Option<Self> {
        let _publication = publication_lock(root)?;
        let held = aterm_dirfd::fstat(root).ok()?;
        if held.st_uid != super::euid() {
            return None;
        }
        let identity = super::Identity {
            device: held.st_dev,
            inode: held.st_ino,
        };
        let previous = read_marker(root).ok()?.map(|marker| marker.receipt);
        if previous
            .as_ref()
            .is_some_and(|receipt| receipt.retained || receipt.identity != identity)
        {
            return None;
        }
        if !private_directory(root)
            && (previous.is_some()
                || !custody_absent(root)
                || !prepare_unmarked()
                || !private_directory(root))
        {
            return None;
        }
        let mut receipt = match previous.clone() {
            Some(receipt) => receipt,
            None => Receipt {
                schema: 4,
                retained: true,
                removal_fence: None,
                root: path.canonicalize().ok()?,
                identity: identity.clone(),
                nonce: aterm_uds::rand::hex_token::<16>().ok()?,
                owners: Vec::new(),
                owners_unknown: true,
                pending: None,
                creating: None,
                package: None,
            },
        };
        receipt.removal_fence = Some(FenceDeclaration {
            token: aterm_uds::rand::hex_token::<16>().ok()?,
            original_schema: previous.as_ref().map(|receipt| receipt.schema),
        });
        receipt.schema = 4;
        receipt.retained = true;
        write_marker(root, &receipt).ok()?;
        let marker = read_marker(root).ok()??;
        if marker.receipt != receipt {
            return None;
        }
        Some(Self {
            root: identity,
            previous,
            marker,
        })
    }

    /// Only the originally pinned root and the exact published inode and bytes qualify.
    /// A same-valued replacement, an explicit keep, unreadability or another nonce revokes.
    pub(in crate::build_cache) fn owns(&self, root: impl std::os::fd::AsFd) -> bool {
        aterm_dirfd::fstat(root.as_fd()).is_ok_and(|held| {
            held.st_dev == self.root.device
                && held.st_ino == self.root.inode
                && super::private(held.st_uid, held.st_mode, super::euid())
        }) && private_directory(root.as_fd())
            && read_marker(root).is_ok_and(|marker| {
                marker.is_some_and(|marker| {
                    marker.identity == self.marker.identity && marker.bytes == self.marker.bytes
                })
            })
    }

    pub(in crate::build_cache) fn allows_at(&self, root: impl std::os::fd::AsFd) -> Option<bool> {
        let held = aterm_dirfd::fstat(root.as_fd()).ok()?;
        (held.st_dev == self.root.device && held.st_ino == self.root.inode).then(|| self.owns(root))
    }

    /// Restore only our own declaration, once all escaped moves are gone or durably back
    /// under their original ancestors. Never restore over a concurrent explicit keep.
    pub(in crate::build_cache) fn disarm(&self, root: &aterm_dirfd::OwnedFd) -> bool {
        let Some(_publication) = publication_lock(root) else {
            return false;
        };
        if !self.owns(root) {
            return false;
        }
        if let Some(previous) = &self.previous {
            write_marker(root, previous).is_ok()
        } else {
            aterm_dirfd::unlinkat(root, MARKER, aterm_dirfd::AtFlags::empty()).is_ok()
                && aterm_dirfd::fsync(root).is_ok()
        }
    }
}

/// An independent directory cursor must prove every custody spelling absent before any
/// unmarked root is sealed. It never rewinds the remover's already active top iterator.
#[cfg(unix)]
fn custody_absent(root: impl std::os::fd::AsFd) -> bool {
    let Ok(mut names) = aterm_dirfd::Dir::read_from(root) else {
        return false;
    };
    names.all(|entry| {
        entry.is_ok_and(|entry| {
            !super::CUSTODY_MARKERS.iter().any(|marker| {
                entry
                    .file_name()
                    .to_bytes()
                    .eq_ignore_ascii_case(marker.as_bytes())
            })
        })
    })
}

#[cfg(unix)]
pub(super) fn write_marker(root: &aterm_dirfd::OwnedFd, receipt: &Receipt) -> Result<(), String> {
    use std::io::Write as _;
    let name = format!(
        ".aterm-retain-{}",
        aterm_uds::rand::hex_token::<16>()
            .map_err(|_| "cache retention publication token is unavailable")?
    );
    let file = aterm_dirfd::openat(
        root,
        name.as_str(),
        aterm_dirfd::OFlags::WRONLY
            | aterm_dirfd::OFlags::CREATE
            | aterm_dirfd::OFlags::EXCL
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::from_bits(0o600),
    )
    .map_err(|_| "cache retention marker cannot be created")?;
    let mut file = std::fs::File::from(file);
    let bytes = aterm_json::to_vec(receipt).map_err(|_| "cache retention cannot be encoded")?;
    if bytes.len() > super::MAX_RECORD {
        return Err("cache retention exceeds the receipt bound".into());
    }
    file.write_all(&bytes)
        .map_err(|_| "cache retention marker cannot be written")?;
    aterm_durable::ordered(&file).map_err(|_| "cache retention marker was not durable")?;
    aterm_dirfd::renameat(root, name.as_str(), root, MARKER)
        .map_err(|_| "cache retention marker cannot be published")?;
    aterm_dirfd::fsync(root).map_err(|_| "cache retention directory was not durable")?;
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "build_cache_retain_tests.rs"]
mod tests;
