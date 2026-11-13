// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S ACTION TELEMETRY (Continuum TM-0, `aterm_telemetry`): one row for each action
//! this aterm watches, from the readings it already takes, and the `telemetry` verb that reads
//! them back.
//!
//! * A tab's command (`kind=command`): the status sweep's block read ([`commands_seen`]) closes
//!   each OSC 133 block once. Its wall time is the shell's own C and D stamps; its CPU is what
//!   the shell and the processes it reaped spent between the reader seeing C and seeing D
//!   ([`CmdMeter::slice`], one `proc_pid_rusage` of the shell at each edge); its peak memory and
//!   a `stalled` stop are the jobs engine's readings of the block's job ([`jobs_passed`]).
//! * An agent's tool call (`kind=agent-call`): the jobs engine's `agent-call` jobs as they end
//!   ([`jobs_passed`]), every figure sampled at its passes but, where its leader's exit was read,
//!   its end. How and when it ended is its leader's wait status and the moment the kernel
//!   reported it, which kernel exit watches on the leader read though aterm is not its parent
//!   ([`CallWatch`]); its label is the program the call ran, never the agent's shell wrapper.
//! * A worker spawn (`kind=spawn`): the `spawn` verb's answer ([`spawned`]).
//!
//! Nothing here samples anything: every figure is a reading another part of the window takes
//! anyway, and a figure no reading covers is written unknown with the source that would supply
//! it. Rows go to `aterm_telemetry`'s writer thread, so no session waits on the disk.

use std::collections::{BTreeMap, VecDeque};
#[cfg(target_vendor = "apple")]
use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
#[cfg(target_vendor = "apple")]
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_telemetry::{Digest, Fig, Kind, Meter, Outcome, Row, Stop};

/// What a figure no reading covers names as its source.
pub(crate) mod source {
    /// A command's CPU: the shell's rusage at its C and D, which the reader did not see both of.
    pub(crate) const SHELL_RUSAGE: &str = "shell-rusage";
    /// Peak memory and an agent call's CPU: the jobs engine's passes, none of which read it.
    pub(crate) const JOBS: &str = "jobs";
    /// Wall time: the shell's OSC 133 stamps.
    pub(crate) const OSC133: &str = "osc133";
}

/// This process's host: its name and facts' digest, once the writer started.
static HOST: OnceLock<Option<(String, Digest)>> = OnceLock::new();

/// Start this process's writer, once: on the log under the state directory, with the host's facts
/// read now. A unit-test build starts none, so no test writes into a real state directory.
pub(crate) fn start() {
    if cfg!(test) {
        return;
    }
    HOST.get_or_init(|| {
        aterm_telemetry::start_default().map(|facts| {
            let digest = facts.digest();
            (facts.host, digest)
        })
    });
}

fn host() -> Option<(&'static str, Digest)> {
    HOST.get()
        .and_then(Option::as_ref)
        .map(|(name, digest)| (name.as_str(), *digest))
}

fn emit(row: Row) {
    aterm_telemetry::emit(row);
}

/// The most blocks a [`CmdMeter`] keeps readings for.
const METER_KEEP: usize = 64;

/// A session's command edges as its PTY reader saw them: the shell's CPU (its own and its reaped
/// children's) and its children when a block began executing and when it stopped. Written by the
/// reader under the terminal lock ([`Self::slice`]), taken by the status sweep once the block's
/// row goes out.
///
/// The shell's reaped-children CPU counts every child it reaped in the window, a background job
/// of an earlier command's among them. So a block's CPU is kept only when no job that ran beside
/// it was reaped in its window: a child of the shell's at the prompt before the block (its
/// previous D) that still ran at the block's C and was gone at its D took its CPU into the
/// window, and the block's CPU is then unknown, never another job's charged to it. A prompt
/// helper that lives on (in both lists) costs nothing; a child of the block's own (born after
/// the prompt) is its own. A job the block left running spends its CPU after the block, which
/// its row does not hold.
#[derive(Debug, Default)]
pub(crate) struct CmdMeter(Mutex<MeterState>);

#[derive(Debug, Default)]
struct MeterState {
    /// Block id → its start.
    started: BTreeMap<u64, Start>,
    /// Block id → its CPU, ns; `None` where it cannot be told from another job's.
    ended: BTreeMap<u64, Option<u64>>,
    /// The shell's children when the newest block stopped: at the prompt, the jobs it kept.
    prompt: Option<Vec<u32>>,
    /// The newest block a command row went out for.
    reported: Option<u64>,
}

/// A block's start: the shell's CPU, ns, and the jobs that ran beside it (the prompt's children
/// still there at its C; `None` where either list was not read).
#[derive(Debug)]
struct Start {
    cpu: u64,
    beside: Option<Vec<u32>>,
}

