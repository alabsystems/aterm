// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Job units (§11.3), their members, and the per-tick pass that turns
//! readings into rows.
//!
//! * **Shell foreground job.** An OSC 133 block (`b<block>`), keyed by the
//!   block: it opens at `C` and its members are the shell that runs the
//!   command line, whatever runs in the shell's own group, and each group the
//!   terminal puts in front while the block is open, command after command
//!   (`make && make test` is one job; so is every `sleep` of a typed
//!   `while :; do sleep 0.5; done`). The shell alone holding the terminal is
//!   its own `read` builtin (or the moment between two commands). Without
//!   shell integration the unit is "the foreground group that is not the
//!   shell's"; its id is then `j<pgid>` (no block names it). It closes at
//!   `D`, or when Ctrl-Z returns the prompt: the stopped group becomes a
//!   background job keeping its id and quiet clock, and `fg` makes it the
//!   next block's foreground job again.
//! * **Shell background jobs.** Every other group of the shell's descendants
//!   (`j<pgid>`). PTY output is credited only to a sole writer.
//! * **Detached child jobs.** A process in the tab that leads its own session
//!   with no terminal is a job whose consumer is its parent (`a<pid>`): an
//!   agent's tool calls, without naming any agent. Its members are its
//!   descendants; a nested detached child is its own job.
//! * **Orphans.** What lives on after its job ended (`o<pid>`), with consumer
//!   nobody: per-process findings only, no φ.
//!
//! Membership is kept: a process first seen as a member stays one, by
//! `(pid, start)`, after it is reparented (the ledger's rule), and another
//! unit never takes it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::step::{self, Cx, Evidence, Fault, Progress, Track};
use super::wait::{self, JobFacts, Scene};
use super::{
    BUSY_ORPHAN_MS, Class, ClassTable, Consumer, Demand, EXITED_KEEP_MS, Findings, GateWord,
    Holder, InputGate, JobCounters, JobKind, JobProc, JobRow, JobTab, JobTick, JobsView,
    KERNEL_WAIT_MS, MemberMap, Named, NamedRow, NamedWait, ProcState, Standing, Stop, SweepProc,
    TierB, Verdict, WaitName, Want, ZOMBIE_AGE_MS, ZombieParent, Zombies, command_class,
};

/// Descendants walked per tab at most.
const TREE_CAP: usize = 4096;
/// Side writes allowed per 2 s tick beyond the output channel's share.
const SIDE_ALLOWANCE: u64 = 64 * 1024;
/// Child CPU that makes a birth or reap "with work", ns per second.
const CHURN_WORK_NS_PER_S: u64 = 50_000_000;
/// Bytes new children wrote that make churn "with work".
const CHURN_WROTE: u64 = 64 * 1024;

/// One remembered member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    /// Its start, compared for equality.
    pub start: Option<u64>,
    /// Its counters at the last reading.
    pub counters: Option<JobCounters>,
    /// Its name.
    pub name: String,
    /// When it was first seen, host ms.
    pub first_ms: u64,
}

/// A job's identity: the tab and either the OSC 133 block (the shell's
/// foreground job: the block is the job, whatever groups run in it) or the
/// leader's `(pid, start)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    tab: u64,
    leader: u32,
    start: Option<u64>,
    block: Option<u64>,
}

impl Key {
    fn same(&self, o: &Self) -> bool {
        if self.tab != o.tab {
            return false;
        }
        match (self.block, o.block) {
            (Some(a), Some(b)) => a == b,
            (None, None) => {
                self.leader == o.leader
                    && (self.start.is_none() || o.start.is_none() || self.start == o.start)
            }
            _ => false,
        }
    }
}

/// One job's state across ticks.
#[derive(Clone, Debug)]
struct Job {
    key: Key,
    id: String,
    kind: JobKind,
    consumer: Consumer,
    promoted_ms: Option<u64>,
    task: Option<String>,
    command: String,
    pgid: u32,
    /// The open block it is the foreground job of.
    block: Option<u64>,
    /// The tab's shell, a member while this job is its block's foreground
    /// job: the shell runs the command line (a loop, an `&&` chain, its own
    /// `read`), and its reaped children's CPU is the job's.
    shell: Option<u32>,
    consumer_pid: Option<u32>,
    /// The last name the row printed, kept for the exited row.
    last_name: String,
    members: MemberMap,
    outputs: HashMap<(u64, u64), u64>,
    pty_bytes: Option<u64>,
    track: Track,
    last_tick_ms: Option<u64>,
    exited_ms: Option<u64>,
    exit: Option<i32>,
    named: Option<NamedWait>,
    named_progress: Option<u64>,
    busy_since: Option<u64>,
    /// This tick it held the foreground at the person's own terminal read:
    /// `tty-raw` or `tty-password` named from a mode word stamped after its
    /// last terminal output, with no key queued, whatever its class
    /// ([`demand_of`]).
    person_read: bool,
    row: Option<JobRow>,
}

/// A unit found this tick.
#[derive(Clone, Debug)]
struct Unit {
    key: Key,
    kind: JobKind,
    consumer: Consumer,
    promoted_ms: Option<u64>,
    task: Option<String>,
    id: String,
    pgid: u32,
    members: Vec<u32>,
    consumer_pid: Option<u32>,
    command: String,
    block: Option<u64>,
    shell: Option<u32>,
}

/// What a tab's pass knows beyond its units.
#[derive(Clone, Copy, Debug, Default)]
struct TabFacts {
    shell_pgid: u32,
    tpgid: Option<u32>,
    has_fg: bool,
    bg_units: usize,
}

/// The stuck-process engine: one per instance, fed each probe tick.
#[derive(Clone, Debug, Default)]
pub struct Jobs {
    jobs: Vec<Job>,
    classes: ClassTable,
    zombies: HashMap<u32, (u64, u32)>,
    zombie_parents: HashMap<u32, u64>,
    d_since: HashMap<u32, u64>,
    #[cfg(any(test, debug_assertions))]
    fault: Option<Fault>,
}

