// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Windows backend of [`crate::platform`] (its module docs list the analogues): a
//! directory **junction** for the activation indirection, a `.cmd` batch wrapper for
//! bin shims and the `agents/` twin (behind the resume-proof frame
//! [`super::CMD_FRAME_HEAD`]), per-user `%LOCALAPPDATA%`-ACL privacy,
//! `GetDiskFreeSpaceExW` for free space, and `spawn().wait()` + `exit` for exec, behind
//! a console control handler that leaves Ctrl-C to the child ([`add_ctrl_handler`]).
//!
//! This backend has not been exercised on a real Windows host. `mod windows` is
//! `#[cfg(windows)]`, so every line below is type-checked on all three Windows cells
//! `crates/aterm-libc` admits, and the pure `.cmd` formatting/parsing is unit-tested on Unix in
//! [`crate::platform`] — but no shim, junction or `GetDiskFreeSpaceExW` call here has ever run.
//!
//! A Windows triple absent from `aterm-libc`'s cell list stops the build rather than falling
//! back to an empty cell.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Whether the calling user can write into `dir`. Best-effort `true` on Windows,
/// matching [`dir_meta_is_private`]: writability rests on the per-user ACL, which this
/// layer does not evaluate. The Unix backend answers this with `access(2)`.
#[must_use]
pub fn dir_writable_by_caller(_dir: &Path) -> bool {
    true
}

/// Appended to a tool name to form the concrete executable name. Applied ONLY by
/// [`crate::store::ToolName::exe_file`]: here the two suffixes name two DIFFERENT files
/// (`bin/ay.cmd` vs `bin\ay.exe`).
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

// The account-home reader's calls, declared dependency-free like every Win32 call in this
// file: `OpenProcessToken` is advapi32's and `GetUserProfileDirectoryW` userenv's, so each
// names its import library; `GetCurrentProcess` is kernel32's (std links it), and the
// token is closed with the `CloseHandle` declared below.
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(
        process_handle: *mut core::ffi::c_void,
        desired_access: u32,
        token_handle: *mut *mut core::ffi::c_void,
    ) -> i32;
}
#[link(name = "userenv")]
unsafe extern "system" {
    fn GetUserProfileDirectoryW(
        token: *mut core::ffi::c_void,
        profile_dir: *mut u16,
        size: *mut u32,
    ) -> i32;
}

