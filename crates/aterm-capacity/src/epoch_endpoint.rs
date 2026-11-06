// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original transport-node custody; only the epoch controller authorizes cleanup.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Component, Path, PathBuf};

use aterm_dirfd::{self as fd, AtFlags, Mode, OFlags};

const FIFO: &str = "credits.fifo";

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[derive(Debug, PartialEq, Eq)]
struct Identity(u64, u64);

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn child_directory(parent: &File, name: impl fd::Arg) -> io::Result<File> {
    aterm_uds::kept_out(|| {
        fd::openat(parent, name, directory_flags(), Mode::empty())
            .map(File::from)
            .map_err(io::Error::from)
    })
}

fn directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(refused("epoch parent must be absolute"));
    }
    let mut directory = aterm_uds::kept_out(|| {
        fd::openat(fd::CWD, "/", directory_flags(), Mode::empty())
            .map(File::from)
            .map_err(io::Error::from)
    })?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = child_directory(&directory, name)?,
            _ => {
                return Err(refused(
                    "epoch parent must have a canonical component chain",
                ));
            }
        }
    }
    Ok(directory)
}

fn owned(file: &File, private: bool) -> io::Result<(Identity, std::fs::Metadata)> {
    let metadata = file.metadata()?;
    // SAFETY: geteuid only reads this process's effective account identity.
    let uid = unsafe { libc::geteuid() };
    let disallowed = if private { 0o077 } else { 0o022 };
    if metadata.uid() != uid || metadata.mode() & disallowed != 0 {
        return Err(refused(
            "epoch node must remain owned with its original privacy",
        ));
    }
    Ok((Identity(metadata.dev(), metadata.ino()), metadata))
}

#[derive(Debug)]
struct Captured {
    directory: File,
    directory_identity: Identity,
    fifo: File,
    fifo_identity: Identity,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Awaiting,
    Captured,
    UnlinkedPendingSync,
    Cleaned,
    Refused,
}

#[derive(Debug)]
pub(super) struct Endpoint {
    path: PathBuf,
    leaf: OsString,
    parent: File,
    parent_identity: Identity,
    captured: Option<Captured>,
    phase: Phase,
}

impl Endpoint {
    pub(super) fn capture_logs(&self, id: u32) -> io::Result<super::epoch_recipe::CapturedLogs> {
        self.validate()?;
        let captured = self
            .captured
            .as_ref()
            .ok_or_else(|| refused("missing original epoch directory"))?;
        let open = |side: &str| -> io::Result<File> {
            let file = aterm_uds::kept_out(|| {
                fd::openat(
                    &captured.directory,
                    format!("root-{id}.{side}"),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(io::Error::from)
            })?;
            let meta = super::epoch_recipe::check_log(&file)?;
            if meta.len() != 0 {
                return Err(refused("root log was written before controller Spawn"));
            }
            Ok(file)
        };
        let logs = super::epoch_recipe::CapturedLogs {
            stdout: open("stdout")?,
            stderr: open("stderr")?,
        };
        self.validate()?;
        Ok(logs)
    }

    pub(super) fn prepare(path: &Path) -> io::Result<Self> {
        let text = path
            .to_str()
            .ok_or_else(|| refused("epoch path must be UTF-8"))?;
        if !path.is_absolute()
            || text.chars().any(char::is_whitespace)
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(refused(
                "epoch path must be absolute, normalized and GNU-representable",
            ));
        }
        let parent_path = path
            .parent()
            .ok_or_else(|| refused("epoch path has no parent"))?;
        let leaf = path
            .file_name()
            .ok_or_else(|| refused("epoch path has no leaf"))?
            .to_owned();
        // Resolve an existing alias such as macOS /tmp once, then pin and walk
        // its actual parent without following any further component links.
        let parent = directory(&parent_path.canonicalize()?)?;
        let (parent_identity, _) = owned(&parent, false)?;
        match fd::fstatat(&parent, &leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Err(error) if error.is_noent() => {}
            Err(error) => return Err(error.into()),
            Ok(_) => return Err(refused("epoch directory must be absent before launch")),
        }
        Ok(Self {
            path: path.to_owned(),
            leaf,
            parent_identity,
            parent,
            captured: None,
            phase: Phase::Awaiting,
        })
    }

    pub(super) fn directory(&self) -> &Path {
        &self.path
    }

    pub(super) fn fifo_path(&self) -> PathBuf {
        self.path.join(FIFO)
    }

    fn validate_parent(&self) -> io::Result<()> {
        owned(&self.parent, false)?;
        let current = directory(
            &self
                .path
                .parent()
                .ok_or_else(|| refused("missing epoch parent"))?
                .canonicalize()?,
        )?;
        let (current_identity, _) = owned(&current, false)?;
        if current_identity != self.parent_identity {
            return Err(refused("original epoch parent identity changed"));
        }
        Ok(())
    }

