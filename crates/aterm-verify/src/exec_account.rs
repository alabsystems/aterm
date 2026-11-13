// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release children use the verifier's accounting without changing their process groups,
//! output transport, signal forwarding or wait ownership.

use super::{Clock, Host, LiveHost, Own, Reading, TreeUsage};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Sample a child's descendants from spawn until its owner finishes waiting. CPU is a lower
/// bound: short-lived processes between reads and descendants reparented before a read are
/// absent. Unknown telemetry stays unknown: a read that missed (a process table the kernel held
/// past its bound) leaves sampling on, and a reading whose last read missed, with none working
/// after it before the child ended, is no reading. No process is signalled or reaped here.
pub struct UsageMeter {
    stop: Option<mpsc::Sender<()>>,
    reader: Option<JoinHandle<Option<TreeUsage>>>,
}

impl UsageMeter {
    /// Start observing `root`, including descendants in other process groups. Existing group
    /// membership and cancellation custody are unchanged.
    #[must_use]
    pub fn start(root: u32) -> Self {
        let host = RootHost::new(root, Kernel);
        let own = Own::default();
        if let Reading::Read(members) = host.read() {
            own.book(root, &members);
        }
        let (stop, receive) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("release-account".into())
            .spawn(move || {
                let began = Instant::now();
                let mut clock = Clock::new(None, &host, root, None).owning(&own);
                clock.tick();
                // Its owner stops it after its wait reaped the root (or drops it unfinished,
                // reading nothing): a read after the stop finds no tree, so the loop's last tick
                // is its last read.
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    receive.recv_timeout(Duration::from_millis(20))
                {
                    clock.tick();
                }
                own.usage(&clock, began.elapsed())
            })
            .ok();
        Self {
            stop: Some(stop),
            reader,
        }
    }

    /// Stop and join the observer. A failed observer supplies no reading.
    #[must_use]
    pub fn finish(mut self) -> Option<TreeUsage> {
        self.join()
    }

    fn join(&mut self) -> Option<TreeUsage> {
        self.stop.take();
        self.reader.take()?.join().ok().flatten()
    }
}

impl Drop for UsageMeter {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

/// Where a [`RootHost`] reads the machine: the kernel's process table, a process group's members
/// and one process's record. [`Kernel`] reads them live; a test scripts them.
trait Source: Sync + std::fmt::Debug {
    /// Every process's record ([`aterm_sysprobe::kproc::records`]).
    fn records(&self) -> Option<Vec<aterm_sysprobe::kproc::Record>>;
    /// What each member of `pgid` has used ([`aterm_sysprobe::group_usage`]).
    fn group(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>>;
    /// `pid`'s record, a definite absence apart from a failed read
    /// ([`aterm_sysprobe::kproc::record_read`]).
    fn record(&self, pid: u32) -> std::io::Result<Option<aterm_sysprobe::kproc::Record>>;
}

/// The live machine.
#[derive(Debug)]
struct Kernel;

impl Source for Kernel {
    fn records(&self) -> Option<Vec<aterm_sysprobe::kproc::Record>> {
        aterm_sysprobe::kproc::records()
    }
    fn group(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        aterm_sysprobe::group_usage(pgid)
    }
    fn record(&self, pid: u32) -> std::io::Result<Option<aterm_sysprobe::kproc::Record>> {
        aterm_sysprobe::kproc::record_read(pid)
    }
}

/// An accounting host rooted in a process rather than a process group. Each read selects
/// descendants from one kernel parent table, reads their groups once each, and filters out
/// unrelated members (including concurrent slices in the same group). The root's birth and
/// each selected member's parent, group and birth bracket those non-atomic reads. A read the
/// machine did not answer (the table or a group unread, a table that does not add up, a tree
/// past [`super::TEST_TREE_CAP`]) is [`Reading::Missed`]; the root gone or replaced, or a birth
/// never read, is [`Reading::Over`]. The birth is read once, at [`RootHost::new`], even when that
/// read fails: a birth first read later could be a process that reused the root's pid after its
/// owner reaped it.
#[derive(Debug)]
struct RootHost<S: Source> {
    root: u32,
    birth: Option<(u64, u32)>,
    source: S,
}

impl<S: Source> RootHost<S> {
    fn new(root: u32, source: S) -> Self {
        Self {
            root,
            birth: source
                .record(root)
                .ok()
                .flatten()
                .map(|r| (r.start_s, r.start_us)),
            source,
        }
    }

    fn read(&self) -> Reading {
        if self.birth.is_none() {
            return Reading::Over;
        }
        let Some(records) = self.source.records() else {
            return Reading::Missed;
        };
        self.read_with(
            &records,
            |pgid| self.source.group(pgid),
            |pid| self.source.record(pid),
        )
    }

    fn read_with(
        &self,
        records: &[aterm_sysprobe::kproc::Record],
        mut read_group: impl FnMut(u32) -> Option<Vec<aterm_sysprobe::MemberUsage>>,
        mut record: impl FnMut(u32) -> std::io::Result<Option<aterm_sysprobe::kproc::Record>>,
    ) -> Reading {
        let Some(birth) = self.birth else {
            return Reading::Over;
        };
        let born = |r: &aterm_sysprobe::kproc::Record| (r.start_s, r.start_us) == birth;
        let mut by_pid = BTreeMap::new();
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for r in records {
            if by_pid.insert(r.pid, r).is_some() {
                return Reading::Missed;
            }
            children.entry(r.ppid).or_default().push(r.pid);
        }
        match by_pid.get(&self.root) {
            Some(root) if born(root) => {}
            _ => return Reading::Over,
        }
        let mut selected = BTreeMap::new();
        let mut groups = BTreeSet::new();
        let mut open = VecDeque::from([self.root]);
        while let Some(pid) = open.pop_front() {
            if selected.len() >= super::TEST_TREE_CAP || selected.contains_key(&pid) {
                return Reading::Missed;
            }
            let Some(r) = by_pid.get(&pid).copied() else {
                return Reading::Missed;
            };
            let Ok(group) = u32::try_from(r.pgid) else {
                return Reading::Missed;
            };
            groups.insert(group);
            selected.insert(pid, r);
            if let Some(next) = children.get(&pid) {
                open.extend(next);
            }
        }
        let mut members = Vec::new();
        for group in groups {
            let Some(read) = read_group(group) else {
                return Reading::Missed;
            };
            for m in read {
                let Some(before) = selected.get(&m.pid) else {
                    continue;
                };
                let Ok(Some(after)) = record(m.pid) else {
                    continue;
                };
                if (before.start_s, before.start_us, before.ppid, before.pgid)
                    == (after.start_s, after.start_us, after.ppid, after.pgid)
                {
                    members.push(m);
                }
            }
        }
        match record(self.root) {
            Ok(Some(after)) if born(&after) => Reading::Read(members),
            Ok(_) => Reading::Over,
            Err(_) => Reading::Missed,
        }
    }
}

impl<S: Source> Host for RootHost<S> {
    fn group_usage(&self, _: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        match self.read() {
            Reading::Read(members) => Some(members),
            Reading::Missed | Reading::Over => None,
        }
    }
    fn group_reading(&self, _: u32) -> Reading {
        self.read()
    }
    fn idle(&self) -> Option<Duration> {
        None
    }
    fn cores(&self) -> u32 {
        LiveHost.cores()
    }
}

#[cfg(test)]
#[path = "exec_account_tests.rs"]
mod tests;
