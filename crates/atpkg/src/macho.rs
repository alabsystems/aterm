// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Mach-O files of a staged tree, found by MAGIC rather than by name, so a platform
//! signature check reaches every binary the tree carries, whatever it is called.

use std::path::{Path, PathBuf};

use crate::install::StageError;

/// Thin Mach-O, as the first four bytes read big-endian: 32/64-bit, either byte order.
const THIN_MAGICS: [u32; 4] = [0xFEED_FACE, 0xFEED_FACF, 0xCEFA_EDFE, 0xCFFA_EDFE];

/// Universal (fat) headers, 32- and 64-bit arch tables, in either byte order.
const FAT_MAGICS: [u32; 4] = [0xCAFE_BABE, 0xCAFE_BABF, 0xBEBA_FECA, 0xBFBA_FECA];

/// Whether `head` (a file's first bytes) opens a Mach-O: a thin magic, or a universal
/// magic followed by a nonzero arch count. Fail closed: the kernel executes universal
/// files whose arch count overlaps a Java class file's version word, so a class file is
/// taken for a Mach-O too (and refused as unsigned); only a count of zero, which names no
/// slice to run, is not one.
#[must_use]
pub(crate) fn is_macho_header(head: &[u8]) -> bool {
    let Some(word) = head.get(..4) else {
        return false;
    };
    let magic = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    if THIN_MAGICS.contains(&magic) {
        return true;
    }
    FAT_MAGICS.contains(&magic) && head.get(4..8).is_some_and(|n| n != [0; 4])
}

/// Whether `head` opens an interpreter script (`#!`), which the kernel runs without any
/// code signature.
fn is_script_header(head: &[u8]) -> bool {
    head.starts_with(b"#!")
}

/// What the regular file at `path` opens as. Opened no-follow and proven regular on the
/// handle, so a FIFO or a link swapped in cannot block or redirect the read.
fn sniff(path: &Path) -> std::io::Result<Sniffed> {
    let head = crate::metadata_io::read_head(path, 8)?;
    Ok(if is_macho_header(&head) {
        Sniffed::MachO
    } else if is_script_header(&head) {
        Sniffed::Script
    } else {
        Sniffed::Other
    })
}

enum Sniffed {
    MachO,
    Script,
    Other,
}

