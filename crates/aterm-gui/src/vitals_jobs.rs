// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VITALS HOST'S JOBS PASS: the stuck-process engine (`aterm_vitals::jobs`, design §11) fed on the probe thread, so the stats bars' marks (⊘ ◔ ☠ ◇ and the drawn pause) and the lens's Jobs well show the verdicts the engine reaches.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::supervise::{BackgroundCall, BackgroundEvidence};

use aterm_sysprobe::jobs::{self as probe, AtStart, JobsReader, ProcFacts, ReadReq};
use aterm_sysprobe::procs::ProcessReading;
use aterm_sysprobe::volumes::Table;
use aterm_vitals::jobs::{
    Class, Consumer, Demand, Fd, FdKind, HeldProc, JobCounters, JobKind, JobProc, JobRow, JobTab,
    JobTick, Jobs, JobsView, Knote, KnoteFilter, LanesSeen, Machine, ProcState, Standing,
    SweepProc, TcpState, ThreadState, TtyMode,
};
use aterm_vitals::model::TabRoot;

/// The engine's fast cadence (§11.3): also the floor between passes, so a
/// 2 Hz lens does not double the detailed reads.
const FAST_MS: u64 = 2_000;

/// The most CPU a process outside every job may have used for the pass to read its start: a held one has about 2 ms, the fork child's work before its exec (leftover M4), so its counters cannot tell it from an idle one and only the thread call decides.
pub(crate) const HELD_CPU_NS: u64 = 20_000_000;

/// The readers the pass asks of this user's processes outside every job, and the held stand-in it reads (`KERNEL` live, `INERT` in unit tests): none reads a process's arguments or environment, which can hang on a program held mid-start (leftover M29).
#[derive(Clone, Copy)]
pub(crate) struct HeldReaders {
    /// Where a process stands at its start, one mount table read for the pass ([`probe::read_start_in`], [`probe::judge_start`]).
    pub(crate) start: fn(u32, &OnceCell<Table>) -> Option<AtStart>,
    /// Its pid version, which moves on exec ([`aterm_uds::process::read_identity`]).
    pub(crate) exec: fn(u32) -> Option<u64>,
    /// Its resource coalition ([`aterm_sysprobe::coalitions::coalition_of`]).
    pub(crate) coalition: fn(u32) -> Option<u64>,
    /// Its executable's file name, the basename of [`aterm_sysprobe::process_path`] (`p_comm` cuts a name at 16 bytes).
    pub(crate) exe_name: fn(u32) -> Option<String>,
    /// The pid a development build stands in as held ([`crate::program_check_watch::held_stand_in`]).
    pub(crate) stand_in: fn() -> Option<u32>,
}

impl HeldReaders {
    /// The kernel's readers.
    pub(crate) const KERNEL: Self = Self {
        start: |pid, mounts| {
            probe::read_start_in(pid, probe::now_us(), mounts).map(|f| probe::judge_start(&f))
        },
        exec: |pid| {
            i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_identity)
                .and_then(|i| i.idversion)
                .and_then(|v| u64::try_from(v).ok())
        },
        coalition: aterm_sysprobe::coalitions::coalition_of,
        exe_name: |pid| {
            aterm_sysprobe::process_path(pid)?
                .file_name()?
                .to_str()
                .map(str::to_owned)
        },
        stand_in: crate::program_check_watch::held_stand_in,
    };

    /// The inert arm: nothing outside a job is read, and nothing stands in.
    pub(crate) const INERT: Self = Self {
        start: |_, _| None,
        exec: |_| None,
        coalition: |_| None,
        exe_name: |_| None,
        stand_in: || None,
    };
}

/// The engine, its reader and the last view, on the probe thread.
pub(crate) struct JobsPass {
    jobs: Jobs,
    reader: JobsReader,
    held: HeldReaders,
    process_limit: Option<u64>,
    /// When the next pass is due, host ms (`None`: at the next reading).
    next_ms: Option<u64>,
    view: Option<Arc<JobsView>>,
}

impl JobsPass {
    pub(crate) fn new() -> Self {
        Self {
            jobs: Jobs::new(),
            reader: JobsReader::new(),
            held: if cfg!(test) {
                HeldReaders::INERT
            } else {
                HeldReaders::KERNEL
            },
            process_limit: probe::process_limit(),
            next_ms: None,
            view: None,
        }
    }

