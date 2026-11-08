// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stuck processes and jobs: what each job in a tab is doing, and whether anyone should worry.

pub mod step;
pub mod units;
pub mod wait;
pub mod wire;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;

pub use step::{Fault, Progress, Tick, Track};
pub use units::{Jobs, NamingError, NamingSelector, NamingTicket, NamingWaiter};
pub use wait::TierB;

/// How long a job reads `running` after a progress event.
pub const RUNNING_WINDOW_MS: u64 = 5_000;
/// How long an exited job keeps its row.
pub const EXITED_KEEP_MS: u64 = 60_000;
/// The floor under μ: a job that printed every second still gets 10 s.
pub const MU_FLOOR_MS: u64 = 10_000;
/// μ for a command nothing has been learned about.
pub const MU_PRIOR_MS: u64 = 60_000;
/// φ × 1000 at which a suspect job reads quiet.
pub const PHI_QUIET_MILLI: u64 = 3_000;
/// φ × 1000 below which a quiet job reads fine again.
pub const PHI_QUIET_LEAVE_MILLI: u64 = 2_500;
/// φ × 1000 at which a stuck-capable job reads stuck.
pub const PHI_STUCK_MILLI: u64 = 8_000;
/// CPU per minute below which a job makes no CPU progress.
pub const NO_PROGRESS_CPU_NS_PER_MIN: u64 = 1_000_000;
/// How long a zombie must have been seen before it is a zombie class or a finding.
pub const ZOMBIE_AGE_MS: u64 = 10_000;
/// How long a thread must have been in an uninterruptible wait to be named `kernel`.
pub const KERNEL_WAIT_MS: u64 = 10_000;
/// How long an agent's foreground call follows or listens before it is "never returns".
pub const NEVER_RETURNS_MS: u64 = 10_000;
/// How long an orphan must burn a core with no side writes to be a busy-silent orphan finding.
pub const BUSY_ORPHAN_MS: u64 = 600_000;
/// How many programs macOS's program check holds at one reading that make a backed-up queue: a
/// normal exec reads held for under 0.3 ms, so this many at once is a line. The rule's other half
/// is one held across two readings; a reader that keeps a count, not rows, reads that half as
/// programs held at both of its readings (`aterm_uds::process::HeldNow::backed_up`).
pub const HELD_AT_ONCE: usize = 3;
/// The intervals a job's own μ is the median of (the last ones).
pub const OWN_RING: usize = 32;
/// Intervals a job needs before its own median is its μ (and before it teaches its command class).
pub const OWN_MIN: usize = 3;
/// Command classes remembered (least recently used dropped).
pub const CLASS_TABLE: usize = 256;
/// Completed jobs' medians a class keeps.
pub const CLASS_RING: usize = 16;

/// A process's scheduling state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProcState {
    /// On a core or runnable.
    Running,
    /// Asleep, interruptibly.
    Sleeping,
    /// Stopped by a signal or a debugger.
    Stopped,
    /// Exited, not yet reaped.
    Zombie,
    /// In an uninterruptible kernel wait (disk, a hung mount).
    Uninterruptible,
    /// Idle or being created.
    Idle,
    /// Unread.
    #[default]
    Unknown,
}

/// One process from the cheap sweep of the whole table (`aterm_sysprobe::procs::ProcessReading`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepProc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Its process group (0 where unread).
    pub pgid: u32,
    /// Its user; `None` where unread.
    pub uid: Option<u32>,
    /// Its name ([`crate::model::ProcSample::name`]).
    pub name: String,
    /// Its state.
    pub state: ProcState,
    /// Whether it leads its session; `None` where unread.
    pub session_leader: Option<bool>,
    /// Whether it has a controlling terminal; `None` where unread.
    pub has_tty: Option<bool>,
    /// Whether it ran a program since its fork; `None` where unread.
    pub exec: Option<bool>,
}

/// A process's cumulative counters (`aterm_sysprobe::jobs::Counters`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobCounters {
    /// User plus system CPU, ns.
    pub cpu_ns: u64,
    /// CPU of the children it reaped, ns.
    pub child_cpu_ns: Option<u64>,
    /// System calls.
    pub syscalls: Option<u64>,
    /// Idle and interrupt wakeups (`None` on Linux).
    pub wakeups: Option<u64>,
    /// Context switches.
    pub ctx_switches: Option<u64>,
    /// Bytes read from disk.
    pub disk_read: Option<u64>,
    /// Bytes written to disk.
    pub disk_written: Option<u64>,
    /// Bytes written through any descriptor, before the page cache.
    pub logical_writes: Option<u64>,
    /// Time runnable (waiting for a core included), ns.
    pub runnable_ns: Option<u64>,
    /// Page-ins.
    pub pageins: Option<u64>,
    /// The largest physical footprint it has had (`ri_lifetime_max_phys_footprint`); `None`
    /// where unread.
    pub peak_footprint: Option<u64>,
}

/// One thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadState {
    /// Its state.
    pub state: ProcState,
    /// The kernel function it waits in (Linux), where public.
    pub wchan: Option<String>,
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
    /// Any other closing state.
    Closing,
    /// Any other state.
    Other,
}

impl TcpState {
    /// The state as `sys jobs` prints it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Closed => "CLOSED",
            Self::Listen => "LISTEN",
            Self::SynSent => "SYN_SENT",
            Self::SynReceived => "SYN_RECEIVED",
            Self::Established => "ESTABLISHED",
            Self::CloseWait => "CLOSE_WAIT",
            Self::Closing => "CLOSING",
            Self::Other => "OTHER",
        }
    }
}

/// What an event-queue entry waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnoteFilter {
    /// Readable.
    Read,
    /// Writable.
    Write,
    /// A timer.
    Timer,
    /// A file changed.
    Vnode,
    /// A process event.
    Proc,
    /// A signal.
    Signal,
    /// A user event.
    User,
    /// Anything else.
    Other,
}

/// One entry of an event queue: the descriptor (or timer id) and the filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Knote {
    /// The descriptor it watches (or the timer's id).
    pub ident: u64,
    /// What it waits for.
    pub filter: KnoteFilter,
}