/// This ACCOUNT's home directory, as the OS knows it — the profile directory of this
/// process's token (`GetUserProfileDirectoryW`, which reads the account's
/// `ProfileList\<SID>\ProfileImagePath`) — which is NOT `%USERPROFILE%`, exactly as the
/// Unix twin's `getpwuid` home is not `$HOME`: a process given a scratch `USERPROFILE`
/// (a lab instance, a test) still answers the real profile, so the callers that ask
/// "is this the account's own home?" (the disk watch's owner rule, the headless package
/// lane's scratch-home gate) can tell the two apart.
///
/// `None` when the token or its profile cannot be read; the caller treats that as
/// "cannot prove this is the real home". The `[machine]` settings are macOS-only and
/// answer not-applicable before this is asked.
#[must_use]
pub fn account_home() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    const TOKEN_QUERY: u32 = 0x0008;
    /// A profile path longer than the wide APIs' own bound is not a profile.
    const MAX_UNITS: u32 = 32_768;
    let mut token: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` takes nothing and returns this process's pseudo-handle,
    // which needs no closing; `&mut token` is a valid out-param the call fills with a
    // token handle on success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        || token.is_null()
    {
        return None;
    }
    // Asked once with no buffer for the length (in UTF-16 units, the NUL included), then
    // once more into a buffer of that length.
    let mut units: u32 = 0;
    // SAFETY: `token` is the live token opened above; a NULL buffer with a zero size is
    // the documented way to ask for the size, which is written into `units`.
    unsafe { GetUserProfileDirectoryW(token, std::ptr::null_mut(), &mut units) };
    let read = (1..=MAX_UNITS).contains(&units).then(|| {
        let mut buf = vec![0u16; units as usize];
        // SAFETY: `buf` holds `units` writable UTF-16 units and outlives the call, which
        // writes at most that many (the NUL included) and updates `units`.
        let ok = unsafe { GetUserProfileDirectoryW(token, buf.as_mut_ptr(), &mut units) };
        (ok != 0).then_some(buf)
    });
    // SAFETY: `token` is live and not closed anywhere else; closed exactly once.
    unsafe { CloseHandle(token) };
    let buf = read.flatten()?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let home = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len]));
    home.is_absolute().then_some(home)
}

/// No effective uid on Windows — privacy is the per-user profile ACL, not owner bits.
/// Returns the `0` sentinel (used only in a diagnostic message never reached on Windows,
/// since [`dir_meta_is_private`] is always `true`).
#[must_use]
pub fn our_uid() -> u32 {
    0
}

/// No process groups on Windows: the caller's `Child::kill` is the whole of it.
pub fn kill_process_group(_leader: u32) {}

// The start-time reader's calls, declared dependency-free like the file queries below (std
// links `kernel32`); the handle is closed with their `CloseHandle`. A `FILETIME` is two
// `u32`s, low half first, as in [`ByHandleFileInformation`].
unsafe extern "system" {
    fn OpenProcess(
        dw_desired_access: u32,
        b_inherit_handle: i32,
        dw_process_id: u32,
    ) -> *mut core::ffi::c_void;
    fn GetExitCodeProcess(h_process: *mut core::ffi::c_void, lp_exit_code: *mut u32) -> i32;
    fn GetProcessTimes(
        h_process: *mut core::ffi::c_void,
        lp_creation_time: *mut [u32; 2],
        lp_exit_time: *mut [u32; 2],
        lp_kernel_time: *mut [u32; 2],
        lp_user_time: *mut [u32; 2],
    ) -> i32;
}

/// The kernel's start-time stamp for `pid` ([`crate::platform::Birth`]), used with the pid
/// to detect reuse, as the Unix twin's is: `GetProcessTimes`'s creation time, in
/// 100-nanosecond ticks since 1601, read through a `PROCESS_QUERY_LIMITED_INFORMATION`
/// handle (the least access that reads it, which Windows grants on most other accounts'
/// processes too). [`Birth::Gone`](crate::platform::Birth::Gone) for pid 0, for a pid
/// `OpenProcess` names nothing under (`ERROR_INVALID_PARAMETER`), and for a process that
/// has exited but whose object a handle still keeps — the zombie's analogue
/// (`GetExitCodeProcess` no longer `STILL_ACTIVE`; one that exited with code 259 reads as
/// running, and is still the process born then);
/// [`Birth::Unreadable`](crate::platform::Birth::Unreadable) when the kernel would not say
/// (`ERROR_ACCESS_DENIED`: a protected or system process) or a query failed.
#[must_use]
pub fn process_birth(pid: u32) -> crate::platform::Birth {
    use crate::platform::Birth;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return Birth::Gone;
    }
    // SAFETY: three integers in; a process handle, or NULL with the reason in the thread's
    // last error, out.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // Read at once, before anything else can move the last error.
        return if io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            Birth::Gone
        } else {
            Birth::Unreadable
        };
    }
    let mut code: u32 = 0;
    let (mut created, mut exited, mut kernel, mut user) =
        ([0u32; 2], [0u32; 2], [0u32; 2], [0u32; 2]);
    // SAFETY: `handle` is the live handle opened above, with the access both queries need;
    // every out-param is a valid, writable value of the type the API fills.
    let (coded, timed) = unsafe {
        (
            GetExitCodeProcess(handle, &mut code) != 0,
            GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0,
        )
    };
    // SAFETY: `handle` is live and not closed anywhere else; closed exactly once.
    unsafe { CloseHandle(handle) };
    match (coded, timed) {
        (true, _) if code != STILL_ACTIVE => Birth::Gone,
        (true, true) => Birth::At((u64::from(created[1]) << 32) | u64::from(created[0])),
        _ => Birth::Unreadable,
    }
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

/// Universal Control is a macOS feature; there is nothing to read here.
#[must_use]
pub fn universal_control_state() -> [crate::machine::Read<bool>; 2] {
    [crate::machine::Read::Absent, crate::machine::Read::Absent]
}

/// Universal Control is a macOS feature; nothing is written and nothing changed.
#[must_use]
pub fn universal_control_disable() -> bool {
    false
}

/// Rosetta 2 is a macOS install: Windows has no runtime to find
/// ([`crate::machine::ROSETTA_APPLIES`] keeps this from being asked).
pub fn rosetta_present(_runtime: &Path) -> Result<bool, String> {
    Ok(false)
}

/// Rosetta 2 is a macOS install; nothing runs here.
pub fn rosetta_install(_within: std::time::Duration) -> Result<(), String> {
    Err("Rosetta 2 is a macOS install".to_string())
}

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

/// No-op: the handle-based twin of [`set_mode`], for the same reason.
pub fn set_mode_on(_f: &File, _mode: u32) -> io::Result<()> {
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

/// Create `path` for writing, failing [`io::ErrorKind::AlreadyExists`] when the NAME
/// already resolves to anything (`CREATE_NEW`, answered by NTFS's own case-insensitive
/// name equality). `mode` is ignored (no POSIX bits).
pub fn open_create_new_write(path: &Path, _mode: u32) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Open `path` the way [`sync_file_contents`] needs it: WITH write access.
/// `FlushFileBuffers` is documented to require `GENERIC_WRITE` on the handle, and it
/// refuses a read-only one with `ERROR_ACCESS_DENIED` (5), which failed every stage in
/// `sync_tree`. Nothing is created or truncated: `write(true)` alone on an existing file.
pub fn open_for_sync(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).open(path)
}

/// Push ONE open file's contents to the filesystem — the Windows analogue of the Unix
/// `fsync(2)`: `FlushFileBuffers`, which is what std's `sync_data` calls here. Used by
/// [`crate::store::sync_tree`] to make a staged tree durable before the renames that
/// publish it, and by the readiness marker's write. The handle must carry write access
/// ([`open_for_sync`]); a read-only one answers `Access is denied`.
pub fn sync_file_contents(f: &File) -> io::Result<()> {
    aterm_durable::durable_data(f)
}

/// No permission bits on Windows — reports `0` (callers mask/treat it as not-applicable).
/// The tree-root hash does NOT fold this: a signed `tree_root` is folded from the modes
/// the stage declared, and a client folding `0` could never reproduce one
/// (`crate::tree`'s module docs) — see [`folded_mode`] and [`HAS_POSIX_MODES`].
#[must_use]
pub fn permission_mode(_meta: &Metadata) -> u32 {
    0
}

/// Whether this platform's filesystems store POSIX permission bits an `fstat` reads
/// back. Windows stores none, so the tree-root walk folds the stage's declared-mode
/// record ([`crate::tree::tree_root_declared`]) and the extraction-time fold records
/// the mode it was asked to apply ([`folded_mode`]).
pub const HAS_POSIX_MODES: bool = false;

/// The mode slot the extraction-time fold records for a file just written with
/// `requested` bits: the request itself, masked to `0o7777`. There is no stored mode to
/// read back here — `set_mode_on` is a no-op and `meta` reports `0` — and the signed
/// root was folded by a producer that applied exactly these bits (`0755`/`0644` after
/// `safe_mode`, `0755` for a raw binary), so the declared mode is the only value that
/// can reproduce it. The content digest and the relpath still describe the bytes laid
/// down; only the slot no filesystem here can store is taken from the request.
#[must_use]
pub fn folded_mode(meta: &Metadata, requested: u32) -> u32 {
    let _ = meta;
    requested & 0o7777
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
/// apply lock). Used for `store/<program>/current` and the sysroot/toolchain dir links.
/// Both paths are normalized to `\` separators first — `mklink` (unlike the Win32 API)
/// rejects `/`-separated paths, reading path segments as switches.
///
/// `cmd.exe` is a console image: built through `aterm_uds::spawnfd::command`, it shares this
/// process's console (a pass the window starts has a windowless one), or gets a
/// windowless one of its own when there is none, where a bare `cmd` would open a
/// visible console window per activated link.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    remove_link(link);
    let out = aterm_uds::spawnfd::command("cmd")
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
///
/// A `.cmd` shim this replaces may be EXECUTING: `cmd.exe` re-reads a batch file at a
/// remembered byte offset after every line, so the shim text itself makes a mid-run
/// re-lay safe — every line that runs a program ends the batch on that line
/// ([`super::CMD_FORWARD_TAIL`]), and every file starts with the frame
/// ([`super::CMD_FRAME_HEAD`]) a legacy file resumes into harmlessly.
fn atomic_write(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::lay::write_in_process(&crate::lay::Executable::new(dest, bytes))
}

/// The two injection refusals every `.cmd` shim renderer makes, fail-closed: a target
/// that could break out of the `@"<target>" %* & @exit /b` quoting (a `"`/`%`/CR/LF/NUL)
/// rather than write an injectable batch wrapper — a managed store path never contains
/// these, so this only ever rejects a pathological path — and, the same way, an env entry
/// that could break out of `@set "NAME=VALUE"` (already refused at manifest parse; this is
/// the I/O site's own refusal).
fn refuse_injectable(target: &Path, env: &crate::shim_env::ShimEnv) -> io::Result<()> {
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
    Ok(())
}

