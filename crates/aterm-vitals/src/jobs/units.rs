// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-tab job units, member tracking, and per-tick stuck-process evaluation.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::step::{self, Cx, Evidence, Fault, Progress, Track};
use super::wait::{self, JobFacts, Scene};
use super::{
    BUSY_ORPHAN_MS, Class, ClassTable, Consumer, Demand, EXITED_KEEP_MS, Findings, GateWord,
    HeldEpisode, HeldProc, Holder, InputGate, JobCounters, JobKind, JobProc, JobRow, JobTab,
    JobTick, JobsView, KERNEL_WAIT_MS, MemberMap, Named, NamedRow, NamedWait, ProcState, Standing,
    StartWait, Stop, SweepProc, TermWord, TierB, TtyMode, Verdict, WaitName, Want, ZOMBIE_AGE_MS,
    ZombieParent, Zombies, command_class,
};

/// Descendants walked per tab at most.
const TREE_CAP: usize = 4096;
/// Side writes allowed per 2 s tick beyond the output channel's share.
const SIDE_ALLOWANCE: u64 = 64 * 1024;
/// Child CPU that makes a birth or reap "with work", ns per second.
const CHURN_WORK_NS_PER_S: u64 = 50_000_000;
/// Bytes new children wrote that make churn "with work".
const CHURN_WROTE: u64 = 64 * 1024;
/// How long past the read that saw `fg` resume a job ([`JobTick::read_ms`]) the reader may still
/// feed the shell's notice of `fg`, which the shell prints before it hands the job the terminal.
const FG_NOTICE_MS: u64 = 1_000;
/// Two wakes of a nested session's reader this close together, each over a tick keys were
/// driven into the tab in, show the person's keys reach it.
const RELAY_WAKES_MS: u64 = 60_000;

/// The shortest span a process's calls are judged and its idle rate learned over, the host's fast
/// cadence ([`super::Demand::Fast`]): over a shorter one a single wake reads as a high rate.
const RATE_MS: u64 = 2_000;

/// Remembered process member of a job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    /// Process start identity.
    pub start: Option<u64>,
    /// Counters from the last reading.
    pub counters: Option<JobCounters>,
    /// Process display name.
    pub name: String,
    /// Host timestamp (ms) when first observed.
    pub first_ms: u64,
    /// The most system calls a minute a pass read it make over a tick of at least [`RATE_MS`]
    /// that no key was typed in and the tab printed nothing in, nor in the next: its own idle
    /// wakes (a runtime's timer threads, a server's poll timeout). Past it, it did more.
    pub idle: Option<u64>,
    /// Its rate over the last tick, while that tick was one its idle rate is learned over: its
    /// idle rate once this tick printed nothing either (a line the reader fed a pass after the
    /// process wrote it is the process's).
    pub pending: Option<u64>,
    /// The passes' readings of its system calls (host ms, count), back to the newest at least
    /// [`RATE_MS`] old: as a neighbour, a pass a moment after the last judges its calls since
    /// that one.
    pub reads: Vec<(u64, u64)>,
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
    /// The last group other than the shell's in front of its block (an `a && b`'s second, the
    /// group a block resumed it under), kept while a tick sees the shell take the terminal back
    /// before `D` (the printed `pgid` is then the shell's). A group one pass saw in front while a
    /// command the shell started was stopped behind it is the job's only once its next pass sees
    /// it there too.
    front_pgid: Option<u32>,
    /// It ran in the background or stopped (`cmd &`, Ctrl-Z): a block that resumes it is `fg`'s,
    /// whose command is not its own, and whose `C` the shell's notice of `fg` follows.
    backgrounded: bool,
    /// Through when the tab's output may be the shell's notice of the `fg` that resumed it: the
    /// resuming tick's read of the group in front plus [`FG_NOTICE_MS`].
    fg_ms: Option<u64>,
    /// The block a pass resumed it in front under while that block's own job kept the block: it
    /// takes the block at a later pass that finds it open with the job still in front.
    deferred: Option<u64>,
    /// Its block, once a pass found another job resumed in front under it (that job's
    /// `deferred`) and no later pass saw a command of its own in front there: the block's `D` may
    /// be the `fg`'s.
    lent: Option<u64>,
    /// What its last tick read of its processes' system calls.
    stir: Stir,
    /// What its last tick read of them since each one's newest reading at least [`RATE_MS`] old
    /// ([`Stir::of_reads`]): what a neighbour's line is weighed against.
    wide: Stir,
    /// A nested session the person's keys reach: two passes at most [`RELAY_WAKES_MS`] apart saw
    /// the session's group in front of its terminal wake from still over a tick keys were driven
    /// into the tab in.
    relays: bool,
    /// The clock of the pass that saw the last such wake, while it is one too few.
    wake_ms: Option<u64>,
    /// A nested session's first terminal word: one its opener keeps every session it opens in
    /// (Emacs turns echo off) names no password prompt.
    first_word: Option<TermWord>,
    /// A nested session whose own terminal is cooked had a line of its reach the tab as a line:
    /// its opener copies its output (`script`), never redraws it (nvim's terminal).
    lines_seen: bool,
    /// The last name the row printed.
    last_name: String,
    /// The last name a member other than the shell gave, the exited row's: the program exits
    /// before the shell writes `D`, and a tick between the two sees the shell alone.
    member_name: String,
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
    /// This tick it held the foreground at the person's own terminal read (`tty-raw` or
    /// `tty-password` from a mode word stamped after its last output, no key queued), whatever its
    /// class ([`demand_of`]).
    person_read: bool,
    row: Option<JobRow>,
}

/// What a tick read of a job's system calls. A block's shell waiting on its command (woken to
/// reap a background job and print its notice) does none of the job's work: its calls count only
/// while it is the job's one live process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stir {
    /// Every live member's count was read at both readings and none moved, and none was born or
    /// reaped (a stopped process makes none, read or not, nor does a zombie).
    Still,
    /// One that was read moved, none past its idle rate ([`Member::idle`]).
    Ticking,
    /// None that was read moved past its idle rate, and one that runs went unread (another
    /// user's), or none was read at an earlier pass.
    Unknown,
    /// None that was read moved past its idle rate, but one was born or reaped (a loop's short
    /// commands), or died since its last reading.
    Churned,
    /// One read at both readings moved past its idle rate.
    Stirred,
}

impl Stir {
    /// What a process whose idle rate is `idle`, read at `at` making `calls` system calls in all,
    /// shows by its calls since its newest reading at least [`RATE_MS`] old (`last`'s calls and
    /// span since the last pass, when it kept none), judged as over [`RATE_MS`] at least. A
    /// neighbour is weighed so: a writer's line read a moment after a pass shows past its rate
    /// only with the calls before it. A job's own credit is judged by its calls since the last
    /// pass alone, as over [`RATE_MS`]: a heartbeat's one wake in that moment shows nothing.
    fn of_reads(
        reads: &[(u64, u64)],
        at: u64,
        calls: u64,
        last: (u64, u64),
        idle: Option<u64>,
    ) -> Self {
        let (since, from) = reads
            .iter()
            .rev()
            .find(|(t, _)| t.saturating_add(RATE_MS) <= at)
            .map_or(last, |(t, c)| (calls.abs_diff(*c), at.saturating_sub(*t)));
        Self::of_calls(since, from, idle)
    }

    /// What `calls` system calls over `dt_ms` show of a process whose idle rate is `idle`, judged
    /// as over a tick of at least [`RATE_MS`].
    fn of_calls(calls: u64, dt_ms: u64, idle: Option<u64>) -> Self {
        if idle.is_some_and(|i| per_min(calls, dt_ms.max(RATE_MS)) <= i) {
            Self::Ticking
        } else {
            Self::Stirred
        }
    }
}

/// Whether a command line runs `exec`, as a word of its own (`exec zsh`, `cd ~ && exec "$SHELL"`).
fn runs_exec(command: &str) -> bool {
    command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '{' | '}'))
        .any(|w| w == "exec")
}

/// `n` over `dt_ms`, a minute.
fn per_min(n: u64, dt_ms: u64) -> u64 {
    n.saturating_mul(60_000) / dt_ms.max(1)
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
    /// A block's group in front, of its members: a job this block resumes holds one of them.
    front: Vec<u32>,
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
    /// A live process is in the group holding the terminal, the shell's own aside.
    front_live: bool,
    /// The group holding the terminal, not the shell's, has no live process (its last one left
    /// for a group of its own).
    front_gone: bool,
    /// The group holding the terminal, not the shell's, is a shell doing job control alone at
    /// its line editor (a `bash` run in the tab, at its prompt): the tab's lines are the
    /// background's, as with no group in front.
    front_idle: bool,
    /// Background groups with a process that is neither stopped nor a zombie (one that can print).
    bg_units: usize,
}

