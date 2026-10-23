// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether an account other than this one can write a file or directory: any other-write bit, or a group-write bit unless the group is this user's own private group and nothing past the mode widens it.

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

/// Whether an account other than this process's effective one can write the open `file`: [`others_can_write_as`] over its owner, group and mode, whether it carries an access ACL and the type of the filesystem it is on, all read from this one descriptor, with this system's account databases saying which group is this user's private one.
/// # Errors
/// The `fstat` that reads its owner, group and mode failed.
#[cfg_attr(trust_verify, trust::skip)]
pub fn others_can_write(file: &std::fs::File) -> std::io::Result<bool> {
    let md = file.metadata()?;
    Ok(judge(&md, acl::on_file(file), filesystem::of_file(file)))
}

/// [`others_can_write`] for what `path` itself names, a final symbolic link not followed (`lstat`, `lgetxattr`): the install-path walk judges each component as it stands.
/// # Errors
/// The `lstat` of `path` failed.
#[cfg_attr(trust_verify, trust::skip)]
pub fn others_can_write_at(path: &std::path::Path) -> std::io::Result<bool> {
    let md = std::fs::symlink_metadata(path)?;
    Ok(judge(&md, acl::at_path(path), filesystem::at_path(path)))
}

/// [`others_can_write_as`] for this process's effective uid over what `md` says, widened by
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

/// Whether an account other than `me` can write a file or directory `owner` owns on group `gid` with `mode`, `widened` saying whether something past the mode may let other accounts write it (an extended access ACL, whose mask its group bits would be, or a remote filesystem): any other-write bit, or a group-write bit unless the file is `me`'s, is not widened, and `private` says `gid` is `me`'s private group.
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

/// The POSIX access ACL's attribute: ext4, XFS, Btrfs and tmpfs keep it, and NFSv3 reads it
const POSIX_ACL: &CStr = c"system.posix_acl_access";

/// The extended-ACL attributes Linux exposes: [`POSIX_ACL`], and the NFSv4 ACL an NFSv4 mount
const ACL_ATTRIBUTES: [&CStr; 2] = [POSIX_ACL, c"system.nfs4_acl"];

/// `<errno.h>`'s `ENODATA` on Linux (the asm-generic value aarch64, x86_64, arm and riscv
const ENODATA: i32 = 61;
/// `<errno.h>`'s `ENOTSUP` (`EOPNOTSUPP`) on Linux, as [`ENODATA`]: the filesystem keeps no
const ENOTSUP: i32 = 95;

/// Whether a file carries an extended access ACL, by what `answer` says the length query of each ACL attribute returned: `None` it exists, `Some(errno)` it failed.
#[must_use]
pub fn acl_carried(mut answer: impl FnMut(&CStr) -> Option<i32>) -> bool {
    ACL_ATTRIBUTES
        .iter()
        .any(|name| !matches!(answer(name), Some(ENODATA | ENOTSUP)))
}

/// The `statfs` `f_type` of each network and FUSE filesystem the rule knows: NFS, SMB/CIFS (both of its magics), Ceph, AFS (kAFS and OpenAFS), 9P, Lustre, GPFS and FUSE (`fuseblk` and virtiofs included), from `<linux/magic.h>` and Lustre's and GPFS's own.
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

/// Whether a filesystem of type `magic` (`statfs`'s `f_type`) is a network or FUSE one ([`REMOTE_FILESYSTEMS`]), where a server or daemon decides who can write.
#[must_use]
pub fn remote_filesystem(magic: u32) -> bool {
    REMOTE_FILESYSTEMS.contains(&magic)
}

/// Whether the filesystem whose type `magic` read is remote, an unreadable type (`None`)
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

/// [`user_private_group`] for this process's effective uid, over the system's account databases (NSS: `getpwuid_r`, `getgrgid_r`, then the passwd enumeration, `getpwent_r`).
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

