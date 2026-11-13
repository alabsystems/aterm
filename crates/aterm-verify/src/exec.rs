// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running the stages' children.

use aterm_sysprobe::activity::Used;
use aterm_uds::spawnfd::{Child, Command, Stdio, Tether as _};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[path = "exec_account.rs"]
mod account;
pub use account::UsageMeter;

/// Where the child's output goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    /// Into the stage report, verbatim, above the ladder line it explains.
    Emit,
    /// `>/dev/null 2>&1`.
    Silent,
}

/// One child invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Extra environment on top of the inherited one, or the whole one under [`Cmd::exact_env`].
    pub envs: Vec<(OsString, OsString)>,
    /// Inherited variables removed for this command alone, after [`ExecEnv::add_env`] and before
    /// [`Self::envs`].
    pub remove_envs: Vec<OsString>,
    pub capture: Capture,
    /// Spawn under utility QoS rather than the run's inherited tier while a person may be at the
    /// desk ([`AWAY_AFTER`]).
    pub demoted: bool,
    /// The directory the child starts in, in place of [`ExecEnv::cwd`].
    pub cwd: Option<PathBuf>,
    /// Whether [`Cmd::envs`] is the child's whole environment with nothing inherited.
    pub exact_env: bool,
    /// [`finalize_exact_command`] froze this complete command and its deterministic spawn-time
    /// environment edits. Public fields remain inspectable, so the receipt is checked again before
    /// content evidence or spawn trusts it; any later mutation invalidates the receipt.
    finalized_receipt: Option<Box<FinalizedCommandReceipt>>,
    /// Text written at the head of the child's captured log before it starts ([`Capture::Emit`]).
    pub preamble: Option<String>,
    /// Grace period after a ceiling `SIGTERM` before the child's group is `SIGKILL`ed.
    pub term_grace: Option<Duration>,
    /// Whether the child's waits for admission or a lock count against its ceiling as work
    /// ([`Cmd::counting_waits`]); otherwise its tree marks them and the ceiling leaves them out.
    pub waits_count: bool,
    /// A test binary whose cases are read as they end ([`CaseWatch`]).
    pub test_cases: Option<CaseWatch>,
    /// The planner's libtest width, independent of its accounting charge in cores.
    planned_test_threads: Option<u32>,
}

/// How a test binary's cases are read while it runs (`exec_test_progress.rs`'s `LiveCases`):
/// each failed case is said at once (a `test-case-failed` event), and under `stop_at_red` the
/// first decisive one ends the binary ([`Run::stopped_at`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseWatch {
    /// cargo's re-run spec for the binary.
    pub spec: String,
    /// The executable cargo ran, which a hung row names even when the spec is shared.
    pub binary: String,
    /// The binary writes libtest's JSON events; a human log's failures are decided at its end.
    pub json: bool,
    /// End the binary at its first decisive red case: the run halts at its first red.
    pub stop_at_red: bool,
    /// How long neither a complete output line nor process-tree CPU may advance before the
    /// binary is hung. Production uses [`TEST_BINARY_HANG`]; tests inject a shorter window.
    pub hang_after: Duration,
}

impl Cmd {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
            remove_envs: Vec::new(),
            capture: Capture::Emit,
            demoted: false,
            cwd: None,
            exact_env: false,
            finalized_receipt: None,
            preamble: None,
            term_grace: None,
            waits_count: false,
            test_cases: None,
            planned_test_threads: None,
        }
    }

    /// A child replayed from a recorded invocation in `cwd` with `env` and nothing inherited.
    #[must_use]
    pub fn replayed(
        argv: &[OsString],
        cwd: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Self {
        let mut c = Self::new(argv.first().cloned().unwrap_or_default());
        c.args = argv.iter().skip(1).cloned().collect();
        c.envs = env;
        c.exact_env = true;
        c.cwd = Some(cwd.into());
        c
    }

    /// Write `text` at the head of the captured log before the child starts ([`Cmd::preamble`]).
    #[must_use]
    pub fn preamble(mut self, text: impl Into<String>) -> Self {
        self.finalized_receipt = None;
        self.preamble = Some(text.into());
        self
    }

    #[must_use]
    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.finalized_receipt = None;
        self.args.push(a.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.finalized_receipt = None;
        self.args.extend(it.into_iter().map(Into::into));
        self
    }

    /// Set `k` to `v` for the child: in place of a value set before, as the child sees only the
    /// last, so each variable is named once wherever the command is read back (a rerun line).
    #[must_use]
    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.finalized_receipt = None;
        let (k, v) = (k.into(), v.into());
        match self.envs.iter_mut().find(|(key, _)| *key == k) {
            Some(set) => set.1 = v,
            None => self.envs.push((k, v)),
        }
        self
    }

    /// Pin a test width already bounded by the dispatcher, and grant each of its `threads` test
    /// threads its share of the `cores` the binary is charged, at least one
    /// ([`test_thread_cores`]), as [`aterm_cores::GRANT_ENV`]: every pool a test sizes, and an
    /// `aterm` it boots, takes at most that many threads, so the binary's own parallelism stays
    /// within its charge. Waiting test threads may outnumber the binary's charged cores; the
    /// core grant still bounds its nested compiler jobs.
    #[must_use]
    pub(crate) fn test_width(mut self, threads: u32, cores: u32) -> Self {
        let threads = threads.max(1);
        self.finalized_receipt = None;
        self.planned_test_threads = Some(threads);
        self.envs.retain(|(name, _)| name != "RUST_TEST_THREADS");
        self.envs
            .push(("RUST_TEST_THREADS".into(), threads.to_string().into()));
        self.env(
            aterm_cores::GRANT_ENV,
            test_thread_cores(threads, cores).to_string(),
        )
    }

    /// Remove one inherited variable from this command. A later [`Self::env`] of the same name
    /// deliberately wins, matching [`std::process::Command`].
    #[must_use]
    pub fn remove_env(mut self, k: impl Into<OsString>) -> Self {
        self.finalized_receipt = None;
        let k = k.into();
        if !self.remove_envs.contains(&k) {
            self.remove_envs.push(k);
        }
        self
    }

    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.finalized_receipt = None;
        self.capture = c;
        self
    }

    /// Run this child at utility QoS (`taskpolicy -c utility`) where macOS has it, while a person
    /// may be at the desk ([`AWAY_AFTER`]): a compile-only child at the default tier stretches a
    /// typed program's echo, and with nobody there utility QoS only starves the check.
    #[must_use]
    pub fn demoted(mut self) -> Self {
        self.finalized_receipt = None;
        self.demoted = true;
        self
    }

    /// At its ceiling, send this child's group `SIGTERM` and give it up to `grace` before
    /// `SIGKILL`.
    #[must_use]
    pub fn term_grace(mut self, grace: Duration) -> Self {
        self.finalized_receipt = None;
        self.term_grace = Some(grace);
        self
    }

    /// Count this child's waits for admission or a lock as work ([`Cmd::waits_count`]): a test
    /// run's, whose tests wait on their own scratch locks and ledgers, so a test stuck in one is a
    /// hang its ceiling still ends. Its waits for a core are still left out, as any child's are,
    /// until its tree has been idle for [`IDLE_HANG`].
    #[must_use]
    pub fn counting_waits(mut self) -> Self {
        self.finalized_receipt = None;
        self.waits_count = true;
        self
    }

    /// The argv as printable strings.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }

    /// Whether spawn-time environment policy has been frozen into this exact command.
    #[must_use]
    pub(crate) fn runtime_environment_is_finalized(&self) -> bool {
        match &self.finalized_receipt {
            Some(receipt) => receipt.matches(self),
            None => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FinalizedCommandReceipt {
    program: PathBuf,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    remove_envs: Vec<OsString>,
    capture: Capture,
    demoted: bool,
    cwd: Option<PathBuf>,
    exact_env: bool,
    preamble: Option<String>,
    term_grace: Option<Duration>,
    waits_count: bool,
    test_cases: Option<CaseWatch>,
    planned_test_threads: Option<u32>,
}

impl FinalizedCommandReceipt {
    fn capture(command: &Cmd) -> Self {
        Self {
            program: command.program.clone(),
            args: command.args.clone(),
            envs: command.envs.clone(),
            remove_envs: command.remove_envs.clone(),
            capture: command.capture.clone(),
            demoted: command.demoted,
            cwd: command.cwd.clone(),
            exact_env: command.exact_env,
            preamble: command.preamble.clone(),
            term_grace: command.term_grace,
            waits_count: command.waits_count,
            test_cases: command.test_cases.clone(),
            planned_test_threads: command.planned_test_threads,
        }
    }

    fn matches(&self, command: &Cmd) -> bool {
        self.program == command.program
            && self.args == command.args
            && self.envs == command.envs
            && self.remove_envs == command.remove_envs
            && self.capture == command.capture
            && self.demoted == command.demoted
            && self.cwd == command.cwd
            && self.exact_env == command.exact_env
            && self.preamble == command.preamble
            && self.term_grace == command.term_grace
            && self.waits_count == command.waits_count
            && self.test_cases == command.test_cases
            && self.planned_test_threads == command.planned_test_threads
    }
}

/// The bytes a child's label grows to before its next argument is left out ([`stage_label`]);
/// a program longer than this is kept whole all the same.
pub const LABEL_MAX: usize = 160;

/// A child's label, the one its run record's `group` line, its survivor line and its `verify:`
/// lines carry: its argv joined by spaces, the program always and each next argument while the
/// whole fits [`LABEL_MAX`] bytes, then ` …+N args` naming how many were left out. So it is at
/// most [`LABEL_MAX`] bytes and that count unless its program alone is longer; the run record
/// bounds its encoded `group` line on its own (`runs::stage_field`).
#[must_use]
pub fn stage_label(argv: &[String]) -> String {
    let mut label = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            if label.len() + 1 + arg.len() > LABEL_MAX {
                let left = argv.len() - i;
                return format!("{label} …+{left} arg{}", if left == 1 { "" } else { "s" });
            }
            label.push(' ');
        }
        label.push_str(arg);
    }
    label
}

/// The invariant part of every invocation.
#[derive(Clone, Copy, Debug)]
pub struct ExecEnv<'a> {
    /// The run's data-only event sink; never inherited by the child.
    pub events: Option<&'a crate::event_log::EventLog>,
    pub cwd: &'a Path,
    pub path: &'a OsStr,
    pub scratch: &'a Path,
    /// The ceiling on one child's work, or `None` to wait forever: the time a wait of its tree
    /// for admission or a lock is marked ([`aterm_toolchain::waits`]) is left out, and so is the
    /// time it waited for a core ([`Host::group_usage`]).
    pub child_ceiling: Option<Duration>,
    /// Variables removed from every child's inherited environment before [`Cmd::envs`].
    pub remove_env: &'a [&'a str],
    /// Variables added to every child after [`Self::remove_env`] and before [`Cmd::envs`].
    pub add_env: &'a [(OsString, OsString)],
    /// The run's record of its stage groups, when it keeps one ([`crate::runs`]).
    pub stage_groups: Option<&'a dyn StageGroupEnd>,
    /// What a child's spawn and its clock read of the machine ([`LIVE_HOST`] in a run).
    pub host: &'a dyn Host,
    /// Owner-private directory beside the run log for a hung test binary's raw stack sample.
    /// It is made lazily only when a binary is proved hung.
    pub hang_samples: Option<&'a Path>,
}

/// What stack sampling retained. Collection can fail after the native sampler created and wrote
/// its owner-private report, so `report` and `error` are independent rather than a `Result`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackSample {
    /// The complete or partial raw all-thread report.
    pub report: Option<PathBuf>,
    /// Why collection did not complete, absent after a successful sample.
    pub error: Option<String>,
}

impl StackSample {
    fn failed(error: io::Error) -> Self {
        let report = aterm_uds::wait_sample::retained_report(&error).map(Path::to_path_buf);
        Self {
            report,
            error: Some(error.to_string()),
        }
    }
}

/// What a child's spawn and its clock read of the machine
/// (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT), injected through
/// [`ExecEnv::host`]: [`LIVE_HOST`] reads the OS, a test's stand-in says what it is told.
pub trait Host: Sync + std::fmt::Debug {
    /// The monotonic clock for work deadlines and start-watch cadence. A fixture may advance
    /// it after its child proves execution; the live host always reads the system clock.
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
    /// What each member of process group `pgid` has used, read now
    /// ([`aterm_sysprobe::group_usage`]); `None` where unread, and the clock then counts every
    /// span with no marked wait as work.
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>>;
    /// [`Host::group_usage`] for the clock's reads of its tree, telling a read that missed from
    /// the end of reading. By default every unread answer is [`Reading::Over`]: a host with no
    /// reading has none later either.
    fn group_reading(&self, pgid: u32) -> Reading {
        self.group_usage(pgid).map_or(Reading::Over, Reading::Read)
    }
    /// A complete reading of `root` and every process whose parent chain reaches it, including
    /// descendants that formed another process group. `None` means the tree cannot be proved
    /// complete, so a no-progress verdict must fail open.
    fn tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        self.group_usage(root).map(|members| {
            members
                .into_iter()
                .map(|member| TreeMember {
                    pid: member.pid,
                    start: member.start,
                    cpu_ns: member.cpu_ns,
                })
                .collect()
        })
    }
    /// A complete descendant-tree reading from a process-table sweep begun by this call. The
    /// live host bypasses its shared cadence cache; an injected host with no cache may use its
    /// current reading.
    fn fresh_tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        self.tree_usage(root)
    }
    /// How often a child's process-group counters are read. The live host uses
    /// [`READ_USAGE_EVERY`]; a test may make its injected progress probe immediate.
    fn usage_every(&self) -> Duration {
        READ_USAGE_EVERY
    }
    /// Take an identity-checked all-thread sample of `pid` into owner-private `output_dir`.
    /// The live implementation reuses [`aterm_uds::wait_sample`]; a test injects its sampler.
    fn sample(&self, pid: u32, output_dir: &Path, canceled: &dyn Fn() -> bool) -> StackSample {
        let sampled = (|| {
            let pid = i32::try_from(pid)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid is out of range"))?;
            let identity = aterm_uds::process::read_identity(pid).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "the test binary identity is unreadable",
                )
            })?;
            aterm_uds::wait_sample::sample(identity, output_dir, canceled)
        })();
        match sampled {
            Ok(sample) => StackSample {
                report: Some(sample.report),
                error: None,
            },
            Err(error) => StackSample::failed(error),
        }
    }
    /// How long since a person last touched this machine's keyboard or mouse
    /// ([`aterm_sysprobe::hid_idle`]); `None` where unread.
    fn idle(&self) -> Option<Duration>;
    /// The machine's logical cores ([`aterm_capacity::cores`]): how many cores a child's tree can
    /// run on at once.
    fn cores(&self) -> u32;
    /// The wall clock members' starts and the syspolicyd's are read on, µs since the epoch
    /// ([`aterm_sysprobe::jobs::now_us`], the default).
    fn now_us(&self) -> u64 {
        aterm_sysprobe::jobs::now_us()
    }
    /// Each of `pids`, members of process group `pgid`, whose one thread never ran
    /// ([`aterm_sysprobe::jobs::StartFacts::lone_zero`]) as its start reads at `now_us`
    /// ([`Host::now_us`]), with its executable and its parents' chain in the group
    /// ([`Unstarted`]); none where unread, the default.
    fn unstarted(&self, _pgid: u32, _pids: &[u32], _now_us: u64) -> Vec<Unstarted> {
        Vec::new()
    }
    /// The syspolicyd running now; [`DaemonNow::Unread`], the default, where unread.
    fn daemon(&self) -> DaemonNow {
        DaemonNow::Unread
    }
    /// Whether `d`'s pool reads full, and how many of this user's held programs forked at or
    /// after `since_us`, its start ([`Backlog`]); both unread, the default.
    fn backlog(&self, _d: aterm_uds::program_check::Daemon, _since_us: u64) -> Backlog {
        Backlog::default()
    }
}

/// One read of a child's tree by its clock ([`Host::group_reading`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// What each member used, read now.
    Read(Vec<aterm_sysprobe::MemberUsage>),
    /// This read failed and a later one may work (a process table the kernel held past its
    /// bound): the clock reads again at its next turn, and until a read works what the tree did
    /// since the last one that did is unknown.
    Missed,
    /// No reading now or later: the host has none, or what it reads has ended.
    Over,
}

/// One member of a complete descendant-tree CPU reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeMember {
    pub pid: u32,
    pub start: u64,
    /// Own CPU plus CPU of descendants this process has already reaped. Across a tree this
    /// preserves work as a child moves from a live row into its parent's reaped counter. Windows
    /// exposes own CPU only; membership changes still disarm comparison there.
    pub cpu_ns: u64,
}

/// The OS's answers ([`Host`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveHost;