/// Per-instance stuck-process evaluation engine.
#[derive(Clone, Debug, Default)]
pub struct Jobs {
    jobs: Vec<Job>,
    classes: ClassTable,
    zombies: HashMap<u32, (u64, u32)>,
    zombie_parents: HashMap<u32, u64>,
    d_since: HashMap<u32, u64>,
    /// The first reading that saw each `(pid, exec)` not started, by what keeps it
    /// ([`StartWait`]), kept for the keys the last reading held.
    held_since: HashMap<(u32, u64, StartWait), u64>,
    /// The backed-up episode, while one is open.
    held_episode: Option<Backlog>,
    /// The episode the last reading closed.
    held_drained: Option<HeldEpisode>,
    /// The programs seen doing job control, by pid, with the start and the program they ran: one
    /// below a tab's shell seen putting a group it made in front (a shell nested in the tab's, a
    /// debugger), so each group it makes is a job of its own, and the tab's root also while its
    /// marks show it at a prompt or its line editor reads the terminal raw in front
    /// ([`discover`]). A pid that ran another program since (`exec ninja`) does none.
    job_control: HashMap<u32, (Option<u64>, String)>,
    /// What each tab's last pass read of its root.
    roots: HashMap<u64, RootSeen>,
    /// Each tab's processes in its shell's own group, by pid and known birth: the block open at
    /// the first pass that saw that process there (none between blocks).
    born: HashMap<u64, HashMap<u32, MakerSeen>>,
    #[cfg(any(test, debug_assertions))]
    fault: Option<Fault>,
}

/// What a tab's last pass read of its root.
#[derive(Clone, Debug, Default)]
struct RootSeen {
    /// The program it ran.
    name: Option<String>,
    /// The newest of the shell's marks it read (`JobTab::shell_ms`).
    mark_ms: Option<u64>,
    /// The program the root ran at the last pass that read a mark newer than the last pass's,
    /// with no block open and the root running what the last pass saw: the program writing the
    /// prompt's marks (a block's `C` is a mark too, and the root may have run `exec` past it).
    prompt_name: Option<String>,
    /// The open block whose command ran `exec` that it sat at a prompt under, alone in its group
    /// in front with its line editor reading the terminal raw, a shell doing job control, and
    /// whether a second pass in a row saw it so.
    unmarked: Option<(u64, bool)>,
}

/// A maker's last known birth and first observed block. Missing birth observations retain the
/// earlier block; only a definite change proves that a reused pid belongs to a new process.
#[derive(Clone, Debug)]
struct MakerSeen {
    start: Option<u64>,
    block: Option<u64>,
}

/// An open backed-up episode as the engine keeps it ([`HeldEpisode`] is what a view says).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Backlog {
    opened_ms: u64,
    last_held_ms: u64,
    peak: u32,
    oldest_ms: u64,
    /// Readings in a row that found nothing held.
    empty: u8,
}

impl Backlog {
    const fn view(self) -> HeldEpisode {
        HeldEpisode {
            opened_ms: self.opened_ms,
            for_ms: self.last_held_ms.saturating_sub(self.opened_ms),
            peak: self.peak,
            oldest_ms: self.oldest_ms,
        }
    }
}

impl Jobs {
    /// Creates an empty [`Jobs`] engine.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Injects a conformance negative-control `fault`.
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

    /// Returns the learned command-class table.
    #[must_use]
    pub fn classes(&self) -> &ClassTable {
        &self.classes
    }

    /// Records a median progress interval for `class`.
    pub fn learn(&mut self, class: &str, median_ms: u64) {
        self.classes.learn(class, median_ms);
    }

    /// Plans which processes require detailed and descriptor readings this tick.
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

    /// Evaluates one probe tick and returns job rows, findings, and next-tick demand.
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
        self.note_held(at, &tick.held);
        let flat_on_dylib = self.flat_on_a_fresh_dylib(&facts);

