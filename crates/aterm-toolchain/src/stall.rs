// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A HOLDER THAT MAKES NO PROGRESS (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT,
//! CW-WAIT-UNBOUNDED).
//!
//! A wait for a named lock ([`crate::named_lock`]) or for room in the capacity ledger is
//! admission behind a live holder, bounded by nothing but that holder: one that lives on and
//! never lets go (stopped, or blocked on something that never comes) keeps every waiter waiting
//! for ever. So the waiter reads the holder's process tree as it names it ([`Work`]) and keeps
//! how long the tree has made no progress ([`Stall`]). Past [`STALLED_AFTER`] the waiter's line
//! says the holder made no progress and for how long, and its mark says so to a ceiling over it
//! ([`crate::waits::Mark::say_stuck`]), which may end the wait (verify ends that check as NOT
//! RUN); a waiter may end its own wait on it too (a capacity claim that asks to,
//! `aterm_capacity::Need::ends_when_stuck`). Nobody signals the holder.
//!
//! WHAT IS PROGRESS. A holder's work runs in its own process, in the processes it starts and in
//! the ones credited to it (a VM's, outside its tree): a build's compilers, a check's test
//! binary, a bench's measuring in its own process, a lane's guest in its VM's vCPU threads, a
//! browser lane's browser. Every one of those processes also keeps its books, and those wakes
//! use CPU time and write whether or not anything works: the holder's disk-claim meter walks its
//! directories and rewrites its claim every minute (`aterm_capacity::Meter`), it sweeps the
//! processes, says its lines and, waiting in its turn, looks again every tenth of a second; a Go
//! program it runs (lima's `limactl shell` between a lane and its ssh client) wakes its runtime
//! every minute and collects its garbage every two; an ssh client sends its keepalives; an idle
//! guest's vCPUs wake for its timers and its host agent logs a time sync. So no change counts by
//! itself. The processes of a holder's work make progress, together, only at a rate no upkeep
//! reaches: runnable a quarter of the time ([`BUSY`]: on a core or waiting for one, so a busy
//! Mac that gives work less than a core does not read as its idling) or a MiB written
//! ([`WRITES`]) over the last minute ([`BUSY_OVER`]), or a process started or ended among them.
//! A process of the tree
//! that waits in its turn for a lock or for room, marked in this user's waits
//! ([`crate::waits::user_dir`]), does nothing of its own while it waits: the tree is then as stuck
//! as what that waits behind ([`Waits`]), working while that works, and stuck for as long as that
//! once its own waiter calls it stuck.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::waits::Waiting;

/// How long a holder's process tree may make no progress before its waiter names it stuck: past
/// a quiet step's silence, well short of verify's 3 h ceiling on a check's work.
pub const STALLED_AFTER: Duration = Duration::from_secs(15 * 60);

/// The share of the time the processes of a holder's work must be runnable, together, for it to
/// be progress: one part in `BUSY`, a quarter of a core, over [`BUSY_OVER`]. Upkeep takes less
/// (on this Mac, 2026-11-06: a meter's walk of a 15,500-entry build directory 0.07-0.22 s of CPU
/// time a minute, and of the whole shared build cache, 583,000 entries, 5.8-6.5 s, a tenth of a
/// core; a lock's wait 18 µs every tenth of a second; an idle `limactl` 55 µs of CPU time and
/// 64 µs runnable in a minute, its ssh client none), and work a core: a process that spins was
/// runnable all of a minute at a load of 166 on 14 cores, which gave it a third of a core.
pub const BUSY: u32 = 4;

/// The bytes the processes of a holder's work must write, together, over [`BUSY_OVER`] for their
/// writes to be progress: a MiB. Upkeep writes less (a meter's claim, a host agent's log line, an
/// idle guest's journal), and work more (a build's objects, a guest's downloads and builds,
/// which its VM writes to its disk).
pub const WRITES: u64 = 1 << 20;

/// How far back the processes of a holder's work are judged busy ([`BUSY`], [`WRITES`]): a
/// minute, a meter's pace, so one walk is spread over the minute it measures.
pub const BUSY_OVER: Duration = Duration::from_secs(60);

/// What the processes of a holder's work have done, as one sweep reads them: the holder's own,
/// every one descended from it and those credited to it. They make progress by being runnable
/// busy or writing much ([`busy`], [`writing`]) or by how many they are changing; a tree that
/// waits in its turn as what it waits behind does ([`Waits`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// Their CPU time, each with what its reaped children used, nanoseconds.
    pub cpu_ns: u64,
    /// The time they were runnable, on a core or waiting for one, each the more of that and its
    /// CPU time (its CPU time where a sweep reads no runnable time, off macOS), with what its
    /// reaped children used, nanoseconds: what [`busy`] judges.
    pub runnable_ns: u64,
    /// The bytes they wrote to disk.
    pub written: u64,
    /// How many they are.
    pub members: usize,
    /// Whether one of them waits in its turn, and behind what.
    pub waits: Waits,
    /// When the sweep read them, which their rates are judged over ([`Stall::read`]); `None`
    /// judges them over the reads' own clock.
    pub at: Option<Instant>,
}

