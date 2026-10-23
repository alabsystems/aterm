// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Windows backend of [`crate::platform`]: junctions for activation, `.cmd` shims, per-user
//! profile-ACL privacy, `GetDiskFreeSpaceExW` for free space, and `spawn().wait()` + `exit` for
//! exec, with Ctrl-C left to the child ([`add_ctrl_handler`]).

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Whether the calling user can write into `dir`: best-effort `true`, since writability rests on the per-user ACL, which this layer does not evaluate.
#[must_use]
pub fn dir_writable_by_caller(_dir: &Path) -> bool {
    true
}

/// Appended to a tool name to form its executable name, only by [`crate::store::ToolName::exe_file`]: the shim and the executable are different files here (`bin/ay.cmd` vs `bin\ay.exe`).
pub const EXE_SUFFIX: &str = ".exe";
/// Appended to a tool name to form its `bin/` shim name (a batch wrapper), only by [`crate::store::ToolName::shim_file`]; stripped only by [`crate::store::ToolName::from_shim_file`].
pub const SHIM_SUFFIX: &str = ".cmd";

/// The default install prefix: `%LOCALAPPDATA%\aterm\pkg` (per-user, ACL-private by default), falling back to `%USERPROFILE%\AppData\Local\aterm\pkg` via `home` when `%LOCALAPPDATA%` is unset.
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("aterm").join("pkg")
    } else {
        home.join("AppData").join("Local").join("aterm").join("pkg")
    }
}

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

