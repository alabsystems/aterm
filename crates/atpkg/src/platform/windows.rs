// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Windows backend of [`crate::platform`]. Each function is the honest Windows
//! analogue of the Unix primitive (see the module docs on [`crate::platform`]): a
//! directory **junction** for the activation indirection, a `.cmd` batch wrapper for
//! bin shims (and, for the `agents/` twin, the same wrapper behind a `goto`-shaped
//! landing prelude — [`super::cmd_landing_prelude`], 2026-09-17; every `.cmd` behind
//! the resume-proof frame [`super::CMD_FRAME_HEAD`], 2026-09-18), per-user
//! `%LOCALAPPDATA%`-ACL privacy (no POSIX mode/owner bits), `GetDiskFreeSpaceExW` for
//! free space, `spawn().wait()` + `exit` for exec (behind a console control handler that
//! leaves Ctrl-C to the child), and `SetConsoleCtrlHandler` for the landing wait's
//! Ctrl-C ([`add_ctrl_handler`], the `signal(SIGINT, …)` twin; 2026-09-17).
//!
//! **This backend has NOT been exercised on a real Windows host.** It is written to be
//! correct-by-construction; the pure `.cmd` formatting/parsing is unit-tested (on Unix) in
//! [`crate::platform`], and running it is still nobody's evidence.
//!
//! IT IS TYPE-CHECKED, ON EVERY WINDOWS LANE THIS REPOSITORY HAS, and this header has twice
//! said otherwise. `mod windows` is `#[cfg(windows)]`, so a compiler reads every line below
//! on all three Windows cells `crates/aterm-libc` now admits — `x86_64-pc-windows-msvc`,
//! `aarch64-pc-windows-msvc` and `x86_64-pc-windows-gnu`. Type-checked is not run: no `.cmd`
//! shim, no junction and no `GetDiskFreeSpaceExW` call below has ever executed on a real
//! Windows host, and that remains the honest limit of the evidence for this file.
//!
//! WHICH WINDOWS TRIPLES REACH THIS FILE AT ALL is decided two crates down: `atpkg` depends
//! on `libc` unconditionally, `[patch.crates-io]` resolves it to `crates/aterm-libc`, and a
//! triple absent from that crate's cell list does not fall back to an empty cell — it stops
//! the build at `compile_error!("aterm-libc has no generated ABI cell for this target")`.
//! Both gaps that cost this repo a lane are closed: `aarch64-pc-windows-msvc`, a triple
//! `atpkg::TARGETS` publishes rows for, and `x86_64-pc-windows-gnu`, the cfg-validation lane
//! `.cargo/config.toml` and `rust-toolchain.toml` both configure. Two standing guards keep
//! the claim true rather than restating it: `crates/atpkg/tests/shipped_triples_have_an_abi_cell.rs`
//! fails when a SHIPPED triple is one `aterm-libc` refuses, and
//! `crates/aterm-libc/tests/target_gate.rs` fails when a CONFIGURED build lane is.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether the calling user can write into `dir`. Best-effort `true` on Windows,
/// matching [`dir_meta_is_private`]: writability rests on the per-user ACL, which this
/// layer does not evaluate. The Unix backend answers this with `access(2)`.
#[must_use]
pub fn dir_writable_by_caller(_dir: &Path) -> bool {
    true
}

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

