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
//! `setpgid`) is absent from the accounting snapshot. Custody reads reconcile previously
//! observed identities against the kernel until positive exit, absence or birth replacement;
//! missing accounting or identity data cannot establish their end. Those conclusive facts
//! are reused for this meter's lifetime. A member first read once the root is gone is the
//! tree's where its parents still lead to a member an earlier read booked inside it; where
//! they lead nowhere known its ancestry is undecided that read, never booked outside for
//! good, so [`Tally::left`]'s children-since can still find it under a known parent.
//! Ancestry through the root itself is fenced to the original root (its pid and its
//! start): a later process given the root's pid authorizes no member, and no read made
//! under it replaces the last one the original root lived for. Cleanup queries retain each
//! newly discovered child's identity, so its parent's end cannot erase the observation.
//! These are sampled ancestry checks, not atomic containment. Never-observed descendants
//! remain outside this observation contract. Off macOS none is read. A caller deciding a tree is drained reads
//! [`TreeWatch::left_observed`] instead: only its [`Left::Read`] with nothing in it says a read
//! of the group found none left, where a group no read ever found ([`Left::Unobserved`]) and a
//! read of a known group that failed ([`Left::Unread`]) say nothing is known, and never that
//! none is left.
//!
//! ITS SURVIVORS ([`TreeMeter::survivors`]), narrower: the live members of that group that
//! descended from the root at the last read made while the root lived, each the same process. A
//! process that left the tree while the root lived (a daemon that forked away from it, its
//! parent ended) is none, though an earlier read saw it inside. The group is the one the first
//! read found the root in, kept, so the reads go on once the root is gone.

use std::collections::{HashMap, HashSet};
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

/// TEST SEAM: the latest attempted group read, not a claim that every group member was readable.
/// The underlying reader omits members whose accounting it could not read. Each successful
/// read holds at most [`aterm_sysprobe::MAX_GROUP_PIDS`] members.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// Monotonic attempted-read number within this meter.
    pub sequence: u64,
    /// Whether this attempt obtained the group's accounting.
    pub available: bool,
    /// Whether the root still lived after this read's ancestry walk.
    pub root_lived: bool,
    /// The group kept by the meter, when known.
    pub group: Option<u32>,
    /// Accounting from this attempt, with eligibility under the kept last-live-root sample.
    pub members: Vec<(Member, bool)>,
}

/// TEST SEAM: one current survivor read and the exact accounting it selected from.
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
pub struct SurvivorObservation {
    /// The selected live PIDs, as in [`TreeMeter::survivors_using`].
    pub pids: Vec<u32>,
    /// Their summed current footprint, bytes.
    pub footprint: u64,
    /// Current group members; unreadable members are omitted by the platform reader.
    pub members: Vec<(Alive, u64)>,
}

/// What a meter has read: the most its tree held together at one read, and the largest lifetime
/// peak one member had, with whether each member seen was in the tree.
#[derive(Debug, Default)]
pub struct Tally {
    together: u64,
    one: u64,
    seen: HashMap<(u32, u64), bool>,
    /// The original root (its pid and its start), from the first read that found it: the
    /// only root process whose ancestry authorizes a member.
    root: Option<(u32, u64)>,
    /// The original root positively ended or its pid was taken anew: conclusive, kept.
    root_ended: bool,
    /// Conclusive kernel termination or replacement facts, reused for this meter's lifetime.
    ended: HashSet<(u32, u64)>,
    /// The process group the tree ran in, from the first read that found its root.
    group: Option<u32>,
    /// The members that descended from the root at the last read made while it lived.
    within: Vec<(u32, u64)>,
    #[cfg(any(test, debug_assertions))]
    observation: Observation,
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
    #[cfg(any(test, debug_assertions))]
    fn observe(&mut self, members: Option<&[Member]>, root_lived: bool) {
        self.observation = Observation {
            sequence: self.observation.sequence.saturating_add(1),
            available: members.is_some(),
            root_lived,
            group: self.group,
            members: members
                .unwrap_or_default()
                .iter()
                .take(aterm_sysprobe::MAX_GROUP_PIDS)
                .map(|m| (*m, self.within.contains(&(m.pid, m.start))))
                .collect(),
        };
    }