impl Jobs {
    /// An engine with nothing seen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same engine with a defect injected into its step: the conformance
    /// test's negative controls (§17).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_fault(mut self, fault: Fault) -> Self {
        self.fault = Some(fault);
        self
    }

    fn fault(&self) -> Option<Fault> {
        #[cfg(any(test, debug_assertions))]
        {
            self.fault
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            None
        }
    }

    /// The learned command classes.
    #[must_use]
    pub fn classes(&self) -> &ClassTable {
        &self.classes
    }

    /// Teach a command class a median interval, as a completed job would
    /// (the host replays a persisted table this way; tests seed it).
    pub fn learn(&mut self, class: &str, median_ms: u64) {
        self.classes.learn(class, median_ms);
    }

    /// Which processes to read in detail this tick, and which need their
    /// descriptors: each tab's shell, its descendants (descriptors for the
    /// leaves and for every member of a detached job), the members this
    /// engine remembers, and each detached job's consumer (for `serves`).
    #[must_use]
    pub fn plan(&self, sweep: &[SweepProc], tabs: &[JobTab]) -> Vec<Want> {
        let by_pid: HashMap<u32, &SweepProc> = sweep.iter().map(|p| (p.pid, p)).collect();
        let children = children_of(sweep);
        let mut want: BTreeMap<u32, bool> = BTreeMap::new();
        for tab in tabs {
            if tab.shell_pid == 0 || !by_pid.contains_key(&tab.shell_pid) {
                continue;
            }
            want.entry(tab.shell_pid).or_insert(false);
            for pid in descendants(tab.shell_pid, &children) {
                let Some(p) = by_pid.get(&pid) else { continue };
                if p.state == ProcState::Zombie {
                    want.entry(pid).or_insert(false);
                    continue;
                }
                let leaf = children.get(&pid).is_none_or(|c| {
                    c.iter()
                        .all(|k| by_pid.get(k).is_none_or(|k| k.state == ProcState::Zombie))
                });
                let detached = owner(pid, tab.shell_pid, &by_pid).is_some();
                if is_detached(p) {
                    // Its consumer: the reader of what it writes.
                    want.insert(p.ppid, true);
                }
                let e = want.entry(pid).or_insert(false);
                *e |= leaf || detached;
            }
        }
        for job in self.jobs.iter().filter(|j| j.exited_ms.is_none()) {
            for pid in job.members.keys() {
                if let Some(p) = by_pid.get(pid) {
                    let e = want.entry(*pid).or_insert(false);
                    *e |= p.state != ProcState::Zombie;
                }
            }
        }
        want.into_iter()
            .map(|(pid, fds)| Want { pid, fds })
            .collect()
    }

    /// One tick: every job's row, the findings and the demand.
    #[expect(
        clippy::too_many_lines,
        reason = "one pass over the tick, in the order the design states it"
    )]
    pub fn absorb(&mut self, tick: &JobTick) -> JobsView {
        let at = tick.at_ms;
        let fault = self.fault();
        let sweep: HashMap<u32, &SweepProc> = tick.sweep.iter().map(|p| (p.pid, p)).collect();
        let facts: HashMap<u32, &JobProc> = tick.procs.iter().map(|p| (p.pid, p)).collect();
        let children = children_of(&tick.sweep);
        self.note_zombies(at, &tick.sweep);
        self.note_d_state(at, &tick.sweep, &facts);

        let alive = |pid: u32, start: Option<u64>| -> bool {
            sweep.contains_key(&pid)
                && match (start, facts.get(&pid).and_then(|f| f.start)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
        };

        // How processes in each job counted last tick (a pipe's peer in
        // another job counts as its job does).
        let mut elsewhere_all: HashMap<u32, (usize, Standing)> = HashMap::new();
        for (i, j) in self.jobs.iter().enumerate() {
            let Some(row) = j.row.as_ref().filter(|_| j.exited_ms.is_none()) else {
                continue;
            };
            let s = row.standing.unwrap_or(Standing::Legit);
            let s = if row.class == Class::Running {
                Standing::Legit
            } else {
                s
            };
            for pid in j.members.keys() {
                elsewhere_all.insert(*pid, (i, s));
            }
        }

        // Claims: a live job keeps its alive members.
        let mut claimed: HashMap<u32, usize> = HashMap::new();
        for (i, j) in self.jobs.iter().enumerate() {
            if j.exited_ms.is_some() {
                continue;
            }
            for (pid, m) in &j.members {
                if alive(*pid, m.start) {
                    claimed.entry(*pid).or_insert(i);
                }
            }
        }

        // Discover this tick's units and give each its job.
        let mut matched: BTreeSet<usize> = BTreeSet::new();
        let mut tab_facts: HashMap<u64, TabFacts> = HashMap::new();
        let mut assigned: HashMap<usize, Vec<u32>> = HashMap::new();
        for tab in &tick.tabs {
            let (units, tf) = discover(tab, &sweep, &facts, &children);
            tab_facts.insert(tab.key, tf);
            for u in units {
                let found = self
                    .jobs
                    .iter()
                    .position(|j| j.exited_ms.is_none() && j.key.same(&u.key));
                let free: Vec<u32> = u
                    .members
                    .iter()
                    .copied()
                    .filter(|p| claimed.get(p).is_none_or(|c| Some(*c) == found))
                    .collect();
                if found.is_none()
                    && u.block.is_some()
                    && let Some(i) = resumed(&u, &claimed, &self.jobs)
                {
                    // `fg` brought a parked job back: that job is the
                    // block's foreground job, its id and quiet clock kept.
                    let j = &mut self.jobs[i];
                    j.kind = JobKind::Shell;
                    j.consumer = Consumer::PersonFg;
                    j.block = u.block;
                    matched.insert(i);
                    assigned
                        .entry(i)
                        .or_default()
                        .extend(free.iter().copied().filter(|p| Some(*p) != u.shell));
                    continue;
                }
                // A block's job opens at `C` whatever runs in it (the shell
                // itself for its own `read`); any other unit needs a live
                // member nobody else holds.
                let any_live = u.block.is_some()
                    || free
                        .iter()
                        .any(|p| sweep.get(p).is_some_and(|s| s.state != ProcState::Zombie));
                let idx = match found {
                    Some(i) => i,
                    None if !any_live => continue,
                    None => {
                        let opened = u
                            .block
                            .and_then(|b| tab.block.as_ref().filter(|blk| blk.id == b))
                            .map_or(at, |blk| blk.started_ms.min(at));
                        self.jobs.push(Job::open(&u, opened));
                        self.jobs.len() - 1
                    }
                };
                for p in &free {
                    claimed.entry(*p).or_insert(idx);
                }
                let j = &mut self.jobs[idx];
                if u.key.start.is_some() && j.key.start.is_none() {
                    j.key.start = u.key.start;
                }
                // A shell job moves between the foreground and the background;
                // an agent's call stays one. (Its leader exited unreaped is a
                // zombie, never detached, so it groups by its process group
                // here: it is still the call, now exited.)
                if j.kind != JobKind::AgentCall || u.kind == JobKind::AgentCall {
                    j.kind = u.kind;
                    j.consumer = u.consumer;
                }
                if j.kind != JobKind::AgentCall {
                    // The group in front of a block changes command by
                    // command.
                    j.pgid = u.pgid;
                }
                j.promoted_ms = u.promoted_ms.or(j.promoted_ms);
                j.task = u.task.clone().or_else(|| j.task.clone());
                j.consumer_pid = u.consumer_pid;
                if !u.command.is_empty() && j.command.is_empty() {
                    j.command.clone_from(&u.command);
                }
                matched.insert(idx);
                assigned.entry(idx).or_default().extend(free);
            }
        }

        // Tier B namings handed this tick.
        for tab in &tick.tabs {
            for n in &tab.named {
                if let Some(j) = self.jobs.iter_mut().find(|j| {
                    j.exited_ms.is_none()
                        && j.key.tab == tab.key
                        && (j.key.leader == n.leader
                            || j.pgid == n.leader
                            || j.members.contains_key(&n.leader))
                }) {
                    j.named = Some(n.clone());
                    j.named_progress = j.track.last_progress_ms;
                }
            }
        }

        // Each live job's tick.
        let tabs: HashMap<u64, &JobTab> = tick.tabs.iter().map(|t| (t.key, t)).collect();
        let mut orphans: Vec<Job> = Vec::new();
        let n = self.jobs.len();
        let scene_elsewhere = |me: usize| -> HashMap<u32, Standing> {
            elsewhere_all
                .iter()
                .filter(|(_, (i, _))| *i != me)
                .map(|(p, (_, s))| (*p, *s))
                .collect()
        };
        let holders: HashMap<u32, (Class, u64)> = self
            .jobs
            .iter()
            .filter(|j| j.exited_ms.is_none())
            .filter_map(|j| j.row.as_ref().map(|r| (j, r)))
            .flat_map(|(j, r)| j.members.keys().map(move |p| (*p, (r.class, j.key.tab))))
            .collect();
        let d_age: HashMap<u32, u64> = self
            .d_since
            .iter()
            .map(|(p, s)| (*p, at.saturating_sub(*s)))
            .collect();
        for i in 0..n {
            if self.jobs[i].exited_ms.is_some() {
                continue;
            }
            let tab = tabs.get(&self.jobs[i].key.tab).copied();
            let tf = tab_facts
                .get(&self.jobs[i].key.tab)
                .copied()
                .unwrap_or_default();
            let class_mu = self
                .classes
                .mu(&command_class(&self.jobs[i].command, self.jobs[i].kind));
            let elsewhere = scene_elsewhere(i);
            let job = &mut self.jobs[i];

            // Members: the remembered and this tick's, alive.
            let prev: MemberMap = job.members.clone();
            let first_tick = job.last_tick_ms.is_none();
            let mut now: MemberMap = BTreeMap::new();
            let candidates: BTreeSet<u32> = prev
                .keys()
                .copied()
                .chain(assigned.get(&i).into_iter().flatten().copied())
                .collect();
            for pid in candidates {
                let start = prev.get(&pid).and_then(|m| m.start);
                if !alive(pid, start) {
                    continue;
                }
                let f = facts.get(&pid);
                let name = f
                    .map(|f| shown_name(f))
                    .or_else(|| sweep.get(&pid).map(|s| s.name.clone()))
                    .unwrap_or_default();
                now.insert(
                    pid,
                    Member {
                        start: start.or_else(|| f.and_then(|f| f.start)),
                        counters: f.and_then(|f| f.counters),
                        name,
                        first_ms: prev.get(&pid).map_or(at, |m| m.first_ms),
                    },
                );
            }
            let zombie = |pid: &u32| sweep.get(pid).is_some_and(|s| s.state == ProcState::Zombie);

            // Has the unit ended?
            let block_closed = job.block.is_some_and(|b| {
                tab.and_then(|t| t.block.as_ref())
                    .is_none_or(|blk| blk.id != b || !blk.open)
            });
            let shell = job.shell;
            let stopped_member = now.keys().filter(|p| !zombie(p)).find_map(|p| {
                let st = facts
                    .get(p)
                    .map_or_else(|| sweep.get(p).map(|s| s.state), |f| Some(f.state));
                (st == Some(ProcState::Stopped) && Some(*p) != shell)
                    .then(|| (*p, now.get(p).map(|m| m.name.clone()).unwrap_or_default()))
            });
            if block_closed
                && job.block.is_some()
                && let Some((sp, _)) = &stopped_member
            {
                // Ctrl-Z returned the prompt: the stopped group is a
                // background job now, its id and quiet clock carried; the
                // shell is the prompt's again.
                let g = facts
                    .get(sp)
                    .map(|f| f.pgid)
                    .or_else(|| sweep.get(sp).map(|s| s.pgid))
                    .filter(|g| *g != 0)
                    .unwrap_or(*sp);
                if let Some(sh) = job.shell.take() {
                    now.remove(&sh);
                }
                job.block = None;
                job.kind = JobKind::ShellBg;
                job.consumer = Consumer::PersonBg;
                job.pgid = g;
                job.key = Key {
                    tab: job.key.tab,
                    leader: g,
                    start: now.get(&g).and_then(|m| m.start),
                    block: None,
                };
            }
            let live: Vec<u32> = now.keys().copied().filter(|p| !zombie(p)).collect();
            let ended = tab.is_none()
                || (job.block.is_some() && block_closed)
                || (job.kind == JobKind::AgentCall && !alive(job.key.leader, job.key.start));
            if (ended || block_closed) && job.block.is_some() {
                job.exit = tab
                    .and_then(|t| t.block.as_ref())
                    .filter(|b| Some(b.id) == job.block)
                    .and_then(|b| b.status);
            }
            let exited = ended || live.is_empty();
            if exited {
                // What lives on after its job ended, the tab's shell aside.
                let shell_pid = tab.map_or(0, |t| t.shell_pid);
                let survivors: MemberMap = now
                    .iter()
                    .filter(|(p, _)| !zombie(p) && **p != shell_pid && Some(**p) != job.shell)
                    .map(|(p, m)| (*p, m.clone()))
                    .collect();
                if !survivors.is_empty() && job.kind != JobKind::Orphan {
                    orphans.push(Job::orphan(job.key.tab, survivors, at));
                }
            }

            // The evidence.
            let dt = job.last_tick_ms.map_or(0, |l| at.saturating_sub(l));
            let floor = job.last_tick_ms.unwrap_or_else(|| {
                tab.and_then(|t| t.block.as_ref())
                    .filter(|b| Some(b.id) == job.block)
                    .map_or(at, |b| b.started_ms)
            });
            let mut refused = false;
            let mut rebased = false;
            let mut cpu: u64 = 0;
            let mut child_work: u64 = 0;
            let mut calls: u64 = 0;
            let mut side: u64 = 0;
            let mut pageins: u64 = 0;
            let mut births_wrote: u64 = 0;
            let mut births = 0u32;
            for pid in &live {
                let m = &now[pid];
                let Some(c) = m.counters else {
                    refused = true;
                    continue;
                };
                match prev.get(pid).and_then(|p| p.counters) {
                    Some(p) => {
                        cpu += c.cpu_ns.saturating_sub(p.cpu_ns);
                        child_work += delta(c.child_cpu_ns, p.child_cpu_ns);
                        calls += delta(c.syscalls, p.syscalls)
                            + delta(c.wakeups.or(c.ctx_switches), p.wakeups.or(p.ctx_switches));
                        side += delta(c.logical_writes, p.logical_writes);
                        pageins += delta(c.pageins, p.pageins);
                    }
                    // A member read for the first time since a refused or
                    // absent reading: its baseline, its CPU since unknown.
                    None if prev.contains_key(pid) => rebased = true,
                    None if !first_tick => {
                        births += 1;
                        cpu += c.cpu_ns;
                        child_work += c.cpu_ns;
                        births_wrote += c.logical_writes.unwrap_or(0);
                        side += c.logical_writes.unwrap_or(0);
                    }
                    None => {}
                }
            }
            let reaps = u32::try_from(prev.keys().filter(|p| !now.contains_key(p)).count())
                .unwrap_or(u32::MAX);
            let churn = births + reaps > 0;
            let churn_work = child_work.saturating_mul(1000)
                >= CHURN_WORK_NS_PER_S.saturating_mul(dt.max(1))
                || births_wrote >= CHURN_WROTE;

            let holds_fg = job.kind == JobKind::Shell;
            let credited_pty =
                holds_fg || (job.kind == JobKind::ShellBg && !tf.has_fg && tf.bg_units == 1);
            let mut progress: Option<(Progress, u64)> = None;
            let mut out_growth: u64 = 0;
            let fresh = |stamp: Option<u64>| stamp.filter(|s| *s > floor && *s <= at);
            if let Some(t) = tab.filter(|_| credited_pty) {
                let canonical = t.tty.is_none_or(|m| m.canonical);
                let raw_counts = fault == Some(Fault::RawBytesProgress);
                if canonical && !raw_counts {
                    if let Some(s) = fresh(t.line_output_ms) {
                        progress = Some((Progress::Line, s));
                    }
                } else if let Some(s) = fresh(t.output_ms) {
                    progress = Some((Progress::Raw, s));
                }
                if let Some(b) = job.pty_bytes {
                    out_growth = t.output_bytes.saturating_sub(b);
                }
                job.pty_bytes = Some(t.output_bytes);
            }
            // The output file: fds 1 and 2 on a regular file.
            let mut files: HashMap<(u64, u64), (u64, bool, bool)> = HashMap::new();
            for pid in &live {
                for f in facts.get(pid).and_then(|f| f.fds.as_deref()).unwrap_or(&[]) {
                    if let (
                        1..=2,
                        super::FdKind::File {
                            regular: true,
                            size,
                            dev,
                            ino,
                            lf_tail,
                        },
                    ) = (f.fd, &f.kind)
                    {
                        let e = files.entry((*dev, *ino)).or_insert((0, false, false));
                        e.0 = e.0.max(*size);
                        e.1 |= *lf_tail == Some(true);
                        e.2 |= *lf_tail == Some(false);
                    }
                }
            }
            let mut file_progress: Option<Progress> = None;
            for (k, (size, lf, cr)) in &files {
                let last = job.outputs.insert(*k, *size);
                let Some(last) = last.filter(|l| size > l) else {
                    continue;
                };
                out_growth += size - last;
                let p = if *lf {
                    Some(Progress::FileLine)
                } else if *cr && fault != Some(Fault::RawBytesProgress) {
                    None
                } else {
                    Some(Progress::FileUnverified)
                };
                if p == Some(Progress::FileLine) || file_progress.is_none() {
                    file_progress = p.or(file_progress);
                }
            }
            job.outputs.retain(|k, _| files.contains_key(k));
            if progress.is_none() {
                progress = file_progress.map(|p| (p, at));
            }
            if !refused && dt > 0 && progress.is_none() {
                let allowance = (out_growth * 2).div_ceil(4096) * 4096 + SIDE_ALLOWANCE * dt / 2000;
                let any = fault == Some(Fault::AnyLogicalWrites);
                if (any && side > 0) || side > allowance {
                    progress = Some((Progress::SideWrites, at));
                } else if churn && (churn_work || fault == Some(Fault::ChurnWithoutWork)) {
                    progress = Some((Progress::ChurnWithWork, at));
                }
            }
            if progress.is_none()
                && holds_fg
                && let Some(s) = tab.and_then(|t| fresh(t.input_consumed_ms))
            {
                progress = Some((Progress::InputConsumed, s));
            }

            // Stopped, zombies, the wait.
            let parked = matches!(job.kind, JobKind::Shell | JobKind::ShellBg)
                && tf.tpgid == Some(tf.shell_pgid);
            let stopped = stopped_member.map(|(pid, name)| Stop { parked, pid, name });
            let zombies =
                zombie_facts(&now, &live, &sweep, &self.zombies, &self.zombie_parents, at);
            let leaves = leaves_of(&live, &now, &sweep);
            let gate = tab.map_or(InputGate::default(), |t| t.input);
            let shell_pid = tab.map_or(0, |t| t.shell_pid);
            let shell_name = sweep.get(&shell_pid).map_or("", |s| s.name.as_str());
            // The shell holds the terminal with nothing of the block running
            // under it: its own `read` (or the moment between two commands).
            let builtin = job.shell.is_some()
                && tf.tpgid == Some(tf.shell_pgid)
                && live.iter().all(|p| Some(*p) == job.shell);
            let jf = JobFacts {
                holds_fg,
                builtin,
                tty: tab.and_then(|t| t.tty.filter(|m| m.stamped_ms >= t.output_ms.unwrap_or(0))),
                queue_empty: matches!(gate.word, GateWord::Clear | GateWord::Unmeasured),
                consumer: job.consumer_pid,
            };
            let lanes = tick.lanes;
            let scene = Scene {
                procs: &facts,
                lanes,
                d_age: &d_age,
                elsewhere: &elsewhere,
            };
            let wait = wait::name_job(&scene, &jf, &leaves, (shell_pid, shell_name));
            job.person_read = matches!(wait.name, WaitName::TtyRaw | WaitName::TtyPassword);

            // A Tier B naming holds until progress or exit.
            if job.named.is_some() && job.track.last_progress_ms != job.named_progress {
                job.named = None;
            }
            if progress.is_some() {
                job.named = None;
            }
            let named = job
                .named
                .as_ref()
                .map(|n| named_row(n, &sweep, &holders, &tabs));
            let named_standing = job
                .named
                .as_ref()
                .and_then(|n| named_standing(n, holds_fg, &holders, &elsewhere_all));

            let ev = Evidence {
                at_ms: at,
                dt_ms: dt,
                exited,
                progress,
                refused,
                cpu_unread: refused || rebased,
                cpu_ns: (!refused).then_some(cpu),
                calls: (!refused).then_some(calls),
                churn,
                churn_work,
                pageins: (!refused).then_some(pageins),
                stopped,
                holds_fg,
                gate,
                zombies,
                wait,
                named: named_standing,
            };
            let cx = Cx {
                consumer: job.consumer,
                kind: job.kind,
                stretch_milli: tick.machine.stretch_milli,
                thrashing: tick.machine.thrashing,
                class_mu,
                fault,
            };
            let out = step::classify_tick(&mut job.track, &ev, &cx);
            // The name the row prints, taken before the members are replaced
            // (an exited job keeps the last one it printed).
            let leader_name = match now_name(job, &live, &sweep, &facts) {
                Some(n) if !exited || job.last_name.is_empty() => n,
                _ => job.last_name.clone(),
            };
            job.last_name.clone_from(&leader_name);
            job.members = now;
            job.last_tick_ms = Some(at);
            if exited {
                job.exited_ms = Some(at);
            }

            // A busy-silent orphan: a core or more, no side writes, 10 min.
            if job.kind == JobKind::Orphan
                && dt > 0
                && !refused
                && cpu >= dt.saturating_mul(1_000_000)
                && side == 0
            {
                job.busy_since.get_or_insert(at.saturating_sub(dt));
            } else if !refused {
                job.busy_since = None;
            }

            let exe = out
                .wait
                .as_ref()
                .map_or(leader_name, |w| w.leaf_name.clone());
            let cpu_mc = (dt > 0 && !refused).then(|| cpu / (dt * 1000));
            let class = out.class;
            job.row = Some(JobRow {
                id: job.id.clone(),
                tab: job.key.tab,
                kind: job.kind,
                consumer: job.consumer,
                promoted_ms: job.promoted_ms,
                task: job.task.clone(),
                pgid: job.pgid,
                leader: (job.key.leader, job.key.start),
                exe,
                members: live.len(),
                class,
                wait: out.wait,
                named: named.filter(|_| class == Class::Waiting),
                stop: out.stop,
                input: (class == Class::UnreadInput).then_some(gate),
                cpu_mc: cpu_mc.filter(|_| matches!(class, Class::UnreadInput | Class::BusySilent)),
                zombies: ev.zombies,
                standing: out.standing,
                quiet_ms: out.quiet_ms,
                phi_milli: out.phi_milli,
                mu_ms: out.mu_ms,
                mu_from: out.mu_from,
                verdict: out.verdict,
                no_progress: out.no_progress,
                episode: out.episode,
                exit: job.exit,
                tick: out.tick,
            });
            if exited && let Some(own) = job.track.own_mu() {
                let class = command_class(&job.command, job.kind);
                self.classes.learn(&class, own);
            }
        }
        self.jobs.extend(orphans);
        // An exited job keeps its row a minute, unless a live job now
        // answers to its id (a reused pid): ids stay unique per tab, and the
        // stale one is gone.
        let live_ids: BTreeSet<(u64, String)> = self
            .jobs
            .iter()
            .filter(|j| j.exited_ms.is_none())
            .map(|j| (j.key.tab, j.id.clone()))
            .collect();
        self.jobs.retain(|j| match j.exited_ms {
            None => true,
            Some(e) => {
                at.saturating_sub(e) < EXITED_KEEP_MS
                    && !live_ids.contains(&(j.key.tab, j.id.clone()))
            }
        });

        let rows: Vec<JobRow> = {
            let mut r: Vec<JobRow> = self.jobs.iter().filter_map(|j| j.row.clone()).collect();
            r.sort_by(|a, b| a.tab.cmp(&b.tab).then_with(|| a.id.cmp(&b.id)));
            r
        };
        let findings = self.findings(tick, &sweep);
        let at_person: BTreeSet<(u64, &str)> = self
            .jobs
            .iter()
            .filter(|j| j.person_read && j.exited_ms.is_none())
            .map(|j| (j.key.tab, j.id.as_str()))
            .collect();
        let demand = demand_of(&rows, &at_person);
        let mut lanes_off = Vec::new();
        if !tick.lanes.kqueue_detail {
            lanes_off.push("lane_off:kqueue_detail");
        }
        if !tick.lanes.pipe_flags {
            lanes_off.push("lane_off:pipe_flags");
        }
        JobsView {
            rows,
            findings,
            demand,
            lanes_off,
        }
    }

    fn note_zombies(&mut self, at: u64, sweep: &[SweepProc]) {
        let mut live: HashMap<u32, u32> = HashMap::new();
        for p in sweep.iter().filter(|p| p.state == ProcState::Zombie) {
            live.insert(p.pid, p.ppid);
        }
        self.zombies
            .retain(|pid, (_, ppid)| live.get(pid).is_some_and(|pp| pp == ppid));
        for (pid, ppid) in &live {
            self.zombies.entry(*pid).or_insert((at, *ppid));
            self.zombie_parents.entry(*ppid).or_insert(at);
        }
        let parents: BTreeSet<u32> = live.values().copied().collect();
        self.zombie_parents.retain(|p, _| parents.contains(p));
    }

    fn note_d_state(&mut self, at: u64, sweep: &[SweepProc], facts: &HashMap<u32, &JobProc>) {
        let mut d: BTreeSet<u32> = sweep
            .iter()
            .filter(|p| p.state == ProcState::Uninterruptible)
            .map(|p| p.pid)
            .collect();
        for (pid, f) in facts {
            let in_d = f.state == ProcState::Uninterruptible
                || f.threads
                    .as_deref()
                    .is_some_and(|ts| ts.iter().any(|t| t.state == ProcState::Uninterruptible));
            if in_d {
                d.insert(*pid);
            } else {
                d.remove(pid);
            }
        }
        self.d_since.retain(|p, _| d.contains(p));
        for p in d {
            self.d_since.entry(p).or_insert(at);
        }
    }

    fn findings(&self, tick: &JobTick, sweep: &HashMap<u32, &SweepProc>) -> Findings {
        let at = tick.at_ms;
        let tab_of: HashMap<u32, u64> = self
            .jobs
            .iter()
            .filter(|j| j.exited_ms.is_none())
            .flat_map(|j| j.members.keys().map(move |p| (*p, j.key.tab)))
            .collect();
        let name = |pid: u32| sweep.get(&pid).map(|s| s.name.clone()).unwrap_or_default();
        let mut f = Findings {
            process_limit: tick.process_limit,
            ..Findings::default()
        };
        for p in &tick.sweep {
            if p.state == ProcState::Stopped {
                f.stopped.push(Named {
                    pid: p.pid,
                    name: p.name.clone(),
                    tab: tab_of.get(&p.pid).copied(),
                    for_ms: 0,
                });
            }
            if tick.self_uid.is_some() && p.uid == tick.self_uid {
                f.own_procs += 1;
            }
        }
        let mut by_parent: BTreeMap<u32, u32> = BTreeMap::new();
        for (first, ppid) in self.zombies.values() {
            if at.saturating_sub(*first) >= ZOMBIE_AGE_MS {
                *by_parent.entry(*ppid).or_default() += 1;
            }
        }
        for (ppid, count) in by_parent {
            let since = self.zombie_parents.get(&ppid).copied().unwrap_or(at);
            f.zombies.push(ZombieParent {
                ppid,
                name: name(ppid),
                count,
                per_min_deci: per_min_deci(count, at.saturating_sub(since)),
                tab: tab_of.get(&ppid).copied(),
            });
        }
        for (pid, since) in &self.d_since {
            let own = sweep
                .get(pid)
                .is_some_and(|s| tick.self_uid.is_some() && s.uid == tick.self_uid);
            let for_ms = at.saturating_sub(*since);
            if own && for_ms >= KERNEL_WAIT_MS {
                f.d_state.push(Named {
                    pid: *pid,
                    name: name(*pid),
                    tab: tab_of.get(pid).copied(),
                    for_ms,
                });
            }
        }
        f.d_state.sort_by_key(|n| n.pid);
        for j in self
            .jobs
            .iter()
            .filter(|j| j.exited_ms.is_none() && j.kind == JobKind::Orphan)
        {
            if let Some(since) = j.busy_since {
                let for_ms = at.saturating_sub(since);
                if for_ms >= BUSY_ORPHAN_MS {
                    f.busy_orphans.push(Named {
                        pid: j.key.leader,
                        name: j
                            .members
                            .get(&j.key.leader)
                            .map(|m| m.name.clone())
                            .unwrap_or_default(),
                        tab: Some(j.key.tab),
                        for_ms,
                    });
                }
            }
        }
        f
    }
}

