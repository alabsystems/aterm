// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The job probe: what the stuck-process engine (`aterm_vitals::jobs`,
//! `docs/DESIGN-system-awareness-2026-10-16.md` §11) needs to know about each member of a job,
//! read only for the processes it names.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub mod darwin;
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// A process's `kinfo_proc`, which a zombie answers too (the sweep's fallback when the short BSD
/// info refuses).
#[cfg(target_os = "macos")]
pub(crate) fn macos_kinfo(pid: std::ffi::c_int) -> Option<darwin::Kinfo> {
    macos::kinfo(pid)
}

/// The device and inode of every vnode `pid` holds open, and whether the list was cut (the build-slot reader's holder proof, `crate::slots`).
#[cfg(target_os = "macos")]
pub(crate) fn macos_vnode_ids(pid: u32) -> Option<(Vec<(u32, u64)>, bool)> {
    macos::vnode_ids(pid)
}

/// Whether a device's path names a terminal, on either platform: a pty's terminal end
/// (`/dev/ttys003`, `/dev/pts/3`) or its master (`/dev/ptmx`, a BSD `/dev/ptyp0`), a serial line
/// (`/dev/tty.usbserial-10`, `/dev/cu.usbserial-10`, `/dev/ttyS0`), a virtual console, the
/// console, or a process's own terminal (`/dev/tty`).
#[must_use]
pub fn terminal_path(path: &[u8]) -> bool {
    [&b"/dev/tty"[..], b"/dev/pty", b"/dev/pts/", b"/dev/cu."]
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || path == b"/dev/ptmx"
        || path == b"/dev/console"
}

/// At most this many threads are read per process.
pub const MAX_THREADS: usize = 256;
/// At most this many descriptors are listed per process.
pub const MAX_FDS: usize = 1024;
/// At most this many descriptors are read in detail per process.
pub const MAX_FD_DETAILS: usize = 128;
/// At most this many knotes are read per event queue.
pub const MAX_KNOTES: usize = 64;
/// The most of an output file's appended bytes read for a line feed.
pub const TAIL_CAP: usize = 64 * 1024;

/// A process's or a thread's state as the kernel reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RunState {
    /// On a core or ready for one.
    Running,
    /// Waiting, interruptibly.
    Sleeping,
    /// Stopped by a signal (`SIGSTOP`, `SIGTSTP`, a tracer).
    Stopped,
    /// Exited, not yet reaped by its parent.
    Zombie,
    /// Waiting in the kernel, uninterruptibly (Linux `D`, Mach `TH_STATE_UNINTERRUPTIBLE`): a disk or a mount.
    Uninterruptible,
    /// Being created, or an idle kernel thread.
    Idle,
    /// Not read.
    #[default]
    Unknown,
}

impl RunState {
    /// The word the probe's readers print.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Sleeping => "sleeping",
            Self::Stopped => "stopped",
            Self::Zombie => "zombie",
            Self::Uninterruptible => "uninterruptible",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// A BSD `p_stat` (`SIDL` 1, `SRUN` 2, `SSLEEP` 3, `SSTOP` 4, `SZOMB` 5). macOS reports `SRUN` for a process blocked in `read(2)` too, so the macOS arm refines `Running` from the threads' states.
    #[must_use]
    pub const fn from_bsd(status: u32) -> Self {
        match status {
            1 => Self::Idle,
            2 => Self::Running,
            3 => Self::Sleeping,
            4 => Self::Stopped,
            5 => Self::Zombie,
            _ => Self::Unknown,
        }
    }

    /// A Mach thread's `pth_run_state` (`TH_STATE_RUNNING` 1, `_STOPPED` 2, `_WAITING` 3, `_UNINTERRUPTIBLE` 4, `_HALTED` 5).
    #[must_use]
    pub const fn from_mach_thread(state: i32) -> Self {
        match state {
            1 => Self::Running,
            2 | 5 => Self::Stopped,
            3 => Self::Sleeping,
            4 => Self::Uninterruptible,
            _ => Self::Unknown,
        }
    }

    /// A Linux `/proc/<pid>/stat` state letter.
    #[must_use]
    pub const fn from_linux(letter: u8) -> Self {
        match letter {
            b'R' => Self::Running,
            b'S' | b'P' | b'W' | b'K' => Self::Sleeping,
            b'D' => Self::Uninterruptible,
            b'T' | b't' => Self::Stopped,
            b'Z' | b'X' | b'x' => Self::Zombie,
            b'I' => Self::Idle,
            _ => Self::Unknown,
        }
    }
}

