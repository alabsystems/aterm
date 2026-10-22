// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The job probe: what the stuck-process engine (`aterm_vitals::jobs`,
//! `docs/DESIGN-system-awareness-2026-10-16.md` §11) needs to know about each
//! member of a job, read only for the processes it names.
//!
//! The engine decides which processes are job members from the process sweep
//! ([`crate::processes`]) and hands this reader a list ([`ReadReq`]); the
//! reader answers one [`ProcFacts`] per process it could read:
//!
//! * **who** — parent, group, session, start time (with the pid, the
//!   process's identity across sweeps), uid, short name;
//! * **its state** — the kernel's run state, and every thread's
//!   ([`RunState`]): running, sleeping, stopped, zombie, uninterruptible;
//! * **its counters** ([`Counters`]) — CPU time (its own and its reaped
//!   children's), system calls, wakeups, context switches, disk bytes read and
//!   written, logical writes, time spent waiting for a core, page-ins;
//! * **its terminal** — the controlling tty and that tty's foreground group;
//! * **its descriptors** ([`FdReading`], on request) — a regular file's size
//!   (an output file grows), a pipe's two handles and wait flags, a TCP
//!   socket's state, peer and queued bytes, an event queue's sleep and the
//!   events it waits for;
//! * **its wait channel** where the OS names one publicly (Linux `wchan`).
//!
//! Nothing here decides anything: a reading the OS refuses is `None`
//! (unknown), never zero, and the engine treats it so.
//!
//! # The arms
//!
//! * **macOS** (the private `macos` module): `proc_pidinfo`
//!   (`PROC_PIDTASKALLINFO`, falling back to `PROC_PIDTBSDINFO` and then the
//!   short BSD info for a zombie), `getsid`, `proc_pid_rusage(RUSAGE_INFO_V4)`,
//!   `PROC_PIDLISTTHREADS` with `PROC_PIDTHREADINFO` per thread,
//!   `PROC_PIDLISTFDS` with `proc_pidfdinfo` per descriptor. The fd-info
//!   layouts the reference `libc` does not declare are decoded from bytes by
//!   [`darwin`] (pure, every host), their offsets held to the SDK by
//!   `tests/proc_info_sdk.rs`. Two optional lanes (design §5.3) self-check
//!   once per process ([`JobsReader::lanes`]): L6 `kqueue_detail` (the
//!   private `PROC_PIDFDKQUEUE_EXTINFO` flavor lists a kqueue's knotes) and L7
//!   `pipe_flags` (a reader blocked on a pipe shows `PIPE_WANTR`).
//! * **Linux** ([`linux`], compiled and tested on every host): `/proc/<pid>/`
//!   `stat`, `status`, `io`, `schedstat`, `wchan`, `task/*/stat`, `fd/*`,
//!   `fdinfo/*` and `net/tcp{,6}`, read under a root ([`JobsReader::at_root`])
//!   so tests run against fixture trees.
//! * **Elsewhere** — every read answers `None`.
//!
//! # An output file's new bytes
//!
//! A detached job's output is a regular file; its growth is progress only if
//! the new bytes hold a line feed (a carriage-return spinner grows its file
//! too). The reader keeps the last size it saw per descriptor and reads the
//! appended tail (at most [`TAIL_CAP`] bytes) only for a file under a
//! temporary-files root on a local, non-automounted mount judged from the
//! kernel's mount table without touching the path ([`tail_allowed`]): any
//! program can point a child's output into a protected folder or onto a hung
//! mount, and opening it there would raise a privacy prompt or hang the probe
//! thread. Every other file's growth is reported with its tail unverified.
//!
//! Bounded: at most [`MAX_THREADS`] threads, [`MAX_FDS`] descriptors listed
//! and [`MAX_FD_DETAILS`] of them read in detail per process.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub mod darwin;
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// A zombie's `kinfo_proc` (the sweep's fallback when the short BSD info
/// refuses).
#[cfg(target_os = "macos")]
pub(crate) fn macos_kinfo(pid: std::ffi::c_int) -> Option<darwin::Kinfo> {
    macos::kinfo(pid)
}

/// The device and inode of every vnode `pid` holds open, and whether the list
/// was cut (the build-slot reader's holder proof, `crate::slots`).
#[cfg(target_os = "macos")]
pub(crate) fn macos_vnode_ids(pid: u32) -> Option<(Vec<(u32, u64)>, bool)> {
    macos::vnode_ids(pid)
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
    /// Waiting in the kernel, uninterruptibly (Linux `D`, Mach
    /// `TH_STATE_UNINTERRUPTIBLE`): a disk or a mount.
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

    /// A BSD `p_stat` (`SIDL` 1, `SRUN` 2, `SSLEEP` 3, `SSTOP` 4, `SZOMB` 5).
    /// macOS reports `SRUN` for a process blocked in `read(2)` too, so the
    /// macOS arm refines `Running` from the threads' states.
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

    /// A Mach thread's `pth_run_state` (`TH_STATE_RUNNING` 1, `_STOPPED` 2,
    /// `_WAITING` 3, `_UNINTERRUPTIBLE` 4, `_HALTED` 5).
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
    /// The kernel function it waits in, where the OS names it publicly
    /// (Linux `task/<tid>/wchan`); `None` elsewhere.
    pub wchan: Option<String>,
}

