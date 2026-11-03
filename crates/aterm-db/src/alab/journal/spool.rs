// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local spool (design §8): `<state>/alab/spool.jsonl`, one event a line, appended by any
//! process that notes what the harness sees ([`Spool::append`]) and taken whole by the one
//! flusher (the Commons owner's, `super::flush`), which keeps what it has not published in its
//! own outbox beside it.
//!
//! An append is the typing hook's whole cost: it encodes the event into a stack buffer, opens the
//! file for appending by a path made once, checks the file's size and writes one line of at most
//! 193 bytes, so it allocates nothing once the folder exists, takes no lock of any store and does
//! no other I/O. A full spool (4 MiB) drops the event rather than wait.
//!
//! The flusher takes the spool by renaming it (`spool.taken`) and reads the taken file only on its
//! next pass, a minute later, so an append that opened the file just before the rename has long
//! finished its one write by then: no line is lost to the take, and no writer waits on it. The
//! taken file is replaced (or removed) only once the outbox holds what was read from it, so a
//! crash or a failed write between the two loses nothing: the next pass reads the same file
//! again, and the outbox keeps each event once.

use super::event::{Event, MAX_EVENT};
use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
};

/// The file writers append to.
pub const SPOOL: &str = "spool.jsonl";
/// The spool as the flusher took it on its last pass.
pub const TAKEN: &str = "spool.taken";
/// The flusher's own queue: absorbed events not yet in a ring table.
pub const OUTBOX: &str = "outbox.jsonl";
/// The spool and the outbox each hold at most this much (design §8).
pub const MAX_SPOOL: u64 = 4 * 1024 * 1024;
/// Events older than this are dropped unpublished (design §8).
pub const KEEP_S: u64 = 30 * 86_400;

/// What one append did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appended {
    /// One line was written.
    Written,
    /// The spool is at its cap: the event is dropped, never waited for.
    Full,
    /// The event does not encode within its cap.
    Refused,
}

/// A journal spool in one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spool {
    dir: PathBuf,
    file: PathBuf,
}

impl Spool {
    /// The spool in `dir`.
    pub fn at(dir: PathBuf) -> Self {
        Self {
            file: dir.join(SPOOL),
            dir,
        }
    }

    /// This user's spool: `<aterm state>/alab/`.
    pub fn local() -> Option<Self> {
        aterm_types::dirs::state_dir().map(|state| Self::at(state.join("alab")))
    }

    /// The folder it lives in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append `event` as one line: the encoding in a stack buffer, an open for appending by the
    /// path made at construction, a size check and one write; no allocation once the folder
    /// exists (it is made 0700 on the first append), no lock, no other I/O.
    ///
    /// # Errors
    /// The folder cannot be made private, or the file cannot be opened or written.
    pub fn append(&self, event: &Event) -> io::Result<Appended> {
        let mut buf = [0u8; MAX_EVENT + 1];
        let Some(n) = event.encode(&mut buf[..MAX_EVENT]) else {
            return Ok(Appended::Refused);
        };
        buf[n] = b'\n';
        let line = &buf[..=n];
        let (file, meta) = match open_private_append(&self.file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                aterm_uds::private_file::ensure_private_dir(&self.dir)?;
                open_private_append(&self.file)?
            }
            opened => opened?,
        };
        if meta.len().saturating_add(line.len() as u64) > MAX_SPOOL {
            return Ok(Appended::Full);
        }
        (&file).write_all(line)?;
        Ok(Appended::Written)
    }

    /// The flusher's intake: the events of the file it took on its previous pass, which stays
    /// until [`Self::take`]. A line that is not an event is skipped.
    ///
    /// # Errors
    /// The taken file exists and cannot be read.
    pub(crate) fn absorb(&self) -> io::Result<Vec<Event>> {
        match self.read(TAKEN, MAX_SPOOL + MAX_EVENT as u64 * 64) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            read => read,
        }
    }

    /// Take the spool for the next pass: rename it over the file the last pass took, or remove
    /// that file when nothing was appended since. Called once the outbox holds what
    /// [`Self::absorb`] returned. A failed rename leaves the taken file as it was: the next
    /// pass reads it again, and the outbox, which already holds its events, keeps each once.
    ///
    /// # Errors
    /// The spool cannot be renamed, or the taken file removed.
    pub(crate) fn take(&self) -> io::Result<()> {
        match std::fs::rename(&self.file, self.dir.join(TAKEN)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match std::fs::remove_file(self.dir.join(TAKEN)) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                    _ => Ok(()),
                }
            }
            renamed => renamed,
        }
    }

    /// The outbox: absorbed events not yet published, oldest first.
    ///
    /// # Errors
    /// The outbox exists and cannot be read.
    pub(crate) fn outbox(&self) -> io::Result<Vec<Event>> {
        match self.read(OUTBOX, MAX_SPOOL) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            read => read,
        }
    }

    /// Replace the outbox with `events`.
    ///
    /// # Errors
    /// The folder cannot be made private or the file written.
    pub(crate) fn save_outbox(&self, events: &[Event]) -> io::Result<()> {
        let mut text = Vec::with_capacity(events.len() * 128);
        let mut buf = [0u8; MAX_EVENT];
        for event in events {
            if let Some(n) = event.encode(&mut buf) {
                text.extend_from_slice(&buf[..n]);
                text.push(b'\n');
            }
        }
        self.write(OUTBOX, &text)
    }

    /// Write `bytes` to `name` in the folder, made private first.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        aterm_uds::private_file::ensure_private_dir(&self.dir)?;
        aterm_uds::private_file::write_private(&self.dir, name, bytes)
    }

    /// Read `name` in the folder, at most `max` bytes.
    pub(crate) fn read_bytes(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        use aterm_uds::private_file::PrivateFileError;
        aterm_uds::private_file::read_private(
            &self.dir,
            name,
            usize::try_from(max).unwrap_or(usize::MAX),
        )
        .map_err(|error| match error {
            PrivateFileError::Missing => io::ErrorKind::NotFound.into(),
            PrivateFileError::Io(error) => error,
            other => io::Error::other(other.to_string()),
        })
    }

    fn read(&self, name: &str, max: u64) -> io::Result<Vec<Event>> {
        Ok(self
            .read_bytes(name, max)?
            .split(|b| *b == b'\n')
            .filter_map(Event::from_line)
            .collect())
    }
}

pub(super) fn open_private_append(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(path)?;
    let mut meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the journal file is not a regular file",
        ));
    }
    if meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the journal file is another user's",
        ));
    }
    if meta.nlink() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the journal file has another link",
        ));
    }
    if meta.permissions().mode() & 0o7777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        meta = file.metadata()?;
        if meta.permissions().mode() & 0o7777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the journal file could not be made 0600",
            ));
        }
    }
    Ok((file, meta))
}
