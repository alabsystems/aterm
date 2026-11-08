// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vitals host's jobs pass: the stuck-process engine (`aterm_vitals::jobs`) fed on the probe
//! thread, so the stats bars' marks and the lens's Jobs well show its verdicts.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

mod lines;
use lines::Lines;
#[cfg(test)]
mod lines_tests;

use aterm_agent::supervise::{BackgroundCall, BackgroundEvidence, HeldCall};

use aterm_sysprobe::jobs::{
    self as probe, AtStart, HELD_CPU_NS, JobsReader, ProcFacts, ReadReq, StartFacts,
};
use aterm_sysprobe::procs::ProcessReading;
use aterm_sysprobe::volumes::Table;
use aterm_vitals::jobs::{
    Block, Class, Consumer, Demand, Fd, FdKind, HeldProc, JobCounters, JobKind, JobProc, JobRow,
    JobTab, JobTick, Jobs, JobsView, Knote, KnoteFilter, LanesSeen, Machine, NamingError,
    NamingSelector, NamingTicket, ProcState, Standing, StartWait, SweepProc, TcpState, TermWord,
    ThreadState, TierB, TtyMode,
};
use aterm_vitals::left::{LeftRow, LeftView};
use aterm_vitals::model::TabRoot;

use crate::program_check_watch::RunningCheck;

/// The engine's fast cadence, also the floor between passes, so a 2 Hz lens does not double the
/// detailed reads.
const FAST_MS: u64 = 2_000;

/// The readers the pass asks of this user's processes outside every job, the held stand-in, the
/// stuck check's board and the wall clock (`KERNEL` live, `INERT` in unit tests). None reads a
/// process's arguments or environment, which can hang on a program held mid-start (leftover
/// M29).
#[derive(Clone, Copy)]
pub(crate) struct HeldReaders {
    /// What a process's start reads, its age taken at the pass's clock (µs since the epoch), one
    /// mount table read for the pass ([`probe::read_start_in`]), judged by [`probe::judge_start`].
    pub(crate) start: fn(u32, u64, &OnceCell<Table>) -> Option<StartFacts>,
    /// Its pid version, which moves on exec ([`aterm_uds::process::read_identity`]).
    pub(crate) exec: fn(u32) -> Option<u64>,
    /// Its resource coalition ([`aterm_sysprobe::coalitions::coalition_of`]).
    pub(crate) coalition: fn(u32) -> Option<u64>,
    /// Its executable's file name, the basename of [`aterm_sysprobe::process_path`] (`p_comm` cuts
    /// a name at 16 bytes).
    pub(crate) exe_name: fn(u32) -> Option<String>,
    /// The pid a development build stands in as held
    /// ([`crate::program_check_watch::held_stand_in`]).
    pub(crate) stand_in: fn() -> Option<u32>,
    /// The syspolicyd the stuck check's last look read, while that look is fresh
    /// ([`crate::program_check_watch::last_read`]): what the stranded judge reads ([`strand`]).
    pub(crate) check: fn() -> Option<RunningCheck>,
    /// How many held programs of this user, every process read, forked at or after a time
    /// ([`aterm_uds::process::held_since`]): asked only when a row would first be stranded.
    pub(crate) backlog: fn(u64) -> Option<usize>,
    /// The wall clock, µs since the epoch ([`probe::now_us`]), read once a pass.
    pub(crate) now_us: fn() -> u64,
}

impl HeldReaders {
    /// The kernel's readers.
    pub(crate) const KERNEL: Self = Self {
        start: probe::read_start_in,
        exec: |pid| {
            if !aterm_uds::process::KEEPS_PID_VERSIONS {
                return None;
            }
            i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_identity)
                .and_then(|i| i.idversion)
                .and_then(|v| u64::try_from(v).ok())
        },
        coalition: aterm_sysprobe::coalitions::coalition_of,
        exe_name: |pid| {
            aterm_sysprobe::process_path(pid)?
                .file_name()?
                .to_str()
                .map(str::to_owned)
        },
        stand_in: crate::program_check_watch::held_stand_in,
        check: crate::program_check_watch::last_read,
        backlog: aterm_uds::process::held_since,
        now_us: probe::now_us,
    };

    /// The inert arm: nothing outside a job is read, nothing stands in, and no check is read.
    pub(crate) const INERT: Self = Self {
        start: |_, _, _| None,
        exec: |_| None,
        coalition: |_| None,
        exe_name: |_| None,
        stand_in: || None,
        check: || None,
        backlog: |_| None,
        now_us: probe::now_us,
    };
}

/// What a pass reads of a build's front ([`Fronts`]): the build it becomes and the ledger it
/// claims in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FrontSeen {
    /// The build it becomes, named as `status` names it
    /// ([`crate::session_status::program::front_program`]).
    becomes: String,
    /// The ledger it claims in ([`atpkg::build_admit::ledger_of_front`]), where one is found.
    ledger: Option<aterm_capacity::Ledger>,
}

/// How a pass reads its fronts: a pid's arguments, the ledger a front claims in, and a ledger's
/// live claims.
#[derive(Clone, Copy)]
pub(crate) struct FrontReaders {
    /// A pid's arguments ([`read_front_argv`]).
    pub(crate) argv: fn(u32) -> Option<Vec<OsString>>,
    /// The ledger the front `pid` claims in ([`ledger_of_front_pid`]).
    pub(crate) ledger: fn(u32) -> Option<aterm_capacity::Ledger>,
    /// A ledger's live claims ([`aterm_capacity::Ledger::list`]); `None` where unread.
    pub(crate) claims: fn(&aterm_capacity::Ledger) -> Option<Vec<aterm_capacity::Record>>,
}

impl FrontReaders {
    /// The live readers.
    pub(crate) const LIVE: Self = Self {
        argv: read_front_argv,
        ledger: ledger_of_front_pid,
        claims: |ledger| ledger.list().ok(),
    };

    /// Readers that read nothing (unit tests).
    pub(crate) const INERT: Self = Self {
        argv: |_| None,
        ledger: |_| None,
        claims: |_| None,
    };
}

/// THE BUILD EACH FRONT AMONG A PASS'S MEMBERS BECOMES, AND WHETHER ITS CLAIM WAITS
/// ([`aterm_uds::build_front::front`], which the engine names its row by, named as `status` names
/// it, [`crate::session_status::program::front_program`]: [`JobProc::becomes`]; and
/// [`atpkg::build_admit::front_claim_in`], which it waits on memory by: [`JobProc::claims_ahead`]).
/// Only a member that runs the front's image by its short name or its executable
/// ([`aterm_uds::build_front::FRONT_IMAGE`]) and whose thread list shows it ran is read, the
/// thread list asked again right before the read ([`read_front_argv`]): a read of a program's
/// arguments while it is held mid-start can hang (leftover M29). Its arguments are read once for
/// each program a pid runs (its start, its short name and its executable: an exec moves the last
/// two), and the ledger it claims in once a pass for every front that claims there.
pub(crate) struct Fronts {
    readers: FrontReaders,
    /// What each pid's program was read as, by the program it ran then.
    seen: HashMap<u32, (FrontKey, Option<FrontSeen>)>,
    /// This pass's claims of each ledger a front claims in, by its folder.
    listed: HashMap<std::path::PathBuf, Option<Vec<aterm_capacity::Record>>>,
}

/// Which program a pid runs, as a pass reads it: its start, its short name and its executable.
type FrontKey = (Option<u64>, String, Option<String>);

/// What a pass says of a front: the build it becomes, and while its claim waits the pids of the
/// claims ahead of it ([`JobProc::claims_ahead`]).
type FrontNow = (String, Option<Vec<u32>>);

impl Fronts {
    /// A reader of fronts through `readers`.
    pub(crate) fn new(readers: FrontReaders) -> Self {
        Self {
            readers,
            seen: HashMap::new(),
            listed: HashMap::new(),
        }
    }

    /// The build `p` becomes where it is a front, read once for the program it runs, and what its
    /// ledger says of its claim this pass: waiting behind the claims it names, or behind ones
    /// unknown where its ledger says nothing of it (none laid yet) or is unread, as `status`
    /// reads it ([`crate::session_status::program::BuildWait::of`]); held (admitted, handing it
    /// to its holder), no wait.
    fn front(&mut self, p: &ProcFacts) -> Option<FrontNow> {
        let image = aterm_uds::build_front::FRONT_IMAGE;
        if p.name != image && p.exe.as_deref() != Some(image) {
            return None;
        }
        let key = (p.start, p.name.clone(), p.exe.clone());
        let cached = self
            .seen
            .get(&p.pid)
            .filter(|(seen, _)| *seen == key)
            .map(|(_, front)| front.clone());
        let seen = match cached {
            Some(front) => front,
            None => {
                let front = (self.readers.argv)(p.pid)
                    .as_deref()
                    .and_then(crate::session_status::program::front_program)
                    .map(|becomes| FrontSeen {
                        becomes,
                        ledger: (self.readers.ledger)(p.pid),
                    });
                self.seen.insert(p.pid, (key, front.clone()));
                front
            }
        }?;
        let claims = self.readers.claims;
        let claim = seen.ledger.as_ref().and_then(|ledger| {
            self.listed
                .entry(ledger.dir().to_path_buf())
                .or_insert_with(|| claims(ledger))
                .as_deref()
                .and_then(|records| atpkg::build_admit::front_claim_in(records, p.pid))
        });
        let ahead = match claim {
            Some(atpkg::build_admit::FrontClaim::Waiting { ahead, .. }) => Some(ahead),
            Some(atpkg::build_admit::FrontClaim::Held) => None,
            None => Some(Vec::new()),
        };
        Some((seen.becomes, ahead))
    }

    /// Forget each pid `read` no longer holds, and this pass's ledgers.
    fn keep(&mut self, read: &[ProcFacts]) {
        let live: BTreeSet<u32> = read.iter().map(|p| p.pid).collect();
        self.seen.retain(|pid, _| live.contains(pid));
        self.listed.clear();
    }
}

/// `pid`'s arguments, read only once its thread list, asked right before the read, shows it ran:
/// a read of a program's arguments while it is held mid-start can hang (leftover M29), and an
/// exec since the pass's reading may have made this pid one.
fn read_front_argv(pid: u32) -> Option<Vec<OsString>> {
    let id = i32::try_from(pid).ok()?;
    if aterm_uds::process::thread_ran(id) != Some(true) {
        return None;
    }
    aterm_uds::process::read_process_argv(id)
}

/// The ledger the front `pid` claims in ([`atpkg::build_admit::ledger_of_front`]): this user's,
/// or in a debug build the one its own environment names.
fn ledger_of_front_pid(pid: u32) -> Option<aterm_capacity::Ledger> {
    atpkg::build_admit::ledger_of_front(&|key| {
        atpkg::caller_shell::process_args(pid)?
            .env_var(key)
            .map(str::to_owned)
    })
}

/// Whether member reading `p` shows it ran past its start: its thread list read, and not the lone
/// `0` of a thread that never ran; never `stand_in`, which a development build reads as held.
fn ran(p: &ProcFacts, stand_in: Option<u32>) -> bool {
    stand_in != Some(p.pid) && p.start_facts.as_ref().is_some_and(|f| !f.lone_zero)
}

/// What the host knows of aterm's Developer Tools grant at a reading, which the engine sets
/// against a held program's fork time (`aterm_vitals::jobs::Findings::program_check_after_grant`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GrantFacts {
    /// When the grant took effect, unix ms (the end of the last verdict refresh that took,
    /// `devtools_helper::Lane::took_at`), only while aterm reads granted.
    pub(crate) grant_ms: Option<u64>,
    /// The sessions another aterm process handed over (`consent_state::session_attribution` reads
    /// `Adopted`): macOS may check their new programs whatever the grant says (the
    /// leftover-processes design's M13).
    pub(crate) adopted: Vec<u64>,
}

/// The engine, its reader and the last view, on the probe thread.
pub(crate) struct JobsPass {
    jobs: Jobs,
    reader: JobsReader,
    held: HeldReaders,
    /// Each pid the passes have read unrun, by its run ([`date`]).
    unrun: Unrun,
    fronts: Fronts,
    process_limit: Option<u64>,
    /// When the next pass is due, host ms (`None`: at the next reading).
    next_ms: Option<u64>,
    view: Option<Arc<JobsView>>,
    /// The last requested jobs pass read a sweep rather than returning its cached view.
    naming_fresh: bool,
    /// The tab a session-mate's environment ties each program outside every job that has not
    /// started to, by pid, as the leftover pass last judged it ([`Self::tie_held`]).
    ties: BTreeMap<u32, aterm_vitals::left::Tie>,
    /// The kernel's exit watches on agent calls' leaders, whose statuses each pass hands the
    /// engine ([`crate::telemetry_host::CallWatch`]).
    calls: crate::telemetry_host::CallWatch,
}

impl JobsPass {
    pub(crate) fn new() -> Self {
        Self {
            jobs: Jobs::new(),
            reader: JobsReader::new(),
            held: if cfg!(test) {
                HeldReaders::INERT
            } else {
                HeldReaders::KERNEL
            },
            unrun: Unrun::new(),
            fronts: Fronts::new(if cfg!(test) {
                FrontReaders::INERT
            } else {
                FrontReaders::LIVE
            }),
            process_limit: probe::process_limit(),
            next_ms: None,
            view: None,
            naming_fresh: false,
            ties: BTreeMap::new(),
            calls: crate::telemetry_host::CallWatch::new(!cfg!(test)),
        }
    }

    /// Takes the leftover pass's ties of programs that have not started to live tabs
    /// ([`crate::vitals_left::LeftPass::ties`]): the next pass names each such program by its
    /// tab, and by an agent's call where it was born after that tab's agent started (as the judge
    /// reads an agent's own program, [`aterm_vitals::left::Tie::after_agent`]), which its job's
    /// tab and kind, where it has a job, override in the engine.
    pub(crate) fn tie_held(&mut self, ties: BTreeMap<u32, aterm_vitals::left::Tie>) {
        self.ties = ties;
    }

    /// Captures a naming ticket only from the jobs pass the host just forced.
    pub(crate) fn capture_name(
        &self,
        session: u64,
        target: &aterm_vitals::sys::query::NameTarget,
    ) -> Result<NamingTicket, NamingError> {
        if !self.naming_fresh {
            return Err(NamingError::Unread);
        }
        let selector = match target {
            aterm_vitals::sys::query::NameTarget::Job(id) => NamingSelector::Job(id),
            aterm_vitals::sys::query::NameTarget::Task(id) => NamingSelector::Task(id),
        };
        self.jobs.naming_target(session, selector)
    }

    /// Applies a sampled word after another actual jobs pass, never a cached reading.
    pub(crate) fn apply_name(
        &mut self,
        ticket: &NamingTicket,
        word: TierB,
    ) -> Result<(), NamingError> {
        if !self.naming_fresh {
            return Err(NamingError::Unread);
        }
        self.jobs.accept_name(ticket, word)
    }

    /// The final jobs pass still publishes this ticket's accepted name.
    pub(crate) fn name_is_shown(&self, ticket: &NamingTicket, word: TierB) -> bool {
        self.naming_fresh && self.jobs.name_is_shown(ticket, word)
    }

    /// One reading's pass: runs the engine when it is due, or whenever `now` (an edge's reading:
    /// the loop paces those on its own clock, and one landing inside this pass's cadence must
    /// still look, or the job it was asked for goes unseen until the next edge). Returns the view
    /// to publish, the last one when the engine did not run.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tick(
        &mut self,
        at_ms: u64,
        read_ms: impl Fn() -> u64,
        sweep: Option<&[ProcessReading]>,
        tabs: &[TabRoot],
        ttys: &[(u64, TtyMode)],
        feeds: &[(u64, TabFeed)],
        grant: &GrantFacts,
        self_pid: u32,
        thrashing: bool,
        now: bool,
    ) -> Option<Arc<JobsView>> {
        self.naming_fresh = false;
        let Some(sweep) = sweep else {
            return self.view.clone();
        };
        if !now && self.next_ms.is_some_and(|due| at_ms < due) {
            return self.view.clone();
        }
        let self_uid = sweep.iter().find(|p| p.pid == self_pid).and_then(|p| p.uid);
        let readings = sweep;
        let sweep: Vec<SweepProc> = sweep.iter().map(sweep_proc).collect();
        let tabs = job_tabs(tabs, ttys, feeds, &grant.adopted);
        let reqs: Vec<ReadReq> = self
            .jobs
            .plan(&sweep, &tabs)
            .iter()
            .map(|w| ReadReq {
                pid: w.pid,
                fds: w.fds,
            })
            .collect();
        let readers = self.held;
        let stand_in = (readers.stand_in)();
        // Each age is the probe's as its reading starts.
        let probe_ms = read_ms();
        // One wall clock for every start the pass reads, so each fork time is its age before it.
        let now_us = (readers.now_us)();
        let read = self.reader.read_all_at(&reqs, now_us);
        let roots: BTreeSet<u32> = tabs.iter().map(|t| t.shell_pid).collect();
        let fronts = &mut self.fronts;
        let procs = job_procs(&read, &roots, stand_in, readers.exec, |p| fronts.front(p));
        fronts.keep(&read);
        let clock = &read_ms;
        let read_ms = clock();
        let mounts = OnceCell::new();
        let ours = OnceCell::new();
        let (mut held, young) = held_list(
            readings,
            &read,
            self_uid,
            stand_in,
            now_us,
            |pid| (readers.start)(pid, now_us, &mounts),
            readers.exec,
            |pid| {
                let ours = *ours.get_or_init(|| (readers.coalition)(self_pid));
                ours.is_some() && (readers.coalition)(pid) == ours
            },
            readers.exe_name,
        );
        name_tied(held.iter_mut().map(|h| &mut h.row), &self.ties);
        let mut unrun = date(&mut held, &young, &self.unrun, now_us);
        let (program_check_daemon, program_check_judged) =
            strand(&mut held, (readers.check)(), now_us, readers.backlog);
        for h in &held {
            if let (Some(by), Some(run)) = (h.stranded_by, unrun.get_mut(&h.row.pid)) {
                run.stranded_by = Some(by);
            }
        }
        self.unrun = unrun;
        let lanes = self.reader.lanes();
        // The statuses of watched calls' leaders read since the last pass, each at its own moment
        // on this pass's clock: read after the sweep, so a leader it no longer lists has left its
        // event or is waited on briefly for it.
        let exits = self
            .calls
            .read(|pid| sweep.iter().any(|p| p.pid == pid), clock);
        let tick = JobTick {
            at_ms,
            read_ms,
            probe_ms,
            self_uid,
            sweep,
            procs,
            tabs,
            machine: Machine {
                stretch_milli: crate::input_stall::machine_contention().factor_milli(),
                thrashing,
            },
            lanes: LanesSeen {
                kqueue_detail: lanes.kqueue_detail.is_on(),
                pipe_flags: lanes.pipe_flags.is_on(),
            },
            process_limit: self.process_limit,
            held: held.into_iter().map(|h| h.row).collect(),
            program_check_daemon,
            program_check_judged,
            grant_ms: grant.grant_ms,
            exits,
        };
        let view = self.jobs.absorb(&tick);
        crate::telemetry_host::jobs_passed(&mut self.jobs, at_ms, &self.calls);
        self.calls.watch(&view.rows);
        self.naming_fresh = true;
        self.next_ms = Some(at_ms + next_pass_ms(view.demand));
        self.view = Some(Arc::new(view));
        self.view.clone()
    }
}

