// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KERNEL'S PROCESS RECORDS, READ AS `ps` AND `lsof` READ THEM, WITHOUT
//! RUNNING EITHER.
//!
//! A `ps` is a child process: on a loaded Mac each costs 0.02-0.4 s of wall
//! (its `tty` column alone scans `/dev` once per process), and a harness that
//! reads a tab's job a few times a second paid seconds of every attach to
//! them. These readers answer in microseconds from the same kernel records:
//!
//! * [`record`], [`records`] — `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID |
//!   KERN_PROC_ALL)`'s `struct kinfo_proc`, the table `ps` itself reads: pid,
//!   parent, process group, controlling terminal and its foreground group,
//!   effective uid, `p_comm`, start time, zombie or not. Every user's process
//!   answers (libproc's `PROC_PIDTBSDINFO` refuses another user's: `EPERM`).
//! * [`argv`] — `KERN_PROCARGS2`'s argument words, for a process of this uid
//!   (any, as root), read and parsed by `crate::procargs`, the workspace's
//!   one reader of that record. `ps` is setuid root and reads every process's, so
//!   [`ps_comm`] and [`ps_args`], `ps`'s `comm` and `args` columns, name
//!   another user's process by its image's path (`proc_pidpath`, which
//!   answers for every user's): the one way the two can differ.
//! * [`image_inode`] — `proc_pidinfo(PROC_PIDREGIONPATHINFO)`'s first
//!   vnode-backed region: the inode of the executable the process mapped,
//!   kept after a new file is renamed over its path (`lsof -d txt`'s first
//!   entry). This uid's processes only, as `lsof` run by this uid.
//!
//! Every other platform answers `None`: their callers keep their own readers
//! (`/proc`, or `ps`).
//!
//! A process can exit, exec or be reparented between two reads: each reader
//! answers for the moment it read, as `ps` does.

/// One process's kernel record (`struct kinfo_proc`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid (`e_ppid`; 0 for the kernel's own).
    pub ppid: u32,
    /// Its process group (`e_pgid`).
    pub pgid: i32,
    /// Its controlling terminal's foreground process group (`e_tpgid`); 0
    /// with no terminal.
    pub tpgid: i32,
    /// Its controlling terminal's device (`e_tdev`), `None` with none
    /// (`NODEV`, which `ps` prints `??`).
    pub tty: Option<i32>,
    /// Its effective uid (`e_ucred.cr_uid`).
    pub uid: u32,
    /// The kernel's short name for it (`p_comm`: its image's basename, at
    /// most 16 bytes).
    pub comm: String,
    /// When it started: unix seconds (`p_starttime.tv_sec`).
    pub start_s: u64,
    /// The microseconds within [`Self::start_s`].
    pub start_us: u32,
    /// Exited and not yet reaped (`p_stat == SZOMB`).
    pub zombie: bool,
}

/// What `ps` prints for a zombie's `comm` and `args`.
pub const DEFUNCT: &str = "<defunct>";

/// `pid`'s record; `None` when no process has that pid (or the platform has
/// no reader here).
#[must_use]
pub fn record(pid: u32) -> Option<Record> {
    imp::record(pid)
}

/// Every process's record; `None` where the table cannot be read.
#[must_use]
pub fn records() -> Option<Vec<Record>> {
    imp::records()
}

/// `pid`'s argument words (`argv`, `argv[0]` first, never its environment),
/// as `ps` reads them: every word the kernel's answer holds
/// ([`crate::procargs::parse`]). `None` when they cannot be read: the
/// process is gone, another user's, a zombie, or its argc is 0, or its record
/// is past what the kernel hands over (`ps` prints that record's tail as
/// words; [`crate::procargs::read`]); and off macOS.
#[must_use]
pub fn argv(pid: u32) -> Option<Vec<Vec<u8>>> {
    let buf = crate::procargs::read(pid)?;
    let words = crate::procargs::parse(&buf)?.argv;
    (!words.is_empty()).then(|| words.into_iter().map(<[u8]>::to_vec).collect())
}

