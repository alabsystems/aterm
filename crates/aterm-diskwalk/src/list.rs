// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LISTERS: one directory's entries with the attributes a walk sums, a bounded batch per call,
//! read relative to the directory's descriptor.

/// What one entry is.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link (counted, never followed).
    Link,
    /// Anything else (a socket, a FIFO, a device).
    Other,
}

/// One entry as a lister read it.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryRef<'a> {
    pub(crate) name: &'a [u8],
    pub(crate) kind: Kind,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) nlink: u32,
    pub(crate) alloc: u64,
    pub(crate) logical: u64,
    pub(crate) private: Option<u64>,
    pub(crate) mtime_s: i64,
    pub(crate) dataless: bool,
    pub(crate) exec: bool,
    /// A child directory's own entry count, where the listing gives it free.
    pub(crate) dir_entries: Option<u32>,
    /// A mount point (another file system covers this name).
    pub(crate) mount_point: bool,
    /// The entry could not be read: its errno.
    pub(crate) error: Option<i32>,
}

#[cfg(unix)]
impl EntryRef<'_> {
    /// An entry the lister knows only by name and a failure.
    pub(crate) const fn failed(name: &[u8], errno: i32) -> EntryRef<'_> {
        EntryRef {
            name,
            kind: Kind::Other,
            dev: 0,
            ino: 0,
            nlink: 1,
            alloc: 0,
            logical: 0,
            private: None,
            mtime_s: 0,
            dataless: false,
            exec: false,
            dir_entries: None,
            mount_point: false,
            error: Some(errno),
        }
    }
}

/// Which lister a walk uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListerChoice {
    /// The platform's bulk lister (`getattrlistbulk`, `getdents64` + `statx`).
    #[default]
    Native,
    /// `readdir` + `fstatat` everywhere: the fallback, and the oracle the native arms are tested
    /// against.
    Portable,
}

/// The per-level buffer: 16 KiB.
#[cfg(unix)]
pub(crate) const LEVEL_BUFFER: usize = 16 * 1024;

/// A device number as every lister and `fstat` spell it, so a child's device compares equal to its
/// root's whichever call read each (a mismatch makes every subfolder a mount boundary, never
/// entered).
#[cfg(target_vendor = "apple")]
#[must_use]
pub(crate) fn dev_of(st_dev: libc::dev_t) -> u64 {
    u64::from(u32::from_ne_bytes(st_dev.to_ne_bytes()))
}

/// A device number as every lister and `fstat` spell it (see the macOS arm): Linux's `dev_t` is
/// already the `u64` both read.
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) const fn dev_of(st_dev: libc::dev_t) -> u64 {
    st_dev
}

/// A device number as every lister and `fstat` spell it (see the macOS arm).
#[cfg(all(unix, not(any(target_vendor = "apple", target_os = "linux"))))]
#[must_use]
pub(crate) fn dev_of(st_dev: libc::dev_t) -> u64 {
    st_dev as u64
}

/// `statx`'s device as `fstat`'s `st_dev` spells it: `makedev(major, minor)` (`(minor & 0xff) |
/// (major << 8) | ((minor & !0xff) << 12)` and the high bits), never `major << 32 | minor`.
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn dev_of_parts(major: u32, minor: u32) -> u64 {
    dev_of(libc::makedev(major, minor))
}

#[cfg(unix)]
pub(crate) use imp::Lister;

#[cfg(unix)]
mod imp {
    use super::{EntryRef, ListerChoice};
    use std::ffi::OsString;
    use std::os::fd::BorrowedFd;

    /// One directory's lister.
    pub(crate) enum Lister {
        #[cfg(target_vendor = "apple")]
        Bulk(super::bulk::Bulk),
        #[cfg(target_os = "linux")]
        Dents(super::dents::Dents),
        Portable(super::portable::Portable),
    }