    /// Book one read of `members`: those `in_tree` takes, asked once per member (a pid and its
    /// start) until it decides, summed by footprint; their lifetime peaks floor the result.
    /// `in_tree` answers `None` where a member's ancestry cannot be decided this read (its
    /// parents lead neither to the original root while it lives nor to a member already
    /// booked inside): that member is summed nowhere and, above all, not booked outside, so
    /// a later read — or [`Self::left`]'s children-since under a known parent — can still
    /// find it the tree's. Only a decided answer is kept for the member's life.
    pub fn book(&mut self, members: &[Member], in_tree: &mut dyn FnMut(u32) -> Option<bool>) {
        let mut sum = 0u64;
        for m in members {
            let inside = match self.seen.get(&(m.pid, m.start)) {
                Some(&inside) => inside,
                None => match in_tree(m.pid) {
                    Some(inside) => {
                        self.seen.insert((m.pid, m.start), inside);
                        inside
                    }
                    None => continue,
                },
            };
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

    /// Book which of `members` (one read's) descend from the root now, as `descends` says, asked
    /// only of those [`Self::book`] took inside; kept only where `root_lived` (the root was still
    /// alive once the walk was done, so every parent it read was the tree's), else the last such
    /// read's stands.
    pub fn mark_within(
        &mut self,
        members: &[Member],
        descends: &mut dyn FnMut(u32) -> bool,
        root_lived: &mut dyn FnMut() -> bool,
    ) {
        let within: Vec<(u32, u64)> = members
            .iter()
            .filter(|m| self.seen.get(&(m.pid, m.start)) == Some(&true) && descends(m.pid))
            .map(|m| (m.pid, m.start))
            .collect();
        if root_lived() {
            self.within = within;
        }
    }

    /// The pids of `now` (the live members of the tree's group) that SURVIVE ITS ROOT: each one
    /// the last read made while the root lived found descending from it ([`Self::mark_within`]),
    /// the same process by its start. A member that read did not find so, and a child born
    /// since, is none.
    #[must_use]
    pub fn survivors(&self, now: &[Alive]) -> Vec<u32> {
        now.iter()
            .filter(|a| {
                self.within.contains(&(a.pid, a.start)) && !self.ended.contains(&(a.pid, a.start))
            })
            .map(|a| a.pid)
            .collect()
    }

    /// [`Self::survivors`] of `now` (the live members of the tree's group, each with its footprint
    /// now, bytes), and the footprint they hold together.
    #[must_use]
    pub fn survivors_using(&self, now: &[(Alive, u64)]) -> (Vec<u32>, u64) {
        now.iter()
            .filter(|(a, _)| {
                self.within.contains(&(a.pid, a.start)) && !self.ended.contains(&(a.pid, a.start))
            })
            .fold((Vec::new(), 0), |(mut pids, sum), (a, footprint)| {
                pids.push(a.pid);
                (pids, sum.saturating_add(*footprint))
            })
    }

    /// The pids of `now` (the live members of the tree's group) LEFT OF THE TREE: each one a read
    /// booked inside it, the same process by its start, and each child since of one left (a
    /// member no read saw). A member a read booked outside it is never left, nor a child of one.
    #[must_use]
    pub fn left(&mut self, now: &[Alive]) -> Vec<u32> {
        self.left_identities(now)
            .into_iter()
            .map(|(pid, _)| pid)
            .collect()
    }

    /// [`Self::left`] with each left process's identity kept (its pid and its start, so a pid
    /// taken anew by another process is another identity): what a caller signs its work on
    /// one left process with, and tells it from a later process given its pid.
    #[must_use]
    pub fn left_identities(&mut self, now: &[Alive]) -> Vec<(u32, u64)> {
        let mut left: Vec<(u32, u64)> = now
            .iter()
            .filter(|a| {
                self.seen.get(&(a.pid, a.start)) == Some(&true)
                    && !self.ended.contains(&(a.pid, a.start))
            })
            .map(|a| (a.pid, a.start))
            .collect();
        loop {
            let before = left.len();
            for a in now {
                let identity = (a.pid, a.start);
                if !self.seen.contains_key(&identity)
                    && left[..before].iter().any(|&(pid, _)| pid == a.ppid)
                    && !left.contains(&identity)
                {
                    left.push(identity);
                }
            }
            if left.len() == before {
                return left;
            }
            for &identity in &left[before..] {
                self.seen.insert(identity, true);
            }
        }
    }

    /// What one attempted read of the tree's group (`now`, `None` where the read failed) says
    /// of what the tree leaves ([`Left`]): no group ever found is [`Left::Unobserved`] and a
    /// failed read of the known group is [`Left::Unread`], neither of them a reading of none.
    #[must_use]
    pub fn left_observed(&mut self, now: Option<&[Alive]>) -> Left {
        match (self.group, now) {
            (None, _) => Left::Unobserved,
            (Some(_), None) => Left::Unread,
            (Some(_), Some(now)) => Left::Read(self.left_identities(now)),
        }
    }

    fn reconcile_known(
        &mut self,
        now: &mut Vec<Alive>,
        mut probe: impl FnMut((u32, u64)) -> Result<Option<Alive>, ()>,
    ) -> bool {
        for (&identity, &inside) in &self.seen {
            if !inside
                || self.ended.contains(&identity)
                || now.iter().any(|a| (a.pid, a.start) == identity)
            {
                continue;
            }
            match probe(identity) {
                Ok(Some(alive)) => now.push(alive),
                Ok(None) => {
                    self.ended.insert(identity);
                }
                Err(()) => return false,
            }
        }
        true
    }
}

/// What one read can say of a tree's survivors ([`TreeMeter::survivors_using_observed`]),
/// as [`Left`] says it of what a tree leaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Survivors {
    /// A read of the tree's group succeeded: its survivors' pids and the footprint they
    /// hold together, bytes. No pid is a read that found none.
    Read(Vec<u32>, u64),
    /// The tree's group is known, and this read of it failed: nothing is known.
    Unread,
    /// No read ever found the tree's group, off macOS included: nothing was ever observed.
    Unobserved,
}

/// What one read can say of what a tree leaves ([`TreeMeter::left_observed`]): unavailable
/// telemetry is its own answer, never a reading of none left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Left {
    /// A read of the tree's group succeeded: the identities (a pid and its start) it leaves.
    /// Empty is a read that found none left.
    Read(Vec<(u32, u64)>),
    /// The tree's group is known, and this read of it failed: nothing is known.
    Unread,
    /// No read ever found the tree's group, off macOS included: nothing was ever observed.
    Unobserved,
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

