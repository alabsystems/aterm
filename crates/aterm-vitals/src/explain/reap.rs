// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reap credit (SA §7.5): attributed CPU charges and independently observed actual own CPU.
//!
//! The charge reconciles inherited kernel counters and is diagnostic, not an actual-work
//! lower bound: an unobserved auto-reaping process can pass up doubled child time. The own-CPU
//! floor is tracked independently by process incarnation; ambiguous charge epochs make that
//! floor the primary actual-work evidence.
//!
//! A sweep reads only the processes alive when it runs, and a build's processes mostly start
//! and finish between two sweeps. The kernel keeps their work: at each reap it adds the child's
//! own CPU and its reaped children's to its parent's reaped-children time, once where the parent
//! waits for it (measured on macOS 27: the parent's growth equals the zombie's final figures to
//! the nanosecond) and twice where the kernel reaps it for the parent, one that ignores `SIGCHLD`
//! or set `SA_NOCLDWAIT` (`P_NOCLDWAIT`; measured: 400.4–401.6 ms for a 200 ms child). So a tab's
//! attributed charge combines its members' own CPU growth and reaped-children time, where the
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
//! parent's reaped time takes to show it; a parent with no row, or whose reading leaves its row
//! unread (another user's `sudo`, or a member that ran `exec sudo`), passes it up only when it
//! goes itself, so it is held there until then and its own parent's reaped time absorbs nothing
//! of it before. A parent gone too, or a zombie, reaped it only if it outlived it
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
//! readings count none of it, so the tab's figure there is a lower bound. That is as far as the
//! readings see the flag: the sweep reads it of every process at each one, and nothing records
//! it between two readings or after a process exits, so a parent that ignores `SIGCHLD` and
//! starts and exits between two readings, or ignores it only between them (around its
//! children's exits), passes its children's doubled time to its reaper unseen, whose reaped
//! growth counts it (save over the two readings after a child read with the flag goes): the tab's
//! figure over such a window can read up to twice that work, exact, which shows only as the
//! split's `unaccounted` reading lower, below 0 where nothing else hides it.
//!
//! A process listed but unread (mid-exit, or another user's after a setuid exec, which keeps its
//! birth) keeps its row, unread; one listed under another birth is a new process at the pid. One
//! last read under launchd leaves nothing pending, and its tail since that reading is lost; one
//! first seen as a zombie or read as one is counted at its final figure, which later readings leave
//! as it is. A member counts from its birth only where it was born since the last step: by its
//! start, unix ms, against the last reading's wall clock, read before its sweep listed the
//! processes, or by its kernel start, newer than every one the last sweep read, so that sweep
//! read it no start (a process still being spawned as it listed, its fork stamped before that
//! reading's clock, had run nothing; and a short job gone before the host read its birth has
//! only this). One older, such as a tab's shell adopted from a predecessor, counts from the step
//! that first reads it, and so does one being spawned as a sweep listed where that sweep read a
//! newer process: what it ran before that step is missed. A delegated job's member counts there
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
//! unaccounted). Missing coverage and ambiguous inherited charges stay separate: the own-CPU
//! floor records actual observed own growth even when conservative reconciliation withholds a
//! retained child's charge to avoid a nonsimultaneous parent read counting it again.
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
    /// Final owning-wait aggregate when the terminal own/child split was unread. Kept as an
    /// aggregate rather than inventing either lifetime counter.
    aggregate_final: Option<u64>,
    /// Lifetime already set against a possible early reap by this exact ancestor. Restoring
    /// an older still-listed child must neither credit that lifetime again nor make that same
    /// ancestor absorb it twice when disappearance is finally observed.
    prepaid: Option<(crate::delegated::ProcessKey, u64)>,
    /// A nonsimultaneous child reading made this parent's reaped growth ambiguous.
    overlapping: bool,
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
    floor_ns: u64,
    ambiguous: u64,
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
    /// Independently observed own CPU only, never an inherited child charge.
    pub floor_ns: u64,
    /// Last step whose attributed raw charge could contain unobserved inherited child time.
    pub ambiguous: u64,
}

/// The ledger's counters at one reading: where a window starts or ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReapTotals {
    /// The step that read them.
    pub step: u64,
    /// Every scope's counter summed, ns: a closed tab's folds into aterm's own, so this sum keeps
    /// every tab's work, closed or open.
    pub all: u64,
    /// Last step that knowingly missed work in any scope, including aterm's own/shared work.
    pub missed: u64,
    /// Every scope's independently observed own CPU, including closed tabs, nanoseconds.
    pub all_floor: u64,
    /// Last ambiguous inherited charge in any scope.
    pub ambiguous: u64,
    /// Last ambiguous inherited charge in aterm's own scope.
    pub own_ambiguous: u64,
    /// Independent nondelegated own CPU grouped by its observed resource coalition.
    pub coalition_floors: IdMap<u64, u64>,
    /// Own CPU in each current pane, separate from a logical tab's pooled counter.
    pub pane_floors: IdMap<u64, u64>,
    /// Independent own CPU whose coalition could not be established for that counter delta.
    pub unmapped_floor: u64,
    /// Each open tab's counter, by key.
    pub tabs: Vec<TabTotal>,
}