/// One reading at an edge: the shell's CPU with its reaped children's, ns, and its children,
/// sorted (`None` where the kernel did not list them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edge {
    pub(crate) cpu: u64,
    pub(crate) children: Option<Vec<u32>>,
}

/// The terminal's current block and whether it is executing, read before a slice is parsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockMark(Option<(u64, bool)>);

/// [`BlockMark`] of `t` now.
pub(crate) fn mark(t: &Terminal) -> BlockMark {
    BlockMark(
        t.current_block()
            .map(|b| (b.id, b.state == aterm_types::BlockState::Executing)),
    )
}

impl BlockMark {
    fn executing(self) -> Option<u64> {
        self.0.filter(|(_, runs)| *runs).map(|(id, _)| id)
    }
}

fn keep_newest<V>(map: &mut BTreeMap<u64, V>) {
    while map.len() > METER_KEEP {
        map.pop_first();
    }
}

impl CmdMeter {
    /// After a slice: where the block executing changed since `before`, read the shell `shell`
    /// once ([`Edge`]) and note it as the end of the block that stopped and the start of the one
    /// that began. A block whose C and D came in one slice gets no reading: its CPU stays unknown
    /// rather than read as none.
    pub(crate) fn slice(&self, before: BlockMark, t: &Terminal, shell: i32) {
        self.slice_with(before, mark(t), shell, read_shell);
    }

    fn slice_with(
        &self,
        before: BlockMark,
        after: BlockMark,
        shell: i32,
        read: impl FnOnce(i32) -> Option<Edge>,
    ) {
        let (was, now) = (before.executing(), after.executing());
        if was == now {
            return;
        }
        let Some(edge) = read(shell) else {
            return;
        };
        let mut st = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = was {
            if let Some(start) = st.started.remove(&id) {
                let alone = match (&start.beside, &edge.children) {
                    (Some(beside), Some(now)) => {
                        beside.iter().all(|pid| now.binary_search(pid).is_ok())
                    }
                    _ => false,
                };
                let cpu = edge.cpu.checked_sub(start.cpu).filter(|_| alone);
                st.ended.insert(id, cpu);
                keep_newest(&mut st.ended);
            }
            st.prompt.clone_from(&edge.children);
        }
        if let Some(id) = now {
            let beside = match (&st.prompt, &edge.children) {
                (Some(prompt), Some(at_c)) => Some(
                    prompt
                        .iter()
                        .copied()
                        .filter(|pid| at_c.binary_search(pid).is_ok())
                        .collect(),
                ),
                _ => None,
            };
            st.started.insert(
                id,
                Start {
                    cpu: edge.cpu,
                    beside,
                },
            );
            keep_newest(&mut st.started);
        }
    }

    /// The CPU block `id` spent, ns, once both its readings are in and no other job's is in it.
    fn take_cpu(&self, id: u64) -> Option<u64> {
        let mut st = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        st.started.remove(&id);
        st.ended.remove(&id).flatten()
    }

    /// Whether block `id` is past the newest one reported, and mark it reported.
    fn report(&self, id: u64) -> bool {
        let mut st = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if st.reported.is_some_and(|r| id <= r) {
            return false;
        }
        st.reported = Some(id);
        true
    }
}

fn read_shell(shell: i32) -> Option<Edge> {
    let pid = u32::try_from(shell).ok().filter(|p| *p > 1)?;
    Some(Edge {
        cpu: aterm_sysprobe::jobs::cpu_with_reaped_ns(pid)?,
        children: aterm_sysprobe::jobs::child_pids(pid),
    })
}

/// Each session's public id (`s-…`, what `aterm ctl @<sid>` and the survey name) by the window's
/// local key, as the status sweep last read it from the session store ([`note_sid`]): a row
/// names its session by the id a reader can join it on. The newest [`SIDS_KEEP`].
static SIDS: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());

const SIDS_KEEP: usize = 512;

/// The status sweep read session `local`'s public id `sid`.
pub(crate) fn note_sid(local: u64, sid: &str) {
    let mut sids = SIDS.lock().unwrap_or_else(PoisonError::into_inner);
    if sids.get(&local).is_some_and(|s| s == sid) {
        return;
    }
    sids.insert(local, sid.to_owned());
    while sids.len() > SIDS_KEEP {
        sids.pop_first();
    }
}

/// Session `local`'s public id, once the sweep noted it.
fn sid_of(local: u64) -> Option<String> {
    SIDS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&local)
        .cloned()
}

/// The jobs engine's readings of each block's foreground job: (session, block) → (peak bytes,
/// judged stuck by the last pass that read it live), the newest [`BLOCK_JOBS_KEEP`].
static BLOCK_JOBS: Mutex<BlockJobs> = Mutex::new(BlockJobs {
    map: BTreeMap::new(),
    order: VecDeque::new(),
});

