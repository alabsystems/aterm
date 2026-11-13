// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::identity`] on the live system, read in this process: the account database
//! (`getpwuid_r`, and `getgrnam_r` for the `admin` group's members, what `dscl` reads as its
//! `GroupMembership`), `hw.machine` (what `uname -m` prints) and `kern.osproductversion`
//! (what `sw_vers -productVersion` prints), with no child process. Elsewhere (Linux) the
//! administrators are the `sudo` and `wheel` groups' members, and `uname(2)` gives the
//! machine word and the kernel's release.

use super::{Identity, User};
use std::ffi::{CStr, c_char};
use std::io;

/// The account behind `uid` and this machine's architecture and macOS version.
pub(super) fn read(uid: u32) -> Result<Identity, String> {
    Ok(Identity {
        user: account(uid)?,
        arch: arch()?,
        os_version: os_version()?,
    })
}

/// The text a database field points at, or `None` for a null or non-UTF-8 field.
///
/// # Safety
/// `field` is null or points at a NUL-terminated string that outlives the call.
unsafe fn text(field: *const c_char) -> Option<String> {
    if field.is_null() {
        return None;
    }
    // SAFETY: the caller's contract: a non-null `field` is NUL-terminated and alive.
    unsafe { CStr::from_ptr(field) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// The largest buffer a reentrant account lookup grows to ([`grown`]): an entry that does not
/// fit in it is an error.
const LOOKUP_CEILING: usize = 1 << 20;

/// Run the reentrant lookup `call` with a buffer of `start` bytes, doubling it while `call`
/// answers `ERANGE` (the entry does not fit) up to [`LOOKUP_CEILING`]: `call`'s answer, or the
/// error code it last returned.
fn grown<T>(
    start: usize,
    mut call: impl FnMut(&mut [u8]) -> Result<T, libc::c_int>,
) -> Result<T, libc::c_int> {
    let mut len = start;
    loop {
        let mut buf = vec![0_u8; len];
        match call(&mut buf) {
            Err(libc::ERANGE) if len < LOOKUP_CEILING => {
                len = len.saturating_mul(2).min(LOOKUP_CEILING);
            }
            answer => return answer,
        }
    }
}

fn account(uid: u32) -> Result<User, String> {
    let found = grown(16 * 1024, |buf| {
        // SAFETY: `pw` and `buf` are buffers handed to the reentrant `getpwuid_r`, which writes
        // the entry's strings into `buf` and points `pw`'s fields at them; the fields are read
        // only when the call succeeded and found an entry, while `buf` is still alive.
        unsafe {
            let mut pw: libc::passwd = std::mem::zeroed();
            let mut out: *mut libc::passwd = std::ptr::null_mut();
            let rc = libc::getpwuid_r(
                uid,
                &raw mut pw,
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
                &raw mut out,
            );
            if rc != 0 {
                return Err(rc);
            }
            if out.is_null() {
                return Ok(None);
            }
            Ok(Some((
                text(pw.pw_name),
                pw.pw_gid,
                text(pw.pw_dir),
                text(pw.pw_shell),
            )))
        }
    })
    .map_err(|rc| {
        format!(
            "the account database could not be read for uid {uid}: {}",
            io::Error::from_raw_os_error(rc)
        )
    })?;
    let Some((name, gid, home, shell)) = found else {
        return Err(format!(
            "the account database names no account for uid {uid}"
        ));
    };
    let name = name.ok_or_else(|| format!("uid {uid}'s account has no readable name"))?;
    let home = home
        .filter(|home| home.starts_with('/'))
        .ok_or_else(|| format!("account {name}: no absolute home directory"))?;
    let mut admin = cfg!(target_os = "macos") && gid == 80;
    for group in ADMIN_GROUPS {
        admin = admin || members(group)?.contains(&name);
    }
    Ok(User {
        name,
        uid,
        gid,
        home,
        shell: shell.unwrap_or_default(),
        admin,
    })
}

/// `<grp.h>`'s `struct group` (macOS and glibc lay it out alike), which aterm's `libc` does not
/// declare on macOS.
#[repr(C)]
struct GroupRecord {
    gr_name: *mut c_char,
    gr_passwd: *mut c_char,
    gr_gid: u32,
    gr_mem: *mut *mut c_char,
}

unsafe extern "C" {
    fn getgrnam_r(
        name: *const c_char,
        grp: *mut GroupRecord,
        buf: *mut c_char,
        buflen: usize,
        result: *mut *mut GroupRecord,
    ) -> libc::c_int;
}

/// The groups whose members administer this machine.
#[cfg(target_os = "macos")]
const ADMIN_GROUPS: [&CStr; 1] = [c"admin"];
#[cfg(not(target_os = "macos"))]
const ADMIN_GROUPS: [&CStr; 2] = [c"sudo", c"wheel"];

/// `group`'s members, as the account database lists them (none without the group).
fn members(group_name: &CStr) -> Result<Vec<String>, String> {
    grown(64 * 1024, |buf| {
        let mut group = GroupRecord {
            gr_name: std::ptr::null_mut(),
            gr_passwd: std::ptr::null_mut(),
            gr_gid: 0,
            gr_mem: std::ptr::null_mut(),
        };
        let mut out: *mut GroupRecord = std::ptr::null_mut();
        // SAFETY: as in `account`: a record and a buffer handed to the reentrant `getgrnam_r`,
        // which points the record's fields into `buf`; the name is NUL-terminated.
        let rc = unsafe {
            getgrnam_r(
                group_name.as_ptr(),
                &raw mut group,
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
                &raw mut out,
            )
        };
        if rc != 0 {
            return Err(rc);
        }
        let mut members = Vec::new();
        if out.is_null() || group.gr_mem.is_null() {
            return Ok(members);
        }
        let mut at = group.gr_mem;
        // SAFETY: on success `gr_mem` is a null-terminated array of NUL-terminated strings in
        // `buf`, which lives until this closure returns.
        unsafe {
            while !(*at).is_null() {
                if let Some(member) = text(*at) {
                    members.push(member);
                }
                at = at.add(1);
            }
        }
        Ok(members)
    })
    .map_err(|rc| {
        format!(
            "the {} group could not be read: {}",
            group_name.to_string_lossy(),
            io::Error::from_raw_os_error(rc)
        )
    })
}

/// The machine word (`arm64`), what `uname -m` prints for this process.
#[cfg(target_os = "macos")]
fn arch() -> Result<String, String> {
    sysctl_text(c"hw.machine")
}

/// macOS's product version (`27.0`), what `sw_vers -productVersion` prints.
#[cfg(target_os = "macos")]
fn os_version() -> Result<String, String> {
    sysctl_text(c"kern.osproductversion")
}

/// The machine word (`aarch64`), what `uname -m` prints.
#[cfg(not(target_os = "macos"))]
fn arch() -> Result<String, String> {
    uname(|names| &names.machine)
}

/// The kernel's release (`6.8.0-45-generic`), what `uname -r` prints.
#[cfg(not(target_os = "macos"))]
fn os_version() -> Result<String, String> {
    uname(|names| &names.release)
}

/// One field of `uname(2)`, NUL- and space-trimmed.
#[cfg(not(target_os = "macos"))]
fn uname(field: impl Fn(&libc::utsname) -> &[c_char]) -> Result<String, String> {
    // SAFETY: `utsname` is plain data `uname` fills; it is read only when the call succeeded.
    let mut names: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `names` is a writable `utsname` that outlives the call.
    if unsafe { libc::uname(&raw mut names) } != 0 {
        return Err(format!("uname: {}", io::Error::last_os_error()));
    }
    let bytes: Vec<u8> = field(&names)
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| c.to_ne_bytes()[0])
        .collect();
    String::from_utf8(bytes)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "uname gave no readable name".into())
}

/// A string sysctl of at most 63 bytes, NUL- and space-trimmed.
#[cfg(target_os = "macos")]
fn sysctl_text(name: &CStr) -> Result<String, String> {
    let mut buf = [0u8; 64];
    let mut len: libc::size_t = buf.len();
    // SAFETY: `name` is NUL-terminated; `buf` is writable for `len` bytes, the kernel writes at
    // most `len` and reports how many in `len`.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    let shown = name.to_string_lossy();
    if rc != 0 {
        return Err(format!("sysctl {shown}: {}", io::Error::last_os_error()));
    }
    let bytes = &buf[..len.min(buf.len())];
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("sysctl {shown} is empty"))
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