    pub(super) fn capture(&mut self) -> io::Result<()> {
        if self.phase != Phase::Awaiting {
            return Err(refused("epoch endpoint capture is one-use"));
        }
        let captured = (|| {
            self.validate_parent()?;
            let directory = child_directory(&self.parent, &self.leaf)?;
            let (directory_identity, _) = owned(&directory, true)?;
            let fifo = aterm_uds::kept_out(|| {
                fd::openat(
                    &directory,
                    FIFO,
                    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(io::Error::from)
            })?;
            let (fifo_identity, metadata) = owned(&fifo, true)?;
            if !metadata.file_type().is_fifo() || metadata.nlink() != 1 {
                return Err(refused("epoch endpoint is not its single-link FIFO"));
            }
            Ok(Captured {
                directory_identity,
                fifo_identity,
                directory,
                fifo,
            })
        })();
        match captured {
            Ok(captured) => {
                self.captured = Some(captured);
                self.phase = Phase::Captured;
                if let Err(error) = self.validate() {
                    self.phase = Phase::Refused;
                    return Err(error);
                }
                Ok(())
            }
            Err(error) => {
                self.phase = Phase::Refused;
                Err(error)
            }
        }
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if self.phase != Phase::Captured {
            return Err(refused("epoch endpoint has no captured live-node custody"));
        }
        self.validate_parent()?;
        let captured = self
            .captured
            .as_ref()
            .ok_or_else(|| refused("missing epoch handles"))?;
        let (captured_dir_id, _) = owned(&captured.directory, true)?;
        let (captured_fifo_id, _) = owned(&captured.fifo, true)?;
        let current_directory = child_directory(&self.parent, &self.leaf)?;
        let (current_dir_id, _) = owned(&current_directory, true)?;
        let current_fifo = aterm_uds::kept_out(|| {
            fd::openat(
                &current_directory,
                FIFO,
                OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(io::Error::from)
        })?;
        let (current_fifo_id, metadata) = owned(&current_fifo, true)?;
        if current_dir_id != captured.directory_identity
            || current_fifo_id != captured.fifo_identity
            || captured_dir_id != captured.directory_identity
            || captured_fifo_id != captured.fifo_identity
            || !metadata.file_type().is_fifo()
            || metadata.nlink() != 1
        {
            return Err(refused("original epoch directory or FIFO identity changed"));
        }
        Ok(())
    }

    // The owning controller must already hold genuine retirement, actual child
    // exit/EOF and its original claim's durable whole-cohort closure.
    pub(super) fn cleanup(&mut self) -> io::Result<()> {
        match self.phase {
            Phase::Cleaned => return Ok(()),
            Phase::UnlinkedPendingSync => {}
            Phase::Captured => {
                if let Err(error) = self.validate() {
                    self.phase = Phase::Refused;
                    return Err(error);
                }
                let captured = self
                    .captured
                    .as_ref()
                    .ok_or_else(|| refused("missing epoch handles"))?;
                let mut available: libc::c_int = 0;
                // SAFETY: this original read-only FIFO descriptor remains live;
                // FIONREAD initializes exactly one c_int. Unlike O_RDWR FIFO
                // handles, read-only handles report inventory correctly on Darwin.
                if unsafe {
                    libc::ioctl(
                        captured.fifo.as_raw_fd(),
                        libc::FIONREAD,
                        &raw mut available,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error());
                }
                if available != 0 {
                    self.phase = Phase::Refused;
                    return Err(refused(
                        "retired epoch FIFO still contains explicit credits",
                    ));
                }
                #[cfg(test)]
                fail_unlink()?;
                fd::unlinkat(&captured.directory, FIFO, AtFlags::empty())?;
                // Record actual unlink before attempting a fallible directory
                // flush. A retry must never require the removed FIFO to reappear.
                self.phase = Phase::UnlinkedPendingSync;
            }
            Phase::Awaiting | Phase::Refused => {
                return Err(refused("epoch endpoint cannot authorize cleanup"));
            }
        }
        let captured = self
            .captured
            .as_ref()
            .ok_or_else(|| refused("missing original epoch directory"))?;
        let (captured_dir_id, _) = owned(&captured.directory, true)?;
        let (captured_fifo_id, fifo_meta) = owned(&captured.fifo, true)?;
        if captured_dir_id != captured.directory_identity
            || captured_fifo_id != captured.fifo_identity
            || fifo_meta.nlink() != 0
        {
            self.phase = Phase::Refused;
            return Err(refused(
                "unlinked original epoch node regained a name or changed custody",
            ));
        }
        match fd::fstatat(&captured.directory, FIFO, AtFlags::SYMLINK_NOFOLLOW) {
            Err(error) if error.is_noent() => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                self.phase = Phase::Refused;
                return Err(refused("a foreign node replaced the unlinked epoch FIFO"));
            }
        }
        #[cfg(test)]
        fail_sync()?;
        aterm_durable::durable(&captured.directory)?;
        self.phase = Phase::Cleaned;
        Ok(())
    }

    pub(super) fn is_cleaned(&self) -> bool {
        self.phase == Phase::Cleaned
    }

    // Only the owning controller's genuine no-child cancellation can call
    // this, after the original claim durably confirms that no helper launched.
    pub(super) fn finish_unlaunched(&mut self) -> io::Result<()> {
        if self.phase == Phase::Cleaned {
            return Ok(());
        }
        if self.phase != Phase::Awaiting {
            return Err(refused(
                "published epoch storage cannot finish as unlaunched",
            ));
        }
        self.validate_parent()?;
        match fd::fstatat(&self.parent, &self.leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Err(error) if error.is_noent() => {
                self.phase = Phase::Cleaned;
                Ok(())
            }
            Err(error) => Err(error.into()),
            Ok(_) => Err(refused("unlaunched epoch path unexpectedly exists")),
        }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_UNLINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_unlink() {
    FAIL_UNLINK.set(true);
}

#[cfg(test)]
pub(crate) fn fail_next_sync() {
    FAIL_SYNC.set(true);
}

#[cfg(test)]
fn fail_unlink() -> io::Result<()> {
    if FAIL_UNLINK.replace(false) {
        Err(io::Error::other("injected original FIFO unlink failure"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn fail_sync() -> io::Result<()> {
    if FAIL_SYNC.replace(false) {
        Err(io::Error::other("injected original directory sync failure"))
    } else {
        Ok(())
    }
}