    /// One reading's pass: run the engine when it is due, or whenever `now` (a reading an edge asked: the loop paces those itself, on its own clock, and one that lands a little inside this pass's cadence must still look, or the job it was asked for goes unseen until the next edge), and return the view to publish (the last one when it is not due).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tick(
        &mut self,
        at_ms: u64,
        sweep: Option<&[ProcessReading]>,
        tabs: &[TabRoot],
        ttys: &[(u64, TtyMode)],
        self_pid: u32,
        thrashing: bool,
        now: bool,
    ) -> Option<Arc<JobsView>> {
        let Some(sweep) = sweep else {
            return self.view.clone();
        };
        if !now && self.next_ms.is_some_and(|due| at_ms < due) {
            return self.view.clone();
        }
        let self_uid = sweep.iter().find(|p| p.pid == self_pid).and_then(|p| p.uid);
        let readings = sweep;
        let sweep: Vec<SweepProc> = sweep.iter().map(sweep_proc).collect();
        let tabs = job_tabs(tabs, ttys);
        let reqs: Vec<ReadReq> = self
            .jobs
            .plan(&sweep, &tabs)
            .iter()
            .map(|w| ReadReq {
                pid: w.pid,
                fds: w.fds,
            })
            .collect();
        let readers = self.held;
        let stand_in = (readers.stand_in)();
        let procs: Vec<JobProc> = self
            .reader
            .read_all(&reqs)
            .iter()
            .map(|p| job_proc(p, stand_in))
            .collect();
        let mounts = OnceCell::new();
        let ours = OnceCell::new();
        let held = held_list(
            readings,
            &procs,
            self_uid,
            |pid| {
                if stand_in == Some(pid) {
                    Some(AtStart::Held)
                } else {
                    (readers.start)(pid, &mounts)
                }
            },
            readers.exec,
            |pid| {
                let ours = *ours.get_or_init(|| (readers.coalition)(self_pid));
                ours.is_some() && (readers.coalition)(pid) == ours
            },
            readers.exe_name,
        );
        let lanes = self.reader.lanes();
        let tick = JobTick {
            at_ms,
            self_uid,
            sweep,
            procs,
            tabs,
            machine: Machine {
                stretch_milli: crate::input_stall::machine_contention().factor_milli(),
                thrashing,
            },
            lanes: LanesSeen {
                kqueue_detail: lanes.kqueue_detail.is_on(),
                pipe_flags: lanes.pipe_flags.is_on(),
            },
            process_limit: self.process_limit,
            held,
        };
        let view = self.jobs.absorb(&tick);
        self.next_ms = Some(at_ms + next_pass_ms(view.demand));
        self.view = Some(Arc::new(view));
        self.view.clone()
    }
}

/// THIS USER'S PROGRAMS HELD AT MACOS'S PROGRAM CHECK, members of a job or not (leftover §5.1): a member of `members` (the pass's detailed readings, each judged from its own thread list) where it reads held; every other row of `sweep` of `self_uid` that is no zombie and has used at most [`HELD_CPU_NS`], where `start` reads it held. Each is keyed by its pid version (`exec`, else its start, so an exec starts a new held time), named by its executable (a member's own reading, else `exe_name`, else its short name) and is aterm's when it is a member or `in_aterm` says so (aterm's coalition: words only). `start` is asked of nothing else, `exe_name` of the held rows outside every job alone.
pub(crate) fn held_list(
    sweep: &[ProcessReading],
    members: &[JobProc],
    self_uid: Option<u32>,
    mut start: impl FnMut(u32) -> Option<AtStart>,
    exec: impl Fn(u32) -> Option<u64>,
    in_aterm: impl Fn(u32) -> bool,
    exe_name: impl Fn(u32) -> Option<String>,
) -> Vec<HeldProc> {
    let Some(uid) = self_uid else {
        return Vec::new();
    };
    let read: BTreeSet<u32> = members.iter().map(|m| m.pid).collect();
    let mut held: Vec<HeldProc> = members
        .iter()
        .filter(|m| m.program_check && m.uid == Some(uid))
        .map(|m| HeldProc {
            pid: m.pid,
            exec: exec(m.pid).or(m.start).unwrap_or(0),
            name: m.exe.clone().unwrap_or_else(|| m.name.clone()),
            aterm: true,
        })
        .collect();
    for p in sweep.iter().filter(|p| {
        p.uid == Some(uid)
            && p.state != probe::RunState::Zombie
            && p.cpu_ns.is_some_and(|c| c <= HELD_CPU_NS)
            && !read.contains(&p.pid)
    }) {
        if start(p.pid) == Some(AtStart::Held) {
            held.push(HeldProc {
                pid: p.pid,
                exec: exec(p.pid).or(p.start).unwrap_or(0),
                name: exe_name(p.pid).unwrap_or_else(|| p.name.clone()),
                aterm: in_aterm(p.pid),
            });
        }
    }
    held
}

/// How old the board's view may be and still speak for the canary ([`JobsBoard::evidence`]).
pub(crate) const JOBS_FRESH: Duration = Duration::from_secs(60);

/// WHETHER A JOBS VIEW `age` OLD SPEAKS FOR NOW: one at most [`JOBS_FRESH`] old does; so does a PARKED one (its demand idle: every live job waits only for the person, or none is live) of any age while its reader runs (`reader`), because a parked engine is read again only on an edge (output that moves a screen, a parked screen's redraw at most once a floor, a session's fork, `exec` or exit the kernel watch hears, the disk watch's look; design §6.1), so no reading since says nothing changed that drew or that the kernel told of.
pub(crate) fn speaks_for_now(view: &JobsView, age: Duration, reader: bool) -> bool {
    age <= JOBS_FRESH || (reader && view.demand == Demand::Idle)
}

/// The view last posted, its tabs' session ids, and when it was first posted.
struct Posted {
    view: Arc<JobsView>,
    sids: BTreeMap<u64, String>,
    at: Instant,
}

/// THE JOBS BOARD: the view the loop last adopted, with its tabs' session ids, read by the
/// harness workers' stall canary ([`JobsBoard::evidence`]); one leaf-locked slot.
pub(crate) struct JobsBoard {
    slot: Mutex<Option<Posted>>,
}

