// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Release asset downloads with SHA-256 verification and atomic `.agh-part` staging.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::client::{Accept, Call, Github, Streamed};
use crate::error::{Error, Kind, Result, Sub};
use crate::github::{How, repo_root};
use crate::model::{AssetObject, AssetState, Sha256Digest};
use crate::policy::RouteTarget;
use crate::slug::Slug;
use crate::wire::{Deadlines, Origin, OriginRole, WireKind, wire_kind_of};

/// Where the bytes go.
pub enum Sink<'a> {
    /// A file; `clobber` replaces an existing file with different bytes.
    File {
        /// The destination.
        path: std::path::PathBuf,
        /// Whether different bytes there are replaced.
        clobber: bool,
    },
    /// Memory, at most this many bytes.
    Bounded(u64),
    /// A writer (`--output -`).
    Writer(&'a mut dyn std::io::Write),
}

/// What the bytes must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expect {
    /// The exact size.
    pub size: u64,
    /// The digest, when GitHub knows it.
    pub digest: Option<Sha256Digest>,
    /// The most bytes read.
    pub limit: u64,
}

impl Expect {
    /// The asset's size (also the limit) and its digest.
    ///
    /// # Errors
    ///
    /// `AssetObject::sha256`'s error.
    pub fn from_asset(asset: &AssetObject) -> Result<Expect> {
        Ok(Expect {
            size: asset.size,
            digest: asset.sha256()?,
            limit: asset.size,
        })
    }
}

/// How the digest was checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestCheck {
    /// The bytes matched the asset's digest.
    Matched,
    /// GitHub knows no digest; the bracket recheck held.
    Absent,
}

/// One finished download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Downloaded {
    /// The asset's id.
    pub asset_id: u64,
    /// The bytes.
    pub bytes: u64,
    /// Their digest.
    pub sha256: Sha256Digest,
    /// `Sink::File`'s path.
    pub path: Option<std::path::PathBuf>,
    /// `Sink::Bounded`'s bytes.
    pub data: Option<Vec<u8>>,
    /// False when the destination already held these bytes.
    pub written: bool,
    /// How the digest was checked.
    pub digest: DigestCheck,
    /// The requests sent.
    pub requests: u32,
}

/// The suffix of a part file's name.
pub(crate) const PART_SUFFIX: &str = ".agh-part";

/// `<dest>.agh-part`.
#[inline]
#[must_use]
pub fn part_path(dest: &std::path::Path) -> std::path::PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(PART_SUFFIX);
    std::path::PathBuf::from(name)
}

/// Returns whether `path`'s file name ends in [`PART_SUFFIX`] (case-insensitive).
#[inline]
#[must_use]
pub fn is_part_name(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.as_encoded_bytes();
        let suffix = PART_SUFFIX.as_bytes();
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    })
}

/// Returns whether `name` is a single plain file name component.
#[inline]
#[must_use]
pub fn plain_file_name(name: &str) -> bool {
    if cfg!(windows) && name.contains(':') {
        return false;
    }
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(part)), None) if part == std::ffi::OsStr::new(name)
    )
}

/// Rejects a destination that exists and is not a regular file (never following a symlink).
///
/// # Errors
///
/// Returns a usage error for a non-regular file or symlink, or a local error if metadata cannot be read.
pub(crate) fn refuse_non_file(dest: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Ok(_) => Err(not_a_file(dest)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(local("cannot read", dest, &e)),
    }
}

/// Usage: `dest` exists and is not a regular file.
fn not_a_file(dest: &Path) -> Error {
    Error::usage(format!(
        "{} exists and is not a regular file: a download writes a file",
        dest.display()
    ))
}

/// Reject a destination whose name ends in [`PART_SUFFIX`].
pub(crate) fn refuse_part_name(dest: &Path) -> Result<()> {
    if is_part_name(dest) {
        return Err(Error::usage(format!(
            "{} ends in {PART_SUFFIX}, the suffix of agh's own part files",
            dest.display()
        )));
    }
    Ok(())
}

/// A local failure: CouldNotRun `Sub::Local`.
fn local(what: &str, path: &Path, e: &std::io::Error) -> Error {
    Error::local(format!("{what} {}: {e}", path.display()))
}

