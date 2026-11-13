// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Owner-only private file and directory operations backed by `astream_sys` checks.

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::secret::Secret;

/// Error returned when reading or opening a private state file ([`read_private`]).
#[derive(Debug)]
pub enum PrivateFileError {
    /// File or state directory does not exist.
    Missing,
    /// File or directory is not owned exclusively by the current user or is accessible to others.
    NotPrivate(&'static str),
    /// File or directory [`read_owned`] or [`read_owned_into`] reads has a group or other write
    /// bit, which their looser rule still refuses.
    Writable(Writable),
    /// File or directory is a symbolic link.
    Symlink,
    /// Target is not a regular file, name spans multiple path components, or size exceeds the cap.
    Malformed(&'static str),
    /// Underlying I/O error.
    Io(io::Error),
}

/// A mark's or another call's I/O error ([`crate::kept_out`] over [`open_private`]).
impl From<io::Error> for PrivateFileError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for PrivateFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("there is no such file"),
            Self::NotPrivate(why) => write!(f, "not private: {why}"),
            Self::Writable(writable) => writable.fmt(f),
            Self::Symlink => f.write_str("it or its directory is a symlink"),
            Self::Malformed(why) => f.write_str(why),
            Self::Io(e) => write!(f, "it could not be opened or read: {e}"),
        }
    }
}

impl std::error::Error for PrivateFileError {}

/// The group or other write bit [`read_owned`]'s rule refused, on the file or on its directory.
/// Only the reads under that rule make one, and only for permission bits with a write bit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Writable {
    dir: bool,
    mode: u32,
}

impl Writable {
    /// Whether the bit is on the directory rather than on the file in it.
    #[must_use]
    pub const fn dir(self) -> bool {
        self.dir
    }

    /// The permission bits (`& 0o777`) judged on the open descriptor.
    #[must_use]
    pub const fn mode(self) -> u32 {
        self.mode
    }

    /// Which write bits are set: `"group-writable"`, `"world-writable"`, or
    /// `"group- and world-writable"`.
    #[must_use]
    pub const fn cause(self) -> &'static str {
        match (self.mode & 0o020 != 0, self.mode & 0o002 != 0) {
            (true, true) => "group- and world-writable",
            (false, true) => "world-writable",
            _ => "group-writable",
        }
    }
}

impl fmt::Display for Writable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = if self.dir { "the directory" } else { "it" };
        write!(
            f,
            "{what} is {} (mode {:04o}; want no group or other write bit)",
            self.cause(),
            self.mode
        )
    }
}

/// Why a file or directory with an access list for other users is not private.
pub const ACL_OPEN: &str = "an access list (ACL) grants other users access; `chmod -N` removes it";
/// Why a file others may read is not private.
pub const FILE_OPEN: &str = "it is open to other users (want 0600)";
/// Why a directory others may enter is not private.
pub const DIR_OPEN: &str = "the directory is open to other users (want 0700)";
/// Why a file with a second hard link is not private: that link is another copy of it.
pub const ANOTHER_LINK: &str = "it has another link";

/// How [`open_judged`] opens and judges a private file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// Read only, under the strict rules ([`read_private`]).
    Read,
    /// Read and write, created 0600 when missing, under the strict rules
    /// ([`open_private_rw`]).
    ReadWrite,
    /// Read only, with group and other read and search bits allowed, and no write bit
    /// ([`read_owned_into`]).
    Owned,
}

/// Whether permission bits `mode` (`& 0o777`) that astream-sys's owner-only rule refused
/// pass [`Opening::Owned`]'s looser one: `Some(loose)` when no group or other WRITE bit is
/// set, `loose` saying a group or other READ bit is (a search bit alone lets nobody read,
/// and is neither refused nor loose); `None` when one is, or the opening is a strict one.
/// astream-sys judges the mode last, so every other clause (kind, owner, access list)
/// held.
fn owned_mode(how: Opening, mode: u32) -> Option<bool> {
    (how == Opening::Owned && mode & 0o022 == 0).then_some(mode & 0o044 != 0)
}

/// `file` held to astream-sys's owner-only rule, loosened as `how` says: its metadata, and
/// whether a group or other read bit let it through (only [`Opening::Owned`] lets one).
fn file_verdict(
    file: &std::fs::File,
    how: Opening,
) -> Result<(std::fs::Metadata, bool), PrivateFileError> {
    use astream_sys::NotPrivate;
    match astream_sys::owner_private(file).map_err(PrivateFileError::Io)? {
        Ok(meta) => Ok((meta, false)),
        Err(NotPrivate::Kind(_)) => Err(PrivateFileError::Malformed("not a regular file")),
        Err(NotPrivate::Owner(_)) => Err(PrivateFileError::NotPrivate("it is another user's")),
        Err(NotPrivate::Acl) => Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        Err(NotPrivate::Mode(mode)) => match owned_mode(how, mode) {
            Some(loose) => Ok((file.metadata().map_err(PrivateFileError::Io)?, loose)),
            None if how == Opening::Owned => {
                Err(PrivateFileError::Writable(Writable { dir: false, mode }))
            }
            None => Err(PrivateFileError::NotPrivate(FILE_OPEN)),
        },
    }
}

/// Reads `name` inside private directory `dir` up to `max` bytes.
/// # Errors
/// As [`open_private`], and [`PrivateFileError::Malformed`] when the file holds more than
/// `max` bytes or [`PrivateFileError::Io`] when it cannot be read.
pub fn read_private(dir: &Path, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
    read_private_with_metadata(dir, name, max).map(|(bytes, _metadata)| bytes)
}

/// Read private bytes and the metadata of the same opened file, even if its name is atomically
/// replaced meanwhile. The read has [`read_private`]'s byte cap and privacy checks.
/// # Errors
/// As [`read_private`].
pub fn read_private_with_metadata(
    dir: &Path,
    name: &str,
    max: usize,
) -> Result<(Vec<u8>, std::fs::Metadata), PrivateFileError> {
    let (file, metadata) = open_private(dir, name)?;
    read_capped_with_len(&file, metadata.len(), max, name).map(|bytes| (bytes, metadata))
}

/// How old a file's last change must be, by its own times, before [`KeptParse`] pins a parse
/// to it: two in-place writes inside one tick of a filesystem's clock leave one stamp, and the
/// coarsest clock a private file's volume keeps ticks every two seconds (FAT's modification
/// time). A file younger than this is read again at every call.
pub const SETTLE: Duration = Duration::from_secs(2);

