// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Release asset downloads with SHA-256 verification and atomic `.agh-part` staging.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use crate::client::{Accept, Call, Github};
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

/// Rejects a destination that exists and is not a regular file.
///
/// # Errors
///
/// Returns a usage error for a non-regular file or a local error if metadata cannot be read.
pub(crate) fn refuse_non_file(dest: &Path) -> Result<()> {
    match std::fs::metadata(dest) {
        Ok(meta) if meta.is_file() => Ok(()),
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

/// Usage for a destination that is itself a part file's name: a later download of the
/// destination it belongs to would remove it, finished and verified as it was.
fn refuse_part_name(dest: &Path) -> Result<()> {
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
    Error::could_not_run(format!("{what} {}: {e}", path.display())).with_sub(Sub::Local)
}

/// A read of the body that failed: CouldNotRun `Wire(kind)`, why `{kind}: {detail}`.
fn wire_read(e: &std::io::Error) -> Error {
    let kind = wire_kind_of(e);
    Error::could_not_run(format!("{}: {e}", kind.slug())).with_sub(Sub::Wire(kind))
}

/// The directory a rename into `dest` changes.
#[cfg(unix)]
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

/// Whether `path` still names the open `file`. A second download of the same destination
/// removes the part file it finds and makes its own, so a part whose name now points at
/// another file belongs to that download: renaming or removing it by name would move or
/// delete the other run's bytes. Unix compares device and inode; elsewhere std has no stable
/// file identity, and the answer is always yes.
#[cfg(unix)]
fn still_named(path: &Path, file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(path), file.metadata()) {
        (Ok(named), Ok(open)) => named.dev() == open.dev() && named.ino() == open.ino(),
        _ => false,
    }
}

/// Whether `path` still names the open `file`. Off unix std has no stable file identity, but
/// none is needed: `Part::create` opens the part file sharing only reads (no
/// `FILE_SHARE_DELETE`), so while it is open no other download can remove or rename it, and
/// that download refuses at its own removal instead.
#[cfg(not(unix))]
fn still_named(_path: &Path, _file: &File) -> bool {
    true
}

/// Windows' `FILE_SHARE_READ`: the one access another opener of the part file is granted.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

/// A part file, removed when dropped unless it was renamed into place or another download of
/// the same destination replaced it.
struct Part {
    path: PathBuf,
    /// The open part file; `None` once `finish` or `drop` closed it, before the rename or the
    /// removal by name (Windows does neither to a file open without `FILE_SHARE_DELETE`).
    file: Option<File>,
    kept: bool,
}

impl Part {
    /// A leftover part file of `dest` is removed, then a fresh one is made (`create_new`). A
    /// `dest` that is itself a part file's name is usage, and nothing is removed. On Windows
    /// it is opened sharing reads only, so no other download removes it while it is open.
    fn create(dest: &Path) -> Result<Part> {
        refuse_part_name(dest)?;
        let path = part_path(dest);
        remove_quietly(&path).map_err(|e| local("cannot remove the old part file", &path, &e))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
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

    /// The CouldNotRun of a use of the part file after it was closed.
    fn closed(&self) -> Error {
        Error::could_not_run(format!("{}: the part file is closed", self.path.display()))
            .with_sub(Sub::Local)
    }

    /// The open part file.
    fn file(&mut self) -> Result<&mut File> {
        let closed = self.closed();
        self.file.as_mut().ok_or(closed)
    }

    /// `durable` on the open part file.
    fn durable(&self) -> Result<()> {
        let Some(file) = &self.file else {
            return Err(self.closed());
        };
        aterm_durable::durable(file).map_err(|e| local("cannot make durable", &self.path, &e))
    }

    /// Rename into `dest`, then, on unix, `durable_dir` of its directory (Windows opens no
    /// directory as a file, so there the rename is the last step, as `TapeSink::finish`'s
    /// file flush is). A part file another download of `dest` replaced is left to that
    /// download: CouldNotRun `Local`, nothing renamed. The file is closed just before the
    /// rename; a failed rename removes it.
    fn finish(mut self, dest: &Path) -> Result<()> {
        let ours = self
            .file
            .as_ref()
            .is_some_and(|file| still_named(&self.path, file));
        if !ours {
            self.kept = true;
            return Err(Error::could_not_run(format!(
                "another download of {} replaced this one's part file",
                dest.display()
            ))
            .with_sub(Sub::Local));
        }
        self.file = None;
        std::fs::rename(&self.path, dest).map_err(|e| local("cannot rename into", dest, &e))?;
        self.kept = true;
        #[cfg(unix)]
        {
            let dir = parent_of(dest);
            aterm_durable::durable_dir(dir).map_err(|e| local("cannot make durable", dir, &e))?;
        }
        Ok(())
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
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(wire_read(&e)),
        };
        seen(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| wrote(&e))?;
        total = total.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
    }
}

/// Streams `body` to `dest` via a durable part file and returns the byte count.
pub(crate) fn stream_to_file(
    body: &mut dyn Read,
    dest: &Path,
    seen: &mut dyn FnMut(&[u8]),
) -> Result<u64> {
    let mut part = Part::create(dest)?;
    let path = part.path.clone();
    let written = copy(body, part.file()?, seen, &|e| {
        local("cannot write", &path, e)
    })?;
    part.durable()?;
    part.finish(dest)?;
    Ok(written)
}

/// `O_NONBLOCK`, which differs: 0o4000 on Linux, 0x0004 on Darwin and the BSDs.
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_NONBLOCK: i32 = 0o4000;
/// `O_NONBLOCK`, which differs: 0o4000 on Linux, 0x0004 on Darwin and the BSDs.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const O_NONBLOCK: i32 = 0x0004;

/// Open `path` to read it without waiting: on unix `O_NONBLOCK`, so a FIFO that appeared after
/// `refuse_non_file` looked never blocks the open (the handle's type is judged next).
fn open_to_hash(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, O_NONBLOCK);
    options.open(path)
}

