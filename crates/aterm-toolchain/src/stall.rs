// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A HOLDER THAT MAKES NO PROGRESS (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT,
//! CW-WAIT-UNBOUNDED).
//!
//! A wait for a named lock ([`crate::named_lock`]) or for room in the capacity ledger is
//! admission behind a live holder, bounded by nothing but that holder: one that lives on and
//! never lets go (stopped, or blocked on something that never comes) keeps every waiter waiting
//! for ever. So the waiter reads the holder's process tree as it names it ([`Work`], the CPU time
//! its processes used and the bytes they wrote) and keeps how long the tree has done neither
//! ([`Stall`]). Any CPU time is progress: a holder that is itself waiting in a chain wakes to look
//! again (a lock's waiter every tenth of a second, the ledger's every few seconds), so it reads
//! as working. Past [`STALLED_AFTER`] the waiter's line says the holder made no progress and for
//! how long, and its mark says so to a ceiling over it ([`crate::waits::Mark::say_stuck`]),
//! which may end the wait (verify ends that check as NOT RUN); a waiter may end its own wait on
//! it too (a capacity claim that asks to, `aterm_capacity::Need::ends_when_stuck`). Nobody
//! signals the holder.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How long a holder's process tree may use no CPU time and write nothing before its waiter
/// names it stuck: past a quiet step's silence (a guest build with no output under a lane holder
/// reads as none), well short of verify's 3 h ceiling on a check's work.
pub const STALLED_AFTER: Duration = Duration::from_secs(15 * 60);

/// What a process tree has done, as one sweep reads it: the CPU time its processes used (each
/// one's own with what its reaped children used), the bytes they wrote, and how many they are.
/// Two reads that differ in any of these are progress between them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// CPU time, nanoseconds.
    pub cpu_ns: u64,
    /// Bytes written to disk.
    pub written: u64,
    /// How many processes the tree holds.
    pub members: usize,
}

/// One process as a sweep reads it, for [`work_of`]: its pid, its parent's, its CPU time (its
/// own with its reaped children's) and the bytes it wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// CPU time, nanoseconds: 0 where unread.
    pub cpu_ns: u64,
    /// Bytes written: 0 where unread.
    pub written: u64,
}

/// The work of `root` and every process descended from it among `procs`; `None` where `root`
/// is not among them (it has exited).
#[must_use]
pub fn work_of(root: u32, procs: &[Proc]) -> Option<Work> {
    work_with(root, &[], procs)
}

/// [`work_of`] `root`, with the trees of `credited` too: processes outside its tree whose work
/// is its own (a VM's, which launchd parents, credited to the capacity claim that boots it), each
/// process counted once however many of them it descends from. `None` where `root` is not among
/// `procs`; a credited process that is not adds nothing.
#[must_use]
pub fn work_with(root: u32, credited: &[u32], procs: &[Proc]) -> Option<Work> {
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
        work.written = work.written.saturating_add(p.written);
        work.members += 1;
        if let Some(kids) = children.get(&p.pid) {
            stack.extend(kids.iter().copied());
        }
    }
    Some(work)
}

/// The work of `pid`'s process tree on this machine now ([`work_of`] over one sweep,
/// `aterm_sysprobe::processes`); `None` where no sweep reads it or `pid` has exited.
#[must_use]
pub fn tree_work(pid: u32) -> Option<Work> {
    let sweep = aterm_sysprobe::processes()?;
    let procs: Vec<Proc> = sweep
        .procs
        .iter()
        .map(|p| Proc {
            pid: p.pid,
            ppid: p.ppid,
            cpu_ns: p
                .cpu_ns
                .unwrap_or(0)
                .saturating_add(p.child_cpu_ns.unwrap_or(0)),
            written: p.disk_written.unwrap_or(0),
        })
        .collect();
    work_of(pid, &procs)
}

/// How long a holder's tree has made no progress, as its waiter's reads find it: from the first
/// read that found its work as it is now. Another holder, or a read that finds no tree, starts
/// it again.
#[derive(Clone, Debug, Default)]
pub struct Stall {
    last: Option<(u32, Work, Instant)>,
}