/// What a read of a file's open descriptor says of it without its bytes: which file it is, how
/// long, and when it was last written and last changed. A `write`, a truncation and a change
/// of owner, mode, access list or link count each move one of them on a volume whose times
/// this kernel sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }

    /// Whether the later of this stamp's two times is at least `settle` before `began`; a time
    /// before the epoch or after `began` is not.
    fn settled(&self, began: SystemTime, settle: Duration) -> bool {
        let (secs, nanos) = self.mtime.max(self.ctime);
        let (Ok(secs), Ok(nanos)) = (u64::try_from(secs), u32::try_from(nanos)) else {
            return false;
        };
        UNIX_EPOCH
            .checked_add(Duration::from_secs(secs))
            .and_then(|at| at.checked_add(Duration::from_nanos(u64::from(nanos))))
            .and_then(|changed| began.duration_since(changed).ok())
            .is_some_and(|age| age >= settle)
    }
}

/// The file a kept parse was read from, held open: while it is, no other file on its volume
/// has its inode number, so a file opened later with this `stamp`'s device and inode is this
/// very file, whatever a clock says.
struct Pin {
    _file: std::fs::File,
    stamp: Stamp,
    /// The wall clock just before the pinned file was opened.
    began: SystemTime,
}

impl Pin {
    /// Whether a file that opened with the stamp `opened`, the wall clock reading `now` just
    /// before, is the pinned file with nothing written to it since: the same file by the held
    /// inode, the same length and times, and a clock not behind the pin's own, where a write
    /// could have been stamped with the time the pin holds.
    fn stands(&self, opened: Stamp, now: SystemTime) -> bool {
        self.stamp == opened && now >= self.began
    }
}

/// One file's kept parse: the bytes it was made of, and the file itself where a later read can
/// tell from its stamp alone that nothing wrote to it.
struct Kept<T> {
    path: PathBuf,
    bytes: Secret,
    parse: T,
    pin: Option<Pin>,
}

/// Parsed private files. Every call opens and judges its file under [`read_private`]'s rules
/// and this call's byte cap, a parse kept or not, and hands a kept one back in two cases only.
///
/// - The file opened is the very file the parse was read from, with the length and times it
///   had then (a pin: that file is held open, so its inode number names no other). No byte is
///   read. A replacement by rename, which is every write aterm makes ([`write_private`]), is
///   another inode and never matches, with no clock involved. A write into the file moves its
///   times, and a parse is pinned only to a file whose times are [`SETTLE`] old, on a volume
///   whose times this kernel sets (not a network, user-space or virtual machine's one), and
///   stands only under a wall clock not behind the one it was made under: a later write
///   leaves other times unless that clock was set back past the pin's and forward again
///   between two reads. A store through a writable mapping of the file is the one write the
///   kernel does not stamp when it happens; no aterm writer makes one.
/// - Otherwise the bytes are read, and equal bytes at the same path reuse the parse.
///
/// At most `bound` files are kept, the first kept forgotten first, each with one open
/// descriptor at most; a file that cannot be opened or read, or is no longer private, is
/// forgotten. Input bytes are scrubbed on eviction and drop; parsed values must themselves be
/// suitable for retention.
pub struct KeptParse<T> {
    kept: Mutex<Vec<Kept<T>>>,
    bound: usize,
}

impl<T: Clone> KeptParse<T> {
    /// No parse kept yet, and none ever of more than `bound` files.
    #[must_use]
    pub const fn new(bound: usize) -> Self {
        Self {
            kept: Mutex::new(Vec::new()),
            bound,
        }
    }

    /// `parse` of `name` in `dir`, opened under [`read_private`]'s rules and held to the byte
    /// cap `max`: what `parse` said before of this very file where nothing wrote to it since
    /// (no byte read), or of the exact bytes just read at that path. Parsing runs outside the
    /// cache mutex.
    /// # Errors
    /// As [`read_private`]: the file is opened and judged at every call, a parse kept or not.
    pub fn read(
        &self,
        dir: &Path,
        name: &str,
        max: usize,
        parse: impl FnOnce(&[u8]) -> T,
    ) -> Result<T, PrivateFileError> {
        let began = SystemTime::now();
        let path = dir.join(name);
        let (file, meta) = open_private(dir, name).inspect_err(|_| self.forget(&path))?;
        if meta.len() > max as u64 {
            return Err(PrivateFileError::Malformed(
                "longer than such a file can be",
            ));
        }
        let stamp = Stamp::of(&meta);
        if let Some(kept) = self.lock().iter().find(|kept| kept.path == path)
            && kept
                .pin
                .as_ref()
                .is_some_and(|pin| pin.stands(stamp, began))
        {
            return Ok(kept.parse.clone());
        }
        let bytes = read_capped_with_len(&file, meta.len(), max, name)
            .map(Secret::from_vec)
            .inspect_err(|_| self.forget(&path))?;
        let known = self
            .lock()
            .iter()
            .find(|kept| kept.path == path && kept.bytes.expose() == bytes.expose())
            .map(|kept| kept.parse.clone());
        let parsed = known.unwrap_or_else(|| parse(bytes.expose()));
        let pin = pins(&file, &meta, stamp, began).then_some(Pin {
            _file: file,
            stamp,
            began,
        });
        let mut kept = self.lock();
        kept.retain(|kept| kept.path != path);
        kept.push(Kept {
            path,
            bytes,
            parse: parsed.clone(),
            pin,
        });
        let over = kept.len().saturating_sub(self.bound);
        kept.drain(..over);
        Ok(parsed)
    }

    /// Keep nothing of the file at `path`: its bytes are scrubbed and its descriptor closed.
    fn forget(&self, path: &Path) {
        self.lock().retain(|kept| kept.path != path);
    }

