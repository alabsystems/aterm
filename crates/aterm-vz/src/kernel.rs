// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pinned kernel as the VM boots it (`docs/DESIGN-vz-guest-2026-10-23.md` §3, §4).
//!
//! `VZLinuxBootLoader` refuses Ubuntu's gzip `vmlinuz` and boots the same kernel
//! gunzipped (fact 34). The gunzip is aterm-codec's: the result is held to no more than
//! the recorded size, the arm64 `Image` magic, and the recorded sha256. The kernel and the
//! initrd are laid together under `kernel/<key>/`, whole or not at all, and never written
//! again once whole.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::{Error, io, pins};

/// Where the arm64 `Image` header carries its magic (Linux's
/// `Documentation/arch/arm64/booting.rst`: `magic`, at byte 56).
pub const MAGIC_AT: usize = 56;

/// The arm64 `Image` magic, `0x644d5241` little-endian.
pub const MAGIC: [u8; 4] = *b"ARMd";

/// The gunzipped kernel's name in the kernel's directory.
pub const IMAGE: &str = "Image";
/// The initrd's name in the kernel's directory.
pub const INITRD: &str = "initrd";

/// A file's size and sha256, which a laid file is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expect<'a> {
    /// The lowercase-hex sha256.
    pub sha256: &'a str,
    /// The size in bytes.
    pub bytes: u64,
}

/// The pins a kernel directory is laid from: the gunzipped `Image` and the initrd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want<'a> {
    /// What the gunzipped `vmlinuz` must be.
    pub image: Expect<'a>,
    /// What the initrd must be.
    pub initrd: Expect<'a>,
}

impl Want<'static> {
    /// The tree's pins ([`pins::IMAGE_SHA256`], [`pins::INITRD`]).
    pub const PINNED: Self = Self {
        image: Expect {
            sha256: pins::IMAGE_SHA256,
            bytes: pins::IMAGE_BYTES,
        },
        initrd: Expect {
            sha256: pins::INITRD.sha256,
            bytes: pins::INITRD.bytes,
        },
    };
}

impl Want<'_> {
    /// The kernel directory's name: both files' content addresses, so a pin bump lays a
    /// new directory and never rewrites one a booted VM read.
    #[must_use]
    pub fn key(&self) -> String {
        fn short(s: &str) -> &str {
            s.get(..16).unwrap_or(s)
        }
        format!("{}-{}", short(self.image.sha256), short(self.initrd.sha256))
    }
}

/// The kernel the VM boots: `VZLinuxBootLoader`'s `kernelURL` and `initialRamdiskURL`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kernel {
    /// The gunzipped arm64 `Image`.
    pub image: PathBuf,
    /// The initrd.
    pub initrd: PathBuf,
}