impl JobsBoard {
    /// An empty board: every session's evidence is `Unknown`.
    pub(crate) const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// Post `view`, its tabs' session ids `sids`, read at `now`.
    pub(crate) fn post(&self, view: Arc<JobsView>, sids: BTreeMap<u64, String>, now: Instant) {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let at = match slot.as_ref() {
            Some(p) if Arc::ptr_eq(&p.view, &view) => p.at,
            _ => now,
        };
        *slot = Some(Posted { view, sids, at });
    }

    /// WHAT THE VIEW SAYS OF SESSION `sid`'S AGENT'S BACKGROUND WORK at `now`: [`BackgroundEvidence::NoProgress`] with the call quiet longest when every live agent call of this session's tab that is background (or not known to be foreground: `Consumer::Agent`, which no join can tell apart today) reads `no_progress`; else `Unknown` — no view, one older than [`JOBS_FRESH`], none of this session's, or a live call of the agent's that does not read `no_progress`, which may be the work the screen waits on (system awareness §14.5's `AllStuck` asks every live job; its `Progressing` is not built), so the ladder is today's.
    pub(crate) fn evidence(&self, sid: &str, now: Instant) -> BackgroundEvidence {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(posted) = slot
            .as_ref()
            .filter(|p| now.saturating_duration_since(p.at) <= JOBS_FRESH)
        else {
            return BackgroundEvidence::Unknown;
        };
        let calls = || {
            posted
                .view
                .rows
                .iter()
                .filter(|r| posted.sids.get(&r.tab).is_some_and(|s| s == sid))
                .filter(|r| background_call_live(r))
        };
        if calls().any(|r| !r.no_progress) {
            return BackgroundEvidence::Unknown;
        }
        calls()
            .max_by_key(|r| r.quiet_ms.unwrap_or(0))
            .map_or(BackgroundEvidence::Unknown, |r| {
                BackgroundEvidence::NoProgress {
                    worst: background_call(r),
                }
            })
    }

    /// WHAT THE VIEW SAYS SESSION `sid`'S AGENT MAY BE TOLD at `now` (the hung-call design's R2;
    /// `harness_host::WorkerIdle::facts`): every live agent call of this session's tab
    /// ([`background_call_live`]) that reads `no_progress`, EACH REGARDLESS OF THE OTHERS (unlike
    /// [`Self::evidence`], a call making progress beside it takes nothing from it), worst (quiet
    /// longest) first, each with its leader's pid for the caller's re-proof, and the view's age;
    /// `None` with no view or one older than [`JOBS_FRESH`]. The rows are copied under the lock
    /// and nothing else is done there.
    pub(crate) fn facts(
        &self,
        sid: &str,
        now: Instant,
    ) -> Option<(Duration, Vec<(u32, BackgroundCall)>)> {
        let (age, mut calls) = {
            let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            let posted = slot
                .as_ref()
                .filter(|p| now.saturating_duration_since(p.at) <= JOBS_FRESH)?;
            let calls: Vec<(u64, u32, BackgroundCall)> = posted
                .view
                .rows
                .iter()
                .filter(|r| posted.sids.get(&r.tab).is_some_and(|s| s == sid))
                .filter(|r| background_call_live(r) && r.no_progress)
                .map(|r| (r.quiet_ms.unwrap_or(0), r.leader.0, background_call(r)))
                .collect();
            (now.saturating_duration_since(posted.at), calls)
        };
        calls.sort_by_key(|c| std::cmp::Reverse(c.0));
        Some((
            age,
            calls
                .into_iter()
                .map(|(_, leader, call)| (leader, call))
                .collect(),
        ))
    }
}

/// WHETHER THE CALL WHOSE LEADER IS `pid`, BORN AT `birth`, STILL LIVES
/// (`harness_host::WorkerIdle::facts`' re-proof at the ask): one read of that process by the jobs
/// engine's own reader, in the engine's own unit, naming the same start and no zombie. A pid a
/// later process took, a call that ended and was reaped, or one that exited unreaped is not
/// alive, so a fact never tells of a call that already ended. Kernel reads only (`proc_pidinfo`,
/// `/proc`), never WindowServer.
pub(crate) fn leader_lives(pid: u32, birth: u64) -> bool {
    JobsReader::new()
        .read(ReadReq { pid, fds: false })
        .is_some_and(|f| f.start == Some(birth) && f.state != probe::RunState::Zombie)
}

/// The process's board: the loop posts each view it adopts (`App::post_jobs_reading`, to `VitalsHost`'s board: this one live, a test's own in a test), the workers read it.
pub(crate) static BOARD: JobsBoard = JobsBoard::new();

/// A row the canary reads as a live background call of the agent's.
fn background_call_live(r: &JobRow) -> bool {
    r.kind == JobKind::AgentCall
        && matches!(r.consumer, Consumer::AgentBg | Consumer::Agent)
        && r.class != Class::Exited
}

/// The canary's plain form of a row: the process it names (the waiting one
/// for a wait, else the leader) and that process's short name
/// ([`comm_word`]), the group, the agent's task id, the quiet age and a Tier
/// B naming's word.
fn background_call(r: &JobRow) -> BackgroundCall {
    BackgroundCall {
        pid: r.wait.as_ref().map_or(r.leader.0, |w| w.leaf),
        pgid: r.pgid,
        birth: r.leader.1,
        exe: comm_word(&r.exe),
        task: r.task.clone(),
        quiet_ms: r.quiet_ms.unwrap_or(0),
        wait: r.named.as_ref().map(|n| n.word.word().to_string()),
    }
}