/// Whether a process of a holder's tree waits in its turn for a lock or for room, as its mark in
/// this user's waits says ([`crate::waits::user_waiting`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Waits {
    /// None does.
    #[default]
    No,
    /// One waits behind holders that are not all stuck: what it waits behind works, so the
    /// tree does.
    Behind,
    /// Every one that waits is stuck behind holders that have made no progress, the one that
    /// made progress last for this long ([`crate::waits::Stuck::span`]).
    Stuck(Duration),
}

/// One process as a sweep reads it, for [`work_of`]: its pid, its parent's, its CPU time and the
/// time it was runnable (each its own with its reaped children's CPU time) and the bytes it
/// wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// CPU time, nanoseconds: 0 where unread.
    pub cpu_ns: u64,
    /// The time its threads were runnable, on a core or waiting for one (macOS
    /// `ri_runnable_time`), nanoseconds: 0 where unread, its CPU time then standing for it.
    pub runnable_ns: u64,
    /// Bytes written: 0 where unread.
    pub written: u64,
}

/// The work of `root` and every process descended from it among `procs`, none of it waiting;
/// `None` where `root` is not among them (it has exited).
#[must_use]
pub fn work_of(root: u32, procs: &[Proc]) -> Option<Work> {
    work_with(root, &[], procs, &[])
}

