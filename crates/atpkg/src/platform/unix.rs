// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Unix backend of [`crate::platform`].

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use aterm_types::fs_restricted::dir_safe_for_private_write;

/// Appended to a tool name to form its executable name.
pub const EXE_SUFFIX: &str = "";
/// Appended to a tool name to form its `bin/` shim name.
pub const SHIM_SUFFIX: &str = "";

/// The default install prefix under `home`.
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// Our effective uid.
#[must_use]
pub fn our_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// SIGKILL the process group `leader` leads (a child started with `process_group(0)`), so whatever it started goes with it.
pub fn kill_process_group(leader: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(leader) {
        // SAFETY: `killpg` takes two integers and touches no memory of ours.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

/// The kernel's start-time stamp for `pid` ([`crate::platform::Birth`]), used with the pid to detect reuse within the current boot.
#[must_use]
pub fn process_birth(pid: u32) -> crate::platform::Birth {
    match libc::pid_t::try_from(pid).ok().filter(|p| *p > 0) {
        Some(pid) => process_birth_platform(pid),
        None => crate::platform::Birth::Gone,
    }
}

#[cfg(target_os = "macos")]
fn process_birth_platform(pid: libc::pid_t) -> crate::platform::Birth {
    use crate::platform::Birth;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let Ok(size) = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()) else {
        return Birth::Unreadable;
    };
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Birth::Gone
        } else {
            Birth::Unreadable
        };
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    if u32::try_from(pid).ok() != Some(info.pbi_pid) {
        return Birth::Unreadable;
    }
    if info.pbi_status == libc::SZOMB {
        return Birth::Gone;
    }
    info.pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|us| us.checked_add(info.pbi_start_tvusec))
        .map_or(Birth::Unreadable, Birth::At)
}

