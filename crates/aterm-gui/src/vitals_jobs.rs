// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vitals host's jobs pass: the stuck-process engine (`aterm_vitals::jobs`) fed on the probe
//! thread, so the stats bars' marks and the lens's Jobs well show its verdicts.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::supervise::{BackgroundCall, BackgroundEvidence};

use aterm_sysprobe::jobs::{self as probe, AtStart, JobsReader, ProcFacts, ReadReq};
use aterm_sysprobe::procs::ProcessReading;
use aterm_sysprobe::volumes::Table;
use aterm_vitals::jobs::{
    Block, Class, Consumer, Demand, Fd, FdKind, HeldProc, JobCounters, JobKind, JobProc, JobRow,
    JobTab, JobTick, Jobs, JobsView, Knote, KnoteFilter, LanesSeen, Machine, ProcState, Standing,
    StartWait, SweepProc, TcpState, TermWord, ThreadState, TtyMode,
};
use aterm_vitals::model::TabRoot;

/// The engine's fast cadence, also the floor between passes, so a 2 Hz lens does not double the
/// detailed reads.
const FAST_MS: u64 = 2_000;

/// The most CPU a process outside every job may have used for the pass to read its start. A held
/// one has used about 2 ms, the fork child's work before its exec (leftover M4), so its counters
/// cannot tell it from an idle one: CPU only selects rows, and the thread call decides.
pub(crate) const HELD_CPU_NS: u64 = 20_000_000;

/// The readers the pass asks of this user's processes outside every job, and the held stand-in
/// (`KERNEL` live, `INERT` in unit tests). None reads a process's arguments or environment, which
/// can hang on a program held mid-start (leftover M29).
#[derive(Clone, Copy)]
pub(crate) struct HeldReaders {
    /// Where a process stands at its start, one mount table read for the pass
    /// ([`probe::read_start_in`], [`probe::judge_start`]).
    pub(crate) start: fn(u32, &OnceCell<Table>) -> Option<AtStart>,
    /// Its pid version, which moves on exec ([`aterm_uds::process::read_identity`]).
    pub(crate) exec: fn(u32) -> Option<u64>,
    /// Its resource coalition ([`aterm_sysprobe::coalitions::coalition_of`]).
    pub(crate) coalition: fn(u32) -> Option<u64>,
    /// Its executable's file name, the basename of [`aterm_sysprobe::process_path`] (`p_comm` cuts
    /// a name at 16 bytes).
    pub(crate) exe_name: fn(u32) -> Option<String>,
    /// The pid a development build stands in as held
    /// ([`crate::program_check_watch::held_stand_in`]).
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

