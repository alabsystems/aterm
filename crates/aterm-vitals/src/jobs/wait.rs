// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier A kernel-fact wait classification and Tier B sampled leaf-frame mapping.

use std::collections::HashMap;
use std::net::SocketAddr;

use super::{
    Fd, FdKind, JobProc, Knote, KnoteFilter, LanesSeen, Standing, TcpState, TtyMode, Wait, WaitName,
};

/// Darwin `PIPE_WANTR`: a reader sleeps on this end.
pub const PIPE_WANTR: u32 = 0x8;
/// Darwin `PIPE_WANTW`: a writer sleeps on this end's buffer.
pub const PIPE_WANTW: u32 = 0x10;
/// A pipe buffer this full blocks its writer.
pub const PIPE_FULL_BYTES: u64 = 65_536;
/// How deep a pipe's peer is followed.
const PEER_DEPTH: usize = 8;

/// A Tier B word: what a sampled stack's leaf frame names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TierB {
    /// `__semwait_signal`, `nanosleep`: a timer.
    Timer,
    /// `flock`, `fcntl`: a file lock (a holder search follows).
    Lock,
    /// `read`, `readv`: a plain read.
    Read,
    /// `select`, `poll`, `kevent`: an event wait, cross-checked with Tier A.
    Event,
    /// `mach_msg*`: a Mach message.
    Ipc,
    /// `__psynch_mutexwait`, `__psynch_cvwait`, `__ulock_wait`: a mutex.
    Mutex,
    /// `__sigsuspend`, `wait4`: waits on its children.
    Children,
    /// Anything else (the report file is offered to the person).
    Unparsed,
}

impl TierB {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        self.wait_name().word()
    }

    /// The wait it names.
    #[must_use]
    pub const fn wait_name(self) -> WaitName {
        match self {
            Self::Timer => WaitName::Timer,
            Self::Lock => WaitName::Lock,
            Self::Read => WaitName::Read,
            Self::Event => WaitName::EventLoop,
            Self::Ipc => WaitName::Ipc,
            Self::Mutex => WaitName::Mutex,
            Self::Children => WaitName::Children,
            Self::Unparsed => WaitName::NamedUnparsed,
        }
    }
}

/// Maps one leaf frame of a waiting thread to its [`TierB`] classification.
#[must_use]
pub fn map_leaf_frame(frame: &str) -> TierB {
    let sym = frame.split_whitespace().next().unwrap_or("");
    let sym = sym.split('$').next().unwrap_or(sym);
    let sym = sym.strip_suffix("_nocancel").unwrap_or(sym);
    let sym = sym.trim_start_matches('_');
    match sym {
        "semwait_signal" | "nanosleep" | "usleep" | "sleep" => TierB::Timer,
        "flock" | "fcntl" => TierB::Lock,
        "read" | "readv" | "pread" => TierB::Read,
        "select" | "pselect" | "poll" | "kevent" | "kevent64" | "kevent_qos" | "kevent_id" => {
            TierB::Event
        }
        "psynch_mutexwait" | "psynch_cvwait" | "psynch_rw_rdlock" | "psynch_rw_wrlock"
        | "ulock_wait" | "ulock_wait2" => TierB::Mutex,
        "sigsuspend" | "wait4" | "waitpid" => TierB::Children,
        s if s.starts_with("mach_msg") => TierB::Ipc,
        _ => TierB::Unparsed,
    }
}

/// The table order of the Tier A names: a lower rank wins.
#[must_use]
pub const fn rank(name: WaitName) -> u8 {
    match name {
        WaitName::ProgramCheck => 0,
        WaitName::TtyPassword => 1,
        WaitName::TtyBuiltinRead => 2,
        WaitName::TtyRaw => 3,
        WaitName::Serves => 4,
        WaitName::Pipe => 5,
        WaitName::PipeFull => 6,
        WaitName::Tcp => 7,
        WaitName::Follow => 8,
        WaitName::Listen => 9,
        WaitName::Timer => 10,
        WaitName::EventLoop => 11,
        WaitName::Kernel => 12,
        _ => 13,
    }
}

/// Returns how a Tier A wait name counts before following a pipe's peer.
#[must_use]
pub const fn standing_of(name: WaitName) -> Standing {
    match name {
        WaitName::ProgramCheck
        | WaitName::TtyPassword
        | WaitName::TtyBuiltinRead
        | WaitName::TtyRaw
        | WaitName::Serves
        | WaitName::Timer
        | WaitName::Follow
        | WaitName::Listen => Standing::Legit,
        WaitName::Tcp | WaitName::Kernel | WaitName::Ipc | WaitName::Mutex | WaitName::Lock => {
            Standing::Capable
        }
        _ => Standing::Capped,
    }
}