/// A read of the body that failed: CouldNotRun `Wire(kind)`, why `{kind}: {detail}`.
fn wire_read(e: &std::io::Error) -> Error {
    let kind = wire_kind_of(e);
    Error::could_not_run(format!("{}: {e}", kind.slug())).with_sub(Sub::Wire(kind))
}
fn parent_of(dest: &Path) -> &Path {
    dest.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Remove `path`, a NotFound ignored.
fn remove_quietly(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whether `path` still names the same inode as the open `file`.
#[cfg(unix)]
fn still_named(path: &Path, file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(path), file.metadata()) {
        (Ok(named), Ok(open)) => named.dev() == open.dev() && named.ino() == open.ino(),
        _ => false,
    }
}

/// Off Unix, `Part::create` opens without `FILE_SHARE_DELETE`, preventing replacement while open.
#[cfg(not(unix))]
fn still_named(_path: &Path, _file: &File) -> bool {
    true
}

/// Windows' `FILE_SHARE_READ`: the one access another opener of the part file is granted.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

/// Staging part file removed on drop unless renamed into place or replaced.
struct Part {
    path: PathBuf,
    /// The open part file; `None` once `finish` or `drop` closed it, before the rename or the
    /// removal by name (Windows does neither to a file open without `FILE_SHARE_DELETE`).
    file: Option<File>,
    kept: bool,
}

impl Part {
    /// Remove any leftover part file for `dest` and create a fresh one.
    fn create(dest: &Path) -> Result<Part> {
        refuse_part_name(dest)?;
        let path = part_path(dest);
        remove_quietly(&path).map_err(|e| local("cannot remove the old part file", &path, &e))?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(FILE_SHARE_READ);
        }
        let file = options
            .open(&path)
            .map_err(|e| local("cannot create", &path, &e))?;
        Ok(Part {
            path,
            file: Some(file),
            kept: false,
        })
    }

    fn closed(path: &Path) -> Error {
        Error::local(format!("{}: the part file is closed", path.display()))
    }

    #[cfg(all(test, unix))]
    fn file(&mut self) -> Result<&mut File> {
        Ok(self.path_and_file()?.1)
    }

    fn path_and_file(&mut self) -> Result<(&Path, &mut File)> {
        // Split the borrow by field: stable rustc rejects an early return
        // that reads `self` while `self.file` is borrowed mutably (Trust's
        // borrow checker accepts it), and the release's x86_64 slice builds
        // with stable.
        let Self { path, file, .. } = self;
        match file.as_mut() {
            Some(file) => Ok((path.as_path(), file)),
            None => Err(Self::closed(path)),
        }
    }

    fn durable(&self) -> Result<()> {
        let Some(file) = &self.file else {
            return Err(Self::closed(&self.path));
        };
        aterm_durable::durable(file).map_err(|e| local("cannot make durable", &self.path, &e))
    }

    fn digest(&mut self) -> Result<Sha256Digest> {
        let (path, file) = self.path_and_file()?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| local("cannot read", path, &e))?;
        let mut hasher = aterm_digest::Sha256::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(local("cannot read", path, &e)),
            }
        }
        Ok(Sha256Digest::from_bytes(hasher.finalize()))
    }

    /// Close and rename the part file into `dest`, syncing the parent directory on Unix.
    fn finish(mut self, dest: &Path) -> Result<()> {
        let ours = self
            .file
            .as_ref()
            .is_some_and(|file| still_named(&self.path, file));
        if !ours {
            self.kept = true;
            return Err(Error::local(format!(
                "another download of {} replaced this one's part file",
                dest.display()
            )));
        }
        self.file = None;
        std::fs::rename(&self.path, dest).map_err(|e| local("cannot rename into", dest, &e))?;
        self.kept = true;
        let dir = parent_of(dest);
        aterm_durable::durable_dir(dir).map_err(|e| local("cannot make durable", dir, &e))
    }
}

impl Drop for Part {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let ours = self
            .file
            .take()
            .is_none_or(|file| still_named(&self.path, &file));
        if ours {
            let _ = remove_quietly(&self.path);
        }
    }
}