/// One thread of a process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadReading {
    /// The OS's id for it (a Mach thread handle, a Linux tid).
    pub id: u64,
    /// Its state.
    pub state: RunState,
    /// User plus system CPU time, ns; `None` where unread.
    pub cpu_ns: Option<u64>,
    /// The kernel function it waits in, where the OS names it publicly (Linux `task/<tid>/wchan`); `None` elsewhere.
    pub wchan: Option<String>,
}

/// A process's cumulative counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// User plus system CPU time, ns.
    pub cpu_ns: u64,
    /// The CPU time of its children that it reaped, ns (macOS `ri_child_{user,system}_time`, Linux `cutime + cstime`).
    pub child_cpu_ns: Option<u64>,
    /// BSD system calls made (macOS `pti_syscalls_unix`; Linux `syscr + syscw`, which counts only the read and write family).
    pub syscalls: Option<u64>,
    /// Interrupt wakeups (macOS `ri_interrupt_wkups`); `None` on Linux.
    pub wakeups: Option<u64>,
    /// Context switches (macOS `pti_csw`, Linux voluntary + involuntary).
    pub ctx_switches: Option<u64>,
    /// Bytes read from disk (`ri_diskio_bytesread`, Linux `read_bytes`).
    pub disk_read: Option<u64>,
    /// Bytes written to disk (`ri_diskio_byteswritten`, Linux `write_bytes`).
    pub disk_written: Option<u64>,
    /// Bytes written through any descriptor, before the page cache (`ri_logical_writes`, Linux `wchar`).
    pub logical_writes: Option<u64>,
    /// Time runnable, waiting for a core included, ns (`ri_runnable_time`, Linux `schedstat`'s run time plus its wait).
    pub runnable_ns: Option<u64>,
    /// Page-ins (`ri_pageins`, Linux major faults).
    pub pageins: Option<u64>,
}

/// A TCP connection's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TcpState {
    /// `CLOSED`.
    Closed,
    /// `LISTEN`.
    Listen,
    /// `SYN_SENT`.
    SynSent,
    /// `SYN_RECEIVED`.
    SynReceived,
    /// `ESTABLISHED`.
    Established,
    /// `CLOSE_WAIT`.
    CloseWait,
    /// `FIN_WAIT_1`.
    FinWait1,
    /// `CLOSING`.
    Closing,
    /// `LAST_ACK`.
    LastAck,
    /// `FIN_WAIT_2`.
    FinWait2,
    /// `TIME_WAIT`.
    TimeWait,
    /// A state this reader does not name.
    Other,
}

impl TcpState {
    /// The protocol's own spelling (`ESTABLISHED`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Closed => "CLOSED",
            Self::Listen => "LISTEN",
            Self::SynSent => "SYN_SENT",
            Self::SynReceived => "SYN_RECEIVED",
            Self::Established => "ESTABLISHED",
            Self::CloseWait => "CLOSE_WAIT",
            Self::FinWait1 => "FIN_WAIT_1",
            Self::Closing => "CLOSING",
            Self::LastAck => "LAST_ACK",
            Self::FinWait2 => "FIN_WAIT_2",
            Self::TimeWait => "TIME_WAIT",
            Self::Other => "OTHER",
        }
    }

    /// Darwin's `tcpsi_state` (`TSI_S_*`, `sys/proc_info.h`).
    #[must_use]
    pub const fn from_darwin(state: i32) -> Self {
        match state {
            0 => Self::Closed,
            1 => Self::Listen,
            2 => Self::SynSent,
            3 => Self::SynReceived,
            4 => Self::Established,
            5 => Self::CloseWait,
            6 => Self::FinWait1,
            7 => Self::Closing,
            8 => Self::LastAck,
            9 => Self::FinWait2,
            10 => Self::TimeWait,
            _ => Self::Other,
        }
    }

    /// Linux's `/proc/net/tcp` `st` column (`TCP_ESTABLISHED` 1 … `TCP_CLOSING` 11, `include/net/tcp_states.h`).
    #[must_use]
    pub const fn from_linux(state: u8) -> Self {
        match state {
            1 => Self::Established,
            2 => Self::SynSent,
            3 => Self::SynReceived,
            4 => Self::FinWait1,
            5 => Self::FinWait2,
            6 => Self::TimeWait,
            7 => Self::Closed,
            8 => Self::CloseWait,
            9 => Self::LastAck,
            10 => Self::Listen,
            11 => Self::Closing,
            _ => Self::Other,
        }
    }
}