    /// One reading's pass: runs the engine when it is due, or whenever `now` (an edge's reading:
    /// the loop paces those on its own clock, and one landing inside this pass's cadence must
    /// still look, or the job it was asked for goes unseen until the next edge). Returns the view
    /// to publish, the last one when the engine did not run.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tick(
        &mut self,
        at_ms: u64,
        read_ms: impl Fn() -> u64,
        sweep: Option<&[ProcessReading]>,
        tabs: &[TabRoot],
        ttys: &[(u64, TtyMode)],
        feeds: &[(u64, TabFeed)],
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
        let tabs = job_tabs(tabs, ttys, feeds);
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
        // Each age is the probe's as its reading starts.
        let probe_ms = read_ms();
        let read = self.reader.read_all(&reqs);
        let roots: BTreeSet<u32> = tabs.iter().map(|t| t.shell_pid).collect();
        let procs = job_procs(&read, &roots, stand_in, readers.exec);
        let read_ms = read_ms();
        let mounts = OnceCell::new();
        let ours = OnceCell::new();
        let held = held_list(
            readings,
            &read,
            self_uid,
            stand_in,
            |pid| (readers.start)(pid, &mounts),
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
            read_ms,
            probe_ms,
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

/// This user's programs that have not started, members of a job or not: each member of `members`
/// (judged from its own thread list, [`member_start`]) that reads held, translating or unstarted,
/// and each other row of `sweep` of `self_uid` that is no zombie, has used at most
/// [`HELD_CPU_NS`] (or is `stand_in`, held whatever its CPU) and that `start` reads so. Each is
/// keyed by its pid version (`exec`, else its start, so an exec starts a new held time), named by
/// its executable (a member's own reading, else `exe_name`, else its short name), and is aterm's
/// when it is a member or `in_aterm` (aterm's coalition) says so. `start` is asked only of those
/// sweep rows but the stand-in, `exec` only of the rows listed, and `exe_name` only of listed rows
/// outside every job.
#[allow(clippy::too_many_arguments)]
pub(crate) fn held_list(
    sweep: &[ProcessReading],
    members: &[ProcFacts],
    self_uid: Option<u32>,
    stand_in: Option<u32>,
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
        .filter(|m| m.uid == Some(uid))
        .filter_map(|m| {
            let wait = start_wait(member_start(m, stand_in)?)?;
            Some(HeldProc {
                pid: m.pid,
                exec: exec(m.pid).or(m.start).unwrap_or(0),
                name: m.exe.clone().unwrap_or_else(|| m.name.clone()),
                aterm: true,
                wait,
            })
        })
        .collect();
    for p in sweep.iter().filter(|p| {
        p.uid == Some(uid)
            && p.state != probe::RunState::Zombie
            && (p.cpu_ns.is_some_and(|c| c <= HELD_CPU_NS) || stand_in == Some(p.pid))
            && !read.contains(&p.pid)
    }) {
        let at = if stand_in == Some(p.pid) {
            Some(AtStart::Held)
        } else {
            start(p.pid)
        };
        if let Some(wait) = at.and_then(start_wait) {
            held.push(HeldProc {
                pid: p.pid,
                exec: exec(p.pid).or(p.start).unwrap_or(0),
                name: exe_name(p.pid).unwrap_or_else(|| p.name.clone()),
                aterm: in_aterm(p.pid),
                wait,
            });
        }
    }
    held
}

/// What keeps a process that stands `at` its start from starting, as the engine names it; `None`
/// for one that started.
const fn start_wait(at: AtStart) -> Option<StartWait> {
    match at {
        AtStart::Started => None,
        AtStart::Held => Some(StartWait::ProgramCheck),
        AtStart::Translating => Some(StartWait::Translating),
        AtStart::Unstarted => Some(StartWait::NotStarted),
    }
}

/// Where member reading `p` stands at its start ([`ProcFacts::program_check`]). `stand_in` (a
/// development build's `program-check held <pid>`) reads held only where the reading judged a
/// start, as the kernel's own held reading needs: never for a zombie or another user's process,
/// whose thread list is not read.
fn member_start(p: &ProcFacts, stand_in: Option<u32>) -> Option<AtStart> {
    let judged = p.program_check();
    if stand_in == Some(p.pid) && judged.is_some() {
        Some(AtStart::Held)
    } else {
        judged
    }
}

/// How old the board's view may be and still speak for the canary ([`JobsBoard::evidence`]).
pub(crate) const JOBS_FRESH: Duration = Duration::from_secs(60);

/// Whether a jobs view `age` old speaks for now: one at most [`JOBS_FRESH`] old does, and so does a
/// parked one (demand idle and no [`holds_a_wait`]) of any age while its reader runs (`reader`). A
/// parked engine is read again only on an edge (output that moves a screen, a parked screen's
/// redraw at most once a floor, a session's fork, exec or exit, the disk watch's look), so no
/// reading since means nothing that draws or that the kernel tells of changed. A start raises no
/// edge, and a view that holds a wait is read on a timer instead, so past [`JOBS_FRESH`] its
/// reader has stopped delivering.
pub(crate) fn speaks_for_now(view: &JobsView, age: Duration, reader: bool) -> bool {
    age <= JOBS_FRESH || (reader && view.demand == Demand::Idle && !holds_a_wait(view))
}

/// Whether `view` names a program of this user that has not started (held by macOS's program
/// check, translating under Rosetta, or on a volume not proven local), or stands in a backed-up
/// episode that has not had its closing reading. Such a view is read again within 30 s whatever
/// its demand (`vitals_host::job_demand_after`), since a start raises no edge.
pub(crate) fn holds_a_wait(view: &JobsView) -> bool {
    let f = &view.findings;
    !f.program_check.is_empty()
        || !f.translating.is_empty()
        || !f.not_started.is_empty()
        || f.program_check_episode.is_some()
}

/// `privacy`'s `program_check=` from `view`, `age` old: how many of this user's programs macOS's
/// program check holds (a lower bound, aterm's and others'), and the longest held to now in ms (its
/// first sighting's age plus the view's); `None` when the view does not speak for now
/// ([`speaks_for_now`]).
pub(crate) fn program_check_of(view: &JobsView, age: Duration, reader: bool) -> Option<(u32, u64)> {
    if !speaks_for_now(view, age, reader) {
        return None;
    }
    let held = &view.findings.program_check;
    let oldest = held.iter().map(|n| n.for_ms).max().unwrap_or(0);
    let age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
    Some((
        u32::try_from(held.len()).unwrap_or(u32::MAX),
        oldest.saturating_add(age_ms),
    ))
}

/// The view last posted, its tabs' session ids, and when it was first posted.
struct Posted {
    view: Arc<JobsView>,
    sids: BTreeMap<u64, String>,
    at: Instant,
}

/// The view the loop last adopted, with its tabs' session ids, read by the harness workers' stall
/// canary ([`JobsBoard::evidence`]); one leaf-locked slot.
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

    /// What the view says of session `sid`'s agent's background work at `now`:
    /// [`BackgroundEvidence::NoProgress`] with the call quiet longest when every live agent call of
    /// the session's tab that is background, or not known to be foreground (`Consumer::Agent`),
    /// reads `no_progress`. Else `Unknown`: no view, one older than [`JOBS_FRESH`], no call of this
    /// session's, or a live call that does not read `no_progress`, which may be the work the screen
    /// waits on.
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

    /// What session `sid`'s agent may be told at `now` (`harness_host::WorkerIdle::facts`): every
    /// live agent call of the session's tab ([`background_call_live`]) that reads `no_progress`,
    /// each regardless of the others (unlike [`Self::evidence`]), worst (quiet longest) first, with
    /// its leader's pid for the caller's re-proof, and the view's age; `None` with no view or one
    /// older than [`JOBS_FRESH`]. A wrapper with observed work in a descendant call is omitted.
    /// Only the copy of the rows runs under the lock.
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
                .filter(|r| {
                    background_call_live(r) && r.no_progress && r.working_descendant.is_none()
                })
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

/// Whether the call whose leader is `pid`, born at `birth`, still lives (the facts' re-proof at
/// the ask): one read by the jobs engine's own reader, in the engine's unit, naming the same start
/// and no zombie, so a reused pid, a reaped call and an unreaped exit are not alive. Kernel reads
/// only (`proc_pidinfo`, `/proc`), never WindowServer.
pub(crate) fn leader_lives(pid: u32, birth: u64) -> bool {
    JobsReader::new()
        .read(ReadReq { pid, fds: false })
        .is_some_and(|f| f.start == Some(birth) && f.state != probe::RunState::Zombie)
}

/// The process's board: the loop posts each view it adopts (`App::post_jobs_reading`, through
/// `VitalsHost`'s board, which a test replaces with its own), and the workers read it.
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
/// (`TASK_COMM_LEN` less its NUL), and macOS's `p_comm` 16 (`MAXCOMLEN`).
pub(crate) const COMM_BYTES: usize = if cfg!(target_os = "linux") { 15 } else { 16 };

/// A row's `exe` (`JobRow::exe`) cut to [`COMM_BYTES`] at a character boundary, as the kernel's
/// short name holds it, so a file name past the canary's 32 bytes ([`BackgroundCall::exe`]) is
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

/// How soon a reading no window shows is due again after one that gave `view`: while a live agent
/// call (any consumer) stands suspect (`Standing::Capable | Capped`, the only rows that accrue φ)
/// and the engine is not idle, the engine's own cadence ([`next_pass_ms`]); else `None`, and the
/// next reading is `vitals_host::job_demand_after`'s or the disk watch's next look.
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

/// How soon a window's reading is due again while a live agent call of `view` reads `no_progress`
/// (what `harness_host::WorkerIdle::facts` tells, only from a view no older than
/// `aterm_agent::supervise::facts::FACT_FRESH_MS`, 5 s): a beat ([`TOLD_BEAT`]) past the engine's
/// cadence, so each reading lands past the pass's due and runs it; else `None`, and the window's
/// own cadence stands. A background window's 5 s cadence never meets the gate, since each reading
/// is paced from the last, then read and adopted.
pub(crate) fn told_after(view: &JobsView) -> Option<Duration> {
    view.rows
        .iter()
        .any(|r| background_call_live(r) && r.no_progress)
        .then(|| Duration::from_millis(next_pass_ms(view.demand)) + TOLD_BEAT)
}

/// The engine's tabs from the host's request: its session key, the number
/// the person sees (`Tab 2` is 2), its shell, its terminal mode word where
/// `ttys` holds a current one, and what its PTY reader read where `feeds`
/// holds it.
fn job_tabs(tabs: &[TabRoot], ttys: &[(u64, TtyMode)], feeds: &[(u64, TabFeed)]) -> Vec<JobTab> {
    tabs.iter()
        .map(|t| {
            let feed = feeds
                .iter()
                .find(|(key, _)| *key == t.key)
                .map(|(_, f)| f.clone())
                .unwrap_or_default();
            JobTab {
                key: t.key,
                number: t
                    .label
                    .rsplit(' ')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
                shell_pid: t.shell_pid,
                block: feed.block,
                ended: feed.ended,
                shell_ms: feed.shell_ms,
                typed_ms: feed.typed_ms,
                tty: ttys.iter().find(|(key, _)| *key == t.key).map(|(_, m)| *m),
                output_ms: feed.output_ms,
                line_output_ms: feed.line_output_ms,
                drawn_ms: feed.drawn_ms,
                alt_screen: feed.alt_screen,
                output_bytes: feed.output_bytes,
                ..JobTab::default()
            }
        })
        .collect()
}

/// What a session's PTY reader read, for the engine: its newest feed that moved the screen, its
/// newest feed that held a line, the bytes it has read, and the newest OSC 133 block. The reader
/// writes it as it reads and feeds ([`Self::read`], [`Self::fed`]); the probe thread reads it at
/// each pass ([`Self::at_pass`]). Without it a printing `ping` reads quiet, and each command of a
/// typed `while` loop is a job of its own. A write that moves no cell (a colour reset at rest) is
/// no output, as it is no edge for the output wake ([`crate::vitals_host::ScreenMoves`]): counted,
/// it would keep a program at rest running and the engine reading every 2 s.
#[derive(Debug, Default)]
pub(crate) struct JobFeed(Mutex<Fed>);

#[derive(Debug, Default)]
struct Fed {
    output: Option<Instant>,
    line: Option<Instant>,
    /// The newest moving feed that drew the screen ([`Lines::drawn`]).
    drawn: Option<Instant>,
    /// The newest line the last pass found stamped past its millisecond, which it left to the
    /// next.
    owed_line: Option<Instant>,
    /// The newest output the last pass found stamped past its millisecond, as `owed_line`.
    owed_output: Option<Instant>,
    /// The same of the newest drawing feed.
    owed_drawn: Option<Instant>,
    /// The terminal showed its alternate screen after the last feed.
    alt: bool,
    /// The read being fed holds text after its last line feed so far: a line feed in its next
    /// chunk ends a line.
    text: bool,
    bytes: u64,
    block: Option<FedBlock>,
    /// The block `block` replaced, closed.
    ended: Option<FedBlock>,
    /// The terminal's current block as a feed last found one (id, state, status), and when a
    /// feed found it changed: the shell wrote one of its own marks.
    marks: Option<(u64, aterm_types::BlockState, Option<i32>)>,
    shell: Option<Instant>,
    /// The keys driven into the session's terminal as the last pass found their count, and the
    /// clock of the newest pass that found it moved ([`JobFeed::typed_at`]).
    driven: Option<aterm_session::sink::DrivenInput>,
    typed: Option<u64>,
}

#[derive(Clone, Debug)]
struct FedBlock {
    id: u64,
    open: bool,
    command: String,
    status: Option<i32>,
    /// When the reader first saw it run (OSC 133 C).
    started: Instant,
}

/// A [`JobFeed`] read at a pass, in host ms on the reader's clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabFeed {
    pub(crate) output_ms: Option<u64>,
    pub(crate) line_output_ms: Option<u64>,
    pub(crate) drawn_ms: Option<u64>,
    pub(crate) alt_screen: bool,
    pub(crate) output_bytes: u64,
    pub(crate) block: Option<Block>,
    pub(crate) ended: Option<Block>,
    pub(crate) shell_ms: Option<u64>,
    pub(crate) typed_ms: Option<u64>,
}

impl JobFeed {
    /// The bytes the reader has read so far.
    pub(crate) fn bytes(&self) -> u64 {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).bytes
    }