impl ReapTotals {
    /// Independent own CPU observed in this pane since `base`.
    #[must_use]
    pub fn pane_floor_since(&self, base: &Self, pane: u64) -> u64 {
        self.pane_floors
            .get(&pane)
            .copied()
            .unwrap_or(0)
            .saturating_sub(base.pane_floors.get(&pane).copied().unwrap_or(0))
    }
    /// Own CPU observed in `coalition` since `base`, without inherited child charges.
    #[must_use]
    pub fn coalition_floor_since(&self, base: &Self, coalition: u64) -> u64 {
        self.coalition_floors
            .get(&coalition)
            .copied()
            .unwrap_or(0)
            .saturating_sub(base.coalition_floors.get(&coalition).copied().unwrap_or(0))
    }
    /// Independent own CPU of this tab since `base`, and whether inherited charges in that
    /// interval were ambiguous. Identity is the stable tab id, including a pane-key change.
    #[must_use]
    pub fn floor_since(&self, base: &Self, key: u64) -> (u64, bool) {
        let Some(tab) = self.tabs.iter().find(|t| t.key == key) else {
            return (0, false);
        };
        let before = base
            .tabs
            .iter()
            .find(|t| t.id == tab.id)
            .map_or(0, |t| t.floor_ns);
        (
            tab.floor_ns.saturating_sub(before),
            tab.ambiguous > base.step,
        )
    }

    /// Own-scope floor over the interval, excluding open tabs. Closed historical counters do
    /// not become fresh work: only the aggregate's growth since `base` participates.
    #[must_use]
    pub fn own_floor_since(&self, base: &Self) -> (u64, bool) {
        let tabs = self.tabs.iter().fold(0_u64, |sum, t| {
            sum.saturating_add(self.floor_since(base, t.key).0)
        });
        (
            self.all_floor
                .saturating_sub(base.all_floor)
                .saturating_sub(tabs),
            self.own_ambiguous > base.step,
        )
    }
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
    /// Native clocks at the whole frontend sweep's end. They do not use `at_ns`'s epoch.
    pub cpu_cut: crate::model::NativeCpuCut,
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
    /// Only real whole sweeps advance the timing boundary, never synthetic terminal passes.
    whole_at_ns: Option<u64>,
    whole_wall_ms: Option<i64>,
    whole_cpu_cut: crate::model::NativeCpuCut,
    floor_rows: IdMap<crate::delegated::ProcessKey, OwnFloor>,
    coalition_floors: IdMap<u64, u64>,
    pane_floors: IdMap<u64, u64>,
    unmapped_floor: u64,
    own_floor: u64,
    ambiguity: u64,
    own_ambiguity: u64,
    terminal_totals: IdMap<crate::delegated::ProcessKey, Option<u64>>,
    terminal_overlap: IdMap<crate::delegated::ProcessKey, u64>,
    baseline_only: IdSet<crate::delegated::ProcessKey>,
    /// Last consumed original-wait journal cursor, separately for every daemon incarnation.
    native_sequences: IdMap<crate::delegated::ProcessKey, u64>,
    /// Accepted transport events remain ours until a whole process sweep can consume them.
    pending_terminals: IdMap<(crate::delegated::ProcessKey, u64), crate::model::NativeTerminal>,
    pending_native_incomplete: bool,
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
    /// Last step that knowingly missed work, whether in a tab or aterm's own scope.
    missed: u64,
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

#[derive(Clone, Copy, Debug)]
struct OwnFloor {
    cpu: u64,
    /// Whole frontend observation that read this counter, including terminal passes at its cut.
    at_ns: u64,
    scope: Scope,
    delegated: bool,
    coalition: Option<u64>,
    version: Option<i32>,
    pane: Option<u64>,
}

impl ReapLedger {
    fn born_in_window(&self, p: &ProcSample, i: &StepIn) -> bool {
        if let (Some(before), Some(now), Some(start)) =
            (self.whole_cpu_cut.boot, i.cpu_cut.boot, p.start)
        {
            return before < start && start <= now;
        }
        p.birth_ms
            .zip(self.whole_wall_ms)
            .is_some_and(|(birth, before)| before < birth && birth <= i.at_unix_ms)
    }

