// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Register a target cargo made before registration existed, where it stands.
//!
//! Cargo writes its `CACHEDIR.TAG` into a build directory only as it makes it, in a private
//! stage it renames into place; a folder that stood before cargo was pointed at it gets none.
//! It also tags its own home's git and registry caches, which may stand before it. Resolving
//! queries can leave `.rustc_info.json` there without compiling, so that file alone grants no
//! custody. Require a compilation invocation fingerprint in a known Cargo or Trust layout as
//! well, through bounded, owned, non-writable-by-others directories without following links.
//! Adoption records the folder's identity and producer workspace and writes only its custody
//! marker: nothing is moved, replaced or removed. Unrecognized layouts stay unmanaged.

#[cfg(unix)]
use super::{CUSTODY_MARKERS, Identity, MARKER, Receipt, clearance, creation, same, write};
#[cfg(unix)]
use std::path::{Path, PathBuf};

/// What cargo writes into the `CACHEDIR.TAG` of a build directory it makes (cargo 1.47 onward,
/// targo included); atpkg's own tag is its first line alone.
#[cfg(unix)]
pub(super) const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n\
                         # This file is a cache directory tag created by cargo.\n\
                         # For information about cache directory tags see https://bford.info/cachedir/\n";

/// The compiler probe cache, also written by resolving queries.
#[cfg(unix)]
const RUSTC_INFO: &str = ".rustc_info.json";