const BLOCK_JOBS_KEEP: usize = 512;

struct BlockJobs {
    map: BTreeMap<(u64, u64), (Option<u64>, bool)>,
    order: VecDeque<(u64, u64)>,
}

fn take_block_job(session: u64, block: u64) -> Option<(Option<u64>, bool)> {
    let mut jobs = BLOCK_JOBS.lock().unwrap_or_else(PoisonError::into_inner);
    let got = jobs.map.remove(&(session, block));
    if got.is_some() {
        jobs.order.retain(|k| *k != (session, block));
    }
    got
}

/// After a jobs pass whose clock reads `clock` (host ms, the clock its moments are on): keep what
/// it read of each block's job for that block's command row, and write a row for each agent call
/// that ended, counted on `calls`'s counts and written where `calls` writes.
pub(crate) fn jobs_passed(
    jobs: &mut aterm_vitals::jobs::Jobs,
    clock: impl Fn() -> u64,
    calls: &CallWatch,
) {
    {
        let mut kept = BLOCK_JOBS.lock().unwrap_or_else(PoisonError::into_inner);
        for m in jobs.block_meters() {
            let key = (m.tab, m.block);
            if kept.map.insert(key, (m.peak_bytes, m.stuck)).is_none() {
                kept.order.push_back(key);
            }
        }
        while kept.order.len() > BLOCK_JOBS_KEEP {
            if let Some(old) = kept.order.pop_front() {
                kept.map.remove(&old);
            }
        }
    }
    // Both clocks read together, after everything the pass read: every moment its calls carry
    // (the pass's own clock, read before its sweep; a leader's exit, read before this) is at or
    // before the first, so none is written past the second.
    let now = (clock(), aterm_telemetry::now_ms());
    calls_ended(
        &jobs.take_ended(),
        now,
        calls.counts,
        (calls.rows.host)(),
        calls.rows.emit,
    );
}

/// The jobs that `ended` by a pass, with its clock and the wall clock read together then
/// (`now`): each agent call that ended with no status counted missed on `counts`, whether or not
/// this process writes rows, and each agent call's row handed to `emit` where it has a `host` to
/// name.
fn calls_ended(
    ended: &[aterm_vitals::jobs::Ended],
    now: (u64, u64),
    counts: &CallCounts,
    host: Option<(&str, Digest)>,
    mut emit: impl FnMut(Row),
) {
    counts.missed(ended);
    let Some((host_name, facts)) = host else {
        return;
    };
    for e in ended {
        if let Some(row) = agent_call_row(e, now, host_name, facts, sid_of(e.tab)) {
            emit(row);
        }
    }
}

/// Agent call `e`'s row, its moments put on the wall clock through `(pass_now, wall_now)`, the
/// pass's clock and the wall clock read together; `None` for any other kind.
fn agent_call_row(
    e: &aterm_vitals::jobs::Ended,
    (pass_now, wall_now): (u64, u64),
    host_name: &str,
    facts: Digest,
    sid: Option<String>,
) -> Option<Row> {
    use aterm_vitals::jobs::ExitOf;
    if e.kind != aterm_vitals::jobs::JobKind::AgentCall {
        return None;
    }
    // A moment on the pass's clock as a wall time: as far before `wall_now` as it is before
    // `pass_now`. Every moment was read before the pair, so none maps past `wall_now`, the row's
    // own making; one that were would be written at it.
    let to_wall = |ms: u64| wall_now.saturating_sub(pass_now.saturating_sub(ms));
    // A watched call ended when its leader did, as the watch stamped it; any other at the pass
    // that found it gone, which may come long after (a windowless instance reads at its edges).
    let end_ms = e.leader_exited_ms.unwrap_or(e.ended_ms);
    // Its leader's own life where both its ends were read (its start from its age at a pass, its
    // end the watch's moment); else pass to pass, from the first that tracked it to the one that
    // found it gone. Never one end of each: a pass reads each end late, so a span between two
    // passes is late at both ends alike, where a pass's end against an exact one is off by that
    // pass's lag alone.
    let wall_ms = match (e.leader_born_ms, e.leader_exited_ms) {
        (Some(born), Some(exited)) => exited.saturating_sub(born),
        _ => e.ended_ms.saturating_sub(e.opened_ms),
    };
    // The program the call ran, as the kernel named it: a call no pass saw run one (shell
    // builtins alone) has no label and no key, as a command no read confirmed has none.
    let program = e.ran.as_deref();
    let what = if e.command.trim().is_empty() {
        program
    } else {
        program.map(|_| e.command.as_str())
    };
    let (outcome, mut stop, code, sig) = match e.ended_by {
        Some(ExitOf::Code(0)) => (Outcome::Ok, Stop::Exit, Some(0), None),
        Some(ExitOf::Code(c)) => (Outcome::Fail, Stop::Exit, Some(c), None),
        Some(ExitOf::Signal(s)) => (Outcome::Fail, Stop::Signal, None, Some(s)),
        None => (Outcome::Unknown, Stop::Unknown, None, None),
    };
    if e.stuck {
        stop = Stop::Stalled;
    }
    Some(Row {
        at_ms: to_wall(end_ms),
        kind: Kind::AgentCall,
        key: what.map(|what| Digest::key(&Kind::AgentCall, what, None)),
        dir: None,
        label: aterm_telemetry::row::label(program.unwrap_or("")),
        sid,
        on: None,
        host: host_name.to_owned(),
        facts,
        wall_ms: Fig::Known(wall_ms),
        cpu_ms: e.cpu_ns.map_or_else(
            || Fig::unknown(source::JOBS),
            |ns| Fig::Known(ns / 1_000_000),
        ),
        peak_bytes: e
            .peak_bytes
            .map_or_else(|| Fig::unknown(source::JOBS), Fig::Known),
        wait_ms: Fig::Na,
        cache: Fig::Na,
        outcome,
        stop,
        code,
        sig,
        meter: Meter::Sampled,
    })
}