/// The bytes of a program's file name the kernel's short name keeps: Linux's `comm` holds 15
/// (`TASK_COMM_LEN` less its NUL), and macOS's `p_comm` 16 (`MAXCOMLEN`), the cut used elsewhere.
pub(crate) const COMM_BYTES: usize = if cfg!(target_os = "linux") { 15 } else { 16 };

/// A row's `exe` (its executable's basename, `JobRow::exe`) cut to [`COMM_BYTES`], back to a
/// character boundary: a test binary's `aterm_gui-e0964a1b2c3d4e5f` is said `aterm_gui-e0964a` on
/// macOS, its `p_comm` ([`BackgroundCall::exe`]), and a file name past the canary's 32 bytes is
/// still said.
fn comm_word(exe: &str) -> String {
    let end = exe
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|end| *end <= COMM_BYTES)
        .last()
        .unwrap_or(0);
    exe.get(..end).unwrap_or_default().to_owned()
}

/// How long until the next pass after one whose demand was `demand`: the
/// engine's interval, never under [`FAST_MS`]; an idle engine (no job, or
/// only waits for the person) looks again at the fast cadence, since a job
/// can start at any keystroke.
fn next_pass_ms(demand: Demand) -> u64 {
    demand.every_ms().unwrap_or(FAST_MS).max(FAST_MS)
}

/// HOW SOON A READING NO WINDOW SHOWS IS DUE AGAIN after one that gave `view` (the hung-call design's D1, "while any agent-call job is suspect"; leftover §9): while a live agent call (any consumer) stands suspect (`Standing::Capable | Capped`, the only rows that accrue φ; every `no_progress` row is one), the engine's own cadence ([`next_pass_ms`] of its `Demand`); else `None`, and the next reading is `vitals_host::job_demand_after`'s (a live job at the view's own `Demand`) or the disk watch's next look.
pub(crate) fn windowless_after(view: &JobsView) -> Option<Duration> {
    let suspect = view.rows.iter().any(|r| {
        r.kind == JobKind::AgentCall
            && r.class != Class::Exited
            && matches!(r.standing, Some(Standing::Capable | Standing::Capped))
    });
    (suspect && view.demand != Demand::Idle)
        .then(|| Duration::from_millis(next_pass_ms(view.demand)))
}

/// How far past the engine's cadence a window's reading lands while [`told_after`] paces it.
const TOLD_BEAT: Duration = Duration::from_millis(250);

/// HOW SOON A WINDOW'S READING IS DUE AGAIN while a live agent call of `view` reads `no_progress` (what `harness_host::WorkerIdle::facts` tells, only from a view no older than the facts gate, `aterm_agent::supervise::facts::FACT_FRESH_MS`, 5 s): a beat ([`TOLD_BEAT`]) past the engine's own cadence, so each reading lands past the pass's due and runs it while staying the bars' paced reading (its split still ticks); else `None`, and the window's own cadence stands. A background window's 5 s never meets the gate: each ask is paced from the last, then read and adopted.
pub(crate) fn told_after(view: &JobsView) -> Option<Duration> {
    view.rows
        .iter()
        .any(|r| background_call_live(r) && r.no_progress)
        .then(|| Duration::from_millis(next_pass_ms(view.demand)) + TOLD_BEAT)
}

/// The engine's tabs from the host's request: its session key, the number
/// the person sees (`Tab 2` is 2), its shell, and its terminal mode word
/// where `ttys` holds a current one.
fn job_tabs(tabs: &[TabRoot], ttys: &[(u64, TtyMode)]) -> Vec<JobTab> {
    tabs.iter()
        .map(|t| JobTab {
            key: t.key,
            number: t
                .label
                .rsplit(' ')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            shell_pid: t.shell_pid,
            tty: ttys.iter().find(|(key, _)| *key == t.key).map(|(_, m)| *m),
            ..JobTab::default()
        })
        .collect()
}

const fn state(s: probe::RunState) -> ProcState {
    match s {
        probe::RunState::Running => ProcState::Running,
        probe::RunState::Sleeping => ProcState::Sleeping,
        probe::RunState::Stopped => ProcState::Stopped,
        probe::RunState::Zombie => ProcState::Zombie,
        probe::RunState::Uninterruptible => ProcState::Uninterruptible,
        probe::RunState::Idle => ProcState::Idle,
        probe::RunState::Unknown => ProcState::Unknown,
    }
}

fn sweep_proc(p: &ProcessReading) -> SweepProc {
    SweepProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        uid: p.uid,
        name: p.name.clone(),
        state: state(p.state),
        session_leader: p.session_leader,
        has_tty: p.has_tty,
    }
}

const fn tcp(s: probe::TcpState) -> TcpState {
    match s {
        probe::TcpState::Closed => TcpState::Closed,
        probe::TcpState::Listen => TcpState::Listen,
        probe::TcpState::SynSent => TcpState::SynSent,
        probe::TcpState::SynReceived => TcpState::SynReceived,
        probe::TcpState::Established => TcpState::Established,
        probe::TcpState::CloseWait => TcpState::CloseWait,
        probe::TcpState::Closing => TcpState::Closing,
        _ => TcpState::Other,
    }
}

