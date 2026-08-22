// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The apply-time re-verify anchor: [`tree_root`] (§4.2/§8).
//!
//! A `.tar.zst` bundle's compressed-asset `sha256` proves *download* integrity, but once
//! extracted the directory cannot be re-checked against it — and the signed manifest is
//! consumed before the (slow) extraction. So the signed per-build manifest also carries a
//! `tree_root`: a SHA-256 over the **sorted `(relpath \0 mode \0 content-sha256)` list of
//! every extracted file**. Re-computing it over the staged tree *under `apply.lock`,
//! immediately before the activation flip* closes the extract→activate TOCTOU window: a
//! file swapped after extraction (or an interrupted/partial extraction) changes the root
//! and the apply aborts.
//!
//! The hash is **content-integrity, not the signature root** (that stays `ring`, §8). It
//! streams each file, so a multi-GB sysroot is never buffered whole. The exact byte
//! format below is the contract the publish-side producer (Phase 6) must reproduce.

use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Compute the [`tree_root`](self) over the directory at `root`.
///
/// For every **regular file** under `root` (recursively) an entry is formed:
/// `<relpath-bytes> 0x00 <octal-perm-bits> 0x00 <lowercase-hex content sha256> 0x0A`,
/// where `relpath` is relative to `root` (raw OS bytes, `/`-separated) and the perm bits
/// are `mode & 0o7777`. The entries are **sorted by relpath bytes** (so the result is
/// independent of directory-walk order) and concatenated, and the SHA-256 of that stream
/// is returned as lowercase hex.
///
/// **Fail-closed on unexpected entry types.** The extracted tree is supposed to contain
/// only regular files and directories ([`crate::extract::vet_entry`] refused every
/// symlink/hardlink/exotic entry at stage time); encountering one here means tampering or
/// a bug, so this returns an error rather than silently skipping it.
pub fn tree_root(root: &Path) -> io::Result<String> {
    let mut entries: Vec<Vec<u8>> = Vec::new();
    // ONE read buffer for the whole walk, threaded down the recursion. A per-file
    // `[0u8; 64 * 1024]` local would be zero-initialized per call and LLVM cannot elide
    // it (the buffer is handed to an opaque `Read::read`), so the emitted body carries a
    // 16-page stack probe plus a 64 KiB `bzero` for EVERY file hashed — a rust sysroot
    // bundle has tens of thousands of files, so that is gigabyte-scale memset laid on top
    // of the real hashing. Heap `vec!`, not a boxed array literal: `Box::new([0u8; N])`
    // builds the array on the stack first and reintroduces exactly what this removes.
    let mut buf = vec![0u8; 64 * 1024];
    walk(root, root, &mut entries, &mut buf)?;
    Ok(root_of_entry_lines(entries))
}

/// Fold a set of canonical entry lines ([`entry_line`]) into the tree root: **sort**,
/// concatenate, SHA-256, lowercase hex.
///
/// Extracted from [`tree_root`] verbatim so the *other* producer of this digest — the
/// extraction-time accumulator in [`crate::extract`], which folds the bytes it already
/// has in a register instead of re-reading the payload from disk — cannot drift from
/// it. `tree_root` is a CROSS-VERSION byte contract (signed manifests embed roots
/// computed by earlier releases), so "two implementations that agree today" is not good
/// enough: there is one formatter and one fold, and both callers go through them.
///
/// The sort is over whole LINES, exactly as before. That is the same order as a sort by
/// relpath whenever the relpaths are distinct — a path byte is never `0x00`, and `\0` is
/// the lowest byte, so at the first position where two lines differ either the paths
/// already differ (same verdict) or one path ended and its `\0` compares below the
/// other's next path byte (again the same verdict as the shorter-prefix-first path
/// order). The accumulator relies on that equivalence; it still sorts, so the property
/// is belt-and-suspenders rather than load-bearing.
pub(crate) fn root_of_entry_lines(mut entries: Vec<Vec<u8>>) -> String {
    entries.sort();
    let mut h = Sha256::new();
    for e in &entries {
        h.update(e);
    }
    hex(&h.finalize())
}

/// Build ONE canonical entry line: `<relpath-bytes> 0x00 <octal-perm-bits> 0x00
/// <lowercase-hex content sha256> 0x0A` (see [`tree_root`] for the contract).
///
/// `mode` is the value [`crate::platform::permission_mode`] reports for the file,
/// masked to `0o7777` by the caller — NOT the mode the extractor asked for. The two are
/// the same on Unix (the extractor `set_mode`s and the filesystem stores it verbatim)
/// and both are `0` on Windows (no POSIX bits), but reading the mode back is what keeps
/// the extraction-time twin honest on any filesystem that would answer differently.
pub(crate) fn entry_line(rel_bytes: &[u8], mode: u32, content_sha_hex: &str) -> Vec<u8> {
    // Saturating spelling of the capacity hint (a no-op on every real
    // input: entry lines are a path + 64 hex chars + separators, nowhere
    // near `usize::MAX`), branch-dominated for the allocation-budget
    // recognizer. A capacity hint is behavior-neutral anyway — the `Vec`
    // grows as needed.
    let cap = rel_bytes
        .len()
        .saturating_add(content_sha_hex.len())
        .saturating_add(16);
    let mut line = if cap <= 4096 {
        Vec::with_capacity(cap)
    } else {
        Vec::with_capacity(4096)
    };
    line.extend_from_slice(rel_bytes);
    line.push(0);
    line.extend_from_slice(oct_mode(mode).as_bytes());
    line.push(0);
    line.extend_from_slice(content_sha_hex.as_bytes());
    line.push(b'\n');
    line
}