impl Host for LiveHost {
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        aterm_sysprobe::group_usage(pgid)
    }

    fn tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        live_tree_usage(root, false)
    }

    fn fresh_tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        live_tree_usage(root, true)
    }

    fn idle(&self) -> Option<Duration> {
        aterm_sysprobe::hid_idle()
    }

    fn cores(&self) -> u32 {
        aterm_capacity::cores()
    }

    #[cfg(target_os = "macos")]
    fn unstarted(&self, pgid: u32, pids: &[u32], now_us: u64) -> Vec<Unstarted> {
        start::read_unstarted(pgid, pids, now_us)
    }

    #[cfg(target_os = "macos")]
    fn daemon(&self) -> DaemonNow {
        static KEPT: std::sync::Mutex<Option<(Instant, (), DaemonNow)>> =
            std::sync::Mutex::new(None);
        start::kept(&KEPT, (), || match aterm_uds::program_check::running() {
            Ok(d) => DaemonNow::Running(d),
            Err(aterm_uds::program_check::Look::NoneRunning) => DaemonNow::NoneRunning,
            Err(_) => DaemonNow::Unread,
        })
    }

    #[cfg(target_os = "macos")]
    fn backlog(&self, d: aterm_uds::program_check::Daemon, since_us: u64) -> Backlog {
        use aterm_uds::program_check::{Look, PS_HANG, look};
        type Kept = Option<(Instant, (aterm_uds::program_check::Daemon, u64), Backlog)>;
        static KEPT: std::sync::Mutex<Kept> = std::sync::Mutex::new(None);
        start::kept(&KEPT, (d, since_us), || Backlog {
            pool_full: match look(PS_HANG) {
                Look::Read(reading) if reading.daemon == d => Some(reading.full()),
                _ => None,
            },
            held_since: aterm_uds::process::held_since(since_us),
        })
    }
}

/// More than this many descendants is an incomplete liveness observation, never evidence of
/// zero CPU. Ordinary test binaries stay many orders below it.
const TEST_TREE_CAP: usize = 4_096;

/// Select `root` and its current descendants from one complete process-table sweep. Parentage,
/// identity and counters must all be readable; any ambiguity fails open.
fn tree_usage_from_sweep(sweep: &aterm_sysprobe::Sweep, root: u32) -> Option<Vec<TreeMember>> {
    if sweep.truncated {
        return None;
    }
    let mut by_pid = BTreeMap::new();
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for process in &sweep.procs {
        if by_pid.insert(process.pid, process).is_some() {
            return None;
        }
        children.entry(process.ppid).or_default().push(process.pid);
    }
    if !by_pid.contains_key(&root) {
        return None;
    }
    let mut open = VecDeque::from([root]);
    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    while let Some(pid) = open.pop_front() {
        if !seen.insert(pid) || members.len() >= TEST_TREE_CAP {
            return None;
        }
        let process = by_pid.get(&pid)?;
        // A current-process sweep loses a descendant that starts and exits between reads. The
        // root's cumulative reaped-child counter is what makes the sweep complete across that
        // gap. Windows' probe cannot supply that counter yet, so it must fail open rather than
        // turn an unread generation into zero and convict a parent waiting on a busy child.
        let reaped_cpu = process.child_cpu_ns?;
        members.push(TreeMember {
            pid,
            start: process.start?,
            cpu_ns: process.cpu_ns?.saturating_add(reaped_cpu),
        });
        if let Some(next) = children.get(&pid) {
            open.extend(next.iter().copied());
        }
    }
    members.sort_unstable();
    Some(members)
}

/// The full process-table sweep behind `sys jobs`, shared by every concurrent test binary. One
/// sweep per usage cadence avoids turning hundreds of binaries into hundreds of table walks.
/// A post-sample confirmation forces a new sweep so another binary's cached pre-resume view can
/// never convict this one.
fn live_tree_usage(root: u32, fresh: bool) -> Option<Vec<TreeMember>> {
    #[derive(Debug, Default)]
    struct Kept {
        at: Option<Instant>,
        sweep: Option<aterm_sysprobe::Sweep>,
    }
    static KEPT: std::sync::OnceLock<std::sync::Mutex<Kept>> = std::sync::OnceLock::new();
    let now = Instant::now();
    let mut kept = KEPT
        .get_or_init(|| std::sync::Mutex::new(Kept::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if fresh
        || kept
            .at
            .is_none_or(|at| now.saturating_duration_since(at) >= READ_USAGE_EVERY)
    {
        kept.sweep = aterm_sysprobe::processes();
        kept.at = Some(now);
    }
    kept.sweep
        .as_ref()
        .and_then(|sweep| tree_usage_from_sweep(sweep, root))
}

/// The host a run's children are spawned and timed on.
pub static LIVE_HOST: LiveHost = LiveHost;

/// How long nobody may have touched the keyboard or mouse before a [`Cmd::demoted`] child is
/// spawned at the run's own tier: no one is at the desk to type, so utility QoS would protect no
/// one's echo and only starve the check behind other sessions' work (measured on 2026-10-27 under
/// `docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT).
pub const AWAY_AFTER: Duration = Duration::from_secs(10 * 60);

/// Whether a person may be at the desk: input within [`AWAY_AFTER`], or an input clock that
/// cannot be read.
fn at_desk(host: &dyn Host) -> bool {
    host.idle().is_none_or(|idle| idle < AWAY_AFTER)
}

/// Process-group lifecycle hooks invoked while group custody ([`group::Live`]) holds the leader.
pub trait StageGroupEnd: Sync + std::fmt::Debug {
    /// Record a newly created stage process group.
    fn group_made(&self, pgid: u32, label: &str);
    /// Sweep a stage process group whose leader has exited but is not yet reaped.
    fn group_ended(&self, pgid: u32, label: &str);
    /// Record the group [`Self::group_ended`] swept, once its leader is reaped: the leader no
    /// longer holds the group's id then, so the kernel's answer about the whole group finds it
    /// empty only when no process is left in it either, and can prove it empty where the
    /// sweep's listing could not ([`group::empty`]).
    fn group_reaped(&self, pgid: u32, label: &str);
}

/// What a child did.
#[derive(Clone, Debug, Default)]
pub struct Run {
    /// Declared libtest mode and a bounded, incomplete sample of observed failures.
    pub case_observations: Option<test_progress::Observations>,
    /// True only on exit status 0.
    pub ok: bool,
    pub output: String,
    /// The exit code.
    pub code: Option<i32>,
    /// Set when the child could not be spawned at all (tool vanished mid-run).
    pub spawn_error: Option<String>,
    /// What its tree used, as its clock read it ([`TreeUsage`]); `None` where no read of its tree
    /// found a member (a host with no reading: every platform but macOS; a child that never
    /// started), never zeros.
    pub usage: Option<TreeUsage>,
    /// Set when verify ended this test binary at its first decisive red case ([`CaseWatch`]):
    /// its run is a FAIL by construction, read from the blocks of the cases that finished.
    pub stopped_at: Option<crate::libtest_json::Stopped>,
    /// Set when verify ended a test binary whose complete output lines and whole descendant-tree
    /// CPU both made no progress for its evidence window. This is a red, never an environment
    /// refusal or a retry.
    pub hung: Option<Hung>,
}

/// A test binary verify proved hung before it ended its owned process group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hung {
    /// Cargo's re-run spec.
    pub spec: String,
    /// The executable that stopped making progress.
    pub binary: String,
    /// Libtest JSON cases with `started` and no terminal event when the sample began.
    pub running: Vec<String>,
    /// The no-output/no-CPU evidence window.
    pub window: Duration,
    /// The complete or partial raw all-thread sample, and any collection error.
    pub sample: StackSample,
    /// Whether the binary supplied libtest JSON, and therefore whether `running` is complete
    /// evidence rather than unavailable.
    pub json: bool,
}

impl Hung {
    /// Keep the synchronous red and its event bounded even when a malicious harness starts many
    /// cases or gives one an enormous name. The full sorted list remains in [`Self::running`].
    fn running_detail(&self) -> String {
        const SHOWN: usize = 8;
        const NAME_BYTES: usize = 200;
        if !self.json {
            return "unavailable (the binary did not run in libtest JSON format)".to_owned();
        }
        if self.running.is_empty() {
            return "none reported by libtest JSON".to_owned();
        }
        let mut shown = self
            .running
            .iter()
            .take(SHOWN)
            .map(|name| crate::report::clip_json(name, NAME_BYTES))
            .collect::<Vec<_>>()
            .join(", ");
        if self.running.len() > SHOWN {
            shown.push_str(&format!(" (+{} more)", self.running.len() - SHOWN));
        }
        shown
    }

    /// The named red kept in the binary's log and event.
    #[must_use]
    pub fn detail(&self) -> String {
        let tests = self.running_detail();
        let sample = match (&self.sample.report, &self.sample.error) {
            (Some(path), None) => format!("stack sample: {}", path.display()),
            (Some(path), Some(why)) => {
                format!(
                    "partial stack sample: {} (collection failed: {why})",
                    path.display()
                )
            }
            (None, Some(why)) => format!("stack sample unavailable: {why}"),
            (None, None) => "stack sample unavailable: no report or error".to_owned(),
        };
        format!(
            "HUNG — test binary {} ({}) printed no complete output line and its descendant tree \
             consumed no CPU for {}; still running: {tests}; {sample}",
            crate::report::clip_json(&self.binary, crate::report::EVENT_ROW_BYTES),
            crate::report::clip_json(&self.spec, crate::report::EVENT_ROW_BYTES),
            aterm_capacity::span(self.window),
        )
    }
}

/// WHAT A CHILD'S TREE USED (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1), as its
/// clock read its process group ([`Host::group_usage`]): on exactly the reads that book its
/// stage's meter ([`crate::memory::Meter::book`]: the clock's reads and the sweep's read of its
/// ended leader), booked into a meter of its own as well, so the CPU of a stage's children sums
/// to the stage's observed CPU ([`crate::memory::Meter::observed_cpu`]), apart from what the
/// output children of a walk the stage runs ([`bounded`]) book into its meter, which no run
/// carries, and from an incarnation two children's reads both find, which each of them counts.
/// A lower bound, as the stage's is: a member that ends between two reads takes what it used
/// since the read before with it, and a descendant that leaves the group (`setsid`, `setpgid`)
/// is never read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeUsage {
    /// CPU time: each process incarnation read (by pid and start) at the most its counter was
    /// read at, summed.
    pub cpu: Duration,
    /// Time runnable, on a core or waiting for one, read and summed as `cpu` is.
    pub runnable: Duration,
    /// The part of its work its tree spent waiting for a core, which its ceiling left out
    /// ([`core_wait`]).
    pub starved: Duration,
    /// The time a wait of its tree for admission or a lock was marked
    /// ([`aterm_toolchain::waits`]), which its ceiling left out too.
    pub waited: Duration,
    /// From its spawn to its end.
    pub wall: Duration,
    /// The most its tree held at one read, footprint bytes, and never less than the largest
    /// lifetime peak a read found ([`crate::memory::Meter::peak`]).
    pub peak_bytes: u64,
    /// The process incarnations read.
    pub processes: usize,
}

/// The OS's own sentence for ENOSPC.
pub const ENOSPC_SENTENCE: &str = "No space left on device";

impl Run {
    #[must_use]
    pub fn trimmed_output(&self) -> &str {
        self.output.trim_end_matches('\n')
    }

    /// Why this child is not a finding about the tree, if it failed for an environment reason.
    #[must_use]
    pub fn environment_failure(&self) -> Option<String> {
        if self.ok {
            return None;
        }
        if let Some(e) = &self.spawn_error {
            return Some(e.clone());
        }
        if self.hung.is_some() {
            // A completed no-progress proof is a tree red even when the sampler's diagnostic
            // happens to name an environmental phrase such as a full disk.
            return None;
        }
        if crate::libtest::has_ceiling_kill(&self.output) {
            return None;
        }
        if out_of_disk(&self.output) {
            return Some(format!("the child ran out of disk ({ENOSPC_SENTENCE})"));
        }
        crate::libtest::environment_refusals(&self.output)
            .map(|names| crate::libtest::refusal_reason("every failing test", &names, &self.output))
    }

    /// How this child ended, for a NOT RUN row whose child named no cause of its own: its exit
    /// code, or that it had none (a signal ended it, the run's interrupt among them).
    #[must_use]
    pub fn unnamed_cause(&self) -> String {
        let ended = self.code.map_or_else(
            || "ended with no exit code (a signal ended it, the run's interrupt among them)".into(),
            |code| format!("exited {code}"),
        );
        {
            #[cfg(unix)]
            let provenance = aterm_uds::signal_origin::records().join("\n");
            #[cfg(not(unix))]
            let provenance = String::new();
            format!(
                "it {ended} and named no cause (what it printed is in the check's log)\n{provenance}"
            )
        }
    }
}

/// Whether a failed child's log says the disk ran out: when it accounts for its failures as failed
/// tests, only if each one's own block names [`ENOSPC_SENTENCE`] (a test that passes may print a full
/// disk it simulated); otherwise anywhere, as a build or a tool that died says it. Passing test
/// binaries and ceiling kills are excluded ([`crate::libtest::out_of_disk`]).
fn out_of_disk(output: &str) -> bool {
    crate::libtest::out_of_disk(output)
}

/// The default ceiling on one stage child's work ([`ExecEnv::child_ceiling`]).
pub const DEFAULT_CHILD_CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// Why an in-process walk ([`bounded`]) gave no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// Still running at the ceiling.
    TimedOut(Duration),
    /// It panicked (the panic printed on stderr).
    Panicked,
    /// The run's end left it unfinished: these words, the halt's once its first proven red ended
    /// the run ([`halted`]), else an interrupt's. The walk's thread is left to the process's exit.
    Stopped(&'static str),
}

impl Unfinished {
    /// The reason, for a NOT RUN row.
    #[must_use]
    pub fn sentence(self) -> String {
        match self {
            Self::TimedOut(d) => format!("did not finish within the {}s ceiling", d.as_secs()),
            Self::Panicked => "panicked (above, on stderr)".to_string(),
            Self::Stopped(why) => format!("was left unfinished: {why}"),
        }
    }
}

/// How often a wait for an in-process walk ([`bounded`]) looks whether the run is ending.
const WALK_LOOK: Duration = Duration::from_millis(50);

/// Run an in-process walk on a thread of its own under the child ceiling, on the wall clock: no
/// walk waits for admission or a lock. Each child the walk starts books into the meter of the
/// stage that called this ([`crate::memory::current`]), on the walk's thread or through the
/// output executor it installs. Its quiet reservation, when the calling stage has one, stays
/// held until the worker actually ends even if the ceiling returns first. The actual stage
/// memory reservation is shared across the same handoff; a cover marker alone cannot hold it. The
/// run's end does not wait for it: once a halt or an interrupt is ending the run ([`ending`]), the
/// wait gives up within [`WALK_LOOK`], closing the walk's command scope so it starts no child.
/// # Errors
/// [`Unfinished`]: the walk timed out, panicked, or was left to the run's end.
pub fn bounded<T: Send + 'static>(
    ceiling: Option<Duration>,
    walk: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Unfinished> {
    bounded_wait(walk, |rx| {
        let started = Instant::now();
        loop {
            let left = ceiling.map(|limit| limit.saturating_sub(started.elapsed()));
            match rx.recv_timeout(left.map_or(WALK_LOOK, |left| left.min(WALK_LOOK))) {
                Ok(answer) => return Ok(answer),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Unfinished::Panicked);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if ending() {
                return Err(Unfinished::Stopped(
                    halted().unwrap_or("the run was interrupted while it ran"),
                ));
            }
            if let (Some(limit), Some(left)) = (ceiling, left)
                && left <= WALK_LOOK
            {
                return Err(Unfinished::TimedOut(limit));
            }
        }
    })
}

fn bounded_wait<T: Send + 'static>(
    walk: impl FnOnce() -> T + Send + 'static,
    wait: impl FnOnce(std::sync::mpsc::Receiver<T>) -> Result<T, Unfinished>,
) -> Result<T, Unfinished> {
    struct Close(std::sync::Arc<output::Scope>);
    impl Drop for Close {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let meter = crate::memory::current();
    let cover = crate::memory::cover();
    let quiet = crate::current_quiet_claim();
    let resources = crate::current_resource_claim();
    let scope = Close(std::sync::Arc::new(output::Scope::metered(meter.clone())));
    let executor = std::sync::Arc::clone(&scope.0);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _metering = crate::memory::metering(meter);
        let _covering = crate::memory::covering(cover);
        let _quiet = crate::holding_quiet_claim(quiet);
        let _resources = crate::holding_resource_claim(resources);
        aterm_uds::output::with_executor(
            std::sync::Arc::new(move |command| executor.output(command)),
            || {
                tx.send(walk()).ok();
            },
        );
    });
    let answer = wait(rx);
    drop(scope);
    if worker.is_finished() {
        let _ = worker.join();
    }
    answer
}

#[cfg(test)]
#[path = "exec_memory_custody_tests.rs"]
mod memory_custody_tests;
#[path = "exec_output.rs"]
mod output;
#[path = "exec_start.rs"]
mod start;
pub use start::{Backlog, DaemonNow, Unstarted};
#[path = "exec_compiler_progress.rs"]
mod compiler_progress;
#[path = "exec_test_progress.rs"]
mod test_progress;
/// A completed libtest case's measured time, retained before its log is rendered.
pub use test_progress::Timing as CaseTiming;

const MAX_CEILING_SECS: f64 = 3_153_600_000.0;