/// What one descriptor is (`aterm_sysprobe::jobs::FdKind`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FdKind {
    /// A file on a file system.
    File {
        /// A regular file.
        regular: bool,
        /// Its size (Linux: the descriptor's offset).
        size: u64,
        /// Its device.
        dev: u64,
        /// Its inode.
        ino: u64,
        /// Whether the bytes appended since the reader's last look hold a line feed.
        lf_tail: Option<bool>,
        /// A dylib a build just wrote, as the host judged it (`aterm_sysprobe::jobs::fresh_dylib`).
        fresh_dylib: bool,
    },
    /// A character device.
    Device {
        /// A terminal.
        tty: bool,
        /// Opened for reading; `true` where unread, `false` for one only written (`tee /dev/tty`).
        read: bool,
        /// Opened as `/dev/tty`, not inherited or duplicated from a terminal (zsh's fd 10).
        ctty: bool,
    },
    /// One end of a pipe.
    Pipe {
        /// This end's kernel handle (Linux: the pipe's inode).
        handle: u64,
        /// The other end's handle (Linux: the same inode).
        peer: u64,
        /// The pipe state word; `None` where the OS gives none or lane L7 is off.
        status: Option<u32>,
        /// Bytes queued in this end's buffer.
        queued: Option<u64>,
        /// Whether this is the write end.
        write_end: Option<bool>,
    },
    /// A TCP socket.
    Tcp {
        /// Its state.
        state: TcpState,
        /// Its peer (an IP literal, never a name).
        remote: Option<SocketAddr>,
        /// Its local port.
        local_port: u16,
        /// Bytes received and not read.
        queued_read: Option<u64>,
    },
    /// Any other socket.
    Socket,
    /// An event queue (kqueue, epoll).
    EventQueue {
        /// Whether a thread sleeps on it now.
        sleeping: Option<bool>,
        /// Its entries; `None` where unlisted (macOS without lane L6).
        knotes: Option<Vec<Knote>>,
    },
    /// A file-change watch (inotify).
    Watch,
    /// Anything else.
    Other,
}

/// One open descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fd {
    /// Its number.
    pub fd: i32,
    /// What it is.
    pub kind: FdKind,
}

/// Everything read about one process (`aterm_sysprobe::jobs::ProcFacts`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobProc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Its process group.
    pub pgid: u32,
    /// Its session; `None` where unread.
    pub sid: Option<u32>,
    /// When it started, compared for equality only; `None` where unread.
    pub start: Option<u64>,
    /// How long it had run when the probe read it ([`JobTick::probe_ms`]), ms; `None` where unread.
    pub age_ms: Option<u64>,
    /// Its user.
    pub uid: Option<u32>,
    /// Its short name (`p_comm`, `comm`).
    pub name: String,
    /// Its executable's basename: the name rows print, `name` where `None`.
    pub exe: Option<String>,
    /// The program a build's front becomes once its memory claim is admitted (`targo`), where
    /// its arguments say it is one (the host asks `aterm_uds::build_front::front`): rows name it
    /// by this, before `exe`. `None` for every other process, and where its arguments were not
    /// read.
    pub becomes: Option<String>,
    /// While a front's claim waits in its ledger (the host asks `atpkg::build_admit::front_claim`
    /// each pass): the pid of each memory claim ahead of it, oldest first, empty where its ledger
    /// says nothing of them (none laid yet, or unread). While it is a leaf it waits on
    /// [`WaitName::Memory`], counted as those claims' holders progress. `None` for every other
    /// process, and for a front whose ledger holds its claim (admitted, handing it to its holder),
    /// which reads by its own wait.
    pub claims_ahead: Option<Vec<u32>>,
    /// Its state.
    pub state: ProcState,
    /// A debugger traces it (a debugger's child at a breakpoint is no job-control stop).
    pub traced: bool,
    /// Its controlling terminal; `None` without one.
    pub tty: Option<u32>,
    /// That terminal's foreground process group.
    pub tpgid: Option<u32>,
    /// That terminal's line discipline, read for a session leader
    /// (`aterm_sysprobe::jobs::term_word`); `None` for any other process, or where aterm's user
    /// may not open the terminal.
    pub term: Option<TermWord>,
    /// Its counters; `None` where refused.
    pub counters: Option<JobCounters>,
    /// Its threads; `None` where refused.
    pub threads: Option<Vec<ThreadState>>,
    /// The kernel function it waits in (Linux).
    pub wchan: Option<String>,
    /// Its descriptors, when asked for; `None` when not asked or refused.
    pub fds: Option<Vec<Fd>>,
    /// The descriptor it sleeps reading or writing, read with its descriptors (Linux
    /// `/proc/<pid>/syscall`); `None` elsewhere, where unread, or in no read or write.
    pub blocked_fd: Option<i32>,
    /// What keeps it from its first instruction where its reading judged it unstarted (macOS's
    /// program check holds it, Rosetta translates it, …: `aterm_sysprobe::jobs::AtStart`);
    /// `None` for one that started, and where no start was judged.
    pub start_wait: Option<StartWait>,
    /// Its pid version where read (macOS `p_idversion`, read for each tab's root): a new process
    /// and each exec take a new one, so it moves where the pid, the start and the name stay
    /// (`exec zsh`); `None` elsewhere.
    pub version: Option<u64>,
    /// The program's exec identity where the host read one for this member; unlike `version`,
    /// this is not limited to tab roots. An unavailable value remains unavailable.
    pub exec_version: Option<u64>,
}

/// What keeps a process whose one thread never ran from its first instruction past 20 s
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.2), as the host judged it
/// (`aterm_sysprobe::jobs::AtStart`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StartWait {
    /// macOS's program check holds it: native, its executable on a local volume.
    #[default]
    ProgramCheck,
    /// An x86_64 process on an arm64 Mac: Rosetta translates it, and aterm's Developer Tools
    /// grant does not reach that.
    Translating,
    /// Its executable is on a volume not proven local: it has not started, and nothing read
    /// says why.
    NotStarted,
    /// It reads stopped or traced: a start-suspended spawn or a debugger's launch, unless macOS's
    /// program check holds a program that reads so, which nothing has measured (the
    /// leftover-processes design's U3).
    StoppedUnstarted {
        /// Its BSD status reads `SSTOP`.
        stopped: bool,
        /// It reads `PROC_FLAG_TRACED`.
        traced: bool,
    },
}