/// The most agent calls' leaders a [`CallWatch`] watches at once (each two kqueues, the status's
/// and the moment's, and the moment's pipe and thread, [`Watched`]): past it a new call goes
/// unwatched until a watch frees, and one that ends unwatched ends with no status, counted
/// missed.
#[cfg(target_vendor = "apple")]
pub(crate) const CALL_WATCH_CAP: usize = 64;

/// How long a pass waits, all told, for the exit events of watched leaders the sweep no longer
/// lists: macOS stops listing a dying process's pid before it queues the exit's event (1 µs to
/// 1.2 ms later under load).
#[cfg(target_vendor = "apple")]
pub(crate) const GONE_WAIT: Duration = Duration::from_millis(10);

/// What this process's agent-call watches did, for the `telemetry` verb's `calls` line: the
/// window's one [`CallWatch`] counts here.
static CALLS: CallCounts = CallCounts::new();

/// What a [`CallWatch`] did, since its process started.
#[derive(Debug)]
pub(crate) struct CallCounts {
    /// Leaders a watch was registered on and proved to be the call's ([`CallWatch::watch`]).
    watched: AtomicU64,
    /// Watched leaders whose status was read ([`CallWatch::read`]).
    read: AtomicU64,
    /// Agent calls that ended with no status ([`jobs_passed`]): unwatched (past the cap, gone
    /// before a pass saw them twice, their start unread or another process's), or watched with
    /// none read by the pass that found them gone.
    missed: AtomicU64,
}

impl CallCounts {
    pub(crate) const fn new() -> Self {
        Self {
            watched: AtomicU64::new(0),
            read: AtomicU64::new(0),
            missed: AtomicU64::new(0),
        }
    }

    /// `(watched, read, missed)`.
    pub(crate) fn get(&self) -> (u64, u64, u64) {
        (
            self.watched.load(Ordering::Relaxed),
            self.read.load(Ordering::Relaxed),
            self.missed.load(Ordering::Relaxed),
        )
    }

    /// The agent calls of `ended` that ended with no status, counted missed.
    fn missed(&self, ended: &[aterm_vitals::jobs::Ended]) {
        let n = ended
            .iter()
            .filter(|e| e.kind == aterm_vitals::jobs::JobKind::AgentCall && e.ended_by.is_none())
            .count() as u64;
        self.missed.fetch_add(n, Ordering::Relaxed);
    }
}

/// What this process's agent-call watches counted: `(watched, read, missed)`.
pub(crate) fn call_counts() -> (u64, u64, u64) {
    CALLS.get()
}

/// Where a [`CallWatch`]'s rows go: the host they are named by (`None`: no writer, no row) and
/// what each is handed to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallRows {
    /// The host's name and facts' digest ([`host`]).
    pub(crate) host: fn() -> Option<(&'static str, Digest)>,
    /// The writer ([`emit`]).
    pub(crate) emit: fn(Row),
}

impl CallRows {
    /// This process's writer, once it started ([`start`]).
    pub(crate) const WINDOW: Self = Self { host, emit };
}

#[cfg(target_vendor = "apple")]
fn count(n: &AtomicU64) {
    n.fetch_add(1, Ordering::Relaxed);
}

