// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Close-only custody of an original ordinary memory reservation.

use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{OwnedFd, RawFd};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use aterm_toolchain::record_lock::CloseOnlyLock;

use crate::{Accounting, Ledger, Record};

/// An actual detached ordinary memory claim, held until the last kernel guard closes.
///
/// The original record and declared peak are preserved; its detached accounting ignores
/// numeric holder credit, ancestry and coverage. This type cannot mutate a claim, bind or
/// retire a cohort, explicitly unlock, or issue Timing evidence. Paths and records returned
/// here are observations, not constructors or permission to release another reservation.
#[derive(Debug)]
pub struct RetainedClaim {
    guard: CloseOnlyLock,
    path: PathBuf,
    record: Record,
}

impl RetainedClaim {
    pub(crate) fn original(guard: CloseOnlyLock, path: PathBuf, record: Record) -> Self {
        Self {
            guard,
            path,
            record,
        }
    }

    /// The original held inode's metadata, without reopening its pathname.
    /// # Errors
    /// The held descriptor cannot be read.
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.guard.metadata()
    }

    /// Duplicate the original guard for explicit controlled descriptor transfer. The
    /// recipient must preserve close-only handling; a low-level FD export is not admission.
    /// # Errors
    /// The OS cannot duplicate the original descriptor.
    pub fn try_clone_fd(&self) -> io::Result<OwnedFd> {
        self.guard.try_clone_fd()
    }

    /// The original claim locator; possessing this path alone supplies no guard custody.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Immutable observation of the original claim, with detached accounting.
    #[must_use]
    pub const fn record(&self) -> &Record {
        &self.record
    }

    /// Adopt an actual inherited detached memory guard in an independently selected ledger.
    /// No pathname or numeric budget on the command line creates a reservation.
    ///
    /// # Safety
    /// Call before threads/callbacks can replace or use `fd`, under the exclusive, continuous
    /// original transfer custody required by [`CloseOnlyLock::adopt_inherited`]. `ledger` must
    /// be the receiver's independently selected canonical account ledger, not one selected by
    /// a sender's pathname or environment. Ineligible descriptor offers remain unowned; a
    /// descriptor accepted by the lock boundary is consumed even if its record then refuses.
    ///
    /// # Errors
    /// The legacy ledger, exact leaf/inode/OFD, detached marker, stable bounded original
    /// record, admission or non-covering memory-only budget cannot be established.
    pub unsafe fn adopt_inherited(fd: RawFd, ledger: &Ledger, leaf: &str) -> io::Result<Self> {
        if ledger.cohort_aware()?
            || !ledger.dir().is_absolute()
            || std::fs::canonicalize(ledger.dir())? != ledger.dir()
        {
            return Err(invalid(
                "detached memory needs the original canonical ordinary ledger",
            ));
        }
        aterm_uds::private_file::check_private_dir(ledger.dir())?;
        let stem = leaf.strip_suffix(".claim");
        if leaf.len() > 96
            || !stem.is_some_and(|stem| {
                !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            })
        {
            return Err(invalid("invalid original memory claim leaf"));
        }
        let path = ledger.dir().join(leaf);
        // SAFETY: the caller supplies the startup ownership and continuous original chain;
        // the expected file is derived only from the independently selected legacy ledger.
        let guard = unsafe { CloseOnlyLock::adopt_inherited(fd, &path) }?;
        let record = read_record(guard.try_clone_fd()?, &path)?;
        check_record(&record, &path, Accounting::Detached)?;
        Ok(Self {
            guard,
            path,
            record,
        })
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

pub(crate) fn check_record(record: &Record, path: &Path, accounting: Accounting) -> io::Result<()> {
    let leaf = format!("{}.claim", record.cover_marker());
    if record.accounting != accounting
        || record.held.is_none()
        || !crate::ledger::memory_only(&record.need)
        || path.file_name() != Some(std::ffi::OsStr::new(&leaf))
    {
        return Err(invalid(
            "the original admitted memory-only record does not match its exact claim leaf",
        ));
    }
    Ok(())
}

pub(crate) fn read_record(fd: OwnedFd, path: &Path) -> io::Result<Record> {
    let file = File::from(fd);
    let before = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    let stamp = |m: &Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.ctime(),
            m.ctime_nsec(),
            m.mtime(),
            m.mtime_nsec(),
        )
    };
    if !before.is_file()
        || !named.is_file()
        || before.dev() != named.dev()
        || before.ino() != named.ino()
        || before.len() > crate::ledger::RECORD_MAX
    {
        return Err(invalid(
            "original memory guard inode or bounded record is unavailable",
        ));
    }
    let mut bytes = vec![0; before.len() as usize];
    file.read_exact_at(&mut bytes, 0)?;
    if stamp(&file.metadata()?) != stamp(&before)
        || stamp(&std::fs::symlink_metadata(path)?) != stamp(&before)
    {
        return Err(invalid("original memory record changed while read"));
    }
    if !bytes.ends_with(b"\n") {
        return Err(invalid("incomplete original memory record"));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| invalid("original memory record is not UTF-8"))?;
    let record = Record::decode(text, true)
        .ok_or_else(|| invalid("original memory record cannot be decoded"))?;
    if record.accounting == Accounting::Detached && !bytes.ends_with(crate::ledger::DETACHED_MEMORY)
    {
        return Err(invalid(
            "detached memory intent is not the complete final record event",
        ));
    }
    Ok(record)
}