/// Recursively collect one canonical entry line per regular file under `dir`, hashing
/// each through the caller's single reusable `buf` (see [`tree_root`]).
fn walk(root: &Path, dir: &Path, out: &mut Vec<Vec<u8>>, buf: &mut [u8]) -> io::Result<()> {
    let mut children: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    // Deterministic recursion order (the final sort over relpaths makes this belt-and-
    // suspenders, but it keeps the walk itself reproducible).
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        // symlink_metadata: never follow a link (and a link is itself unexpected here).
        let meta = std::fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_dir() {
            walk(root, &path, out, buf)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "path escaped tree root")
            })?;
            let mode = crate::platform::permission_mode(&meta) & 0o7777;
            let fsha = file_sha256_with(&path, buf)?;
            // `platform::os_str_bytes` (the raw OS bytes; `OsStrExt::as_bytes` on Unix)
            // goes via `call1` (hoisted, used twice below): std's INLINED `unsafe` (the
            // `OsStr` byte-slice cast) is otherwise attributed to this function's spans
            // as missing-SAFETY-comment refutations under the strict Trust gate (see
            // `lib.rs`). Same call, same receiver; behavior identical.
            let rel_bytes = crate::call1(crate::platform::os_str_bytes, rel.as_os_str());
            out.push(entry_line(rel_bytes, mode, &fsha));
        } else {
            // symlink / device / fifo / socket — must not be in an extracted bundle.
            // Manual concat of the previous
            // `format!("unexpected non-file/dir entry in tree: {}", path.display())`
            // — byte-identical (`Path::display` renders exactly the lossy UTF-8
            // decode `to_string_lossy` produces): the `format!` expansion embeds
            // `fmt::Arguments` construction (with inlined `unsafe`) that the
            // strict Trust gate cannot lower and fails closed on.
            // `Path::to_string_lossy` goes via `call1` (see `lib.rs`).
            let mut msg = String::from("unexpected non-file/dir entry in tree: ");
            msg.push_str(&crate::call1(std::path::Path::to_string_lossy, &path));
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
        }
    }
    Ok(())
}

/// Render the masked permission bits in octal — byte-identical to the previous
/// `format!("{mode:o}")` for every masked input (minimal digits, no leading
/// zeros, so `0` renders as `"0"` and `0o644` as `"644"`): the `format!`
/// expansion embeds `fmt::Arguments` construction (with inlined `unsafe`) that
/// the strict Trust gate cannot lower and fails closed on. LOOP-FREE, digit by
/// constant shift (same idiom as `dec_u64` in `lib.rs`): the re-mask makes the
/// helper total (a no-op for the already-masked caller), each digit is `< 8` by
/// the `& 0x7`, and `wrapping_shr` by a constant `< 32` is a plain shift with
/// no panic obligations.
fn oct_mode(mode: u32) -> String {
    let mode = mode & 0o7777;
    let mut out = String::new();
    let mut started = false;
    macro_rules! emit_digit {
        ($sh:expr) => {
            let d = (mode.wrapping_shr($sh) & 0x7) as u8;
            if started || d != 0 {
                started = true;
                out.push(char::from(b'0'.wrapping_add(d)));
            }
        };
    }
    emit_digit!(9);
    emit_digit!(6);
    emit_digit!(3);
    // Ones digit is emitted unconditionally, so `0` renders as "0".
    let _ = started;
    out.push(char::from(b'0'.wrapping_add((mode & 0x7) as u8)));
    out
}

/// Streamed SHA-256 of a file's contents → lowercase hex (public producer helper: the
/// publish pipeline computes a manifest's `sha256` with the *same* hash the client checks).
pub fn sha256_file(path: &Path) -> io::Result<String> {
    file_sha256(path)
}

/// Streamed SHA-256 of a file's contents → lowercase hex. Never buffers the whole file.
/// Reused by the install path for the downloaded-asset integrity check (§9).
///
/// One-shot wrapper: allocates a single read buffer and delegates. The tree walk, which
/// hashes tens of thousands of files, calls [`file_sha256_with`] with ONE buffer instead
/// (see [`tree_root`]).
pub(crate) fn file_sha256(path: &Path) -> io::Result<String> {
    let mut buf = vec![0u8; 64 * 1024];
    file_sha256_with(path, &mut buf)
}

