// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether an account other than this one can write a file or directory: any other-write bit,
//! or a group-write bit unless the group is this user's own private group and nothing past the
//! mode widens it.

use std::ffi::CStr;
use std::os::unix::fs::MetadataExt as _;

/// One account's passwd facts: its name and its primary group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: Vec<u8>,
    pub gid: u32,
}

/// One group's facts: its name and its supplementary members' names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: Vec<u8>,
    pub members: Vec<Vec<u8>>,
}

/// Whether an account other than this process's effective one can write the open `file`.
/// # Errors
/// The `fstat` that reads its owner, group and mode failed.
#[cfg_attr(trust_verify, trust::skip)]
pub fn others_can_write(file: &std::fs::File) -> std::io::Result<bool> {
    let md = file.metadata()?;
    Ok(judge(&md, acl::on_file(file), filesystem::of_file(file)))
}

/// [`others_can_write`] for what `path` itself names, a final symbolic link not followed.
/// # Errors
/// The `lstat` of `path` failed.
#[cfg_attr(trust_verify, trust::skip)]
pub fn others_can_write_at(path: &std::path::Path) -> std::io::Result<bool> {
    let md = std::fs::symlink_metadata(path)?;
    Ok(judge(&md, acl::at_path(path), filesystem::at_path(path)))
}

/// [`others_can_write_as`] for this process's effective uid over what `md` says, widened by
/// an access ACL (`acl`) or by a remote filesystem or one of no readable type (`magic`).
#[cfg_attr(trust_verify, trust::skip)]
fn judge(md: &std::fs::Metadata, acl: bool, magic: Option<u32>) -> bool {
    others_can_write_as(
        crate::peer::our_uid(),
        md.uid(),
        md.gid(),
        md.mode(),
        acl || remote(magic),
        is_user_private_group,
    )
}

/// Whether an account other than `me` can write a path owned by `owner` on group `gid` with `mode`.
#[must_use]
pub fn others_can_write_as(
    me: u32,
    owner: u32,
    gid: u32,
    mode: u32,
    widened: bool,
    private: impl FnOnce(u32) -> bool,
) -> bool {
    mode & 0o022 != 0 && !(mode & 0o002 == 0 && owner == me && !widened && private(gid))
}

const POSIX_ACL: &CStr = c"system.posix_acl_access";

const ACL_ATTRIBUTES: [&CStr; 2] = [POSIX_ACL, c"system.nfs4_acl"];

const ENODATA: i32 = 61;
const ENOTSUP: i32 = 95;

/// Whether a file carries an extended access ACL, based on `answer`'s attribute length queries.
#[must_use]
pub fn acl_carried(mut answer: impl FnMut(&CStr) -> Option<i32>) -> bool {
    ACL_ATTRIBUTES
        .iter()
        .any(|name| !matches!(answer(name), Some(ENODATA | ENOTSUP)))
}

/// The `statfs` `f_type` of each network and FUSE filesystem the rule knows.
pub const REMOTE_FILESYSTEMS: [u32; 10] = [
    0x6969,
    0xFF53_4D42,
    0xFE53_4D42,
    0x00C3_6400,
    0x6B41_4653,
    0x5346_414F,
    0x0102_1997,
    0x0BD0_0BD0,
    0x4750_4653,
    0x6573_5546,
];

/// Whether a filesystem of type `magic` (`statfs`'s `f_type`) is a network or FUSE one.
#[must_use]
pub fn remote_filesystem(magic: u32) -> bool {
    REMOTE_FILESYSTEMS.contains(&magic)
}

fn remote(magic: Option<u32>) -> bool {
    magic.is_none_or(remote_filesystem)
}

/// Whether group `gid` can hold no account but `uid`.
#[must_use]
pub fn user_private_group(
    uid: u32,
    gid: u32,
    account: Option<&Account>,
    group: Option<&Group>,
    primaries: &[u32],
) -> bool {
    let (Some(account), Some(group)) = (account, group) else {
        return false;
    };
    uid != 0
        && account.gid == gid
        && group.name == account.name
        && group.members.iter().all(|member| *member == account.name)
        && primaries.iter().all(|&other| other == uid)
}

/// [`user_private_group`] for this process's effective uid, over the system's account databases.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn is_user_private_group(gid: u32) -> bool {
    private_group_by(
        crate::peer::our_uid(),
        gid,
        nss::account,
        nss::group,
        nss::primary_members,
    )
}

fn private_group_by(
    uid: u32,
    gid: u32,
    account: impl FnOnce(u32) -> Option<Account>,
    group: impl FnOnce(u32) -> Option<Group>,
    primaries: impl FnOnce(u32) -> Option<Vec<u32>>,
) -> bool {
    let account = account(uid);
    let group = group(gid);
    user_private_group(uid, gid, account.as_ref(), group.as_ref(), &[])
        && primaries(gid).is_some_and(|primaries| {
            user_private_group(uid, gid, account.as_ref(), group.as_ref(), &primaries)
        })
}

