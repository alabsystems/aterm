// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Register a target cargo made before registration existed, where it stands.
//!
//! Cargo writes its `CACHEDIR.TAG` into a build directory only as it makes it, in a private stage
//! it renames into place; a folder that stood before cargo was pointed at it gets none. It also
//! tags its own home's git and registry caches where they stand, and a build writes wherever its
//! target names, those caches included, so they are told by their names in a cargo home and by what
//! they hold ([`home_cache`]); no custody, created, adopted or already on record, covers one. A
//! compilation leaves, in its build directory, the rustc facts cargo keeps (`.rustc_info.json`)
//! beside a profile `Layout::new` laid there: its build lock ([`BUILD_LOCK`]) or the folders its
//! build prepared ([`PREPARED`]), none of which it lays as a link or a file, and, in that layout,
//! the invocation fingerprint of a unit it compiled ([`invoked`]). A `clean -p` leaves the facts
//! beside a profile that holds its build lock and either nothing a build prepares (one before any
//! build) or the folders a build prepared with the units it cleaned taken out ([`cleaned`]); where
//! it took every unit a build compiled, no unit's folder stands in any profile of that folder
//! ([`unit_free`]). A
//! resolving verb (`metadata`, `tree`, `fetch`, `generate-lockfile`) only rewrites those facts in a
//! build directory that already stands, and lays no profile and no fingerprint. So that tag,
//! cargo's exact bytes in this user's own file at the top of this user's private folder, with the
//! facts and either a laid profile and a compilation's fingerprint, or a profile `clean -p` left
//! and no unit's folder, together at its top or in its build leaf, no source, no custody, and
//! nothing of a cargo home's cache, proves cargo made the folder as its build directory and
//! compiled, prepared a compilation or ran `clean -p` in it. The fingerprint and the profile
//! `clean -p` left are read through bounded, owned folders no group or other user can write,
//! without following links. Adoption records the folder's own identity and the producer's
//! workspace, as creation does, and writes nothing in it but the custody marker: nothing is moved,
//! replaced or removed. Any other existing folder stays unmanaged, and a producer requiring
//! managed output refuses it: among them, a build directory whose every laid profile `clean
//! --profile` (or `--release`) removed, which keeps only the facts a resolving verb also leaves,
//! and one that proves itself (at its top, or in its build leaf) by a profile `clean -p` left
//! while a profile there keeps a unit's folder with no unit invoked (a build cut short after
//! `prepare_init` made its units' folders and before its first unit's job). A dry run (`install
//! --dry-run`; Targo's `build` takes no `--dry-run`) prepares its profile and runs no job: in the
//! new layout, Targo 16387's default, `BuildRunner::prepare` makes every unit's folder, so it is
//! refused as a build cut short is; in the old layout it makes none, which a `clean -p` of every
//! package a build compiled leaves too, so it reads as that and is adopted.

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

/// The rustc facts cargo keeps at the top of a build directory once it resolves a workspace
/// there: a build writes them, and so does a resolving verb in a build directory that already
/// stands, so they count only beside a profile `Layout::new` laid ([`built`]).
#[cfg(unix)]
const RUSTC_INFO: &str = ".rustc_info.json";

/// The folders Targo's `BuildDirLayout::prepare` makes in a profile folder of the build
/// directory, in either layout, as a compilation (or an `install --dry-run`) starts: it is reached
/// only through `Layout::prepare`, which only `BuildRunner::prepare` calls, from `compile_ws`.
#[cfg(unix)]
const PREPARED: [&str; 2] = ["build", "incremental"];

/// The lock `Layout::new` lays in the build directory's profile folder (`build_dest`), wherever
/// that directory is not on NFS, for a build and for `clean -p` alike: no resolving verb reaches
/// it.
#[cfg(unix)]
const BUILD_LOCK: &str = ".cargo-build-lock";

/// The most top entries [`prepared`] reads: once it read this many with no laid profile
/// among them, the folder proves nothing.
#[cfg(unix)]
const MAX_TOP: usize = 256;