    /// What a read now can say of what its tree leaves ([`TreeWatch::left_observed`]).
    #[must_use]
    pub fn left_observed(&self) -> Left {
        self.watch().left_observed()
    }

    /// The pids that SURVIVE ITS ROOT now ([`Tally::survivors`]): the live members of the group
    /// it ran in that descended from it at the last read made while it lived. Empty where no read
    /// found the group, and off macOS.
    #[must_use]
    pub fn survivors(&self) -> Vec<u32> {
        let tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        tally
            .group
            .map(|group| tally.survivors(&alive_in(group)))
            .unwrap_or_default()
    }

    /// The survivors now ([`Self::survivors`]) and the footprint they hold together, bytes
    /// ([`Tally::survivors_using`]): what a holder keeps a killed build's claim for. Empty, 0,
    /// where no read found the group, and off macOS.
    #[must_use]
    pub fn survivors_using(&self) -> (Vec<u32>, u64) {
        let tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        tally
            .group
            .map(|group| tally.survivors_using(&alive_using(group)))
            .unwrap_or_default()
    }

    /// What a read now can say of its survivors and their footprint ([`Survivors`]): a caller
    /// deciding whether any survivor keeps a claim held takes a failed read of the known
    /// group ([`Survivors::Unread`]) for no answer, never for none left, and a group no read
    /// ever found ([`Survivors::Unobserved`]) for no observed survivor at all.
    #[must_use]
    pub fn survivors_using_observed(&self) -> Survivors {
        self.survivors_using_read(alive_using_observed)
    }

