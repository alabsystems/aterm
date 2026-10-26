// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reap credit (SA §7.5): a tab's CPU in the processes that finished between two readings.
//!
//! A sweep reads only the processes alive when it runs, and a build's processes mostly start
//! and finish between two sweeps. The kernel keeps their work: at each reap it adds the child's
//! own CPU and its reaped children's to its parent's reaped-children time, once where the parent
//! waits for it (measured on macOS 27: the parent's growth equals the zombie's final figures to
//! the nanosecond) and twice where the kernel reaps it for the parent, one that ignores `SIGCHLD`
//! or set `SA_NOCLDWAIT` (`P_NOCLDWAIT`; measured: 400.4–401.6 ms for a 200 ms child). So a tab's
//! work is its members' own CPU growth plus the growth of their reaped-children time, where the
//! members are this ledger's, not one reading's parent chain: a process takes the tab each
//! reading puts it in (a pane shared into two tabs counts where the host lists it); first seen in
//! none, its parent's row's scope, else aterm's own where the reading puts it in aterm, else, for
//! one whose parent died, the tab whose session it is in (every process in a session descends from
//! its leader, the process its pane started); and it keeps its last tab after its parent dies and
//! it moves to launchd. A tab keeps its counter while it is open, whichever panes it holds: its id
//! follows the host's identity for the tab ([`TabRoot::tab_id`]), though its key (its least
//! pane's) moves when that pane closes. A process whose usage goes unread (another user's, such
//! as root's `sudo`) gets no row.
//!
//! A member that was itself read has had its work counted from its own rows; when it goes, that
//! much is pending against its nearest member ancestor still alive (through processes with no
//! row), whose reaped-children time will hold it again, and that growth is absorbed rather than
//! counted twice. A parent still alive reaped it, so that is certain and waits however long the
//! parent's reaped time takes to show it; a parent with no row (another user's `sudo`) passes it
//! up only when it goes itself, so it is held there until then and its own parent's reaped time
//! absorbs nothing of it before. A parent gone too, or a zombie, reaped it only if it outlived it
//! (a build killed with its compilers leaves them to launchd), and its reaper's reaped time then
//! holds both already, or by its next reading where it reaped while the sweep ran; so that is
//! uncertain: it waits through the reaper's next reading and is then dropped. A reaper whose
//! reaped time did not move though a child it stayed the parent of went certainly reaped it after
//! its reading, so the uncertain work waits for its next reading as at once. What a member held
//! uncertain stays uncertain when it goes, and waits as long at its reaper. Uncertain work left
//! after a reading, or taken out at a later one, says launchd may have reaped it, so what was
//! taken out against it may be the reaper's other children's: the ledger marks each such step as
//! one that missed some of the tab's work. So a killed build takes out at most a reading or two
//! of its reaper's reaped growth, and the tab's figure over them is a lower bound, save where that
//! growth covered all of it at the first reading. Reaped growth past what is absorbed counts,
//! save where it may hold work twice: a process the kernel reaps for, at its reading or the one
//! before, and the reaper of a gone one (or of a member gone under one) over its next two
//! readings count none of it, so the tab's figure there is a lower bound.
//!
//! A process listed but unread (mid-exit, or another user's after a setuid exec, which keeps its
//! birth) keeps its row, unread; one listed under another birth is a new process at the pid. One
//! last read under launchd leaves nothing pending, and its tail since that reading is lost; one
//! first seen as a zombie or read as one is counted at its final figure, which later readings leave
//! as it is. A member counts from its birth only where it was born since the last step (by its
//! start, unix ms, against the last reading's wall clock, read before its sweep listed the
//! processes; where the host read it no birth, a short job gone before it could, by its kernel
//! start against the newest the last sweep read); one older, such as a tab's shell adopted from a
//! predecessor, counts from the step that first reads it. A delegated job's member counts there
//! while the job holds it, from where this ledger's counter left it ([`ReapLedger::credited`]);
//! after, one of aterm's processes counts here again, and one outside them stays out, its work its
//! own coalition's.
//!
//! Each step that knowingly misses some of a tab's work marks it ([`TabTotal::missed`]): a tail
//! launchd reaped or a live process with no row holds, uncertain work as above, reaped growth
//! that may hold work twice, what a process older than the last step ran before its first
//! reading, a member of aterm's back from a delegated job (whose work since the job's last
//! reading no figure read), and a job's member first read, gone, or reaping children no reading
//! counted (the job's figure reads its members' own CPU from a window's start to its end). What
//! no sweep read and no member reaped marks nothing: a build server's compilers, and an orphan
//! born and reaped by launchd between two sweeps, are in no member's reaped time, and no exact
//! signal shows them (a coalition's task counters count each fork, exec and exit, whoever reaps
//! it); aterm's coalition holds what its counters kept of them, as the split's unattributed work,
//! and the rest is unaccounted (a probe's launchd job kept 958 of 1,200 ms of its orphans' work
//! on macOS 27; over a live loop of 6 ms orphans, 819 mc, the split's windows read 824–1,035 mc
//! unaccounted), so a tab's figure over such a window can read exact and be short. A tab's
//! counter only grows, and over any window it grows at least as much as the own CPU of the
//! members it read at both ends that no job held between and that stayed in it (the live-only
//! figure), since what is absorbed comes out of reaped growth alone.
//! `aterm_models_misc::reap_credit_model` is the model; `tests/conformance_reap_credit.rs` binds
//! it.

use super::{IdMap, IdSet};
use crate::model::{Membership, ProcSample, ProcState, TabRoot};