/// Names each program that has not started by the leftover pass's tie of it
/// ([`JobsPass::tie_held`]): the tie's tab, and an agent's call where it was born after that tab's
/// agent started; the engine prefers a job's own tab and kind, where the program has a job.
fn name_tied<'a>(
    held: impl IntoIterator<Item = &'a mut HeldProc>,
    ties: &BTreeMap<u32, aterm_vitals::left::Tie>,
) {
    for h in held {
        let tie = ties.get(&h.pid);
        h.tab = tie.map(|t| t.key);
        h.job = tie.filter(|t| t.after_agent).map(|_| JobKind::AgentCall);
    }
}

/// A program of this user that has not started, as one pass reads it ([`held_list`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// The engine's row; [`strand`] sets its `stranded`.
    pub(crate) row: HeldProc,
    /// What its start read; `None` for the stand-in, read held unasked, which is never stranded.
    pub(crate) facts: Option<StartFacts>,
    /// Its CPU as this pass read it, ns; `None` where unread.
    pub(crate) cpu_ns: Option<u64>,
    /// A time its exec had come by, µs since the epoch ([`date`]); `None` until dated.
    pub(crate) sent_by_us: Option<u64>,
    /// The syspolicyd that was running when a pass judged it stranded, kept while the passes
    /// read it unrun ([`date`], [`strand`]).
    pub(crate) stranded_by: Option<u32>,
}

/// A row of this user a pass read unrun, neither stopped nor traced, that it does not list held:
/// younger than [`probe::PROGRAM_CHECK_AGE_MS`], or of an age unread. [`date`] dates it all the
/// same, so a check sent before a syspolicyd started dates from the first pass that read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unlisted {
    /// Its pid.
    pub(crate) pid: u32,
    /// Its pid version, bound to a fresh read that found it still unrun ([`bound_version`]).
    pub(crate) exec: u64,
    /// Its CPU as this pass read it, ns.
    pub(crate) cpu_ns: u64,
}

/// Whether a start that reads `f` stands unrun in a way a check can hold: its one thread never
/// ran ([`StartFacts::lone_zero`]), and it reads neither stopped nor traced, since a program
/// stopped at its start may send its check only once it is let go.
const fn unrun(f: &StartFacts) -> bool {
    f.lone_zero && !f.stopped && !f.traced
}

/// This user's programs that have not started, members of a job or not: each member of `members`
/// (judged from its own thread list, [`member_start`]) that reads held, translating, unstarted, or
/// unstarted and stopped or traced (any [`AtStart`] but `Started`), and each other row of `sweep`
/// of `self_uid` that is no zombie, has used at most [`HELD_CPU_NS`] (or is `stand_in`, held
/// whatever its CPU) and whose `start` facts judge so. Each is keyed by its pid version (`exec`,
/// else its start, so an exec starts a new held time), named by its executable (a member's own
/// reading, else `exe_name`, else its short name), is aterm's when it is a member or `in_aterm`
/// (aterm's coalition) says so, and forked its start facts' age before `now_us`, the clock the
/// members' reading and `start` took those ages at ([`fork_ms`]): a member's own reading's facts,
/// else the first `start` read's (the stand-in outside every job, read held unasked, has none).
/// An actual exec version is bound to a fresh `start` read of each listed row between equal
/// versions; the stand-in supplies its own held reading. Each listed row keeps those facts (none
/// for the stand-in) and its CPU (a member's counters, else the sweep's), undated. Beside them,
/// each such member or row its facts read [`unrun`] that is not listed, whose CPU was read and
/// whose pid version binds to a fresh `start` read that finds it still unrun ([`Unlisted`]).
/// `exe_name` is asked only of listed rows outside every job.
#[allow(clippy::too_many_arguments)]
pub(crate) fn held_list(
    sweep: &[ProcessReading],
    members: &[ProcFacts],
    self_uid: Option<u32>,
    stand_in: Option<u32>,
    now_us: u64,
    mut start: impl FnMut(u32) -> Option<StartFacts>,
    exec: impl Fn(u32) -> Option<u64>,
    in_aterm: impl Fn(u32) -> bool,
    exe_name: impl Fn(u32) -> Option<String>,
) -> (Vec<Held>, Vec<Unlisted>) {
    let Some(uid) = self_uid else {
        return (Vec::new(), Vec::new());
    };
    let mut young = Vec::new();
    let mut unlisted = |pid: u32,
                        facts: Option<&StartFacts>,
                        cpu_ns: Option<u64>,
                        start: &mut dyn FnMut(u32) -> Option<StartFacts>| {
        let Some(cpu_ns) = cpu_ns.filter(|_| facts.is_some_and(unrun)) else {
            return;
        };
        if let Some(version) = bound_version(
            exec(pid),
            || start(pid).as_ref().is_some_and(unrun),
            || exec(pid),
        ) {
            young.push(Unlisted {
                pid,
                exec: version,
                cpu_ns,
            });
        }
    };
    let read: BTreeSet<u32> = members.iter().map(|m| m.pid).collect();
    let mut held: Vec<Held> = Vec::new();
    for m in members.iter().filter(|m| m.uid == Some(uid)) {
        let Some(at) = member_start(m, stand_in) else {
            continue;
        };
        let cpu_ns = m.counters.map(|c| c.cpu_ns);
        let Some(wait) = start_wait(at) else {
            unlisted(m.pid, m.start_facts.as_ref(), cpu_ns, &mut start);
            continue;
        };
        let version = exec(m.pid);
        let exec_version = version_at_wait(
            version,
            wait,
            || {
                if stand_in == Some(m.pid) {
                    Some(StartWait::ProgramCheck)
                } else {
                    start(m.pid).and_then(|f| start_wait(probe::judge_start(&f)))
                }
            },
            || exec(m.pid),
        );
        held.push(Held {
            row: HeldProc {
                pid: m.pid,
                exec: version.or(m.start).unwrap_or(0),
                exec_version,
                name: m.exe.clone().unwrap_or_else(|| m.name.clone()),
                aterm: true,
                wait,
                tab: None,
                job: None,
                started_ms: m.start_facts.as_ref().and_then(|f| fork_ms(f, now_us)),
                stranded: false,
            },
            facts: m.start_facts.filter(|_| stand_in != Some(m.pid)),
            cpu_ns,
            sent_by_us: None,
            stranded_by: None,
        });
    }
    for p in sweep.iter().filter(|p| {
        p.uid == Some(uid)
            && p.state != probe::RunState::Zombie
            && (p.cpu_ns.is_some_and(|c| c <= HELD_CPU_NS) || stand_in == Some(p.pid))
            && !read.contains(&p.pid)
    }) {
        let (at, facts) = if stand_in == Some(p.pid) {
            (Some(AtStart::Held), None)
        } else {
            let facts = start(p.pid);
            (facts.as_ref().map(probe::judge_start), facts)
        };
        let Some(wait) = at.and_then(start_wait) else {
            unlisted(p.pid, facts.as_ref(), p.cpu_ns, &mut start);
            continue;
        };
        let version = exec(p.pid);
        let exec_version = version_at_wait(
            version,
            wait,
            || {
                if stand_in == Some(p.pid) {
                    Some(StartWait::ProgramCheck)
                } else {
                    start(p.pid).and_then(|f| start_wait(probe::judge_start(&f)))
                }
            },
            || exec(p.pid),
        );
        held.push(Held {
            row: HeldProc {
                pid: p.pid,
                exec: version.or(p.start).unwrap_or(0),
                exec_version,
                name: exe_name(p.pid).unwrap_or_else(|| p.name.clone()),
                aterm: in_aterm(p.pid),
                wait,
                tab: None,
                job: None,
                started_ms: facts.as_ref().and_then(|f| fork_ms(f, now_us)),
                stranded: false,
            },
            facts,
            cpu_ns: p.cpu_ns,
            sent_by_us: None,
            stranded_by: None,
        });
    }
    (held, young)
}

/// What the passes have read of one pid unrun ([`date`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    /// The pid version every pass of the run read it as ([`HeldProc::exec_version`], an
    /// [`Unlisted::exec`]).
    exec: u64,
    /// The CPU, ns, every pass of the run read it at.
    cpu: u64,
    /// The first such pass's wall clock, µs since the epoch.
    since_us: u64,
    /// The syspolicyd that was running when a pass of the run judged it stranded ([`strand`]).
    stranded_by: Option<u32>,
}

/// Each pid the passes have read unrun, by its run ([`Run`]).
type Unrun = BTreeMap<u32, Run>;

/// Dates each of `held` whose start read [`unrun`], with its CPU and its actual pid version
/// ([`HeldProc::exec_version`]) read, as verify's watch dates a stage child's members: its
/// [`Held::sent_by_us`] is `was`'s time for it when the pass before also read it unrun as the
/// same version at the same CPU, else `now_us`, and its [`Held::stranded_by`] that run's. Each of
/// `young`, read unrun but not yet held, is dated the same way, so a row is dated from the first
/// pass that read it unrun, held or not. A row that ran, went unread, read stopped or traced, or
/// reads another version or CPU starts over, since it may have exec'd again; its fork dates
/// nothing, since an exec in the same pid keeps it. Returns the runs the next pass reads as `was`.
fn date(held: &mut [Held], young: &[Unlisted], was: &Unrun, now_us: u64) -> Unrun {
    let mut runs = Unrun::new();
    let mut run = |pid: u32, exec: u64, cpu: u64| -> Run {
        let run = match was.get(&pid) {
            Some(r) if r.exec == exec && r.cpu == cpu => *r,
            _ => Run {
                exec,
                cpu,
                since_us: now_us,
                stranded_by: None,
            },
        };
        runs.insert(pid, run);
        run
    };
    for h in held {
        let (Some(facts), Some(cpu), Some(exec)) = (h.facts, h.cpu_ns, h.row.exec_version) else {
            continue;
        };
        if !unrun(&facts) {
            continue;
        }
        let r = run(h.row.pid, exec, cpu);
        h.sent_by_us = Some(r.since_us);
        h.stranded_by = r.stranded_by;
    }
    for y in young {
        run(y.pid, y.exec, y.cpu_ns);
    }
    runs
}

/// The stranded judge over one pass's `held` at `now_us` (leftover §5.1). A row a pass of its run
/// judged stranded stays so ([`Held::stranded_by`]): where its check went does not change with
/// what a later pass reads. Against `check`, the stuck check's fresh look, while its pool reads
/// not full, each other row is [`probe::stranded`] by what its start read and its date ([`date`])
/// against that daemon's start, its age read on the awake clock (the lesser of its wall-clock age
/// and [`RunningCheck::awake_ms`], so a sleep counts for nothing) and the backlog being how many
/// of `held` are held and forked at or after that start
/// ([`aterm_uds::process::held_since_among`]); a row that would be is only once `backlog`, every
/// held process of this user so forked, reads 0, which is asked only then. Returns the
/// syspolicyd the verdicts stand against ([`aterm_vitals::jobs::JobTick::program_check_daemon`]:
/// the one the look read, else the one a kept row was judged under) and whether every row the
/// check holds was judged ([`aterm_vitals::jobs::JobTick::program_check_judged`]): the look's
/// pool reads not full, and each is stranded or forked at or after that start, its check the
/// running one's. Any other may be stranded or in the running one's line, and nothing read tells
/// which. Nothing it reads feeds a signal: the host ends no held program.
fn strand(
    held: &mut [Held],
    check: Option<RunningCheck>,
    now_us: u64,
    backlog: impl FnOnce(u64) -> Option<usize>,
) -> (Option<u32>, bool) {
    for h in held.iter_mut() {
        h.row.stranded = h.stranded_by.is_some();
    }
    let alone = check.filter(|c| !c.pool_full);
    if let Some(c) = alone {
        let start = c.daemon.started_us();
        let up_us = now_us
            .saturating_sub(start)
            .min(c.awake_ms.saturating_mul(1_000));
        let pids: Vec<u32> = held.iter().map(|h| h.row.pid).collect();
        let facts: BTreeMap<u32, StartFacts> = held
            .iter()
            .filter_map(|h| Some((h.row.pid, h.facts?)))
            .collect();
        let own =
            aterm_uds::process::held_since_among(&pids, |pid| facts.get(&pid).copied(), start);
        let fresh: Vec<usize> = (0..held.len())
            .filter(|&i| {
                let h = &held[i];
                !h.row.stranded
                    && h.facts.is_some_and(|f| {
                        probe::stranded(
                            &f,
                            h.sent_by_us,
                            start,
                            start.saturating_add(up_us),
                            Some(false),
                            Some(own),
                        )
                    })
            })
            .collect();
        if !fresh.is_empty() && backlog(start) == Some(0) {
            for i in fresh {
                held[i].row.stranded = true;
                held[i].stranded_by = Some(c.daemon.pid);
            }
        }
    }
    let judged = |h: &Held| {
        h.row.wait != StartWait::ProgramCheck
            || h.row.stranded
            || alone.is_some_and(|c| {
                h.facts
                    .and_then(|f| f.start_us)
                    .is_some_and(|fork| fork >= c.daemon.started_us())
            })
    };
    let by = check
        .map(|c| c.daemon.pid)
        .or_else(|| held.iter().find_map(|h| h.stranded_by));
    (by, alone.is_some() && held.iter().all(judged))
}

/// When a process whose start reads `facts`, its age taken at `now_us` (µs since the epoch),
/// forked: unix ms; `None` with its age unread.
fn fork_ms(facts: &StartFacts, now_us: u64) -> Option<u64> {
    facts.age_ms.map(|age| (now_us / 1000).saturating_sub(age))
}

/// Binds a fresh wait observation to one actual exec. Cached member facts alone cannot bind it:
/// the process may have execed between those facts and the first version read.
fn version_at_wait(
    before: Option<u64>,
    wait: StartWait,
    current_wait: impl FnOnce() -> Option<StartWait>,
    after: impl FnOnce() -> Option<u64>,
) -> Option<u64> {
    bound_version(before, || current_wait() == Some(wait), after)
}

/// The version read `before` when a fresh read between it and a second version read `after`
/// still `holds` and that second read equals it; `None` otherwise ([`version_at_wait`], an
/// [`Unlisted`] row's version).
fn bound_version(
    before: Option<u64>,
    holds: impl FnOnce() -> bool,
    after: impl FnOnce() -> Option<u64>,
) -> Option<u64> {
    let version = before?;
    (holds() && after() == Some(version)).then_some(version)
}

/// What keeps a process that stands `at` its start from starting, as the engine names it; `None`
/// for one that started.
const fn start_wait(at: AtStart) -> Option<StartWait> {
    match at {
        AtStart::Started => None,
        AtStart::Held => Some(StartWait::ProgramCheck),
        AtStart::Translating => Some(StartWait::Translating),
        AtStart::Unstarted => Some(StartWait::NotStarted),
        AtStart::StoppedUnstarted { stopped, traced } => {
            Some(StartWait::StoppedUnstarted { stopped, traced })
        }
    }
}

/// Where member reading `p` stands at its start ([`ProcFacts::program_check`]). `stand_in` (a
/// development build's `program-check held <pid>`) reads held only where the reading judged a
/// start, as the kernel's own held reading needs: never for a zombie or another user's process,
/// whose thread list is not read.
fn member_start(p: &ProcFacts, stand_in: Option<u32>) -> Option<AtStart> {
    let judged = p.program_check();
    if stand_in == Some(p.pid) && judged.is_some() {
        Some(AtStart::Held)
    } else {
        judged
    }
}

/// How old the board's view may be and still speak for the canary ([`JobsBoard::evidence`]).
pub(crate) const JOBS_FRESH: Duration = Duration::from_secs(60);

/// Whether a jobs view `age` old, read with the leftover view `left`, speaks for now: one at most
/// [`JOBS_FRESH`] old does, and so does one that no timer reads again ([`read_on_a_timer`]) of any
/// age while its reader runs (`reader`). Such an engine is read again only on an edge (output that
/// moves a screen, a parked screen's redraw at most once a floor, a session's fork, exec or exit,
/// the disk watch's look) or a pull, so no reading since means nothing that draws or that the
/// kernel tells of changed; its quiet ages are told on to now. A start raises no edge, and a view
/// that holds a wait or a suspect agent call, or a reading whose leftover view offers a row, is
/// read on a timer instead, so past [`JOBS_FRESH`] its reader has stopped delivering.
pub(crate) fn speaks_for_now(
    view: &JobsView,
    left: Option<&LeftView>,
    age: Duration,
    reader: bool,
) -> bool {
    age <= JOBS_FRESH || (reader && !read_on_a_timer(view, left))
}

/// Whether a reading no window shows is due again on a timer after one that gave `view` and `left`:
/// within Slow's 30 s ([`held_on_slow`]), or a live agent call stands suspect
/// ([`windowless_after`]). A live job's own demand is no timer with no window.
pub(crate) fn read_on_a_timer(view: &JobsView, left: Option<&LeftView>) -> bool {
    held_on_slow(view, left) || windowless_after(view).is_some()
}

/// Whether a reading no window shows is due again within Slow's 30 s after one that gave `view`
/// and `left` (`vitals_host::held_wait_after`), for what no edge tells: `view`
/// [`holds_a_wait`], since a start raises no edge, or `left` offers a row whose owner is gone
/// ([`aterm_vitals::left::holds_an_offer`]), whose next reading no edge of a live session brings.
pub(crate) fn held_on_slow(view: &JobsView, left: Option<&LeftView>) -> bool {
    holds_a_wait(view) || left.is_some_and(aterm_vitals::left::holds_an_offer)
}

/// Whether `view` names a program of this user that has not started (held by macOS's program
/// check, translating under Rosetta, on a volume not proven local, or reading stopped or traced),
/// or stands in a backed-up episode that has not had its closing reading. Such a view is read
/// again within 30 s whatever its demand (`vitals_host::held_wait_after`), since a start (a
/// debugger's resume included) raises no edge.
pub(crate) fn holds_a_wait(view: &JobsView) -> bool {
    let f = &view.findings;
    !f.program_check.is_empty()
        || !f.translating.is_empty()
        || !f.not_started.is_empty()
        || !f.stopped_unstarted.is_empty()
        || f.program_check_episode.is_some()
}

/// `privacy`'s count of this user's programs at macOS's program check ([`program_check_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldCount {
    /// How many it holds: a lower bound, aterm's and others'.
    pub(crate) held: u32,
    /// The longest held to now, ms: its first sighting's age plus the view's.
    pub(crate) oldest_ms: u64,
    /// How many of them are stranded ([`aterm_vitals::jobs::Findings::program_check_stranded`]);
    /// `None` with none named while the view did not judge every held row
    /// ([`aterm_vitals::jobs::Findings::program_check_judged`]).
    pub(crate) stranded: Option<u32>,
}

/// `privacy`'s `program_check=` from `view`, `age` old ([`HeldCount`]); `None` when the view, read
/// with the leftover view `left`, does not speak for now ([`speaks_for_now`]).
pub(crate) fn program_check_of(
    view: &JobsView,
    left: Option<&LeftView>,
    age: Duration,
    reader: bool,
) -> Option<HeldCount> {
    if !speaks_for_now(view, left, age, reader) {
        return None;
    }
    let f = &view.findings;
    let oldest = f.program_check.iter().map(|n| n.for_ms).max().unwrap_or(0);
    let age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
    let count = |list: &[aterm_vitals::jobs::Named]| u32::try_from(list.len()).unwrap_or(u32::MAX);
    Some(HeldCount {
        held: count(&f.program_check),
        oldest_ms: oldest.saturating_add(age_ms),
        stranded: f.program_check_daemon.and_then(|_| {
            let n = count(&f.program_check_stranded);
            (n > 0 || f.program_check_judged).then_some(n)
        }),
    })
}

/// The view last posted, its tabs' session ids, and when it was first posted.
struct Posted {
    view: Arc<JobsView>,
    sids: BTreeMap<u64, String>,
    at: Instant,
}

