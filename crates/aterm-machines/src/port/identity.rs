// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::identity`] on the live system, read in this process: the account database
//! (`getpwuid_r`, and `getgrnam_r` for the `admin` group's members, what `dscl` reads as its
//! `GroupMembership`), `hw.machine` (what `uname -m` prints) and `kern.osproductversion`
//! (what `sw_vers -productVersion` prints), with no child process.

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

fn account(uid: u32) -> Result<User, String> {
    // SAFETY: `pw` and `buf` are buffers handed to the reentrant `getpwuid_r`, which writes the
    // entry's strings into `buf` and points `pw`'s fields at them; the fields are read only when
    // the call succeeded and found an entry, while `buf` is still alive.
    let (name, gid, home, shell) = unsafe {
        let mut pw: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0u8; 16 * 1024];
        let mut out: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            &raw mut pw,
            buf.as_mut_ptr().cast::<c_char>(),
            buf.len(),
            &raw mut out,
        );
        if rc != 0 {
            return Err(format!(
                "the account database could not be read for uid {uid}: {}",
                io::Error::from_raw_os_error(rc)
            ));
        }
        if out.is_null() {
            return Err(format!(
                "the account database names no account for uid {uid}"
            ));
        }
        (
            text(pw.pw_name),
            pw.pw_gid,
            text(pw.pw_dir),
            text(pw.pw_shell),
        )
    };
    let name = name.ok_or_else(|| format!("uid {uid}'s account has no readable name"))?;
    let home = home
        .filter(|home| home.starts_with('/'))
        .ok_or_else(|| format!("account {name}: no absolute home directory"))?;
    let admin = gid == 80 || admin_members()?.contains(&name);
    Ok(User {
        name,
        uid,
        gid,
        home,
        shell: shell.unwrap_or_default(),
        admin,
    })
}

/// `<grp.h>`'s `struct group` on macOS, which aterm's `libc` does not declare.
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

/// The `admin` group's members, as the account database lists them (none without the group).
fn admin_members() -> Result<Vec<String>, String> {
    let mut group = GroupRecord {
        gr_name: std::ptr::null_mut(),
        gr_passwd: std::ptr::null_mut(),
        gr_gid: 0,
        gr_mem: std::ptr::null_mut(),
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut out: *mut GroupRecord = std::ptr::null_mut();
    // SAFETY: as in `account`: a record and a buffer handed to the reentrant `getgrnam_r`, which
    // points the record's fields into `buf`; the name is NUL-terminated.
    let rc = unsafe {
        getgrnam_r(
            c"admin".as_ptr(),
            &raw mut group,
            buf.as_mut_ptr().cast::<c_char>(),
            buf.len(),
            &raw mut out,
        )
    };
    if rc != 0 {
        return Err(format!(
            "the admin group could not be read: {}",
            io::Error::from_raw_os_error(rc)
        ));
    }
    let mut members = Vec::new();
    if out.is_null() || group.gr_mem.is_null() {
        return Ok(members);
    }
    let mut at = group.gr_mem;
    // SAFETY: on success `gr_mem` is a null-terminated array of NUL-terminated strings in `buf`,
    // which lives until the end of this function.
    unsafe {
        while !(*at).is_null() {
            if let Some(member) = text(*at) {
                members.push(member);
            }
            at = at.add(1);
        }
    }
    Ok(members)
}

/// The machine word (`arm64`), what `uname -m` prints for this process.
fn arch() -> Result<String, String> {
    sysctl_text(c"hw.machine")
}

/// macOS's product version (`27.0`), what `sw_vers -productVersion` prints.
fn os_version() -> Result<String, String> {
    sysctl_text(c"kern.osproductversion")
}

/// A string sysctl of at most 63 bytes, NUL- and space-trimmed.
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
mod tests {
    /// Read only: this process's own account and the host's facts, beside what `id`, `uname`
    /// and `sw_vers` print for them.
    #[test]
    fn the_live_identity_is_what_the_tools_print() {
        // SAFETY: `geteuid` reads this process's effective uid and cannot fail.
        let uid = unsafe { libc::geteuid() };
        let identity = super::read(uid).unwrap();
        let tool = |program: &str, arg: &str| {
            let out = aterm_uds::spawnfd::command(program)
                .arg(arg)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        assert_eq!(identity.user.name, tool("/usr/bin/id", "-un"));
        assert_eq!(identity.user.uid, uid);
        assert_eq!(identity.arch, tool("/usr/bin/uname", "-m"));
        assert_eq!(
            identity.os_version,
            tool("/usr/bin/sw_vers", "-productVersion")
        );
        assert!(identity.user.home.starts_with('/'), "{identity:?}");
    }
}