/// `path` relative to `root` for a message, or whole when it is not under it.
fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Every regular Mach-O file under `root`, sorted, walking without following a symlink.
///
/// Only a regular file is checked in place, so the shapes that would let code escape the
/// check are refused ([`StageError::SignerRefused`]): an interpreter script (`#!`), which
/// runs unsigned; a symlink that resolves to a Mach-O — the check would verify its target,
/// not whatever the link names when it runs; and any entry that is neither a directory, a
/// regular file nor a symlink, which no staging lane lays and whose contents cannot be
/// read without the risk of blocking.
///
/// # Errors
/// [`StageError::SignerRefused`] for any of those shapes; [`StageError::Io`] when the tree
/// cannot be read.
pub(crate) fn find_macho(root: &Path) -> Result<Vec<PathBuf>, StageError> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(StageError::Io)? {
            let entry = entry.map_err(StageError::Io)?;
            let path = entry.path();
            let kind = entry.file_type().map_err(StageError::Io)?;
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                match sniff(&path).map_err(StageError::Io)? {
                    Sniffed::MachO => found.push(path),
                    Sniffed::Script => {
                        let mut m = shown(root, &path);
                        m.push_str(" is an interpreter script, which runs unsigned");
                        return Err(StageError::SignerRefused(m));
                    }
                    Sniffed::Other => {}
                }
            } else if kind.is_symlink() {
                // A link that resolves nowhere (dangling, a loop) runs nothing.
                let Ok(target) = std::fs::canonicalize(&path) else {
                    continue;
                };
                let target_is_file =
                    std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_file());
                if target_is_file
                    && matches!(sniff(&target).map_err(StageError::Io)?, Sniffed::MachO)
                {
                    let mut m = shown(root, &path);
                    m.push_str(" is a symlink to a Mach-O; only a regular file is verified");
                    return Err(StageError::SignerRefused(m));
                }
            } else {
                let mut m = shown(root, &path);
                m.push_str(" is neither a regular file, a directory nor a symlink");
                return Err(StageError::SignerRefused(m));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Whether `head` opens this platform's native executable format: Mach-O on macOS, ELF
/// on every other unix. Elsewhere no file carries an execute bit to keep.
fn is_native_executable(head: &[u8]) -> bool {
    if cfg!(target_os = "macos") {
        is_macho_header(head)
    } else {
        head.starts_with(b"\x7fELF")
    }
}

/// Every regular file under `root` whose mode carries an execute bit, sorted, walking
/// without following a symlink.
///
/// # Errors
/// [`StageError::Io`] when the tree cannot be read.
pub(crate) fn executable_files(root: &Path) -> Result<Vec<PathBuf>, StageError> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(StageError::Io)? {
            let entry = entry.map_err(StageError::Io)?;
            let kind = entry.file_type().map_err(StageError::Io)?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let meta = entry.metadata().map_err(StageError::Io)?;
                if crate::platform::permission_mode(&meta) & 0o111 != 0 {
                    found.push(entry.path());
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Clear every execute bit on a regular file under `root` that is not this platform's
/// native executable ([`is_native_executable`]): a vendor tree's data (codex 0.156.0 ships
/// `codex-resources/voice/runtime.json` at 0755) must not be runnable through `/bin/sh`.
/// Returns whether any mode changed.
///
/// # Errors
/// [`StageError::Io`] when the tree cannot be read or a mode cannot be set.
pub(crate) fn demote_foreign_executables(root: &Path) -> Result<bool, StageError> {
    let mut changed = false;
    for path in executable_files(root)? {
        let head = crate::metadata_io::read_head(&path, 8).map_err(StageError::Io)?;
        if is_native_executable(&head) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path).map_err(StageError::Io)?;
        let mode = crate::platform::permission_mode(&meta) & 0o7777;
        crate::platform::set_mode(&path, mode & !0o111).map_err(StageError::Io)?;
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-macho-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn with_tail(head: &[u8]) -> Vec<u8> {
        let mut bytes = head.to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        bytes
    }

    /// `magic` (big-endian) then `nfat` in the given byte order.
    fn fat_header(magic: u32, nfat: u32, big_endian: bool) -> Vec<u8> {
        let mut h = magic.to_be_bytes().to_vec();
        if big_endian {
            h.extend_from_slice(&nfat.to_be_bytes());
        } else {
            h.extend_from_slice(&nfat.to_le_bytes());
        }
        h
    }

    /// Every thin magic in either byte order is a Mach-O, and so is a universal header of
    /// either width and either order with ANY nonzero arch count — 45 and 200 among them,
    /// counts the kernel executes (measured: a 45-arch wrapper around a signed arm64 binary
    /// runs), and the range a Java class file's version word falls in.
    #[test]
    fn thin_and_fat_headers_are_recognised_at_any_arch_count() {
        for magic in THIN_MAGICS {
            assert!(is_macho_header(&magic.to_be_bytes()), "{magic:#x}");
        }
        for magic in FAT_MAGICS {
            for nfat in [1u32, 2, 44, 45, 52, 200, 818, u32::MAX] {
                for big_endian in [true, false] {
                    assert!(
                        is_macho_header(&fat_header(magic, nfat, big_endian)),
                        "{magic:#x} nfat={nfat} big_endian={big_endian}"
                    );
                }
            }
        }
        let java8 = [0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x34];
        assert!(
            is_macho_header(&java8),
            "a class file is checked, not trusted"
        );
    }

    /// A universal header counting no arch (it names no slice to run), a truncated
    /// header, and ordinary files are not Mach-O.
    #[test]
    fn empty_fat_headers_and_other_files_are_not() {
        for magic in FAT_MAGICS {
            assert!(!is_macho_header(&fat_header(magic, 0, true)), "{magic:#x}");
        }
        assert!(!is_macho_header(&[0xCA, 0xFE, 0xBA, 0xBE]));
        assert!(!is_macho_header(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 1]));
        assert!(!is_macho_header(&[0xFE, 0xED]));
        assert!(!is_macho_header(b"\x7fELF\x02\x01\x01\x00"));
        assert!(!is_macho_header(b"#!/bin/sh\n"));
        assert!(!is_macho_header(b""));
    }

    /// The walk finds thin and fat Mach-Os at any depth by content — whatever their
    /// names, and whatever their arch count — and skips everything else; the list is
    /// sorted.
    #[test]
    fn the_walk_finds_thin_and_fat_files_by_content() {
        let d = tmp("walk");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::create_dir_all(d.join("libexec/deep")).unwrap();
        std::fs::write(d.join("bin/tool"), with_tail(&0xCFFA_EDFEu32.to_be_bytes())).unwrap();
        let fat = fat_header(0xCAFE_BABE, 2, true);
        std::fs::write(d.join("libexec/deep/helper.txt"), with_tail(&fat)).unwrap();
        let fat45 = fat_header(0xCAFE_BABE, 45, true);
        std::fs::write(d.join("libexec/rg"), with_tail(&fat45)).unwrap();
        let fat200 = fat_header(0xCAFE_BABE, 200, true);
        std::fs::write(d.join("libexec/deep/zsh"), with_tail(&fat200)).unwrap();
        std::fs::write(d.join("README"), b"not code").unwrap();
        std::fs::write(d.join("NOTICE.md"), b"# Notices\n").unwrap();
        std::fs::write(d.join("bin/elf"), with_tail(b"\x7fELF")).unwrap();
        std::fs::write(d.join("empty"), b"").unwrap();
        assert_eq!(
            find_macho(&d).unwrap(),
            vec![
                d.join("bin/tool"),
                d.join("libexec/deep/helper.txt"),
                d.join("libexec/deep/zsh"),
                d.join("libexec/rg"),
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A tree with no Mach-O answers an empty list, not an error — requiring one is the
    /// gate's decision, not the walk's.
    #[test]
    fn a_tree_without_macho_is_an_empty_list() {
        let d = tmp("none");
        std::fs::write(d.join("runtime.json"), b"{\"a\": 1}\n").unwrap();
        assert_eq!(find_macho(&d).unwrap(), Vec::<PathBuf>::new());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An interpreter script anywhere in the tree is refused by name: the kernel runs it
    /// with no signature to check, beside Mach-Os that all verify.
    #[test]
    fn an_interpreter_script_is_refused() {
        let d = tmp("script");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::create_dir_all(d.join("codex-path")).unwrap();
        std::fs::write(
            d.join("bin/codex"),
            with_tail(&0xCFFA_EDFEu32.to_be_bytes()),
        )
        .unwrap();
        std::fs::write(d.join("codex-path/rg"), b"#!/bin/sh\nexec evil\n").unwrap();
        match find_macho(&d) {
            Err(StageError::SignerRefused(m)) => {
                let mut want = Path::new("codex-path").join("rg").display().to_string();
                want.push_str(" is an interpreter script, which runs unsigned");
                assert_eq!(m, want);
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A symlink resolving to a Mach-O is refused by name — inside the tree or out of it —
    /// while a symlink to anything else, or to nothing, is left alone.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_macho_is_refused_and_other_links_are_not() {
        let d = tmp("links");
        let outside = tmp("links-outside");
        std::fs::write(
            outside.join("real"),
            with_tail(&0xFEED_FACFu32.to_be_bytes()),
        )
        .unwrap();
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("notes"), b"text").unwrap();
        std::os::unix::fs::symlink("../notes", d.join("bin/notes")).unwrap();
        std::os::unix::fs::symlink("../missing", d.join("bin/dangling")).unwrap();
        assert_eq!(find_macho(&d).unwrap(), Vec::<PathBuf>::new());

        std::os::unix::fs::symlink(outside.join("real"), d.join("bin/claude")).unwrap();
        match find_macho(&d) {
            Err(StageError::SignerRefused(m)) => {
                assert!(m.starts_with("bin/claude is a symlink to a Mach-O"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_file(d.join("bin/claude")).unwrap();

        std::fs::write(d.join("real"), with_tail(&0xFEED_FACFu32.to_be_bytes())).unwrap();
        std::os::unix::fs::symlink("../real", d.join("bin/claude")).unwrap();
        assert!(matches!(find_macho(&d), Err(StageError::SignerRefused(_))));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A FIFO is refused without being opened — reading one would block the stage.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_being_read() {
        let d = tmp("fifo");
        let fifo = d.join("claude");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        match find_macho(&d) {
            Err(StageError::SignerRefused(m)) => {
                assert!(m.contains("neither a regular file"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
