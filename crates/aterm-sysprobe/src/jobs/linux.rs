// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The job probe's Linux arm: `/proc/<pid>/…` read under a root.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    Counters, FdKind, FdReading, Knote, KnoteFilter, MAX_FD_DETAILS, MAX_FDS, MAX_KNOTES,
    MAX_THREADS, ProcFacts, RunState, TcpState, ThreadReading,
};
use crate::linux::tcp::TcpDump;

/// The kernel's clock ticks per second (`_SC_CLK_TCK`), 100 where unread.
#[must_use]
pub fn clock_ticks() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `sysconf` reads a constant.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u64::try_from(hz).ok().filter(|h| *h > 0).unwrap_or(100)
    }
    #[cfg(not(target_os = "linux"))]
    {
        100
    }
}

/// The fields of `/proc/<pid>/stat` the probe reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatLine {
    /// `comm`, between the first `(` and the last `)`.
    pub name: String,
    /// The state letter.
    pub state: u8,
    /// The parent.
    pub ppid: u32,
    /// The process group.
    pub pgrp: u32,
    /// The session.
    pub session: u32,
    /// The controlling terminal's device, 0 for none ([`parse_stat`] reads a negative one as
    /// its 32 bits).
    pub tty_nr: u32,
    /// The terminal's foreground group, `-1` (here 0) for none.
    pub tpgid: u32,
    /// The kernel's flags for it (`PF_*`).
    pub flags: u64,
    /// Major faults.
    pub majflt: u64,
    /// User time, ticks.
    pub utime: u64,
    /// System time, ticks.
    pub stime: u64,
    /// Reaped children's user time, ticks.
    pub cutime: u64,
    /// Reaped children's system time, ticks.
    pub cstime: u64,
    /// The thread count.
    pub threads: u64,
    /// Start time, ticks since boot.
    pub start: u64,
}

impl StatLine {
    /// Whether it is a kernel thread (`PF_KTHREAD`): it maps no user memory.
    #[must_use]
    pub const fn kernel_thread(&self) -> bool {
        self.flags & 0x0020_0000 != 0
    }

    /// Whether it ran a program since its fork (`PF_FORKNOEXEC` clear).
    #[must_use]
    pub const fn exec(&self) -> bool {
        self.flags & 0x0000_0040 == 0
    }

    /// Whether it is exiting (`PF_EXITING`, set as its exit starts and never cleared): its exit
    /// closes every descriptor before it is a zombie.
    #[must_use]
    pub const fn exiting(&self) -> bool {
        self.flags & 0x0000_0004 != 0
    }
}

/// The controlling terminal's device from `stat`'s `tty_nr`: the kernel prints the encoded device
/// as a C `int`, so a pts minor of 2^19 or more prints negative and is read back as its 32 bits. A
/// field that is neither an `int` nor a `u32` makes the line malformed, never "no terminal".
fn tty_device(field: &str) -> Option<u32> {
    let t: i64 = crate::parse_int(field)?;
    u32::try_from(t)
        .ok()
        .or_else(|| i32::try_from(t).ok().map(i32::cast_unsigned))
}

/// Parse a `/proc/<pid>/stat` line; `None` when it is malformed.
#[must_use]
pub fn parse_stat(text: &str) -> Option<StatLine> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_owned();
    let f: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let num = |i: usize| -> Option<u64> { crate::parse_uint(f.get(i)?) };
    let tpgid: i64 = crate::parse_int(f.get(5)?)?;
    let cut = |t: i64| u64::try_from(t.max(0)).unwrap_or(0);
    let cutime: i64 = crate::parse_int(f.get(13)?)?;
    let cstime: i64 = crate::parse_int(f.get(14)?)?;
    Some(StatLine {
        name,
        state: *f.first()?.as_bytes().first()?,
        ppid: crate::parse_uint(f.get(1)?)?,
        pgrp: crate::parse_uint(f.get(2)?)?,
        session: crate::parse_uint(f.get(3)?)?,
        tty_nr: tty_device(f.get(4)?)?,
        tpgid: u32::try_from(tpgid).unwrap_or(0),
        flags: num(6)?,
        majflt: num(9)?,
        utime: num(11)?,
        stime: num(12)?,
        cutime: cut(cutime),
        cstime: cut(cstime),
        threads: num(17)?,
        start: num(19)?,
    })
}