#[cfg(target_os = "linux")]
fn process_birth_platform(pid: libc::pid_t) -> crate::platform::Birth {
    use crate::platform::Birth;
    let stat = match aterm_uds::process::read_proc_text(std::path::Path::new(&format!(
        "/proc/{pid}/stat"
    ))) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Birth::Gone,
        Err(_) => return Birth::Unreadable,
    };
    let Some(close) = stat.rfind(')') else {
        return Birth::Unreadable;
    };
    let mut fields = stat[close + 1..].split_ascii_whitespace();
    match fields.next() {
        Some("Z") => Birth::Gone,
        Some(_) => fields
            .nth(18)
            .and_then(crate::store::parse_build_name)
            .map_or(Birth::Unreadable, Birth::At),
        None => Birth::Unreadable,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_birth_platform(_pid: libc::pid_t) -> crate::platform::Birth {
    crate::platform::Birth::Unreadable
}

/// This ACCOUNT's home directory, as the OS knows it.
#[must_use]
pub fn account_home() -> Option<PathBuf> {
    aterm_uds::account::home()
}

/// Mark `dir` as excluded from Time Machine ([`aterm_uds::backup`]). Best effort: a store with no
/// backup mark still works.
pub fn exclude_from_backup(dir: &Path) {
    let _ = aterm_uds::backup::Backups::SYSTEM.exclude_path(dir);
}

/// The names of the extended attributes on `path` itself, a link not followed (`listxattr(2)`
/// with `XATTR_NOFOLLOW`).
///
/// # Errors
/// `path` holds a NUL byte, or `listxattr` fails, the list outgrowing its first answer
/// before the second call included.
#[cfg(target_os = "macos")]
pub fn xattr_names(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call; a null buffer of size 0
    // asks only for the list's length, and nothing is written.
    let want =
        unsafe { libc::listxattr(c.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
    let want = usize::try_from(want).map_err(|_| io::Error::last_os_error())?;
    if want == 0 {
        return Ok(Vec::new());
    }
    let mut list = vec![0u8; want];
    // SAFETY: `c` is as above and `list` is a writable buffer of the stated length; `listxattr`
    // writes at most that many bytes and returns how many it wrote, or -1.
    let got = unsafe {
        libc::listxattr(
            c.as_ptr(),
            list.as_mut_ptr().cast::<libc::c_char>(),
            list.len(),
            libc::XATTR_NOFOLLOW,
        )
    };
    let got = usize::try_from(got).map_err(|_| io::Error::last_os_error())?;
    list.truncate(got);
    Ok(list
        .split(|&b| b == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

/// The `defaults(1)` domain of macOS Universal Control, per-host (`-currentHost`).
#[cfg(target_os = "macos")]
pub const UNIVERSAL_CONTROL_DOMAIN: &str = "com.apple.universalcontrol";
/// The two keys that switch it off.
#[cfg(target_os = "macos")]
pub const UNIVERSAL_CONTROL_KEYS: [&str; 2] = ["Disable", "DisableMagicEdges"];

/// Universal Control's per-host switches, as one `defaults -currentHost read` of their domain
/// answers ([`crate::machine::universal_control_from_domain`]).
#[must_use]
pub fn universal_control_state() -> [crate::machine::Read<bool>; 2] {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/defaults");
        cmd.arg("-currentHost")
            .arg("read")
            .arg(UNIVERSAL_CONTROL_DOMAIN);
        crate::machine::universal_control_from_domain(
            bounded_exit_and_stdout(&mut cmd),
            UNIVERSAL_CONTROL_KEYS,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        [crate::machine::Read::Absent, crate::machine::Read::Absent]
    }
}

/// Write both Universal Control switches true for the current host (`defaults -currentHost write com.apple.universalcontrol <key> -bool true`).
#[must_use]
pub fn universal_control_disable() -> bool {
    #[cfg(target_os = "macos")]
    {
        UNIVERSAL_CONTROL_KEYS.iter().all(|key| {
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/defaults");
            cmd.arg("-currentHost")
                .arg("write")
                .arg(UNIVERSAL_CONTROL_DOMAIN)
                .arg(key)
                .arg("-bool")
                .arg("true");
            bounded_stdout(&mut cmd).is_some()
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Whether Rosetta 2's runtime file is at `runtime` ([`crate::machine::ROSETTA_RUNTIME`]).
pub fn rosetta_present(runtime: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(runtime) {
        Ok(meta) => Ok(meta.is_file()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The Rosetta 2 install command (`softwareupdate --install-rosetta --agree-to-license`).
#[cfg(target_os = "macos")]
const ROSETTA_INSTALL: [&str; 3] = [
    "/usr/sbin/softwareupdate",
    "--install-rosetta",
    "--agree-to-license",
];

/// Install Rosetta 2 ([`ROSETTA_INSTALL`]) as this user, never through `sudo`, and wait at most `within` (killed past it).
pub fn rosetta_install(within: std::time::Duration) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let [program, args @ ..] = ROSETTA_INSTALL;
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.args(args);
        let Some(out) = crate::doctor::output_bounded_capturing(&mut cmd, within) else {
            return Err(format!(
                "`softwareupdate --install-rosetta` did not start, or ran past {} s and was \
                 stopped",
                within.as_secs()
            ));
        };
        if out.status.success() {
            return Ok(());
        }
        let last_line = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .map(|l| l.chars().take(200).collect::<String>())
        };
        let said = last_line(&out.stderr)
            .or_else(|| last_line(&out.stdout))
            .unwrap_or_default();
        let code = out
            .status
            .code()
            .map_or_else(|| "on a signal".to_string(), |c| c.to_string());
        Err(if said.is_empty() {
            format!("`softwareupdate --install-rosetta` exited {code}")
        } else {
            format!("`softwareupdate --install-rosetta` exited {code}: {said}")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = within;
        Err("Rosetta 2 is a macOS install".to_string())
    }
}

/// [`bounded_stdout`], keeping the child's success bit: `defaults read` exits 1 with empty
/// output for "this key is not set", an answer to tell apart from a read that did not run.
#[cfg(target_os = "macos")]
fn bounded_exit_and_stdout(cmd: &mut aterm_uds::spawnfd::Command) -> Option<(bool, Vec<u8>)> {
    bounded_stdout_inner(cmd, BOUNDED_CEILING)
}

/// Run `cmd` with a bounded wall clock ([`BOUNDED_CEILING`]); its stdout on a zero exit,
/// `None` on a spawn failure, a non-zero exit or the deadline.
#[cfg(target_os = "macos")]
fn bounded_stdout(cmd: &mut aterm_uds::spawnfd::Command) -> Option<Vec<u8>> {
    match bounded_stdout_inner(cmd, BOUNDED_CEILING)? {
        (true, out) => Some(out),
        (false, _) => None,
    }
}

/// How long a production probe waits for its child before killing it and answering `None`.
#[cfg(target_os = "macos")]
const BOUNDED_CEILING: std::time::Duration = std::time::Duration::from_secs(5);

/// The deadline behind [`bounded_stdout`]: `crate::doctor::output_bounded_within`, which
/// kills the child at `ceiling` (`aterm_uds::reaper` collects it once it ends) and reads its
/// stdout while it runs, never waiting on a process the child left behind holding the pipe.
#[cfg(target_os = "macos")]
fn bounded_stdout_inner(
    cmd: &mut aterm_uds::spawnfd::Command,
    ceiling: std::time::Duration,
) -> Option<(bool, Vec<u8>)> {
    crate::doctor::output_bounded_within(cmd, ceiling).map(|out| (out.status.success(), out.stdout))
}

/// The system-prefix counterpart of `ensure_private_dir`'s `0700`.
pub fn ensure_shared_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{}: store directory is a symlink; refusing", dir.display()),
        ));
    }
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755)?;
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o755) {
        set_mode(dir, 0o755)?;
    }
    Ok(())
}

/// Whether a directory's metadata says it is system-owned.
#[must_use]
pub fn dir_meta_is_system(meta: &Metadata) -> bool {
    meta.uid() == 0 && meta.mode() & 0o022 == 0
}

/// Whether the CALLING user may take `path`'s name out of the folder holding it (rename it away
/// or remove it), as the kernel authorizes that now. On macOS `access(2)` is asked for
/// `_DELETE_OK` (`<sys/unistd.h>`; `libc` has no name for it), which weighs what a rename of it
/// weighs: its own flags and ACL (`uchg`, `schg`, an append-only flag, a `deny delete` entry) and
/// its folder's (its write bit, a `delete_child` entry, the sticky bit). Elsewhere what Linux's
/// `may_delete` weighs, as far as it can be read: the folder's write bit
/// ([`dir_writable_by_caller`]); a sticky folder that neither it nor the entry is the caller's
/// ([`sticky_keeps`]); and, on Linux, the entry's and its folder's immutable or append-only
/// inode attribute (`chattr +i`, `+a`: `statx`'s `stx_attributes` as `stx_attributes_mask` says
/// the filesystem reports them, [`binds`]). Where `statx` cannot be asked (`ENOSYS` before Linux
/// 4.11, a sandbox's refusal, a libc without it such as musl's by default) the attributes are not
/// read, and a rename one refuses once the intent is on record closes that intent
/// (`build_cache::Unused::Refused`).
#[must_use]
pub fn may_take_name(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        /// `_DELETE_OK` of `<sys/unistd.h>`: the right to delete the file or folder.
        const DELETE_OK: libc::c_int = 1 << 12;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `c` is a NUL-terminated C string that outlives the call; `access` only reads it.
        unsafe { libc::access(c.as_ptr(), DELETE_OK) == 0 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let Some(folder) = path.parent() else {
            return false;
        };
        if !dir_writable_by_caller(folder) {
            return false;
        }
        let (Ok(entry), Ok(holder)) = (fs::symlink_metadata(path), fs::metadata(folder)) else {
            return false;
        };
        let bound = |at: &Path, follow: bool| attributes(at, follow).is_some_and(binds);
        !sticky_keeps(holder.mode(), holder.uid(), entry.uid(), our_uid())
            && !bound(path, false)
            && !bound(folder, true)
    }
}

/// `STATX_ATTR_IMMUTABLE` (`<linux/stat.h>`): nothing may change, rename or remove the inode.
#[cfg(any(not(target_os = "macos"), test))]
const ATTR_IMMUTABLE: u64 = 0x10;
/// `STATX_ATTR_APPEND` (`<linux/stat.h>`): the inode may only be appended to; it, or an entry of
/// it, is never renamed or removed.
#[cfg(any(not(target_os = "macos"), test))]
const ATTR_APPEND: u64 = 0x20;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
const _: () = assert!(
    ATTR_IMMUTABLE == libc::STATX_ATTR_IMMUTABLE as u64
        && ATTR_APPEND == libc::STATX_ATTR_APPEND as u64
);

/// Whether an inode's attributes, `(stx_attributes, stx_attributes_mask)` as `statx` reads them,
/// keep its name, or the names in it, from being taken: immutable or append-only, among those its
/// filesystem reports it supports.
#[cfg(any(not(target_os = "macos"), test))]
const fn binds((attributes, mask): (u64, u64)) -> bool {
    attributes & mask & (ATTR_IMMUTABLE | ATTR_APPEND) != 0
}

/// Whether a sticky folder (`folder_mode` with `S_ISVTX`) keeps the caller `me` from taking the
/// name of an entry in it: one that neither the folder (`folder_uid`) nor the entry
/// (`entry_uid`) is the caller's, unless the caller is root (who holds `CAP_FOWNER`).
#[cfg(any(not(target_os = "macos"), test))]
const fn sticky_keeps(folder_mode: u32, folder_uid: u32, entry_uid: u32, me: u32) -> bool {
    /// `S_ISVTX`: the sticky bit.
    const STICKY: u32 = 0o1000;
    folder_mode & STICKY != 0 && me != 0 && me != folder_uid && me != entry_uid
}

/// Linux's inode attributes of `path` (`statx`, the last component followed only when `follow`)
/// and the ones its filesystem reports it supports: `(stx_attributes, stx_attributes_mask)`;
/// `None` where `statx` cannot be asked or fails, which reads no attribute.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn attributes(path: &Path, follow: bool) -> Option<(u64, u64)> {
    #[cfg(test)]
    if let Some(seamed) = seams::attributes(path) {
        return seamed;
    }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    let mut out = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, which only reads it, and
    // `out` points at a `statx` the call writes on success.
    let rc = unsafe {
        libc::statx(
            libc::AT_FDCWD,
            c.as_ptr(),
            flags,
            libc::STATX_TYPE,
            out.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: a zeroed `statx` is a valid one, and the call succeeded, so it filled `out`.
    let out = unsafe { out.assume_init() };
    Some((out.stx_attributes, out.stx_attributes_mask))
}

/// Off Linux's glibc no inode attribute is read ([`may_take_name`]).
#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
const fn attributes(_: &Path, _: bool) -> Option<(u64, u64)> {
    None
}

/// THE TESTS' STAND-IN FOR `statx`'s attributes, on the thread that sets it: what
/// [`attributes`] reads of a path, `None` as an unanswered `statx` (`ENOSYS`). Test builds on
/// Linux's glibc alone.
#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
pub(crate) mod seams {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    type Read = Option<(u64, u64)>;

    thread_local! {
        static ATTRIBUTES: RefCell<Vec<(PathBuf, Read)>> = const { RefCell::new(Vec::new()) };
    }

    /// Read `path`'s attributes as `read` on this thread (`None`: `statx` unanswered).
    pub(crate) fn attributes_of(path: &Path, read: Read) {
        ATTRIBUTES.with(|a| a.borrow_mut().push((path.to_path_buf(), read)));
    }

    /// The `(stx_attributes, stx_attributes_mask)` of `STATX_ATTR_IMMUTABLE` set.
    pub(crate) const IMMUTABLE: (u64, u64) = (super::ATTR_IMMUTABLE, super::ATTR_IMMUTABLE);

    /// Forget every stand-in of this thread.
    pub(crate) fn clear() {
        ATTRIBUTES.with(|a| a.borrow_mut().clear());
    }

    pub(super) fn attributes(path: &Path) -> Option<Read> {
        ATTRIBUTES.with(|a| {
            a.borrow()
                .iter()
                .rev()
                .find(|(p, _)| p == path)
                .map(|(_, read)| *read)
        })
    }
}

/// Whether a directory's metadata says it is private-write-safe.
#[must_use]
pub fn dir_meta_is_private(meta: &Metadata) -> bool {
    dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode())
}

/// Whether the CALLING user can write into `dir`.
#[must_use]
pub fn dir_writable_by_caller(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a NUL-terminated C string that outlives the call; `access` only reads it.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Whether `meta` (from `symlink_metadata`) is a link-like indirection the fail-closed prefix chain check must not trust as a directory.
#[must_use]
pub fn is_reparse(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Remove whatever indirection sits at `link`.
pub fn remove_link(link: &Path) {
    let _ = fs::remove_file(link);
}

/// Force a file to `0600`.
pub fn harden_file(path: &Path) -> io::Result<()> {
    crate::metadata_io::open_regular_with_meta(path)?
        .0
        .set_permissions(Permissions::from_mode(0o600))
}

/// Set a regular file's or directory's permission bits to `mode`, refusing symlinks and special files.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let perms = Permissions::from_mode(mode);
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => {
            let meta = f.metadata()?;
            if !meta.is_file() && !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{}: not a regular file or directory", path.display()),
                ));
            }
            f.set_permissions(perms)
        }
        Err(e) => {
            if e.kind() != io::ErrorKind::PermissionDenied
                || !fs::symlink_metadata(path).is_ok_and(|m| m.is_file() || m.is_dir())
            {
                return Err(e);
            }
            // An owner may chmod an unreadable entry. Keep that recovery, but never
            // follow a link exchanged in after the metadata check. A platform that
            // cannot honor NOFOLLOW refuses instead of falling back to chmod(path).
            aterm_uds::atomic_file::set_mode_nofollow(path, mode)
        }
    }
}

/// Set an open file's permission bits to `mode` (`fchmod(2)`) for `crate::extract::write_capped`.
pub fn set_mode_on(f: &File, mode: u32) -> io::Result<()> {
    f.set_permissions(Permissions::from_mode(mode))
}

/// The BSD file flags that stop an entry's owner changing or removing it.
#[cfg(target_os = "macos")]
pub const OWNER_BINDING_FLAGS: u32 =
    libc::UF_IMMUTABLE | libc::UF_APPEND | libc::SF_IMMUTABLE | libc::SF_APPEND;

/// Set `path`'s BSD file flags to exactly `flags` (`chflags(2)`, which follows a link).
#[cfg(target_os = "macos")]
pub fn set_file_flags(path: &Path, flags: u32) -> io::Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, which reads it and nothing else.
    if unsafe { libc::chflags(c.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Open `path` for a fresh (create+truncate) write with initial permission `mode`.
pub fn open_create_write(path: &Path, mode: u32) -> io::Result<File> {
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: not a regular file", path.display()),
        ));
    }
    if meta.len() != 0 {
        f.set_len(0)?;
    }
    if meta.permissions().mode() & 0o7777 != mode & 0o7777 {
        f.set_permissions(Permissions::from_mode(mode))?;
    }
    Ok(f)
}

/// Create `path` exclusively (`O_CREAT|O_EXCL`) with mode `mode`, failing [`io::ErrorKind::AlreadyExists`] if it exists.
pub fn open_create_new_write(path: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Open `path` for [`sync_file_contents`].
pub fn open_for_sync(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

/// Push one open file's contents out of the page cache: a plain `fsync(2)`, retried on
/// `EINTR`, through aterm-durable (`primitive::fsync`), so astream-sys counts it `plain`.
pub fn sync_file_contents(f: &File) -> io::Result<()> {
    aterm_durable::primitive::fsync(f)
}

/// A file's permission bits (`st_mode`), as read by the tree-root hash and doctor.
#[must_use]
pub fn permission_mode(meta: &Metadata) -> u32 {
    meta.permissions().mode()
}

/// Whether this platform's filesystems store POSIX permission bits an `fstat` reads back.
pub const HAS_POSIX_MODES: bool = true;

/// The mode the extraction-time fold records for a file just written with `requested` bits.
#[must_use]
pub fn folded_mode(meta: &Metadata, requested: u32) -> u32 {
    let _ = requested;
    permission_mode(meta) & 0o7777
}

/// The raw OS bytes of an `OsStr` (no lossy conversion), for the tree-root path hash.
#[must_use]
pub fn os_str_bytes(s: &OsStr) -> &[u8] {
    s.as_bytes()
}

/// Free bytes on the volume holding `dir` (which must exist), or `None` on any `statvfs` failure.
#[must_use]
pub fn volume_free_bytes(dir: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid C string and `&mut st` is a writable `statvfs` out-parameter.
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

/// Atomically point `link` at `target`.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    static SYMLINK_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let file_name = link
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link has no file name"))?;
    let seq = SYMLINK_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = link.with_file_name(format!(".{file_name}.tmp-{}-{seq}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Install a bin shim at `shim` forwarding to `target`, exporting `env` first (empty: the plain shim).
pub fn install_shim_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    crate::lay::write_in_process(&shim_executable_to_env(shim, target, env)?)
}

/// The shim [`install_shim_to_env`] lays, rendered but not written.
pub fn shim_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<crate::lay::Executable> {
    let route = crate::compat::route_for_shim(shim, target);
    let mut body = super::sh_shim_content_routed(target, env, route.as_deref());
    if cfg!(target_os = "macos")
        && let Some(tool) = target.file_name().and_then(|word| word.to_str())
        && matches!(tool, "targo" | "targo-tippy")
    {
        let standing = crate::metadata_io::read_bounded_regular_utf8(shim, 64 << 10)
            .ok()
            .and_then(|text| crate::build_cache::registered::shim_helper(&text));
        let helper = crate::stub::Embedder::this_process()
            .embed(standing.as_deref())
            .or(standing);
        if let Some(helper) = helper.as_deref()
            && !crate::build_cache::registered::helper_speaks_hop(helper)
        {
            let call = crate::build_cache::registered::shim_call(helper, tool);
            body = body.replacen(
                "# aterm: build intermediates out of Spotlight\n",
                &format!("{call}# aterm: build intermediates out of Spotlight\n"),
                1,
            );
        } else if let Some(helper) = helper
            && let Some(block) = crate::build_cache::shim_block(tool)
            && let Some(at) = body.find(&block)
        {
            let target_q = super::sh_shim_quote(target);
            let routed = route.as_deref().and_then(|route| {
                let root = super::sh_shim_quote(route);
                let exec = format!("exec {root} \"$@\"");
                let line = body
                    .lines()
                    .find(|l| l.starts_with("[ ") && l.ends_with(&exec))?;
                Some((line[..line.len() - exec.len()].to_owned(), root))
            });
            let hop = crate::build_cache::registered::shim_hop(
                &helper,
                tool,
                &target_q,
                routed
                    .as_ref()
                    .map(|(tests, root)| (tests.as_str(), root.as_str())),
            );
            body.insert_str(at + block.len(), &hop);
        }
    }
    Ok(crate::lay::Executable::new(shim, body))
}

/// The `agents/` twin's shim.
pub fn twin_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<crate::lay::Executable> {
    let route = crate::compat::route_for_shim(shim, target);
    Ok(crate::lay::Executable::new(
        shim,
        super::sh_shim_content_twin(target, env, route.as_deref(), prelude),
    ))
}

/// Lay the `agents/` twin [`twin_executable_to_env`] renders.
pub fn install_twin_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<()> {
    crate::lay::write_in_process(&twin_executable_to_env(shim, target, env, prelude)?)
}

/// Install a failing tombstone shim at `shim`.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' {} 1>&2\nexit 70\n",
        crate::stub::sh_single_quote(message)
    );

    crate::lay::write_in_process(&crate::lay::Executable::new(shim, script.as_bytes()))
}

/// What a single read of `shim` ([`super::ShimRead`]) learns.
#[must_use]
pub fn read_shim(shim: &Path) -> super::ShimRead {
    match crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES) {
        Ok(body) => super::ShimRead {
            target: super::parse_sh_shim_target(&body),
            env: super::parse_sh_shim_env(&body),
            body: Some(body),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => super::ShimRead {
            target: None,
            env: crate::shim_env::ShimEnv::NONE,
            body: None,
        },
        Err(_) => super::ShimRead {
            target: fs::read_link(shim).ok(),
            env: crate::shim_env::ShimEnv::NONE,
            body: None,
        },
    }
}

/// Resolve the store/checkout target a `bin/<tool>` shim execs, or `None` for no shim or a tombstone (no `exec` line).
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    match crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES) {
        Ok(content) => super::parse_sh_shim_target(&content),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => fs::read_link(shim).ok(),
    }
}

/// The environment the shim at `shim` exports before it execs.
#[must_use]
pub fn shim_env_of(shim: &Path) -> crate::shim_env::ShimEnv {
    match crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES) {
        Ok(content) => super::parse_sh_shim_env(&content),
        Err(_) => crate::shim_env::ShimEnv::NONE,
    }
}

/// Replace the current process image with `command` (`execve`).
pub fn exec_or_run(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    command.exec()
}

/// SIGTERM and SIGHUP armed to pass on to the child [`wait_standing_aside`] waits on, from before that child is spawned ([`arm_forwarding`]).
pub struct Forwarding {
    previous: [(libc::c_int, libc::sighandler_t); 2],
    /// This process's turn at its signal dispositions ([`FORWARDING_TURN`]), held until they are
    /// put back.
    _turn: std::sync::MutexGuard<'static, ()>,
}

/// ONE FORWARDING AT A TIME: whatever arms this process's signal dispositions to forward them to a
/// child it waits on ([`arm_forwarding`], a build's front) holds this until it has put them back.
/// Two that overlapped would each keep what the other set as the disposition to restore, and the
/// one that ended last would leave the other's (a SIGINT ignored, a handler with no child) on the
/// process for good. A process runs one such wait (a lease, a front); a test binary runs many at
/// once.
pub static FORWARDING_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Spawn a test's child in this process's forwarding turn ([`FORWARDING_TURN`]), so it starts
/// with the SIGINT and SIGQUIT dispositions this process has outside any wait: a
/// [`wait_standing_aside`] ignores both for the whole process, only inside its turn, and a child
/// another test of the binary spawned meanwhile would start with them ignored and keep them so.
#[cfg(test)]
pub(crate) fn spawn_outside_a_stand_aside<T>(spawn: impl FnOnce() -> T) -> T {
    let _turn = FORWARDING_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    spawn()
}

/// Arm the SIGTERM and SIGHUP forwarding before the child is spawned, once no other forwarding is
/// armed in this process ([`FORWARDING_TURN`]).
#[must_use]
pub fn arm_forwarding() -> Forwarding {
    use std::sync::atomic::Ordering;
    let turn = FORWARDING_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    HELD.store(0, Ordering::SeqCst);
    FORWARD_TO.store(ARMING, Ordering::SeqCst);
    let forward = forward_to_child as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: `signal(2)` with a handler that makes only async-signal-safe calls
    // ([`forward_to_child`]); the previous dispositions are kept to be restored on drop.
    let previous = unsafe {
        [
            (libc::SIGTERM, libc::signal(libc::SIGTERM, forward)),
            (libc::SIGHUP, libc::signal(libc::SIGHUP, forward)),
        ]
    };
    for (sig, was) in &previous {
        if *was == libc::SIG_IGN {
            // SAFETY: puts back the `SIG_IGN` this process was started with.
            unsafe { libc::signal(*sig, libc::SIG_IGN) };
        }
    }
    Forwarding {
        previous,
        _turn: turn,
    }
}

impl Drop for Forwarding {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        FORWARD_TO.store(0, Ordering::SeqCst);
        for (sig, was) in self.previous {
            if was != libc::SIG_ERR {
                // SAFETY: puts back the disposition [`arm_forwarding`] replaced.
                unsafe { libc::signal(sig, was) };
            }
        }
        let held = HELD.swap(0, Ordering::SeqCst);
        if held != 0 {
            // SAFETY: delivers the held SIGTERM or SIGHUP to its restored disposition.
            unsafe {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "re-raises a SIGTERM or SIGHUP held for a child that never started: neither dumps core"
                )]
                libc::raise(held);
            }
        }
    }
}