impl Job {
    fn open(u: &Unit, at: u64) -> Self {
        Self {
            key: u.key.clone(),
            id: u.id.clone(),
            kind: u.kind,
            consumer: u.consumer,
            promoted_ms: u.promoted_ms,
            task: u.task.clone(),
            command: u.command.clone(),
            pgid: u.pgid,
            block: u.block,
            shell: u.shell,
            consumer_pid: u.consumer_pid,
            last_name: String::new(),
            members: BTreeMap::new(),
            outputs: HashMap::new(),
            pty_bytes: None,
            track: Track::new(at),
            last_tick_ms: None,
            exited_ms: None,
            exit: None,
            named: None,
            named_progress: None,
            busy_since: None,
            person_read: false,
            row: None,
        }
    }

    fn orphan(tab: u64, members: MemberMap, at: u64) -> Self {
        let (leader, m) = members
            .iter()
            .next()
            .map(|(p, m)| (*p, m.clone()))
            .unwrap_or_default();
        Self {
            key: Key {
                tab,
                leader,
                start: m.start,
                block: None,
            },
            id: format!("o{leader}"),
            kind: JobKind::Orphan,
            consumer: Consumer::Nobody,
            promoted_ms: None,
            task: None,
            command: m.name.clone(),
            pgid: leader,
            block: None,
            shell: None,
            consumer_pid: None,
            last_name: m.name.clone(),
            members,
            outputs: HashMap::new(),
            pty_bytes: None,
            track: Track::new(at),
            last_tick_ms: Some(at),
            exited_ms: None,
            exit: None,
            named: None,
            named_progress: None,
            busy_since: None,
            person_read: false,
            row: None,
        }
    }
}