/// The longest parent chain the ledger walks.
const MAX_HOPS: usize = 64;

/// Where a member's work counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// A tab, by the ledger's id for it ([`TabTotal::id`]), which stays while any of the tab's
    /// panes does, though its key ([`TabRoot::tab`], its least pane's) moves when that pane closes.
    Tab(u64),
    /// aterm's processes in no tab: itself, its helpers, and what closed tabs left behind.
    Own,
}

/// One member, as the ledger last read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    /// Its kernel start: with the pid, its identity (a pid is reused).
    start: u64,
    /// Its start, unix ms, where read: what tells a pid reused by another user's process, whose
    /// kernel start goes unread, from a setuid exec, which keeps it.
    birth: Option<i64>,
    /// Where its work counts.
    scope: Scope,
    /// Its own CPU at its last reading, ns.
    cpu: u64,
    /// Its reaped children's CPU at its last reading, ns.
    child: u64,
    /// Its own CPU as far as its counter here took it, ns: its last reading's while its work
    /// counts here, held from the reading before a delegated job took it over; `None` for one a
    /// job held from its first reading ([`ReapLedger::credited`]).
    credited: Option<u64>,
    /// Its nearest member ancestor at its last reading, by pid and start (its parent, or the
    /// nearest above processes with no row); `None` under launchd.
    parent: Option<(u32, u64)>,
    /// Its parent's pid at its last reading, a member or not.
    ppid: u32,
    /// What its reaped-children time has yet to show of members gone while their parent stayed
    /// alive, which that parent certainly reaped, however late this one's reaped time shows it:
    /// their last-read figures, work counted save reaped growth passed over as doubled.
    pending: u64,
    /// The same, uncertain, left by step [`Self::soon_step`]: from members gone with their parent
    /// or under a zombie one, which reaped one only if it outlived it, so its reaper's reaped time
    /// holds it by its next reading after this or never; and what a member gone under this one
    /// held uncertain.
    soon: u64,
    /// Such work left by an earlier step: dropped after this row's next reading.
    late: u64,
    /// The step [`Self::soon`] came by.
    soon_step: u64,
    /// Whether its last reading was of a zombie: its final figures.
    zombie: bool,
    /// Whether a delegated job holds it, or held it and it is outside aterm's processes: its
    /// work counts there (or in its own coalition), so its readings count nothing here and only
    /// say what its reaper will absorb.
    delegated: bool,
    /// Whether a delegated job held it at its last reading: the job's figure reads only its
    /// members alive at both ends of a window, so its work since then is in no figure of the tab.
    job: bool,
    /// The step its work counts from: `None` from its birth (born since the step before it was
    /// first read).
    from: Option<u64>,
    /// The last step that read its usage.
    read: u64,
    /// Whether the kernel reaped its children for it at its last reading
    /// ([`ProcSample::reaps_unwaited`]), adding each one's time to its reaped time twice.
    unwaited: bool,
    /// Whether the kernel reaped its parent's children for it at its last reading: a parent with
    /// no row that reaps it then passes its time up twice.
    under_unwaited: bool,
    /// The last step through which its reaped time's growth past what it absorbs counts nothing:
    /// a member gone under it whose reaped time may hold some children's work twice passes it up
    /// with its reap, which its reaped time shows at that step or the next.
    distrust: u64,
    /// The last step at which a member gone while it stayed that member's parent left it certain
    /// work, which its reaped time shows at that step's reading unless the reap came after it.
    fresh: u64,
}

impl Row {
    /// Moves what an earlier step left in [`Self::soon`] to [`Self::late`].
    fn age(&mut self, step: u64) {
        if self.soon_step != step {
            self.late = self.late.saturating_add(self.soon);
            self.soon = 0;
            self.soon_step = step;
        }
    }

    /// What its reaped-children time may yet show, uncertain.
    const fn doubt(&self) -> u64 {
        self.soon.saturating_add(self.late)
    }
}

/// One member's row, as a test reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowView {
    /// Where its work counts.
    pub scope: Scope,
    /// Its own CPU at its last reading, ns.
    pub cpu_ns: u64,
    /// Its reaped children's CPU at its last reading, ns.
    pub child_ns: u64,
    /// Its kernel start.
    pub start: u64,
    /// Its nearest member ancestor's pid at its last reading; `None` under launchd.
    pub parent: Option<u32>,
    /// Work counted that its reaped-children time has yet to show, certain, ns.
    pub pending_ns: u64,
    /// The same, uncertain (left by members gone with their parent), ns.
    pub doubt_ns: u64,
    /// Whether its last reading was of a zombie.
    pub zombie: bool,
}

/// One open tab's counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counter {
    /// Its members' work, ns.
    ns: u64,
    /// The last step that knowingly missed some of its work; 0 for none.
    missed: u64,
}

/// One open tab's counter at one reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabTotal {
    /// The tab's key ([`TabRoot::tab`]).
    pub key: u64,
    /// The ledger's id for it ([`Scope::Tab`]).
    pub id: u64,
    /// Its counter, ns.
    pub ns: u64,
    /// The last step that knowingly missed some of its work ([module docs](self)); 0 for none.
    pub missed: u64,
}

/// The ledger's counters at one reading: where a window starts or ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReapTotals {
    /// The step that read them.
    pub step: u64,
    /// Every scope's counter summed, ns: a closed tab's folds into aterm's own, so this sum keeps
    /// every tab's work, closed or open.
    pub all: u64,
    /// Each open tab's counter, by key.
    pub tabs: Vec<TabTotal>,
}