    /// How many kept parses hold their file open.
    #[cfg(test)]
    fn pinned(&self) -> usize {
        self.lock().iter().filter(|kept| kept.pin.is_some()).count()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Kept<T>>> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a parse of the open `file`, whose descriptor read `meta` (`stamp`) after the wall
/// clock read `began`, may be pinned to it ([`KeptParse`]): the file has its one name, its
/// times are [`SETTLE`] old ([`settle`] in a test), and this kernel's clock sets them.
fn pins(file: &std::fs::File, meta: &std::fs::Metadata, stamp: Stamp, began: SystemTime) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    #[cfg(any(test, debug_assertions))]
    let settle = settle::now();
    #[cfg(not(any(test, debug_assertions)))]
    let settle = Some(SETTLE);
    meta.nlink() == 1
        && settle.is_none_or(|settle| stamp.settled(began, settle))
        && times_are_this_kernels(file)
}

/// Whether this kernel sets the times of the files on the open `file`'s volume
/// ([`crate::file_watch::volume_times_are_this_kernels`]); in a test, not on a thread that
/// says its volume's times come from elsewhere ([`TIMES_FROM_ELSEWHERE`]).
fn times_are_this_kernels(file: &std::fs::File) -> bool {
    #[cfg(test)]
    if TIMES_FROM_ELSEWHERE.get() {
        return false;
    }
    crate::file_watch::volume_times_are_this_kernels(file)
}

#[cfg(test)]
thread_local! {
    /// Whether this thread's [`KeptParse`] reads take every volume as one whose times another
    /// clock sets (a network, user-space or virtual machine's one), which no test can mount.
    static TIMES_FROM_ELSEWHERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `file`'s bytes, refused when there are more than `max` ([`read_capped_into`] for a vector).
#[cfg(test)]
fn read_capped(file: std::fs::File, max: usize) -> Result<Vec<u8>, PrivateFileError> {
    let len = file.metadata().map_err(PrivateFileError::Io)?.len();
    read_capped_with_len(&file, len, max, "")
}

/// `file`'s bytes, at most `max`, read as the file `name`, which [`tally`] counts them under.
fn read_capped_with_len(
    file: &std::fs::File,
    file_len: u64,
    max: usize,
    name: &str,
) -> Result<Vec<u8>, PrivateFileError> {
    if file_len > max as u64 {
        return Err(PrivateFileError::Malformed(
            "longer than such a file can be",
        ));
    }
    let init = usize::try_from(file_len)
        .unwrap_or(max)
        .min(max)
        .min(64 * 1024)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(init);
    if let Err(e) = file
        .take((max as u64).saturating_add(1))
        .read_to_end(&mut bytes)
    {
        crate::secret::scrub(&mut bytes);
        return Err(PrivateFileError::Io(e));
    }
    #[cfg(any(test, debug_assertions))]
    tally::add(name, bytes.len());
    #[cfg(not(any(test, debug_assertions)))]
    let _ = name;
    if bytes.len() > max {
        crate::secret::scrub(&mut bytes);
        return Err(PrivateFileError::Malformed(
            "longer than such a file can be",
        ));
    }
    Ok(bytes)
}

const ONE_COMPONENT: &str = "a private file's name is one path component";

fn one_component(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0'))
}

fn one_component_io(name: &str) -> io::Result<()> {
    if one_component(name) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidInput, ONE_COMPONENT))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirIs {
    /// There is none.
    Missing,
    /// This user's, no access-list grant, no group or other mode bit.
    Private,
    /// This user's and no access-list grant, but these mode bits (`& 0o777`) open
    /// it to others: narrowing them makes it private.
    Open(u32),
    /// A symlink, which the rule never follows.
    Symlink,
    /// Something other than a directory.
    NotDir,
    /// Another user's.
    Others,
    /// An access-list entry allows someone something.
    Acl,
}

impl DirIs {
    /// astream-sys's owner-only verdict on a directory, in this module's words: the one
    /// mapping both of [`dir_is`]'s routes take.
    fn of(verdict: Result<(), astream_sys::NotPrivate>) -> Self {
        use astream_sys::NotPrivate;
        match verdict {
            Ok(()) => Self::Private,
            Err(NotPrivate::Mode(mode)) => Self::Open(mode),
            Err(NotPrivate::Kind(_)) => Self::NotDir,
            Err(NotPrivate::Owner(_)) => Self::Others,
            Err(NotPrivate::Acl) => Self::Acl,
        }
    }
}

/// `dir`'s verdict now, by astream-sys's owner-only rule: over the path's attributes
/// ([`astream_sys::owner_private_dir_by_path`], one `lstatx_np` on macOS arm64, which opens
/// nothing), and where those cannot decide ([`astream_sys::PathUndecided`]: every path off
/// macOS arm64, a path that holds a NUL or whose lookup or stat failed other than for a
/// missing name, an incomplete reading, an object that carries an access list, and this
/// user's directory without its owner's read or search bit), over the directory opened
/// ([`dir_is_opened`]). A symlink at its last component is
/// [`DirIs::Symlink`], never followed (a path that ends in a slash is followed by the lookup
/// and the open alike). Neither route binds what follows: each file is looked up again by
/// path ([`open_in`]).
fn dir_is(dir: &Path) -> io::Result<DirIs> {
    use astream_sys::DirByPath;
    #[cfg(any(test, debug_assertions))]
    tally::judged(dir);
    match astream_sys::owner_private_dir_by_path(dir) {
        DirByPath::Missing => return Ok(DirIs::Missing),
        DirByPath::Symlink => return Ok(DirIs::Symlink),
        DirByPath::Judged(verdict) => return Ok(DirIs::of(verdict)),
        DirByPath::Undecided(_) => {}
    }
    #[cfg(any(test, debug_assertions))]
    tally::opened(dir);
    dir_is_opened(dir)
}

/// [`dir_is`] over `dir` opened (`open_nonblocking`, refusing a link at its last component)
/// and judged on that descriptor (`owner_private_dir`). An open refused otherwise than for a
/// missing name is judged from `dir`'s `symlink_metadata`: a symlink is [`DirIs::Symlink`],
/// then the rule's kind and owner clauses (`kind_and_owner_of`) answer for the object, and
/// where both hold, or the metadata cannot be read, the open's error is the answer.
fn dir_is_opened(dir: &Path) -> io::Result<DirIs> {
    let opened = match astream_sys::open_nonblocking(dir, astream_sys::Links::Refuse) {
        Ok(opened) => opened,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DirIs::Missing),
        Err(e) => {
            return match std::fs::symlink_metadata(dir) {
                Ok(meta) if meta.file_type().is_symlink() => Ok(DirIs::Symlink),
                Ok(meta) => match astream_sys::kind_and_owner_of(&meta, true) {
                    Ok(()) => Err(e),
                    Err(broken) => Ok(DirIs::of(Err(broken))),
                },
                Err(_) => Err(e),
            };
        }
    };
    Ok(DirIs::of(
        astream_sys::owner_private_dir(&opened)?.map(drop),
    ))
}

/// A directory judged once under [`open_private`]'s directory rules, so that several of its
/// files open and read with one directory check between them instead of one each; each file
/// is still looked up, opened and judged on its open descriptor as [`open_private`] does.
/// aterm-journal's per-access `intact` check opens four. The verdict is the directory's when
/// [`JudgedDir::judge`] took it: use it for opens made at once, never across a wait.
pub struct JudgedDir<'a> {
    dir: &'a Path,
    judged: DirIs,
}

#[path = "private_file_dir.rs"]
mod directory;
pub use directory::{DirectoryView, PrivateDirectory};

impl<'a> JudgedDir<'a> {
    /// `dir`'s verdict, taken now. A missing, linked, foreign or open directory is no error
    /// here: each [`Self::open`] refuses on it as [`open_private`] would.
    /// # Errors
    /// [`PrivateFileError::Io`] when the directory could not be judged.
    pub fn judge(dir: &'a Path) -> Result<Self, PrivateFileError> {
        Ok(Self {
            dir,
            judged: dir_is(dir).map_err(PrivateFileError::Io)?,
        })
    }