/// `/proc/<pid>/io`'s counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoLine {
    /// Bytes written through any descriptor.
    pub wchar: Option<u64>,
    /// Read-family system calls.
    pub syscr: Option<u64>,
    /// Write-family system calls.
    pub syscw: Option<u64>,
    /// Bytes read from storage.
    pub read_bytes: Option<u64>,
    /// Bytes written to storage.
    pub write_bytes: Option<u64>,
}

fn keyed(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| crate::parse_uint(v.split_whitespace().next()?))?
    })
}

/// Parse `/proc/<pid>/io`.
#[must_use]
pub fn parse_io(text: &str) -> IoLine {
    IoLine {
        wchar: keyed(text, "wchar"),
        syscr: keyed(text, "syscr"),
        syscw: keyed(text, "syscw"),
        read_bytes: keyed(text, "read_bytes"),
        write_bytes: keyed(text, "write_bytes"),
    }
}

/// Parse `/proc/<pid>/status`'s real uid and context switches.
#[must_use]
pub fn parse_status(text: &str) -> (Option<u32>, Option<u64>) {
    let uid = text.lines().find_map(|l| {
        l.strip_prefix("Uid:")
            .and_then(|r| crate::parse_uint(r.split_whitespace().next()?))
    });
    let csw = keyed(text, "voluntary_ctxt_switches")
        .zip(keyed(text, "nonvoluntary_ctxt_switches"))
        .map(|(a, b)| a.saturating_add(b));
    (uid, csw)
}

/// `/proc/<pid>/schedstat`: `(ns on a core, ns waiting for one)`.
#[must_use]
pub fn parse_schedstat(text: &str) -> Option<(u64, u64)> {
    let mut f = text.split_whitespace();
    Some((crate::parse_uint(f.next()?)?, crate::parse_uint(f.next()?)?))
}

/// The `read`, `write`, `readv` and `writev` call numbers.
#[cfg(target_arch = "x86_64")]
pub const READ_WRITE: [u64; 4] = [0, 1, 19, 20];
/// The `read`, `write`, `readv` and `writev` call numbers.
#[cfg(any(target_arch = "x86", target_arch = "arm"))]
pub const READ_WRITE: [u64; 4] = [3, 4, 145, 146];
/// The `read`, `write`, `readv` and `writev` call numbers (the generic table: aarch64, riscv64, loongarch64).
#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "arm")))]
pub const READ_WRITE: [u64; 4] = [63, 64, 65, 66];

/// `/proc/<pid>/syscall`: the descriptor a sleeping read or write names (its call number, then its first argument in hex); `None` for `running`, for `-1` (in no call), and for any other call.
#[must_use]
pub fn parse_syscall(text: &str) -> Option<i32> {
    let mut f = text.split_whitespace();
    let nr: u64 = crate::parse_uint(f.next()?)?;
    if !READ_WRITE.contains(&nr) {
        return None;
    }
    let arg = f.next()?;
    let hex = arg.strip_prefix("0x").unwrap_or(arg);
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    i32::try_from(u64::from_str_radix(hex, 16).ok()?).ok()
}

/// `/proc/<pid>/wchan`: the kernel function, `None` for `0` or empty (on a core, or not waiting).
/// A compiler's clone suffix goes (`poll_schedule_timeout.constprop.0` is `poll_schedule_timeout`).
#[must_use]
pub fn parse_wchan(text: &str) -> Option<String> {
    let w = text.trim();
    let w = w.split_once('.').map_or(w, |(f, _)| f);
    (!w.is_empty() && w != "0").then(|| w.chars().take(64).collect())
}

/// What a `fd/<n>` link names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FdLink {
    /// `pipe:[inode]`.
    Pipe(u64),
    /// `socket:[inode]`.
    Socket(u64),
    /// `anon_inode:[eventpoll]`.
    EventPoll,
    /// `anon_inode:inotify`.
    Inotify,
    /// A terminal ([`super::terminal_path`]).
    Tty,
    /// Any other device under `/dev`.
    Device,
    /// A file's path.
    Path(PathBuf),
    /// Anything else.
    Other,
}

