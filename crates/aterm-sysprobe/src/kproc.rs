// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel process records and arguments ([`record`], [`records`], [`table`], [`argv`],
//! [`ps_comm`], [`ps_args`], [`image_inode`]).

/// One process's kernel record (`struct kinfo_proc`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid (`e_ppid`; 0 for the kernel's own).
    pub ppid: u32,
    /// Its process group (`e_pgid`).
    pub pgid: i32,
    /// Its controlling terminal's foreground process group (`e_tpgid`); 0 with no terminal.
    pub tpgid: i32,
    /// Controlling terminal device (`e_tdev`), or `None` (`NODEV`).
    pub tty: Option<i32>,
    /// Its effective uid (`e_ucred.cr_uid`).
    pub uid: u32,
    /// The kernel's short name for it (`p_comm`: its image's basename, at most 16 bytes).
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

/// `pid`'s record; `None` when it is gone or its record cannot be read.
/// Use [`record_read`] when absence must be distinguished from an unread record.
#[must_use]
pub fn record(pid: u32) -> Option<Record> {
    record_read(pid).ok().flatten()
}

/// When `pid`'s live process started, in Unix seconds. An exited, unreaped process
/// owns no ongoing work; its retained kernel record supplies no live start.
#[must_use]
pub fn live_start_s(pid: u32) -> Option<u64> {
    record(pid)
        .filter(|record| !record.zombie)
        .map(|record| record.start_s)
}

/// `pid`'s record, distinguishing a definite absence from a failed read.
/// # Errors
/// An invalid pid, an unavailable platform reader, a kernel read error other than `ESRCH`,
/// a short or malformed record, or (`ResourceBusy`, nothing read) an exec of this process that
/// has its gate closed ([`crate::exec_gate`]). A successful empty read and `ESRCH` mean no such
/// process.
pub fn record_read(pid: u32) -> std::io::Result<Option<Record>> {
    imp::record_read(pid)
}

/// Every process's record; `None` where the table cannot be read, or where the kernel did not
/// answer within a bound (macOS: a process in the middle of an exec holds every read of the
/// table until that exec finishes). The read that outlasts the bound stays in the kernel, on a
/// thread of this process: a process that then replaces itself does so through
/// [`crate::exec_gate::close`], never a bare exec, which would wait on that thread for good.
#[must_use]
pub fn records() -> Option<Vec<Record>> {
    table()?.records()
}

/// One read of the whole table (`KERN_PROC_ALL`): every record the kernel held at that read, as
/// the kernel wrote them, so a reader parses only the records it asks for. `None` on the terms of
/// [`records`].
#[must_use]
pub fn table() -> Option<Table> {
    imp::table().map(Table)
}

/// One read of the whole process table ([`table`]), as the kernel wrote its records.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Table(Vec<u8>);

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Table({} records)", self.0.len() / KINFO_SIZE)
    }
}

/// `struct kinfo_proc`'s size, the stride of a [`Table`].
const KINFO_SIZE: usize = crate::jobs::darwin::KINFO_SIZE;

/// `kp_eproc.e_ucred.cr_uid`: the effective uid, `ps`'s `uid` column.
const KINFO_EUID_AT: usize = 420;

impl Table {
    /// Each record of this read, as the kernel wrote it.
    fn records_all(&self) -> impl Iterator<Item = &[u8; KINFO_SIZE]> {
        self.0.as_chunks::<KINFO_SIZE>().0.iter()
    }

    /// Every record of this read; `None` where one does not parse, and off macOS.
    #[must_use]
    pub fn records(&self) -> Option<Vec<Record>> {
        self.records_all().map(|r| imp::parse_record(r)).collect()
    }

    /// The records of this read whose pid `wanted` holds, only those parsed; one that does
    /// not parse is left out, and off macOS every one is.
    pub fn records_of(&self, wanted: impl Fn(u32) -> bool) -> impl Iterator<Item = Record> {
        self.records_all()
            .filter(move |r| read_u32(*r, crate::jobs::darwin::KINFO_PID_AT).is_some_and(&wanted))
            .filter_map(|r| imp::parse_record(r))
    }