    impl Lister {
        /// A lister for one directory.
        #[cfg_attr(
            not(target_vendor = "apple"),
            expect(
                unused_variables,
                reason = "only the macOS arm reads a private size (ATTR_CMNEXT_PRIVATESIZE)"
            )
        )]
        pub(crate) fn new(
            choice: ListerChoice,
            buf: Vec<u8>,
            skip: Vec<OsString>,
            private: bool,
        ) -> Self {
            match choice {
                ListerChoice::Portable => {
                    Lister::Portable(super::portable::Portable::new(skip, buf))
                }
                ListerChoice::Native => {
                    #[cfg(target_vendor = "apple")]
                    {
                        if skip.is_empty() {
                            Lister::Bulk(super::bulk::Bulk::new(buf, private))
                        } else {
                            Lister::Portable(super::portable::Portable::new(skip, buf))
                        }
                    }
                    #[cfg(target_os = "linux")]
                    {
                        Lister::Dents(super::dents::Dents::new(buf, skip))
                    }
                    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
                    {
                        Lister::Portable(super::portable::Portable::new(skip, buf))
                    }
                }
            }
        }

        /// One bounded call: visit each entry it returned.
        /// # Errors
        /// The raw errno of a listing call that failed (the directory became unreadable or vanished
        /// mid-listing).
        pub(crate) fn next_batch(
            &mut self,
            fd: BorrowedFd<'_>,
            visit: &mut dyn FnMut(&EntryRef<'_>),
        ) -> Result<bool, i32> {
            match self {
                #[cfg(target_vendor = "apple")]
                Lister::Bulk(b) => b.next_batch(fd, visit),
                #[cfg(target_os = "linux")]
                Lister::Dents(d) => d.next_batch(fd, visit),
                Lister::Portable(p) => p.next_batch(fd, visit),
            }
        }

        /// Give the level buffer back for the next directory.
        pub(crate) fn into_buffer(self) -> Vec<u8> {
            match self {
                #[cfg(target_vendor = "apple")]
                Lister::Bulk(b) => b.into_buffer(),
                #[cfg(target_os = "linux")]
                Lister::Dents(d) => d.into_buffer(),
                Lister::Portable(p) => p.into_buffer(),
            }
        }
    }
}

/// Little-endian-agnostic native reads from a byte buffer, bounds-checked: the two listers that
/// parse a kernel buffer (`getattrlistbulk` records, `getdents64` records).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(crate) struct Rd<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) at: usize,
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
impl Rd<'_> {
    pub(crate) fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let end = self.at.checked_add(N)?;
        let bytes: [u8; N] = self.b.get(self.at..end)?.try_into().ok()?;
        self.at = end;
        Some(bytes)
    }
    /// The typed reads only the `getattrlistbulk` parser makes.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take::<4>().map(u32::from_ne_bytes)
    }
    #[cfg(target_vendor = "apple")]
    pub(crate) fn i64(&mut self) -> Option<i64> {
        self.take::<8>().map(i64::from_ne_bytes)
    }
    #[cfg(target_vendor = "apple")]
    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.take::<8>().map(u64::from_ne_bytes)
    }
}

/// `st_mode` (or `ACCESSMASK`) has any execute bit.
#[cfg(unix)]
pub(crate) const fn any_exec(mode: u32) -> bool {
    mode & 0o111 != 0
}

#[cfg(target_vendor = "apple")]
pub(crate) mod bulk {

    use super::{EntryRef, Kind, Rd, any_exec};
    use std::os::fd::{AsRawFd as _, BorrowedFd};

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct AttrList {
        pub(crate) bitmapcount: u16,
        pub(crate) reserved: u16,
        pub(crate) commonattr: u32,
        pub(crate) volattr: u32,
        pub(crate) dirattr: u32,
        pub(crate) fileattr: u32,
        pub(crate) forkattr: u32,
    }