    fn floor_is_consecutive(&self, before: &OwnFloor, now: u64) -> bool {
        Some(before.at_ns) == self.whole_at_ns || before.at_ns == now
    }

    fn terminal_in_window(&self, record: &crate::model::NativeTerminal, i: &StepIn) -> bool {
        let (Some(before), Some(now), Some(exited)) = (
            self.whole_cpu_cut.continuous_ns,
            i.cpu_cut.continuous_ns,
            record.timing.exited_ns,
        ) else {
            return false;
        };
        if !(before < exited && exited <= now) {
            return false;
        }
        // An exit in this interval does not place its whole lifetime here. An existing
        // consecutive own-counter baseline bounds its delta; otherwise its birth must be
        // proved inside this actual model interval, including deferred whole-sweep adoption.
        let consecutive = record.process.start.is_some_and(|start| {
            self.floor_rows
                .get(&crate::delegated::ProcessKey {
                    pid: record.process.pid,
                    start,
                })
                .is_some_and(|row| self.floor_is_consecutive(row, i.at_ns))
        });
        consecutive || self.born_in_window(&record.process, i)
    }

    fn observe_own_floor(&mut self, i: &StepIn) {
        self.name_tabs(i.tabs);
        for ((p, member), delegated) in i.procs.iter().zip(i.members).zip(i.delegated) {
            let (Some(start), Some(cpu)) = (p.start, p.cpu_ns) else {
                continue;
            };
            let key = crate::delegated::ProcessKey { pid: p.pid, start };
            let previous = self.floor_rows.get(&key).copied();
            if previous.is_some_and(|p| p.delegated) && !*delegated && !member.aterm {
                self.floor_rows.remove(&key);
                continue;
            }
            if p.uid != Some(i.self_uid) && !member.aterm && previous.is_none() {
                continue;
            }
            let scope = member
                .tab
                .and_then(|index| i.tabs.get(index))
                .and_then(|tab| {
                    self.keys
                        .iter()
                        .find(|(key, _)| *key == tab.tab)
                        .map(|(_, id)| Scope::Tab(*id))
                })
                .or_else(|| {
                    previous.map(|p| match p.scope {
                        Scope::Tab(id) if !self.keys.iter().any(|(_, open)| *open == id) => {
                            Scope::Own
                        }
                        scope => scope,
                    })
                })
                .or_else(|| member.aterm.then_some(Scope::Own));
            let Some(scope) = scope else { continue };
            let pane = member
                .tab
                .and_then(|index| i.tabs.get(index))
                .map(|tab| tab.key)
                .or_else(|| {
                    previous
                        .and_then(|before| before.pane)
                        .filter(|key| i.tabs.iter().any(|tab| tab.key == *key))
                });
            let born = self.born_in_window(p, i);
            let regressed = previous.is_some_and(|before| cpu < before.cpu);
            let consecutive = !regressed
                && previous.is_some_and(|before| self.floor_is_consecutive(&before, i.at_ns));
            let delta = if consecutive {
                cpu.saturating_sub(previous.map_or(0, |before| before.cpu))
            } else if previous.is_none() && born {
                cpu
            } else {
                0
            };
            if !*delegated
                && self.whole_at_ns.is_some()
                && !consecutive
                && (previous.is_some() || !born)
                && (cpu > 0 || regressed)
            {
                // A readable counter after a gap bounds a lifetime, not this interval.
                // Rebaseline it; the raw reconciled charge remains separately visible.
                // A rollback keeps its prior maximum and read stamp until a later recovery.
                miss(&mut self.tabs, &mut self.missed, scope, self.step + 1);
                ambiguous(
                    &mut self.tabs,
                    &mut self.ambiguity,
                    &mut self.own_ambiguity,
                    scope,
                    self.step + 1,
                );
            }
            let coalition = p.coalition.or_else(|| {
                previous
                    .filter(|before| p.version.is_some() && p.version == before.version)
                    .and_then(|before| before.coalition)
            });
            self.floor_rows.insert(
                key,
                OwnFloor {
                    cpu: previous
                        .filter(|_| regressed)
                        .map_or(cpu, |before| before.cpu),
                    at_ns: previous
                        .filter(|_| regressed)
                        .map_or(i.at_ns, |before| before.at_ns),
                    scope,
                    delegated: *delegated,
                    coalition,
                    version: p
                        .version
                        .or_else(|| previous.and_then(|before| before.version)),
                    pane,
                },
            );
            if !*delegated {
                if let Some(pane) = pane {
                    let total = self.pane_floors.entry(pane).or_default();
                    *total = total.saturating_add(delta);
                }
                if let Some(coalition) = coalition {
                    let total = self.coalition_floors.entry(coalition).or_default();
                    *total = total.saturating_add(delta);
                } else {
                    self.unmapped_floor = self.unmapped_floor.saturating_add(delta);
                }
                match scope {
                    Scope::Own => self.own_floor = self.own_floor.saturating_add(delta),
                    Scope::Tab(id) => {
                        let counter = self.tabs.entry(id).or_default();
                        counter.floor_ns = counter.floor_ns.saturating_add(delta);
                    }
                }
            }
        }
    }
    /// Delivered native evidence still waiting for a complete process/ownership reading.
    pub(crate) fn has_pending_native(&self) -> bool {
        !self.pending_terminals.is_empty() || self.pending_native_incomplete
    }