/// Copy `body` into `out`, each chunk also handed to `seen`. The byte count.
fn copy(
    body: &mut dyn Read,
    out: &mut dyn Write,
    seen: &mut dyn FnMut(&[u8]),
    wrote: &dyn Fn(&std::io::Error) -> Error,
) -> Result<u64> {
    let mut buf = [0u8; 16 * 1024];
    let mut total = 0u64;
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(wire_read(&e)),
        };
        out.write_all(&buf[..n]).map_err(|e| wrote(&e))?;
        seen(&buf[..n]);
        total = total.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PartialBody {
    start: u64,
    total: Option<u64>,
}

fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    let total = total.parse().ok()?;
    (start <= end && end < total).then_some((start, end, total))
}

fn partial_body(
    streamed: &Streamed,
    offset: u64,
    exact_total: Option<u64>,
    cap: u64,
) -> Result<PartialBody> {
    let (start, total) = match streamed.status {
        200 => (0, None),
        206 => {
            let value = streamed.header("content-range").ok_or_else(|| {
                Error::protocol("a resumed body answered 206 with no Content-Range")
            })?;
            let (start, _, total) = content_range(value).ok_or_else(|| {
                Error::protocol(format!(
                    "a resumed body answered an invalid Content-Range: {value}"
                ))
            })?;
            if start != offset {
                return Err(Error::protocol(format!(
                    "a resumed body starts at {start}, not the requested {offset}"
                )));
            }
            if total > cap || exact_total.is_some_and(|expected| total != expected) {
                return Err(Error::protocol(format!(
                    "a resumed body's total is {total}, outside the expected byte bound"
                )));
            }
            (start, Some(total))
        }
        status => {
            return Err(Error::protocol(format!(
                "a streamed download answered unexpected HTTP {status}"
            )));
        }
    };
    Ok(PartialBody { start, total })
}

fn prepare_file(
    path: &Path,
    file: &mut File,
    streamed: &Streamed,
    offset: u64,
    exact_total: Option<u64>,
    cap: u64,
) -> Result<PartialBody> {
    let partial = partial_body(streamed, offset, exact_total, cap)?;
    let start = partial.start;
    if start == 0 {
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .map_err(|e| local("cannot reset", path, &e))?;
    } else {
        let held = file
            .metadata()
            .map_err(|e| local("cannot inspect", path, &e))?
            .len();
        if held != start {
            return Err(Error::local(format!(
                "{}: the resumed prefix changed from {start} to {held} bytes",
                path.display()
            )));
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|e| local("cannot resume", path, &e))?;
    }
    Ok(partial)
}

fn prepare_partial(
    part: &mut Part,
    streamed: &Streamed,
    offset: u64,
    exact_total: Option<u64>,
    cap: u64,
) -> Result<PartialBody> {
    let (path, file) = part.path_and_file()?;
    prepare_file(path, file, streamed, offset, exact_total, cap)
}

/// One durable part file that file transfers fill, over one call or several: a later call
/// resumes from the exact prefix the earlier ones left (a route change included), and the
/// client's shared progress budget bounds every Range retry within each call.
pub(crate) struct ResumableFile {
    part: Part,
    dest: PathBuf,
    /// The bytes the part file holds.
    offset: Cell<u64>,
    /// Every byte any call has delivered (a prefix a 200 replaced counted again).
    arrived: Cell<u64>,
}

impl ResumableFile {
    /// A fresh part file for `dest`.
    ///
    /// # Errors
    ///
    /// Usage for a destination `refuse_part_name` or `refuse_non_file` refuses; the part
    /// file's local error.
    pub(crate) fn create(dest: &Path) -> Result<ResumableFile> {
        refuse_part_name(dest)?;
        refuse_non_file(dest)?;
        Ok(ResumableFile {
            part: Part::create(dest)?,
            dest: dest.to_path_buf(),
            offset: Cell::new(0),
            arrived: Cell::new(0),
        })
    }

    /// Every byte delivered so far, by every call.
    pub(crate) fn arrived(&self) -> u64 {
        self.arrived.get()
    }

