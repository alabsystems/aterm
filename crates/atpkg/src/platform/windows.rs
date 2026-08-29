// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Windows backend of [`crate::platform`]. Each function is the honest Windows
//! analogue of the Unix primitive (see the module docs on [`crate::platform`]): a
//! directory **junction** for the activation indirection, a `.cmd` batch wrapper for
//! bin shims, per-user `%LOCALAPPDATA%`-ACL privacy (no POSIX mode/owner bits),
//! `GetDiskFreeSpaceExW` for free space, and `spawn().wait()` + `exit` for exec.
//!
//! **This backend has NOT been exercised on a real Windows host** — it is written to
//! be correct-by-construction and cross-compiles clean for `x86_64-pc-windows-gnu`;
//! the pure `.cmd` formatting/parsing is unit-tested (on Unix) in [`crate::platform`].

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Appended to a tool name to form the concrete executable name. Applied ONLY by
/// [`crate::store::ToolName::exe_file`]: here the two suffixes name two DIFFERENT files, so
/// every hand-written append was a chance to build `bin/ay.cmd` when `bin\ay.exe` was meant.
pub const EXE_SUFFIX: &str = ".exe";
/// Appended to a tool name to form the concrete `bin/` shim filename (a batch wrapper).
/// Applied ONLY by [`crate::store::ToolName::shim_file`] and stripped ONLY by
/// [`crate::store::ToolName::from_shim_file`].
pub const SHIM_SUFFIX: &str = ".cmd";

/// The default install prefix: `%LOCALAPPDATA%\aterm\pkg` (per-user, ACL-private by
/// default), falling back to `%USERPROFILE%\AppData\Local\aterm\pkg` via `home` when
/// `%LOCALAPPDATA%` is unset.
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("aterm").join("pkg")
    } else {
        home.join("AppData").join("Local").join("aterm").join("pkg")
    }
}

/// No effective uid on Windows — privacy is the per-user profile ACL, not owner bits.
/// Returns the `0` sentinel (used only in a diagnostic message never reached on Windows,
/// since [`dir_meta_is_private`] is always `true`).
#[must_use]
pub fn our_uid() -> u32 {
    0
}

/// Best-effort private-dir predicate: `true`. POSIX owner/mode bits do not apply;
/// confidentiality rests on the per-user `%LOCALAPPDATA%` profile ACL.
#[must_use]
pub fn dir_meta_is_private(_meta: &Metadata) -> bool {
    true
}

/// Backup exclusion is a macOS convention (`com_apple_backup_excludeItem`); Windows
/// backup tooling has no equivalent per-directory opt-out to honour, so this is a
/// no-op that exists to keep `store::ensure_dir` one shape across platforms.
pub fn exclude_from_backup(_dir: &Path) {}

/// Shared-directory creation. On Windows POSIX modes do not apply, so this is just
/// `create_dir_all`; the per-directory ACL governs access, exactly as it does for
/// `ensure_private_dir`.
pub fn ensure_shared_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// System-owned predicate: always `false` on Windows. The Unix analogue proves a
/// root-owned chain from POSIX owner/mode bits, which have no meaning here — and a
/// best-effort `true` would be the wrong direction for a check whose entire purpose
/// is to be fail-closed. Returning `false` means a configured system prefix simply
/// falls back to the trusted default, exactly as any other failed chain check does.
#[must_use]
pub fn dir_meta_is_system(_meta: &Metadata) -> bool {
    false
}