    /// Bound accounting metadata to coalitions still listed or still held by an observed
    /// process. An inactive coalition's prior snapshots keep their own immutable counters.
    pub(crate) fn retain_coalitions(&mut self, read: &super::CoalSample) {
        let mut present: IdSet<_> = read.rows.iter().map(|row| row.id).collect();
        present.extend(self.floor_rows.values().filter_map(|row| row.coalition));
        self.coalition_floors.retain(|id, _| present.contains(id));
    }
    /// Accept transport custody before checking whether a process sweep can run. A source
    /// cursor may advance on a truncated or machine-only observation; those observations must
    /// not discard final counters already delivered to the model.
    pub(crate) fn ingest_terminals(
        &mut self,
        terminals: &[crate::model::NativeTerminal],
        incomplete: bool,
    ) {
        self.pending_native_incomplete |= incomplete;
        for record in terminals {
            if record.seq
                <= self
                    .native_sequences
                    .get(&record.host)
                    .copied()
                    .unwrap_or(0)
            {
                continue;
            }
            self.pending_terminals
                .entry((record.host, record.seq))
                .or_insert_with(|| record.clone());
        }
        while self.pending_terminals.len() > 65_536 {
            let oldest = self
                .pending_terminals
                .iter()
                .min_by_key(|(_, r)| (r.process.start, r.seq))
                .map(|(key, _)| *key);
            if let Some(key) = oldest {
                self.pending_terminals.remove(&key);
            }
            self.pending_native_incomplete = true;
        }
    }