/// Process table, lane capabilities, and wait ages used for Tier A classification.
pub struct Scene<'a> {
    /// The detailed readings, by pid.
    pub procs: &'a HashMap<u32, &'a JobProc>,
    /// The reader's lanes.
    pub lanes: LanesSeen,
    /// Milliseconds each pid has been in an uninterruptible wait.
    pub d_age: &'a HashMap<u32, u64>,
    /// How a process outside this job counts (its job's standing).
    pub elsewhere: &'a HashMap<u32, Standing>,
    /// Whether macOS program-check holds are reported as [`WaitName::ProgramCheck`].
    pub program_check: bool,
}

/// The job-level facts the tty names read.
#[derive(Clone, Copy, Debug, Default)]
pub struct JobFacts {
    /// The job holds the terminal's foreground.
    pub holds_fg: bool,
    /// The job is a shell itself reading a line (its own `read` builtin, a nested session's shell
    /// at its prompt), taken while no leaf names a wait of its own other than the reader's own
    /// event loop, where that can hold its terminal.
    pub builtin: bool,
    /// The process reading that line, the job's leader: the tab's shell for its block, or the
    /// nested session's leader.
    pub reader: u32,
    /// The terminal mode word, only when stamped after the job's last output.
    pub tty: Option<TtyMode>,
    /// No typed input is queued.
    pub queue_empty: bool,
    /// The detached job's consumer (its leader's parent).
    pub consumer: Option<u32>,
}

/// Classifies the wait of a job from its waiting `leaves` and `shell`.
#[must_use]
pub fn name_job(
    scene: &Scene<'_>,
    job: &JobFacts,
    leaves: &[(u32, String)],
    shell: (u32, &str),
) -> Wait {
    let first = leaves
        .first()
        .map_or((shell.0, shell.1.to_owned()), |(p, n)| (*p, n.clone()));
    let mut best: Option<Wait> = None;
    let mut own_wait = false;
    for (pid, name) in leaves {
        let Some(w) = leaf_wait(scene, job.consumer, *pid, name, 0) else {
            continue;
        };
        own_wait |=
            !(*pid == job.reader && w.name == WaitName::EventLoop && reads_terminal(scene, *pid));
        if best.as_ref().is_none_or(|b| rank(w.name) < rank(b.name)) {
            best = Some(w);
        }
    }
    if let Some(w) = best.as_ref().filter(|w| w.name == WaitName::ProgramCheck) {
        // A held leaf is not reading the terminal, whatever mode its program left it in.
        return w.clone();
    }
    // A shell's line read, while no leaf names a wait of its own (a REPL blocked on its socket,
    // a `cd` into a dead share, a background job hung on its peer): the reader's own event loop
    // may be the read itself (a REPL's loop with the terminal in its set, a timed `read` in
    // `select`), one that lists no read of a terminal (asyncio's, on its socket) is not.
    if job.builtin && !own_wait {
        return Wait::bare(WaitName::TtyBuiltinRead, Standing::Legit, shell.0, shell.1);
    }
    if job.holds_fg
        && job.queue_empty
        && let Some(t) = job.tty
    {
        if t.canonical && !t.echo {
            return Wait::bare(WaitName::TtyPassword, Standing::Legit, first.0, &first.1);
        }
        if !t.canonical {
            return Wait::bare(WaitName::TtyRaw, Standing::Legit, first.0, &first.1);
        }
    }
    best.unwrap_or_else(|| Wait::bare(WaitName::Unnamed, Standing::Capped, first.0, &first.1))
}

/// The wait `pid` names of its own, as a leaf of a job whose consumer is `consumer`.
#[must_use]
pub fn wait_of(scene: &Scene<'_>, consumer: Option<u32>, pid: u32, name: &str) -> Option<Wait> {
    leaf_wait(scene, consumer, pid, name, 0)
}

/// Whether `pid` names no wait of its own but its event loop over its terminal (a REPL's loop with
/// the terminal in its set), as a line read's reader ([`name_job`]'s builtin read).
#[must_use]
pub fn reads_line(scene: &Scene<'_>, consumer: Option<u32>, pid: u32, name: &str) -> bool {
    leaf_wait(scene, consumer, pid, name, 0)
        .is_none_or(|w| w.name == WaitName::EventLoop && reads_terminal(scene, pid))
}

