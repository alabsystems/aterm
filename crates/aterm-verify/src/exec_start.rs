// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stage child's tree at macOS's program check (`docs/DESIGN-leftover-processes-2026-10-16.md`
//! §5.1, §8.1 step 5): which held members its clock judges, whether one sent its check to a
//! syspolicyd that is no longer running (stranded, [`aterm_sysprobe::jobs::stranded`]), and what
//! verify says of it. A held program is work in line: only a stranded one ends its child before
//! the ceiling, and at the ceiling a leader that has not started, or a member held past the bound
//! on a first exec, decided nothing. verify ends only its own stage child this way, through the
//! group it holds, and never touches the program's file.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_sysprobe::jobs::{self as probe, AtStart, StartFacts};
use aterm_uds::program_check::Daemon;

use super::Host;

/// A member of a child's tree whose one thread never ran ([`Host::unstarted`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unstarted {
    /// Its pid.
    pub pid: u32,
    /// What its start read: [`StartFacts::lone_zero`], which [`probe::judge_start`] judges.
    pub facts: StartFacts,
    /// Its executable; `None` when unread.
    pub exe: Option<PathBuf>,
    /// The executables its parents in the group run, nearest first, up to and including the
    /// group's leader (none for the leader itself); `None` when a parent's record or executable
    /// is unread or the walk leaves the group ([`chain_of`]).
    pub chain: Option<Vec<PathBuf>>,
}

/// The syspolicyd running now ([`Host::daemon`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonNow {
    /// It runs.
    Running(Daemon),
    /// None runs (launchd starts one at the next request).
    NoneRunning,
    /// The process table or a candidate's image could not be read, or off macOS.
    Unread,
}

/// Whether the syspolicyd running now answers alone ([`Host::backlog`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Backlog {
    /// Whether its pool reads full; `None` when unread, or when the read found another daemon.
    pub pool_full: Option<bool>,
    /// How many of this user's held programs forked at or after its start; `None` when unread.
    pub held_since: Option<usize>,
}

/// What a watch found that ends its child unrun: why, and whether the program held is the
/// leader, which never ran, or a deeper member, under a leader that started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Verdict {
    pub(super) words: String,
    pub(super) leader: bool,
}

/// How often a clock asks where its tree's quiet members stand at their start, after its first
/// read of the tree; one read of the daemon or its backlog serves every clock this long
/// ([`kept`]).
pub(super) const LOOK_EVERY: Duration = Duration::from_secs(5);

/// How many parents a walk up a group takes before it reads as unread ([`chain_of`]).
#[cfg(any(target_os = "macos", test))]
const MAX_CHAIN: usize = 64;

/// Whether a member whose parents in the group run `chain` ([`Unstarted::chain`]) is its child's
/// to judge: every one was read, and none runs from a directory named `deps`, where cargo puts
/// its test and bench binaries. A program a test started is the test's to bound (its own hang
/// detector), and a failed read never counts as leave to judge.
#[must_use]
pub(super) fn in_scope(chain: Option<&[PathBuf]>) -> bool {
    chain.is_some_and(|chain| {
        !chain
            .iter()
            .any(|p| p.parent().and_then(Path::file_name) == Some(OsStr::new("deps")))
    })
}

/// The executables `pid`'s parents in process group `pgid` run, nearest first, up to and
/// including its leader `pgid` ([`Unstarted::chain`]), read by `record` (a pid's parent and
/// group) and `path` (its executable). `None` when a record or path is unread, a parent is
/// outside the group, or the walk passes [`MAX_CHAIN`] parents.
#[cfg(any(target_os = "macos", test))]
pub(super) fn chain_of(
    pid: u32,
    pgid: u32,
    record: impl Fn(u32) -> Option<(u32, u32)>,
    path: impl Fn(u32) -> Option<PathBuf>,
) -> Option<Vec<PathBuf>> {
    let mut chain = Vec::new();
    let mut at = pid;
    while at != pgid {
        if chain.len() == MAX_CHAIN {
            return None;
        }
        let (parent, _) = record(at)?;
        let (_, group) = record(parent)?;
        if group != pgid {
            return None;
        }
        chain.push(path(parent)?);
        at = parent;
    }
    Some(chain)
}