impl StartWait {
    /// The word a `finding` row's `kind=` prints: `program-check`, `translating`, `not-started`
    /// or `stopped-unstarted`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::ProgramCheck => "program-check",
            Self::Translating => "translating",
            Self::NotStarted => "not-started",
            Self::StoppedUnstarted { .. } => "stopped-unstarted",
        }
    }
}

/// One process of aterm's user, in a job or not, whose one thread never ran past 20 s: held at
/// macOS's program check, translating, not started, or stopped or traced ([`StartWait`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldProc {
    /// Its pid.
    pub pid: u32,
    /// Which program the pid runs: its pid version where read (it moves on exec), else its
    /// start. A wrapper that execs its held tool starts a new held time.
    pub exec: u64,
    /// Its actual pid version, which changes on exec, proved around the current wait reading.
    /// Unlike [`Self::exec`], this never substitutes the process's birth or zero for an
    /// unavailable or inconsistent version.
    pub exec_version: Option<u64>,
    /// Its executable's basename, else its short name.
    pub name: String,
    /// Whether aterm started it (a tab's member, or aterm's resource coalition): words only.
    pub aterm: bool,
    /// What keeps it from starting.
    pub wait: StartWait,
    /// The tab of the session a group-mate's environment ties it to while that session lives,
    /// for one outside every job (the host's W-session read; its own environment is never read).
    pub tab: Option<u64>,
    /// The kind of job that tie names it by, for one outside every job: an agent's call where it
    /// was born after that session's agent started (`left::Tie::after_agent`), else none.
    pub job: Option<JobKind>,
    /// When it forked, unix ms; `None` where unread.
    pub started_ms: Option<u64>,
    /// Its check went to a syspolicyd that is no longer running: held since before the one
    /// running now started, which answers alone (`aterm_sysprobe::jobs::stranded`). The host
    /// judges it only against [`JobTick::program_check_daemon`], and keeps the verdict while it
    /// reads the row unrun as the same exec at the same CPU. No part of the held key: a row that
    /// turns stranded keeps its held time.
    pub stranded: bool,
}

/// One OSC 133 command block of a tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    /// The block's id, stable within the session (`b<id>`).
    pub id: u64,
    /// When `C` (command start) was seen, host ms.
    pub started_ms: u64,
    /// `C` seen and no `D` yet: the command runs.
    pub open: bool,
    /// The command line, as the shell reported it.
    pub command: String,
    /// The exit status `D` carried, once closed.
    pub status: Option<i32>,
    /// `status` says job control stopped the command (Ctrl-Z): it lives on, parked. The host
    /// decodes it, the signal's number being the host's.
    pub stopped: bool,
}

/// The session's terminal mode word (§6.6), as last stamped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtyMode {
    /// `ICANON`.
    pub canonical: bool,
    /// `ECHO`.
    pub echo: bool,
    /// When the word was read, host ms.
    pub stamped_ms: u64,
}

/// A terminal's line discipline as a sweep read it (`aterm_sysprobe::jobs::TermWord`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TermWord {
    /// `ICANON`.
    pub canonical: bool,
    /// `ECHO`.
    pub echo: bool,
    /// Bytes typed that its reader has not read (`FIONREAD`: a canonical terminal's whole lines).
    pub queued: u32,
}

/// The unread-input gate's word (`aterm_session::input_backlog::InputWord`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GateWord {
    /// No reading.
    #[default]
    Unmeasured,
    /// Nothing unread.
    Clear,
    /// Unread input, not published.
    Pending,
    /// Canonical type-ahead.
    Typeahead,
    /// A published stall.
    Stalled,
    /// Input queued to a stopped job.
    Stopped,
}

impl GateWord {
    /// The gate's wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unmeasured => "-",
            Self::Clear => "clear",
            Self::Pending => "pending",
            Self::Typeahead => "typeahead",
            Self::Stalled => "stalled",
            Self::Stopped => "stopped",
        }
    }
}

/// The unread-input gate's reading for a tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputGate {
    /// Its word.
    pub word: GateWord,
    /// Bytes unread.
    pub bytes: u64,
    /// How long the oldest unread byte has waited, ms.
    pub wait_ms: u64,
}

/// Who waits for a job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Consumer {
    /// The person, the job holding the terminal.
    #[default]
    PersonFg,
    /// The person, a background job.
    PersonBg,
    /// An agent's foreground tool call: the agent waits for it.
    AgentFg,
    /// An agent's background call (run in the background, or promoted at a timeout).
    AgentBg,
    /// An agent, foreground or background unknown.
    Agent,
    /// Nobody: an orphan.
    Nobody,
}

impl Consumer {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::PersonFg => "person-fg",
            Self::PersonBg => "person-bg",
            Self::AgentFg => "agent-fg",
            Self::AgentBg => "agent-bg",
            Self::Agent => "agent",
            Self::Nobody => "nobody",
        }
    }
}

/// What the agent-transcript join learned about one detached call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallHint {
    /// The call's leader pid.
    pub leader: u32,
    /// Foreground or background.
    pub consumer: Consumer,
    /// Promoted to background at this timeout, ms (`timedOutAfterMs`).
    pub promoted_ms: Option<u64>,
    /// The agent's task id for it.
    pub task: Option<String>,
    /// The call's command line (its `eval '…'` argv after quote removal), for the command class.
    pub command: Option<String>,
}

/// A Tier B naming the host made for one job, on request (§11.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedWait {
    /// A pid of the job.
    pub leader: u32,
    /// The mapped word.
    pub word: TierB,
    /// For a `lock`, the holder the search found.
    pub holder: Option<u32>,
}

