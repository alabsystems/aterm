// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Publish a fully marked cache, never an empty unowned target at its public name.
//!
//! Fresh creation supplies the standard cache tag before exposure. Completed caches keep
//! their existing contents; admission never repairs an older missing or invalid tag.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Creating {
    pub(super) directory: PathBuf,
}

#[cfg(unix)]
use super::{
    Identity, MARKER, Receipt, identity, marker_binds, package, read, retention, scope, write,
};
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
pub(super) fn pin(path: &Path) -> Option<aterm_dirfd::OwnedFd> {
    let fd = aterm_uds::kept_out(|| {
        aterm_dirfd::openat(
            aterm_dirfd::CWD,
            path,
            aterm_dirfd::OFlags::RDONLY
                | aterm_dirfd::OFlags::DIRECTORY
                | aterm_dirfd::OFlags::NOFOLLOW
                | aterm_dirfd::OFlags::CLOEXEC,
            aterm_dirfd::Mode::empty(),
        )
        .map_err(std::io::Error::from)
    })
    .ok()?;
    let captured = aterm_dirfd::fstat(&fd).ok()?;
    let named = identity(path)?;
    (captured.st_dev == named.device
        && captured.st_ino == named.inode
        && retention::private_directory(&fd)
        && path.canonicalize().ok().as_deref() == Some(path))
    .then_some(fd)
}

/// Whether the entry `name` of the folder held as `parent` is the private folder `expected`
/// names, as a record keeps it ([`Identity::names`]): its volume is read by that name only where
/// `expected` keeps a volume's UUID (an interrupted creation resumed in a later boot).
#[cfg(unix)]
pub(super) fn named(
    parent: &aterm_dirfd::OwnedFd,
    name: &std::ffi::OsStr,
    expected: &Identity,
) -> bool {
    aterm_dirfd::fstatat(parent, name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|at| {
        let now = if expected.volume.is_some() {
            Identity::at(std::os::fd::AsFd::as_fd(parent), name, &at)
        } else {
            Some(Identity::unrecorded(at.st_dev, at.st_ino))
        };
        now.is_some_and(|now| expected.names(&now))
            && super::private(at.st_uid, at.st_mode, super::euid())
    })
}

#[cfg(unix)]
fn only_creation_files(root: &aterm_dirfd::OwnedFd) -> Option<bool> {
    let mut entries = aterm_dirfd::Dir::read_from(root).ok()?;
    let mut marker = false;
    let mut tag = false;
    for entry in &mut entries {
        let entry = entry.ok()?;
        match entry.file_name().to_bytes() {
            b"." | b".." => {}
            name if name == MARKER.as_bytes() => marker = true,
            b"CACHEDIR.TAG" => tag = true,
            _ => return None,
        }
    }
    Some(marker && tag && valid_cache_tag(root)?)
}

#[cfg(unix)]
fn cache_tag_bytes() -> String {
    format!("{}\n", aterm_toolchain::lasting::CACHEDIR_TAG_SIGNATURE)
}

/// The tag describes generated cache output; only the separate identity receipt grants custody.
#[cfg(unix)]
fn write_cache_tag(root: &aterm_dirfd::OwnedFd) -> Option<()> {
    use std::io::Write as _;
    let file = aterm_dirfd::openat(
        root,
        "CACHEDIR.TAG",
        aterm_dirfd::OFlags::WRONLY
            | aterm_dirfd::OFlags::CREATE
            | aterm_dirfd::OFlags::EXCL
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC
            | aterm_dirfd::OFlags::NONBLOCK,
        aterm_dirfd::Mode::from_bits(0o600),
    )
    .ok()?;
    let mut file = std::fs::File::from(file);
    file.write_all(cache_tag_bytes().as_bytes()).ok()?;
    aterm_durable::ordered(&file).ok()?;
    sync_directory(root, Point::Tag)
}

#[cfg(unix)]
fn valid_cache_tag(root: &aterm_dirfd::OwnedFd) -> Option<bool> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let file = aterm_dirfd::openat(
        root,
        "CACHEDIR.TAG",
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::NONBLOCK
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(file);
    let before = file.metadata().ok()?;
    let expected = cache_tag_bytes();
    if !before.is_file()
        || before.uid() != super::euid()
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
        || before.len() != expected.len() as u64
        || !retention::private_metadata(&file)
    {
        return Some(false);
    }
    let mut actual = Vec::with_capacity(expected.len() + 1);
    (&file)
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut actual)
        .ok()?;
    let after = file.metadata().ok()?;
    let named =
        aterm_dirfd::fstatat(root, "CACHEDIR.TAG", aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW).ok()?;
    Some(
        actual == expected.as_bytes()
            && before.len() == after.len()
            && before.modified().ok() == after.modified().ok()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && after.nlink() == 1
            && retention::private_metadata(&file)
            && named.st_dev == after.dev()
            && named.st_ino == after.ino(),
    )
}

#[cfg(unix)]
pub(super) fn ensure_parent(home: &Path, parent: &Path) -> Option<aterm_dirfd::OwnedFd> {
    let relative = parent.strip_prefix(home).ok()?;
    let mut held = pin(home)?;
    let mut at = home.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return None;
        };
        at.push(name);
        held = super::directory_child(&held, name, &at, Point::Parent)?;
    }
    Some(held)
}