/// Parse a `--timeout` value ([`DEFAULT_CHILD_CEILING`]).
#[must_use]
pub fn parse_ceiling(text: &str) -> Option<Option<Duration>> {
    let text = text.trim();
    if text == "0"
        || text.eq_ignore_ascii_case("off")
        || text.eq_ignore_ascii_case("none")
        || text.eq_ignore_ascii_case("never")
    {
        return Some(None);
    }
    match text.parse::<f64>() {
        Ok(v) if v.is_finite() && (0.0..=MAX_CEILING_SECS).contains(&v) => {
            Some((v > 0.0).then(|| Duration::from_secs_f64(v)))
        }
        _ => None,
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// macOS's QoS launcher.
pub const TASKPOLICY: &str = aterm_uds::os_path!("taskpolicy");

/// Open one spawned test binary's live capture stream. Kept injectable so the fail-closed path
/// has a real forever-blocking negative control rather than relying on a filesystem race.
type CaseOpener = dyn Fn(&Path, bool) -> io::Result<test_progress::LiveCases> + Sync;

/// Spawn, wait, and collect.
#[must_use]
pub fn run(cmd: &Cmd, env: ExecEnv<'_>) -> Run {
    run_captured(
        cmd,
        env,
        None,
        None,
        None,
        None,
        &test_progress::LiveCases::open,
    )
}

/// [`run`], with the local test dispatcher closed synchronously once a hang is reconfirmed and
/// before its process group is ended. This orders the first red before every later admission.
#[must_use]
pub(crate) fn run_test(cmd: &Cmd, env: ExecEnv<'_>, hung_red: &(dyn Fn() + Sync)) -> Run {
    run_captured(
        cmd,
        env,
        None,
        Some(hung_red),
        None,
        None,
        &test_progress::LiveCases::open,
    )
}

/// [`run_test`] with an injected live-capture opener, for the negative control that makes the
/// real child block forever while monitoring initialization fails.
#[cfg(test)]
fn run_test_opening(
    cmd: &Cmd,
    env: ExecEnv<'_>,
    hung_red: &(dyn Fn() + Sync),
    open_cases: &CaseOpener,
) -> Run {
    run_captured(cmd, env, None, Some(hung_red), None, None, open_cases)
}

/// [`run_test`], with an exclusive cross-process admission lock held by the caller across the
/// last first-red check and this test binary's actual spawn. The descriptor is explicitly
/// unlocked immediately after the spawn attempt, and on every earlier return. This closes the
/// check/start race for rustdoc runtools in separate processes without serializing binaries that
/// are already running.
#[must_use]
pub(crate) fn run_test_admitted(
    cmd: &Cmd,
    env: ExecEnv<'_>,
    hung_red: &(dyn Fn() + Sync),
    admission: &File,
    external_stop: &(dyn Fn() -> Option<String> + Sync),
) -> Run {
    run_captured(
        cmd,
        env,
        None,
        Some(hung_red),
        Some(admission),
        Some(external_stop),
        &test_progress::LiveCases::open,
    )
}

/// One compiler invocation's machine stdout, separate from its diagnostic stderr.
pub(crate) struct MachineRun {
    pub run: Run,
    pub stdout: Result<String, String>,
}

/// Capture compiler stdout without allowing diagnostic text to become machine evidence.
pub(crate) fn run_machine(cmd: &Cmd, env: ExecEnv<'_>, progress: Option<&str>) -> MachineRun {
    let mut stdout = Err("compiler stdout was not captured".to_owned());
    let run = run_captured(
        cmd,
        env,
        Some(MachineCapture {
            stdout: &mut stdout,
            progress,
        }),
        None,
        None,
        None,
        &test_progress::LiveCases::open,
    );
    MachineRun { run, stdout }
}

struct MachineCapture<'a> {
    stdout: &'a mut Result<String, String>,
    progress: Option<&'a str>,
}

fn run_captured(
    cmd: &Cmd,
    env: ExecEnv<'_>,
    machine: Option<MachineCapture<'_>>,
    hung_red: Option<&(dyn Fn() + Sync)>,
    admission: Option<&File>,
    external_stop: Option<&(dyn Fn() -> Option<String> + Sync)>,
    open_cases: &CaseOpener,
) -> Run {
    let mut run = run_captured_inner(
        cmd,
        env,
        machine,
        hung_red,
        admission,
        external_stop,
        open_cases,
    );
    if let Some(watch) = &cmd.test_cases {
        run.case_observations
            .get_or_insert_with(|| test_progress::Observations {
                json: watch.json,
                ..Default::default()
            });
    }
    run
}

/// Take a bounded sample from the original capture, never its rendered human summary.
#[cfg(test)]
pub(crate) fn case_observations(path: &Path, json: bool) -> test_progress::Observations {
    let mut observations = test_progress::Observations {
        json,
        read_end: test_progress::ReadEnd::Unread,
        failures: Vec::new(),
        ..Default::default()
    };
    if let Ok(mut cases) = test_progress::LiveCases::open(path, json) {
        let _ = cases.poll();
        observations = cases.observations().clone();
    }
    observations
}

#[derive(Clone, Debug)]
enum EnvironmentEdit {
    Set(OsString, OsString),
    Remove(OsString),
}

/// The deterministic environment edits applied after a [`Cmd`]'s own environment. They are
/// materialized once for content evidence and applied by the spawn path itself, so a release
/// never keys a command which differs from the one its child receives.
///
/// Compiler-width configuration is asked of its real configurator on a probe command. Its
/// environment edits are lossless; if it ever starts changing argv, program or cwd as well, the
/// caller cannot represent that in a content proof and must fail the proof closed.
fn runtime_environment_edits(cmd: &Cmd, env: ExecEnv<'_>) -> Result<Vec<EnvironmentEdit>, String> {
    let mut edits = Vec::new();
    if let Some(marker) = crate::memory::cover() {
        edits.push(EnvironmentEdit::Set(
            aterm_capacity::COVER_ENV.into(),
            marker.as_ref().into(),
        ));
    }
    if let Some(jobs) = crate::compile_width::current() {
        let jobs = cmd
            .envs
            .iter()
            .rev()
            .find(|(name, _)| name == "CARGO_BUILD_JOBS")
            .and_then(|(_, value)| crate::parse_build_jobs(Some(value.as_os_str())))
            .map_or(jobs, |asked| jobs.min(asked));
        edits.push(EnvironmentEdit::Set(
            "CARGO_BUILD_JOBS".into(),
            jobs.to_string().into(),
        ));

        let sentinel = OsStr::new("aterm-verify-compile-width-environment-probe");
        let mut probe = aterm_uds::spawnfd::command(sentinel);
        crate::compile_width::configure(&mut probe);
        if probe.get_program() != sentinel
            || probe.get_args().next().is_some()
            || probe.get_current_dir().is_some()
        {
            return Err(
                "compile-width configuration changes more than the child environment".into(),
            );
        }
        edits.extend(probe.get_envs().map(|(name, value)| match value {
            Some(value) => EnvironmentEdit::Set(name.to_owned(), value.to_owned()),
            None => EnvironmentEdit::Remove(name.to_owned()),
        }));
    }
    if let Some(grant) = crate::current_resource_claim()
        .map(|claim| claim.cores())
        .filter(|cores| *cores > 0)
    {
        let ceiling = |wanted: &str| {
            cmd.envs
                .iter()
                .rev()
                .find(|(name, _)| name == wanted)
                .or_else(|| {
                    if cmd.exact_env {
                        None
                    } else {
                        env.add_env.iter().rev().find(|(name, _)| name == wanted)
                    }
                })
                .and_then(|(_, value)| crate::parse_build_jobs(Some(value.as_os_str())))
                .map_or(grant, |asked| asked.min(grant))
        };
        edits.push(EnvironmentEdit::Set(
            "CARGO_BUILD_JOBS".into(),
            ceiling("CARGO_BUILD_JOBS").to_string().into(),
        ));
        if cmd.planned_test_threads.is_none() {
            edits.push(EnvironmentEdit::Set(
                "RUST_TEST_THREADS".into(),
                ceiling("RUST_TEST_THREADS").to_string().into(),
            ));
        }
        edits.push(EnvironmentEdit::Set("BUILD_SLOT_HELD".into(), "1".into()));
    }
    if let Some(threads) = cmd.planned_test_threads {
        edits.push(EnvironmentEdit::Set(
            "RUST_TEST_THREADS".into(),
            threads.to_string().into(),
        ));
    }
    Ok(edits)
}

/// The cores each of a binary's `threads` test threads may use of the `cores` it is charged:
/// an equal share, rounded down, at least one. Rounding down keeps the threads' pools together
/// within the charge; the floor of one lets a binary charged fewer cores than threads (its
/// threads mostly wait) still run a pool of one.
#[must_use]
pub(crate) fn test_thread_cores(threads: u32, cores: u32) -> u32 {
    (cores / threads.max(1)).max(1)
}

fn apply_environment_edits(command: &mut Command, edits: &[EnvironmentEdit]) {
    for edit in edits {
        match edit {
            EnvironmentEdit::Set(name, value) => {
                command.env(name, value);
            }
            EnvironmentEdit::Remove(name) => {
                command.env_remove(name);
            }
        }
    }
}

fn apply_cmd_environment_edits(command: &mut Cmd, edits: &[EnvironmentEdit]) {
    for edit in edits {
        match edit {
            EnvironmentEdit::Set(name, value) => {
                // A recorded process environment may repeat a name. `Command` applies this
                // runtime edit after every recorded value, so the materialized command must do
                // the same: remove them all and append the value the child actually receives.
                command.envs.retain(|(held, _)| held != name);
                command.envs.push((name.clone(), value.clone()));
            }
            EnvironmentEdit::Remove(name) => {
                command.envs.retain(|(held, _)| held != name);
            }
        }
    }
}

/// Materialize the exact cwd and environment a command will have after this thread's held
/// resource claim, memory cover, compiler-width policy and wait accounting are applied.
///
/// This is for release content evidence and the command later dispatched. A command which
/// inherits state, is demoted through `taskpolicy`, or needs a fresh wait-marker directory cannot
/// be represented ahead of its spawn and is refused. The caller records it as unbounded and
/// still runs it normally.
pub(crate) fn finalize_exact_command(mut command: Cmd, env: ExecEnv<'_>) -> Result<Cmd, String> {
    if !command.exact_env {
        return Err("the child inherits an environment".into());
    }
    if command.demoted {
        return Err("the child may be wrapped by taskpolicy".into());
    }
    if !command.waits_count {
        return Err("the child needs a per-spawn wait-marker directory".into());
    }
    if command.runtime_environment_is_finalized() {
        return Ok(command);
    }
    if command.cwd.is_none() {
        command.cwd = Some(env.cwd.to_path_buf());
    }
    // In an exact command removals precede its explicit environment, so an explicit value wins.
    command.remove_envs.clear();
    let edits = runtime_environment_edits(&command, env)?;
    apply_cmd_environment_edits(&mut command, &edits);
    command
        .envs
        .retain(|(name, _)| name != aterm_toolchain::waits::VAR);
    command.finalized_receipt = Some(Box::new(FinalizedCommandReceipt::capture(&command)));
    Ok(command)
}

/// Turn an inherited-environment command into the exact command this verifier would spawn, then
/// apply [`finalize_exact_command`]'s run-time edits. The returned command is both the content
/// evidence and the command which must be dispatched; rebuilding the inherited environment later
/// would create a time-of-check/time-of-use gap.
///
/// A demoted command or one with per-spawn wait markers has process behavior which cannot be
/// represented by a stable argv/environment receipt and is refused.
pub(crate) fn materialize_exact_command(mut command: Cmd, env: ExecEnv<'_>) -> Result<Cmd, String> {
    if command.exact_env {
        return finalize_exact_command(command, env);
    }
    if command.demoted {
        return Err("the child may be wrapped by taskpolicy".into());
    }
    if !command.waits_count {
        return Err("the child needs a per-spawn wait-marker directory".into());
    }

    let mut environment: BTreeMap<OsString, (OsString, OsString)> = std::env::vars_os()
        .map(|(name, value)| (environment_key(&name), (name, value)))
        .collect();
    for removed in env.remove_env {
        if let Some(prefix) = removed.strip_suffix('*') {
            let prefix = environment_key(OsStr::new(prefix));
            environment.retain(|name, _| {
                !name
                    .as_encoded_bytes()
                    .starts_with(prefix.as_encoded_bytes())
            });
        } else {
            environment.remove(&environment_key(OsStr::new(removed)));
        }
    }
    for (name, value) in env.add_env {
        environment.insert(environment_key(name), (name.clone(), value.clone()));
    }
    for name in &command.remove_envs {
        environment.remove(&environment_key(name));
    }
    for (name, value) in &command.envs {
        environment.insert(environment_key(name), (name.clone(), value.clone()));
    }
    command.envs = environment.into_values().collect();
    command.remove_envs.clear();
    command.exact_env = true;
    finalize_exact_command(command, env)
}

fn environment_key(name: &OsStr) -> OsString {
    #[cfg(windows)]
    {
        name.to_string_lossy().to_uppercase().into()
    }
    #[cfg(not(windows))]
    {
        name.to_owned()
    }
}

fn run_captured_inner(
    cmd: &Cmd,
    env: ExecEnv<'_>,
    machine: Option<MachineCapture<'_>>,
    hung_red: Option<&(dyn Fn() + Sync)>,
    admission: Option<&File>,
    external_stop: Option<&(dyn Fn() -> Option<String> + Sync)>,
    open_cases: &CaseOpener,
) -> Run {
    let mut admission = SpawnAdmission(admission);
    let mut c = spawn_command(cmd, env, Path::new(TASKPOLICY));
    c.current_dir(cmd.cwd.as_deref().unwrap_or(env.cwd));
    if cmd.exact_env {
        c.env_clear();
    } else {
        c.env("PATH", env.path);
        for k in env.remove_env {
            match k.strip_suffix('*') {
                Some(prefix) => {
                    for (name, _) in std::env::vars_os() {
                        if name.as_encoded_bytes().starts_with(prefix.as_bytes()) {
                            c.env_remove(name);
                        }
                    }
                }
                None => {
                    c.env_remove(k);
                }
            }
        }
        for (k, v) in env.add_env {
            c.env(k, v);
        }
    }
    for k in &cmd.remove_envs {
        c.env_remove(k);
    }
    for (k, v) in &cmd.envs {
        c.env(k, v);
    }
    // A replayed child's recorded cover and scheduler limits belong to the run that recorded it,
    // so the held claim's actual values replace them. A release content command has already
    // materialized those edits through [`finalize_exact_command`]; execute that frozen receipt
    // instead of recomputing policy after its evidence was taken.
    let runtime_edits = if cmd.runtime_environment_is_finalized() {
        Ok(Vec::new())
    } else {
        runtime_environment_edits(cmd, env)
    };
    match runtime_edits {
        Ok(edits) => apply_environment_edits(&mut c, &edits),
        Err(_) => {
            // Preserve execution if compiler policy ever grows a non-environment mutation. Such a
            // command's evidence fails closed, while its actual spawn retains the configurator's
            // full behavior.
            if let Some(marker) = crate::memory::cover() {
                c.env(aterm_capacity::COVER_ENV, &*marker);
            }
            if let Some(jobs) = crate::compile_width::current() {
                let jobs = cmd
                    .envs
                    .iter()
                    .rev()
                    .find(|(name, _)| name == "CARGO_BUILD_JOBS")
                    .and_then(|(_, value)| crate::parse_build_jobs(Some(value.as_os_str())))
                    .map_or(jobs, |asked| jobs.min(asked));
                c.env("CARGO_BUILD_JOBS", jobs.to_string());
                crate::compile_width::configure(&mut c);
            }
            if let Some(grant) = crate::current_resource_claim()
                .map(|claim| claim.cores())
                .filter(|cores| *cores > 0)
            {
                let ceiling = |wanted: &str| {
                    cmd.envs
                        .iter()
                        .rev()
                        .find(|(name, _)| name == wanted)
                        .or_else(|| {
                            if cmd.exact_env {
                                None
                            } else {
                                env.add_env.iter().rev().find(|(name, _)| name == wanted)
                            }
                        })
                        .and_then(|(_, value)| crate::parse_build_jobs(Some(value.as_os_str())))
                        .map_or(grant, |asked| asked.min(grant))
                };
                c.env("CARGO_BUILD_JOBS", ceiling("CARGO_BUILD_JOBS").to_string());
                if cmd.planned_test_threads.is_none() {
                    c.env(
                        "RUST_TEST_THREADS",
                        ceiling("RUST_TEST_THREADS").to_string(),
                    );
                }
                c.env("BUILD_SLOT_HELD", "1");
            }
            if let Some(threads) = cmd.planned_test_threads {
                c.env("RUST_TEST_THREADS", threads.to_string());
            }
        }
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let marks = if cmd.waits_count {
        None
    } else {
        Marks::lay(env.scratch, seq)
    };
    match &marks {
        Some(m) => c.env(aterm_toolchain::waits::VAR, &m.dir),
        None => c.env_remove(aterm_toolchain::waits::VAR),
    };
    let marks = marks.as_ref().map(|m| m.dir.as_path());

    let label = stage_label(&cmd.argv());
    match &cmd.capture {
        Capture::Silent => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
            finish(
                c,
                cmd,
                env,
                None,
                &label,
                marks,
                FinishControls {
                    compiler: None,
                    local_red: hung_red,
                    admission: &mut admission,
                    external_stop,
                    open_cases,
                },
            )
            .0
        }
        Capture::Emit => {
            // Quiet transcript writes without changing the short root for Unix sockets.
            let log = aterm_tempfile::quiet_in(env.scratch)
                .join(format!("stage.{}.{seq}.log", std::process::id()));
            let mut file = match create_stage_log(&log) {
                Ok(f) => f,
                Err(e) => return spawn_failure(cmd, &e.to_string()),
            };
            if let Some(text) = &cmd.preamble
                && let Err(e) = file.write_all(text.as_bytes())
            {
                std::fs::remove_file(&log).ok();
                return spawn_failure(cmd, &e.to_string());
            }
            let stdout_log = machine.as_ref().map(|_| log.with_extension("stdout"));
            let file2 = match &stdout_log {
                Some(path) => create_stage_log(path),
                None => file.try_clone(),
            };
            let file2 = match file2 {
                Ok(f) => f,
                Err(e) => {
                    std::fs::remove_file(&log).ok();
                    return spawn_failure(cmd, &e.to_string());
                }
            };
            let compiler = machine
                .as_ref()
                .and_then(|machine| machine.progress)
                .and_then(|stage| {
                    compiler_progress::Progress::open(stdout_log.as_deref()?, &file2, stage).ok()
                });
            if machine.is_some() {
                c.stdout(file2).stderr(file);
            } else {
                c.stdout(file).stderr(file2);
            }
            let (mut r, logged) = finish(
                c,
                cmd,
                env,
                Some(&log),
                &label,
                marks,
                FinishControls {
                    compiler,
                    local_red: hung_red,
                    admission: &mut admission,
                    external_stop,
                    open_cases,
                },
            );
            r.output
                .insert_str(0, logged.as_deref().unwrap_or_default());
            if let Some(machine) = machine {
                let path = stdout_log.expect("machine capture has a stdout file");
                *machine.stdout = match crate::read_captured(&path) {
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(text) => Ok(text),
                        Err(error) => {
                            r.output
                                .push_str("\n--- invalid UTF-8 compiler stdout ---\n");
                            r.output
                                .push_str(&String::from_utf8_lossy(error.as_bytes()));
                            Err("compiler stdout was not UTF-8".to_owned())
                        }
                    },
                    Err(error) => Err(format!("could not read compiler stdout: {error}")),
                };
                std::fs::remove_file(path).ok();
            }
            std::fs::remove_file(&log).ok();
            r
        }
    }
}