/// What an event queue waits for (a kqueue knote's filter, an epoll interest's events).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnoteFilter {
    /// Readable (`EVFILT_READ`, `EPOLLIN`).
    Read,
    /// Writable (`EVFILT_WRITE`, `EPOLLOUT`).
    Write,
    /// A timer (`EVFILT_TIMER`).
    Timer,
    /// A file changed (`EVFILT_VNODE`).
    Vnode,
    /// A process event (`EVFILT_PROC`).
    Proc,
    /// A signal (`EVFILT_SIGNAL`).
    Signal,
    /// A user event (`EVFILT_USER`).
    User,
    /// Any other filter, by its number.
    Other(i16),
}

impl KnoteFilter {
    /// Darwin's filter number (`sys/event.h`).
    #[must_use]
    pub const fn from_darwin(filter: i16) -> Self {
        match filter {
            -1 => Self::Read,
            -2 => Self::Write,
            -4 => Self::Vnode,
            -5 => Self::Proc,
            -6 => Self::Signal,
            -7 => Self::Timer,
            -10 => Self::User,
            other => Self::Other(other),
        }
    }
}

/// One event an event queue waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Knote {
    /// What it is about: a descriptor for `Read`/`Write`/`Vnode`, a pid for `Proc`, a timer's id.
    pub ident: u64,
    /// What it waits for.
    pub filter: KnoteFilter,
}

/// What one descriptor is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FdKind {
    /// A file on a file system.
    File {
        /// A regular file (not a directory, a device or a FIFO).
        regular: bool,
        /// Its size in bytes (Linux: the descriptor's offset, which an appending writer advances, so no path is touched).
        size: u64,
        /// Its device.
        dev: u64,
        /// Its inode.
        ino: u64,
        /// Whether the bytes appended since this reader last looked hold a line feed: `None` when nothing was appended, when this is the first look, or when the file is not one the reader may open ([`tail_allowed`]); its growth then counts unverified.
        lf_tail: Option<bool>,
        /// A dylib a build just wrote ([`fresh_dylib`]): `false` where its change time is unread, and off macOS.
        fresh_dylib: bool,
    },
    /// A character device; `tty` for a terminal.
    Device {
        /// A terminal (a pty or a console).
        tty: bool,
    },
    /// One end of a pipe.
    Pipe {
        /// This end's kernel handle (Linux: the pipe's inode, shared by both ends).
        handle: u64,
        /// The other end's handle (Linux: the same inode).
        peer: u64,
        /// The kernel's pipe state word (Darwin `pipe_status`: `0x8` a reader waits, `0x10` a writer waits); `None` where the OS gives none.
        status: Option<u32>,
        /// Bytes queued in this end's buffer; `None` where unread.
        queued: Option<u64>,
        /// Whether this is the write end; `None` where unknown.
        write_end: Option<bool>,
    },
    /// A TCP socket.
    Tcp {
        /// Its state.
        state: TcpState,
        /// Its peer; `None` for a listener or where unread.
        remote: Option<SocketAddr>,
        /// Its local port.
        local_port: u16,
        /// Bytes received and not yet read; `None` where unread.
        queued_read: Option<u64>,
    },
    /// Any other socket.
    Socket,
    /// An event queue (a kqueue, an epoll instance).
    EventQueue {
        /// Whether a thread sleeps on it now; `None` where unread.
        sleeping: Option<bool>,
        /// What it waits for; `None` where the OS does not list it (macOS without lane L6).
        knotes: Option<Vec<Knote>>,
    },
    /// A file-change watch (Linux inotify).
    Watch,
    /// Anything else, or a socket left unread (macOS: its process was not settled, or was
    /// spawning).
    Other,
}

/// One open descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FdReading {
    /// Its number.
    pub fd: i32,
    /// What it is.
    pub kind: FdKind,
}

/// A terminal's line discipline: how its reader in front takes the keys typed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TermWord {
    /// `ICANON`: a line at a time.
    pub canonical: bool,
    /// `ECHO`.
    pub echo: bool,
    /// Bytes typed that its reader has not read (`FIONREAD`: a canonical terminal's whole lines).
    pub queued: u32,
}