impl ReapTotals {
    /// Tab `key`'s counter, ns; 0 for a tab it holds none of.
    #[must_use]
    pub fn tab(&self, key: u64) -> u64 {
        self.tabs.iter().find(|t| t.key == key).map_or(0, |t| t.ns)
    }

    /// Tab `key`'s growth from `base` to these counters, ns, and whether a step between them
    /// knowingly missed some of its work: by the id it has here, so a pane closed between the two,
    /// which moves its key, moves nothing; from 0 for a tab opened since.
    #[must_use]
    pub fn since(&self, base: &Self, key: u64) -> (u64, bool) {
        let Some(t) = self.tabs.iter().find(|t| t.key == key) else {
            return (0, false);
        };
        let before = base.tabs.iter().find(|b| b.id == t.id).map_or(0, |b| b.ns);
        (t.ns.saturating_sub(before), t.missed > base.step)
    }
}

/// What one step reads: one whole sweep.
#[derive(Clone, Copy, Debug)]
pub struct StepIn<'a> {
    /// The reading's stamp, ns on the host's monotonic clock.
    pub at_ns: u64,
    /// The reading's wall clock, unix ms, taken before its sweep listed the processes: a process
    /// whose start ([`ProcSample::birth_ms`]) is at or after the last step's was not in its
    /// sweep, so was born since.
    pub at_unix_ms: i64,
    /// The sweep.
    pub procs: &'a [ProcSample],
    /// Each process's scopes by this reading's parent chains, in [`Self::procs`]' order.
    pub members: &'a [Membership],
    /// Whether each process is a delegated job's member, in the same order.
    pub delegated: &'a [bool],
    /// Every tab, in the host's order.
    pub tabs: &'a [TabRoot],
    /// This aterm's user: only its processes' usage is read.
    pub self_uid: u32,
    /// Whether the sweep stopped at its cap, so a member missing from it may be alive.
    pub truncated: bool,
}

/// A defect a negative control injects into the step (`tests/conformance_reap_credit.rs`).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReapFault {
    /// Each counter is its scope's members' `cpu + child` now: the naive Δ(live + reaped), which
    /// loses a member's work when launchd reaps it.
    Naive,
    /// A zombie's reading keeps its row's last live figures, not its final ones.
    ForgetZombieRead,
    /// A member's parent stays the one it was first seen under, and what a gone member leaves
    /// waits there however long, so a member launchd reaped leaves its work pending against it.
    KeepFirstParent,
    /// What a member gone with its parent left waits like the rest, however long, so work
    /// launchd reaped is taken out of its reaper's later children.
    HoldEveryPending,
    /// Nothing a gone member left is absorbed, so a member its parent reaped counts twice: once
    /// from its own rows, again in its parent's reaped time.
    AbsorbNothing,
}

/// Work counted of members gone under a live process whose usage goes unread (another user's,
/// such as root's `sudo`), which reaped them and will pass their time to its own parent only when
/// it goes: held here until then, so no member's reaped time absorbs it before it can show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    /// The holder's start, unix ms, where read: what tells it from a later process at its pid.
    birth: Option<i64>,
    /// Its parent's pid at its last listing.
    ppid: u32,
    /// Its nearest member ancestor at its last listing; `None` under launchd.
    parent: Option<(u32, u64)>,
    /// Whose work it holds.
    scope: Scope,
    /// Work counted that it reaped.
    pending: u64,
    /// The same, uncertain: what the members gone under it held uncertain.
    doubt: u64,
    /// Whether its reaped time may hold some of that twice: the kernel reaped its children for it
    /// (or for a member under it) while it held work.
    doubled: bool,
}

/// The tabs' and aterm's cumulative CPU, live and reaped ([module docs](self)).
#[derive(Debug, Default)]
pub struct ReapLedger {
    /// Every member alive at the last step, by pid.
    rows: IdMap<u32, Row>,
    /// Each open tab's counter, by its id.
    tabs: IdMap<u64, Counter>,
    /// Each open tab's key and id, in the last step's tabs' order.
    keys: Vec<(u64, u64)>,
    /// Each open pane's tab's id, by the pane's key ([`TabRoot::key`]): what keeps the id of a
    /// tab the host names no identity for when the pane whose key it took closes.
    panes: IdMap<u64, u64>,
    /// Each open tab's id, by the host's identity for it ([`TabRoot::tab_id`]): what keeps a
    /// tab's id while it lasts, whichever panes it holds.
    hosts: IdMap<u64, u64>,
    /// Work held by live processes that have no row, by pid.
    held: IdMap<u32, Held>,
    /// The last tab id given.
    next_id: u64,
    /// aterm's own counter (and closed tabs'), ns.
    own: u64,
    /// The steps taken.
    step: u64,
    /// The stamp of the reading it last stepped on.
    at_ns: Option<u64>,
    /// That reading's wall clock, unix ms.
    at_unix_ms: Option<i64>,
    /// The newest kernel start that reading's sweep read.
    newest: Option<u64>,
    /// Scratch: this step's swept pids, to their index.
    here: IdMap<u32, usize>,
    /// Scratch: each first-seen process's scope.
    memo: IdMap<u32, Option<Scope>>,
    /// Scratch: each of this step's tabs' id, in its tabs' order.
    tab_ids: Vec<u64>,
    /// The defect injected into each step (negative controls only).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fault: Option<ReapFault>,
}

