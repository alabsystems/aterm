// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A retiring tree owns neither another worktree nor its private Git registry.
//!
//! Inspect only the two trees about to go. Source is searched structurally for Git
//! stores; a directory merely named `worktrees` is not a registry. The outer Git
//! administration tree, physical `.git` directories and the legacy module area
//! are metadata scopes even when an interrupted removal took their store markers.
//! No registry entry grants deletion permission: nonempty or unreadable stays.
//! This is a bounded custody observation, not exclusion of concurrent writers or
//! a global audit of object-alternate borrowers outside the retiring trees.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ENTRIES: usize = 1_000_000;
const DEPTH: usize = 64;
const ELAPSED: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Directory,
    File,
    Link,
    Other,
}

pub(super) struct Directory {
    pub(super) path: PathBuf,
    #[cfg(unix)]
    file: File,
}

#[cfg(unix)]
const OPEN: aterm_dirfd::OFlags = aterm_dirfd::OFlags::from_bits(
    aterm_dirfd::OFlags::RDONLY.bits()
        | aterm_dirfd::OFlags::DIRECTORY.bits()
        | aterm_dirfd::OFlags::NOFOLLOW.bits()
        | aterm_dirfd::OFlags::NONBLOCK.bits()
        | aterm_dirfd::OFlags::CLOEXEC.bits(),
);

fn physical(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    meta.is_dir() && !meta.file_type().is_symlink()
}

#[cfg(not(unix))]
fn kind(meta: &Metadata) -> Kind {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if meta.file_attributes() & 0x400 != 0 {
            return Kind::Link;
        }
    }
    if meta.file_type().is_symlink() {
        Kind::Link
    } else if physical(meta) {
        Kind::Directory
    } else if meta.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn same(before: &Metadata, after: &Metadata) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if (
            before.dev(),
            before.ino(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
        {
            return Ok(false);
        }
    }
    Ok(physical(before)
        && physical(after)
        && before.len() == after.len()
        && before.modified()? == after.modified()?)
}

impl Directory {
    pub(super) fn root(path: &Path) -> io::Result<Option<Self>> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !physical(&meta) {
            return Err(io::Error::other("custody root is not a physical directory"));
        }
        #[cfg(unix)]
        let file = File::from(aterm_dirfd::openat(
            aterm_dirfd::CWD,
            path,
            OPEN,
            aterm_dirfd::Mode::empty(),
        )?);
        #[cfg(unix)]
        if !same(&meta, &file.metadata()?)? {
            return Err(io::Error::other("custody root changed while opening"));
        }
        Ok(Some(Self {
            path: path.to_path_buf(),
            #[cfg(unix)]
            file,
        }))
    }

    pub(super) fn child(&self, name: &OsStr) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                path: self.path.join(name),
                file: File::from(aterm_dirfd::openat(
                    &self.file,
                    name,
                    OPEN,
                    aterm_dirfd::Mode::empty(),
                )?),
            })
        }
        #[cfg(not(unix))]
        {
            Self::root(&self.path.join(name))?
                .ok_or_else(|| io::Error::other("custody directory disappeared"))
        }
    }

    pub(super) fn metadata(&self) -> io::Result<Metadata> {
        #[cfg(unix)]
        {
            self.file.metadata()
        }
        #[cfg(not(unix))]
        {
            fs::symlink_metadata(&self.path)
        }
    }

    pub(super) fn names(&self, budget: &mut Budget) -> io::Result<BTreeMap<OsString, Kind>> {
        let before = self.metadata()?;
        let mut names = BTreeMap::new();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            for entry in aterm_dirfd::Dir::read_from(&self.file)? {
                budget.entry()?;
                let entry = entry?;
                let bytes = entry.file_name().to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                let name = OsStr::from_bytes(bytes).to_os_string();
                let observed = aterm_dirfd::fstatat(
                    &self.file,
                    &name,
                    aterm_dirfd::AtFlags::SYMLINK_NOFOLLOW,
                )?;
                let kind = aterm_dirfd::FileType::from_raw_mode(observed.st_mode);
                let kind = if kind.is_dir() {
                    Kind::Directory
                } else if kind.is_symlink() {
                    Kind::Link
                } else if kind.is_file() {
                    Kind::File
                } else {
                    Kind::Other
                };
                if names.insert(name, kind).is_some() {
                    return Err(io::Error::other("custody directory repeated an entry"));
                }
            }
        }
        #[cfg(not(unix))]
        {
            for entry in fs::read_dir(&self.path)? {
                budget.entry()?;
                let entry = entry?;
                let meta = fs::symlink_metadata(entry.path())?;
                let kind = kind(&meta);
                if names.insert(entry.file_name(), kind).is_some() {
                    return Err(io::Error::other("custody directory repeated an entry"));
                }
            }
        }
        if !same(&before, &self.metadata()?)? {
            return Err(io::Error::other(
                "custody directory changed during observation",
            ));
        }
        Ok(names)
    }

    pub(super) fn read(&self, name: &OsStr, limit: usize) -> io::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let flags = aterm_dirfd::OFlags::RDONLY
                | aterm_dirfd::OFlags::NOFOLLOW
                | aterm_dirfd::OFlags::NONBLOCK
                | aterm_dirfd::OFlags::CLOEXEC;
            let file = File::from(aterm_dirfd::openat(
                &self.file,
                name,
                flags,
                aterm_dirfd::Mode::empty(),
            )?);
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::other(
                    "private Git metadata is not a regular file",
                ));
            }
            crate::metadata_io::read_bounded_regular_file(file, &metadata, limit)
        }
        #[cfg(not(unix))]
        {
            crate::metadata_io::read_bounded_regular(&self.path.join(name), limit)
        }
    }

    fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            path: self.path.clone(),
            #[cfg(unix)]
            file: self.file.try_clone()?,
        })
    }
}