/// The line discipline of the pseudo-terminal `dev` that `pid` holds (a controlling terminal's
/// device, as [`ProcFacts::tty`] holds it), and the keys queued in it, read through its device
/// node (Linux: through `pid`'s own descriptor of it, the node of whichever `devpts` holds it):
/// `None` when no node is `dev`'s or aterm's user may not open it (a terminal sudo gave root), and
/// when the queue cannot be counted: an unknown count is no word, never an empty queue.
#[cfg(unix)]
#[must_use]
pub fn term_word(pid: u32, dev: u32) -> Option<TermWord> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let path = term_node(pid, dev)?;
    // Opened for its attributes alone: never as a controlling terminal, never waiting.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .ok()?;
    word_of(f.as_raw_fd(), queued_on)
}

/// The word of the terminal open at `fd`, its queue counted by `queued`: `None` when either read
/// fails.
#[cfg(unix)]
fn word_of(fd: i32, queued: impl FnOnce(i32) -> Option<u32>) -> Option<TermWord> {
    // SAFETY: `termios` is plain integers and bytes; all-zero is valid.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `tcgetattr` fails cleanly on a bad or non-terminal `fd`, and `t` is a writable
    // `termios`.
    let rc = unsafe { libc::tcgetattr(fd, &raw mut t) };
    if rc != 0 {
        return None;
    }
    Some(TermWord {
        canonical: t.c_lflag & libc::ICANON != 0,
        echo: t.c_lflag & libc::ECHO != 0,
        queued: queued(fd)?,
    })
}

/// The bytes queued for the reader of the terminal open at `fd` (`FIONREAD`), `None` where the
/// call fails or the count is negative.
#[cfg(unix)]
fn queued_on(fd: i32) -> Option<u32> {
    let mut queued: libc::c_int = 0;
    // SAFETY: `ioctl` fails cleanly on a bad `fd`, and `queued` is a writable `int`.
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &raw mut queued) };
    if rc != 0 {
        return None;
    }
    u32::try_from(queued).ok()
}

/// The node of the pseudo-terminal `dev`: macOS's `/dev/ttysNNN` (major 16, the minor its
/// number).
#[cfg(target_os = "macos")]
fn term_node(_pid: u32, dev: u32) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let path = PathBuf::from(format!("/dev/ttys{:03}", dev & 0x00ff_ffff));
    (u32::try_from(std::fs::metadata(&path).ok()?.rdev()).ok() == Some(dev)).then_some(path)
}

/// `pid`'s descriptor of the terminal `dev`: `/dev/pts/N` names a terminal of aterm's own
/// `devpts`, and a sandbox below the tab (`systemd-nspawn`, `bwrap --dev`) numbers its own.
#[cfg(all(unix, not(target_os = "macos")))]
fn term_node(pid: u32, dev: u32) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .ok()?
        .take(256)
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            std::fs::metadata(p)
                .is_ok_and(|m| u32::try_from(m.rdev()).ok() == Some(dev) && m.rdev() != 0)
        })
}

/// Everything read about one process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcFacts {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Its process group.
    pub pgid: u32,
    /// Its session (the session leader's pid); `None` where unread.
    pub sid: Option<u32>,
    /// When it started, compared for equality only: µs since the epoch on macOS, clock ticks since boot on Linux; `None` where unread.
    pub start: Option<u64>,
    /// Its user; `None` where unread.
    pub uid: Option<u32>,
    /// Its short name (`p_comm`, `comm`).
    pub name: String,
    /// Its executable's file name (the basename of `proc_pidpath`, of the `/proc/<pid>/exe` link): the name a row prints, which `p_comm`'s 16 bytes cut and a renamed process changes; `None` where unread (a zombie, another user's process on Linux) or not UTF-8.
    pub exe: Option<String>,
    /// Its state (on macOS refined from its threads).
    pub state: RunState,
    /// Its controlling terminal's device; `None` when it has none.
    pub tty: Option<u32>,
    /// That terminal's foreground process group; `None` without a terminal.
    pub tpgid: Option<u32>,
    /// That terminal's line discipline, read for a session leader through the terminal's device
    /// node ([`term_word`]); `None` for any other process, or where aterm's user may not open it.
    pub term: Option<TermWord>,
    /// Its counters; `None` where the OS refused them.
    pub counters: Option<Counters>,
    /// Its threads; `None` where refused.
    pub threads: Option<Vec<ThreadReading>>,
    /// Whether it has more than [`MAX_THREADS`] threads.
    pub threads_truncated: bool,
    /// The kernel function it waits in (Linux `wchan`); `None` elsewhere or when it waits in none.
    pub wchan: Option<String>,
    /// Its descriptors, when asked for; `None` when not asked or refused.
    pub fds: Option<Vec<FdReading>>,
    /// Whether it holds more descriptors than were read.
    pub fds_truncated: bool,
    /// What its start reads, from the thread handles and the BSD record this reading took ([`StartFacts`], judged by [`Self::program_check`]); `None` where unread (a zombie, a refused list, the Linux arm, which has no program check).
    pub start_facts: Option<StartFacts>,
}