    /// The reader read `bytes`, which it feeds next, chunk by chunk.
    pub(crate) fn read(&self, bytes: &[u8]) {
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        fed.text = false;
        fed.bytes = fed
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    }

    /// A feed of `chunk` left `term` as it stands (the reader holds the term lock); `moved` when it
    /// moved the screen ([`crate::vitals_host::ScreenMoves::fed`]). A moving feed is output `now`.
    /// A chunk holding a line feed and text of its read (in the chunk, or in one the parse cut
    /// before it) is a line `now`: a bare line end is the echo of a typed Enter, which must not
    /// restart a hung command's quiet clock. Each stamp is taken under the feed's lock, so one made
    /// after a pass read the feed is past that pass's clock, and a line and the shell's marks
    /// parsed with it share one stamp.
    ///
    /// The block held is the newest that ran: open from OSC 133 C (started `now` when first seen
    /// so), closed by D with D's status, and kept until another block runs. The shell writes D and
    /// the next A together, and A archives the closed block at once, so a feed that finds a newer
    /// prompt's block closes the held one with its archived status; a block never seen open is held
    /// only while nothing ran before it. A terminal holding no block (a reset, a clear) leaves an
    /// open one open until the next prompt. The block a newer one replaces is kept as the ended
    /// one with its status. A feed that finds the current block changed stamps the shell's marks
    /// `now`; a terminal left with no block is none of the shell's.
    pub(crate) fn fed(
        &self,
        term: &aterm_core::terminal::Terminal,
        chunk: &[u8],
        moved: bool,
        now: impl FnOnce() -> Instant,
    ) {
        enum Turn {
            Keep,
            Open(u64),
            Close(Option<i32>),
            Hold(u64, Option<i32>),
        }
        let current = term.current_block();
        let marks = current.map(|c| (c.id, c.state, c.exit_code));
        let lines = Lines::of(chunk);
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let line = lines.ends && (lines.ended || lines.tail || fed.text);
        fed.text = lines.tail || (fed.text && !lines.ends);
        let marked = marks.is_some() && marks != fed.marks;
        let alt = term.is_alternate_screen();
        let runs = |c: &aterm_types::OutputBlock| c.state == aterm_types::BlockState::Executing;
        let turn = match (current, fed.block.as_ref()) {
            (Some(c), Some(w)) if c.id == w.id => match (runs(c), w.open) {
                (true, true) => Turn::Keep,
                (true, false) => Turn::Open(c.id),
                (false, _) if (w.open, w.status) == (false, c.exit_code) => Turn::Keep,
                (false, _) => Turn::Close(c.exit_code),
            },
            (Some(c), _) if runs(c) => Turn::Open(c.id),
            (Some(_), Some(w)) if w.open => {
                Turn::Close(term.block_by_id(w.id).and_then(|b| b.exit_code))
            }
            (Some(c), None) => Turn::Hold(c.id, c.exit_code),
            _ => Turn::Keep,
        };
        if matches!(turn, Turn::Keep) && !moved && !marked && !line && alt == fed.alt {
            return;
        }
        fed.alt = alt;
        let now = now();
        if moved {
            fed.output = Some(now);
            if lines.drawn {
                fed.drawn = Some(now);
            }
        }
        if line {
            fed.line = Some(now);
        }
        if marked {
            fed.marks = marks;
            fed.shell = Some(now);
        }
        match turn {
            Turn::Keep => {}
            Turn::Open(id) => {
                if let Some(mut w) = fed.block.take().filter(|w| w.id != id) {
                    if w.open {
                        w.open = false;
                        w.status = term.block_by_id(w.id).and_then(|b| b.exit_code);
                    }
                    fed.ended = Some(w);
                }
                fed.block = Some(FedBlock {
                    id,
                    open: true,
                    command: crate::session_status::executing_command_line(term)
                        .unwrap_or_default(),
                    status: None,
                    started: now,
                });
            }
            Turn::Close(status) => {
                if let Some(w) = fed.block.as_mut() {
                    w.open = false;
                    w.status = status;
                }
            }
            Turn::Hold(id, status) => {
                fed.block = Some(FedBlock {
                    id,
                    open: false,
                    command: String::new(),
                    status,
                    started: now,
                });
            }
        }
    }