    /// Credit exact native terminal generations before their disappearance reaches the normal
    /// sweep. Parent child counters are held at their prior values until every terminal has
    /// become pending: otherwise one child's final sample could first charge a sibling's
    /// unseen completion to the daemon. Final generations remain separate from a live process
    /// that already reused their PID.
    pub(crate) fn step_with_terminals(
        &mut self,
        i: &StepIn,
        terminals: &[crate::model::NativeTerminal],
        incomplete: bool,
    ) {
        self.ingest_terminals(terminals, incomplete);
        if i.truncated {
            return;
        }
        let terminals: Vec<_> = std::mem::take(&mut self.pending_terminals)
            .into_values()
            .collect();
        let incomplete = std::mem::take(&mut self.pending_native_incomplete);
        let mut records: Vec<_> = terminals
            .iter()
            .filter(|r| r.seq > self.native_sequences.get(&r.host).copied().unwrap_or(0))
            .collect();
        records.sort_by_key(|r| (r.host.pid, r.host.start, r.seq));
        let mut invalid = Vec::new();
        let baseline = (self.at_unix_ms, self.newest);
        let records: Vec<_> = records
            .into_iter()
            .filter(|record| {
                self.native_sequences.insert(record.host, record.seq);
                if self.terminal_in_window(record, i) {
                    true
                } else {
                    // Preserve the receipt in Snapshot, consume its cursor once, and withhold
                    // both synthetic raw charge and own floor. The real reaper counter remains
                    // free to grow once in whichever ordinary sweep actually reads it.
                    invalid.push(record.tab);
                    false
                }
            })
            .collect();
        let mut final_children = IdMap::<_, u64>::default();
        for record in &records {
            if let Some(cpu) = record
                .process
                .cpu_ns
                .zip(record.process.child_cpu_ns)
                .map(|(cpu, child)| cpu.saturating_add(child))
                .or(record.wait_cpu_ns)
            {
                let sum = final_children.entry(record.host).or_default();
                *sum = sum.saturating_add(cpu);
            }
        }
        let mut parent_children = IdMap::default();
        for (host, final_children) in final_children {
            let old = self
                .rows
                .get(&host.pid)
                .filter(|r| r.start == host.start)
                .map(|r| r.child);
            let raw = i
                .procs
                .iter()
                .find(|p| p.pid == host.pid && p.start == Some(host.start))
                .and_then(|p| p.child_cpu_ns);
            let held = old.or_else(|| raw.map(|raw| raw.saturating_sub(final_children)));
            parent_children.insert(host, held);
        }
        let mut remaining = Vec::new();
        for record in records {
            let process = &record.process;
            if process.start.is_none()
                || (process.cpu_ns.zip(process.child_cpu_ns).is_none()
                    && record.wait_cpu_ns.is_none())
                || process.pid == record.host.pid
                || process.ppid != record.host.pid
            {
                invalid.push(record.tab);
                continue;
            }
            remaining.push(record);
        }
        // Different PIDs share one synthetic sweep. A PID reused several times needs one
        // pass per generation, oldest first; native root generations never coexist in a row.
        remaining.sort_by_key(|r| (r.process.start, r.seq));
        while !remaining.is_empty() {
            let mut seen = IdSet::default();
            let (batch, later): (Vec<_>, Vec<_>) = remaining
                .into_iter()
                .partition(|r| seen.insert(r.process.pid));
            remaining = later;
            let mut procs = i.procs.to_vec();
            let mut members = i.members.to_vec();
            let mut delegated = i.delegated.to_vec();
            for record in &batch {
                let mut final_process = record.process.clone();
                final_process.state = ProcState::Zombie;
                if procs
                    .iter()
                    .any(|p| p.pid == record.host.pid && p.start != Some(record.host.start))
                {
                    // This is another generation of the custodian's PID, so it reaped none of
                    // this old custodian's children.
                    final_process.ppid = 1;
                }
                let member = Membership {
                    aterm: true,
                    shared: false,
                    tab: record
                        .tab
                        .and_then(|key| i.tabs.iter().position(|t| t.key == key || t.tab == key)),
                };
                match procs.iter().position(|p| p.pid == record.process.pid) {
                    Some(k) => {
                        procs[k] = final_process;
                        members[k] = member;
                        delegated[k] = false;
                    }
                    None => {
                        procs.push(final_process);
                        members.push(member);
                        delegated.push(false);
                    }
                }
            }
            for p in &mut procs {
                if let Some(start) = p.start
                    && let Some(&child) =
                        parent_children.get(&crate::delegated::ProcessKey { pid: p.pid, start })
                {
                    p.child_cpu_ns = child;
                }
            }
            self.terminal_totals = batch
                .iter()
                .filter_map(|record| {
                    Some((
                        crate::delegated::ProcessKey {
                            pid: record.process.pid,
                            start: record.process.start?,
                        },
                        record.wait_cpu_ns,
                    ))
                })
                .collect();
            self.step_observed(
                &StepIn {
                    procs: &procs,
                    members: &members,
                    delegated: &delegated,
                    ..*i
                },
                false,
            );
            for record in batch
                .iter()
                .filter(|r| r.process.cpu_ns.zip(r.process.child_cpu_ns).is_none())
            {
                let Some(total) = record.wait_cpu_ns else {
                    continue;
                };
                if total > record.process.cpu_ns.unwrap_or(0) {
                    let scope = record
                        .tab
                        .and_then(|key| {
                            self.keys
                                .iter()
                                .find(|(k, _)| *k == key)
                                .map(|(_, id)| Scope::Tab(*id))
                        })
                        .unwrap_or(Scope::Own);
                    ambiguous(
                        &mut self.tabs,
                        &mut self.ambiguity,
                        &mut self.own_ambiguity,
                        scope,
                        self.step,
                    );
                }
                let overlap = record
                    .process
                    .start
                    .and_then(|start| {
                        self.terminal_overlap.get(&crate::delegated::ProcessKey {
                            pid: record.process.pid,
                            start,
                        })
                    })
                    .copied()
                    .unwrap_or(0);
                let scope = record
                    .tab
                    .and_then(|key| {
                        self.keys
                            .iter()
                            .find(|(k, _)| *k == key)
                            .map(|(_, id)| Scope::Tab(*id))
                    })
                    .unwrap_or(Scope::Own);
                let parent = self
                    .rows
                    .get(&record.host.pid)
                    .filter(|p| p.start == record.host.start)
                    .map(|p| (record.host.pid, p.start));
                if let Some(row) = self
                    .rows
                    .get_mut(&record.process.pid)
                    .filter(|p| Some(p.start) == record.process.start)
                {
                    let doubtful = row.soon.saturating_add(row.late);
                    let known = row
                        .cpu
                        .saturating_add(row.child)
                        .saturating_add(row.pending.saturating_add(doubtful).max(overlap));
                    let untrusted_children = row.unwaited
                        || row.under_unwaited
                        || record.process.reaps_unwaited != Some(false);
                    if !untrusted_children {
                        credit(
                            &mut self.tabs,
                            &mut self.own,
                            scope,
                            total.saturating_sub(known),
                        );
                    }
                    if doubtful > 0 || overlap > 0 || total < known || untrusted_children {
                        miss(&mut self.tabs, &mut self.missed, scope, self.step);
                    }
                    if untrusted_children {
                        row.distrust = row.distrust.max(self.step + 1);
                    }
                    row.scope = scope;
                    row.aggregate_final = Some(total);
                    row.pending = 0;
                    row.soon = 0;
                    row.late = 0;
                    row.zombie = true;
                    row.parent = parent;
                    row.ppid = parent.map_or(1, |(pid, _)| pid);
                } else {
                    let born = self.born_in_window(&record.process, i);
                    if born && record.process.reaps_unwaited == Some(false) {
                        credit(
                            &mut self.tabs,
                            &mut self.own,
                            scope,
                            total.saturating_sub(overlap),
                        );
                        if overlap > 0 {
                            miss(&mut self.tabs, &mut self.missed, scope, self.step);
                        }
                    } else {
                        // The combined wait is known, but no reading bounds how much preceded
                        // this window. Retain that aggregate in the snapshot and mark timing
                        // coverage unknown rather than assign its whole lifetime to this tick.
                        miss(&mut self.tabs, &mut self.missed, scope, self.step);
                    }
                    if let Some((pid, _)) = parent
                        && let Some(p) = self.rows.get_mut(&pid)
                    {
                        p.pending = p.pending.saturating_add(total);
                    }
                }
            }
            self.terminal_totals.clear();
            // A synthetic terminal pass is no new birth boundary for this whole sweep.
            // In particular a live incarnation that reused a terminal's PID may be new since
            // the prior whole sweep despite being older than another current process.
            (self.at_unix_ms, self.newest) = baseline;
        }
        self.step(i);
        let present: IdSet<_> = i
            .procs
            .iter()
            .filter_map(|p| {
                Some(crate::delegated::ProcessKey {
                    pid: p.pid,
                    start: p.start?,
                })
            })
            .collect();
        self.floor_rows.retain(|key, _| present.contains(key));
        self.pane_floors
            .retain(|key, _| i.tabs.iter().any(|tab| tab.key == *key));
        if incomplete {
            self.missed = self.step;
            for counter in self.tabs.values_mut() {
                counter.missed = self.step;
            }
        }
        for tab in invalid {
            let scope = tab
                .and_then(|key| {
                    self.keys
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, id)| Scope::Tab(*id))
                })
                .unwrap_or(Scope::Own);
            miss(&mut self.tabs, &mut self.missed, scope, self.step);
            ambiguous(
                &mut self.tabs,
                &mut self.ambiguity,
                &mut self.own_ambiguity,
                scope,
                self.step,
            );
        }
    }

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
                    floor_ns: c.floor_ns,
                    ambiguous: c.ambiguous,
                }
            })
            .collect();
        tabs.sort_unstable_by_key(|t| t.key);
        ReapTotals {
            step: self.step,
            missed: self.missed,
            all_floor: self
                .tabs
                .values()
                .fold(self.own_floor, |sum, c| sum.saturating_add(c.floor_ns)),
            ambiguous: self.ambiguity,
            own_ambiguous: self.own_ambiguity,
            coalition_floors: self.coalition_floors.clone(),
            pane_floors: self.pane_floors.clone(),
            unmapped_floor: self.unmapped_floor,
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

    /// Reads one whole sweep. Counter reads within a sweep need not be simultaneous: a child
    /// sampled first can already have been reaped by a parent sampled later. Let that child's
    /// known lifetime become pending before the later parent counter is credited, then restore
    /// the retained child reading as a baseline. That overlap is ambiguous, never an exact
    /// claim that the child died; a known escaped parent chain or a later child counter stays
    /// independent.
    pub fn step(&mut self, i: &StepIn) {
        self.step_observed(i, true);
    }

    fn step_observed(&mut self, i: &StepIn, whole: bool) {
        if i.truncated {
            return;
        }
        self.observe_own_floor(i);
        self.terminal_overlap.clear();
        let by_pid: IdMap<_, _> = i.procs.iter().map(|p| (p.pid, p)).collect();
        let mut overlaps = IdMap::default();
        for parent in i.procs {
            let Some(start) = parent.start else { continue };
            let key = crate::delegated::ProcessKey {
                pid: parent.pid,
                start,
            };
            let previous = self.rows.get(&parent.pid).filter(|p| p.start == start);
            let grew = parent
                .child_cpu_ns
                .is_some_and(|cpu| cpu > previous.map_or(0, |p| p.child));
            let terminal = self.terminal_totals.get(&key);
            let aggregate_grew = terminal
                .copied()
                .flatten()
                .is_some_and(|cpu| cpu > previous.map_or(0, |p| p.cpu.saturating_add(p.child)));
            if !grew && !aggregate_grew {
                continue;
            }
            for child in i.procs {
                let Some(child_start) = child.start else {
                    continue;
                };
                if child.pid == parent.pid
                    || child
                        .cpu_ns
                        .unwrap_or(0)
                        .saturating_add(child.child_cpu_ns.unwrap_or(0))
                        == 0
                {
                    continue;
                }
                let earlier = match (child.at_ns, parent.at_ns) {
                    (Some(child), Some(parent)) => child < parent,
                    _ => terminal.is_some(),
                };
                if !earlier {
                    continue;
                }
                let mut up = child.ppid;
                let mut distance = 0;
                while up > 1 && distance < MAX_HOPS {
                    distance += 1;
                    if up == parent.pid {
                        break;
                    }
                    let next = by_pid.get(&up).map(|p| p.ppid);
                    let Some(next) = next.filter(|next| *next != up) else {
                        break;
                    };
                    up = next;
                }
                if up != parent.pid {
                    continue;
                }
                let child_key = crate::delegated::ProcessKey {
                    pid: child.pid,
                    start: child_start,
                };
                let known = self
                    .rows
                    .get(&child.pid)
                    .filter(|p| p.start == child_start)
                    .map_or(0, |p| {
                        p.cpu.saturating_add(p.child).saturating_sub(
                            p.prepaid
                                .filter(|(to, _)| *to == key)
                                .map_or(0, |(_, cpu)| cpu),
                        )
                    });
                let entry = overlaps.entry(child_key).or_insert((key, known, distance));
                if distance < entry.2 {
                    *entry = (key, known, distance);
                }
            }
        }
        if overlaps.is_empty() {
            self.step_inner(i);
            if whole {
                self.accept_whole_cut(i);
            }
            return;
        }
        for (parent, known, _) in overlaps.values() {
            let cpu = self.terminal_overlap.entry(*parent).or_insert(0_u64);
            *cpu = cpu.saturating_add(*known);
        }
        let baseline = (self.at_unix_ms, self.newest);
        let mut procs = Vec::with_capacity(i.procs.len());
        let mut members = Vec::with_capacity(i.members.len());
        let mut delegated = Vec::with_capacity(i.delegated.len());
        for ((p, member), job) in i.procs.iter().zip(i.members).zip(i.delegated) {
            if p.start.is_some_and(|start| {
                overlaps.contains_key(&crate::delegated::ProcessKey { pid: p.pid, start })
            }) {
                continue;
            }
            procs.push(p.clone());
            members.push(*member);
            delegated.push(*job);
        }
        self.step_inner(&StepIn {
            procs: &procs,
            members: &members,
            delegated: &delegated,
            ..*i
        });
        (self.at_unix_ms, self.newest) = baseline;
        self.baseline_only.extend(overlaps.keys().copied());
        self.step_inner(i);
        self.baseline_only.clear();
        for (child, (parent, _, _)) in overlaps {
            if let Some(row) = self
                .rows
                .get_mut(&child.pid)
                .filter(|p| p.start == child.start)
            {
                row.prepaid = Some((parent, row.cpu.saturating_add(row.child)));
                miss(&mut self.tabs, &mut self.missed, row.scope, self.step);
                ambiguous(
                    &mut self.tabs,
                    &mut self.ambiguity,
                    &mut self.own_ambiguity,
                    row.scope,
                    self.step,
                );
            }
            if let Some(row) = self
                .rows
                .get_mut(&parent.pid)
                .filter(|p| p.start == parent.start)
            {
                row.overlapping = true;
                miss(&mut self.tabs, &mut self.missed, row.scope, self.step);
                ambiguous(
                    &mut self.tabs,
                    &mut self.ambiguity,
                    &mut self.own_ambiguity,
                    row.scope,
                    self.step,
                );
            }
        }
        if whole {
            self.accept_whole_cut(i);
        }
    }

    fn accept_whole_cut(&mut self, i: &StepIn) {
        self.whole_at_ns = Some(i.at_ns);
        self.whole_wall_ms = Some(i.at_unix_ms);
        self.whole_cpu_cut = i.cpu_cut;
    }

    /// The pending-credit step over one consistent set of present generations.
    fn step_inner(&mut self, i: &StepIn) {
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
            missed,
            step,
            here,
            memo,
            tab_ids,
            held,
            baseline_only,
            terminal_overlap,
            own_floor,
            ambiguity,
            own_ambiguity,
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
                *own_floor = own_floor.saturating_add(c.floor_ns);
                *own_ambiguity = (*own_ambiguity).max(c.ambiguous);
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
            p.start.is_some()
                && (p.uid == Some(i.self_uid) || i.members.get(k).is_some_and(|m| m.aterm))
                && p.cpu_ns.is_some()
        };

        // Processes with no row gone since the last step that held members' work: each passes it
        // to its parent with its own reap, as a gone member does (below): to a member that is its
        // parent and alive, certain; to another live parent, which has no row or whose row its
        // reading leaves unread (a member that ran `exec sudo`), held again; else uncertain, at
        // its nearest member ancestor still listed, or, with none, lost.
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
                    miss(tabs, missed, q.scope, step);
                }
                continue;
            }
            if h.ppid > 1 && alive_at(h.ppid) && h.parent.is_some_and(|(p, _)| p != h.ppid) {
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
                        miss(tabs, missed, q.scope, step);
                    }
                }
                None => miss(tabs, missed, h.scope, step),
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
            let prepaid = r
                .prepaid
                .filter(|(to, _)| {
                    reaper(rows, r.parent, gone_now)
                        .and_then(|pid| rows.get(&pid).map(|row| (pid, row.start)))
                        .is_some_and(|(pid, start)| pid == to.pid && start == to.start)
                })
                .map_or(0, |(_, cpu)| cpu);
            let certain = r
                .aggregate_final
                .unwrap_or_else(|| r.cpu.saturating_add(r.child))
                .saturating_sub(prepaid)
                .saturating_add(r.pending);
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
                miss(tabs, missed, r.scope, step);
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
                    miss(tabs, missed, r.scope, step);
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
                            miss(tabs, missed, p.scope, step);
                        }
                    }
                }
                None if !r.delegated && (!r.zombie || r.doubt() > 0) => {
                    miss(tabs, missed, r.scope, step);
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
            if row.soon > 0 || row.late > 0 {
                ambiguous(tabs, ambiguity, own_ambiguity, row.scope, step);
            }
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
            if unseen && !row.delegated {
                ambiguous(tabs, ambiguity, own_ambiguity, row.scope, step);
            }
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
                doubt || (doubled && unseen) || row.overlapping
            };
            if short || back {
                miss(tabs, missed, row.scope, step);
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
        // process the last sweep could not read that is older than one it read, and every
        // process at the first step; after the first, what such a process ran since the last
        // step is missed. Born since is by its birth against the last reading's wall clock, or by
        // its kernel start against the newest the last sweep read: one alive then was listed
        // then, so one newer than every start read then was born since, or was listed then with
        // its own start unread, such as one still being spawned as it listed (its fork stamped
        // before that reading's wall clock), which had run nothing; this alone dates a short job
        // gone before the host read its birth.
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
                let born = (p.birth_ms.zip(since_ms).is_some_and(|(b, t)| b >= t)
                    || since_start.is_some_and(|s| start > s))
                    && !baseline_only.contains(&crate::delegated::ProcessKey { pid: p.pid, start });
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
                        aggregate_final: None,
                        prepaid: None,
                        overlapping: false,
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
                        miss(tabs, missed, scope, step);
                    }
                    continue;
                }
                if born && unwaited {
                    // Its reaped time may hold its children's work twice: its own counts alone.
                    credit(tabs, own, scope, cpu);
                    if child > 0 {
                        miss(tabs, missed, scope, step);
                    }
                } else if born {
                    let overlap = terminal_overlap
                        .get(&crate::delegated::ProcessKey { pid: p.pid, start })
                        .copied()
                        .unwrap_or(0);
                    credit(
                        tabs,
                        own,
                        scope,
                        cpu.saturating_add(child.saturating_sub(overlap)),
                    );
                    if overlap > 0 {
                        miss(tabs, missed, scope, step);
                    }
                } else if since_ms.is_some() {
                    miss(tabs, missed, scope, step);
                }
                if born && child > 0 && !delegated {
                    ambiguous(tabs, ambiguity, own_ambiguity, scope, step);
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

/// Marks step `step` as one that missed work globally and in the tab where there is one.
fn miss(tabs: &mut IdMap<u64, Counter>, missed: &mut u64, scope: Scope, step: u64) {
    *missed = step;
    if let Scope::Tab(id) = scope {
        tabs.entry(id).or_default().missed = step;
    }
}

fn ambiguous(
    tabs: &mut IdMap<u64, Counter>,
    all: &mut u64,
    own: &mut u64,
    scope: Scope,
    step: u64,
) {
    *all = step;
    match scope {
        Scope::Tab(id) => tabs.entry(id).or_default().ambiguous = step,
        Scope::Own => *own = step,
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