    /// Send `call` and write its body after the prefix already held (or over it, when the
    /// server answers 200), at most `cap` bytes in all. The bytes held.
    ///
    /// # Errors
    ///
    /// The call's or the write's error; Truncated when the body ends before its total.
    pub(crate) fn call(&mut self, gh: &Github, call: &Call, cap: u64) -> Result<u64> {
        let Self {
            part,
            offset,
            arrived,
            ..
        } = self;
        gh.call_stream_resumable_with(
            call,
            || offset.get(),
            |mut streamed| {
                let partial = prepare_partial(part, &streamed, offset.get(), None, cap)?;
                offset.set(partial.start);
                let (path, file) = part.path_and_file()?;
                let copied = copy(
                    &mut streamed.body,
                    file,
                    &mut |chunk| {
                        let n = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                        offset.set(offset.get().saturating_add(n));
                        arrived.set(arrived.get().saturating_add(n));
                    },
                    &|e| local("cannot write", path, e),
                )?;
                let all = partial.start.saturating_add(copied);
                if let Some(total) = partial.total
                    && all < total
                {
                    return Err(Error::could_not_run(format!(
                        "truncated: download sent {all} of {total} bytes"
                    ))
                    .with_sub(Sub::Wire(WireKind::Truncated)));
                }
                Ok(all)
            },
        )
    }

    /// Make the bytes durable and rename the part file onto the destination.
    ///
    /// # Errors
    ///
    /// The local error; Usage when something other than a regular file took the destination.
    pub(crate) fn finish(self) -> Result<()> {
        self.part.durable()?;
        refuse_non_file(&self.dest)?;
        self.part.finish(&self.dest)
    }
}

/// `O_NONBLOCK`, which differs: 0o4000 on Linux, 0x0004 on Darwin and the BSDs.
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_NONBLOCK: i32 = 0o4000;
/// `O_NONBLOCK`, which differs: 0o4000 on Linux, 0x0004 on Darwin and the BSDs.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const O_NONBLOCK: i32 = 0x0004;

/// Open `path` to read it without waiting: on unix `O_NONBLOCK`, so a FIFO that appeared after
/// a look at the path never blocks the open (the handle's type is judged next). A regular
/// file's reads are the same with it. An upload's file, `--notes-file` and a tape's files (its
/// head, a replayed body) open here too.
pub(crate) fn open_nonblocking(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, O_NONBLOCK);
    options.open(path)
}

/// `(O_NONBLOCK | O_NOFOLLOW)` on Unix targets, matching `astream_sys`'s per-architecture flags
/// (`asm-generic/fcntl.h` vs ARM/PowerPC overrides on Linux, and `<sys/fcntl.h>` on Darwin/BSDs).
#[cfg(unix)]
const fn dest_open_flags() -> i32 {
    if cfg!(all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "arm",
            target_arch = "aarch64",
            target_arch = "powerpc",
            target_arch = "powerpc64"
        )
    )) {
        O_NONBLOCK | 0o100000
    } else if cfg!(any(target_os = "linux", target_os = "android")) {
        O_NONBLOCK | 0o400000
    } else if cfg!(target_os = "openbsd") {
        O_NONBLOCK | 0x0200
    } else {
        O_NONBLOCK | 0x0100
    }
}

/// Whether `open(2)` refused `path` because its final component is a symlink (`ELOOP`, or
/// FreeBSD/NetBSD's `EFTYPE`).
#[cfg(unix)]
fn is_symlink_open_refusal(error: &std::io::Error) -> bool {
    let code = error.raw_os_error();
    if cfg!(any(target_os = "linux", target_os = "android")) {
        code == Some(40)
    } else if cfg!(any(
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd"
    )) {
        matches!(code, Some(62 | 79))
    } else {
        code == Some(62)
    }
}

/// Open destination `path` for digest inspection without waiting on a FIFO or following a symlink.
pub(crate) fn open_regular_dest_nonblocking(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, dest_open_flags());
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::custom_flags(&mut options, 0x0020_0000);
    options.open(path)
}

/// The sha256 of the regular file at `path` (never following a symlink); `None` when absent.
/// Anything there other than a regular file is usage (`refuse_non_file`), judged unfollowed and on
/// the open handle.
#[cfg(all(test, unix))]
fn file_digest(path: &Path) -> Result<Option<Sha256Digest>> {
    Ok(file_digest_if_size(path, None)?.flatten())
}

