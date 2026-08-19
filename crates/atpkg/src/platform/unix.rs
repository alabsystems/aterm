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
/// Applied ONLY by [`crate::store::ToolName::exe_file`] — that both suffixes are `""` here is
/// precisely why appending either by hand went unnoticed until the Windows backend landed.
pub const EXE_SUFFIX: &str = "";
/// Appended to a tool name to form the concrete `bin/` shim filename. Empty on Unix
/// (the shim is a bare symlink named `<tool>`). Applied ONLY by
/// [`crate::store::ToolName::shim_file`] and stripped ONLY by
/// [`crate::store::ToolName::from_shim_file`].
pub const SHIM_SUFFIX: &str = "";

/// The default install prefix under `home`: `…/Library/Application Support/aterm/pkg`,
/// a sibling of the updater's `Updates` dir (so the two share the hardened support root).
///
/// MIRRORED (deliberately, not depended on) by
/// `aterm-spec::verify::unix_store_bin_dir` — the verification tier's
/// discovery probes `<prefix>/bin` without dragging this crate into every
/// conformance consumer. Moving this prefix means updating that mirror too.
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

/// Mark `dir` as excluded from Time Machine and every backup tool that honours the
/// convention, by setting the `com.apple.metadata:com_apple_backup_excludeItem`
/// extended attribute.
///
/// The store is multiple GB of extracted toolchain that is re-downloadable and
/// signature-verifiable from the signed index; backing it up copies bytes the machine
/// can always reconstruct, and re-copies them on every update pass. It cannot simply
/// live in `~/Library/Caches` instead — a purge there would silently break an
/// installed toolchain, and the verified lane needs a stable path — so the exclusion
/// is set explicitly.
///
/// Best-effort and deliberately infallible: a volume or filesystem that refuses
/// extended attributes (a network home, an exFAT prefix) costs the user backup space
/// and nothing else. Never let a storage courtesy fail an install.
///
/// The value is the documented `bplist00` byte sequence for a `true` boolean, written
/// verbatim rather than via a plist encoder so this stays a dependency-free `setxattr`.
pub fn exclude_from_backup(dir: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    const ATTR: &[u8] = b"com.apple.metadata:com_apple_backup_excludeItem\0";
    // CFBoolean true, as a binary plist: header, the `true` marker (0x09), trailer.
    const TRUE_BPLIST: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, // "bplist00"
        0x09, // kCFBinaryPlistMarkerTrue
        0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x09,
    ];
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return; // an interior NUL cannot name a real path
    };
    // SAFETY: `path` and `ATTR` are NUL-terminated C strings that outlive the call, the
    // value pointer/length describe a `'static` slice, and the return value is ignored
    // because failure is explicitly acceptable here.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            ATTR.as_ptr().cast::<libc::c_char>(),
            TRUE_BPLIST.as_ptr().cast::<libc::c_void>(),
            TRUE_BPLIST.len(),
            0,
            0,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Other Unixes have no Time Machine convention to honour.
        let _ = (path, ATTR, TRUE_BPLIST);
    }
}

/// The system-prefix counterpart of `ensure_private_dir`'s `0700`. A root-owned store
/// exists precisely so every user can execute out of it, so it must be readable and
/// traversable by all — while staying writable only by root, which is what the prefix
/// chain check enforces and what Trust's launcher predicate requires
/// (`mode & 0o022 == 0`). `0755` satisfies both; `0700` satisfies both checks too and
/// then denies every non-root user at exec time.
///
/// Fails closed on a symlink at the target, mirroring `ensure_private_dir`: a
/// pre-created link must never capture our writes.
pub fn ensure_shared_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{}: store directory is a symlink; refusing", dir.display()),
        ));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Whether a directory's metadata says it is SYSTEM-owned: owned by root (uid 0) and
/// not group/other-writable.
///
/// The second trusted chain shape, and the one a verified toolchain requires. The
/// `$HOME` chain check answers "no attacker-writable ancestor" by proving every
/// component is ours; a root-owned chain answers the same question at least as
/// strongly — nothing below root can swap a component. This shape is for a genuinely
/// shared multi-user store.
///
/// CORRECTION 2026-08-18: this used to say a root-owned chain additionally carried
/// "PATHNAME EXECUTION AUTHORITY, which a user-owned path cannot", and that Trust's
/// verified launcher rejects a user-owned toolchain outright. Not true of current
/// Trust: its default `CallerOwned` mode admits a component owned by root OR by the
/// invoking identity (`process_authority.rs` — and it REFUSES to authenticate at all
/// when targo runs as root). A user-owned 0700 prefix proves fine.
///
/// Deliberately NOT "root-owned OR ours": admitting our own uid here would let the
/// system prefix fall back to exactly the property the verified lane refuses.
#[must_use]
pub fn dir_meta_is_system(meta: &Metadata) -> bool {
    meta.uid() == 0 && meta.mode() & 0o022 == 0
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
/// directory-indirection primitive behind `channels/<ch>/current` and the sysroot dir links.
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
    // An EXEC STUB, not a symlink. See `platform::sh_shim_content` for why: Trust's
    // `targo` refuses to authenticate when its own `current_exe` is a symlink or a
    // non-canonical path, so a symlinked shim made the product's headline tool fail
    // on 100% of successful installs. `exec` hands the process image to the real
    // binary at its real path, which also keeps its sysroot siblings resolvable.
    //
    // Written temp+rename so the swap stays atomic exactly as `atomic_symlink` was:
    // a shim on the user's PATH is never briefly absent or half-written.
    let body = super::sh_shim_content(target);
    let tmp = shim.with_extension("atpkg-new");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = open_create_write(&tmp, 0o755)?;
        use std::io::Write as _;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, shim)
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
    // Parses the exec stub rather than reading a link. Every store answer is built on
    // this — `active_builds`, `prune_stale_shims`, gc's retention — so it must return
    // exactly what `read_link` used to. A TOMBSTONE (a failing notice script with no
    // `exec` line) yields `None`, mirroring `read_link`'s `Err` for a non-symlink.
    //
    // A symlink left by an older atpkg still resolves, so an in-place upgrade keeps
    // working until the next activation rewrites the shim as a stub.
    if let Ok(target) = fs::read_link(shim) {
        return Some(target);
    }
    let content = crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES).ok()?;
    super::parse_sh_shim_target(&content)
}

/// Replace the current process image with `command` (`execve`); returns only on failure.
pub fn exec_or_run(command: &mut Command) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    // `exec` replaces this process and never returns on success; the returned value is
    // the error that PREVENTED the exec.
    command.exec()
}