const fn filter(f: probe::KnoteFilter) -> KnoteFilter {
    match f {
        probe::KnoteFilter::Read => KnoteFilter::Read,
        probe::KnoteFilter::Write => KnoteFilter::Write,
        probe::KnoteFilter::Timer => KnoteFilter::Timer,
        probe::KnoteFilter::Vnode => KnoteFilter::Vnode,
        probe::KnoteFilter::Proc => KnoteFilter::Proc,
        probe::KnoteFilter::Signal => KnoteFilter::Signal,
        probe::KnoteFilter::User => KnoteFilter::User,
        probe::KnoteFilter::Other(_) => KnoteFilter::Other,
    }
}

fn fd(f: &probe::FdReading) -> Fd {
    let kind = match &f.kind {
        probe::FdKind::File {
            regular,
            size,
            dev,
            ino,
            lf_tail,
        } => FdKind::File {
            regular: *regular,
            size: *size,
            dev: *dev,
            ino: *ino,
            lf_tail: *lf_tail,
        },
        probe::FdKind::Device { tty } => FdKind::Device { tty: *tty },
        probe::FdKind::Pipe {
            handle,
            peer,
            status,
            queued,
            write_end,
        } => FdKind::Pipe {
            handle: *handle,
            peer: *peer,
            status: *status,
            queued: *queued,
            write_end: *write_end,
        },
        probe::FdKind::Tcp {
            state,
            remote,
            local_port,
            queued_read,
        } => FdKind::Tcp {
            state: tcp(*state),
            remote: *remote,
            local_port: *local_port,
            queued_read: *queued_read,
        },
        probe::FdKind::Socket => FdKind::Socket,
        probe::FdKind::EventQueue { sleeping, knotes } => FdKind::EventQueue {
            sleeping: *sleeping,
            knotes: knotes.as_ref().map(|ks| {
                ks.iter()
                    .map(|k| Knote {
                        ident: k.ident,
                        filter: filter(k.filter),
                    })
                    .collect()
            }),
        },
        probe::FdKind::Watch => FdKind::Watch,
        probe::FdKind::Other => FdKind::Other,
    };
    Fd { fd: f.fd, kind }
}

