// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-thread D-state census (stats deck A4 §6.5, M-DSTATE in
//! `docs/measured/stats-deck-a4-n1-2026-11-06/procs.md`): which of this user's processes hold a
//! thread in an uninterruptible wait (Mach `TH_STATE_UNINTERRUPTIBLE`, a disk or a mount that
//! does not answer). macOS reports every live process's BSD status as running, so only its
//! threads say it, and it refuses other users' threads: the census reads this user's.
//!
//! A pass reads processes in pid order from where the last stopped, each process's threads as the
//! job probe reads them (at most [`crate::jobs::MAX_THREADS`]), until its thread time would pass
//! [`PASS_BUDGET_NS`]. It stops *before* a process whose read could cross the budget: the margin
//! is the costliest one-process read of the open cover and of the last one that ended, at least
//! [`MARGIN_FLOOR_NS`] and at most [`MARGIN_CAP_NS`], so one costly read holds it up for two
//! covers at most, and a pass still reads half its budget of its cover meanwhile. The list's own
//! time is spent but is no process's read, so it is not learned. Stopping once the budget is spent
//! overshot in 80 of 159 passes at 1,850 own processes (N1, max 5.32 ms); the margin held 244 of
//! 244 at or under 4.73 ms. A pass's cover reads at least one process, so a cover always moves.
//! Each pass starts by re-reading the processes the pass before marked, so a thread held
//! uninterruptibly for two passes running is found 30 s apart even when a cover takes two passes
//! ([`Marked::again`]).
//!
//! The census is generic over its [`Source`] so its budget and order are held by tests with no
//! kernel; [`Kernel`] is macOS's.

/// A pass's budget of the census worker's thread time: 5 ms (A4 §6.5, §13).
pub const PASS_BUDGET_NS: u64 = 5_000_000;

/// The least a pass holds back before a process: N1's costliest one-process read was 0.57 ms,
/// so a pass that has seen none yet stops 0.6 ms short.
pub const MARGIN_FLOOR_NS: u64 = 600_000;

/// The most a pass holds back before a process, however costly a read it has seen: half the
/// budget ([`Census::with_budget`]'s, [`PASS_BUDGET_NS`] here). A read past it may cross the
/// budget, as the pass's first read always may.
pub const MARGIN_CAP_NS: u64 = PASS_BUDGET_NS / 2;

/// One process as a pass read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// Its threads were read: how many, and how many wait uninterruptibly.
    Threads {
        /// Threads read.
        threads: u32,
        /// Of them, uninterruptible.
        uninterruptible: u32,
    },
    /// Its threads were not read: a zombie (which has none), or a process the kernel refuses.
    Refused,
    /// It is gone.
    Gone,
}

/// What the census reads, so its budget and order are testable without a kernel.
pub trait Source {
    /// This user's live pids, ascending, into `pids` (cleared first); `false` when the list is
    /// refused.
    fn list(&mut self, pids: &mut Vec<u32>) -> bool;
    /// One process's threads.
    fn read(&mut self, pid: u32) -> Read;
    /// The calling thread's CPU time, ns.
    fn thread_ns(&mut self) -> u64;
    /// A process's start (µs since the epoch) and name, read for a process a pass marks; `None`
    /// once it is gone.
    fn identity(&mut self, pid: u32) -> Option<(u64, String)>;
}

/// A process a pass found holding a thread uninterruptibly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marked {
    /// Its pid.
    pub pid: u32,
    /// Its start, µs since the epoch: with the pid, its identity across passes.
    pub start_us: u64,
    /// Its name.
    pub name: String,
    /// Its threads in an uninterruptible wait.
    pub uninterruptible: u32,
    /// The pass before marked it too (the same pid and start): held for two passes running.
    pub again: bool,
}

