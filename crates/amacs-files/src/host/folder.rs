// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOLDER A COMMIT WORKS IN ([`Folder`]): opened once, by a path no link may stand anywhere
//! on, and every later step of the commit (its save lock, its temporary, each read of the
//! target, the rename or link that publishes, the sync of the folder's entries) reaches the
//! folder through that one handle, never by the path again. A save is judged by its path (the
//! owner-keys fence asks whether the file is the aterm.toml in force, or would be made in its
//! folder) before its commit runs; a process that swaps a folder of that path for a link to
//! another folder, between the judgement and a step or between two steps, so never has the
//! commit work in the folder the link names. A link there when the folder is opened refuses
//! the commit; one put there after it leaves every step in the folder opened, wherever that
//! folder was moved, and the commit's checks before and after it publishes (`hold_folder`)
//! fail while the path does not reach that folder. Off unix a commit works by path.

pub(super) use imp::Folder;
#[cfg(all(test, unix))]
pub(super) use imp::walk;

/// Whether `error` says a link stands on the path a folder was opened by.
pub(super) fn is_link(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Whether `error` says no folder is at the path one was opened by: nothing there, or a
/// file where a folder of the path would be.
pub(super) fn is_missing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// The name of the temporary a commit of `name` writes, its `attempt`th for `seed`.
fn temporary_name(name: &std::ffi::OsStr, seed: u64, attempt: u64) -> String {
    format!(
        ".{}.aterm-{}-{}-{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        seed,
        attempt
    )
}

/// How many names a commit tries for its temporary before it gives up.
const TEMPORARY_ATTEMPTS: u64 = 64;

#[cfg(unix)]
mod imp {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File};
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Component, Path};

    use aterm_dirfd::{
        AsFd as _, AtFlags, BorrowedFd, CWD, FileType, Mode, OFlags, OwnedFd, fstat, fstatat,
        linkat, mkdirat, openat, renameat, unlinkat,
    };
    use aterm_gui_consent::durable::Flush;

    use super::super::{
        DocumentHostError, RegularReadHandle, StepFailure, admit_write_lock, regular_read_opened,
    };
    use crate::io::SaveTouch;

    /// A commit's folder: the descriptor every step reaches the folder's entries through.
    pub(in crate::host) struct Folder {
        dir: File,
    }

    /// How each folder of the path is opened, the commit's own included: to look names up in
    /// alone, which needs no read permission on it, as a path's lookup needs none, so a folder
    /// this user may write and search but not list (a drop box) takes a save as it did by path:
    /// macOS's `O_SEARCH`, Linux's `O_PATH`, for reading elsewhere. Each open adds how a link is
    /// refused: `O_NOFOLLOW` for a name in a folder already open, macOS's `O_NOFOLLOW_ANY` for a
    /// whole path (the two together are `EINVAL` there).
    #[cfg(target_vendor = "apple")]
    fn folder_flags() -> OFlags {
        OFlags::SEARCH | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn folder_flags() -> OFlags {
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    fn folder_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NONBLOCK | OFlags::CLOEXEC
    }

    /// `fd` as a folder, or `ENOTDIR`.
    fn folder(fd: OwnedFd) -> io::Result<File> {
        if !FileType::from_raw_mode(fstat(&fd)?.st_mode).is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        Ok(File::from(fd))
    }

    /// The folder `name` in the open folder `at`, opened with `flags` and never through a link
    /// in its place: a link there answers `ELOOP` (where `O_DIRECTORY` says `ENOTDIR` first, or
    /// `O_PATH` opens the link itself), anything else not a folder `ENOTDIR`.
    fn child(at: BorrowedFd<'_>, name: &OsStr, flags: OFlags) -> io::Result<OwnedFd> {
        let loop_error = || io::Error::from_raw_os_error(libc::ELOOP);
        let fd = match openat(at, name, flags | OFlags::NOFOLLOW, Mode::empty()) {
            Ok(fd) => fd,
            Err(errno) => {
                let error = io::Error::from(errno);
                let link_there = error.raw_os_error() == Some(libc::ENOTDIR)
                    && fstatat(at, name, AtFlags::SYMLINK_NOFOLLOW)
                        .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode).is_symlink());
                return Err(if link_there { loop_error() } else { error });
            }
        };
        let kind = FileType::from_raw_mode(fstat(&fd)?.st_mode);
        if kind.is_symlink() {
            return Err(loop_error());
        }
        if !kind.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        Ok(fd)
    }

    /// The folder at the absolute `path`, opened with no link followed anywhere on it: a link
    /// in the place of any folder of the path, or of the folder itself, answers `ELOOP`.
    /// macOS's `O_NOFOLLOW_ANY` does it in one open, with a path lookup's permissions.
    #[cfg(target_vendor = "apple")]
    fn open_no_links(path: &Path) -> io::Result<File> {
        folder(openat(
            CWD,
            path,
            folder_flags() | OFlags::NOFOLLOW_ANY,
            Mode::empty(),
        )?)
    }

    #[cfg(not(target_vendor = "apple"))]
    fn open_no_links(path: &Path) -> io::Result<File> {
        walk(path)
    }

    /// [`open_no_links`] one name at a time from `/`, each folder looked up in the one before
    /// it and opened without following a link in its place.
    #[cfg_attr(
        all(target_vendor = "apple", not(test)),
        allow(dead_code, reason = "Apple opens a path in one call")
    )]
    pub(in crate::host) fn walk(path: &Path) -> io::Result<File> {
        if !path.has_root() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a commit's folder is named by an absolute path",
            ));
        }
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => names.push(name),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a commit's folder is named by a canonical path",
                    ));
                }
            }
        }
        let Some((last, above)) = names.split_last() else {
            return folder(openat(CWD, "/", folder_flags(), Mode::empty())?);
        };
        let mut at = openat(CWD, "/", folder_flags(), Mode::empty())?;
        for name in above {
            at = child(at.as_fd(), name, folder_flags())?;
        }
        Ok(File::from(child(at.as_fd(), last, folder_flags())?))
    }

    impl Folder {
        /// Open the folder at the canonical `path` ([`open_no_links`]); with `create`, make the
        /// folders of it that are missing first, each in the folder above it.
        pub(in crate::host) fn open(path: &Path, create: bool) -> io::Result<Self> {
            let mut missing = Vec::new();
            let mut at = path;
            let mut dir = loop {
                match open_no_links(at) {
                    Ok(dir) => break dir,
                    Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                        let (Some(name), Some(above)) = (at.file_name(), at.parent()) else {
                            return Err(error);
                        };
                        missing.push(name);
                        at = above;
                    }
                    Err(error) => return Err(error),
                }
            };
            for name in missing.into_iter().rev() {
                match mkdirat(&dir, name, Mode::from_bits(0o777)) {
                    Ok(()) => {}
                    Err(errno) if errno.raw_os_error() == libc::EEXIST => {}
                    Err(errno) => return Err(errno.into()),
                }
                dir = File::from(child(dir.as_fd(), name, folder_flags())?);
            }
            Ok(Self { dir })
        }

        /// Whether the folder `path` names now ([`open_no_links`]) is still this one (device and
        /// inode): `Ok(false)` where none is there, and `ELOOP` where a link stands on it.
        pub(in crate::host) fn is_reached_by(&self, path: &Path) -> io::Result<bool> {
            let now = match open_no_links(path) {
                Ok(now) => now,
                Err(error) if super::is_missing(&error) => return Ok(false),
                Err(error) => return Err(error),
            };
            let (now, held) = (fstat(&now)?, fstat(&self.dir)?);
            Ok(now.st_dev == held.st_dev && now.st_ino == held.st_ino)
        }

        /// Open (making it, owner-only) the save lock `name` in this folder; `path` names it
        /// in a failure's message.
        pub(in crate::host) fn open_write_lock(
            &self,
            name: &OsStr,
            path: &Path,
        ) -> Result<File, StepFailure> {
            let opened = openat(
                &self.dir,
                name,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|errno| {
                let error = io::Error::from(errno);
                let touch = if fstatat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
                    SaveTouch::Lock
                } else {
                    SaveTouch::Folder
                };
                StepFailure::io(
                    touch,
                    format!("could not open save lock {}: {error}", path.display()),
                    &error,
                )
            })?;
            admit_write_lock(File::from(opened), path)
        }

        /// Open the regular file `name` in this folder for reading, never through a link and
        /// never waiting on a special file; `path` names it in a failure.
        pub(in crate::host) fn open_regular_read(
            &self,
            name: &OsStr,
            path: &Path,
        ) -> Result<RegularReadHandle, DocumentHostError> {
            regular_read_opened(
                path,
                openat(
                    &self.dir,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(io::Error::from),
            )
        }

        /// Make a fresh temporary for `name` in this folder with `mode` (narrowed by the umask):
        /// its name, and the file open for writing.
        pub(in crate::host) fn create_temporary(
            &self,
            name: &OsStr,
            seed: u64,
            mode: u32,
        ) -> Result<(OsString, File), StepFailure> {
            #[allow(
                clippy::unnecessary_cast,
                reason = "mode_t is u16 on Apple and u32 on Linux"
            )]
            let mode = Mode::from_bits(mode as libc::mode_t);
            for attempt in 0..super::TEMPORARY_ATTEMPTS {
                let temporary = OsString::from(super::temporary_name(name, seed, attempt));
                match openat(
                    &self.dir,
                    &temporary,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::NONBLOCK
                        | OFlags::CLOEXEC,
                    mode,
                ) {
                    Ok(file) => return Ok((temporary, File::from(file))),
                    Err(errno) if errno.raw_os_error() == libc::EEXIST => {}
                    Err(errno) => {
                        let error = io::Error::from(errno);
                        return Err(StepFailure::io(
                            SaveTouch::Folder,
                            error.to_string(),
                            &error,
                        ));
                    }
                }
            }
            Err(StepFailure::own(
                "could not allocate a unique sibling temporary file",
            ))
        }

        /// The permissions of the regular file `name` in this folder, where one is there.
        pub(in crate::host) fn regular_permissions(&self, name: &OsStr) -> Option<fs::Permissions> {
            let stat = fstatat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
            FileType::from_raw_mode(stat.st_mode)
                .is_file()
                .then(|| fs::Permissions::from_mode(stat.st_mode & 0o7777))
        }

        /// Publish `temporary` as `name`: replace it where it `existed`, or make it only if it is
        /// still absent (a link, then the temporary's name removed).
        pub(in crate::host) fn publish(
            &self,
            temporary: &OsStr,
            name: &OsStr,
            existed: bool,
        ) -> io::Result<()> {
            if existed {
                renameat(&self.dir, temporary, &self.dir, name)?;
            } else {
                linkat(&self.dir, temporary, &self.dir, name, AtFlags::empty())?;
                unlinkat(&self.dir, temporary, AtFlags::empty())?;
            }
            Ok(())
        }

        /// Remove `temporary` from this folder.
        pub(in crate::host) fn remove(&self, temporary: &OsStr) -> io::Result<()> {
            Ok(unlinkat(&self.dir, temporary, AtFlags::empty())?)
        }

        /// Flush this folder's entries to the device with `flush`: through the handle on macOS
        /// (an `O_SEARCH` descriptor syncs) and elsewhere off Linux. Linux's `O_PATH` handle
        /// takes no `fsync` (`EBADF`), so there the folder is first opened through it for
        /// reading, which a folder this user may not list refuses: such a save is published
        /// and reported unverified, as one by path was.
        pub(in crate::host) fn sync(&self, flush: Flush) -> io::Result<()> {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                let readable = openat(
                    &self.dir,
                    ".",
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                aterm_gui_consent::durable::finish(&File::from(readable), flush)
            }
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            {
                aterm_gui_consent::durable::finish(&self.dir, flush)
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File};
    use std::io;
    use std::path::{Path, PathBuf};

    use aterm_gui_consent::durable::Flush;

    use super::super::{
        DocumentHostError, RegularReadHandle, StepFailure, open_regular_read, open_write_lock,
        replace_file, sync_directory,
    };
    use crate::io::SaveTouch;

    /// A commit's folder, by its path: every step names its entries by path.
    pub(in crate::host) struct Folder {
        path: PathBuf,
    }

    impl Folder {
        pub(in crate::host) fn open(path: &Path, create: bool) -> io::Result<Self> {
            if create {
                fs::create_dir_all(path)?;
            } else if !fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            Ok(Self {
                path: path.to_path_buf(),
            })
        }

        /// No handle is held here, so nothing to compare.
        pub(in crate::host) fn is_reached_by(&self, _path: &Path) -> io::Result<bool> {
            Ok(true)
        }

        pub(in crate::host) fn open_write_lock(
            &self,
            _name: &OsStr,
            path: &Path,
        ) -> Result<File, StepFailure> {
            open_write_lock(path)
        }

        pub(in crate::host) fn open_regular_read(
            &self,
            _name: &OsStr,
            path: &Path,
        ) -> Result<RegularReadHandle, DocumentHostError> {
            open_regular_read(path)
        }

        pub(in crate::host) fn create_temporary(
            &self,
            name: &OsStr,
            seed: u64,
            _mode: u32,
        ) -> Result<(OsString, File), StepFailure> {
            for attempt in 0..super::TEMPORARY_ATTEMPTS {
                let temporary = OsString::from(super::temporary_name(name, seed, attempt));
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(self.path.join(&temporary))
                {
                    Ok(file) => return Ok((temporary, file)),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(StepFailure::io(
                            SaveTouch::Folder,
                            error.to_string(),
                            &error,
                        ));
                    }
                }
            }
            Err(StepFailure::own(
                "could not allocate a unique sibling temporary file",
            ))
        }

        pub(in crate::host) fn regular_permissions(&self, name: &OsStr) -> Option<fs::Permissions> {
            fs::symlink_metadata(self.path.join(name))
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map(|metadata| metadata.permissions())
        }

        pub(in crate::host) fn publish(
            &self,
            temporary: &OsStr,
            name: &OsStr,
            existed: bool,
        ) -> io::Result<()> {
            replace_file(&self.path.join(temporary), &self.path.join(name), existed)
        }

        pub(in crate::host) fn remove(&self, temporary: &OsStr) -> io::Result<()> {
            fs::remove_file(self.path.join(temporary))
        }

        pub(in crate::host) fn sync(&self, flush: Flush) -> io::Result<()> {
            sync_directory(&self.path, flush)
        }
    }
}