impl ProcFacts {
    /// Where it stands at its start: [`judge_start`] over [`Self::start_facts`].
    #[must_use]
    pub fn program_check(&self) -> Option<AtStart> {
        self.start_facts.as_ref().map(judge_start)
    }
}

/// How long before a process's start a dylib it holds open may have been written and still read as fresh ([`fresh_dylib`]): a build writes a proc macro, then starts the compiler that loads it.
pub const FRESH_DYLIB_US: u64 = 3_600_000_000;

/// Whether a descriptor's file at `path`, its data last changed at `mtime_s` (seconds since the
/// epoch), is a dylib a build just wrote for the process that started at `start_us` (µs since the
/// epoch): its name ends `.dylib`, and it changed at most [`FRESH_DYLIB_US`] before that start, or
/// since; `false` with no start read or a change time before the epoch. macOS checks a program's
/// new code as it loads it, so a started program that holds one open while its CPU stays flat may
/// wait for that check (the leftover-processes design's §5.1): a heuristic, never a verdict.
#[must_use]
pub fn fresh_dylib(path: &[u8], mtime_s: i64, start_us: Option<u64>) -> bool {
    let (Some(start_us), Ok(mtime_s)) = (start_us, u64::try_from(mtime_s)) else {
        return false;
    };
    path.ends_with(b".dylib")
        && mtime_s
            .saturating_mul(1_000_000)
            .saturating_add(FRESH_DYLIB_US)
            >= start_us
}

/// How long since its fork a process whose one thread never ran must have stood before it reads as held at macOS's program check: a normal exec reads its first thread's handle as `0` for under 0.3 ms (M2).
pub const PROGRAM_CHECK_AGE_MS: u64 = 20_000;

/// Where a process stands at its start (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.2), judged by [`judge_start`] from its [`StartFacts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtStart {
    /// It runs: its thread set its handle, or it is younger than [`PROGRAM_CHECK_AGE_MS`], stopped or traced.
    Started,
    /// Held at macOS's program check: one thread that never ran, past [`PROGRAM_CHECK_AGE_MS`], neither stopped nor traced, native, its executable on a local volume.
    Held,
    /// The same signature in an x86_64 process on an arm64 Mac: Rosetta translates it, and the Developer Tools grant does not reach that.
    Translating,
    /// The same signature on an executable whose volume is not proven local: it has not started, and nothing read says why.
    Unstarted,
}

/// What [`judge_start`] judges a process's start by, as [`read_start`] reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartFacts {
    /// Its thread list is one handle, `0`: its one thread never ran far enough to set it (M1).
    pub lone_zero: bool,
    /// Its BSD status is `SSTOP`: a start-suspended spawn, a `SIGSTOP`, a tracer's stop.
    pub stopped: bool,
    /// A debugger traces it (`PROC_FLAG_TRACED`).
    pub traced: bool,
    /// An x86_64 process on an arm64 Mac.
    pub translated: bool,
    /// Whether its executable's volume is local and not automounted; `None` when unread (no path, a deleted file, no mount holds it).
    pub local: Option<bool>,
    /// Milliseconds since its fork; `None` when unread.
    pub age_ms: Option<u64>,
}

/// Where a process with `f` stands at its start: [`AtStart::Started`] unless it shows the held
/// signature (its one thread never ran, past [`PROGRAM_CHECK_AGE_MS`], neither stopped nor
/// traced); then [`AtStart::Translating`] for an x86_64 process on an arm64 Mac, else
/// [`AtStart::Unstarted`] unless its executable's volume reads local, else [`AtStart::Held`].
#[must_use]
pub fn judge_start(f: &StartFacts) -> AtStart {
    if !(f.lone_zero && !f.stopped && !f.traced && f.age_ms >= Some(PROGRAM_CHECK_AGE_MS)) {
        return AtStart::Started;
    }
    if f.translated {
        AtStart::Translating
    } else if f.local != Some(true) {
        AtStart::Unstarted
    } else {
        AtStart::Held
    }
}