/// The names a cargo home's git and registry caches take (`<home>/git`, `<home>/registry`), the
/// only folders Targo tags without making them as a build directory (its
/// `exclude_from_backups_and_indexing`, called by the git and registry sources alone). Targo
/// derives both from its home alone, so the name means a cache only in a cargo home
/// ([`HOME_MARKS`]).
#[cfg(unix)]
const HOME_CACHE_NAMES: &[&str] = &["git", "registry"];

/// What Targo writes at the top of its home and nowhere else: the package cache's locks
/// (`.package-cache`, which it holds before it tags or fills `<home>/git`, and
/// `.package-cache-mutate`) and its global cache tracker (`.global-cache`). Its default home,
/// `~/.cargo`, is a cargo home whatever it holds.
#[cfg(unix)]
const HOME_MARKS: &[&str] = &[".package-cache", ".package-cache-mutate", ".global-cache"];

/// What Targo keeps in a cargo home's caches: `<home>/git/{db,checkouts}` and
/// `<home>/registry/{index,cache}` (its `src`, a [`SOURCE`] name, already refuses). A target or
/// build directory holds one of these names at its top only as a custom profile's folder
/// (`[profile.cache]`), which holds that profile's lock ([`super::super::CARGO_LOCKS`]), or as
/// a custom target's (`--target cache.json`: `Layout::new` names it the file's stem), whose
/// profiles one level down hold theirs; a folder that holds one otherwise is such a cache,
/// whatever its name, as when a cargo home's cache is a link to it ([`keeps_cache`]).
#[cfg(unix)]
const HOME_CACHE: &[&str] = &["db", "checkouts", "index", "cache"];

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
/// held: a canonical private folder of this user's, no cargo home's cache ([`home_cache`]), whose
/// top holds cargo's tag ([`cargo_tag`]), no [`SOURCE`] name, nothing a cargo home's cache keeps
/// ([`keeps_cache`]) and no custody marker, whose build leaf, where one stands, holds no custody
/// marker either, which holds the rustc facts beside a profile `Layout::new` laid ([`built`]) at
/// its top or in that leaf, and which no registration but its own (at `record`) holds, within or
/// around it. `None` for any other. The caller has put `root` in [`super::scope`] (so never under
/// `~/Library`, where the shared build cache and the index keep their own custody) and holds the
/// admission lock, so no registration joins that census until it lets go.
#[cfg(unix)]
pub(super) fn proven(home: &Path, root: &Path, record: &Path) -> Option<Proven> {
    if home_cache(home, root) {
        return None;
    }
    clearance(home, root, record.file_name()).ok()?;
    let held = creation::pin(root)?;
    let stat = aterm_dirfd::fstat(&held).ok()?;
    if !source_free(&held)
        || !CUSTODY_MARKERS.iter().all(|name| absent(&held, name))
        || keeps_cache(&held)
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

/// Whether `root`, a path in this user's HOME, is or lies in a cargo home's git or registry
/// cache, which atpkg never takes into custody, by creation or adoption, nor keeps there once
/// recorded ([`same`]): a folder named as one ([`HOME_CACHE_NAMES`]) in a cargo home, `~/.cargo`
/// or a folder holding a [`HOME_MARKS`] file, at `root` or around it; the folder `~/.cargo`'s
/// cache names, through a link or not, or one in it; or a folder whose top keeps what such a
/// cache keeps ([`keeps_cache`]), under any name. A mark that cannot be read counts as one, and
/// so does a `root` that stands but cannot be read. A folder inside a package that lies in such
/// a cache (a git checkout's `<name>/<rev>`, a registry extraction's `<index>/<crate>`, each a
/// folder holding a manifest, [`super::super::checkout::MANIFESTS`]) is that package's build's,
/// not the cache's: Targo's fetches write a checkout or an extraction whole, never into a
/// folder a build made in it. A cache whose home is not `~/.cargo`, named through a link and
/// holding nothing yet, shows none of this.
#[cfg(unix)]
pub(super) fn home_cache(home: &Path, root: &Path) -> bool {
    let present = |at: &Path| {
        !std::fs::symlink_metadata(at)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    };
    let default = home.join(".cargo");
    let default_held = std::fs::canonicalize(&default).ok();
    let in_cargo_home = |at: &Path| {
        at == default
            || default_held.as_deref() == Some(at)
            || HOME_MARKS.iter().any(|mark| present(&at.join(mark)))
    };
    // Whether a folder between `cache` and `root`, `root` excluded, holds a package.
    let packaged = |cache: &Path| {
        root.ancestors()
            .skip(1)
            .take_while(|at| *at != cache && at.starts_with(cache))
            .any(|at| {
                super::super::checkout::MANIFESTS.iter().any(|name| {
                    std::fs::metadata(at.join(name)).is_ok_and(|metadata| metadata.is_file())
                })
            })
    };
    let named = root
        .ancestors()
        .take_while(|at| *at != home && at.starts_with(home))
        .any(|at| {
            at.file_name()
                .is_some_and(|name| HOME_CACHE_NAMES.iter().any(|cache| name == *cache))
                && at.parent().is_some_and(in_cargo_home)
                && !packaged(at)
        });
    let linked = HOME_CACHE_NAMES.iter().any(|name| {
        std::fs::canonicalize(default.join(name))
            .is_ok_and(|cache| root.starts_with(&cache) && !packaged(&cache))
    });
    named
        || linked
        || match aterm_dirfd::openat(
            aterm_dirfd::CWD,
            root,
            aterm_dirfd::OFlags::RDONLY
                | aterm_dirfd::OFlags::DIRECTORY
                | aterm_dirfd::OFlags::NOFOLLOW
                | aterm_dirfd::OFlags::CLOEXEC
                | aterm_dirfd::OFlags::NONBLOCK,
            aterm_dirfd::Mode::empty(),
        ) {
            Ok(held) => keeps_cache(&held),
            Err(error) => !error.is_noent(),
        }
}

/// Whether the folder `dir` holds keeps, at its top, what a cargo home's cache keeps: a
/// [`HOME_CACHE`] entry that is neither a custom profile's folder, a folder, no link, that holds
/// a profile's lock ([`locked`]), nor a custom target's, a folder, no link, a folder at whose top
/// ([`subfolders`]) holds one. An entry that cannot be read counts.
#[cfg(unix)]
fn keeps_cache(dir: &aterm_dirfd::OwnedFd) -> bool {
    HOME_CACHE.iter().any(|name| {
        match aterm_dirfd::fstatat(dir, *name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW) {
            Err(error) if error.is_noent() => false,
            Ok(at) if aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir() => {
                !open_folder(dir, std::ffi::OsStr::new(name)).is_some_and(|entry| {
                    locked(&entry)
                        || subfolders(&entry).is_some_and(|profiles| profiles.iter().any(locked))
                })
            }
            _ => true,
        }
    })
}

/// The folder `name` in the folder `dir` holds, opened with no link followed.
#[cfg(unix)]
fn open_folder(dir: &aterm_dirfd::OwnedFd, name: &std::ffi::OsStr) -> Option<aterm_dirfd::OwnedFd> {
    aterm_dirfd::openat(
        dir,
        name,
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::DIRECTORY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC
            | aterm_dirfd::OFlags::NONBLOCK,
        aterm_dirfd::Mode::empty(),
    )
    .ok()
}

/// Whether the folder `profile` holds a profile's lock ([`super::super::CARGO_LOCKS`]) at its
/// top, a regular file, no link: `Layout::new` lays each in the profile folder it makes.
#[cfg(unix)]
fn locked(profile: &aterm_dirfd::OwnedFd) -> bool {
    super::super::CARGO_LOCKS.iter().any(|lock| {
        aterm_dirfd::fstatat(profile, *lock, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|at| aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_file())
    })
}

/// The folders at the top of the folder `dir` holds, none a link, each opened: `None` at a
/// failed read or past [`MAX_TOP`] entries.
#[cfg(unix)]
fn subfolders(dir: &aterm_dirfd::OwnedFd) -> Option<Vec<aterm_dirfd::OwnedFd>> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut found = Vec::new();
    let mut read = 0;
    for entry in aterm_dirfd::Dir::read_from(dir).ok()? {
        let entry = entry.ok()?;
        let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        read += 1;
        if read > MAX_TOP {
            return None;
        }
        let folder = aterm_dirfd::fstatat(dir, name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|at| aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir());
        if folder {
            found.extend(open_folder(dir, name));
        }
    }
    Some(found)
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

/// Whether cargo compiled, or ran a `clean -p`, in the folder `dir` holds, as its build
/// directory: its [`RUSTC_INFO`] stands there, a regular file, no link ([`rustc_facts`]), beside
/// either a profile `Layout::new` laid ([`prepared`]) and a compilation's invocation fingerprint
/// ([`invoked`]), or a profile `clean -p` left ([`cleaned`]) with no unit's folder in any profile
/// ([`unit_free`]). The facts alone are what a resolving verb leaves in a build directory that
/// already stands.
#[cfg(unix)]
pub(super) fn built(dir: &aterm_dirfd::OwnedFd) -> bool {
    rustc_facts(dir) && ((prepared(dir) && invoked(dir)) || (cleaned(dir) && unit_free(dir)))
}

/// Whether the folder `dir` holds has cargo's [`RUSTC_INFO`] at its top, a regular file, no
/// link: a layout observation alone, which proves no build ran there ([`built`] adds that).
#[cfg(unix)]
pub(super) fn rustc_facts(dir: &aterm_dirfd::OwnedFd) -> bool {
    aterm_dirfd::fstatat(dir, RUSTC_INFO, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|at| aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_file())
}

/// Whether a folder at the top of the folder `dir` holds, itself no link, is a profile
/// `Layout::new` laid there as the build directory's: one where no [`PREPARED`] name stands as
/// anything but a folder, no link (Targo makes each a folder, never a link or a file), and that
/// holds every one of them (a build prepared it) or the [`BUILD_LOCK`], a regular file, no link
/// (`clean -p`'s included, which prepares none). Read entry by entry: true at the first such
/// profile; false once [`MAX_TOP`] entries were read without one, or at a failed read.
#[cfg(unix)]
fn prepared(dir: &aterm_dirfd::OwnedFd) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let is_folder = |at: &aterm_dirfd::OwnedFd, name: &std::ffi::OsStr| {
        aterm_dirfd::fstatat(at, name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|at| aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir())
    };
    // `Some(true)` for a folder, no link; `Some(false)` where nothing stands; `None` otherwise.
    let prepared_at = |at: &aterm_dirfd::OwnedFd, name: &str| match aterm_dirfd::fstatat(
        at,
        name,
        aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(at) if aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir() => Some(true),
        Err(error) if error.is_noent() => Some(false),
        _ => None,
    };
    let Ok(entries) = aterm_dirfd::Dir::read_from(dir) else {
        return false;
    };
    let mut read = 0;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        read += 1;
        if read > MAX_TOP {
            return false;
        }
        if !is_folder(dir, name) {
            continue;
        }
        let Ok(profile) = aterm_dirfd::openat(
            dir,
            name,
            aterm_dirfd::OFlags::RDONLY
                | aterm_dirfd::OFlags::DIRECTORY
                | aterm_dirfd::OFlags::NOFOLLOW
                | aterm_dirfd::OFlags::CLOEXEC
                | aterm_dirfd::OFlags::NONBLOCK,
            aterm_dirfd::Mode::empty(),
        ) else {
            continue;
        };
        let Some(present) = PREPARED
            .iter()
            .map(|prepared| prepared_at(&profile, prepared))
            .collect::<Option<Vec<bool>>>()
        else {
            continue;
        };
        let build_locked =
            aterm_dirfd::fstatat(&profile, BUILD_LOCK, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|at| aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_file());
        if build_locked || present.iter().all(|&present| present) {
            return true;
        }
    }
    false
}