    fn survivors_using_read(
        &self,
        read: impl FnOnce(u32) -> Option<Vec<(Alive, u64)>>,
    ) -> Survivors {
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(group) = tally.group else {
            return Survivors::Unobserved;
        };
        match read(group) {
            Some(now) => {
                let mut alive: Vec<_> = now.iter().map(|&(a, _)| a).collect();
                if !tally.reconcile_known(&mut alive, known_alive)
                    || alive.iter().any(|a| {
                        tally.within.contains(&(a.pid, a.start))
                            && !tally.ended.contains(&(a.pid, a.start))
                            && !now.iter().any(|(measured, _)| {
                                (measured.pid, measured.start) == (a.pid, a.start)
                            })
                    })
                {
                    return Survivors::Unread;
                }
                let (pids, footprint) = tally.survivors_using(&now);
                Survivors::Read(pids, footprint)
            }
            None => Survivors::Unread,
        }
    }

    /// TEST SEAM: the latest attempted meter read, including an unavailable attempt rather
    /// than silently returning an older successful measurement.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn observation(&self) -> Observation {
        self.tally
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observation
            .clone()
    }

    /// TEST SEAM: the same single read and selection as [`Self::survivors_using`], with the
    /// current members' identities and accounting retained for diagnosis. Those members
    /// come from the same read and do not decide which ones are credited.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn survivors_observed(&self) -> SurvivorObservation {
        let tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let now = tally.group.map(alive_using).unwrap_or_default();
        let (pids, footprint) = tally.survivors_using(&now);
        SurvivorObservation {
            pids,
            footprint,
            members: now,
        }
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
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        tally
            .group
            .map(|group| tally.left(&alive_in(group)))
            .unwrap_or_default()
    }

    /// What a read now can say of what the tree leaves ([`Left`], [`Tally::left_observed`]):
    /// the read a caller ends a tree's custody on, since only a [`Left::Read`] that is empty
    /// is a read of none left.
    #[must_use]
    pub fn left_observed(&self) -> Left {
        self.left_using(alive_in_observed, known_alive)
    }

    fn left_using(
        &self,
        read: impl FnOnce(u32) -> Option<Vec<Alive>>,
        probe: impl FnMut((u32, u64)) -> Result<Option<Alive>, ()>,
    ) -> Left {
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(group) = tally.group else {
            return Left::Unobserved;
        };
        let Some(mut now) = read(group) else {
            return Left::Unread;
        };
        if !tally.reconcile_known(&mut now, probe) {
            return Left::Unread;
        }
        tally.left_observed(Some(&now))
    }
}

// Accounting can omit a live identity. Only positive kernel absence/exit or a changed birth
// closes a previously observed identity; unavailable metadata must retain uncertainty.
#[cfg(target_os = "macos")]
fn known_alive((pid, start): (u32, u64)) -> Result<Option<Alive>, ()> {
    use aterm_uds::process::{self, ProcessPresence};
    let raw = i32::try_from(pid).map_err(|_| ())?;
    match process::process_presence(raw) {
        ProcessPresence::Absent | ProcessPresence::Exited => return Ok(None),
        ProcessPresence::Unknown => return Err(()),
        ProcessPresence::Present => {}
    }
    let before = process::read_process_start(raw).ok_or(())?;
    if before.boot.0 != start {
        return Ok(None);
    }
    let Some(record) = aterm_sysprobe::kproc::record_read(pid).map_err(|_| ())? else {
        return Ok(None);
    };
    if record.zombie {
        return Ok(None);
    }
    let after = process::read_process_start(raw).ok_or(())?;
    if before != after {
        return Ok(None);
    }
    Ok(Some(Alive {
        pid,
        start,
        ppid: record.ppid,
    }))
}

#[cfg(not(target_os = "macos"))]
fn known_alive(_identity: (u32, u64)) -> Result<Option<Alive>, ()> {
    Err(())
}