/// This ACCOUNT's home directory, as the OS knows it — the profile directory of this process's token (`GetUserProfileDirectoryW`, which reads the account's `ProfileList\<SID>\ProfileImagePath`) — which is NOT `%USERPROFILE%`, exactly as the Unix twin's `getpwuid` home is not `$HOME`: a process given a scratch `USERPROFILE` (a lab instance, a test) still answers the real profile, so the callers that ask "is this the account's own home?" (the disk watch's owner rule, the headless package lane's scratch-home gate) can tell the two apart.
#[must_use]
pub fn account_home() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    const TOKEN_QUERY: u32 = 0x0008;
    /// A profile path longer than the wide APIs' own bound is not a profile.
    const MAX_UNITS: u32 = 32_768;
    let mut token: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` takes nothing and returns this process's pseudo-handle, which needs no closing; `&mut token` is a valid out-param the call fills with a token handle on success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        || token.is_null()
    {
        return None;
    }
    let mut units: u32 = 0;
    // SAFETY: `token` is the live token opened above; a NULL buffer with a zero size is the documented way to ask for the size, which is written into `units`.
    unsafe { GetUserProfileDirectoryW(token, std::ptr::null_mut(), &mut units) };
    let read = (1..=MAX_UNITS).contains(&units).then(|| {
        let mut buf = vec![0u16; units as usize];
        // SAFETY: `buf` holds `units` writable UTF-16 units and outlives the call, which writes at most that many (the NUL included) and updates `units`.
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

/// No effective uid on Windows (privacy is the profile ACL): the `0` sentinel, used only in a diagnostic that [`dir_meta_is_private`] (always `true`) never reaches.
#[must_use]
pub fn our_uid() -> u32 {
    0
}

/// No process groups on Windows: the caller's `Child::kill` is the whole of it.
pub fn kill_process_group(_leader: u32) {}

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

/// The kernel's start-time stamp for `pid` ([`crate::platform::Birth`]), used with the pid to detect reuse, as the Unix twin's is: `GetProcessTimes`'s creation time, in 100-nanosecond ticks since 1601, read through a `PROCESS_QUERY_LIMITED_INFORMATION` handle (the least access that reads it, which Windows grants on most other accounts' processes too).
#[must_use]
pub fn process_birth(pid: u32) -> crate::platform::Birth {
    use crate::platform::Birth;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return Birth::Gone;
    }
    // SAFETY: three integers in; a process handle, or NULL with the reason in the thread's last error, out.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return if io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            Birth::Gone
        } else {
            Birth::Unreadable
        };
    }
    let mut code: u32 = 0;
    let (mut created, mut exited, mut kernel, mut user) =
        ([0u32; 2], [0u32; 2], [0u32; 2], [0u32; 2]);
    // SAFETY: `handle` is the live handle opened above, with the access both queries need; every out-param is a valid, writable value of the type the API fills.
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

/// Best-effort private-dir predicate: `true`.
#[must_use]
pub fn dir_meta_is_private(_meta: &Metadata) -> bool {
    true
}

/// No-op: backup exclusion is a macOS convention with no Windows per-directory equivalent.
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

/// Rosetta 2 is a macOS install: Windows has no runtime to find ([`crate::machine::ROSETTA_APPLIES`] keeps this from being asked).
pub fn rosetta_present(_runtime: &Path) -> Result<bool, String> {
    Ok(false)
}

/// Rosetta 2 is a macOS install; nothing runs here.
pub fn rosetta_install(_within: std::time::Duration) -> Result<(), String> {
    Err("Rosetta 2 is a macOS install".to_string())
}

/// Shared-directory creation: `create_dir_all`, since POSIX modes do not apply and the ACL governs access.
pub fn ensure_shared_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// System-owned predicate: always `false`.
#[must_use]
pub fn dir_meta_is_system(_meta: &Metadata) -> bool {
    false
}

/// Whether `meta` (from `symlink_metadata`) is a link-like indirection the fail-closed prefix chain check must not trust as a directory: any reparse point, including a junction (`mklink /J`, no admin), which `is_symlink()` reports `false` (it is `IO_REPARSE_TAG_MOUNT_POINT`).
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

/// Open `path` for a fresh (create+truncate) write.
pub fn open_create_write(path: &Path, _mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Create `path` for writing, failing [`io::ErrorKind::AlreadyExists`] when the name already resolves to anything (`CREATE_NEW`, under NTFS's case-insensitive name equality).
pub fn open_create_new_write(path: &Path, _mode: u32) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Open `path` with write access, as [`sync_file_contents`] needs: `FlushFileBuffers` requires `GENERIC_WRITE` and refuses a read-only handle with `ERROR_ACCESS_DENIED`.
pub fn open_for_sync(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).open(path)
}

/// Push one open file's contents to the filesystem: `FlushFileBuffers` (std's `sync_data` here), the analogue of `fsync(2)`.
pub fn sync_file_contents(f: &File) -> io::Result<()> {
    aterm_durable::durable_data(f)
}

/// No permission bits: reports `0`.
#[must_use]
pub fn permission_mode(_meta: &Metadata) -> u32 {
    0
}

/// Whether this platform's filesystems store POSIX permission bits an `fstat` reads back.
pub const HAS_POSIX_MODES: bool = false;

/// The mode the extraction-time fold records for a file just written with `requested` bits: the request, masked to `0o7777`.
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

unsafe extern "system" {
    fn GetDiskFreeSpaceExW(
        lp_directory_name: *const u16,
        lp_free_bytes_available_to_caller: *mut u64,
        lp_total_number_of_bytes: *mut u64,
        lp_total_number_of_free_bytes: *mut u64,
    ) -> i32;
}

/// Free bytes on the volume holding `dir` (which must exist), or `None` on any error: fails open, like the Unix `statvfs` path.
#[must_use]
pub fn volume_free_bytes(dir: &Path) -> Option<u64> {
    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut free_to_caller: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call, `free_to_caller` is a valid out-param, and the two optional totals are NULL.
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
/// are read; the rest is declared to get the layout right.
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

/// The OS identity of the directory at `dir`: `(volume serial, 64-bit file index)`, the NTFS analogue of `(dev, ino)`, which must not change across a trustworthy enumeration.
#[must_use]
pub fn directory_identity(dir: &Path) -> Option<(u32, u64)> {
    const FILE_SHARE_ALL: u32 = 0x0000_0007;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const INVALID_HANDLE_VALUE: isize = -1;

    let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call; the optional security-attributes and template params are NULL.
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
    // SAFETY: `handle` is live from the successful `CreateFileW`, and `info` is a correctly laid-out writable out-param.
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

/// Remove whatever indirection sits at `link`, best-effort: `remove_dir` unlinks a junction (not its target's contents), `remove_file` a stale plain file.
pub fn remove_link(link: &Path) {
    let _ = fs::remove_dir(link);
    let _ = fs::remove_file(link);
}

/// `p` with every `/` rewritten to `\`, wide-char-wise (lossless). `cmd` built-ins read `/x`
/// anywhere on a line as a switch, so an `mklink` argument like `store/trust/671` (which
/// `Path::join` of a multi-component `&str` produces) fails as `Invalid switch - "trust"`.
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

/// Point `link` at directory `target` via a junction (`mklink /J`, no admin).
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

/// Best-effort atomic write of `bytes` to `dest`: sibling temp, remove `dest` (Windows
/// `rename` does not replace a file), rename.
fn atomic_write(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::lay::write_in_process(&crate::lay::Executable::new(dest, bytes))
}

/// The two injection refusals every `.cmd` shim renderer makes, fail-closed: a target that
/// could break out of the `@"<target>" %* & @exit /b` quoting (`"`, `%`, CR, LF, NUL), and an
/// env entry that could break out of `@set "NAME=VALUE"` (also refused at manifest parse).
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