/// Wait for `child` while this process stands aside from the terminal's interrupt and quit
/// keys, as `system(3)` does, passing SIGTERM and SIGHUP on to it (`forwarding`, armed
/// before its spawn).
///
/// # Errors
/// The wait's.
pub fn wait_standing_aside(
    child: &mut aterm_uds::spawnfd::TransparentChild,
    forwarding: Forwarding,
) -> io::Result<std::process::ExitStatus> {
    use std::sync::atomic::Ordering;
    let pid = libc::pid_t::try_from(child.id()).unwrap_or(0);
    FORWARD_TO.store(pid, Ordering::SeqCst);
    let held = HELD.swap(0, Ordering::SeqCst);
    if held != 0 && pid > 0 {
        // SAFETY: passes the held signal on to the child this process spawned.
        unsafe { libc::kill(pid, held) };
    }
    // SAFETY: `signal(2)` with `SIG_IGN`; the previous dispositions are restored below. Set
    // after the spawn, so the child does not inherit them.
    let previous = unsafe {
        [
            (libc::SIGINT, libc::signal(libc::SIGINT, libc::SIG_IGN)),
            (libc::SIGQUIT, libc::signal(libc::SIGQUIT, libc::SIG_IGN)),
        ]
    };
    let status = child.wait();
    let _ = FORWARD_TO.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    for (sig, was) in previous {
        if was != libc::SIG_ERR {
            // SAFETY: puts back the disposition this function replaced a moment ago.
            unsafe { libc::signal(sig, was) };
        }
    }
    drop(forwarding);
    status
}