/// The inode of the executable image `pid` runs: its lowest vnode-backed
/// mapping, which keeps the file the process mapped whatever has happened to
/// its path since. `None` when unreadable (gone, another user's).
#[must_use]
pub fn image_inode(pid: u32) -> Option<u64> {
    imp::image_inode(pid)
}

/// `ps -o comm=` for `r` in `locale`: `<defunct>` for a zombie, else
/// `argv[0]` escaped as `ps` escapes it ([`vis`]), else [`unread`].
#[must_use]
pub fn ps_comm(r: &Record, locale: Locale) -> String {
    if r.zombie {
        return DEFUNCT.to_string();
    }
    readable_argv(r)
        .and_then(|words| words.into_iter().next())
        .and_then(|word| vis(&word, locale))
        .unwrap_or_else(|| unread(r, locale))
}

/// `ps -ww -o args=` (`command=`) for `r` in `locale`: `<defunct>` for a
/// zombie, else its argument words joined by spaces and escaped as one
/// string, as `ps` escapes it ([`vis`]), else [`unread`].
#[must_use]
pub fn ps_args(r: &Record, locale: Locale) -> String {
    if r.zombie {
        return DEFUNCT.to_string();
    }
    readable_argv(r)
        .and_then(|words| vis(&words.join(&b' '), locale))
        .unwrap_or_else(|| unread(r, locale))
}

/// [`argv`] of `r` where the kernel answers it: never another user's
/// process unless this one is root (`KERN_PROCARGS2` refuses across
/// effective uids), which is not asked.
fn readable_argv(r: &Record) -> Option<Vec<Vec<u8>>> {
    imp::reads_args_of(r.uid).then(|| argv(r.pid)).flatten()
}

/// What names a process whose arguments are not read: `ps`'s own fallback,
/// `(p_comm)`, where `ps` could not read them either (a process of this
/// uid, or any as root: the kernel refuses `KERN_PROCARGS2` only across
/// effective uids). Another user's process, whose `argv[0]` `ps` (setuid
/// root) reads, is named by its image's path, escaped as `argv[0]` would
/// be: `argv[0]` is that path for nearly every daemon, where `p_comm` is
/// its name cut to 16 bytes (measured 2026-10-20: the path's basename was
/// `ps`'s `comm` basename for 327 of 327 other users' processes, `p_comm`
/// for 217); `p_comm` where the path is unread too.
fn unread(r: &Record, locale: Locale) -> String {
    if imp::reads_args_of(r.uid) {
        let comm = format!("({})", r.comm);
        vis(comm.as_bytes(), locale).unwrap_or(comm)
    } else {
        imp::image_path(r.pid)
            .and_then(|path| vis(&path, locale))
            .unwrap_or_else(|| r.comm.clone())
    }
}

/// The character classes `ps` escapes in: its `LC_CTYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    /// A UTF-8 locale (macOS's default; every macOS UTF-8 locale shares one
    /// `LC_CTYPE`): a printable character is kept.
    Utf8,
    /// The C locale (`LC_ALL=C`): a byte at or above 0x80 is escaped.
    C,
}

/// `text` as `ps` prints it: the C library's `strnvis(…, VIS_TAB | VIS_NL |
/// VIS_NOSLASH)`, the call `ps` makes, in `locale`'s character classes (this
/// thread's locale is switched for the call and put back). A newline is
/// `\012`, a tab `\011`, another control character `^A`, a high byte in the
/// C locale `M-C` or `M^@`; a UTF-8 locale keeps what prints, and a string
/// holding a byte that is no UTF-8 is written out byte by byte, up to the
/// first NUL that writes. `None` off macOS or when the locale cannot be
/// loaded.
#[must_use]
pub fn vis(text: &[u8], locale: Locale) -> Option<String> {
    imp::vis(text, locale)
}