/// Whether the original root still lives, as one read can tell: only positive kernel
/// absence, exit or birth replacement ends it; unavailable metadata decides nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootState {
    /// The process the meter was started on, by its start, still lives.
    Live,
    /// That process positively ended, or its pid is another process's now.
    Ended,
    /// Its identity could not be read this time.
    Unknown,
}

/// The most parents one ancestry walk follows: deeper than any build nests.
const DEPTH_MAX: usize = 64;

/// Whether `pid` is the tree's, by walking its parents: inside where they reach the
/// original root while it lives or an `anchors` member (one this read lists and an
/// earlier read booked inside, the same process by its start). Reaching the system
/// parent decides outside only while the original root lives. Missing metadata,
/// an ended root, cycles and the depth bound decide nothing (`None`), so they cannot
/// permanently book a leftover's child outside. A reused root pid grants no ancestry.
fn ancestry_verdict(
    pid: u32,
    root: u32,
    root_state: RootState,
    anchors: &HashSet<u32>,
    parent: &mut dyn FnMut(u32) -> Option<u32>,
) -> Option<bool> {
    let mut at = pid;
    for _ in 0..DEPTH_MAX {
        if at == root {
            return match root_state {
                RootState::Live => Some(true),
                RootState::Ended | RootState::Unknown => None,
            };
        }
        if at <= 1 {
            return (root_state == RootState::Live).then_some(false);
        }
        if at != pid && anchors.contains(&at) {
            return Some(true);
        }
        at = parent(at)?;
    }
    None
}

/// `pid`'s parent, including the system parent; `None` where metadata is unavailable
/// or invalid, which is distinct from a positively completed ancestry walk.
#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    match aterm_sysprobe::kproc::record(pid) {
        Some(r) if r.ppid != pid => Some(r.ppid),
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn parent_of(pid: u32) -> Option<u32> {
    let _ = pid;
    None
}

/// The original root's state now and its start where first read here: with its start
/// known, the same kernel presence and birth reconciliation custody reads use
/// ([`known_alive`]); without it, presence and a first start read. An ended answer is
/// the caller's to keep — a pid taken anew never revives the original root.
#[cfg(target_os = "macos")]
fn probe_root(root: u32, birth: Option<u64>) -> (RootState, Option<u64>) {
    use aterm_uds::process::{self, ProcessPresence};
    if let Some(birth) = birth {
        return (
            match known_alive((root, birth)) {
                Ok(Some(_)) => RootState::Live,
                Ok(None) => RootState::Ended,
                Err(()) => RootState::Unknown,
            },
            Some(birth),
        );
    }
    let Ok(raw) = i32::try_from(root) else {
        return (RootState::Unknown, None);
    };
    match process::process_presence(raw) {
        ProcessPresence::Absent | ProcessPresence::Exited => (RootState::Ended, None),
        ProcessPresence::Unknown => (RootState::Unknown, None),
        ProcessPresence::Present => match process::read_process_start(raw) {
            Some(start) => match aterm_sysprobe::kproc::record_read(root) {
                Ok(Some(record)) if !record.zombie => (RootState::Live, Some(start.boot.0)),
                Ok(_) => (RootState::Ended, Some(start.boot.0)),
                Err(_) => (RootState::Unknown, Some(start.boot.0)),
            },
            None => (RootState::Unknown, None),
        },
    }
}

#[cfg(not(target_os = "macos"))]
fn probe_root(root: u32, birth: Option<u64>) -> (RootState, Option<u64>) {
    let _ = root;
    (RootState::Unknown, birth)
}