/// Classify a `fd/<n>` link's target.
#[must_use]
pub fn parse_fd_link(target: &str) -> FdLink {
    let inode = |rest: &str| crate::parse_uint(rest.strip_prefix('[')?.strip_suffix(']')?);
    if let Some(n) = target.strip_prefix("pipe:").and_then(inode) {
        FdLink::Pipe(n)
    } else if let Some(n) = target.strip_prefix("socket:").and_then(inode) {
        FdLink::Socket(n)
    } else if target == "anon_inode:[eventpoll]" {
        FdLink::EventPoll
    } else if target == "anon_inode:inotify" {
        FdLink::Inotify
    } else if super::terminal_path(target.as_bytes()) {
        FdLink::Tty
    } else if target.starts_with("/dev/") {
        FdLink::Device
    } else if target.starts_with('/')
        && !target.starts_with("/proc/")
        && !target.starts_with("/sys/")
    {
        FdLink::Path(PathBuf::from(target))
    } else {
        FdLink::Other
    }
}

/// `/proc/<pid>/fdinfo/<n>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FdInfo {
    /// The descriptor's offset.
    pub pos: u64,
    /// Its open flags (octal in the file).
    pub flags: u32,
    /// Its inode, where the kernel prints one.
    pub ino: Option<u64>,
    /// An epoll set's interests: `(tfd, events)`.
    pub interests: Vec<(u64, u32)>,
}

impl FdInfo {
    /// Opened write-only (`O_WRONLY`).
    #[must_use]
    pub const fn write_only(&self) -> bool {
        self.flags & 0o3 == 0o1
    }
}

/// Parse `/proc/<pid>/fdinfo/<n>`.
#[must_use]
pub fn parse_fdinfo(text: &str) -> FdInfo {
    let mut info = FdInfo::default();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("pos:") {
            info.pos = crate::parse_uint(v.trim()).unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("flags:") {
            let v = v.trim();
            info.flags = v
                .bytes()
                .all(|b| matches!(b, b'0'..=b'7'))
                .then(|| u32::from_str_radix(v, 8).ok())
                .flatten()
                .unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("ino:") {
            info.ino = crate::parse_uint(v.trim());
        } else if let Some(v) = line.strip_prefix("tfd:") {
            let mut f = v.split_whitespace();
            let tfd = f.next().and_then(crate::parse_uint);
            let events = f
                .skip_while(|w| *w != "events:")
                .nth(1)
                .filter(|e| e.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|e| u32::from_str_radix(e, 16).ok());
            if let (Some(t), Some(e)) = (tfd, events)
                && info.interests.len() < MAX_KNOTES
            {
                info.interests.push((t, e));
            }
        }
    }
    info
}

/// One TCP socket, as an all-states dump names it ([`TcpDump::sockets`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpRow {
    /// The socket's inode.
    pub inode: u64,
    /// Its state.
    pub state: TcpState,
    /// Its local port.
    pub local_port: u16,
    /// Its peer (`None` for the wildcard on port 0).
    pub remote: Option<SocketAddr>,
    /// Bytes received, not read.
    pub rx_queue: u64,
}

/// `Max processes`' soft limit in a `/proc/<pid>/limits`; `None` for `unlimited` or where absent.
#[must_use]
pub fn max_processes(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("Max processes"))
        .and_then(|r| crate::parse_uint(r.split_whitespace().next()?))
}

/// Whether `wchan` is an event wait (`epoll_wait`, `poll`, `select`).
#[must_use]
pub fn event_wait(wchan: &str) -> bool {
    matches!(
        wchan,
        "ep_poll"
            | "do_epoll_wait"
            | "do_select"
            | "do_sys_poll"
            | "poll_schedule_timeout"
            | "core_sys_select"
            | "do_poll"
    ) || wchan.starts_with("ep_poll")
}

fn read_text(path: &Path) -> Option<String> {
    crate::read_bounded(path)
}

fn ticks_ns(ticks: u64, hz: u64) -> u64 {
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(hz.max(1))).unwrap_or(u64::MAX)
}

/// At most this many threads' waits are summed per process
/// ([`threads_wait_ns`]); past it the sum is the first ones', a lower bound.
pub const MAX_WAIT_THREADS: usize = 4096;

/// Every live thread's wait for a core, ns: the sum of each
/// `task/<tid>/schedstat`'s second field (`run_delay`). The process's own
/// `schedstat` is its group leader's alone: in the Linux lane's guest a
/// process of four spinning threads read 16.7 ms on a core there against
/// 3.0 s in its `stat`. A thread gone between the listing and its read is
/// skipped; `None` where no thread's is read (no `task` directory, or a
/// kernel without `CONFIG_SCHED_INFO`).
///
/// A process's runnable time is then its CPU time (`stat`, every thread's,
/// living or ended) plus this, so runnable less CPU is exactly the live
/// threads' wait; an ended thread's wait leaves the sum with it. [`read`]
/// sums it in its one pass over `task/`, beside the threads' readings.
#[must_use]
pub fn threads_wait_ns(dir: &Path) -> Option<u64> {
    wait_sum(&dir.join("task"), &task_ids(dir)?)
}