/// Gunzip `gz` into `out` in house, holding what comes out to `expect`: never more than
/// its bytes, the arm64 magic at [`MAGIC_AT`], and its sha256. The bytes written, flushed.
///
/// # Errors
/// [`Error::Input`]: the stream is not whole gzip (aterm-codec checks every member's CRC
/// and length), it inflates past `expect.bytes`, it is no arm64 `Image`, or its size or
/// sha256 is not `expect`'s. [`Error::Io`]: `out` refused a write or the flush (a full
/// volume). `out` then holds a partial write its caller discards.
pub fn gunzip_image(gz: impl Read, mut out: impl Write, expect: &Expect<'_>) -> Result<u64, Error> {
    let written_out =
        |e: std::io::Error| Error::Io(format!("could not write the gunzipped kernel: {e}"));
    let mut reader =
        aterm_codec::inflate::stream::GzipReader::new(gz).take(expect.bytes.saturating_add(1));
    let mut hash = aterm_digest::Sha256::new();
    let mut head = [0u8; MAGIC_AT + MAGIC.len()];
    let mut written: u64 = 0;
    let mut buf = [0u8; 32 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Input(format!("the kernel does not gunzip: {e}"))),
        };
        let chunk = &buf[..n];
        let at = usize::try_from(written).unwrap_or(usize::MAX);
        if at < head.len() {
            let take = (head.len() - at).min(n);
            head[at..at + take].copy_from_slice(&chunk[..take]);
        }
        written += n as u64;
        if written > expect.bytes {
            return Err(Error::Input(format!(
                "the kernel inflates past the pinned Image's {} bytes: refused",
                expect.bytes
            )));
        }
        hash.update(chunk);
        out.write_all(chunk).map_err(written_out)?;
    }
    if written < head.len() as u64 || head[MAGIC_AT..] != MAGIC {
        return Err(Error::Input(format!(
            "the gunzipped kernel is no arm64 Image: no `ARMd` magic at byte {MAGIC_AT}"
        )));
    }
    if written != expect.bytes {
        return Err(Error::Input(format!(
            "the gunzipped kernel is {written} bytes, the pinned Image {}",
            expect.bytes
        )));
    }
    let got = crate::hex(&hash.finalize());
    if got != expect.sha256 {
        return Err(Error::Input(format!(
            "the gunzipped kernel hashes to {got}, the pinned Image to {}",
            expect.sha256
        )));
    }
    out.flush().map_err(written_out)?;
    Ok(written)
}

/// Copy `from` into `out`, holding it to `expect`'s size and sha256, and flush `out`.
///
/// # Errors
/// [`Error::Input`]: `from` cannot be read, or is not `expect`'s. [`Error::Io`]: `out`
/// refused a write or the flush.
fn copy_checked(from: &Path, mut out: impl Write, expect: &Expect<'_>) -> Result<(), Error> {
    let file = crate::open_regular_nonblock(from)
        .map_err(|e| Error::Input(format!("could not open {}: {e}", from.display())))?;
    let mut reader = file.take(expect.bytes.saturating_add(1));
    let mut hash = aterm_digest::Sha256::new();
    let mut buf = [0u8; 32 * 1024];
    let mut copied: u64 = 0;
    let written_out =
        |e: std::io::Error| Error::Io(format!("could not copy {}: {e}", from.display()));
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(Error::Input(format!(
                    "could not read {}: {e}",
                    from.display()
                )));
            }
        };
        copied += n as u64;
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(written_out)?;
    }
    let got = crate::hex(&hash.finalize());
    if copied != expect.bytes || got != expect.sha256 {
        return Err(Error::Input(format!(
            "{} is {copied} bytes hashing to {got}; the pin is {} bytes, {}",
            from.display(),
            expect.bytes,
            expect.sha256
        )));
    }
    out.flush().map_err(written_out)
}

/// Whether `kernel` lies whole for `want`: both regular files (no link followed) of their
/// pins' sizes. A lay makes both durable before its one rename, so a file under the key
/// is never a crash's truncation; their sha256 was held when they were laid, and they are
/// 0444.
fn whole(kernel: &Kernel, want: &Want<'_>) -> bool {
    let real_parent = |path: &Path| {
        path.parent()
            .is_some_and(|dir| std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir()))
    };
    let sized = |path: &Path, bytes: u64| {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == bytes)
    };
    real_parent(&kernel.image)
        && (kernel.initrd.parent() == kernel.image.parent() || real_parent(&kernel.initrd))
        && sized(&kernel.image, want.image.bytes)
        && sized(&kernel.initrd, want.initrd.bytes)
}

/// Write `path` through `write`, then flush it to stable media.
fn write_durably(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), Error>,
) -> Result<(), Error> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| io("create", path, &e))?;
    let mut out = std::io::BufWriter::new(file);
    write(&mut out).map_err(|e| match e {
        Error::Io(why) => Error::Io(format!("{}: {why}", path.display())),
        other => other,
    })?;
    let file = out.into_inner().map_err(|e| io("write", path, e.error()))?;
    aterm_durable::durable(&file).map_err(|e| io("flush", path, &e))
}