/// Install a bin shim at `shim` (a `.cmd`) forwarding to `target` (`…\<tool>.exe`),
/// setting `env` first (design S7; an empty `env` is the plain shim — the form without
/// an environment is [`super::install_shim_to`]): [`shim_executable_to_env`]'s body,
/// its refusals ([`refuse_injectable`]) and all, written through [`atomic_write`].
pub fn install_shim_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    let body = shim_executable_to_env(shim, target, env)?.body;
    atomic_write(shim, &body)
}

/// The shim [`install_shim_to_env`] lays, RENDERED but not written — the same two
/// injection refusals, the `.cmd` body — for the callers that render a whole pass of shims
/// before they lay it ([`crate::lay::write_in_process`]).
pub fn shim_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<crate::lay::Executable> {
    refuse_injectable(target, env)?;
    Ok(crate::lay::Executable::new(
        shim,
        super::cmd_shim_content_env(target, env),
    ))
}

/// The `agents/` twin on Windows: the `.cmd` shim with `prelude` — the caller's
/// [`super::twin_prelude`], empty today, so the twin is the framed shim under the twin's
/// name — ahead of its `@set` lines and forward line ([`super::cmd_shim_content_twin`]).
/// The same two injection refusals as [`shim_executable_to_env`]. RENDERED but not
/// written. Unverified on a Windows host, like everything here.
pub fn twin_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<crate::lay::Executable> {
    refuse_injectable(target, env)?;
    Ok(crate::lay::Executable::new(
        shim,
        super::cmd_shim_content_twin(target, env, prelude),
    ))
}