/// Whether `meta` (from `symlink_metadata`) is a link-like indirection that must NOT be
/// trusted as a real directory in the fail-closed prefix chain check. On Windows this is
/// ANY reparse point — crucially including a directory **junction** (`mklink /J`, needs no
/// admin), which `FileType::is_symlink()` reports as `false` because it carries
/// `IO_REPARSE_TAG_MOUNT_POINT`, not `IO_REPARSE_TAG_SYMLINK`. Checking the
/// `FILE_ATTRIBUTE_REPARSE_POINT` bit (0x400) catches both, closing the CWE-379
/// junction-swap hole the Unix `is_symlink()` check closes with a symlink.
#[must_use]
pub fn is_reparse(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_type().is_symlink() || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// No-op: POSIX `0600` hardening has no analogue (per-user ACL).
pub fn harden_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// No-op: POSIX permission bits have no analogue on Windows.
pub fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Open `path` for a fresh (create+truncate) write. `mode` is ignored (no POSIX bits).
pub fn open_create_write(path: &Path, _mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// No permission bits on Windows — reports `0` (callers mask/treat it as not-applicable;
/// the tree-root hash is therefore self-consistent per-platform, not Unix-comparable).
#[must_use]
pub fn permission_mode(_meta: &Metadata) -> u32 {
    0
}

/// The encoded bytes of an `OsStr` (WTF-8), for the tree-root path hash.
#[must_use]
pub fn os_str_bytes(s: &OsStr) -> &[u8] {
    s.as_encoded_bytes()
}

// The one Win32 call the free-space query needs, declared dependency-free (std already
// links `kernel32`). Fills `*lpFreeBytesAvailableToCaller` with the bytes free to the
// (unprivileged) caller — the Windows analogue of `statvfs`'s `f_bavail`.
unsafe extern "system" {
    fn GetDiskFreeSpaceExW(
        lp_directory_name: *const u16,
        lp_free_bytes_available_to_caller: *mut u64,
        lp_total_number_of_bytes: *mut u64,
        lp_total_number_of_free_bytes: *mut u64,
    ) -> i32;
}

/// Free bytes on the volume holding `dir` (which must EXIST), or `None` on any error.
/// Fails **OPEN** (`None`), the same contract as the Unix `statvfs` path.
#[must_use]
pub fn volume_free_bytes(dir: &Path) -> Option<u64> {
    // A NUL-terminated wide (UTF-16) path is what the -W API expects.
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut free_to_caller: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call;
    // `&mut free_to_caller` is a valid writable out-param; the two total-size
    // out-params are optional and passed as NULL, which the API accepts.
    let rc = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc == 0 { None } else { Some(free_to_caller) }
}

/// `BY_HANDLE_FILE_INFORMATION`. Only the volume serial and the two file-index halves
/// are read; the rest is declared to get the layout right. `FILETIME` is two `u32`s.
#[repr(C)]
#[derive(Default)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: [u32; 2],
    last_access_time: [u32; 2],
    last_write_time: [u32; 2],
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

// The identity query, declared dependency-free like `GetDiskFreeSpaceExW` above.
// `std::os::windows::fs::MetadataExt` exposes these same two fields, but ONLY behind
// the unstable `windows_by_handle` feature — which a crate that must also build on
// stable cannot require. `GetFileInformationByHandle` is the stable route to the
// identical kernel data.
unsafe extern "system" {
    fn CreateFileW(
        lp_file_name: *const u16,
        dw_desired_access: u32,
        dw_share_mode: u32,
        lp_security_attributes: *mut core::ffi::c_void,
        dw_creation_disposition: u32,
        dw_flags_and_attributes: u32,
        h_template_file: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
    fn GetFileInformationByHandle(
        h_file: *mut core::ffi::c_void,
        lp_file_information: *mut ByHandleFileInformation,
    ) -> i32;
    fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
}

/// The OS-level identity of the directory at `dir`: `(volume serial, 64-bit file
/// index)` — the NTFS analogue of POSIX `(dev, ino)`, and the value that must not
/// change across an enumeration for that enumeration to be trustworthy.
///
/// Opened with `FILE_FLAG_OPEN_REPARSE_POINT` so the identity is the LINK's own, never
/// its target's — matching `symlink_metadata`, whose result the caller vets separately.
/// Fails **CLOSED**: `None` on any error, which a caller must treat as "identity
/// unknown" (i.e. a failure), never as "unchanged".
#[must_use]
pub fn directory_identity(dir: &Path) -> Option<(u32, u64)> {
    const FILE_SHARE_ALL: u32 = 0x0000_0007; // READ | WRITE | DELETE
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000; // required to open a DIRECTORY
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const INVALID_HANDLE_VALUE: isize = -1;

    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call; the
    // security-attributes and template-file params are optional and passed as NULL,
    // which the API accepts. Desired access is 0 — `FILE_READ_ATTRIBUTES` is implied,
    // and it is all the identity query needs.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_ALL,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info = ByHandleFileInformation::default();
    // SAFETY: `handle` is a live handle from the successful `CreateFileW` above, and
    // `info` is a valid, correctly-laid-out writable out-param.
    let rc = unsafe { GetFileInformationByHandle(handle, &mut info) };
    // SAFETY: `handle` is live and not closed anywhere else; closed exactly once.
    unsafe { CloseHandle(handle) };
    if rc == 0 {
        return None;
    }
    Some((
        info.volume_serial_number,
        (u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low),
    ))
}

/// Remove whatever indirection currently sits at `link` (a junction is a directory
/// reparse point → `remove_dir` unlinks just the junction, not its target's contents;
/// a stale plain file → `remove_file`). Both attempts are best-effort. `pub` so callers
/// (e.g. `ops::uninstall` dropping a channel `current` junction) can drop a link without
/// `remove_file`, which fails on a directory junction (`ERROR_ACCESS_DENIED`).
pub fn remove_link(link: &Path) {
    let _ = fs::remove_dir(link);
    let _ = fs::remove_file(link);
}

/// A copy of `p` with every `/` separator rewritten to `\`. Win32 path APIs accept
/// either separator, but `cmd` built-ins tokenize `/x` anywhere on the line as a
/// switch — so an `mklink` argument like `store/trust/671` (which `Path::join` with a
/// multi-component `&str` happily produces) fails as `Invalid switch - "trust"`.
/// Rewritten wide-char-wise (lossless for non-UTF-8 `OsStr` content).
fn backslashed(p: &Path) -> std::ffi::OsString {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = p
        .as_os_str()
        .encode_wide()
        .map(|c| {
            if c == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                c
            }
        })
        .collect();
    std::ffi::OsString::from_wide(&wide)
}

/// Point `link` at directory `target` via a **junction** (`mklink /J`, no admin required).
/// Not atomically swappable like a POSIX rename; any existing link is removed first, so
/// there is a brief window where `link` is absent (acceptable — activation runs under the
/// apply lock). Used for `channels/<ch>/current` and the sysroot/toolchain dir links.
/// Both paths are normalized to `\` separators first — `mklink` (unlike the Win32 API)
/// rejects `/`-separated paths, reading path segments as switches.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    remove_link(link);
    let out = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(backslashed(link))
        .arg(backslashed(target))
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        let mut msg = String::from("mklink /J failed: ");
        msg.push_str(String::from_utf8_lossy(&out.stderr).trim());
        Err(io::Error::other(msg))
    }
}