/// What [`Host::unstarted`] reads of `pids` in group `pgid` on macOS: each one's start in one
/// mount table read, kept when its one thread never ran, with its executable and its parents'
/// chain. Nothing of any argument or environment is read (leftover M29).
#[cfg(target_os = "macos")]
pub(super) fn read_unstarted(pgid: u32, pids: &[u32], now_us: u64) -> Vec<Unstarted> {
    let mounts = std::cell::OnceCell::new();
    let record = |pid| {
        aterm_sysprobe::kproc::record(pid).map(|r| (r.ppid, u32::try_from(r.pgid).unwrap_or(0)))
    };
    pids.iter()
        .filter_map(|&pid| {
            let facts = probe::read_start_in(pid, now_us, &mounts)?;
            facts.lone_zero.then(|| Unstarted {
                pid,
                facts,
                exe: aterm_sysprobe::process_path(pid),
                chain: chain_of(pid, pgid, record, aterm_sysprobe::process_path),
            })
        })
        .collect()
}

/// `read()`, or what it gave for `key` within [`LOOK_EVERY`]: one read of the daemon and its
/// backlog serves a test pool's every worker. The cell is not held while `read` runs, which
/// may wait [`aterm_uds::program_check::PS_HANG`].
#[cfg(target_os = "macos")]
pub(super) fn kept<K: Copy + PartialEq, T: Copy>(
    cell: &std::sync::Mutex<Option<(Instant, K, T)>>,
    key: K,
    read: impl FnOnce() -> T,
) -> T {
    let lock = || {
        cell.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((at, k, v)) = *lock()
        && k == key
        && at.elapsed() < LOOK_EVERY
    {
        return v;
    }
    let v = read();
    *lock() = Some((Instant::now(), key, v));
    v
}

/// [`aterm_uds::deadline::FIRST_EXEC_HANG`] in µs, the bound on a first exec.
fn first_exec_hang_us() -> u64 {
    u64::try_from(aterm_uds::deadline::FIRST_EXEC_HANG.as_micros()).unwrap_or(u64::MAX)
}

/// A child's watch over its tree's starts: when it last asked, since when it has read each
/// member unrun, and whether it has said that a member is held past
/// [`aterm_uds::deadline::FIRST_EXEC_HANG`].
pub(super) struct Watch<'a> {
    host: &'a dyn Host,
    /// The child's process group, its leader's pid.
    pgid: u32,
    /// The leader's name: its program's basename.
    leader: String,
    asked: Option<Instant>,
    /// Each member every look since the first of these read unrun: since when (µs since the
    /// epoch, [`Host::now_us`]) and its CPU then, as its clock last read it.
    unrun: BTreeMap<u32, (u64, Option<u64>)>,
    said: bool,
}

