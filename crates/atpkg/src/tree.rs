// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The apply-time re-verify anchor: [`tree_root`] (§4.2/§8).

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::Path;

use aterm_digest::Sha256;

use crate::manifest::DigestAlg;

/// The permission bits a stage declared for each regular file it laid, keyed by the file's line bytes ([`rel_line_bytes`]): what the on-disk walk folds into the mode slot on a filesystem with no permission bits to read back.
pub type DeclaredModes = BTreeMap<Vec<u8>, u32>;

/// The line bytes of a path relative to the tree root: its raw OS bytes with the separator spelled `/`, the one producer of the relpath slot, shared by the on-disk walk and the extraction-time fold ([`crate::extract::rel_bytes_under`]).
#[inline]
#[must_use]
pub(crate) fn rel_line_bytes(rel: &Path) -> Vec<u8> {
    let raw = crate::platform::os_str_bytes(rel.as_os_str());
    #[cfg(windows)]
    {
        raw.iter()
            .map(|&b| if b == b'\\' { b'/' } else { b })
            .collect()
    }
    #[cfg(not(windows))]
    {
        raw.to_vec()
    }
}

/// Compute the [`tree_root`](self) over the directory at `root`.
#[inline]
pub fn tree_root(root: &Path) -> io::Result<String> {
    tree_root_declared(root, &DeclaredModes::new())
}

/// [`tree_root`], with the stage's [`DeclaredModes`] to fold on a filesystem that has no permission bits of its own.
/// # Errors
/// The walk's I/O errors, and the fail-closed refusal of a non-file, non-dir, non-link entry.
pub fn tree_root_declared(root: &Path, modes: &DeclaredModes) -> io::Result<String> {
    let mut entries: Vec<Vec<u8>> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    walk(root, root, modes, &mut entries, &mut buf)?;
    Ok(root_of_entry_lines(entries))
}

/// Fold a set of canonical entry lines ([`entry_line`]) into the tree root: sort, concatenate, SHA-256, lowercase hex.
pub(crate) fn root_of_entry_lines(mut entries: Vec<Vec<u8>>) -> String {
    entries.sort();
    let mut h = Sha256::new();
    for e in &entries {
        h.update(e);
    }
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// Build one canonical entry line: `<relpath-bytes> 0x00 <octal-perm-bits> 0x00 <lowercase-hex content sha256> 0x0A` (see [`tree_root`]).
#[inline]
pub(crate) fn entry_line(rel_bytes: &[u8], mode: u32, content_sha_hex: &str) -> Vec<u8> {
    framed_line(rel_bytes, oct_mode(mode).as_bytes(), content_sha_hex)
}

/// The mode-slot tag of a symlink's entry line.
pub(crate) const SYMLINK_TAG: &str = "symlink";

/// Lowercase-hex SHA-256 of a symlink's raw target bytes — the digest slot of its line.
#[inline]
pub(crate) fn symlink_target_sha(target_bytes: &[u8]) -> String {
    sha256_bytes(target_bytes)
}

/// Build one canonical entry line for a symlink: `<relpath-bytes> 0x00 symlink 0x00 <lowercase-hex sha256 of the raw target bytes> 0x0A`.
#[inline]
pub(crate) fn symlink_line(rel_bytes: &[u8], target_sha_hex: &str) -> Vec<u8> {
    framed_line(rel_bytes, SYMLINK_TAG.as_bytes(), target_sha_hex)
}

/// The one line framer: `<rel> 0x00 <mode-slot> 0x00 <digest> 0x0A`; the mode slot is the
/// only thing a symlink line fills differently.
fn framed_line(rel_bytes: &[u8], mode_slot: &[u8], digest_hex: &str) -> Vec<u8> {
    let cap = rel_bytes
        .len()
        .saturating_add(digest_hex.len())
        .saturating_add(16);
    let mut line = if cap <= 4096 {
        Vec::with_capacity(cap)
    } else {
        Vec::with_capacity(4096)
    };
    line.extend_from_slice(rel_bytes);
    line.push(0);
    line.extend_from_slice(mode_slot);
    line.push(0);
    line.extend_from_slice(digest_hex.as_bytes());
    line.push(b'\n');
    line
}

/// Recursively collect one canonical entry line per regular file and symlink under `dir`,
/// hashing each file through the caller's single reusable `buf`; `modes` is the
/// declared-mode record [`tree_root_declared`] folds where the inode stores no bits.
fn walk(
    root: &Path,
    dir: &Path,
    modes: &DeclaredModes,
    out: &mut Vec<Vec<u8>>,
    buf: &mut [u8],
) -> io::Result<()> {
    let mut children: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let ft = child.file_type()?;
        if ft.is_symlink() {
            let rel = path.strip_prefix(root).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "path escaped tree root")
            })?;
            let target = std::fs::read_link(&path)?;
            let rel_bytes = rel_line_bytes(rel);
            let target_bytes = crate::platform::os_str_bytes(target.as_os_str());
            out.push(symlink_line(&rel_bytes, &symlink_target_sha(target_bytes)));
        } else if ft.is_dir() {
            walk(root, &path, modes, out, buf)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "path escaped tree root")
            })?;
            let rel_bytes = rel_line_bytes(rel);
            let (fsha, meta) = file_sha256_and_meta(&path, buf)?;
            let mode = if crate::platform::HAS_POSIX_MODES {
                crate::platform::permission_mode(&meta) & 0o7777
            } else {
                modes.get(&rel_bytes).copied().unwrap_or(0) & 0o7777
            };
            // Descriptor fstat from file_sha256_and_meta avoids a separate lstat per file.
            out.push(entry_line(&rel_bytes, mode, &fsha));
        } else {
            // device / fifo / socket — must not be in an extracted bundle.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected non-file/dir entry in tree: {}", path.display()),
            ));
        }
    }
    Ok(())
}