/// One read of `root`'s tree booked into `tally`: the members of the group the first read found
/// it in (kept, so a read once the root is gone still finds them), each new one decided by
/// its ancestry to the original root or to a member already booked inside
/// ([`ancestry_verdict`]), and which of those descend from it while it lives
/// ([`Tally::mark_within`]).
fn read_into(root: u32, tally: &Mutex<Tally>) {
    let known = tally.lock().unwrap_or_else(PoisonError::into_inner).group;
    let Some((group, members)) = members_of(root, known) else {
        #[cfg(any(test, debug_assertions))]
        tally
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(None, false);
        return;
    };
    let mut tally = tally.lock().unwrap_or_else(PoisonError::into_inner);
    tally.group.get_or_insert(group);
    if tally.root.is_none()
        && let Some(member) = members.iter().find(|m| m.pid == root)
    {
        tally.root = Some((root, member.start));
    }
    // A listed root pid with another start is positive replacement: the original root
    // ended before its pid could be taken anew, accounting complete or not.
    if let Some((_, birth)) = tally.root
        && members.iter().any(|m| m.pid == root && m.start != birth)
    {
        tally.root_ended = true;
    }
    let (root_state, adopted) = if tally.root_ended {
        (RootState::Ended, tally.root.map(|(_, birth)| birth))
    } else {
        probe_root(root, tally.root.map(|(_, birth)| birth))
    };
    if tally.root.is_none() {
        tally.root = adopted.map(|birth| (root, birth));
    }
    if root_state == RootState::Ended {
        tally.root_ended = true;
    }
    let birth = tally.root.map(|(_, birth)| birth);
    let anchors: HashSet<u32> = members
        .iter()
        .filter(|m| {
            tally.seen.get(&(m.pid, m.start)) == Some(&true)
                && !tally.ended.contains(&(m.pid, m.start))
        })
        .map(|m| m.pid)
        .collect();
    // Stage new decisions until the original root's identity has been checked again.
    // A root that ended during the walk cannot authorize a new permanent booking.
    let mut parents: HashMap<u32, Option<u32>> = HashMap::with_capacity(members.len());
    let mut cached_parent = |pid: u32| *parents.entry(pid).or_insert_with(|| parent_of(pid));
    let mut verdicts: HashMap<u32, Option<bool>> = members
        .iter()
        .filter(|m| !tally.seen.contains_key(&(m.pid, m.start)))
        .map(|m| {
            (
                m.pid,
                ancestry_verdict(m.pid, root, root_state, &anchors, &mut cached_parent),
            )
        })
        .collect();
    // Survivors are narrower than leftovers: an old anchor that was reparented
    // while the root lived must not replace a current chain to the original root.
    let no_anchors = HashSet::new();
    let within: HashSet<u32> = if root_state == RootState::Live {
        members
            .iter()
            .filter(|m| {
                tally.seen.get(&(m.pid, m.start)) == Some(&true)
                    || verdicts.get(&m.pid) == Some(&Some(true))
            })
            .filter(|m| {
                ancestry_verdict(m.pid, root, root_state, &no_anchors, &mut cached_parent)
                    == Some(true)
            })
            .map(|m| m.pid)
            .collect()
    } else {
        HashSet::new()
    };
    let (after, _) = if root_state == RootState::Ended {
        (RootState::Ended, birth)
    } else {
        probe_root(root, birth)
    };
    if root_state == RootState::Live && after != RootState::Live {
        for (&pid, verdict) in &mut verdicts {
            *verdict = ancestry_verdict(pid, root, after, &anchors, &mut cached_parent);
        }
    }
    tally.book(&members, &mut |pid| verdicts.get(&pid).copied().flatten());
    let root_lived = root_state == RootState::Live && after == RootState::Live;
    tally.mark_within(&members, &mut |pid| within.contains(&pid), &mut || {
        root_lived
    });
    if after == RootState::Ended {
        tally.root_ended = true;
    }
    #[cfg(any(test, debug_assertions))]
    tally.observe(Some(&members), root_lived);
}