/// One pass's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    /// The processes it found holding a thread uninterruptibly, pid ascending.
    pub marked: Vec<Marked>,
    /// This user's processes listed; 0 where the list was refused.
    pub listed: u32,
    /// Processes it read (re-reads included).
    pub read: u32,
    /// Processes whose threads it could not read.
    pub refused: u32,
    /// Whether it read to the end of the list: a cover is done.
    pub cover_done: bool,
    /// Passes the cover it ended took, this one included, where it ended one.
    pub passes_in_cover: u32,
    /// Its thread time, ns.
    pub thread_ns: u64,
    /// The margin it held back, ns.
    pub margin_ns: u64,
    /// Its costliest one-process read, ns.
    pub costliest_ns: u64,
    /// Whether the list was refused (nothing read).
    pub list_refused: bool,
}

/// The census's state between passes.
#[derive(Debug)]
pub struct Census<S> {
    source: S,
    budget_ns: u64,
    /// The next pid a cover reads; 0 at a cover's start.
    cursor: u32,
    /// Passes the open cover has taken.
    passes_in_cover: u32,
    /// The costliest one-process read of the open cover so far, ns.
    open_costliest: u64,
    /// The costliest one-process read of the last cover that ended, ns.
    ended_costliest: u64,
    /// The last pass's marks, re-read first by the next.
    last: Vec<Marked>,
    pids: Vec<u32>,
}

impl<S: Source> Census<S> {
    /// A census over `source` with [`PASS_BUDGET_NS`].
    #[must_use]
    pub fn new(source: S) -> Self {
        Self::with_budget(source, PASS_BUDGET_NS)
    }

    /// A census over `source` with a budget of `budget_ns` a pass.
    #[must_use]
    pub fn with_budget(source: S, budget_ns: u64) -> Self {
        Self {
            source,
            budget_ns,
            cursor: 0,
            passes_in_cover: 0,
            open_costliest: 0,
            ended_costliest: 0,
            last: Vec::new(),
            pids: Vec::new(),
        }
    }

    /// The source, for a test's look at what it was asked.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// One pass: the last pass's marks first, then the cover from where it stopped, each process
    /// read only while the time spent and the margin stay within the budget (the first always).
    pub fn pass(&mut self) -> Pass {
        let t0 = self.source.thread_ns();
        let mut clock = Clock {
            t0,
            prev: t0,
            reads: 0,
            costliest: 0,
        };
        let mut out = Pass {
            margin_ns: self.margin_ns(),
            ..Pass::default()
        };
        let mut marked: Vec<Marked> = Vec::new();
        let mut pending: Vec<Marked> = Vec::new();
        let mut reread: Vec<u32> = Vec::new();

        for m in std::mem::take(&mut self.last) {
            if !self.may_read(&clock) {
                pending.push(m);
                continue;
            }
            reread.push(m.pid);
            let read = self.source.read(m.pid);
            Self::note(&mut clock, &mut out, read);
            if let Read::Threads {
                uninterruptible, ..
            } = read
                && uninterruptible > 0
                && let Some((start_us, name)) = self.source.identity(m.pid)
            {
                marked.push(Marked {
                    pid: m.pid,
                    again: start_us == m.start_us,
                    start_us,
                    name,
                    uninterruptible,
                });
            }
            self.tick(&mut clock);
        }

        let mut pids = std::mem::take(&mut self.pids);
        let listed = self.source.list(&mut pids);
        // The list's time is spent, but it is no process's read: neither charged nor learned.
        clock.prev = self.source.thread_ns();
        if listed {
            out.listed = u32::try_from(pids.len()).unwrap_or(u32::MAX);
            self.passes_in_cover = self.passes_in_cover.saturating_add(1);
            let mut i = pids.partition_point(|&p| p < self.cursor);
            let mut cover_reads = 0u32;
            loop {
                let Some(&pid) = pids.get(i) else {
                    out.cover_done = true;
                    out.passes_in_cover = self.passes_in_cover;
                    self.passes_in_cover = 0;
                    self.cursor = 0;
                    self.ended_costliest = std::mem::take(&mut self.open_costliest);
                    break;
                };
                if cover_reads > 0 && !self.may_read(&clock) {
                    self.cursor = pid;
                    break;
                }
                i += 1;
                if reread.contains(&pid) {
                    continue;
                }
                let read = self.source.read(pid);
                Self::note(&mut clock, &mut out, read);
                cover_reads += 1;
                if let Read::Threads {
                    uninterruptible, ..
                } = read
                    && uninterruptible > 0
                    && let Some((start_us, name)) = self.source.identity(pid)
                {
                    marked.push(Marked {
                        pid,
                        start_us,
                        name,
                        uninterruptible,
                        again: false,
                    });
                }
                self.tick(&mut clock);
            }
        } else {
            out.list_refused = true;
        }
        self.pids = pids;

        marked.sort_by_key(|m| m.pid);
        // A mark the budget left unread waits for the next pass, as the pass before marked it.
        self.last = marked.clone();
        for p in pending {
            if !self.last.iter().any(|m| m.pid == p.pid) {
                self.last.push(p);
            }
        }
        out.marked = marked;
        out.thread_ns = clock.prev.saturating_sub(clock.t0);
        out.costliest_ns = clock.costliest;
        out
    }

