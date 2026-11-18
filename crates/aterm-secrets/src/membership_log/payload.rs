// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable ciphertext bytes, owned directly by a draft or shared with a signed record, and the
//! signed bytes a record shares them from ([`Wire`]): in memory, or, for a record an open read
//! from the vault's head ([`super::head`]), left in the records file until something asks for
//! them ([`Shelved`]).

use crate::{Error, Result};
use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;

/// A record's signed bytes: in memory, or shelved in the file they were read from.
#[derive(Clone)]
pub(crate) enum Wire {
    /// Every byte, in memory.
    Whole(Arc<[u8]>),
    /// The record's frame in memory and the rest in its file, read back on first use.
    #[cfg(any(unix, windows))]
    Shelved(Arc<Shelved>),
}

impl Wire {
    /// Every signed byte: a shelved record's read back from its file on first use and kept
    /// ([`Shelved::bytes`]): what an open of its payload decrypts. Empty where that read fails:
    /// bytes no record verifies by, so a local open of the payload fails its authentication, and
    /// the next ask reads again. What sends or writes a record's bytes asks [`Wire::read`]
    /// instead, which keeps nothing and fails on such a read.
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Whole(bytes) => bytes,
            #[cfg(any(unix, windows))]
            Self::Shelved(shelved) => shelved.bytes().unwrap_or_default(),
        }
    }

    /// The bytes it keeps in memory from its start: every byte, or a shelved record's frame,
    /// read back or not (a version-2 record's runs through its signature).
    pub(crate) fn frame(&self) -> &[u8] {
        match self {
            Self::Whole(bytes) => bytes,
            #[cfg(any(unix, windows))]
            Self::Shelved(shelved) => &shelved.frame,
        }
    }

    /// The signed length, read back or not.
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Whole(bytes) => bytes.len(),
            #[cfg(any(unix, windows))]
            Self::Shelved(shelved) => shelved.len,
        }
    }

    /// The bytes it keeps in memory: all of them, or a shelved record's frame and whatever was
    /// read back.
    pub(crate) fn resident(&self) -> usize {
        match self {
            Self::Whole(bytes) => bytes.len(),
            #[cfg(any(unix, windows))]
            Self::Shelved(shelved) => shelved
                .frame
                .len()
                .saturating_add(shelved.read.get().map_or(0, |read| read.len())),
        }
    }

    /// Whether its bytes past the frame are still in the file, never read back.
    #[cfg(any(unix, windows))]
    pub(crate) fn is_shelved(&self) -> bool {
        matches!(self, Self::Shelved(shelved) if shelved.read.get().is_none())
    }

    /// Every signed byte, keeping none it reads: borrowed where they are in memory, and a
    /// shelved record's not read back yet read from its file for this use alone, so sending or
    /// writing every record of a log read through the head does not bring every replaced body
    /// into memory. One that cannot be read back exactly is an error, never a short stream.
    ///
    /// # Errors
    /// A shelved record's bytes cannot be read back as they were.
    pub(crate) fn read(&self) -> std::io::Result<std::borrow::Cow<'_, [u8]>> {
        match self {
            Self::Whole(bytes) => Ok(std::borrow::Cow::Borrowed(bytes)),
            #[cfg(any(unix, windows))]
            Self::Shelved(shelved) => match shelved.read.get() {
                Some(bytes) => Ok(std::borrow::Cow::Borrowed(bytes)),
                None => shelved
                    .read_back()
                    .map(std::borrow::Cow::Owned)
                    .ok_or_else(unread),
            },
        }
    }

    /// Write every signed byte to `writer` ([`Wire::read`]).
    ///
    /// # Errors
    /// The writer fails, or a shelved record's bytes cannot be read back as they were.
    pub(crate) fn write_to<W: std::io::Write + ?Sized>(
        &self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(&self.read()?)
    }

    /// Whether it is shelved: its frame alone in memory, read back or not.
    #[cfg(any(unix, windows))]
    pub(crate) const fn on_shelf(&self) -> bool {
        matches!(self, Self::Shelved(_))
    }

    /// The records file a shelved record's bytes are read back from.
    #[cfg(any(unix, windows))]
    pub(crate) fn source(&self) -> Option<&Arc<Source>> {
        match self {
            Self::Whole(_) => None,
            Self::Shelved(shelved) => Some(&shelved.source),
        }
    }
}

/// A shelved record whose bytes did not read back as they were.
#[cfg(any(unix, windows))]
fn unread() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "a shelved membership record could not be read back",
    )
}

/// The records file a shelved record's bytes are read back from: the file the head that shelved
/// it was checked against, held open, so a writer that renames a new file over the path leaves
/// these bytes where they were. Every writer of a records file appends to it or replaces it
/// whole, and none rewrites its bytes in place.
#[cfg(any(unix, windows))]
pub(crate) struct Source {
    file: std::fs::File,
    /// The file's length when the head was checked against it: what holding it open keeps on
    /// disk once a writer has replaced it ([`Source::len`]).
    len: u64,
    /// How many bytes were read back from it ([`Source::read_bytes`]).
    #[cfg(test)]
    read: std::sync::atomic::AtomicUsize,
}