    /// Each record's pid, its parent's, and whether the kernel reaps its children for it
    /// (`P_NOCLDWAIT`); `None` where one does not read.
    pub(crate) fn reaps_unwaited(&self) -> Option<Vec<(u32, u32, bool)>> {
        use crate::jobs::darwin::{KINFO_FLAG_AT, KINFO_PID_AT, KINFO_PPID_AT, P_NOCLDWAIT};
        self.records_all()
            .map(|r| {
                let flag = read_u32(r, KINFO_FLAG_AT)?;
                let ppid = read_u32(r, KINFO_PPID_AT)?;
                Some((read_u32(r, KINFO_PID_AT)?, ppid, flag & P_NOCLDWAIT != 0))
            })
            .collect()
    }

    /// A table holding `rows` (each record, and whether the kernel reaps its children for it),
    /// laid out as macOS's `kinfo_proc` lays them: a stand-in for a read of the table.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn of_records(rows: &[(Record, bool)]) -> Self {
        use crate::jobs::darwin::{
            KINFO_COMM_AT, KINFO_FLAG_AT, KINFO_PGID_AT, KINFO_PID_AT, KINFO_PPID_AT,
            KINFO_START_SEC_AT, KINFO_START_USEC_AT, KINFO_STAT_AT, KINFO_TDEV_AT, KINFO_TPGID_AT,
            NODEV, P_NOCLDWAIT,
        };
        let mut bytes = vec![0u8; rows.len() * KINFO_SIZE];
        for (r, (record, reaps)) in bytes.as_chunks_mut::<KINFO_SIZE>().0.iter_mut().zip(rows) {
            let mut put = |at: usize, word: &[u8]| r[at..at + word.len()].copy_from_slice(word);
            put(KINFO_START_SEC_AT, &record.start_s.to_ne_bytes());
            put(KINFO_START_USEC_AT, &record.start_us.to_ne_bytes());
            put(
                KINFO_FLAG_AT,
                &(if *reaps { P_NOCLDWAIT } else { 0 }).to_ne_bytes(),
            );
            // `p_stat`: `SZOMB` (5) or `SRUN` (2), xnu's `sys/proc.h`.
            put(KINFO_STAT_AT, &[if record.zombie { 5 } else { 2 }]);
            put(KINFO_PID_AT, &record.pid.to_ne_bytes());
            let comm = record.comm.as_bytes();
            put(KINFO_COMM_AT, &comm[..comm.len().min(16)]);
            put(KINFO_EUID_AT, &record.uid.to_ne_bytes());
            put(KINFO_PPID_AT, &record.ppid.to_ne_bytes());
            put(KINFO_PGID_AT, &record.pgid.to_ne_bytes());
            let tdev = record.tty.map_or(NODEV, i32::cast_unsigned);
            put(KINFO_TDEV_AT, &tdev.to_ne_bytes());
            put(KINFO_TPGID_AT, &record.tpgid.to_ne_bytes());
        }
        Self(bytes)
    }
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The signals `pid` ignores now (`p_sigignore`, bit `n - 1` for signal `n`); `None` when it is
/// gone, its record cannot be read, or this platform has no such record.
#[must_use]
pub fn ignored_signals(pid: u32) -> Option<u32> {
    imp::ignored_signals(pid)
}

/// `pid`'s argument words (`argv`, `argv[0]` first, never its environment), as `ps` reads them.
#[must_use]
pub fn argv(pid: u32) -> Option<Vec<Vec<u8>>> {
    let buf = crate::procargs::read(pid).ok()?;
    let words = crate::procargs::parse(&buf)?.argv;
    (!words.is_empty()).then(|| words.into_iter().map(<[u8]>::to_vec).collect())
}

/// The inode of the executable image `pid` runs.
#[must_use]
pub fn image_inode(pid: u32) -> Option<u64> {
    image_file(pid).map(|file| file.inode)
}

/// The kernel's file identity for a loaded executable. Inodes are scoped to their device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageFile {
    /// `vst_dev`, normalized as `std::os::unix::fs::MetadataExt::dev` normalizes `st_dev`.
    pub device: u64,
    /// `vst_ino`.
    pub inode: u64,
}

/// The device and inode of the executable image `pid` runs, read from its kernel mapping.
/// Unavailable reads and unsupported platforms return `None`.
#[must_use]
pub fn image_file(pid: u32) -> Option<ImageFile> {
    imp::image_file(pid)
}

