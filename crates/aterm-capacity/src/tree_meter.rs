// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MEMORY A BUILD'S PROCESS TREE TAKES (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! BUILD-ADMIT), read as VERIFY-MEM reads a stage's: every [`READ_EVERY`], and once as it starts,
//! the members of the root's process group (`aterm_sysprobe::group_usage`) that descend from the
//! root are summed by their footprints, and the meter's peak is the most they held together at
//! any read, never less than the largest lifetime peak the kernel kept for one of them. A build
//! run in its caller's group (a `targo` a front supervises in a terminal's job) shares it with
//! its caller and any other stage of a pipeline, so the members are told apart by their parents
//! (`aterm_sysprobe::kproc`), each pid once per start. Off macOS no group is read, and the peak is
//! the floor its caller hands [`TreeMeter::stop`] (the reaped tree's largest resident peak).
//!
//! WHAT A TREE LEAVES ([`TreeWatch::left`]): once its root is gone, the members of the group it
//! ran in that a read saw inside it, each the same process (a pid and its start), and those
//! members' children since: a compiler or build script a killed `cargo` left running, reparented
//! away so its parents no longer lead to the root. One that left the group (`setsid`,
//! `setpgid`) is not read. Off macOS none is read.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often a running meter reads its tree: VERIFY-MEM's half second.
pub const READ_EVERY: Duration = Duration::from_millis(500);

/// One member of a tree, as one read sees it: the pid, when it started (so a pid taken anew is
/// another member), its footprint now and its lifetime peak, bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its pid.
    pub pid: u32,
    /// When it started, in the platform's own ticks: with [`Self::pid`], which process it is.
    pub start: u64,
    /// Its footprint now, bytes.
    pub footprint: u64,
    /// The most footprint it has had, bytes.
    pub peak: u64,
}

/// What a meter has read: the most its tree held together at one read, and the largest lifetime
/// peak one member had, with whether each member seen was in the tree.
#[derive(Debug, Default)]
pub struct Tally {
    together: u64,
    one: u64,
    seen: HashMap<(u32, u64), bool>,
    /// The process group the tree ran in, from the first read that found its root.
    group: Option<u32>,
}

/// One live member of a group, as [`Tally::left`] reads it: the pid, when it started, and its
/// parent's pid now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alive {
    /// Its pid.
    pub pid: u32,
    /// When it started, in the platform's own ticks (as [`Member::start`]).
    pub start: u64,
    /// Its parent's pid now.
    pub ppid: u32,
}

impl Tally {
    /// Book one read of `members`: those `in_tree` takes, asked once per member (a pid and its
    /// start), summed by footprint; their lifetime peaks floor the result.
    pub fn book(&mut self, members: &[Member], in_tree: &mut dyn FnMut(u32) -> bool) {
        let mut sum = 0u64;
        for m in members {
            let inside = *self
                .seen
                .entry((m.pid, m.start))
                .or_insert_with(|| in_tree(m.pid));
            if inside {
                sum = sum.saturating_add(m.footprint);
                self.one = self.one.max(m.peak);
            }
        }
        self.together = self.together.max(sum);
    }

    /// The peak read so far, bytes: [`Self::book`]'s sums and lifetime peaks, the most of them.
    #[must_use]
    pub fn peak(&self) -> u64 {
        self.together.max(self.one)
    }

    /// The pids of `now` (the live members of the tree's group) LEFT OF THE TREE: each one a read
    /// booked inside it, the same process by its start, and each child since of one left (a
    /// member no read saw). A member a read booked outside it is never left, nor a child of one.
    #[must_use]
    pub fn left(&self, now: &[Alive]) -> Vec<u32> {
        let mut left: Vec<u32> = now
            .iter()
            .filter(|a| self.seen.get(&(a.pid, a.start)) == Some(&true))
            .map(|a| a.pid)
            .collect();
        loop {
            let more: Vec<u32> = now
                .iter()
                .filter(|a| {
                    !self.seen.contains_key(&(a.pid, a.start))
                        && left.contains(&a.ppid)
                        && !left.contains(&a.pid)
                })
                .map(|a| a.pid)
                .collect();
            if more.is_empty() {
                return left;
            }
            left.extend(more);
        }
    }
}

/// A running read of one tree's memory ([`TreeMeter::start`]).
pub struct TreeMeter {
    root: u32,
    tally: Arc<Mutex<Tally>>,
    stop: Option<mpsc::Sender<()>>,
    reader: Option<JoinHandle<()>>,
}

