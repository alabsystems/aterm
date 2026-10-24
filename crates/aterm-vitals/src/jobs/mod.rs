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
pub use units::Jobs;
pub use wait::{TierB, map_leaf_frame};

/// How long a job reads RUNNING after a progress event.
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
    /// Its short name.
    pub name: String,
    /// Its state.
    pub state: ProcState,
    /// Whether it leads its session; `None` where unread.
    pub session_leader: Option<bool>,
    /// Whether it has a controlling terminal; `None` where unread.
    pub has_tty: Option<bool>,
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
    },
    /// A character device.
    Device {
        /// A terminal.
        tty: bool,
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
    /// Its user.
    pub uid: Option<u32>,
    /// Its short name (`p_comm`, `comm`).
    pub name: String,
    /// Its executable's basename: the name rows print, `name` where `None`.
    pub exe: Option<String>,
    /// Its state.
    pub state: ProcState,
    /// Its controlling terminal; `None` without one.
    pub tty: Option<u32>,
    /// That terminal's foreground process group.
    pub tpgid: Option<u32>,
    /// Its counters; `None` where refused.
    pub counters: Option<JobCounters>,
    /// Its threads; `None` where refused.
    pub threads: Option<Vec<ThreadState>>,
    /// The kernel function it waits in (Linux).
    pub wchan: Option<String>,
    /// Its descriptors, when asked for; `None` when not asked or refused.
    pub fds: Option<Vec<Fd>>,
    /// Whether macOS's program check holds it before its first instruction (`aterm_sysprobe::jobs::AtStart::Held`).
    pub program_check: bool,
}

/// One process of aterm's user that macOS's program check holds, member of a job or not, as the host read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldProc {
    /// Its pid.
    pub pid: u32,
    /// Which program the pid runs: its pid version where read (it moves on exec), else its start. A wrapper that execs its held tool starts a new held time.
    pub exec: u64,
    /// Its executable's basename, else its short name.
    pub name: String,
    /// Whether aterm started it (a tab's member, or aterm's resource coalition): words only.
    pub aterm: bool,
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

/// What the agent-transcript join (P2b) learned about one detached call.
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
    /// PTY bytes read so far (cumulative).
    pub output_bytes: u64,
    /// When the program last consumed typed input, host ms.
    pub input_consumed_ms: Option<u64>,
    /// The transcript join's hints for detached calls.
    pub calls: Vec<CallHint>,
    /// Tier B namings made on request.
    pub named: Vec<NamedWait>,
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
    /// aterm's own user (for the own-uid findings).
    pub self_uid: Option<u32>,
    /// The cheap sweep of every process.
    pub sweep: Vec<SweepProc>,
    /// The detailed readings of what [`Jobs::plan`] asked for (a process gone or refused meanwhile is simply absent).
    pub procs: Vec<JobProc>,
    /// Every tab.
    pub tabs: Vec<JobTab>,
    /// The machine.
    pub machine: Machine,
    /// The reader's lanes.
    pub lanes: LanesSeen,
    /// The per-user process limit (`kern.maxprocperuid`, `RLIMIT_NPROC`).
    pub process_limit: Option<u64>,
    /// aterm's user's processes macOS's program check holds now.
    pub held: Vec<HeldProc>,
}

/// One process the host should read in detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Want {
    /// Its pid.
    pub pid: u32,
    /// Whether its descriptors are needed.
    pub fds: bool,
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
    /// Tier B: a plain `read` (a canonical terminal read).
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
    /// Its name.
    pub name: String,
    /// The tab, when it is in one.
    pub tab: Option<u64>,
    /// How long, ms (D-state, busy orphans, and held since the first reading that saw it held).
    pub for_ms: u64,
}

/// A stretch of readings in which macOS's program check is backed up (`docs/DESIGN-leftover-processes-2026-10-16.md` §5.1): it opens at the first reading where one of aterm's user's programs was held at the reading before too, or three are held at once, and closes at the second reading in a row that finds none held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeldEpisode {
    /// The reading it opened at, host ms.
    pub opened_ms: u64,
    /// From its opening to its last reading that found a program held (this one, while one is held), ms.
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
    /// Own processes macOS's program check holds, members of a job or not, each held since the first reading that saw it held, by pid; a lower bound (only aterm's user is read).
    pub program_check: Vec<Named>,
    /// Of those, the ones aterm started: how many, and the longest held, ms.
    pub program_check_aterm: (u32, u64),
    /// The backed-up episode this reading is in.
    pub program_check_episode: Option<HeldEpisode>,
    /// The episode that closed at this reading (set on that one view only).
    pub program_check_drained: Option<HeldEpisode>,
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
    /// The tabs PARKED ON THE PERSON'S READ this tick, ascending.
    pub parked_tabs: Vec<u64>,
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

/// φ × 1000 for `quiet_ms` against `mu_ms`: `t /`, the exponential phi-accrual form.
#[must_use]
pub fn phi_milli(quiet_ms: u64, mu_ms: u64) -> u64 {
    let den = u128::from(mu_ms.max(1)) * 2_302_585;
    let num = u128::from(quiet_ms) * 1_000_000_000;
    u64::try_from(num / den).unwrap_or(u64::MAX)
}

/// The members a job keeps, by pid.
pub(crate) type MemberMap = BTreeMap<u32, units::Member>;