/// Inspects the regular file at `path` (never following a symlink): returns `Ok(None)` when
/// absent, `Ok(Some(None))` when present as a regular file whose length differs from
/// `expected_size`, and `Ok(Some(Some(digest)))` when hashed.
fn file_digest_if_size(
    path: &Path,
    expected_size: Option<u64>,
) -> Result<Option<Option<Sha256Digest>>> {
    #[cfg(not(unix))]
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Err(not_a_file(path)),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(local("cannot read", path, &e)),
    }
    let mut file = match open_regular_dest_nonblocking(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(ref e) if is_symlink_open_refusal(e) => return Err(not_a_file(path)),
        Err(_) if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_file()) => {
            return Err(not_a_file(path));
        }
        Err(e) => return Err(local("cannot read", path, &e)),
    };
    let metadata = file
        .metadata()
        .map_err(|e| local("cannot read", path, &e))?;
    if !metadata.is_file() {
        return Err(not_a_file(path));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            return Err(not_a_file(path));
        }
    }
    if expected_size.is_some_and(|size| metadata.len() != size) {
        return Ok(Some(None));
    }
    let mut hasher = aterm_digest::Sha256::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(local("cannot read", path, &e)),
        }
    }
    Ok(Some(Some(Sha256Digest::from_bytes(hasher.finalize()))))
}

/// Conflict: `dest` holds other bytes and `clobber` is off.
fn exists_with_other_bytes(dest: &Path) -> Error {
    Error::new(
        Kind::Conflict,
        format!(
            "{} exists with different bytes; --clobber replaces it",
            dest.display()
        ),
    )
}

/// Where the bytes are while they stream.
enum Dest<'a> {
    File {
        part: Option<Part>,
        dest: PathBuf,
        clobber: bool,
    },
    Memory(Vec<u8>),
    Writer {
        _dir: aterm_tempfile::TempDir,
        path: PathBuf,
        spool: File,
        writer: &'a mut dyn Write,
    },
}

fn validate_download(name: &str, expect: &Expect, bytes: u64, got: Sha256Digest) -> Result<()> {
    if bytes > expect.size {
        return Err(Error::could_not_run(format!(
            "too_large: asset {name} sent {bytes} bytes, not {}",
            expect.size
        ))
        .with_sub(Sub::Wire(WireKind::TooLarge)));
    }
    if bytes < expect.size {
        return Err(Error::could_not_run(format!(
            "truncated: asset {name} sent {bytes} of {} bytes",
            expect.size
        ))
        .with_sub(Sub::Wire(WireKind::Truncated)));
    }
    if let Some(expected) = expect.digest
        && expected != got
    {
        return Err(Error::new(
            Kind::Conflict,
            format!("asset {name}'s bytes do not match its digest"),
        )
        .with_sub(Sub::DigestMismatch {
            expected: expected.to_string(),
            got: got.to_string(),
        }));
    }
    Ok(())
}

/// Downloads release asset `asset` into `sink` with size and digest verification.
pub(crate) fn fetch(
    gh: &Github,
    slug: &Slug,
    how: &How,
    asset: &AssetObject,
    sink: Sink<'_>,
    expect: Expect,
) -> Result<Downloaded> {
    fetch_inner(gh, slug, how, asset, sink, &expect).map_err(|error| {
        if error.repo.is_none() {
            error.with_repo(slug.to_string())
        } else {
            error
        }
    })
}