/// The view the loop last adopted, with its tabs' session ids, read by the harness workers' stall
/// canary ([`JobsBoard::evidence`]), and the leftover view of the same snapshot
/// ([`JobsBoard::leftover`]); each one leaf-locked slot.
pub(crate) struct JobsBoard {
    slot: Mutex<Option<Posted>>,
    left: Mutex<Option<Arc<LeftView>>>,
}

impl JobsBoard {
    /// An empty board: every session's evidence is `Unknown`, and no left row is read.
    pub(crate) const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            left: Mutex::new(None),
        }
    }

    /// Post the leftover view of the snapshot the loop adopted (`None` where it carries none).
    pub(crate) fn post_left(&self, left: Option<Arc<LeftView>>) {
        *self.left.lock().unwrap_or_else(PoisonError::into_inner) = left;
    }

    /// WHAT SESSION `sid`'S AGENT MAY BE TOLD of the processes it left running at `now_ms` (unix
    /// ms; `harness_host::WorkerIdle::facts`): its left rows in the newest leftover view posted,
    /// each re-proved by `lives` after the lock is let go ([`leftover_reading`]); `None` with none
    /// posted. Only the `Arc`'s copy runs under the lock.
    pub(crate) fn leftover(
        &self,
        sid: &str,
        now_ms: i64,
        lives: fn(&str) -> bool,
    ) -> Option<aterm_agent::supervise::LeftoverReading> {
        let view = self
            .left
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        leftover_reading(&view, sid, now_ms, lives)
    }

    /// Post `view`, its tabs' session ids `sids`, read at `now`.
    pub(crate) fn post(&self, view: Arc<JobsView>, sids: BTreeMap<u64, String>, now: Instant) {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let at = match slot.as_ref() {
            Some(p) if Arc::ptr_eq(&p.view, &view) => p.at,
            _ => now,
        };
        *slot = Some(Posted { view, sids, at });
    }

    /// What the view says of session `sid`'s agent's background work at `now`:
    /// [`BackgroundEvidence::NoProgress`] with the call quiet longest when every live agent call of
    /// the session's tab that is background, or not known to be foreground (`Consumer::Agent`),
    /// reads `no_progress`. Else `Unknown`: no view, one older than [`JOBS_FRESH`], no call of this
    /// session's, or a live call that does not read `no_progress`, which may be the work the screen
    /// waits on.
    pub(crate) fn evidence(&self, sid: &str, now: Instant) -> BackgroundEvidence {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(posted) = slot
            .as_ref()
            .filter(|p| now.saturating_duration_since(p.at) <= JOBS_FRESH)
        else {
            return BackgroundEvidence::Unknown;
        };
        let calls = || {
            posted
                .view
                .rows
                .iter()
                .filter(|r| posted.sids.get(&r.tab).is_some_and(|s| s == sid))
                .filter(|r| background_call_live(r))
        };
        if calls().any(|r| !r.no_progress) {
            return BackgroundEvidence::Unknown;
        }
        calls()
            .max_by_key(|r| r.quiet_ms.unwrap_or(0))
            .map_or(BackgroundEvidence::Unknown, |r| {
                BackgroundEvidence::NoProgress {
                    worst: background_call(r),
                }
            })
    }

    /// What session `sid`'s agent may be told at `now` (`harness_host::WorkerIdle::facts`): every
    /// live agent call of the session's tab ([`background_call_live`]) that reads `no_progress`,
    /// each regardless of the others (unlike [`Self::evidence`]), worst (quiet longest) first, with
    /// its leader's pid for the caller's re-proof, and the view's age; `None` with no view or one
    /// older than [`JOBS_FRESH`]. A wrapper with observed work in a descendant call is omitted.
    /// Only the copy of the rows runs under the lock.
    pub(crate) fn facts(
        &self,
        sid: &str,
        now: Instant,
    ) -> Option<(Duration, Vec<(u32, BackgroundCall)>)> {
        let (age, mut calls) = {
            let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            let posted = slot
                .as_ref()
                .filter(|p| now.saturating_duration_since(p.at) <= JOBS_FRESH)?;
            let calls: Vec<(u64, u32, BackgroundCall)> = posted
                .view
                .rows
                .iter()
                .filter(|r| posted.sids.get(&r.tab).is_some_and(|s| s == sid))
                .filter(|r| {
                    background_call_live(r) && r.no_progress && r.working_descendant.is_none()
                })
                .map(|r| (r.quiet_ms.unwrap_or(0), r.leader.0, background_call(r)))
                .collect();
            (now.saturating_duration_since(posted.at), calls)
        };
        calls.sort_by_key(|c| std::cmp::Reverse(c.0));
        Some((
            age,
            calls
                .into_iter()
                .map(|(_, leader, call)| (leader, call))
                .collect(),
        ))
    }

    /// THE PROGRAMS OF SESSION `sid`'S AGENT'S CALLS THAT MACOS'S PROGRAM CHECK HOLDS, as the view
    /// says at `now` (`docs/DESIGN-leftover-processes-2026-10-16.md` §6.3;
    /// `harness_host::WorkerIdle::facts`): the view's held rows (`Findings.program_check`) of this
    /// session's tab named [`JobKind::AgentCall`], members of one of its agent's calls or of what
    /// one left (an orphan unit is named by the job it outlived), and the held rows outside every
    /// job that a session-mate's environment ties to this tab and that were born after its agent
    /// started, which the tie names the same (W-session, [`JobsPass::tie_held`]: a program born
    /// and orphaned between two readings), oldest (held longest) first, each held
    /// for its `for_ms` as of the view, with the view's age; `None` with no view or one older than
    /// [`JOBS_FRESH`]. A held member of the shell's own jobs, or of what one left, is the
    /// person's, never the agent's, and so is one in a nested terminal session
    /// (`JobKind::Nested`: sudo's, `script`'s, or an agent's pty call, whose consumer the engine
    /// names a person). The rows are copied under the lock and nothing else is done there.
    pub(crate) fn held(&self, sid: &str, now: Instant) -> Option<(Duration, Vec<HeldCall>)> {
        let (age, mut held) = {
            let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            let posted = slot
                .as_ref()
                .filter(|p| now.saturating_duration_since(p.at) <= JOBS_FRESH)?;
            let held: Vec<HeldCall> = posted
                .view
                .findings
                .program_check
                .iter()
                .filter(|n| {
                    n.tab
                        .and_then(|tab| posted.sids.get(&tab))
                        .is_some_and(|s| s == sid)
                })
                .filter(|n| n.job == Some(JobKind::AgentCall))
                .map(|n| HeldCall {
                    pid: n.pid,
                    exec_version: n.exec_version,
                    held_ms: n.for_ms,
                })
                .collect();
            (now.saturating_duration_since(posted.at), held)
        };
        held.sort_by_key(|h| std::cmp::Reverse(h.held_ms));
        Some((age, held))
    }
}

/// The agent's left rows of session `sid` in `view` as the facts seam reads them at `now_ms` (unix
/// ms): the rows [`aterm_vitals::left::agents_left_read`] gives (the ones `status left=` counts),
/// each re-proved at the ask by `lives` on its token ([`left_lives`] live: the process it names is
/// that one still), as the held part's programs are, so a fact is told from a part as of now
/// whatever the view's age. How many it proves, the oldest's age told on by the view's age, as of
/// `now_ms`. A view that names none is a part with none, as of the view; a partial view (its sweep
/// stopped at its cap, or its reading at its bound of new candidates) can say a row is left but
/// never that none is, and a re-proof that proves none of its rows proves no absence: each is a
/// part unread (`None`), which neither opens nor closes an episode.
pub(crate) fn leftover_reading(
    view: &LeftView,
    sid: &str,
    now_ms: i64,
    lives: fn(&str) -> bool,
) -> Option<aterm_agent::supervise::LeftoverReading> {
    let rows = aterm_vitals::left::agents_left_read(view, sid)?;
    let as_of_ms = i64::try_from(view.as_of_ms).ok()?;
    if rows.is_empty() {
        return Some(aterm_agent::supervise::LeftoverReading {
            as_of_ms,
            count: 0,
            oldest_ms: None,
        });
    }
    let age_ms = now_ms.saturating_sub(as_of_ms).max(0).unsigned_abs();
    let proved: Vec<&LeftRow> = rows
        .into_iter()
        .filter(|r| r.token.as_deref().is_some_and(lives))
        .collect();
    (!proved.is_empty()).then(|| aterm_agent::supervise::LeftoverReading {
        as_of_ms: now_ms,
        count: u32::try_from(proved.len()).unwrap_or(u32::MAX),
        oldest_ms: proved
            .iter()
            .filter_map(|r| r.started_ms)
            .max()
            .map(|ms| ms.saturating_add(age_ms)),
    })
}

/// Whether the process a left row's `token` names is that one still (the leftover part's
/// re-proof at the ask, [`leftover_reading`]): its identity read now (`end`'s own read,
/// [`aterm_uds::process::read_identity`]) is the token's, pid, birth and, where the kernel keeps
/// one, pid version, so an ended process, a reused pid and an exec since are not.
pub(crate) fn left_lives(token: &str) -> bool {
    aterm_uds::process::Identity::parse(token)
        .is_some_and(|id| aterm_uds::process::read_identity(id.pid).is_some_and(|now| now == id))
}

/// Whether macOS's program check still holds the same exec that the view timed (the facts'
/// re-proof at the ask, `harness_host::WorkerIdle::facts`). Its actual version must match before
/// and after the held and age reads, and its process must be no younger than its told wait. A
/// reused pid or a new exec in the same process cannot inherit the old exec's wait. An
/// unavailable version proves nothing. Kernel reads only, never WindowServer.
pub(crate) fn held_as_long(held: &HeldCall) -> bool {
    held_as_long_with(
        held,
        HeldReaders::KERNEL.exec,
        crate::program_check_watch::held_now,
        has_run,
    )
}

/// The identity-bracketed proof, with inert readers in the exec-transition control.
fn held_as_long_with(
    held: &HeldCall,
    mut exec: impl FnMut(u32) -> Option<u64>,
    held_now: impl FnOnce(u32) -> bool,
    has_run: impl FnOnce(u32, u64) -> bool,
) -> bool {
    let Some(version) = held.exec_version else {
        return false;
    };
    exec(held.pid) == Some(version)
        && held_now(held.pid)
        && has_run(held.pid, held.held_ms)
        && exec(held.pid) == Some(version)
}

/// Whether process `pid` has run at least `ms`, by the jobs engine's own reader (its age: the wall
/// clock less its start).
fn has_run(pid: u32, ms: u64) -> bool {
    JobsReader::new()
        .read(ReadReq { pid, fds: false })
        .and_then(|f| f.age_ms)
        .is_some_and(|age| age >= ms)
}

/// WHAT SESSION `sid`'S AGENT MAY BE TOLD of its calls at no progress at (`now`, `now_ms`), as
/// the facts seam reads it: the board's no-progress calls ([`JobsBoard::facts`] at `now`), each
/// leader re-proved alive at its birth by `lives` after the board's lock is let go, the reading
/// stamped `now_ms` less the view's age; its held programs and left rows unread (`held` and
/// `left` are `None`). `None` with no view or one older than [`JOBS_FRESH`]. Both clocks are
/// given, so a test moves them together. The one reading aterm's own loop
/// (`harness_host::WorkerIdle::facts`, which adds the held part and the left rows) and a proven
/// self-claim's act (`harness fact`) both take.
pub(crate) fn facts_reading(
    board: &JobsBoard,
    lives: fn(u32, u64) -> bool,
    sid: &str,
    now: Instant,
    now_ms: i64,
) -> Option<aterm_agent::supervise::FactsReading> {
    let (age, calls) = board.facts(sid, now)?;
    let age_ms = i64::try_from(age.as_millis()).unwrap_or(i64::MAX);
    Some(aterm_agent::supervise::FactsReading {
        as_of_ms: now_ms.saturating_sub(age_ms),
        no_progress: calls
            .into_iter()
            .filter(|(leader, call)| call.birth.is_some_and(|birth| lives(*leader, birth)))
            .map(|(_, call)| call)
            .collect(),
        held: None,
        left: None,
    })
}

/// Whether the call whose leader is `pid`, born at `birth`, still lives (the facts' re-proof at
/// the ask): one read by the jobs engine's own reader, in the engine's unit, naming the same start
/// and no zombie, so a reused pid, a reaped call and an unreaped exit are not alive. Kernel reads
/// only (`proc_pidinfo`, `/proc`), never WindowServer.
pub(crate) fn leader_lives(pid: u32, birth: u64) -> bool {
    JobsReader::new()
        .read(ReadReq { pid, fds: false })
        .is_some_and(|f| f.start == Some(birth) && f.state != probe::RunState::Zombie)
}

/// The process's board: the loop posts each view it adopts (`App::post_jobs_reading`, through
/// `VitalsHost`'s board, which a test replaces with its own), and the workers read it.
pub(crate) static BOARD: JobsBoard = JobsBoard::new();

/// A row the canary reads as a live background call of the agent's.
fn background_call_live(r: &JobRow) -> bool {
    r.kind == JobKind::AgentCall
        && matches!(r.consumer, Consumer::AgentBg | Consumer::Agent)
        && r.class != Class::Exited
}

/// The canary's plain form of a row: the process it names (the waiting one
/// for a wait, else the leader) and that process's short name
/// ([`comm_word`]), the group, the agent's task id, the quiet age and a Tier
/// B naming's word.
fn background_call(r: &JobRow) -> BackgroundCall {
    BackgroundCall {
        pid: r.wait.as_ref().map_or(r.leader.0, |w| w.leaf),
        pgid: r.pgid,
        birth: r.leader.1,
        exe: comm_word(&r.exe),
        task: r.task.clone(),
        quiet_ms: r.quiet_ms.unwrap_or(0),
        wait: r.named.as_ref().map(|n| n.word.word().to_string()),
    }
}

/// The bytes of a program's file name the kernel's short name keeps: Linux's `comm` holds 15
/// (`TASK_COMM_LEN` less its NUL), and macOS's `p_comm` 16 (`MAXCOMLEN`).
pub(crate) const COMM_BYTES: usize = if cfg!(target_os = "linux") { 15 } else { 16 };

/// A row's `exe` (`JobRow::exe`) cut to [`COMM_BYTES`] at a character boundary, as the kernel's
/// short name holds it, so a file name past the canary's 32 bytes ([`BackgroundCall::exe`]) is
/// still said.
fn comm_word(exe: &str) -> String {
    let end = exe
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|end| *end <= COMM_BYTES)
        .last()
        .unwrap_or(0);
    exe.get(..end).unwrap_or_default().to_owned()
}

/// How long until the next pass after one whose demand was `demand`: the
/// engine's interval, never under [`FAST_MS`]; an idle engine (no job, or
/// only waits for the person) looks again at the fast cadence, since a job
/// can start at any keystroke.
fn next_pass_ms(demand: Demand) -> u64 {
    demand.every_ms().unwrap_or(FAST_MS).max(FAST_MS)
}

/// How soon a reading no window shows is due again after one that gave `view`: while a live agent
/// call (any consumer) stands suspect (`Standing::Capable | Capped`, the only rows that accrue φ)
/// and the engine is not idle, the engine's own cadence ([`next_pass_ms`]); else `None`, and the
/// next reading is `vitals_host::held_wait_after`'s, the next edge's or the disk watch's next look.
pub(crate) fn windowless_after(view: &JobsView) -> Option<Duration> {
    let suspect = view.rows.iter().any(|r| {
        r.kind == JobKind::AgentCall
            && r.class != Class::Exited
            && matches!(r.standing, Some(Standing::Capable | Standing::Capped))
    });
    (suspect && view.demand != Demand::Idle)
        .then(|| Duration::from_millis(next_pass_ms(view.demand)))
}

/// How far past the engine's cadence a window's reading lands while [`told_after`] paces it.
const TOLD_BEAT: Duration = Duration::from_millis(250);

/// How soon a window's reading is due again while a live agent call of `view` reads `no_progress`
/// (what `harness_host::WorkerIdle::facts` tells, only from a view no older than
/// `aterm_agent::supervise::facts::FACT_FRESH_MS`, 5 s): a beat ([`TOLD_BEAT`]) past the engine's
/// cadence, so each reading lands past the pass's due and runs it; else `None`, and the window's
/// own cadence stands. A background window's 5 s cadence never meets the gate, since each reading
/// is paced from the last, then read and adopted.
pub(crate) fn told_after(view: &JobsView) -> Option<Duration> {
    view.rows
        .iter()
        .any(|r| background_call_live(r) && r.no_progress)
        .then(|| Duration::from_millis(next_pass_ms(view.demand)) + TOLD_BEAT)
}

/// The engine's tabs from the host's request: its session key, the number
/// the person sees (`Tab 2` is 2), its shell, its terminal mode word where
/// `ttys` holds a current one, what its PTY reader read where `feeds`
/// holds it, and whether another aterm process handed it over (`adopted`).
fn job_tabs(
    tabs: &[TabRoot],
    ttys: &[(u64, TtyMode)],
    feeds: &[(u64, TabFeed)],
    adopted: &[u64],
) -> Vec<JobTab> {
    tabs.iter()
        .map(|t| {
            let feed = feeds
                .iter()
                .find(|(key, _)| *key == t.key)
                .map(|(_, f)| f.clone())
                .unwrap_or_default();
            JobTab {
                key: t.key,
                number: t
                    .label
                    .rsplit(' ')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
                shell_pid: t.shell_pid,
                block: feed.block,
                ended: feed.ended,
                shell_ms: feed.shell_ms,
                typed_ms: feed.typed_ms,
                tty: ttys.iter().find(|(key, _)| *key == t.key).map(|(_, m)| *m),
                output_ms: feed.output_ms,
                line_output_ms: feed.line_output_ms,
                drawn_ms: feed.drawn_ms,
                alt_screen: feed.alt_screen,
                output_bytes: feed.output_bytes,
                adopted: adopted.contains(&t.key),
                ..JobTab::default()
            }
        })
        .collect()
}

/// What a session's PTY reader read, for the engine: its newest feed that moved the screen, its
/// newest feed that held a line, the bytes it has read, and the newest OSC 133 block. The reader
/// writes it as it reads and feeds ([`Self::read`], [`Self::fed`]); the probe thread reads it at
/// each pass ([`Self::at_pass`]). Without it a printing `ping` reads quiet, and each command of a
/// typed `while` loop is a job of its own. A write that moves no cell (a colour reset at rest) is
/// no output, as it is no edge for the output wake ([`crate::vitals_host::ScreenMoves`]): counted,
/// it would keep a program at rest running and the engine reading every 2 s.
#[derive(Debug, Default)]
pub(crate) struct JobFeed(Mutex<Fed>, crate::telemetry_host::CmdMeter);

impl JobFeed {
    /// The session's command edges for its telemetry rows ([`crate::telemetry_host::CmdMeter`]).
    pub(crate) const fn meter(&self) -> &crate::telemetry_host::CmdMeter {
        &self.1
    }
}