/// Names at a folder's top that make it source, not a build directory.
#[cfg(unix)]
const SOURCE: &[&str] = &[
    ".git",
    ".hg",
    ".jj",
    ".svn",
    "Cargo.toml",
    "Cargo.lock",
    "Targo.toml",
    "src",
    "build.rs",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// A folder that proved cargo made it ([`proven`]), held, with the path and identity it proved
/// under.
#[cfg(unix)]
pub(super) struct Proven {
    root: PathBuf,
    held: aterm_dirfd::OwnedFd,
    identity: Identity,
}

#[cfg(unix)]
impl Proven {
    /// Whether `receipt` is this folder's adoption, as [`begin`] records it, which [`finish`]
    /// may complete: an ordinary disposable receipt of this path and this identity, with
    /// known owners and nothing pending, kept or being created.
    pub(super) fn resumes(&self, receipt: &Receipt) -> bool {
        receipt.schema == 1
            && !receipt.retained
            && receipt.removal_fence.is_none()
            && receipt.pending.is_none()
            && receipt.creating.is_none()
            && receipt.package.is_none()
            && !receipt.owners_unknown
            && !receipt.owners.is_empty()
            && receipt.root == self.root
            && receipt.identity.names(&self.identity)
    }

    /// Whether `receipt`, ordinary disposable custody of this path (schema 1: no keep, no
    /// package policy), names a folder that no longer stands here, so it binds nothing here and
    /// an adoption replaces it, as a creation does at a path with no folder; reclaim's
    /// settlement drops such a receipt anyway. A receipt of this very folder stays, one it
    /// cannot finish included.
    pub(super) fn supersedes(&self, receipt: &Receipt) -> bool {
        receipt.schema == 1 && !receipt.identity.names(&self.identity)
    }
}

/// `root`, held, once it proves cargo made it as its build directory, read through the folder
/// held: a canonical private folder of this user's, whose top holds cargo's tag
/// ([`cargo_tag`]), no [`SOURCE`] name and no custody marker, whose build leaf, where one
/// stands, holds no custody marker either, which holds the rustc facts of a build ([`built`])
/// at its top or in that leaf, and which no registration but its own (at `record`) holds,
/// within or around it. `None` for any other. The caller has put `root` in [`super::scope`]
/// (so never under `~/Library`, where the shared build cache and the index keep their own
/// custody) and holds the admission lock, so no registration joins that census until it lets
/// go.
#[cfg(unix)]
pub(super) fn proven(home: &Path, root: &Path, record: &Path) -> Option<Proven> {
    clearance(home, root, record.file_name()).ok()?;
    let held = creation::pin(root)?;
    let stat = aterm_dirfd::fstat(&held).ok()?;
    if !source_free(&held)
        || !CUSTODY_MARKERS.iter().all(|name| absent(&held, name))
        || !cargo_tag(&held)
    {
        return None;
    }
    // Hold each component separately: a quiet folder that is a symlink grants no adoption
    // authority over what it reaches.
    let quiet = match aterm_dirfd::fstatat(&held, ".ubd", aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW) {
        Err(error) if error.is_noent() => None,
        Ok(at) if aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir() => Some(
            aterm_dirfd::openat(
                &held,
                ".ubd",
                aterm_dirfd::OFlags::RDONLY
                    | aterm_dirfd::OFlags::DIRECTORY
                    | aterm_dirfd::OFlags::NOFOLLOW
                    | aterm_dirfd::OFlags::CLOEXEC,
                aterm_dirfd::Mode::empty(),
            )
            .ok()?,
        ),
        _ => return None,
    };
    let leaf = match quiet.as_ref().map(|quiet| {
        aterm_dirfd::fstatat(
            quiet,
            "build.noindex",
            aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW,
        )
    }) {
        None => None,
        Some(Err(error)) if error.is_noent() => None,
        Some(Ok(at)) if aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir() => {
            let leaf = aterm_dirfd::openat(
                quiet.as_ref()?,
                "build.noindex",
                aterm_dirfd::OFlags::RDONLY
                    | aterm_dirfd::OFlags::DIRECTORY
                    | aterm_dirfd::OFlags::NOFOLLOW
                    | aterm_dirfd::OFlags::CLOEXEC,
                aterm_dirfd::Mode::empty(),
            )
            .ok()?;
            if !CUSTODY_MARKERS.iter().all(|name| absent(&leaf, name)) {
                return None;
            }
            Some(leaf)
        }
        _ => return None,
    };
    if !built(&held) && !leaf.as_ref().is_some_and(built) {
        return None;
    }
    // Recorded by the adoption, so with its volume.
    let identity = Identity::held(&held)?;
    if (identity.device, identity.inode) != (stat.st_dev, stat.st_ino) {
        return None;
    }
    Some(Proven {
        root: root.to_path_buf(),
        held,
        identity,
    })
}

/// Record the folder `proven` holds for `owner` at `record`: the receipt first, inert until
/// its marker binds it ([`same`]), then the marker ([`finish`]). Called under the admission
/// lock and the root's gate, with no receipt at `record` or one this adoption supersedes
/// ([`Proven::supersedes`]). `None` when a write fails or the folder changed; a receipt left on
/// record without its marker is finished by a later call.
#[cfg(unix)]
pub(super) fn begin(home: &Path, record: &Path, owner: &Path, proven: &Proven) -> Option<Receipt> {
    let receipt = Receipt {
        schema: 1,
        retained: false,
        removal_fence: None,
        root: proven.root.clone(),
        identity: proven.identity.clone(),
        nonce: aterm_uds::rand::hex_token::<16>().ok()?,
        owners: vec![owner.to_path_buf()],
        owners_unknown: false,
        pending: None,
        creating: None,
        package: None,
    };
    write(record, &receipt)?;
    creation::step(creation::Point::Adopted)?;
    finish(home, &receipt, proven)
}

/// Publish the marker of `receipt`, the adoption on record that `proven` resumes
/// ([`Proven::resumes`]), never over another marker, and keep it only once it binds
/// ([`same`]): a folder renamed away meanwhile, or whose path no longer proves its custody,
/// has the marker it just got taken back, so none names a path its folder left, and the
/// receipt stays on record unfinished. Called under the admission lock and the root's gate,
/// which every writer of the record holds.
#[cfg(unix)]
pub(super) fn finish(home: &Path, receipt: &Receipt, proven: &Proven) -> Option<Receipt> {
    publish_marker(&proven.held, receipt)?;
    if same(home, receipt) {
        return Some(receipt.clone());
    }
    let _ = aterm_dirfd::unlinkat(&proven.held, MARKER, aterm_dirfd::AtFlags::empty());
    None
}

/// Write `receipt` as the marker of the folder `root` holds: a fresh private file, flushed,
/// renamed to [`MARKER`] only where none stands, the folder flushed after. A file that never
/// became the marker is unlinked.
#[cfg(unix)]
fn publish_marker(root: &aterm_dirfd::OwnedFd, receipt: &Receipt) -> Option<()> {
    use std::io::Write as _;
    let bytes = aterm_json::to_vec(receipt).ok()?;
    if bytes.len() > super::MAX_RECORD {
        return None;
    }
    let name = format!(
        ".aterm-adopting-{}",
        aterm_uds::rand::hex_token::<16>().ok()?
    );
    let file = aterm_dirfd::openat(
        root,
        name.as_str(),
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
    let published = file.write_all(&bytes).is_ok()
        && aterm_durable::ordered(&file).is_ok()
        && aterm_dirfd::renameat_with(
            root,
            name.as_str(),
            root,
            MARKER,
            aterm_dirfd::RenameFlags::NOREPLACE,
        )
        .is_ok();
    if !published {
        let _ = aterm_dirfd::unlinkat(root, name.as_str(), aterm_dirfd::AtFlags::empty());
        return None;
    }
    aterm_durable::primitive::fsync(root).ok()
}

/// Whether nothing named `name` stands in the folder `dir` holds, its absence known.
#[cfg(unix)]
fn absent(dir: &aterm_dirfd::OwnedFd, name: &str) -> bool {
    aterm_dirfd::fstatat(dir, name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
        .is_err_and(|error| error.is_noent())
}

/// Whether the held directory contains none of the bounded source-root declarations.
#[cfg(unix)]
pub(super) fn source_free(dir: &aterm_dirfd::OwnedFd) -> bool {
    SOURCE.iter().all(|name| absent(dir, name))
}

/// Whether the compiler probe is a regular file, for read-only layout observation only.
#[cfg(unix)]
pub(super) fn compiler_info(dir: &aterm_dirfd::OwnedFd) -> bool {
    aterm_dirfd::fstatat(dir, RUSTC_INFO, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|at| aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_file())
}

/// Compiler invocation evidence, not a claim that compilation succeeded. Resolving queries
/// write the probe but never an invocation fingerprint. Profiles can have arbitrary names;
/// cross compilation adds one target component. Trust keeps fingerprints in its unit layout.
#[cfg(unix)]
fn built(dir: &aterm_dirfd::OwnedFd) -> bool {
    if !compiler_info(dir) {
        return false;
    }
    let layouts: &[&[Option<&str>]] = &[
        &[None, Some(".fingerprint"), None],
        &[None, None, Some(".fingerprint"), None],
        &[None, Some("build"), None, None, Some("fingerprint")],
        &[None, None, Some("build"), None, None, Some("fingerprint")],
    ];
    let mut remaining = 16_384;
    layouts
        .iter()
        .any(|layout| invocation_at(dir, layout, &mut remaining) == Some(true))
}

/// Only open a held directory's direct child, without following a link or trusting a
/// directory writable by another user. Its owner can already create the tag we require.
#[cfg(unix)]
fn evidence_dir(
    parent: &aterm_dirfd::OwnedFd,
    name: &std::ffi::CStr,
) -> Option<aterm_dirfd::OwnedFd> {
    use std::os::unix::ffi::OsStrExt as _;
    let held = aterm_dirfd::openat(
        parent,
        std::ffi::OsStr::from_bytes(name.to_bytes()),
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::DIRECTORY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::empty(),
    )
    .ok()?;
    let at = aterm_dirfd::fstat(&held).ok()?;
    super::private(at.st_uid, at.st_mode, super::euid()).then_some(held)
}

/// A fixed layout bounds depth; one shared entry budget bounds the whole observation.
/// An unreadable or oversized search never supplies custody evidence.
#[cfg(unix)]
fn invocation_at(
    dir: &aterm_dirfd::OwnedFd,
    layout: &[Option<&str>],
    remaining: &mut usize,
) -> Option<bool> {
    let Some((component, rest)) = layout.split_first() else {
        return Some(
            aterm_dirfd::fstatat(
                dir,
                "invoked.timestamp",
                aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW,
            )
            .is_ok_and(|at| {
                aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_file()
                    && super::private(at.st_uid, at.st_mode, super::euid())
                    && at.st_nlink == 1
            }),
        );
    };
    if *remaining == 0 {
        return None;
    }
    if let Some(name) = component {
        *remaining -= 1;
        let name = std::ffi::CString::new(*name).ok()?;
        return match evidence_dir(dir, &name) {
            Some(child) => invocation_at(&child, rest, remaining),
            None => Some(false),
        };
    }
    let mut entries = aterm_dirfd::Dir::read_from(dir).ok()?;
    for entry in &mut entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        *remaining = remaining.checked_sub(1)?;
        if let Some(child) = evidence_dir(dir, name)
            && invocation_at(&child, rest, remaining)?
        {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether the folder `root` holds has cargo's tag at its top: a file of this user's, no link,
/// its only name, writable by no group or other user, that reads as [`CARGO_TAG`] exactly.
#[cfg(unix)]
pub(super) fn cargo_tag(root: &aterm_dirfd::OwnedFd) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let Ok(file) = aterm_dirfd::openat(
        root,
        "CACHEDIR.TAG",
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::NONBLOCK
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::empty(),
    ) else {
        return false;
    };
    let file = std::fs::File::from(file);
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.uid() != super::euid()
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.len() != CARGO_TAG.len() as u64
    {
        return false;
    }
    let mut bytes = Vec::with_capacity(CARGO_TAG.len() + 1);
    (&file)
        .take(CARGO_TAG.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .is_ok()
        && bytes == CARGO_TAG.as_bytes()
}

#[cfg(all(test, unix))]
#[path = "build_cache_adopt_tests.rs"]
mod tests;
