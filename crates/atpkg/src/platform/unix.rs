// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Unix backend of [`crate::platform`]. Every function here is the crate's
//! ORIGINAL behavior moved verbatim — symlink activation, `chmod 0600`/mode setting,
//! `statvfs` free-space, `getuid`-based ownership predicates, and `execve` — so a
//! Unix build is byte-for-byte identical to before the platform abstraction existed.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_types::fs_restricted::dir_safe_for_private_write;

/// Appended to a tool name to form the concrete executable name. Empty on Unix.
pub const EXE_SUFFIX: &str = "";
/// Appended to a tool name to form the concrete `bin/` shim filename. Empty on Unix
/// (the shim is a bare symlink named `<tool>`).
pub const SHIM_SUFFIX: &str = "";

/// The default install prefix under `home`: `…/Library/Application Support/aterm/pkg`,
/// a sibling of the updater's `Updates` dir (so the two share the hardened support root).
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("aterm")
        .join("pkg")
}

/// Our effective uid.
#[must_use]
pub fn our_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Whether a directory's metadata says it is private-write-safe: owned by our uid and
/// not group/other-writable (the shared [`dir_safe_for_private_write`] predicate).
#[must_use]
pub fn dir_meta_is_private(meta: &Metadata) -> bool {
    dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode())
}

/// Whether `meta` (from `symlink_metadata`) is a link-like indirection that must NOT be
/// trusted as a real directory in the fail-closed prefix chain check. On Unix that is
/// exactly a symlink; the Windows backend also treats a directory **junction** (a reparse
/// point that reports `is_symlink() == false`) as disqualifying.
#[must_use]
pub fn is_reparse(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Remove whatever indirection sits at `link`. On Unix a `channels/<ch>/current` link is a
/// symlink, so `remove_file` unlinks it (never following into the target). Best-effort.
pub fn remove_link(link: &Path) {
    let _ = fs::remove_file(link);
}

/// Force a file to `0600` (owner-only). The Unix hardening for the durable
/// floor/pin/links/cache state files.
pub fn harden_file(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o600))
}

/// Set a file's permission bits to `mode`.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// Open `path` for a fresh (create+truncate) write with initial permission `mode`.
pub fn open_create_write(path: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)
}

/// A file's permission bits (`st_mode`), as read by the tree-root hash and doctor.
#[must_use]
pub fn permission_mode(meta: &Metadata) -> u32 {
    meta.permissions().mode()
}

/// The raw OS bytes of an `OsStr` (no lossy conversion), for the tree-root path hash.
#[must_use]
pub fn os_str_bytes(s: &OsStr) -> &[u8] {
    s.as_bytes()
}

/// Free bytes on the volume holding `dir` (which must EXIST), or `None` on any
/// `statvfs` failure. Uses `f_bavail` — blocks free to an UNPRIVILEGED user (correct:
/// atpkg never runs as root) — times the fragment size, saturating so a pathological
/// filesystem can never wrap to a bogus "fits".
#[must_use]
pub fn volume_free_bytes(dir: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, and `&mut st` is a
    // valid, writable out-param of the exact `statvfs` type the libc call expects.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let frsize = if st.f_frsize != 0 {
        st.f_frsize
    } else {
        st.f_bsize
    };
    Some((st.f_bavail as u64).saturating_mul(frsize as u64))
}

/// Atomically point `link` at `target`: create a sibling temp symlink and `rename(2)` it
/// over `link`. `rename` is atomic on POSIX, so the swap has no window where `link` is
/// missing or partially written — even if a previous `link` already existed. The
/// directory-indirection primitive behind `channels/<ch>/current` and the Kani dir links.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    // `Path::file_name` / `OsStr::to_str` go via `call1`: std's INLINED `unsafe`
    // (the `from_utf8_unchecked` fast path, the `OsStr` byte-slice casts) is
    // otherwise attributed to this function's spans as missing-SAFETY-comment
    // refutations under the strict Trust gate (see `lib.rs`). Same calls, same
    // receivers; behavior identical.
    let file_name = match crate::call1(std::path::Path::file_name, link) {
        Some(name) => crate::call1(std::ffi::OsStr::to_str, name),
        None => None,
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link has no file name"))?;
    // Manual rendering of the previous
    // `format!(".{file_name}.tmp-{}", std::process::id())` — byte-identical: the
    // `format!` expansion embeds `fmt::Arguments` construction (with inlined
    // `unsafe`) that the strict gate cannot lower and fails closed on.
    let mut tmp_name = String::from(".");
    tmp_name.push_str(file_name);
    tmp_name.push_str(".tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = link.with_file_name(tmp_name);
    // A leftover temp from a crashed run must not block us.
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    // Atomic replace. On failure, clean the temp so it can't accumulate.
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Install a bin shim at `shim` forwarding to `target`. On Unix a shim IS a symlink,
/// so this is exactly [`atomic_symlink`].
pub fn install_shim_to(shim: &Path, target: &Path) -> io::Result<()> {
    atomic_symlink(target, shim)
}

/// Wrap `s` in single quotes for safe embedding in a `/bin/sh` script, escaping any embedded
/// single quote as the POSIX `'\''` sequence. A shim name only ever reaches here after
/// `shim_allowed` (no `/`, no NUL, non-empty), but that gate does NOT forbid other shell
/// metacharacters, so the failing-shim body must never let a crafted `exposes` name break out
/// of the quoted string. Built by hand (no `format!`) for the strict Trust gate (see `lib.rs`).
fn sh_single_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Install a **failing tombstone shim** at `shim` — a tiny `sh` script that prints
/// `message` to stderr and exits 70 (`EX_SOFTWARE`). Written atomically (temp `0755` +
/// `rename(2)`), replacing whatever shim (symlink or file) was there.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    // The failing script. `printf '%s\n' <quoted>` keeps the message a fixed format with the
    // (quoted) tool-bearing text as a separate arg — no format-string or shell injection — and
    // exits 70 (EX_SOFTWARE), a clear nonzero. Built with `push_str` (no `format!`, Trust gate).
    let mut script = String::from("#!/bin/sh\nprintf '%s\\n' ");
    script.push_str(&sh_single_quote(message));
    script.push_str(" 1>&2\nexit 70\n");

    // Atomic install: write a sibling temp, make it executable, then `rename(2)` over `shim`.
    // `rename` is atomic on POSIX and replaces the destination regardless of its prior type
    // (symlink or regular file), so the live shim flips to the tombstone with no torn window.
    let file_name = match crate::call1(std::path::Path::file_name, shim) {
        Some(name) => crate::call1(std::ffi::OsStr::to_str, name),
        None => None,
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "shim has no file name"))?;
    let mut tmp_name = String::from(".");
    tmp_name.push_str(file_name);
    tmp_name.push_str(".tomb-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = shim.with_file_name(tmp_name);
    let _ = fs::remove_file(&tmp);
    crate::call2(std::fs::write, tmp.as_path(), script.as_bytes())?;
    fs::set_permissions(&tmp, Permissions::from_mode(0o755))?;
    if let Err(e) = fs::rename(&tmp, shim) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Resolve the store/checkout target a `bin/<tool>` shim points at, or `None` if there is
/// no shim. On Unix the shim is a symlink, so this is exactly its `read_link`. A tombstone
/// (a regular file, not a symlink) yields `None`.
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    fs::read_link(shim).ok()
}

/// Replace the current process image with `command` (`execve`); returns only on failure.
pub fn exec_or_run(command: &mut Command) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    // `exec` replaces this process and never returns on success; the returned value is
    // the error that PREVENTED the exec.
    command.exec()
}