/// A caller-held cross-process start admission. The normal path releases it immediately after
/// `spawn`; every earlier return releases it here. A failed unlock leaves the descriptor here so
/// dropping it still closes the lock before [`run_captured`] returns.
struct SpawnAdmission<'a>(Option<&'a File>);

impl SpawnAdmission<'_> {
    fn release(&mut self) -> io::Result<()> {
        let Some(file) = self.0 else {
            return Ok(());
        };
        file.unlock()?;
        self.0 = None;
        Ok(())
    }
}

impl Drop for SpawnAdmission<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            let _ = file.unlock();
        }
    }
}

fn create_stage_log(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

/// Make (or validate) the owner-private directory a hung binary's raw sample is retained in.
/// A pre-existing symlink, foreign owner or group/world-accessible directory is refused by the
/// shared private-file boundary before `/usr/bin/sample` is launched.
fn prepare_sample_dir(path: &Path) -> io::Result<()> {
    let made = {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(path)
    };
    if let Err(error) = made
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    aterm_uds::private_file::check_private_dir(path)
}

/// Sample `pid` before its hung group is ended. The sampler's own one-minute detector is
/// shortened by cancellation so a broken sampler cannot violate A3's 30-second red-to-verdict
/// target.
fn sample_hung(env: ExecEnv<'_>, pid: u32) -> StackSample {
    let Some(dir) = env.hang_samples else {
        return StackSample {
            report: None,
            error: Some("the run log has no sample directory".to_owned()),
        };
    };
    if let Err(error) = prepare_sample_dir(dir) {
        return StackSample {
            report: None,
            error: Some(format!("cannot prepare {}: {error}", dir.display())),
        };
    }
    let begun = Instant::now();
    env.host.sample(pid, dir, &|| {
        ending() || begun.elapsed() >= STACK_SAMPLE_HANG
    })
}

/// The directory a child's tree marks its waits in ([`aterm_toolchain::waits::VAR`]), removed
/// with what a waiter killed mid-wait left in it once the child has been reaped.
struct Marks {
    dir: PathBuf,
}

impl Marks {
    /// A fresh one under `scratch`; `None` where it cannot be made, and the child's clock then
    /// counts every wait as work, as a run with no marks did.
    fn lay(scratch: &Path, seq: u64) -> Option<Marks> {
        let dir = scratch.join(format!("waits.{}.{seq}", std::process::id()));
        std::fs::create_dir(&dir).ok()?;
        Some(Marks { dir })
    }
}

impl Drop for Marks {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How often a child's clock looks for a marked wait in its tree: as often as its poll loop wakes
/// at most, well inside the [`aterm_toolchain::named_lock::TICK`] a lock's waiter lets pass before
/// it first asks whether to stop, so a wait the run's end stops has been seen before it ends.
const LOOK_EVERY: Duration = Duration::from_millis(25);

/// How often a child's clock reads what its tree used ([`Host::group_usage`]) while its work is
/// short of its ceiling; at the ceiling it reads once more before it judges.
pub(crate) const READ_USAGE_EVERY: Duration = Duration::from_millis(500);

/// How long a test binary may produce neither a complete output line nor any descendant-tree CPU
/// before verify proves it hung. Three minutes is three times the project's one-minute detector
/// for an event that must happen: long enough for a heavily loaded Mac to schedule and account
/// one instruction, while reducing the measured 26-minute deadlock to a bounded red. This is a
/// liveness window over evidence, never a test latency budget.
pub const TEST_BINARY_HANG: Duration = Duration::from_secs(3 * 60);

/// A stack sampler normally finishes in one second. Its own failure must not turn A3's
/// red-to-verdict target into another indefinite wait, so cancellation reaches it after 20 s.
const STACK_SAMPLE_HANG: Duration = Duration::from_secs(20);

/// How often a child's stage meter reads its process group in its first [`READ_USAGE_EVERY`]: a
/// child that ends before the first full read (a 400 ms link of 3 GiB) is still read near its peak,
/// and not booked as nothing or not at all.
const EARLY_METER_EVERY: Duration = Duration::from_millis(25);

/// Evidence that one test binary is still alive: either a complete output line or a generation
/// of CPU use by any member of its descendant tree. An unread/incomplete process probe can prove
/// nothing and disarms the window until a fresh complete baseline is available.
#[derive(Clone, Copy, Debug)]
struct TestProgress {
    window: Duration,
    last: Instant,
    lines: u64,
    cpu: u64,
    armed: bool,
}

impl TestProgress {
    fn new(window: Duration, now: Instant) -> Self {
        Self {
            window,
            last: now,
            lines: 0,
            cpu: 0,
            armed: false,
        }
    }

    /// Observe the two progress generations at `now`. `None` means the descendant-tree probe is
    /// unread or may be truncated, so elapsed wall time cannot convict the binary.
    fn hung(&mut self, now: Instant, lines: Option<u64>, cpu: Option<u64>) -> bool {
        let (Some(lines), Some(cpu)) = (lines, cpu) else {
            self.armed = false;
            self.last = now;
            if let Some(lines) = lines {
                self.lines = lines;
            }
            return false;
        };
        if !self.armed || self.lines != lines || self.cpu != cpu {
            self.armed = true;
            self.last = now;
            self.lines = lines;
            self.cpu = cpu;
            return false;
        }
        now.saturating_duration_since(self.last) >= self.window
    }
}

/// Whether a child begun at `begun`, its group last read for its meter at `last`, is due a read
/// at `now`: at once, then every [`EARLY_METER_EVERY`] through its first [`READ_USAGE_EVERY`],
/// then every [`READ_USAGE_EVERY`].
fn meter_due(begun: Instant, last: Option<Instant>, now: Instant) -> bool {
    let every = if now.saturating_duration_since(begun) < READ_USAGE_EVERY {
        EARLY_METER_EVERY
    } else {
        READ_USAGE_EVERY
    };
    last.is_none_or(|t| now.saturating_duration_since(t) >= every)
}

/// How long a child's tree may stay idle ([`aterm_sysprobe::activity::Used::idle`]: no CPU time
/// and no I/O) before a span it spent waiting for a core counts as work again: a tree that gets no
/// core at all for this long is a hang its ceiling ends, as a stopped one is.
const IDLE_HANG: Duration = Duration::from_secs(10);

/// How far back a child's clock judges its tree's wait for a core ([`CoreWaitSpan`]). The kernel
/// books a thread's runnable and CPU time only when the thread next gets a core or leaves one, so
/// the wait of a starved tree shows in a later read than the window it fell in: a utility-tier busy
/// loop's runnable time read every 25 ms on this Mac on 2026-10-27 stayed flat in 61-66% of the
/// reads and then grew in lumps of up to 925 ms. A tree that gets no core for longer than this is a
/// hang ([`IDLE_HANG`]).
const CORE_WAIT_SPAN: Duration = Duration::from_secs(10);

/// A child's tree's wait for a core longer than this is said as the child ends
/// ([`Clock::starved_line`]).
const STARVED_SAID: Duration = Duration::from_secs(60);

/// The part of a window of `wall` that a tree which used `used` in it spent waiting for a core,
/// on a machine of `cores`: the window less the share of its demand it got, where its demand is
/// the time it was runnable, at most every core for the whole window, and never more than the
/// time its threads were runnable and not running. A tree that was runnable for none of the window
/// (stopped, or blocked in a read, a lock or a sleep) waited for no core in it.
fn core_wait(wall: Duration, used: Used, cores: u32) -> Duration {
    let waiting = used.runnable.saturating_sub(used.cpu);
    let demand = used.runnable.min(wall.saturating_mul(cores.max(1)));
    if demand.is_zero() || waiting.is_zero() {
        return Duration::ZERO;
    }
    let share = (used.cpu.as_secs_f64() / demand.as_secs_f64()).clamp(0.0, 1.0);
    wall.mul_f64(1.0 - share).min(waiting)
}

/// One read of a child's tree, as its clock's [`CoreWaitSpan`] keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TreeRead {
    /// The wall time since the read before.
    wall: Duration,
    /// What the tree used in that time ([`aterm_sysprobe::activity::between`]).
    used: Used,
    /// The work the clock counted in that time.
    worked: Duration,
    /// What this read moved from the clock's work to its wait for a core.
    moved: Duration,
}

/// The reads of a child's tree in the last [`CORE_WAIT_SPAN`], judged together for its wait for a
/// core: a wait the kernel books late, in a lump, is still left out, and a tree whose share of the
/// cores changes is judged on its last span, not on its whole life.
#[derive(Debug, Default)]
struct CoreWaitSpan {
    reads: VecDeque<TreeRead>,
}

impl CoreWaitSpan {
    /// Keep a read of `wall`, in which the tree used `used` and the clock counted `worked` as work,
    /// dropping the oldest reads while the ones after them still cover [`CORE_WAIT_SPAN`]. Then say
    /// what this read moves from the clock's work, `left` in all, to its wait for a core: the wait
    /// over the kept reads together, up to the last in which the kernel booked any runnable or CPU
    /// time ([`core_wait`] of their summed wall and use on `cores`), at most the work counted in
    /// them, less what every kept read moved before, and at most `left`. Reads after the last
    /// booking are judged once one covers them, so a tree that stops after a stretch starved
    /// waited for no core in its stop. A read moves nothing unless that wait has grown past what
    /// the kept reads moved.
    fn book(
        &mut self,
        wall: Duration,
        used: Used,
        worked: Duration,
        cores: u32,
        left: Duration,
    ) -> Duration {
        self.reads.push_back(TreeRead {
            wall,
            used,
            worked,
            moved: Duration::ZERO,
        });
        let mut covered: Duration = self.reads.iter().map(|r| r.wall).sum();
        while self.reads.len() > 1
            && let Some(oldest) = self.reads.front()
            && covered.saturating_sub(oldest.wall) >= CORE_WAIT_SPAN
        {
            covered = covered.saturating_sub(oldest.wall);
            self.reads.pop_front();
        }
        let booked = self
            .reads
            .iter()
            .rposition(|r| !r.used.runnable.is_zero() || !r.used.cpu.is_zero())
            .map_or(0, |last| last + 1);
        let mut all = TreeRead::default();
        for (i, r) in self.reads.iter().enumerate() {
            if i < booked {
                all.wall += r.wall;
                all.used.cpu += r.used.cpu;
                all.used.runnable += r.used.runnable;
                all.used.io = all.used.io.saturating_add(r.used.io);
                all.worked += r.worked;
            }
            all.moved += r.moved;
        }
        let moved = core_wait(all.wall, all.used, cores)
            .min(all.worked)
            .saturating_sub(all.moved)
            .min(left);
        if let Some(this) = self.reads.back_mut() {
            this.moved = moved;
        }
        moved
    }

    /// Forget every read: a tree idle for [`IDLE_HANG`] spent that time in a hang, which no later
    /// read's lump may book as a wait for a core.
    fn clear(&mut self) {
        self.reads.clear();
    }
}

/// A clock's reads of what its child's tree used (CEILING-WAIT's wait for a core).
struct Usage<'a> {
    host: &'a dyn Host,
    /// The child's process group: its tree.
    pgid: u32,
    cores: u32,
    /// When it last read, and each member that read found.
    read_at: Instant,
    seen: aterm_sysprobe::activity::Members,
    /// When it last tried to read, the read missed ([`Reading::Missed`]) or not: its cadence.
    tried_at: Instant,
    /// The work counted since that read.
    worked: Duration,
    /// When a read last found the tree active, not idle ([`IDLE_HANG`]).
    active_at: Instant,
    /// The reads its wait for a core is judged on.
    span: CoreWaitSpan,
}

/// Complete descendant-tree readings kept separately from the process-group meter: a child may
/// form another group, and group usage is unavailable off macOS, while the shared process sweep
/// still proves its CPU progress.
struct TreeProgressClock<'a> {
    host: &'a dyn Host,
    root: u32,
    read_at: Instant,
    seen: Vec<TreeMember>,
    observations: u64,
    comparable: bool,
    generation: u64,
}

/// A CHILD'S OWN METER ([`TreeUsage`]): every read its clock and its sweep make of its tree, booked
/// as its stage's meter books them ([`crate::memory::Meter::book`]), and each incarnation's most
/// runnable time beside them.
#[derive(Debug, Default)]
struct Own {
    meter: crate::memory::Meter,
    /// The most runnable time each process incarnation, by pid and start, was read at.
    runnable: std::sync::Mutex<BTreeMap<(u32, u64), u64>>,
}

impl Own {
    /// Book a read of the process group `pgid`, whose members are `members`.
    fn book(&self, pgid: u32, members: &[aterm_sysprobe::MemberUsage]) {
        self.meter.book(pgid, members);
        let mut runnable = self
            .runnable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for m in members {
            let most = runnable.entry((m.pid, m.start)).or_default();
            *most = (*most).max(m.runnable_ns);
        }
    }

    /// What the child used, its clock `clock` and its wall `wall`; `None` when no read booked a
    /// member of its tree, or when the clock's last read missed and none worked after it
    /// ([`Clock::missed`]): what the tree used since is unknown, never the figure of its last read.
    fn usage(&self, clock: &Clock<'_>, wall: Duration) -> Option<TreeUsage> {
        if clock.missed {
            return None;
        }
        let cpu = self.meter.observed_cpu()?;
        let runnable = self
            .runnable
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .fold(0_u64, |sum, ns| sum.saturating_add(*ns));
        Some(TreeUsage {
            cpu: Duration::from_nanos(cpu.nanoseconds),
            runnable: Duration::from_nanos(runnable),
            starved: clock.starved,
            waited: clock.waited,
            wall,
            peak_bytes: self.meter.peak().unwrap_or(0),
            processes: cpu.processes,
        })
    }
}

/// A child's clock (`docs/DESIGN-adaptive-build-2026-10-24.md`, CEILING-WAIT): the time it
/// worked, which its ceiling bounds, apart from the time a wait of its tree for admission or a
/// lock was marked in `marks` ([`aterm_toolchain::waits`]), which only its holders bound, and the
/// time its tree waited for a core ([`core_wait`]), which the tree's own idleness bounds
/// ([`IDLE_HANG`]). A wait's liveness is its own: its holder named once a minute, and a
/// dead holder lets its lock go; a live holder whose processes make no progress for its waiter's
/// bound is named stuck in the mark, and the child is then ended ([`Clock::stuck`]), its holder
/// left alone (CW-WAIT-UNBOUNDED). Each span counts as the last look found it; a read of the tree
/// then moves from its work to [`Clock::starved`] what the reads of the last [`CORE_WAIT_SPAN`]
/// show it waited for a core and have not moved yet ([`CoreWaitSpan::book`]). Each read also
/// books the tree's footprint into its stage's memory meter (VERIFY-MEM, [`crate::memory`]) and
/// into the child's own ([`Own`], [`TreeUsage`]), and the clock's end takes the tree out of its
/// stage's.
struct Clock<'a> {
    host: &'a dyn Host,
    marks: Option<&'a Path>,
    last: Instant,
    looked: Option<Instant>,
    worked: Duration,
    waited: Duration,
    /// The time its tree waited for a core, which its ceiling leaves out too.
    starved: Duration,
    /// The waits the last look found.
    waits: Vec<aterm_toolchain::waits::Waiting>,
    /// The waits the preceding look found. A mark is rewritten in place, so one read can catch
    /// the tail of the record it replaces: only the same live mark and holder set found stuck
    /// on both looks can end the child ([`Clock::stuck`]).
    waits_before: Vec<aterm_toolchain::waits::Waiting>,
    /// `None` once its host has no reading of the tree: every span with no marked wait is work.
    usage: Option<Usage<'a>>,
    /// Its last read of the tree missed ([`Reading::Missed`]) and none has worked since: what the
    /// tree used after the last read that worked is unknown ([`Own::usage`]).
    missed: bool,
    /// The complete descendant tree's CPU progress, independent of process-group accounting.
    tree: Option<TreeProgressClock<'a>>,
    /// The child's own meter, which each read that books its stage's meter books too
    /// ([`TreeUsage`]); `None` for a clock that keeps no account of its child.
    own: Option<&'a Own>,
    /// The stage meter its reads of the tree book into, and the tree's process group.
    meter: Option<(Arc<crate::memory::Meter>, u32)>,
    /// When the child began, and when its first [`READ_USAGE_EVERY`]'s reads for its meter last
    /// read the tree ([`meter_due`]).
    begun: Instant,
    metered_at: Option<Instant>,
}