/// The kernel laid under `dir` (`vz.noindex/kernel`) for `want`, laid first where it is not
/// there whole ([`whole`]): `vmlinuz` gunzipped to [`IMAGE`] and `initrd` copied to
/// [`INITRD`], each held to `want`, made durable and 0444 in a scratch directory renamed
/// into place. A key directory that is not whole is replaced. Every other kernel directory
/// (`<16 hex>-<16 hex>`) and unfinished scratch (`.partial-*`) under `dir` is removed, as the
/// framework reads the kernel only when a VM starts; nothing else under `dir` is touched.
///
/// # Errors
/// [`Error::Input`] when a file is missing or is not its pin's (nothing is laid),
/// [`Error::Io`] when the directory cannot be written (a full volume among them).
pub fn lay(dir: &Path, vmlinuz: &Path, initrd: &Path, want: &Want<'_>) -> Result<Kernel, Error> {
    if let Ok(meta) = std::fs::symlink_metadata(dir)
        && !meta.file_type().is_dir()
    {
        return Err(Error::Io(format!(
            "could not create {}: not a real directory",
            dir.display()
        )));
    }
    let key = want.key();
    let dest = dir.join(&key);
    let kernel = Kernel {
        image: dest.join(IMAGE),
        initrd: dest.join(INITRD),
    };
    if !whole(&kernel, want) {
        std::fs::create_dir_all(dir).map_err(|e| io("create", dir, &e))?;
        if !std::fs::symlink_metadata(dir)
            .map_err(|e| io("read", dir, &e))?
            .file_type()
            .is_dir()
        {
            return Err(Error::Io(format!(
                "could not create {}: not a real directory",
                dir.display()
            )));
        }
        let scratch = aterm_tempfile::Builder::new()
            .prefix(SCRATCH)
            .tempdir_in(dir)
            .map_err(|e| io("create a scratch in", dir, &e))?;
        let gz = crate::open_regular_nonblock(vmlinuz)
            .map_err(|e| Error::Input(format!("{}: {e}", vmlinuz.display())))?;
        let image = scratch.path().join(IMAGE);
        write_durably(&image, |out| {
            gunzip_image(std::io::BufReader::new(gz), out, &want.image)
                .map(|_| ())
                .map_err(|e| match e {
                    Error::Input(why) => Error::Input(format!("{}: {why}", vmlinuz.display())),
                    other => other,
                })
        })?;
        let copy = scratch.path().join(INITRD);
        write_durably(&copy, |out| copy_checked(initrd, out, &want.initrd))?;
        crate::read_only(&image)?;
        crate::read_only(&copy)?;
        aterm_durable::durable_dir(scratch.path()).map_err(|e| io("flush", scratch.path(), &e))?;
        if std::fs::symlink_metadata(&dest).is_ok() {
            remove(&dest).map_err(|e| io("replace the kernel that is not whole at", &dest, &e))?;
        }
        if let Err(e) = std::fs::rename(scratch.path(), &dest)
            && !whole(&kernel, want)
        {
            return Err(io("place", &dest, &e));
        }
        aterm_durable::durable_dir(dir).map_err(|e| io("flush", dir, &e))?;
    }
    reclaim(dir, &key);
    Ok(kernel)
}

/// The prefix of a lay's scratch directory under the kernel directory.
const SCRATCH: &str = ".partial-";

/// Whether `name` is a kernel directory's: `<16 hex>-<16 hex>` ([`Want::key`]).
fn is_key(name: &str) -> bool {
    name.split_once('-')
        .is_some_and(|(image, initrd)| crate::is_hex(image, 16) && crate::is_hex(initrd, 16))
}

/// Remove `path`, a directory with what it holds or any other entry, never following a link.
fn remove(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Remove every kernel directory and lay scratch under `dir` but `keep`.
fn reclaim(dir: &Path, keep: &str) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name != keep
            && (is_key(name) || name.starts_with(SCRATCH))
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;