/// A process's cumulative counters. Each is `None` where the OS does not
/// give it; a refused reading is the whole [`ProcFacts::counters`] at `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// User plus system CPU time, ns.
    pub cpu_ns: u64,
    /// The CPU time of its children that it reaped, ns (macOS
    /// `ri_child_{user,system}_time`, Linux `cutime + cstime`).
    pub child_cpu_ns: Option<u64>,
    /// BSD system calls made (macOS `pti_syscalls_unix`; Linux `syscr +
    /// syscw`, which counts only the read and write family).
    pub syscalls: Option<u64>,
    /// Interrupt wakeups (macOS `ri_interrupt_wkups`); `None` on Linux.
    pub wakeups: Option<u64>,
    /// Context switches (macOS `pti_csw`, Linux voluntary + involuntary).
    pub ctx_switches: Option<u64>,
    /// Bytes read from disk (`ri_diskio_bytesread`, Linux `read_bytes`).
    pub disk_read: Option<u64>,
    /// Bytes written to disk (`ri_diskio_byteswritten`, Linux
    /// `write_bytes`).
    pub disk_written: Option<u64>,
    /// Bytes written through any descriptor, before the page cache
    /// (`ri_logical_writes`, Linux `wchar`).
    pub logical_writes: Option<u64>,
    /// Time runnable, waiting for a core included, ns (`ri_runnable_time`,
    /// Linux `schedstat`'s run time plus its wait).
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

    /// Linux's `/proc/net/tcp` `st` column (`TCP_ESTABLISHED` 1 …
    /// `TCP_CLOSING` 11, `include/net/tcp_states.h`).
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

/// What an event queue waits for (a kqueue knote's filter, an epoll
/// interest's events).
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
    /// What it is about: a descriptor for `Read`/`Write`/`Vnode`, a pid for
    /// `Proc`, a timer's id.
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
        /// Its size in bytes (Linux: the descriptor's offset, which an
        /// appending writer advances, so no path is touched).
        size: u64,
        /// Its device.
        dev: u64,
        /// Its inode.
        ino: u64,
        /// Whether the bytes appended since this reader last looked hold a
        /// line feed: `None` when nothing was appended, when this is the
        /// first look, or when the file is not one the reader may open
        /// ([`tail_allowed`]); its growth then counts unverified.
        lf_tail: Option<bool>,
    },
    /// A character device; `tty` for a terminal.
    Device {
        /// A terminal (a pty or a console).
        tty: bool,
    },
    /// One end of a pipe.
    Pipe {
        /// This end's kernel handle (Linux: the pipe's inode, shared by both
        /// ends).
        handle: u64,
        /// The other end's handle (Linux: the same inode).
        peer: u64,
        /// The kernel's pipe state word (Darwin `pipe_status`: `0x8` a reader
        /// waits, `0x10` a writer waits); `None` where the OS gives none.
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
        /// What it waits for; `None` where the OS does not list it (macOS
        /// without lane L6).
        knotes: Option<Vec<Knote>>,
    },
    /// A file-change watch (Linux inotify).
    Watch,
    /// Anything else.
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
    /// When it started, compared for equality only: µs since the epoch on
    /// macOS, clock ticks since boot on Linux; `None` where unread.
    pub start: Option<u64>,
    /// Its user; `None` where unread.
    pub uid: Option<u32>,
    /// Its short name (`p_comm`, `comm`).
    pub name: String,
    /// Its executable's file name (the basename of `proc_pidpath`, of the
    /// `/proc/<pid>/exe` link): the name a row prints, which `p_comm`'s 16
    /// bytes cut and a renamed process changes; `None` where unread (a
    /// zombie, another user's process on Linux) or not UTF-8.
    pub exe: Option<String>,
    /// Its state (on macOS refined from its threads).
    pub state: RunState,
    /// Its controlling terminal's device; `None` when it has none.
    pub tty: Option<u32>,
    /// That terminal's foreground process group; `None` without a terminal.
    pub tpgid: Option<u32>,
    /// Its counters; `None` where the OS refused them.
    pub counters: Option<Counters>,
    /// Its threads; `None` where refused.
    pub threads: Option<Vec<ThreadReading>>,
    /// Whether it has more than [`MAX_THREADS`] threads.
    pub threads_truncated: bool,
    /// The kernel function it waits in (Linux `wchan`); `None` elsewhere or
    /// when it waits in none.
    pub wchan: Option<String>,
    /// Its descriptors, when asked for; `None` when not asked or refused.
    pub fds: Option<Vec<FdReading>>,
    /// Whether it holds more descriptors than were read.
    pub fds_truncated: bool,
}

/// One process the engine wants read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadReq {
    /// The pid.
    pub pid: u32,
    /// Whether to read its descriptors too.
    pub fds: bool,
}