    pub(crate) const ATTR_BIT_MAP_COUNT: u16 = 5;
    pub(crate) const ATTR_CMN_NAME: u32 = 0x0000_0001;
    pub(crate) const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    pub(crate) const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    pub(crate) const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
    pub(crate) const ATTR_CMN_ACCESSMASK: u32 = 0x0002_0000;
    pub(crate) const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
    pub(crate) const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    pub(crate) const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    pub(crate) const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    pub(crate) const ATTR_DIR_ENTRYCOUNT: u32 = 0x0000_0002;
    pub(crate) const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0004;
    pub(crate) const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;
    pub(crate) const DIR_MNTSTATUS_TRIGGER: u32 = 0x0000_0002;
    pub(crate) const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    pub(crate) const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
    pub(crate) const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    pub(crate) const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
    pub(crate) const FSOPT_NOFOLLOW: u64 = 0x0000_0001;
    pub(crate) const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x0000_0020;
    /// `<sys/stat.h>`: a File Provider placeholder whose data is not local.
    pub(crate) const SF_DATALESS: u32 = 0x4000_0000;
    /// `<sys/vnode.h>` `enum vtype`.
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;

    unsafe extern "C" {
        fn getattrlistbulk(
            dirfd: libc::c_int,
            attr_list: *mut AttrList,
            attr_buf: *mut libc::c_void,
            attr_buf_size: usize,
            options: u64,
        ) -> libc::c_int;
        fn fgetattrlist(
            fd: libc::c_int,
            attr_list: *mut AttrList,
            attr_buf: *mut libc::c_void,
            attr_buf_size: usize,
            options: libc::c_uint,
        ) -> libc::c_int;
        fn getattrlistat(
            fd: libc::c_int,
            path: *const libc::c_char,
            attr_list: *mut AttrList,
            attr_buf: *mut libc::c_void,
            attr_buf_size: usize,
            options: libc::c_ulong,
        ) -> libc::c_int;
    }

    /// One directory's bulk lister.
    pub(crate) struct Bulk {
        buf: Vec<u8>,
        private: bool,
    }

    impl Bulk {
        pub(crate) fn new(mut buf: Vec<u8>, private: bool) -> Self {
            buf.resize(super::LEVEL_BUFFER, 0);
            Self { buf, private }
        }

        pub(crate) fn into_buffer(self) -> Vec<u8> {
            self.buf
        }

        const fn attrs(&self) -> AttrList {
            AttrList {
                bitmapcount: ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: ATTR_CMN_RETURNED_ATTRS
                    | ATTR_CMN_ERROR
                    | ATTR_CMN_NAME
                    | ATTR_CMN_DEVID
                    | ATTR_CMN_OBJTYPE
                    | ATTR_CMN_MODTIME
                    | ATTR_CMN_ACCESSMASK
                    | ATTR_CMN_FLAGS
                    | ATTR_CMN_FILEID,
                volattr: 0,
                dirattr: ATTR_DIR_ENTRYCOUNT | ATTR_DIR_MOUNTSTATUS,
                fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE,
                forkattr: if self.private {
                    ATTR_CMNEXT_PRIVATESIZE
                } else {
                    0
                },
            }
        }