/// Install a `.cmd` bin shim at `shim` that sets `env` (empty: the plain shim), then forwards to `target` (`…\<tool>.exe`): [`shim_executable_to_env`]'s body, written through [`atomic_write`].
pub fn install_shim_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    let body = shim_executable_to_env(shim, target, env)?.body;
    atomic_write(shim, &body)
}

/// The shim [`install_shim_to_env`] lays, rendered but not written, for callers that render a whole pass of shims first ([`crate::lay::write_in_process`]).
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

/// The `agents/` twin: the `.cmd` shim with `prelude` ([`super::twin_prelude`], empty on Windows) ahead of its `@set` and forward lines ([`super::cmd_shim_content_twin`]), with the same refusals as [`shim_executable_to_env`].
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

/// Lay the `.cmd` twin ([`twin_executable_to_env`]) through [`atomic_write`].
pub fn install_twin_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<()> {
    let body = twin_executable_to_env(shim, target, env, prelude)?.body;
    atomic_write(shim, &body)
}

/// Install a failing tombstone shim at `shim` (a `.cmd`) that prints `message` to stderr and exits 70: the Windows analogue of the Unix `sh` tombstone.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    atomic_write(shim, super::cmd_tombstone_content(message).as_bytes())
}

/// Resolve the store/checkout target a `bin/<tool>.cmd` shim forwards to, or `None`.
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    super::read_cmd_shim_target(shim)
}

/// [`resolve_shim`] for a reader that must tell "no shim" from "could not read": the program the `.cmd` at `shim` forwards to (`Ok(Some)`), `Ok(None)` for a file that is gone or holds no forward line (a tombstone, a stub, any other batch file, bytes that are not UTF-8), and `Err` when it is there but could not be read through the same bounded, reparse-refusing handle (over [`super::MAX_SHIM_BYTES`], a reparse point, a sharing refusal, changed mid-read).
/// # Errors
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

/// The environment the `.cmd` shim at `shim` sets before it forwards — its `@set` lines ([`super::read_cmd_shim_env`]; NONE for a tombstone, a stub, or anything unreadable).
#[must_use]
pub fn shim_env_of(shim: &Path) -> crate::shim_env::ShimEnv {
    super::read_cmd_shim_env(shim)
}

/// A console control handler: `ctrl_type` is one of the `CTRL_*_EVENT` values; `TRUE` (non-zero) means "handled, call no further handler", `FALSE` passes the event on to the next handler and finally to the default one, which ends the process.
pub type CtrlHandler = unsafe extern "system" fn(ctrl_type: u32) -> i32;

/// `CTRL_C_EVENT`: the user pressed Ctrl-C in the console (or `GenerateConsoleCtrlEvent`).
pub const CTRL_C_EVENT: u32 = 0;
/// `CTRL_BREAK_EVENT`: Ctrl-Break, delivered to every process of the console's group.
pub const CTRL_BREAK_EVENT: u32 = 1;

unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler_routine: Option<CtrlHandler>, add: i32) -> i32;
}

/// Install `handler` at the front of this process's console control handler list (called on its own thread for each Ctrl-C, Ctrl-Break or close event).
pub fn add_ctrl_handler(handler: CtrlHandler) -> bool {
    // SAFETY: `handler` is a plain `extern "system"` function that lives for the whole
    // program; kernel32 keeps only its address. No memory of ours is handed over.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

/// The wrapper's handler while a child owns the console: `TRUE` for Ctrl-C and Ctrl-Break
/// (the child got the same event and decides; this process stays to collect its exit code),
/// `FALSE` for close, logoff or shutdown, which end every console process anyway.
unsafe extern "system" fn swallow_ctrl_c(ctrl_type: u32) -> i32 {
    i32::from(matches!(ctrl_type, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

/// Run `command` to completion, then `exit` with its code (no `execve` on Windows).
pub fn exec_or_run(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    let _ = add_ctrl_handler(swallow_ctrl_c);
    match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => e,
        },
        Err(e) => e,
    }
}

/// Nothing to arm off unix: Windows sends no SIGTERM or SIGHUP to pass on.
pub struct Forwarding;

/// [`Forwarding`]'s stand-in.
#[must_use]
pub fn arm_forwarding() -> Forwarding {
    Forwarding
}

/// [`exec_or_run`]'s Ctrl-C rule for a holder that waits on its child (`aterm pkg lease`): [`swallow_ctrl_c`], installed after the spawn, leaves the console's Ctrl-C to the child.
/// # Errors
/// The wait's.
pub fn wait_standing_aside(
    child: &mut aterm_uds::spawnfd::TransparentChild,
    _forwarding: Forwarding,
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
    /// own profile, with its hive.
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
    /// loaded profile.
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