/// The [`StartFacts`] of `pid` at `now_us` (µs since the epoch, [`now_us`]): one two-slot thread list, and for a lone `0` only its BSD record, its arch and its executable's mount. `None` when the list or a record the judgment needs cannot be read (gone, another user's), and off macOS. Nothing of its arguments or environment is read: such a read of a program held mid-start can hang (M29).
#[must_use]
pub fn read_start(pid: u32, now_us: u64) -> Option<StartFacts> {
    read_start_in(pid, now_us, &std::cell::OnceCell::new())
}

/// [`read_start`], the mount table taken from `mounts` and read into it the first time a lone `0` needs it: a reader of many pids hands every read one cell, as [`JobsReader::read_all`] does.
#[must_use]
pub fn read_start_in(
    pid: u32,
    now_us: u64,
    mounts: &std::cell::OnceCell<crate::volumes::Table>,
) -> Option<StartFacts> {
    #[cfg(target_os = "macos")]
    {
        macos::read_start(pid, now_us, mounts)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, now_us, mounts);
        None
    }
}

/// Where `pid` stands at its start now ([`read_start`], [`judge_start`]); `None` when unread.
#[must_use]
pub fn at_start(pid: u32) -> Option<AtStart> {
    read_start(pid, now_us()).map(|f| judge_start(&f))
}

/// The wall clock in µs since the epoch, as the kernel counts a process's start; 0 before the epoch.
#[must_use]
pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

/// Whether the volume `table` places `path` on is local and not automounted ([`local_mount`]); `None` when no mount of `table` holds it, or when it calls it local but was cut ([`crate::volumes::Table::complete`]): a mount it left out may be the one that holds `path`.
fn local_at(path: &Path, table: &crate::volumes::Table) -> Option<bool> {
    match crate::volumes::mount_of(path, table).map(local_mount)? {
        true if !table.complete => None,
        local => Some(local),
    }
}

/// A mount whose files are on this Mac: local, and neither automounted nor autofs.
fn local_mount(m: &crate::volumes::Mount) -> bool {
    m.local && !m.automounted && m.fstype != "autofs"
}

/// One process the engine wants read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadReq {
    /// The pid.
    pub pid: u32,
    /// Whether to read its descriptors too.
    pub fds: bool,
}

/// An optional lane's state after its self-check (the system-awareness design's §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// The self-check passed.
    On,
    /// The self-check failed, or the platform has no such lane; the word says why.
    Off(&'static str),
}

impl Lane {
    /// Whether the lane is on.
    #[must_use]
    pub const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }

    /// `on`, or `off:<why>`.
    #[must_use]
    pub fn word(self) -> String {
        match self {
            Self::On => "on".to_owned(),
            Self::Off(why) => format!("off:{why}"),
        }
    }
}

/// The job probe's two optional lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lanes {
    /// L6: a kqueue's knotes are listed (the `follow`, `listen` and `timer` wait names).
    pub kqueue_detail: Lane,
    /// L7: a pipe end shows whether a reader or a writer waits on it (the `pipe` and `pipe-full` wait names).
    pub pipe_flags: Lane,
}

/// Whether another process's sockets may be read in detail now by a reader on a timer of its own
/// (one that runs `lsof -i` on it each second): on macOS, it is not exiting and not a fork under
/// 2 s old that has not exec'd, which may be mid-spawn (`macos::sockets_readable`); `true`
/// elsewhere. A close that starts after the call, a spawn's or a fresh program's, such a reader
/// meets by chance alone.
#[must_use]
pub fn sockets_readable(pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::sockets_readable(pid, now_us())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        true
    }
}

/// The job probe.
#[derive(Debug)]
pub struct JobsReader {
    /// Where the Linux arm reads `/proc` (the real one unless a test names a
    /// fixture tree).
    root: PathBuf,
    /// Clock ticks per second for the Linux arm's `stat` times.
    hz: u64,
    lanes: Option<Lanes>,
    /// The last size seen per `(pid, start, fd, ino)`.
    tails: HashMap<TailKey, u64>,
    /// The processes the last pass found running a program they exec'd, by `(pid, start)`, each
    /// with its short name then: the exec'd ones whose sockets this pass may read
    /// (`macos::settled`). Each pass replaces it, so a process a pass leaves out is new to the next.
    execd: HashMap<(u32, u64), String>,
}

/// An output file's identity for the tail check.
type TailKey = (u32, u64, i32, u64);

impl Default for JobsReader {
    fn default() -> Self {
        Self::new()
    }
}

impl JobsReader {
    /// A reader of this machine.
    #[must_use]
    pub fn new() -> Self {
        Self::at_root(Path::new("/proc"), linux::clock_ticks())
    }