#[derive(Debug, Default)]
struct Fed {
    output: Option<Instant>,
    line: Option<Instant>,
    /// The newest moving feed that drew the screen ([`Lines::drawn`]).
    drawn: Option<Instant>,
    /// The newest line the last pass found stamped past its millisecond, which it left to the
    /// next.
    owed_line: Option<Instant>,
    /// The newest output the last pass found stamped past its millisecond, as `owed_line`.
    owed_output: Option<Instant>,
    /// The same of the newest drawing feed.
    owed_drawn: Option<Instant>,
    /// The terminal showed its alternate screen after the last feed.
    alt: bool,
    /// The read being fed holds text after its last line feed so far: a line feed in its next
    /// chunk ends a line.
    text: bool,
    bytes: u64,
    block: Option<FedBlock>,
    /// The block `block` replaced, closed.
    ended: Option<FedBlock>,
    /// The terminal's current block as a feed last found one (id, state, status), and when a
    /// feed found it changed: the shell wrote one of its own marks.
    marks: Option<(u64, aterm_types::BlockState, Option<i32>)>,
    shell: Option<Instant>,
    /// The keys driven into the session's terminal as the last pass found their count, and the
    /// clock of the newest pass that found it moved ([`JobFeed::typed_at`]).
    driven: Option<aterm_session::sink::DrivenInput>,
    typed: Option<u64>,
}

#[derive(Clone, Debug)]
struct FedBlock {
    id: u64,
    open: bool,
    command: String,
    status: Option<i32>,
    /// When the reader first saw it run (OSC 133 C).
    started: Instant,
}

/// A [`JobFeed`] read at a pass, in host ms on the reader's clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabFeed {
    pub(crate) output_ms: Option<u64>,
    pub(crate) line_output_ms: Option<u64>,
    pub(crate) drawn_ms: Option<u64>,
    pub(crate) alt_screen: bool,
    pub(crate) output_bytes: u64,
    pub(crate) block: Option<Block>,
    pub(crate) ended: Option<Block>,
    pub(crate) shell_ms: Option<u64>,
    pub(crate) typed_ms: Option<u64>,
}

impl JobFeed {
    /// The bytes the reader has read so far.
    pub(crate) fn bytes(&self) -> u64 {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).bytes
    }

    /// The reader read `bytes`, which it feeds next, chunk by chunk.
    pub(crate) fn read(&self, bytes: &[u8]) {
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        fed.text = false;
        fed.bytes = fed
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    }

    /// A feed of `chunk` left `term` as it stands (the reader holds the term lock); `moved` when it
    /// moved the screen ([`crate::vitals_host::ScreenMoves::fed`]). A moving feed is output `now`.
    /// A chunk holding a line feed and text of its read (in the chunk, or in one the parse cut
    /// before it) is a line `now`: a bare line end is the echo of a typed Enter, which must not
    /// restart a hung command's quiet clock. Each stamp is taken under the feed's lock, so one made
    /// after a pass read the feed is past that pass's clock, and a line and the shell's marks
    /// parsed with it share one stamp.
    ///
    /// The block held is the newest that ran: open from OSC 133 C (started `now` when first seen
    /// so), closed by D with D's status, and kept until another block runs. The shell writes D and
    /// the next A together, and A archives the closed block at once, so a feed that finds a newer
    /// prompt's block closes the held one with its archived status; a block never seen open is held
    /// only while nothing ran before it. A terminal holding no block (a reset, a clear) leaves an
    /// open one open until the next prompt. The block a newer one replaces is kept as the ended
    /// one with its status. A feed that finds the current block changed stamps the shell's marks
    /// `now`; a terminal left with no block is none of the shell's.
    pub(crate) fn fed(
        &self,
        term: &aterm_core::terminal::Terminal,
        chunk: &[u8],
        moved: bool,
        now: impl FnOnce() -> Instant,
    ) {
        enum Turn {
            Keep,
            Open(u64),
            Close(Option<i32>),
            Hold(u64, Option<i32>),
        }
        let current = term.current_block();
        let marks = current.map(|c| (c.id, c.state, c.exit_code));
        let lines = Lines::of(chunk);
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let line = lines.ends && (lines.ended || lines.tail || fed.text);
        fed.text = lines.tail || (fed.text && !lines.ends);
        let marked = marks.is_some() && marks != fed.marks;
        let alt = term.is_alternate_screen();
        let runs = |c: &aterm_types::OutputBlock| c.state == aterm_types::BlockState::Executing;
        let turn = match (current, fed.block.as_ref()) {
            (Some(c), Some(w)) if c.id == w.id => match (runs(c), w.open) {
                (true, true) => Turn::Keep,
                (true, false) => Turn::Open(c.id),
                (false, _) if (w.open, w.status) == (false, c.exit_code) => Turn::Keep,
                (false, _) => Turn::Close(c.exit_code),
            },
            (Some(c), _) if runs(c) => Turn::Open(c.id),
            (Some(_), Some(w)) if w.open => {
                Turn::Close(term.block_by_id(w.id).and_then(|b| b.exit_code))
            }
            (Some(c), None) => Turn::Hold(c.id, c.exit_code),
            _ => Turn::Keep,
        };
        if matches!(turn, Turn::Keep) && !moved && !marked && !line && alt == fed.alt {
            return;
        }
        fed.alt = alt;
        let now = now();
        if moved {
            fed.output = Some(now);
            if lines.drawn {
                fed.drawn = Some(now);
            }
        }
        if line {
            fed.line = Some(now);
        }
        if marked {
            fed.marks = marks;
            fed.shell = Some(now);
        }
        match turn {
            Turn::Keep => {}
            Turn::Open(id) => {
                if let Some(mut w) = fed.block.take().filter(|w| w.id != id) {
                    if w.open {
                        w.open = false;
                        w.status = term.block_by_id(w.id).and_then(|b| b.exit_code);
                    }
                    fed.ended = Some(w);
                }
                fed.block = Some(FedBlock {
                    id,
                    open: true,
                    command: crate::session_status::executing_command_line(term)
                        .unwrap_or_default(),
                    status: None,
                    started: now,
                });
            }
            Turn::Close(status) => {
                if let Some(w) = fed.block.as_mut() {
                    w.open = false;
                    w.status = status;
                }
            }
            Turn::Hold(id, status) => {
                fed.block = Some(FedBlock {
                    id,
                    open: false,
                    command: String::new(),
                    status,
                    started: now,
                });
            }
        }
    }

    /// The feed now: [`Self::at_pass`] with no pass's clock.
    #[cfg(test)]
    pub(crate) fn at(&self, epoch: Instant) -> TabFeed {
        self.at_pass(epoch, None)
    }

    /// Notes the count of keys driven into the session's terminal (the person's or a driver's) as a
    /// pass at `at_ms` finds it, and returns the clock of the newest pass that found it moved since
    /// the pass before; the first pass only notes it.
    pub(crate) fn typed_at(
        &self,
        at_ms: u64,
        driven: aterm_session::sink::DrivenInput,
    ) -> Option<u64> {
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if fed.driven.is_some_and(|d| d != driven) {
            fed.typed = Some(at_ms);
        }
        fed.driven = Some(driven);
        fed.typed
    }

    /// The feed at a pass whose clock reads `at_ms` (`None`: no pass), on the clock whose zero is
    /// `epoch` (the probe thread's). Each stamp is whole ms rounded up, so a read after a pass's
    /// clock is stamped past it, and at least 1 ms, so a mode word stamped 0 is older than all of
    /// them ([`crate::vitals_host::TtyWord::current`]). A newest line or output stamped past
    /// `at_ms` is the next pass's, even if a newer one overwrites it first, and this pass reads the
    /// one the last left it, so a writer that never stops still has its lines read.
    pub(crate) fn at_pass(&self, epoch: Instant, at_ms: Option<u64>) -> TabFeed {
        let ms = |at: Instant| {
            u64::try_from(
                at.saturating_duration_since(epoch)
                    .as_nanos()
                    .div_ceil(1_000_000),
            )
            .unwrap_or(u64::MAX)
            .max(1)
        };
        let block = |b: &FedBlock| Block {
            id: b.id,
            started_ms: ms(b.started),
            open: b.open,
            command: b.command.clone(),
            status: b.status,
            stopped: b.status.is_some_and(stopped),
        };
        let defer = |newest: Option<Instant>, owed: &mut Option<Instant>| match at_ms {
            Some(a) if newest.is_some_and(|s| ms(s) > a) => std::mem::replace(owed, newest),
            _ => {
                *owed = None;
                newest
            }
        };
        let mut fed = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let fed = &mut *fed;
        let line = defer(fed.line, &mut fed.owed_line);
        let output = defer(fed.output, &mut fed.owed_output);
        let drawn = defer(fed.drawn, &mut fed.owed_drawn);
        TabFeed {
            output_ms: output.map(ms),
            line_output_ms: line.map(ms),
            drawn_ms: drawn.map(ms),
            alt_screen: fed.alt,
            output_bytes: fed.bytes,
            block: fed.block.as_ref().map(block),
            ended: fed.ended.as_ref().map(block),
            shell_ms: fed.shell.map(ms),
            typed_ms: fed.typed,
        }
    }
}