impl ReapLedger {
    /// Whether the last step read the reading stamped `at_ns`.
    #[must_use]
    pub fn stepped_at(&self, at_ns: u64) -> bool {
        self.at_ns == Some(at_ns)
    }

    /// The counters after the last step.
    #[must_use]
    pub fn totals(&self) -> ReapTotals {
        let mut tabs: Vec<TabTotal> = self
            .keys
            .iter()
            .map(|&(key, id)| {
                let c = self.tabs.get(&id).copied().unwrap_or_default();
                TabTotal {
                    key,
                    id,
                    ns: c.ns,
                    missed: c.missed,
                }
            })
            .collect();
        tabs.sort_unstable_by_key(|t| t.key);
        ReapTotals {
            step: self.step,
            all: self
                .tabs
                .values()
                .fold(self.own, |sum, c| sum.saturating_add(c.ns)),
            tabs,
        }
    }

    /// Whether process `pid` started at `start` is a member whose work from step `since` to the
    /// last step is all counted: read at or before `since` (or born since the step before it was
    /// first read), and read at the last step.
    #[must_use]
    pub fn counted(&self, pid: u32, start: Option<u64>, since: u64) -> bool {
        self.rows.get(&pid).is_some_and(|r| {
            Some(r.start) == start
                && !r.delegated
                && r.from.is_none_or(|f| f <= since)
                && r.read == self.step
        })
    }

    /// The own CPU of process `pid` started at `start` as far as a counter here took it, ns: for a
    /// delegated job's member, where the split's live figure for it takes over, so a member a
    /// job took over since a window began counts once. `None` for no member, or one a job held
    /// from its first reading.
    #[must_use]
    pub fn credited(&self, pid: u32, start: Option<u64>) -> Option<u64> {
        self.rows
            .get(&pid)
            .filter(|r| Some(r.start) == start)
            .and_then(|r| r.credited)
    }