/// One tab, as the host sees it this tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobTab {
    /// The host's stable key for it (its session id).
    pub key: u64,
    /// Its number as the person sees it (`tab2`).
    pub number: u32,
    /// Its shell's pid; 0 when it has none.
    pub shell_pid: u32,
    /// The latest OSC 133 block; `None` without shell integration.
    pub block: Option<Block>,
    /// The block the latest one replaced, closed: a command that ended and was followed by
    /// another between two ticks keeps its status.
    pub ended: Option<Block>,
    /// When the shell last wrote one of its own OSC 133 marks (a prompt, a command taken, its
    /// end), host ms: lines read since may be its prompt's, or a command's that ran between ticks.
    pub shell_ms: Option<u64>,
    /// The terminal mode word; `None` where unread.
    pub tty: Option<TtyMode>,
    /// The unread-input gate.
    pub input: InputGate,
    /// When aterm last read any PTY output, host ms.
    pub output_ms: Option<u64>,
    /// When aterm last read PTY output holding a line feed, host ms.
    pub line_output_ms: Option<u64>,
    /// When aterm last read PTY output that drew the screen (put the cursor on a row of its own
    /// choosing or moved rows: nvim, Emacs, a pager, whose line feeds are cursor moves), host ms.
    pub drawn_ms: Option<u64>,
    /// The terminal shows its alternate screen, as the reader last fed it (a full-screen
    /// program's: vim, less, htop; never a line editor's at a prompt).
    pub alt_screen: bool,
    /// PTY bytes read so far (cumulative).
    pub output_bytes: u64,
    /// When the program last consumed typed input, host ms.
    pub input_consumed_ms: Option<u64>,
    /// When a pass found keys driven into the tab's terminal since the pass before (the
    /// person's, a driver's): that pass's clock, host ms.
    pub typed_ms: Option<u64>,
    /// The transcript join's hints for detached calls.
    pub calls: Vec<CallHint>,
    /// Tier B namings made on request.
    pub named: Vec<NamedWait>,
    /// The tab's session was adopted from another aterm process (an update's hand-off): macOS may
    /// check its new programs whatever aterm's Developer Tools grant says (the leftover-processes
    /// design's M13).
    pub adopted: bool,
}

/// The machine, as the host reads it this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    /// `Contention::factor_milli`: thresholds stretch by this (1000 = idle, at most 10,000).
    pub stretch_milli: u32,
    /// The machine reads `thrashing` (§9.2).
    pub thrashing: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            stretch_milli: 1000,
            thrashing: false,
        }
    }
}

/// Which optional lanes the reader has (`aterm_sysprobe::jobs::Lanes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanesSeen {
    /// L6: kqueue entries (`follow`, `listen`, `timer`).
    pub kqueue_detail: bool,
    /// L7: pipe wait flags (`pipe`, `pipe-full`).
    pub pipe_flags: bool,
}

impl Default for LanesSeen {
    fn default() -> Self {
        Self {
            kqueue_detail: true,
            pipe_flags: true,
        }
    }
}

/// Everything the host hands one tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobTick {
    /// The host's monotonic clock, ms.
    pub at_ms: u64,
    /// The host's clock, ms, once it had read [`JobTick::procs`]: each shell's terminal group,
    /// which says what is in front, is read after the sweep, past `at_ms`.
    pub read_ms: u64,
    /// The host's clock, ms, as the probe began reading [`JobTick::procs`], each
    /// [`JobProc::age_ms`] taken then: a reading that runs late (the sweep before it, under load)
    /// starts past `at_ms`.
    pub probe_ms: u64,
    /// aterm's own user (for the own-uid findings).
    pub self_uid: Option<u32>,
    /// The cheap sweep of every process.
    pub sweep: Vec<SweepProc>,
    /// The detailed readings of what [`Jobs::plan`] asked for (a process gone or refused
    /// meanwhile is absent).
    pub procs: Vec<JobProc>,
    /// Every tab.
    pub tabs: Vec<JobTab>,
    /// The machine.
    pub machine: Machine,
    /// The reader's lanes.
    pub lanes: LanesSeen,
    /// The per-user process limit (`kern.maxprocperuid`, `RLIMIT_NPROC`).
    pub process_limit: Option<u64>,
    /// aterm's user's processes that have not started: held at macOS's program check,
    /// translated by Rosetta, on a volume not proven local, or stopped or traced.
    pub held: Vec<HeldProc>,
    /// The syspolicyd [`HeldProc::stranded`] was judged against, by pid: the one the stuck
    /// check's board read lately, else the one a kept verdict was judged under; `None` with
    /// neither, when no row is named stranded.
    pub program_check_daemon: Option<u32>,
    /// Whether every held row was judged against it: its pool read not full, and each row the
    /// check holds is stranded or forked since its start, its check the running one's. `false`
    /// while one may be stranded and nothing read tells.
    pub program_check_judged: bool,
    /// When aterm's Developer Tools grant took effect, unix ms, while the host reads aterm granted:
    /// from then on macOS checks no new program aterm starts (the leftover-processes design's
    /// M13). `None` when not granted or not known.
    pub grant_ms: Option<u64>,
    /// How and when each agent call's leader the host watches ended ([`JobRow::leader`]), read
    /// since its last pass: the kernel's wait status of the process, decoded by the host, and
    /// the moment the kernel reported it ([`LeaderExit`]). A call whose leader has no line here
    /// ends with no status, at the pass that finds it gone.
    pub exits: Vec<LeaderExit>,
}

/// How a process ended, as the kernel's wait status of it says (decoded by the host, which
/// alone reads the kernel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExitOf {
    /// It exited with this status (`exit(3)`: 3).
    Code(i32),
    /// This signal ended it.
    Signal(i32),
}

/// The end of a process the host watched ([`JobTick::exits`]): its pid and start, as the job's
/// leader names them ([`JobRow::leader`]), so a pid taken anew is another process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LeaderExit {
    /// Its pid.
    pub pid: u32,
    /// Its start, as [`JobProc::start`] reads it.
    pub start: Option<u64>,
    /// How it ended.
    pub end: ExitOf,
    /// When it ended, on the tick's clock ([`JobTick::at_ms`]): the moment the host's watch saw
    /// the kernel report the exit, however long before the pass that read it (at the latest, the
    /// read itself).
    pub at_ms: u64,
}