/// The sha256 of the file at `path`; `None` when there is no file. Anything there but a
/// regular file is usage (`refuse_non_file`), judged on the open handle.
fn file_digest(path: &Path) -> Result<Option<Sha256Digest>> {
    let mut file = match open_to_hash(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) if path.is_dir() => return Err(not_a_file(path)),
        Err(e) => return Err(local("cannot read", path, &e)),
    };
    let is_file = file
        .metadata()
        .map_err(|e| local("cannot read", path, &e))?
        .is_file();
    if !is_file {
        return Err(not_a_file(path));
    }
    let mut hasher = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(local("cannot read", path, &e)),
        }
    }
    Ok(Some(Sha256Digest::from_bytes(hasher.finalize())))
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
        part: Part,
        dest: PathBuf,
        clobber: bool,
    },
    Memory(Vec<u8>),
    Writer(&'a mut dyn Write),
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
        match file_digest(path)? {
            Some(there) if there == digest => {
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
            part: Part::create(&path)?,
            dest: path,
            clobber,
        },
        Sink::Bounded(_) => Dest::Memory(Vec::new()),
        Sink::Writer(writer) => Dest::Writer(writer),
    };
    let streamed = gh.call_stream(&call)?;
    let mut requests = streamed.requests;
    let mut body = streamed.body;
    let mut hasher = aterm_digest::Sha256::new();
    let mut seen = |chunk: &[u8]| hasher.update(chunk);
    let bytes = match &mut into {
        Dest::File { part, .. } => {
            let path = part.path.clone();
            copy(&mut body, part.file()?, &mut seen, &|e| {
                local("cannot write", &path, e)
            })?
        }
        Dest::Memory(data) => copy(&mut body, data, &mut seen, &|e| {
            Error::could_not_run(format!("cannot hold the bytes: {e}")).with_sub(Sub::Local)
        })?,
        Dest::Writer(writer) => copy(&mut body, *writer, &mut seen, &|e| {
            Error::could_not_run(format!("cannot write the bytes: {e}")).with_sub(Sub::Local)
        })?,
    };
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
    let got = Sha256Digest::from_bytes(hasher.finalize());
    let digest = match expect.digest {
        Some(expected) if expected != got => {
            return Err(Error::new(
                Kind::Conflict,
                format!("asset {name}'s bytes do not match its digest"),
            )
            .with_sub(Sub::DigestMismatch {
                expected: expected.to_string(),
                got: got.to_string(),
            }));
        }
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
            part.durable()?;
            match file_digest(&dest)? {
                None => part.finish(&dest)?,
                Some(there) if there == got => {
                    drop(part);
                    done.written = false;
                }
                Some(_) if clobber => part.finish(&dest)?,
                Some(_) => return Err(exists_with_other_bytes(&dest)),
            }
            done.path = Some(dest);
        }
        Dest::Memory(data) => done.data = Some(data),
        Dest::Writer(_) => {}
    }
    Ok(done)
}

/// Step 3: the call for the bytes.
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
mod tests {
    use super::*;

    /// A second download of the same destination removed this one's part file and made its
    /// own: this one neither renames nor removes the other's bytes.
    #[test]
    fn a_part_file_another_download_replaced_is_left_to_it() {
        let root = aterm_tempfile::Builder::new()
            .prefix("agh-part-race-")
            .tempdir()
            .expect("a scratch directory");
        let dest = root.path().join("asset.bin");
        let mut first = Part::create(&dest).expect("the first part file");
        first
            .file()
            .expect("open")
            .write_all(b"first")
            .expect("the first bytes");
        let mut second = Part::create(&dest).expect("the second part file");
        second
            .file()
            .expect("open")
            .write_all(b"second")
            .expect("the second bytes");

        let error = first
            .finish(&dest)
            .expect_err("the first part file was replaced");
        assert_eq!(error.kind, Kind::CouldNotRun);
        assert_eq!(error.sub, Sub::Local);
        assert!(!dest.exists());
        assert_eq!(
            std::fs::read(part_path(&dest)).expect("the second part file"),
            b"second"
        );

        second.finish(&dest).expect("the second download finishes");
        assert_eq!(std::fs::read(&dest).expect("the destination"), b"second");
        assert!(!part_path(&dest).exists());
    }

    /// A part file dropped on an error is removed when it is still this download's.
    #[test]
    fn a_dropped_part_file_is_removed() {
        let root = aterm_tempfile::Builder::new()
            .prefix("agh-part-drop-")
            .tempdir()
            .expect("a scratch directory");
        let dest = root.path().join("asset.bin");
        drop(Part::create(&dest).expect("a part file"));
        assert!(!part_path(&dest).exists());
    }
}