/// What Targo's `Layout::prepare` lays in a profile folder as a compilation starts, in the old
/// layout: `BuildDirLayout::prepare`'s `deps`, `.fingerprint`, `incremental` and `build`, and
/// `ArtifactDirLayout::prepare`'s `examples` where the build directory is the artifact
/// directory. `clean -p` reaches `Layout::new` alone, which lays none of them, and removes
/// none it finds (`DirectoriesToClean` removes the entries it marks in them, never the folders).
#[cfg(unix)]
const LAID_BY_PREPARE: [&str; 5] = ["deps", ".fingerprint", "examples", "build", "incremental"];

/// Whether a folder at the top of the folder `dir` holds is a profile `clean -p` left there as
/// the build directory's: a folder, no link, of this user's and writable by no group or other
/// user, whose [`BUILD_LOCK`] is a regular file, no link, its only name, of this user's and
/// writable by no group or other user, where each [`LAID_BY_PREPARE`] name stands as a folder,
/// no link, or not at all, its absence known, and that holds either none of them or every
/// [`PREPARED`] folder. Targo's `clean_specs` lays the lock through `Layout::new` and nothing a
/// build prepares, so a `clean -p` before any build leaves none, and one after a build leaves the
/// folders that build's `Layout::prepare` laid (both layouts lay [`PREPARED`]), with the units it
/// cleaned taken out of them ([`unit_free`] reads whether any is left). Read entry by entry: true
/// at the first such profile; false once [`MAX_TOP`] entries were read without one, or at a
/// failed read.
#[cfg(unix)]
fn cleaned(dir: &aterm_dirfd::OwnedFd) -> bool {
    let Ok(entries) = aterm_dirfd::Dir::read_from(dir) else {
        return false;
    };
    let mut read = 0;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        read += 1;
        if read > MAX_TOP {
            return false;
        }
        let Some(profile) = evidence_dir(dir, name) else {
            continue;
        };
        let locked =
            aterm_dirfd::fstatat(&profile, BUILD_LOCK, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|at| {
                    aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_file()
                        && super::private(at.st_uid, at.st_mode, super::euid())
                        && at.st_nlink == 1
                });
        // `Some(true)` for a folder, no link; `Some(false)` where nothing stands; `None` otherwise.
        let laid = LAID_BY_PREPARE
            .iter()
            .map(|laid| {
                match aterm_dirfd::fstatat(&profile, *laid, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW)
                {
                    Ok(at) if aterm_dirfd::FileType::from_raw_mode(at.st_mode).is_dir() => {
                        Some((*laid, true))
                    }
                    Err(error) if error.is_noent() => Some((*laid, false)),
                    _ => None,
                }
            })
            .collect::<Option<Vec<(&str, bool)>>>();
        let as_left = laid.is_some_and(|laid| {
            laid.iter().all(|&(_, stands)| !stands)
                || PREPARED
                    .iter()
                    .all(|prepared| laid.contains(&(*prepared, true)))
        });
        if locked && as_left {
            return true;
        }
    }
    false
}