fn delta(now: Option<u64>, then: Option<u64>) -> u64 {
    match (now, then) {
        (Some(a), Some(b)) => a.saturating_sub(b),
        _ => 0,
    }
}

fn per_min_deci(count: u32, elapsed_ms: u64) -> u64 {
    u64::from(count) * 600_000 / elapsed_ms.max(60_000)
}

fn is_detached(p: &SweepProc) -> bool {
    p.session_leader == Some(true) && p.has_tty == Some(false) && p.state != ProcState::Zombie
}

fn children_of(sweep: &[SweepProc]) -> HashMap<u32, Vec<u32>> {
    let mut c: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in sweep {
        if p.ppid != p.pid {
            c.entry(p.ppid).or_default().push(p.pid);
        }
    }
    for v in c.values_mut() {
        v.sort_unstable();
    }
    c
}

/// Every descendant of `root` (not `root`), breadth first.
fn descendants(root: u32, children: &HashMap<u32, Vec<u32>>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<u32> = BTreeSet::from([root]);
    let mut queue = vec![root];
    while let Some(p) = queue.pop() {
        for k in children.get(&p).into_iter().flatten() {
            if out.len() >= TREE_CAP {
                return out;
            }
            if seen.insert(*k) {
                out.push(*k);
                queue.push(*k);
            }
        }
    }
    out
}

