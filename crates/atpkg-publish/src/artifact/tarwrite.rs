// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `.tar.zst` writer: a deterministic ustar/PAX archive of a staged tree, compressed
//! through the [`Compressor`] port.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use aterm_zstd::xxh64::Xxh64;

use crate::error::{Error, Result};

/// The zstd level every pack is compressed at.
pub const LEVEL: i32 = 19;

/// The program every pack is compressed by.
pub const COMPRESSOR: &str = "zstd";

/// [`COMPRESSOR`]'s path on `PATH`.
/// # Errors
/// What is missing, worded for the lane's log: no executable `zstd` on `PATH`.
pub fn compressor_on_path() -> std::result::Result<PathBuf, String> {
    compressor_in(std::env::var_os("PATH").as_deref())
}

/// [`compressor_on_path`] over the `PATH` value `path` (`None`: unset).
/// # Errors
/// As [`compressor_on_path`].
pub fn compressor_in(path: Option<&OsStr>) -> std::result::Result<PathBuf, String> {
    path.and_then(|path| {
        std::env::split_paths(path)
            .map(|d| d.join(COMPRESSOR))
            .find(|p| atpkg::toolchain::is_executable_file(p))
    })
    .ok_or_else(|| {
        format!(
            "no `{COMPRESSOR}` on this builder's PATH (every pack is compressed by the zstd CLI)"
        )
    })
}

/// What [`write`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    /// The `.tar.zst` written.
    pub asset: PathBuf,
    /// Tar members (directories, files and hard links).
    pub members: usize,
    /// The asset's sha256.
    pub sha256: String,
    /// The asset's size in bytes.
    pub size: u64,
    /// The decompressed tar stream's length.
    pub stream_bytes: u64,
    /// The sum of every regular file's size (each name of a hard-link group counted).
    pub content_bytes: u64,
    /// What the manifest signs as `[artifact.cost] disk_installed`.
    pub disk_installed: u64,
    /// The client's `tree_root` of the stage, after normalization.
    pub tree_root: String,
}

/// The signed size of a packed tree.
#[must_use]
pub fn disk_installed(content_bytes: u64, stream_bytes: u64) -> u64 {
    content_bytes.max(stream_bytes.div_ceil(2))
}

/// One member, in archive order.
enum Member {
    Dir(Vec<u8>),
    File {
        rel: Vec<u8>,
        path: PathBuf,
        size: u64,
    },
    Link {
        rel: Vec<u8>,
        target: Vec<u8>,
    },
}

/// What compresses a pack's tar stream into its asset.
pub trait Compressor {
    /// Write the zstd frames of the stream `tar` writes into `asset` (an empty file open
    /// for writing).
    ///
    /// # Errors
    ///
    /// Why the stream could not be compressed (a compressor that did not start or
    /// failed), else `tar`'s own error.
    fn compress(
        &self,
        asset: &std::fs::File,
        tar: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()>;
}

/// The zstd CLI at `program`, run as [`COMPRESSOR`] is: the tar stream on its stdin, the
/// asset as its stdout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZstdCli {
    /// A name looked up on `PATH` ([`COMPRESSOR`] by default), or a path.
    pub program: OsString,
}

impl Default for ZstdCli {
    fn default() -> Self {
        Self {
            program: COMPRESSOR.into(),
        }
    }
}

/// Raw zstd frames, uncompressed and each with its content checksum, of at most
/// `frame_max` stream bytes each: what aterm-zstd's raw-block writer
/// (`aterm_zstd::frame::write_raw`) writes, which every client's decoder reads like the
/// CLI's frames.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawFrames {
    /// The most stream bytes one frame holds, at most `aterm_zstd::frame::WINDOW_MAX`
    /// (a smaller one makes a small stream span several frames).
    pub frame_max: usize,
}

#[cfg(any(test, debug_assertions))]
impl RawFrames {
    /// The largest frames: one per `aterm_zstd::frame::WINDOW_MAX` stream bytes.
    pub const WHOLE: RawFrames = RawFrames {
        frame_max: aterm_zstd::frame::WINDOW_MAX,
    };
}