    /// The margin a pass holds back before a process: the costliest read of the open cover and
    /// the last that ended, at least [`MARGIN_FLOOR_NS`] and at most half the budget.
    fn margin_ns(&self) -> u64 {
        self.open_costliest
            .max(self.ended_costliest)
            .max(MARGIN_FLOOR_NS)
            .min(self.budget_ns / 2)
    }

    /// Whether the next process may be read: the first of a pass always (and the cover's first,
    /// whatever the re-reads spent, so a cover always moves); then only while the time spent and
    /// the margin stay within the budget.
    fn may_read(&self, clock: &Clock) -> bool {
        clock.reads == 0
            || clock
                .prev
                .saturating_sub(clock.t0)
                .saturating_add(self.margin_ns())
                <= self.budget_ns
    }

    fn note(clock: &mut Clock, out: &mut Pass, read: Read) {
        out.read += 1;
        clock.reads += 1;
        if !matches!(read, Read::Threads { .. }) {
            out.refused += 1;
        }
    }

    /// Takes the clock after a process (its read and any identity read), and learns its cost.
    fn tick(&mut self, clock: &mut Clock) {
        let now = self.source.thread_ns();
        let cost = now.saturating_sub(clock.prev);
        clock.prev = now;
        clock.costliest = clock.costliest.max(cost);
        self.open_costliest = self.open_costliest.max(cost);
    }
}

/// A pass's thread clock.
#[derive(Clone, Copy, Debug)]
struct Clock {
    t0: u64,
    prev: u64,
    reads: u32,
    costliest: u64,
}

/// macOS's source: this user's processes (`proc_listpids(PROC_UID_ONLY)`), each one's threads
/// by the job probe's reader, the worker's thread clock, and the process's `kinfo_proc` for its
/// start and name.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct Kernel {
    uid: u32,
}

#[cfg(target_os = "macos")]
impl Kernel {
    /// The source for this process's effective user.
    #[must_use]
    pub fn new() -> Self {
        // SAFETY: `geteuid` cannot fail.
        Self {
            uid: unsafe { libc::geteuid() },
        }
    }
}

#[cfg(target_os = "macos")]
impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
impl Source for Kernel {
    fn list(&mut self, pids: &mut Vec<u32>) -> bool {
        crate::macos::uid_pids(self.uid, pids)
    }

    fn read(&mut self, pid: u32) -> Read {
        crate::jobs::census_read(pid)
    }

    fn thread_ns(&mut self) -> u64 {
        thread_ns()
    }

    fn identity(&mut self, pid: u32) -> Option<(u64, String)> {
        let k = crate::jobs::macos_kinfo(std::ffi::c_int::try_from(pid).ok()?)?;
        Some((k.start_us, k.comm))
    }
}

/// The calling thread's CPU time, ns (`CLOCK_THREAD_CPUTIME_ID`); 0 where the clock refuses.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub fn thread_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a writable timespec; the clock id is a constant the OS defines.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
    if rc != 0 {
        return 0;
    }
    u64::try_from(ts.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(ts.tv_nsec).unwrap_or(0))
}

#[cfg(test)]
#[path = "dstate_tests.rs"]
mod tests;