/// A kernel exit watch on each live agent call's leader ([`aterm_vitals::jobs::JobRow::leader`]),
/// on the jobs pass's thread: aterm is not the leader's parent, but a process of the same user may
/// watch it (`EVFILT_PROC` `NOTE_EXIT | NOTE_EXITSTATUS`), and the status it reads is the call's
/// own, which the agent alone reaps. Each watch is two on the one pid ([`Watched`]): the pass
/// reads the status from an [`aterm_uds::exitwatch::ExitWatch`], and an
/// [`aterm_uds::exitwatch::ExitMoment`]'s thread stamps the moment the kernel reports the exit, so
/// the call's end is its own, however long before the pass that reads it. [`Self::watch`]
/// registers a watch after each pass on each new call, then re-reads the leader's start, so a pid
/// taken anew is never watched as the call's; [`Self::read`] hands the next pass each status read,
/// decoded, with its moment on the pass's clock ([`aterm_vitals::jobs::LeaderExit`]). Off Apple
/// platforms there is no such watch, and every call ends with no status.
#[derive(Debug)]
pub(crate) struct CallWatch {
    /// Where it counts what it did: [`CALLS`], which the `telemetry` verb reads.
    counts: &'static CallCounts,
    /// Where its calls' rows go ([`jobs_passed`]): [`CallRows::WINDOW`].
    rows: CallRows,
    #[cfg(target_vendor = "apple")]
    live: bool,
    #[cfg(target_vendor = "apple")]
    watches: BTreeMap<(u32, u64), Watched>,
    /// Open calls whose leader no watch may be the call's: gone before the watch, or another
    /// process than the start the row read. Never asked again while the call is open.
    #[cfg(target_vendor = "apple")]
    refused: BTreeSet<(u32, u64)>,
}

/// One leader's watches. The kernel posts a pid's exit to every kqueue watching it at once, so
/// both report the one exit: the status the pass reads, and the moment a thread stamped as the
/// kernel reported it. The pass never waits on the thread: a status it reads before the stamp
/// lands (or with no moment's watch, its thread not started) takes the read's own instant.
#[cfg(target_vendor = "apple")]
#[derive(Debug)]
struct Watched {
    exit: aterm_uds::exitwatch::ExitWatch,
    moment: Option<aterm_uds::exitwatch::ExitMoment<Instant>>,
}

impl CallWatch {
    /// A watch; `live` false makes one that never registers (a unit test's jobs pass, whose
    /// scripted pids are no process of its own).
    pub(crate) fn new(live: bool) -> Self {
        Self::counting(live, &CALLS)
    }

    /// [`Self::new`], counting on `counts`.
    pub(crate) fn counting(live: bool, counts: &'static CallCounts) -> Self {
        #[cfg(not(target_vendor = "apple"))]
        let _ = live;
        Self {
            counts,
            rows: CallRows::WINDOW,
            #[cfg(target_vendor = "apple")]
            live,
            #[cfg(target_vendor = "apple")]
            watches: BTreeMap::new(),
            #[cfg(target_vendor = "apple")]
            refused: BTreeSet::new(),
        }
    }

