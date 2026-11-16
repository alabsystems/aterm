// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What bounds a repository transfer ([`super::System::run_repository`]: a clone, a fetch, a
//! merge that may fetch what a partial clone lacks): a window without progress, never a wall
//! clock. A transfer of a large history over a slow link may take hours and finishes; one whose
//! link stopped carrying it ends one window after its last progress, as
//! [`super::RunFailed::Stalled`].
//!
//! Progress is any byte the transfer writes (Git's progress meter, which moves while objects
//! arrive, deltas resolve or files are written, and its remote's `remote:` lines) and, where Git
//! writes nothing for a while, work its process tree does: CPU time at a share of a core
//! ([`WORK_SHARE`]), or disk read or written, the way an index-pack, a connectivity check or a
//! checkout does. A transfer that waits on its link spends next to nothing: Git's progress meter
//! wakes it once a second for microseconds, which stays far below that share.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How long a repository transfer may go without progress before it is ended.
pub const TRANSFER_STALL: Duration = Duration::from_secs(5 * 60);

/// The least share of one core, as its denominator, that a transfer's processes spend between
/// two readings for their CPU time to count as work: a hundredth. An index-pack resolving deltas
/// or a checkout writing files runs near a whole core, and a loaded Mac still gives it far more
/// than a hundredth; a transfer that waits on its link spends a few microseconds a second.
const WORK_SHARE: u128 = 100;

/// How often a transfer's process tree is read while it writes nothing: every five seconds, or
/// four times a window shorter than twenty seconds. While it writes, nothing is read.
const LOOK: Duration = Duration::from_secs(5);

/// What a transfer's process tree has done so far, summed over its root and every process below
/// it ([`tree_work`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Work {
    /// User plus system CPU time, each member's own and its reaped children's, nanoseconds.
    pub(super) cpu_ns: u64,
    /// Bytes read from and written to the disk; `None` where any member's count was refused.
    pub(super) io_bytes: Option<u64>,
}

/// Whether a transfer still makes progress, judged on each wake of the transfer's waiter.
#[derive(Debug)]
pub(super) struct Window {
    /// How long it may go without progress.
    stall: Duration,
    /// When it last made progress.
    last: Instant,
    /// The bytes it had written by then.
    written: u64,
    /// When its tree was last read, whether or not the reading could be made; `None` before the
    /// first since its last write.
    looked: Option<Instant>,
    /// Its tree's last reading that could be made, and when that was taken; `None` before the
    /// first since its last write.
    seen: Option<(Instant, Work)>,
}

impl Window {
    /// A window of `stall` over a transfer started at `now`.
    pub(super) const fn new(stall: Duration, now: Instant) -> Self {
        Self {
            stall,
            last: now,
            written: 0,
            looked: None,
            seen: None,
        }
    }

    /// How often the tree is read while the transfer writes nothing ([`LOOK`]).
    fn look(&self) -> Duration {
        LOOK.min(self.stall / 4).max(Duration::from_millis(1))
    }

    /// Whether the transfer has stalled at `now`, having written `written` bytes in all: it has
    /// written no byte for the whole window, and no reading of its tree in that time showed work
    /// since the reading before it ([`worked`]). The tree is read through `read` once the
    /// transfer has been quiet for a look, then once a look. A tree that cannot be read is no
    /// evidence either way: it shows no work and restarts nothing, and the next reading that can
    /// be made is judged against the last one that could. So a transfer that writes nothing and
    /// whose tree is never readable stalls at the end of its window, as an idle one does, and
    /// holds nothing (the repository lease, align.lock) past it.
    pub(super) fn stalled(
        &mut self,
        now: Instant,
        written: u64,
        read: impl FnOnce() -> Option<Work>,
    ) -> bool {
        if written != self.written {
            self.written = written;
            self.last = now;
            self.looked = None;
            self.seen = None;
            return false;
        }
        let look = self.look();
        let quiet = now.saturating_duration_since(self.last);
        let due = self
            .looked
            .is_none_or(|at| now.saturating_duration_since(at) >= look);
        if quiet >= look && due {
            self.looked = Some(now);
            if let Some(work) = read() {
                if let Some((at, before)) = self.seen
                    && worked(before, work, now.saturating_duration_since(at))
                {
                    self.last = now;
                }
                self.seen = Some((now, work));
            }
        }
        now.saturating_duration_since(self.last) >= self.stall
    }
}

/// Whether a tree read `before` and then `after`, `elapsed` apart, did work in between: it read
/// or wrote the disk, or spent at least [`WORK_SHARE`]'s share of a core. A count that fell (a
/// member left the tree unreaped) shows none.
fn worked(before: Work, after: Work, elapsed: Duration) -> bool {
    let disk = matches!(
        (before.io_bytes, after.io_bytes),
        (Some(was), Some(now)) if now > was
    );
    let spent = u128::from(after.cpu_ns.saturating_sub(before.cpu_ns));
    disk || spent.saturating_mul(WORK_SHARE) >= elapsed.as_nanos().max(1)
}

/// What `root` and every process below it have done so far, from one sweep of the process table
/// (which reads it inside the exec gate); `None` where the table was cut short, the root is not
/// in it, a pid repeats, or any member's CPU time cannot be read. A member's reaped children
/// stay in its count of their CPU time, so the sum does not fall when an index-pack ends.
pub(super) fn tree_work(root: u32) -> Option<Work> {
    let sweep = aterm_sysprobe::processes()?;
    if sweep.truncated {
        return None;
    }
    let mut children: BTreeMap<u32, Vec<&aterm_sysprobe::ProcessReading>> = BTreeMap::new();
    let mut top = None;
    for process in &sweep.procs {
        if process.pid == root {
            top = Some(process);
        }
        children.entry(process.ppid).or_default().push(process);
    }
    let mut open = vec![top?];
    let mut seen = BTreeSet::new();
    let mut work = Work {
        cpu_ns: 0,
        io_bytes: Some(0),
    };
    while let Some(process) = open.pop() {
        if !seen.insert(process.pid) {
            return None;
        }
        work.cpu_ns = work
            .cpu_ns
            .saturating_add(process.cpu_ns?.saturating_add(process.child_cpu_ns?));
        work.io_bytes = work
            .io_bytes
            .zip(process.disk_read.zip(process.disk_written))
            .map(|(sum, (read, written))| sum.saturating_add(read).saturating_add(written));
        if let Some(below) = children.get(&process.pid) {
            open.extend(below.iter().copied());
        }
    }
    Some(work)
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