/// The account databases, read through glibc's NSS.
#[cfg(target_os = "linux")]
mod nss {
    use super::{Account, Group};
    use std::ffi::{CStr, c_char, c_int};

    /// `<pwd.h>`'s `struct passwd` on glibc.
    #[repr(C)]
    struct PasswdRecord {
        pw_name: *mut c_char,
        pw_passwd: *mut c_char,
        pw_uid: u32,
        pw_gid: u32,
        pw_gecos: *mut c_char,
        pw_dir: *mut c_char,
        pw_shell: *mut c_char,
    }

    impl PasswdRecord {
        fn empty() -> Self {
            Self {
                pw_name: std::ptr::null_mut(),
                pw_passwd: std::ptr::null_mut(),
                pw_uid: 0,
                pw_gid: 0,
                pw_gecos: std::ptr::null_mut(),
                pw_dir: std::ptr::null_mut(),
                pw_shell: std::ptr::null_mut(),
            }
        }
    }

    /// `<grp.h>`'s `struct group` on glibc.
    #[repr(C)]
    struct GroupRecord {
        gr_name: *mut c_char,
        gr_passwd: *mut c_char,
        gr_gid: u32,
        gr_mem: *mut *mut c_char,
    }

    const ENOENT: c_int = 2;
    const ERANGE: c_int = 34;

    unsafe extern "C" {
        fn getpwuid_r(
            uid: u32,
            pwd: *mut PasswdRecord,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut PasswdRecord,
        ) -> c_int;
        fn getgrgid_r(
            gid: u32,
            grp: *mut GroupRecord,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut GroupRecord,
        ) -> c_int;
        fn setpwent();
        fn getpwent_r(
            pwd: *mut PasswdRecord,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut PasswdRecord,
        ) -> c_int;
        fn endpwent();
    }

    const RECORD_LIMIT: usize = 1 << 20;

    /// `uid`'s passwd name and primary group, or `None` when the system has no such account.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn account(uid: u32) -> Option<Account> {
        let mut size = 4096usize;
        while size <= RECORD_LIMIT {
            let mut buf = vec![0 as c_char; size];
            let mut record = PasswdRecord::empty();
            let mut found: *mut PasswdRecord = std::ptr::null_mut();
            // SAFETY: `record` and `found` are live out-parameters; `buf` is a live allocation of exactly `size` bytes that outlives the call and every pointer written into `record`, which is read only on success, before `buf` is dropped.
            let ret =
                unsafe { getpwuid_r(uid, &raw mut record, buf.as_mut_ptr(), size, &raw mut found) };
            match ret {
                0 if !found.is_null() && !record.pw_name.is_null() => {
                    // SAFETY: on success `pw_name` is a NUL-terminated string inside `buf`.
                    let name = unsafe { CStr::from_ptr(record.pw_name) }
                        .to_bytes()
                        .to_vec();
                    return Some(Account {
                        name,
                        gid: record.pw_gid,
                    });
                }
                ERANGE => size *= 2,
                _ => return None,
            }
        }
        None
    }

    /// Group `gid`'s name and members, or `None` when the system has no such group.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn group(gid: u32) -> Option<Group> {
        let mut size = 4096usize;
        while size <= RECORD_LIMIT {
            let mut buf = vec![0 as c_char; size];
            let mut record = GroupRecord {
                gr_name: std::ptr::null_mut(),
                gr_passwd: std::ptr::null_mut(),
                gr_gid: 0,
                gr_mem: std::ptr::null_mut(),
            };
            let mut found: *mut GroupRecord = std::ptr::null_mut();
            // SAFETY: as in [`account`]: live out-parameters, and a `size`-byte buffer that outlives every pointer read out of `record` below.
            let ret =
                unsafe { getgrgid_r(gid, &raw mut record, buf.as_mut_ptr(), size, &raw mut found) };
            match ret {
                0 if !found.is_null() && !record.gr_name.is_null() => {
                    // SAFETY: on success `gr_name` is a NUL-terminated string inside `buf`.
                    let name = unsafe { CStr::from_ptr(record.gr_name) }
                        .to_bytes()
                        .to_vec();
                    let mut members = Vec::new();
                    let mut at = record.gr_mem;
                    // SAFETY: on success `gr_mem` is null or a null-terminated array, inside `buf`, of NUL-terminated strings inside `buf`.
                    unsafe {
                        while !at.is_null() && !(*at).is_null() {
                            members.push(CStr::from_ptr(*at).to_bytes().to_vec());
                            at = at.add(1);
                        }
                    }
                    return Some(Group { name, members });
                }
                ERANGE => size *= 2,
                _ => return None,
            }
        }
        None
    }

    /// What one step of the passwd enumeration answered.
    #[derive(Clone, Copy, Debug)]
    pub(super) enum Step {
        /// An entry: its uid and its primary group.
        Entry { uid: u32, gid: u32 },
        /// The end of the enumeration (`ENOENT`).
        End,
        /// The record buffer it was handed cannot hold the next entry (`ERANGE`).
        TooSmall,
        /// Any other answer.
        Failed,
    }

    /// Collect uids whose primary group is `gid` across the passwd enumeration `next`.
    pub(super) fn walk(gid: u32, mut next: impl FnMut(usize) -> Step) -> Option<Vec<u32>> {
        let mut size = 4096usize;
        let mut uids = Vec::new();
        loop {
            match next(size) {
                Step::Entry { uid, gid: primary } => {
                    if primary == gid {
                        uids.push(uid);
                    }
                }
                Step::End => return Some(uids),
                Step::TooSmall if size < RECORD_LIMIT => size *= 2,
                Step::TooSmall | Step::Failed => return None,
            }
        }
    }

    static ENUMERATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The uids of every passwd entry whose primary group is `gid`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn primary_members(gid: u32) -> Option<Vec<u32>> {
        let _walk = ENUMERATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut buf: Vec<c_char> = Vec::new();
        // SAFETY: the enumeration is serialized by `ENUMERATION`, and the stream is closed below, before the lock is released.
        unsafe { setpwent() };
        let uids = walk(gid, |size| {
            buf.resize(size, 0);
            let mut record = PasswdRecord::empty();
            let mut found: *mut PasswdRecord = std::ptr::null_mut();
            // SAFETY: `record` and `found` are live out-parameters and `buf` a live allocation of exactly `size` bytes; only the record's integers are read, on success, before the next call reuses `buf`.
            let ret =
                unsafe { getpwent_r(&raw mut record, buf.as_mut_ptr(), size, &raw mut found) };
            match ret {
                0 if !found.is_null() => Step::Entry {
                    uid: record.pw_uid,
                    gid: record.pw_gid,
                },
                ENOENT => Step::End,
                ERANGE => Step::TooSmall,
                _ => Step::Failed,
            }
        });
        // SAFETY: closes the stream opened above, still under `ENUMERATION`.
        unsafe { endpwent() };
        uids
    }
}