/// The folders Targo lays at the top of an artifact or build root, a target's included, that are
/// no profile: rustdoc's `doc`, the tests' `tmp` (`CARGO_TARGET_TMPDIR`), `package`'s packages
/// and extracted sources, and `cargo-timings`. Targo reserves `tmp`, `package` and every `cargo*`
/// as a profile name; `doc` is one of its predefined profiles, whose folder is also rustdoc's, so
/// [`free_of_units`] reads one of these names as a profile where it holds a cargo lock or a
/// `.fingerprint`, as every profile `Layout::new` and `prepare` laid off NFS does.
#[cfg(unix)]
const NO_PROFILE: [&str; 4] = ["doc", "tmp", "package", "cargo-timings"];

/// Whether the folder `dir` holds keeps no unit's folder where a compilation makes one: no entry
/// in a profile's `.fingerprint` (the old layout's `<unit>` folders) or `build` (the new layout's
/// `<package>` folders, the old one's build scripts'), the profile at the top or under a target's
/// folder, the same places [`invoked`] reads. Every folder there counts as a profile but one of
/// Targo's [`NO_PROFILE`] folders that holds neither a cargo lock nor a `.fingerprint`, so a
/// crate's `doc/<crate>/build` page, a test's `tmp/<case>/build` or an extracted package's
/// `package/<name>-<version>/build` is none. Targo's `prepare_init` makes each unit's folder
/// before any job starts, and `clean -p` removes the folders of every package it cleans, so a
/// build cut short before its first unit's job keeps one with no invocation, while a build whose
/// every compiled package `clean -p` took keeps none. A `.fingerprint` or `build` that is no
/// folder, a link among the folders it reads as a profile or a target's (a profile or target
/// that is a link), a failed read and more than the entries [`invoked`] may read each count as a
/// unit. It reads the folder it is handed, its top or its build leaf, and not the other.
#[cfg(unix)]
fn unit_free(dir: &aterm_dirfd::OwnedFd) -> bool {
    let mut remaining = 16_384_usize;
    free_of_units(dir, true, &mut remaining).is_some()
}