    /// The same ledger with `fault` injected into each step (negative controls only).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_fault(mut self, fault: ReapFault) -> Self {
        self.fault = Some(fault);
        self
    }

    /// Member `pid`'s row, as the last step left it.
    #[must_use]
    pub fn row(&self, pid: u32) -> Option<RowView> {
        self.rows.get(&pid).map(|r| RowView {
            start: r.start,
            scope: r.scope,
            cpu_ns: r.cpu,
            child_ns: r.child,
            parent: r.parent.map(|(p, _)| p),
            pending_ns: r.pending,
            doubt_ns: r.doubt(),
            zombie: r.zombie,
        })
    }

    /// Gives each of `tabs` its tab's id: the one the host's identity for the tab
    /// ([`TabRoot::tab_id`]) had at the last step; else, unclaimed, the one a pane of it was in
    /// at the last step, where that tab or this one has no such identity (a session in no tab
    /// hands its id to the tab that takes it in, never one tab's to another, such as the tab a
    /// pane is shared into); else a new one.
    fn name_tabs(&mut self, tabs: &[TabRoot]) {
        self.keys.clear();
        self.tab_ids.clear();
        let hosted: IdSet<u64> = self.hosts.values().copied().collect();
        // A tab is its key's first root: a split tab's panes share its key and its identity.
        let firsts = || {
            tabs.iter()
                .enumerate()
                .filter(|(k, t)| tabs[..*k].iter().all(|o| o.tab != t.tab))
                .map(|(_, t)| t)
        };
        for t in firsts() {
            let kept = t.tab_id.and_then(|h| self.hosts.get(&h).copied());
            if let Some(id) = kept.filter(|id| self.keys.iter().all(|(_, o)| o != id)) {
                self.keys.push((t.tab, id));
            }
        }
        for t in firsts() {
            if self.keys.iter().any(|(k, _)| *k == t.tab) {
                continue;
            }
            let kept = tabs
                .iter()
                .filter(|o| o.tab == t.tab)
                .filter_map(|o| self.panes.get(&o.key).copied())
                .find(|id| {
                    (t.tab_id.is_none() || !hosted.contains(id))
                        && self.keys.iter().all(|(_, o)| o != id)
                });
            let id = kept.unwrap_or_else(|| {
                self.next_id += 1;
                self.next_id
            });
            self.keys.push((t.tab, id));
        }
        self.panes.clear();
        self.hosts.clear();
        for t in tabs {
            let id = self
                .keys
                .iter()
                .find(|(k, _)| *k == t.tab)
                .map_or(0, |(_, id)| *id);
            self.tab_ids.push(id);
            self.panes.insert(t.key, id);
            if let Some(h) = t.tab_id {
                self.hosts.entry(h).or_insert(id);
            }
        }
    }

    /// Reads one whole sweep. A cut one is skipped: a member it missed may be alive.
    pub fn step(&mut self, i: &StepIn) {
        if i.truncated {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        let fault = self.fault;
        #[cfg(not(any(test, debug_assertions)))]
        let fault: Option<()> = None;
        self.step += 1;
        self.at_ns = Some(i.at_ns);
        let since_ms = self.at_unix_ms.replace(i.at_unix_ms);
        let since_start = std::mem::replace(
            &mut self.newest,
            i.procs.iter().filter_map(|p| p.start).max(),
        );
        self.name_tabs(i.tabs);
        let Self {
            rows,
            tabs,
            keys,
            own,
            step,
            here,
            memo,
            tab_ids,
            held,
            ..
        } = self;
        let step = *step;
        let open: IdSet<u64> = keys.iter().map(|(_, id)| *id).collect();
        // A closed tab's counter folds into aterm's own, and its members' later work counts
        // there ("left behind by closed tabs", SA §7.4), save where a reading puts one in an
        // open tab (a pane shared into another).
        tabs.retain(|id, c| {
            let keep = open.contains(id);
            if !keep {
                *own = own.saturating_add(c.ns);
            }
            keep
        });
        here.clear();
        for (k, p) in i.procs.iter().enumerate() {
            here.insert(p.pid, k);
        }
        // Listed as the same process: under its start, or, where that goes unread (mid-exit, or
        // another user's after a setuid exec, which keeps both), not under another birth.
        let listed = |pid: u32, r: &Row| {
            here.get(&pid)
                .map(|&k| &i.procs[k])
                .filter(|p| match p.start {
                    Some(s) => s == r.start,
                    None => r.birth.zip(p.birth_ms).is_none_or(|(a, b)| a == b),
                })
        };
        let gone_now = |pid: u32, r: &Row| listed(pid, r).is_none();
        let alive = |p: u32, q: &Row| listed(p, q).is_some_and(|x| x.state != ProcState::Zombie);
        let alive_at = |pid: u32| {
            here.get(&pid)
                .is_some_and(|&k| i.procs[k].state != ProcState::Zombie)
        };
        let unwaited_at = |pid: u32| {
            here.get(&pid)
                .is_some_and(|&k| i.procs[k].reaps_unwaited == Some(true))
        };
        let countable = |k: usize| {
            let p = &i.procs[k];
            p.start.is_some() && p.uid == Some(i.self_uid) && p.cpu_ns.is_some()
        };

        // Processes with no row gone since the last step that held members' work: each passes it
        // to its parent with its own reap, as a gone member does (below): to a member that is its
        // parent and alive, certain; to another live process with no row, held again; else
        // uncertain, at its nearest member ancestor still listed, or, with none, lost.
        let freed: Vec<(u32, Held)> = held
            .iter()
            .filter(|(pid, h)| {
                here.get(pid)
                    .map(|&k| &i.procs[k])
                    .is_none_or(|p| h.birth.zip(p.birth_ms).is_some_and(|(a, b)| a != b))
            })
            .map(|(pid, h)| (*pid, *h))
            .collect();
        for (pid, _) in &freed {
            held.remove(pid);
        }
        for (_, h) in freed {
            let parent = h.parent.filter(|(p, start)| {
                *p == h.ppid
                    && rows
                        .get(p)
                        .filter(|q| q.start == *start)
                        .is_some_and(|q| alive(*p, q))
            });
            if let Some(q) = parent.and_then(|(p, _)| rows.get_mut(&p)) {
                q.age(step);
                q.pending = q.pending.saturating_add(h.pending);
                q.soon = q.soon.saturating_add(h.doubt);
                if h.pending > 0 {
                    q.fresh = step;
                }
                if h.doubled {
                    q.distrust = q.distrust.max(step + 1);
                }
                if h.doubt > 0 && !q.delegated {
                    miss(tabs, q.scope, step);
                }
                continue;
            }
            if h.ppid > 1 && alive_at(h.ppid) && !rows.contains_key(&h.ppid) && h.parent.is_some() {
                let next = &i.procs[here[&h.ppid]];
                let e = held.entry(h.ppid).or_insert(Held {
                    birth: next.birth_ms,
                    ppid: next.ppid,
                    parent: h.parent,
                    scope: h.scope,
                    pending: 0,
                    doubt: 0,
                    doubled: false,
                });
                e.pending = e.pending.saturating_add(h.pending);
                e.doubt = e.doubt.saturating_add(h.doubt);
                e.doubled |= h.doubled || next.reaps_unwaited == Some(true);
                continue;
            }
            match reaper(rows, h.parent, gone_now).and_then(|to| rows.get_mut(&to)) {
                Some(q) => {
                    q.age(step);
                    q.soon = q.soon.saturating_add(h.pending).saturating_add(h.doubt);
                    if h.doubled {
                        q.distrust = q.distrust.max(step + 1);
                    }
                    if h.doubt > 0 && !q.delegated {
                        miss(tabs, q.scope, step);
                    }
                }
                None => miss(tabs, h.scope, step),
            }
        }

        // Members gone since the last step: what was counted of each is owed by its nearest live
        // member ancestor, whose reaped-children time holds it once that reaped it. A parent
        // still there and alive reaped it, so that is certain; one that has no row (another
        // user's `sudo`) passes it up only when it goes itself, so it holds it until then. A
        // parent gone too, or a zombie, reaped it only if it outlived it (a build's compilers
        // killed with it went to launchd), and then its reaper's reaped time holds both already,
        // so that is uncertain and waits one reading more and no longer, as what the gone member
        // itself held uncertain does. Where no member reaped it, its tail since its last reading
        // is lost. One whose reaped time may hold some work twice (the kernel reaped its children
        // for it, or for one under it) passes that up: its reaper's next two readings count
        // nothing of their reaped growth past what they absorb.
        let gone: Vec<u32> = rows
            .iter()
            .filter(|(pid, r)| gone_now(**pid, r))
            .map(|(pid, _)| *pid)
            .collect();
        for &pid in &gone {
            let r = rows[&pid];
            let certain = r.cpu.saturating_add(r.child).saturating_add(r.pending);
            let direct = r.parent.is_some_and(|(p, _)| p == r.ppid);
            let stayed = r.ppid > 1
                && match r.parent {
                    Some((p, start)) if p == r.ppid => rows
                        .get(&p)
                        .filter(|q| q.start == start)
                        .is_some_and(|q| alive(p, q)),
                    _ => alive_at(r.ppid),
                };
            let doubled = r.unwaited || r.under_unwaited || r.distrust > r.read;
            // A job's member gone: the job's figure, live members' alone, read none of its work
            // since the window began.
            if r.job {
                miss(tabs, r.scope, step);
            }
            let hold =
                fault_is(fault, Fault::HoldEveryPending) || fault_is(fault, Fault::KeepFirstParent);
            if stayed && !direct && !hold && reaper(rows, r.parent, gone_now).is_some() {
                let holder = &i.procs[here[&r.ppid]];
                let e = held.entry(r.ppid).or_insert(Held {
                    birth: holder.birth_ms,
                    ppid: holder.ppid,
                    parent: r.parent,
                    scope: r.scope,
                    pending: 0,
                    doubt: 0,
                    doubled: false,
                });
                e.pending = e.pending.saturating_add(certain);
                e.doubt = e.doubt.saturating_add(r.doubt());
                e.doubled |= doubled || r.under_unwaited || holder.reaps_unwaited == Some(true);
                // Its tail since its last reading is in the holder's reaped time, in no figure
                // until the holder goes; a zombie's last reading was final.
                if !r.delegated && !r.zombie {
                    miss(tabs, r.scope, step);
                }
                continue;
            }
            match reaper(rows, r.parent, gone_now) {
                Some(to) => {
                    if let Some(p) = rows.get_mut(&to) {
                        p.age(step);
                        if stayed || hold {
                            p.pending = p.pending.saturating_add(certain);
                            if stayed && to == r.ppid && certain > 0 {
                                p.fresh = step;
                            }
                        } else {
                            p.soon = p.soon.saturating_add(certain);
                        }
                        if hold {
                            p.pending = p.pending.saturating_add(r.doubt());
                        } else {
                            p.soon = p.soon.saturating_add(r.doubt());
                        }
                        if doubled {
                            p.distrust = p.distrust.max(step + 1);
                        }
                        // Work a reading already found its reaper's reaped time short of: what
                        // is taken out against it here may be the reaper's other children's.
                        if r.doubt() > 0 && !p.delegated {
                            miss(tabs, p.scope, step);
                        }
                    }
                }
                None if !r.delegated && (!r.zombie || r.doubt() > 0) => {
                    miss(tabs, r.scope, step);
                }
                None => {}
            }
        }
        for pid in &gone {
            rows.remove(pid);
        }

        let tab_of = |k: usize| {
            i.members
                .get(k)
                .and_then(|m| m.tab)
                .and_then(|t| tab_ids.get(t))
                .map(|id| Scope::Tab(*id))
        };

        // Members read again: their growth, less what their pending absorbs.
        let mut fresh: Vec<usize> = Vec::new();
        for (&pid, &k) in here.iter() {
            let p = &i.procs[k];
            let Some(row) = rows.get_mut(&pid) else {
                if countable(k) {
                    fresh.push(k);
                }
                continue;
            };
            if !countable(k) {
                continue;
            }
            let forget = fault_is(fault, Fault::Forget) && p.state == ProcState::Zombie;
            let (cpu, child) = if forget {
                (row.cpu, row.child)
            } else {
                (
                    p.cpu_ns.unwrap_or(row.cpu),
                    p.child_cpu_ns.unwrap_or(row.child),
                )
            };
            row.age(step);
            // What its own children left goes first (it reaped them), then what members gone with
            // their parent left this step, then an earlier step's, which this reading not
            // showing means launchd reaped it. Uncertain work its reaped time falls short of says
            // launchd may have, so what a reading takes out against it may be its other
            // children's: this one where any is left, and the next, which drops the rest.
            let suspect = row.late > 0;
            let grew = child.saturating_sub(row.child);
            let mut left = grew;
            if !fault_is(fault, Fault::AbsorbNothing) {
                for slot in [&mut row.pending, &mut row.soon, &mut row.late] {
                    let absorbed = (*slot).min(left);
                    *slot -= absorbed;
                    left -= absorbed;
                }
            }
            let doubt = suspect || row.soon > 0;
            // A child it stayed the parent of went this step, but its reaped time did not move:
            // it reaped that child while the sweep ran, after this reading, so its next reading is
            // the first that can show what the child passed up, and takes it out as at once.
            if p.child_cpu_ns.is_some() && grew == 0 && row.fresh == step && row.soon > 0 {
                row.soon_step = step + 1;
            }
            row.late = 0;
            // Reaped growth past what it absorbs is work of children no reading counted. Where the
            // kernel reaped its children for it (at either reading, or for a member gone under it
            // since), that growth may hold some of their work twice, so none of it counts, and the
            // step missed what it held once.
            let unseen = left > 0;
            let unwaited = p.reaps_unwaited == Some(true);
            let doubled = unwaited || row.unwaited || row.distrust >= step;
            if doubled {
                left = 0;
            }
            let work = cpu.saturating_sub(row.cpu).saturating_add(left);
            row.cpu = cpu;
            row.child = child;
            row.read = step;
            row.zombie = p.state == ProcState::Zombie;
            row.birth = row.birth.or(p.birth_ms);
            row.unwaited = unwaited;
            row.under_unwaited = unwaited_at(p.ppid);
            // A delegated job's member counts there while the job holds it; after, one of aterm's
            // processes counts here again (the split read none of its work since the job's last
            // reading), one outside them stays out, its work its own coalition's. The job's
            // figure is its members' own CPU, so what one reaped of children no reading counted
            // is in no figure of the tab.
            let held = i.delegated.get(k).copied().unwrap_or(false);
            let back = row.delegated && !held && i.members.get(k).is_some_and(|m| m.aterm);
            row.delegated = held || (row.delegated && !back);
            row.job = held;
            // It counts in the tab this reading puts it in (a pane shared into another tab counts
            // where the host lists it); one in no tab's chain (its parent died and launchd took
            // it) keeps the tab it had while that is open, and then counts as aterm's own.
            match tab_of(k) {
                Some(tab) => row.scope = tab,
                None if matches!(row.scope, Scope::Tab(id) if !open.contains(&id)) => {
                    row.scope = Scope::Own;
                }
                None => {}
            }
            if !row.delegated {
                credit(tabs, own, row.scope, work);
                row.credited = Some(cpu);
            }
            let short = if row.delegated {
                held && unseen
            } else {
                doubt || (doubled && unseen)
            };
            if short || back {
                miss(tabs, row.scope, step);
            }
        }

        // Each session an open tab's process is in, by its id: every process in a session
        // descends from its leader, the tab's process that its pane started, so only that tab's
        // processes can be in it. One that two scopes' processes are in (aterm's own among them)
        // names neither. A delegated job's member is in neither count: its work counts there.
        let mut sessions: IdMap<u32, Option<u64>> = IdMap::default();
        if !fresh.is_empty() {
            for (k, p) in i.procs.iter().enumerate() {
                if p.sid == 0 || i.delegated.get(k).copied().unwrap_or(false) {
                    continue;
                }
                let scope = tab_of(k).or_else(|| {
                    rows.get(&p.pid)
                        .filter(|r| Some(r.start) == p.start)
                        .map(|r| r.scope)
                        .or_else(|| {
                            i.members
                                .get(k)
                                .is_some_and(|m| m.aterm)
                                .then_some(Scope::Own)
                        })
                });
                let tab = match scope {
                    Some(Scope::Tab(id)) if open.contains(&id) => Some(id),
                    Some(_) => None,
                    None => continue,
                };
                sessions
                    .entry(p.sid)
                    .and_modify(|t| {
                        if *t != tab {
                            *t = None;
                        }
                    })
                    .or_insert(tab);
            }
        }

        // Processes seen for the first time: a member takes the reading's tab, else its
        // parent's row's scope, else aterm's own where the reading puts it in aterm, else, for
        // one whose chain reaches launchd (it outlived its parent), the tab whose session it is
        // in; the walk up passes processes whose usage is not read (another user's `sudo`),
        // which get no row. It counts from its birth where it was born since the last step, so
        // nothing counted it before, else from now: a tab's shell adopted from a predecessor, a
        // process the last sweep could not read, and every process at the first step; after the
        // first, what such a process ran since the last step is missed. Born since is by its
        // birth against the last reading's wall clock, or, where the host read it no birth (a
        // short job gone before it could), by its kernel start against the newest the last sweep
        // read: one alive then was listed then, so one newer than every start read then was born
        // since, or was listed then with its own start unread.
        memo.clear();
        let mut first: Vec<u32> = Vec::new();
        for &k0 in &fresh {
            if memo.contains_key(&i.procs[k0].pid) {
                continue;
            }
            let mut chain = vec![k0];
            let mut above: Option<Scope> = loop {
                let p = &i.procs[*chain.last().unwrap_or(&k0)];
                if let Some(m) = memo.get(&p.ppid) {
                    break *m;
                }
                if let Some(r) = rows.get(&p.ppid) {
                    break Some(r.scope);
                }
                match here.get(&p.ppid) {
                    Some(&kp)
                        if p.ppid > 1
                            && p.ppid != p.pid
                            && chain.len() < MAX_HOPS
                            && !chain.contains(&kp) =>
                    {
                        chain.push(kp);
                    }
                    _ => break None,
                }
            };
            for &k in chain.iter().rev() {
                let p = &i.procs[k];
                let scope = tab_of(k)
                    .or(above)
                    .or_else(|| {
                        i.members
                            .get(k)
                            .is_some_and(|m| m.aterm)
                            .then_some(Scope::Own)
                    })
                    .or_else(|| {
                        sessions
                            .get(&p.sid)
                            .copied()
                            .flatten()
                            .filter(|_| p.sid != 0)
                            .map(Scope::Tab)
                    })
                    .map(|scope| match scope {
                        Scope::Tab(id) if !open.contains(&id) => Scope::Own,
                        scope => scope,
                    });
                memo.insert(p.pid, scope);
                above = scope;
                let (Some(scope), Some(start), true) = (scope, p.start, countable(k)) else {
                    continue;
                };
                let born = match p.birth_ms {
                    Some(b) => since_ms.is_some_and(|t| b >= t),
                    None => since_start.is_some_and(|s| start > s),
                };
                let (cpu, child) = (p.cpu_ns.unwrap_or(0), p.child_cpu_ns.unwrap_or(0));
                let delegated = i.delegated.get(k).copied().unwrap_or(false);
                let unwaited = p.reaps_unwaited == Some(true);
                rows.insert(
                    p.pid,
                    Row {
                        start,
                        birth: p.birth_ms,
                        scope,
                        cpu,
                        child,
                        credited: (!delegated).then_some(cpu),
                        parent: None,
                        ppid: p.ppid,
                        pending: 0,
                        soon: 0,
                        late: 0,
                        soon_step: step,
                        zombie: p.state == ProcState::Zombie,
                        delegated,
                        job: delegated,
                        from: (!born).then_some(step),
                        read: step,
                        unwaited,
                        under_unwaited: unwaited_at(p.ppid),
                        distrust: 0,
                        fresh: 0,
                    },
                );
                first.push(p.pid);
                // One that held work with no row holds it in this row's reaped time now.
                held.remove(&p.pid);
                if delegated {
                    // The job's figure reads a member from a window's start: one first read since
                    // is in none of its figures over that window.
                    if since_ms.is_some() {
                        miss(tabs, scope, step);
                    }
                    continue;
                }
                if born && unwaited {
                    // Its reaped time may hold its children's work twice: its own counts alone.
                    credit(tabs, own, scope, cpu);
                    if child > 0 {
                        miss(tabs, scope, step);
                    }
                } else if born {
                    credit(tabs, own, scope, cpu.saturating_add(child));
                } else if since_ms.is_some() {
                    miss(tabs, scope, step);
                }
            }
        }

        // Each member read now takes its nearest member ancestor, whose reaped time will hold
        // it: through processes that have no row, so a member under another user's `sudo` still
        // passes its pending up; none where the chain reaches launchd. Each process holding work
        // takes its parent and nearest member ancestor the same way.
        let keep = fault_is(fault, Fault::KeepFirstParent);
        let parents: Vec<(u32, Option<(u32, u64)>)> = rows
            .iter()
            .filter(|(pid, r)| r.read == step && (!keep || first.contains(pid)))
            .map(|(pid, _)| (*pid, row_above(rows, here, i.procs, *pid)))
            .collect();
        for (pid, parent) in parents {
            if let (Some(r), Some(&k)) = (rows.get_mut(&pid), here.get(&pid)) {
                r.parent = parent;
                r.ppid = i.procs[k].ppid;
            }
        }
        for (pid, h) in held.iter_mut() {
            if let Some(&k) = here.get(pid) {
                h.ppid = i.procs[k].ppid;
                h.parent = row_above(rows, here, i.procs, *pid);
                h.doubled |= i.procs[k].reaps_unwaited == Some(true);
            }
        }

        if fault_is(fault, Fault::Naive) {
            *own = 0;
            for c in tabs.values_mut() {
                c.ns = 0;
            }
            for r in rows.values().filter(|r| !r.delegated) {
                credit(tabs, own, r.scope, r.cpu.saturating_add(r.child));
            }
        }
    }
}