/// [`FORWARD_TO`] while the forwarding is armed and the child's pid is not known yet.
const ARMING: libc::pid_t = -1;

/// The child [`wait_standing_aside`] passes SIGTERM and SIGHUP on to; [`ARMING`] before its
/// pid is known; `0` while it waits on none.
static FORWARD_TO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// A SIGTERM or SIGHUP that arrived while [`FORWARD_TO`] was [`ARMING`], to pass on once the
/// child's pid is known; `0` for none.
static HELD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// [`wait_standing_aside`]'s handler: the signal sent on to the child it waits on, held while
/// that child's pid is not known yet, or, with none (a handler outliving its wait), the
/// default action.
extern "C" fn forward_to_child(sig: libc::c_int) {
    use std::sync::atomic::Ordering;
    let pid = FORWARD_TO.load(Ordering::SeqCst);
    // SAFETY: `kill`, `signal` and `raise` are async-signal-safe, and so are lock-free atomics.
    unsafe {
        if pid > 0 {
            libc::kill(pid, sig);
        } else if pid == ARMING {
            HELD.store(sig, Ordering::SeqCst);
            let now = FORWARD_TO.load(Ordering::SeqCst);
            if now > 0 {
                let held = HELD.swap(0, Ordering::SeqCst);
                if held != 0 {
                    libc::kill(now, held);
                }
            }
        } else {
            libc::signal(sig, libc::SIG_DFL);
            #[expect(
                clippy::disallowed_methods,
                reason = "re-raises SIGTERM or SIGHUP (the two it forwards) with the default action: neither dumps core"
            )]
            libc::raise(sig);
        }
    }
}