impl<'a> Clock<'a> {
    fn new(
        marks: Option<&'a Path>,
        host: &'a dyn Host,
        pgid: u32,
        meter: Option<Arc<crate::memory::Meter>>,
    ) -> Clock<'a> {
        let now = host.monotonic();
        if let Some(m) = &meter {
            m.started();
        }
        Clock {
            host,
            meter: meter.map(|m| (m, pgid)),
            begun: now,
            metered_at: None,
            marks,
            last: now,
            looked: None,
            worked: Duration::ZERO,
            waited: Duration::ZERO,
            starved: Duration::ZERO,
            waits: Vec::new(),
            waits_before: Vec::new(),
            own: None,
            missed: false,
            usage: Some(Usage {
                host,
                pgid,
                cores: host.cores(),
                read_at: now,
                seen: BTreeMap::new(),
                tried_at: now,
                worked: Duration::ZERO,
                active_at: now,
                span: CoreWaitSpan::default(),
            }),
            tree: None,
        }
    }

    /// Read the descendant tree only for a child carrying [`CaseWatch`]. Other verifier stages
    /// retain the existing cheap process-group meter and do not cause process-table sweeps.
    fn watching_tree(mut self, yes: bool) -> Self {
        if yes {
            self.tree = Some(TreeProgressClock {
                host: self.usage.as_ref().expect("a new clock has its host").host,
                root: self
                    .usage
                    .as_ref()
                    .expect("a new clock has its process group")
                    .pgid,
                read_at: self.last,
                seen: Vec::new(),
                observations: 0,
                comparable: false,
                generation: 0,
            });
        }
        self
    }

    /// This clock books its reads into `own` as well ([`Clock::own`]).
    fn owning(mut self, own: &'a Own) -> Clock<'a> {
        self.own = Some(own);
        self
    }

    /// Book a read of the tree's members into its stage's meter and its own, where it has them.
    fn book(&self, members: &[aterm_sysprobe::MemberUsage]) {
        if let Some((meter, pgid)) = &self.meter {
            meter.book(*pgid, members);
        }
        if let (Some(own), Some(u)) = (self.own, &self.usage) {
            own.book(u.pgid, members);
        }
    }

    /// Count the span since the last tick, then look again once [`LOOK_EVERY`] has passed, and
    /// read the tree once [`READ_USAGE_EVERY`] has; through the first of those, read it for its
    /// meters alone (its stage's, its own) as often as [`meter_due`] says.
    fn tick(&mut self) {
        self.tick_kind(true);
    }

    /// Advance every clock except the cached descendant-tree probe. A post-sample confirmation
    /// follows this immediately with [`Self::read_tree_usage_fresh`], preserving the last
    /// comparable pre-sample baseline even when the shared cached sweep is unreadable.
    fn tick_before_fresh_tree_usage(&mut self) {
        self.tick_kind(false);
    }

    fn tick_kind(&mut self, read_tree: bool) {
        let now = self.host.monotonic();
        if now.saturating_duration_since(self.begun) < READ_USAGE_EVERY
            && meter_due(self.begun, self.metered_at, now)
        {
            self.metered_at = Some(now);
            if (self.meter.is_some() || self.own.is_some())
                && let Some(u) = &self.usage
            {
                match u.host.group_reading(u.pgid) {
                    Reading::Read(members) => {
                        self.missed = false;
                        self.book(&members);
                    }
                    Reading::Missed => self.missed = true,
                    // Its next read at its cadence ends the reads (`read_usage`).
                    Reading::Over => {}
                }
            }
        }
        let span = now.saturating_duration_since(self.last);
        if self.waits.is_empty() {
            self.worked += span;
            if let Some(u) = &mut self.usage {
                u.worked += span;
            }
        } else {
            self.waited += span;
        }
        self.last = now;
        if self
            .looked
            .is_none_or(|t| now.saturating_duration_since(t) >= LOOK_EVERY)
        {
            self.look();
        }
        if self
            .usage
            .as_ref()
            .is_some_and(|u| now.saturating_duration_since(u.tried_at) >= u.host.usage_every())
        {
            self.read_usage();
        }
        if read_tree
            && self.tree.as_ref().is_some_and(|tree| {
                now.saturating_duration_since(tree.read_at) >= tree.host.usage_every()
            })
        {
            self.read_tree_usage();
        }
    }

    /// Read what the tree used since the last read, book what it holds and its live rate into its
    /// stage's meter ([`crate::memory::Meter::book_at`]) and into its own
    /// ([`Clock::own`]), and move to
    /// [`Clock::starved`] the part of its work that the reads of its span show it spent waiting
    /// for a core and have not moved yet
    /// ([`CoreWaitSpan::book`]), unless it has been idle for [`IDLE_HANG`]: then the
    /// span is forgotten and nothing moves. A host with no reading ends the reads
    /// ([`Reading::Over`]); a read that missed ([`Reading::Missed`]) books nothing, and the next
    /// one that works covers the whole span since the last that did.
    fn read_usage(&mut self) {
        let Some(u) = &mut self.usage else {
            return;
        };
        let now = self.last;
        u.tried_at = now;
        let members = match u.host.group_reading(u.pgid) {
            Reading::Read(members) => members,
            Reading::Missed => {
                self.missed = true;
                return;
            }
            Reading::Over => {
                self.usage = None;
                return;
            }
        };
        self.missed = false;
        let wall = now.saturating_duration_since(u.read_at);
        let worked = std::mem::take(&mut u.worked);
        u.read_at = now;
        if let Some((meter, pgid)) = &self.meter {
            meter.book_at(*pgid, &members, now);
        }
        if let Some(own) = self.own {
            own.book(u.pgid, &members);
        }
        let (used, seen) = aterm_sysprobe::activity::between(&u.seen, members);
        u.seen = seen;
        if !used.idle() {
            u.active_at = now;
        }
        if now.saturating_duration_since(u.active_at) >= IDLE_HANG {
            u.span.clear();
            return;
        }
        let moved = u.span.book(wall, used, worked, u.cores, self.worked);
        self.worked -= moved;
        self.starved += moved;
    }

    /// Read a complete descendant tree. An unread tree, changed membership, or a counter that
    /// moved backwards is incomparable and therefore disarms the hang window for this read.
    fn read_tree_usage(&mut self) {
        self.read_tree_usage_kind(false);
    }

    /// Read a complete descendant tree without the live host's cadence cache. This is the final
    /// post-sample observation: it must begin after sampling, because a cached pre-resume view is
    /// not independent evidence that the binary remained still.
    fn read_tree_usage_fresh(&mut self) {
        self.read_tree_usage_kind(true);
    }

    fn read_tree_usage_kind(&mut self, fresh: bool) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        tree.read_at = self.last;
        let reading = if fresh {
            tree.host.fresh_tree_usage(tree.root)
        } else {
            tree.host.tree_usage(tree.root)
        };
        let Some(mut now) = reading else {
            tree.seen.clear();
            tree.observations = 0;
            tree.comparable = false;
            return;
        };
        now.sort_unstable();
        if now.is_empty() {
            tree.seen.clear();
            tree.observations = 0;
            tree.comparable = false;
            return;
        }
        if tree.observations == 0 {
            tree.seen = now;
            tree.observations = 1;
            tree.comparable = true;
            return;
        }
        let same = tree.seen.len() == now.len()
            && tree
                .seen
                .iter()
                .zip(&now)
                .all(|(before, after)| (before.pid, before.start) == (after.pid, after.start));
        let monotone = same
            && tree
                .seen
                .iter()
                .zip(&now)
                .all(|(before, after)| before.cpu_ns <= after.cpu_ns);
        if monotone
            && tree
                .seen
                .iter()
                .zip(&now)
                .any(|(before, after)| before.cpu_ns < after.cpu_ns)
        {
            tree.generation = tree.generation.saturating_add(1);
        }
        tree.seen = now;
        tree.observations = tree.observations.saturating_add(1);
        tree.comparable = monotone;
    }

    /// The process tree's CPU progress generation, only after a successful complete reading.
    fn cpu_progress(&self) -> Option<u64> {
        let tree = self.tree.as_ref()?;
        tree.comparable.then_some(tree.generation)
    }

    /// Whether its work has reached `limit`, as of a read of its tree that covers it: one more
    /// read first, judged with the reads of its span ([`CoreWaitSpan`]), so a wait for a core the
    /// kernel has booked since the last read is left out before the work is judged. The time since
    /// the tree's last booking is work until a read books it.
    fn over(&mut self, limit: Duration) -> bool {
        if self.worked < limit {
            return false;
        }
        if self.usage.as_ref().is_some_and(|u| u.tried_at < self.last) {
            self.read_usage();
        }
        self.worked >= limit
    }

    /// The members of its tree whose CPU time read at most [`aterm_sysprobe::jobs::HELD_CPU_NS`]
    /// at its last read of the tree, as a held program's does, each with that time
    /// ([`start::Watch::stranded`]); `None` before that read, and once its host has no reading.
    fn quiet_members(&self) -> Option<Vec<(u32, u64)>> {
        let u = self.usage.as_ref().filter(|u| !u.seen.is_empty())?;
        Some(
            u.seen
                .values()
                .filter(|m| m.cpu_ns <= aterm_sysprobe::jobs::HELD_CPU_NS)
                .map(|m| (m.pid, m.cpu_ns))
                .collect(),
        )
    }

    /// What a child that waited longer than [`STARVED_SAID`] for a core says as it ends.
    fn starved_line(&self) -> Option<String> {
        (self.starved > STARVED_SAID).then(|| {
            format!(
                "it waited {} for a core beside {} of work, which the ceiling leaves out",
                aterm_capacity::span(self.starved),
                aterm_capacity::span(self.worked)
            )
        })
    }

    /// Look for its tree's marked waits now. A look that finds none once the run is ending
    /// ([`ending`]) keeps what the last look found: the interrupt marks the run ending before its
    /// `SIGTERM` reaches the child, and a waiter that ends its wait on that signal lets its mark
    /// go while it still lives to say so, so an empty look after the end may be the end's doing.
    fn look(&mut self) {
        if let Some(dir) = self.marks {
            let waits = aterm_toolchain::waits::waiting(dir);
            self.looked = Some(self.host.monotonic());
            if waits.is_empty() && ending() {
                return;
            }
            if !waits.is_empty() {
                ends::saw();
            }
            self.waits_before = std::mem::replace(&mut self.waits, waits);
        }
    }

    /// Why the child must end now, when the last look found its tree waiting behind holders its
    /// waiter found stuck ([`aterm_toolchain::waits::Stuck`]), none of them in the child's own
    /// process group `pgid` (the group its end signals, so a holder is never signalled), on two
    /// looks in a row at the same mark and holder set: what it waited for, the holder named,
    /// and how long that has made no progress. Another mark or a replacement holder lends no
    /// confirmation, and changing diagnostic text or elapsed span does not discard it.
    fn stuck(&self, pgid: u32) -> Option<String> {
        let (w, stuck) = self.waits.iter().find_map(|w| {
            let stuck = w.stuck.as_ref()?;
            let holders: BTreeSet<_> = stuck.holders.iter().copied().collect();
            if holders.is_empty()
                || !self.waits_before.iter().any(|before| {
                    w.same_mark(before)
                        && before.stuck.as_ref().is_some_and(|stuck| {
                            stuck.holders.iter().copied().collect::<BTreeSet<_>>() == holders
                        })
                })
            {
                return None;
            }
            (!stuck.holders.iter().any(|&h| in_group(h, pgid))).then_some((w, stuck))
        })?;
        let now = aterm_time::unix_secs().unwrap_or(0);
        Some(format!(
            "this child waited for admission or a lock behind a holder that has made no progress \
             for {}, so it was ended and decided nothing (the holder was left alone): {} (waiting \
             {})",
            aterm_capacity::span(stuck.span),
            w.text,
            aterm_capacity::span(Duration::from_secs(now.saturating_sub(w.since)))
        ))
    }

    /// Why a child the run's end ended decided nothing: what it waited for, holder named, when
    /// the last look found it waiting ([`Clock::look`]).
    fn ended(&self) -> String {
        if let Some(words) = halted() {
            return words.to_owned();
        }
        if self.waits.is_empty() {
            return "the run was interrupted while this child ran".to_string();
        }
        let now = aterm_time::unix_secs().unwrap_or(0);
        let waits: Vec<String> = self
            .waits
            .iter()
            .map(|w| {
                format!(
                    "{} (waiting {})",
                    w.text,
                    aterm_capacity::span(Duration::from_secs(now.saturating_sub(w.since)))
                )
            })
            .collect();
        format!(
            "the run was interrupted while this child waited for admission or a lock, so it \
             decided nothing: {}",
            waits.join("; ")
        )
    }
}

impl Drop for Clock<'_> {
    /// The child's tree is reaped or given up: what it held is its stage's no longer.
    fn drop(&mut self) {
        if let Some((meter, pgid)) = &self.meter {
            meter.ended(*pgid);
        }
    }
}

/// Whether `pid` is in process group `pgid`: a holder there would get the child's end.
fn in_group(pid: u32, pgid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: `getpgid` takes an integer and reads the process table; it signals nothing.
        let group = unsafe { libc::getpgid(pid) };
        group > 0 && u32::try_from(group).is_ok_and(|g| g == pgid)
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, pgid);
        false
    }
}

/// How long a child its stuck holder ends ([`Clock::stuck`]) has after its `SIGTERM` to say so
/// before its group gets `SIGKILL`, as the interrupt's handler gives a stage group.
#[cfg(not(test))]
const STUCK_GRACE: Duration = Duration::from_secs(2);
/// A unit test's child is a stand-in that must say why it ends once its `SIGTERM` lands, so it
/// has the hang detector to say it.
#[cfg(test)]
const STUCK_GRACE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The words of the halt that ended the run at its first proven red (`group::halt`), which
/// every child it ended or kept from starting says in place of an interrupt's; none while no halt
/// has, and never off Unix, where no group is ended.
pub(crate) fn halted() -> Option<&'static str> {
    #[cfg(unix)]
    let halted = group::halted();
    #[cfg(not(unix))]
    let halted = None;
    halted
}

/// Whether the run must end now: an interrupt is ending it.
pub(crate) fn ending() -> bool {
    #[cfg(unix)]
    let stopping = group::stopping();
    #[cfg(not(unix))]
    let stopping = false;
    stopping || ends::now()
}

/// A test's stand-in for the run's interrupt, on its own thread only: the process-wide one would
/// end every test's children. It marks the run ending, as the handler does first; the test then
/// sends its child the handler's `SIGTERM` itself.
#[cfg(test)]
#[path = "exec_ends_tests.rs"]
pub(crate) mod ends;

#[cfg(not(test))]
mod ends {
    pub(super) const fn now() -> bool {
        false
    }

    pub(super) const fn saw() {}
}

/// A check or measurement child with a signal environment independent of verify's launcher.
pub(crate) fn check_command(program: impl AsRef<OsStr>) -> Command {
    check_signals(aterm_uds::spawnfd::command(program))
}

fn check_signals(command: Command) -> Command {
    #[cfg(unix)]
    {
        let mut command = command;
        command.default_signals();
        command
    }
    #[cfg(not(unix))]
    {
        command
    }
}

fn spawn_command(cmd: &Cmd, env: ExecEnv<'_>, taskpolicy: &Path) -> Command {
    let native = |program: &OsStr| {
        #[cfg(unix)]
        {
            aterm_uds::spawnfd::transparent(program).into_command()
        }
        #[cfg(not(unix))]
        {
            aterm_uds::spawnfd::command(program)
        }
    };
    let mut c = if cmd.demoted
        && at_desk(env.host)
        && crate::is_executable_file(taskpolicy)
        && resolves(cmd, env)
    {
        let mut c = native(taskpolicy.as_os_str());
        c.args(["-c", "utility"]);
        let dyld = dyld_assignments(&cmd.envs);
        if !dyld.is_empty()
            && crate::is_executable_file(Path::new(ENV))
            && !cmd.program.as_os_str().to_string_lossy().contains('=')
        {
            c.arg(ENV).args(dyld);
        }
        c.arg(&cmd.program);
        c
    } else {
        native(cmd.program.as_os_str())
    };
    c.args(&cmd.args).stdin(Stdio::null());
    // Stages lead separate groups, so each needs its own parent-death tie too.
    c.tether();
    #[cfg(unix)]
    {
        c.process_group(0);
    }
    check_signals(c)
}

