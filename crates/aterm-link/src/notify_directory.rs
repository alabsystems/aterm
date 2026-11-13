// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Notify-store I/O rooted in an already opened directory, never its later pathname.

use aterm_dirfd::{AtFlags, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::{ffi::OsStringExt as _, fs::OpenOptionsExt as _};
use std::path::Path;
use std::sync::Arc;

const SAFE: OFlags = OFlags::from_bits(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);

#[derive(Debug)]
pub(super) struct Directory {
    pub(super) file: File,
    /// A diagnostic label only; every operation below uses `file`.
    pub(super) path: std::path::PathBuf,
}

#[cfg(test)]
thread_local! {
    /// A test's mutant: while set, each operation below on this thread resolves its names in
    /// the directory its [`Directory::path`] names at that moment, as a build that wrote the
    /// store by its path did, never in the descriptor it holds ([`Directory::at`]).
    pub(super) static BY_PATH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The directory an operation resolves its names in ([`Directory::at`]).
enum At<'a> {
    Held(&'a File),
    #[cfg(test)]
    ByPath(File),
}

impl AsFd for At<'_> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            At::Held(file) => file.as_fd(),
            #[cfg(test)]
            At::ByPath(file) => file.as_fd(),
        }
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        File::options()
            .read(true)
            .custom_flags((OFlags::DIRECTORY | SAFE).bits())
            .open(path)
    })
}

fn component(name: &OsStr) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.as_bytes().iter().any(|b| matches!(b, 0 | b'/'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not one notify-store component",
        ));
    }
    Ok(())
}