/// [`work_of`] `root`, with the trees of `credited` too: processes outside its tree whose work
/// is its own (a VM's, which launchd parents, credited to the capacity claim that boots it or
/// named in the record of the lane lock its run holds), each process counted once however many
/// of them it descends from; and whether any of those processes waits, by its mark among
/// `waits`. `None` where `root` is not among `procs`; a credited process that is not adds
/// nothing.
#[must_use]
pub fn work_with(root: u32, credited: &[u32], procs: &[Proc], waits: &[Waiting]) -> Option<Work> {
    let mut by_pid: HashMap<u32, &Proc> = HashMap::new();
    let mut children: HashMap<u32, Vec<&Proc>> = HashMap::new();
    for p in procs {
        by_pid.insert(p.pid, p);
        if p.ppid != p.pid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    let mut stack = vec![*by_pid.get(&root)?];
    stack.extend(credited.iter().filter_map(|c| by_pid.get(c).copied()));
    let mut seen = HashSet::new();
    let mut work = Work::default();
    while let Some(p) = stack.pop() {
        if !seen.insert(p.pid) {
            continue;
        }
        work.cpu_ns = work.cpu_ns.saturating_add(p.cpu_ns);
        work.runnable_ns = work.runnable_ns.saturating_add(p.runnable_ns.max(p.cpu_ns));
        work.written = work.written.saturating_add(p.written);
        work.members += 1;
        if let Some(kids) = children.get(&p.pid) {
            stack.extend(kids.iter().copied());
        }
    }
    let mut marked = waits.iter().filter(|w| seen.contains(&w.pid)).peekable();
    if marked.peek().is_some() {
        let spans: Option<Vec<Duration>> =
            marked.map(|w| w.stuck.as_ref().map(|s| s.span)).collect();
        work.waits = spans
            .and_then(|s| s.into_iter().min())
            .map_or(Waits::Behind, Waits::Stuck);
    }
    Some(work)
}

/// This machine's processes now, as a stall reads them, and when the sweep read them
/// (`aterm_sysprobe::processes`); `None` where no sweep reads them.
#[must_use]
pub fn sweep() -> Option<(Vec<Proc>, Instant)> {
    let sweep = aterm_sysprobe::processes()?;
    let at = Instant::now();
    let procs = sweep
        .procs
        .iter()
        .map(|p| {
            let reaped = p.child_cpu_ns.unwrap_or(0);
            Proc {
                pid: p.pid,
                ppid: p.ppid,
                cpu_ns: p.cpu_ns.unwrap_or(0).saturating_add(reaped),
                runnable_ns: p.runnable_ns.map_or(0, |r| r.saturating_add(reaped)),
                written: p.disk_written.unwrap_or(0),
            }
        })
        .collect();
    Some((procs, at))
}

/// The work of `pid`'s process tree on this machine now, with the trees of `credited`
/// ([`work_with`] over one [`sweep`] and the waits marked in this user's directory,
/// [`crate::waits::user_waiting`]), stamped with when the sweep read it; `None` where no sweep
/// reads it or `pid` has exited.
#[must_use]
pub fn tree_work(pid: u32, credited: &[u32]) -> Option<Work> {
    let (procs, at) = sweep()?;
    work_with(pid, credited, &procs, &crate::waits::user_waiting())
        .map(|w| Work { at: Some(at), ..w })
}

/// Whether `ran` nanoseconds runnable over `over` is busy: at least one part in [`BUSY`] of it.
#[must_use]
pub fn busy(ran: u64, over: Duration) -> bool {
    ran > 0 && u128::from(ran) * u128::from(BUSY) >= over.as_nanos()
}

/// Whether `wrote` bytes over `over` is writing much: at least [`WRITES`] a [`BUSY_OVER`].
#[must_use]
pub fn writing(wrote: u64, over: Duration) -> bool {
    wrote > 0 && u128::from(wrote) * BUSY_OVER.as_nanos() >= u128::from(WRITES) * over.as_nanos()
}

/// How long a holder's tree has made no progress, as its waiter's reads find it: from the first
/// read that found its processes as many as they are now and since which they were runnable
/// busy and wrote much nowhere, each judged over [`BUSY_OVER`], or for as long as what it waits
/// behind has made none, whichever is longer. Another holder, or a read that finds no tree,
/// starts it again.
#[derive(Clone, Debug)]
pub struct Stall {
    last: Option<Last>,
    /// How far back its reads judge them busy: [`BUSY_OVER`], or a test's compressed pace's.
    over: Duration,
}

impl Default for Stall {
    fn default() -> Self {
        Stall::over(BUSY_OVER)
    }
}

/// The holder a [`Stall`] last read.
#[derive(Clone, Debug)]
struct Last {
    pid: u32,
    /// How many processes the read that last found progress found.
    members: usize,
    /// When that read was.
    since: Instant,
    /// Their runnable time and writes at each read of the last [`BUSY_OVER`], with the last read
    /// before them, oldest first, each at the moment its sweep read it ([`Work::at`]).
    seen: VecDeque<(Instant, u64, u64)>,
}

impl Last {
    /// Book `work`, read at `at`: whether its processes were runnable busy or wrote much since
    /// the last read at least `over` before (the oldest read, where none is that old).
    fn worked(&mut self, work: &Work, at: Instant, over: Duration) -> bool {
        self.seen.push_back((at, work.runnable_ns, work.written));
        while self
            .seen
            .get(1)
            .is_some_and(|(then, ..)| at.saturating_duration_since(*then) >= over)
        {
            self.seen.pop_front();
        }
        self.seen.front().is_some_and(|(then, ran, wrote)| {
            let over = at.saturating_duration_since(*then);
            busy(work.runnable_ns.saturating_sub(*ran), over)
                || writing(work.written.saturating_sub(*wrote), over)
        })
    }
}

impl Stall {
    /// A stall whose reads judge the holder's processes busy over `over` in place of
    /// [`BUSY_OVER`]: a test's pace, compressed in time with its reads and its bound.
    #[must_use]
    pub fn over(over: Duration) -> Stall {
        Stall { last: None, over }
    }

    /// Book a read, at `now`, of holder `pid`'s tree as `work`: how long the tree has made no
    /// progress, zero where it did since the last read; `None` where it was not read.
    pub fn read(&mut self, pid: u32, work: Option<Work>, now: Instant) -> Option<Duration> {
        let Some(work) = work else {
            self.last = None;
            return None;
        };
        let at = work.at.unwrap_or(now);
        let behind = match work.waits {
            Waits::Stuck(span) => span,
            Waits::No | Waits::Behind => Duration::ZERO,
        };
        let last = match &mut self.last {
            Some(last) if last.pid == pid => last,
            _ => {
                self.last = Some(Last {
                    pid,
                    members: work.members,
                    since: now,
                    seen: VecDeque::from([(at, work.runnable_ns, work.written)]),
                });
                return Some(behind);
            }
        };
        if last.worked(&work, at, self.over)
            || work.waits == Waits::Behind
            || work.members != last.members
        {
            last.members = work.members;
            last.since = now;
            return Some(Duration::ZERO);
        }
        Some(now.saturating_duration_since(last.since).max(behind))
    }

    /// [`Self::read`], kept only once it reaches `after`: how long the holder has been stuck.
    pub fn stuck(
        &mut self,
        pid: u32,
        work: Option<Work>,
        now: Instant,
        after: Duration,
    ) -> Option<Duration> {
        self.read(pid, work, now).filter(|d| *d >= after)
    }
}

/// What a line says after the holder it names, once that holder has made no progress for
/// `span`: `which has made no progress for 16 min (it and what it runs only kept their books:
/// runnable under a quarter of a core, under 1 MiB written a minute)`.
#[must_use]
pub fn no_progress(span: Duration) -> String {
    format!(
        "which has made no progress for {} (it and what it runs only kept their books: runnable \
         under a quarter of a core, under 1 MiB written a minute)",
        words(span)
    )
}

/// `span` as a line says it: whole minutes (`16 min`), or seconds under one (`40 s`).
#[must_use]
pub fn words(span: Duration) -> String {
    match span.as_secs() {
        s @ 0..60 => format!("{s} s"),
        s => format!("{} min", s / 60),
    }
}

#[cfg(test)]
#[path = "stall_tests.rs"]
mod tests;