    /// [`Self::counting`], its rows written through `rows` (a test's own writer).
    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) fn writing(live: bool, counts: &'static CallCounts, rows: CallRows) -> Self {
        Self {
            rows,
            ..Self::counting(live, counts)
        }
    }

    /// Before a pass, after its sweep: the status of each watched leader the kernel has queued,
    /// its watch let go, with the moment its exit was stamped as `now_ms` (the pass's clock now)
    /// reads it. A leader the sweep no longer lists (`listed` false) is waited on briefly, all
    /// such leaders within [`GONE_WAIT`] together; one still without a status keeps its watch,
    /// which [`Self::watch`] lets go once the engine no longer holds its call open.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn read(
        &mut self,
        listed: impl Fn(u32) -> bool,
        now_ms: impl Fn() -> u64,
    ) -> Vec<aterm_vitals::jobs::LeaderExit> {
        self.read_within(listed, now_ms, GONE_WAIT)
    }

    /// [`Self::read`], waiting up to `gone_wait` for the leaders the sweep no longer lists.
    #[cfg(target_vendor = "apple")]
    fn read_within(
        &mut self,
        listed: impl Fn(u32) -> bool,
        now_ms: impl Fn() -> u64,
        gone_wait: Duration,
    ) -> Vec<aterm_vitals::jobs::LeaderExit> {
        let deadline = Instant::now() + gone_wait;
        let counts = self.counts;
        let mut read = Vec::new();
        self.watches.retain(|&(pid, start), watch| {
            let raw = watch.exit.exit_status().or_else(|| {
                if listed(pid) {
                    None
                } else {
                    watch
                        .exit
                        .exit_status_within(deadline.saturating_duration_since(Instant::now()))
                }
            });
            let Some(end) = raw.and_then(exit_of) else {
                return true;
            };
            let read_at = Instant::now();
            let moment = watch
                .moment
                .as_ref()
                .and_then(aterm_uds::exitwatch::ExitMoment::get)
                .map_or(read_at, |m| m.min(read_at));
            count(&counts.read);
            read.push((pid, start, end, moment));
            false
        });
        // One reading of both clocks puts every moment on the pass's.
        let (now, now_ms) = (Instant::now(), now_ms());
        read.into_iter()
            .map(|(pid, start, end, moment)| {
                let ago = u64::try_from(now.saturating_duration_since(moment).as_millis())
                    .unwrap_or(u64::MAX);
                aterm_vitals::jobs::LeaderExit {
                    pid,
                    start: Some(start),
                    end,
                    at_ms: now_ms.saturating_sub(ago),
                }
            })
            .collect()
    }

    /// See the Apple twin: no exit watch here.
    #[cfg(not(target_vendor = "apple"))]
    pub(crate) fn read(
        &mut self,
        _listed: impl Fn(u32) -> bool,
        _now_ms: impl Fn() -> u64,
    ) -> Vec<aterm_vitals::jobs::LeaderExit> {
        Vec::new()
    }

    /// After a pass: a watch on each live agent call's leader `rows` names that none holds yet,
    /// proved after registering to be the process whose start the row read
    /// ([`aterm_uds::process::read_birth`]); and the watches of calls `rows` no longer holds
    /// open let go.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn watch(&mut self, rows: &[aterm_vitals::jobs::JobRow]) {
        self.watch_with(rows, |pid| {
            i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_birth)
                .map(|b| b.0)
        });
    }

    /// See the Apple twin: no exit watch here.
    #[cfg(not(target_vendor = "apple"))]
    pub(crate) fn watch(&mut self, _rows: &[aterm_vitals::jobs::JobRow]) {}

    #[cfg(target_vendor = "apple")]
    fn watch_with(
        &mut self,
        rows: &[aterm_vitals::jobs::JobRow],
        birth: impl Fn(u32) -> Option<u64>,
    ) {
        if !self.live {
            return;
        }
        // An open call with no start read takes no watch: a status could be another process's
        // given the pid. A later pass that reads its start watches it then.
        let open: HashSet<(u32, u64)> = rows
            .iter()
            .filter(|r| {
                r.kind == aterm_vitals::jobs::JobKind::AgentCall
                    && r.verdict != aterm_vitals::jobs::Verdict::Exited
            })
            .filter_map(|r| Some((r.leader.0, r.leader.1?)))
            .collect();
        self.watches.retain(|k, _| open.contains(k));
        self.refused.retain(|k| open.contains(k));
        let mut new: Vec<(u32, u64)> = open
            .into_iter()
            .filter(|k| !self.watches.contains_key(k) && !self.refused.contains(k))
            .collect();
        new.sort_unstable();
        for (pid, start) in new {
            if self.watches.len() >= CALL_WATCH_CAP {
                break;
            }
            // Registered first, then the start re-read: the watches hold the exit of whatever
            // process had the pid as they registered, which the re-read proves is the call's. A
            // moment's watch that fails (no thread) leaves the status's, read at the pass.
            let watch = aterm_uds::exitwatch::ExitWatch::watch(pid).map(|exit| Watched {
                exit,
                moment: aterm_uds::exitwatch::ExitMoment::watch(pid, Instant::now),
            });
            match watch {
                Some(w) if birth(pid) == Some(start) => {
                    count(&self.counts.watched);
                    self.watches.insert((pid, start), w);
                }
                // Gone before the watch (its call ends with no status), or a pid taken anew.
                _ => {
                    self.refused.insert((pid, start));
                }
            }
        }
    }

    /// How many leaders it watches now.
    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) fn watching(&self) -> usize {
        self.watches.len()
    }

    /// How many of its watches have stamped an exit not read yet.
    #[cfg(all(test, target_vendor = "apple"))]
    fn stamps(&self) -> usize {
        self.watches
            .values()
            .filter(|w| w.moment.as_ref().is_some_and(|m| m.get().is_some()))
            .count()
    }

    /// Whether its watch on leader `pid` has stamped an exit not read yet; `None` while it watches
    /// no leader of that pid.
    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) fn stamped(&self, pid: u32) -> Option<bool> {
        self.watches
            .iter()
            .find(|((p, _), _)| *p == pid)
            .map(|(_, w)| w.moment.as_ref().is_some_and(|m| m.get().is_some()))
    }
}

/// A leader's `wait(2)` status as how it ended: an exit's code or the signal that ended it;
/// `None` for any other status (a stop, which an exit watch never reports).
#[cfg(target_vendor = "apple")]
fn exit_of(raw: i32) -> Option<aterm_vitals::jobs::ExitOf> {
    use aterm_vitals::jobs::ExitOf;
    match crate::spawn::child_exit_of_wait_status(raw)? {
        aterm_pty::ChildExit::Code(c) => Some(ExitOf::Code(c)),
        aterm_pty::ChildExit::Signal(s) => Some(ExitOf::Signal(i32::from(s))),
    }
}