/// Lay the `.cmd` twin ([`twin_executable_to_env`]): the same body, written through
/// the same temp + remove + rename as every `.cmd` shim.
pub fn install_twin_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<()> {
    let body = twin_executable_to_env(shim, target, env, prelude)?.body;
    atomic_write(shim, &body)
}

/// Install a **failing tombstone shim** at `shim` (a `.cmd`) that prints `message` to
/// stderr and exits 70 — the Windows analogue of the Unix `sh` tombstone.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    atomic_write(shim, super::cmd_tombstone_content(message).as_bytes())
}

/// Resolve the store/checkout target a `bin/<tool>.cmd` shim forwards to, or `None`.
/// Parses the batch wrapper's `@"<target>" %* & @exit /b` line (or a legacy shim's
/// `@"<target>" %*` line). A tombstone `.cmd` (no forward target) yields `None`.
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    super::read_cmd_shim_target(shim)
}

/// [`resolve_shim`] for a reader that must tell "no shim" from "could not read": the
/// program the `.cmd` at `shim` forwards to (`Ok(Some)`), `Ok(None)` for a file that is
/// gone or holds no forward line (a tombstone, a stub, any other batch file, bytes that
/// are not UTF-8), and `Err` when it is there but could not be read through the same
/// bounded, reparse-refusing handle (over [`super::MAX_SHIM_BYTES`], a reparse point, a
/// sharing refusal, changed mid-read). The disk watch counts a shim naming a program
/// inside a build directory as an installed link into it, and one it could not read
/// keeps every candidate.
///
/// # Errors
///
/// The read's, but `NotFound`.
pub fn read_shim_target(shim: &Path) -> io::Result<Option<PathBuf>> {
    match crate::metadata_io::read_bounded_regular(shim, super::MAX_SHIM_BYTES) {
        Ok(bytes) => Ok(std::str::from_utf8(&bytes)
            .ok()
            .and_then(super::parse_cmd_shim_target)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The environment the `.cmd` shim at `shim` sets before it forwards — its `@set` lines
/// ([`super::read_cmd_shim_env`]; NONE for a tombstone, a stub, or anything unreadable).
#[must_use]
pub fn shim_env_of(shim: &Path) -> crate::shim_env::ShimEnv {
    super::read_cmd_shim_env(shim)
}

/// A console control handler: `ctrl_type` is one of the `CTRL_*_EVENT` values; `TRUE`
/// (non-zero) means "handled, call no further handler", `FALSE` passes the event on to
/// the next handler and finally to the default one, which ends the process.
pub type CtrlHandler = unsafe extern "system" fn(ctrl_type: u32) -> i32;

/// `CTRL_C_EVENT`: the user pressed Ctrl-C in the console (or `GenerateConsoleCtrlEvent`).
pub const CTRL_C_EVENT: u32 = 0;
/// `CTRL_BREAK_EVENT`: Ctrl-Break, delivered to every process of the console's group.
pub const CTRL_BREAK_EVENT: u32 = 1;

// The console control seam, declared dependency-free like every Win32 call in this file
// (std links `kernel32`). A `NULL` routine with `add = TRUE` sets the process's
// ignore-Ctrl-C FLAG — which CHILDREN INHERIT, so a `claude` spawned under it would
// never see its own Ctrl-C — which is why every caller here installs a REAL routine:
// handler routines are per-process and are not inherited.
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler_routine: Option<CtrlHandler>, add: i32) -> i32;
}

/// Install `handler` at the front of this process's console control handler list — it is
/// called (on its own thread) for every Ctrl-C / Ctrl-Break / close event for the rest of
/// the process. `false` when the call failed; the caller then has the default behaviour
/// (the process ends on Ctrl-C), never a wrong claim. The Windows analogue of
/// `signal(SIGINT, …)`, armed by [`exec_or_run`] for its child's lifetime.
pub fn add_ctrl_handler(handler: CtrlHandler) -> bool {
    // SAFETY: `handler` is a plain `extern "system"` function that lives for the whole
    // program; kernel32 keeps only its address. No memory of ours is handed over.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

/// The wrapper's own Ctrl-C handler while a child owns the console: `TRUE` for Ctrl-C and
/// Ctrl-Break — this process stays alive to collect the child's exit code, the child
/// (which received the same event from the console) decides what the key means —
/// `FALSE` for a console close, logoff or shutdown, which end every process of the
/// console anyway and must not be swallowed.
unsafe extern "system" fn swallow_ctrl_c(ctrl_type: u32) -> i32 {
    i32::from(matches!(ctrl_type, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

/// Run `command` to completion, then `exit` with its code (Windows has no `execve`, so
/// this cannot replace the process image). Returns the error only if spawn/wait failed.
/// Stdio is inherited (std's default), so the child owns the terminal exactly as an
/// `exec`'d image would; the code is the child's REAL one (`exit /b <n>` from a `.cmd`
/// included), `1` only when the child died with no code.
///
/// **Ctrl-C belongs to the child**: the console delivers `CTRL_C_EVENT` to EVERY process
/// attached to it, and a process with no handler ends on it. Before the spawn this
/// wrapper installs [`swallow_ctrl_c`] — the shape rustup's proxies and cargo's runners
/// use — so a Ctrl-C typed inside an agent (which `claude` treats as "stop this turn")
/// does not kill the wrapper, lose the exit code and orphan the agent. Installed for the
/// rest of this process's life. NOT the `NULL`-routine ignore flag, which children
/// inherit.
///
/// A `.cmd` program (the `bin/<program>.cmd` shim `cli::cmd_landing` runs through here)
/// goes through std's own `cmd.exe /d /c` lane with batch-safe quoting; std may refuse
/// arguments it cannot quote safely. `cmd.exe` may ask `Terminate batch job (Y/N)?` after
/// a Ctrl-C — once per batch level, so up to TWICE on a legacy twin's landing hand-over
/// (`agents\<program>.cmd`, then `bin\<program>.cmd`). Unverified on a Windows host.
///
/// Every caller builds `command` through `aterm_uds::spawnfd::command`, or raw (the
/// reviewed pass-throughs) with `aterm_uds::spawn::set_flags`, both `Console::Quiet`, which
/// adds nothing from a console: typed at a prompt the tool shares that console, its
/// Ctrl-C and its code page, exactly as a bare child did.
pub fn exec_or_run(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    // Best effort: when the install fails the wrapper keeps the default disposition (it
    // ends on Ctrl-C), and the child still runs — nothing here may stand between the
    // user and the tool.
    let _ = add_ctrl_handler(swallow_ctrl_c);
    match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => e,
        },
        Err(e) => e,
    }
}

/// [`exec_or_run`]'s rule for a holder that waits on its child (`aterm pkg lease`): the
/// console's Ctrl-C belongs to the child ([`swallow_ctrl_c`], installed after the spawn),
/// and this process lives on to collect its status. Unverified on a Windows host, like
/// everything here.
///
/// # Errors
/// The wait's.
pub fn wait_standing_aside(
    child: &mut aterm_uds::spawnfd::TransparentChild,
) -> io::Result<std::process::ExitStatus> {
    let _ = add_ctrl_handler(swallow_ctrl_c);
    child.wait()
}

/// The child's exit code as this process's (a Windows status is always a code).
#[must_use]
pub fn exit_code_like(status: std::process::ExitStatus) -> std::process::ExitCode {
    std::process::ExitCode::from(
        status
            .code()
            .and_then(|code| u8::try_from(code & 0xff).ok())
            .unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE ACCOUNT'S PROFILE, MEASURED ON THE MACHINE RUNNING THE TEST: the token's
    /// profile directory resolves, is absolute and exists, and is a loaded profile (its
    /// `NTUSER.DAT` hive is there: a scratch `USERPROFILE` has none), the same twice.
    /// A `None` here makes every Windows window's disk watch measure-only and the
    /// headless package lane never run, with no other test failing.
    #[test]
    fn the_account_home_resolves_on_the_machine_running_this_test() {
        let account =
            account_home().expect("GetUserProfileDirectoryW must resolve this account's profile");
        assert!(account.is_absolute(), "{}", account.display());
        assert!(
            account.is_dir(),
            "the profile exists: {}",
            account.display()
        );
        assert!(
            std::fs::symlink_metadata(account.join("NTUSER.DAT")).is_ok(),
            "a loaded profile holds its hive: {}",
            account.display()
        );
        assert_eq!(account_home(), Some(account));
    }

    /// The scratch profile [`probe_the_account_home_under_a_scratch_profile`] runs under,
    /// set only in the child of this test binary that runs it.
    const SCRATCH_PROFILE_ENV: &str = "ATPKG_TEST_SCRATCH_PROFILE";
    /// That probe's name, as `--exact` wants it.
    const SCRATCH_PROFILE_PROBE: &str =
        "platform::windows::tests::probe_the_account_home_under_a_scratch_profile";

    /// THE ACCOUNT'S PROFILE IS NOT `%USERPROFILE%`: a process whose `USERPROFILE` and
    /// `HOME` name a scratch folder (a lab instance, a worker) still answers the token's
    /// own profile, with its hive. NEGATIVE CONTROL: an `account_home` that read
    /// `USERPROFILE` would answer the scratch folder here, which holds no `NTUSER.DAT`.
    /// The environment is process-wide, so the probe runs in a child of this test binary.
    #[test]
    fn the_account_home_is_not_the_userprofile_a_process_is_given() {
        let scratch = crate::testdir::dir("atpkg-scratch-profile-");
        let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "--nocapture", SCRATCH_PROFILE_PROBE])
            .env(SCRATCH_PROFILE_ENV, scratch.as_os_str())
            .env("USERPROFILE", scratch.as_os_str())
            .env("HOME", scratch.as_os_str())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("run the probe");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the account home under a scratch USERPROFILE: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// [`the_account_home_is_not_the_userprofile_a_process_is_given`]'s child: under the
    /// scratch profile its parent named, the account's home is another folder, and a
    /// loaded profile. Passes vacuously in any other run.
    #[test]
    fn probe_the_account_home_under_a_scratch_profile() {
        let Some(scratch) = std::env::var_os(SCRATCH_PROFILE_ENV) else {
            return;
        };
        let scratch = PathBuf::from(scratch);
        assert_eq!(
            std::env::var_os("USERPROFILE").map(PathBuf::from).as_ref(),
            Some(&scratch),
            "the probe runs under the scratch profile"
        );
        let account = account_home().expect("the token's profile resolves");
        let canon = |p: &Path| fs::canonicalize(p).expect("there");
        assert_ne!(canon(&account), canon(&scratch), "not USERPROFILE");
        assert!(
            fs::symlink_metadata(account.join("NTUSER.DAT")).is_ok(),
            "a loaded profile holds its hive: {}",
            account.display()
        );
        assert!(fs::symlink_metadata(scratch.join("NTUSER.DAT")).is_err());
    }
}