const ENV: &str = aterm_uds::os_path!("env");

fn dyld_assignments(envs: &[(OsString, OsString)]) -> Vec<OsString> {
    envs.iter()
        .filter(|(k, _)| k.to_string_lossy().starts_with("DYLD_"))
        .map(|(k, v)| {
            let mut a = k.clone();
            a.push("=");
            a.push(v);
            a
        })
        .collect()
}

fn resolves(cmd: &Cmd, env: ExecEnv<'_>) -> bool {
    let program = cmd.program.as_path();
    let cwd = cmd.cwd.as_deref().unwrap_or(env.cwd);
    if program.components().count() > 1 {
        return crate::is_executable_file(&cwd.join(program));
    }
    let path = cmd
        .envs
        .iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map_or(env.path, |(_, v)| v.as_os_str());
    std::env::split_paths(path).any(|d| crate::is_executable_file(&cwd.join(d).join(program)))
}

/// Spawn `c` and reap it, ending it at `env.child_ceiling` of work ([`Clock`]: its tree's waits
/// marked in `marks` and its waits for a core read of `env.host` left out). Once the run is ending
/// ([`ending`]) the interrupt's handler ends it, and its row says what it waited for, if it
/// waited. A child whose tree waits behind a stuck holder ([`Clock::stuck`]) is ended
/// (`SIGTERM`, [`STUCK_GRACE`], then `SIGKILL`) and could not run, naming the holder. A child
/// whose tree holds a member stranded at macOS's program check ([`start::Watch::stranded`]), said
/// at once on stderr, and, at its ceiling, one whose leader has not started or whose tree holds a
/// member held past the bound on a first exec ([`start::Watch::at_ceiling`]) are ended unrun
/// ([`kill_unrun`]): a failed run with `spawn_error` set, which no row reads as a finding. A
/// leader that never ran gets `SIGKILL` alone; one that started, over a deeper member that never
/// ran, gets `SIGTERM` and its `term_grace` first, as the stuck holder's end gives it before the
/// ceiling ([`STUCK_GRACE`] when it has none) and as the ceiling's own kill gives it at the
/// ceiling. A child that ends after waiting longer than [`STARVED_SAID`] for a core says so on
/// stderr.
fn emit_case(
    events: Option<&crate::event_log::EventLog>,
    watch: &CaseWatch,
    seen: &test_progress::Seen,
    evidence: Option<&test_progress::Failure>,
    binary_elapsed: Duration,
    halts: bool,
) {
    use test_progress::Seen;
    // A spared case's own block says its machine, not the tree, refused it: it reads
    // NOT RUN, as its binary's row will; a red or an undecided case reads FAIL.
    let running_long = matches!(seen, Seen::RunningLong { .. });
    let outcome = if running_long {
        "RUNNING"
    } else if matches!(seen, Seen::Spared { .. }) {
        "NOT RUN"
    } else {
        "FAIL"
    };
    let (case, says, execution) = match &seen {
        Seen::RunningLong { case } => (Some(case.as_str()), None, None),
        Seen::Red {
            case,
            marks,
            execution,
        }
        | Seen::Spared {
            case,
            marks,
            execution,
        } => (Some(case.as_str()), block_says(*marks), *execution),
        Seen::Undecided { case } => (case.as_deref(), Some("unknown"), None),
    };
    crate::event_log::emit(
        events,
        &format!(
            "verify-event: {}",
            crate::event_log::event_value(
                if running_long {
                    crate::event_log::EventKind::TestCaseRunningLong
                } else {
                    crate::event_log::EventKind::TestCaseFailed
                },
                aterm_json::json!({
                    "outcome": outcome,
                    "test": watch.spec, "case": case.map(|name| crate::report::clip_json(name, crate::report::EVENT_ROW_BYTES)),
                    "at_unix_ms": aterm_time::unix_millis(),
                    "binary_elapsed_s": binary_elapsed.as_secs_f64(),
                    "case_execution_s": execution.map(|duration| duration.as_secs_f64()),
                    "binary_completed": false,
                    "halts": halts, "block_says": says,
                    "sample_complete": false,
                    "output": evidence.map(|f| f.output.as_str()),
                    "omitted_bytes": evidence.map(|f| f.omitted_bytes),
                    "source_path": evidence.and_then(|f| f.source_path.as_deref()),
                    "start_line": evidence.and_then(|f| f.start_line),
                    "start_col": evidence.and_then(|f| f.start_col),
                    "detail": match seen {
                        Seen::RunningLong { .. } => "libtest reports this case is still running; no verdict",
                        Seen::Red { .. } => "its block decides its binary a FAIL",
                        Seen::Spared { .. } => "its block says its machine, not the tree",
                        Seen::Undecided { .. } => "its block is decided at its binary's end",
                    },
                })
            )
        ),
    );
}

/// Observe a bounded final batch only after the owner has positively reaped its child.
/// No observation here can stop a process or set the returned run's `stopped_at`.
fn observe_reaped(
    cases: &mut test_progress::LiveCases,
    watch: &CaseWatch,
    events: Option<&crate::event_log::EventLog>,
    binary_elapsed: Duration,
) {
    for seen in cases.poll() {
        let evidence = cases.observations().failures.iter().find(|f| match &seen {
            test_progress::Seen::Red { case, .. } | test_progress::Seen::Spared { case, .. } => {
                f.case == *case
            }
            _ => false,
        });
        emit_case(events, watch, &seen, evidence, binary_elapsed, false);
    }
}

fn observed_case(
    watch: &CaseWatch,
    seen: test_progress::Seen,
    begun: Instant,
    events: Option<&crate::event_log::EventLog>,
    evidence: Option<&test_progress::Failure>,
) -> Option<crate::libtest_json::Stopped> {
    use test_progress::Seen;
    let halts = watch.stop_at_red && matches!(seen, Seen::Red { .. }) && !ending();
    emit_case(events, watch, &seen, evidence, begun.elapsed(), halts);
    match (halts, seen) {
        (true, Seen::Red { case, .. }) => Some(crate::libtest_json::Stopped {
            spec: watch.spec.clone(),
            case,
        }),
        _ => None,
    }
}

fn discard_sample(sample: &StackSample) {
    if let Some(path) = &sample.report {
        let _ = std::fs::remove_file(path);
    }
}

struct FinishControls<'a, 'lock> {
    compiler: Option<compiler_progress::Progress>,
    local_red: Option<&'a (dyn Fn() + Sync)>,
    admission: &'a mut SpawnAdmission<'lock>,
    external_stop: Option<&'a (dyn Fn() -> Option<String> + Sync)>,
    open_cases: &'a CaseOpener,
}

struct ObservedFinish<'a, 'control, 'lock> {
    controls: FinishControls<'control, 'lock>,
    cases: &'a mut Option<test_progress::LiveCases>,
}

fn finish(
    c: Command,
    cmd: &Cmd,
    env: ExecEnv<'_>,
    log: Option<&Path>,
    label: &str,
    marks: Option<&Path>,
    controls: FinishControls<'_, '_>,
) -> (Run, Option<String>) {
    let mut cases = None;
    let (mut run, logged) = finish_inner(
        c,
        cmd,
        env,
        log,
        label,
        marks,
        ObservedFinish {
            controls,
            cases: &mut cases,
        },
    );
    if let Some(watch) = &cmd.test_cases {
        run.case_observations = Some(cases.as_ref().map_or_else(
            || test_progress::Observations {
                json: watch.json,
                ..Default::default()
            },
            |cases| cases.observations().clone(),
        ));
    }
    (run, logged)
}

fn finish_inner(
    mut c: Command,
    cmd: &Cmd,
    env: ExecEnv<'_>,
    log: Option<&Path>,
    label: &str,
    marks: Option<&Path>,
    observed: ObservedFinish<'_, '_, '_>,
) -> (Run, Option<String>) {
    let ObservedFinish { controls, cases } = observed;
    let FinishControls {
        mut compiler,
        local_red,
        admission,
        external_stop,
        open_cases,
    } = controls;
    let (ceiling, stage_groups) = (env.child_ceiling, env.stage_groups);
    let read_log = |log: Option<&Path>| {
        log.map(|p| {
            String::from_utf8_lossy(&crate::read_captured(p).unwrap_or_default()).into_owned()
        })
    };
    let own = Own::default();
    #[cfg(unix)]
    let sweep = |pgid: u32| {
        // The unreaped leader still has its final counters, booked into the stage's meter and
        // the child's own alike. Descendants which exited between earlier samples remain absent
        // from this lower bound.
        if let Some(members) = env.host.group_usage(pgid) {
            if let Some(meter) = crate::memory::current() {
                meter.book(pgid, &members);
            }
            own.book(pgid, &members);
        }
        if let Some(groups) = stage_groups {
            groups.group_ended(pgid, label);
        }
    };
    #[cfg(unix)]
    let record = |pgid: u32| {
        if let Some(groups) = stage_groups {
            groups.group_reaped(pgid, label);
        }
    };
    #[cfg(not(unix))]
    let _ = (stage_groups, label);
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait(halted().unwrap_or("the run was interrupted before this child started")),
            read_log(log),
        );
    }
    #[cfg(unix)]
    let spawning = group::Spawning::begin();
    #[cfg(unix)]
    if group::stopping() {
        return (
            failed_to_wait(halted().unwrap_or("the run was interrupted before this child started")),
            read_log(log),
        );
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (failed_to_wait(&e.to_string()), read_log(log)),
    };
    if let Err(error) = admission.release() {
        eprintln!(
            "verify: {label}: could not release its cross-process start admission after spawn: \
             {error}; later starts remain closed until this binary ends"
        );
    }
    #[cfg(unix)]
    let mut live = group::Live::enter(child.id());
    #[cfg(unix)]
    if let Some(groups) = stage_groups {
        groups.group_made(child.id(), label);
    }
    #[cfg(unix)]
    drop(spawning);

    *cases = match cmd.test_cases.as_ref() {
        None => None,
        Some(watch) => {
            let opened = log.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a watched test binary has no capture stream",
                )
            });
            let opened = opened.and_then(|path| open_cases(path, watch.json));
            match opened {
                Ok(cases) => Some(cases),
                Err(error) => {
                    let why = format!(
                        "cannot monitor {} for output progress: {error}",
                        watch.binary
                    );
                    let killed = kill_and_reap(
                        &mut child,
                        #[cfg(unix)]
                        &mut live,
                        None,
                        #[cfg(unix)]
                        REAP_AFTER_KILL,
                    );
                    return (
                        Run {
                            case_observations: None,
                            ok: false,
                            output: format!("aterm-verify: NOT RUN — {why}\n{killed}"),
                            code: None,
                            spawn_error: Some(why),
                            stopped_at: None,
                            hung: None,
                            usage: None,
                        },
                        read_log(log),
                    );
                }
            }
        }
    };
    let begun = Instant::now();
    let mut progress = cmd
        .test_cases
        .as_ref()
        .map(|watch| TestProgress::new(watch.hang_after, begun));
    let mut clock = Clock::new(marks, env.host, child.id(), crate::memory::current())
        .watching_tree(cmd.test_cases.is_some())
        .owning(&own);
    let mut starts = start::Watch::new(env.host, child.id(), &cmd.program);
    let used = |run: Run, clock: &Clock<'_>| Run {
        usage: own.usage(clock, begun.elapsed()),
        ..run
    };
    let mut told = false;
    let mut nap = Duration::from_millis(1);
    let mut pending = VecDeque::new();
    let mut external_stop_due = Instant::now();
    loop {
        let now = Instant::now();
        if now >= external_stop_due {
            external_stop_due = now + READ_USAGE_EVERY;
            if let Some(why) = external_stop.and_then(|stop| stop()) {
                eprintln!("verify: {label}: NOT RUN — {why}");
                let killed = kill_and_reap(
                    &mut child,
                    #[cfg(unix)]
                    &mut live,
                    None,
                    #[cfg(unix)]
                    REAP_AFTER_KILL,
                );
                return (
                    used(
                        Run {
                            case_observations: None,
                            ok: false,
                            output: format!("aterm-verify: NOT RUN — {why}\n{killed}"),
                            code: None,
                            spawn_error: Some(why),
                            stopped_at: None,
                            hung: None,
                            usage: None,
                        },
                        &clock,
                    ),
                    read_log(log),
                );
            }
        }
        if let (Some(watch), Some(cases)) = (&cmd.test_cases, cases.as_mut()) {
            pending.extend(cases.poll());
            while let Some(seen) = pending.pop_front() {
                if let Some(stopped) = observed_case(
                    watch,
                    seen.clone(),
                    begun,
                    env.events,
                    cases.observations().failures.iter().find(|f| match &seen {
                        test_progress::Seen::Red { case, .. }
                        | test_progress::Seen::Spared { case, .. } => f.case == *case,
                        _ => false,
                    }),
                ) {
                    // The red is ended here, inside the child's own run and before anything
                    // calls the run's halt (`Halt::red`, which `testrun` calls once this returns):
                    // the halt makes `ending` true, and a child reaped while the run is ending is
                    // released as interrupted (`Live::release_for_reap`), which would turn this
                    // red into a NOT RUN.
                    if let Some(red) = local_red {
                        red();
                    }
                    let killed = kill_and_reap(
                        &mut child,
                        #[cfg(unix)]
                        &mut live,
                        Some(STUCK_GRACE),
                        #[cfg(unix)]
                        REAP_AFTER_KILL,
                    );
                    return (
                        used(
                            Run {
                                case_observations: None,
                                ok: false,
                                output: killed,
                                code: None,
                                spawn_error: None,
                                stopped_at: Some(stopped),
                                hung: None,
                                usage: None,
                            },
                            &clock,
                        ),
                        read_log(log),
                    );
                }
            }
        }
        #[cfg(unix)]
        let status = live.reap_stage_sweeping(&mut child, false, sweep, record);
        #[cfg(not(unix))]
        let status = child.try_wait();
        match status {
            Ok(Some(st)) => {
                if let Some(line) = clock.starved_line() {
                    eprintln!("verify: {label}: {line}");
                }
                if let (Some(watch), Some(cases)) = (&cmd.test_cases, cases.as_mut()) {
                    observe_reaped(cases, watch, env.events, begun.elapsed());
                }
                if let Some(compiler) = compiler.as_mut() {
                    compiler.poll(env.events);
                }
                return (used(reaped(Ok(st)), &clock), read_log(log));
            }
            Ok(None) => {}
            Err(_) if ending() => {
                clock.look();
                return (used(failed_to_wait(&clock.ended()), &clock), read_log(log));
            }
            Err(e) => {
                return (used(failed_to_wait(&e.to_string()), &clock), read_log(log));
            }
        }
        clock.tick();
        let suspected = !ending()
            && match (&cmd.test_cases, cases.as_ref(), &mut progress) {
                (Some(_), Some(cases), Some(progress)) => {
                    progress.hung(clock.last, cases.lines(), clock.cpu_progress())
                }
                _ => false,
            };
        if suspected {
            let sample = sample_hung(env, child.id());
            // A failed sampler may answer immediately. Wait for an independent process reading,
            // then force a new process-table sweep and recheck every signal: sampling is
            // evidence collection, not the verdict. The cadence begins after sampling, so a
            // target resumed by the sampler gets a whole observation interval in which to run.
            let confirm_at = Instant::now() + env.host.usage_every();
            while !ending() && Instant::now() < confirm_at {
                std::thread::sleep(
                    LOOK_EVERY.min(confirm_at.saturating_duration_since(Instant::now())),
                );
            }
            #[cfg(unix)]
            let status = live.reap_stage_sweeping(&mut child, false, sweep, record);
            #[cfg(not(unix))]
            let status = child.try_wait();
            match status {
                Ok(Some(st)) => {
                    discard_sample(&sample);
                    if let (Some(watch), Some(cases)) = (&cmd.test_cases, cases.as_mut()) {
                        observe_reaped(cases, watch, env.events, begun.elapsed());
                    }
                    if let Some(compiler) = compiler.as_mut() {
                        compiler.poll(env.events);
                    }
                    return (used(reaped(Ok(st)), &clock), read_log(log));
                }
                Ok(None) => {}
                Err(_) if ending() => {
                    discard_sample(&sample);
                    clock.look();
                    return (used(failed_to_wait(&clock.ended()), &clock), read_log(log));
                }
                Err(error) => {
                    discard_sample(&sample);
                    return (
                        used(failed_to_wait(&error.to_string()), &clock),
                        read_log(log),
                    );
                }
            }
            clock.tick_before_fresh_tree_usage();
            clock.read_tree_usage_fresh();
            if let Some(cases) = cases.as_mut() {
                pending.extend(cases.poll());
            }
            let confirmed = !ending()
                && match (&cmd.test_cases, cases.as_ref(), &mut progress) {
                    (Some(_), Some(cases), Some(progress)) => {
                        progress.hung(clock.last, cases.lines(), clock.cpu_progress())
                    }
                    _ => false,
                };
            if confirmed && let (Some(watch), Some(cases)) = (&cmd.test_cases, cases.as_ref()) {
                // This is the decision point. Close admission under the dispatcher's own lock
                // before ending the binary or writing its finished event.
                if watch.stop_at_red
                    && let Some(red) = local_red
                {
                    red();
                }
                let hung = Hung {
                    spec: watch.spec.clone(),
                    binary: watch.binary.clone(),
                    running: cases.running(),
                    window: watch.hang_after,
                    sample,
                    json: watch.json,
                };
                eprintln!("verify: {}", hung.detail());
                let killed = kill_and_reap(
                    &mut child,
                    #[cfg(unix)]
                    &mut live,
                    None,
                    #[cfg(unix)]
                    REAP_AFTER_KILL,
                );
                return (
                    used(
                        Run {
                            case_observations: None,
                            ok: false,
                            output: format!("aterm-verify: {}\n{killed}", hung.detail()),
                            code: None,
                            spawn_error: None,
                            stopped_at: None,
                            hung: Some(hung),
                            usage: None,
                        },
                        &clock,
                    ),
                    read_log(log),
                );
            }
            discard_sample(&sample);
        }
        if ending() {
            // What its tree waits for as the run ends, said at once, ahead of the row the run's
            // report gives it once every stage group is ended. The handler ends the child too (its
            // one `SIGTERM`, up to 2 s, then `SIGKILL`), so it is not signalled here: a lab ends
            // its wait on that `SIGTERM` and says NOT RUN, naming the holder, and a second signal
            // would end it at once, unsaid.
            if !told {
                told = true;
                clock.look();
                if !clock.waits.is_empty() {
                    eprintln!("verify: {label}: {}", clock.ended());
                }
            }
        } else if let Some(why) = clock.stuck(child.id()) {
            eprintln!("verify: {label}: {why}");
            let killed = kill_and_reap(
                &mut child,
                #[cfg(unix)]
                &mut live,
                Some(cmd.term_grace.unwrap_or(STUCK_GRACE)),
                #[cfg(unix)]
                REAP_AFTER_KILL,
            );
            return (
                used(
                    Run {
                        case_observations: None,
                        stopped_at: None,
                        hung: None,
                        ok: false,
                        output: killed,
                        code: None,
                        spawn_error: Some(why),
                        usage: None,
                    },
                    &clock,
                ),
                read_log(log),
            );
        } else if let Some(v) = starts.stranded(|| clock.quiet_members(), label) {
            eprintln!("verify: {label}: {}", v.words);
            let grace = (!v.leader).then(|| cmd.term_grace.unwrap_or(STUCK_GRACE));
            let note = kill_unrun(
                &mut child,
                #[cfg(unix)]
                &mut live,
                grace,
            );
            return (
                used(spawn_failure(cmd, &format!("{}{note}", v.words)), &clock),
                read_log(log),
            );
        } else if let Some(limit) = ceiling
            && clock.over(limit)
        {
            if let Some(v) = starts.at_ceiling(clock.quiet_members(), limit) {
                let note = kill_unrun(
                    &mut child,
                    #[cfg(unix)]
                    &mut live,
                    if v.leader { None } else { cmd.term_grace },
                );
                return (
                    used(spawn_failure(cmd, &format!("{}{note}", v.words)), &clock),
                    read_log(log),
                );
            }
            let killed = kill_and_reap(
                &mut child,
                #[cfg(unix)]
                &mut live,
                cmd.term_grace,
                #[cfg(unix)]
                REAP_AFTER_KILL,
            );
            return (
                used(over_ceiling(cmd, &clock, limit, &killed), &clock),
                read_log(log),
            );
        }
        if !ending()
            && let Some(compiler) = compiler.as_mut()
        {
            compiler.poll(env.events);
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(25));
    }
}