/// The status sweep's block read of session `session` (under its terminal guard `term`): a row for
/// each block that stopped executing since the last one reported.
pub(crate) fn commands_seen(
    meter: &CmdMeter,
    session: u64,
    term: &Terminal,
    read: &crate::session_log::BlockRead,
) {
    use crate::session_log::BlockEnd;
    let Some((host_name, facts)) = host() else {
        return;
    };
    let sid = sid_of(session);
    for fact in &read.facts {
        if matches!(fact.end, BlockEnd::Running) || !meter.report(fact.id) {
            continue;
        }
        let block = term.block_by_id(fact.id);
        let line = block
            .and_then(|b| b.commandline.as_deref())
            .map(str::trim)
            .unwrap_or("");
        let dir = block.and_then(|b| b.working_directory.as_deref());
        let Some(blk) = block_facts(fact, line, dir) else {
            continue;
        };
        emit(command_row(
            &blk,
            meter.take_cpu(fact.id),
            take_block_job(session, fact.id),
            (sid.as_deref(), host_name, facts),
        ));
    }
}

/// What a closed block's `fact` says of its command, its line `line` run in `dir`; `None` while
/// it runs. Its program is the block's detail ([`crate::session_status::command_detail`]): a
/// shell builtin or keyword from the reader's fixed list, or a program the kernel saw run in its
/// window (the command's own, or the wrapper or interpreter that ran it); never a word only
/// typed, since a line's first word may be a password typed at a prompt that read no echo.
fn block_facts<'a>(
    fact: &'a crate::session_log::BlockFact,
    line: &'a str,
    dir: Option<&'a str>,
) -> Option<BlockFacts<'a>> {
    use crate::session_log::BlockEnd;
    let (exit, ended_ms) = match fact.end {
        BlockEnd::Running => return None,
        BlockEnd::Done(done) => (Some(done.exit_code), done.ended_ms),
        BlockEnd::Unended { ended_ms } => (None, ended_ms),
    };
    let program = fact
        .detail
        .as_deref()
        .and_then(|d| d.split_whitespace().next());
    Some(BlockFacts {
        line,
        dir,
        program,
        started_ms: fact.started_ms,
        ended_ms,
        exit,
    })
}

/// What a closed block says of its command.
struct BlockFacts<'a> {
    line: &'a str,
    dir: Option<&'a str>,
    program: Option<&'a str>,
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
    /// `None`: the block closed with no D (its status unknown); `Some(status)` with D.
    exit: Option<Option<i32>>,
}

fn command_row(
    b: &BlockFacts<'_>,
    cpu_ns: Option<u64>,
    job: Option<(Option<u64>, bool)>,
    (sid, host_name, facts): (Option<&str>, &str, Digest),
) -> Row {
    let status = b.exit.flatten();
    let (mut stop, code, sig) = Stop::of_shell_status(status);
    let stuck = job.is_some_and(|(_, stuck)| stuck);
    if stuck {
        stop = Stop::Stalled;
    }
    let outcome = match status {
        Some(0) => Outcome::Ok,
        Some(_) => Outcome::Fail,
        None => Outcome::Unknown,
    };
    let at_ms = b
        .ended_ms
        .or(b.started_ms)
        .unwrap_or_else(aterm_telemetry::now_ms);
    Row {
        at_ms,
        kind: Kind::Command,
        // A line no read confirmed a program for gets no key: its words may be a secret typed at
        // a prompt, which an unsalted digest would let a reader confirm by guessing.
        key: b
            .program
            .map(|_| Digest::key(&Kind::Command, b.line, b.dir)),
        dir: b.dir.map(Digest::dir),
        label: aterm_telemetry::row::label(b.program.unwrap_or("")),
        sid: sid.map(str::to_owned),
        on: None,
        host: host_name.to_owned(),
        facts,
        wall_ms: match (b.started_ms, b.ended_ms) {
            (Some(s), Some(e)) => Fig::Known(e.saturating_sub(s)),
            _ => Fig::unknown(source::OSC133),
        },
        cpu_ms: cpu_ns.map_or_else(
            || Fig::unknown(source::SHELL_RUSAGE),
            |ns| Fig::Known(ns / 1_000_000),
        ),
        peak_bytes: job
            .and_then(|(peak, _)| peak)
            .map_or_else(|| Fig::unknown(source::JOBS), Fig::Known),
        wait_ms: Fig::Na,
        cache: Fig::Na,
        outcome,
        stop,
        code,
        sig,
        meter: Meter::Exact,
    }
}