/// The nearest detached leader at or above `pid`, below `shell`.
fn owner(pid: u32, shell: u32, sweep: &HashMap<u32, &SweepProc>) -> Option<u32> {
    let mut cur = pid;
    for _ in 0..TREE_CAP {
        let p = sweep.get(&cur)?;
        if cur != shell && is_detached(p) {
            return Some(cur);
        }
        if p.ppid == shell || p.ppid == cur || p.ppid <= 1 {
            return None;
        }
        cur = p.ppid;
    }
    None
}

/// This tick's units of one tab.
fn discover(
    tab: &JobTab,
    sweep: &HashMap<u32, &SweepProc>,
    facts: &HashMap<u32, &JobProc>,
    children: &HashMap<u32, Vec<u32>>,
) -> (Vec<Unit>, TabFacts) {
    let shell = tab.shell_pid;
    let mut tf = TabFacts::default();
    let Some(sp) = sweep.get(&shell).filter(|_| shell != 0) else {
        return (Vec::new(), tf);
    };
    let sf = facts.get(&shell);
    tf.shell_pgid = sf.map_or(sp.pgid, |f| f.pgid);
    tf.tpgid = sf.and_then(|f| f.tpgid);
    let start_of = |pid: u32| facts.get(&pid).and_then(|f| f.start);
    let tree = descendants(shell, children);
    let mut units = Vec::new();
    let mut detached: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for pid in &tree {
        match owner(*pid, shell, sweep) {
            Some(d) => detached.entry(d).or_default().push(*pid),
            None => {
                if let Some(p) = sweep.get(pid) {
                    groups.entry(p.pgid).or_default().push(*pid);
                }
            }
        }
    }
    for (d, members) in detached {
        let hint = tab.calls.iter().find(|c| c.leader == d);
        let p = sweep.get(&d);
        units.push(Unit {
            key: Key {
                tab: tab.key,
                leader: d,
                start: start_of(d),
                block: None,
            },
            kind: JobKind::AgentCall,
            consumer: hint.map_or(Consumer::Agent, |h| h.consumer),
            promoted_ms: hint.and_then(|h| h.promoted_ms),
            task: hint.and_then(|h| h.task.clone()),
            id: format!("a{d}"),
            pgid: p.map_or(d, |p| p.pgid),
            members,
            consumer_pid: p.map(|p| p.ppid),
            command: hint
                .and_then(|h| h.command.clone())
                .unwrap_or_else(|| p.map(|p| p.name.clone()).unwrap_or_default()),
            block: None,
            shell: None,
        });
    }
    // The open block is the shell's foreground job (§11.3): the shell that
    // runs the command line, whatever runs in its own group (a command
    // substitution), and the group in front, command after command
    // (`make && make test`, each `sleep` of a typed loop).
    let block = tab.block.as_ref().filter(|b| b.open);
    let front = tf.tpgid.filter(|g| *g != tf.shell_pgid && *g != 0);
    if let Some(b) = block {
        let mut members = vec![shell];
        members.extend(groups.get(&tf.shell_pgid).into_iter().flatten().copied());
        if let Some(g) = front {
            members.extend(groups.get(&g).into_iter().flatten().copied());
        }
        tf.has_fg = true;
        units.push(Unit {
            key: Key {
                tab: tab.key,
                leader: shell,
                start: start_of(shell),
                block: Some(b.id),
            },
            kind: JobKind::Shell,
            consumer: Consumer::PersonFg,
            promoted_ms: None,
            task: None,
            id: format!("b{}", b.id),
            pgid: front.unwrap_or(tf.shell_pgid),
            members,
            consumer_pid: None,
            command: b.command.clone(),
            block: Some(b.id),
            shell: Some(shell),
        });
    }
    for (g, members) in groups {
        if g == tf.shell_pgid || g == 0 {
            continue;
        }
        let fg = front == Some(g);
        if fg && block.is_some() {
            // Folded into the block's job above.
            continue;
        }
        let leader_name = sweep
            .get(&g)
            .or_else(|| members.first().and_then(|p| sweep.get(p)))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if fg {
            tf.has_fg = true;
        } else {
            tf.bg_units += 1;
        }
        units.push(Unit {
            key: Key {
                tab: tab.key,
                leader: g,
                start: start_of(g),
                block: None,
            },
            kind: if fg { JobKind::Shell } else { JobKind::ShellBg },
            consumer: if fg {
                Consumer::PersonFg
            } else {
                Consumer::PersonBg
            },
            promoted_ms: None,
            task: None,
            id: format!("j{g}"),
            pgid: g,
            members,
            consumer_pid: None,
            command: leader_name,
            block: None,
            shell: None,
        });
    }
    (units, tf)
}