/// What a failed case's block says of its machine, for its `test-case-failed` event: `null` for a
/// block that says neither (a plain failure).
fn block_says(marks: crate::libtest::Marks) -> Option<&'static str> {
    match (marks.refused, marks.disk) {
        (true, true) => Some("both"),
        (true, false) => Some("refusal"),
        (false, true) => Some("disk"),
        (false, false) => None,
    }
}

/// Send `SIGTERM` to `child`'s group and wait up to `grace` for `child` to exit: `None` when the
/// signal could not be sent, else whether it exited within the grace (it is not reaped). The
/// ceiling's and the stuck holder's end ([`kill_and_reap`]) and the unrun end ([`kill_unrun`])
/// share it.
#[cfg(unix)]
fn term_within(child: &mut Child, live: &mut group::Live, grace: Duration) -> Option<bool> {
    live.term().ok()?;
    let now = Instant::now();
    let deadline = now
        .checked_add(grace)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600));
    while Instant::now() < deadline {
        if live.exited(child, false).unwrap_or(false) {
            return Some(true);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Some(false)
}

/// End `child`'s group: with a `grace`, `SIGTERM` first and up to `grace` for its leader to exit
/// ([`term_within`]); then `SIGKILL` and the leader's reap, which, for a leader still there after
/// the grace, gives up on Unix at `reap_bound` ([`reap_killed`]). What it adds to the child's
/// block: nothing, the grace its leader exited within, what failed, or a leader left as it is.
fn kill_and_reap(
    child: &mut Child,
    #[cfg(unix)] live: &mut group::Live,
    grace: Option<Duration>,
    #[cfg(unix)] reap_bound: Duration,
) -> String {
    #[cfg(unix)]
    if let Some(grace) = grace
        && term_within(child, live, grace) == Some(true)
    {
        let _ = live.kill();
        let _ = live.reap_stage(child, true);
        return format!(
            "  (SIGTERM first, with a {:.1}s grace: the child exited within it)\n",
            grace.as_secs_f64()
        );
    }
    #[cfg(not(unix))]
    let _ = grace;
    #[cfg(unix)]
    let note = match live.kill() {
        Ok(()) => String::new(),
        Err(e) if live.holds(child.id()) => match child.kill() {
            Ok(()) => format!(
                "  (killing its process group failed: {e} — SIGKILL reached the direct child only)\n"
            ),
            Err(k) => format!("  (SIGKILL itself failed: {e}; {k})\n"),
        },
        Err(e) => format!("  (group custody was lost: {e}; its old id was not signalled)\n"),
    };
    #[cfg(not(unix))]
    let note = match child.kill() {
        Ok(()) => String::new(),
        Err(e) => format!("  (SIGKILL itself failed: {e})\n"),
    };
    #[cfg(unix)]
    let note = match reap_killed(child, live, reap_bound) {
        Some(left) => format!("{note}  ({left})\n"),
        None => note,
    };
    #[cfg(not(unix))]
    let _ = child.wait();
    note
}

/// How long a child sent `SIGKILL` may take to end before verify gives up its reap: whether a
/// program held at its first exec dies of it is unmeasured (leftover U11), and a wait on one that
/// does not would hang the run. The one-minute hang detector.
#[cfg(unix)]
const REAP_AFTER_KILL: Duration = Duration::from_secs(60);

/// Reap `child`, just sent `SIGKILL`, looking every 25 ms for at most `bound`: `None` once it is
/// reaped or its custody reads lost, else the clause that says it did not end and was left as it
/// is, never signalled again here (its handle, once dropped, hands it to `aterm_uds`' reaper).
#[cfg(unix)]
fn reap_killed(child: &mut Child, live: &mut group::Live, bound: Duration) -> Option<String> {
    let begun = Instant::now();
    loop {
        match live.exited(child, false) {
            Ok(true) => {
                let _ = live.reap_stage(child, true);
                return None;
            }
            Ok(false) if begun.elapsed() < bound => std::thread::sleep(Duration::from_millis(25)),
            Ok(false) => {
                return Some(format!(
                    "it did not end within {} of SIGKILL: pid {}, left as it is",
                    aterm_capacity::span(bound),
                    child.id()
                ));
            }
            Err(_) => return None,
        }
    }
}

/// End a child verify ends unrun ([`start`]): verify's own stage child, through the group it
/// holds, and nothing else. With no `grace` (the program that never ran is the leader, and a
/// program whose first instruction never ran has installed no handler) `SIGKILL` comes alone.
/// With a `grace` (it is a deeper member, under a leader that started and may clean up on
/// `SIGTERM`) the group gets `SIGTERM` first, and the `SIGKILL` once the leader exits or the grace
/// ends ([`term_within`], as [`kill_and_reap`] gives it). The reap after the `SIGKILL` is bounded
/// at [`REAP_AFTER_KILL`] ([`reap_killed`]). Its file is never touched. What it adds to the
/// child's why: nothing, the grace's end, what failed, or a leader left as it is.
fn kill_unrun(
    child: &mut Child,
    #[cfg(unix)] live: &mut group::Live,
    grace: Option<Duration>,
) -> String {
    #[cfg(unix)]
    {
        let termed = match grace.map(|g| (g, term_within(child, live, g))) {
            Some((g, Some(true))) => format!(
                "; its leader, which had started, was sent SIGTERM first and exited within its \
                 {:.1}s grace",
                g.as_secs_f64()
            ),
            Some((g, Some(false))) => format!(
                "; its leader, which had started, was sent SIGTERM first, and SIGKILL after its \
                 {:.1}s grace",
                g.as_secs_f64()
            ),
            Some((_, None)) | None => String::new(),
        };
        let killed = match live.kill() {
            Ok(()) => String::new(),
            Err(e) if live.holds(child.id()) => match child.kill() {
                Ok(()) => {
                    format!("; killing its process group failed ({e}), so SIGKILL reached it alone")
                }
                Err(k) => format!("; SIGKILL itself failed ({e}; {k})"),
            },
            Err(e) => format!("; its group custody was lost ({e}), so it was not signalled"),
        };
        let left = reap_killed(child, live, REAP_AFTER_KILL)
            .map_or_else(String::new, |l| format!("; {l}"));
        format!("{termed}{killed}{left}")
    }
    #[cfg(not(unix))]
    {
        let _ = grace;
        let killed = match child.kill() {
            Ok(()) => String::new(),
            Err(e) => format!("; SIGKILL itself failed ({e})"),
        };
        let _ = child.wait();
        killed
    }
}

fn over_ceiling(cmd: &Cmd, clock: &Clock<'_>, limit: Duration, kill: &str) -> Run {
    let argv = cmd.argv().join(" ");
    let waited = waited_clause(clock.waited, clock.starved);
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!(
            "{} — child killed after {:.1}s of work, over the {:.1}s ceiling \
             (--timeout <seconds|off>){waited}\n\x20 child: {argv}\n{kill}",
            crate::findings::CEILING_KILL,
            clock.worked.as_secs_f64(),
            limit.as_secs_f64(),
        ),
        code: None,
        spawn_error: None,
        usage: None,
    }
}

/// What a ceiling kill's line adds about the time the ceiling left out: `lock` spent in marked
/// waits for admission or a lock, `core` waiting for a core; each named only when it shows as more
/// than `0.0s`.
fn waited_clause(lock: Duration, core: Duration) -> String {
    let shows = |d: Duration| d >= Duration::from_millis(50);
    let secs = |d: Duration| d.as_secs_f64();
    match (shows(lock), shows(core)) {
        (false, false) => String::new(),
        (true, false) => format!(
            "; it also waited {:.1}s for admission or a lock, which the ceiling leaves out",
            secs(lock)
        ),
        (false, true) => format!(
            "; it also waited {:.1}s for a core, which the ceiling leaves out",
            secs(core)
        ),
        (true, true) => format!(
            "; it also waited {:.1}s for admission or a lock and {:.1}s for a core, which the \
             ceiling leaves out",
            secs(lock),
            secs(core)
        ),
    }
}

/// Process-group custody and interrupt handling for stage children.
#[cfg(unix)]
pub mod group {
    /// The actual owned child implementations used by stage execution and GUI smoke.
    /// Custody checks the owned child's pid before observation or reap.
    pub(crate) trait WaitChild {
        fn id(&self) -> u32;
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus>;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    }