/// `None`: there is no `getpwuid` and no per-host preference domain to protect. The
/// `[machine]` settings are macOS-only and answer not-applicable before this is asked.
#[must_use]
pub fn account_home() -> Option<PathBuf> {
    None
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

/// `None`: there is no Spotlight index to ask. Windows Search exposes no per-directory
/// opt-out for [`crate::noindex`] to honour or to MEASURE, so both Spotlight primitives
/// are the not-applicable sentinel here — they exist so `noindex` is one shape across
/// platforms and no call site needs a `cfg` (in practice
/// [`crate::noindex::verify`] answers `Verdict::NotApplicable` before it reaches this).
///
/// `None` is "the question could not be asked", NEVER "not indexed" — see
/// `crate::noindex::decide`, which turns every unanswerable question into `Unknown`
/// rather than into a claim that a directory is excluded.
#[must_use]
pub fn spotlight_query(_scope: &Path, _filename: &str) -> Option<bool> {
    None
}

/// `None`: there is no per-volume indexing switch this layer can read (`mdutil` has no
/// Windows analogue). Refines a message only; no verdict depends on it.
#[must_use]
pub fn spotlight_index_state(_path: &Path) -> Option<super::IndexState> {
    None
}

/// Universal Control is a macOS feature; there is nothing to read here.
#[must_use]
pub fn universal_control_state() -> [crate::machine::KeyRead; 2] {
    [
        crate::machine::KeyRead::Absent,
        crate::machine::KeyRead::Absent,
    ]
}

/// Universal Control is a macOS feature; nothing is written and nothing changed.
#[must_use]
pub fn universal_control_disable() -> bool {
    false
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

/// Push ONE open file's contents to the filesystem — the Windows analogue of the Unix
/// `fsync(2)`: `FlushFileBuffers`, which is what std's `sync_data` calls here. Used by
/// [`crate::store::sync_tree`] to make a staged tree durable before the renames that
/// publish it, and by the readiness marker's write.
pub fn sync_file_contents(f: &File) -> io::Result<()> {
    f.sync_data()
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
///
/// A `.cmd` shim this replaces may be EXECUTING (2026-09-17): `cmd.exe` runs a batch file
/// by re-reading it at a remembered byte offset after every line, so what the shim text
/// does about a re-lay mid-run is the shim's own business — every line that runs a
/// program ends the batch on that line ([`super::CMD_FORWARD_TAIL`]), and every file
/// starts with the frame ([`super::CMD_FRAME_HEAD`], 2026-09-18) that a file from
/// before that tail resumes into harmlessly.
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
/// Fail-closed: refuse a target that could break out of the `@"<target>" %* & @exit /b`
/// quoting (a `"`/`%`/CR/LF/NUL) rather than write an injectable batch wrapper — a managed
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

/// The shim [`install_shim_to_env`] lays, RENDERED but not written — the same two
/// injection refusals, the `.cmd` body — for the callers that lay a whole pass of shims
/// in one go ([`crate::lay::lay_executables`]; no provenance lane exists here, so it
/// writes in-process).
pub fn shim_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<crate::lay::Executable> {
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
    Ok(crate::lay::Executable::new(
        shim,
        super::cmd_shim_content_env(target, env),
    ))
}

/// The `agents/` twin on Windows (2026-09-17, residual R4 closed): the `.cmd` shim with
/// `prelude` — the [`super::cmd_landing_prelude`] the caller rendered — ahead of its
/// `@set` lines and forward line ([`super::cmd_shim_content_twin`]), so a `claude` typed
/// while `<prefix>/landing/claude` stands hands over to the embedded co-located
/// `atpkg __landing` and falls through to the store build when that `atpkg` is gone.
/// The same two injection refusals as [`shim_executable_to_env`]; the prelude's own
/// paths were guarded when it was rendered (an unsafe one renders EMPTY, the plain shim).
/// RENDERED but not written. Unverified on a Windows host, like everything here.
pub fn twin_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<crate::lay::Executable> {
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
/// Parses the batch wrapper's `@"<target>" %* & @exit /b` line (or the `@"<target>" %*`
/// line of a shim laid before 2026-09-17, until it is re-laid) — the Windows inverse of
/// `read_link`. A tombstone `.cmd` (no forward target) yields `None`.
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
/// called (on its own thread) for every Ctrl-C / Ctrl-Break / close event until removed by
/// [`remove_ctrl_handler`]. `false` when the call failed; the caller then has the default
/// behaviour (the process ends on Ctrl-C), never a wrong claim. The Windows analogue of
/// `signal(SIGINT, …)` — the seam `cli::cmd_landing` arms for the landing wait's duration
/// (2026-09-17; unverified on a Windows host).
pub fn add_ctrl_handler(handler: CtrlHandler) -> bool {
    // SAFETY: `handler` is a plain `extern "system"` function that lives for the whole
    // program; kernel32 keeps only its address. No memory of ours is handed over.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

/// Remove a handler [`add_ctrl_handler`] installed; `false` when it was not installed.
pub fn remove_ctrl_handler(handler: CtrlHandler) -> bool {
    // SAFETY: as in `add_ctrl_handler`; removing a routine that is not installed is a
    // documented failure (`FALSE`), not undefined behaviour.
    unsafe { SetConsoleCtrlHandler(Some(handler), 0) != 0 }
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
/// **Ctrl-C belongs to the child** (2026-09-17, review finding): the console delivers
/// `CTRL_C_EVENT` to EVERY process attached to it, and a process with no handler ends on
/// it. Before the spawn this wrapper installs [`swallow_ctrl_c`] — the shape rustup's
/// proxies and cargo's runners use — so the first Ctrl-C typed inside an agent (which
/// `claude` treats as "stop this turn", not "exit") no longer kills the wrapper, loses the
/// exit code and orphans the agent; the child gets the event as before and answers it
/// its own way. Installed for the rest of this process's life (it ends with the child's
/// code). NOT the `NULL`-routine ignore flag, which children inherit.
///
/// A `.cmd` program (the `bin/<program>.cmd` shim `cli::cmd_landing` runs through here
/// after the wait) is routed by std through its own `cmd.exe /d /c` lane with batch-safe
/// quoting — std may refuse arguments it cannot quote safely, the same as for any `.cmd`
/// shim today. That lane is std's; it shares only the `cmd.exe` binary with
/// [`atomic_symlink`]'s explicit `cmd /C mklink`, and nothing in this crate has exercised
/// it. While a batch file is the child, `cmd.exe` itself may ask `Terminate batch job
/// (Y/N)?` after a Ctrl-C — its own prompt for every `.cmd` shim, in any shell, since
/// before this lane. And on the landing path there are TWO batch levels, not one
/// (review, 2026-09-17): the `agents\<program>.cmd` twin the user's shell is running,
/// and the `bin\<program>.cmd` shim this function spawns through `cmd.exe /d /c` — each
/// `cmd.exe` that received the Ctrl-C asks its own question once the agent exits, so a
/// Ctrl-C typed inside the agent (which `claude` treats as "stop this turn" and keeps
/// running through) can raise the prompt up to TWICE, in turn, when the agent finally
/// ends — where the plain `bin\` shim asks once. Unverified on a Windows host, like
/// everything here.
pub fn exec_or_run(command: &mut Command) -> io::Error {
    // Best effort: when the install fails the wrapper simply keeps the default
    // disposition (it ends on Ctrl-C, as it did before 2026-09-17), and the child still
    // runs — nothing here may stand between the user and the tool.
    let _ = add_ctrl_handler(swallow_ctrl_c);
    match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => e,
        },
        Err(e) => e,
    }
}