/// [`unit_free`]'s reading of the folders `at` holds as profiles and, where `targets`, as the
/// folders of targets whose own folders it reads the same way: `Some` where no profile's
/// `.fingerprint` or `build` holds an entry, `None` for one that does and for what cannot be
/// told, `remaining` the entries it may still read. A folder holding a cargo lock or a
/// `.fingerprint` is a profile, which holds no target's folder, so it is not read further.
#[cfg(unix)]
fn free_of_units(at: &aterm_dirfd::OwnedFd, targets: bool, remaining: &mut usize) -> Option<()> {
    use std::os::unix::ffi::OsStrExt as _;
    for entry in aterm_dirfd::Dir::read_from(at).ok()? {
        let entry = entry.ok()?;
        let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());
        if name == "." || name == ".." {
            continue;
        }
        *remaining = remaining.checked_sub(1)?;
        let stat = aterm_dirfd::fstatat(at, name, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW).ok()?;
        let kind = aterm_dirfd::FileType::from_raw_mode(stat.st_mode);
        if kind.is_symlink() {
            return None;
        }
        if !kind.is_dir() {
            continue;
        }
        let folder = open_folder(at, name)?;
        let profile = locked(&folder) || !absent(&folder, ".fingerprint");
        if !profile && NO_PROFILE.iter().any(|no_profile| name == *no_profile) {
            continue;
        }
        profile_free_of_units(&folder)?;
        if targets && !profile {
            free_of_units(&folder, false, remaining)?;
        }
    }
    Some(())
}