    impl WaitChild for std::process::Child {
        fn id(&self) -> u32 {
            Self::id(self)
        }
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            Self::wait(self)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            Self::try_wait(self)
        }
    }

    impl WaitChild for aterm_uds::spawnfd::Child {
        fn id(&self) -> u32 {
            Self::id(self)
        }
        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            Self::wait(self)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            Self::try_wait(self)
        }
    }

    use std::ffi::c_long;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use libc::{P_PID, WNOHANG, WNOWAIT, siginfo_t, waitid};
    use libc::{killpg, nanosleep, signal};

    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    const SIG_DFL: libc::sighandler_t = libc::SIG_DFL;

    const GRACE_ROUNDS: u32 = 200;
    const NAP_MS: c_long = 10;

    static LIVE: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

    static STOPPING: AtomicBool = AtomicBool::new(false);
    /// Set when a wait for capacity took the interrupt and [`stop_run`] ends the run.
    static WAIT_STOPPED: AtomicBool = AtomicBool::new(false);
    /// Set when the run's first proven red ends it ([`halt`]) before any interrupt did: an
    /// interrupt after it is the run's first, which it reports before it dies of.
    static HALTING: AtomicBool = AtomicBool::new(false);
    static SPAWNING: AtomicUsize = AtomicUsize::new(0);
    /// The interrupt the run dies of once it has reported ([`end_interrupted`]), set by the
    /// handler once every stage group is ended; 0 while there is none.
    static PENDING: AtomicI32 = AtomicI32::new(0);
    /// Set as the handler begins ending the stage groups, before [`PENDING`] is.
    static HANDLING: AtomicBool = AtomicBool::new(false);
    /// The write end of the pipe [`kill_on_interrupt`]'s watchdog reads ([`REPORT_GRACE`]); -1
    /// where it has none, and the handler then re-raises its signal at once.
    static WATCHDOG: AtomicI32 = AtomicI32::new(-1);

    /// How long an interrupted run has, once every stage group is ended, to write its log, its
    /// verdict and its `log:` line and to remove its scratch before its watchdog re-raises the
    /// signal regardless ([`end_interrupted`]).
    pub const REPORT_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

    /// The grace a test's stand-in run set for its watchdog, in milliseconds (0: none set).
    #[cfg(test)]
    static REPORT_GRACE_FOR_TEST_MS: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// Hold this process's interrupted run to `grace` instead of [`REPORT_GRACE`]: a stand-in run
    /// whose report must happen sets the hang detector, so a loaded host's slow report is not cut
    /// short; one whose grace must fire keeps [`REPORT_GRACE`].
    #[cfg(test)]
    pub(crate) fn set_report_grace_for_test(grace: std::time::Duration) {
        let ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX).max(1);
        REPORT_GRACE_FOR_TEST_MS.store(ms, Ordering::SeqCst);
    }

    /// [`REPORT_GRACE`], or the grace a test set with `set_report_grace_for_test`.
    fn report_grace() -> std::time::Duration {
        #[cfg(test)]
        match REPORT_GRACE_FOR_TEST_MS.load(Ordering::SeqCst) {
            0 => {}
            ms => return std::time::Duration::from_millis(ms),
        }
        REPORT_GRACE
    }

    /// Counts the spawn-to-registration window.
    pub struct Spawning(());
    impl Spawning {
        #[must_use]
        pub fn begin() -> Self {
            SPAWNING.fetch_add(1, Ordering::SeqCst);
            Self(())
        }
    }
    impl Drop for Spawning {
        fn drop(&mut self) {
            SPAWNING.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A group may be signalled only while this handle holds its leader.
    pub struct Live {
        slot: Option<usize>,
        group: Option<u32>,
    }
    impl Live {
        #[must_use]
        pub fn enter(pgid: u32) -> Self {
            let g = i32::try_from(pgid).unwrap_or(0);
            let live = Self {
                slot: LIVE.iter().position(|slot| {
                    g > 0
                        && slot
                            .compare_exchange(0, g, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                }),
                group: (g > 0).then_some(pgid),
            };
            if stopping() {
                let _ = live.kill();
            }
            live
        }
        fn leave(&mut self) {
            if let Some(i) = self.slot.take() {
                LIVE[i].store(0, Ordering::SeqCst);
            }
        }
        fn held(&self) -> std::io::Result<u32> {
            self.group
                .ok_or_else(|| std::io::Error::other("the group leader is no longer held unreaped"))
        }
        pub(crate) fn holds(&self, pid: u32) -> bool {
            self.group == Some(pid)
        }
        pub fn kill(&self) -> std::io::Result<()> {
            signal_group(self.held()?, SIGKILL)
        }
        pub fn term(&self) -> std::io::Result<()> {
            signal_group(self.held()?, SIGTERM)
        }
        fn release_for_reap(&mut self, pid: u32) -> std::io::Result<()> {
            if self.held()? != pid {
                return Err(std::io::Error::other("child does not own this group"));
            }
            self.leave();
            // `super::ending`, not `stopping`: a test's per-thread end reads as the run's.
            if super::ending() {
                let _ = self.kill();
                return Err(std::io::Error::other(
                    halted().unwrap_or("the run was interrupted while this child ran"),
                ));
            }
            self.group = None;
            Ok(())
        }
        pub(crate) fn exited(
            &mut self,
            child: &mut impl WaitChild,
            block: bool,
        ) -> std::io::Result<bool> {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                if self.held()? != child.id() {
                    return Err(std::io::Error::other("child does not own this group"));
                }
                let seen = exited(self.held()?, block);
                if seen.is_err() {
                    self.leave();
                    self.group = None;
                }
                seen
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let seen = if block {
                    child.wait().map(Some)
                } else {
                    child.try_wait()
                };
                if !matches!(seen, Ok(None)) {
                    self.leave();
                    self.group = None;
                }
                seen.map(|status| status.is_some())
            }
        }
        pub(super) fn reap_stage(
            &mut self,
            child: &mut impl WaitChild,
            block: bool,
        ) -> std::io::Result<Option<std::process::ExitStatus>> {
            self.reap_stage_sweeping(child, block, |_| {}, |_| {})
        }

        /// Reap `child`, running `sweep` once after the leader exits and before it is reaped, and
        /// `reaped` with the same group once it is (macOS and Linux only).
        pub(crate) fn reap_stage_sweeping(
            &mut self,
            child: &mut impl WaitChild,
            block: bool,
            sweep: impl FnOnce(u32),
            reaped: impl FnOnce(u32),
        ) -> std::io::Result<Option<std::process::ExitStatus>> {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                if !self.exited(child, block)? {
                    return Ok(None);
                }
                let swept = self.group;
                if let Some(pgid) = swept {
                    sweep(pgid);
                }
                self.release_for_reap(child.id())?;
                let status = child.wait()?;
                if let Some(pgid) = swept {
                    reaped(pgid);
                }
                Ok(Some(status))
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = (sweep, reaped);
                self.exited(child, block)?;
                child.try_wait()
            }
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        pub(super) fn exited_output(
            &mut self,
            child: &aterm_uds::spawnfd::Child,
        ) -> std::io::Result<bool> {
            if self.held()? != child.id() {
                return Err(std::io::Error::other(
                    "output child does not own this group",
                ));
            }
            let seen = exited(self.held()?, false);
            if seen.is_err() {
                self.leave();
                self.group = None;
            }
            seen
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        pub(super) fn reap_output(
            &mut self,
            child: &mut aterm_uds::spawnfd::Child,
        ) -> std::io::Result<std::process::ExitStatus> {
            if !self.holds(child.id()) {
                return Err(std::io::Error::other(
                    "output child does not own this group",
                ));
            }
            exited(self.held()?, true)?;
            self.release_for_reap(child.id())?;
            child.wait()
        }
    }
    impl Drop for Live {
        fn drop(&mut self) {
            self.leave();
        }
    }

    /// Scoped only to the exact child owned by one custody fixture.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    #[path = "custody_audit_tests.rs"]
    pub(super) mod custody_audit;

    /// Whether process group `pgid` has no process left in it, by the kernel's answer about the
    /// whole group (`killpg(pgid, 0)`, which sends nothing): `Some(true)` when the group is gone
    /// (`ESRCH`); `Some(false)` when a process is in it, whether or not this user may signal it
    /// (`0`, or `EPERM`: another user's, and on macOS a zombie its parent has not reaped, which
    /// Linux answers `0` for when it is this user's); `None` for a `pgid` of 1 or less
    /// (`killpg` reads 0 as this process's own group and 1 as every process this user may
    /// signal) or past `i32::MAX`, unasked, and for any other answer. macOS also answers `EPERM`
    /// for a group that holds no process, only the old image a member's exec left, which the
    /// kernel tears down after the exec, on a loaded machine after the member itself is reaped;
    /// there an `EPERM` is settled by the group's listing ([`judged`]). Until its leader is
    /// reaped the leader keeps the group, so it is asked after the reap; a group made since
    /// under the freed id then reads as not empty, never as empty.
    #[must_use]
    pub fn empty(pgid: u32) -> Option<bool> {
        let g = i32::try_from(pgid).ok().filter(|&g| g > 1)?;
        // SAFETY: two integers; signal 0 sends nothing, and `g` > 1 names one group.
        let answer = if unsafe { killpg(g, 0) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().raw_os_error())
        };
        judged(answer, || aterm_uds::process::group_members(pgid))
    }

    /// [`empty`]'s verdict on `killpg`'s `answer` (`Ok` for 0, else its errno). An `EPERM` on
    /// Apple reads `listing`, the group's members as `proc_listpids` finds them: it lists every
    /// live and zombie process of every user and skips only what an exec left of an old image
    /// (measured on macOS 27: a reaped `/bin/sh` leader, whose shim execs `bash`, answered
    /// `EPERM` and listed nothing for up to 1.6 s on a loaded machine, then `ESRCH`), so a whole
    /// listing that finds no process proves the group empty, as the sweep's whole listing does
    /// ([`crate::runs::sweep_group`]); a listing cut short, or none, leaves it not empty. A
    /// process still being created in the group is in neither answer, as in no listing.
    #[must_use]
    pub(crate) fn judged(
        answer: Result<(), Option<i32>>,
        listing: impl FnOnce() -> Option<aterm_uds::process::GroupMembers>,
    ) -> Option<bool> {
        match answer {
            Ok(()) => Some(false),
            Err(Some(libc::ESRCH)) => Some(true),
            Err(Some(libc::EPERM)) => Some(
                cfg!(target_vendor = "apple")
                    && listing().is_some_and(|members| members.complete && members.pids.is_empty()),
            ),
            Err(_) => None,
        }
    }

    fn signal_group(pgid: u32, sig: i32) -> std::io::Result<()> {
        #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
        custody_audit::before_signal(pgid, sig)?;
        #[cfg(test)]
        if sig == SIGKILL && spared::armed() {
            return Ok(());
        }
        let g = i32::try_from(pgid).map_err(|_| std::io::Error::other("pid out of range"))?;
        // SAFETY: callers retain the unreaped group leader; two integer arguments.
        if unsafe { killpg(g, sig) } == 0 {
            Ok(())
        } else {
            aterm_uds::process::settle_group_signal(g, std::io::Error::last_os_error())
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(super) fn exited(pid: u32, block: bool) -> std::io::Result<bool> {
        const EINTR: i32 = 4;
        let options = libc::WEXITED | WNOWAIT | if block { 0 } else { WNOHANG };
        let mut nap = std::time::Duration::from_millis(1);
        loop {
            // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is a valid
            // value; the kernel overwrites it.
            let mut info: siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is live and writable, and `pid` names the child whose exit is observed
            // without reaping it (`WNOWAIT`).
            let got = unsafe { waitid(P_PID, pid, &raw mut info, options) };
            if got == 0 {
                #[cfg(target_os = "macos")]
                let ended = info.si_signo != 0 && (1..=3).contains(&info.si_code);
                #[cfg(target_os = "linux")]
                let ended = info.si_signo != 0;
                if ended || !block {
                    return Ok(ended);
                }
                std::thread::sleep(nap);
                nap = (nap * 2).min(std::time::Duration::from_millis(25));
                continue;
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(EINTR) {
                return Err(e);
            }
        }
    }

    /// Has an interrupt begun ending the run?
    #[must_use]
    pub fn stopping() -> bool {
        STOPPING.load(Ordering::SeqCst)
    }

    fn sweep(sig: i32) {
        for slot in &LIVE {
            let g = slot.load(Ordering::SeqCst);
            if g > 0 {
                // SAFETY: as in `kill`.
                unsafe {
                    killpg(g, sig);
                }
            }
        }
    }

    extern "C" fn on_signal(sig: i32, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
        // SAFETY: the SA_SIGINFO action supplies this live kernel packet.
        unsafe { aterm_uds::signal_origin::capture(sig, info) };
        if STOPPING.swap(true, Ordering::SeqCst) {
            if WAIT_STOPPED.load(Ordering::SeqCst) || PENDING.load(Ordering::SeqCst) != 0 {
                // The run is ending on a wait's interrupt ([`stop_run`]), or reporting after one
                // ([`end_interrupted`]); another ends it now.
                // SAFETY: both take integers only.
                unsafe {
                    signal(sig, SIG_DFL);
                    reraise(sig);
                }
            }
            if HALTING.load(Ordering::SeqCst) {
                // A halt is ending the run and has ended its groups ([`halt`]): this is the run's
                // first interrupt, so it reports, then dies of it.
                die_after_report(sig);
            }
            return;
        }
        HANDLING.store(true, Ordering::SeqCst);
        sweep(SIGTERM);
        let nap = libc::timespec {
            tv_sec: 0,
            tv_nsec: NAP_MS * 1_000_000,
        };
        for _ in 0..GRACE_ROUNDS {
            if LIVE.iter().all(|slot| slot.load(Ordering::SeqCst) == 0)
                && SPAWNING.load(Ordering::SeqCst) == 0
            {
                break;
            }
            // SAFETY: `nanosleep` reads the struct we own and writes nothing when `rem` is null.
            unsafe {
                nanosleep(&raw const nap, std::ptr::null_mut());
            }
        }
        sweep(SIGKILL);
        die_after_report(sig);
    }

    /// The run reports what it has, then dies of `sig` ([`end_interrupted`]); its watchdog
    /// re-raises it past [`REPORT_GRACE`] if it has not. Without a watchdog, at once. Called from
    /// the handler: async-signal-safe.
    fn die_after_report(sig: i32) {
        PENDING.store(sig, Ordering::SeqCst);
        let fd = WATCHDOG.load(Ordering::SeqCst);
        let byte = u8::try_from(sig).unwrap_or(u8::MAX);
        // SAFETY: `write` is async-signal-safe; `byte` lives across the call and `fd` is the
        // watchdog pipe's write end, which nothing closes.
        if fd >= 0 && unsafe { libc::write(fd, (&raw const byte).cast(), 1) } == 1 {
            return;
        }
        // SAFETY: both take integers only.
        unsafe {
            signal(sig, SIG_DFL);
            reraise(sig);
        }
    }

    /// End an interrupted run once it has reported: die of the interrupt the handler ended its
    /// stage groups for, as the run would have without reporting, once the handler has ended
    /// them (a stage's group leaves the live list before the handler's next look sees it gone).
    /// A run no interrupt reached, or one a wait's interrupt ended ([`stop_run`], which exits
    /// with its verdict), returns.
    pub fn end_interrupted() {
        while HANDLING.load(Ordering::SeqCst) && PENDING.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(NAP_MS.unsigned_abs()));
        }
        let sig = PENDING.load(Ordering::SeqCst);
        if sig != 0 {
            // SAFETY: both take integers only.
            unsafe {
                signal(sig, SIG_DFL);
                reraise(sig);
            }
        }
    }

    /// The watchdog an interrupted run's report is held to: woken by the handler through its
    /// pipe ([`WATCHDOG`]), it re-raises the handler's signal [`REPORT_GRACE`] later unless the
    /// run has died of it by then ([`end_interrupted`]). Set once; `false` where its pipe or its
    /// thread could not be made.
    fn watchdog() -> bool {
        use std::io::Read as _;
        use std::os::fd::IntoRawFd as _;
        if WATCHDOG.load(Ordering::SeqCst) >= 0 {
            return true;
        }
        let Ok((mut read, write)) = std::io::pipe() else {
            return false;
        };
        let spawned = std::thread::Builder::new()
            .name("verify-interrupt-watchdog".into())
            .spawn(move || {
                let mut sig = [0u8];
                if read.read_exact(&mut sig).is_ok() {
                    std::thread::sleep(report_grace());
                    let sig = PENDING.load(Ordering::SeqCst);
                    // SAFETY: both take integers only.
                    unsafe {
                        signal(sig, SIG_DFL);
                        reraise(sig);
                    }
                }
            });
        if spawned.is_err() {
            return false;
        }
        WATCHDOG.store(write.into_raw_fd(), Ordering::SeqCst);
        true
    }

    /// Re-raise `sig` at its default action.
    /// # Safety
    /// As `raise`: none beyond an integer argument.
    unsafe fn reraise(sig: i32) {
        #[expect(
            clippy::disallowed_methods,
            reason = "re-raises the interrupt the run was sent (SIGHUP/SIGINT/SIGTERM), never a \
                      core-dump signal"
        )]
        // SAFETY: `raise` takes an integer and touches no memory of ours.
        unsafe {
            libc::raise(sig);
        }
    }

    /// The words of the halt that ended the run ([`halt`]); none while no halt has.
    static HALTED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    /// End the run at its first proven red (`crate::sched::Halt`, design §4.3 A3), as [`stop_run`]
    /// does: no stage child starts after this, and every live one's group is ended (SIGTERM, a
    /// grace, then SIGKILL). Each child it ends or keeps from starting says `words` ([`halted`])
    /// and reads as stopped, not as a finding, so its row is NOT RUN; a run an interrupt already
    /// ends keeps the interrupt's words. An interrupt after it is the run's first: the run reports,
    /// then dies of it. Returns once every live group has left or the grace is spent.
    pub fn halt(words: &str) {
        if !stopping() {
            let _ = HALTED.set(words.to_owned());
            HALTING.store(true, Ordering::SeqCst);
        }
        end_groups();
    }

    /// The words of the halt that ended the run ([`halt`]), if one did.
    #[must_use]
    pub fn halted() -> Option<&'static str> {
        HALTED.get().map(String::as_str)
    }

    /// End the run as [`kill_on_interrupt`]'s handler would, but without dying of it: a wait for
    /// capacity took the interrupt (`aterm_toolchain::interrupt`) and named what it waited for.
    /// No stage child starts after this, every live one's group is ended (SIGTERM, a grace, then
    /// SIGKILL) and reads as interrupted, not as a finding (`Live::release_for_reap`), no stage
    /// starts ([`stopping`]), and the run goes on to report what it has; a further interrupt
    /// ends it at once.
    pub fn stop_run() {
        WAIT_STOPPED.store(true, Ordering::SeqCst);
        end_groups();
    }

    /// Mark the run ending ([`stopping`]) and end every live stage group: SIGTERM, a grace, then
    /// SIGKILL. Returns at once when the run was already ending.
    fn end_groups() {
        if STOPPING.swap(true, Ordering::SeqCst) {
            return;
        }
        sweep(SIGTERM);
        for _ in 0..GRACE_ROUNDS {
            if LIVE.iter().all(|slot| slot.load(Ordering::SeqCst) == 0)
                && SPAWNING.load(Ordering::SeqCst) == 0
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(NAP_MS.unsigned_abs()));
        }
        sweep(SIGKILL);
    }

    /// On `SIGINT`, `SIGTERM` or `SIGHUP`, end every live stage child's group; the run then
    /// reports what it has and dies of the signal ([`end_interrupted`]), held to
    /// [`REPORT_GRACE`] by a watchdog.
    pub fn kill_on_interrupt() {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return;
        }
        if let Err(why) = aterm_uds::signal_origin::start() {
            eprintln!("signal provenance unavailable: {why}");
        }
        watchdog();
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: the SA_SIGINFO handler lives for the process and does only
            // async-signal-safe work; the action and its mask are initialized here.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
                libc::sigemptyset(&raw mut action.sa_mask);
                libc::sigaction(sig, &raw const action, std::ptr::null_mut());
            }
        }
    }

    /// Hand an in-place exec a race-free interrupt boundary once this image has no live stage
    /// groups. A signal the old handlers already took makes this return `false`; after every
    /// disposition is default, a signal in the remaining exec gap ends the process instead of
    /// being consumed into this image's soon-to-disappear atomics. The new image installs its
    /// own handlers immediately. When exec fails, the caller must call [`kill_on_interrupt`]
    /// again before doing more work.
    #[must_use]
    pub fn default_interrupts_for_exec() -> bool {
        if !cfg!(any(target_os = "macos", target_os = "linux")) || stopping() {
            return !stopping();
        }
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: SIG_DFL is a valid disposition and `sig` is one of the three constants.
            unsafe {
                signal(sig, SIG_DFL);
            }
        }
        !stopping()
    }

    /// The interrupt the run will die of once it has reported; 0 while there is none.
    #[cfg(test)]
    pub(crate) fn pending() -> i32 {
        PENDING.load(Ordering::SeqCst)
    }

    /// How many stage groups are listed now.
    #[cfg(test)]
    pub(crate) fn live_count() -> usize {
        LIVE.iter()
            .filter(|slot| slot.load(Ordering::SeqCst) != 0)
            .count()
    }

    /// A unit test's stand-in for a leader `SIGKILL` does not end, as a program macOS holds at
    /// its first exec may not (leftover U11, unmeasured): while a `Scope` is armed on a thread, a
    /// `SIGKILL` that thread sends a group is sent nowhere and answers `Ok`, so the group's leader
    /// runs on.
    #[cfg(test)]
    #[path = "spared_tests.rs"]
    pub(super) mod spared;
}

fn reaped(st: io::Result<std::process::ExitStatus>) -> Run {
    match st {
        Ok(st) => Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: st.success(),
            output: ended_by_signal(st),
            code: st.code(),
            spawn_error: None,
            usage: None,
        },
        Err(e) => failed_to_wait(&e.to_string()),
    }
}

/// The line a child killed by a signal cannot write for itself, naming the signal; empty for
/// a child that exited.
fn ended_by_signal(st: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = st.signal() {
            let core = if st.core_dumped() {
                ", core dumped"
            } else {
                ""
            };
            return format!("aterm-verify: the child was killed by signal {sig}{core}\n");
        }
    }
    let _ = st;
    String::new()
}

fn failed_to_wait(why: &str) -> Run {
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: Some(why.to_string()),
        usage: None,
    }
}

fn spawn_failure(cmd: &Cmd, why: &str) -> Run {
    Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!("aterm-verify: cannot run {}: {why}", cmd.program.display()),
        code: None,
        spawn_error: Some(why.to_string()),
        usage: None,
    }
}

/// Run a child and return its merged stdout and stderr with trailing newlines stripped.
#[must_use]
pub fn capture_reply(cmd: &Cmd, env: ExecEnv<'_>) -> String {
    let r = run(&cmd.clone().capture(Capture::Emit), env);
    r.trimmed_output().to_string()
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "exec_custody_tests.rs"]
mod custody_tests;

#[cfg(test)]
#[path = "exec_memory_tests.rs"]
mod memory_tests;