/// Leave this process the way `status` says its child left.
#[must_use = "return or inspect the child exit status"]
pub fn exit_code_like(status: std::process::ExitStatus) -> std::process::ExitCode {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(code) = status.code() {
        return std::process::ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1));
    }
    let Some(sig) = status.signal() else {
        return std::process::ExitCode::from(1);
    };
    if matches!(
        sig,
        libc::SIGINT | libc::SIGTERM | libc::SIGHUP | libc::SIGPIPE
    ) {
        // SAFETY: the default action for these ends the process without a core; `raise`
        // delivers it to this thread before it returns.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            #[expect(
                clippy::disallowed_methods,
                reason = "re-raises SIGINT, SIGTERM, SIGHUP or SIGPIPE: none dumps core"
            )]
            libc::raise(sig);
        }
    }
    std::process::ExitCode::from(u8::try_from(128 + sig).unwrap_or(255))
}

#[cfg(test)]
#[path = "unix_forwarding_tests.rs"]
mod forwarding_tests;

/// The reading [`may_take_name`] makes off macOS, held to what Linux's `may_delete` refuses, on
/// every Unix this crate's tests run on.
#[cfg(test)]
mod take_name_tests {
    use super::{ATTR_APPEND, ATTR_IMMUTABLE, binds, sticky_keeps};