/// The engine's reading of `p`; `stand_in` (a development build's `program-check held <pid>`) reads as held where the reading judged a start, as the kernel's own held reading needs (no zombie, none of another user's: their thread list is not read).
fn job_proc(p: &ProcFacts, stand_in: Option<u32>) -> JobProc {
    JobProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        sid: p.sid,
        start: p.start,
        uid: p.uid,
        name: p.name.clone(),
        exe: p.exe.clone(),
        state: state(p.state),
        tty: p.tty,
        tpgid: p.tpgid,
        counters: p.counters.map(|c| JobCounters {
            cpu_ns: c.cpu_ns,
            child_cpu_ns: c.child_cpu_ns,
            syscalls: c.syscalls,
            wakeups: c.wakeups,
            ctx_switches: c.ctx_switches,
            disk_read: c.disk_read,
            disk_written: c.disk_written,
            logical_writes: c.logical_writes,
            runnable_ns: c.runnable_ns,
            pageins: c.pageins,
        }),
        threads: p.threads.as_ref().map(|ts| {
            ts.iter()
                .map(|t| ThreadState {
                    state: state(t.state),
                    wchan: t.wchan.clone(),
                })
                .collect()
        }),
        wchan: p.wchan.clone(),
        fds: p.fds.as_ref().map(|fs| fs.iter().map(fd).collect()),
        program_check: p.program_check() == Some(AtStart::Held)
            || (stand_in == Some(p.pid) && p.start_facts.is_some()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The engine's cadence, and with no window the suspect rule's: while a
    /// live agent call stands suspect, the engine's demand (2 s fast, 30 s
    /// slow); an idle engine, an exited, running or listening call and jobs
    /// that are no agent's ask nothing of it (a live job is still read at its
    /// own demand, through `vitals_host::job_demand_after`).
    #[test]
    fn the_engine_is_asked_again_at_its_own_cadence_never_faster_than_two_seconds() {
        assert_eq!(next_pass_ms(Demand::Fast), 2_000);
        assert_eq!(next_pass_ms(Demand::Slow), 30_000);
        assert_eq!(next_pass_ms(Demand::Idle), 2_000);

        let view = |rows: Vec<JobRow>, demand| JobsView {
            rows,
            demand,
            ..JobsView::default()
        };
        let call = || JobRow {
            standing: Some(Standing::Capped),
            ..call_row(4, Consumer::AgentBg, true, 31, 27098, "python3")
        };
        let mut running = call();
        running.standing = None;
        let mut serving = call();
        serving.standing = Some(Standing::Legit);
        let mut shell = call();
        shell.kind = JobKind::Shell;
        let mut ended = call();
        ended.class = Class::Exited;
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(windowless_after(&view(vec![call()], Demand::Fast)), secs(2));
        assert_eq!(
            windowless_after(&view(vec![call()], Demand::Slow)),
            secs(30)
        );
        assert_eq!(windowless_after(&view(vec![call()], Demand::Idle)), None);
        assert_eq!(windowless_after(&view(vec![shell], Demand::Fast)), None);
        assert_eq!(windowless_after(&view(vec![ended], Demand::Fast)), None);
        assert_eq!(windowless_after(&view(Vec::new(), Demand::Idle)), None);
        assert_eq!(windowless_after(&view(vec![running], Demand::Fast)), None);
        assert_eq!(windowless_after(&view(vec![serving], Demand::Slow)), None);
        let beat = |ms| Some(Duration::from_millis(ms));
        assert_eq!(told_after(&view(vec![call()], Demand::Fast)), beat(2_250));
        let mut moving = call();
        moving.no_progress = false;
        assert_eq!(told_after(&view(vec![moving], Demand::Fast)), None);
    }

    /// THE CANARY SAYS THE KERNEL'S SHORT NAME: a row naming an integration
    /// test binary by its whole file name (38 bytes, past the canary's 32) is
    /// said by its first [`COMM_BYTES`], as the kernel's short name holds it,
    /// never as `-`; a short name is said whole and a multi-byte one is cut on
    /// a character.
    #[test]
    fn the_canary_names_a_call_by_the_kernels_short_name() {
        let said =
            |exe: &str| background_call(&call_row(4, Consumer::Agent, true, 31, 27098, exe)).exe;
        let long = "conformance_occupancy-0123456789abcdef";
        assert_eq!(said(long), long[..COMM_BYTES]);
        assert_eq!(said("python3"), "python3");
        let split = format!("{}\u{e9}x", "a".repeat(COMM_BYTES - 1));
        assert_eq!(said(&split), "a".repeat(COMM_BYTES - 1));
    }

    /// An agent's call in tab `tab` read `no_progress` (or not), by `consumer`,
    /// quiet `min` minutes, naming process `pid` (`exe`).
    pub(crate) fn call_row(
        tab: u64,
        consumer: Consumer,
        no_progress: bool,
        min: u64,
        pid: u32,
        exe: &str,
    ) -> JobRow {
        use aterm_vitals::jobs::Verdict;
        JobRow {
            tab,
            kind: JobKind::AgentCall,
            consumer,
            no_progress,
            leader: (pid, None),
            pgid: pid,
            exe: exe.into(),
            ..crate::stats_paint::tests::job_row(
                &format!("a{pid}"),
                Verdict::Quiet,
                Some(9_000),
                Some(min * 60_000),
            )
        }
    }

    /// THE CANARY'S EVIDENCE IS THIS SESSION'S BACKGROUND CALL THAT MADE NO
    /// PROGRESS, the one quiet longest: of tab 4's rows (session `s-hung`),
    /// an agent call of no known consumer at 20 minutes and a background one
    /// at 31 minutes waiting in process 27098, the 31-minute one, named by
    /// its waiting process; never a foreground call, a person's job or
    /// another session's call, however long their quiet, and an exited call
    /// does not count.
    #[test]
    fn the_board_names_this_sessions_background_call_quiet_longest() {
        use aterm_vitals::jobs::{NamedRow, Standing, TierB, Wait, WaitName};
        let board = JobsBoard::new();
        let t0 = Instant::now();
        assert_eq!(board.evidence("s-hung", t0), BackgroundEvidence::Unknown);
        let mut waiting = call_row(4, Consumer::AgentBg, true, 31, 27090, "sh");
        waiting.wait = Some(Wait::bare(
            WaitName::Unnamed,
            Standing::Capped,
            27098,
            "aterm_gui-e0964a",
        ));
        waiting.exe = "aterm_gui-e0964a".into();
        waiting.task = Some("bplle5dxj".into());
        waiting.named = Some(NamedRow {
            word: TierB::Timer,
            holder: None,
        });
        let mut person = call_row(4, Consumer::PersonFg, true, 600, 700, "sleep");
        person.kind = JobKind::Shell;
        let mut ended = call_row(4, Consumer::AgentBg, false, 120, 503, "cargo");
        ended.class = Class::Exited;
        let rows = vec![
            call_row(4, Consumer::Agent, true, 20, 501, "Python"),
            waiting.clone(),
            call_row(4, Consumer::AgentFg, true, 99, 502, "node"),
            ended,
            person.clone(),
            call_row(9, Consumer::AgentBg, true, 500, 504, "make"),
            call_row(9, Consumer::AgentBg, false, 5, 505, "cargo"),
        ];
        let view = || {
            Arc::new(JobsView {
                rows: rows.clone(),
                ..JobsView::default()
            })
        };
        let sids = BTreeMap::from([(4, "s-hung".to_string()), (9, "s-other".to_string())]);
        let first = view();
        board.post(Arc::clone(&first), sids.clone(), t0);
        let hung = BackgroundEvidence::NoProgress {
            worst: BackgroundCall {
                pid: 27098,
                pgid: 27090,
                birth: None,
                exe: "aterm_gui-e0964a"[..COMM_BYTES].into(),
                task: Some("bplle5dxj".into()),
                quiet_ms: 31 * 60_000,
                wait: Some("timer".into()),
            },
        };
        assert_eq!(board.evidence("s-hung", t0), hung);
        assert_eq!(board.evidence("s-calm", t0), BackgroundEvidence::Unknown);
        let late = t0 + JOBS_FRESH + Duration::from_secs(1);
        assert_eq!(board.evidence("s-hung", late), BackgroundEvidence::Unknown);
        board.post(Arc::clone(&first), sids.clone(), late);
        assert_eq!(
            board.evidence("s-hung", late),
            BackgroundEvidence::Unknown,
            "the same view posted again keeps its age"
        );
        board.post(view(), sids.clone(), late);
        assert_eq!(board.evidence("s-hung", late), hung, "a new view is fresh");
        let mut building = rows.clone();
        building.push(call_row(4, Consumer::AgentBg, false, 2, 506, "cargo"));
        let view = Arc::new(JobsView {
            rows: building,
            ..JobsView::default()
        });
        board.post(view, sids, late);
        assert_eq!(
            board.evidence("s-hung", late),
            BackgroundEvidence::Unknown,
            "a live call that is not at no progress"
        );
    }

    /// Each tab keeps its key, number and shell, and takes the terminal word
    /// handed for its key.
    #[test]
    fn a_tab_keeps_its_session_key_shell_and_number_and_its_own_terminal_word() {
        let root = |key, label: &str| TabRoot {
            key,
            tab: key,
            shell_pid: 900 + u32::try_from(key).expect("small"),
            label: label.into(),
            program: String::new(),
        };
        let raw = TtyMode {
            canonical: false,
            echo: false,
            stamped_ms: 7,
        };
        let tabs = job_tabs(&[root(41, "Tab 3"), root(42, "Tab 4")], &[(41, raw)]);
        assert_eq!(
            (tabs[0].key, tabs[0].number, tabs[0].shell_pid, tabs[0].tty),
            (41, 3, 941, Some(raw))
        );
        assert_eq!((tabs[1].key, tabs[1].tty), (42, None));
    }

    /// The pass on this host's real process table: this test process stands
    /// in for a tab's shell.
    #[test]
    fn a_pass_over_the_live_table_yields_a_view_and_keeps_it_between_passes() {
        let Some(sweep) = aterm_sysprobe::processes() else {
            // A host without a process sweep publishes no jobs.
            return;
        };
        let me = std::process::id();
        let tabs = [TabRoot {
            key: 1,
            tab: 1,
            shell_pid: me,
            label: "Tab 1".into(),
            program: String::new(),
        }];
        let mut pass = JobsPass::new();
        let first = pass
            .tick(10_000, Some(&sweep.procs), &tabs, &[], me, false, false)
            .expect("a view");
        let again = pass
            .tick(10_500, Some(&sweep.procs), &tabs, &[], me, false, false)
            .expect("the same view");
        assert!(
            Arc::ptr_eq(&first, &again),
            "inside the cadence nothing is re-read"
        );
        assert!(
            pass.tick(10_600, None, &tabs, &[], me, false, true)
                .is_some(),
            "a reading without a sweep keeps the last view"
        );
    }

    /// AN EDGE'S READING LOOKS (the loop paces edges on its own clock): one
    /// asked inside the pass's cadence runs the engine anyway, so the job it
    /// was asked for is seen now.
    #[test]
    fn an_edges_reading_inside_the_cadence_runs_the_engine() {
        let Some(sweep) = aterm_sysprobe::processes() else {
            // A host without a process sweep publishes no jobs.
            return;
        };
        let me = std::process::id();
        let tabs = [TabRoot {
            key: 1,
            tab: 1,
            shell_pid: me,
            label: "Tab 1".into(),
            program: String::new(),
        }];
        let mut pass = JobsPass::new();
        let first = pass
            .tick(10_000, Some(&sweep.procs), &tabs, &[], me, false, false)
            .expect("a view");
        let paced = pass
            .tick(11_900, Some(&sweep.procs), &tabs, &[], me, false, false)
            .expect("the last view");
        assert!(Arc::ptr_eq(&first, &paced), "paced: the engine waits");
        let edge = pass
            .tick(11_900, Some(&sweep.procs), &tabs, &[], me, false, true)
            .expect("a new view");
        assert!(!Arc::ptr_eq(&first, &edge), "an edge's reading looks");
    }

    /// THE PASS COUNTS THIS USER'S HELD PROGRAMS, MEMBERS OR NOT: a member held
    /// by its own reading, and of the rest only this user's live rows with at
    /// most 20 ms of CPU are asked for their start (a held one has about 2 ms);
    /// each held row is keyed by its pid version, and an orphan is aterm's by
    /// its coalition.
    #[test]
    fn the_jobs_pass_counts_this_users_held_programs_members_or_not() {
        use std::cell::RefCell;
        const ME: u32 = 501;
        let row = |pid, uid, cpu_ms: u64, state| ProcessReading {
            pid,
            uid: Some(uid),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            state,
            ..ProcessReading::default()
        };
        let sweep = [
            row(10, ME, 400, probe::RunState::Sleeping),
            row(20, ME, 2, probe::RunState::Sleeping),
            row(21, ME, 2, probe::RunState::Sleeping),
            row(22, ME, 2, probe::RunState::Sleeping),
            row(30, ME, 30, probe::RunState::Sleeping),
            row(40, 0, 2, probe::RunState::Sleeping),
            row(50, ME, 0, probe::RunState::Zombie),
        ];
        let members = [
            JobProc {
                pid: 10,
                uid: Some(ME),
                name: "zsh".into(),
                exe: Some("gh".into()),
                program_check: true,
                ..JobProc::default()
            },
            JobProc {
                pid: 22,
                uid: Some(ME),
                name: "sleep".into(),
                ..JobProc::default()
            },
        ];
        let asked = RefCell::new(Vec::new());
        let held = held_list(
            &sweep,
            &members,
            Some(ME),
            |pid| {
                asked.borrow_mut().push(pid);
                Some(AtStart::Held)
            },
            |pid| Some(u64::from(pid) * 100 + 7),
            |pid| pid == 20,
            |pid| (pid == 20).then(|| "codex-command-runner".to_owned()),
        );
        assert_eq!(
            asked.into_inner(),
            [20, 21],
            "only this user's live rows outside every job with at most 20 ms of CPU are asked: \
             never the 30 ms row, another user's, a zombie or a member"
        );
        let got: Vec<(u32, u64, &str, bool)> = held
            .iter()
            .map(|h| (h.pid, h.exec, h.name.as_str(), h.aterm))
            .collect();
        assert_eq!(
            got,
            [
                (10, 1007, "gh", true),
                (20, 2007, "codex-command-runner", true),
                (21, 2107, "p21", false)
            ],
            "each named by its executable where one reads, never by p_comm's 16 bytes"
        );
        assert!(
            held_list(
                &sweep,
                &members,
                None,
                |_| Some(AtStart::Held),
                |_| None,
                |_| true,
                |_| None,
            )
            .is_empty(),
            "no user read, nothing counted"
        );
    }

    /// THE PASS HANDS THE ENGINE ITS HELD LIST: over a sweep with no tab,
    /// the rows its start reader reads held, and the pid the stand-in names,
    /// reach the view's program-check findings with their executables' names,
    /// and only the ones in this process's coalition count as aterm's.
    #[test]
    fn the_pass_hands_the_engine_this_users_held_programs_and_the_stand_in() {
        const ME: u32 = 501;
        let row = |pid, cpu_ms: u64| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let sweep = [row(10, 400), row(20, 2), row(21, 2), row(22, 2), row(23, 2)];
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |pid, _| {
                Some(if matches!(pid, 20 | 23) {
                    AtStart::Held
                } else {
                    AtStart::Started
                })
            },
            exec: |pid| Some(u64::from(pid)),
            coalition: |pid| Some(if pid == 23 { 8 } else { 7 }),
            exe_name: |pid| (pid == 20).then(|| "codex-command-runner".to_owned()),
            stand_in: || Some(21),
        };
        let view = pass
            .tick(10_000, Some(&sweep), &[], &[], 10, false, true)
            .expect("a view");
        let held: Vec<(u32, &str, Option<u64>)> = view
            .findings
            .program_check
            .iter()
            .map(|n| (n.pid, n.name.as_str(), n.tab))
            .collect();
        assert_eq!(
            held,
            [
                (20, "codex-command-runner", None),
                (21, "p21", None),
                (23, "p23", None)
            ],
            "the reader's held rows and the stand-in's, outside every tab"
        );
        assert_eq!(
            view.findings.program_check_aterm,
            (2, 0),
            "aterm's: the two in this process's coalition, not 23 in another"
        );
    }

    /// THE STAND-IN READS HELD WHERE THE READING JUDGED A START: a member
    /// whose thread list was read, never a zombie's or another user's reading
    /// (no start read), and a member the kernel reads held stays held.
    #[test]
    fn a_stand_in_reads_held_only_where_the_reading_judged_a_start() {
        let read = ProcFacts {
            pid: 30,
            start_facts: Some(probe::StartFacts::default()),
            ..ProcFacts::default()
        };
        let unread = ProcFacts {
            pid: 31,
            ..ProcFacts::default()
        };
        let held = ProcFacts {
            pid: 32,
            start_facts: Some(probe::StartFacts {
                lone_zero: true,
                local: Some(true),
                age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
                ..probe::StartFacts::default()
            }),
            ..ProcFacts::default()
        };
        assert!(job_proc(&read, Some(30)).program_check);
        assert!(!job_proc(&read, None).program_check);
        assert!(
            !job_proc(&unread, Some(31)).program_check,
            "no start read: a zombie, or another user's"
        );
        assert!(job_proc(&held, Some(30)).program_check, "the kernel's own");
    }

    /// A view speaks for now for a minute; a PARKED one (demand idle) for as
    /// long as its reader runs.
    #[test]
    fn a_parked_view_speaks_for_now_while_its_reader_runs() {
        let parked = JobsView::default();
        assert_eq!(parked.demand, Demand::Idle);
        let watched = JobsView {
            demand: Demand::Fast,
            ..JobsView::default()
        };
        let late = JOBS_FRESH + Duration::from_secs(1);
        assert!(speaks_for_now(&watched, JOBS_FRESH, false));
        assert!(speaks_for_now(&parked, late, true), "parked, read on edges");
        assert!(!speaks_for_now(&watched, late, true), "due long ago");
        assert!(!speaks_for_now(&parked, late, false), "no reader");
    }

    /// THE RE-PROOF READS THE ENGINE'S OWN BIRTH: this process, read by the jobs reader as the
    /// engine reads a leader, lives at its own start, and a start one unit off is another process
    /// (a pid a later process took), which does not. NEGATIVE CONTROL: a pid no process holds
    /// lives at no start.
    #[test]
    fn a_leader_lives_only_at_its_own_birth() {
        let me = std::process::id();
        let birth = JobsReader::new()
            .read(ReadReq {
                pid: me,
                fds: false,
            })
            .and_then(|f| f.start)
            .expect("this process's start");
        assert!(leader_lives(me, birth));
        assert!(!leader_lives(me, birth + 1));
        assert!(!leader_lives(u32::MAX - 1, birth));
    }
}