/// `root`'s process group (`known`, else the one the kernel records for it now) and its members
/// now; `None` where none is read.
#[cfg(target_os = "macos")]
fn members_of(root: u32, known: Option<u32>) -> Option<(u32, Vec<Member>)> {
    let pgid = match known {
        Some(group) => group,
        None => u32::try_from(aterm_sysprobe::kproc::record(root)?.pgid).ok()?,
    };
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
fn members_of(root: u32, known: Option<u32>) -> Option<(u32, Vec<Member>)> {
    let _ = (root, known);
    None
}

/// The live members of process group `group` now, each with its parent: none that has exited
/// unreaped.
#[cfg(target_os = "macos")]
fn alive_in(group: u32) -> Vec<Alive> {
    alive_in_observed(group).unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn alive_in(group: u32) -> Vec<Alive> {
    let _ = group;
    Vec::new()
}

/// One listed accounting member's live ancestry, checked against its captured birth.
/// An unavailable record is uncertainty; positive absence/exit/replacement is completion.
#[cfg(target_os = "macos")]
fn member_alive(member: &aterm_sysprobe::MemberUsage) -> Result<Option<Alive>, ()> {
    use aterm_uds::process::{self, ProcessPresence};
    let Some(record) = aterm_sysprobe::kproc::record_read(member.pid).map_err(|_| ())? else {
        return Ok(None);
    };
    if record.zombie {
        return Ok(None);
    }
    let raw = i32::try_from(member.pid).map_err(|_| ())?;
    let Some(start) = process::read_process_start(raw) else {
        return match process::process_presence(raw) {
            ProcessPresence::Absent | ProcessPresence::Exited => Ok(None),
            ProcessPresence::Unknown | ProcessPresence::Present => Err(()),
        };
    };
    if start.boot.0 != member.start {
        return Ok(None);
    }
    Ok(Some(Alive {
        pid: member.pid,
        start: member.start,
        ppid: record.ppid,
    }))
}

/// [`alive_in`] with unavailable metadata kept distinct from an empty reading. Accounting
/// can omit members entirely; custody readers must also reconcile their known identities.
#[cfg(target_os = "macos")]
fn alive_in_observed(group: u32) -> Option<Vec<Alive>> {
    let members = aterm_sysprobe::group_usage(group)?;
    let mut alive = Vec::with_capacity(members.len());
    for member in members {
        if let Some(member) = member_alive(&member).ok()? {
            alive.push(member);
        }
    }
    Some(alive)
}

#[cfg(not(target_os = "macos"))]
fn alive_in_observed(group: u32) -> Option<Vec<Alive>> {
    let _ = group;
    None
}

/// [`alive_in`], each member with its footprint now, bytes.
#[cfg(target_os = "macos")]
fn alive_using(group: u32) -> Vec<(Alive, u64)> {
    alive_using_observed(group).unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn alive_using(group: u32) -> Vec<(Alive, u64)> {
    let _ = group;
    Vec::new()
}

/// [`alive_using`] where the group's own read succeeded, and `None` where it failed or (off
/// macOS) no reader reads one, as [`alive_in_observed`] distinguishes them.
#[cfg(target_os = "macos")]
fn alive_using_observed(group: u32) -> Option<Vec<(Alive, u64)>> {
    let members = aterm_sysprobe::group_usage(group)?;
    let mut alive = Vec::with_capacity(members.len());
    for member in members {
        if let Some(identity) = member_alive(&member).ok()? {
            alive.push((identity, member.footprint));
        }
    }
    Some(alive)
}

#[cfg(not(target_os = "macos"))]
fn alive_using_observed(group: u32) -> Option<Vec<(Alive, u64)>> {
    let _ = group;
    None
}

#[cfg(test)]
#[path = "tree_meter_tests.rs"]
mod tests;

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn an_unavailable_attempt_never_reuses_a_successful_measurement() {
        let mut tally = Tally {
            group: Some(9),
            within: vec![(42, 7)],
            ..Tally::default()
        };
        let member = Member {
            pid: 42,
            start: 7,
            footprint: 96 << 20,
            peak: 96 << 20,
        };
        tally.observe(Some(&[member]), true);
        assert_eq!(tally.observation.sequence, 1);
        assert_eq!(tally.observation.members, [(member, true)]);
        tally.observe(None, false);
        assert_eq!(tally.observation.sequence, 2);
        assert!(!tally.observation.available);
        assert!(tally.observation.members.is_empty());
        assert_eq!(tally.within, [(42, 7)], "diagnosis does not change policy");
        tally.observe(Some(&[Member { start: 8, ..member }]), true);
        assert!(
            !tally.observation.members[0].1,
            "a reused PID is not eligible"
        );
    }
}