/// [`file_sha256`] against a caller-owned read buffer — the per-file body, verbatim, with
/// the buffer hoisted out of the hot per-file frame.
fn file_sha256_with(path: &Path, buf: &mut [u8]) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    loop {
        let n = f.read(&mut *buf)?;
        if n == 0 {
            break;
        }
        // The `Read` contract guarantees `n <= buf.len()`; the clamp is a no-op
        // on every conforming reader (`File` is) that hands the strict Trust
        // gate the dominating bound its slice proof needs, and `get` + full-
        // slice fallback restates it in a panic-free shape (same idiom as
        // `extract::write_capped`).
        let n = if n <= buf.len() { n } else { buf.len() };
        let chunk = match buf.get(..n) {
            Some(c) => c,
            None => &buf[..],
        };
        h.update(chunk);
    }
    Ok(hex(&h.finalize()))
}

/// Lowercase-hex encode a digest.
///
/// Byte-identical to the previous `format!("{b:02x}")` loop — rewritten
/// `format!`-free (the expansion embeds `fmt::Arguments` construction the
/// strict Trust gate cannot lower) with pure nibble arithmetic (no table
/// indexing: the gate's bitmask engine refuted the `< 16` bound it needed;
/// `wrapping_*` ops are total and carry no obligations at all). The capacity
/// hint is clamped behind a dominating comparison for the allocation-budget
/// recognizer; every real input is a digest (32 bytes -> 64 chars), so the
/// clamp never bites — and a capacity hint is behavior-neutral anyway.
pub(crate) fn hex(bytes: &[u8]) -> String {
    /// The lowercase hex digit for nibble `n` (`n < 16` at every call site).
    fn nibble(n: u8) -> char {
        if n < 10 {
            char::from(b'0'.wrapping_add(n))
        } else {
            char::from(b'a'.wrapping_add(n.wrapping_sub(10)))
        }
    }
    let cap = bytes.len().saturating_mul(2);
    let mut s = if cap <= 128 {
        String::with_capacity(cap)
    } else {
        String::with_capacity(128)
    };
    for b in bytes {
        s.push(nibble(*b >> 4));
        s.push(nibble(*b & 0x0f));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// `tree_root` is a cross-version byte contract (signed manifests embed roots
    /// computed by earlier releases), so the manual octal renderer must be
    /// byte-identical to the `format!("{mode:o}")` it replaced — exhaustively,
    /// over every maskable mode.
    #[test]
    fn oct_mode_matches_format_exhaustive() {
        for mode in 0u32..=0o7777 {
            assert_eq!(oct_mode(mode), format!("{mode:o}"), "mode = {mode:#o}");
        }
    }

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-tree-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, content: &[u8]) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    // file_sha256 matches the canonical SHA-256("abc") vector.
    #[test]
    fn file_sha256_known_vector() {
        let d = tmp("vec");
        let f = d.join("v");
        write(&f, b"abc");
        assert_eq!(
            file_sha256(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // tree_root is a 64-char lowercase hex digest and is DETERMINISTIC across recomputes.
    #[test]
    fn tree_root_is_deterministic() {
        let d = tmp("det");
        write(&d.join("bin/ay"), b"binary");
        write(&d.join("share/doc/readme"), b"hello");
        write(&d.join("lib/a.so"), b"\x00\x01\x02");
        let r1 = tree_root(&d).unwrap();
        let r2 = tree_root(&d).unwrap();
        assert_eq!(r1, r2);
        assert_eq!(r1.len(), 64);
        assert!(r1.bytes().all(|b| b.is_ascii_hexdigit()));
        let _ = std::fs::remove_dir_all(&d);
    }

    // Any change — content, mode, added/removed file — moves the root (TOCTOU detection).
    #[test]
    fn tree_root_changes_on_any_mutation() {
        let d = tmp("mut");
        write(&d.join("bin/ay"), b"binary");
        write(&d.join("data"), b"x");
        let base = tree_root(&d).unwrap();

        // Content change.
        write(&d.join("data"), b"y");
        let changed_content = tree_root(&d).unwrap();
        assert_ne!(base, changed_content, "content change must move the root");
        write(&d.join("data"), b"x"); // restore
        assert_eq!(tree_root(&d).unwrap(), base);

        // Mode change — chmod fixture, Unix-only.
        #[cfg(unix)]
        {
            let exe = d.join("bin/ay");
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            let changed_mode = tree_root(&d).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
            let restored = tree_root(&d).unwrap();
            assert_ne!(changed_mode, restored, "mode change must move the root");
        }

        // Added file.
        let before_add = tree_root(&d).unwrap();
        write(&d.join("extra"), b"new");
        assert_ne!(
            before_add,
            tree_root(&d).unwrap(),
            "an added file must move the root"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // A symlink in the tree (which vet_entry never extracts) is fail-closed, not skipped.
    #[cfg(unix)] // symlink fixture — Unix-only
    #[test]
    fn symlink_in_tree_is_an_error() {
        let d = tmp("link");
        write(&d.join("real"), b"x");
        std::os::unix::fs::symlink("real", d.join("link")).unwrap();
        assert!(
            tree_root(&d).is_err(),
            "an unexpected symlink in the tree must fail closed, not be silently skipped"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