    /// [`Self::judge`] of a directory that must be private: [`check_private_dir`], whose verdict
    /// this holds for the files that follow.
    /// # Errors
    /// As [`check_private_dir`].
    pub fn private(dir: &'a Path) -> io::Result<Self> {
        let judged = dir_is(dir)?;
        check_dir_verdict(dir, judged)?;
        Ok(Self { dir, judged })
    }

    /// Whether `dir` was an existing owner-private `0700` directory with no ACL grants when
    /// [`Self::judge`] took its verdict.
    #[must_use]
    pub fn is_private(&self) -> bool {
        matches!(self.judged, DirIs::Private)
    }

    /// Whether `dir` did not exist when [`Self::judge`] took its verdict.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self.judged, DirIs::Missing)
    }

    /// [`open_private`] of `name` under this verdict.
    /// # Errors
    /// As [`open_private`].
    pub fn open(&self, name: &str) -> Result<(std::fs::File, std::fs::Metadata), PrivateFileError> {
        if !one_component(name) {
            return Err(PrivateFileError::Malformed(ONE_COMPONENT));
        }
        open_in(self.dir, self.judged, name, Opening::Read).map(|(file, meta, _loose)| (file, meta))
    }

    /// [`read_private`] of `name` under this verdict.
    /// # Errors
    /// As [`read_private`].
    pub fn read(&self, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
        self.read_with_metadata(name, max)
            .map(|(bytes, _meta)| bytes)
    }

    /// [`read_private_with_metadata`] of `name` under this verdict.
    /// # Errors
    /// As [`read_private_with_metadata`].
    pub fn read_with_metadata(
        &self,
        name: &str,
        max: usize,
    ) -> Result<(Vec<u8>, std::fs::Metadata), PrivateFileError> {
        let (file, meta) = self.open(name)?;
        read_capped_with_len(&file, meta.len(), max, name).map(|bytes| (bytes, meta))
    }

    /// The first `len` bytes of `name` under this verdict, opened as [`Self::open`] opens it,
    /// or all of it when it holds fewer: a reader that needs only a record's head reads no more.
    /// # Errors
    /// As [`Self::open`], and [`PrivateFileError::Io`] when the file cannot be read.
    pub fn read_head(&self, name: &str, len: usize) -> Result<Vec<u8>, PrivateFileError> {
        let (mut file, _meta) = self.open(name)?;
        let mut head = vec![0; len];
        let read = read_full(&mut file, &mut head).map_err(PrivateFileError::Io)?;
        head.truncate(read);
        #[cfg(any(test, debug_assertions))]
        tally::add(name, read);
        Ok(head)
    }
}

/// Opens `name` in `dir` under [`read_private`] rules and returns the file and metadata.
/// # Errors
/// [`PrivateFileError::Missing`] when `dir` or `name` is missing; [`PrivateFileError::Symlink`]
/// when either is a symlink; [`PrivateFileError::NotPrivate`] when either is another user's,
/// has an access list or a group or other mode bit, or the file has a second link (one replaced
/// or removed since the open, which has none, opens as it stood then);
/// [`PrivateFileError::Malformed`] when `name` is more than one path component, `dir` is no
/// directory or the file no regular file; [`PrivateFileError::Io`] when a lookup, open or
/// judgement fails otherwise.
pub fn open_private(
    dir: &Path,
    name: &str,
) -> Result<(std::fs::File, std::fs::Metadata), PrivateFileError> {
    open_judged(dir, name, Opening::Read).map(|(file, meta, _loose)| (file, meta))
}

/// `name` directly in `dir`, under `dir`'s verdict from [`dir_is`] (taken from the
/// directory's attributes where they decide it, so the directory is opened only where they
/// cannot), the file opened as `how` says and judged on its open descriptor (astream-sys's
/// owner-only rule, never a second `stat` by path), with whether a group or other read bit
/// was set on the file or its directory (only [`Opening::Owned`] lets one through).
fn open_judged(
    dir: &Path,
    name: &str,
    how: Opening,
) -> Result<(std::fs::File, std::fs::Metadata, bool), PrivateFileError> {
    if !one_component(name) {
        return Err(PrivateFileError::Malformed(ONE_COMPONENT));
    }
    let judged = dir_is(dir).map_err(PrivateFileError::Io)?;
    open_in(dir, judged, name, how)
}

/// [`open_judged`] once `name` is known to be one component and `dir` has been judged:
/// the file looked up, opened and judged under `judged`, `dir`'s verdict.
fn open_in(
    dir: &Path,
    judged: DirIs,
    name: &str,
    how: Opening,
) -> Result<(std::fs::File, std::fs::Metadata, bool), PrivateFileError> {
    match judged {
        DirIs::Missing => return Err(PrivateFileError::Missing),
        DirIs::Symlink => return Err(PrivateFileError::Symlink),
        DirIs::NotDir => {
            return Err(PrivateFileError::Malformed("the directory is no directory"));
        }
        _ => {}
    }
    let path = dir.join(name);
    let dir_loose = match judged {
        DirIs::Private => false,
        DirIs::Open(mode) if let Some(loose) = owned_mode(how, mode) => loose,
        _ => {
            match std::fs::symlink_metadata(&path) {
                // A missing file is created by a read-write opening, judged like any other.
                Err(e) if e.kind() == io::ErrorKind::NotFound && how != Opening::ReadWrite => {
                    return Err(PrivateFileError::Missing);
                }
                Err(e)
                    if e.kind() != io::ErrorKind::PermissionDenied
                        && e.kind() != io::ErrorKind::NotFound =>
                {
                    return Err(PrivateFileError::Io(e));
                }
                _ => {}
            }
            return Err(match judged {
                DirIs::Others => PrivateFileError::NotPrivate("the directory is another user's"),
                DirIs::Acl => PrivateFileError::NotPrivate(ACL_OPEN),
                DirIs::Open(mode) if how == Opening::Owned => {
                    PrivateFileError::Writable(Writable { dir: true, mode })
                }
                _ => PrivateFileError::NotPrivate(DIR_OPEN),
            });
        }
    };
    let opened = match how {
        Opening::Read | Opening::Owned => {
            astream_sys::open_nonblocking(&path, astream_sys::Links::Refuse)
        }
        Opening::ReadWrite => open_rw_created_0600(&path, libc::O_NOFOLLOW | libc::O_CLOEXEC),
    };
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PrivateFileError::Symlink);
        }
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    let (file, meta, file_loose) = judge_opened(file, how)?;
    Ok((file, meta, dir_loose || file_loose))
}