/// Whether `pid`'s sleeping event queues may hold its terminal: one lists a read of a terminal
/// it holds, or one lists no entries (macOS without lane L6), or none sleeps on one (Linux's
/// `select`, by its wait channel).
fn reads_terminal(scene: &Scene<'_>, pid: u32) -> bool {
    let Some(p) = scene.procs.get(&pid) else {
        return false;
    };
    let fds: &[Fd] = p.fds.as_deref().unwrap_or(&[]);
    let mut queues = fds
        .iter()
        .filter_map(|f| match &f.kind {
            FdKind::EventQueue {
                sleeping: Some(true),
                knotes,
            } => Some(knotes),
            _ => None,
        })
        .peekable();
    queues.peek().is_none()
        || queues.any(|knotes| {
            knotes.as_ref().is_none_or(|k| {
                k.iter().any(|k| {
                    k.filter == KnoteFilter::Read
                        && fd_is(fds, k.ident, |f| {
                            matches!(f, FdKind::Device { tty: true, .. })
                        })
                })
            })
        })
}

fn leaf_wait(
    scene: &Scene<'_>,
    consumer: Option<u32>,
    pid: u32,
    name: &str,
    depth: usize,
) -> Option<Wait> {
    let p = scene.procs.get(&pid)?;
    let wchan = p.wchan.as_deref().unwrap_or("");
    let fds: &[Fd] = p.fds.as_deref().unwrap_or(&[]);
    let mk = |n: WaitName| Wait::bare(n, standing_of(n), pid, name);
    if scene.program_check && p.program_check {
        return Some(mk(WaitName::ProgramCheck));
    }

    // Linux names no pipe's waiter: the wait channel says the process sleeps on a pipe, and the
    // descriptor its read or write names, where read, which one.
    let named = |f: &Fd| p.blocked_fd.is_none_or(|b| b == f.fd);
    let linux_reading =
        wchan.starts_with("pipe_read") || wchan == "pipe_wait" || wchan == "wait_for_partner";
    for f in fds {
        let FdKind::Pipe {
            handle,
            peer,
            status,
            write_end,
            ..
        } = &f.kind
        else {
            continue;
        };
        if *write_end == Some(true) {
            continue;
        }
        let flagged = scene.lanes.pipe_flags && status.is_some_and(|s| s & PIPE_WANTR != 0);
        if !(flagged || (linux_reading && named(f))) {
            continue;
        }
        let writer = holder(scene, *peer, *handle, true, pid);
        if writer.is_some() && writer == consumer {
            let mut w = mk(WaitName::Serves);
            w.peer = writer;
            return Some(w);
        }
        let mut w = mk(WaitName::Pipe);
        w.peer = writer;
        w.standing = peer_standing(scene, consumer, writer, depth);
        return Some(w);
    }
    if wchan == "wait_for_partner" {
        return Some(mk(WaitName::Pipe));
    }

    let linux_writing = wchan.starts_with("pipe_write") || wchan == "pipe_wait_writable";
    for f in fds {
        let FdKind::Pipe {
            handle,
            peer,
            write_end,
            ..
        } = &f.kind
        else {
            continue;
        };
        if *write_end == Some(false) {
            continue;
        }
        let reader = holder(scene, *peer, *handle, false, pid);
        let full = scene.lanes.pipe_flags && reader_end_full(scene, *peer, *handle, pid);
        if !(full || (linux_writing && *write_end == Some(true) && named(f))) {
            continue;
        }
        let mut w = mk(WaitName::PipeFull);
        w.peer = reader;
        w.standing = peer_standing(scene, consumer, reader, depth);
        return Some(w);
    }

    for f in fds {
        let FdKind::EventQueue {
            sleeping: Some(true),
            knotes,
        } = &f.kind
        else {
            continue;
        };
        let Some(knotes) = knotes else {
            return Some(mk(WaitName::EventLoop));
        };
        if let Some(k) = knotes
            .iter()
            .find(|k| k.filter == KnoteFilter::Read && fd_is(fds, k.ident, is_followed_file))
        {
            let mut w = mk(WaitName::Follow);
            w.fd = Some(k.ident);
            return Some(w);
        }
        if let Some(port) = knotes
            .iter()
            .filter(|k| k.filter == KnoteFilter::Read)
            .find_map(|k| listen_port(fds, k.ident))
        {
            let mut w = mk(WaitName::Listen);
            w.port = Some(port);
            return Some(w);
        }
        if let Some(c) = queue_on_connections(fds, knotes) {
            let mut w = mk(WaitName::Tcp);
            w.tcp = Some(c);
            return Some(w);
        }
        if !knotes.is_empty() && knotes.iter().all(|k| k.filter == KnoteFilter::Timer) {
            return Some(mk(WaitName::Timer));
        }
        return Some(mk(WaitName::EventLoop));
    }

    if let Some(port) = fds.iter().find_map(|f| match &f.kind {
        FdKind::Tcp {
            state: TcpState::Listen,
            local_port,
            ..
        } => Some(*local_port),
        _ => None,
    }) {
        let mut w = mk(WaitName::Listen);
        w.port = Some(port);
        return Some(w);
    }
    if let Some(c) = fds.iter().find_map(idle_connection) {
        let mut w = mk(WaitName::Tcp);
        w.tcp = Some(c);
        return Some(w);
    }
    if wchan.contains("inotify") {
        return Some(mk(WaitName::Follow));
    }
    if is_sleep_wchan(wchan) {
        return Some(mk(WaitName::Timer));
    }
    if is_event_wchan(wchan) {
        return Some(mk(WaitName::EventLoop));
    }

    if scene
        .d_age
        .get(&pid)
        .is_some_and(|&ms| ms >= super::KERNEL_WAIT_MS)
    {
        return Some(mk(WaitName::Kernel));
    }
    None
}