/// The thread ids in `dir`'s `task/`, ascending: the one listing [`read`]
/// takes of them. `None` without a `task` directory.
fn task_ids(dir: &Path) -> Option<Vec<u64>> {
    let mut tids: Vec<u64> = std::fs::read_dir(dir.join("task"))
        .ok()?
        .flatten()
        .filter_map(|e| crate::parse_uint(e.file_name().to_str()?))
        .collect();
    tids.sort_unstable();
    Some(tids)
}

/// The waits of the first [`MAX_WAIT_THREADS`] of `tids` under `task`,
/// summed ([`threads_wait_ns`]).
fn wait_sum(task: &Path, tids: &[u64]) -> Option<u64> {
    let mut sum = None::<u64>;
    for tid in tids.iter().take(MAX_WAIT_THREADS) {
        if let Some((_, wait)) = read_text(&task.join(tid.to_string()).join("schedstat"))
            .and_then(|t| parse_schedstat(&t))
        {
            sum = Some(sum.unwrap_or(0).saturating_add(wait));
        }
    }
    sum
}

/// The readings of the first [`MAX_THREADS`] of `tids` under `task`, and
/// whether there were more.
fn threads(task: &Path, tids: &[u64], hz: u64) -> (Vec<ThreadReading>, bool) {
    let out = tids
        .iter()
        .take(MAX_THREADS)
        .filter_map(|&tid| {
            let t = task.join(tid.to_string());
            let stat = read_text(&t.join("stat")).and_then(|s| parse_stat(&s))?;
            Some(ThreadReading {
                id: tid,
                state: RunState::from_linux(stat.state),
                cpu_ns: Some(ticks_ns(stat.utime.saturating_add(stat.stime), hz)),
                wchan: read_text(&t.join("wchan")).and_then(|w| parse_wchan(&w)),
            })
        })
        .collect();
    (out, tids.len() > MAX_THREADS)
}

/// What a descriptor's open file is, read through its `fd/<n>` link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenFile {
    /// A regular file: not a directory, a FIFO, a socket or a device.
    regular: bool,
    /// Its device, as `stat`'s `st_dev` spells it.
    dev: u64,
    /// Its inode.
    ino: u64,
}