/// The live job a block's group in front already belongs to, when that job
/// is a background one (a parked job `fg` resumed): its index.
fn resumed(u: &Unit, claimed: &HashMap<u32, usize>, jobs: &[Job]) -> Option<usize> {
    u.members
        .iter()
        .filter(|p| Some(**p) != u.shell)
        .find_map(|p| claimed.get(p).copied())
        .filter(|i| jobs[*i].block.is_none() && jobs[*i].kind == JobKind::ShellBg)
}

/// The waiting leaves: live members with no live member below them, deepest
/// first.
fn leaves_of(
    live: &[u32],
    now: &MemberMap,
    sweep: &HashMap<u32, &SweepProc>,
) -> Vec<(u32, String)> {
    let set: BTreeSet<u32> = live.iter().copied().collect();
    let parent = |p: u32| sweep.get(&p).map(|s| s.ppid);
    let has_child: BTreeSet<u32> = live
        .iter()
        .filter_map(|p| parent(*p))
        .filter(|pp| set.contains(pp))
        .collect();
    let depth = |p: u32| {
        let mut d = 0usize;
        let mut cur = p;
        while let Some(pp) = parent(cur).filter(|pp| set.contains(pp) && *pp != cur) {
            d += 1;
            cur = pp;
            if d > TREE_CAP {
                break;
            }
        }
        d
    };
    let mut leaves: Vec<(usize, u32)> = live
        .iter()
        .filter(|p| !has_child.contains(p))
        .map(|p| (depth(*p), *p))
        .collect();
    leaves.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    leaves
        .into_iter()
        .map(|(_, p)| (p, now.get(&p).map(|m| m.name.clone()).unwrap_or_default()))
        .collect()
}