fn holder(scene: &Scene<'_>, peer: u64, this: u64, want_write: bool, not: u32) -> Option<u32> {
    let mut found: Option<u32> = None;
    for (pid, p) in scene.procs {
        for f in p.fds.as_deref().unwrap_or(&[]) {
            let FdKind::Pipe {
                handle, write_end, ..
            } = &f.kind
            else {
                continue;
            };
            if *handle != peer {
                continue;
            }
            let shared = peer == this;
            if shared && *write_end != Some(want_write) {
                continue;
            }
            if *pid == not && shared {
                continue;
            }
            if found.is_none_or(|f| *pid < f) {
                found = Some(*pid);
            }
        }
    }
    found
}

fn reader_end_full(scene: &Scene<'_>, peer: u64, this: u64, _not: u32) -> bool {
    scene.procs.values().any(|p| {
        p.fds.as_deref().unwrap_or(&[]).iter().any(|f| {
            matches!(
                &f.kind,
                FdKind::Pipe { handle, peer: back, status: Some(s), queued: Some(q), .. }
                    if *handle == peer && *back == this && s & PIPE_WANTW != 0 && *q >= PIPE_FULL_BYTES
            )
        })
    })
}

fn peer_standing(
    scene: &Scene<'_>,
    consumer: Option<u32>,
    peer: Option<u32>,
    depth: usize,
) -> Standing {
    let Some(peer) = peer else {
        return Standing::Capped;
    };
    if let Some(s) = scene.elsewhere.get(&peer) {
        return *s;
    }
    if depth >= PEER_DEPTH {
        return Standing::Capped;
    }
    let name = scene.procs.get(&peer).map_or("", |p| p.name.as_str());
    leaf_wait(scene, consumer, peer, name, depth + 1).map_or(Standing::Capped, |w| w.standing)
}

fn fd_is(fds: &[Fd], ident: u64, pred: fn(&FdKind) -> bool) -> bool {
    fds.iter()
        .any(|f| u64::try_from(f.fd).is_ok_and(|n| n == ident) && pred(&f.kind))
}

fn is_followed_file(k: &FdKind) -> bool {
    matches!(k, FdKind::File { regular: true, .. } | FdKind::Watch)
}

fn listen_port(fds: &[Fd], ident: u64) -> Option<u16> {
    fds.iter().find_map(|f| match &f.kind {
        FdKind::Tcp {
            state: TcpState::Listen,
            local_port,
            ..
        } if u64::try_from(f.fd).is_ok_and(|n| n == ident) => Some(*local_port),
        _ => None,
    })
}

fn idle_connection(f: &Fd) -> Option<(Option<SocketAddr>, TcpState, u64)> {
    match &f.kind {
        FdKind::Tcp {
            state: state @ (TcpState::Established | TcpState::SynSent),
            remote,
            queued_read: Some(0),
            ..
        } => Some((*remote, *state, 0)),
        _ => None,
    }
}

fn queue_on_connections(
    fds: &[Fd],
    knotes: &[Knote],
) -> Option<(Option<SocketAddr>, TcpState, u64)> {
    let mut first = None;
    for k in knotes {
        if !matches!(k.filter, KnoteFilter::Read | KnoteFilter::Write) {
            return None;
        }
        let c = fds
            .iter()
            .filter(|f| u64::try_from(f.fd).is_ok_and(|n| n == k.ident))
            .find_map(idle_connection)?;
        first.get_or_insert(c);
    }
    first
}

/// A Linux `wchan` that is a sleep (`nanosleep`, `clock_nanosleep`).
#[must_use]
pub fn is_sleep_wchan(w: &str) -> bool {
    matches!(
        w,
        "hrtimer_nanosleep" | "do_nanosleep" | "clock_nanosleep" | "common_nsleep" | "msleep"
    )
}

/// A Linux `wchan` that is an event wait (`epoll_wait`, `poll`, `select`).
#[must_use]
pub fn is_event_wchan(w: &str) -> bool {
    matches!(
        w,
        "ep_poll"
            | "do_epoll_wait"
            | "do_select"
            | "do_sys_poll"
            | "poll_schedule_timeout"
            | "core_sys_select"
            | "do_poll"
    ) || w.starts_with("ep_poll")
}