/// An optional lane's state after its self-check (design §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// The self-check passed.
    On,
    /// The self-check failed, or the platform has no such lane; the word
    /// says why.
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
    /// L6: a kqueue's knotes are listed (the `follow`, `listen` and `timer`
    /// wait names).
    pub kqueue_detail: Lane,
    /// L7: a pipe end shows whether a reader or a writer waits on it (the
    /// `pipe` and `pipe-full` wait names).
    pub pipe_flags: Lane,
}

/// The job probe. It keeps the lanes' verdicts (checked once) and the last
/// size seen of each output file, so it is one per probe thread.
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

    /// A reader whose Linux arm reads `root` as `/proc`, with `hz` clock
    /// ticks per second: a fixture tree in tests (the macOS arm ignores it).
    #[must_use]
    pub fn at_root(root: &Path, hz: u64) -> Self {
        Self {
            root: root.to_path_buf(),
            hz: hz.max(1),
            lanes: None,
            tails: HashMap::new(),
        }
    }

    /// The optional lanes, self-checked on the first call and remembered
    /// (a failed lane is not retried in this process).
    pub fn lanes(&mut self) -> Lanes {
        *self.lanes.get_or_insert_with(check_lanes)
    }

    /// Read every process `reqs` names, in order; a process gone or refused
    /// meanwhile is left out. The tail sizes of processes not named are
    /// forgotten.
    pub fn read_all(&mut self, reqs: &[ReadReq]) -> Vec<ProcFacts> {
        let lanes = if reqs.iter().any(|r| r.fds) {
            Some(self.lanes())
        } else {
            None
        };
        let mounts = std::cell::OnceCell::new();
        let mut out = Vec::with_capacity(reqs.len());
        for r in reqs {
            let Some(mut facts) = self.read_raw(r.pid, r.fds, lanes) else {
                continue;
            };
            if facts.state != RunState::Zombie {
                facts.exe = self.exe_name(r.pid);
            }
            if let Some(fds) = facts.fds.as_mut() {
                self.check_tails(&facts_key(r.pid, facts.start), fds, &mounts);
            }
            out.push(facts);
        }
        let live: std::collections::HashSet<(u32, u64)> =
            out.iter().map(|f| facts_key(f.pid, f.start)).collect();
        self.tails.retain(|k, _| live.contains(&(k.0, k.1)));
        out
    }

    /// Read one process; `None` when it is gone or unreadable.
    pub fn read(&mut self, req: ReadReq) -> Option<ProcFacts> {
        self.read_all(&[req]).pop()
    }

    fn read_raw(&self, pid: u32, fds: bool, lanes: Option<Lanes>) -> Option<ProcFacts> {
        #[cfg(target_os = "macos")]
        {
            let _ = (&self.root, self.hz);
            macos::read(pid, fds, lanes.unwrap_or(OFF_LANES))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = lanes;
            if cfg!(target_os = "linux") || self.root != Path::new("/proc") {
                linux::read(&self.root, pid, fds, self.hz)
            } else {
                None
            }
        }
    }

    /// The file name of `pid`'s executable, as the kernel holds its path
    /// (`proc_pidpath`; the `exe` link under the root on Linux).
    fn exe_name(&self, pid: u32) -> Option<String> {
        #[cfg(target_os = "macos")]
        let path = {
            let _ = &self.root;
            crate::process_path(pid)
        };
        #[cfg(not(target_os = "macos"))]
        let path = std::fs::read_link(self.root.join(pid.to_string()).join("exe"))
            .ok()
            .filter(|p| p.is_absolute());
        path?.file_name()?.to_str().map(str::to_owned)
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
            // Only stdout and stderr are an output channel.
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
        // Linux names a pipe wait by `wchan` and lists an epoll set's
        // interests in `fdinfo`: both lanes are the kernel's own, always on.
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

/// The folders whose files a job's output may be opened in: the system's
/// temporary-files roots, where no privacy prompt applies.
pub const TAIL_ROOTS: [&str; 7] = [
    "/private/tmp/",
    "/private/var/folders/",
    "/tmp/",
    "/var/folders/",
    "/var/tmp/",
    "/dev/shm/",
    "/run/user/",
];

/// Whether an output file at `path` may be opened to read its new bytes:
/// an absolute path with no `.` or `..`, under a [`TAIL_ROOTS`] folder, on
/// a mount `table` calls local and not automounted. PURE: the path is
/// judged from the kernel's mount table alone, so a hung mount cannot block
/// it.
#[must_use]
pub fn tail_allowed(path: &Path, table: &crate::volumes::Table) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if !TAIL_ROOTS.iter().any(|r| text.starts_with(r)) {
        return false;
    }
    crate::volumes::mount_of(path, table)
        .is_some_and(|m| m.local && !m.automounted && m.fstype != "autofs")
}

/// Whether the bytes of `path` between `from` and `to` hold a line feed:
/// at most the last [`TAIL_CAP`] of them, read with one `pread` on a
/// descriptor opened read-only, non-blocking and without following a final
/// symlink. `None` when it cannot be opened or read.
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

/// The most processes one user may run (`kern.maxprocperuid` on macOS,
/// `Max processes` in `/proc/self/limits` on Linux); `None` where unknown or
/// unlimited.
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