/// `ps -o comm=` for `r` in `locale` ([`vis`], [`unread`]).
#[must_use]
pub fn ps_comm(r: &Record, locale: Locale) -> String {
    comm_read(r, locale).unwrap_or_else(|| unread(r, locale))
}

/// [`ps_comm`] where `r`'s name was read: `None` where the kernel reads `r`'s arguments and did
/// not answer them (its `KERN_PROCARGS2` read failed, as on a loaded host or mid-exec), where
/// `ps` prints the placeholder `(comm)`, which names no program and is never a read name.
#[must_use]
pub fn comm_read(r: &Record, locale: Locale) -> Option<String> {
    comm_read_by(r, locale, argv)
}

fn comm_read_by(
    r: &Record,
    locale: Locale,
    argv: impl FnOnce(u32) -> Option<Vec<Vec<u8>>>,
) -> Option<String> {
    if r.zombie {
        return Some(DEFUNCT.to_string());
    }
    if !imp::reads_args_of(r.uid) {
        return Some(unread(r, locale));
    }
    argv(r.pid)
        .and_then(|words| words.into_iter().next())
        .and_then(|word| vis(&word, locale))
}

/// `ps -ww -o args=` (`command=`) for `r` in `locale` ([`vis`], [`unread`]).
#[must_use]
pub fn ps_args(r: &Record, locale: Locale) -> String {
    if r.zombie {
        return DEFUNCT.to_string();
    }
    readable_argv(r)
        .and_then(|words| vis(&words.join(&b' '), locale))
        .unwrap_or_else(|| unread(r, locale))
}

/// Whether the kernel withholds `r`'s arguments and environment from this process for as long
/// as both run: `KERN_PROCARGS2` answers only for a process of this one's effective user, or to
/// root. A lasting state, never a failed read: every later read of `r` finds it again. Off macOS
/// no such rule is known here and this is `false`.
#[must_use]
pub fn withholds_args(r: &Record) -> bool {
    cfg!(target_os = "macos") && !imp::reads_args_of(r.uid)
}

/// [`argv`] of `r` where the kernel answers it.
fn readable_argv(r: &Record) -> Option<Vec<Vec<u8>>> {
    imp::reads_args_of(r.uid).then(|| argv(r.pid)).flatten()
}

/// What names a process whose arguments are not read.
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
    /// A UTF-8 locale (macOS's default).
    Utf8,
    /// The C locale (`LC_ALL=C`): a byte at or above 0x80 is escaped.
    C,
}

/// `text` as `ps` prints it.
#[must_use]
pub fn vis(text: &[u8], locale: Locale) -> Option<String> {
    imp::vis(text, locale)
}