#[cfg(any(test, debug_assertions))]
impl Compressor for RawFrames {
    fn compress(
        &self,
        asset: &std::fs::File,
        tar: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        /// Buffers the stream and writes one frame per `max` bytes of it.
        struct Frames<W: Write> {
            out: W,
            pending: Vec<u8>,
            max: usize,
        }
        impl<W: Write> Frames<W> {
            fn frame(&mut self) -> io::Result<()> {
                aterm_zstd::frame::write_raw(
                    &mut self.out,
                    &self.pending,
                    aterm_zstd::frame::Checksum::Xxh64,
                )?;
                self.pending.clear();
                Ok(())
            }
        }
        impl<W: Write> Write for Frames<W> {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                let k = data.len().min(self.max - self.pending.len());
                self.pending.extend_from_slice(&data[..k]);
                if self.pending.len() == self.max {
                    self.frame()?;
                }
                Ok(k)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut frames = Frames {
            out: io::BufWriter::new(asset),
            pending: Vec::new(),
            max: self.frame_max.clamp(1, aterm_zstd::frame::WINDOW_MAX),
        };
        tar(&mut frames)?;
        if !frames.pending.is_empty() {
            frames.frame()?;
        }
        frames.out.flush()
    }
}

/// Pack the tree at `root` into `asset` (replaced), after normalizing its modes,
/// compressed by the zstd CLI on `PATH` ([`ZstdCli::default`]).
///
/// # Errors
/// A refusal for a symlink or special file in the tree, or a tree with no file at all; a
/// could-not-run for any I/O failure.
pub fn write(root: &Path, asset: &Path) -> Result<Packed> {
    write_with(root, asset, &ZstdCli::default())
}

/// [`write`], compressing with `compressor`: a pack's [`super::tools::Tools::compressor`],
/// a zstd CLI at another path, or a stand-in that shows how the pack refuses a compressor
/// that fails or writes another stream.
///
/// # Errors
/// As [`write`].
pub fn write_with(root: &Path, asset: &Path, compressor: &dyn Compressor) -> Result<Packed> {
    let members = plan(root)?;
    if !members.iter().any(|m| matches!(m, Member::File { .. })) {
        return Err(Error::refused(format!(
            "{} holds no file: an empty archive is never signed",
            root.display()
        )));
    }
    let content_bytes = content(root)?;
    let stream_bytes = encode(&members, asset, compressor)
        .map_err(|e| Error::io(format!("write {}", asset.display()), &e))?;
    let sha256 = atpkg::sha256_file(asset)
        .map_err(|e| Error::io(format!("hash {}", asset.display()), &e))?;
    let size = std::fs::metadata(asset)
        .map_err(|e| Error::io(format!("stat {}", asset.display()), &e))?
        .len();
    let tree_root = atpkg::tree_root(root)
        .map_err(|e| Error::io(format!("tree_root {}", root.display()), &e))?;
    Ok(Packed {
        asset: asset.to_path_buf(),
        members: members.len(),
        sha256,
        size,
        stream_bytes,
        content_bytes,
        disk_installed: disk_installed(content_bytes, stream_bytes),
        tree_root,
    })
}

fn rel_bytes(rel: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        rel.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        rel.to_string_lossy().replace('\\', "/").into_bytes()
    }
}

/// Walk `root`, normalize every mode, and lay the members out sorted by path bytes, the
/// first name of a hard-link group its file and every later name an `h` member.
fn plan(root: &Path) -> Result<Vec<Member>> {
    let mut found: Vec<(Vec<u8>, PathBuf, std::fs::Metadata)> = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| Error::io(format!("read {}", dir.display()), &e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(format!("read {}", dir.display()), &e))?;
            let child = rel.join(entry.file_name());
            let path = root.join(&child);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| Error::io(format!("stat {}", path.display()), &e))?;
            let ft = meta.file_type();
            if ft.is_symlink() {
                return Err(Error::refused(format!(
                    "{} is a symlink: the signed tree is symlink-free by contract (the \
                     client's extractor refuses every link in a release payload)",
                    child.display()
                )));
            }
            if !ft.is_dir() && !ft.is_file() {
                return Err(Error::refused(format!(
                    "{} is neither a file nor a directory",
                    child.display()
                )));
            }
            normalize_mode(&path, &meta)?;
            if ft.is_dir() {
                stack.push(child.clone());
            }
            found.push((rel_bytes(&child), path, meta));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let mut first_of: BTreeMap<(u64, u64), Vec<u8>> = BTreeMap::new();
    let mut members = Vec::with_capacity(found.len());
    for (rel, path, meta) in found {
        if meta.is_dir() {
            members.push(Member::Dir(rel));
            continue;
        }
        if let Some(key) = link_key(&meta) {
            if let Some(target) = first_of.get(&key) {
                members.push(Member::Link {
                    rel,
                    target: target.clone(),
                });
                continue;
            }
            first_of.insert(key, rel.clone());
        }
        members.push(Member::File {
            rel,
            path,
            size: meta.len(),
        });
    }
    Ok(members)
}