        let alive = |pid: u32, start: Option<u64>| -> bool {
            sweep.contains_key(&pid)
                && match (start, facts.get(&pid).and_then(|f| f.start)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
        };

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

        let mut matched: BTreeSet<usize> = BTreeSet::new();
        let mut tab_facts: HashMap<u64, TabFacts> = HashMap::new();
        let mut assigned: HashMap<usize, Vec<u32>> = HashMap::new();
        let mut handed_to: HashMap<u32, usize> = HashMap::new();
        let mut merged: BTreeSet<usize> = BTreeSet::new();
        let mut gave_way: BTreeSet<usize> = BTreeSet::new();
        self.job_control.retain(|p, (start, name)| {
            alive(*p, *start) && sweep.get(p).is_some_and(|s| s.name == *name)
        });
        // A shell that gave way to another in place (`exec zsh`) writes no marks again (the new
        // one's integration does not load), so the block it ran in never closes: two passes in a
        // row that find the root at a prompt under it (alone in its group in front, its line
        // editor reading the terminal raw, a shell doing job control) close it, with no status,
        // when its command ran `exec`: no other block leaves a shell at a prompt under it (pwsh
        // runs a cmdlet in itself at a raw word, and `vared` reads raw keys under its block). And
        // the root is a shell while it runs the program that wrote its prompt's marks (passes may
        // read the block before the `C` of an `exec` and the root after it) or, in a tab whose
        // marks never showed, at a pass that finds its line editor reading raw keys in front,
        // running the program the last pass saw.
        let mut roots = std::mem::take(&mut self.roots);
        roots.retain(|k, _| tick.tabs.iter().any(|t| t.key == *k));
        self.born
            .retain(|k, _| tick.tabs.iter().any(|t| t.key == *k));
        let mut root_shell: HashMap<u64, bool> = HashMap::new();
        let tabs_now: Vec<Cow<'_, JobTab>> = tick
            .tabs
            .iter()
            .map(|tab| {
                let seen = roots.entry(tab.key).or_default();
                let root = sweep.get(&tab.shell_pid).filter(|_| tab.shell_pid != 0);
                let name = root.map(|r| r.name.as_str());
                let steady = name.is_some() && seen.name.as_deref().is_none_or(|n| Some(n) == name);
                let rf = facts.get(&tab.shell_pid);
                let pgid = rf.map(|f| f.pgid).or_else(|| root.map(|r| r.pgid));
                let raw_front = pgid.is_some()
                    && rf.and_then(|f| f.tpgid) == pgid
                    && tab.tty.is_some_and(|m| !m.canonical);
                let open = tab.block.as_ref().filter(|b| b.open).map(|b| b.id);
                if steady
                    && open.is_none()
                    && tab.shell_ms.is_some()
                    && tab.shell_ms != seen.mark_ms
                {
                    seen.prompt_name = name.map(str::to_owned);
                }
                seen.mark_ms = tab.shell_ms;
                root_shell.insert(
                    tab.key,
                    if tab.shell_ms.is_some() {
                        seen.prompt_name.as_deref() == name
                    } else {
                        steady && raw_front
                    },
                );
                let alone = pgid.is_some_and(|g| {
                    !tick.sweep.iter().any(|p| {
                        p.pgid == g && p.pid != tab.shell_pid && p.state != ProcState::Zombie
                    })
                });
                let execs = tab
                    .block
                    .as_ref()
                    .filter(|b| b.open)
                    .is_some_and(|b| runs_exec(&b.command));
                let prompt =
                    raw_front && alone && execs && self.job_control.contains_key(&tab.shell_pid);
                seen.unmarked = match (open, seen.unmarked) {
                    (Some(b), Some((u, true))) if u == b => Some((b, true)),
                    (Some(b), Some((u, false))) if u == b && prompt => Some((b, true)),
                    (Some(b), _) if prompt => Some((b, false)),
                    _ => None,
                };
                seen.name = name.map(str::to_owned);
                match (open, seen.unmarked) {
                    (Some(b), Some((u, true))) if u == b => {
                        let mut t = tab.clone();
                        if let Some(k) = t.block.as_mut() {
                            k.open = false;
                        }
                        Cow::Owned(t)
                    }
                    _ => Cow::Borrowed(tab),
                }
            })
            .collect();
        self.roots = roots;
        for tab in tabs_now.iter().map(|t| &**t) {
            let (units, tf) = discover(
                tab,
                &sweep,
                &facts,
                &children,
                &mut self.job_control,
                self.born.entry(tab.key).or_default(),
                root_shell.get(&tab.key).copied().unwrap_or(false),
            );
            tab_facts.insert(tab.key, tf);
            // A command the shell started that job control stopped (a debugger's traced child has
            // the debugger for its parent).
            let shell_stopped = |p: &u32| {
                sweep.get(p).is_some_and(|s| s.ppid == tab.shell_pid)
                    && facts
                        .get(p)
                        .map_or_else(|| sweep.get(p).map(|s| s.state), |f| Some(f.state))
                        == Some(ProcState::Stopped)
            };
            for u in units {
                // A group in front with no block open is the job that holds one of its
                // processes, until that job's pass keys it by the group: its block closed while
                // the group held the terminal (a `fg` no block marks resumed it). A detached call
                // is no such group, even one a pass saw in its agent's group before its `setsid`:
                // the agent's job stays a shell job.
                let found = self
                    .jobs
                    .iter()
                    .position(|j| j.exited_ms.is_none() && j.key.same(&u.key))
                    .or_else(|| {
                        u.members
                            .iter()
                            .find_map(|p| claimed.get(p).copied())
                            .filter(|_| u.block.is_none() && u.kind == JobKind::Shell)
                    });
                // A job whose block a newer one replaced hands this block what it holds of it
                // (the shell, and a group in front it took before `D` was fed): its command is
                // over.
                let handed = |c: usize| {
                    let j = &self.jobs[c];
                    u.block.is_some() && j.block.is_some() && j.block != u.block
                };
                let mut free: Vec<u32> = u
                    .members
                    .iter()
                    .copied()
                    .filter(|p| {
                        claimed
                            .get(p)
                            .is_none_or(|c| Some(*c) == found || handed(*c))
                    })
                    .collect();
                let stopped_behind = |j: &Job| {
                    j.members
                        .keys()
                        .any(|p| !u.members.contains(p) && shell_stopped(p))
                };
                let resumes = u.block.and_then(|_| resumed(&u, tab, &claimed, &self.jobs));
                let behind = found.is_some_and(|f| stopped_behind(&self.jobs[f]));
                // The block's own command lives on in the shell's tree outside the unit (a
                // debugger whose child it put in front, a nested shell that brought a job
                // forward): the job in front is part of it. A survivor launchd adopted, a command
                // that exited before the pass read it, and one stopped behind the group in front
                // are no such command.
                let lives = |f: usize| {
                    let j = &self.jobs[f];
                    let mut unknown = false;
                    for (p, m) in &j.members {
                        if Some(*p) != j.shell
                            && !u.members.contains(p)
                            && alive(*p, m.start)
                            && facts.contains_key(p)
                        {
                            match in_tree(*p, tab.shell_pid, &sweep) {
                                Some(true) => return Some(true),
                                Some(false) => {}
                                None => unknown = true,
                            }
                        }
                    }
                    (!unknown).then_some(false)
                };
                let own_command = resumes
                    .and(found)
                    .filter(|_| !behind)
                    .map(|f| (f, lives(f)));
                // An incomplete parent chain proves neither a command outside the tree nor
                // that it owns the job in front. Keep both jobs until a pass can decide.
                if own_command.is_some_and(|(_, live)| live.is_none()) {
                    continue;
                }
                let unsure = found.is_some_and(|f| self.jobs[f].front_pgid.is_some());
                if let Some(i) = resumes
                    && let Some((f, Some(true))) = own_command
                {
                    let taken = std::mem::take(&mut self.jobs[i].members);
                    for p in taken.keys() {
                        claimed.insert(*p, f);
                    }
                    self.jobs[f].members.extend(taken);
                    self.jobs[i].exited_ms = Some(at);
                    merged.insert(i);
                } else if let Some(i) =
                    resumes.filter(|i| unsure && self.jobs[*i].deferred != u.block)
                {
                    // The group in front is a background job's while the block's own job had a
                    // command of its own in front: the pass read the tab before `D` (a `fg`
                    // typed ahead), or the block's list brought the job forward (`make && fg`).
                    // The job resumes in front, and the block stays its own job's until a pass
                    // finds it still open with the job in front. One deferral stands under a
                    // block: the job a list brought forward last is the one whose status its
                    // `fg` returns.
                    for (k, o) in self.jobs.iter_mut().enumerate() {
                        if k != i && o.key.tab == tab.key && o.deferred == u.block {
                            o.deferred = None;
                        }
                    }
                    let j = &mut self.jobs[i];
                    if j.backgrounded {
                        j.fg_ms = Some(tick.read_ms.max(at) + FG_NOTICE_MS);
                    }
                    j.front_pgid = Some(u.pgid);
                    j.kind = JobKind::Shell;
                    j.consumer = Consumer::PersonFg;
                    j.deferred = u.block;
                    for p in free.iter().filter(|p| u.front.contains(p)) {
                        if claimed.get(p).is_some_and(|c| *c != i) {
                            handed_to.insert(*p, i);
                        }
                        claimed.insert(*p, i);
                        assigned.entry(i).or_default().push(*p);
                    }
                    free.retain(|p| !u.front.contains(p));
                    if let Some(f) = found {
                        self.jobs[f].lent = u.block;
                    }
                } else if let Some(i) = resumes {
                    // Else the block's own job gives way to the job its group in front is
                    // (`fg`'s block read before the shell handed the terminal over, a list that
                    // ends in `fg`). One that had a command of its own in front parks with it
                    // stopped behind, else ends with no status (the block's `D` is the resumed
                    // job's); one that never had (`fg`'s) leaves no row.
                    if let Some(f) = found {
                        if unsure {
                            gave_way.insert(f);
                        } else {
                            self.jobs[f].exited_ms = Some(at);
                            merged.insert(f);
                        }
                    }
                    let j = &mut self.jobs[i];
                    // A job a tick saw in front before its block takes the block's command; one
                    // that ran in the background (a stopped or `&` job `fg` resumed) keeps its
                    // own (the new block's is `fg`).
                    if j.block.is_none() && !j.backgrounded && !u.command.is_empty() {
                        j.command.clone_from(&u.command);
                    }
                    // A job a newer block resumes was stopped under its old one (`fg` resumes
                    // it). A job a pass already resumed in front under this block keeps that
                    // pass's notice window.
                    j.backgrounded |= j.block.is_some();
                    if j.backgrounded && j.deferred.take() != u.block {
                        j.fg_ms = Some(tick.read_ms.max(at) + FG_NOTICE_MS);
                    }
                    j.front_pgid = Some(u.pgid);
                    j.kind = JobKind::Shell;
                    j.consumer = Consumer::PersonFg;
                    j.block = u.block;
                    // The block is its identity from now on, so the next tick finds it again and
                    // opens no second job; the shell is a member, as in every block's job.
                    j.key.block = u.block;
                    j.shell = u.shell;
                    for p in &free {
                        if claimed.get(p).is_some_and(|c| *c != i) {
                            handed_to.insert(*p, i);
                        }
                        claimed.insert(*p, i);
                    }
                    matched.insert(i);
                    assigned
                        .entry(i)
                        .or_default()
                        .extend(free.iter().copied().filter(|p| Some(*p) != u.shell));
                    continue;
                }
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
                    if claimed.get(p).is_some_and(|c| *c != idx) {
                        handed_to.insert(*p, idx);
                    }
                    claimed.insert(*p, idx);
                }
                let j = &mut self.jobs[idx];
                if u.key.start.is_some() && j.key.start.is_none() {
                    j.key.start = u.key.start;
                }
                if j.kind != JobKind::AgentCall || u.kind == JobKind::AgentCall {
                    j.kind = u.kind;
                    j.consumer = u.consumer;
                    j.backgrounded |= u.kind == JobKind::ShellBg;
                }
                let was = j.pgid;
                let mine = u.front.iter().all(|p| claimed.get(p) == Some(&idx));
                // A command of its own in front under its block again (the list moved on past
                // the job it brought forward): the block's `D` is its own.
                if mine && !u.front.is_empty() {
                    j.lent = None;
                }
                if j.kind != JobKind::AgentCall && mine {
                    // The group in front of a block changes command by command; another job's
                    // (one `fg` resumed behind a stop whose `D` the reader has not fed) is none of
                    // this one's.
                    j.pgid = u.pgid;
                }
                // A group in front while a command the shell started is stopped outside the
                // unit may be a command typed ahead of the stop (a debugger's traced child is
                // the debugger's own); still in front at the job's next pass, it is the job's (a
                // command typed ahead has its own block by then).
                if j.kind == JobKind::Shell
                    && u.pgid != tf.shell_pgid
                    && (!stopped_behind(j) || was == u.pgid)
                {
                    j.front_pgid = Some(u.pgid);
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

        for tab in tabs_now.iter().map(|t| &**t) {
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

        let tabs: HashMap<u64, &JobTab> = tabs_now.iter().map(|t| (t.key, &**t)).collect();
        // Each block a job was resumed in front under while the block's own job kept it, and
        // whether that job lives on as the shell waits for it: a child of the shell's outside
        // its group (stopped again, in front, or behind a later command of the list). `fg`
        // waits for no grandchild a quit job's leader left, nor for a process substitution.
        let mut deferring: HashMap<(u64, u64), bool> = HashMap::new();
        for j in &self.jobs {
            let Some(b) = j.deferred else { continue };
            let shell_pgid = tab_facts.get(&j.key.tab).map(|f| f.shell_pgid);
            let shell = tabs.get(&j.key.tab).map(|t| t.shell_pid);
            let lives = j.members.iter().any(|(pid, member)| {
                alive(*pid, member.start)
                    && sweep.get(pid).is_some_and(|s| {
                        s.state != ProcState::Zombie
                            && Some(s.ppid) == shell
                            && Some(s.pgid) != shell_pgid
                    })
            });
            *deferring.entry((j.key.tab, b)).or_default() |= lives;
        }
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
        // What each job's members did since its last pass (as a neighbour, since their readings
        // at least RATE_MS old), by this tick's readings of the members it had (one born or
        // reaped, or another user's that runs unread), and what its last pass read: a
        // neighbour's, whatever the jobs' order.
        let stir_last: Vec<Stir> = self.jobs.iter().map(|j| j.stir.max(j.wide)).collect();
        let read_now: Vec<Stir> = self
            .jobs
            .iter()
            .enumerate()
            .map(|(k, j)| {
                if j.exited_ms.is_some() {
                    return Stir::Still;
                }
                // A block's shell counts here: the notice it prints as it reaps a background job
                // is a line of its own. One that died since its reading may have written its last
                // line; a zombie, as a stopped process, writes nothing more.
                let dt = j.last_tick_ms.map_or(0, |l| at.saturating_sub(l));
                let mut read = Stir::Still;
                for (p, m) in &j.members {
                    let f = facts.get(p);
                    let now = f.and_then(|f| f.counters).and_then(|c| c.syscalls);
                    let was = m.counters.and_then(|c| c.syscalls);
                    let state = f.map(|f| f.state);
                    let runs =
                        state.is_some_and(|s| !matches!(s, ProcState::Stopped | ProcState::Zombie));
                    read = match (now, was) {
                        _ if !alive(*p, m.start) => read.max(Stir::Churned),
                        (_, Some(_)) if state == Some(ProcState::Zombie) => read.max(Stir::Churned),
                        (Some(a), Some(b)) if a != b => {
                            read.max(Stir::of_reads(&m.reads, at, a, (a.abs_diff(b), dt), m.idle))
                        }
                        (Some(_), Some(_)) => read,
                        _ if runs => read.max(Stir::Unknown),
                        _ => read,
                    };
                }
                let born = assigned.get(&k).is_some_and(|ps| {
                    ps.iter()
                        .any(|p| !j.members.contains_key(p) && Some(*p) != j.shell)
                });
                if born { read.max(Stir::Churned) } else { read }
            })
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
            // A job whose command is unknown has no class: it reads its own μ or the prior.
            let class_mu = (!self.jobs[i].command.is_empty())
                .then(|| {
                    self.classes
                        .mu(&command_class(&self.jobs[i].command, self.jobs[i].kind))
                })
                .flatten();
            let elsewhere = scene_elsewhere(i);
            // Another job of the tab not read still at this pass or its last (also one that ended
            // at either) may have written what the tab printed. A nested session is no neighbour
            // (its opener's job is judged in its stead), nor is a job that relays this one's
            // writes up to the tab.
            let me = &self.jobs[i];
            let relays_me = |o: &Job| {
                let mut cur = me;
                for _ in 0..n {
                    let Some(op) = cur.consumer_pid.filter(|_| cur.kind == JobKind::Nested) else {
                        return false;
                    };
                    if o.members.contains_key(&op) {
                        return true;
                    }
                    match self.jobs.iter().find(|j| {
                        j.exited_ms.is_none()
                            && j.key.tab == me.key.tab
                            && j.members.contains_key(&op)
                    }) {
                        Some(j) => cur = j,
                        None => return false,
                    }
                }
                false
            };
            let neighbour_stirred = self.jobs.iter().enumerate().any(|(k, o)| {
                k != i
                    && o.key.tab == me.key.tab
                    && o.kind != JobKind::Nested
                    && o.exited_ms
                        .is_none_or(|e| me.last_tick_ms.is_none_or(|l| e >= l))
                    && (read_now[k] > Stir::Ticking || stir_last[k] > Stir::Ticking)
                    && !relays_me(o)
            });
            let job = &mut self.jobs[i];

            let prev: MemberMap = job.members.clone();
            let first_tick = job.last_tick_ms.is_none();
            let mut now: MemberMap = BTreeMap::new();
            let candidates: BTreeSet<u32> = prev
                .keys()
                .copied()
                .filter(|p| handed_to.get(p).is_none_or(|to| *to == i))
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
                let mut reads = prev.get(&pid).map(|m| m.reads.clone()).unwrap_or_default();
                if let Some(c) = f.and_then(|f| f.counters).and_then(|c| c.syscalls) {
                    reads.push((at, c));
                }
                if let Some(k) = reads
                    .iter()
                    .rposition(|(t, _)| t.saturating_add(RATE_MS) <= at)
                {
                    reads.drain(..k);
                }
                now.insert(
                    pid,
                    Member {
                        start: start.or_else(|| f.and_then(|f| f.start)),
                        counters: f.and_then(|f| f.counters),
                        name,
                        first_ms: prev.get(&pid).map_or(at, |m| m.first_ms),
                        idle: prev.get(&pid).and_then(|m| m.idle),
                        pending: prev.get(&pid).and_then(|m| m.pending),
                        reads,
                    },
                );
            }
            let zombie = |pid: &u32| sweep.get(pid).is_some_and(|s| s.state == ProcState::Zombie);

            // A block's job that gave the block way is done with it, as if it had closed.
            let block_closed = gave_way.contains(&i)
                || job.block.is_some_and(|b| {
                    tab.and_then(|t| t.block.as_ref())
                        .is_none_or(|blk| blk.id != b || !blk.open)
                });
            let shell = job.shell;
            let pgid_of = |p: &u32| {
                facts
                    .get(p)
                    .map(|f| f.pgid)
                    .or_else(|| sweep.get(p).map(|s| s.pgid))
            };
            let state_of = |p: &u32| {
                facts
                    .get(p)
                    .map_or_else(|| sweep.get(p).map(|s| s.state), |f| Some(f.state))
            };
            let shellish = matches!(job.kind, JobKind::Shell | JobKind::ShellBg);
            // One of the job's own groups other than the shell's holds the terminal (a debugger at
            // its prompt, the command bash moved on to past a stopped one): a member stopped in
            // another group is no stop of the job's.
            let own_front = shellish
                && tf.tpgid.is_some_and(|t| {
                    t != tf.shell_pgid && now.keys().any(|p| !zombie(p) && pgid_of(p) == Some(t))
                });
            // A block that closed while one of the job's own groups holds the terminal again: if
            // the job ran in the background or the block ended in a stop, the pass read a `D`
            // before the `fg` that resumed the job (the shell takes the terminal back before it
            // prints a `D`). The job runs on in front without the block, its status or the shell,
            // keyed by its group as a parked job is, so that group's unit finds it while no block
            // opens (a zle widget's `fg` writes none). Otherwise a command typed ahead of the
            // block's end closed it: the job ends with its block and leaves that group to its
            // own unit.
            if block_closed
                && own_front
                && let Some(g) = tf.tpgid
            {
                let resumed_here = job.backgrounded
                    || job
                        .block
                        .is_some_and(|b| tab.is_some_and(|t| stopped_block(t, b)));
                if resumed_here {
                    job.block = None;
                    job.backgrounded = true;
                    if let Some(sh) = job.shell.take() {
                        now.remove(&sh);
                    }
                    job.pgid = g;
                    job.key = Key {
                        tab: job.key.tab,
                        leader: g,
                        start: now.get(&g).and_then(|m| m.start),
                        block: None,
                    };
                } else {
                    now.retain(|p, _| pgid_of(p) != Some(g));
                }
            }
            // A nested session's terminal held by one of its groups (the command its shell runs in
            // front, that shell at its prompt): a member stopped in another group is a job its
            // shell parked, and the session is judged by its group in front.
            let nested_front = (job.kind == JobKind::Nested)
                .then(|| facts.get(&job.key.leader).and_then(|f| f.tpgid))
                .flatten()
                .filter(|t| now.keys().any(|p| !zombie(p) && pgid_of(p) == Some(*t)));
            // Job control made a group when its maker does job control (`Jobs::job_control`: the
            // tab's shell, a nested one, a debugger): a stop in it is theirs to resume. A stopped
            // member in such a group speaks for the job before one in a group another program
            // made, whatever their pids.
            let shell_pid = tab.map_or(0, |t| t.shell_pid);
            let controlled = |m: u32| self.job_control.contains_key(&m);
            let maker_in = |g: u32| {
                now.keys()
                    .filter(|p| !zombie(p) && pgid_of(p) == Some(g))
                    .find_map(|p| {
                        sweep
                            .get(p)
                            .filter(|s| pgid_of(&s.ppid) != Some(g))
                            .map(|s| s.ppid)
                    })
            };
            let stopped_member = now
                .keys()
                .filter(|p| {
                    !zombie(p)
                        && state_of(p) == Some(ProcState::Stopped)
                        && Some(**p) != shell
                        && !(own_front && pgid_of(p) != tf.tpgid)
                        && !nested_front.is_some_and(|t| pgid_of(p) != Some(t))
                })
                .min_by_key(|p| {
                    pgid_of(p)
                        .and_then(maker_in)
                        .is_some_and(|m| !controlled(m))
                })
                .map(|p| (*p, now.get(p).map(|m| m.name.clone()).unwrap_or_default()));
            // A stopped member parks the job in the background, as does a block that ended in a
            // stop while its command runs on (`bg` before a tick saw the stop).
            let parked_by = stopped_member.as_ref().map(|(p, _)| *p).or_else(|| {
                job.block
                    .filter(|b| block_closed && tab.is_some_and(|t| stopped_block(t, *b)))
                    .and_then(|_| {
                        now.keys()
                            .copied()
                            .find(|p| !zombie(p) && Some(*p) != shell && *p != shell_pid)
                    })
            });
            if block_closed
                && job.block.is_some()
                && let Some(sp) = parked_by
            {
                let g = facts
                    .get(&sp)
                    .map(|f| f.pgid)
                    .or_else(|| sweep.get(&sp).map(|s| s.pgid))
                    .filter(|g| *g != 0)
                    .unwrap_or(sp);
                if let Some(sh) = job.shell.take() {
                    now.remove(&sh);
                }
                job.block = None;
                job.kind = JobKind::ShellBg;
                job.consumer = Consumer::PersonBg;
                job.pgid = g;
                job.backgrounded = true;
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
                || (matches!(job.kind, JobKind::AgentCall | JobKind::Nested)
                    && !alive(job.key.leader, job.key.start));
            // A block's job that lent its block to a resumed job takes the block's status only
            // when it cannot be the `fg`'s: `fg` returns its job's status (a stop's when it
            // stopped) and a list's `D` comes once that job left, so a `D` that is no stop, read
            // while that job lives on or after a newer block resumed it, is the block's own
            // command's. A job that quit leaves it to nobody.
            if ended && let Some(b) = job.block {
                let d = tab
                    .into_iter()
                    .flat_map(|t| t.block.iter().chain(&t.ended))
                    .find(|x| x.id == b);
                let own = job.lent != Some(b)
                    || (d.is_some_and(|x| !x.stopped)
                        && deferring.get(&(job.key.tab, b)).is_none_or(|lives| *lives));
                job.exit = d.filter(|_| own).and_then(|x| x.status);
            }
            // A job with no block none of whose live members is in a group of its own in the
            // shell's tree is over: what lives on is the shell's (a process substitution) or
            // nobody's (one launchd adopted when its leader exited), an orphan's.
            let over = shellish
                && job.block.is_none()
                && tab.is_some()
                && live
                    .iter()
                    .all(|p| pgid_of(p) == Some(tf.shell_pgid) || adopted(*p, shell_pid, &sweep));
            let exited = ended || live.is_empty() || over;
            if exited {
                let survivors: MemberMap = now
                    .iter()
                    .filter(|(p, _)| {
                        !zombie(p)
                            && **p != shell_pid
                            && Some(**p) != job.shell
                            && facts.contains_key(*p)
                    })
                    .map(|(p, m)| (*p, m.clone()))
                    .collect();
                if !survivors.is_empty() && job.kind != JobKind::Orphan {
                    orphans.push(Job::orphan(job.key.tab, survivors, at));
                }
            }

            let dt = job.last_tick_ms.map_or(0, |l| at.saturating_sub(l));
            // A block's own `C` is its job's floor: what the tab printed up to it is the shell's,
            // also when the reader fed `C` after a pass's clock and the job's next tick is its
            // second. (The shell's notice of the `fg` that resumed a job follows `C`: `fg_ms`.)
            let own_start = tab
                .and_then(|t| t.block.as_ref())
                .filter(|b| Some(b.id) == job.block)
                .map(|b| b.started_ms);
            let floor = job.last_tick_ms.map_or_else(
                || own_start.unwrap_or(at),
                |l| l.max(own_start.unwrap_or(0)),
            );
            let mut refused = false;
            let mut rebased = false;
            let mut cpu: u64 = 0;
            let mut child_work: u64 = 0;
            let mut calls: u64 = 0;
            let mut side: u64 = 0;
            let mut pageins: u64 = 0;
            let mut births_wrote: u64 = 0;
            let mut births = 0u32;
            // Which live members made a system call over the tick, by readings of each, and
            // whether one that runs went unread: a job none of whose processes made one wrote
            // nothing to the terminal.
            let mut moved: Vec<u32> = Vec::new();
            let mut ticked = false;
            let mut unread = false;
            let mut idled: Vec<(u32, Option<u64>, Option<u64>)> = Vec::new();
            let waiting_shell = |p: &u32| Some(*p) == job.shell && live.len() > 1;
            let printed = tab
                .and_then(|t| t.output_ms)
                .is_some_and(|o| o > floor && o <= at);
            let typed = tab.is_some_and(|t| t.typed_ms.is_some_and(|s| s > floor && s <= at));
            let drawn = tab.is_some_and(|t| t.drawn_ms.is_some_and(|s| s > floor && s <= at));
            for pid in &live {
                let m = &now[pid];
                let stopped = state_of(pid) == Some(ProcState::Stopped);
                let Some(c) = m.counters else {
                    refused = true;
                    unread |= !stopped;
                    continue;
                };
                let was = prev.get(pid).and_then(|p| p.counters);
                match (c.syscalls, was.and_then(|p| p.syscalls)) {
                    (Some(a), Some(b)) => {
                        let d = a.abs_diff(b);
                        if d > 0 && !waiting_shell(pid) {
                            match Stir::of_calls(d, dt, m.idle) {
                                Stir::Stirred => moved.push(*pid),
                                _ => ticked = true,
                            }
                        }
                        // What it made over a tick no key was typed in is its idle rate once the
                        // tab printed nothing in that tick or this one.
                        let idle = m.idle.max(m.pending.filter(|_| !printed));
                        let rate = (dt >= RATE_MS && !printed && !typed).then(|| per_min(d, dt));
                        idled.push((*pid, idle, rate));
                    }
                    _ => unread |= !stopped,
                }
                match was {
                    Some(p) => {
                        cpu += c.cpu_ns.saturating_sub(p.cpu_ns);
                        child_work += delta(c.child_cpu_ns, p.child_cpu_ns);
                        calls += delta(c.syscalls, p.syscalls)
                            + delta(c.wakeups.or(c.ctx_switches), p.wakeups.or(p.ctx_switches));
                        side += delta(c.logical_writes, p.logical_writes);
                        pageins += delta(c.pageins, p.pageins);
                    }
                    None if prev.contains_key(pid) => rebased = true,
                    // The job's shell lived long before it joined (`fg`, a block learned late,
                    // a block whose predecessor held it at its first tick): a baseline, never a
                    // birth with its lifetime's CPU and writes.
                    None if Some(*pid) == job.shell => {}
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
            // A member that left the job alive (the shell at a stop, one a newer block took)
            // is no reap: churn is processes exiting.
            let reaps = u32::try_from(
                prev.iter()
                    .filter(|(p, m)| !now.contains_key(p) && !alive(**p, m.start))
                    .count(),
            )
            .unwrap_or(u32::MAX);
            for (pid, idle, pending) in idled {
                if let Some(m) = now.get_mut(&pid) {
                    m.idle = idle;
                    m.pending = pending;
                }
            }
            let churn = births + reaps > 0;
            let stir = if !moved.is_empty() {
                Stir::Stirred
            } else if churn {
                Stir::Churned
            } else if unread {
                Stir::Unknown
            } else if ticked {
                Stir::Ticking
            } else {
                Stir::Still
            };
            let churn_work = child_work.saturating_mul(1000)
                >= CHURN_WORK_NS_PER_S.saturating_mul(dt.max(1))
                || births_wrote >= CHURN_WROTE;

            // A shell job holds the terminal unless a live group in front is another job's (one a
            // pass resumed in front under the block's own job); a group in front that emptied (its
            // last process left for a group of its own) is no job's, and the tab's one background
            // group (the pipeline's stage that left, `timeout`'s) may be printing.
            let holds_fg = job.kind == JobKind::Shell && (own_front || !tf.front_live);
            // A nested session's opener holds the terminal, through each session it sits in: what
            // the session writes, the opener relays (sudo, `script`), or it draws its own (an
            // agent's redraws).
            let opener_front = job.kind == JobKind::Nested
                && job
                    .consumer_pid
                    .is_some_and(|o| in_front(o, shell_pid, tf.tpgid, &sweep, &facts));
            // The person's keys reach a nested session whose group in front of its terminal wakes
            // from still (or its idle rate) over a tick keys were driven into the tab in, its opener in front
            // (`script`, sudo and an editor's terminal relay them; an agent's pty call takes the
            // agent's keys alone, which may wake it in such a tick by chance, seldom twice so
            // close). Two such wakes at most RELAY_WAKES_MS apart show it relays, and it does from
            // then on. A still over such a tick shows nothing: the opener may hold the keys
            // (Emacs's comint until Enter) or send them to another of its windows (nvim), and a
            // reader outside the session may take them (ssh's shared connection).
            let term = facts.get(&job.key.leader).and_then(|f| f.term);
            if let Some(g) = nested_front
                && opener_front
                && job.stir <= Stir::Ticking
                && typed
                && moved.iter().any(|p| pgid_of(p) == Some(g))
            {
                if job
                    .wake_ms
                    .is_some_and(|w| at.saturating_sub(w) <= RELAY_WAKES_MS)
                {
                    job.relays = true;
                } else {
                    job.wake_ms = Some(at);
                }
            }
            // And the session's own terminal is one of its groups', the person's keys seen
            // reaching it: its reader in front takes them, by that terminal's word.
            let nested_holds = opener_front && nested_front.is_some() && job.relays;
            let holds = holds_fg || nested_holds;
            // What the terminal printed is a job's only while one of its processes made system
            // calls past its idle rate (this tick or the last, which a line the reader fed past
            // the clock follows): a job whose processes all sit blocked wrote none of it. One whose
            // processes only ticked at their idle rates (a runtime's timers) may have, as may one
            // whose running processes went unread (another user's), one whose only stir was its
            // short commands' births and reaps (a loop polling with `sleep`), and a server outside
            // the tab's tree that the job in front talks to over a socket (ssh's shared
            // connection, tmux's server), unless another job of the tab may have written.
            let either = |s: Stir| stir == s || job.stir == s;
            let relay = holds_fg
                && live.iter().any(|p| {
                    facts
                        .get(p)
                        .and_then(|f| f.fds.as_deref())
                        .is_some_and(|fds| {
                            fds.iter().any(|f| matches!(f.kind, super::FdKind::Socket))
                        })
                });
            // A nested session takes none over a tick keys were typed in, or one the tab's screen
            // was drawn in, unless it made calls of its own: its opener echoes the person's keys
            // (nvim's other window, Emacs's buffer) and paints on its own (nvim's statusline).
            let stirred = either(Stir::Stirred)
                || (!neighbour_stirred
                    && !(job.kind == JobKind::Nested && (typed || drawn))
                    && (stir != Stir::Still || job.stir != Stir::Still || relay));
            job.stir = stir;
            job.wide = read_now[i];
            let credited_pty = stirred
                && (holds_fg
                    || opener_front
                    || (job.kind == JobKind::ShellBg
                        && (!tf.has_fg || tf.front_gone || tf.front_idle)
                        && tf.bg_units == 1
                        && live.iter().any(|p| state_of(p) != Some(ProcState::Stopped))));
            let mut progress: Option<(Progress, u64)> = None;
            let mut out_growth: u64 = 0;
            let fresh = |stamp: Option<u64>| stamp.filter(|s| *s > floor && *s <= at);
            // The tab's output is a job's only over a tick the shell wrote no mark in (a block's
            // own `C` is its floor; else it may be the prompt, a stop's notice, or a command that
            // ran between two ticks) and past the notice of the `fg` that resumed the job.
            let shells = tab.is_some_and(|t| t.shell_ms.is_some_and(|s| s > floor));
            let own = |stamp: Option<u64>| {
                fresh(stamp).filter(|s| !shells && job.fg_ms.is_none_or(|f| *s > f))
            };
            if let Some(t) = tab.filter(|_| credited_pty) {
                // The mode word is the foreground reader's (a background job's is the shell's
                // line editor, raw at its prompt): a background writer is judged by its lines.
                // A nested session at a cooked word of its own, once one of its lines reached the
                // tab as a line, is judged by its lines too: what it prints without one (a
                // spinner's `\r`) is no progress, though its opener keeps the tab's word raw. An
                // opener that draws the screen (nvim) copies none of its lines as lines.
                let session_lines = job.kind == JobKind::Nested
                    && term.is_some_and(|w| w.canonical)
                    && fresh(t.drawn_ms).is_none();
                let canonical = job.kind == JobKind::ShellBg
                    || t.tty.is_none_or(|m| m.canonical)
                    || (session_lines && job.lines_seen);
                let raw_counts = fault == Some(Fault::RawBytesProgress);
                let line = canonical && !raw_counts;
                if let Some(s) = own(t.line_output_ms).filter(|_| line || session_lines) {
                    progress = Some((Progress::Line, s));
                    job.lines_seen |= session_lines;
                } else if let Some(s) = own(t.output_ms).filter(|_| !line) {
                    progress = Some((Progress::Raw, s));
                }
                if let Some(b) = job.pty_bytes {
                    out_growth = t.output_bytes.saturating_sub(b);
                }
                job.pty_bytes = Some(t.output_bytes);
            }
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
                            ..
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

            // A member stopped outside the group in front is parked (job control took the terminal
            // from it), unless a program that does no job control made its group (a ninja step
            // stopped at an ssh passphrase prompt): that stop is the program's job's, held while
            // the program runs in the background (`ninja &`), since no `fg` resumes it. Stopped in
            // the group in front, a member holds that group (a pipeline's stage). A nested
            // session's member is parked while its opener is out of front (sudo stops itself once
            // its command stops) or outside its terminal's group in front.
            let session_front = facts.get(&job.key.leader).and_then(|f| f.tpgid);
            let maker_of = |g: u32| {
                live.iter().filter(|p| pgid_of(p) == Some(g)).find_map(|p| {
                    sweep
                        .get(p)
                        .filter(|s| pgid_of(&s.ppid) != Some(g))
                        .map(|s| s.ppid)
                })
            };
            let stopped = stopped_member.map(|(pid, name)| Stop {
                parked: (shellish
                    && tf.tpgid.is_some_and(|t| pgid_of(&pid) != Some(t))
                    && pgid_of(&pid).and_then(maker_of).is_none_or(controlled))
                    || (job.kind == JobKind::Nested
                        && (!opener_front
                            || session_front.is_some_and(|t| pgid_of(&pid) != Some(t)))),
                pid,
                name,
            });
            let zombies =
                zombie_facts(&now, &live, &sweep, &self.zombies, &self.zombie_parents, at);
            let leaves = leaves_of(&live, &now, &sweep);
            let gate = tab.map_or(InputGate::default(), |t| t.input);
            let shell_pid = tab.map_or(0, |t| t.shell_pid);
            let shell_name = sweep.get(&shell_pid).map_or("", |s| s.name.as_str());
            // The shell itself reads a line: its `read` builtin (while the root is still the shell
            // that does job control, not a program it gave way to), or a nested session's own
            // leader at its prompt, alone in its group in front of its terminal, reading a
            // cooked line (a shell in Emacs's buffer, which keeps echo off and sends each line at
            // Enter). Either is a line read only while it names no wait of its own (`name_job`).
            let leader_group = pgid_of(&job.key.leader);
            let builtin = (job.shell.is_some()
                && controlled(shell_pid)
                && tf.tpgid == Some(tf.shell_pgid)
                && live.iter().all(|p| Some(*p) == job.shell))
                || (nested_holds
                    && nested_front == leader_group
                    && term.is_some_and(|w| w.canonical && w.queued == 0)
                    && live
                        .iter()
                        .filter(|p| pgid_of(p) == leader_group)
                        .all(|p| *p == job.key.leader));
            // A nested session's reader is judged by its own terminal's word, read with this
            // tick's sweep: the tab's is its opener's relay (sudo and `script` keep it raw). A
            // no-echo line word the session has had since its first reading names no password
            // prompt (`first_word`).
            if job.first_word.is_none() {
                job.first_word = term;
            }
            let first = job.first_word;
            let session_word = term
                .filter(|w| {
                    !(w.canonical && !w.echo && first.is_some_and(|f| f.canonical && !f.echo))
                })
                .map(|w| TtyMode {
                    canonical: w.canonical,
                    echo: w.echo,
                    stamped_ms: at,
                });
            let jf = JobFacts {
                holds_fg: holds,
                builtin,
                reader: job.key.leader,
                tty: if nested_holds {
                    session_word
                } else {
                    tab.and_then(|t| t.tty.filter(|m| m.stamped_ms >= t.output_ms.unwrap_or(0)))
                },
                // A nested session's keys queue in its own terminal; the tab's, its relay drains.
                queue_empty: if nested_holds {
                    term.is_none_or(|w| w.queued == 0)
                } else {
                    matches!(gate.word, GateWord::Clear | GateWord::Unmeasured)
                },
                consumer: job.consumer_pid,
            };
            let lanes = tick.lanes;
            let scene = Scene {
                procs: &facts,
                lanes,
                d_age: &d_age,
                elsewhere: &elsewhere,
                program_check: fault != Some(Fault::IgnoreProgramCheck),
            };
            let wait = wait::name_job(&scene, &jf, &leaves, (shell_pid, shell_name));
            // The zombies' parent waits legitimately by a wait of its own, or by the job's wait
            // when that names it: a REPL at its prompt reads the terminal, which no leaf names.
            let zombie_parent_waits = zombies.as_ref().is_some_and(|z| {
                let name = sweep.get(&z.parent).map_or("", |s| s.name.as_str());
                (wait.standing == Standing::Legit && wait.leaf == z.parent)
                    || wait::wait_of(&scene, jf.consumer, z.parent, name)
                        .is_some_and(|w| w.standing == Standing::Legit)
            });
            job.person_read = matches!(wait.name, WaitName::TtyRaw | WaitName::TtyPassword);

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
                holds_fg: holds,
                // The tab's gate is its own reader's: a nested session's relay drains it.
                gate: if holds_fg { gate } else { InputGate::default() },
                zombies,
                zombie_parent_waits,
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
            let leader_name = match now_name(job, &live, &sweep, &facts) {
                Some(n) if !exited || job.last_name.is_empty() => n,
                _ if !job.member_name.is_empty() => job.member_name.clone(),
                _ => job.last_name.clone(),
            };
            if live.iter().any(|p| Some(*p) != job.shell) {
                job.member_name.clone_from(&leader_name);
            }
            job.last_name.clone_from(&leader_name);
            job.members = now;
            job.last_tick_ms = Some(at);
            if exited {
                job.exited_ms = Some(at);
            }

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
            if exited
                && !job.command.is_empty()
                && let Some(own) = job.track.own_mu()
            {
                let class = command_class(&job.command, job.kind);
                self.classes.learn(&class, own);
            }
        }
        self.jobs.extend(orphans);
        let live_ids: BTreeSet<(u64, String)> = self
            .jobs
            .iter()
            .filter(|j| j.exited_ms.is_none())
            .map(|j| (j.key.tab, j.id.clone()))
            .collect();
        // A job another took over leaves no row.
        let mut k = 0;
        self.jobs.retain(|j| {
            k += 1;
            !merged.contains(&(k - 1))
                && match j.exited_ms {
                    None => true,
                    Some(e) => {
                        at.saturating_sub(e) < EXITED_KEEP_MS
                            && !live_ids.contains(&(j.key.tab, j.id.clone()))
                    }
                }
        });

        let rows: Vec<JobRow> = {
            let mut r: Vec<JobRow> = self.jobs.iter().filter_map(|j| j.row.clone()).collect();
            r.sort_by(|a, b| a.tab.cmp(&b.tab).then_with(|| a.id.cmp(&b.id)));
            r
        };
        let findings = self.findings(tick, &sweep, &flat_on_dylib);
        let at_person: BTreeSet<(u64, &str)> = self
            .jobs
            .iter()
            .filter(|j| j.person_read && j.exited_ms.is_none())
            .map(|j| (j.key.tab, j.id.as_str()))
            .collect();
        let demand = demand_of(&rows, &at_person);
        let parked_tabs = parked_tabs(&rows, &at_person);
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
            parked_tabs,
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

    /// Folds this reading's unstarted programs into their first sightings, and those macOS's
    /// program check holds ([`StartWait::ProgramCheck`]) into the backed-up episode. A reading is
    /// backed up when a key held now was held at the reading before (its first sighting is kept)
    /// or three are held at once (a normal exec reads held for under 0.3 ms); the episode opens at
    /// the first backed-up reading and closes at the second reading in a row with nothing held. A
    /// `Translating` or `NotStarted` row is no part of it: its wait is not the check's.
    fn note_held(&mut self, at: u64, held: &[HeldProc]) {
        let all: BTreeSet<(u32, u64, StartWait)> = held.iter().map(held_key).collect();
        let keys: BTreeSet<&(u32, u64, StartWait)> = all
            .iter()
            .filter(|k| k.2 == StartWait::ProgramCheck)
            .collect();
        let backed_up = keys.len() >= 3 || keys.iter().any(|k| self.held_since.contains_key(k));
        self.held_since.retain(|k, _| all.contains(k));
        for k in &all {
            self.held_since.entry(*k).or_insert(at);
        }
        let count = u32::try_from(keys.len()).unwrap_or(u32::MAX);
        let oldest = keys
            .iter()
            .filter_map(|k| self.held_since.get(k))
            .map(|s| at.saturating_sub(*s))
            .max()
            .unwrap_or(0);
        self.held_drained = None;
        match self.held_episode.as_mut() {
            None if backed_up => {
                self.held_episode = Some(Backlog {
                    opened_ms: at,
                    last_held_ms: at,
                    peak: count,
                    oldest_ms: oldest,
                    empty: 0,
                });
            }
            None => {}
            Some(e) if keys.is_empty() => {
                e.empty = e.empty.saturating_add(1);
                if e.empty >= 2 {
                    self.held_drained = Some(e.view());
                    self.held_episode = None;
                }
            }
            Some(e) => {
                e.empty = 0;
                e.last_held_ms = at;
                e.peak = e.peak.max(count);
                e.oldest_ms = e.oldest_ms.max(oldest);
            }
        }
    }

    /// The live job members flat on a fresh dylib at this reading, read before it updates them:
    /// started (not held), holding a dylib a build just wrote open, with the CPU their member
    /// record kept from the reading before at the same start. [`Self::findings`] names them only
    /// inside a backed-up episode.
    fn flat_on_a_fresh_dylib(&self, facts: &HashMap<u32, &JobProc>) -> BTreeMap<u32, String> {
        let mut flat = BTreeMap::new();
        for j in self.jobs.iter().filter(|j| j.exited_ms.is_none()) {
            for (pid, m) in &j.members {
                let Some(f) = facts.get(pid) else { continue };
                let fresh = f.fds.as_deref().is_some_and(|fds| {
                    fds.iter().any(|fd| {
                        matches!(
                            fd.kind,
                            super::FdKind::File {
                                fresh_dylib: true,
                                ..
                            }
                        )
                    })
                });
                let same = m.start.is_some() && m.start == f.start;
                let still = matches!(
                    (m.counters, f.counters),
                    (Some(a), Some(b)) if a.cpu_ns == b.cpu_ns
                );
                if fresh && same && still && !f.program_check {
                    flat.insert(*pid, shown_name(f));
                }
            }
        }
        flat
    }

    fn findings(
        &self,
        tick: &JobTick,
        sweep: &HashMap<u32, &SweepProc>,
        flat_on_dylib: &BTreeMap<u32, String>,
    ) -> Findings {
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
        let mut seen: BTreeSet<(u32, u64, StartWait)> = BTreeSet::new();
        for h in &tick.held {
            let Some(since) = self.held_since.get(&held_key(h)) else {
                continue;
            };
            if !seen.insert(held_key(h)) {
                continue;
            }
            let for_ms = at.saturating_sub(*since);
            let named = Named {
                pid: h.pid,
                name: h.name.clone(),
                tab: tab_of.get(&h.pid).copied(),
                for_ms,
            };
            match h.wait {
                StartWait::ProgramCheck => {
                    if h.aterm {
                        f.program_check_aterm.0 += 1;
                        f.program_check_aterm.1 = f.program_check_aterm.1.max(for_ms);
                    }
                    f.program_check.push(named);
                }
                StartWait::Translating => f.translating.push(named),
                StartWait::NotStarted => f.not_started.push(named),
            }
        }
        for list in [&mut f.program_check, &mut f.translating, &mut f.not_started] {
            list.sort_by_key(|n| n.pid);
        }
        f.program_check_episode = self.held_episode.map(Backlog::view);
        f.program_check_drained = self.held_drained;
        if self.held_episode.is_some() {
            f.probably_program_check = flat_on_dylib
                .iter()
                .map(|(pid, name)| Named {
                    pid: *pid,
                    name: name.clone(),
                    tab: tab_of.get(pid).copied(),
                    for_ms: 0,
                })
                .collect();
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
            front_pgid: None,
            backgrounded: false,
            fg_ms: None,
            deferred: None,
            lent: None,
            stir: Stir::Unknown,
            wide: Stir::Still,
            relays: false,
            wake_ms: None,
            first_word: None,
            lines_seen: false,
            last_name: String::new(),
            member_name: String::new(),
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
            front_pgid: None,
            backgrounded: false,
            fg_ms: None,
            deferred: None,
            lent: None,
            stir: Stir::Unknown,
            wide: Stir::Still,
            relays: false,
            wake_ms: None,
            first_word: None,
            lines_seen: false,
            last_name: m.name.clone(),
            member_name: m.name.clone(),
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

/// The key a held time is kept by: the pid, the program it runs (an exec starts a new one) and
/// what keeps it.
const fn held_key(h: &HeldProc) -> (u32, u64, StartWait) {
    (h.pid, h.exec, h.wait)
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

/// A remembered member's ancestry in the whole sweep, independent of discovery's cap;
/// `None` when a missing parent or a cycle prevents a decision.
fn in_tree(pid: u32, root: u32, sweep: &HashMap<u32, &SweepProc>) -> Option<bool> {
    let mut seen = BTreeSet::new();
    let mut cur = pid;
    for _ in 0..sweep.len() {
        if !seen.insert(cur) {
            return None;
        }
        let p = sweep.get(&cur)?;
        if p.ppid == root {
            return Some(true);
        }
        if p.ppid <= 1 {
            return Some(false);
        }
        cur = p.ppid;
    }
    None
}

/// What owns a process below the tab's shell other than its group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// The nearest detached leader at or above it.
    Call(u32),
    /// The innermost leader of a terminal session nested in the tab's at or above it (a session
    /// leader with a terminal of its own: sudo's `use_pty` monitor, `script`'s shell, an agent's
    /// pty call), opened by a command.
    Nested(u32),
    /// A terminal session a shell opened for its own use (zsh's `zpty`, an async prompt's
    /// worker): no command's.
    Shell,
}

/// The owner of `pid` below `shell`, if a session of its own holds it. A session is a shell's
/// own when its leader is a fork that never ran a program of its own, keeping its parent's
/// program and user, the parent running the tab's root's program (zsh's `zpty`); one that ran a
/// program (`codex exec` under an agent root, `script`'s shell) is the opener's command.
fn owner(pid: u32, shell: u32, sweep: &HashMap<u32, &SweepProc>) -> Option<Owner> {
    let root = sweep.get(&shell).map(|s| s.name.as_str());
    let mut cur = pid;
    let mut nested = None;
    for _ in 0..TREE_CAP {
        let p = sweep.get(&cur)?;
        if cur != shell && is_detached(p) {
            return Some(Owner::Call(cur));
        }
        if nested.is_none()
            && cur != shell
            && p.session_leader == Some(true)
            && p.has_tty == Some(true)
        {
            let shells = p.exec == Some(false)
                && sweep.get(&p.ppid).is_some_and(|q| {
                    q.name == p.name && q.uid == p.uid && Some(q.name.as_str()) == root
                });
            nested = Some(if shells {
                Owner::Shell
            } else {
                Owner::Nested(cur)
            });
        }
        if p.ppid == shell || p.ppid == cur || p.ppid <= 1 {
            return nested;
        }
        cur = p.ppid;
    }
    None
}

/// `opener`'s group holds its terminal, and each terminal session nested in the tab's that it
/// sits in is in front of its own in turn, up to the tab's: what a session it opened writes
/// reaches the tab, and the person's keys reach it.
fn in_front(
    opener: u32,
    shell: u32,
    tab_tpgid: Option<u32>,
    sweep: &HashMap<u32, &SweepProc>,
    facts: &HashMap<u32, &JobProc>,
) -> bool {
    let pgid_of = |p: u32| {
        facts
            .get(&p)
            .map(|f| f.pgid)
            .or_else(|| sweep.get(&p).map(|s| s.pgid))
    };
    let mut o = opener;
    for _ in 0..TREE_CAP {
        let Some(g) = pgid_of(o) else {
            return false;
        };
        let outer = if o == shell {
            None
        } else {
            match owner(o, shell, sweep) {
                None => None,
                Some(Owner::Nested(l)) => Some(l),
                Some(Owner::Call(_) | Owner::Shell) => return false,
            }
        };
        let Some(l) = outer else {
            return tab_tpgid == Some(g);
        };
        if facts.get(&l).and_then(|f| f.tpgid) != Some(g) {
            return false;
        }
        let Some(up) = sweep.get(&l).map(|s| s.ppid) else {
            return false;
        };
        o = up;
    }
    false
}

/// `pid`'s parent is one of `shell`'s ancestors (launchd, init, a subreaper the tab runs under):
/// it took `pid` in when its parent exited. A parent elsewhere (a debugger that attached from
/// another tab) is no adoption.
fn adopted(pid: u32, shell: u32, sweep: &HashMap<u32, &SweepProc>) -> bool {
    let Some(ppid) = sweep.get(&pid).map(|p| p.ppid) else {
        return false;
    };
    let mut cur = shell;
    for _ in 0..sweep.len() {
        let Some(p) = sweep.get(&cur) else {
            return ppid <= 1;
        };
        if p.ppid == ppid || ppid <= 1 {
            return true;
        }
        if p.ppid <= 1 || p.ppid == cur {
            return false;
        }
        cur = p.ppid;
    }
    false
}

/// This tick's units of one tab, with the programs seen doing job control in it noted in
/// `job_control`.
fn discover(
    tab: &JobTab,
    sweep: &HashMap<u32, &SweepProc>,
    facts: &HashMap<u32, &JobProc>,
    children: &HashMap<u32, Vec<u32>>,
    job_control: &mut HashMap<u32, (Option<u64>, String)>,
    born: &mut HashMap<u32, MakerSeen>,
    root_shell: bool,
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
    let mut nested: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut groups: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for pid in &tree {
        match owner(*pid, shell, sweep) {
            Some(Owner::Call(d)) => detached.entry(d).or_default().push(*pid),
            Some(Owner::Nested(l)) => nested.entry(l).or_default().push(*pid),
            Some(Owner::Shell) => {}
            None => {
                if let Some(p) = sweep.get(pid) {
                    groups.entry(p.pgid).or_default().push(*pid);
                }
            }
        }
    }
    // A group a program made for its own children (ninja's steps, nextest's tests, watchexec's
    // command) is that program's job's: the program waits on it, and its work is the program's
    // progress. A program that put a group it made in front does job control (a shell nested in
    // the tab's, a debugger): each group it makes is a job of its own.
    let made_by = |g: u32, members: &[u32]| {
        members
            .iter()
            .filter_map(|p| sweep.get(p))
            .find(|p| sweep.get(&p.ppid).is_none_or(|q| q.pgid != g))
            .map(|p| p.ppid)
    };
    // The tab's root among them, and also at a pass that found it a shell (`Jobs::absorb`): a
    // root that is neither (`aterm -e ninja`, or the shell gave way to it: `exec ninja`) does no
    // job control, a stop in a group it made is held, and in an open block the groups it or
    // another program the block started in the shell's own group made are the block's (`exec
    // cmake --build`'s ninja, `$(ninja)`, a build in a pwsh tab, which keeps its children in its
    // own group).
    let front_maker = tf
        .tpgid
        .and_then(|t| groups.get(&t).map(|ms| (t, ms)))
        .and_then(|(t, ms)| made_by(t, ms));
    for m in front_maker.into_iter().chain(root_shell.then_some(shell)) {
        if let Some(q) = sweep.get(&m) {
            job_control.insert(m, (start_of(m), q.name.clone()));
        }
    }
    let open = tab.block.as_ref().filter(|b| b.open).map(|b| b.id);
    let own: &[u32] = groups.get(&tf.shell_pgid).map_or(&[], Vec::as_slice);
    born.retain(|p, _| own.contains(p));
    for p in own {
        let start = start_of(*p);
        let seen = born.entry(*p).or_insert(MakerSeen { start, block: open });
        match (seen.start, start) {
            (Some(before), Some(now)) if before != now => {
                *seen = MakerSeen { start, block: open };
            }
            (None, Some(_)) => seen.start = start,
            _ => {}
        }
    }
    let made: BTreeMap<u32, u32> = groups
        .iter()
        .filter(|(g, _)| **g != tf.shell_pgid)
        .filter_map(|(g, ms)| {
            let m = made_by(*g, ms).filter(|m| !job_control.contains_key(m))?;
            let h = sweep.get(&m).map(|q| q.pgid)?;
            if h == tf.shell_pgid {
                // Only a maker the open block started: a build a pwsh `Start-Job` left running
                // in pwsh's group is no later command's.
                let ours =
                    open.is_some() && (m == shell || born.get(&m).is_some_and(|b| b.block == open));
                return ours.then_some((*g, tf.shell_pgid));
            }
            (h != *g && groups.contains_key(&h)).then_some((*g, h))
        })
        .collect();
    for g in made.keys() {
        let mut to = *g;
        for _ in 0..made.len() {
            match made.get(&to) {
                Some(h) if h != g => to = *h,
                _ => break,
            }
        }
        if to != *g
            && let Some(ms) = groups.remove(g)
        {
            groups.entry(to).or_default().extend(ms);
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
            front: Vec::new(),
            consumer_pid: p.map(|p| p.ppid),
            // Only the join names a call's command: its leader is the agent's shell wrapper
            // (`zsh -c …` leads every Claude Code call), whose name would put every call in one
            // command class.
            command: hint.and_then(|h| h.command.clone()).unwrap_or_default(),
            block: None,
            shell: None,
        });
    }
    // A terminal session nested in the tab's is a call of the program that opened it, judged by
    // its own processes: the opener's own output (an agent's redraws) is none of its. A session
    // nested in another is a row of its own beside it (an agent's pty call in `script`).
    for (l, members) in nested {
        let p = sweep.get(&l);
        let opener = p.map(|p| p.ppid);
        let in_front = opener.is_some_and(|o| in_front(o, shell, tf.tpgid, sweep, facts));
        units.push(Unit {
            key: Key {
                tab: tab.key,
                leader: l,
                start: start_of(l),
                block: None,
            },
            kind: JobKind::Nested,
            consumer: if in_front {
                Consumer::PersonFg
            } else {
                Consumer::PersonBg
            },
            promoted_ms: None,
            task: None,
            id: format!("n{l}"),
            pgid: p.map_or(l, |p| p.pgid),
            members,
            front: Vec::new(),
            consumer_pid: opener,
            // As a detached call's: the leader is a relay or a shell (`sudo`, `zsh`), whose name
            // would put every session in one command class.
            command: String::new(),
            block: None,
            shell: None,
        });
    }
    let block = tab.block.as_ref().filter(|b| b.open);
    let front = tf.tpgid.filter(|g| *g != tf.shell_pgid && *g != 0);
    tf.front_live = front.and_then(|g| groups.get(&g)).is_some_and(|m| {
        m.iter()
            .any(|p| sweep.get(p).is_some_and(|s| s.state != ProcState::Zombie))
    });
    tf.front_gone = front.is_some() && !tf.front_live;
    tf.front_idle = tab.tty.is_some_and(|m| !m.canonical)
        && front.and_then(|g| groups.get(&g)).is_some_and(|ms| {
            let mut live = ms
                .iter()
                .filter(|p| sweep.get(p).is_some_and(|s| s.state != ProcState::Zombie));
            live.next().is_some_and(|p| job_control.contains_key(p)) && live.next().is_none()
        });
    if let Some(b) = block {
        let mut members = vec![shell];
        members.extend(groups.get(&tf.shell_pgid).into_iter().flatten().copied());
        let in_front: Vec<u32> = front
            .and_then(|g| groups.get(&g))
            .cloned()
            .unwrap_or_default();
        members.extend(&in_front);
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
            front: in_front,
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
        } else if members.iter().any(|p| {
            sweep
                .get(p)
                .is_some_and(|s| !matches!(s.state, ProcState::Stopped | ProcState::Zombie))
        }) {
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
            front: Vec::new(),
            consumer_pid: None,
            command: leader_name,
            block: None,
            shell: None,
        });
    }
    (units, tf)
}

/// The index of the live job a block's group in front already belongs to, when that block resumes
/// it: a background or parked job (`fg`), the group a tick saw in front before its block, or an
/// older block's job whose last group in front is in front again, where that block ended in a stop
/// (a stop and `fg` no tick saw) or the group is the one its key names (it took the block of a
/// command no pass saw, and the newer block is a `fg` typed ahead of that command's end). Only the
/// group in front counts: the shell's own group holds what no command owns (a process
/// substitution, a survivor an earlier command left).
fn resumed(u: &Unit, tab: &JobTab, claimed: &HashMap<u32, usize>, jobs: &[Job]) -> Option<usize> {
    u.front
        .iter()
        .find_map(|p| claimed.get(p).copied())
        .filter(|i| {
            let j = &jobs[*i];
            match j.block {
                None => matches!(j.kind, JobKind::ShellBg | JobKind::Shell),
                Some(b) => {
                    Some(b) != u.block
                        && j.kind == JobKind::Shell
                        && j.front_pgid == Some(u.pgid)
                        && (stopped_block(tab, b) || j.key.leader == u.pgid)
                }
            }
        })
}

/// Block `id` of `tab` ended in a job-control stop: its command lives on, parked.
fn stopped_block(tab: &JobTab, id: u64) -> bool {
    tab.block
        .iter()
        .chain(&tab.ended)
        .any(|b| b.id == id && !b.open && b.stopped)
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
/// legitimate waits that are not the person's, idle otherwise.
fn demand_of(rows: &[JobRow], at_person: &BTreeSet<(u64, &str)>) -> Demand {
    let mut d = Demand::Idle;
    for r in rows.iter().filter(|r| r.verdict != Verdict::Exited) {
        let reads_keys = at_person.contains(&(r.tab, r.id.as_str()));
        if (r.class == Class::Running && !reads_keys) || watched_fast(r) {
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

/// A live job the engine watches at its fast cadence whatever it waits on: stuck, or capable of
/// suspicion (an agent's call).
fn watched_fast(r: &JobRow) -> bool {
    r.verdict == Verdict::Stuck || matches!(r.standing, Some(Standing::Capable | Standing::Capped))
}

/// The tabs, ascending, whose every live job holds the foreground at the person's own terminal read
/// and is not [`watched_fast`] ([`JobsView::parked_tabs`]).
fn parked_tabs(rows: &[JobRow], at_person: &BTreeSet<(u64, &str)>) -> Vec<u64> {
    let mut parked = BTreeSet::new();
    let mut not = BTreeSet::new();
    for r in rows.iter().filter(|r| r.verdict != Verdict::Exited) {
        if at_person.contains(&(r.tab, r.id.as_str())) && !watched_fast(r) {
            parked.insert(r.tab);
        } else {
            not.insert(r.tab);
        }
    }
    parked.difference(&not).copied().collect()
}