/// The shells a row may name (`p_comm`, a leading login `-` ignored): an agent call led by one is
/// its agent's wrapper (`zsh -c …` leads every Claude Code call), not the program it ran.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ash", "ksh", "mksh", "oksh", "loksh", "pdksh", "yash",
    "tcsh", "csh", "nu", "xonsh", "elvish", "ion", "pwsh", "rc", "es",
];

/// Whether `comm` names a shell (a leading login `-` ignored).
#[must_use]
pub fn is_shell_name(comm: &str) -> bool {
    let name = comm.trim_start_matches('-');
    SHELLS.contains(&name)
}

/// One process the host should read in detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Want {
    /// Its pid.
    pub pid: u32,
    /// Whether its descriptors are needed.
    pub fds: bool,
}

/// A job that ended, as its passes read it ([`Jobs::take_ended`]): what its telemetry row is
/// made of. Its figures are the passes' (every 2 s while a job runs): `ended_ms` is the pass that
/// found it gone, `peak_bytes` and `cpu_ns` the most any pass read, so lower bounds. An agent
/// call whose leader's exit the host read ends at its leader's own moment
/// ([`Self::leader_exited_ms`]); any other at the pass that found it gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    /// Its tab's key (the session id).
    pub tab: u64,
    /// The OSC 133 block it was the foreground job of, if one.
    pub block: Option<u64>,
    /// Its kind.
    pub kind: JobKind,
    /// Who waited for it.
    pub consumer: Consumer,
    /// Its id.
    pub id: String,
    /// Its command, as the join named it (an agent call's) or the block read it; may be empty.
    pub command: String,
    /// The program name its last pass showed.
    pub name: String,
    /// The pass that first tracked it, host ms.
    pub opened_ms: u64,
    /// The last pass that read it alive, host ms.
    pub last_read_ms: Option<u64>,
    /// The pass that found it gone, host ms.
    pub ended_ms: u64,
    /// Its exit status, where its block said it.
    pub exit: Option<i32>,
    /// How an agent call's leader ended, where the host watched it ([`JobTick::exits`]).
    pub ended_by: Option<ExitOf>,
    /// When an agent call's leader ended, host ms, as the host read it ([`LeaderExit::at_ms`]):
    /// set exactly where [`Self::ended_by`] is.
    pub leader_exited_ms: Option<u64>,
    /// The program an agent call ran: its leader's name once that names no shell (the leader
    /// `exec`ed the program, or the agent ran it with no wrapper), else the last a member other
    /// than its leader showed, the wrapper's own children first and the wrapper's forks (its
    /// subshells, named as it is) passed over. `None` where no pass saw one: a call of shell
    /// builtins alone, or one that ended between passes.
    pub ran: Option<String>,
    /// The largest lifetime-peak footprint a pass read of a member, bytes.
    pub peak_bytes: Option<u64>,
    /// The most CPU a pass read its members (and the children they reaped) to have spent, ns.
    pub cpu_ns: Option<u64>,
    /// The last pass that read it live judged it stuck.
    pub stuck: bool,
}

/// What the passes read so far of a block's foreground job ([`Jobs::block_meters`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockMeter {
    /// Its tab's key (the session id).
    pub tab: u64,
    /// The block.
    pub block: u64,
    /// The largest lifetime-peak footprint a pass read of a member other than the shell, bytes.
    pub peak_bytes: Option<u64>,
    /// The last pass that read it live judged it stuck.
    pub stuck: bool,
}

/// What kind of unit a job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// The shell's foreground job.
    Shell,
    /// Another group in the tab's session.
    ShellBg,
    /// A detached child.
    AgentCall,
    /// A terminal session nested in the tab's (sudo's `use_pty`, `script`, an agent's pty call).
    Nested,
    /// What lived on after its job ended.
    Orphan,
}

impl JobKind {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::ShellBg => "shell-bg",
            Self::AgentCall => "agent-call",
            Self::Nested => "nested",
            Self::Orphan => "orphan",
        }
    }
}

/// A job's class (§11.5): what it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// No live member.
    Exited,
    /// The tty's program leaves typed input unread (the gate's word).
    UnreadInput,
    /// A progress event within [`RUNNING_WINDOW_MS`].
    Running,
    /// A member is stopped.
    Stopped,
    /// The machine thrashes and the job waits for pages.
    Thrashing,
    /// Burning CPU with no output.
    BusySilent,
    /// Waking often with no progress.
    Polling,
    /// An unreaped child of a live member.
    Zombie,
    /// Waiting (see its [`Wait`]).
    Waiting,
}

impl Class {
    /// Every class.
    pub const ALL: [Self; 9] = [
        Self::Exited,
        Self::UnreadInput,
        Self::Running,
        Self::Stopped,
        Self::Thrashing,
        Self::BusySilent,
        Self::Polling,
        Self::Zombie,
        Self::Waiting,
    ];

    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::UnreadInput => "unread-input",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Thrashing => "thrashing",
            Self::BusySilent => "busy-silent",
            Self::Polling => "polling",
            Self::Zombie => "zombie",
            Self::Waiting => "waiting",
        }
    }

    /// The class a wire word names.
    #[must_use]
    pub fn from_word(w: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.word() == w)
    }
}

/// The closed verdict set (§11.7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Nothing to say.
    #[default]
    Fine,
    /// A password prompt on the person's foreground job.
    WaitingYou,
    /// Stopped by job control, the shell holding the terminal again.
    Parked,
    /// φ ≥ 3 on a suspect wait.
    Quiet,
    /// Busy with no output, φ ≥ 3 (never stuck).
    Busy,
    /// The machine is swapping, φ ≥ 3 (never stuck).
    Slow,
    /// φ ≥ 8 on a stuck-capable class, or the gate's published stall.
    Stuck,
    /// An agent's foreground call that follows or listens: it never returns by itself.
    NeverReturns,
    /// No live member (kept [`EXITED_KEEP_MS`]).
    Exited,
}