/// Adds `ns` to `scope`'s counter.
fn credit(tabs: &mut IdMap<u64, Counter>, own: &mut u64, scope: Scope, ns: u64) {
    match scope {
        Scope::Tab(id) => {
            let c = tabs.entry(id).or_default();
            c.ns = c.ns.saturating_add(ns);
        }
        Scope::Own => *own = own.saturating_add(ns),
    }
}

/// Marks step `step` as one that missed some of `scope`'s work, where `scope` is a tab.
fn miss(tabs: &mut IdMap<u64, Counter>, scope: Scope, step: u64) {
    if let Scope::Tab(id) = scope {
        tabs.entry(id).or_default().missed = step;
    }
}

/// The nearest member above the swept process `pid`: up its parents through processes that
/// have no row; `None` where the chain reaches launchd or leaves the sweep.
fn row_above(
    rows: &IdMap<u32, Row>,
    here: &IdMap<u32, usize>,
    procs: &[ProcSample],
    pid: u32,
) -> Option<(u32, u64)> {
    let mut at = procs[*here.get(&pid)?].ppid;
    for _ in 0..MAX_HOPS {
        if at <= 1 {
            return None;
        }
        let p = &procs[*here.get(&at)?];
        if let Some(r) = rows.get(&at).filter(|r| Some(r.start) == p.start) {
            return Some((at, r.start));
        }
        if p.ppid == at {
            return None;
        }
        at = p.ppid;
    }
    None
}