/// The relpath slot ([`rel_line_bytes`]) of every line [`tree_root`] folds under `root`, sorted: each regular file and symlink, recursively, from the directory listings alone (no file opened, no link followed).
/// # Errors
/// The listing's I/O errors.
pub(crate) fn line_names(root: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    collect_line_names(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

/// The recursion of [`line_names`].
fn collect_line_names(root: &Path, dir: &Path, out: &mut Vec<Vec<u8>>) -> io::Result<()> {
    for child in std::fs::read_dir(dir)? {
        let child = child?;
        let path = child.path();
        if child.file_type()?.is_dir() {
            collect_line_names(root, &path, out)?;
        } else {
            let rel = path.strip_prefix(root).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "path escaped tree root")
            })?;
            out.push(rel_line_bytes(rel));
        }
    }
    Ok(())
}

/// Render the masked permission bits in octal: minimal digits, no leading zeros, so `0` renders as `"0"` and `0o644` as `"644"`.
#[inline]
pub(crate) fn oct_mode(mode: u32) -> String {
    format!("{:o}", mode & 0o7777)
}

/// SHA-256 of bytes in memory → lowercase hex: [`sha256_file`]'s digest of a file holding exactly these bytes.
#[inline]
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// Streamed SHA-256 of a file's contents → lowercase hex (public producer helper: the publish pipeline computes a manifest's `sha256` with the *same* hash the client checks).
#[inline]
pub fn sha256_file(path: &Path) -> io::Result<String> {
    file_sha256(path)
}

/// Streamed SHA-256 of a file's contents → lowercase hex; never buffers the whole file.
pub(crate) fn file_sha256(path: &Path) -> io::Result<String> {
    let mut buf = vec![0u8; 64 * 1024];
    file_sha256_with(path, &mut buf)
}

/// [`file_sha256`] against a caller-owned read buffer — the per-file body, with the
/// buffer hoisted out of the hot per-file frame.
fn file_sha256_with(path: &Path, buf: &mut [u8]) -> io::Result<String> {
    let mut h = BulkSha256::new();
    read_chunks(path, buf, |chunk| h.update(chunk))?;
    Ok(h.finalize_hex())
}

/// Streaming SHA-256 for bulk content: the downloaded archive's signed digest ([`file_sha256`], which `install::check_signed_digest` gates on) and the content slot of each extracted file's [`entry_line`] (the fold in [`crate::extract`] and the on-disk [`walk`]), so every byte an install lays passes through it twice.
pub(crate) struct BulkSha256(ring::digest::Context);

impl BulkSha256 {
    #[inline]
    pub(crate) fn new() -> Self {
        Self(ring::digest::Context::new(&ring::digest::SHA256))
    }

    #[inline]
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The digest as lowercase hex.
    #[inline]
    pub(crate) fn finalize_hex(self) -> String {
        aterm_codec::hex::encode_lower(self.0.finish().as_ref())
    }
}

/// Streamed SHA-512 of a file's contents → lowercase hex: the payload digest of a vendor that signs only SHA-512 ([`DigestAlg::Sha512`]).
pub(crate) fn file_sha512(path: &Path) -> io::Result<String> {
    let mut h = ring::digest::Context::new(&ring::digest::SHA512);
    let mut buf = vec![0u8; 64 * 1024];
    read_chunks(path, &mut buf, |chunk| h.update(chunk))?;
    Ok(aterm_codec::hex::encode_lower(h.finish().as_ref()))
}

/// `alg`'s streamed digest of a file's contents → lowercase hex.
pub(crate) fn file_digest(path: &Path, alg: DigestAlg) -> io::Result<String> {
    match alg {
        DigestAlg::Sha256 => file_sha256(path),
        DigestAlg::Sha512 => file_sha512(path),
    }
}

/// Feed `path`'s contents to `update`, one `buf` at a time.
fn read_chunks(path: &Path, buf: &mut [u8], mut update: impl FnMut(&[u8])) -> io::Result<()> {
    let mut f = crate::metadata_io::open_regular(path)?;
    loop {
        let n = f.read(&mut *buf)?;
        if n == 0 {
            return Ok(());
        }
        update(&buf[..n.min(buf.len())]);
    }
}

fn file_sha256_and_meta(path: &Path, buf: &mut [u8]) -> io::Result<(String, std::fs::Metadata)> {
    let (mut f, meta) = crate::metadata_io::open_regular_with_meta(path)?;
    let mut h = BulkSha256::new();
    loop {
        let n = f.read(&mut *buf)?;
        if n == 0 {
            return Ok((h.finalize_hex(), meta));
        }
        h.update(&buf[..n.min(buf.len())]);
    }
}

pub(crate) fn sha256_opened(mut f: std::fs::File) -> io::Result<String> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut h = BulkSha256::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finalize_hex());
        }
        h.update(&buf[..n]);
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