impl Verdict {
    /// Every verdict.
    pub const ALL: [Self; 9] = [
        Self::Fine,
        Self::WaitingYou,
        Self::Parked,
        Self::Quiet,
        Self::Busy,
        Self::Slow,
        Self::Stuck,
        Self::NeverReturns,
        Self::Exited,
    ];

    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fine => "fine",
            Self::WaitingYou => "waiting-you",
            Self::Parked => "parked",
            Self::Quiet => "quiet",
            Self::Busy => "busy",
            Self::Slow => "slow",
            Self::Stuck => "stuck",
            Self::NeverReturns => "never-returns",
            Self::Exited => "exited",
        }
    }

    /// The verdict a wire word names; `done` is `await sys`'s alias of `exited`.
    #[must_use]
    pub fn from_word(w: &str) -> Option<Self> {
        if w == "done" {
            return Some(Self::Exited);
        }
        Self::ALL.into_iter().find(|v| v.word() == w)
    }

    /// The mark a chip or row draws; `None` for a verdict shown as words alone or not at all.
    #[must_use]
    pub const fn mark(self) -> Option<char> {
        match self {
            Self::WaitingYou => Some('◇'),
            Self::Quiet => Some('◔'),
            Self::Stuck => Some('⊘'),
            Self::NeverReturns => Some('↻'),
            _ => None,
        }
    }

    /// A suspect rank an episode can say once (quiet, busy, slow, stuck).
    #[must_use]
    pub const fn is_suspect(self) -> bool {
        matches!(self, Self::Quiet | Self::Busy | Self::Slow | Self::Stuck)
    }
}

/// The named waits (§11.5 Tier A, and the Tier B words a request adds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaitName {
    /// Held at macOS's program check before its first instruction: work in line.
    ProgramCheck,
    /// An x86_64 program Rosetta is translating before its first instruction: work in line.
    Translating,
    /// A build's front waiting for its memory claim in the capacity ledger
    /// ([`JobProc::claims_ahead`]): work in line behind other claims.
    Memory,
    /// The person's foreground job reads a password (canonical, no echo).
    TtyPassword,
    /// The shell's own `read` builtin.
    TtyBuiltinRead,
    /// A full-screen program reading keys.
    TtyRaw,
    /// A detached job reading a pipe its consumer writes: a helper server.
    Serves,
    /// Blocked reading a pipe; the writer decides.
    Pipe,
    /// Blocked writing a full pipe; the reader decides.
    PipeFull,
    /// A TCP connection with nothing queued to read.
    Tcp,
    /// Following a file (`tail -f`).
    Follow,
    /// Waiting on a listening socket.
    Listen,
    /// A timer only.
    Timer,
    /// An event loop with nothing pending.
    EventLoop,
    /// An uninterruptible kernel wait (disk, a hung mount).
    Kernel,
    /// None of the above.
    Unnamed,
    /// Tier B: a file lock.
    Lock,
    /// Tier B: a plain `read`; its descriptor and terminal status are unproven.
    Read,
    /// Tier B: a Mach message.
    Ipc,
    /// Tier B: a mutex or condition variable.
    Mutex,
    /// Tier B: waits on its children.
    Children,
    /// Tier B: a stack whose leaf maps to no word.
    NamedUnparsed,
}

impl WaitName {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::ProgramCheck => "program-check",
            Self::Translating => "translating",
            Self::Memory => "memory",
            Self::TtyPassword => "tty-password",
            Self::TtyBuiltinRead => "tty-builtin-read",
            Self::TtyRaw => "tty-raw",
            Self::Serves => "serves",
            Self::Pipe => "pipe",
            Self::PipeFull => "pipe-full",
            Self::Tcp => "tcp",
            Self::Follow => "follow",
            Self::Listen => "listen",
            Self::Timer => "timer",
            Self::EventLoop => "event-loop",
            Self::Kernel => "kernel",
            Self::Unnamed => "unnamed",
            Self::Lock => "lock",
            Self::Read => "read",
            Self::Ipc => "ipc",
            Self::Mutex => "mutex",
            Self::Children => "children",
            Self::NamedUnparsed => "named-unparsed",
        }
    }
}

/// How a wait counts (§11.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Standing {
    /// Someone legitimately waits on it: φ is 0.
    Legit,
    /// Suspect, and it may reach stuck.
    Capable,
    /// Suspect, capped at quiet (busy, slow).
    Capped,
}

/// A named wait and what it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The name.
    pub name: WaitName,
    /// How it counts, the pipe's peer decided.
    pub standing: Standing,
    /// The process it names: the pipe's writer or reader, the server's consumer.
    pub peer: Option<u32>,
    /// The waiting process.
    pub leaf: u32,
    /// The waiting process's name.
    pub leaf_name: String,
    /// For `tcp`: the peer, its state and the bytes queued.
    pub tcp: Option<(Option<SocketAddr>, TcpState, u64)>,
    /// For `follow`: the descriptor followed.
    pub fd: Option<u64>,
    /// For `listen`: the port.
    pub port: Option<u16>,
    /// For `pipe` and `pipe-full`: the end it sleeps on, by its handle and its peer's.
    pub pipe: Option<(u64, u64)>,
}

impl Wait {
    /// A wait with only a name and standing.
    #[must_use]
    pub fn bare(name: WaitName, standing: Standing, leaf: u32, leaf_name: &str) -> Self {
        Self {
            name,
            standing,
            peer: None,
            leaf,
            leaf_name: leaf_name.to_owned(),
            tcp: None,
            fd: None,
            port: None,
            pipe: None,
        }
    }
}

/// A stopped member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    /// Parked (job control took the terminal back) or held.
    pub parked: bool,
    /// The stopped process.
    pub pid: u32,
    /// Its name.
    pub name: String,
}

/// A job's unreaped children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zombies {
    /// Zombies seen at least [`ZOMBIE_AGE_MS`].
    pub count: u32,
    /// The parent holding most of them.
    pub parent: u32,
    /// New zombies a minute, × 10, since the parent's first.
    pub per_min_deci: u64,
}

/// Where a μ came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MuFrom {
    /// The job's own progress intervals.
    Own,
    /// The command class's learned median.
    Class,
    /// The prior.
    Prior,
}

impl MuFrom {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Class => "class",
            Self::Prior => "prior",
        }
    }
}