    /// A reader whose Linux arm reads `root` as `/proc`, with `hz` clock ticks per second: a fixture tree in tests (the macOS arm ignores it).
    #[must_use]
    pub fn at_root(root: &Path, hz: u64) -> Self {
        Self {
            root: root.to_path_buf(),
            hz: hz.max(1),
            lanes: None,
            tails: HashMap::new(),
            execd: HashMap::new(),
        }
    }

    /// The optional lanes, self-checked on the first call and remembered (a failed lane is not retried in this process).
    pub fn lanes(&mut self) -> Lanes {
        *self.lanes.get_or_insert_with(check_lanes)
    }

    /// Read every process `reqs` names, in order; a process gone or refused meanwhile is left out.
    /// The call is a pass: a process that ran a program it exec'd has its sockets read only when
    /// the last pass read it running that program too, so one this call leaves out reads its
    /// sockets `Other` at the next.
    pub fn read_all(&mut self, reqs: &[ReadReq]) -> Vec<ProcFacts> {
        self.read_all_at(reqs, now_us())
    }

    /// [`Self::read_all`], each start's age taken at `now` (µs since the epoch, [`now_us`]).
    fn read_all_at(&mut self, reqs: &[ReadReq], now: u64) -> Vec<ProcFacts> {
        let lanes = if reqs.iter().any(|r| r.fds) {
            Some(self.lanes())
        } else {
            None
        };
        let mounts = std::cell::OnceCell::new();
        let mut out = Vec::with_capacity(reqs.len());
        let mut execd = HashMap::new();
        for r in reqs {
            let Some((mut facts, start, runs_its_exec)) = self.read_raw(r.pid, r.fds, lanes, now)
            else {
                continue;
            };
            if runs_its_exec {
                execd.insert(facts_key(r.pid, facts.start), facts.name.clone());
            }
            let exe = (facts.state != RunState::Zombie)
                .then(|| self.exe_path(r.pid))
                .flatten();
            facts.exe = exe
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                .map(str::to_owned);
            // A session leader's terminal is the session's (`script`'s, an agent's pty call's):
            // how its reader in front takes keys. A fixture tree names no device of this machine.
            #[cfg(unix)]
            if facts.sid == Some(r.pid)
                && (cfg!(target_os = "macos") || self.root == Path::new("/proc"))
            {
                facts.term = facts.tty.and_then(|dev| term_word(r.pid, dev));
            }
            facts.start_facts = start.map(|mut f| {
                if f.lone_zero {
                    f.local = exe
                        .as_deref()
                        .and_then(|p| local_at(p, mounts.get_or_init(crate::volumes::table)));
                }
                f
            });
            if let Some(fds) = facts.fds.as_mut() {
                self.check_tails(&facts_key(r.pid, facts.start), fds, &mounts);
            }
            out.push(facts);
        }
        let live: std::collections::HashSet<(u32, u64)> =
            out.iter().map(|f| facts_key(f.pid, f.start)).collect();
        self.tails.retain(|k, _| live.contains(&(k.0, k.1)));
        self.execd = execd;
        out
    }

    /// Read one process; `None` when it is gone or unreadable. A pass of its own
    /// ([`Self::read_all`]): every other process is new to the next.
    pub fn read(&mut self, req: ReadReq) -> Option<ProcFacts> {
        self.read_all(&[req]).pop()
    }

    /// One process's facts, the [`StartFacts`] its thread list gave (the macOS arm; `local` left for [`Self::read_all`], which holds the executable's path), and whether it runs a program it exec'd (macOS; `false` elsewhere).
    fn read_raw(
        &self,
        pid: u32,
        fds: bool,
        lanes: Option<Lanes>,
        now_us: u64,
    ) -> Option<(ProcFacts, Option<StartFacts>, bool)> {
        #[cfg(target_os = "macos")]
        {
            let _ = (&self.root, self.hz);
            let execd_then = |start, name: &str| {
                self.execd
                    .get(&facts_key(pid, start))
                    .is_some_and(|then| then == name)
            };
            macos::read(pid, fds, lanes.unwrap_or(OFF_LANES), now_us, execd_then)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (lanes, now_us, &self.execd);
            if cfg!(target_os = "linux") || self.root != Path::new("/proc") {
                linux::read(&self.root, pid, fds, self.hz).map(|f| (f, None, false))
            } else {
                None
            }
        }
    }