/// macOS kernel and libc constants ([`crate::jobs::darwin::SDK_ROWS`]).
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("KERN_PROC_ALL", imp::KERN_PROC_ALL as i64),
    ("KERN_PROCARGS2", crate::procargs::KERN_PROCARGS2 as i64),
    ("KERN_ARGMAX", crate::procargs::KERN_ARGMAX as i64),
    (
        "offsetof(struct kinfo_proc, kp_eproc.e_ucred.cr_uid)",
        KINFO_EUID_AT as i64,
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
        "offsetof(struct proc_regionwithpathinfo, prp_vip.vip_vi.vi_stat.vst_dev)",
        imp::REGION_DEVICE as i64,
    ),
    (
        "sizeof(((struct proc_regionwithpathinfo *)0)->prp_vip.vip_vi.vi_stat.vst_dev)",
        4,
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::jobs::darwin::{
        KERN_PROC, KERN_PROC_PID, KINFO_COMM_AT, KINFO_PGID_AT, KINFO_PID_AT, KINFO_PPID_AT,
        KINFO_SIGIGNORE_AT, KINFO_SIZE, KINFO_START_SEC_AT, KINFO_START_USEC_AT, KINFO_STAT_AT,
        KINFO_TDEV_AT, KINFO_TPGID_AT, NODEV,
    };

    use super::{Locale, Record};
    use crate::exec_gate::{ExecGate, Reading};

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

    /// A `locale_t` from `newlocale`, never freed or changed.
    struct Ctype(*mut c_void);
    // SAFETY: The locale object is immutable once made and libc reads it
    // from whichever thread installs it.
    unsafe impl Send for Ctype {}
    // SAFETY: As for `Send`: shared reads of an immutable object.
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

    const MAX_VIS_BYTES: usize = 1024 * 1024;

    pub(super) fn vis(text: &[u8], locale: Locale) -> Option<String> {
        if text.len() > MAX_VIS_BYTES {
            return None;
        }
        let src = CString::new(text).ok()?;
        let loc = ctype(locale)?;
        let cap = text.len().checked_mul(8)?.checked_add(1)?;
        let mut dst = vec![0u8; cap];
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
        if let Some(nul) = dst.iter().position(|b| *b == 0) {
            dst.truncate(nul);
        }
        Some(String::from_utf8_lossy(&dst).into_owned())
    }

    /// `KERN_PROC_ALL` (`sys/sysctl.h`): every process's `kinfo_proc`.
    pub(super) const KERN_PROC_ALL: c_int = 0;
    /// `p_comm`'s length, `MAXCOMLEN + 1`.
    pub(super) const COMM_LEN: usize = 17;
    /// `SZOMB` (`sys/proc.h`): `p_stat` of an exited, unreaped process.
    pub(super) const SZOMB: u8 = 5;

    /// `PROC_PIDREGIONPATHINFO` (`sys/proc_info.h`).
    pub(super) const PROC_PIDREGIONPATHINFO: c_int = 8;
    pub(super) const REGION_SIZE: usize = 1272;
    pub(super) const REGION_ADDRESS: usize = 80;
    pub(super) const REGION_LEN: usize = 88;
    pub(super) const REGION_DEVICE: usize = 96;
    pub(super) const REGION_INODE: usize = 104;
    /// The regions walked before the image's.
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

    /// One `kinfo_proc` record's fields ([`super::Table`]).
    pub(super) fn parse_record(b: &[u8]) -> Option<Record> {
        let comm = b.get(KINFO_COMM_AT..KINFO_COMM_AT + COMM_LEN)?;
        let end = comm.iter().position(|c| *c == 0).unwrap_or(COMM_LEN);
        let tty = (u32_at(b, KINFO_TDEV_AT)? != NODEV).then_some(i32_at(b, KINFO_TDEV_AT)?);
        Some(Record {
            pid: u32_at(b, KINFO_PID_AT)?,
            ppid: u32_at(b, KINFO_PPID_AT)?,
            pgid: i32_at(b, KINFO_PGID_AT)?,
            tpgid: i32_at(b, KINFO_TPGID_AT)?,
            tty,
            uid: u32_at(b, super::KINFO_EUID_AT)?,
            comm: String::from_utf8_lossy(&comm[..end]).into_owned(),
            start_s: u64_at(b, KINFO_START_SEC_AT)?,
            start_us: u32_at(b, KINFO_START_USEC_AT)?,
            zombie: *b.get(KINFO_STAT_AT)? == SZOMB,
        })
    }

    /// One `sysctl` read of `mib`, a `kern.proc` name, into `buf`; the bytes it wrote. Its
    /// caller reads inside this process's exec gate ([`crate::exec_gate`]) and shows the
    /// [`Reading`] that says so: one read, however many `sysctl`s it takes, counts once there.
    fn sysctl_into(
        _in_kernel: &Reading,
        mib: &mut [c_int],
        buf: &mut [u8],
    ) -> std::io::Result<usize> {
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

    /// The size `mib`, a `kern.proc` name, would answer (`sysctl` with no buffer), asked inside
    /// the exec gate as [`sysctl_into`] reads.
    fn sysctl_len(_in_kernel: &Reading, mib: &mut [c_int]) -> Option<usize> {
        let mut len: libc::size_t = 0;
        let n = libc::c_uint::try_from(mib.len()).ok()?;
        // SAFETY: A null `oldp` asks only for the size, written to `len`.
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

    /// `pid`'s whole `kinfo_proc`, about that pid; `None` when no process has it. Read on the
    /// caller's thread: a process held in the middle of an exec holds a read of its own record
    /// alone, so a lane shared by every pid would let one held process fail the reads of all.
    /// Read inside this process's exec gate ([`crate::exec_gate`]): `ResourceBusy`, with nothing
    /// read, while an exec of this process has it closed.
    fn kinfo(pid: u32) -> std::io::Result<Option<[u8; KINFO_SIZE]>> {
        let native_pid = c_int::try_from(pid).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        let mut mib = [libc::CTL_KERN, KERN_PROC, KERN_PROC_PID, native_pid];
        let mut buf = [0u8; KINFO_SIZE];
        let Some(in_kernel) = crate::exec_gate::reading() else {
            return Err(std::io::ErrorKind::ResourceBusy.into());
        };
        // Marked once the gate lets the read be made: a lane's crumb names a call its reader
        // is in, never one a closed gate refused.
        crate::lane::mark("sysctl KERN_PROC_PID", Some(pid));
        let n = match sysctl_into(&in_kernel, &mut mib, &mut buf) {
            Ok(n) => n,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(None),
            Err(error) => return Err(error),
        };
        match n {
            0 => Ok(None),
            KINFO_SIZE if u32_at(&buf, KINFO_PID_AT) == Some(pid) => Ok(Some(buf)),
            _ => Err(std::io::ErrorKind::InvalidData.into()),
        }
    }

    pub(super) fn record_read(pid: u32) -> std::io::Result<Option<Record>> {
        kinfo(pid)?
            .map(|buf| parse_record(&buf).ok_or_else(|| std::io::ErrorKind::InvalidData.into()))
            .transpose()
    }

    pub(super) fn ignored_signals(pid: u32) -> Option<u32> {
        u32_at(&kinfo(pid).ok()??, KINFO_SIGIGNORE_AT)
    }

    /// How long a read of the whole table is waited for ([`table`]). One read takes about 100 µs
    /// (749 records, m22, 2026-10-25), but the kernel answers it only once each process it lists
    /// is through an exec in progress. On 2026-11-06 a `posix_spawn` that never finished (the
    /// installed aterm's `secrets serve` starting `dns-sd`) held every `KERN_PROC_ALL` read on the
    /// machine for 13 minutes, `ps` and the process holding the capacity ledger's lock among them,
    /// while libproc read the same process at once.
    const TABLE_WAIT: Duration = Duration::from_secs(1);

    /// This process's reads of the table, one at a time on a worker of their own
    /// ([`crate::lane`]): a read the kernel holds holds that worker alone, names the lane
    /// stalled, and every later read gives up at once until it returns. The worker reads inside
    /// this process's exec gate ([`inside`]): the read its asker gave up on is still in the
    /// kernel, on a thread of this process, and an exec made over it would stay in exec for as
    /// long as the kernel holds that read.
    static TABLE: crate::lane::Lane<(), Option<Vec<u8>>> =
        crate::lane::Lane::new("kproc_table", |()| {
            inside(crate::exec_gate::process(), table_now)
        })
        .registered();

    /// The whole table's `kinfo_proc` records (`KERN_PROC_ALL`), a whole number of them; `None`
    /// also where the kernel did not answer within [`table_wait`] or holds an earlier read (the
    /// lane is then stalled), and while an exec of this process has its gate closed
    /// ([`crate::exec_gate`]), which stalls nothing.
    pub(super) fn table() -> Option<Vec<u8>> {
        TABLE.ask_within((), table_wait()).answer().flatten()
    }

    /// How long [`table`] waits: [`TABLE_WAIT`], or, in a Cargo test binary's debug build
    /// ([`crate::test_binary::this_is_a_test_binary`]), [`TEST_TABLE_WAIT`]. A test that reads the
    /// real table needs its answer: the read must happen, and a loaded machine may answer it past
    /// the production bound, so there it waits as a hang detector, and a read the kernel holds
    /// still fails it, by name, after a minute. The lane's stall is proved over its own stand-in
    /// readers (`lane_tests.rs`).
    pub(super) fn table_wait() -> Duration {
        #[cfg(debug_assertions)]
        if crate::test_binary::this_is_a_test_binary() {
            return TEST_TABLE_WAIT;
        }
        TABLE_WAIT
    }

    /// [`table_wait`] in a test binary: the tests' hang detector (`aterm_spec::wait::MUST_HAPPEN`,
    /// which this crate's library cannot name).
    #[cfg(debug_assertions)]
    const TEST_TABLE_WAIT: Duration = Duration::from_secs(60);

    /// A lane's read of the process table, made inside `gate`: `read` gets the [`Reading`] this
    /// thread, the lane's worker, holds from before its first `sysctl` until `read` returns,
    /// however long the kernel holds it and whether or not its asker still waits, so an exec
    /// through `gate` waits for it and is refused while it is still there
    /// ([`crate::exec_gate`]). `None`, with `read` not called, while an exec has `gate` closed:
    /// the worker answers its asker at once, with nothing in the kernel, so the lane is not
    /// stalled.
    pub(super) fn inside<T>(gate: &'static ExecGate, read: fn(&Reading) -> Option<T>) -> Option<T> {
        let in_kernel = gate.reading()?;
        read(&in_kernel)
    }

    /// The bytes the table's last whole read answered, 0 before one did: the next read sizes its
    /// buffer from them and asks the kernel for no size first ([`table_sized`]). Only the lane's
    /// worker reads the table ([`TABLE`]), one read at a time.
    static LAST_TABLE_LEN: AtomicUsize = AtomicUsize::new(0);

    /// [`table`]'s read itself, unbounded: one read inside the exec gate, whichever of its
    /// `sysctl`s the kernel holds.
    fn table_now(in_kernel: &Reading) -> Option<Vec<u8>> {
        crate::lane::mark("sysctl KERN_PROC_ALL", None);
        table_sized(
            &LAST_TABLE_LEN,
            || sysctl_len(in_kernel, &mut [libc::CTL_KERN, KERN_PROC, KERN_PROC_ALL]),
            |buf| {
                sysctl_into(
                    in_kernel,
                    &mut [libc::CTL_KERN, KERN_PROC, KERN_PROC_ALL],
                    buf,
                )
            },
        )
    }

    /// The whole table, read by `read` (one `KERN_PROC_ALL` read into the buffer it is given:
    /// the bytes it wrote, or the kernel's error). The buffer is the size `last` keeps (the bytes
    /// the last whole read answered, which this updates) plus a slack of an eighth and 16
    /// records. Where `last` keeps none, or the table outgrew that buffer (`ENOMEM`: xnu writes
    /// the records that fit and refuses the rest), it asks `size` (the bytes the read would
    /// answer, with the kernel's own slack) and reads into a buffer of that size plus the same
    /// slack, up to four times, as a table that keeps growing may need. `None` where `size`
    /// answers none, a read answers part of a record, or the kernel answers any other error. A
    /// read that fits answers every record the kernel held at that read, whatever sized its
    /// buffer: the size answer only sizes it. A table that outgrew the kept size and its slack
    /// (by more than an eighth and 16 records since the last whole read, as under a burst of new
    /// processes) costs one read more than a read sized first: the refused read, which walks the
    /// whole table before xnu refuses it, then the size and the read, three `sysctl`s where a
    /// read sized first makes two, and at most five reads where sizing before each makes four.
    pub(super) fn table_sized(
        last: &AtomicUsize,
        mut size: impl FnMut() -> Option<usize>,
        mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
    ) -> Option<Vec<u8>> {
        // `None`: the table outgrew `len` and its slack; `Some` is the read's answer.
        let mut into = |len: usize| -> Option<Option<Vec<u8>>> {
            let mut buf = vec![0u8; len + len / 8 + KINFO_SIZE * 16];
            match read(&mut buf) {
                Ok(n) if n % KINFO_SIZE == 0 => {
                    buf.truncate(n);
                    last.store(n, Ordering::Relaxed);
                    Some(Some(buf))
                }
                Ok(_) => Some(None),
                Err(e) if e.raw_os_error() == Some(libc::ENOMEM) => None,
                Err(_) => Some(None),
            }
        };
        let kept = last.load(Ordering::Relaxed);
        if kept > 0
            && let Some(answer) = into(kept)
        {
            return answer;
        }
        for _ in 0..4 {
            if let Some(answer) = into(size()?) {
                return answer;
            }
        }
        None
    }

    pub(super) fn image_file(pid: u32) -> Option<super::ImageFile> {
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
                return Some(super::ImageFile {
                    device: i32_at(&buf, REGION_DEVICE)? as u64,
                    inode,
                });
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

    pub(super) fn record_read(_pid: u32) -> std::io::Result<Option<Record>> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub(super) fn table() -> Option<Vec<u8>> {
        None
    }

    pub(super) fn parse_record(_record: &[u8]) -> Option<Record> {
        None
    }

    pub(super) fn ignored_signals(_pid: u32) -> Option<u32> {
        None
    }

    pub(super) fn image_file(_pid: u32) -> Option<super::ImageFile> {
        None
    }
}

#[cfg(test)]
#[path = "kproc_tests.rs"]
mod tests;