fn zombie_facts(
    now: &MemberMap,
    live: &[u32],
    sweep: &HashMap<u32, &SweepProc>,
    seen: &HashMap<u32, (u64, u32)>,
    parents: &HashMap<u32, u64>,
    at: u64,
) -> Option<Zombies> {
    let live: BTreeSet<u32> = live.iter().copied().collect();
    let mut by_parent: BTreeMap<u32, u32> = BTreeMap::new();
    for pid in now.keys() {
        let Some((first, ppid)) = seen.get(pid) else {
            continue;
        };
        if sweep.get(pid).is_some_and(|s| s.state == ProcState::Zombie)
            && at.saturating_sub(*first) >= ZOMBIE_AGE_MS
            && live.contains(ppid)
        {
            *by_parent.entry(*ppid).or_default() += 1;
        }
    }
    let count: u32 = by_parent.values().sum();
    let (parent, _) = by_parent
        .iter()
        .max_by_key(|(p, c)| (**c, std::cmp::Reverse(**p)))?;
    let since = parents.get(parent).copied().unwrap_or(at);
    Some(Zombies {
        count,
        parent: *parent,
        per_min_deci: per_min_deci(count, at.saturating_sub(since)),
    })
}

/// The name of the process a job's row stands for: the group in front of a
/// block (or the job's leader) while it lives, else its first live member,
/// the shell last; `None` when none of them can be named.
fn now_name(
    job: &Job,
    live: &[u32],
    sweep: &HashMap<u32, &SweepProc>,
    facts: &HashMap<u32, &JobProc>,
) -> Option<String> {
    let name_of = |pid: u32| {
        facts
            .get(&pid)
            .map(|f| shown_name(f))
            .or_else(|| sweep.get(&pid).map(|s| s.name.clone()))
            .filter(|n| !n.is_empty())
    };
    let not_shell = |p: &u32| Some(*p) != job.shell;
    [job.pgid, job.key.leader]
        .into_iter()
        .filter(|p| live.contains(p) && not_shell(p))
        .chain(live.iter().copied().filter(not_shell))
        .chain(job.shell.filter(|s| live.contains(s)))
        .find_map(name_of)
}

