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
    /// The syspolicyd the stuck check's look read, told whether a row of the pass waits at the
    /// program check ([`crate::program_check_watch::last_read_for`]): while one does, the watch
    /// is rung and only a fresh look is handed; what the stranded judge reads ([`strand`]).
    pub(crate) check: fn(bool) -> Option<RunningCheck>,
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
        check: crate::program_check_watch::last_read_for,
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
        check: |_| None,
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
        let mut procs = job_procs(&read, &roots, stand_in, readers.exec, |p| fronts.front(p));
        // One read per actual native pass, after detailed descriptors were sampled. Never
        // consult a request's path or HOME; tests remain inert like the other host readers.
        if !cfg!(test) {
            let waits = aterm_capacity::build_slot_wait::read(&read, now_us);
            for p in &mut procs {
                if let Some(wait) = waits
                    .iter()
                    .find(|w| w.process.pid == p.pid && Some(w.process.birth_us) == p.start)
                {
                    p.build_admission = Some(wait.blockers.iter().map(|h| h.pid).collect());
                }
            }
        }
        // Which members aterm's own server holds an `await` of parked, by pid and birth: in-process
        // state, no kernel read.
        crate::parked_awaits::mark(&mut procs, &crate::parked_awaits::PARKED, Instant::now());
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
        let waiting = held.iter().any(|h| h.row.wait == StartWait::ProgramCheck);
        let (program_check_daemon, program_check_judged) =
            strand(&mut held, (readers.check)(waiting), now_us, readers.backlog);
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
        crate::telemetry_host::jobs_passed(&mut self.jobs, clock, &self.calls);
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

    /// What the view says of session `sid`'s agent's background work at `now`, over every live
    /// agent call of the session's tab that is background, or not known to be foreground
    /// (`Consumer::Agent`): [`BackgroundEvidence::NoProgress`] with the call quiet longest when
    /// each reads `no_progress`; [`BackgroundEvidence::Waiting`] when one does not (it progresses,
    /// or waits on what it declared, and its end wakes the agent); `Unknown` with no view, one
    /// older than [`JOBS_FRESH`], or no call of this session's.
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
            return BackgroundEvidence::Waiting;
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
    /// worst (quiet longest) first, with its leader's pid for the caller's re-proof; how many other
    /// live calls of its the view names (each progressing or waiting on what it declared, whose
    /// end wakes the agent); and the view's age. `None` with no view or one older than
    /// [`JOBS_FRESH`]. Only the copy of the rows runs under the lock.
    pub(crate) fn facts(&self, sid: &str, now: Instant) -> Option<BoardFacts> {
        let (age, waiting, mut calls) = {
            let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            let posted = slot
                .as_ref()
                .filter(|p| now.saturating_duration_since(p.at) <= JOBS_FRESH)?;
            let live = || {
                posted
                    .view
                    .rows
                    .iter()
                    .filter(|r| posted.sids.get(&r.tab).is_some_and(|s| s == sid))
                    .filter(|r| background_call_live(r))
            };
            let waiting = live().filter(|r| !r.no_progress).count();
            let calls: Vec<(u64, u32, BackgroundCall)> = live()
                .filter(|r| r.no_progress)
                .map(|r| (r.quiet_ms.unwrap_or(0), r.leader.0, background_call(r)))
                .collect();
            (now.saturating_duration_since(posted.at), waiting, calls)
        };
        calls.sort_by_key(|c| std::cmp::Reverse(c.0));
        Some(BoardFacts {
            age,
            waiting: u32::try_from(waiting).unwrap_or(u32::MAX),
            calls: calls
                .into_iter()
                .map(|(_, leader, call)| (leader, call))
                .collect(),
        })
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
    let BoardFacts {
        age,
        waiting,
        calls,
    } = board.facts(sid, now)?;
    let age_ms = i64::try_from(age.as_millis()).unwrap_or(i64::MAX);
    Some(aterm_agent::supervise::FactsReading {
        as_of_ms: now_ms.saturating_sub(age_ms),
        waiting,
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

/// What [`JobsBoard::facts`] read of one session's agent's calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardFacts {
    /// The view's age.
    pub(crate) age: Duration,
    /// How many live calls of the agent's do not read `no_progress`.
    pub(crate) waiting: u32,
    /// Each call at no progress, worst first, with its leader's pid.
    pub(crate) calls: Vec<(u32, BackgroundCall)>,
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

/// The plain form of a row the facts seam reads: the process it names (the waiting one for a
/// wait, else the leader), the group, the leader's birth, the engine's episode, the agent's task
/// id, the quiet age, and what it waits on: a Tier B naming's word, else the engine's own wait
/// name, with the process holding that wait where one is named (a lock's holder, a pipe's peer).
/// Never the program's name: nothing the prompt channel carries comes from it.
fn background_call(r: &JobRow) -> BackgroundCall {
    let held_by = r
        .named
        .as_ref()
        .and_then(|n| n.holder.as_ref().map(|h| h.pid))
        .or_else(|| r.wait.as_ref().and_then(|w| w.peer));
    BackgroundCall {
        pid: r.wait.as_ref().map_or(r.leader.0, |w| w.leaf),
        pgid: r.pgid,
        birth: r.leader.1,
        episode: r.episode,
        task: r.task.clone(),
        quiet_ms: r.quiet_ms.unwrap_or(0),
        wait: r
            .named
            .as_ref()
            .map(|n| n.word.word())
            .or_else(|| r.wait.as_ref().map(|w| w.name.word()))
            .map(str::to_string),
        held_by,
    }
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
        build_admission: None,
        aterm_wait: false,
    }
}

#[cfg(test)]
#[path = "vitals_jobs_tests.rs"]
pub(crate) mod tests;