/// The `spawn` verb answered `reply` for a worker of `agent` (`-` for a shell) in `cwd` after
/// `wall_ms`: a `kind=spawn` row.
pub(crate) fn spawned(agent: &str, cwd: Option<&str>, wall_ms: u64, reply: &str) {
    let Some((host_name, facts)) = host() else {
        return;
    };
    emit(spawn_row(agent, cwd, wall_ms, reply, host_name, facts));
}

/// The spawn verb's row: the worker its answer named ([`aterm_ctl::spawned_sid_from_answer`],
/// as a fleet spawn's sender reads it), no `on=` (it runs here), `ok` or `refused` by the answer.
fn spawn_row(
    agent: &str,
    cwd: Option<&str>,
    wall_ms: u64,
    reply: &str,
    host_name: &str,
    facts: Digest,
) -> Row {
    let sid = aterm_ctl::spawned_sid_from_answer(reply);
    aterm_ctl::fleet_telemetry::Spawn {
        agent,
        cwd,
        wall_ms,
        outcome: if reply.starts_with("OK") {
            Outcome::Ok
        } else {
            Outcome::Refused
        },
        sid: sid.as_deref(),
        on: None,
    }
    .row(aterm_telemetry::now_ms(), host_name, facts)
}

/// The `telemetry` verb: `telemetry [rows] [--since <t>] [--kind <k>] [--limit <n>]`, its JSON
/// form when `json`.
pub(crate) fn cmd(rest: &str, json: bool) -> String {
    let Some(dir) = aterm_telemetry::log::default_dir() else {
        return "ERR telemetry: this aterm has no state directory\n".to_owned();
    };
    cmd_in(&dir, rest, json, aterm_telemetry::now_ms(), call_counts())
}

const USAGE: &str = "ERR usage: telemetry [rows] [--since <n>s|m|h|d|<unix ms>] [--kind <kind>] [--limit <n>] [--json]\n";

/// [`cmd`] on the log in `dir` at `now_ms`, the summary closed by this process's agent-call
/// watches as `calls` (`(watched, read, missed)`, [`call_counts`]).
fn cmd_in(
    dir: &std::path::Path,
    rest: &str,
    json: bool,
    now_ms: u64,
    calls: (u64, u64, u64),
) -> String {
    let mut rows = false;
    let mut since = None;
    let mut kind: Option<String> = None;
    let mut limit = 50usize;
    let mut words = rest.split_whitespace();
    while let Some(w) = words.next() {
        let (key, inline) = match w.split_once('=') {
            Some((k, v)) => (k.trim_start_matches('-'), Some(v.to_owned())),
            None => (w.trim_start_matches('-'), None),
        };
        let mut value = || inline.clone().or_else(|| words.next().map(str::to_owned));
        match key {
            "rows" if inline.is_none() => rows = true,
            "summary" if inline.is_none() => rows = false,
            "since" => {
                let Some(v) = value() else {
                    return USAGE.to_owned();
                };
                match aterm_telemetry::summary::parse_since(&v, now_ms) {
                    Ok(t) => since = Some(t),
                    Err(e) => return format!("ERR telemetry: {e}\n"),
                }
            }
            "kind" => match value() {
                Some(v) => kind = Some(v),
                None => return USAGE.to_owned(),
            },
            "limit" | "n" => match value().and_then(|v| v.parse().ok()) {
                Some(n) => limit = n,
                None => return USAGE.to_owned(),
            },
            _ => return USAGE.to_owned(),
        }
    }
    let files = aterm_telemetry::Limits::DEFAULT.files;
    let mut reading = match aterm_telemetry::log::read(dir, files, since) {
        Ok(r) => r,
        Err(e) => return format!("ERR telemetry: {e}\n"),
    };
    if let Some(k) = kind.as_deref() {
        reading.rows.retain(|r| r.kind.word() == k);
    }
    let shown = dir.display().to_string();
    if json {
        let body = aterm_telemetry::summary::render_json(&reading, since, &shown, rows);
        return aterm_control::wire::json_ok(&body);
    }
    let text = if rows {
        aterm_telemetry::summary::render_rows(&reading, since, &shown, None, limit)
    } else {
        let mut text = aterm_telemetry::summary::render(&reading, since, &shown);
        let (watched, read, missed) = calls;
        // This window's own exit watches on agent calls' leaders ([`CallWatch`]): how many it
        // proved and watched, read a status of, and missed (each such call ends with none).
        let _ = writeln!(text, "calls watched={watched} read={read} missed={missed}");
        text
    };
    let n = text.lines().count();
    format!("OK {n}\n{text}")
}

#[cfg(test)]
#[path = "telemetry_host_tests.rs"]
mod tests;