/// A process's name as rows show it: its executable's basename where the
/// reader gave one (`proc_pidpath`, `/proc/<pid>/exe`), else its short name.
fn shown_name(f: &JobProc) -> String {
    f.exe
        .as_deref()
        .filter(|e| !e.is_empty())
        .map_or_else(|| f.name.clone(), str::to_owned)
}

fn named_row(
    n: &NamedWait,
    sweep: &HashMap<u32, &SweepProc>,
    holders: &HashMap<u32, (Class, u64)>,
    tabs: &HashMap<u64, &JobTab>,
) -> NamedRow {
    NamedRow {
        word: n.word,
        holder: n.holder.filter(|_| n.word == TierB::Lock).map(|pid| {
            let h = holders.get(&pid);
            Holder {
                pid,
                exe: sweep.get(&pid).map(|s| s.name.clone()).unwrap_or_default(),
                tab: h.and_then(|(_, t)| tabs.get(t)).map(|t| t.number),
                class: h.map(|(c, _)| *c),
            }
        }),
    }
}

/// How a Tier B naming counts: `None` leaves the Tier A standing (an event
/// wait is cross-checked by Tier A).
fn named_standing(
    n: &NamedWait,
    holds_fg: bool,
    holders: &HashMap<u32, (Class, u64)>,
    elsewhere: &HashMap<u32, (usize, Standing)>,
) -> Option<Standing> {
    Some(match n.word {
        TierB::Timer => Standing::Legit,
        TierB::Read if holds_fg => Standing::Legit,
        TierB::Read | TierB::Children | TierB::Unparsed => Standing::Capped,
        TierB::Event => return None,
        TierB::Ipc | TierB::Mutex => Standing::Capable,
        TierB::Lock => match n.holder {
            Some(h) if holders.get(&h).is_some_and(|(c, _)| *c == Class::Running) => {
                Standing::Legit
            }
            Some(h)
                if elsewhere
                    .get(&h)
                    .is_some_and(|(_, s)| *s == Standing::Legit) =>
            {
                Standing::Legit
            }
            _ => Standing::Capable,
        },
    })
}

/// How soon to look again: fast while anything runs or can accrue, slow for
/// legitimate waits that are not the person's, idle otherwise. A job that
/// holds the foreground at the person's own terminal read (`at_person`: its
/// tab and id, [`Job::person_read`]) is the person's wait even while it
/// makes progress the terminal does not show (a full-screen program at its
/// prompt appending to its own log, as agy 1.4.0 does every 5 s while its
/// telemetry is refused): it can accrue nothing while it reads keys, so it
/// is watched again at its next terminal output (the host's edge), as the
/// design's "a session parked at a raw-mode prompt: 0 job samples until its
/// next PTY output" says; one that stalls or spins is still watched fast.
fn demand_of(rows: &[JobRow], at_person: &BTreeSet<(u64, &str)>) -> Demand {
    let mut d = Demand::Idle;
    for r in rows.iter().filter(|r| r.verdict != Verdict::Exited) {
        let reads_keys = at_person.contains(&(r.tab, r.id.as_str()));
        if (r.class == Class::Running && !reads_keys)
            || r.verdict == Verdict::Stuck
            || matches!(r.standing, Some(Standing::Capable | Standing::Capped))
        {
            return Demand::Fast;
        }
        let person = reads_keys
            || matches!(r.verdict, Verdict::WaitingYou | Verdict::Parked)
            || r.wait.as_ref().is_some_and(|w| {
                matches!(
                    w.name,
                    WaitName::TtyPassword | WaitName::TtyBuiltinRead | WaitName::TtyRaw
                )
            });
        if !person {
            d = Demand::Slow;
        }
    }
    d
}