/// Whether the profile `profile` holds keeps no entry in its `.fingerprint` or `build`, each a
/// folder, no link, or absent: `None` otherwise.
#[cfg(unix)]
fn profile_free_of_units(profile: &aterm_dirfd::OwnedFd) -> Option<()> {
    for place in [".fingerprint", "build"] {
        match aterm_dirfd::fstatat(profile, place, aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW) {
            Err(error) if error.is_noent() => continue,
            Ok(stat) if aterm_dirfd::FileType::from_raw_mode(stat.st_mode).is_dir() => {}
            _ => return None,
        }
        let held = open_folder(profile, std::ffi::OsStr::new(place))?;
        for entry in aterm_dirfd::Dir::read_from(&held).ok()? {
            if !matches!(entry.ok()?.file_name().to_bytes(), b"." | b"..") {
                return None;
            }
        }
    }
    Some(())
}

/// Whether the folder `dir` holds keeps a compilation's invocation evidence: an
/// `invoked.timestamp` Targo's `set_invocation_time` writes as a unit's job starts, in a
/// profile's `.fingerprint/<unit>` (the old layout) or `build/<package>/<hash>/fingerprint` (the
/// new one), the profile at the top or under a target's folder. Not a claim that compilation
/// succeeded. Resolving queries and `clean -p` write none. Profiles can have arbitrary names;
/// cross compilation adds one target component.
#[cfg(unix)]
fn invoked(dir: &aterm_dirfd::OwnedFd) -> bool {
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