/// [`user_private_group`] over what `account`, `group` and `primaries` read for `uid` and
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
        /// A record for a lookup to fill.
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

    /// `<errno.h>`'s `ENOENT` on Linux: `getpwent_r`'s end of the enumeration.
    const ENOENT: c_int = 2;
    /// `<errno.h>`'s `ERANGE` on Linux: the record buffer is too small.
    const ERANGE: c_int = 34;

    unsafe extern "C" {
        /// `<pwd.h>`: the reentrant `getpwuid`.
        fn getpwuid_r(
            uid: u32,
            pwd: *mut PasswdRecord,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut PasswdRecord,
        ) -> c_int;
        /// `<grp.h>`: the reentrant `getgrgid`.
        fn getgrgid_r(
            gid: u32,
            grp: *mut GroupRecord,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut GroupRecord,
        ) -> c_int;
        /// `<pwd.h>`: rewind the passwd enumeration.
        fn setpwent();
        /// `<pwd.h>`: the next passwd entry, into the caller's record and buffer: 0, `ENOENT`
        fn getpwent_r(
            pwd: *mut PasswdRecord,
            buf: *mut c_char,
            buflen: usize,
            result: *mut *mut PasswdRecord,
        ) -> c_int;
        /// `<pwd.h>`: close the passwd enumeration.
        fn endpwent();
    }

    /// The largest record buffer a lookup grows to before it gives no answer.
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

    /// The uids of every entry whose primary group is `gid`, over an enumeration that `next` steps with a record buffer of the size it is handed, or `None` when the walk fails: any answer but an entry or the end, or an entry no buffer up to [`RECORD_LIMIT`] holds.
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

    /// The passwd enumeration's one owner in this process: `setpwent`, `getpwent_r` and
    static ENUMERATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The uids of every passwd entry whose primary group is `gid`, or `None` when the enumeration fails ([`walk`]).
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
#[cfg(target_os = "linux")]
mod acl {
    use std::ffi::{c_char, c_int, c_void};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    unsafe extern "C" {
        /// `<sys/xattr.h>`: an open file's attribute; a zero `size` asks only its length.
        fn fgetxattr(fd: c_int, name: *const c_char, value: *mut c_void, size: usize) -> isize;
        /// `<sys/xattr.h>`: a path's attribute, a final symbolic link not followed.
        fn lgetxattr(
            path: *const c_char,
            name: *const c_char,
            value: *mut c_void,
            size: usize,
        ) -> isize;
    }

    /// What one attribute-length call that returned `ret` answered: `None` it succeeded,
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
            /// `<sys/xattr.h>`: set a path's attribute.
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
    #[repr(C)]
    struct Statfs {
        f_type: i64,
        _room: [i64; 31],
    }

    impl Statfs {
        /// A record for a call to fill.
        fn empty() -> Self {
            Self {
                f_type: 0,
                _room: [0; 31],
            }
        }

        /// `f_type` after a call that returned `ret`: its low 32 bits, every magic number's
        fn magic(&self, ret: c_int) -> Option<u32> {
            if ret == 0 {
                u32::try_from(self.f_type & 0xFFFF_FFFF).ok()
            } else {
                None
            }
        }
    }

    unsafe extern "C" {
        /// `<sys/vfs.h>`: the filesystem a path is on, a final symbolic link followed.
        fn statfs(path: *const c_char, buf: *mut Statfs) -> c_int;
        /// `<sys/vfs.h>`: the filesystem an open file is on.
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

    pub(super) fn account(_uid: u32) -> Option<Account> {
        None
    }

    pub(super) fn group(_gid: u32) -> Option<Group> {
        None
    }

    pub(super) fn primary_members(_gid: u32) -> Option<Vec<u32>> {
        None
    }
}

/// Off Linux the ACL is not read; it answers "may carry one", which grants no exemption
#[cfg(not(target_os = "linux"))]
mod acl {
    pub(super) fn on_file(_file: &std::fs::File) -> bool {
        true
    }

    pub(super) fn at_path(_path: &std::path::Path) -> bool {
        true
    }
}

/// Off Linux the filesystem type is not read; it answers "unknown", which grants no
#[cfg(not(target_os = "linux"))]
mod filesystem {
    pub(super) fn of_file(_file: &std::fs::File) -> Option<u32> {
        None
    }