#[cfg(any(unix, windows))]
impl Source {
    pub(crate) fn new(file: std::fs::File, len: u64) -> Self {
        Self {
            file,
            len,
            #[cfg(test)]
            read: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The file's length, which a store holding it open keeps on disk.
    pub(crate) const fn len(&self) -> u64 {
        self.len
    }

    /// How many bytes were read back from the file so far: what a test of an open that reads no
    /// replaced body counts.
    #[cfg(test)]
    pub(crate) fn read_bytes(&self) -> usize {
        self.read.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn read_at(&self, at: u64, len: usize) -> Option<Vec<u8>> {
        #[cfg(windows)]
        use crate::fsx::FileAt as _;
        #[cfg(unix)]
        use std::os::unix::fs::FileExt as _;
        let mut bytes = vec![0; len];
        self.file.read_exact_at(&mut bytes, at).ok()?;
        #[cfg(test)]
        self.read
            .fetch_add(len, std::sync::atomic::Ordering::Relaxed);
        Some(bytes)
    }
}

/// A record whose frame is in memory and whose payload and signature stay in its records file.
/// What comes back is checked: it begins with the frame and gives the record's id (which the
/// record had when it was verified in full, before the head that shelved it was written): its
/// SHA-256 for version 1, and for version 2 the SHA-256 of its shed form, with the ciphertext the
/// one its signed fields name ([`super::whole_id`]).
#[cfg(any(unix, windows))]
pub(crate) struct Shelved {
    pub(crate) id: [u8; 32],
    pub(crate) len: usize,
    /// The record's bytes up to its payload.
    pub(crate) frame: Box<[u8]>,
    /// Where it starts in [`Shelved::source`].
    pub(crate) at: u64,
    pub(crate) source: Arc<Source>,
    /// What was read back and checked. A read that failed or did not check is not kept: one
    /// transient error (an `EIO`, an interrupted read) leaves the next ask to read again.
    read: std::sync::OnceLock<Arc<[u8]>>,
}

#[cfg(any(unix, windows))]
impl Shelved {
    pub(crate) fn new(
        id: [u8; 32],
        len: usize,
        frame: Box<[u8]>,
        at: u64,
        source: Arc<Source>,
    ) -> Self {
        Self {
            id,
            len,
            frame,
            at,
            source,
            read: std::sync::OnceLock::new(),
        }
    }

    /// Every signed byte, read back once and kept; `None` where they cannot be read as they were
    /// now, which is not kept.
    fn bytes(&self) -> Option<&[u8]> {
        if let Some(bytes) = self.read.get() {
            return Some(bytes);
        }
        let bytes: Arc<[u8]> = self.read_back()?.into();
        Some(self.read.get_or_init(|| bytes))
    }

    fn read_back(&self) -> Option<Vec<u8>> {
        let bytes = self.source.read_at(self.at, self.len)?;
        let id = if self.frame.get(super::MAGIC.len()) == Some(&super::VERSION_TWO) {
            super::whole_id(&bytes)
        } else {
            Some(super::digest(&bytes))
        };
        (bytes.starts_with(&self.frame) && id == Some(self.id)).then_some(bytes)
    }
}

/// Immutable ciphertext. A decoded payload shares its signed record's backing bytes; cloning
/// it keeps those bytes alive independently of the record or the caller's input buffer. A
/// shelved record's payload is read back with its record the first time its bytes are asked for
/// ([`Wire::bytes`]); its length is known without them ([`Payload::len`]).
#[derive(Clone)]
pub struct Payload {
    bytes: Wire,
    range: Range<usize>,
}

impl Payload {
    pub(super) fn in_wire(bytes: &Wire, range: Range<usize>) -> Result<Self> {
        if range.start > range.end || range.end > bytes.len() {
            return Err(Error::new(
                "membership payload is outside its signed record",
            ));
        }
        Ok(Self {
            bytes: bytes.clone(),
            range,
        })
    }

    /// A shed version-2 record's ciphertext ([`super::Record::is_shed`]): `len` bytes its record
    /// signs the SHA-256 of, of which it holds none, so it reads as no bytes and a local open of
    /// it fails its authentication, as a shelved payload that does not read back does.
    pub(super) fn shed(len: usize) -> Self {
        Self {
            bytes: Wire::Whole(Arc::from(Vec::new())),
            range: 0..len,
        }
    }

    /// Whether it is a shed record's ciphertext ([`Payload::shed`]): its length, and no bytes.
    #[must_use]
    pub fn is_shed(&self) -> bool {
        self.range.end > self.bytes.len()
    }

    /// Its length, without reading a shelved record back.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.range.end - self.range.start
    }

    /// Whether it is empty, without reading a shelved record back.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<Vec<u8>> for Payload {
    fn from(bytes: Vec<u8>) -> Self {
        let range = 0..bytes.len();
        Self {
            bytes: Wire::Whole(Arc::from(bytes)),
            range,
        }
    }
}

impl Deref for Payload {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes
            .bytes()
            .get(self.range.clone())
            .unwrap_or_default()
    }
}

impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_shed() {
            return write!(f, "<{} bytes shed>", self.len());
        }
        #[cfg(any(unix, windows))]
        if self.bytes.is_shelved() {
            return write!(f, "<{} bytes shelved>", self.len());
        }
        fmt::Debug::fmt(self.as_ref(), f)
    }
}

impl fmt::Debug for Wire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Whole(bytes) => fmt::Debug::fmt(bytes, f),
            #[cfg(any(unix, windows))]
            Self::Shelved(shelved) => write!(f, "<{} bytes shelved>", shelved.len),
        }
    }
}