    /// The feed now: [`Self::at_pass`] with no pass's clock.
    #[cfg(test)]
    pub(crate) fn at(&self, epoch: Instant) -> TabFeed {
        self.at_pass(epoch, None)
    }

    /// Notes the count of keys driven into the session's terminal (the person's or a driver's) as a
    /// pass at `at_ms` finds it, and returns the clock of the newest pass that found it moved since
    /// the pass before; the first pass only notes it.
    pub(crate) fn typed_at(
        &self,
        at_ms: u64,
        driven: aterm_session::sink::DrivenInput,
    ) -> Option<u64> {
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if fed.driven.is_some_and(|d| d != driven) {
            fed.typed = Some(at_ms);
        }
        fed.driven = Some(driven);
        fed.typed
    }

    /// The feed at a pass whose clock reads `at_ms` (`None`: no pass), on the clock whose zero is
    /// `epoch` (the probe thread's). Each stamp is whole ms rounded up, so a read after a pass's
    /// clock is stamped past it, and at least 1 ms, so a mode word stamped 0 is older than all of
    /// them ([`crate::vitals_host::TtyWord::current`]). A newest line or output stamped past
    /// `at_ms` is the next pass's, even if a newer one overwrites it first, and this pass reads the
    /// one the last left it, so a writer that never stops still has its lines read.
    pub(crate) fn at_pass(&self, epoch: Instant, at_ms: Option<u64>) -> TabFeed {
        let ms = |at: Instant| {
            u64::try_from(
                at.saturating_duration_since(epoch)
                    .as_nanos()
                    .div_ceil(1_000_000),
            )
            .unwrap_or(u64::MAX)
            .max(1)
        };
        let block = |b: &FedBlock| Block {
            id: b.id,
            started_ms: ms(b.started),
            open: b.open,
            command: b.command.clone(),
            status: b.status,
            stopped: b.status.is_some_and(stopped),
        };
        let defer = |newest: Option<Instant>, owed: &mut Option<Instant>| match at_ms {
            Some(a) if newest.is_some_and(|s| ms(s) > a) => std::mem::replace(owed, newest),
            _ => {
                *owed = None;
                newest
            }
        };
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let fed = &mut *fed;
        let line = defer(fed.line, &mut fed.owed_line);
        let output = defer(fed.output, &mut fed.owed_output);
        let drawn = defer(fed.drawn, &mut fed.owed_drawn);
        TabFeed {
            output_ms: output.map(ms),
            line_output_ms: line.map(ms),
            drawn_ms: drawn.map(ms),
            alt_screen: fed.alt,
            output_bytes: fed.bytes,
            block: fed.block.as_ref().map(block),
            ended: fed.ended.as_ref().map(block),
            shell_ms: fed.shell.map(ms),
            typed_ms: fed.typed,
        }
    }
}

/// What a fed chunk holds of lines.
struct Lines {
    /// A line feed.
    ends: bool,
    /// A line feed after text in the chunk itself.
    ended: bool,
    /// Text after its last line feed, or all of it when it holds none.
    tail: bool,
    /// A control sequence that puts the cursor on a row of the program's choosing or moves rows
    /// (CUP, HVP, VPA, DECSTBM, IL, DL, SU, SD): the program draws the screen (nvim, Emacs, a
    /// pager), and its line feeds are cursor moves, not lines it printed. A spinner's `\r` and
    /// erase draw nothing.
    drawn: bool,
}

impl Lines {
    fn of(chunk: &[u8]) -> Self {
        let mut lines = Self {
            ends: false,
            ended: false,
            tail: false,
            drawn: false,
        };
        lines.drawn = (0..chunk.len()).any(|i| {
            let Some(rest) = chunk.get(i..).and_then(|c| c.strip_prefix(b"\x1b[")) else {
                return false;
            };
            rest.iter()
                .find(|b| (0x40..=0x7e).contains(*b))
                .is_some_and(|b| matches!(b, b'H' | b'f' | b'd' | b'r' | b'L' | b'M' | b'S' | b'T'))
        });
        for &b in chunk {
            match b {
                b'\n' => {
                    lines.ends = true;
                    lines.ended |= lines.tail;
                    lines.tail = false;
                }
                b'\r' => {}
                _ => lines.tail = true,
            }
        }
        lines
    }
}