pub(super) struct Budget {
    entries: usize,
    start: Instant,
}

impl Budget {
    pub(super) fn new() -> Self {
        Self {
            entries: 0,
            start: Instant::now(),
        }
    }

    fn entry(&mut self) -> io::Result<()> {
        self.entries += 1;
        self.check(0)
    }

    pub(super) fn check(&self, depth: usize) -> io::Result<()> {
        if self.entries > ENTRIES || depth > DEPTH || self.start.elapsed() > ELAPSED {
            Err(io::Error::other(
                "private Git custody observation reached its work bound",
            ))
        } else {
            Ok(())
        }
    }
}

fn store(names: &BTreeMap<OsString, Kind>) -> bool {
    names.get(OsStr::new("HEAD")) == Some(&Kind::File)
        && names.get(OsStr::new("objects")) == Some(&Kind::Directory)
}

fn walk(
    directory: Directory,
    relative: &Path,
    metadata: bool,
    private_metadata: bool,
    depth: usize,
    budget: &mut Budget,
    stores: &mut Vec<Directory>,
) -> io::Result<()> {
    budget.check(depth)?;
    let names = directory.names(budget)?;
    let is = |name: &str, kind| names.get(OsStr::new(name)) == Some(&kind);
    let common = store(&names);
    let administrative = is("commondir", Kind::File) && is("gitdir", Kind::File);
    if administrative && !(metadata && relative.as_os_str().is_empty()) {
        return Err(io::Error::other(format!(
            "{} holds private Git worktree registration metadata",
            directory.path.display(),
        )));
    }
    let metadata = metadata || common;
    let private_metadata = private_metadata || common;
    if common {
        if stores.len() >= 64 {
            return Err(io::Error::other(
                "private Git store count reached its work bound",
            ));
        }
        stores.push(directory.duplicate()?);
    } else if private_metadata && !administrative && names.values().any(|kind| *kind == Kind::File)
    {
        return Err(io::Error::other(format!(
            "{} holds incomplete or unrecognized private Git metadata",
            directory.path.display(),
        )));
    }
    if metadata && let Some(kind) = names.get(OsStr::new("worktrees")) {
        if *kind != Kind::Directory {
            return Err(io::Error::other(format!(
                "{} has an unreadable or linked Git worktree registry",
                directory.path.display(),
            )));
        }
        let registry = directory.child(OsStr::new("worktrees"))?;
        let registered = registry.names(budget)?;
        // Inside a module namespace, "worktrees" may itself name a module.
        // Only its complete store shape establishes that interpretation. A
        // damaged parent store with an ambiguous nonempty registry stays.
        if !registered.is_empty() && (common || administrative || !store(&registered)) {
            return Err(io::Error::other(format!(
                "{} owns registered Git worktrees; their source and private metadata stay",
                directory.path.display(),
            )));
        }
    }
    for (name, kind) in names {
        if metadata && kind == Kind::Link {
            return Err(io::Error::other(format!(
                "{} has linked private Git metadata",
                directory.path.join(&name).display(),
            )));
        }
        if kind != Kind::Directory {
            continue;
        }
        if common
            && ["objects", "refs", "logs", "hooks", "info"]
                .iter()
                .any(|skip| name == *skip)
            || administrative && name == "logs"
        {
            continue;
        }
        let child_relative = relative.join(&name);
        let child_metadata =
            metadata || name == ".git" || child_relative == Path::new(".aterm-verify/modules");
        // The bound outer admin can also hold ordinary outputs. Its module namespace,
        // physical Git directories and legacy module area carry the stronger damaged-store
        // presumption; the structural registry search still covers every admin descendant.
        let child_private = private_metadata
            || name == ".git"
            || child_relative == Path::new(".aterm-verify/modules")
            || (metadata && relative.as_os_str().is_empty() && name == "modules");
        walk(
            directory.child(&name)?,
            &child_relative,
            child_metadata,
            child_private,
            depth + 1,
            budget,
            stores,
        )?;
    }
    Ok(())
}

/// Recheck source-private and admin-private registries, including interrupted stores.
/// A missing root is allowed: the receipt separately proves which removal phase owns it.
pub(super) fn clear(wt: &Path, admin: Option<&Path>) -> Result<Vec<Directory>, String> {
    let mut budget = Budget::new();
    let mut stores = Vec::new();
    for (root, metadata) in [(wt, false)]
        .into_iter()
        .chain(admin.map(|root| (root, true)))
    {
        let result = (|| {
            if let Some(directory) = Directory::root(root)? {
                walk(
                    directory,
                    Path::new(""),
                    metadata,
                    false,
                    0,
                    &mut budget,
                    &mut stores,
                )?;
            }
            Ok::<(), io::Error>(())
        })();
        if let Err(error) = result {
            return Err(format!(
                "private Git custody keeps {}: {error}",
                root.display()
            ));
        }
    }
    Ok(stores)
}