/// Whether a file carries an extended access ACL ([`super::acl_carried`]), asked of the file
/// itself: the length of each ACL attribute.
#[cfg(target_os = "linux")]
mod acl {
    use std::ffi::{c_char, c_int, c_void};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    unsafe extern "C" {
        fn fgetxattr(fd: c_int, name: *const c_char, value: *mut c_void, size: usize) -> isize;
        fn lgetxattr(
            path: *const c_char,
            name: *const c_char,
            value: *mut c_void,
            size: usize,
        ) -> isize;
    }

    fn answered(ret: isize) -> Option<i32> {
        if ret >= 0 {
            None
        } else {
            Some(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    }

    /// Whether the open `file` carries an extended access ACL.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn on_file(file: &std::fs::File) -> bool {
        super::acl_carried(|name| {
            // SAFETY: a live descriptor and a NUL-terminated name; a null value with a zero size asks only the attribute's length, so nothing is written.
            answered(unsafe { fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) })
        })
    }

    /// Whether what `path` itself names carries an extended access ACL.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn at_path(path: &std::path::Path) -> bool {
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return true;
        };
        super::acl_carried(|name| {
            // SAFETY: as in [`on_file`], over a NUL-terminated path.
            answered(unsafe { lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) })
        })
    }

    /// [`super::grant_group_class_by_acl`].
    #[cfg(any(test, debug_assertions))]
    pub(super) fn grant_group_class(path: &std::path::Path, uid: u32) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        unsafe extern "C" {
            fn setxattr(
                path: *const c_char,
                name: *const c_char,
                value: *const c_void,
                size: usize,
                flags: c_int,
            ) -> c_int;
        }
        const UNDEFINED: u32 = u32::MAX;
        let mode = std::fs::metadata(path)?.mode();
        let class =
            |shift: u32| u16::try_from(mode.checked_shr(shift).unwrap_or(0) & 0o7).unwrap_or(0);
        let mut blob = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (0x01u16, class(6), UNDEFINED),
            (0x02, class(3), uid),
            (0x04, class(3), UNDEFINED),
            (0x10, class(3), UNDEFINED),
            (0x20, class(0), UNDEFINED),
        ] {
            blob.extend_from_slice(&tag.to_le_bytes());
            blob.extend_from_slice(&perm.to_le_bytes());
            blob.extend_from_slice(&id.to_le_bytes());
        }
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path and name, and a value of exactly `blob.len()` bytes, all live across the call, which only reads them.
        let ret = unsafe {
            setxattr(
                path.as_ptr(),
                super::POSIX_ACL.as_ptr(),
                blob.as_ptr().cast(),
                blob.len(),
                0,
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// The type of the filesystem a file is on (`statfs`'s `f_type`), for [`super::remote`].
#[cfg(target_os = "linux")]
mod filesystem {
    use std::ffi::{c_char, c_int};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    /// `<sys/vfs.h>`'s `struct statfs` as glibc lays it out on x86_64 and aarch64, aterm's
    /// Linux targets: `f_type`, a 64-bit `long`, first, then room for the fields after it,
    /// which nothing here reads.
    #[repr(C)]
    struct Statfs {
        f_type: i64,
        _room: [i64; 31],
    }

    impl Statfs {
        fn empty() -> Self {
            Self {
                f_type: 0,
                _room: [0; 31],
            }
        }

        fn magic(&self, ret: c_int) -> Option<u32> {
            if ret == 0 {
                u32::try_from(self.f_type & 0xFFFF_FFFF).ok()
            } else {
                None
            }
        }
    }

    unsafe extern "C" {
        fn statfs(path: *const c_char, buf: *mut Statfs) -> c_int;
        fn fstatfs(fd: c_int, buf: *mut Statfs) -> c_int;
    }

    /// The type of the filesystem the open `file` is on, or `None` when it cannot be read.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn of_file(file: &std::fs::File) -> Option<u32> {
        let mut buf = Statfs::empty();
        // SAFETY: a live descriptor, and a live buffer larger than glibc's `struct statfs`, which the call only writes.
        let ret = unsafe { fstatfs(file.as_raw_fd(), &raw mut buf) };
        buf.magic(ret)
    }

    /// The type of the filesystem `path` is on, or `None` when it cannot be read.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn at_path(path: &std::path::Path) -> Option<u32> {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut buf = Statfs::empty();
        // SAFETY: as in [`of_file`], over a NUL-terminated path.
        let ret = unsafe { statfs(path.as_ptr(), &raw mut buf) };
        buf.magic(ret)
    }
}