impl Stall {
    /// Book a read, at `now`, of holder `pid`'s tree as `work`: how long the tree has done no
    /// work, zero where it did since the last read; `None` where it was not read.
    pub fn read(&mut self, pid: u32, work: Option<Work>, now: Instant) -> Option<Duration> {
        let Some(work) = work else {
            self.last = None;
            return None;
        };
        match &mut self.last {
            Some((p, w, since)) if *p == pid => {
                if *w != work {
                    *w = work;
                    *since = now;
                }
                Some(now.saturating_duration_since(*since))
            }
            _ => {
                self.last = Some((pid, work, now));
                Some(Duration::ZERO)
            }
        }
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
/// `span`: `which has made no progress for 16 min (no CPU time, nothing written)`.
#[must_use]
pub fn no_progress(span: Duration) -> String {
    format!(
        "which has made no progress for {} (no CPU time, nothing written)",
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
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, cpu_ns: u64, written: u64) -> Proc {
        Proc {
            pid,
            ppid,
            cpu_ns,
            written,
        }
    }

    /// A tree's work is its root's and every descendant's, and nothing of a process outside it;
    /// a root no sweep holds has none.
    #[test]
    fn a_trees_work_is_its_own_processes() {
        let procs = [
            p(1, 0, 1_000, 0),
            p(10, 1, 5, 7),
            p(11, 10, 3, 2),
            p(12, 10, 0, 1),
            p(20, 1, 100, 100),
        ];
        assert_eq!(
            work_of(10, &procs),
            Some(Work {
                cpu_ns: 8,
                written: 10,
                members: 3
            })
        );
        assert_eq!(work_of(99, &procs), None);
        assert_eq!(work_of(1, &procs).map(|w| w.members), Some(5));
    }

    /// A tree's work with what it is credited is the credited processes' trees too, each process
    /// once: a credited process inside the tree, or credited twice, adds nothing more, and one
    /// that is gone adds nothing; a root that is gone has none, whatever it is credited.
    #[test]
    fn a_credited_process_works_for_its_tree() {
        let procs = [
            p(1, 0, 1_000, 0),
            p(10, 1, 5, 7),
            p(11, 10, 3, 2),
            p(30, 1, 40, 9),
            p(31, 30, 2, 1),
        ];
        let vm = Some(Work {
            cpu_ns: 50,
            written: 19,
            members: 4,
        });
        assert_eq!(work_with(10, &[30], &procs), vm);
        assert_eq!(work_with(10, &[30, 31, 11, 30], &procs), vm);
        assert_eq!(work_with(10, &[99], &procs), work_of(10, &procs));
        assert_eq!(work_with(99, &[30], &procs), None);
    }

    /// The stall counts from the first read that found the tree's work as it is: work since the
    /// last read sets it to zero, and a new holder or an unread tree starts it again; `stuck`
    /// keeps it once it reaches its bound.
    #[test]
    fn a_stall_counts_from_the_last_work_seen() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let w = |cpu: u64| {
            Some(Work {
                cpu_ns: cpu,
                written: 0,
                members: 1,
            })
        };
        let mut s = Stall::default();
        assert_eq!(s.read(7, w(1), at(0)), Some(Duration::ZERO));
        assert_eq!(s.read(7, w(1), at(60)), Some(Duration::from_secs(60)));
        assert_eq!(s.read(7, w(2), at(120)), Some(Duration::ZERO), "it worked");
        assert_eq!(s.read(7, w(2), at(300)), Some(Duration::from_secs(180)));
        assert_eq!(
            s.read(8, w(2), at(360)),
            Some(Duration::ZERO),
            "another holder"
        );
        assert_eq!(s.read(8, None, at(420)), None);
        assert_eq!(s.read(8, w(2), at(480)), Some(Duration::ZERO), "read again");
        let after = Duration::from_secs(600);
        assert_eq!(s.stuck(8, w(2), at(1000), after), None);
        assert_eq!(
            s.stuck(8, w(2), at(1080), after),
            Some(Duration::from_secs(600))
        );
        assert_eq!(words(Duration::from_secs(16 * 60 + 59)), "16 min");
        assert_eq!(words(Duration::from_secs(59)), "59 s");
    }

    /// This process's tree reads on a machine with a sweep, holding at least this process; a pid
    /// no process has reads as none.
    #[test]
    fn this_processs_tree_reads() {
        if aterm_sysprobe::processes().is_none() {
            return;
        }
        let me = tree_work(std::process::id()).expect("this process is swept");
        assert!(me.members >= 1, "{me:?}");
        assert_eq!(tree_work(u32::MAX), None);
    }
}
