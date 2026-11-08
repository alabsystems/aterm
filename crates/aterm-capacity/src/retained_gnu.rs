// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The original cohort's one-use GNU transport epoch. Only the owning cohort
//! creates it from unused retained CPU custody, and only that same cohort's
//! native closure permits burning its returned supplementary bytes.

use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read as _, Write as _},
    num::NonZeroU32,
    os::{
        fd::{AsFd as _, OwnedFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _},
        },
    },
    path::PathBuf,
};

fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// Original transport descriptions for one atomic native child handoff.
/// This value grants no CPU or settlement authority. Its creating cohort
/// retains the epoch and every CPU capability until positive retirement.
#[derive(Debug)]
pub struct Handoff {
    pub(super) read: OwnedFd,
    pub(super) write: OwnedFd,
    pub(super) path: PathBuf,
}

impl Handoff {
    /// Consume this handoff without creating another scheduler or pool.
    #[must_use]
    pub fn into_parts(self) -> ([OwnedFd; 2], PathBuf) {
        ([self.read, self.write], self.path)
    }
}

#[derive(Debug)]
struct Transport {
    // This private namespace and all original open descriptions live together.
    // The control description alone is nonblocking; child descriptions are
    // separately opened, so retirement never changes a borrower's OFD flags.
    directory: aterm_tempfile::TempDir,
    control: File,
    read: File,
    write: File,
    identity: (u64, u64),
    supplementary: usize,
    burned: usize,
    poisoned: bool,
    retired: bool,
}

impl Transport {
    fn new(ceiling: NonZeroU32) -> io::Result<Self> {
        let directory = aterm_tempfile::Builder::new()
            .prefix("aterm-cohort-gnu-")
            .suffix(".noindex")
            .tempdir()?;
        let path = directory.path().join("credits");
        if path
            .as_os_str()
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            return Err(refused("original GNU FIFO locator contains whitespace"));
        }
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| refused("original GNU FIFO path contains NUL"))?;
        let (mut control, (read, write)) = aterm_uds::kept_out(|| {
            // SAFETY: this is a fresh entry in our new private directory.
            if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let control = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)?;
            let read = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)?;
            let write = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)?;
            Ok::<_, io::Error>((control, (read, write)))
        })?;
        let supplementary = ceiling.get() as usize - 1;
        // The executing root already consumes the implicit CPU unit. Only
        // the separately owned paid supplementary units become GNU bytes.
        control.write_all(&vec![b'+'; supplementary])?;
        let metadata = control.metadata()?;
        let epoch = Self {
            directory,
            control,
            read,
            write,
            identity: (metadata.dev(), metadata.ino()),
            supplementary,
            burned: 0,
            poisoned: false,
            retired: false,
        };
        epoch.check()?;
        // Only these fresh, unpublished descriptions change flags. The
        // separately opened control remains nonblocking for bounded retirement.
        for file in [&epoch.read, &epoch.write] {
            use std::os::fd::AsRawFd as _;
            // SAFETY: these are the original owned FIFO descriptions, before
            // any handoff or clone. Both fcntl operations preserve access mode.
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0
                || unsafe {
                    libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK)
                } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(epoch)
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("credits")
    }

    pub(super) fn check(&self) -> io::Result<()> {
        for metadata in [
            self.control.metadata()?,
            self.read.metadata()?,
            self.write.metadata()?,
            std::fs::symlink_metadata(self.path())?,
        ] {
            if !metadata.file_type().is_fifo() || (metadata.dev(), metadata.ino()) != self.identity
            {
                return Err(refused("original GNU FIFO identity changed"));
            }
        }
        Ok(())
    }

    pub(super) fn handoff(&self) -> io::Result<Handoff> {
        self.check()?;
        Ok(Handoff {
            read: aterm_uds::kept_out(|| self.read.as_fd().try_clone_to_owned())?,
            write: aterm_uds::kept_out(|| self.write.as_fd().try_clone_to_owned())?,
            path: self.path(),
        })
    }

    /// The owning cohort calls this only AFTER its actual native closure.
    /// Missing bytes retain the original CPU debt and the partial burn; no
    /// Drop or retry replenishes credits. An unfunded byte poisons the epoch.
    pub(super) fn retire(&mut self) -> io::Result<()> {
        if self.poisoned {
            return Err(refused("original GNU transport retirement is poisoned"));
        }
        if self.retired {
            return Ok(());
        }
        self.check()?;
        let mut byte = [0];
        loop {
            match self.control.read(&mut byte) {
                Ok(1) if self.burned < self.supplementary && byte[0] == b'+' => {
                    self.burned += 1;
                }
                Ok(1) => {
                    self.poisoned = true;
                    return Err(refused("original GNU transport has an unfunded byte"));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if self.burned != self.supplementary {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "original GNU supplementary bytes remain borrowed",
                        ));
                    }
                    self.retired = true;
                    return Ok(());
                }
                Ok(_) => return Err(refused("original GNU FIFO unexpectedly closed")),
                Err(error) => return Err(error),
            }
        }
    }
}

/// Actual local preparation survives failed persistence and failed opens.
/// Its random original identity has no constructor from serialized data.
#[derive(Debug)]
pub(crate) struct Epoch {
    id: String,
    transport: Option<Transport>,
    retired: bool,
}

impl Epoch {
    pub(super) fn fresh() -> io::Result<Self> {
        Ok(Self {
            id: aterm_uds::rand::hex_token::<16>()?,
            transport: None,
            retired: false,
        })
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(super) fn open(&mut self, ceiling: NonZeroU32) -> io::Result<()> {
        if self.transport.is_some() || self.retired {
            return Err(refused("original GNU transport preparation is one-use"));
        }
        self.transport = Some(Transport::new(ceiling)?);
        Ok(())
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if self.retired {
            return Err(refused("original GNU epoch already retired"));
        }
        self.transport
            .as_ref()
            .ok_or_else(|| refused("original GNU preparation is incomplete"))?
            .check()
    }
    pub(super) fn handoff(&self) -> io::Result<Handoff> {
        if self.retired {
            return Err(refused("original GNU epoch already retired"));
        }
        self.transport
            .as_ref()
            .ok_or_else(|| refused("original GNU preparation is incomplete"))?
            .handoff()
    }
    pub(super) fn retire(&mut self) -> io::Result<()> {
        if let Some(transport) = &mut self.transport {
            transport.retire()?;
        }
        // No transport means preparation failed before any description could
        // be handed out. The same original owner can positively cancel it.
        self.retired = true;
        Ok(())
    }
    pub(crate) fn retirement_id(&self) -> io::Result<&str> {
        if !self.retired {
            return Err(refused("original GNU epoch has no owning retirement"));
        }
        Ok(&self.id)
    }
}
