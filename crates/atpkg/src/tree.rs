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
        let meta = std::fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
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
            let mode = if crate::platform::HAS_POSIX_MODES {
                crate::platform::permission_mode(&meta) & 0o7777
            } else {
                modes.get(&rel_bytes).copied().unwrap_or(0) & 0o7777
            };
            let fsha = file_sha256_with(&path, buf)?;
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
    let mut f = std::fs::File::open(path)?;
    loop {
        let n = f.read(&mut *buf)?;
        if n == 0 {
            return Ok(());
        }
        update(&buf[..n.min(buf.len())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn tmp(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-tree-{label}-"))
    }

    fn write(p: &Path, content: &[u8]) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn a_long_file_streams_to_the_one_shot_digest() {
        let d = tmp("vec");
        let f = d.join("v");
        let long: Vec<u8> = (0..200_003_u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        write(&f, &long);
        assert_eq!(
            file_sha512(&f).unwrap(),
            aterm_codec::hex::encode_lower(
                ring::digest::digest(&ring::digest::SHA512, &long).as_ref()
            )
        );
        assert_eq!(
            file_sha256(&f).unwrap(),
            aterm_codec::hex::encode_lower(&Sha256::digest(&long))
        );
        assert!(file_sha512(&d.join("absent")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// [`BulkSha256`] gives the scalar reference's digest (archive digests and tree roots
    /// are signed, cross-version contracts): FIPS 180-4's published vectors, then
    /// pseudo-random messages split in two at each awkward boundary (empty, one byte, the
    /// padding's 55/56, a block's 63/64/65, past a 64 KiB read buffer) and fed in runs of
    /// awkward chunk sizes.
    #[test]
    fn bulk_sha256_is_the_reference_digest() {
        let bulk = |parts: &[&[u8]]| {
            let mut h = BulkSha256::new();
            for p in parts {
                h.update(p);
            }
            h.finalize_hex()
        };
        let million_a = vec![b'a'; 1_000_000];
        let vectors: [(&[u8], &str); 5] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
            ),
            (
                &million_a,
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            ),
        ];
        for (msg, want) in vectors {
            assert_eq!(bulk(&[msg]), want);
            assert_eq!(aterm_codec::hex::encode_lower(&Sha256::digest(msg)), want);
        }

        let mut x = 0x9e37_79b9_7f4a_7c15_u64;
        let data: Vec<u8> = (0..(1 << 20) + 1)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[7]
            })
            .collect();
        let edges = [0, 1, 55, 56, 63, 64, 65, 64 * 1024 + 1, (1 << 20) + 1];
        for len in edges {
            let msg = &data[..len];
            let want = aterm_codec::hex::encode_lower(&Sha256::digest(msg));
            for split in edges.into_iter().filter(|&s| s <= len) {
                let (a, b) = msg.split_at(split);
                assert_eq!(bulk(&[a, b]), want, "len {len}, split at {split}");
            }
            for step in [55, 63, 64, 65] {
                let chunks: Vec<&[u8]> = msg.chunks(step).collect();
                assert_eq!(bulk(&chunks), want, "len {len}, {step}-byte chunks");
            }
        }
    }

    /// The cross-version byte contract pinned to a literal, computed independently from the
    /// documented format (`sort(<relpath> 0x00 <octal mode> 0x00 <hex sha256> 0x0A)`,
    /// concatenated, SHA-256): the recompute tests stay green if the primitive, framing or
    /// sort changes, and signed manifests embed roots from earlier releases.
    #[test]
    fn tree_root_matches_the_pinned_cross_version_vector() {
        const GOLDEN: &str = "e44608970d54f90c4d54ad7852b174304b86395b89f59b3fcc04a11ae22e38e0";
        let d = tmp("golden");
        write(&d.join("bin/ay"), b"binary");
        write(&d.join("data"), b"x");
        #[cfg(unix)]
        {
            std::fs::set_permissions(d.join("bin/ay"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            std::fs::set_permissions(d.join("data"), std::fs::Permissions::from_mode(0o644))
                .unwrap();
            assert_eq!(
                tree_root(&d).unwrap(),
                GOLDEN,
                "tree_root moved: every signed manifest in the wild disagrees with this build"
            );
        }
        let mut modes = DeclaredModes::new();
        modes.insert(b"bin/ay".to_vec(), 0o755);
        modes.insert(b"data".to_vec(), 0o644);
        let declared = tree_root_declared(&d, &modes).unwrap();
        assert_eq!(
            declared, GOLDEN,
            "the declared-mode walk must reproduce the root a Unix producer signed"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The relpath slot is `/`-separated on every platform, though `Path::join` spells the
    /// separator `\` on Windows.
    #[test]
    fn rel_line_bytes_spell_the_separator_as_a_slash() {
        let rel = Path::new("bin").join("sub").join("claude.exe");
        assert_eq!(rel_line_bytes(&rel), b"bin/sub/claude.exe".to_vec());
        assert_eq!(
            rel_line_bytes(Path::new("claude.exe")),
            b"claude.exe".to_vec()
        );
    }

    /// A signed one-file row (`bin/claude.exe`, mode `0755`) folds to its signed root from
    /// exactly that line, and the same content under a `\` separator and a `0` mode folds
    /// to the different digest a client with those spellings produced.
    #[test]
    fn the_measured_windows_claude_row_folds_from_the_declared_line() {
        const CONTENT: &str = "0e4195524b73eb77efbdf3e2b36de5322a29f0ca575dfd2d9b4f946b1d425469";
        let signed = root_of_entry_lines(vec![entry_line(b"bin/claude.exe", 0o755, CONTENT)]);
        assert_eq!(
            signed,
            "15c4346e4e80077255d03ad4590b17d834b2853c5b5e013cd7bb1fb656219165"
        );
        let former_windows_fold =
            root_of_entry_lines(vec![entry_line(b"bin\\claude.exe", 0, CONTENT)]);
        assert_eq!(
            former_windows_fold,
            "3a446f3033188e9ec3bdaa0868f30863bc7508d3986290fc803316fa7dcd18b5"
        );
        let d = tmp("claude-row");
        write(&d.join("bin/claude.exe"), b"stand-in");
        let mut modes = DeclaredModes::new();
        modes.insert(b"bin/claude.exe".to_vec(), 0o755);
        let expected = root_of_entry_lines(vec![entry_line(
            b"bin/claude.exe",
            0o755,
            &file_sha256(&d.join("bin/claude.exe")).unwrap(),
        )]);
        #[cfg(unix)]
        std::fs::set_permissions(
            d.join("bin/claude.exe"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(tree_root_declared(&d, &modes).unwrap(), expected);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tree_root_changes_on_any_mutation() {
        let d = tmp("mut");
        write(&d.join("bin/ay"), b"binary");
        write(&d.join("data"), b"x");
        let base = tree_root(&d).unwrap();

        write(&d.join("data"), b"y");
        let changed_content = tree_root(&d).unwrap();
        assert_ne!(base, changed_content, "content change must move the root");
        write(&d.join("data"), b"x");
        assert_eq!(tree_root(&d).unwrap(), base);

        #[cfg(unix)]
        {
            let exe = d.join("bin/ay");
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            let changed_mode = tree_root(&d).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
            let restored = tree_root(&d).unwrap();
            assert_ne!(changed_mode, restored, "mode change must move the root");
        }
        {
            let mut exec = DeclaredModes::new();
            exec.insert(b"bin/ay".to_vec(), 0o755);
            let mut plain = DeclaredModes::new();
            plain.insert(b"bin/ay".to_vec(), 0o644);
            let as_exec = tree_root_declared(&d, &exec).unwrap();
            let as_plain = tree_root_declared(&d, &plain).unwrap();
            if crate::platform::HAS_POSIX_MODES {
                assert_eq!(
                    as_exec, as_plain,
                    "a stored mode is read back, never declared"
                );
                assert_eq!(as_exec, tree_root(&d).unwrap());
            } else {
                assert_ne!(
                    as_exec, as_plain,
                    "a declared mode change must move the root"
                );
            }
        }

        let before_add = tree_root(&d).unwrap();
        write(&d.join("extra"), b"new");
        assert_ne!(
            before_add,
            tree_root(&d).unwrap(),
            "an added file must move the root"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A symlink is digested by its raw target bytes, never followed or skipped, and the
    /// digest moves exactly when the target string moves; a dangling link is a good line.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_digested_by_its_target_bytes_and_never_followed() {
        let d = tmp("link");
        write(&d.join("real"), b"x");
        std::os::unix::fs::symlink("real", d.join("link")).unwrap();
        let base = tree_root(&d).unwrap();
        assert_eq!(base, tree_root(&d).unwrap(), "deterministic");
        std::os::unix::fs::symlink("real", d.join("link2")).unwrap();
        let with_twin = tree_root(&d).unwrap();
        assert_ne!(base, with_twin, "an added link must move the root");
        std::fs::remove_file(d.join("link2")).unwrap();
        assert_eq!(tree_root(&d).unwrap(), base);
        std::fs::remove_file(d.join("link")).unwrap();
        std::os::unix::fs::symlink("nowhere", d.join("link")).unwrap();
        let dangling = tree_root(&d).unwrap();
        assert_ne!(base, dangling, "a changed target must move the root");
        std::fs::remove_file(d.join("link")).unwrap();
        std::os::unix::fs::symlink("elsewhere", d.join("link")).unwrap();
        assert_ne!(dangling, tree_root(&d).unwrap());
        write(&d.join("sub/inner"), b"inner");
        std::fs::remove_file(d.join("link")).unwrap();
        std::os::unix::fs::symlink("sub", d.join("link")).unwrap();
        let via_dir = tree_root(&d).unwrap();
        std::fs::remove_file(d.join("link")).unwrap();
        std::os::unix::fs::symlink("sub", d.join("link")).unwrap();
        assert_eq!(via_dir, tree_root(&d).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The symlink line pinned to a literal computed independently from the documented
    /// format, `sort(<relpath> 0x00 symlink 0x00 <hex sha256(target bytes)> 0x0A ...)`
    /// concatenated and hashed, over `real` (content `x`, mode 0644) and `link -> real`.
    #[cfg(unix)]
    #[test]
    fn symlink_tree_root_matches_the_pinned_cross_version_vector() {
        let d = tmp("golden-link");
        write(&d.join("real"), b"x");
        std::fs::set_permissions(d.join("real"), std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink("real", d.join("link")).unwrap();
        assert_eq!(
            tree_root(&d).unwrap(),
            "9b629b43d3775c6c84f676fd875ce601e5d9c3473b8e71b6470b12d48ee2d952",
            "the symlink line moved: every vendor manifest signed against it disagrees"
        );
        assert_eq!(
            symlink_line(b"link", &symlink_target_sha(b"real")),
            b"link\0symlink\0aa33996d60e89311b4d1a920dae03c6d7fa3ae1956c52662e273aad4683e577f\n"
                .to_vec()
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