impl<'a> Watch<'a> {
    /// The watch over the tree of the child `pgid` leads, which runs `program`.
    pub(super) fn new(host: &'a dyn Host, pgid: u32, program: &Path) -> Watch<'a> {
        Watch {
            host,
            pgid,
            leader: basename(program),
            asked: None,
            unrun: BTreeMap::new(),
            said: false,
        }
    }

    /// The members of `members` (each a pid and its CPU, `None` when unread) that
    /// [`Host::unstarted`] reads unrun at `now_us`, each with the time its exec had come by: the
    /// first of the looks, each of which read it unrun at the same CPU, that run to this one. A
    /// member read ran, unread, or at another CPU (it ran between two looks, and may have exec'd
    /// again) starts over at `now_us`; its fork dates nothing, since an exec in the same pid (a
    /// shell's `exec`, `taskpolicy`'s) keeps it.
    fn look(&mut self, members: &[(u32, Option<u64>)], now_us: u64) -> Vec<(Unstarted, u64)> {
        let pids: Vec<u32> = members.iter().map(|&(pid, _)| pid).collect();
        let mut unrun = BTreeMap::new();
        let read: Vec<(Unstarted, u64)> = self
            .host
            .unstarted(self.pgid, &pids, now_us)
            .into_iter()
            .filter(|m| m.facts.lone_zero)
            .map(|m| {
                let cpu = members.iter().find(|&&(p, _)| p == m.pid).and_then(|m| m.1);
                let since = match self.unrun.get(&m.pid) {
                    Some(&(since, Some(then))) if cpu == Some(then) => since,
                    _ => now_us,
                };
                unrun.insert(m.pid, (since, cpu));
                (m, since)
            })
            .collect();
        self.unrun = unrun;
        read
    }

    /// The verdict for the stranded member of its tree whose exec is oldest, if one is. It asks
    /// once `quiet` names the members whose CPU read at most [`probe::HELD_CPU_NS`] at the
    /// clock's last read of the tree, each with that CPU (`None` before the first), then every
    /// [`LOOK_EVERY`], in this order: the members [`Host::unstarted`] gives that are held and
    /// [`in_scope`], each dated by [`Watch::look`]; the daemon, only if one is; its backlog, only
    /// if one is [`probe::before_daemon`] it. A held member seen unrun for
    /// [`aterm_uds::deadline::FIRST_EXEC_HANG`] that is not stranded is said once, on stderr under
    /// `label`: it is work in line, and its child waits on it.
    pub(super) fn stranded(
        &mut self,
        quiet: impl FnOnce() -> Option<Vec<(u32, u64)>>,
        label: &str,
    ) -> Option<Verdict> {
        let now = Instant::now();
        if self
            .asked
            .is_some_and(|t| now.saturating_duration_since(t) < LOOK_EVERY)
        {
            return None;
        }
        let quiet = quiet()?;
        self.asked = Some(now);
        if quiet.is_empty() {
            self.unrun.clear();
            return None;
        }
        let now_us = self.host.now_us();
        let members: Vec<(u32, Option<u64>)> = quiet
            .into_iter()
            .map(|(pid, cpu)| (pid, Some(cpu)))
            .collect();
        let held: Vec<(Unstarted, u64)> = self
            .look(&members, now_us)
            .into_iter()
            .filter(|(m, _)| {
                probe::judge_start(&m.facts) == AtStart::Held && in_scope(m.chain.as_deref())
            })
            .collect();
        if held.is_empty() {
            return None;
        }
        let daemon = self.host.daemon();
        if let DaemonNow::Running(d) = daemon {
            let start = d.started_us();
            if held
                .iter()
                .any(|&(_, since)| probe::before_daemon(Some(since), start, now_us))
            {
                let b = self.host.backlog(d, start);
                if let Some((m, since)) = held
                    .iter()
                    .filter(|(m, since)| {
                        probe::stranded(
                            &m.facts,
                            Some(*since),
                            start,
                            now_us,
                            b.pool_full,
                            b.held_since,
                        )
                    })
                    .min_by_key(|&&(_, since)| since)
                {
                    return Some(Verdict {
                        words: self.stranded_words(m, *since, d, now_us),
                        leader: m.pid == self.pgid,
                    });
                }
            }
        }
        if !self.said
            && let Some((m, since)) = held
                .iter()
                .filter(|&&(_, since)| now_us.saturating_sub(since) >= first_exec_hang_us())
                .min_by_key(|&&(_, since)| since)
        {
            self.said = true;
            eprintln!(
                "verify: {label}: macOS's program check has held `{}` (pid {}) at its first exec \
                 for {} {}; a held program is work in line, so verify waits on it, at most to its \
                 ceiling (--timeout); `aterm ctl @self status` says whether syspolicyd=stuck",
                self.name(m),
                m.pid,
                held_for(*since, now_us),
                under(daemon, now_us),
            );
        }
        None
    }

    /// Why its child decided nothing as its ceiling `limit` ends it, read afresh of its leader
    /// and of `quiet`'s members ([`Watch::stranded`]'s, `None` when unread): its leader never
    /// started (held at macOS's program check, translating under Rosetta, not started, or stopped
    /// or traced before its first instruction, [`AtStart::StoppedUnstarted`]), or a
    /// held member [`in_scope`] has been seen unrun ([`Watch::look`]) for
    /// [`aterm_uds::deadline::FIRST_EXEC_HANG`], past the bound on a first exec, the oldest named.
    /// `None` otherwise: a leader that started or reads unread, with no such member, ends at its
    /// ceiling as a finding, and so does one whose member is held within that bound, a first exec
    /// being work in line.
    pub(super) fn at_ceiling(
        &mut self,
        quiet: Option<Vec<(u32, u64)>>,
        limit: Duration,
    ) -> Option<Verdict> {
        let now_us = self.host.now_us();
        let mut members: Vec<(u32, Option<u64>)> = quiet
            .unwrap_or_default()
            .into_iter()
            .map(|(pid, cpu)| (pid, Some(cpu)))
            .collect();
        if !members.iter().any(|&(pid, _)| pid == self.pgid) {
            members.push((self.pgid, None));
        }
        let read = self.look(&members, now_us);
        let ceiling = aterm_capacity::span(limit);
        let unrun = "so nothing it would run was verified; verify ended it unrun, its file left \
                     in place";
        let may_run = "Its check may still be running: `aterm ctl @self status` says whether \
                       syspolicyd=stuck, which only a restart of the Mac clears, and a build that \
                       writes over this file before that check ends can crash syspolicyd";
        if let Some((leader, _)) = read.iter().find(|(m, _)| m.pid == self.pgid) {
            let (name, pid) = (&self.leader, leader.pid);
            let words = match probe::judge_start(&leader.facts) {
                AtStart::Started => None,
                AtStart::Held => Some(format!(
                    "macOS's program check held `{name}` (pid {pid}) at its first exec when its \
                     {ceiling} ceiling ended it, {}, {unrun}. {may_run}",
                    under(self.host.daemon(), now_us),
                )),
                AtStart::Translating => Some(format!(
                    "`{name}` (pid {pid}) was still translating (Rosetta) at its first exec when \
                     its {ceiling} ceiling ended it, {unrun}"
                )),
                AtStart::Unstarted => Some(format!(
                    "`{name}` (pid {pid}) had not started when its {ceiling} ceiling ended it, \
                     {unrun}"
                )),
                AtStart::StoppedUnstarted { stopped, traced } => Some(format!(
                    "`{name}` (pid {pid}) had not started when its {ceiling} ceiling ended it: its \
                     one thread never ran, and it read {}, {unrun}",
                    match (stopped, traced) {
                        (true, true) => "stopped and traced",
                        (true, false) => "stopped",
                        _ => "traced",
                    }
                )),
            };
            if let Some(words) = words {
                return Some(Verdict {
                    words,
                    leader: true,
                });
            }
        }
        let (m, since) = read
            .iter()
            .filter(|(m, since)| {
                m.pid != self.pgid
                    && probe::judge_start(&m.facts) == AtStart::Held
                    && in_scope(m.chain.as_deref())
                    && now_us.saturating_sub(*since) >= first_exec_hang_us()
            })
            .min_by_key(|&&(_, since)| since)?;
        Some(Verdict {
            words: format!(
                "macOS's program check held `{}` (pid {}), under `{}`, at its first exec for {} \
                 when its {ceiling} ceiling ended it, {}, past the {} a first exec may take, so \
                 the run decided nothing about the tree; verify ended it unrun, its file left in \
                 place. {may_run}",
                self.name(m),
                m.pid,
                self.leader,
                held_for(*since, now_us),
                under(self.host.daemon(), now_us),
                aterm_capacity::span(aterm_uds::deadline::FIRST_EXEC_HANG),
            ),
            leader: false,
        })
    }

    /// Why `m`, seen unrun since `since` and stranded under `d`, decided nothing.
    fn stranded_words(&self, m: &Unstarted, since: u64, d: Daemon, now_us: u64) -> String {
        let up = Duration::from_micros(now_us.saturating_sub(d.started_us()));
        format!(
            "macOS's program check stranded `{}` (pid {}) at its first exec: held, as verify saw \
             it, since before the syspolicyd running now (pid {}) started {} ago, so its check \
             went to a syspolicyd that is no longer running, or has waited for the one running \
             now past the {} a first exec may take; held {}. verify ended it unrun (SIGKILL), its \
             file left in place, and nothing it would run was verified; the syspolicyd running \
             now checks a new run's programs",
            self.name(m),
            m.pid,
            d.pid,
            aterm_capacity::span(up),
            aterm_capacity::span(aterm_uds::deadline::FIRST_EXEC_HANG),
            held_for(since, now_us),
        )
    }

    /// `m`'s name: the leader's program, else its own executable's basename.
    fn name(&self, m: &Unstarted) -> String {
        if m.pid == self.pgid {
            self.leader.clone()
        } else {
            m.exe
                .as_deref()
                .map_or_else(|| "a member of its tree".to_string(), basename)
        }
    }
}

/// How long a member seen unrun since `since` has been, at `now_us`.
fn held_for(since: u64, now_us: u64) -> String {
    aterm_capacity::span(Duration::from_micros(now_us.saturating_sub(since)))
}

/// `path`'s last component, or all of it.
fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Which syspolicyd a held program waits under, read at `now_us`.
fn under(daemon: DaemonNow, now_us: u64) -> String {
    match daemon {
        DaemonNow::Running(d) => format!(
            "under the syspolicyd running now (pid {}, up {})",
            d.pid,
            aterm_capacity::span(Duration::from_micros(now_us.saturating_sub(d.started_us())))
        ),
        DaemonNow::NoneRunning => "with no syspolicyd running now".to_string(),
        DaemonNow::Unread => "under a syspolicyd verify could not read".to_string(),
    }
}