impl TreeMeter {
    /// Start reading the tree rooted at `root`: once now, then every [`READ_EVERY`] on a thread of
    /// its own until [`Self::stop`].
    #[must_use]
    pub fn start(root: u32) -> TreeMeter {
        let tally = Arc::new(Mutex::new(Tally::default()));
        read_into(root, &tally);
        let (tx, rx) = mpsc::channel::<()>();
        let reading = Arc::clone(&tally);
        let reader = std::thread::Builder::new()
            .name("build-meter".to_owned())
            .spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(READ_EVERY) {
                    read_into(root, &reading);
                }
            })
            .ok();
        TreeMeter {
            root,
            tally,
            stop: Some(tx),
            reader,
        }
    }

    /// Read the tree once more, now.
    pub fn read(&self) {
        read_into(self.root, &self.tally);
    }

    /// A handle that reads this tree and what it leaves ([`TreeWatch`]), for another thread.
    #[must_use]
    pub fn watch(&self) -> TreeWatch {
        TreeWatch {
            root: self.root,
            tally: Arc::clone(&self.tally),
        }
    }

    /// The pids its tree leaves now ([`TreeWatch::left`]).
    #[must_use]
    pub fn left(&self) -> Vec<u32> {
        self.watch().left()
    }

    /// The peak read so far, bytes.
    #[must_use]
    pub fn peak(&self) -> u64 {
        self.tally
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .peak()
    }

    /// Stop reading: the peak, never less than `floor` (the reaped tree's largest resident peak,
    /// which a caller reads from its child's accounting).
    #[must_use]
    pub fn stop(mut self, floor: u64) -> u64 {
        self.end();
        self.peak().max(floor)
    }

    fn end(&mut self) {
        drop(self.stop.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for TreeMeter {
    fn drop(&mut self) {
        self.end();
    }
}

/// A meter's tree, read from another thread ([`TreeMeter::watch`]): a read now, and what the tree
/// leaves.
#[derive(Clone, Debug)]
pub struct TreeWatch {
    root: u32,
    tally: Arc<Mutex<Tally>>,
}

impl TreeWatch {
    /// Read the tree once more, now: booked as the meter's own reads are. (Not `read`: the
    /// lock-order census names a lock by such a method's name.)
    pub fn look(&self) {
        read_into(self.root, &self.tally);
    }

    /// The pids the tree LEAVES now ([`Tally::left`]): the live members of the group it ran in,
    /// unreaped exits aside, that a read saw inside it, and their children since. Empty where no
    /// read found the group, and off macOS.
    #[must_use]
    pub fn left(&self) -> Vec<u32> {
        let tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        tally
            .group
            .map(|group| tally.left(&alive_in(group)))
            .unwrap_or_default()
    }
}

/// One read of `root`'s tree booked into `tally`.
fn read_into(root: u32, tally: &Mutex<Tally>) {
    let Some((group, members)) = members_of(root) else {
        return;
    };
    let mut tally = tally.lock().unwrap_or_else(PoisonError::into_inner);
    tally.group.get_or_insert(group);
    tally.book(&members, &mut |pid| descends(pid, root));
}

/// `root`'s process group and its members now; `None` where none is read.
#[cfg(target_os = "macos")]
fn members_of(root: u32) -> Option<(u32, Vec<Member>)> {
    let pgid = u32::try_from(aterm_sysprobe::kproc::record(root)?.pgid).ok()?;
    let members = aterm_sysprobe::group_usage(pgid)?;
    Some((
        pgid,
        members
            .iter()
            .map(|m| Member {
                pid: m.pid,
                start: m.start,
                footprint: m.footprint,
                peak: m.peak_footprint,
            })
            .collect(),
    ))
}

#[cfg(not(target_os = "macos"))]
fn members_of(root: u32) -> Option<(u32, Vec<Member>)> {
    let _ = root;
    None
}

/// The live members of process group `group` now, each with its parent: none that has exited
/// unreaped.
#[cfg(target_os = "macos")]
fn alive_in(group: u32) -> Vec<Alive> {
    aterm_sysprobe::group_usage(group)
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let record = aterm_sysprobe::kproc::record(m.pid)?;
            (!record.zombie).then_some(Alive {
                pid: m.pid,
                start: m.start,
                ppid: record.ppid,
            })
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn alive_in(group: u32) -> Vec<Alive> {
    let _ = group;
    Vec::new()
}

/// The most parents [`descends`] walks: deeper than any build nests.
#[cfg(target_os = "macos")]
const DEPTH_MAX: usize = 64;

/// Whether `pid` is `root` or descends from it, by the parents the kernel records now.
#[cfg(target_os = "macos")]
fn descends(pid: u32, root: u32) -> bool {
    let mut at = pid;
    for _ in 0..DEPTH_MAX {
        if at == root {
            return true;
        }
        match aterm_sysprobe::kproc::record(at) {
            Some(r) if r.ppid > 1 && r.ppid != at => at = r.ppid,
            _ => return false,
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
fn descends(pid: u32, root: u32) -> bool {
    pid == root
}

#[cfg(test)]
#[path = "tree_meter_tests.rs"]
mod tests;