/// A block's status says job control stopped its command (Ctrl-Z, a background read of the
/// terminal): the shell reports 128 plus the signal, whose number is this host's (zsh on macOS
/// reports 146 for SIGTSTP, on Linux 148).
fn stopped(status: i32) -> bool {
    #[cfg(unix)]
    {
        matches!(
            status.checked_sub(128),
            Some(libc::SIGTSTP | libc::SIGSTOP | libc::SIGTTIN | libc::SIGTTOU)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
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
        exec: p.exec,
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
            fresh_dylib,
        } => FdKind::File {
            regular: *regular,
            size: *size,
            dev: *dev,
            ino: *ino,
            lf_tail: *lf_tail,
            fresh_dylib: *fresh_dylib,
        },
        probe::FdKind::Device { tty, read, ctty } => FdKind::Device {
            tty: *tty,
            read: *read,
            ctty: *ctty,
        },
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

/// The engine's readings of `read` ([`job_proc`]), each of a tab's root (`roots`) with its pid
/// version (`exec`, asked of those roots alone): it tells the engine the root ran a program anew
/// (`exec zsh`).
fn job_procs(
    read: &[ProcFacts],
    roots: &BTreeSet<u32>,
    stand_in: Option<u32>,
    exec: impl Fn(u32) -> Option<u64>,
) -> Vec<JobProc> {
    read.iter()
        .map(|p| JobProc {
            version: roots.contains(&p.pid).then(|| exec(p.pid)).flatten(),
            ..job_proc(p, stand_in)
        })
        .collect()
}

/// The engine's reading of `p`, held at macOS's program check where [`member_start`] reads it so
/// (`stand_in` included).
fn job_proc(p: &ProcFacts, stand_in: Option<u32>) -> JobProc {
    JobProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        sid: p.sid,
        start: p.start,
        age_ms: p.age_ms,
        uid: p.uid,
        name: p.name.clone(),
        exe: p.exe.clone(),
        state: state(p.state),
        traced: p.traced,
        tty: p.tty,
        tpgid: p.tpgid,
        term: p.term.map(|t| TermWord {
            canonical: t.canonical,
            echo: t.echo,
            queued: t.queued,
        }),
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
        blocked_fd: p.blocked_fd,
        program_check: member_start(p, stand_in) == Some(AtStart::Held),
        version: None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The engine's cadence, and with no window the suspect rule's: a live agent call that stands
    /// suspect asks for the engine's demand; an idle engine, an exited, running or listening call,
    /// and a job that is no agent's ask nothing.
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

    /// A file name past the canary's 32 bytes is said by its first [`COMM_BYTES`], never as `-`; a
    /// multi-byte one is cut on a character.
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

    /// The canary's evidence is this session's background call quiet longest, named by its waiting
    /// process; never a foreground call, a person's job, an exited call or another session's.
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

    #[test]
    fn a_working_descendant_qualifies_only_its_wrappers_per_call_alert() {
        let board = JobsBoard::new();
        let now = Instant::now();
        let mut wrapper = call_row(4, Consumer::Agent, true, 35, 400, "observer");
        wrapper.working_descendant = Some((402, 99));
        let controller = call_row(4, Consumer::Agent, true, 34, 401, "controller");
        let active = call_row(4, Consumer::Agent, false, 1, 402, "trustc");
        let unrelated = call_row(9, Consumer::Agent, true, 50, 900, "other-tab");
        let post = |rows| {
            board.post(
                Arc::new(JobsView {
                    rows,
                    ..JobsView::default()
                }),
                BTreeMap::from([(4, "ours".into()), (9, "other".into())]),
                now,
            )
        };
        post(vec![
            wrapper.clone(),
            controller.clone(),
            active.clone(),
            unrelated,
        ]);
        assert_eq!(board.evidence("ours", now), BackgroundEvidence::Unknown);
        let (_, facts) = board.facts("ours", now).expect("fresh");
        assert_eq!(facts.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [401]);
        wrapper.working_descendant = None;
        post(vec![wrapper, controller, active]);
        let (_, facts) = board.facts("ours", now).expect("fresh");
        assert_eq!(
            facts.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
            [400, 401]
        );
    }

    /// Each tab keeps its key, number and shell, and takes the terminal word
    /// and the reader's feed handed for its key.
    #[test]
    fn a_tab_keeps_its_session_key_shell_and_number_and_its_own_terminal_word_and_feed() {
        let root = |key, label: &str| TabRoot {
            key,
            tab: key,
            shell_pid: 900 + u32::try_from(key).expect("small"),
            label: label.into(),
            program: String::new(),
            tab_id: None,
        };
        let raw = TtyMode {
            canonical: false,
            echo: false,
            stamped_ms: 7,
        };
        let fed = TabFeed {
            output_ms: Some(9),
            line_output_ms: Some(8),
            drawn_ms: Some(7),
            alt_screen: true,
            output_bytes: 120,
            block: Some(Block {
                id: 3,
                started_ms: 5,
                open: true,
                command: "make".into(),
                status: None,
                stopped: false,
            }),
            ended: Some(Block {
                id: 2,
                started_ms: 1,
                open: false,
                command: "ls".into(),
                status: Some(146),
                stopped: true,
            }),
            shell_ms: Some(4),
            typed_ms: Some(6),
        };
        let tabs = job_tabs(
            &[root(41, "Tab 3"), root(42, "Tab 4")],
            &[(41, raw)],
            &[(42, fed.clone())],
        );
        assert_eq!(
            (tabs[0].key, tabs[0].number, tabs[0].shell_pid, tabs[0].tty),
            (41, 3, 941, Some(raw))
        );
        assert_eq!(
            (tabs[0].output_ms, tabs[0].output_bytes, &tabs[0].block),
            (None, 0, &None)
        );
        assert_eq!((tabs[1].key, tabs[1].tty), (42, None));
        assert_eq!(
            TabFeed {
                output_ms: tabs[1].output_ms,
                line_output_ms: tabs[1].line_output_ms,
                drawn_ms: tabs[1].drawn_ms,
                alt_screen: tabs[1].alt_screen,
                output_bytes: tabs[1].output_bytes,
                block: tabs[1].block.clone(),
                ended: tabs[1].ended.clone(),
                shell_ms: tabs[1].shell_ms,
                typed_ms: tabs[1].typed_ms,
            },
            fed
        );
    }

    /// Every read counts its bytes; only a line feed with text of its read stamps a line (not a
    /// `\r` redraw or a typed Enter's echo, but a record led by its line feed), only a feed that
    /// moved the screen stamps output, and only one that puts the cursor on a row of its own stamps
    /// a drawing (not a spinner's erase). The alternate screen reads as the last feed left it.
    #[test]
    fn a_line_feed_stamps_a_line_and_only_a_moving_feed_stamps_output() {
        const RESET: &[u8] = b"\x1b[39m\x1b[0m";
        let epoch = Instant::now();
        let feed = JobFeed::default();
        let term = aterm_core::terminal::Terminal::new(24, 80);
        assert_eq!(feed.at(epoch), TabFeed::default(), "nothing read");
        for (bytes, moved, ms) in [
            (&b"64 bytes from 127.0.0.1\r\n"[..], true, 1_000),
            (b"\r 50%", true, 2_500),
            (RESET, false, 4_000),
            (b"\r\n", false, 5_000),
        ] {
            feed.read(bytes);
            feed.fed(&term, bytes, moved, || epoch + Duration::from_millis(ms));
        }
        let read = feed.at(epoch);
        assert_eq!(
            (read.output_ms, read.line_output_ms, read.output_bytes),
            (Some(2_500), Some(1_000), 41)
        );
        let long = [b'x'; 16];
        feed.read(&[&long[..], b"\r\n"].concat());
        feed.fed(&term, &long, true, || epoch + Duration::from_millis(6_000));
        feed.fed(&term, b"\r\n", true, || {
            epoch + Duration::from_millis(6_001)
        });
        assert_eq!(
            feed.at(epoch).line_output_ms,
            Some(6_001),
            "a line whose line feed the parse cut from its text"
        );
        let record = b"\r\nThu Oct 23 12:00:05";
        feed.read(record);
        feed.fed(&term, record, true, || epoch + Duration::from_millis(7_000));
        assert_eq!(
            feed.at(epoch).line_output_ms,
            Some(7_000),
            "a writer that leads with its line feed"
        );
        feed.fed(&term, b"", true, || epoch);
        assert_eq!(
            feed.at(epoch).output_ms,
            Some(1),
            "a stamp is at least 1 ms"
        );
        let spinner = "\r\x1b[2K\x1b[?25l\u{280b} fetching".as_bytes();
        feed.fed(&term, spinner, true, || {
            epoch + Duration::from_millis(8_000)
        });
        assert_eq!(feed.at(epoch).drawn_ms, None, "a spinner draws nothing");
        let redraw = b"\x1b[12;1Htests/test_api.py::test_x PASSED\x1b[K\r\n";
        feed.fed(&term, redraw, true, || epoch + Duration::from_millis(9_000));
        assert_eq!(
            feed.at(epoch).drawn_ms,
            Some(9_000),
            "nvim's row of its terminal"
        );
        let mut full = aterm_core::terminal::Terminal::new(24, 80);
        for (bytes, alt) in [(&b"\x1b[?1049h"[..], true), (b"\x1b[?1049l", false)] {
            full.process(bytes);
            feed.fed(&full, bytes, false, || {
                epoch + Duration::from_millis(10_000)
            });
            assert_eq!(feed.at(epoch).alt_screen, alt, "{bytes:?}");
        }
    }

    /// A writer that never stops (`kubectl logs -f`) has a line fed past each pass's clock, which a
    /// newer one overwrites before the next pass; that pass still reads it.
    #[test]
    fn a_line_fed_past_a_passs_clock_is_the_next_passs() {
        const LINE: &[u8] = b"GET /healthz 200\r\n";
        let epoch = Instant::now();
        let term = aterm_core::terminal::Terminal::new(24, 80);
        let at = |us: u64| epoch + Duration::from_micros(us);
        for offset in [100, 900] {
            let feed = JobFeed::default();
            let line = |us| {
                feed.read(LINE);
                feed.fed(&term, LINE, true, || at(us));
            };
            let pass = |ms| {
                let f = feed.at_pass(epoch, Some(ms));
                (f.line_output_ms, f.output_ms)
            };
            // The newest line and output round past the pass's millisecond.
            line(1_000_000 + offset);
            assert_eq!(pass(1_000), (None, None), "offset {offset}");
            line(2_000_000 + offset);
            assert_eq!(pass(2_000), (Some(1_001), Some(1_001)), "offset {offset}");
            assert_eq!(pass(4_000), (Some(2_001), Some(2_001)), "offset {offset}");
            line(5_000_000 + offset);
            assert_eq!(pass(5_000), (None, None), "consumed stamp, offset {offset}");
        }
        for before in [0, 1] {
            let feed = JobFeed::default();
            let line = |us| {
                feed.read(LINE);
                feed.fed(&term, LINE, true, || at(us));
            };
            line(1_000_000 - before);
            let f = feed.at_pass(epoch, Some(1_000));
            assert_eq!((f.line_output_ms, f.output_ms), (Some(1_000), Some(1_000)));
            line(2_000_100);
            let f = feed.at_pass(epoch, Some(2_000));
            assert_eq!((f.line_output_ms, f.output_ms), (None, None));
        }
    }

    /// A PASS THAT FINDS KEYS DRIVEN SINCE THE LAST STAMPS ITS CLOCK: the first
    /// pass only notes the sink's count, and the stamp stays until keys move it.
    #[cfg(unix)]
    #[test]
    fn a_pass_that_finds_keys_driven_since_the_last_stamps_its_clock() {
        let mut fds = [0; 2];
        // SAFETY: `fds` is two writable ints.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let [r, w] = fds;
        let sink = aterm_session::sink::SinkWriter::new(w);
        let feed = JobFeed::default();
        let first = feed.typed_at(1_000, sink.driven_input());
        let still = feed.typed_at(3_000, sink.driven_input());
        assert_eq!(sink.write_frame(b"x").expect("a key"), 1);
        let typed = feed.typed_at(5_000, sink.driven_input());
        let kept = feed.typed_at(7_000, sink.driven_input());
        // SAFETY: both ends are this test's.
        unsafe {
            libc::close(r);
            libc::close(w);
        }
        assert_eq!(
            (first, still, typed, kept),
            (None, None, Some(5_000), Some(5_000))
        );
    }

    /// The feed holds the newest block that ran: open from OSC 133 C with its command, closed by D
    /// with its status (D and the next A in one feed included), held through the next prompt, and
    /// kept as the ended one once a newer block runs. The shell's marks are stamped, and a line
    /// parsed with them shares their stamp; a command's output and a reset are not marks, and a
    /// reset leaves the block open until the next prompt. A block Ctrl-Z ended is stopped.
    #[test]
    fn the_feed_holds_the_newest_block_that_ran_and_its_status() {
        let epoch = Instant::now();
        let feed = JobFeed::default();
        let put = |term: &mut aterm_core::terminal::Terminal, bytes: &[u8], moved, ms| {
            term.process(bytes);
            feed.fed(term, bytes, moved, || epoch + Duration::from_millis(ms));
        };
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        put(&mut term, b"$ ls\n", false, 100);
        assert_eq!(feed.at(epoch).block, None, "no shell integration");
        put(
            &mut term,
            b"\x1b]133;A\x07$ \x1b]133;B\x07while true; do date; sleep 5; done\n",
            false,
            200,
        );
        let typed = feed.at(epoch).block.expect("the prompt's block");
        assert!(!typed.open, "typed, not run: {typed:?}");
        put(&mut term, b"\x1b]133;C\x07", false, 300);
        let running = feed.at(epoch).block.expect("the running block");
        assert_eq!(
            (
                running.id,
                running.open,
                running.started_ms,
                running.command.as_str(),
                running.status
            ),
            (
                typed.id,
                true,
                300,
                "while true; do date; sleep 5; done",
                None
            )
        );
        put(&mut term, b"Thu Oct 23\n", false, 5_300);
        assert_eq!(
            feed.at(epoch).block.as_ref(),
            Some(&running),
            "output keeps it"
        );
        put(&mut term, b"^C\x1b]133;D;130\x07", false, 9_000);
        let ended = feed.at(epoch).block.expect("the ended block");
        assert_eq!(
            (
                ended.id,
                ended.open,
                ended.status,
                ended.started_ms,
                &ended.command
            ),
            (running.id, false, Some(130), 300, &running.command)
        );
        put(&mut term, b"\x1b]133;A\x07$ ", false, 9_100);
        assert_eq!(
            feed.at(epoch).block,
            Some(ended.clone()),
            "the next prompt holds it"
        );
        put(
            &mut term,
            b"\x1b]133;B\x07make\n\x1b]133;C\x07",
            false,
            9_200,
        );
        let make = feed.at(epoch).block.expect("the next running block");
        assert!(make.id != running.id && make.open, "{make:?}");
        assert_eq!(feed.at(epoch).ended, Some(ended), "the block it replaced");
        put(&mut term, b"\x1b]133;D;2\x07\x1b]133;A\x07$ ", false, 9_900);
        let failed = feed.at(epoch).block.expect("the failed block");
        assert_eq!(
            (failed.id, failed.open, failed.status, failed.started_ms),
            (make.id, false, Some(2), 9_200),
            "D and A in one feed"
        );
        put(
            &mut term,
            b"\x1b]133;B\x07top\n\x1b]133;C\x07",
            false,
            10_000,
        );
        let top = feed.at(epoch).block.expect("top runs");
        put(
            &mut term,
            b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07less\n\x1b]133;C\x07",
            false,
            11_000,
        );
        let less = feed.at(epoch).block.expect("less runs");
        let now = feed.at(epoch);
        assert_eq!(
            now.block.map(|b| (b.open, b.command, b.started_ms)),
            Some((true, "less".to_owned(), 11_000))
        );
        assert_eq!(
            now.ended
                .map(|b| (b.id, b.open, b.status, b.command, b.started_ms)),
            Some((top.id, false, Some(0), top.command, 10_000)),
            "a block that ended and the next that ran in one feed"
        );
        assert_eq!(feed.at(epoch).output_ms, None, "no feed moved the screen");
        assert_eq!(
            (feed.at(epoch).shell_ms, feed.at(epoch).line_output_ms),
            (Some(11_000), Some(11_000)),
            "the shell's marks, and a line parsed with them shares their stamp"
        );
        put(&mut term, b"README\n", true, 12_000);
        assert_eq!(
            feed.at(epoch).shell_ms,
            Some(11_000),
            "a command's output is none of the shell's marks"
        );
        put(&mut term, b"\x1bc", true, 13_000);
        assert_eq!(
            (
                feed.at(epoch).block.map(|b| (b.id, b.open)),
                feed.at(epoch).shell_ms
            ),
            (Some((less.id, true)), Some(11_000)),
            "a reset leaves the command running, and is none of the shell's marks"
        );
        put(&mut term, b"\x1b]133;A\x07$ ", false, 14_000);
        assert_eq!(
            feed.at(epoch).block.map(|b| (b.id, b.open, b.status)),
            Some((less.id, false, None)),
            "the next prompt closes it, its D lost to the reset"
        );
        assert!(!failed.stopped, "an exit is no stop");
        #[cfg(unix)]
        {
            put(
                &mut term,
                b"\x1b]133;B\x07vim\n\x1b]133;C\x07",
                false,
                15_000,
            );
            let stop = format!("\x1b]133;D;{}\x07\x1b]133;A\x07$ ", 128 + libc::SIGTSTP);
            put(&mut term, stop.as_bytes(), false, 16_000);
            let vim = feed.at(epoch).block.expect("vim stopped");
            assert!(vim.stopped && !vim.open, "Ctrl-Z's status: {vim:?}");
        }
    }

    /// The pass on this host's real process table, this test process standing in for a tab's
    /// shell: inside the cadence it keeps its last view, an edge's reading runs the engine anyway,
    /// and a reading without a sweep keeps the last view.
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
            tab_id: None,
        }];
        let mut pass = JobsPass::new();
        let first = pass
            .tick(
                10_000,
                || 10_000,
                Some(&sweep.procs),
                &tabs,
                &[],
                &[],
                me,
                false,
                false,
            )
            .expect("a view");
        let paced = pass
            .tick(
                11_900,
                || 11_900,
                Some(&sweep.procs),
                &tabs,
                &[],
                &[],
                me,
                false,
                false,
            )
            .expect("the last view");
        assert!(Arc::ptr_eq(&first, &paced), "paced: the engine waits");
        let edge = pass
            .tick(
                11_900,
                || 11_900,
                Some(&sweep.procs),
                &tabs,
                &[],
                &[],
                me,
                false,
                true,
            )
            .expect("a new view");
        assert!(!Arc::ptr_eq(&first, &edge), "an edge's reading looks");
        assert!(
            pass.tick(11_950, || 11_950, None, &tabs, &[], &[], me, false, true)
                .is_some(),
            "a reading without a sweep keeps the last view"
        );
    }

    /// Members count by their own reading; of the rest only this user's live rows with at most
    /// 20 ms of CPU are asked, and the stand-in reads held unasked whatever its CPU.
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
        let unstarted = |translated| probe::StartFacts {
            lone_zero: true,
            translated,
            local: Some(true),
            age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
            ..probe::StartFacts::default()
        };
        let member = |pid, name: &str, exe: Option<&str>, facts| ProcFacts {
            pid,
            uid: Some(ME),
            name: name.into(),
            exe: exe.map(str::to_owned),
            start_facts: Some(facts),
            ..ProcFacts::default()
        };
        let members = [
            member(10, "zsh", Some("gh"), unstarted(false)),
            member(22, "sleep", None, probe::StartFacts::default()),
            member(23, "x86tool", None, unstarted(true)),
        ];
        let sweep = [&sweep[..], &[row(23, ME, 2, probe::RunState::Sleeping)][..]].concat();
        let asked = RefCell::new(Vec::new());
        let versions = RefCell::new(Vec::new());
        let held = held_list(
            &sweep,
            &members,
            Some(ME),
            Some(30),
            |pid| {
                asked.borrow_mut().push(pid);
                Some(if pid == 21 {
                    AtStart::Unstarted
                } else {
                    AtStart::Held
                })
            },
            |pid| {
                versions.borrow_mut().push(pid);
                Some(u64::from(pid) * 100 + 7)
            },
            |pid| pid == 20,
            |pid| (pid == 20).then(|| "codex-command-runner".to_owned()),
        );
        assert_eq!(
            asked.into_inner(),
            [20, 21],
            "only this user's live rows outside every job with at most 20 ms of CPU are asked: \
             never the 30 ms row (the stand-in's, read held unasked), another user's, a zombie \
             or a member"
        );
        assert_eq!(
            versions.into_inner(),
            [10, 23, 20, 21, 30],
            "the pid version is read of the rows the list names alone: never of member 22, \
             which started"
        );
        let got: Vec<(u32, u64, &str, bool, StartWait)> = held
            .iter()
            .map(|h| (h.pid, h.exec, h.name.as_str(), h.aterm, h.wait))
            .collect();
        assert_eq!(
            got,
            [
                (10, 1007, "gh", true, StartWait::ProgramCheck),
                (23, 2307, "x86tool", true, StartWait::Translating),
                (
                    20,
                    2007,
                    "codex-command-runner",
                    true,
                    StartWait::ProgramCheck
                ),
                (21, 2107, "p21", false, StartWait::NotStarted),
                (30, 3007, "p30", false, StartWait::ProgramCheck),
            ],
            "each named by its executable where one reads, never by p_comm's 16 bytes, with what \
             keeps it; the stand-in's 30 ms row counted"
        );
        assert!(
            held_list(
                &sweep,
                &members,
                None,
                Some(30),
                |_| Some(AtStart::Held),
                |_| None,
                |_| true,
                |_| None,
            )
            .is_empty(),
            "no user read, nothing counted"
        );
    }

    /// The pass hands the engine its held list: held rows and the stand-in reach the program-check
    /// findings, a translating row `translating`, and only those in this process's coalition count
    /// as aterm's.
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
                Some(match pid {
                    20 | 23 => AtStart::Held,
                    22 => AtStart::Translating,
                    _ => AtStart::Started,
                })
            },
            exec: |pid| Some(u64::from(pid)),
            coalition: |pid| Some(if pid == 23 { 8 } else { 7 }),
            exe_name: |pid| (pid == 20).then(|| "codex-command-runner".to_owned()),
            stand_in: || Some(10),
        };
        let view = pass
            .tick(
                10_000,
                || 10_000,
                Some(&sweep),
                &[],
                &[],
                &[],
                10,
                false,
                true,
            )
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
                (10, "p10", None),
                (20, "codex-command-runner", None),
                (23, "p23", None)
            ],
            "the reader's held rows and the stand-in's, outside every tab"
        );
        assert_eq!(
            view.findings
                .translating
                .iter()
                .map(|n| n.pid)
                .collect::<Vec<_>>(),
            [22],
            "the translating row under its own kind"
        );
        assert_eq!(
            view.findings.program_check_aterm,
            (2, 0),
            "aterm's: the two in this process's coalition, not 23 in another"
        );
    }

    /// With this process's coalition unread, no held program outside every job counts as aterm's:
    /// one whose own coalition is unread too is no match.
    #[test]
    fn with_this_process_coalition_unread_no_held_program_is_aterms() {
        const ME: u32 = 501;
        let row = |pid, cpu_ms: u64| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |_, _| Some(AtStart::Held),
            coalition: |_| None,
            ..HeldReaders::INERT
        };
        let view = pass
            .tick(
                10_000,
                || 10_000,
                Some(&[row(10, 400), row(20, 2)]),
                &[],
                &[],
                &[],
                10,
                false,
                true,
            )
            .expect("a view");
        assert_eq!(
            view.findings
                .program_check
                .iter()
                .map(|n| n.pid)
                .collect::<Vec<_>>(),
            [20],
            "held"
        );
        assert_eq!(view.findings.program_check_aterm.0, 0, "not aterm's");
    }

    /// Only a tab's root is asked its pid version, and only its reading carries one.
    #[test]
    fn only_a_tabs_root_is_asked_its_pid_version() {
        use std::cell::RefCell;
        let read = [
            ProcFacts {
                pid: 10,
                ..ProcFacts::default()
            },
            ProcFacts {
                pid: 11,
                ..ProcFacts::default()
            },
        ];
        let asked = RefCell::new(Vec::new());
        let procs = job_procs(&read, &BTreeSet::from([10]), None, |pid| {
            asked.borrow_mut().push(pid);
            Some(u64::from(pid) * 100 + 7)
        });
        assert_eq!(
            procs.iter().map(|p| (p.pid, p.version)).collect::<Vec<_>>(),
            [(10, Some(1007)), (11, None)]
        );
        assert_eq!(asked.into_inner(), [10]);
    }

    /// The stand-in reads held only where the reading judged a start (never a zombie's or another
    /// user's), and a member the kernel reads held stays held.
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

    /// A view speaks for now for a minute, and a parked one for as long as its reader runs; one
    /// that holds a wait is read on a timer, not on edges, so past the minute it does not.
    #[test]
    fn a_parked_view_speaks_for_now_while_its_reader_runs() {
        use aterm_vitals::jobs::{Findings, HeldEpisode, Named};
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
        let held = JobsView {
            findings: Findings {
                program_check: vec![Named {
                    pid: 25311,
                    name: "gh".into(),
                    tab: None,
                    for_ms: 35_940_000,
                }],
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let episode = JobsView {
            findings: Findings {
                program_check_episode: Some(HeldEpisode::default()),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let translating = JobsView {
            findings: Findings {
                translating: held.findings.program_check.clone(),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let unstarted = JobsView {
            findings: Findings {
                not_started: held.findings.program_check.clone(),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        for view in [&held, &episode, &translating, &unstarted] {
            assert_eq!(view.demand, Demand::Idle);
            assert!(speaks_for_now(view, JOBS_FRESH, true), "{view:?}");
            assert!(
                !speaks_for_now(view, late, true),
                "a held wait is read on a timer, not on edges: {view:?}"
            );
        }
    }

    /// Privacy's count: every held program of this user, aterm's and others', and the longest held
    /// to now; nothing from a view that does not speak for now.
    #[test]
    fn privacys_count_is_every_held_program_and_the_longest_wait_to_now() {
        use aterm_vitals::jobs::{Findings, Named};
        let named = |pid, for_ms| Named {
            pid,
            name: "gh".into(),
            tab: None,
            for_ms,
        };
        let view = JobsView {
            findings: Findings {
                program_check: vec![named(25311, 35_940_000), named(25312, 20_000)],
                program_check_aterm: (1, 20_000),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        assert_eq!(
            program_check_of(&view, Duration::from_millis(1_500), false),
            Some((2, 35_941_500)),
            "both held, the oldest plus the view's age"
        );
        assert_eq!(
            program_check_of(&JobsView::default(), Duration::ZERO, false),
            Some((0, 0)),
            "none held"
        );
        assert_eq!(
            program_check_of(&view, JOBS_FRESH + Duration::from_secs(1), true),
            None,
            "a view that does not speak for now"
        );
    }

    /// This process lives at its own start, a start one unit off is another process, and a pid no
    /// process holds lives at no start (the negative control).
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