/// A block's status says job control stopped its command (Ctrl-Z, a background read of the
/// terminal): the shell reports 128 plus the signal, whose number is this host's (zsh on macOS
/// reports 146 for SIGTSTP, on Linux 148).
fn stopped(status: i32) -> bool {
    #[cfg(unix)]
    {
        matches!(
            status.checked_sub(128),
            Some(libc::SIGTSTP | libc::SIGSTOP | libc::SIGTTIN | libc::SIGTTOU)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

const fn state(s: probe::RunState) -> ProcState {
    match s {
        probe::RunState::Running => ProcState::Running,
        probe::RunState::Sleeping => ProcState::Sleeping,
        probe::RunState::Stopped => ProcState::Stopped,
        probe::RunState::Zombie => ProcState::Zombie,
        probe::RunState::Uninterruptible => ProcState::Uninterruptible,
        probe::RunState::Idle => ProcState::Idle,
        probe::RunState::Unknown => ProcState::Unknown,
    }
}

fn sweep_proc(p: &ProcessReading) -> SweepProc {
    SweepProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        uid: p.uid,
        name: p.name.clone(),
        state: state(p.state),
        session_leader: p.session_leader,
        has_tty: p.has_tty,
        exec: p.exec,
    }
}

const fn tcp(s: probe::TcpState) -> TcpState {
    match s {
        probe::TcpState::Closed => TcpState::Closed,
        probe::TcpState::Listen => TcpState::Listen,
        probe::TcpState::SynSent => TcpState::SynSent,
        probe::TcpState::SynReceived => TcpState::SynReceived,
        probe::TcpState::Established => TcpState::Established,
        probe::TcpState::CloseWait => TcpState::CloseWait,
        probe::TcpState::Closing => TcpState::Closing,
        _ => TcpState::Other,
    }
}

const fn filter(f: probe::KnoteFilter) -> KnoteFilter {
    match f {
        probe::KnoteFilter::Read => KnoteFilter::Read,
        probe::KnoteFilter::Write => KnoteFilter::Write,
        probe::KnoteFilter::Timer => KnoteFilter::Timer,
        probe::KnoteFilter::Vnode => KnoteFilter::Vnode,
        probe::KnoteFilter::Proc => KnoteFilter::Proc,
        probe::KnoteFilter::Signal => KnoteFilter::Signal,
        probe::KnoteFilter::User => KnoteFilter::User,
        probe::KnoteFilter::Other(_) => KnoteFilter::Other,
    }
}

fn fd(f: &probe::FdReading) -> Fd {
    let kind = match &f.kind {
        probe::FdKind::File {
            regular,
            size,
            dev,
            ino,
            lf_tail,
            fresh_dylib,
        } => FdKind::File {
            regular: *regular,
            size: *size,
            dev: *dev,
            ino: *ino,
            lf_tail: *lf_tail,
            fresh_dylib: *fresh_dylib,
        },
        probe::FdKind::Device { tty, read, ctty } => FdKind::Device {
            tty: *tty,
            read: *read,
            ctty: *ctty,
        },
        probe::FdKind::Pipe {
            handle,
            peer,
            status,
            queued,
            write_end,
        } => FdKind::Pipe {
            handle: *handle,
            peer: *peer,
            status: *status,
            queued: *queued,
            write_end: *write_end,
        },
        probe::FdKind::Tcp {
            state,
            remote,
            local_port,
            queued_read,
        } => FdKind::Tcp {
            state: tcp(*state),
            remote: *remote,
            local_port: *local_port,
            queued_read: *queued_read,
        },
        probe::FdKind::Socket => FdKind::Socket,
        probe::FdKind::EventQueue { sleeping, knotes } => FdKind::EventQueue {
            sleeping: *sleeping,
            knotes: knotes.as_ref().map(|ks| {
                ks.iter()
                    .map(|k| Knote {
                        ident: k.ident,
                        filter: filter(k.filter),
                    })
                    .collect()
            }),
        },
        probe::FdKind::Watch => FdKind::Watch,
        probe::FdKind::Other => FdKind::Other,
    };
    Fd { fd: f.fd, kind }
}

/// The engine's readings of `read` ([`job_proc`]), each of a tab's root (`roots`) with its pid
/// version (`exec`): it tells the engine the root ran a program anew
/// (`exec zsh`); and each member that [`ran`] with the build it becomes and the claims ahead of
/// it where it is a build's front (`front`, asked of those members alone: [`Fronts::front`]).
/// Only roots and members that ran are asked their exec version; an ordinary member carries it
/// as naming identity, separate from the root's program-change version.
fn job_procs(
    read: &[ProcFacts],
    roots: &BTreeSet<u32>,
    stand_in: Option<u32>,
    exec: impl Fn(u32) -> Option<u64>,
    mut front: impl FnMut(&ProcFacts) -> Option<FrontNow>,
) -> Vec<JobProc> {
    read.iter()
        .map(|p| {
            let (becomes, claims_ahead) = ran(p, stand_in)
                .then(|| front(p))
                .flatten()
                .map_or((None, None), |(becomes, ahead)| (Some(becomes), ahead));
            let exec_version = (roots.contains(&p.pid) || ran(p, stand_in))
                .then(|| exec(p.pid))
                .flatten();
            JobProc {
                version: roots.contains(&p.pid).then_some(exec_version).flatten(),
                exec_version: ran(p, stand_in).then_some(exec_version).flatten(),
                becomes,
                claims_ahead,
                ..job_proc(p, stand_in)
            }
        })
        .collect()
}

/// The engine's reading of `p`, with what keeps it from its first instruction where
/// [`member_start`] reads it unstarted (`stand_in`'s held included).
fn job_proc(p: &ProcFacts, stand_in: Option<u32>) -> JobProc {
    JobProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        sid: p.sid,
        start: p.start,
        age_ms: p.age_ms,
        uid: p.uid,
        name: p.name.clone(),
        exe: p.exe.clone(),
        state: state(p.state),
        traced: p.traced,
        tty: p.tty,
        tpgid: p.tpgid,
        term: p.term.map(|t| TermWord {
            canonical: t.canonical,
            echo: t.echo,
            queued: t.queued,
        }),
        counters: p.counters.map(|c| JobCounters {
            cpu_ns: c.cpu_ns,
            child_cpu_ns: c.child_cpu_ns,
            syscalls: c.syscalls,
            wakeups: c.wakeups,
            ctx_switches: c.ctx_switches,
            disk_read: c.disk_read,
            disk_written: c.disk_written,
            logical_writes: c.logical_writes,
            runnable_ns: c.runnable_ns,
            pageins: c.pageins,
            peak_footprint: c.peak_footprint,
        }),
        threads: p.threads.as_ref().map(|ts| {
            ts.iter()
                .map(|t| ThreadState {
                    state: state(t.state),
                    wchan: t.wchan.clone(),
                })
                .collect()
        }),
        wchan: p.wchan.clone(),
        fds: p.fds.as_ref().map(|fs| fs.iter().map(fd).collect()),
        blocked_fd: p.blocked_fd,
        start_wait: member_start(p, stand_in).and_then(start_wait),
        version: None,
        exec_version: None,
        becomes: None,
        claims_ahead: None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Start facts [`probe::judge_start`] judges `at`, read the held age after the fork.
    fn judged(at: AtStart) -> StartFacts {
        let unstarted = StartFacts {
            lone_zero: true,
            user_ns: Some(0),
            program_check_wait: true,
            local: Some(true),
            age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
            ..StartFacts::default()
        };
        let facts = match at {
            AtStart::Started => StartFacts::default(),
            AtStart::Held => unstarted,
            AtStart::Translating => StartFacts {
                translated: true,
                ..unstarted
            },
            AtStart::Unstarted => StartFacts {
                local: Some(false),
                ..unstarted
            },
            AtStart::StoppedUnstarted { stopped, traced } => StartFacts {
                stopped,
                traced,
                ..unstarted
            },
        };
        assert_eq!(probe::judge_start(&facts), at);
        facts
    }

    /// The engine's cadence, and with no window the suspect rule's: a live agent call that stands
    /// suspect asks for the engine's demand; an idle engine, an exited, running or listening call,
    /// and a job that is no agent's ask nothing.
    #[test]
    fn the_engine_is_asked_again_at_its_own_cadence_never_faster_than_two_seconds() {
        assert_eq!(next_pass_ms(Demand::Fast), 2_000);
        assert_eq!(next_pass_ms(Demand::Slow), 30_000);
        assert_eq!(next_pass_ms(Demand::Idle), 2_000);

        let view = |rows: Vec<JobRow>, demand| JobsView {
            rows,
            demand,
            ..JobsView::default()
        };
        let call = || JobRow {
            standing: Some(Standing::Capped),
            ..call_row(4, Consumer::AgentBg, true, 31, 27098, "python3")
        };
        let mut running = call();
        running.standing = None;
        let mut serving = call();
        serving.standing = Some(Standing::Legit);
        let mut shell = call();
        shell.kind = JobKind::Shell;
        let mut ended = call();
        ended.class = Class::Exited;
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(windowless_after(&view(vec![call()], Demand::Fast)), secs(2));
        assert_eq!(
            windowless_after(&view(vec![call()], Demand::Slow)),
            secs(30)
        );
        assert_eq!(windowless_after(&view(vec![call()], Demand::Idle)), None);
        assert_eq!(windowless_after(&view(vec![shell], Demand::Fast)), None);
        assert_eq!(windowless_after(&view(vec![ended], Demand::Fast)), None);
        assert_eq!(windowless_after(&view(Vec::new(), Demand::Idle)), None);
        assert_eq!(windowless_after(&view(vec![running], Demand::Fast)), None);
        assert_eq!(windowless_after(&view(vec![serving], Demand::Slow)), None);
        let beat = |ms| Some(Duration::from_millis(ms));
        assert_eq!(told_after(&view(vec![call()], Demand::Fast)), beat(2_250));
        let mut moving = call();
        moving.no_progress = false;
        assert_eq!(told_after(&view(vec![moving], Demand::Fast)), None);
    }

    /// A file name past the canary's 32 bytes is said by its first [`COMM_BYTES`], never as `-`; a
    /// multi-byte one is cut on a character.
    #[test]
    fn the_canary_names_a_call_by_the_kernels_short_name() {
        let said =
            |exe: &str| background_call(&call_row(4, Consumer::Agent, true, 31, 27098, exe)).exe;
        let long = "conformance_occupancy-0123456789abcdef";
        assert_eq!(said(long), long[..COMM_BYTES]);
        assert_eq!(said("python3"), "python3");
        let split = format!("{}\u{e9}x", "a".repeat(COMM_BYTES - 1));
        assert_eq!(said(&split), "a".repeat(COMM_BYTES - 1));
    }

    /// An agent's call in tab `tab` read `no_progress` (or not), by `consumer`,
    /// quiet `min` minutes, naming process `pid` (`exe`).
    pub(crate) fn call_row(
        tab: u64,
        consumer: Consumer,
        no_progress: bool,
        min: u64,
        pid: u32,
        exe: &str,
    ) -> JobRow {
        use aterm_vitals::jobs::Verdict;
        JobRow {
            tab,
            kind: JobKind::AgentCall,
            orphan_of: None,
            consumer,
            no_progress,
            leader: (pid, None),
            pgid: pid,
            exe: exe.into(),
            ..crate::stats_paint::tests::job_row(
                &format!("a{pid}"),
                Verdict::Quiet,
                Some(9_000),
                Some(min * 60_000),
            )
        }
    }

    /// The canary's evidence is this session's background call quiet longest, named by its waiting
    /// process; never a foreground call, a person's job, an exited call or another session's.
    #[test]
    fn the_board_names_this_sessions_background_call_quiet_longest() {
        use aterm_vitals::jobs::{NamedRow, Standing, TierB, Wait, WaitName};
        let board = JobsBoard::new();
        let t0 = Instant::now();
        assert_eq!(board.evidence("s-hung", t0), BackgroundEvidence::Unknown);
        let mut waiting = call_row(4, Consumer::AgentBg, true, 31, 27090, "sh");
        waiting.wait = Some(Wait::bare(
            WaitName::Unnamed,
            Standing::Capped,
            27098,
            "aterm_gui-e0964a",
        ));
        waiting.exe = "aterm_gui-e0964a".into();
        waiting.task = Some("bplle5dxj".into());
        waiting.named = Some(NamedRow {
            word: TierB::Timer,
            holder: None,
        });
        let mut person = call_row(4, Consumer::PersonFg, true, 600, 700, "sleep");
        person.kind = JobKind::Shell;
        let mut ended = call_row(4, Consumer::AgentBg, false, 120, 503, "cargo");
        ended.class = Class::Exited;
        let rows = vec![
            call_row(4, Consumer::Agent, true, 20, 501, "Python"),
            waiting.clone(),
            call_row(4, Consumer::AgentFg, true, 99, 502, "node"),
            ended,
            person.clone(),
            call_row(9, Consumer::AgentBg, true, 500, 504, "make"),
            call_row(9, Consumer::AgentBg, false, 5, 505, "cargo"),
        ];
        let view = || {
            Arc::new(JobsView {
                rows: rows.clone(),
                ..JobsView::default()
            })
        };
        let sids = BTreeMap::from([(4, "s-hung".to_string()), (9, "s-other".to_string())]);
        let first = view();
        board.post(Arc::clone(&first), sids.clone(), t0);
        let hung = BackgroundEvidence::NoProgress {
            worst: BackgroundCall {
                pid: 27098,
                pgid: 27090,
                birth: None,
                exe: "aterm_gui-e0964a"[..COMM_BYTES].into(),
                task: Some("bplle5dxj".into()),
                quiet_ms: 31 * 60_000,
                wait: Some("timer".into()),
            },
        };
        assert_eq!(board.evidence("s-hung", t0), hung);
        assert_eq!(board.evidence("s-calm", t0), BackgroundEvidence::Unknown);
        let late = t0 + JOBS_FRESH + Duration::from_secs(1);
        assert_eq!(board.evidence("s-hung", late), BackgroundEvidence::Unknown);
        board.post(Arc::clone(&first), sids.clone(), late);
        assert_eq!(
            board.evidence("s-hung", late),
            BackgroundEvidence::Unknown,
            "the same view posted again keeps its age"
        );
        board.post(view(), sids.clone(), late);
        assert_eq!(board.evidence("s-hung", late), hung, "a new view is fresh");
        let mut building = rows.clone();
        building.push(call_row(4, Consumer::AgentBg, false, 2, 506, "cargo"));
        let view = Arc::new(JobsView {
            rows: building,
            ..JobsView::default()
        });
        board.post(view, sids, late);
        assert_eq!(
            board.evidence("s-hung", late),
            BackgroundEvidence::Unknown,
            "a live call that is not at no progress"
        );
    }

    #[test]
    fn a_working_descendant_qualifies_only_its_wrappers_per_call_alert() {
        let board = JobsBoard::new();
        let now = Instant::now();
        let mut wrapper = call_row(4, Consumer::Agent, true, 35, 400, "observer");
        wrapper.working_descendant = Some((402, 99));
        let controller = call_row(4, Consumer::Agent, true, 34, 401, "controller");
        let active = call_row(4, Consumer::Agent, false, 1, 402, "trustc");
        let unrelated = call_row(9, Consumer::Agent, true, 50, 900, "other-tab");
        let post = |rows| {
            board.post(
                Arc::new(JobsView {
                    rows,
                    ..JobsView::default()
                }),
                BTreeMap::from([(4, "ours".into()), (9, "other".into())]),
                now,
            )
        };
        post(vec![
            wrapper.clone(),
            controller.clone(),
            active.clone(),
            unrelated,
        ]);
        assert_eq!(board.evidence("ours", now), BackgroundEvidence::Unknown);
        let (_, facts) = board.facts("ours", now).expect("fresh");
        assert_eq!(facts.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(), [401]);
        wrapper.working_descendant = None;
        post(vec![wrapper, controller, active]);
        let (_, facts) = board.facts("ours", now).expect("fresh");
        assert_eq!(
            facts.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
            [400, 401]
        );
    }

    /// Each tab keeps its key, number and shell, takes the terminal word
    /// and the reader's feed handed for its key, and reads adopted only when
    /// its key is among the handed-over sessions.
    #[test]
    fn a_tab_keeps_its_session_key_shell_and_number_and_its_own_terminal_word_and_feed() {
        let root = |key, label: &str| TabRoot {
            key,
            tab: key,
            shell_pid: 900 + u32::try_from(key).expect("small"),
            label: label.into(),
            program: String::new(),
            tab_id: None,
            agent_leader: None,
        };
        let raw = TtyMode {
            canonical: false,
            echo: false,
            stamped_ms: 7,
        };
        let fed = TabFeed {
            output_ms: Some(9),
            line_output_ms: Some(8),
            drawn_ms: Some(7),
            alt_screen: true,
            output_bytes: 120,
            block: Some(Block {
                id: 3,
                started_ms: 5,
                open: true,
                command: "make".into(),
                status: None,
                stopped: false,
            }),
            ended: Some(Block {
                id: 2,
                started_ms: 1,
                open: false,
                command: "ls".into(),
                status: Some(146),
                stopped: true,
            }),
            shell_ms: Some(4),
            typed_ms: Some(6),
        };
        let tabs = job_tabs(
            &[root(41, "Tab 3"), root(42, "Tab 4")],
            &[(41, raw)],
            &[(42, fed.clone())],
            &[42],
        );
        assert_eq!(
            (tabs[0].key, tabs[0].number, tabs[0].shell_pid, tabs[0].tty),
            (41, 3, 941, Some(raw))
        );
        assert_eq!(
            (tabs[0].adopted, tabs[1].adopted),
            (false, true),
            "only the handed-over session reads adopted"
        );
        assert_eq!(
            (tabs[0].output_ms, tabs[0].output_bytes, &tabs[0].block),
            (None, 0, &None)
        );
        assert_eq!((tabs[1].key, tabs[1].tty), (42, None));
        assert_eq!(
            TabFeed {
                output_ms: tabs[1].output_ms,
                line_output_ms: tabs[1].line_output_ms,
                drawn_ms: tabs[1].drawn_ms,
                alt_screen: tabs[1].alt_screen,
                output_bytes: tabs[1].output_bytes,
                block: tabs[1].block.clone(),
                ended: tabs[1].ended.clone(),
                shell_ms: tabs[1].shell_ms,
                typed_ms: tabs[1].typed_ms,
            },
            fed
        );
    }

    /// Every read counts its bytes; only a line feed with text of its read stamps a line (not a
    /// `\r` redraw or a typed Enter's echo, but a record led by its line feed), only a feed that
    /// moved the screen stamps output, and only one that puts the cursor on a row of its own stamps
    /// a drawing (not a spinner's erase). The alternate screen reads as the last feed left it.
    #[test]
    fn a_line_feed_stamps_a_line_and_only_a_moving_feed_stamps_output() {
        const RESET: &[u8] = b"\x1b[39m\x1b[0m";
        let epoch = Instant::now();
        let feed = JobFeed::default();
        let term = aterm_core::terminal::Terminal::new(24, 80);
        assert_eq!(feed.at(epoch), TabFeed::default(), "nothing read");
        for (bytes, moved, ms) in [
            (&b"64 bytes from 127.0.0.1\r\n"[..], true, 1_000),
            (b"\r 50%", true, 2_500),
            (RESET, false, 4_000),
            (b"\r\n", false, 5_000),
        ] {
            feed.read(bytes);
            feed.fed(&term, bytes, moved, || epoch + Duration::from_millis(ms));
        }
        let read = feed.at(epoch);
        assert_eq!(
            (read.output_ms, read.line_output_ms, read.output_bytes),
            (Some(2_500), Some(1_000), 41)
        );
        let long = [b'x'; 16];
        feed.read(&[&long[..], b"\r\n"].concat());
        feed.fed(&term, &long, true, || epoch + Duration::from_millis(6_000));
        feed.fed(&term, b"\r\n", true, || {
            epoch + Duration::from_millis(6_001)
        });
        assert_eq!(
            feed.at(epoch).line_output_ms,
            Some(6_001),
            "a line whose line feed the parse cut from its text"
        );
        let record = b"\r\nThu Oct 23 12:00:05";
        feed.read(record);
        feed.fed(&term, record, true, || epoch + Duration::from_millis(7_000));
        assert_eq!(
            feed.at(epoch).line_output_ms,
            Some(7_000),
            "a writer that leads with its line feed"
        );
        feed.fed(&term, b"", true, || epoch);
        assert_eq!(
            feed.at(epoch).output_ms,
            Some(1),
            "a stamp is at least 1 ms"
        );
        let spinner = "\r\x1b[2K\x1b[?25l\u{280b} fetching".as_bytes();
        feed.fed(&term, spinner, true, || {
            epoch + Duration::from_millis(8_000)
        });
        assert_eq!(feed.at(epoch).drawn_ms, None, "a spinner draws nothing");
        let redraw = b"\x1b[12;1Htests/test_api.py::test_x PASSED\x1b[K\r\n";
        feed.fed(&term, redraw, true, || epoch + Duration::from_millis(9_000));
        assert_eq!(
            feed.at(epoch).drawn_ms,
            Some(9_000),
            "nvim's row of its terminal"
        );
        let mut full = aterm_core::terminal::Terminal::new(24, 80);
        for (bytes, alt) in [(&b"\x1b[?1049h"[..], true), (b"\x1b[?1049l", false)] {
            full.process(bytes);
            feed.fed(&full, bytes, false, || {
                epoch + Duration::from_millis(10_000)
            });
            assert_eq!(feed.at(epoch).alt_screen, alt, "{bytes:?}");
        }
    }

    /// A writer that never stops (`kubectl logs -f`) has a line fed past each pass's clock, which a
    /// newer one overwrites before the next pass; that pass still reads it.
    #[test]
    fn a_line_fed_past_a_passs_clock_is_the_next_passs() {
        const LINE: &[u8] = b"GET /healthz 200\r\n";
        let epoch = Instant::now();
        let term = aterm_core::terminal::Terminal::new(24, 80);
        let at = |us: u64| epoch + Duration::from_micros(us);
        for offset in [100, 900] {
            let feed = JobFeed::default();
            let line = |us| {
                feed.read(LINE);
                feed.fed(&term, LINE, true, || at(us));
            };
            let pass = |ms| {
                let f = feed.at_pass(epoch, Some(ms));
                (f.line_output_ms, f.output_ms)
            };
            // The newest line and output round past the pass's millisecond.
            line(1_000_000 + offset);
            assert_eq!(pass(1_000), (None, None), "offset {offset}");
            line(2_000_000 + offset);
            assert_eq!(pass(2_000), (Some(1_001), Some(1_001)), "offset {offset}");
            assert_eq!(pass(4_000), (Some(2_001), Some(2_001)), "offset {offset}");
            line(5_000_000 + offset);
            assert_eq!(pass(5_000), (None, None), "consumed stamp, offset {offset}");
        }
        for before in [0, 1] {
            let feed = JobFeed::default();
            let line = |us| {
                feed.read(LINE);
                feed.fed(&term, LINE, true, || at(us));
            };
            line(1_000_000 - before);
            let f = feed.at_pass(epoch, Some(1_000));
            assert_eq!((f.line_output_ms, f.output_ms), (Some(1_000), Some(1_000)));
            line(2_000_100);
            let f = feed.at_pass(epoch, Some(2_000));
            assert_eq!((f.line_output_ms, f.output_ms), (None, None));
        }
    }

    /// A PASS THAT FINDS KEYS DRIVEN SINCE THE LAST STAMPS ITS CLOCK: the first
    /// pass only notes the sink's count, and the stamp stays until keys move it.
    #[cfg(unix)]
    #[test]
    fn a_pass_that_finds_keys_driven_since_the_last_stamps_its_clock() {
        let mut fds = [0; 2];
        // SAFETY: `fds` is two writable ints.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let [r, w] = fds;
        let sink = aterm_session::sink::SinkWriter::new(w);
        let feed = JobFeed::default();
        let first = feed.typed_at(1_000, sink.driven_input());
        let still = feed.typed_at(3_000, sink.driven_input());
        assert_eq!(sink.write_frame(b"x").expect("a key"), 1);
        let typed = feed.typed_at(5_000, sink.driven_input());
        let kept = feed.typed_at(7_000, sink.driven_input());
        // SAFETY: both ends are this test's.
        unsafe {
            libc::close(r);
            libc::close(w);
        }
        assert_eq!(
            (first, still, typed, kept),
            (None, None, Some(5_000), Some(5_000))
        );
    }

    /// The feed holds the newest block that ran: open from OSC 133 C with its command, closed by D
    /// with its status (D and the next A in one feed included), held through the next prompt, and
    /// kept as the ended one once a newer block runs. The shell's marks are stamped, and a line
    /// parsed with them shares their stamp; a command's output and a reset are not marks, and a
    /// reset leaves the block open until the next prompt. A block Ctrl-Z ended is stopped.
    #[test]
    fn the_feed_holds_the_newest_block_that_ran_and_its_status() {
        let epoch = Instant::now();
        let feed = JobFeed::default();
        let put = |term: &mut aterm_core::terminal::Terminal, bytes: &[u8], moved, ms| {
            term.process(bytes);
            feed.fed(term, bytes, moved, || epoch + Duration::from_millis(ms));
        };
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        put(&mut term, b"$ ls\n", false, 100);
        assert_eq!(feed.at(epoch).block, None, "no shell integration");
        put(
            &mut term,
            b"\x1b]133;A\x07$ \x1b]133;B\x07while true; do date; sleep 5; done\n",
            false,
            200,
        );
        let typed = feed.at(epoch).block.expect("the prompt's block");
        assert!(!typed.open, "typed, not run: {typed:?}");
        put(&mut term, b"\x1b]133;C\x07", false, 300);
        let running = feed.at(epoch).block.expect("the running block");
        assert_eq!(
            (
                running.id,
                running.open,
                running.started_ms,
                running.command.as_str(),
                running.status
            ),
            (
                typed.id,
                true,
                300,
                "while true; do date; sleep 5; done",
                None
            )
        );
        put(&mut term, b"Thu Oct 23\n", false, 5_300);
        assert_eq!(
            feed.at(epoch).block.as_ref(),
            Some(&running),
            "output keeps it"
        );
        put(&mut term, b"^C\x1b]133;D;130\x07", false, 9_000);
        let ended = feed.at(epoch).block.expect("the ended block");
        assert_eq!(
            (
                ended.id,
                ended.open,
                ended.status,
                ended.started_ms,
                &ended.command
            ),
            (running.id, false, Some(130), 300, &running.command)
        );
        put(&mut term, b"\x1b]133;A\x07$ ", false, 9_100);
        assert_eq!(
            feed.at(epoch).block,
            Some(ended.clone()),
            "the next prompt holds it"
        );
        put(
            &mut term,
            b"\x1b]133;B\x07make\n\x1b]133;C\x07",
            false,
            9_200,
        );
        let make = feed.at(epoch).block.expect("the next running block");
        assert!(make.id != running.id && make.open, "{make:?}");
        assert_eq!(feed.at(epoch).ended, Some(ended), "the block it replaced");
        put(&mut term, b"\x1b]133;D;2\x07\x1b]133;A\x07$ ", false, 9_900);
        let failed = feed.at(epoch).block.expect("the failed block");
        assert_eq!(
            (failed.id, failed.open, failed.status, failed.started_ms),
            (make.id, false, Some(2), 9_200),
            "D and A in one feed"
        );
        put(
            &mut term,
            b"\x1b]133;B\x07top\n\x1b]133;C\x07",
            false,
            10_000,
        );
        let top = feed.at(epoch).block.expect("top runs");
        put(
            &mut term,
            b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07less\n\x1b]133;C\x07",
            false,
            11_000,
        );
        let less = feed.at(epoch).block.expect("less runs");
        let now = feed.at(epoch);
        assert_eq!(
            now.block.map(|b| (b.open, b.command, b.started_ms)),
            Some((true, "less".to_owned(), 11_000))
        );
        assert_eq!(
            now.ended
                .map(|b| (b.id, b.open, b.status, b.command, b.started_ms)),
            Some((top.id, false, Some(0), top.command, 10_000)),
            "a block that ended and the next that ran in one feed"
        );
        assert_eq!(feed.at(epoch).output_ms, None, "no feed moved the screen");
        assert_eq!(
            (feed.at(epoch).shell_ms, feed.at(epoch).line_output_ms),
            (Some(11_000), Some(11_000)),
            "the shell's marks, and a line parsed with them shares their stamp"
        );
        put(&mut term, b"README\n", true, 12_000);
        assert_eq!(
            feed.at(epoch).shell_ms,
            Some(11_000),
            "a command's output is none of the shell's marks"
        );
        put(&mut term, b"\x1bc", true, 13_000);
        assert_eq!(
            (
                feed.at(epoch).block.map(|b| (b.id, b.open)),
                feed.at(epoch).shell_ms
            ),
            (Some((less.id, true)), Some(11_000)),
            "a reset leaves the command running, and is none of the shell's marks"
        );
        put(&mut term, b"\x1b]133;A\x07$ ", false, 14_000);
        assert_eq!(
            feed.at(epoch).block.map(|b| (b.id, b.open, b.status)),
            Some((less.id, false, None)),
            "the next prompt closes it, its D lost to the reset"
        );
        assert!(!failed.stopped, "an exit is no stop");
        #[cfg(unix)]
        {
            put(
                &mut term,
                b"\x1b]133;B\x07vim\n\x1b]133;C\x07",
                false,
                15_000,
            );
            let stop = format!("\x1b]133;D;{}\x07\x1b]133;A\x07$ ", 128 + libc::SIGTSTP);
            put(&mut term, stop.as_bytes(), false, 16_000);
            let vim = feed.at(epoch).block.expect("vim stopped");
            assert!(vim.stopped && !vim.open, "Ctrl-Z's status: {vim:?}");
        }
    }

    /// The pass on this host's real process table, this test process standing in for a tab's
    /// shell: inside the cadence it keeps its last view, an edge's reading runs the engine anyway,
    /// and a reading without a sweep keeps the last view.
    #[test]
    fn an_edges_reading_inside_the_cadence_runs_the_engine() {
        let Some(sweep) = aterm_sysprobe::processes() else {
            // A host without a process sweep publishes no jobs.
            return;
        };
        let me = std::process::id();
        let tabs = [TabRoot {
            key: 1,
            tab: 1,
            shell_pid: me,
            label: "Tab 1".into(),
            program: String::new(),
            tab_id: None,
            agent_leader: None,
        }];
        let mut pass = JobsPass::new();
        let first = pass
            .tick(
                10_000,
                || 10_000,
                Some(&sweep.procs),
                &tabs,
                &[],
                &[],
                &GrantFacts::default(),
                me,
                false,
                false,
            )
            .expect("a view");
        let paced = pass
            .tick(
                11_900,
                || 11_900,
                Some(&sweep.procs),
                &tabs,
                &[],
                &[],
                &GrantFacts::default(),
                me,
                false,
                false,
            )
            .expect("the last view");
        assert!(Arc::ptr_eq(&first, &paced), "paced: the engine waits");
        let edge = pass
            .tick(
                11_900,
                || 11_900,
                Some(&sweep.procs),
                &tabs,
                &[],
                &[],
                &GrantFacts::default(),
                me,
                false,
                true,
            )
            .expect("a new view");
        assert!(!Arc::ptr_eq(&first, &edge), "an edge's reading looks");
        assert!(
            pass.tick(
                11_950,
                || 11_950,
                None,
                &tabs,
                &[],
                &[],
                &GrantFacts::default(),
                me,
                false,
                true
            )
            .is_some(),
            "a reading without a sweep keeps the last view"
        );
    }

    /// Members count by their own reading; of the rest only this user's live rows with at most
    /// 20 ms of CPU are asked. An actual version is proved around a fresh start read for each
    /// named row; the stand-in reads held unasked whatever its CPU. A stopped or traced row that
    /// never ran is handed on with what it read, a member (24) or outside every job (25).
    #[test]
    fn the_jobs_pass_counts_this_users_held_programs_members_or_not() {
        use std::cell::RefCell;
        const ME: u32 = 501;
        let row = |pid, uid, cpu_ms: u64, state| ProcessReading {
            pid,
            uid: Some(uid),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            state,
            ..ProcessReading::default()
        };
        let sweep = [
            row(10, ME, 400, probe::RunState::Sleeping),
            row(20, ME, 2, probe::RunState::Sleeping),
            row(21, ME, 2, probe::RunState::Sleeping),
            row(22, ME, 2, probe::RunState::Sleeping),
            row(30, ME, 30, probe::RunState::Sleeping),
            row(40, 0, 2, probe::RunState::Sleeping),
            row(50, ME, 0, probe::RunState::Zombie),
        ];
        let unstarted = |translated| probe::StartFacts {
            lone_zero: true,
            user_ns: Some(0),
            program_check_wait: true,
            translated,
            local: Some(true),
            age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
            ..probe::StartFacts::default()
        };
        let member = |pid, name: &str, exe: Option<&str>, facts| ProcFacts {
            pid,
            uid: Some(ME),
            name: name.into(),
            exe: exe.map(str::to_owned),
            start_facts: Some(facts),
            ..ProcFacts::default()
        };
        let members = [
            member(10, "zsh", Some("gh"), unstarted(false)),
            member(22, "sleep", None, probe::StartFacts::default()),
            member(23, "x86tool", None, unstarted(true)),
            member(
                24,
                "debuggee",
                None,
                probe::StartFacts {
                    traced: true,
                    ..unstarted(false)
                },
            ),
        ];
        let traced = AtStart::StoppedUnstarted {
            stopped: false,
            traced: true,
        };
        let spawned = AtStart::StoppedUnstarted {
            stopped: true,
            traced: false,
        };
        let sweep = [
            &sweep[..],
            &[
                row(23, ME, 2, probe::RunState::Sleeping),
                row(25, ME, 2, probe::RunState::Stopped),
            ][..],
        ]
        .concat();
        let asked = RefCell::new(Vec::new());
        let versions = RefCell::new(Vec::new());
        let (held, young) = held_list(
            &sweep,
            &members,
            Some(ME),
            Some(30),
            0,
            |pid| {
                asked.borrow_mut().push(pid);
                Some(judged(if pid == 21 {
                    AtStart::Unstarted
                } else if pid == 23 {
                    AtStart::Translating
                } else if pid == 24 {
                    traced
                } else if pid == 25 {
                    spawned
                } else {
                    AtStart::Held
                }))
            },
            |pid| {
                versions.borrow_mut().push(pid);
                Some(u64::from(pid) * 100 + 7)
            },
            |pid| pid == 20,
            |pid| match pid {
                20 => Some("codex-command-runner".to_owned()),
                25 => Some("start-suspended-helper".to_owned()),
                _ => None,
            },
        );
        assert_eq!(
            asked.into_inner(),
            [10, 23, 24, 20, 20, 21, 21, 25, 25],
            "held members are re-probed to bind their version; nonmembers first selected by \
             CPU get the same proof; never another user's, a zombie or the stand-in"
        );
        assert_eq!(
            versions.into_inner(),
            [10, 10, 23, 23, 24, 24, 20, 20, 21, 21, 30, 30, 25, 25],
            "the actual version brackets each fresh wait proof; never member 22, which started"
        );
        let got: Vec<(u32, u64, &str, bool, StartWait)> = held
            .iter()
            .map(|h| &h.row)
            .map(|h| (h.pid, h.exec, h.name.as_str(), h.aterm, h.wait))
            .collect();
        assert_eq!(
            got,
            [
                (10, 1007, "gh", true, StartWait::ProgramCheck),
                (23, 2307, "x86tool", true, StartWait::Translating),
                (
                    24,
                    2407,
                    "debuggee",
                    true,
                    StartWait::StoppedUnstarted {
                        stopped: false,
                        traced: true
                    }
                ),
                (
                    20,
                    2007,
                    "codex-command-runner",
                    true,
                    StartWait::ProgramCheck
                ),
                (21, 2107, "p21", false, StartWait::NotStarted),
                (30, 3007, "p30", false, StartWait::ProgramCheck),
                (
                    25,
                    2507,
                    "start-suspended-helper",
                    false,
                    StartWait::StoppedUnstarted {
                        stopped: true,
                        traced: false
                    }
                ),
            ],
            "each named by its executable where one reads, never by p_comm's 16 bytes, with what \
             keeps it; the stand-in's 30 ms row counted"
        );
        assert!(held.iter().all(|h| h.row.exec_version == Some(h.row.exec)));
        assert_eq!(young, [], "none read unrun but unlisted");
        assert!(
            held_list(
                &sweep,
                &members,
                None,
                Some(30),
                0,
                |_| Some(judged(AtStart::Held)),
                |_| None,
                |_| true,
                |_| None,
            )
            .0
            .is_empty(),
            "no user read, nothing counted"
        );
    }

    /// Cached held facts select a member, but only a fresh held probe between equal actual
    /// versions certifies its exec. NEGATIVE CONTROLS: the member has started, execs during the
    /// fresh probe, or has no readable version; its descriptive row remains unproved.
    #[test]
    fn a_held_version_is_bound_to_a_fresh_start_read() {
        use std::cell::Cell;
        let member = ProcFacts {
            pid: 601,
            uid: Some(501),
            start: Some(7),
            start_facts: Some(probe::StartFacts {
                lone_zero: true,
                user_ns: Some(0),
                program_check_wait: true,
                local: Some(true),
                age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
                ..probe::StartFacts::default()
            }),
            ..ProcFacts::default()
        };
        let read = |before, fresh, after| {
            let version = Cell::new(before);
            let (rows, _) = held_list(
                &[],
                std::slice::from_ref(&member),
                Some(501),
                None,
                0,
                |_| {
                    version.set(after);
                    Some(judged(fresh))
                },
                |_| version.get(),
                |_| true,
                |_| None,
            );
            assert_eq!(rows.len(), 1, "the cached descriptive held row remains");
            rows[0].row.exec_version
        };
        assert_eq!(read(Some(17), AtStart::Held, Some(17)), Some(17));
        assert_eq!(read(Some(17), AtStart::Held, Some(18)), None);
        assert_eq!(read(Some(17), AtStart::Started, Some(17)), None);
        assert_eq!(read(None, AtStart::Held, None), None);
    }

    /// Each held row forks its start facts' age before the clock those ages were taken at: a
    /// member by its own reading's facts, a row outside every job by the start reader's, never by
    /// the sweep's kernel start (Mach ticks on macOS). The stand-in outside every job is read
    /// held unasked, so its fork time is unread.
    #[test]
    fn a_held_row_forks_its_start_facts_age_before_the_pass_clock() {
        const ME: u32 = 501;
        const NOW_US: u64 = 1_790_000_000_000_000;
        let row = |pid| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(2_000_000),
            start: Some(987_654_321),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let held_for = |age_ms| StartFacts {
            age_ms: Some(age_ms),
            ..judged(AtStart::Held)
        };
        let member = ProcFacts {
            pid: 10,
            uid: Some(ME),
            name: "gh".into(),
            start: Some(987_654_321),
            start_facts: Some(held_for(90_000)),
            ..ProcFacts::default()
        };
        let (held, _) = held_list(
            &[row(10), row(20), row(30)],
            std::slice::from_ref(&member),
            Some(ME),
            Some(30),
            NOW_US,
            |pid| (pid == 20).then(|| held_for(25_000)),
            |_| None,
            |_| true,
            |_| None,
        );
        let forks: Vec<(u32, Option<u64>)> =
            held.iter().map(|h| (h.row.pid, h.row.started_ms)).collect();
        assert_eq!(
            forks,
            [
                (10, Some(NOW_US / 1000 - 90_000)),
                (20, Some(NOW_US / 1000 - 25_000)),
                (30, None),
            ]
        );
    }

    /// The pass hands the engine its held list: held rows and the stand-in reach the program-check
    /// findings, a translating row `translating`, and only those in this process's coalition count
    /// as aterm's.
    #[test]
    fn the_pass_hands_the_engine_this_users_held_programs_and_the_stand_in() {
        const ME: u32 = 501;
        let row = |pid, cpu_ms: u64| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let sweep = [row(10, 400), row(20, 2), row(21, 2), row(22, 2), row(23, 2)];
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |pid, _, _| {
                Some(judged(match pid {
                    20 | 23 => AtStart::Held,
                    22 => AtStart::Translating,
                    _ => AtStart::Started,
                }))
            },
            exec: |pid| Some(u64::from(pid)),
            coalition: |pid| Some(if pid == 23 { 8 } else { 7 }),
            exe_name: |pid| (pid == 20).then(|| "codex-command-runner".to_owned()),
            stand_in: || Some(10),
            ..HeldReaders::INERT
        };
        let view = pass
            .tick(
                10_000,
                || 10_000,
                Some(&sweep),
                &[],
                &[],
                &[],
                &GrantFacts::default(),
                10,
                false,
                true,
            )
            .expect("a view");
        let held: Vec<(u32, &str, Option<u64>)> = view
            .findings
            .program_check
            .iter()
            .map(|n| (n.pid, n.name.as_str(), n.tab))
            .collect();
        assert_eq!(
            held,
            [
                (10, "p10", None),
                (20, "codex-command-runner", None),
                (23, "p23", None)
            ],
            "the reader's held rows and the stand-in's, outside every tab"
        );
        assert_eq!(
            view.findings
                .translating
                .iter()
                .map(|n| n.pid)
                .collect::<Vec<_>>(),
            [22],
            "the translating row under its own kind"
        );
        assert_eq!(
            view.findings.program_check_aterm,
            (2, 0),
            "aterm's: the two in this process's coalition, not 23 in another"
        );
    }

    /// THE PASS NAMES STRANDED ONLY A HELD PROGRAM OLDER THAN A DAEMON THAT ANSWERS ALONE (leftover
    /// §5.1): with the syspolicyd running now started at T, up the bound awake, its pool not full,
    /// a member and a sweep row seen unrun since T − 8 s read stranded by the facts `held_list`
    /// kept, once every held program of this user reads no backlog. None does beside a held row
    /// forked at T or after (a backlog), nor with the daemon a µs short of the bound by the wall
    /// clock or the awake one; a full pool or no fresh look judges nothing; a row seen unrun only
    /// from T, a translating row, an undated one and the stand-in are never stranded. Every row is
    /// judged only while each is stranded or forked since T, and a row a pass judged stranded
    /// stays so whatever this one reads.
    #[test]
    fn the_pass_names_stranded_only_a_held_program_older_than_a_daemon_that_answers_alone() {
        use aterm_vitals::program_check::Daemon;
        const ME: u32 = 501;
        const SEC: u64 = 1_000_000;
        const T: u64 = 1_791_000_000 * SEC;
        const AWAKE_MS: u64 = probe::STRANDED_AFTER_US / 1_000;
        let daemon = Daemon {
            pid: 11291,
            start_s: T / SEC,
            start_us: 0,
        };
        let up = T + probe::STRANDED_AFTER_US;
        let awake = |pool_full, awake_ms| {
            Some(RunningCheck {
                daemon,
                pool_full,
                awake_ms,
            })
        };
        let check = |pool_full| awake(pool_full, AWAKE_MS);
        let forked = |at, fork_us| StartFacts {
            start_us: Some(fork_us),
            ..judged(at)
        };
        let row = |pid| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(2_000_000),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let member = ProcFacts {
            pid: 10,
            uid: Some(ME),
            name: "band".into(),
            start_facts: Some(forked(AtStart::Held, T - 8 * SEC)),
            ..ProcFacts::default()
        };
        let (rows, young) = held_list(
            &[row(20), row(30)],
            std::slice::from_ref(&member),
            Some(ME),
            Some(30),
            T,
            |_| Some(forked(AtStart::Held, T - 8 * SEC)),
            |pid| Some(u64::from(pid)),
            |_| false,
            |_| None,
        );
        assert_eq!(young, [], "none read unrun but unlisted");
        assert_eq!(
            rows.iter()
                .map(|h| (h.row.pid, h.facts))
                .collect::<Vec<_>>(),
            [
                (10, member.start_facts),
                (20, member.start_facts),
                (30, None)
            ],
            "the member's own reading, the sweep row's read, none for the stand-in"
        );
        let alone: Vec<Held> = rows
            .into_iter()
            .map(|h| Held {
                sent_by_us: Some(T - 8 * SEC),
                ..h
            })
            .collect();
        let verdict_by = |mut rows: Vec<Held>, check, now_us, backlog: Option<usize>| {
            let (by, every) = strand(&mut rows, check, now_us, |since| {
                assert_eq!(since, T, "the backlog since the daemon's start");
                backlog
            });
            let stranded: Vec<u32> = rows
                .iter()
                .filter(|h| h.row.stranded)
                .map(|h| h.row.pid)
                .collect();
            (by, every, stranded)
        };
        let verdict = |rows, check, now_us| verdict_by(rows, check, now_us, Some(0));
        assert_eq!(
            verdict(alone.clone(), check(false), up),
            (Some(11291), false, vec![10, 20]),
            "held since before the daemon running now, which answers alone; never the stand-in, \
             which no read judges"
        );
        let held_row = |pid, at, fork_us, sent_by_us| {
            let facts = forked(at, fork_us);
            Held {
                row: HeldProc {
                    pid,
                    exec: u64::from(pid),
                    exec_version: Some(u64::from(pid)),
                    name: format!("p{pid}"),
                    wait: start_wait(probe::judge_start(&facts)).expect("not started"),
                    ..HeldProc::default()
                },
                facts: Some(facts),
                cpu_ns: Some(2_000_000),
                sent_by_us: Some(sent_by_us),
                stranded_by: None,
            }
        };
        let beside = |pid, at, fork_us, sent_by_us| {
            let mut rows = alone.clone();
            rows.push(held_row(pid, at, fork_us, sent_by_us));
            rows
        };
        let traced = AtStart::StoppedUnstarted {
            stopped: false,
            traced: true,
        };
        for (case, rows, want) in [
            (
                "a held row forked at T + 1 µs, a slow first exec the daemon running now has yet \
                 to answer, is its backlog, and no verdict stands for the rows before T",
                beside(40, AtStart::Held, T + 1, T + 1),
                (Some(11291), false, vec![]),
            ),
            (
                "a held row forked at T is its backlog",
                beside(41, AtStart::Held, T, T),
                (Some(11291), false, vec![]),
            ),
            (
                "CONTROL: a row forked before T but seen unrun only from T on may have exec'd \
                 since, so it is not stranded, and the others still are",
                beside(42, AtStart::Held, T - 8 * SEC, T),
                (Some(11291), false, vec![10, 20]),
            ),
            (
                "CONTROL: a translating row forked since T is never stranded nor a backlog",
                beside(43, AtStart::Translating, T + SEC, T + SEC),
                (Some(11291), false, vec![10, 20]),
            ),
            (
                "CONTROL: a traced unstarted row forked since T is never stranded nor a backlog",
                beside(46, traced, T + SEC, T + SEC),
                (Some(11291), false, vec![10, 20]),
            ),
            (
                "CONTROL: rows all forked since T, one translating and one traced, are all judged, \
                 none stranded",
                vec![
                    held_row(44, AtStart::Held, T + SEC, T + SEC),
                    held_row(45, AtStart::Translating, T - 8 * SEC, T - 8 * SEC),
                    held_row(47, traced, T - 8 * SEC, T - 8 * SEC),
                ],
                (Some(11291), true, vec![]),
            ),
        ] {
            assert_eq!(verdict(rows, check(false), up), want, "{case}");
        }
        assert_eq!(
            verdict(alone.clone(), check(false), up - 1),
            (Some(11291), false, vec![]),
            "the daemon a µs short of the bound on a first exec gives no verdict"
        );
        assert_eq!(
            verdict(alone.clone(), awake(false, AWAKE_MS - 1), up + 3_600 * SEC),
            (Some(11291), false, vec![]),
            "an hour up by the wall clock but a ms short of the bound awake: a sleep counts for \
             nothing"
        );
        for (backlog, want) in [(Some(1), vec![]), (None, vec![]), (Some(0), vec![10, 20])] {
            assert_eq!(
                verdict_by(alone.clone(), check(false), up, backlog),
                (Some(11291), false, want),
                "a held program of this user the pass does not list ({backlog:?}) is a backlog too"
            );
        }
        assert_eq!(
            verdict(alone.clone(), check(true), up),
            (Some(11291), false, vec![]),
            "CONTROL: a full pool judges nothing"
        );
        assert_eq!(
            verdict(alone.clone(), None, up),
            (None, false, vec![]),
            "CONTROL: no fresh look judges nothing"
        );
        let undated: Vec<Held> = alone
            .iter()
            .cloned()
            .map(|h| Held {
                sent_by_us: None,
                ..h
            })
            .collect();
        assert_eq!(
            verdict(undated, check(false), up),
            (Some(11291), false, vec![]),
            "an undated row is not judged"
        );
        let kept: Vec<Held> = alone
            .iter()
            .cloned()
            .map(|h| Held {
                stranded_by: h.facts.map(|_| 690),
                ..h
            })
            .collect();
        let mut later = kept.clone();
        later.push(held_row(40, AtStart::Held, T + 1, T + 1));
        for (case, rows, check, want) in [
            ("no fresh look", kept.clone(), None, Some(690)),
            ("a full pool", kept.clone(), check(true), Some(11291)),
            ("a backlog", later, check(false), Some(11291)),
        ] {
            assert_eq!(
                verdict(rows, check, up),
                (want, false, vec![10, 20]),
                "a row a pass judged stranded stays so beside {case}, against the daemon read now \
                 or else the one it was judged under"
            );
        }
    }

    /// THE PASS HANDS THE ENGINE THE DAEMON IT JUDGED AGAINST: a held row its passes read unrun at
    /// one CPU since before the syspolicyd the stuck check reads now started, once that daemon is
    /// up the bound, is the engine's stranded row and privacy's count, with the daemon's pid. A row
    /// first read after that start, and one whose CPU moved between the passes, start over and are
    /// only held; with no check read (the inert arm's) the same rows are held and none is judged.
    #[test]
    fn the_pass_hands_the_engine_the_daemon_it_judged_against() {
        use aterm_vitals::jobs::Named;
        use aterm_vitals::program_check::Daemon;
        use std::sync::atomic::{AtomicU64, Ordering};
        const ME: u32 = 501;
        const SEC: u64 = 1_000_000;
        const T: u64 = 1_791_000_000 * SEC;
        static NOW: AtomicU64 = AtomicU64::new(0);
        let row = |pid, cpu_ns| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ns),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let readers = HeldReaders {
            start: |_, _, _| {
                Some(StartFacts {
                    start_us: Some(T - 30 * SEC),
                    ..judged(AtStart::Held)
                })
            },
            exec: |pid| Some(u64::from(pid)),
            check: || {
                let (pid, started) = if NOW.load(Ordering::SeqCst) < T {
                    (690, T - 3_600 * SEC)
                } else {
                    (11291, T)
                };
                Some(RunningCheck {
                    daemon: Daemon {
                        pid,
                        start_s: started / SEC,
                        start_us: 0,
                    },
                    pool_full: false,
                    awake_ms: u64::MAX,
                })
            },
            backlog: |_| Some(0),
            now_us: || NOW.load(Ordering::SeqCst),
            ..HeldReaders::INERT
        };
        let run = |readers: HeldReaders| {
            let mut pass = JobsPass::new();
            pass.held = readers;
            NOW.store(T - 8 * SEC, Ordering::SeqCst);
            let first = [row(10, 400_000_000), row(20, 2_000_000), row(22, 2_000_000)];
            let _ = pass.tick(
                10_000,
                || 10_000,
                Some(&first),
                &[],
                &[],
                &[],
                &GrantFacts::default(),
                10,
                false,
                true,
            );
            NOW.store(T + probe::STRANDED_AFTER_US, Ordering::SeqCst);
            let then = [
                row(10, 400_000_000),
                row(20, 2_000_000),
                row(21, 2_000_000),
                row(22, 3_000_000),
            ];
            pass.tick(
                12_000,
                || 12_000,
                Some(&then),
                &[],
                &[],
                &[],
                &GrantFacts::default(),
                10,
                false,
                true,
            )
            .expect("a view")
        };
        let pids = |list: &[Named]| list.iter().map(|n| n.pid).collect::<Vec<_>>();
        let view = run(readers);
        let f = &view.findings;
        assert_eq!(pids(&f.program_check), [20, 21, 22], "every one held");
        assert_eq!(
            pids(&f.program_check_stranded),
            [20],
            "only the row read unrun at one CPU since before the daemon started"
        );
        assert_eq!(
            f.program_check_daemon,
            Some(11291),
            "the daemon judged against"
        );
        assert_eq!(
            program_check_of(&view, None, Duration::ZERO, false).map(|c| c.stranded),
            Some(Some(1))
        );
        let inert = run(HeldReaders {
            check: HeldReaders::INERT.check,
            ..readers
        });
        let f = &inert.findings;
        assert_eq!(pids(&f.program_check), [20, 21, 22], "CONTROL: still held");
        assert_eq!(
            (pids(&f.program_check_stranded), f.program_check_daemon),
            (vec![], None),
            "CONTROL: no check read judges nothing"
        );
        assert_eq!(
            program_check_of(&inert, None, Duration::ZERO, false).map(|c| c.stranded),
            Some(None)
        );
    }

    /// A PASS DATES A ROW FROM ITS FIRST UNRUN READ AND KEEPS ITS VERDICT: a row read unrun at
    /// T − 1 s, before the syspolicyd running now started at T, still younger than the held bar, is
    /// dated then, so it reads stranded once the daemon is up the bound; the verdict stands beside
    /// a backlog and through a pass with no fresh look. A row whose CPU moved, a young row whose
    /// thread ran, and a young row that read stopped start over.
    #[test]
    fn a_pass_dates_a_row_from_its_first_unrun_read_and_keeps_its_verdict() {
        use aterm_vitals::program_check::Daemon;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        const ME: u32 = 501;
        const SEC: u64 = 1_000_000;
        const T: u64 = 1_791_000_000 * SEC;
        static NOW: AtomicU64 = AtomicU64::new(0);
        static LOOK: AtomicBool = AtomicBool::new(true);
        let row = |pid, cpu_ns| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ns),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |pid, now_us, _| {
                let fork = match pid {
                    50..=52 => T - 6 * SEC,
                    60 => T + 300 * SEC,
                    _ => T - 30 * SEC,
                };
                let before = NOW.load(Ordering::SeqCst) < T;
                Some(StartFacts {
                    lone_zero: pid != 51 || !before,
                    user_ns: Some(0),
                    program_check_wait: true,
                    stopped: pid == 52 && before,
                    local: Some(true),
                    age_ms: Some(now_us.saturating_sub(fork) / 1_000),
                    start_us: Some(fork),
                    ..StartFacts::default()
                })
            },
            exec: |pid| Some(u64::from(pid)),
            check: || {
                let now = NOW.load(Ordering::SeqCst);
                let (pid, started) = if now < T {
                    (690, T - 3_600 * SEC)
                } else {
                    (11291, T)
                };
                LOOK.load(Ordering::SeqCst).then_some(RunningCheck {
                    daemon: Daemon {
                        pid,
                        start_s: started / SEC,
                        start_us: 0,
                    },
                    pool_full: false,
                    awake_ms: now.saturating_sub(started) / 1_000,
                })
            },
            backlog: |_| Some(0),
            now_us: || NOW.load(Ordering::SeqCst),
            ..HeldReaders::INERT
        };
        let mut tick = |at_ms, now_us, rows: &[ProcessReading]| {
            NOW.store(now_us, Ordering::SeqCst);
            let view = pass
                .tick(
                    at_ms,
                    || at_ms,
                    Some(rows),
                    &[],
                    &[],
                    &[],
                    &GrantFacts::default(),
                    10,
                    false,
                    true,
                )
                .expect("a view");
            let f = &view.findings;
            (
                f.program_check.iter().map(|n| n.pid).collect::<Vec<_>>(),
                f.program_check_stranded
                    .iter()
                    .map(|n| n.pid)
                    .collect::<Vec<_>>(),
                f.program_check_daemon,
                f.program_check_judged,
            )
        };
        let cpu = 2_000_000;
        let me = row(10, 400_000_000);
        let first = [
            me.clone(),
            row(20, cpu),
            row(22, cpu),
            row(50, cpu),
            row(51, cpu),
            row(52, cpu),
        ];
        assert_eq!(
            tick(10_000, T - SEC, &first),
            (vec![20, 22], vec![], Some(690), true),
            "50, 51 and 52 are five seconds old, under the held bar; 20 and 22 forked since 690 \
             started are judged, none stranded"
        );
        let up = T + probe::STRANDED_AFTER_US;
        assert_eq!(
            tick(12_000, up, &first),
            (
                vec![20, 22, 50, 51, 52],
                vec![20, 22, 50],
                Some(11291),
                false
            ),
            "50 read unrun before it was held, from T − 1 s; CONTROL: 51's thread ran at its \
             first read and 52 read stopped, so each dates from its second, after T"
        );
        let backlog = [
            me,
            row(20, cpu),
            row(22, cpu + 1),
            row(50, cpu),
            row(51, cpu),
            row(52, cpu),
            row(60, cpu),
        ];
        assert_eq!(
            tick(14_000, up + 2 * SEC, &backlog),
            (
                vec![20, 22, 50, 51, 52, 60],
                vec![20, 50],
                Some(11291),
                false
            ),
            "kept beside 60, a backlog forked at T + 5 m; CONTROL: 22's CPU moved, so its run \
             starts over and its verdict goes"
        );
        LOOK.store(false, Ordering::SeqCst);
        assert_eq!(
            tick(16_000, up + 4 * SEC, &backlog),
            (
                vec![20, 22, 50, 51, 52, 60],
                vec![20, 50],
                Some(11291),
                false
            ),
            "kept with no fresh look, against the daemon it was judged under"
        );
    }

    /// The pass hands the engine the grant's facts, also from the vitals request through
    /// `vitals_host::jobs_step`: aterm's held program forked after the grant counts against it;
    /// with no grant time nothing counts, and nothing outside every tab while a tab the facts name
    /// adopted is open.
    #[test]
    fn the_pass_hands_the_engine_the_grant_time_and_the_adopted_tabs() {
        const ME: u32 = 501;
        let row = |pid, cpu_ms: u64| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let pass = || {
            let mut pass = JobsPass::new();
            pass.held = HeldReaders {
                start: |_, _, _| Some(judged(AtStart::Held)),
                coalition: |_| Some(7),
                ..HeldReaders::INERT
            };
            pass
        };
        let tab = TabRoot {
            key: 41,
            tab: 41,
            shell_pid: 941,
            label: "Tab 1".into(),
            program: String::new(),
            tab_id: None,
            agent_leader: None,
        };
        let after_grant = |grant: &GrantFacts| {
            pass()
                .tick(
                    10_000,
                    || 10_000,
                    Some(&[row(10, 400), row(20, 2)]),
                    std::slice::from_ref(&tab),
                    &[],
                    &[],
                    grant,
                    10,
                    false,
                    true,
                )
                .expect("a view")
                .findings
                .program_check_after_grant
        };
        let granted = |adopted: Vec<u64>| GrantFacts {
            grant_ms: Some(1),
            adopted,
        };
        assert_eq!(
            after_grant(&granted(Vec::new())),
            1,
            "aterm's held program forked after the grant"
        );
        assert_eq!(after_grant(&GrantFacts::default()), 0, "no grant time");
        assert_eq!(
            after_grant(&granted(vec![41])),
            0,
            "outside every tab while a tab is adopted"
        );

        let req = crate::vitals_host::Request {
            self_pid: 10,
            jobs_now: true,
            grant: granted(Vec::new()),
            ..crate::vitals_host::Request::default()
        };
        let epoch = Instant::now();
        let view = crate::vitals_host::jobs_step(
            &mut pass(),
            &req.tab_reads(epoch, || epoch),
            Some(&[row(10, 400), row(20, 2)]),
            &req,
            false,
        )
        .expect("a view");
        assert_eq!(
            view.findings.program_check_after_grant, 1,
            "the request's grant reaches the engine"
        );
    }

    /// With this process's coalition unread, no held program outside every job counts as aterm's:
    /// one whose own coalition is unread too is no match.
    #[test]
    fn with_this_process_coalition_unread_no_held_program_is_aterms() {
        const ME: u32 = 501;
        let row = |pid, cpu_ms: u64| ProcessReading {
            pid,
            uid: Some(ME),
            cpu_ns: Some(cpu_ms * 1_000_000),
            name: format!("p{pid}"),
            ..ProcessReading::default()
        };
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |_, _, _| Some(judged(AtStart::Held)),
            coalition: |_| None,
            ..HeldReaders::INERT
        };
        let view = pass
            .tick(
                10_000,
                || 10_000,
                Some(&[row(10, 400), row(20, 2)]),
                &[],
                &[],
                &[],
                &GrantFacts::default(),
                10,
                false,
                true,
            )
            .expect("a view");
        assert_eq!(
            view.findings
                .program_check
                .iter()
                .map(|n| n.pid)
                .collect::<Vec<_>>(),
            [20],
            "held"
        );
        assert_eq!(view.findings.program_check_aterm.0, 0, "not aterm's");
    }

    /// A root's program-change version stays separate from a running member's naming identity.
    /// Unread, held and stand-in members are never asked their exec version.
    #[test]
    fn root_program_versions_and_running_member_identities_are_read_separately() {
        use std::cell::RefCell;
        let read = [
            ProcFacts {
                pid: 10,
                ..ProcFacts::default()
            },
            ProcFacts {
                pid: 11,
                ..ProcFacts::default()
            },
            ProcFacts {
                pid: 12,
                start_facts: Some(judged(AtStart::Started)),
                ..ProcFacts::default()
            },
            ProcFacts {
                pid: 13,
                start_facts: Some(judged(AtStart::Held)),
                ..ProcFacts::default()
            },
            ProcFacts {
                pid: 14,
                start_facts: Some(judged(AtStart::Started)),
                ..ProcFacts::default()
            },
        ];
        let asked = RefCell::new(Vec::new());
        let procs = job_procs(
            &read,
            &BTreeSet::from([10]),
            Some(14),
            |pid| {
                asked.borrow_mut().push(pid);
                Some(u64::from(pid) * 100 + 7)
            },
            |_| None,
        );
        assert_eq!(
            procs
                .iter()
                .map(|p| (p.pid, p.version, p.exec_version))
                .collect::<Vec<_>>(),
            [
                (10, Some(1007), None),
                (11, None, None),
                (12, None, Some(1207)),
                (13, None, None),
                (14, None, None)
            ]
        );
        assert_eq!(asked.into_inner(), [10, 12]);
    }

    /// A forced reading without a sweep cannot authorize naming from its cached jobs view.
    #[test]
    fn naming_needs_an_actual_jobs_pass_instead_of_a_carried_view() {
        let mut pass = JobsPass::new();
        let target = aterm_vitals::sys::query::NameTarget::Job("missing".into());
        for (at_ms, sweep, now, expected) in [
            (1, Some(&[][..]), true, NamingError::Gone),
            (2, Some(&[][..]), false, NamingError::Unread),
            (3, None, true, NamingError::Unread),
        ] {
            assert!(
                pass.tick(
                    at_ms,
                    || at_ms,
                    sweep,
                    &[],
                    &[],
                    &[],
                    &GrantFacts::default(),
                    0,
                    false,
                    now
                )
                .is_some()
            );
            assert!(matches!(pass.capture_name(7, &target), Err(error) if error == expected));
        }
    }

    /// How many times a stand-in's arguments, and a stand-in ledger, were read.
    static FRONT_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static LEDGER_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    /// Whether the stand-in ledger holds pid 40's claim (admitted) rather than keeping it waiting.
    static ADMITTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// The hop's argv for `tool` at `target`.
    fn hop(tool: &str, target: &str) -> Vec<OsString> {
        [
            "/Applications/aterm.app/Contents/MacOS/atpkg",
            "__build_admit",
            "--shim",
            tool,
            "-",
            target,
            "--unverified",
            "build",
        ]
        .iter()
        .map(Into::into)
        .collect()
    }

    /// Pids 40 and 46 run a shim's front (the hop's argv; 46's target a name `status` cleans,
    /// `targo@dev`), 41 another verb of the same image; each read counts.
    fn stand_in_argv(pid: u32) -> Option<Vec<OsString>> {
        FRONT_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(match pid {
            40 => hop("targo", "/store/trust/bin/targo"),
            46 => hop("targo-tippy", "/store/trust/bin/targo@dev"),
            41 => [
                "/Applications/aterm.app/Contents/MacOS/atpkg",
                "__build_cache_register",
                "targo",
            ]
            .iter()
            .map(Into::into)
            .collect(),
            _ => return None,
        })
    }

    /// The stand-in ledger: pid 77 holds 8 GiB; pid 40's claim waits behind it, or is held once
    /// [`ADMITTED`]; pid 46's waits behind both.
    fn stand_in_claims(_: &aterm_capacity::Ledger) -> Option<Vec<aterm_capacity::Record>> {
        LEDGER_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let record = |pid: u32, start_ms: u64, held: bool| aterm_capacity::Record {
            accounting: aterm_capacity::Accounting::Holder,
            timing: aterm_capacity::TimingEvidence::default(),
            pid,
            start_ms,
            seq: 0,
            checkout: "/ws".to_owned(),
            need: aterm_capacity::Need::new("a build").mem(8 << 30),
            held: held.then_some(aterm_capacity::Held {
                admitted_ms: start_ms,
                cores: 1,
                alone: false,
            }),
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        let admitted = ADMITTED.load(std::sync::atomic::Ordering::SeqCst);
        Some(vec![
            record(77, 1_000, true),
            record(40, 2_000, admitted),
            record(46, 3_000, false),
        ])
    }

    /// A MEMBER IS NAMED BY THE BUILD ITS FRONT BECOMES, WAITING BEHIND THE CLAIMS ITS LEDGER
    /// NAMES: only a member running the front's image is asked, its arguments read once for the
    /// program it runs and its ledger once a pass for every front that claims there; it is named
    /// by the one rule `status` names a front by (`targo@dev` reads `targodev` in both); once
    /// its ledger holds its claim it is still named by its build but waits for none. A member whose
    /// thread list shows it never ran (held mid-start) and the development stand-in are never
    /// read. NEGATIVE CONTROL: another verb of the same image becomes nothing, a build typed
    /// plainly and a front that exec'd its build are not read, being no front's image.
    #[test]
    fn a_member_is_named_by_the_build_its_front_becomes() {
        use std::sync::atomic::Ordering;
        let ran = |pid: u32, name: &str, lone_zero: bool| ProcFacts {
            pid,
            name: name.to_owned(),
            exe: Some(name.to_owned()),
            start: Some(u64::from(pid)),
            start_facts: Some(probe::StartFacts {
                lone_zero,
                ..probe::StartFacts::default()
            }),
            ..ProcFacts::default()
        };
        let read = [
            ran(40, "atpkg", false),
            ran(41, "atpkg", false),
            ran(42, "targo", false),
            ran(43, "atpkg", true),
            ProcFacts {
                pid: 44,
                ..ProcFacts::default()
            },
            ran(46, "atpkg", false),
        ];
        let readers = FrontReaders {
            argv: stand_in_argv,
            ledger: |_| {
                Some(aterm_capacity::Ledger::at(std::path::PathBuf::from(
                    "/scratch",
                )))
            },
            claims: stand_in_claims,
        };
        let mut fronts = Fronts::new(readers);
        let pass = |fronts: &mut Fronts, read: &[ProcFacts], stand_in: Option<u32>| {
            let procs = job_procs(
                read,
                &BTreeSet::new(),
                stand_in,
                |_| None,
                |p| fronts.front(p),
            )
            .into_iter()
            .map(|p| (p.pid, p.becomes, p.claims_ahead))
            .collect::<Vec<_>>();
            fronts.keep(read);
            procs
        };
        let reads = || {
            (
                FRONT_READS.load(Ordering::SeqCst),
                LEDGER_READS.load(Ordering::SeqCst),
            )
        };
        FRONT_READS.store(0, Ordering::SeqCst);
        LEDGER_READS.store(0, Ordering::SeqCst);
        ADMITTED.store(false, Ordering::SeqCst);
        let named = pass(&mut fronts, &read, None);
        assert_eq!(
            named,
            [
                (40, Some("targo".to_owned()), Some(vec![77])),
                (41, None, None),
                (42, None, None),
                (43, None, None),
                (44, None, None),
                (46, Some("targodev".to_owned()), Some(vec![77, 40])),
            ]
        );
        assert_eq!(
            crate::session_status::program::front_program(&hop("targo-tippy", "/b/targo@dev")),
            Some("targodev".to_owned()),
            "as `status` names it"
        );
        assert_eq!(
            reads(),
            (3, 1),
            "the members that ran its image; one ledger read a pass"
        );
        assert_eq!(pass(&mut fronts, &read, None), named);
        assert_eq!(
            reads(),
            (3, 2),
            "arguments once a program, the ledger each pass"
        );
        ADMITTED.store(true, Ordering::SeqCst);
        let admitted = pass(&mut fronts, &read[..1], None);
        assert_eq!(
            admitted,
            [(40, Some("targo".to_owned()), None)],
            "its ledger holds its claim"
        );
        let execd = [ran(40, "targo", false)];
        assert_eq!(pass(&mut fronts, &execd, None), [(40, None, None)]);
        assert_eq!(reads(), (3, 3), "the build it exec'd is not read");
        let mut again = Fronts::new(readers);
        assert_eq!(pass(&mut again, &read[..1], Some(40)), [(40, None, None)]);
        assert_eq!(reads(), (3, 3), "the stand-in is never read");
    }

    /// The stand-in reads held only where the reading judged a start (never a zombie's or another
    /// user's), and a member the kernel reads held stays held.
    #[test]
    fn a_stand_in_reads_held_only_where_the_reading_judged_a_start() {
        let read = ProcFacts {
            pid: 30,
            start_facts: Some(probe::StartFacts::default()),
            ..ProcFacts::default()
        };
        let unread = ProcFacts {
            pid: 31,
            ..ProcFacts::default()
        };
        let held = ProcFacts {
            pid: 32,
            start_facts: Some(probe::StartFacts {
                lone_zero: true,
                user_ns: Some(0),
                program_check_wait: true,
                local: Some(true),
                age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
                ..probe::StartFacts::default()
            }),
            ..ProcFacts::default()
        };
        let held_at = |p: &ProcFacts, stand_in| job_proc(p, stand_in).start_wait;
        assert_eq!(held_at(&read, Some(30)), Some(StartWait::ProgramCheck));
        assert_eq!(held_at(&read, None), None);
        assert_eq!(
            held_at(&unread, Some(31)),
            None,
            "no start read: a zombie, or another user's"
        );
        assert_eq!(
            held_at(&held, Some(30)),
            Some(StartWait::ProgramCheck),
            "the kernel's own"
        );
    }

    /// A member that has not started hands the engine what keeps it from its first instruction,
    /// each as its reading judges it: Rosetta translating it, a volume not proven local, a stop or
    /// a tracer; one that started hands none. NEGATIVE CONTROL: hand the engine only a held start,
    /// and a member Rosetta translates reads `wait=unnamed` beside its own `translating` finding.
    #[test]
    fn a_member_that_has_not_started_hands_the_engine_its_start_wait() {
        let at = |translated, local, stopped| ProcFacts {
            pid: 40,
            start_facts: Some(probe::StartFacts {
                lone_zero: true,
                user_ns: Some(0),
                program_check_wait: true,
                stopped,
                translated,
                local,
                age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
                ..probe::StartFacts::default()
            }),
            ..ProcFacts::default()
        };
        let wait = |p: &ProcFacts| job_proc(p, None).start_wait;
        assert_eq!(
            wait(&at(true, Some(true), false)),
            Some(StartWait::Translating)
        );
        assert_eq!(wait(&at(false, None, false)), Some(StartWait::NotStarted));
        assert_eq!(
            wait(&at(false, Some(true), true)),
            Some(StartWait::StoppedUnstarted {
                stopped: true,
                traced: false
            })
        );
        assert_eq!(
            wait(&at(false, Some(true), false)),
            Some(StartWait::ProgramCheck)
        );
        let started = ProcFacts {
            pid: 41,
            start_facts: Some(probe::StartFacts::default()),
            ..ProcFacts::default()
        };
        assert_eq!(wait(&started), None, "it ran past its start");
    }

    /// A leftover view offering a closed tab's row (an owner gone), which a reading with no window
    /// reads again within 30 s.
    pub(crate) fn offered_left() -> LeftView {
        LeftView {
            rows: vec![aterm_vitals::left::LeftRow {
                pid: 2606,
                state: aterm_vitals::left::State::Left,
                peer: None,
                check_ms: None,
                started_ms: Some(1_500),
                cpu_ms: None,
                exe_judged: None,
                tier: aterm_vitals::left::Tier::Offer,
                why: aterm_vitals::left::Why::OwnerGone,
                proof: Vec::new(),
                listen: false,
                from: aterm_vitals::left::Source::Closed(7),
                teller: None,
                token: None,
                foreground: false,
                after_agent: false,
            }],
            ..LeftView::default()
        }
    }

    /// A view speaks for now for a minute, and one read again only on edges (a parked one, or a
    /// running job's: its own demand arms nothing with no window) for as long as its reader runs;
    /// one read on a timer (a suspect agent call, a held wait, a leftover offer read with it) is
    /// not read on edges, so past the minute it does not. A program that never ran and reads
    /// stopped or traced is such a wait: a debugger's resume raises no edge. NEGATIVE CONTROL:
    /// leave its finding out of `holds_a_wait`, and that view speaks past the minute.
    #[test]
    fn a_view_read_only_on_edges_speaks_for_now_while_its_reader_runs() {
        use aterm_vitals::jobs::{Findings, HeldEpisode, Named, StoppedUnstarted, Verdict};
        let parked = JobsView::default();
        assert_eq!(parked.demand, Demand::Idle);
        let running = JobsView {
            demand: Demand::Fast,
            rows: vec![crate::stats_paint::tests::job_row(
                "j41",
                Verdict::Fine,
                None,
                Some(1_000),
            )],
            ..JobsView::default()
        };
        let watched = JobsView {
            demand: Demand::Fast,
            rows: vec![JobRow {
                standing: Some(Standing::Capped),
                ..call_row(0, Consumer::AgentBg, true, 31, 27098, "python3")
            }],
            ..JobsView::default()
        };
        assert!(windowless_after(&watched).is_some(), "{watched:?}");
        let late = JOBS_FRESH + Duration::from_secs(1);
        assert!(speaks_for_now(&watched, None, JOBS_FRESH, false));
        assert!(
            speaks_for_now(&parked, None, late, true),
            "parked, read on edges"
        );
        assert!(
            speaks_for_now(&running, None, late, true),
            "running, read on edges"
        );
        assert!(!speaks_for_now(&watched, None, late, true), "due long ago");
        assert!(!speaks_for_now(&parked, None, late, false), "no reader");
        assert!(!speaks_for_now(&running, None, late, false), "no reader");
        let held = JobsView {
            findings: Findings {
                program_check: vec![Named {
                    pid: 25311,
                    exec_version: None,
                    name: "gh".into(),
                    tab: None,
                    job: None,
                    for_ms: 35_940_000,
                }],
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let episode = JobsView {
            findings: Findings {
                program_check_episode: Some(HeldEpisode::default()),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let translating = JobsView {
            findings: Findings {
                translating: held.findings.program_check.clone(),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let unstarted = JobsView {
            findings: Findings {
                not_started: held.findings.program_check.clone(),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        let stopped = JobsView {
            findings: Findings {
                stopped_unstarted: vec![StoppedUnstarted {
                    named: held.findings.program_check[0].clone(),
                    stopped: true,
                    traced: true,
                }],
                ..Findings::default()
            },
            ..JobsView::default()
        };
        for view in [&held, &episode, &translating, &unstarted, &stopped] {
            assert_eq!(view.demand, Demand::Idle);
            assert!(speaks_for_now(view, None, JOBS_FRESH, true), "{view:?}");
            assert!(
                !speaks_for_now(view, None, late, true),
                "a held wait is read on a timer, not on edges: {view:?}"
            );
        }
    }

    /// A reading whose leftover view offers a closed tab's row is read again within 30 s with no
    /// window (`vitals_host::held_wait_after`), so its jobs view, parked or running, speaks for now
    /// for a minute and then, its reader having stopped delivering, no more; read without the
    /// offer, the same views speak on. NEGATIVE CONTROL: `read_on_a_timer` that looks at the jobs
    /// view alone lets both old views speak past the minute.
    #[test]
    fn a_view_read_with_a_leftover_offer_is_read_on_a_timer() {
        use aterm_vitals::jobs::Verdict;
        let offered = offered_left();
        assert!(aterm_vitals::left::holds_an_offer(&offered));
        let parked = JobsView::default();
        let running = JobsView {
            demand: Demand::Fast,
            rows: vec![crate::stats_paint::tests::job_row(
                "j41",
                Verdict::Fine,
                None,
                Some(1_000),
            )],
            ..JobsView::default()
        };
        let late = JOBS_FRESH + Duration::from_secs(1);
        for view in [&parked, &running] {
            assert!(read_on_a_timer(view, Some(&offered)), "{view:?}");
            assert!(speaks_for_now(view, Some(&offered), JOBS_FRESH, true));
            assert!(
                !speaks_for_now(view, Some(&offered), late, true),
                "the offer's 30 s re-read never came: {view:?}"
            );
            assert!(
                speaks_for_now(view, Some(&LeftView::default()), late, true),
                "no offer, read on edges: {view:?}"
            );
        }
        assert_eq!(
            program_check_of(&parked, Some(&offered), late, true),
            None,
            "privacy's count pulls past the minute"
        );
        assert_eq!(
            program_check_of(&parked, None, late, true).map(|c| (c.held, c.oldest_ms)),
            Some((0, 61_000)),
            "none held, read on edges"
        );
    }

    /// Privacy's count: every held program of this user, aterm's and others', the longest held to
    /// now, and how many are stranded: a count whenever one is named, `0` only when the view judged
    /// every held row, none otherwise; nothing from a view that does not speak for now.
    #[test]
    fn privacys_count_is_every_held_program_and_the_longest_wait_to_now() {
        use aterm_vitals::jobs::{Findings, Named};
        let named = |pid, for_ms| Named {
            pid,
            exec_version: None,
            name: "gh".into(),
            tab: None,
            job: None,
            for_ms,
        };
        let view = JobsView {
            findings: Findings {
                program_check: vec![named(25311, 35_940_000), named(25312, 20_000)],
                program_check_aterm: (1, 20_000),
                ..Findings::default()
            },
            ..JobsView::default()
        };
        assert_eq!(
            program_check_of(&view, None, Duration::from_millis(1_500), false),
            Some(HeldCount {
                held: 2,
                oldest_ms: 35_941_500,
                stranded: None
            }),
            "both held, the oldest plus the view's age, none judged stranded or not"
        );
        assert_eq!(
            program_check_of(&JobsView::default(), None, Duration::ZERO, false),
            Some(HeldCount {
                held: 0,
                oldest_ms: 0,
                stranded: None
            }),
            "none held"
        );
        assert_eq!(
            program_check_of(&view, None, JOBS_FRESH + Duration::from_secs(1), true),
            None,
            "a view that does not speak for now"
        );
        let judged = |stranded: Vec<Named>, every| JobsView {
            findings: Findings {
                program_check_stranded: stranded,
                program_check_daemon: Some(11291),
                program_check_judged: every,
                ..view.findings.clone()
            },
            ..JobsView::default()
        };
        for (stranded, every, want) in [
            (vec![named(25311, 35_940_000)], false, Some(1)),
            (Vec::new(), true, Some(0)),
            (Vec::new(), false, None),
        ] {
            assert_eq!(
                program_check_of(&judged(stranded, every), None, Duration::ZERO, false)
                    .map(|c| c.stranded),
                Some(want),
                "a view counts its stranded rows, and none only when it judged every row \
                 ({every})"
            );
        }
    }

    /// THE BOARD'S HELD PROGRAMS ARE THIS SESSION'S CALLS' AND THEIR ORPHANS': of the view's held
    /// rows, session `s-a` (tab 4) is given the members named its agent's calls (a live call's, and
    /// the orphan an ended call left, which the engine names by the call), oldest first, each held
    /// for its `for_ms` as of the view, with the view's age; not the member of its shell's job or
    /// of what one left (the person's), of a nested terminal session, tab 5's call (session
    /// `s-b`'s) or a row no tab holds. A session with no held row is given none; a view past
    /// [`JOBS_FRESH`] and a board no view was posted to give no reading.
    #[test]
    fn the_boards_held_programs_are_this_sessions_calls_and_their_orphans() {
        use aterm_vitals::jobs::{Findings, Named};
        let held = |pid, tab, job, for_ms| Named {
            pid,
            exec_version: Some(u64::from(pid)),
            name: "git-remote-https".into(),
            tab,
            job,
            for_ms,
        };
        let view = Arc::new(JobsView {
            findings: Findings {
                program_check: vec![
                    held(601, Some(4), Some(JobKind::AgentCall), 30_000),
                    held(602, Some(4), Some(JobKind::AgentCall), 1_140_000),
                    held(603, Some(4), Some(JobKind::Shell), 9_000_000),
                    held(604, Some(5), Some(JobKind::AgentCall), 5_000),
                    held(605, None, None, 9_999_000),
                    held(606, Some(4), Some(JobKind::Nested), 9_000_000),
                ],
                ..Findings::default()
            },
            ..JobsView::default()
        });
        let sids = BTreeMap::from([
            (4, "s-a".to_string()),
            (5, "s-b".to_string()),
            (6, "s-c".to_string()),
        ]);
        let board = JobsBoard::new();
        let posted = Instant::now();
        board.post(Arc::clone(&view), sids.clone(), posted);
        let later = posted + Duration::from_secs(2);
        let pids = |sid: &str| {
            board.held(sid, later).map(|(age, rows)| {
                (
                    age,
                    rows.iter()
                        .map(|h| (h.pid, h.exec_version, h.held_ms))
                        .collect(),
                )
            })
        };
        assert_eq!(
            pids("s-a"),
            Some((
                Duration::from_secs(2),
                vec![(602, Some(602), 1_140_000), (601, Some(601), 30_000)]
            ))
        );
        assert_eq!(
            pids("s-b"),
            Some((Duration::from_secs(2), vec![(604, Some(604), 5_000)]))
        );
        assert_eq!(pids("s-c"), Some((Duration::from_secs(2), Vec::new())));
        assert_eq!(
            board.held("s-a", posted + JOBS_FRESH + Duration::from_secs(1)),
            None,
            "a view past the bound"
        );
        assert_eq!(JobsBoard::new().held("s-a", later), None, "no view");
    }

    /// The board, posted `posted`, over the engine's view of held programs outside every job
    /// that W-session ties to tab 4 (session `s-a`), one born after its agent started (702) and
    /// one before (704), and to tab 5 (`s-b`, 703, after its agent), beside one call's member of
    /// tab 4's (701).
    fn tied_board(posted: Instant) -> JobsBoard {
        use aterm_vitals::jobs::{HeldProc, JobTick, Jobs, StartWait};
        use aterm_vitals::left::Tie;
        let held = |pid| HeldProc {
            pid,
            exec: u64::from(pid),
            exec_version: Some(u64::from(pid)),
            name: "sh".into(),
            aterm: true,
            wait: StartWait::ProgramCheck,
            ..HeldProc::default()
        };
        let tie = |key, after_agent| Tie {
            key,
            closed: false,
            after_agent,
        };
        let mut rows = vec![held(702), held(703), held(704)];
        name_tied(
            &mut rows,
            &BTreeMap::from([
                (702, tie(4, true)),
                (703, tie(5, true)),
                (704, tie(4, false)),
            ]),
        );
        let mut engine = Jobs::new();
        let tick = |at_ms| JobTick {
            at_ms,
            read_ms: at_ms,
            self_uid: Some(501),
            held: rows.clone(),
            ..JobTick::default()
        };
        engine.absorb(&tick(1_000));
        let mut view = engine.absorb(&tick(601_000));
        view.findings.program_check.push(aterm_vitals::jobs::Named {
            pid: 701,
            exec_version: Some(701),
            name: "sh".into(),
            tab: Some(4),
            job: Some(JobKind::AgentCall),
            for_ms: 30_000,
        });
        let view = Arc::new(view);
        let board = JobsBoard::new();
        board.post(
            view,
            BTreeMap::from([(4, "s-a".to_string()), (5, "s-b".to_string())]),
            posted,
        );
        board
    }

    /// A SESSION-MATE TIED HELD ROW COUNTS ONCE ITS AGENT HAD STARTED: a held program in no job
    /// (born and orphaned between two readings) that a session-mate's environment ties to this
    /// tab, born after the tab's agent started, is this tab's agent's to be told of, beside its
    /// calls' members; one born before the agent started is the person's, never told.
    /// NEGATIVE CONTROL: name every tied program an agent's call, and the person's is told too.
    #[test]
    fn a_session_mate_tied_held_row_counts() {
        let posted = Instant::now();
        let board = tied_board(posted);
        let (_, rows) = board
            .held("s-a", posted + Duration::from_secs(1))
            .expect("a fresh view");
        let pids: Vec<u32> = rows.iter().map(|h| h.pid).collect();
        assert_eq!(
            pids,
            vec![702, 701],
            "held longest first, the person's never"
        );
    }

    /// ANOTHER SESSION'S HELD ROW DOES NOT COUNT: a program W-session ties to tab 5 is session
    /// `s-b`'s, never `s-a`'s, and a session no held row is tied to is told of none.
    #[test]
    fn another_sessions_held_row_does_not() {
        let posted = Instant::now();
        let board = tied_board(posted);
        let later = posted + Duration::from_secs(1);
        let pids = |sid: &str| -> Vec<u32> {
            board
                .held(sid, later)
                .expect("a fresh view")
                .1
                .iter()
                .map(|h| h.pid)
                .collect()
        };
        assert_eq!(pids("s-b"), vec![703]);
        assert!(!pids("s-a").contains(&703));
        assert_eq!(pids("s-c"), Vec::<u32>::new());
    }

    /// A HELD PID IS PROVED ONLY IN A PROCESS AS OLD AS ITS TOLD WAIT ([`held_as_long`]): this
    /// process has run a moment, and a pid no process holds has run none. NEGATIVE CONTROL: a wait
    /// longer than this process has lived is a later process's, one that took the pid since; and
    /// this process, past its start, is never held.
    #[test]
    fn a_held_pid_is_proved_only_in_a_process_as_old_as_its_wait() {
        let me = std::process::id();
        assert!(has_run(me, 0), "this process has run");
        assert!(
            !has_run(me, 10 * 365 * 24 * 3_600_000),
            "a wait longer than this process has lived"
        );
        assert!(!has_run(u32::MAX - 1, 0), "a pid no process holds");
        assert!(
            !held_as_long(&HeldCall {
                pid: me,
                exec_version: (HeldReaders::KERNEL.exec)(me),
                held_ms: 0,
            }),
            "this process is past its start"
        );
    }

    /// A fork's age survives exec, but its old held duration does not. The unchanged exec is
    /// proved with the same pid and sufficient age; a later exec, an exec during the probe and
    /// an unavailable version are unproved even while the program is held and its fork is old.
    #[test]
    fn a_held_wait_is_reproved_only_for_its_original_exec() {
        let held = HeldCall {
            pid: 601,
            exec_version: Some(17),
            held_ms: 1_140_000,
        };
        let prove = |held: &HeldCall, versions: [Option<u64>; 2]| {
            let mut versions = versions.into_iter();
            held_as_long_with(
                held,
                |pid| {
                    assert_eq!(pid, 601, "the same pid across exec");
                    versions.next().flatten()
                },
                |_| true,
                |_, ms| 10_000_000 >= ms,
            )
        };
        assert!(prove(&held, [Some(17), Some(17)]), "the unchanged exec");
        assert!(
            !prove(&held, [Some(18), Some(18)]),
            "a newer exec in the same old process cannot inherit the wait"
        );
        assert!(!prove(&held, [Some(17), Some(18)]), "exec during proof");
        assert!(
            !prove(&held, [Some(17), None]),
            "the final identity was unread"
        );
        assert!(
            !prove(&held, [None, None]),
            "the current identity was unread"
        );
        assert!(
            !prove(
                &HeldCall {
                    exec_version: None,
                    ..held
                },
                [Some(17), Some(17)]
            ),
            "a birth fallback is no actual exec version"
        );
    }

    /// This process lives at its own start, a start one unit off is another process, and a pid no
    /// process holds lives at no start (the negative control).
    #[test]
    fn a_leader_lives_only_at_its_own_birth() {
        let me = std::process::id();
        let birth = JobsReader::new()
            .read(ReadReq {
                pid: me,
                fds: false,
            })
            .and_then(|f| f.start)
            .expect("this process's start");
        assert!(leader_lives(me, birth));
        assert!(!leader_lives(me, birth + 1));
        assert!(!leader_lives(u32::MAX - 1, birth));
    }
}