/// `fetch`, before its errors name the repository.
fn fetch_inner(
    gh: &Github,
    slug: &Slug,
    how: &How,
    asset: &AssetObject,
    sink: Sink<'_>,
    expect: &Expect,
) -> Result<Downloaded> {
    let name = &asset.name;
    if let Sink::File { path, .. } = &sink {
        refuse_part_name(path)?;
        refuse_non_file(path)?;
    }
    if asset.state() != AssetState::Uploaded {
        return Err(Error::new(
            Kind::Incomplete,
            format!("asset {name} is {}, not uploaded", asset.state),
        )
        .with_sub(Sub::NotUploaded {
            state: asset.state.clone(),
        }));
    }
    if let (Sink::File { path, clobber }, Some(digest)) = (&sink, expect.digest) {
        match file_digest_if_size(path, Some(expect.size))? {
            Some(Some(there)) if there == digest => {
                return Ok(Downloaded {
                    asset_id: asset.id,
                    bytes: expect.size,
                    sha256: digest,
                    path: Some(path.clone()),
                    data: None,
                    written: false,
                    digest: DigestCheck::Matched,
                    requests: 0,
                });
            }
            Some(_) if !clobber => return Err(exists_with_other_bytes(path)),
            _ => {}
        }
    }
    let limit = match &sink {
        Sink::Bounded(cap) => expect.limit.min(*cap),
        Sink::File { .. } | Sink::Writer(_) => expect.limit,
    };
    let call = request(gh, slug, how, asset, expect)?.limit(limit);
    let mut into = match sink {
        Sink::File { path, clobber } => Dest::File {
            part: Some(Part::create(&path)?),
            dest: path,
            clobber,
        },
        Sink::Bounded(_) => Dest::Memory(Vec::with_capacity(
            usize::try_from(expect.size.min(limit).min(64 * 1024)).unwrap_or(0),
        )),
        Sink::Writer(writer) => {
            let dir = aterm_tempfile::tempdir()
                .map_err(|e| Error::local(format!("download spool directory: {e}")))?;
            let path = dir.path().join("body");
            let spool = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| local("cannot create", &path, &e))?;
            Dest::Writer {
                _dir: dir,
                path,
                spool,
                writer,
            }
        }
    };
    let (bytes, got, mut requests) = match &mut into {
        Dest::File { part, .. } => {
            let part = part
                .as_mut()
                .ok_or_else(|| Error::local("the download part file is absent"))?;
            let offset = Cell::new(0u64);
            gh.call_stream_resumable_with(
                &call,
                || offset.get(),
                |mut streamed| {
                    let partial =
                        prepare_partial(part, &streamed, offset.get(), Some(expect.size), limit)?;
                    offset.set(partial.start);
                    let copied = {
                        let (path, file) = part.path_and_file()?;
                        copy(
                            &mut streamed.body,
                            file,
                            &mut |chunk| {
                                offset.set(offset.get().saturating_add(
                                    u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                                ));
                            },
                            &|e| local("cannot write", path, e),
                        )?
                    };
                    let bytes = partial.start.saturating_add(copied);
                    if partial.total.is_some_and(|total| bytes < total) {
                        return Err(Error::could_not_run(format!(
                            "truncated: asset {name} sent {bytes} of {} bytes",
                            expect.size
                        ))
                        .with_sub(Sub::Wire(WireKind::Truncated)));
                    }
                    let got = part.digest()?;
                    validate_download(name, expect, bytes, got)?;
                    Ok((bytes, got, streamed.requests))
                },
            )?
        }
        Dest::Memory(data) => {
            let offset = Cell::new(0u64);
            gh.call_stream_resumable_with(
                &call,
                || offset.get(),
                |mut streamed| {
                    let partial = partial_body(&streamed, offset.get(), Some(expect.size), limit)?;
                    if partial.start == 0 {
                        data.clear();
                    } else if u64::try_from(data.len()).unwrap_or(u64::MAX) != partial.start {
                        return Err(Error::local(format!(
                            "the resumed memory prefix changed from {} to {} bytes",
                            partial.start,
                            data.len()
                        )));
                    }
                    offset.set(partial.start);
                    let copied =
                        copy(
                            &mut streamed.body,
                            data,
                            &mut |chunk| {
                                offset.set(offset.get().saturating_add(
                                    u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                                ));
                            },
                            &|e| Error::local(format!("cannot hold the bytes: {e}")),
                        )?;
                    let bytes = partial.start.saturating_add(copied);
                    if partial.total.is_some_and(|total| bytes < total) {
                        return Err(Error::could_not_run(format!(
                            "truncated: asset {name} sent {bytes} of {} bytes",
                            expect.size
                        ))
                        .with_sub(Sub::Wire(WireKind::Truncated)));
                    }
                    let got =
                        Sha256Digest::from_bytes(aterm_digest::Sha256::digest(data.as_slice()));
                    validate_download(name, expect, bytes, got)?;
                    Ok((bytes, got, streamed.requests))
                },
            )?
        }
        Dest::Writer { path, spool, .. } => {
            let offset = Cell::new(0u64);
            gh.call_stream_resumable_with(
                &call,
                || offset.get(),
                |mut streamed| {
                    let partial = prepare_file(
                        path,
                        spool,
                        &streamed,
                        offset.get(),
                        Some(expect.size),
                        limit,
                    )?;
                    offset.set(partial.start);
                    let bytes =
                        partial.start.saturating_add(copy(
                            &mut streamed.body,
                            spool,
                            &mut |chunk| {
                                offset.set(offset.get().saturating_add(
                                    u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                                ));
                            },
                            &|e| Error::local(format!("cannot write the download spool: {e}")),
                        )?);
                    if partial.total.is_some_and(|total| bytes < total) {
                        return Err(Error::could_not_run(format!(
                            "truncated: asset {name} sent {bytes} of {} bytes",
                            expect.size
                        ))
                        .with_sub(Sub::Wire(WireKind::Truncated)));
                    }
                    let got = file_digest_if_size(path, Some(bytes))?
                        .flatten()
                        .ok_or_else(|| {
                            Error::local(format!(
                                "the download spool {} vanished or changed size",
                                path.display()
                            ))
                        })?;
                    validate_download(name, expect, bytes, got)?;
                    Ok((bytes, got, streamed.requests))
                },
            )?
        }
    };
    let digest = match expect.digest {
        Some(_) => DigestCheck::Matched,
        None => {
            let (again, sent) =
                crate::github::releases::asset_object_counted(gh, slug, how, asset.id)?;
            requests = requests.saturating_add(sent);
            if again.size != asset.size || again.updated_at != asset.updated_at {
                let sub = if again.size == asset.size {
                    Sub::None
                } else {
                    Sub::SizeMismatch {
                        expected: asset.size,
                        got: again.size,
                    }
                };
                return Err(Error::new(
                    Kind::Conflict,
                    format!("asset {name} changed while it downloaded"),
                )
                .with_sub(sub));
            }
            DigestCheck::Absent
        }
    };
    let mut done = Downloaded {
        asset_id: asset.id,
        bytes,
        sha256: got,
        path: None,
        data: None,
        written: true,
        digest,
        requests,
    };
    match into {
        Dest::File {
            part,
            dest,
            clobber,
        } => {
            let part = part.ok_or_else(|| Error::local("the download part file is absent"))?;
            part.durable()?;
            match file_digest_if_size(&dest, Some(bytes))? {
                None => part.finish(&dest)?,
                Some(Some(there)) if there == got => {
                    drop(part);
                    done.written = false;
                }
                Some(_) if clobber => part.finish(&dest)?,
                Some(_) => return Err(exists_with_other_bytes(&dest)),
            }
            done.path = Some(dest);
        }
        Dest::Memory(data) => done.data = Some(data),
        Dest::Writer {
            mut spool, writer, ..
        } => {
            spool
                .seek(SeekFrom::Start(0))
                .and_then(|_| std::io::copy(&mut spool, writer))
                .map_err(|e| Error::local(format!("cannot write the bytes: {e}")))?;
        }
    }
    Ok(done)
}

/// Build the download request for `asset`.
fn request(
    gh: &Github,
    slug: &Slug,
    how: &How,
    asset: &AssetObject,
    expect: &Expect,
) -> Result<Call> {
    let call = if how.anonymous {
        let url = &asset.browser_download_url;
        let on_web = Origin::parse(url).filter(|(origin, _)| origin == gh.endpoints().web());
        let Some((_, path_and_query)) = on_web else {
            return Err(Error::protocol(format!(
                "asset {}'s browser_download_url {url} is not on the web origin",
                asset.name
            )));
        };
        how.get(path_and_query, RouteTarget::Repo(slug.clone()))
            .origin(OriginRole::Web)
    } else {
        how.get(
            format!("{}/releases/assets/{}", repo_root(slug), asset.id),
            RouteTarget::Repo(slug.clone()),
        )
    };
    Ok(call
        .accept(Accept::OctetStream)
        .hop(true)
        .deadlines(Deadlines::for_transfer(expect.size)))
}

#[cfg(all(test, unix))]
#[path = "download_tests.rs"]
mod tests;