/// Atomically (best-effort) write `bytes` to `dest`: sibling temp + remove-dest + rename.
/// Windows `rename` does not replace an existing file, so `dest` is removed first (a brief
/// non-atomic window, documented — the state files this backs are per-user and serialized).
fn atomic_write(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp_name = String::from(".");
    if let Some(n) = dest.file_name().and_then(OsStr::to_str) {
        tmp_name.push_str(n);
    } else {
        tmp_name.push_str("shim");
    }
    tmp_name.push_str(".tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = parent.join(tmp_name);
    let _ = fs::remove_file(&tmp);
    fs::write(&tmp, bytes)?;
    let _ = fs::remove_file(dest);
    if let Err(e) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Install a bin shim at `shim` (a `.cmd`) forwarding to `target` (`…\<tool>.exe`),
/// setting `env` first (design S7; an empty `env` is the plain shim — the form without
/// an environment is [`super::install_shim_to`]).
/// Fail-closed: refuse a target that could break out of the `@"<target>" %*` quoting
/// (a `"`/`%`/CR/LF/NUL) rather than write an injectable batch wrapper — a managed
/// store path never contains these, so this only ever rejects a pathological path —
/// and, the same way, an env entry that could break out of `@set "NAME=VALUE"`
/// (already refused at manifest parse; this is the I/O site's own refusal).
pub fn install_shim_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    if !super::cmd_target_is_injection_safe(target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to shim an unsafe target path (quote/%/newline): {}",
                target.display()
            ),
        ));
    }
    if !super::cmd_env_is_injection_safe(env) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to shim an unsafe shim_env entry (quote/%/newline)",
        ));
    }
    atomic_write(shim, super::cmd_shim_content_env(target, env).as_bytes())
}

/// Install a **failing tombstone shim** at `shim` (a `.cmd`) that prints `message` to
/// stderr and exits 70 — the Windows analogue of the Unix `sh` tombstone.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    atomic_write(shim, super::cmd_tombstone_content(message).as_bytes())
}

/// Resolve the store/checkout target a `bin/<tool>.cmd` shim forwards to, or `None`.
/// Parses the batch wrapper's `@"<target>" %*` line — the Windows inverse of `read_link`.
/// A tombstone `.cmd` (no forward target) yields `None`.
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    super::read_cmd_shim_target(shim)
}

/// The environment the `.cmd` shim at `shim` sets before it forwards — its `@set` lines
/// ([`super::read_cmd_shim_env`]; NONE for a tombstone, a stub, or anything unreadable).
#[must_use]
pub fn shim_env_of(shim: &Path) -> crate::shim_env::ShimEnv {
    super::read_cmd_shim_env(shim)
}

/// Run `command` to completion, then `exit` with its code (Windows has no `execve`, so
/// this cannot replace the process image). Returns the error only if spawn/wait failed.
pub fn exec_or_run(command: &mut Command) -> io::Error {
    match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => e,
        },
        Err(e) => e,
    }
}