/// `file`, just opened as `how` says, judged on its descriptor ([`file_verdict`]) and by its
/// links. A second link is another copy of it. None is a name replaced or removed since the
/// open (an atomic write's rename over it, as each fleet heartbeat's rewrite of the book makes):
/// a read takes the whole file as it stood at the open, while a read-write opening is
/// [`PrivateFileError::Missing`], as its writes would reach no name.
fn judge_opened(
    file: std::fs::File,
    how: Opening,
) -> Result<(std::fs::File, std::fs::Metadata, bool), PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    let (meta, file_loose) = file_verdict(&file, how)?;
    match meta.nlink() {
        1 => {}
        0 if how == Opening::ReadWrite => return Err(PrivateFileError::Missing),
        0 => {}
        _ => return Err(PrivateFileError::NotPrivate(ANOTHER_LINK)),
    }
    Ok((file, meta, file_loose))
}

/// `path` opened read and write with the extra open `flags`, created when missing and then
/// made exactly 0600. `create` with a mode is masked by the umask, so the creating open
/// (`O_EXCL`, so it is this open that made the file) says the mode again on the
/// DESCRIPTOR: under a umask that clears an owner bit the file would otherwise come out
/// 0400, and the next read-write open of it would be refused. An existing file is opened
/// as it stands, and its mode is never changed. `O_EXCL` never follows a link, so a
/// symlink at `path` (a dangling one included) falls to the plain open, where
/// `O_NOFOLLOW` in `flags` refuses it.
fn open_rw_created_0600(path: &Path, flags: libc::c_int) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut existing = std::fs::OpenOptions::new();
    existing.read(true).write(true).custom_flags(flags);
    let mut creating = existing.clone();
    creating.create_new(true).mode(0o600);
    match creating.open(path) {
        Ok(file) => {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            Ok(file)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => existing.open(path),
        Err(e) => Err(e),
    }
}

/// `<dir>/<name>` opened read and write, created 0600 when missing, under [`read_private`]'s
/// rules: `dir` the user's own 0700 directory, the file the user's own regular file with no
/// second link, no symlink anywhere and no group or other bit. The cursor is at 0. The write
/// journal's file (agh, design §6.8): scanned, truncated past a torn tail, appended, and made
/// durable through the one descriptor.
///
/// # Errors
///
/// As [`read_private`]; a missing file is created exactly 0600 whatever the umask, not
/// `Missing` (that only for one removed between the creating open and the plain one, or
/// replaced or removed after its open).
pub fn open_private_rw(dir: &Path, name: &str) -> Result<std::fs::File, PrivateFileError> {
    open_judged(dir, name, Opening::ReadWrite).map(|(file, _meta, _loose)| file)
}

/// Byte count and `(dev, ino)` identity read by [`read_private_into`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivateRead {
    /// Number of bytes read into the caller's buffer.
    pub len: usize,
    /// Device ID from `fstat`.
    pub dev: u64,
    /// Inode number from `fstat`.
    pub ino: u64,
}

/// Reads `name` in `dir` into `buf` (up to `max` bytes) and returns [`PrivateRead`].
/// # Errors
/// Returns [`PrivateFileError`] if privacy checks fail or the file exceeds `max`/`buf.len()`.
pub fn read_private_into(
    dir: &Path,
    name: &str,
    max: usize,
    buf: &mut [u8],
) -> Result<PrivateRead, PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    let (mut file, meta) = open_private(dir, name)?;
    let len = read_capped_into(&mut file, meta.len(), max, buf, name)?;
    Ok(PrivateRead {
        len,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Reads `name` in `dir` up to `max` bytes under [`read_owned_into`]'s rules.
/// # Errors
/// As [`read_owned_into`].
pub fn read_owned(dir: &Path, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
    let (file, meta, _loose) = open_judged(dir, name, Opening::Owned)?;
    read_capped_with_len(&file, meta.len(), max, name)
}

/// What [`read_owned_into`] read: the length, the file's identity, and whether the file or
/// its directory had a group or other read bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedRead {
    /// Bytes read into the caller's buffer.
    pub len: usize,
    /// The file's device.
    pub dev: u64,
    /// The file's inode.
    pub ino: u64,
    /// The file or its directory is group- or world-readable.
    pub loose: bool,
}

/// [`read_private_into`]'s rules, loosened for one legacy case only: the file and its
/// directory may carry group and other **read** bits (`loose` says so). Still refused:
/// - a symlink, file or directory;
/// - anything but a regular file;
/// - another user's file or directory;
/// - an access list that grants other users access;
/// - a second link;
/// - a group or other **write** bit on either.
///
/// Use this for owned metadata whose read bits may remain open, including legacy
/// profiles. Credentials still require [`read_private_into`]'s strict read mode.
///
/// # Errors
///
/// As [`read_private_into`], except that a group or other write bit on the file or its
/// directory is [`PrivateFileError::Writable`].
pub fn read_owned_into(
    dir: &Path,
    name: &str,
    max: usize,
    buf: &mut [u8],
) -> Result<OwnedRead, PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    let (mut file, meta, loose) = open_judged(dir, name, Opening::Owned)?;
    let len = read_capped_into(&mut file, meta.len(), max, buf, name)?;
    Ok(OwnedRead {
        len,
        dev: meta.dev(),
        ino: meta.ino(),
        loose,
    })
}

/// The read [`read_private_into`] and [`read_owned_into`] share: at most `min(max,
/// buf.len())` bytes into `buf`, a file past that cap `Malformed`, and on `Err` none of the
/// file left in `buf`.
fn read_capped_into(
    file: &mut std::fs::File,
    file_len: u64,
    max: usize,
    buf: &mut [u8],
    name: &str,
) -> Result<usize, PrivateFileError> {
    let cap = max.min(buf.len());
    if file_len > cap as u64 {
        crate::secret::scrub(&mut buf[..cap]);
        return Err(PrivateFileError::Malformed(
            "longer than such a file can be",
        ));
    }
    let len = match read_full(file, &mut buf[..cap]) {
        Ok(len) => len,
        Err(e) => {
            crate::secret::scrub(&mut buf[..cap]);
            return Err(PrivateFileError::Io(e));
        }
    };
    if len == cap {
        let mut more = [0u8; 1];
        let past = read_full(file, &mut more);
        crate::secret::scrub(&mut more);
        let refused = match past {
            Ok(0) => None,
            Ok(_) => Some(PrivateFileError::Malformed(
                "longer than such a file can be",
            )),
            Err(e) => Some(PrivateFileError::Io(e)),
        };
        if let Some(refused) = refused {
            crate::secret::scrub(&mut buf[..cap]);
            return Err(refused);
        }
    }
    #[cfg(any(test, debug_assertions))]
    tally::add(name, len);
    #[cfg(not(any(test, debug_assertions)))]
    let _ = name;
    Ok(len)
}

fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Current state of a directory under [`ensure_private_dir`] rules ([`judge_private_dir`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateDir {
    /// Directory does not exist (`ensure_private_dir` would create it with mode `0700`).
    Missing,
    /// Directory is already owner-private with mode `0700` and no ACL grants.
    Private,
    /// Directory is owned by this user with no ACL grants, but has group/other mode bits set.
    Open(u32),
}

fn dir_refused(dir: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("the directory {} {why}", dir.display()),
    )
}

fn dir_acl_refused(dir: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "the directory {}: {}",
            dir.display(),
            ACL_OPEN.replace("`chmod -N`", &format!("`chmod -N {}`", dir.display()))
        ),
    )
}

/// Inspects `dir` under [`ensure_private_dir`] rules without modifying it.
/// # Errors
/// Returns an error if `dir` is a symlink, not a directory, not owned, or has ACL grants.
pub fn judge_private_dir(dir: &Path) -> io::Result<PrivateDir> {
    match dir_is(dir)? {
        DirIs::Missing => Ok(PrivateDir::Missing),
        DirIs::Private => Ok(PrivateDir::Private),
        DirIs::Open(mode) => Ok(PrivateDir::Open(mode)),
        DirIs::Symlink => Err(dir_refused(
            dir,
            "is a symlink; it must be a directory of its own",
        )),
        DirIs::NotDir => Err(dir_refused(dir, "is no directory")),
        DirIs::Others => Err(dir_refused(dir, "is another user's")),
        DirIs::Acl => Err(dir_acl_refused(dir)),
    }
}

/// Ensures `dir` exists as a mode-`0700` private directory, returning prior mode bits if narrowed.
/// # Errors
/// Returns an error naming `dir` if it cannot be created or fails owner/symlink/ACL checks, or is not private once made or narrowed (a volume that ignores modes; `NotFound` where it is gone by then).
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    ensure_private_dir_by(dir, make_0700, narrow_0700)
}

fn make_0700(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

fn narrow_0700(dir: &Path) -> io::Result<()> {
    set_dir_mode_nofollow(dir, 0o700)
}

/// Change a real directory's mode through an open descriptor, recovering an
/// unreadable directory with kernel-enforced nofollow chmod on permission denial.
///
/// # Errors
/// Refuses non-directories and symlinks; returns open or chmod errors. There is
/// no path-based chmod fallback when the platform cannot honor nofollow.
pub fn set_dir_mode_nofollow(dir: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)
    {
        Ok(f) => {
            if f.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o7777 == mode)
            {
                return Ok(());
            }
            f.set_permissions(std::fs::Permissions::from_mode(mode))
        }
        Err(e)
            if e.kind() == io::ErrorKind::PermissionDenied
                && std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) =>
        {
            crate::atomic_file::set_mode_nofollow(dir, mode)
        }
        Err(e) => Err(e),
    }
}

/// [`ensure_private_dir`], with `make` making `dir` and `narrow` making it 0700.
fn ensure_private_dir_by(
    dir: &Path,
    make: impl FnOnce(&Path) -> io::Result<()>,
    narrow: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Option<u32>> {
    let named = |e: io::Error| match e.raw_os_error() {
        Some(_) => io::Error::new(e.kind(), format!("the directory {}: {e}", dir.display())),
        None => e,
    };
    let judge = || judge_private_dir(dir).map_err(named);
    let removed = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {} was removed as it was made 0700",
                dir.display()
            ),
        )
    };
    let was = match judge()? {
        PrivateDir::Private => return Ok(None),
        PrivateDir::Missing => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!(
                            "the directory {}: {} could not be made: {e}",
                            dir.display(),
                            parent.display()
                        ),
                    )
                })?;
            }
            match make(dir) {
                Ok(()) => None,
                // Another writer made it since the judge: judged again before a chmod, which follows a symlink.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match judge()? {
                    PrivateDir::Private => return Ok(None),
                    PrivateDir::Missing => return Err(removed()),
                    PrivateDir::Open(mode) => Some(mode),
                },
                Err(e) => return Err(named(e)),
            }
        }
        PrivateDir::Open(mode) => Some(mode),
    };
    narrow(dir).map_err(named)?;
    match judge()? {
        PrivateDir::Private => Ok(was),
        PrivateDir::Missing => Err(removed()),
        PrivateDir::Open(_) => Err(dir_refused(
            dir,
            "is not 0700 and this user's after it was made so",
        )),
    }
}

/// Verifies that `dir` already exists as an owner-private `0700` directory with no ACL grants.
/// # Errors
/// Returns an error if `dir` is missing, loose, symlinked, not owned, or has ACL grants.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    check_dir_verdict(dir, dir_is(dir)?)
}

fn check_dir_verdict(dir: &Path, verdict: DirIs) -> io::Result<()> {
    let refused = |why: &dyn fmt::Display| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the directory {}: {why}", dir.display()),
        )
    };
    match verdict {
        DirIs::Private => Ok(()),
        DirIs::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {}: there is no such directory",
                dir.display()
            ),
        )),
        DirIs::Symlink => Err(refused(&PrivateFileError::Symlink)),
        DirIs::NotDir => Err(refused(&"no directory")),
        DirIs::Others => Err(refused(&"it is another user's")),
        DirIs::Acl => Err(refused(&ACL_OPEN)),
        DirIs::Open(_) => Err(refused(&"it is open to other users (want 0700)")),
    }
}

/// Verifies that `dir` already exists as an owned real directory (never a symlink or another user's).
///
/// Unlike [`check_private_dir`], group/other mode bits and inherited ACLs are permitted so callers
/// that clean up or record state inside an owned directory do not reject loose permissions.
/// # Errors
/// Returns `NotFound` when `dir` is missing, `NotADirectory` when `dir` is not a directory, or
/// `PermissionDenied` when `dir` is a symlink or owned by another user.
pub fn check_owned_dir(dir: &Path) -> io::Result<()> {
    match dir_is(dir)? {
        DirIs::Private | DirIs::Open(_) | DirIs::Acl => Ok(()),
        DirIs::Missing => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the directory {}: there is no such directory",
                dir.display()
            ),
        )),
        DirIs::NotDir => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("the directory {} is no directory", dir.display()),
        )),
        DirIs::Symlink => Err(dir_refused(
            dir,
            "is a symlink; it must be a directory of its own",
        )),
        DirIs::Others => Err(dir_refused(dir, "is another user's")),
    }
}

/// Atomically writes `bytes` to `name` in private directory `dir` with mode `0600`.
/// # Errors
/// Returns an error if `name` spans multiple components, `dir` is not private, or write fails.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    one_component_io(name)?;
    check_private_dir(dir)?;
    crate::atomic_file::write(&dir.join(name), bytes, 0o600)
}