/// The nearest member still listed from `up`, a gone member's or a holder's nearest member
/// ancestor: up its parents through members also gone; `None` where the chain meets no member
/// (launchd).
fn reaper(
    rows: &IdMap<u32, Row>,
    mut up: Option<(u32, u64)>,
    gone: impl Fn(u32, &Row) -> bool,
) -> Option<u32> {
    for _ in 0..MAX_HOPS {
        let (pid, start) = up?;
        let row = rows.get(&pid).filter(|p| p.start == start)?;
        if !gone(pid, row) {
            return Some(pid);
        }
        up = row.parent;
    }
    None
}

/// The faults by name, so a release build (no faults) reads the same code.
#[derive(Clone, Copy)]
enum Fault {
    Naive,
    Forget,
    KeepFirstParent,
    HoldEveryPending,
    AbsorbNothing,
}

#[cfg(any(test, debug_assertions))]
fn fault_is(fault: Option<ReapFault>, f: Fault) -> bool {
    matches!(
        (fault, f),
        (Some(ReapFault::Naive), Fault::Naive)
            | (Some(ReapFault::ForgetZombieRead), Fault::Forget)
            | (Some(ReapFault::KeepFirstParent), Fault::KeepFirstParent)
            | (Some(ReapFault::HoldEveryPending), Fault::HoldEveryPending)
            | (Some(ReapFault::AbsorbNothing), Fault::AbsorbNothing)
    )
}

#[cfg(not(any(test, debug_assertions)))]
fn fault_is(_: Option<()>, _: Fault) -> bool {
    false
}

#[cfg(test)]
#[path = "reap_tests.rs"]
mod reap_tests;