    /// AN IMMUTABLE OR APPEND-ONLY INODE KEEPS ITS NAME, as far as its filesystem reports the
    /// attribute: either bit set and reported binds; a bit the mask does not report, or another
    /// attribute (compressed 0x4, nodump 0x40), does not. NEGATIVE CONTROLS: the same bits with no
    /// mask, and no bits with the whole mask.
    #[test]
    fn an_immutable_or_append_only_inode_keeps_its_name() {
        let all = u64::MAX;
        assert!(binds((ATTR_IMMUTABLE, all)));
        assert!(binds((ATTR_APPEND, all)));
        assert!(binds((ATTR_IMMUTABLE | 0x4, ATTR_IMMUTABLE)));
        assert!(!binds((ATTR_IMMUTABLE | ATTR_APPEND, 0)), "not reported");
        assert!(
            !binds((ATTR_IMMUTABLE, ATTR_APPEND)),
            "another bit reported"
        );
        assert!(!binds((0x4 | 0x40, all)), "other attributes");
        assert!(!binds((0, all)));
    }

    /// A STICKY FOLDER KEEPS ANOTHER'S NAME: in a sticky folder the caller may take an entry's
    /// name only when the folder or the entry is theirs, or they are root. NEGATIVE CONTROL: the
    /// same folder without the sticky bit keeps nothing.
    #[test]
    fn a_sticky_folder_keeps_the_name_of_an_entry_neither_it_nor_the_entry_is_the_callers() {
        let (me, them, other) = (501, 502, 503);
        assert!(sticky_keeps(0o1777, them, other, me));
        assert!(!sticky_keeps(0o1777, me, other, me), "the folder is mine");
        assert!(!sticky_keeps(0o1777, them, me, me), "the entry is mine");
        assert!(!sticky_keeps(0o1777, them, other, 0), "root");
        assert!(!sticky_keeps(0o0777, them, other, me), "not sticky");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn the_rosetta_probe_reads_a_file_and_never_guesses_an_unsearchable_one() {
        let root = crate::testdir::dir("atpkg-rosetta-probe-");
        let runtime = root.join("oah").join("libRosettaRuntime");
        assert_eq!(rosetta_present(&runtime), Ok(false), "no parent");
        fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        assert_eq!(rosetta_present(&runtime), Ok(false), "nothing there");
        fs::create_dir(&runtime).unwrap();
        assert_eq!(rosetta_present(&runtime), Ok(false), "a directory");
        fs::remove_dir(&runtime).unwrap();
        fs::write(&runtime, b"\xcf\xfa\xed\xfe").unwrap();
        assert_eq!(rosetta_present(&runtime), Ok(true));
        let under_a_file = runtime.join("libRosettaRuntime");
        assert_eq!(rosetta_present(&under_a_file), Ok(false), "ENOTDIR");
        let oah = runtime.parent().unwrap();
        fs::set_permissions(oah, Permissions::from_mode(0o000)).unwrap();
        if our_uid() != 0 {
            assert!(rosetta_present(&runtime).is_err(), "EACCES is no answer");
        }
        fs::set_permissions(oah, Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_shared_dir_already_0755_is_not_rewritten_and_a_drifted_one_is_forced() {
        let root = crate::testdir::dir("atpkg-shared-dir-");
        let dir = root.join("prefix");
        let stamp = |p: &Path| {
            let m = fs::symlink_metadata(p).unwrap();
            (
                m.mode() & 0o7777,
                m.ctime(),
                m.ctime_nsec(),
                m.mtime(),
                m.mtime_nsec(),
            )
        };
        ensure_shared_dir(&dir).unwrap();
        let first = stamp(&dir);
        assert_eq!(first.0, 0o755, "created shared: {:o}", first.0);
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_shared_dir(&dir).unwrap();
        assert_eq!(
            stamp(&dir),
            first,
            "an unchanged 0755 directory was written"
        );
        for drifted in [0o700, 0o775, 0o1755] {
            fs::set_permissions(&dir, Permissions::from_mode(drifted)).unwrap();
            ensure_shared_dir(&dir).unwrap();
            assert_eq!(stamp(&dir).0, 0o755, "{drifted:o} not forced to 0755");
        }
        let link = root.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        let refused = ensure_shared_dir(&link);
        let named = stamp(&dir).0;
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
        assert_eq!(named, 0o700, "the refused link's target was chmodded");
    }

    #[test]
    fn the_account_home_resolves_on_the_machine_running_this_test() {
        let account = account_home().expect(
            "getpwuid_r must resolve this account's home; a None here disables the \
             [machine] doctor everywhere",
        );
        assert!(
            account.is_absolute(),
            "a passwd home is an absolute path: {}",
            account.display()
        );
        assert!(
            !account.as_os_str().is_empty(),
            "an empty pw_dir is not a home"
        );
        assert!(
            account.is_dir(),
            "the account home must exist: {}",
            account.display()
        );
        assert_eq!(account_home(), Some(account));
    }
}