/// One job, as the engine sees it this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    /// Its id, stable per session.
    pub id: String,
    /// The tab it belongs to.
    pub tab: u64,
    /// The unit kind.
    pub kind: JobKind,
    /// For an orphan unit, the kind of the job it outlived (an agent's call, a shell's job).
    pub orphan_of: Option<JobKind>,
    /// Who waits for it.
    pub consumer: Consumer,
    /// Promoted to background at this timeout, ms.
    pub promoted_ms: Option<u64>,
    /// The agent's task id.
    pub task: Option<String>,
    /// Its process group (its leader pid on POSIX).
    pub pgid: u32,
    /// The leader's pid and start.
    pub leader: (u32, Option<u64>),
    /// The process the row names: the waiting leaf for a wait, else the leader.
    pub exe: String,
    /// Live members.
    pub members: usize,
    /// What it is doing.
    pub class: Class,
    /// The wait, for a waiting job.
    pub wait: Option<Wait>,
    /// A Tier B naming that holds this episode.
    pub named: Option<NamedRow>,
    /// The stopped member, for a stopped job.
    pub stop: Option<Stop>,
    /// The gate's reading, for unread input.
    pub input: Option<InputGate>,
    /// CPU over the last tick, millicores.
    pub cpu_mc: Option<u64>,
    /// Its unreaped children.
    pub zombies: Option<Zombies>,
    /// How it counts this tick.
    pub standing: Option<Standing>,
    /// Quiet age, ms: the suspect quiet for a suspect job, the wait's age for a legitimate one.
    pub quiet_ms: Option<u64>,
    /// φ × 1000 (0 for a legitimate wait; `None` where φ is not used).
    pub phi_milli: Option<u64>,
    /// μ (stretched as the class says), ms.
    pub mu_ms: Option<u64>,
    /// Where μ came from.
    pub mu_from: Option<MuFrom>,
    /// The verdict.
    pub verdict: Verdict,
    /// An agent's call quiet at φ ≥ 8 whose members used under [`NO_PROGRESS_CPU_NS_PER_MIN`].
    pub no_progress: bool,
    /// A descendant call with current CPU work or progress within [`RUNNING_WINDOW_MS`]
    /// (leader pid and start).
    /// This qualifies the local verdict; it neither credits output nor changes resource totals.
    pub working_descendant: Option<(u32, u64)>,
    /// The suspect episode this verdict belongs to (each says a rank once).
    pub episode: u32,
    /// The exit status, once known.
    pub exit: Option<i32>,
    /// What this tick saw (the conformance projection reads it).
    pub tick: Tick,
}

/// A Tier B naming on a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedRow {
    /// The mapped word.
    pub word: TierB,
    /// For a `lock`, the holder the search found.
    pub holder: Option<Holder>,
}

/// A lock's holder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// Its pid.
    pub pid: u32,
    /// Its name.
    pub exe: String,
    /// The number of the tab it is in.
    pub tab: Option<u32>,
    /// Its job's class, when it is in a job.
    pub class: Option<Class>,
}

/// Unreaped children of one parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZombieParent {
    /// The parent.
    pub ppid: u32,
    /// Its name.
    pub name: String,
    /// Zombies seen at least [`ZOMBIE_AGE_MS`].
    pub count: u32,
    /// New a minute, × 10.
    pub per_min_deci: u64,
    /// The tab, when the parent is in one.
    pub tab: Option<u64>,
}

/// One process a finding names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    /// Its pid.
    pub pid: u32,
    /// The actual pid version of a held program, when read; `None` for other findings or an
    /// unavailable version. A later proof must match this version before reusing its held time.
    pub exec_version: Option<u64>,
    /// Its name.
    pub name: String,
    /// The tab, when it is in one.
    pub tab: Option<u64>,
    /// The kind of the live job it is a member of, an orphan unit's member the kind of the job
    /// the unit outlived (an agent's call's leftover reads [`JobKind::AgentCall`], the shell's
    /// job's [`JobKind::Shell`]); `None` outside every job.
    pub job: Option<JobKind>,
    /// How long, ms (D-state, busy orphans, and held since the first reading that saw it held).
    pub for_ms: u64,
}

/// A process whose one thread never ran past 20 s while it reads stopped or traced
/// ([`StartWait::StoppedUnstarted`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoppedUnstarted {
    /// The process, since the first reading that saw it so.
    pub named: Named,
    /// Its BSD status reads `SSTOP`.
    pub stopped: bool,
    /// It reads `PROC_FLAG_TRACED`.
    pub traced: bool,
}

/// A stretch of readings in which macOS's program check is backed up
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §5.1). It opens at the first reading where one
/// of aterm's user's programs was held at the reading before too, or three are held at once, and
/// closes at the second reading in a row that finds none held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeldEpisode {
    /// The reading it opened at, host ms.
    pub opened_ms: u64,
    /// From its opening to its last reading that found a program held (this one, while one is
    /// held), ms.
    pub for_ms: u64,
    /// The most programs held at one reading.
    pub peak: u32,
    /// The longest any of them was held, ms.
    pub oldest_ms: u64,
}