/// Off Linux nothing is read: no account and no group, so no group is anyone's private one.
#[cfg(not(target_os = "linux"))]
mod nss {
    use super::{Account, Group};

    pub(super) const fn account(_uid: u32) -> Option<Account> {
        None
    }

    pub(super) const fn group(_gid: u32) -> Option<Group> {
        None
    }

    pub(super) const fn primary_members(_gid: u32) -> Option<Vec<u32>> {
        None
    }
}

/// Off Linux the ACL is not read; it answers "may carry one", which grants no exemption
/// (and none is asked for, since no group is private there).
#[cfg(not(target_os = "linux"))]
mod acl {
    pub(super) const fn on_file(_file: &std::fs::File) -> bool {
        true
    }

    pub(super) const fn at_path(_path: &std::path::Path) -> bool {
        true
    }
}

/// Off Linux the filesystem type is not read; it answers "unknown", which grants no
/// exemption.
#[cfg(not(target_os = "linux"))]
mod filesystem {
    pub(super) const fn of_file(_file: &std::fs::File) -> Option<u32> {
        None
    }

    pub(super) const fn at_path(_path: &std::path::Path) -> Option<u32> {
        None
    }
}

/// Test seam (Linux): grant account `uid` `path`'s group-class permissions via a POSIX access ACL.
/// # Errors
/// The `stat` or the `setxattr` failed (`ENOTSUP` on a filesystem that keeps no ACLs).
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
pub fn grant_group_class_by_acl(path: &std::path::Path, uid: u32) -> std::io::Result<()> {
    acl::grant_group_class(path, uid)
}

/// The type of the filesystem `path` is on (`statfs`'s `f_type`), as the judge reads it, or
/// `None` when it cannot be read.
#[cfg(target_os = "linux")]
pub(crate) fn filesystem_at(path: &std::path::Path) -> Option<u32> {
    filesystem::at_path(path)
}

/// The type of the filesystem the open `file` is on (`statfs`'s `f_type`), or `None` when it
/// cannot be read.
#[cfg(target_os = "linux")]
pub(crate) fn filesystem_of(file: &std::fs::File) -> Option<u32> {
    filesystem::of_file(file)
}

/// Test seam (Linux): the type of the filesystem `path` is on, as the judge reads it.
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
#[must_use]
pub fn filesystem_type_at(path: &std::path::Path) -> Option<u32> {
    filesystem::at_path(path)
}

/// Test seam (Linux): return the passwd uids whose primary group is `gid`.
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
#[must_use]
pub fn primary_members(gid: u32) -> Option<Vec<u32>> {
    nss::primary_members(gid)
}

#[cfg(test)]
#[path = "shared_write_tests.rs"]
mod tests;