impl Directory {
    pub(super) fn open(path: &Path) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            file: open_directory(path)?,
            path: path.to_path_buf(),
        }))
    }

    /// The directory each name below is resolved in: the one held open (`file`), whatever its
    /// path names now. Only a test's mutant ([`BY_PATH`]) resolves it by its path instead.
    #[cfg_attr(
        not(test),
        expect(clippy::unnecessary_wraps, reason = "the test mutant's open can fail")
    )]
    fn at(&self) -> io::Result<At<'_>> {
        #[cfg(test)]
        if BY_PATH.with(std::cell::Cell::get) {
            return open_directory(&self.path).map(At::ByPath);
        }
        Ok(At::Held(&self.file))
    }

    /// A new open file description of this same directory, including for its exclusive lock.
    /// A duplicate of the I/O descriptor would share lock lifetime with every clone.
    pub(super) fn independent(&self) -> io::Result<File> {
        aterm_uds::kept_out(|| {
            aterm_dirfd::openat(
                &self.file,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | SAFE,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(io::Error::from)
        })
    }

    pub(super) fn child(&self, name: &OsStr, create: bool) -> io::Result<Arc<Self>> {
        component(name)?;
        if create {
            match aterm_dirfd::mkdirat(self.at()?, name, Mode::from_bits(0o700)) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == libc::EEXIST => {}
                Err(error) => return Err(error.into()),
            }
        }
        let file = self.open_file(name, OFlags::RDONLY | OFlags::DIRECTORY, 0)?;
        Ok(Arc::new(Self {
            file,
            path: self.path.join(name),
        }))
    }

    pub(super) fn parent(
        self: &Arc<Self>,
        name: &str,
        create: bool,
    ) -> io::Result<(Arc<Self>, OsString)> {
        let mut parts = name.split('/').peekable();
        let mut directory = Arc::clone(self);
        while let Some(part) = parts.next() {
            let part = OsStr::new(part);
            component(part)?;
            if parts.peek().is_none() {
                return Ok((directory, part.to_owned()));
            }
            directory = directory.child(part, create)?;
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty notify-store path",
        ))
    }

    pub(super) fn open_file(
        &self,
        name: &OsStr,
        flags: OFlags,
        mode: libc::mode_t,
    ) -> io::Result<File> {
        component(name)?;
        let at = self.at()?;
        aterm_uds::kept_out(|| {
            aterm_dirfd::openat(&at, name, flags | SAFE, Mode::from_bits(mode))
                .map(File::from)
                .map_err(io::Error::from)
        })
    }

    pub(super) fn read(&self, name: &OsStr, max: u64) -> io::Result<Vec<u8>> {
        let file = self.open_file(name, OFlags::RDONLY, 0)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular notify-store file",
            ));
        }
        if metadata.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "notify-store file exceeds size limit",
            ));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "notify-store file exceeds size limit",
            ));
        }
        Ok(bytes)
    }

    pub(super) fn stat(&self, name: &OsStr) -> io::Result<aterm_dirfd::Stat> {
        component(name)?;
        aterm_dirfd::fstatat(self.at()?, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)
    }

    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        // fdopendir can change descriptor flags on Darwin. Mark the completed stream under
        // the same spawn exclusion that covers its open, as PrivateDirectory does.
        let at = self.at()?;
        let stream = aterm_uds::kept_out(|| {
            let file = aterm_dirfd::openat(
                &at,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | SAFE,
                Mode::empty(),
            )?;
            aterm_dirfd::Dir::new(file).map_err(io::Error::from)
        })?;
        let mut names = Vec::new();
        for entry in stream {
            let name = OsString::from_vec(
                entry
                    .map_err(io::Error::from)?
                    .file_name()
                    .to_bytes()
                    .to_vec(),
            );
            if name != "." && name != ".." {
                names.push(name);
            }
        }
        Ok(names)
    }

    pub(super) fn rename(&self, from: &OsStr, to_directory: &Self, to: &OsStr) -> io::Result<()> {
        component(from)?;
        component(to)?;
        aterm_dirfd::renameat(self.at()?, from, to_directory.at()?, to).map_err(io::Error::from)
    }

    pub(super) fn remove(&self, name: &OsStr, directory: bool) -> io::Result<()> {
        component(name)?;
        aterm_dirfd::unlinkat(
            self.at()?,
            name,
            if directory {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            },
        )
        .map_err(io::Error::from)
    }

    pub(super) fn remove_tree(&self, name: &OsStr) -> io::Result<()> {
        match self.child(name, false) {
            Ok(child) => {
                for entry in child.names()? {
                    child.remove_tree(&entry)?;
                }
                self.remove(name, true)
            }
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                self.remove(name, false)
            }
            Err(error) => Err(error),
        }
    }

    /// The caller holds this store's exclusive lease. Other store writers cannot
    /// retarget its temporary entries; explicit identity checks also refuse a
    /// replacement observed before publication or cleanup.
    pub(super) fn temporary(
        self: &Arc<Self>,
        prefix: &str,
        directory: bool,
    ) -> io::Result<Temporary> {
        component(OsStr::new(prefix))?;
        for _ in 0..8 {
            let name = OsString::from(format!(
                ".{prefix}.tmp.{}.{}",
                std::process::id(),
                aterm_uds::rand::hex_token::<8>()?
            ));
            let mut created = None;
            let made = if directory {
                match aterm_dirfd::mkdirat(self.at()?, &name, Mode::from_bits(0o700))
                    .map_err(io::Error::from)
                {
                    Ok(()) => {
                        let identity = self.stat(&name).map(|meta| (meta.st_dev, meta.st_ino))?;
                        created = Some(CreatedName {
                            parent: Arc::clone(self),
                            name: name.clone(),
                            identity,
                            directory: true,
                            armed: true,
                        });
                        self.child(&name, false).map(TemporaryData::Directory)
                    }
                    Err(error) => Err(error),
                }
            } else {
                let at = self.at()?;
                aterm_uds::kept_out(|| {
                    let file = File::from(aterm_dirfd::openat(
                        &at,
                        &name,
                        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | SAFE,
                        Mode::from_bits(0o600),
                    )?);
                    let identity = identity(&file)?;
                    created = Some(CreatedName {
                        parent: Arc::clone(self),
                        name: name.clone(),
                        identity,
                        directory: false,
                        armed: true,
                    });
                    Ok(file)
                })
                .map(TemporaryData::File)
            };
            match made {
                Ok(data) => {
                    let mut original = created.expect("successful creation retained its identity");
                    let identity = original.identity;
                    let held = match &data {
                        TemporaryData::File(file) => self::identity(file)?,
                        TemporaryData::Directory(directory) => self::identity(&directory.file)?,
                    };
                    if held != identity {
                        return Err(io::Error::other("notify temporary changed while opening"));
                    }
                    original.armed = false;
                    return Ok(Temporary {
                        parent: Arc::clone(self),
                        name,
                        data,
                        identity,
                        armed: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && created.is_none() => {
                }
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "notify temporary names collided",
        ))
    }
}

pub(super) struct Temporary {
    pub(super) parent: Arc<Directory>,
    pub(super) name: OsString,
    pub(super) data: TemporaryData,
    identity: (u64, u64),
    armed: bool,
}

pub(super) enum TemporaryData {
    File(File),
    Directory(Arc<Directory>),
}

fn identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

struct CreatedName {
    parent: Arc<Directory>,
    name: OsString,
    identity: (u64, u64),
    directory: bool,
    armed: bool,
}

impl Drop for CreatedName {
    fn drop(&mut self) {
        if self.armed
            && self
                .parent
                .stat(&self.name)
                .is_ok_and(|meta| (meta.st_dev, meta.st_ino) == self.identity)
        {
            // Before the directory is opened and used, only an empty entry is ours
            // to remove. A failed open must never recurse into unexpected contents.
            let _ = self.parent.remove(&self.name, self.directory);
        }
    }
}

impl Temporary {
    fn original_named(&self) -> bool {
        let file = match &self.data {
            TemporaryData::File(file) => file,
            TemporaryData::Directory(directory) => &directory.file,
        };
        identity(file).is_ok_and(|held| held == self.identity)
            && self
                .parent
                .stat(&self.name)
                .is_ok_and(|named| (named.st_dev, named.st_ino) == self.identity)
    }

    pub(super) fn publish(mut self, destination: &Directory, name: &OsStr) -> io::Result<()> {
        if !self.original_named() {
            return Err(io::Error::other(
                "notify temporary entry changed before publication",
            ));
        }
        match &self.data {
            TemporaryData::Directory(_) => {
                component(name)?;
                aterm_dirfd::renameat_with(
                    self.parent.at()?,
                    &self.name,
                    destination.at()?,
                    name,
                    aterm_dirfd::RenameFlags::NOREPLACE,
                )?;
            }
            TemporaryData::File(_) => self.parent.rename(&self.name, destination, name)?,
        }
        self.armed = false;
        let published = destination.stat(name)?;
        if (published.st_dev, published.st_ino) != self.identity {
            return Err(io::Error::other(
                "notify published entry changed; publication may have occurred",
            ));
        }
        Ok(())
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.armed || !self.original_named() {
            return;
        }
        match &self.data {
            TemporaryData::File(_) => {
                let _ = self.parent.remove(&self.name, false);
            }
            TemporaryData::Directory(directory) => {
                if let Ok(names) = directory.names() {
                    for name in names {
                        let _ = directory.remove_tree(&name);
                    }
                    if self.original_named() {
                        let _ = self.parent.remove(&self.name, true);
                    }
                }
            }
        }
    }
}