/// Per-process findings (§11.9), whatever the jobs' classes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Findings {
    /// Stopped processes, every user.
    pub stopped: Vec<Named>,
    /// Zombies, by parent, every user.
    pub zombies: Vec<ZombieParent>,
    /// Own processes with a thread in an uninterruptible wait ≥ 10 s.
    pub d_state: Vec<Named>,
    /// Orphans burning a core for ≥ 10 min with no side writes.
    pub busy_orphans: Vec<Named>,
    /// Own processes macOS's program check holds, in a job or not, each since the first reading
    /// that saw it held, by pid; a lower bound (only aterm's user is read).
    pub program_check: Vec<Named>,
    /// Of those, the ones aterm started: how many, and the longest held, ms.
    pub program_check_aterm: (u32, u64),
    /// Of aterm's, how many forked at or after [`JobTick::grant_ms`] where no adopted tab could
    /// hold them (in a tab no reading named adopted, which stays so after it closes, or in none
    /// while no reading named a tab adopted). One means the grant did not take; one started
    /// before it may still wait.
    pub program_check_after_grant: u32,
    /// Of [`Findings::program_check`], the ones stranded ([`HeldProc::stranded`]), each as its
    /// row there; empty while [`Findings::program_check_daemon`] is `None`.
    pub program_check_stranded: Vec<Named>,
    /// The syspolicyd they were judged against ([`JobTick::program_check_daemon`]).
    pub program_check_daemon: Option<u32>,
    /// Whether every held row was judged against it ([`JobTick::program_check_judged`]), so
    /// none but those named is stranded.
    pub program_check_judged: bool,
    /// The backed-up episode this reading is in.
    pub program_check_episode: Option<HeldEpisode>,
    /// The episode that closed at this reading (set on that one view only).
    pub program_check_drained: Option<HeldEpisode>,
    /// Own x86_64 processes on an arm64 Mac whose one thread never ran past 20 s, each since the
    /// first reading that saw it so ([`StartWait::Translating`]).
    pub translating: Vec<Named>,
    /// Own processes with the same signature whose executable is not proven on a local volume,
    /// each since the first reading that saw it so ([`StartWait::NotStarted`]).
    pub not_started: Vec<Named>,
    /// Own processes with the same signature that read stopped or traced, each since the first
    /// reading that saw it so, with what this reading read ([`StartWait::StoppedUnstarted`]):
    /// never counted as held. One that reads stopped is in [`Self::stopped`] too, which lists
    /// every stopped process whether or not it ran.
    pub stopped_unstarted: Vec<StoppedUnstarted>,
    /// Inside a backed-up episode, the job members that started but whose CPU did not move since
    /// the reading before while they hold a fresh dylib open: probably waiting for the same
    /// check, a heuristic (§5.1's dylib case). `for_ms` is 0.
    pub probably_program_check: Vec<Named>,
    /// Own-uid processes, zombies included.
    pub own_procs: u64,
    /// The per-user limit.
    pub process_limit: Option<u64>,
}

/// How soon the host should sample jobs again (§11.3 "Demand").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Demand {
    /// Nothing to watch: only waits for the person, or no job.
    #[default]
    Idle,
    /// Only legitimate waits: every 30 s.
    Slow,
    /// A job runs or can accrue suspicion: every 2 s.
    Fast,
}

impl Demand {
    /// The interval, ms; `None` when idle.
    #[must_use]
    pub const fn every_ms(self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Slow => Some(30_000),
            Self::Fast => Some(2_000),
        }
    }
}

/// What one tick says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobsView {
    /// Every job, by tab then id.
    pub rows: Vec<JobRow>,
    /// The findings.
    pub findings: Findings,
    /// How soon to look again.
    pub demand: Demand,
    /// Lanes that are off, as `lane_off:<name>` words for `sys jobs --json`.
    pub lanes_off: Vec<&'static str>,
    /// The tabs, ascending, whose every live job waits at the person's own terminal read this
    /// tick and none is watched at the fast cadence (stuck, or on a suspect wait).
    pub parked_tabs: Vec<u64>,
    /// The pass's clock ([`JobTick::at_ms`]): every figure of the view is as of it, however
    /// long a snapshot carries the view after it.
    pub at_ms: u64,
}

impl JobsView {
    /// The row with this id.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&JobRow> {
        self.rows
            .iter()
            .filter(|r| r.id == id)
            .min_by_key(|r| r.verdict == Verdict::Exited)
    }

    /// The row with this id in tab `tab`.
    #[must_use]
    pub fn row_in(&self, tab: u64, id: &str) -> Option<&JobRow> {
        self.rows.iter().find(|r| r.tab == tab && r.id == id)
    }
}

/// The command class of a command line.
#[must_use]
pub fn command_class(command: &str, kind: JobKind) -> String {
    let word = command
        .split_whitespace()
        .find(|w| !is_assignment(w))
        .unwrap_or("");
    let base = word.rsplit('/').next().unwrap_or(word);
    let mut end = base.len().min(32);
    while !base.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} {}", &base[..end], kind.word())
}

fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
                && !name.as_bytes()[0].is_ascii_digit()
        }
        None => false,
    }
}

/// Learned μ per command class.
#[derive(Clone, Debug, Default)]
pub struct ClassTable {
    rows: HashMap<String, (u64, VecDeque<u64>)>,
    clock: u64,
}

impl ClassTable {
    /// Teach `class` one completed job's median interval.
    pub fn learn(&mut self, class: &str, median_ms: u64) {
        self.clock += 1;
        let clock = self.clock;
        let row = self
            .rows
            .entry(class.to_owned())
            .or_insert_with(|| (clock, VecDeque::new()));
        row.0 = clock;
        row.1.push_back(median_ms);
        while row.1.len() > CLASS_RING {
            row.1.pop_front();
        }
        while self.rows.len() > CLASS_TABLE {
            let Some(oldest) = self
                .rows
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.rows.remove(&oldest);
        }
    }

    /// The class's μ once it has [`OWN_MIN`] medians.
    #[must_use]
    pub fn mu(&self, class: &str) -> Option<u64> {
        let (_, ring) = self.rows.get(class)?;
        (ring.len() >= OWN_MIN).then(|| median(ring.iter().copied()))
    }

    /// Classes held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether nothing has been learned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The median of `values` (the lower middle for an even count); 0 for none.
#[must_use]
pub fn median(values: impl IntoIterator<Item = u64>) -> u64 {
    let mut v: Vec<u64> = values.into_iter().collect();
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[(v.len() - 1) / 2]
}

/// φ × 1000 for `quiet_ms` against `mu_ms`: `t / (μ · ln 10)`, the exponential phi-accrual form.
#[must_use]
pub fn phi_milli(quiet_ms: u64, mu_ms: u64) -> u64 {
    let den = u128::from(mu_ms.max(1)) * 2_302_585;
    let num = u128::from(quiet_ms) * 1_000_000_000;
    u64::try_from(num / den).unwrap_or(u64::MAX)
}

/// The members a job keeps, by pid.
pub(crate) type MemberMap = BTreeMap<u32, units::Member>;