/// [`write_private`] for a writer whose promise rests on `name`'s new entry: `flush` finishes the
/// file and, after the rename, `dir`, and a directory whose entries cannot be flushed here is
/// refused like any failed flush ([`crate::atomic_file::write_entry_flushed`]), where
/// [`write_private`] goes on without that flush.
/// # Errors
/// Returns an error if `name` spans multiple components, `dir` is not private, or the write
/// fails; or `dir`'s flush after the rename failed or is unavailable here, when `name` holds
/// `bytes` but its entry may not survive a power loss.
pub fn write_private_flushed(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    flush: aterm_durable::Flush,
) -> io::Result<()> {
    one_component_io(name)?;
    check_private_dir(dir)
        .map_err(|error| crate::atomic_file::write_error("check private directory", dir, error))?;
    crate::atomic_file::write_entry_flushed(&dir.join(name), bytes, 0o600, flush)
}

/// Writes `bytes` to a NEW `name` in private directory `dir` with mode `0600`, and never over one
/// that is there: for a secret whose replacement would strand what it protects. `prepare` runs on
/// the file before it gets its name, so whatever it marks is on the file from the name's first
/// moment. The name comes from a hard link, because a link fails on a name that exists where a
/// rename would replace it, so from the link until the temporary is unlinked `name` has two links.
///
/// A crash leaves no `name` and maybe the temporary, holding the bytes, or `name` whole and flushed
/// with that temporary still its second link. A reader that refuses a second link (astream's
/// owner-only secret rule) refuses `name` until the temporary is gone, so a caller of such a reader
/// runs [`crate::atomic_file::sweep_temps_beside`] on `name` before it reads. A reader that meets a
/// live writer inside that moment refuses too, and the next read does not.
///
/// Answers whether it wrote: `false` when `name` already exists, which is left as it was.
/// # Errors
/// Returns an error if `name` spans multiple components, `dir` is not private, or `prepare`, a
/// write or a flush fails; `name` then does not exist unless it did before, or the flush of the
/// directory failed after the name was made.
pub fn write_private_new(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    prepare: impl FnOnce(&std::fs::File) -> io::Result<()>,
) -> io::Result<bool> {
    use std::io::Write as _;
    one_component_io(name)?;
    check_private_dir(dir)?;
    let path = dir.join(name);
    let (temp, mut file) = crate::atomic_file::create_temp_beside(&path, 0o600)?;
    let linked = (|| {
        prepare(&file)?;
        file.write_all(bytes)?;
        aterm_durable::Flush::Durable.file(&file)?;
        // A hard link fails on a name that exists, where a rename would replace it.
        std::fs::hard_link(&temp, &path)
    })();
    let removed = std::fs::remove_file(&temp);
    match linked {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        other => other?,
    }
    removed?;
    aterm_durable::Flush::Durable.dir(dir)?;
    Ok(true)
}

/// Removes single-component file `name` from `dir`, returning `Ok(true)` if it existed (none does where `dir` is no directory).
/// # Errors
/// Returns an error if `name` spans multiple components, `dir` is a symlink or owned by another user, or unlinking fails.
pub fn remove_private(dir: &Path, name: &str) -> io::Result<bool> {
    one_component_io(name)?;
    match check_owned_dir(dir) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(e),
    }
    match std::fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(true),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Creates `dir` with mode `0700` or verifies that an existing `dir` passes [`check_private_dir`].
/// # Errors
/// Returns an error if creation or [`check_private_dir`] fails.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    match dir_is(dir)? {
        DirIs::Private => return Ok(()),
        DirIs::Missing => match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {
                narrow_0700(dir)?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        },
        other => return check_dir_verdict(dir, other),
    }
    check_private_dir(dir)
}

/// An exclusive lock on `<dir>/<name>`, lifted from aterm-net's `fleet_member` (`BookLock`).
///
/// - **How it is opened.** `name` must be one path component, and `dir` must pass
///   [`check_private_dir`]. The file is opened read-write, created exactly 0600 whatever the
///   umask, with `O_NOFOLLOW | O_NONBLOCK`, so it is never a link and the open never waits
///   on a FIFO. Anything but a regular file is refused.
/// - **How it is taken.** An exclusive `flock`, tried again with a backoff that starts at
///   1 ms and doubles to 50 ms until `within` has passed.
/// - **How it is released.** `LOCK_UN` in `Drop`, before the close. That is `clippy.toml`'s
///   flock release rule: a lock let go by dropping its `File` stays held in every child
///   forked meanwhile.
pub struct PrivateLock(std::fs::File);

/// Duplicates share this lock description, including after this process dies.
/// Ordinary owner drop explicitly unlocks them all; keep this owner until its receivers finish.
impl std::os::fd::AsFd for PrivateLock {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.0)
    }
}

impl PrivateLock {
    /// Take the lock on `<dir>/<name>`, waiting at most `within`.
    ///
    /// # Errors
    ///
    /// - `ErrorKind::InvalidInput` when `name` is not one path component (empty, `.`, `..`
    ///   or a `/` in it), before `dir` is judged or anything is opened (only `dir` is judged,
    ///   so a deeper name would escape the rules);
    /// - `ErrorKind::WouldBlock` with the text `busy` when another holder kept it for `within`
    ///   (a `within` past what a clock can represent waits with no deadline);
    /// - `ErrorKind::InvalidData`, `<dir>/<name> is no regular file`;
    /// - [`check_private_dir`]'s refusal, which names `dir`;
    /// - the open's own error.
    pub fn take(
        dir: &Path,
        name: &str,
        within: Duration,
        busy: &'static str,
    ) -> io::Result<PrivateLock> {
        Self::take_with(dir, name, within, busy, || {})
    }

    /// [`take`](Self::take), with a `busy` text made at run time (one that names `dir`, say)
    /// and `held` called each time the lock is found held, before the backoff's sleep (a
    /// test's hook to see a writer wait).
    ///
    /// # Errors
    ///
    /// As [`take`](Self::take).
    pub fn take_with(
        dir: &Path,
        name: &str,
        within: Duration,
        busy: &str,
        held: impl FnMut(),
    ) -> io::Result<PrivateLock> {
        let file = Self::open(dir, name)?;
        // `None`: a wait no clock can represent, which has no deadline.
        let deadline = Instant::now().checked_add(within);
        crate::file_lock::lock_exclusive_with_deadline(&file, deadline, busy, held)?;
        Ok(Self(file))
    }