    /// `pid`'s executable, as the kernel holds its path (`proc_pidpath`;
    /// the `exe` link under the root on Linux).
    fn exe_path(&self, pid: u32) -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let _ = &self.root;
            crate::process_path(pid)
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::fs::read_link(self.root.join(pid.to_string()).join("exe"))
                .ok()
                .filter(|p| p.is_absolute())
        }
    }

    /// Fill each output file's `lf_tail` from the bytes appended since the
    /// last look, where [`tail_allowed`] lets the file be opened.
    fn check_tails(
        &mut self,
        key: &(u32, u64),
        fds: &mut [FdReading],
        mounts: &std::cell::OnceCell<crate::volumes::Table>,
    ) {
        for f in fds.iter_mut() {
            let FdKind::File {
                regular: true,
                size,
                ino,
                lf_tail,
                ..
            } = &mut f.kind
            else {
                continue;
            };
            if !(1..=2).contains(&f.fd) {
                continue;
            }
            let tk = (key.0, key.1, f.fd, *ino);
            let prev = self.tails.insert(tk, *size);
            let Some(prev) = prev.filter(|p| *size > *p) else {
                continue;
            };
            let Some(path) = self.output_path(key.0, f.fd) else {
                continue;
            };
            let table = mounts.get_or_init(crate::volumes::table);
            if !tail_allowed(&path, table) {
                continue;
            }
            *lf_tail = tail_has_lf(&path, prev, *size);
        }
    }

    /// The path the kernel holds for `pid`'s descriptor `fd` (its name
    /// cache, macOS; the `fd/` link, Linux): no file system is asked.
    fn output_path(&self, pid: u32, fd: i32) -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            let _ = &self.root;
            macos::fd_path(pid, fd)
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::fs::read_link(
                self.root
                    .join(pid.to_string())
                    .join("fd")
                    .join(fd.to_string()),
            )
            .ok()
            .filter(|p| p.is_absolute())
        }
    }
}

/// Lanes for a platform with neither.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const OFF_LANES: Lanes = Lanes {
    kqueue_detail: Lane::Off("unsupported"),
    pipe_flags: Lane::Off("unsupported"),
};

fn facts_key(pid: u32, start: Option<u64>) -> (u32, u64) {
    (pid, start.unwrap_or(0))
}

fn check_lanes() -> Lanes {
    #[cfg(target_os = "macos")]
    {
        macos::check_lanes()
    }
    #[cfg(target_os = "linux")]
    {
        Lanes {
            kqueue_detail: Lane::On,
            pipe_flags: Lane::On,
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        OFF_LANES
    }
}

/// The folders whose files a job's output may be opened in: the system's temporary-files roots, where no privacy prompt applies.
pub const TAIL_ROOTS: [&str; 7] = [
    "/private/tmp/",
    "/private/var/folders/",
    "/tmp/",
    "/var/folders/",
    "/var/tmp/",
    "/dev/shm/",
    "/run/user/",
];

/// Whether an output file at `path` may be opened to read its new bytes: an absolute path with no `.` or `..`, under a [`TAIL_ROOTS`] folder, on a mount `table` proves local and not automounted ([`local_at`]).
#[must_use]
pub fn tail_allowed(path: &Path, table: &crate::volumes::Table) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if !TAIL_ROOTS.iter().any(|r| text.starts_with(r)) {
        return false;
    }
    local_at(path, table) == Some(true)
}

/// Whether the bytes of `path` between `from` and `to` hold a line feed: at most the last [`TAIL_CAP`] of them, read with one `pread` on a descriptor opened read-only, non-blocking and without following a final symlink.
#[must_use]
pub fn tail_has_lf(path: &Path, from: u64, to: u64) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
        if to <= from {
            return None;
        }
        let len = usize::try_from(to - from)
            .unwrap_or(usize::MAX)
            .min(TAIL_CAP);
        let at = to - len as u64;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        let mut buf = vec![0u8; len];
        let n = file.read_at(&mut buf, at).ok()?;
        Some(buf[..n].contains(&b'\n'))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, from, to);
        None
    }
}

/// The most processes one user may run (`kern.maxprocperuid` on macOS, `Max processes` in `/proc/self/limits` on Linux); `None` where unknown or unlimited.
#[must_use]
pub fn process_limit() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::sysctl_int(c"kern.maxprocperuid").and_then(|v| u64::try_from(v).ok())
    }
    #[cfg(target_os = "linux")]
    {
        crate::read_bounded(Path::new("/proc/self/limits")).and_then(|t| linux::max_processes(&t))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(test)]
mod tests;