/// `(dev, ino)` for a file with more than one name, else `None`.
fn link_key(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn normalize_mode(path: &Path, meta: &std::fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = meta.permissions().mode() & 0o7777;
        let want = atpkg::extract::safe_mode(mode, meta.is_dir());
        if mode != want {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(want))
                .map_err(|e| Error::io(format!("chmod {}", path.display()), &e))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, meta);
    Ok(())
}

fn mode_of(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).map_or(0o644, |m| m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o644
    }
}

/// Every regular file's size, each name of a hard-link group counted.
fn content(root: &Path) -> Result<u64> {
    let files = crate::artifact::relocate::walk_files(root).map_err(Error::cannot_run)?;
    let mut sum = 0u64;
    for rel in files {
        let p = root.join(rel);
        sum += std::fs::metadata(&p)
            .map_err(|e| Error::io(format!("stat {}", p.display()), &e))?
            .len();
    }
    Ok(sum)
}

/// Counts and hashes the bytes written through it: the decompressed stream's length and
/// XXH64, which [`decodes_back`] holds the asset to.
struct Counting<W> {
    inner: W,
    n: u64,
    hash: Xxh64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let k = self.inner.write(buf)?;
        self.n += k as u64;
        self.hash.update(&buf[..k]);
        Ok(k)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// How much of the compressor's stderr a failure quotes; the rest is read and dropped.
const STDERR_KEPT: u64 = 16 * 1024;

/// Owns the compressor until its exit is reaped; dropped unreaped, it kills and reaps the
/// child.
struct CompressorChild {
    child: aterm_uds::spawnfd::Child,
    input: Option<io::BufWriter<std::process::ChildStdin>>,
    reaped: bool,
}

impl CompressorChild {
    const fn new(child: aterm_uds::spawnfd::Child) -> Self {
        Self {
            child,
            input: None,
            reaped: false,
        }
    }

    fn close_input(&mut self) {
        if let Some(input) = self.input.take() {
            let (pipe, _buffered) = input.into_parts();
            drop(pipe);
        }
    }

    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.close_input();
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for CompressorChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            self.close_input();
            let _ = self.child.wait();
        }
    }
}

/// Writes the archive through `compressor` into `asset`; returns the tar stream's length.
fn encode(members: &[Member], asset: &Path, compressor: &dyn Compressor) -> io::Result<u64> {
    let file = std::fs::File::create(asset)?;
    let (mut n, mut sum) = (0u64, 0u64);
    compressor.compress(&file, &mut |input| {
        let mut out = Counting {
            inner: input,
            n: 0,
            hash: Xxh64::new(0),
        };
        let streamed = tar(&mut out, members).and_then(|()| out.flush());
        (n, sum) = (out.n, out.hash.digest());
        streamed
    })?;
    aterm_durable::durable(&file)?;
    decodes_back(asset, n, sum)?;
    Ok(n)
}

impl Compressor for ZstdCli {
    fn compress(
        &self,
        asset: &std::fs::File,
        tar: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        use aterm_uds::spawnfd::Stdio;
        let shown = self.program.to_string_lossy();
        let child = aterm_uds::spawnfd::command(&self.program)
            .args(["-q", &format!("-{LEVEL}"), "--single-thread", "-c"])
            .stdin(Stdio::piped())
            .stdout(asset.try_clone()?)
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("`{shown}` (the pack's compressor) did not start: {e}"),
                )
            })?;
        let child = CompressorChild::new(child);
        let (streamed, said, status) = std::thread::scope(move |scope| {
            let mut child = child;
            let stdin = child
                .child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("the compressor's stdin was not piped"))?;
            child.input = Some(io::BufWriter::new(stdin));
            let mut stderr = child
                .child
                .stderr
                .take()
                .ok_or_else(|| io::Error::other("the compressor's stderr was not piped"))?;
            let drain = std::thread::Builder::new()
                .name("pack-zstd-stderr".to_string())
                .spawn_scoped(scope, move || {
                    let mut said = Vec::new();
                    let _ = stderr.by_ref().take(STDERR_KEPT).read_to_end(&mut said);
                    let _ = io::copy(&mut stderr, &mut io::sink());
                    said
                })?;
            let streamed = tar(child.input.as_mut().expect("the input was installed"));
            let status = child.wait()?;
            let said = drain
                .join()
                .map_err(|_| io::Error::other("the compressor's stderr drain panicked"))?;
            Ok::<_, io::Error>((streamed, said, status))
        })?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "`{shown}` failed ({status}): {}",
                String::from_utf8_lossy(&said).trim()
            )));
        }
        streamed
    }
}

