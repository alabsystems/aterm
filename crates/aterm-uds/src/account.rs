// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account home directory, login name, and login shell from the account database. On Windows only
//! [`home`]: the user's profile folder as the shell knows it, never `USERPROFILE`.

use std::path::PathBuf;

/// Returns this account's home directory from `getpwuid_r(getuid())`.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn home() -> Option<PathBuf> {
    // SAFETY: `getuid` reads this process's real uid and cannot fail.
    home_of(unsafe { libc::getuid() })
}

/// Returns this account's profile folder (`SHGetKnownFolderPath(FOLDERID_Profile)`), read from
/// the user's profile, never from `USERPROFILE`: a process handed a scratch `USERPROFILE` (which
/// `aterm_types::dirs::home_dir` reads on Windows) still reads its login's own folder here.
/// `None` where the shell answers none (a user with no profile loaded).
#[cfg(windows)]
#[must_use]
pub fn home() -> Option<PathBuf> {
    use crate::win::ffi;
    use std::os::windows::ffi::OsStringExt as _;
    let profile = ffi::FOLDERID_PROFILE;
    let mut path: *mut u16 = std::ptr::null_mut();
    // SAFETY: the folder id is a live local; flags 0 and a null token ask for this process's
    // user; `path` receives a buffer the shell allocated, freed below whatever the answer.
    let hr = unsafe { ffi::SHGetKnownFolderPath(&raw const profile, 0, 0, &raw mut path) };
    let folder = (hr >= 0 && !path.is_null()).then(|| {
        // SAFETY: on success `path` is a NUL-terminated wide string the shell wrote; the scan
        // stops at its NUL, within the buffer.
        let len = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();
        // SAFETY: `len` wide characters precede the NUL, all within the buffer.
        std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(path, len) })
    });
    // SAFETY: the shell's buffer (or null), freed once; the caller frees it on failure too.
    unsafe { ffi::CoTaskMemFree(path.cast()) };
    folder
        .map(PathBuf::from)
        .filter(|folder| folder.is_absolute())
}

/// Returns the home directory the account database names for `uid`.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn home_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: `pw` and `buf` are stack buffers handed to the reentrant `getpwuid_r`, which writes
    // the entry's strings into `buf` and points `pw`'s fields at them; `pw_dir` is read only when
    // the call succeeded and found an entry, while `buf` is still alive.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(pw.pw_dir).to_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
}

/// Returns the login name for `uid` from the account database.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn user_name(uid: u32) -> Option<String> {
    // SAFETY: same contract as `home_of`: stack buffers handed to the reentrant call, `pw_name`
    // read
    // only on success while `buf` lives.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_name.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(pw.pw_name).to_str().ok()?;
        (!name.is_empty()).then(|| name.to_string())
    }
}

/// Returns this account's login shell from `getpwuid_r(getuid())`.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell() -> Option<PathBuf> {
    // SAFETY: `getuid` reads this process's real uid and cannot fail.
    shell_of(unsafe { libc::getuid() })
}

/// Returns the login shell the account database names for `uid`.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn shell_of(uid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: as in `home_of`: stack buffers handed to the reentrant `getpwuid_r`, and `pw_shell`
    // read only when the call succeeded and found an entry, while `buf` lives.
    unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = [0u8; 4096];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 || out.is_null() || pw.pw_shell.is_null() {
            return None;
        }
        let bytes = std::ffi::CStr::from_ptr(pw.pw_shell).to_bytes();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
}

/// The group macOS gives its administrators (`admin`), whose members its `sudo` admits.
#[cfg(target_os = "macos")]
pub const ADMIN_GID: u32 = 80;

/// Whether this process's account is one of the Mac's administrators, the accounts `sudo`
/// admits: its groups (`getgroups`, as its login set them) hold `admin` ([`ADMIN_GID`]). Read
/// without root; `None` where the groups do not read, and off macOS.
#[cfg(unix)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn administrator() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a count query: a zero size and a null buffer, which the call never writes.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let len = usize::try_from(count).ok().filter(|n| *n > 0)?;
        let mut groups = vec![0 as libc::gid_t; len];
        // SAFETY: `groups` is a writable buffer of exactly the `count` entries the call is told.
        let got = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        let got = usize::try_from(got).ok()?;
        Some(groups.get(..got)?.contains(&ADMIN_GID))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