/// The open file behind `link` (a `/proc/<pid>/fd/<n>`), or `None` where
/// it cannot be read (the process or descriptor is gone, or not ours); the
/// caller then counts the descriptor a regular file, its offset the only
/// growth there is to read.
///
/// A path link names directories and named FIFOs as well as files: a walker
/// holds its directory open, a daemon reads a control FIFO. Neither is an
/// output file, and a directory's offset on ext4 is a hash cookie that
/// moves on every `getdents64`, which read as an appending writer's growth.
///
/// Linux asks `statx(STATX_TYPE | STATX_INO, AT_STATX_DONT_SYNC)` of the
/// link itself, so the kernel follows it to the open file (no path is
/// resolved again, a deleted file included) and asks no file server to
/// revalidate: an NFS or FUSE file whose server hangs answers from the
/// inode the kernel holds, never waits. Elsewhere (a fixture tree in a test)
/// the link is followed by `std`.
#[cfg(target_os = "linux")]
fn open_file(link: &Path) -> Option<OpenFile> {
    use std::os::unix::ffi::OsStrExt as _;
    #[cfg(target_arch = "x86_64")]
    const SYS_STATX: libc::c_long = 332;
    #[cfg(target_arch = "aarch64")]
    const SYS_STATX: libc::c_long = 291;
    const AT_STATX_DONT_SYNC: libc::c_int = 0x4000;
    const STATX_TYPE: u32 = 0x1;
    const STATX_INO: u32 = 0x100;
    const S_IFMT: u16 = 0o170_000;
    const S_IFREG: u16 = 0o100_000;
    /// `struct statx` (`<linux/stat.h>`), 256 bytes; only the fields read
    /// are named.
    #[repr(C)]
    struct Statx {
        mask: u32,
        _blksize: u32,
        _attributes: u64,
        _nlink: u32,
        _uid: u32,
        _gid: u32,
        mode: u16,
        _spare0: u16,
        ino: u64,
        _rest: [u64; 11],
        _rdev: [u32; 2],
        dev_major: u32,
        dev_minor: u32,
        _spare2: [u64; 14],
    }
    const _: () = assert!(std::mem::size_of::<Statx>() == 256);
    let path = std::ffi::CString::new(link.as_os_str().as_bytes()).ok()?;
    let mut sx = std::mem::MaybeUninit::<Statx>::zeroed();
    // SAFETY: a NUL-terminated path and a writable 256-byte `struct statx`;
    // the syscall writes no more than its size.
    let rc = unsafe {
        libc::syscall(
            SYS_STATX,
            libc::AT_FDCWD,
            path.as_ptr(),
            AT_STATX_DONT_SYNC,
            STATX_TYPE | STATX_INO,
            sx.as_mut_ptr(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: zeroed, then filled by the successful call; every field is a
    // plain integer.
    let sx = unsafe { sx.assume_init() };
    (sx.mask & STATX_TYPE != 0).then(|| OpenFile {
        regular: sx.mode & S_IFMT == S_IFREG,
        dev: libc::makedev(sx.dev_major, sx.dev_minor),
        ino: sx.ino,
    })
}

/// [`open_file`] off Linux: a fixture tree's link, followed by `std`.
#[cfg(not(target_os = "linux"))]
fn open_file(link: &Path) -> Option<OpenFile> {
    let meta = std::fs::metadata(link).ok()?;
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt as _;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    Some(OpenFile {
        regular: meta.is_file(),
        dev,
        ino,
    })
}

/// `dir`'s descriptors. A socket is TCP when `tcp`'s dump names its inode: the newest dump, shared
/// by every TCP reader in the process ([`crate::linux::tcp::newest_within`]), never this
/// namespace's `net/tcp` table, which walks the whole established hash for each process that holds
/// a socket. A process in another network namespace, whose sockets that dump cannot name, has
/// them named from its namespace's own tables, while a dump is held
/// ([`crate::linux::tcp::namespace_sockets`]).
fn fds(
    dir: &Path,
    sleeping: Option<bool>,
    tcp: &dyn Fn() -> Option<Arc<TcpDump>>,
) -> Option<(Vec<FdReading>, bool)> {
    let mut nums: Vec<i32> = std::fs::read_dir(dir.join("fd"))
        .ok()?
        .flatten()
        .filter_map(|e| crate::parse_uint(e.file_name().to_str()?))
        .collect();
    nums.sort_unstable();
    let mut truncated = nums.len() > MAX_FDS;
    nums.truncate(MAX_FDS);
    let mut dump: Option<Option<Arc<TcpDump>>> = None;
    let mut theirs: Option<Option<crate::linux::TcpSockets>> = None;
    let mut out = Vec::with_capacity(nums.len());
    for (i, fd) in nums.into_iter().enumerate() {
        if i >= MAX_FD_DETAILS {
            truncated = true;
            out.push(FdReading {
                fd,
                kind: FdKind::Other,
            });
            continue;
        }
        let target = std::fs::read_link(dir.join("fd").join(fd.to_string()))
            .ok()
            .map(|t| t.to_string_lossy().into_owned());
        let link = target.as_deref().map_or(FdLink::Other, parse_fd_link);
        let info = || {
            read_text(&dir.join("fdinfo").join(fd.to_string()))
                .map(|t| parse_fdinfo(&t))
                .unwrap_or_default()
        };
        let kind = match link {
            FdLink::Pipe(ino) => FdKind::Pipe {
                handle: ino,
                peer: ino,
                status: None,
                queued: None,
                write_end: Some(info().write_only()),
            },
            FdLink::Socket(ino) => {
                let dump = dump.get_or_insert_with(tcp);
                let row = dump.as_ref().and_then(|d| {
                    d.sockets.get(&ino).or_else(|| {
                        theirs
                            .get_or_insert_with(|| crate::linux::tcp::namespace_sockets(dir))
                            .as_deref()
                            .and_then(|t| t.get(&ino))
                    })
                });
                match row {
                    Some(r) => FdKind::Tcp {
                        state: r.state,
                        remote: r.remote,
                        local_port: r.local_port,
                        queued_read: Some(r.rx_queue),
                    },
                    None => FdKind::Socket,
                }
            }
            FdLink::EventPoll => FdKind::EventQueue {
                sleeping,
                knotes: Some(
                    info()
                        .interests
                        .into_iter()
                        .map(|(tfd, events)| Knote {
                            ident: tfd,
                            filter: if events & 0x1 != 0 {
                                KnoteFilter::Read
                            } else if events & 0x4 != 0 {
                                KnoteFilter::Write
                            } else {
                                KnoteFilter::Other(0)
                            },
                        })
                        .collect(),
                ),
            },
            FdLink::Inotify => FdKind::Watch,
            FdLink::Tty => FdKind::Device {
                tty: true,
                read: !info().write_only(),
                ctty: target.as_deref() == Some("/dev/tty"),
            },
            FdLink::Device => FdKind::Device {
                tty: false,
                read: true,
                ctty: false,
            },
            FdLink::Path(_) => {
                let i = info();
                let open = open_file(&dir.join("fd").join(fd.to_string()));
                FdKind::File {
                    regular: open.is_none_or(|o| o.regular),
                    size: i.pos,
                    dev: open.map_or(0, |o| o.dev),
                    ino: i.ino.or(open.map(|o| o.ino)).unwrap_or(0),
                    lf_tail: None,
                    fresh_dylib: false,
                }
            }
            FdLink::Other => FdKind::Other,
        };
        out.push(FdReading { fd, kind });
    }
    Some((out, truncated))
}

/// Read one process under `root` (`/proc`, or a fixture tree), with `hz` clock ticks per second,
/// its TCP sockets named by `tcp`'s dump ([`fds`]).
#[must_use]
pub fn read(
    root: &Path,
    pid: u32,
    want_fds: bool,
    hz: u64,
    tcp: &dyn Fn() -> Option<Arc<TcpDump>>,
) -> Option<ProcFacts> {
    let dir = root.join(pid.to_string());
    let stat = parse_stat(&read_text(&dir.join("stat"))?)?;
    let state = RunState::from_linux(stat.state);
    let tty = (stat.tty_nr != 0).then_some(stat.tty_nr);
    let (uid, csw) = read_text(&dir.join("status"))
        .map(|t| parse_status(&t))
        .unwrap_or_default();
    let mut facts = ProcFacts {
        pid,
        ppid: stat.ppid,
        pgid: stat.pgrp,
        sid: Some(stat.session),
        start: Some(stat.start),
        uid,
        name: stat.name.clone(),
        state,
        traced: stat.state == b't',
        tty,
        tpgid: tty.and(Some(stat.tpgid).filter(|g| *g != 0)),
        ..ProcFacts::default()
    };
    if state == RunState::Zombie {
        return Some(facts);
    }
    let tids = task_ids(&dir);
    let task = dir.join("task");
    if let Some(io) = read_text(&dir.join("io")).map(|t| parse_io(&t)) {
        let cpu_ns = ticks_ns(stat.utime.saturating_add(stat.stime), hz);
        facts.counters = Some(Counters {
            cpu_ns,
            child_cpu_ns: Some(ticks_ns(stat.cutime.saturating_add(stat.cstime), hz)),
            syscalls: io.syscr.zip(io.syscw).map(|(r, w)| r.saturating_add(w)),
            wakeups: None,
            ctx_switches: csw,
            disk_read: io.read_bytes,
            disk_written: io.write_bytes,
            logical_writes: io.wchar,
            runnable_ns: tids
                .as_deref()
                .and_then(|tids| wait_sum(&task, tids))
                .map(|wait| cpu_ns.saturating_add(wait)),
            pageins: Some(stat.majflt),
            peak_footprint: None,
        });
    }
    facts.wchan = read_text(&dir.join("wchan")).and_then(|w| parse_wchan(&w));
    if let Some(tids) = tids.as_deref() {
        let (ts, cut) = threads(&task, tids, hz);
        facts.threads = Some(ts);
        facts.threads_truncated = cut;
    }
    if want_fds {
        let sleeping = facts.wchan.as_deref().map(event_wait);
        if let Some((list, cut)) = fds(&dir, sleeping, tcp) {
            facts.fds = Some(list);
            facts.fds_truncated = cut;
        }
        if state == RunState::Sleeping {
            facts.blocked_fd = read_text(&dir.join("syscall")).and_then(|t| parse_syscall(&t));
        }
    }
    Some(facts)
}