/// Every constant the macOS arm reads the kernel and the C library by that
/// the job probe's table ([`crate::jobs::darwin::SDK_ROWS`]) does not
/// already hold, as the C expression the SDK must agree with:
/// `tests/proc_info_sdk.rs` asserts each on `arm64` and `x86_64`.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("KERN_PROC_ALL", imp::KERN_PROC_ALL as i64),
    ("KERN_PROCARGS2", crate::procargs::KERN_PROCARGS2 as i64),
    ("KERN_ARGMAX", crate::procargs::KERN_ARGMAX as i64),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_ucred.cr_uid)",
        imp::KINFO_EUID_AT as i64,
    ),
    ("MAXCOMLEN + 1", imp::COMM_LEN as i64),
    ("SZOMB", imp::SZOMB as i64),
    ("PROC_PIDREGIONPATHINFO", imp::PROC_PIDREGIONPATHINFO as i64),
    (
        "sizeof(struct proc_regionwithpathinfo)",
        imp::REGION_SIZE as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_prinfo.pri_address)",
        imp::REGION_ADDRESS as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_prinfo.pri_size)",
        imp::REGION_LEN as i64,
    ),
    (
        "offsetof(struct proc_regionwithpathinfo, prp_vip.vip_vi.vi_stat.vst_ino)",
        imp::REGION_INODE as i64,
    ),
    ("VIS_TAB", imp::VIS_TAB as i64),
    ("VIS_NL", imp::VIS_NL as i64),
    ("VIS_NOSLASH", imp::VIS_NOSLASH as i64),
    ("LC_CTYPE_MASK", imp::LC_CTYPE_MASK as i64),
];

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    // `struct kinfo_proc`'s size and offsets are the job probe's, held to the
    // SDK there; every other constant this arm reads by is a row of
    // [`super::SDK_ROWS`].
    use crate::jobs::darwin::{
        KERN_PROC, KERN_PROC_PID, KINFO_COMM_AT, KINFO_PGID_AT, KINFO_PID_AT, KINFO_PPID_AT,
        KINFO_SIZE, KINFO_START_SEC_AT, KINFO_START_USEC_AT, KINFO_STAT_AT, KINFO_TDEV_AT,
        KINFO_TPGID_AT, NODEV,
    };

    use super::{Locale, Record};

    // `<vis.h>` and `<xlocale.h>`'s calls, which the first-party `libc` does
    // not declare: `ps`'s own escaper and the per-thread locale it runs under.
    unsafe extern "C" {
        fn strnvis(dst: *mut c_char, dlen: usize, src: *const c_char, flag: c_int) -> c_int;
        fn newlocale(mask: c_int, locale: *const c_char, base: *mut c_void) -> *mut c_void;
        fn uselocale(locale: *mut c_void) -> *mut c_void;
    }
    /// `<vis.h>`: the flags `ps` passes.
    pub(super) const VIS_TAB: c_int = 0x0008;
    pub(super) const VIS_NL: c_int = 0x0010;
    pub(super) const VIS_NOSLASH: c_int = 0x0040;
    /// `<xlocale.h>`.
    pub(super) const LC_CTYPE_MASK: c_int = 1 << 1;

    /// A `locale_t` from `newlocale`, never freed or changed: macOS's libc
    /// lets any thread install one with `uselocale`.
    struct Ctype(*mut c_void);
    // SAFETY: the locale object is immutable once made and libc reads it
    // from whichever thread installs it.
    unsafe impl Send for Ctype {}
    // SAFETY: as for `Send`: shared reads of an immutable object.
    unsafe impl Sync for Ctype {}

    fn ctype(locale: Locale) -> Option<*mut c_void> {
        static C: OnceLock<Ctype> = OnceLock::new();
        static UTF8: OnceLock<Ctype> = OnceLock::new();
        let (cell, name) = match locale {
            Locale::C => (&C, c"C"),
            Locale::Utf8 => (&UTF8, c"UTF-8"),
        };
        let made = cell.get_or_init(|| {
            // SAFETY: a NUL-terminated name and no base locale; a null
            // answer (no such locale) is kept and read as none.
            Ctype(unsafe { newlocale(LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut()) })
        });
        (!made.0.is_null()).then_some(made.0)
    }

    pub(super) fn reads_args_of(uid: u32) -> bool {
        // SAFETY: `geteuid` reads this process's effective uid.
        let me = unsafe { libc::geteuid() };
        uid == me || me == 0
    }

    /// `proc_pidpath`'s answer for `pid`, any user's ([`crate::process_path`]).
    pub(super) fn image_path(pid: u32) -> Option<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt as _;
        crate::process_path(pid).map(|p| p.as_os_str().as_bytes().to_vec())
    }

    pub(super) fn vis(text: &[u8], locale: Locale) -> Option<String> {
        let src = CString::new(text).ok()?;
        let loc = ctype(locale)?;
        // Four bytes per input byte is `strvis`'s own bound (`ps` allocates
        // it); twice that leaves room for a UTF-8 locale's re-encoding.
        let mut dst = vec![0u8; text.len() * 8 + 1];
        // SAFETY: `loc` is a live locale object; the previous one is put back
        // below, on this thread, before anything else runs on it.
        let previous = unsafe { uselocale(loc) };
        // SAFETY: `dst` is writable for `dst.len()` bytes, the length passed,
        // and `src` is NUL-terminated.
        let n = unsafe {
            strnvis(
                dst.as_mut_ptr().cast::<c_char>(),
                dst.len(),
                src.as_ptr(),
                VIS_TAB | VIS_NL | VIS_NOSLASH,
            )
        };
        // SAFETY: `previous` is what `uselocale` answered for this thread.
        unsafe { uselocale(previous) };
        dst.truncate(usize::try_from(n).ok()?);
        // `ps` prints a C string: a NUL its escaper wrote (a code point's
        // zero byte, written out after a byte that is no UTF-8) ends it.
        if let Some(nul) = dst.iter().position(|b| *b == 0) {
            dst.truncate(nul);
        }
        Some(String::from_utf8_lossy(&dst).into_owned())
    }

    /// `KERN_PROC_ALL` (`sys/sysctl.h`): every process's `kinfo_proc`.
    pub(super) const KERN_PROC_ALL: c_int = 0;
    /// `kp_eproc.e_ucred.cr_uid`: the effective uid, `ps`'s `uid` column.
    pub(super) const KINFO_EUID_AT: usize = 420;
    /// `p_comm`'s length, `MAXCOMLEN + 1`.
    pub(super) const COMM_LEN: usize = 17;
    /// `SZOMB` (`sys/proc.h`): `p_stat` of an exited, unreaped process.
    pub(super) const SZOMB: u8 = 5;

    /// `PROC_PIDREGIONPATHINFO` (`sys/proc_info.h`) and its answer, `struct
    /// proc_regionwithpathinfo`: the region's address and size, and the
    /// backing vnode's inode.
    pub(super) const PROC_PIDREGIONPATHINFO: c_int = 8;
    pub(super) const REGION_SIZE: usize = 1272;
    pub(super) const REGION_ADDRESS: usize = 80;
    pub(super) const REGION_LEN: usize = 88;
    pub(super) const REGION_INODE: usize = 104;
    /// The regions walked before the image's: none precede it in a Mach-O
    /// process (`__PAGEZERO` maps no region), so a handful is generous.
    const REGIONS: usize = 64;

    fn u32_at(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }

    fn i32_at(b: &[u8], at: usize) -> Option<i32> {
        Some(i32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }

    fn u64_at(b: &[u8], at: usize) -> Option<u64> {
        Some(u64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
    }

    fn parse(b: &[u8]) -> Option<Record> {
        let comm = b.get(KINFO_COMM_AT..KINFO_COMM_AT + COMM_LEN)?;
        let end = comm.iter().position(|c| *c == 0).unwrap_or(COMM_LEN);
        let tty = (u32_at(b, KINFO_TDEV_AT)? != NODEV).then_some(i32_at(b, KINFO_TDEV_AT)?);
        Some(Record {
            pid: u32_at(b, KINFO_PID_AT)?,
            ppid: u32_at(b, KINFO_PPID_AT)?,
            pgid: i32_at(b, KINFO_PGID_AT)?,
            tpgid: i32_at(b, KINFO_TPGID_AT)?,
            tty,
            uid: u32_at(b, KINFO_EUID_AT)?,
            comm: String::from_utf8_lossy(&comm[..end]).into_owned(),
            start_s: u64_at(b, KINFO_START_SEC_AT)?,
            start_us: u32_at(b, KINFO_START_USEC_AT)?,
            zombie: *b.get(KINFO_STAT_AT)? == SZOMB,
        })
    }

    /// One `sysctl` read of `mib` into `buf`; the bytes it wrote.
    fn sysctl_into(mib: &mut [c_int], buf: &mut [u8]) -> std::io::Result<usize> {
        let mut len: libc::size_t = buf.len();
        let n = libc::c_uint::try_from(mib.len()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        // SAFETY: `mib` holds `n` ints; `buf` is writable for `len` bytes and
        // the kernel writes at most `len`, reporting how many in `len`.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                n,
                buf.as_mut_ptr().cast::<c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            Ok(len.min(buf.len()))
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// The size `mib` would answer (`sysctl` with no buffer).
    fn sysctl_len(mib: &mut [c_int]) -> Option<usize> {
        let mut len: libc::size_t = 0;
        let n = libc::c_uint::try_from(mib.len()).ok()?;
        // SAFETY: a null `oldp` asks only for the size, written to `len`.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                n,
                std::ptr::null_mut(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0).then_some(len)
    }

    pub(super) fn record(pid: u32) -> Option<Record> {
        let pid = c_int::try_from(pid).ok()?;
        let mut mib = [libc::CTL_KERN, KERN_PROC, KERN_PROC_PID, pid];
        let mut buf = [0u8; KINFO_SIZE];
        // An absent pid answers 0 bytes, not an error.
        let n = sysctl_into(&mut mib, &mut buf).ok()?;
        (n == KINFO_SIZE).then(|| parse(&buf)).flatten()
    }

    pub(super) fn records() -> Option<Vec<Record>> {
        let mut mib = [libc::CTL_KERN, KERN_PROC, KERN_PROC_ALL, 0];
        // The table grows between the size and the read: room for an eighth
        // more, and a few tries before it is called unreadable.
        for _ in 0..4 {
            let len = sysctl_len(&mut mib[..3])?;
            let mut buf = vec![0u8; len + len / 8 + KINFO_SIZE * 16];
            match sysctl_into(&mut mib[..3], &mut buf) {
                Ok(n) if n % KINFO_SIZE == 0 => {
                    let (records, _) = buf[..n].as_chunks::<KINFO_SIZE>();
                    return records.iter().map(|r| parse(r)).collect();
                }
                Ok(_) => return None,
                Err(e) if e.raw_os_error() == Some(libc::ENOMEM) => {}
                Err(_) => return None,
            }
        }
        None
    }

    pub(super) fn image_inode(pid: u32) -> Option<u64> {
        let pid = c_int::try_from(pid).ok().filter(|p| *p > 0)?;
        let mut at = 0u64;
        for _ in 0..REGIONS {
            let mut buf = [0u8; REGION_SIZE];
            // SAFETY: `buf` is writable for `REGION_SIZE` bytes, the size
            // passed; libproc returns how many it wrote.
            let n = unsafe {
                libc::proc_pidinfo(
                    pid,
                    PROC_PIDREGIONPATHINFO,
                    at,
                    buf.as_mut_ptr().cast::<c_void>(),
                    REGION_SIZE as c_int,
                )
            };
            if usize::try_from(n).ok()? != REGION_SIZE {
                return None;
            }
            let inode = u64_at(&buf, REGION_INODE)?;
            if inode != 0 {
                return Some(inode);
            }
            let next = u64_at(&buf, REGION_ADDRESS)?.checked_add(u64_at(&buf, REGION_LEN)?)?;
            if next <= at {
                return None;
            }
            at = next;
        }
        None
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Locale, Record};

    pub(super) fn reads_args_of(_uid: u32) -> bool {
        false
    }

    pub(super) fn image_path(_pid: u32) -> Option<Vec<u8>> {
        None
    }

    pub(super) fn vis(_text: &[u8], _locale: Locale) -> Option<String> {
        None
    }

    pub(super) fn record(_pid: u32) -> Option<Record> {
        None
    }

    pub(super) fn records() -> Option<Vec<Record>> {
        None
    }

    pub(super) fn image_inode(_pid: u32) -> Option<u64> {
        None
    }
}

#[cfg(test)]
#[path = "kproc_tests.rs"]
mod tests;