    pub(super) fn at_path(_path: &std::path::Path) -> Option<u32> {
        None
    }
}

/// Test seam (Linux): gives account `uid` the permissions of `path`'s group class through a POSIX access ACL whose mask is those same bits, so the mode reads as before, as `setfacl -m u:<uid>:<group bits>` would.
/// # Errors
/// The `stat` or the `setxattr` failed (`ENOTSUP` on a filesystem that keeps no ACLs).
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
pub fn grant_group_class_by_acl(path: &std::path::Path, uid: u32) -> std::io::Result<()> {
    acl::grant_group_class(path, uid)
}

/// Test seam (Linux): the type of the filesystem `path` is on, as the judge reads it, or `None` when it cannot be read.
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
#[must_use]
pub fn filesystem_type_at(path: &std::path::Path) -> Option<u32> {
    filesystem::at_path(path)
}

/// Test seam (Linux): the passwd enumeration [`is_user_private_group`] reads, the uids whose primary group is `gid`, or `None` when it fails.
#[cfg(all(target_os = "linux", any(test, debug_assertions)))]
#[must_use]
pub fn primary_members(gid: u32) -> Option<Vec<u32>> {
    nss::primary_members(gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, gid: u32) -> Account {
        Account {
            name: name.into(),
            gid,
        }
    }

    fn group(name: &str, members: &[&str]) -> Group {
        Group {
            name: name.into(),
            members: members.iter().map(|m| m.as_bytes().to_vec()).collect(),
        }
    }

    /// Ubuntu's own shape is admitted; each fact that lets another account into the group
    #[test]
    fn only_a_group_no_other_account_can_hold_is_private() {
        let me = account("ann", 1000);
        let own = group("ann", &[]);
        assert!(user_private_group(
            1000,
            1000,
            Some(&me),
            Some(&own),
            &[1000]
        ));
        assert!(
            user_private_group(1000, 1000, Some(&me), Some(&group("ann", &["ann"])), &[]),
            "listing its owner as a member adds no one"
        );
        for (why, uid, gid, account, group, primaries) in [
            (
                "root",
                0,
                0,
                Some(account("root", 0)),
                Some(group("root", &[])),
                vec![0],
            ),
            (
                "not the primary group",
                1000,
                1001,
                Some(me.clone()),
                Some(group("ann", &[])),
                vec![],
            ),
            (
                "another name",
                1000,
                1000,
                Some(me.clone()),
                Some(group("staff", &[])),
                vec![],
            ),
            (
                "a member",
                1000,
                1000,
                Some(me.clone()),
                Some(group("ann", &["bob"])),
                vec![],
            ),
            (
                "another's primary",
                1000,
                1000,
                Some(me.clone()),
                Some(own.clone()),
                vec![1000, 1001],
            ),
            ("no account", 1000, 1000, None, Some(own.clone()), vec![]),
            ("no group", 1000, 1000, Some(me.clone()), None, vec![]),
        ] {
            assert!(
                !user_private_group(uid, gid, account.as_ref(), group.as_ref(), &primaries),
                "{why}"
            );
        }
    }

    /// The reader the system's answer goes through: the enumeration of the accounts whose
    #[test]
    fn the_other_accounts_primary_groups_are_read_last_and_decide() {
        let me = || Some(account("ann", 1000));
        let own = |_: u32| Some(group("ann", &[]));
        assert!(private_group_by(
            1000,
            1000,
            |_| me(),
            own,
            |_| Some(vec![1000])
        ));
        for (why, primaries) in [
            ("another account's primary group", Some(vec![1000, 1001])),
            ("an enumeration that failed", None),
        ] {
            assert!(
                !private_group_by(1000, 1000, |_| me(), own, |_| primaries),
                "{why}"
            );
        }
        assert!(
            !private_group_by(
                1000,
                1000,
                |_| me(),
                |_| Some(group("staff", &[])),
                |_| unreachable!()
            ),
            "another group's name: no enumeration"
        );
        assert!(
            !private_group_by(1000, 1000, |_| None, own, |_| unreachable!()),
            "no account: no enumeration"
        );
    }

    /// Group-write is this user's own write only on this user's file in their private
    #[test]
    fn only_a_private_groups_write_is_the_owners_own() {
        let private = |gid: u32| gid == 1000;
        for (why, owner, gid, mode, widened, shared) in [
            ("owner-only", 1000, 1000, 0o100_644, false, false),
            ("private-group write", 1000, 1000, 0o100_664, false, false),
            (
                "a 0775 directory in the private group",
                1000,
                1000,
                0o040_775,
                false,
                false,
            ),
            ("another group's write", 1000, 100, 0o100_664, false, true),
            ("world write", 1000, 1000, 0o100_666, false, true),
            ("world write alone", 1000, 1000, 0o100_646, false, true),
            ("another owner's file", 1001, 1000, 0o100_664, false, true),
            ("a sticky shared directory", 0, 0, 0o041_777, false, true),
            (
                "private-group write, widened",
                1000,
                1000,
                0o100_664,
                true,
                true,
            ),
            (
                "a 0775 directory, widened",
                1000,
                1000,
                0o040_775,
                true,
                true,
            ),
            (
                "an ACL whose mask grants no write",
                1000,
                1000,
                0o100_644,
                true,
                false,
            ),
            ("world write, widened", 1000, 1000, 0o100_646, true, true),
        ] {
            assert_eq!(
                others_can_write_as(1000, owner, gid, mode, widened, private),
                shared,
                "{why}"
            );
        }
        assert!(
            !others_can_write_as(1000, 1000, 100, 0o100_644, false, |_| unreachable!()),
            "no write bit for others: the group is never looked up"
        );
        assert!(
            others_can_write_as(1000, 1000, 1000, 0o100_664, true, |_| unreachable!()),
            "widened: the group is never looked up"
        );
    }

    /// An access ACL is either attribute's: a file on NFSv4 (no POSIX attribute, an NFSv4
    #[test]
    fn an_access_acl_is_either_attributes() {
        const EIO: i32 = 5;
        const EACCES: i32 = 13;
        for (why, posix, nfs4, carried) in [
            ("ext4 without an ACL", Some(ENODATA), Some(ENOTSUP), false),
            (
                "a filesystem that keeps none",
                Some(ENOTSUP),
                Some(ENOTSUP),
                false,
            ),
            ("FUSE without either", Some(ENODATA), Some(ENODATA), false),
            ("a POSIX ACL", None, Some(ENOTSUP), true),
            ("an NFSv4 ACL", Some(ENOTSUP), None, true),
            ("an unreadable POSIX ACL", Some(EACCES), Some(ENODATA), true),
            (
                "an unreachable NFSv4 server",
                Some(ENOTSUP),
                Some(EIO),
                true,
            ),
        ] {
            let answer = acl_carried(|name| match name.to_bytes() {
                b"system.posix_acl_access" => posix,
                b"system.nfs4_acl" => nfs4,
                other => panic!("{why}: asked {other:?}"),
            });
            assert_eq!(answer, carried, "{why}");
        }
    }

    /// Every remote type takes the exemption back, as does a type that cannot be read; local
    #[test]
    fn a_remote_or_unknown_filesystem_widens_the_mode() {
        assert!(remote(None), "an unreadable type");
        for magic in REMOTE_FILESYSTEMS {
            assert!(remote(Some(magic)), "{magic:#x}");
        }
        for magic in [
            0xEF53,
            0x0102_1994,
            0x9123_683E,
            0x5846_5342,
            0x794C_7630,
            0x9FA0,
        ] {
            assert!(!remote(Some(magic)), "{magic:#x}");
        }
    }

    /// A path is judged as it stands, a final link not followed: a link reads as its own
    #[test]
    fn a_link_is_judged_as_it_stands_not_through() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-uds-shared-write.")
            .tempdir()
            .expect("scratch");
        let file = scratch.path().join("file");
        std::fs::write(&file, "").expect("write");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&file, &link).expect("symlink");
        #[cfg(target_vendor = "apple")]
        {
            let c = std::ffi::CString::new(link.as_os_str().as_encoded_bytes()).expect("path");
            // SAFETY: a NUL-terminated path, live for the call.
            let rc = unsafe {
                libc::fchmodat(libc::AT_FDCWD, c.as_ptr(), 0o777, libc::AT_SYMLINK_NOFOLLOW)
            };
            assert_eq!(rc, 0, "lchmod: {}", std::io::Error::last_os_error());
        }
        assert!(!others_can_write_at(&file).expect("judged"));
        assert!(others_can_write_at(&link).expect("judged"));
    }

    /// The walk over the accounts: it keeps the uids whose primary group is the one asked,
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failure_mid_walk_is_no_answer() {
        use nss::{Step, walk};
        let script = |steps: Vec<Step>| {
            let mut steps = steps.into_iter();
            let mut sizes = Vec::new();
            let got = walk(1000, |size| {
                sizes.push(size);
                steps.next().expect("asked past the script")
            });
            (got, sizes)
        };
        let entry = |uid, gid| Step::Entry { uid, gid };
        let (got, _) = script(vec![
            entry(0, 0),
            entry(1000, 1000),
            entry(1001, 100),
            entry(1002, 1000),
            Step::End,
        ]);
        assert_eq!(got, Some(vec![1000, 1002]));
        let (got, _) = script(vec![entry(1000, 1000), Step::Failed]);
        assert_eq!(got, None, "a backend failure after the entries so far");
        let mut grown = vec![Step::TooSmall; 8];
        grown.extend([entry(1000, 1000), Step::End]);
        let (got, sizes) = script(grown);
        assert_eq!(got, Some(vec![1000]), "an entry a 1 MiB buffer holds");
        assert_eq!(sizes.first(), Some(&4096));
        assert_eq!(sizes[8], 1 << 20);
        let (got, sizes) = script(vec![Step::TooSmall; 9]);
        assert_eq!(got, None, "an entry no buffer up to the limit holds");
        assert_eq!(sizes.last(), Some(&(1 << 20)));
    }

    /// The enumeration on this machine reads the real accounts: root has gid 0, and this
    #[cfg(target_os = "linux")]
    #[test]
    fn the_enumeration_reads_this_machines_accounts() {
        let roots = nss::primary_members(0).expect("the enumeration");
        assert!(roots.contains(&0), "{roots:?}");
        let uid = crate::peer::our_uid();
        let mine = nss::account(uid).expect("this user's passwd entry");
        let members = nss::primary_members(mine.gid).expect("the enumeration");
        assert!(members.contains(&uid), "{members:?}");
    }

    /// The filesystem type is read where it is: `/proc` is procfs, through its path and
    #[cfg(target_os = "linux")]
    #[test]
    fn the_filesystem_type_is_read() {
        const PROC_SUPER_MAGIC: u32 = 0x9FA0;
        assert_eq!(
            filesystem::at_path(std::path::Path::new("/proc")),
            Some(PROC_SUPER_MAGIC)
        );
        let status = std::fs::File::open("/proc/self/status").expect("open");
        assert_eq!(filesystem::of_file(&status), Some(PROC_SUPER_MAGIC));
        assert_eq!(
            filesystem::at_path(std::path::Path::new("/proc/self/no-such-entry")),
            None
        );
    }

    /// On this machine's own filesystem: a file and a directory on this process's primary
    #[cfg(target_os = "linux")]
    #[test]
    fn an_access_acl_takes_back_the_private_groups_exemption() {
        use std::os::unix::fs::PermissionsExt as _;
        let chmod = |path: &std::path::Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-uds-shared-write.")
            .tempdir()
            .expect("scratch");
        let local = !remote(filesystem::at_path(scratch.path()));
        let nobody = 65534;
        for (name, dir, mode) in [
            ("file-664", false, 0o664),
            ("file-644", false, 0o644),
            ("dir-775", true, 0o775),
            ("dir-755", true, 0o755),
        ] {
            let path = scratch.path().join(name);
            if dir {
                std::fs::create_dir(&path).expect("mkdir");
            } else {
                std::fs::write(&path, "").expect("write");
            }
            chmod(&path, mode);
            let gid = std::os::unix::fs::MetadataExt::gid(&std::fs::metadata(&path).expect("stat"));
            let private = is_user_private_group(gid);
            let open = || std::fs::File::open(&path).expect("open");
            assert!(!acl::on_file(&open()), "{name}: no ACL yet");
            assert!(!acl::at_path(&path), "{name}: no ACL yet");
            let plain = mode & 0o020 != 0 && !(private && local);
            assert_eq!(others_can_write(&open()).expect("judged"), plain, "{name}");
            assert_eq!(others_can_write_at(&path).expect("judged"), plain, "{name}");
            let md = std::fs::symlink_metadata(&path).expect("lstat");
            for (magic, remote) in [(Some(0x6969), true), (None, true), (Some(0xEF53), false)] {
                assert_eq!(
                    judge(&md, false, magic),
                    mode & 0o020 != 0 && !(private && !remote),
                    "{name} on a stand-in filesystem {magic:x?}"
                );
            }
            grant_group_class_by_acl(&path, nobody)
                .unwrap_or_else(|e| panic!("{name}: an access ACL on ext4 or tmpfs: {e}"));
            assert_eq!(
                std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&path).expect("stat"))
                    & 0o777,
                mode,
                "{name}: the ACL leaves the mode bits as they were"
            );
            assert!(acl::on_file(&open()), "{name}: the descriptor sees the ACL");
            assert!(acl::at_path(&path), "{name}: the path sees the ACL");
            let shared = mode & 0o020 != 0;
            assert_eq!(
                others_can_write(&open()).expect("judged"),
                shared,
                "{name}, ACL"
            );
            assert_eq!(
                others_can_write_at(&path).expect("judged"),
                shared,
                "{name}, ACL"
            );
        }
    }
}