/// Reads `asset` back through the decoder every client installs it with: one or more
/// checksummed frames that decode to exactly the `n`-byte stream written, whose XXH64
/// is `sum`.
fn decodes_back(asset: &Path, n: u64, sum: u64) -> io::Result<()> {
    let mut decoder = aterm_zstd::Decoder::new(std::fs::File::open(asset)?).require_checksum();
    let mut hash = Xxh64::new(0);
    let mut got = 0u64;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let k = decoder.read(&mut buf)?;
        if k == 0 {
            break;
        }
        hash.update(&buf[..k]);
        got += k as u64;
    }
    if (got, hash.digest()) != (n, sum) {
        return Err(io::Error::other(format!(
            "the compressed asset decodes to {got} bytes (XXH64 {:016x}), not the {n}-byte \
             stream written (XXH64 {sum:016x})",
            hash.digest()
        )));
    }
    Ok(())
}

/// The tar stream: every member, then the two zero records that end the archive.
fn tar(out: &mut impl Write, members: &[Member]) -> io::Result<()> {
    for m in members {
        match m {
            Member::Dir(rel) => {
                let mut name = rel.clone();
                name.push(b'/');
                entry(out, &name, b'5', 0o755, 0, None)?;
            }
            Member::Link { rel, target } => entry(out, rel, b'1', 0o644, 0, Some(target))?,
            Member::File { rel, path, size } => {
                entry(out, rel, b'0', mode_of(path), *size, None)?;
                let mut f = std::fs::File::open(path)?;
                let copied = io::copy(&mut f, out)?;
                if copied != *size {
                    return Err(io::Error::other(format!(
                        "{} changed size while it was archived ({size} → {copied} bytes)",
                        path.display()
                    )));
                }
                pad(out, *size)?;
            }
        }
    }
    out.write_all(&[0u8; 1024])
}

fn pad(out: &mut impl Write, size: u64) -> io::Result<()> {
    let rem = (size % 512) as usize;
    if rem != 0 {
        out.write_all(&vec![0u8; 512 - rem])?;
    }
    Ok(())
}

/// The largest size a ustar `size` field holds (11 octal digits).
const USTAR_MAX_SIZE: u64 = 0o777_7777_7777;

/// One member's header(s): a PAX `x` header first when the path, the link target or the
/// size does not fit ustar, then the ustar header itself.
fn entry(
    out: &mut impl Write,
    name: &[u8],
    flag: u8,
    mode: u32,
    size: u64,
    link: Option<&[u8]>,
) -> io::Result<()> {
    let split = split_name(name);
    let mut pax = Vec::new();
    if split.is_none() {
        pax.extend(pax_record("path", name));
    }
    if let Some(l) = link.filter(|l| l.len() > 100) {
        pax.extend(pax_record("linkpath", l));
    }
    if size > USTAR_MAX_SIZE {
        pax.extend(pax_record("size", size.to_string().as_bytes()));
    }
    if !pax.is_empty() {
        out.write_all(&header(
            b"././@PaxHeader",
            b"",
            b'x',
            0o644,
            pax.len() as u64,
            b"",
        ))?;
        out.write_all(&pax)?;
        pad(out, pax.len() as u64)?;
    }
    let (prefix, short) = split.unwrap_or((&[][..], &name[..name.len().min(100)]));
    let link = link.map_or(&[][..], |l| &l[..l.len().min(100)]);
    out.write_all(&header(
        short,
        prefix,
        flag,
        mode,
        size.min(USTAR_MAX_SIZE),
        link,
    ))
}

/// `name` as ustar's `(prefix, name)`: whole when it fits 100 bytes, else split at the
/// first `/` that leaves a prefix of at most 155 bytes and a name of at most 100.
fn split_name(name: &[u8]) -> Option<(&[u8], &[u8])> {
    if name.len() <= 100 {
        return Some((&[], name));
    }
    name.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'/')
        .map(|(i, _)| (&name[..i], &name[i + 1..]))
        .find(|(p, n)| p.len() <= 155 && !n.is_empty() && n.len() <= 100)
}

fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let body = key.len() + value.len() + 3;
    let mut len = body + 1;
    while len != body + len.to_string().len() {
        len = body + len.to_string().len();
    }
    let mut r = format!("{len} {key}=").into_bytes();
    r.extend_from_slice(value);
    r.push(b'\n');
    r
}

fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
}

fn header(name: &[u8], prefix: &[u8], flag: u8, mode: u32, size: u64, link: &[u8]) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    octal(&mut h[100..108], u64::from(mode));
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 0);
    h[156] = flag;
    h[157..157 + link.len()].copy_from_slice(link);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix);
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    let s = format!("{sum:06o}");
    h[148..154].copy_from_slice(s.as_bytes());
    h[154] = 0;
    h[155] = b' ';
    h
}