    /// THE LOCK ON `<dir>/<name>` WHENEVER ITS HOLDER LETS IT GO, opened as [`take`](Self::take)
    /// opens it: one try, then, the lock found held, `held` once and a blocking `flock` that
    /// sleeps in the kernel until the holder lets go or its process ends. No clock and no
    /// backoff wakes the thread meanwhile, so a process that waits to be the one keeper of
    /// something spends nothing while another keeps it, and takes over at once.
    ///
    /// # Errors
    ///
    /// As [`take`](Self::take), but never `WouldBlock`; and the `flock`'s own error.
    pub fn wait_for(dir: &Path, name: &str, held: impl FnOnce()) -> io::Result<PrivateLock> {
        let file = Self::open(dir, name)?;
        // One try (a past deadline still makes its first attempt), then the kernel's wait.
        match crate::file_lock::lock_exclusive_until(&file, Instant::now(), "held") {
            Ok(()) => return Ok(Self(file)),
            Err(error) if error.kind() != io::ErrorKind::WouldBlock => return Err(error),
            Err(_) => held(),
        }
        crate::file_lock::lock_exclusive_blocking(&file)?;
        Ok(Self(file))
    }

    /// The lock file `<dir>/<name>`, admitted as [`take`](Self::take) says, not yet locked.
    fn open(dir: &Path, name: &str) -> io::Result<std::fs::File> {
        one_component_io(name)?;
        check_private_dir(dir)?;
        let path = dir.join(name);
        let file = crate::kept_out(|| {
            open_rw_created_0600(&path, libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        })?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is no regular file", path.display()),
            ));
        }
        Ok(file)
    }
}

impl Drop for PrivateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// What this thread read through this module's reads, by file name, while [`tally::counted`]
/// runs, and which directories it judged, and opened to judge, while [`tally::directories`]
/// runs: the instruments a test uses to hold a path to the bytes it reads and the
/// directories `dir_is` opens to judge.
#[cfg(any(test, debug_assertions))]
pub mod tally {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// What [`counted`] saw of one file name: how many reads, and their bytes in all.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Read {
        /// The file's name in its directory.
        pub name: String,
        /// How many reads of it.
        pub reads: usize,
        /// The bytes those reads returned.
        pub bytes: usize,
    }

    thread_local! {
        static READ: RefCell<Option<Vec<Read>>> = const { RefCell::new(None) };
    }

    /// `run`'s result, and each file name this thread read through this module while it ran,
    /// in the order first read. A count running around this one counts none of what this one
    /// does.
    pub fn counted<R>(run: impl FnOnce() -> R) -> (R, Vec<Read>) {
        let outer = READ.with(|read| read.replace(Some(Vec::new())));
        let ran = run();
        let read = READ.with(|read| read.replace(outer)).unwrap_or_default();
        (ran, read)
    }

    pub(super) fn add(name: &str, bytes: usize) {
        READ.with(|read| {
            if let Some(read) = read.borrow_mut().as_mut() {
                let at = match read.iter().position(|seen| seen.name == name) {
                    Some(at) => at,
                    None => {
                        read.push(Read {
                            name: name.to_owned(),
                            ..Read::default()
                        });
                        read.len() - 1
                    }
                };
                read[at].reads += 1;
                read[at].bytes += bytes;
            }
        });
    }

    /// What [`directories`] saw this thread judge: each directory this module judged
    /// (`dir_is`), and each of those it opened to judge, where the path's attributes could
    /// not decide, with how many times, in the order first seen.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Directories {
        /// Each directory judged, and how many times.
        pub judged: Vec<(PathBuf, usize)>,
        /// Each directory opened to be judged, and how many times.
        pub opened: Vec<(PathBuf, usize)>,
    }

    thread_local! {
        static DIRECTORIES: RefCell<Option<Directories>> = const { RefCell::new(None) };
    }

    /// `run`'s result, and each directory this thread judged by path (`dir_is`) while it
    /// ran. A count running around this one counts none of what this one does; with no count
    /// running, a judgement records nothing.
    pub fn directories<R>(run: impl FnOnce() -> R) -> (R, Directories) {
        let outer = DIRECTORIES.with(|seen| seen.replace(Some(Directories::default())));
        let ran = run();
        let seen = DIRECTORIES
            .with(|seen| seen.replace(outer))
            .unwrap_or_default();
        (ran, seen)
    }

    pub(super) fn judged(dir: &Path) {
        note(dir, |seen| &mut seen.judged);
    }

    pub(super) fn opened(dir: &Path) {
        note(dir, |seen| &mut seen.opened);
    }

    fn note(dir: &Path, list: impl FnOnce(&mut Directories) -> &mut Vec<(PathBuf, usize)>) {
        DIRECTORIES.with(|seen| {
            if let Some(seen) = seen.borrow_mut().as_mut() {
                let list = list(seen);
                match list.iter_mut().find(|(at, _)| at == dir) {
                    Some((_, times)) => *times += 1,
                    None => list.push((dir.to_path_buf(), 1)),
                }
            }
        });
    }
}

/// How old a file's last change must be before this thread's [`KeptParse`] reads pin a parse
/// to it: [`SETTLE`], unless a test says otherwise for what it runs. A test that reads a file
/// it has just written cannot wait out [`SETTLE`] and must not depend on how two clocks compare,
/// so it says the age it wants.
#[cfg(any(test, debug_assertions))]
pub mod settle {
    use std::cell::Cell;
    use std::time::Duration;

    thread_local! {
        static AGE: Cell<Option<Duration>> = const { Cell::new(Some(super::SETTLE)) };
    }

    /// The age this thread asks of a file's times now, or `None` for any.
    pub(super) fn now() -> Option<Duration> {
        AGE.with(Cell::get)
    }

    /// Puts the age back as a run ends, a panic's unwinding included.
    struct Back(Option<Duration>);

    impl Drop for Back {
        fn drop(&mut self) {
            AGE.with(|settle| settle.set(self.0));
        }
    }

    fn with<R>(age: Option<Duration>, run: impl FnOnce() -> R) -> R {
        let _back = Back(AGE.with(|settle| settle.replace(age)));
        run()
    }

    /// `run`'s result, with every file this thread reads through a [`super::KeptParse`]
    /// meanwhile counted old enough to pin, whatever its times. Every other condition of a pin
    /// stands. A file written into in place afterwards, at the same length, may then keep the
    /// stamp it was pinned under: a test under this replaces its files by rename
    /// ([`super::write_private`]) or changes their length.
    pub fn none<R>(run: impl FnOnce() -> R) -> R {
        with(None, run)
    }

    /// `run`'s result, with the age asked of a file's times `age` meanwhile on this thread.
    #[cfg(test)]
    pub(super) fn after<R>(age: Duration, run: impl FnOnce() -> R) -> R {
        with(Some(age), run)
    }
}

#[cfg(test)]
#[path = "private_file_tests.rs"]
mod tests;