        pub(crate) fn next_batch(
            &mut self,
            fd: BorrowedFd<'_>,
            visit: &mut dyn FnMut(&EntryRef<'_>),
        ) -> Result<bool, i32> {
            let mut al = self.attrs();
            let options = if self.private {
                FSOPT_ATTR_CMN_EXTENDED
            } else {
                0
            };
            // SAFETY: `fd` is a live directory descriptor, `al` a valid attrlist and `buf` writable
            // for its length; the kernel writes at most that many bytes and answers how many
            // entries it packed.
            let n = unsafe {
                getattrlistbulk(
                    fd.as_raw_fd(),
                    &raw mut al,
                    self.buf.as_mut_ptr().cast(),
                    self.buf.len(),
                    options,
                )
            };
            if n < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
            }
            if n == 0 {
                return Ok(false);
            }
            let mut off = 0usize;
            for _ in 0..n {
                let Some(len) = self
                    .buf
                    .get(off..off + 4)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_ne_bytes)
                else {
                    break;
                };
                let len = len as usize;
                let Some(entry) = self.buf.get(off..off + len) else {
                    break;
                };
                if len < 4 {
                    break;
                }
                if let Some(e) = parse(entry, self.private) {
                    visit(&e);
                }
                off += len;
            }
            Ok(true)
        }
    }

    /// One packed entry (its leading length included).
    pub(crate) fn parse(entry: &[u8], private: bool) -> Option<EntryRef<'_>> {
        let mut rd = Rd { b: entry, at: 4 };
        let common = rd.u32()?;
        let _vol = rd.u32()?;
        let dir = rd.u32()?;
        let file = rd.u32()?;
        let fork = rd.u32()?;
        let error = if common & ATTR_CMN_ERROR != 0 {
            Some(rd.u32()?)
        } else {
            None
        };
        let mut name: &[u8] = &[];
        if common & ATTR_CMN_NAME != 0 {
            let at = rd.at;
            let off = i32::from_ne_bytes(rd.take::<4>()?);
            let len = rd.u32()? as usize;
            let start = usize::try_from(i64::try_from(at).ok()? + i64::from(off)).ok()?;
            let raw = entry.get(start..start.checked_add(len)?)?;
            name = raw.strip_suffix(&[0]).unwrap_or(raw);
        }
        if let Some(err) = error.filter(|e| *e != 0) {
            return Some(EntryRef::failed(
                name,
                i32::try_from(err).unwrap_or(i32::MAX),
            ));
        }
        let dev = if common & ATTR_CMN_DEVID != 0 {
            u64::from(rd.u32()?)
        } else {
            0
        };
        let objtype = if common & ATTR_CMN_OBJTYPE != 0 {
            rd.u32()?
        } else {
            0
        };
        let mtime_s = if common & ATTR_CMN_MODTIME != 0 {
            let s = rd.i64()?;
            let _ns = rd.i64()?;
            s
        } else {
            0
        };
        let mode = if common & ATTR_CMN_ACCESSMASK != 0 {
            rd.u32()?
        } else {
            0
        };
        let flags = if common & ATTR_CMN_FLAGS != 0 {
            rd.u32()?
        } else {
            0
        };
        let ino = if common & ATTR_CMN_FILEID != 0 {
            rd.u64()?
        } else {
            0
        };
        let dir_entries = if dir & ATTR_DIR_ENTRYCOUNT != 0 {
            Some(rd.u32()?)
        } else {
            None
        };
        let mount_status = if dir & ATTR_DIR_MOUNTSTATUS != 0 {
            rd.u32()?
        } else {
            0
        };
        let nlink = if file & ATTR_FILE_LINKCOUNT != 0 {
            rd.u32()?
        } else {
            1
        };
        let logical = if file & ATTR_FILE_TOTALSIZE != 0 {
            u64::try_from(rd.i64()?).unwrap_or(0)
        } else {
            0
        };
        let alloc = if file & ATTR_FILE_ALLOCSIZE != 0 {
            u64::try_from(rd.i64()?).unwrap_or(0)
        } else {
            0
        };
        let private_size = if private && fork & ATTR_CMNEXT_PRIVATESIZE != 0 {
            Some(u64::try_from(rd.i64()?).unwrap_or(0))
        } else {
            None
        };
        let kind = match objtype {
            VDIR => Kind::Dir,
            VREG => Kind::File,
            VLNK => Kind::Link,
            _ => Kind::Other,
        };
        Some(EntryRef {
            name,
            kind,
            dev,
            ino,
            nlink: nlink.max(1),
            alloc,
            logical,
            private: private_size,
            mtime_s,
            dataless: flags & SF_DATALESS != 0,
            exec: kind == Kind::File && any_exec(mode),
            dir_entries: if kind == Kind::Dir { dir_entries } else { None },
            mount_point: mount_status & (DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER) != 0,
            error: None,
        })
    }

    /// `ATTR_DIR_ENTRYCOUNT` of an open directory, without listing it.
    pub(crate) fn entry_count(fd: BorrowedFd<'_>) -> Result<u32, i32> {
        let mut al = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: 0,
            dirattr: ATTR_DIR_ENTRYCOUNT,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buf = [0u8; 16];
        // SAFETY: a live descriptor, a valid attrlist, a 16-byte writable buffer.
        let r = unsafe {
            fgetattrlist(
                fd.as_raw_fd(),
                &raw mut al,
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        let mut rd = Rd { b: &buf, at: 4 };
        rd.u32().ok_or(libc::EIO)
    }

    /// `ATTR_DIR_ENTRYCOUNT` of the directory `name` in `dir`, not following a link, without
    /// listing or opening it.
    pub(crate) fn entry_count_at(dir: BorrowedFd<'_>, name: &std::ffi::CStr) -> Result<u32, i32> {
        let mut al = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: 0,
            dirattr: ATTR_DIR_ENTRYCOUNT,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buf = [0u8; 16];
        // SAFETY: a live descriptor, a NUL-terminated name, a valid attrlist and a 16-byte writable
        // buffer.
        let r = unsafe {
            getattrlistat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &raw mut al,
                buf.as_mut_ptr().cast(),
                buf.len(),
                FSOPT_NOFOLLOW as libc::c_ulong,
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        let mut rd = Rd { b: &buf, at: 4 };
        rd.u32().ok_or(libc::EIO)
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod dents {

    use super::{EntryRef, Kind, Rd, any_exec};
    use std::ffi::OsString;
    use std::os::fd::{AsRawFd as _, BorrowedFd};
    use std::os::unix::ffi::OsStrExt as _;

    #[cfg(target_arch = "x86_64")]
    const SYS_GETDENTS64: libc::c_long = 217;
    #[cfg(target_arch = "x86_64")]
    const SYS_STATX: libc::c_long = 332;
    #[cfg(target_arch = "aarch64")]
    const SYS_GETDENTS64: libc::c_long = 61;
    #[cfg(target_arch = "aarch64")]
    const SYS_STATX: libc::c_long = 291;

    const AT_STATX_DONT_SYNC: libc::c_int = 0x4000;
    const AT_NO_AUTOMOUNT: libc::c_int = 0x800;
    const STATX_TYPE: u32 = 0x1;
    const STATX_MODE: u32 = 0x2;
    const STATX_NLINK: u32 = 0x4;
    const STATX_MTIME: u32 = 0x40;
    const STATX_INO: u32 = 0x100;
    const STATX_SIZE: u32 = 0x200;
    const STATX_BLOCKS: u32 = 0x400;
    const STATX_ATTR_MOUNT_ROOT: u64 = 0x2000;
    const STATX_ATTR_AUTOMOUNT: u64 = 0x1000;
    const S_IFMT: u32 = 0o170_000;
    const S_IFDIR: u32 = 0o040_000;
    const S_IFREG: u32 = 0o100_000;
    const S_IFLNK: u32 = 0o120_000;

    /// `struct statx`, 256 bytes.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Statx {
        mask: u32,
        blksize: u32,
        attributes: u64,
        nlink: u32,
        uid: u32,
        gid: u32,
        mode: u16,
        spare0: u16,
        ino: u64,
        size: u64,
        blocks: u64,
        attributes_mask: u64,
        atime: [u64; 2],
        btime: [u64; 2],
        ctime: [u64; 2],
        mtime_sec: i64,
        mtime_nsec: u32,
        mtime_reserved: i32,
        rdev_major: u32,
        rdev_minor: u32,
        dev_major: u32,
        dev_minor: u32,
        spare2: [u64; 14],
    }
    const _: () = assert!(std::mem::size_of::<Statx>() == 256);

    pub(crate) struct Dents {
        buf: Vec<u8>,
        skip: Vec<OsString>,
    }

    impl Dents {
        pub(crate) fn new(mut buf: Vec<u8>, skip: Vec<OsString>) -> Self {
            buf.resize(super::LEVEL_BUFFER, 0);
            Self { buf, skip }
        }

        pub(crate) fn into_buffer(self) -> Vec<u8> {
            self.buf
        }

        pub(crate) fn next_batch(
            &mut self,
            fd: BorrowedFd<'_>,
            visit: &mut dyn FnMut(&EntryRef<'_>),
        ) -> Result<bool, i32> {
            // SAFETY: a live directory descriptor and a writable buffer of its length; the kernel
            // fills at most that many bytes.
            let n = unsafe {
                libc::syscall(
                    SYS_GETDENTS64,
                    fd.as_raw_fd(),
                    self.buf.as_mut_ptr(),
                    self.buf.len(),
                )
            };
            if n < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
            }
            if n == 0 {
                return Ok(false);
            }
            let n = usize::try_from(n).unwrap_or(0).min(self.buf.len());
            let mut off = 0usize;
            while off + 19 <= n {
                let mut rd = Rd {
                    b: &self.buf,
                    at: off + 16,
                };
                let Some(reclen) = rd.take::<2>().map(u16::from_ne_bytes) else {
                    break;
                };
                let reclen = usize::from(reclen);
                if reclen == 0 || off + reclen > n {
                    break;
                }
                let raw = &self.buf[off + 19..off + reclen];
                let name = raw.split(|b| *b == 0).next().unwrap_or(raw);
                off += reclen;
                if name == b"." || name == b".." {
                    continue;
                }
                if self.skip.iter().any(|s| s.as_bytes() == name) {
                    visit(&EntryRef {
                        mount_point: true,
                        kind: Kind::Dir,
                        error: None,
                        ..EntryRef::failed(name, 0)
                    });
                    continue;
                }
                visit(&stat_one(fd, name));
            }
            Ok(true)
        }
    }

    fn stat_one<'a>(fd: BorrowedFd<'_>, name: &'a [u8]) -> EntryRef<'a> {
        let Ok(c) = std::ffi::CString::new(name) else {
            return EntryRef::failed(name, libc::EINVAL);
        };
        // SAFETY: `Statx` is integers: all-zero is a value.
        let mut st: Statx = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor, a NUL-terminated name and a writable 256-byte `statx` buffer.
        let r = unsafe {
            libc::syscall(
                SYS_STATX,
                fd.as_raw_fd(),
                c.as_ptr(),
                AT_STATX_DONT_SYNC | libc::AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT,
                STATX_TYPE
                    | STATX_MODE
                    | STATX_NLINK
                    | STATX_MTIME
                    | STATX_INO
                    | STATX_SIZE
                    | STATX_BLOCKS,
                &raw mut st,
            )
        };
        if r != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return EntryRef::failed(name, errno);
        }
        let mode = u32::from(st.mode);
        let kind = match mode & S_IFMT {
            S_IFDIR => Kind::Dir,
            S_IFREG => Kind::File,
            S_IFLNK => Kind::Link,
            _ => Kind::Other,
        };
        let dev = super::dev_of_parts(st.dev_major, st.dev_minor);
        EntryRef {
            name,
            kind,
            dev,
            ino: st.ino,
            nlink: st.nlink.max(1),
            alloc: st.blocks.saturating_mul(512),
            logical: st.size,
            private: None,
            mtime_s: st.mtime_sec,
            dataless: false,
            exec: kind == Kind::File && any_exec(mode),
            dir_entries: None,
            mount_point: st.attributes_mask
                & st.attributes
                & (STATX_ATTR_MOUNT_ROOT | STATX_ATTR_AUTOMOUNT)
                != 0,
            error: None,
        }
    }
}

#[cfg(unix)]
pub(crate) mod portable {

    use super::{EntryRef, Kind, any_exec};
    use std::ffi::OsString;
    use std::os::fd::{AsRawFd as _, BorrowedFd};
    use std::os::unix::ffi::OsStrExt as _;

    /// Entries one portable call reads.
    const BATCH: usize = 128;

    #[cfg(target_vendor = "apple")]
    const SF_DATALESS: u32 = super::bulk::SF_DATALESS;

    pub(crate) struct Portable {
        dir: Option<aterm_dirfd::Dir>,
        skip: Vec<OsString>,
        buf: Vec<u8>,
        names: Vec<u8>,
        ends: Vec<usize>,
    }

    impl Portable {
        pub(crate) const fn new(skip: Vec<OsString>, buf: Vec<u8>) -> Self {
            Self {
                dir: None,
                skip,
                buf,
                names: Vec::new(),
                ends: Vec::new(),
            }
        }

        pub(crate) fn into_buffer(self) -> Vec<u8> {
            self.buf
        }

        pub(crate) fn next_batch(
            &mut self,
            fd: BorrowedFd<'_>,
            visit: &mut dyn FnMut(&EntryRef<'_>),
        ) -> Result<bool, i32> {
            if self.dir.is_none() {
                self.dir = Some(aterm_dirfd::Dir::read_from(fd).map_err(|e| e.raw_os_error())?);
            }
            let Some(dir) = self.dir.as_mut() else {
                return Ok(false);
            };
            self.names.clear();
            self.ends.clear();
            let mut more = false;
            for entry in dir.by_ref() {
                let entry = entry.map_err(|e| e.raw_os_error())?;
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                self.names.extend_from_slice(name);
                self.ends.push(self.names.len());
                if self.ends.len() == BATCH {
                    more = true;
                    break;
                }
            }
            let mut start = 0usize;
            for &end in &self.ends {
                let name = &self.names[start..end];
                start = end;
                if self.skip.iter().any(|s| s.as_bytes() == name) {
                    visit(&EntryRef {
                        mount_point: true,
                        kind: Kind::Dir,
                        error: None,
                        ..EntryRef::failed(name, 0)
                    });
                    continue;
                }
                visit(&stat_at(fd, name));
            }
            Ok(more || !self.ends.is_empty())
        }
    }

    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        expect(
            clippy::unnecessary_cast,
            reason = "stat's field widths differ by platform (mode_t is u16 on Apple)"
        )
    )]
    pub(crate) fn stat_at<'a>(fd: BorrowedFd<'_>, name: &'a [u8]) -> EntryRef<'a> {
        let Ok(c) = std::ffi::CString::new(name) else {
            return EntryRef::failed(name, libc::EINVAL);
        };
        // SAFETY: `stat` is integers: all-zero is a value.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor, a NUL-terminated name, a writable stat.
        let r = unsafe {
            libc::fstatat(
                fd.as_raw_fd(),
                c.as_ptr(),
                &raw mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if r != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return EntryRef::failed(name, errno);
        }
        let mode = st.st_mode as u32;
        let kind = match mode & (libc::S_IFMT as u32) {
            m if m == libc::S_IFDIR as u32 => Kind::Dir,
            m if m == libc::S_IFREG as u32 => Kind::File,
            m if m == libc::S_IFLNK as u32 => Kind::Link,
            _ => Kind::Other,
        };
        #[cfg(target_vendor = "apple")]
        let dataless = st.st_flags & SF_DATALESS != 0;
        #[cfg(not(target_vendor = "apple"))]
        let dataless = false;
        EntryRef {
            name,
            kind,
            dev: super::dev_of(st.st_dev),
            ino: st.st_ino as u64,
            nlink: u32::try_from(st.st_nlink as u64).unwrap_or(u32::MAX).max(1),
            alloc: u64::try_from(st.st_blocks as i64)
                .unwrap_or(0)
                .saturating_mul(512),
            logical: u64::try_from(st.st_size as i64).unwrap_or(0),
            private: None,
            mtime_s: st.st_mtime as i64,
            dataless,
            exec: kind == Kind::File && any_exec(mode),
            dir_entries: None,
            mount_point: false,
            error: None,
        }
    }
}