#[cfg(unix)]
pub(super) fn begin(
    home: &Path,
    record: &Path,
    root: &Path,
    owner: &Path,
    mut policy: Option<package::Policy>,
) -> Option<Receipt> {
    let nonce = aterm_uds::rand::hex_token::<16>().ok()?;
    let parent = root.parent()?;
    let held_parent = ensure_parent(home, parent)?;
    let name = format!(".aterm-cache-creating-{nonce}.noindex");
    aterm_dirfd::mkdirat(
        &held_parent,
        name.as_str(),
        aterm_dirfd::Mode::from_bits(0o700),
    )
    .ok()?;
    let directory = parent.join(name);
    let held_stage = pin(&directory)?;
    if let Some(policy) = &mut policy {
        policy.mark_created();
    }
    let receipt = Receipt {
        schema: 5,
        retained: false,
        removal_fence: None,
        root: root.to_path_buf(),
        identity: identity(&directory)?,
        nonce,
        owners: vec![owner.to_path_buf()],
        owners_unknown: false,
        pending: None,
        creating: Some(Creating { directory }),
        package: policy,
    };
    step(Point::Created)?;
    // Before durable intent a crash can leave only private bookkeeping, never build output.
    // The marker is complete before intent, so recovery never repairs an unproved marker.
    write_cache_tag(&held_stage)?;
    retention::write_marker(&held_stage, &complete(&receipt)).ok()?;
    step(Point::Marker)?;
    if !named(
        &held_parent,
        receipt.creating.as_ref()?.directory.file_name()?,
        &receipt.identity,
    ) || !only_creation_files(&held_stage)?
    {
        return None;
    }
    aterm_durable::primitive::fsync(&held_parent).ok()?;
    write(record, &receipt)?;
    step(Point::Intent)?;
    finish(home, record, &receipt)
}

#[cfg(unix)]
fn complete(receipt: &Receipt) -> Receipt {
    let mut published = receipt.clone();
    published.schema = published
        .package
        .as_ref()
        .map_or(1, package::Policy::schema);
    published.creating = None;
    published
}

/// Resume only an exact staged identity; no pathname or cache tag alone proves creation.
#[cfg(unix)]
pub(super) fn finish(home: &Path, record: &Path, receipt: &Receipt) -> Option<Receipt> {
    let creating = receipt.creating.as_ref()?;
    let parent = receipt.root.parent()?;
    if receipt.schema != 5
        || receipt.retained
        || receipt.pending.is_some()
        || receipt.owners.is_empty()
        || receipt.owners_unknown
        || !scope(home, &receipt.root)
        || !scope(home, &creating.directory)
        || creating.directory.parent() != Some(parent)
        || creating.directory.file_name()?.to_str()?
            != format!(".aterm-cache-creating-{}.noindex", receipt.nonce)
        || read(record).as_ref() != Some(receipt)
        || !receipt
            .package
            .as_ref()
            .is_none_or(|policy| policy.creation_allowed(home))
    {
        return None;
    }
    let held_parent = pin(parent)?;
    let stage_name = creating.directory.file_name()?;
    let root_name = receipt.root.file_name()?;
    let published = complete(receipt);
    match std::fs::symlink_metadata(&receipt.root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let stage = pin(&creating.directory)?;
            if !named(&held_parent, stage_name, &receipt.identity) {
                return None;
            }
            match (only_creation_files(&stage)?, retention::receipt_at(&stage)) {
                (true, Some(marker)) if marker_binds(&marker, &published) => {}
                _ => return None,
            }
            step(Point::Publishing)?;
            if !named(&held_parent, stage_name, &receipt.identity)
                || read(record).as_ref() != Some(receipt)
                || !receipt
                    .package
                    .as_ref()
                    .is_none_or(|policy| policy.creation_allowed(home))
            {
                return None;
            }
            aterm_dirfd::renameat_with(
                &held_parent,
                stage_name,
                &held_parent,
                root_name,
                aterm_dirfd::RenameFlags::NOREPLACE,
            )
            .ok()?;
            aterm_durable::primitive::fsync(&held_parent).ok()?;
            step(Point::Renamed)?;
        }
        Ok(_)
            if std::fs::symlink_metadata(&creating.directory)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
        _ => return None,
    }
    let target = pin(&receipt.root)?;
    if !named(&held_parent, root_name, &receipt.identity)
        || !only_creation_files(&target)?
        || !retention::receipt_at(&target).is_some_and(|marker| marker_binds(&marker, &published))
        || read(record).as_ref() != Some(receipt)
        || !receipt
            .package
            .as_ref()
            .is_none_or(|policy| policy.creation_allowed(home))
    {
        return None;
    }
    aterm_durable::primitive::fsync(&held_parent).ok()?;
    write(record, &published)?;
    step(Point::Committed)?;
    (super::same(home, &published) && read(record).as_ref() == Some(&published))
        .then_some(published)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Point {
    Index,
    Gate,
    Parent,
    Created,
    Tag,
    Intent,
    Marker,
    Publishing,
    Renamed,
    Committed,
    /// An adoption's receipt is on record, its marker not yet published.
    Adopted,
}

#[cfg(all(test, unix))]
#[path = "build_cache_create_faults_tests.rs"]
pub(super) mod faults;

#[cfg(unix)]
pub(super) fn step(point: Point) -> Option<()> {
    #[cfg(test)]
    {
        faults::at(point)
    }
    #[cfg(not(test))]
    {
        let _ = point;
        Some(())
    }
}

/// A missing hierarchy or gate flush must fail before any target can be published.
#[cfg(unix)]
pub(super) fn sync_directory(directory: &aterm_dirfd::OwnedFd, point: Point) -> Option<()> {
    step(point)?;
    aterm_durable::primitive::fsync(directory).ok()
}

#[cfg(all(test, unix))]
#[path = "build_cache_create_tests.rs"]
mod tests;
